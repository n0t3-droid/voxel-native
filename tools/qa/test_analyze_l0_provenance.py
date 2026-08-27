from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import numpy as np
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_l0_provenance as analyzer  # noqa: E402


def _ron(value: object, indent: int = 0) -> str:
    padding = " " * indent
    child = indent + 4
    child_padding = " " * child
    if isinstance(value, dict):
        if not value:
            return "()"
        fields = [
            f"{child_padding}{name}: {_ron(item, child)},"
            for name, item in value.items()
        ]
        return "(\n" + "\n".join(fields) + f"\n{padding})"
    if isinstance(value, list):
        if not value:
            return "[]"
        items = [f"{child_padding}{_ron(item, child)}," for item in value]
        return "[\n" + "\n".join(items) + f"\n{padding}]"
    if value is None:
        return "None"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, (int, float)):
        return repr(value)
    raise TypeError(type(value))


class FourArmFixture:
    def __init__(self, root: Path, capture_count: int = 1) -> None:
        self.root = root
        self.root.mkdir(parents=True, exist_ok=True)
        self.capture_count = capture_count
        self.paths: dict[str, Path] = {}
        self.reports: dict[str, dict[str, object]] = {}
        for spec in analyzer.ARM_SPECS:
            run_dir = root / spec.key
            run_dir.mkdir()
            self.paths[spec.key] = run_dir
            self.reports[spec.key] = self._report(spec, run_dir)
            for index in range(capture_count):
                wall_pixels = 8 if spec.mode == analyzer.POINT_MODE else 3
                self.write_image(spec.key, index, wall_pixels)
            self.write_report(spec.key)

    def _report(
        self, spec: analyzer.ArmSpec, run_dir: Path
    ) -> dict[str, object]:
        observations: list[dict[str, object]] = []
        screenshots: list[str] = []
        for index in range(self.capture_count):
            path = (
                f"{run_dir.parent.name}\\{run_dir.name}\\"
                f"shot_{index:04}_context.png"
            )
            screenshots.append(path)
            observations.append(
                {
                    "capture_index": index,
                    "screenshot_path": path,
                    "scheduled_capture_seconds": 2.5 + index * 2.0,
                    "player_camera_translation_metres": [
                        100.0 + index,
                        50.0,
                        -25.0,
                    ],
                    "player_camera_rotation_xyzw": [0.0, 0.0, 0.0, 1.0],
                }
            )
        plan_hash = "1" * 16 if spec.world_profile == "Natural" else "2" * 16
        mode_cap = 228_822 if spec.mode == analyzer.POINT_MODE else 263_142
        streaming: dict[str, object] = {
            "enabled": True,
            "profile": spec.world_profile,
            "desired_terrain_grammar": "V3",
            "active_terrain_grammar": "V3",
            "desired_l0_height_mode": spec.mode,
            "active_l0_height_mode": spec.mode,
            "resident_l0_height_mode": spec.mode,
            "l0_probe_spacing_metres": 8,
            "budget_l0_height_queries": 12_805,
            "resident_entities": 6,
            "resident_vertices": 9,
            "resident_indices": 36,
            "ring_vertices": [4, 1, 1, 1, 1, 1],
            "ring_indices": [6, 6, 6, 6, 6, 6],
            "resident_mesh_bytes": 1_000,
            "resident_fluid_entities": 0,
            "resident_fluid_vertices": 0,
            "resident_fluid_indices": 0,
            "resident_fluid_mesh_bytes": 0,
            "resident_semantic_cohort_entities": 0,
            "resident_semantic_cohort_vertices": 0,
            "resident_semantic_cohort_indices": 0,
            "resident_semantic_cohort_mesh_bytes": 0,
            "resident_semantic_cohort_count": 0,
            "scheduler_resident_entities": 6,
            "scheduler_resident_vertices": 9,
            "scheduler_resident_indices": 36,
            "scheduler_ring_vertices": [4, 1, 1, 1, 1, 1],
            "scheduler_ring_indices": [6, 6, 6, 6, 6, 6],
            "scheduler_resident_mesh_bytes": 1_000,
            "scheduler_resident_fluid_entities": 0,
            "scheduler_resident_fluid_vertices": 0,
            "scheduler_resident_fluid_indices": 0,
            "scheduler_resident_fluid_mesh_bytes": 0,
            "scheduler_resident_semantic_cohort_entities": 0,
            "scheduler_resident_semantic_cohort_vertices": 0,
            "scheduler_resident_semantic_cohort_indices": 0,
            "scheduler_resident_semantic_cohort_mesh_bytes": 0,
            "scheduler_resident_semantic_cohort_count": 0,
            "resident_observation_valid": True,
            "resident_entity_count_overflow": False,
            "resident_duplicate_levels": 0,
            "resident_out_of_range_levels": 0,
            "resident_scheduler_mismatch": False,
            "resident_budget_exceeded": False,
            "resident_observation_rejections": 0,
            "resident_fluid_observation_valid": True,
            "resident_fluid_entity_count_overflow": False,
            "resident_fluid_duplicate_slots": 0,
            "resident_fluid_out_of_range_levels": 0,
            "resident_fluid_scheduler_mismatch": False,
            "resident_fluid_budget_exceeded": False,
            "resident_fluid_kind_integrity_valid": True,
            "resident_fluid_observation_rejections": 0,
            "resident_semantic_cohort_observation_valid": True,
            "resident_semantic_cohort_entity_count_overflow": False,
            "resident_semantic_cohort_scheduler_mismatch": False,
            "resident_semantic_cohort_budget_exceeded": False,
            "resident_semantic_cohort_payload_integrity_valid": True,
            "resident_semantic_cohort_observation_rejections": 0,
            "live_sample_cache_windows": 6,
            "live_sample_cache_bytes": mode_cap,
            "peak_live_sample_cache_windows": 6,
            "peak_live_sample_cache_bytes": mode_cap,
            "budget_entities": 6,
            "budget_vertices": 35_000,
            "budget_indices": 150_000,
            "budget_mesh_bytes": 2_280_000,
            "budget_sample_cache_bytes": 524_288,
            "pending_rebuilds": 0,
            "dirty_mask": 0,
            "build_in_flight": False,
            "surface_material_mode": "LodProvenanceV1",
            "hydro_mode": "Disabled",
            "semantic_cohort_mode": "Disabled",
            "budget_rejections": 0,
            "last_l0_center_queries": 0,
            "last_l0_half_x_queries": 0,
            "last_l0_half_z_queries": 0,
            "last_l0_cache_update": "IncrementalStrip",
            "last_l0_cache_shift_x_cells": 0,
            "last_l0_cache_shift_z_cells": 0,
            "last_l0_reused_height_samples": (
                4_225 if spec.mode == analyzer.POINT_MODE else 12_805
            ),
            "last_l0_trimmed_vertices": 0,
            "last_l0_trimmed_up_vertices": 0,
            "last_l0_trimmed_down_vertices": 0,
            "last_l0_max_abs_adjustment_metres": 0.0,
        }
        return {
            "qa_report_schema_version": spec.schema,
            "evidence_disposition": spec.disposition,
            "run_identity": {
                "package_version": "0.1.0",
                "build_profile": "release",
                "instance_label": f"fixture-{spec.key}",
                "world_name": f"fixture-{spec.key}",
                "world_seed": 12_345,
                "world_profile": spec.world_profile,
                "scenery_quality": "Lush",
                "terrain_grammar": "V3",
                "git_sha": "a" * 40,
                "git_dirty": True,
                "source_fingerprint": "sha256:" + "b" * 64,
                "executable_hash": "sha256:" + "c" * 64,
                "toolchain": "rustc fixture; host: x86_64-pc-windows-msvc",
                "hardware": "fixture hardware",
            },
            "world_edit_store_status": "compatible",
            "world_edit_store_compatible": True,
            "world_edit_store_seed": 12_345,
            "world_edit_store_profile": spec.world_profile,
            "world_edit_store_scenery_quality": "Lush",
            "world_edit_store_terrain_grammar": "V3",
            "world_edit_store_edited_chunks": 0,
            "world_edit_store_block_reason_code": None,
            "viewport": {
                "logical_width": 8.0,
                "logical_height": 8.0,
                "physical_width": 8,
                "physical_height": 8,
                "scale_factor": 1.0,
                "base_scale_factor": 1.0,
                "dpi_percent": 100.0,
            },
            "planetary_streaming": streaming,
            "requested_route_focus": spec.route_focus,
            "resolved_route_focus": spec.route_focus,
            "route_focus_available": True,
            "route_focus_unavailable_reason": None,
            "route_focus_search_cap_exhausted": False,
            "camera_route_preflight_applicable": True,
            "camera_route_policy": "preflight-v1",
            "camera_route_plan_hash": plan_hash,
            "camera_route_available": True,
            "camera_route_unavailable_reason": None,
            "camera_route_variant_index": 0,
            "camera_route_variant_count": 8,
            "camera_route_validation_samples": 16,
            "camera_route_selected_clear_samples": 16,
            "camera_route_voxel_queries": 100,
            "camera_route_voxel_query_cap": 153_600,
            "camera_route_required_chunk_checks": 100,
            "camera_route_loaded_chunk_checks": 90,
            "camera_route_proven_air_chunk_checks": 10,
            "camera_route_unloaded_chunk_checks": 0,
            "camera_route_candidate_body_occlusions": 0,
            "camera_route_candidate_los_occlusions": 0,
            "camera_route_minimum_clearance_voxels": 1,
            "camera_route_work_cap_exhausted": False,
            "requested_duration_seconds": 10.0,
            "duration_seconds": 10.0,
            "loaded_chunks": 1,
            "mesh_entities": 1,
            "pending_terrain": 0,
            "pending_meshes": 0,
            "dirty_chunks": 0,
            "screenshots": screenshots,
            "screenshot_observation_cap": 600,
            "screenshot_path_max_chars": 512,
            "screenshot_observation_count": self.capture_count,
            "screenshot_observation_valid": True,
            "screenshot_observation_cap_exhausted": False,
            "screenshot_observation_rejections": 0,
            "screenshot_observations": observations,
        }

    def write_report(self, key: str) -> None:
        (self.paths[key] / "report.ron").write_text(
            _ron(self.reports[key]) + "\n", encoding="utf-8"
        )

    def write_image(
        self, key: str, index: int, wall_pixels: int, size: tuple[int, int] = (8, 8)
    ) -> None:
        width, height = size
        pixels = np.zeros((height, width, 3), dtype=np.uint8)
        for offset in range(wall_pixels):
            row, column = divmod(offset, width)
            pixels[row, column] = [255, 0, 0]
        Image.fromarray(pixels, mode="RGB").save(
            self.paths[key] / f"shot_{index:04}_context.png", format="PNG"
        )

    def analyze(self) -> dict[str, object]:
        return analyzer.analyze(
            self.paths["natural_point"],
            self.paths["natural_cardinal"],
            self.paths["astral_point"],
            self.paths["astral_cardinal"],
        )


class AnalyzeL0ProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.viewport_patch = mock.patch.object(analyzer, "EXPECTED_VIEWPORT", (8, 8))
        self.viewport_patch.start()
        self.addCleanup(self.viewport_patch.stop)
        self.fixture = FourArmFixture(Path(self.temporary.name) / "qa_runs")

    def test_valid_four_arm_evidence_passes_pending_human_inspection(self) -> None:
        # Occlusion counters cover rejected route variants.  The selected
        # variant is still valid when the final clear-sample count is exact.
        self.fixture.reports["natural_point"][
            "camera_route_candidate_body_occlusions"
        ] = 1
        self.fixture.reports["natural_point"][
            "camera_route_candidate_los_occlusions"
        ] = 5
        self.fixture.write_report("natural_point")
        ledger = self.fixture.analyze()
        self.assertEqual(
            ledger["automated_decision"], "pass-pending-mandatory-human-inspection"
        )
        self.assertFalse(ledger["canonical_publishable"])
        frame = ledger["profiles"]["Natural"]["frames"][0]
        self.assertEqual(frame["point"]["wall_pixels"], 8)
        self.assertEqual(frame["cardinal_trimmed"]["wall_pixels"], 3)
        self.assertAlmostEqual(frame["wall_pixel_reduction_fraction"], 0.625)
        self.assertEqual(
            frame["point"]["camera_pose"]["translation_metres"],
            [100.0, 50.0, -25.0],
        )
        self.assertRegex(
            ledger["profiles"]["Natural"]["pair_identity"][
                "point_report_sha256"
            ],
            r"^[0-9a-f]{64}$",
        )
        self.assertTrue(frame["stop_test_1_half_baseline_pass"])
        self.assertTrue(frame["stop_test_2_absolute_five_percent_pass"])

    def test_stop_test_one_rejects_candidate_above_half_baseline(self) -> None:
        for key in ("natural_point", "astral_point"):
            self.fixture.write_image(key, 0, 4)
        ledger = self.fixture.analyze()
        self.assertEqual(ledger["automated_decision"], "reject")
        self.assertFalse(
            ledger["stop_tests"]["1_every_candidate_frame_at_most_half_baseline"]
        )
        self.assertTrue(
            ledger["stop_tests"]["2_every_candidate_frame_at_most_five_percent_viewport"]
        )

    def test_stop_test_two_rejects_candidate_above_five_percent(self) -> None:
        for key in ("natural_cardinal", "astral_cardinal"):
            self.fixture.write_image(key, 0, 4)
        ledger = self.fixture.analyze()
        self.assertEqual(ledger["automated_decision"], "reject")
        self.assertTrue(
            ledger["stop_tests"]["1_every_candidate_frame_at_most_half_baseline"]
        )
        self.assertFalse(
            ledger["stop_tests"]["2_every_candidate_frame_at_most_five_percent_viewport"]
        )

    def test_zero_baseline_requires_and_accepts_zero_candidate(self) -> None:
        for key in self.fixture.paths:
            self.fixture.write_image(key, 0, 0)
        ledger = self.fixture.analyze()
        frame = ledger["profiles"]["Natural"]["frames"][0]
        self.assertEqual(
            ledger["automated_decision"], "pass-pending-mandatory-human-inspection"
        )
        self.assertTrue(frame["zero_baseline_rule_applied"])
        self.assertIsNone(frame["candidate_to_baseline_ratio"])
        self.assertIsNone(frame["wall_pixel_reduction_fraction"])

    def test_zero_baseline_rejects_nonzero_candidate_without_division(self) -> None:
        for key in ("natural_point", "astral_point"):
            self.fixture.write_image(key, 0, 0)
        for key in ("natural_cardinal", "astral_cardinal"):
            self.fixture.write_image(key, 0, 1)
        ledger = self.fixture.analyze()
        self.assertEqual(ledger["automated_decision"], "reject")
        self.assertFalse(
            ledger["stop_tests"]["1_every_candidate_frame_at_most_half_baseline"]
        )

    def test_unsupported_diagnostic_schema_fails_closed(self) -> None:
        report = self.fixture.reports["natural_cardinal"]
        report["qa_report_schema_version"] = (
            "2.4.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1"
        )
        self.fixture.write_report("natural_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "schema_version"):
            self.fixture.analyze()

    def test_homogeneous_historical_25_cohort_uses_old_dpi_contract(self) -> None:
        for spec in analyzer.ARM_SPECS:
            report = self.fixture.reports[spec.key]
            report["qa_report_schema_version"] = (
                analyzer.LEGACY_POINT_SCHEMA
                if spec.mode == analyzer.POINT_MODE
                else analyzer.LEGACY_CANDIDATE_SCHEMA
            )
            report["viewport"].pop("base_scale_factor")
            self.fixture.write_report(spec.key)

        ledger = self.fixture.analyze()
        self.assertEqual(
            {
                run["qa_report_schema_version"]
                for run in ledger["runs"].values()
            },
            {analyzer.LEGACY_POINT_SCHEMA, analyzer.LEGACY_CANDIDATE_SCHEMA},
        )
        self.assertEqual(
            ledger["automated_decision"],
            "pass-pending-mandatory-human-inspection",
        )

    def test_mixed_diagnostic_schema_generations_fail_closed(self) -> None:
        report = self.fixture.reports["natural_cardinal"]
        report["qa_report_schema_version"] = analyzer.LEGACY_CANDIDATE_SCHEMA
        report["viewport"].pop("base_scale_factor")
        self.fixture.write_report("natural_cardinal")

        with self.assertRaisesRegex(analyzer.AnalysisError, "schema generations"):
            self.fixture.analyze()

    def test_diagnostic_viewport_requires_base_scale_factor(self) -> None:
        report = self.fixture.reports["natural_cardinal"]
        report["viewport"].pop("base_scale_factor")
        self.fixture.write_report("natural_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "base_scale_factor"):
            self.fixture.analyze()

    def test_diagnostic_viewport_accepts_exact_pixel_override_with_os_dpi(self) -> None:
        for report in self.fixture.reports.values():
            report["viewport"]["base_scale_factor"] = 2.0
            report["viewport"]["dpi_percent"] = 200.0
        for key in self.fixture.reports:
            self.fixture.write_report(key)

        ledger = self.fixture.analyze()
        self.assertEqual(
            {
                run["qa_report_schema_version"]
                for run in ledger["runs"].values()
            },
            {analyzer.POINT_SCHEMA, analyzer.CANDIDATE_SCHEMA},
        )
        self.assertEqual(
            ledger["automated_decision"],
            "pass-pending-mandatory-human-inspection",
        )

    def test_source_fingerprint_mismatch_fails_closed(self) -> None:
        identity = self.fixture.reports["astral_cardinal"]["run_identity"]
        identity["source_fingerprint"] = "sha256:" + "d" * 64
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "source_fingerprint"):
            self.fixture.analyze()

    def test_cache_accounting_requires_the_x64_windows_host_contract(self) -> None:
        identity = self.fixture.reports["natural_point"]["run_identity"]
        identity["toolchain"] = "rustc fixture; host: aarch64-pc-windows-msvc"
        self.fixture.write_report("natural_point")
        with self.assertRaisesRegex(analyzer.AnalysisError, "cache-accounting host contract"):
            self.fixture.analyze()

    def test_duplicate_capture_index_fails_closed(self) -> None:
        report = self.fixture.reports["natural_point"]
        observation = copy.deepcopy(report["screenshot_observations"][0])
        observation["screenshot_path"] = (
            f"qa_runs\\natural_point\\shot_0001_context.png"
        )
        report["screenshot_observations"].append(observation)
        report["screenshots"].append(observation["screenshot_path"])
        report["screenshot_observation_count"] = 2
        self.fixture.write_report("natural_point")
        with self.assertRaisesRegex(analyzer.AnalysisError, "duplicate, missing, or unordered"):
            self.fixture.analyze()

    def test_camera_pose_mismatch_beyond_tight_tolerance_fails_closed(self) -> None:
        observation = self.fixture.reports["natural_cardinal"][
            "screenshot_observations"
        ][0]
        observation["player_camera_translation_metres"][0] += 0.51
        self.fixture.write_report("natural_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "camera position delta"):
            self.fixture.analyze()

    def test_screenshot_path_escape_fails_closed(self) -> None:
        report = self.fixture.reports["natural_point"]
        escaped = "..\\shot_0000_context.png"
        report["screenshots"][0] = escaped
        report["screenshot_observations"][0]["screenshot_path"] = escaped
        self.fixture.write_report("natural_point")
        with self.assertRaisesRegex(analyzer.AnalysisError, "canonical three-component"):
            self.fixture.analyze()

    def test_absolute_rooted_drive_unc_and_extra_prefix_paths_fail_closed(self) -> None:
        report = self.fixture.reports["natural_point"]
        observation = report["screenshot_observations"][0]
        canonical = observation["screenshot_path"]
        attacks = (
            (
                "posix absolute",
                "/qa_runs/natural_point/shot_0000_context.png",
                "absolute, rooted, drive-qualified, or UNC",
            ),
            (
                "windows drive",
                "C:\\qa_runs\\natural_point\\shot_0000_context.png",
                "absolute, rooted, drive-qualified, or UNC",
            ),
            (
                "UNC",
                "\\\\server\\share\\qa_runs\\natural_point\\shot_0000_context.png",
                "absolute, rooted, drive-qualified, or UNC",
            ),
            (
                "arbitrary prefix",
                "evil\\qa_runs\\natural_point\\shot_0000_context.png",
                "canonical three-component",
            ),
        )
        for label, attack, error_pattern in attacks:
            with self.subTest(label=label):
                report["screenshots"][0] = attack
                observation["screenshot_path"] = attack
                self.fixture.write_report("natural_point")
                with self.assertRaisesRegex(analyzer.AnalysisError, error_pattern):
                    self.fixture.analyze()
                report["screenshots"][0] = canonical
                observation["screenshot_path"] = canonical

    def test_unbound_png_and_dimension_mismatch_fail_closed(self) -> None:
        self.fixture.write_image("natural_point", 1, 0)
        with self.assertRaisesRegex(analyzer.AnalysisError, "PNG set disagrees"):
            self.fixture.analyze()
        (self.fixture.paths["natural_point"] / "shot_0001_context.png").unlink()
        self.fixture.write_image("natural_point", 0, 3, size=(7, 8))
        with self.assertRaisesRegex(analyzer.AnalysisError, "expected 8x8"):
            self.fixture.analyze()

    def test_topology_and_cache_budget_mismatches_fail_closed(self) -> None:
        streaming = self.fixture.reports["astral_cardinal"]["planetary_streaming"]
        streaming["ring_vertices"] = [3, 2, 1, 1, 1, 1]
        streaming["scheduler_ring_vertices"] = [3, 2, 1, 1, 1, 1]
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "topology differs"):
            self.fixture.analyze()

        streaming["ring_vertices"] = [4, 1, 1, 1, 1, 1]
        streaming["scheduler_ring_vertices"] = [4, 1, 1, 1, 1, 1]
        streaming["peak_live_sample_cache_bytes"] = 263_143
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "must equal the settled"):
            self.fixture.analyze()

    def test_settled_cache_bytes_cannot_be_underreported(self) -> None:
        streaming = self.fixture.reports["natural_point"]["planetary_streaming"]
        streaming["live_sample_cache_bytes"] = 228_821
        streaming["peak_live_sample_cache_bytes"] = 228_821
        self.fixture.write_report("natural_point")
        with self.assertRaisesRegex(analyzer.AnalysisError, "settled six-window"):
            self.fixture.analyze()

    def test_query_planes_cannot_redistribute_the_shared_budget(self) -> None:
        streaming = self.fixture.reports["natural_cardinal"]["planetary_streaming"]
        redistributions = (
            ((4_226, 4_289, 4_290), "4225-query plane cap"),
            ((4_225, 4_291, 4_289), "4290-query plane cap"),
            ((4_225, 4_289, 4_291), "4290-query plane cap"),
        )
        for (center, half_x, half_z), error_pattern in redistributions:
            with self.subTest(center=center, half_x=half_x, half_z=half_z):
                # Every case preserves the shared 12,805-query total.  The
                # individual plane ceiling is therefore the decisive check.
                streaming["last_l0_center_queries"] = center
                streaming["last_l0_half_x_queries"] = half_x
                streaming["last_l0_half_z_queries"] = half_z
                self.fixture.write_report("natural_cardinal")
                with self.assertRaisesRegex(analyzer.AnalysisError, error_pattern):
                    self.fixture.analyze()

    def test_excessive_trim_and_effect_inconsistency_fail_closed(self) -> None:
        streaming = self.fixture.reports["astral_cardinal"]["planetary_streaming"]
        streaming["last_l0_trimmed_vertices"] = 3_722
        streaming["last_l0_trimmed_up_vertices"] = 3_722
        streaming["last_l0_max_abs_adjustment_metres"] = 1.0
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "3721-vertex lattice cap"):
            self.fixture.analyze()

        streaming["last_l0_trimmed_vertices"] = 1
        streaming["last_l0_trimmed_up_vertices"] = 1
        streaming["last_l0_max_abs_adjustment_metres"] = 0.0
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "adjustment magnitude is inconsistent"):
            self.fixture.analyze()

    def test_candidate_query_planes_must_share_zero_nonzero_state(self) -> None:
        streaming = self.fixture.reports["astral_cardinal"]["planetary_streaming"]
        streaming["last_l0_center_queries"] = 1
        streaming["last_l0_half_x_queries"] = 0
        streaming["last_l0_half_z_queries"] = 1
        self.fixture.write_report("astral_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "zero/nonzero populations"):
            self.fixture.analyze()

    def test_l0_query_and_reuse_counters_match_exact_cache_shift_identity(self) -> None:
        for key in ("natural_point", "astral_point"):
            streaming = self.fixture.reports[key]["planetary_streaming"]
            streaming["last_l0_cache_update"] = "IncrementalStrip"
            streaming["last_l0_cache_shift_x_cells"] = -1
            streaming["last_l0_cache_shift_z_cells"] = 0
            streaming["last_l0_center_queries"] = 65
            streaming["last_l0_half_x_queries"] = 0
            streaming["last_l0_half_z_queries"] = 0
            streaming["last_l0_reused_height_samples"] = 4_160
            self.fixture.write_report(key)
        for key in ("natural_cardinal", "astral_cardinal"):
            streaming = self.fixture.reports[key]["planetary_streaming"]
            streaming["last_l0_cache_update"] = "IncrementalStrip"
            streaming["last_l0_cache_shift_x_cells"] = -1
            streaming["last_l0_cache_shift_z_cells"] = 0
            streaming["last_l0_center_queries"] = 65
            streaming["last_l0_half_x_queries"] = 65
            streaming["last_l0_half_z_queries"] = 66
            streaming["last_l0_reused_height_samples"] = 12_609
            self.fixture.write_report(key)

        self.assertEqual(
            self.fixture.analyze()["automated_decision"],
            "pass-pending-mandatory-human-inspection",
        )

        streaming = self.fixture.reports["natural_cardinal"]["planetary_streaming"]
        streaming["last_l0_center_queries"] = 64
        self.fixture.write_report("natural_cardinal")
        with self.assertRaisesRegex(analyzer.AnalysisError, "exact IncrementalStrip"):
            self.fixture.analyze()

    def test_teleport_fallback_accepts_only_zero_sentinel_or_large_shift(self) -> None:
        point = analyzer.ARM_SPECS[0]
        self.assertEqual(
            analyzer._expected_l0_sampling_identity(point, "TeleportFallback", 0, 0),
            (4_225, 0, 0, 0),
        )
        self.assertEqual(
            analyzer._expected_l0_sampling_identity(point, "TeleportFallback", -65, 1),
            (4_225, 0, 0, 0),
        )
        with self.assertRaisesRegex(analyzer.AnalysisError, "zero sentinel"):
            analyzer._expected_l0_sampling_identity(
                point, "TeleportFallback", 1, 0
            )

    def test_fixed_viewport_contract_has_explicit_failure(self) -> None:
        viewport = self.fixture.reports["natural_point"]["viewport"]
        viewport["physical_width"] = 7
        self.fixture.write_report("natural_point")
        with self.assertRaisesRegex(
            analyzer.AnalysisError, "fixed diagnostic contract requires 8x8"
        ):
            self.fixture.analyze()

    def test_ron_parser_rejects_duplicate_fields_comments_and_nonfinite(self) -> None:
        with self.assertRaisesRegex(analyzer.RonParseError, "duplicate field"):
            analyzer.RonParser("(a: 1, a: 2)").parse()
        with self.assertRaisesRegex(analyzer.RonParseError, "comments are forbidden"):
            analyzer.RonParser("(a: 1, // hidden\nb: 2)").parse()
        with self.assertRaisesRegex(analyzer.RonParseError, "non-finite"):
            analyzer.RonParser("(a: NaN)").parse()

    def test_largest_component_uses_eight_connectivity_and_exact_mask(self) -> None:
        path = Path(self.temporary.name) / "diagonal.png"
        pixels = np.zeros((8, 8, 3), dtype=np.uint8)
        pixels[0, 0] = [201, 9, 29]
        pixels[1, 1] = [255, 0, 0]
        pixels[2, 2] = [255, 0, 0]
        pixels[7, 7] = [200, 0, 0]  # strict red threshold excludes this pixel
        Image.fromarray(pixels, mode="RGB").save(path, format="PNG")
        _, snapshot = analyzer._safe_read(path, analyzer.MAX_IMAGE_BYTES, "diagonal")
        _, _, _, largest, occupancy = analyzer._measure_png(snapshot, 8, 8, "diagonal")
        self.assertEqual(largest, 3)
        self.assertAlmostEqual(occupancy, 3 / 64)

    def test_hash_recheck_detects_report_mutation(self) -> None:
        report_path = self.fixture.paths["natural_point"] / "report.ron"
        _, snapshot = analyzer._safe_read(
            report_path, analyzer.MAX_REPORT_BYTES, "fixture report"
        )
        report_path.write_text("(corrupted: true)\n", encoding="utf-8")
        with self.assertRaisesRegex(analyzer.AnalysisError, "hash or file identity changed"):
            analyzer._verify_snapshot_unchanged(
                snapshot, analyzer.MAX_REPORT_BYTES, "fixture report"
            )

    def test_atomic_output_is_create_new_and_never_overwrites(self) -> None:
        ledger = self.fixture.analyze()
        output = Path(self.temporary.name) / "ledger.json"
        input_dirs = list(self.fixture.paths.values())
        written = analyzer.write_json_create_new_atomic(output, ledger, input_dirs)
        self.assertEqual(written, output)
        self.assertEqual(json.loads(output.read_text(encoding="utf-8")), ledger)
        original = output.read_bytes()
        with self.assertRaisesRegex(analyzer.AnalysisError, "already exists"):
            analyzer.write_json_create_new_atomic(output, ledger, input_dirs)
        self.assertEqual(output.read_bytes(), original)

    def test_protected_output_and_input_symlink_are_rejected(self) -> None:
        protected = Path(self.temporary.name) / "qa_runs"
        ledger = self.fixture.analyze()
        with self.assertRaisesRegex(analyzer.AnalysisError, "may not be inside"):
            analyzer.write_json_create_new_atomic(
                protected / "ledger.json", ledger, list(self.fixture.paths.values())
            )

        outside = Path(self.temporary.name) / "outside.png"
        self.fixture.write_image("natural_point", 0, 1)
        source = self.fixture.paths["natural_point"] / "shot_0000_context.png"
        source.replace(outside)
        try:
            os.symlink(outside, source)
        except (OSError, NotImplementedError):
            self.skipTest("the test host does not permit symlink creation")
        with self.assertRaisesRegex(analyzer.AnalysisError, "symlink or reparse"):
            self.fixture.analyze()

    @unittest.skipUnless(os.name == "nt", "Windows filesystem alias regression")
    def test_windows_protected_output_aliases_are_rejected(self) -> None:
        ledger = self.fixture.analyze()
        input_dirs = list(self.fixture.paths.values())
        protected = Path(self.temporary.name) / "qa_runs"
        output_name = "__codex_protected_alias_probe__.json"

        for alias in ("qa_runs ", "QA_RUNS ", "qa_runs. "):
            with self.subTest(alias=alias):
                with self.assertRaisesRegex(
                    analyzer.AnalysisError,
                    "alias or symlink|may not be inside",
                ):
                    analyzer.write_json_create_new_atomic(
                        Path(self.temporary.name) / alias / output_name,
                        ledger,
                        input_dirs,
                    )
                self.assertFalse((protected / output_name).exists())

    @unittest.skipUnless(os.name == "nt", "Windows extended-path regression")
    def test_extended_namespace_output_inside_input_run_is_rejected(self) -> None:
        input_run = Path(self.temporary.name) / "input_run"
        input_run.mkdir()
        output = input_run / "__codex_input_alias_probe__.json"
        extended_output = "\\\\?\\" + str(output)
        with self.assertRaisesRegex(analyzer.AnalysisError, "input run directory"):
            analyzer._validate_output_path(extended_output, [input_run])
        self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
