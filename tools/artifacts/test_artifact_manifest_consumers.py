#!/usr/bin/env python3
"""Deterministic, no-output tests for the manifest-backed artifact builders."""

from __future__ import annotations

import copy
import hashlib
import inspect
import json
import re
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import build_evidence_manifest as manifest_builder
import build_elite_visual_report as docx_builder
import build_elite_visual_pdf as pdf_builder
import evidence_manifest_consumer as consumer
import test_build_evidence_manifest as fixtures


TOOLS_DIR = Path(__file__).resolve().parent
DOCX_BUILDER = TOOLS_DIR / "build_elite_visual_report.py"
PDF_BUILDER = TOOLS_DIR / "build_elite_visual_pdf.py"
XLSX_BUILDER = TOOLS_DIR / "build_elite_qa_workbook.mjs"
PPTX_BUILDER = TOOLS_DIR / "build_elite_command_center_deck.mjs"


class ArtifactManifestConsumerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "repo"
        self.root.mkdir(parents=True)
        run = self.root / "qa_runs" / "run_current"
        run.mkdir(parents=True)
        (run / "report.ron").write_text(fixtures.modern_report(), encoding="utf-8")
        (run / "shot_0000.png").write_bytes(fixtures.PNG_1X1)
        self.manifest = manifest_builder.build_manifest(
            [run], repo_root=self.root, generated_at_utc=fixtures.FIXED_TIME
        )
        self.assertEqual(self.manifest["overall_classification"], "Observed")
        self.manifest_path = self.root / "fixtures" / "manifest.json"
        self.manifest_path.parent.mkdir()
        self.write_manifest(self.manifest)
        self.reference_image = self.root / "fixtures" / "favorite.png"
        self.reference_image.write_bytes(fixtures.PNG_1X1)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_manifest(self, value: dict) -> None:
        self.manifest_path.write_text(
            json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
            newline="\n",
        )

    def replace_manifest_with_run_count(self, run_count: int) -> None:
        runs = []
        generation = len(list((self.root / "qa_runs").glob("manifest_set_*"))) + 1
        for index in range(run_count):
            name = f"manifest_set_{generation}_run_{index + 1}"
            run = self.root / "qa_runs" / name
            run.mkdir(parents=True)
            report = fixtures.modern_report().replace(
                '    route_focus: "streaming",',
                f'    route_focus: "route_{index + 1}_' + ('x' * 32) + '",',
            )
            (run / "report.ron").write_text(report, encoding="utf-8")
            (run / "shot_0000.png").write_bytes(fixtures.PNG_1X1)
            runs.append(run)
        self.manifest = manifest_builder.build_manifest(
            runs, repo_root=self.root, generated_at_utc=fixtures.FIXED_TIME
        )
        self.write_manifest(self.manifest)

    def output(self, name: str) -> Path:
        return self.root / "artifacts" / name

    def test_current_observed_manifest_loads_and_rehashes_explicit_png(self) -> None:
        evidence = consumer.load_canonical_evidence(self.manifest_path)
        self.assertEqual(evidence.data["schema_version"], "1.0.0")
        self.assertEqual(len(evidence.runs), 1)
        screenshots = consumer.verified_screenshots(evidence, self.root)
        self.assertEqual(len(screenshots), 1)
        self.assertEqual(screenshots[0][1], "qa_runs/run_current/shot_0000.png")

    def test_consumer_rejects_stale_schema_nonobserved_and_legacy_runs(self) -> None:
        for mutation in ("schema", "classification", "legacy"):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.manifest)
                if mutation == "schema":
                    changed["schema_version"] = "0.9.0"
                elif mutation == "classification":
                    changed["overall_classification"] = "Blocked"
                else:
                    changed["runs"][0]["report_schema_variant"] = "legacy"
                self.write_manifest(changed)
                with self.assertRaises(consumer.EvidenceContractError):
                    consumer.load_canonical_evidence(self.manifest_path)
        self.write_manifest(self.manifest)

    def test_consumer_rejects_missing_quantiles_planetary_fields_and_screenshots(self) -> None:
        mutations = (
            lambda value: value["runs"][0]["raw_observations"]["route_frame_times"].pop("p95_ms"),
            lambda value: value["runs"][0]["raw_observations"]["planetary_streaming"]["telemetry"].pop("surface_material_mode"),
            lambda value: value["runs"][0]["raw_observations"]["screenshots"].update(referenced_files=[]),
        )
        for mutation in mutations:
            changed = copy.deepcopy(self.manifest)
            mutation(changed)
            self.write_manifest(changed)
            with self.assertRaises(consumer.EvidenceContractError):
                consumer.load_canonical_evidence(self.manifest_path)
        self.write_manifest(self.manifest)

    def test_consumer_rejects_png_changed_after_manifest_generation(self) -> None:
        evidence = consumer.load_canonical_evidence(self.manifest_path)
        screenshot = self.root / "qa_runs" / "run_current" / "shot_0000.png"
        screenshot.write_bytes(fixtures.PNG_1X1 + b"changed")
        with self.assertRaises(consumer.EvidenceContractError):
            consumer.verified_screenshots(evidence, self.root)

    def test_output_paths_are_explicit_nonclobbering_and_protected(self) -> None:
        safe = self.output("report.docx")
        self.assertEqual(
            consumer.validate_output_path(safe, self.root, ".docx"), safe.resolve()
        )
        safe.parent.mkdir(parents=True)
        safe.write_bytes(b"user-owned")
        with self.assertRaises(consumer.EvidenceContractError):
            consumer.validate_output_path(safe, self.root, ".docx")
        with self.assertRaises(consumer.EvidenceContractError):
            consumer.validate_output_path(
                self.root / "qa_runs" / "artifact.docx", self.root, ".docx"
            )

    def run_python_check(self, builder: Path, output: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                "-B",
                str(builder),
                "--evidence-manifest",
                str(self.manifest_path),
                "--output",
                str(output),
                "--repo-root",
                str(self.root),
                "--check-only",
            ],
            cwd=TOOLS_DIR.parents[1],
            text=True,
            capture_output=True,
            check=False,
        )

    def test_python_builders_check_same_manifest_without_creating_artifacts(self) -> None:
        for builder, filename in (
            (DOCX_BUILDER, "report.docx"),
            (PDF_BUILDER, "report.pdf"),
        ):
            with self.subTest(builder=builder.name):
                output = self.output(filename)
                first = self.run_python_check(builder, output)
                second = self.run_python_check(builder, output)
                self.assertEqual(first.returncode, 0, first.stderr)
                self.assertEqual(first.stdout, second.stdout)
                summary = json.loads(first.stdout)
                self.assertEqual(summary["manifest_sha256"], consumer.load_canonical_evidence(self.manifest_path).manifest_sha256)
                self.assertFalse(output.exists())

    def test_pdf_table_geometry_and_budget_page_plan_prevent_render_orphans(self) -> None:
        self.assertEqual(sum(pdf_builder.RUN_TABLE_WIDTHS_MM), pdf_builder.PDF_CONTENT_WIDTH_MM)
        self.assertEqual(pdf_builder.RUN_TABLE_HEADERS[-1], "PNGs")
        self.assertGreaterEqual(pdf_builder.RUN_TABLE_WIDTHS_MM[-1], 16)
        self.assertEqual(sum(pdf_builder.BUDGET_TABLE_WIDTHS_MM), pdf_builder.PDF_CONTENT_WIDTH_MM)
        self.assertEqual(
            len(pdf_builder.BUDGET_TABLE_HEADERS),
            len(pdf_builder.BUDGET_TABLE_WIDTHS_MM),
        )

        for run_count in range(1, 101):
            page_sizes = pdf_builder.plan_budget_pages(run_count)
            with self.subTest(run_count=run_count, page_sizes=page_sizes):
                self.assertEqual(sum(page_sizes), run_count)
                self.assertLessEqual(max(page_sizes), pdf_builder.BUDGET_RUNS_PER_PAGE)
                self.assertLessEqual(max(page_sizes) - min(page_sizes), 1)
                if len(page_sizes) > 1:
                    self.assertGreaterEqual(min(page_sizes), 2)

        self.assertEqual(pdf_builder.plan_budget_pages(3), (3,))
        self.assertEqual(pdf_builder.plan_budget_pages(9), (5, 4))
        self.assertEqual(pdf_builder.plan_budget_pages(17), (6, 6, 5))
        evidence = consumer.load_canonical_evidence(self.manifest_path)
        rows = pdf_builder.budget_run_rows(evidence)
        self.assertEqual(len(rows), len(evidence.runs))
        self.assertEqual(len(rows[0]), len(pdf_builder.BUDGET_TABLE_HEADERS))
        self.assertIn("live ", rows[0][5])
        self.assertIn("peak ", rows[0][5])
        source = PDF_BUILDER.read_text(encoding="utf-8")
        self.assertIn("split_by_row=False", source)
        self.assertIn("flowables.append(KeepTogether(block))", source)

    def test_pdf_claim_pages_are_balanced_and_file_kind_label_does_not_wrap(self) -> None:
        self.assertEqual(sum(pdf_builder.CLAIM_TABLE_WIDTHS_MM), pdf_builder.PDF_CONTENT_WIDTH_MM)
        self.assertEqual(pdf_builder.CLAIM_ROWS_PER_PAGE, 8)
        for claim_count in range(1, 101):
            page_sizes = pdf_builder.plan_claim_pages(claim_count)
            sentinel_rows = [
                [f"scope-{index}", "Observed", f"claim-{index}", f"evidence-{index}"]
                for index in range(claim_count)
            ]
            chunks = pdf_builder.balanced_claim_chunks(sentinel_rows)
            with self.subTest(claim_count=claim_count, page_sizes=page_sizes):
                self.assertEqual(sum(page_sizes), claim_count)
                self.assertEqual([len(chunk) for chunk in chunks], list(page_sizes))
                self.assertEqual([row for chunk in chunks for row in chunk], sentinel_rows)
                self.assertLessEqual(max(page_sizes), pdf_builder.CLAIM_ROWS_PER_PAGE)
                self.assertLessEqual(max(page_sizes) - min(page_sizes), 1)
                if len(page_sizes) > 1:
                    self.assertGreaterEqual(min(page_sizes), 2)

        self.assertEqual(pdf_builder.plan_claim_pages(9), (5, 4))
        self.assertEqual(pdf_builder.plan_claim_pages(16), (8, 8))
        self.assertEqual(pdf_builder.plan_claim_pages(17), (6, 6, 5))
        source = PDF_BUILDER.read_text(encoding="utf-8")
        self.assertIn("*claim_flowables(claims)", source)
        claim_source = inspect.getsource(pdf_builder.claim_flowables)
        self.assertIn("split_by_row=False", claim_source)
        self.assertIn("KeepTogether(block)", claim_source)

        self.assertEqual(sum(pdf_builder.FILE_IDENTITY_WIDTHS_MM), pdf_builder.PDF_CONTENT_WIDTH_MM)
        self.assertGreaterEqual(pdf_builder.FILE_IDENTITY_WIDTHS_MM[0], 30)
        label = pdf_builder.display_file_kind("generator_source")
        self.assertEqual(label.replace("\u00a0", " "), "generator source")
        self.assertIn("\u00a0", label)
        evidence = consumer.load_canonical_evidence(self.manifest_path)
        semantic_kinds = [record["kind"] for record in evidence.data["file_hashes"]]
        display_kinds = [row[0] for row in pdf_builder.file_identity_rows(evidence)]
        self.assertIn("generator_source", semantic_kinds)
        self.assertIn(pdf_builder.GENERATOR_SOURCE_DISPLAY_LABEL, display_kinds)
        claims = list(consumer.iter_claims(evidence))
        rows = pdf_builder.claim_rows(claims)
        self.assertEqual(len(rows), len(claims))
        for row, (scope, claim) in zip(rows, claims):
            self.assertEqual(
                row,
                [
                    scope,
                    claim["classification"],
                    claim["statement"],
                    "\n".join(claim["evidence"]) or "None",
                ],
            )
        self.assertEqual(
            [kind for kind in semantic_kinds if kind != "generator_source"],
            [kind for kind in display_kinds if kind != pdf_builder.GENERATOR_SOURCE_DISPLAY_LABEL],
        )
        for row, record in zip(pdf_builder.file_identity_rows(evidence), evidence.data["file_hashes"]):
            self.assertEqual(row[1], record["path"])
            self.assertEqual(row[2], pdf_builder.fmt_number(record["size_bytes"], 0))
            self.assertEqual(row[3], pdf_builder.display_hash(record["sha256"]))

    def test_docx_callout_and_identity_pages_are_structurally_indivisible(self) -> None:
        callout_source = inspect.getsource(docx_builder.add_callout)
        self.assertIn("paragraph.paragraph_format.keep_together = True", callout_source)
        self.assertIn("keep_row_together(table.rows[0])", callout_source)

        self.assertEqual(sum(docx_builder.FILE_IDENTITY_WIDTHS_DXA), docx_builder.PAGE_WIDTH_DXA)
        for row_count in range(1, 101):
            page_sizes = docx_builder.plan_balanced_table_pages(row_count)
            sentinel_rows = [[index, f"path-{index}", index * 10, f"hash-{index}"] for index in range(row_count)]
            chunks = docx_builder.balanced_row_chunks(sentinel_rows)
            with self.subTest(row_count=row_count, page_sizes=page_sizes):
                self.assertEqual(sum(page_sizes), row_count)
                self.assertEqual([len(chunk) for chunk in chunks], list(page_sizes))
                self.assertEqual([row for chunk in chunks for row in chunk], sentinel_rows)
                self.assertLessEqual(max(page_sizes), docx_builder.FILE_IDENTITY_ROWS_PER_PAGE)
                self.assertLessEqual(max(page_sizes) - min(page_sizes), 1)
                if len(page_sizes) > 1:
                    self.assertGreaterEqual(min(page_sizes), 2)

        self.assertEqual(docx_builder.plan_balanced_table_pages(3), (3,))
        self.assertEqual(docx_builder.plan_balanced_table_pages(9), (5, 4))
        self.assertEqual(docx_builder.plan_balanced_table_pages(17), (6, 6, 5))
        evidence = consumer.load_canonical_evidence(self.manifest_path)
        rows = docx_builder.file_identity_rows(evidence)
        self.assertEqual(len(rows), len(evidence.data["file_hashes"]))
        for row, record in zip(rows, evidence.data["file_hashes"]):
            self.assertEqual(row[0], record["kind"])
            self.assertEqual(row[1], record["path"])
            self.assertEqual(row[2], docx_builder.fmt_number(record["size_bytes"], 0))
            self.assertEqual(row[3], docx_builder.display_hash(record["sha256"]))

        identity_source = inspect.getsource(docx_builder.add_file_identity_pages)
        self.assertIn("heading.paragraph_format.page_break_before = True", identity_source)
        self.assertIn("keep_as_block=True", identity_source)
        matrix_source = inspect.getsource(docx_builder.add_matrix)
        self.assertIn("keep_row_together(table.rows[0], repeat_header=True)", matrix_source)
        self.assertIn("keep_table_as_block(table)", matrix_source)

    @unittest.skipUnless(shutil.which("node"), "Node.js is unavailable")
    def test_workbook_builder_check_only_is_dependency_free_and_no_output(self) -> None:
        output = self.output("report.xlsx")
        command = [
            shutil.which("node") or "node",
            str(XLSX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--check-only",
        ]
        first = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        second = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, second.stdout)
        summary = json.loads(first.stdout)
        self.assertEqual(summary["schema_version"], "1.0.0")
        self.assertFalse(output.exists())

    @unittest.skipUnless(shutil.which("node"), "Node.js is unavailable")
    def test_presentation_builder_check_only_is_dependency_free_deterministic_and_no_output(self) -> None:
        output = self.output("command-center.pptx")
        command = [
            shutil.which("node") or "node",
            str(PPTX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--reference-image",
            str(self.reference_image),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--check-only",
        ]
        first = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        second = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(first.stdout, second.stdout)
        summary = json.loads(first.stdout)
        self.assertEqual(summary["artifact_kind"], "pptx")
        self.assertEqual(summary["slide_size"], {"width": 1280, "height": 720})
        self.assertEqual(summary["slide_count"], 7)
        self.assertEqual(
            summary["slide_ids"],
            [
                "01-opening",
                "02-overview",
                "03-architecture",
                "04-evidence",
                "05-performance",
                "06-limits",
                "07-next-slice",
            ],
        )
        self.assertEqual(summary["reference_sha256"], hashlib.sha256(fixtures.PNG_1X1).hexdigest())
        self.assertEqual(summary["visual_acceptance"], "not_recorded")
        self.assertFalse(output.exists())
        real_without_qa = subprocess.run(
            command[:-1],
            cwd=TOOLS_DIR.parents[1],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(real_without_qa.returncode, 2)
        self.assertIn("--qa-dir is required for a real deck build", real_without_qa.stderr)
        self.assertFalse(output.exists())

    def test_presentation_builder_has_manifest_bound_structure_sources_and_alt_text(self) -> None:
        text = PPTX_BUILDER.read_text(encoding="utf-8")
        required = (
            'slideSize: { width: 1280, height: 720 }',
            'title: "Voxel-Native Evidence Command Center"',
            'title: "One manifest. Bounded truth."',
            'title: "Evidence architecture, no hidden selection"',
            'title: "What current evidence actually shows"',
            'title: "Observed route frame-time distribution"',
            'title: "Current limits are part of the evidence"',
            'title: "Hydro v1 evidence boundary"',
            '"IMPLEMENTED / RENDER-ONLY V1"',
            '"Implemented render-only v1. Hydro-current telemetry is recorded.',
            'Human same-binary visual acceptance is pending.',
            'Implementation source: src/planetary_streaming.rs',
            'Human visual review and formal visual acceptance remain pending.',
            'slide.charts.add("bar"',
            'slide.speakerNotes.textFrame.setText',
            '"[Sources]"',
            'alt: "User-provided visual direction:',
            'visual_acceptance: "not_recorded"',
            'await verifiedScreenshots(evidence, repoRoot, MAX_DECK_SCREENSHOTS)',
            'await publishNoClobber(temporary, output)',
        )
        for token in required:
            with self.subTest(token=token):
                self.assertIn(token, text)
        self.assertNotIn("VISUALLY ACCEPTED", text)
        self.assertNotRegex(text, r"\b\d+\s+tests?\s+(passed|green)\b")
        for stale in (
            "behind a disabled gate",
            "Candidate broad-hydrographic-continuity direction",
            "no implementation or result claimed",
            "It must remain disabled",
            "Wire Hydro telemetry",
            "A Hydro-current manifest with QA/report/manifest telemetry and same-binary captures is still required.",
        ):
            with self.subTest(stale=stale):
                self.assertNotIn(stale, text)
        font_sizes = [int(value) for value in re.findall(r"fontSize:\s*(\d+)", text)]
        self.assertTrue(font_sizes)
        self.assertGreaterEqual(min(font_sizes), 22)
        self.assertIn("const MAX_DECK_RUNS = 4;", text)
        self.assertIn("const TYPE_PX = Object.freeze({ body: 22, mid: 32, slideTitle: 48, deckTitle: 68 });", text)
        self.assertIn('autoFit: style.autoFit ?? "none"', text)
        self.assertIn('selected.length === evidence.data.runs.length', text)
        self.assertNotIn('name: `orbit-${slideNumber}`', text)
        self.assertNotIn('name: `orbit-inner-${slideNumber}`', text)

    @unittest.skipUnless(shutil.which("node"), "Node.js is unavailable")
    def test_presentation_layout_contract_pins_nonoverlapping_text_intervals(self) -> None:
        module_uri = PPTX_BUILDER.resolve().as_uri()
        command = [
            shutil.which("node") or "node",
            "--input-type=module",
            "-e",
            f'import({json.dumps(module_uri)}).then(m => process.stdout.write(JSON.stringify(m.LAYOUT_CONTRACT)))',
        ]
        result = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        layout = json.loads(result.stdout)
        self.assertEqual(
            layout,
            {
                "opening": {
                    "title": {"top": 132, "height": 252},
                    "subtitle": {"top": 410, "height": 116},
                },
                "architectureCard": {
                    "top": 236,
                    "height": 252,
                    "titleOffsetTop": 18,
                    "titleHeight": 78,
                    "bodyOffsetTop": 112,
                    "bodyBottomInset": 20,
                },
                "performanceSamples": {
                    "label": {"top": 238, "height": 84},
                    "value": {"top": 336, "height": 58},
                },
            },
        )
        opening = layout["opening"]
        self.assertLessEqual(
            opening["title"]["top"] + opening["title"]["height"] + 20,
            opening["subtitle"]["top"],
        )
        card = layout["architectureCard"]
        self.assertLessEqual(
            card["titleOffsetTop"] + card["titleHeight"] + 16,
            card["bodyOffsetTop"],
        )
        self.assertLess(
            card["bodyOffsetTop"],
            card["height"] - card["bodyBottomInset"],
        )
        performance = layout["performanceSamples"]
        self.assertLessEqual(
            performance["label"]["top"] + performance["label"]["height"] + 14,
            performance["value"]["top"],
        )
        source = PPTX_BUILDER.read_text(encoding="utf-8")
        for token in (
            '"Evidence\\nCommand\\nCenter"',
            '"03 · STRICT\\nCONSUMER"',
            '"04 · ARTIFACT\\nLANES"',
            "LAYOUT_CONTRACT.opening.title.top",
            "LAYOUT_CONTRACT.architectureCard.bodyOffsetTop",
            "LAYOUT_CONTRACT.performanceSamples.value.top",
        ):
            self.assertIn(token, source)

    @unittest.skipUnless(shutil.which("node"), "Node.js is unavailable")
    def test_presentation_builder_rejects_clobber_and_protected_qa_without_loading_runtime(self) -> None:
        output = self.output("command-center.pptx")
        output.parent.mkdir(parents=True)
        output.write_bytes(b"user-owned")
        base = [
            shutil.which("node") or "node",
            str(PPTX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--reference-image",
            str(self.reference_image),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--check-only",
        ]
        clobber = subprocess.run(base, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(clobber.returncode, 2)
        self.assertEqual(output.read_bytes(), b"user-owned")

        fresh_output = self.output("fresh-command-center.pptx")
        protected = base.copy()
        protected[protected.index(str(output))] = str(fresh_output)
        protected[-1:-1] = ["--qa-dir", str(self.root / "qa_runs" / "deck-qa")]
        rejected = subprocess.run(protected, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("protected directory 'qa_runs'", rejected.stderr)
        self.assertFalse(fresh_output.exists())

    @unittest.skipUnless(shutil.which("node"), "Node.js is unavailable")
    def test_presentation_builder_accepts_four_runs_and_rejects_five_without_artifact_runtime(self) -> None:
        self.replace_manifest_with_run_count(4)
        output = self.output("four-runs.pptx")
        base = [
            shutil.which("node") or "node",
            str(PPTX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--reference-image",
            str(self.reference_image),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--check-only",
        ]
        accepted = subprocess.run(base, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        self.assertEqual(json.loads(accepted.stdout)["run_count"], 4)
        self.assertFalse(output.exists())

        self.replace_manifest_with_run_count(5)
        rejected = subprocess.run(base, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("deck run count exceeds the fixed cap of 4", rejected.stderr)
        self.assertFalse(output.exists())

    @unittest.skipUnless(
        shutil.which("node") and hasattr(os, "symlink"),
        "Node.js or symlink support is unavailable",
    )
    def test_presentation_builder_rejects_qa_directory_through_symlink(self) -> None:
        link = self.root / "outside-looking-link"
        try:
            os.symlink(self.root / "qa_runs", link, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"directory symlink unavailable: {error}")
        output = self.output("symlink-check.pptx")
        command = [
            shutil.which("node") or "node",
            str(PPTX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--reference-image",
            str(self.reference_image),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--qa-dir",
            str(link / "deck-qa"),
            "--check-only",
        ]
        rejected = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("protected directory 'qa_runs'", rejected.stderr)
        self.assertFalse(output.exists())

    @unittest.skipUnless(
        shutil.which("node") and hasattr(os, "symlink"),
        "Node.js or symlink support is unavailable",
    )
    def test_presentation_builder_rejects_output_through_symlink(self) -> None:
        link = self.root / "outside-looking-output-link"
        try:
            os.symlink(self.root / "qa_runs", link, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"directory symlink unavailable: {error}")
        output = link / "deck.pptx"
        command = [
            shutil.which("node") or "node",
            str(PPTX_BUILDER),
            "--evidence-manifest",
            str(self.manifest_path),
            "--reference-image",
            str(self.reference_image),
            "--output",
            str(output),
            "--repo-root",
            str(self.root),
            "--check-only",
        ]
        rejected = subprocess.run(command, cwd=TOOLS_DIR.parents[1], text=True, capture_output=True, check=False)
        self.assertEqual(rejected.returncode, 2)
        self.assertIn("protected directory 'qa_runs'", rejected.stderr)
        self.assertFalse(output.exists())

    def test_builders_contain_no_legacy_run_selection_or_result_defaults(self) -> None:
        forbidden = (
            "--run-dir",
            "status.ron",
            "2026-08-09",
            "--tests",
            ".glob(",
        )
        for builder in (DOCX_BUILDER, PDF_BUILDER, XLSX_BUILDER, PPTX_BUILDER):
            text = builder.read_text(encoding="utf-8")
            for token in forbidden:
                with self.subTest(builder=builder.name, token=token):
                    self.assertNotIn(token, text)


if __name__ == "__main__":
    unittest.main()
