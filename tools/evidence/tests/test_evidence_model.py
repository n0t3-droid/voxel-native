from __future__ import annotations

import copy
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


EVIDENCE_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = EVIDENCE_DIR.parents[1]
sys.path.insert(0, str(EVIDENCE_DIR))

import evidence_model as model  # noqa: E402


def node(
    alias: str,
    kind: str,
    identity_key: str,
    *,
    classification: str | None = None,
    task_state: str | None = None,
    data: dict | None = None,
) -> dict:
    result = {
        "alias": alias,
        "data": data or {},
        "identity": {"key": identity_key},
        "kind": kind,
        "title": f"Title for {alias}",
    }
    if classification is not None:
        result["classification"] = classification
    if task_state is not None:
        result["task_state"] = task_state
    return result


def edge(relation: str, source: str, target: str, data: dict | None = None) -> dict:
    return {"data": data or {}, "from": source, "relation": relation, "to": target}


def candidate(nodes: list[dict], edges: list[dict]) -> dict:
    return {
        "edges": edges,
        "nodes": nodes,
        "schema_version": model.CANDIDATE_SCHEMA_VERSION,
    }


class EvidenceGraphTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_candidate(self, name: str, value: dict, *, sort_keys: bool = False) -> Path:
        path = self.root / name
        path.write_text(
            json.dumps(value, ensure_ascii=False, sort_keys=sort_keys),
            encoding="utf-8",
            newline="\n",
        )
        return path

    def compile(self, *paths: Path) -> dict:
        return model.compile_candidate_files(paths, repo_root=self.root)

    def minimal_candidate(self) -> dict:
        return candidate(
            [
                node("source", "source_file", "source", classification="Passed"),
                node("claim", "claim", "claim", classification="Observed"),
                node("task-a", "task", "task-a", task_state="running"),
                node("task-b", "task", "task-b", task_state="ready"),
                node("agent", "agent", "agent"),
                node("artifact", "artifact", "artifact"),
            ],
            [
                edge("derived_from", "claim", "source"),
                edge("supports", "source", "claim"),
                edge("depends_on", "task-a", "task-b"),
                edge("assigned_to", "task-a", "agent"),
                edge("renders", "artifact", "claim"),
            ],
        )

    def test_typed_canonical_ids_are_stable_and_namespaced(self) -> None:
        identity_a = {"z": [3, 2, 1], "a": {"value": "same"}}
        identity_b = {"a": {"value": "same"}, "z": [3, 2, 1]}
        claim_a = model.derive_node_id("claim", identity_a)
        claim_b = model.derive_node_id("claim", identity_b)
        issue = model.derive_node_id("issue", identity_b)
        self.assertEqual(claim_a, claim_b)
        self.assertNotEqual(claim_a, issue)
        self.assertRegex(claim_a, r"^claim:[0-9a-f]{64}$")

    def test_repeated_compile_and_explicit_input_order_are_byte_stable(self) -> None:
        first = self.write_candidate(
            "a.json",
            candidate(
                [
                    node("claim", "claim", "claim", classification="Observed"),
                    node("source", "source_file", "source", classification="Passed"),
                ],
                [edge("derived_from", "claim", "source")],
            ),
        )
        second = self.write_candidate(
            "b.json",
            candidate(
                [
                    node("agent", "agent", "agent"),
                    node("task", "task", "task", task_state="ready"),
                ],
                [edge("assigned_to", "task", "agent")],
            ),
        )
        graph_a = self.compile(first, second)
        graph_b = self.compile(second, first)
        self.assertEqual(graph_a, graph_b)
        self.assertEqual(model.graph_json(graph_a), model.graph_json(graph_b))
        self.assertEqual(
            graph_a["nodes"],
            sorted(graph_a["nodes"], key=lambda item: (item["kind"], item["id"])),
        )
        self.assertEqual(
            graph_a["edges"],
            sorted(
                graph_a["edges"],
                key=lambda item: (
                    item["relation"],
                    item["from"],
                    item["to"],
                    model.canonical_sha256(item["data"]),
                ),
            ),
        )

    def test_record_order_keeps_semantic_ids_but_changes_exact_source_provenance(self) -> None:
        value = self.minimal_candidate()
        reordered = copy.deepcopy(value)
        reordered["nodes"].reverse()
        reordered["edges"].reverse()
        graph_a = self.compile(self.write_candidate("order-a.json", value))
        graph_b = self.compile(self.write_candidate("order-b.json", reordered))
        self.assertEqual(graph_a["nodes"], graph_b["nodes"])
        self.assertEqual(graph_a["edges"], graph_b["edges"])
        self.assertNotEqual(graph_a["source_inputs"], graph_b["source_inputs"])
        self.assertNotEqual(graph_a["graph_sha256"], graph_b["graph_sha256"])

    def test_all_classifications_and_task_states_remain_separate(self) -> None:
        nodes = [
            node(f"claim-{value}", "claim", f"claim-{value}", classification=value)
            for value in model.EVIDENCE_CLASSIFICATIONS
        ]
        nodes.extend(
            node(f"task-{value}", "task", f"task-{value}", task_state=value)
            for value in model.TASK_STATES
        )
        graph = self.compile(self.write_candidate("statuses.json", candidate(nodes, [])))
        self.assertEqual(graph["evidence_classifications"], list(model.EVIDENCE_CLASSIFICATIONS))
        self.assertEqual(graph["task_states"], list(model.TASK_STATES))
        self.assertEqual(graph["summary"]["classification_counts"], {value: 1 for value in model.EVIDENCE_CLASSIFICATIONS})
        self.assertEqual(graph["summary"]["task_state_counts"], {value: 1 for value in model.TASK_STATES})
        for item in graph["nodes"]:
            if item["kind"] == "task":
                self.assertIn("task_state", item)
                self.assertNotIn("classification", item)

    def test_task_classification_and_non_task_state_are_rejected(self) -> None:
        cases = [
            candidate(
                [node("task", "task", "task", classification="Observed", task_state="ready")],
                [],
            ),
            candidate(
                [node("claim", "claim", "claim", classification="Observed", task_state="ready")],
                [],
            ),
        ]
        for index, value in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"status-{index}.json", value))

    def test_duplicate_alias_identity_and_edge_are_rejected(self) -> None:
        cases = [
            candidate(
                [
                    node("same", "claim", "one", classification="Observed"),
                    node("same", "issue", "two", classification="Blocked"),
                ],
                [],
            ),
            candidate(
                [
                    node("first", "claim", "same", classification="Observed"),
                    node("second", "claim", "same", classification="Observed"),
                ],
                [],
            ),
            candidate(
                [
                    node("source", "source_file", "source"),
                    node("claim", "claim", "claim", classification="Observed"),
                ],
                [
                    edge("supports", "source", "claim"),
                    edge("supports", "source", "claim", {"different": True}),
                ],
            ),
        ]
        for index, value in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"duplicate-{index}.json", value))

    def test_missing_endpoint_and_typed_relation_mismatches_are_rejected(self) -> None:
        cases = [
            candidate(
                [node("claim", "claim", "claim", classification="Observed")],
                [edge("supports", "claim", "missing")],
            ),
            candidate(
                [
                    node("claim", "claim", "claim", classification="Observed"),
                    node("task", "task", "task", task_state="ready"),
                ],
                [edge("depends_on", "task", "claim")],
            ),
            candidate(
                [
                    node("task", "task", "task", task_state="ready"),
                    node("artifact", "artifact", "artifact"),
                ],
                [edge("assigned_to", "task", "artifact")],
            ),
        ]
        for index, value in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"endpoint-{index}.json", value))

    def test_task_self_dependency_and_cycle_are_rejected(self) -> None:
        self_loop = candidate(
            [node("a", "task", "a", task_state="ready")],
            [edge("depends_on", "a", "a")],
        )
        cycle = candidate(
            [
                node("a", "task", "a", task_state="ready"),
                node("b", "task", "b", task_state="planned"),
                node("c", "task", "c", task_state="blocked"),
            ],
            [
                edge("depends_on", "a", "b"),
                edge("depends_on", "b", "c"),
                edge("depends_on", "c", "a"),
            ],
        )
        for name, value in (("self", self_loop), ("cycle", cycle)):
            with self.subTest(name=name):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"{name}.json", value))

    def test_unknown_schema_fields_kinds_relations_and_states_are_rejected(self) -> None:
        base = self.minimal_candidate()
        cases = []
        changed = copy.deepcopy(base)
        changed["schema_version"] = "evidence-graph-candidate/2.0.0"
        cases.append(changed)
        changed = copy.deepcopy(base)
        changed["unknown"] = True
        cases.append(changed)
        changed = copy.deepcopy(base)
        changed["nodes"][0]["unknown"] = True
        cases.append(changed)
        changed = copy.deepcopy(base)
        changed["nodes"][0]["kind"] = "mystery"
        cases.append(changed)
        changed = copy.deepcopy(base)
        changed["edges"][0]["relation"] = "implies"
        cases.append(changed)
        changed = copy.deepcopy(base)
        changed["nodes"][2]["task_state"] = "in_progress"
        cases.append(changed)
        for index, value in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"unsupported-{index}.json", value))

    def test_duplicate_json_keys_and_nonfinite_numbers_are_rejected(self) -> None:
        duplicate = self.root / "duplicate-key.json"
        duplicate.write_text(
            '{"schema_version":"evidence-graph-candidate/1.0.0","nodes":[],"nodes":[],"edges":[]}',
            encoding="utf-8",
        )
        nonfinite = self.root / "nonfinite.json"
        nonfinite.write_text(
            '{"schema_version":"evidence-graph-candidate/1.0.0","nodes":'
            '[{"alias":"x","kind":"observation","identity":{"x":1},"title":"x","data":{"value":NaN}}],"edges":[]}',
            encoding="utf-8",
        )
        for path in (duplicate, nonfinite):
            with self.subTest(path=path.name):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(path)

    def test_string_integer_depth_and_path_caps_are_rejected(self) -> None:
        too_long = candidate(
            [node("claim", "claim", "claim", classification="Observed", data={"text": "x" * (model.MAX_STRING_CHARS + 1)})],
            [],
        )
        too_large_integer = candidate(
            [node("claim", "claim", "claim", classification="Observed", data={"value": 2**63})],
            [],
        )
        too_long_path = candidate(
            [node("claim", "claim", "claim", classification="Observed", data={"report_path": "p" * (model.MAX_PATH_CHARS + 1)})],
            [],
        )
        nested: dict = {"value": 1}
        for _ in range(model.MAX_JSON_DEPTH + 2):
            nested = {"child": nested}
        too_deep = candidate(
            [node("claim", "claim", "claim", classification="Observed", data=nested)],
            [],
        )
        for index, value in enumerate((too_long, too_large_integer, too_long_path, too_deep)):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"value-cap-{index}.json", value))

    def test_population_caps_are_fail_closed(self) -> None:
        cases = [
            [node(f"task-{i}", "task", f"task-{i}", task_state="planned") for i in range(model.MAX_TASKS + 1)],
            [node(f"agent-{i}", "agent", f"agent-{i}") for i in range(model.MAX_AGENTS + 1)],
            [node(f"external-{i}", "external_asset", f"external-{i}") for i in range(model.MAX_EXTERNAL_ASSETS + 1)],
            [node(f"artifact-{i}", "artifact", f"artifact-{i}") for i in range(model.MAX_ARTIFACTS + 1)],
        ]
        for index, nodes in enumerate(cases):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    self.compile(self.write_candidate(f"population-{index}.json", candidate(nodes, [])))

    def test_global_node_edge_and_dependency_caps_are_fail_closed(self) -> None:
        with self.assertRaises(model.EvidenceGraphError):
            model._validate_caps(
                [{"kind": "observation"}] * (model.MAX_NODES + 1),
                [],
            )
        with self.assertRaises(model.EvidenceGraphError):
            model._validate_caps(
                [],
                [{"relation": "references"}] * (model.MAX_EDGES + 1),
            )
        with self.assertRaises(model.EvidenceGraphError):
            model._validate_caps(
                [],
                [{"relation": "depends_on"}] * (model.MAX_DEPENDENCIES + 1),
            )

        dependency_nodes = [
            node(f"task-{index}", "task", f"task-{index}", task_state="planned")
            for index in range(model.MAX_TASKS)
        ]
        dependency_edges: list[dict] = []
        for source_index in range(model.MAX_TASKS):
            for target_index in range(source_index + 1, model.MAX_TASKS):
                dependency_edges.append(
                    edge(
                        "depends_on",
                        f"task-{source_index}",
                        f"task-{target_index}",
                    )
                )
                if len(dependency_edges) == model.MAX_DEPENDENCIES + 1:
                    break
            if len(dependency_edges) == model.MAX_DEPENDENCIES + 1:
                break
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(
                self.write_candidate(
                    "dependency-cap.json",
                    candidate(dependency_nodes, dependency_edges),
                )
            )

    def test_shared_input_byte_cap_rejects_before_parse(self) -> None:
        oversized = self.root / "oversized.json"
        oversized.write_bytes(b" " * (model.MAX_GRAPH_BYTES + 1))
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(oversized)

    def test_bounded_reader_accepts_exact_limit_and_rejects_one_extra_byte(self) -> None:
        exact = self.root / "exact.bin"
        exact.write_bytes(b"x" * 257)
        self.assertEqual(model._read_bounded_file(exact, maximum=257), b"x" * 257)
        with self.assertRaises(model.EvidenceGraphError):
            model._read_bounded_file(exact, maximum=256)

    def test_duplicate_explicit_input_and_unsafe_paths_are_rejected(self) -> None:
        path = self.write_candidate("candidate.json", self.minimal_candidate())
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(path, path)
        latest_dir = self.root / "latest"
        latest_dir.mkdir()
        latest = latest_dir / "candidate.json"
        latest.write_text("{}", encoding="utf-8")
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(latest)
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(self.root / "nested" / ".." / "candidate.json")

    @unittest.skipUnless(os.name == "nt", "Windows extended-path regression")
    def test_extended_namespace_duplicate_candidate_is_rejected(self) -> None:
        source = self.write_candidate("extended-duplicate.json", self.minimal_candidate())
        extended = "\\\\?\\" + str(source)
        with self.assertRaisesRegex(model.EvidenceGraphError, "duplicate canonical"):
            self.compile(source, extended)

    def test_input_count_cap_is_fail_closed(self) -> None:
        paths = [
            self.write_candidate(f"input-{index}.json", candidate([], []))
            for index in range(model.MAX_INPUTS + 1)
        ]
        with self.assertRaises(model.EvidenceGraphError):
            self.compile(*paths)
        with self.assertRaises(model.EvidenceGraphError):
            self.compile()

    def test_graph_hash_summary_order_and_identity_tampering_are_rejected(self) -> None:
        graph = self.compile(self.write_candidate("valid.json", self.minimal_candidate()))
        tampered_graphs = []
        changed = copy.deepcopy(graph)
        changed["summary"]["node_count"] += 1
        changed["graph_sha256"] = model.canonical_sha256(model._graph_payload(changed))
        tampered_graphs.append(changed)
        changed = copy.deepcopy(graph)
        changed["nodes"].reverse()
        changed["graph_sha256"] = model.canonical_sha256(model._graph_payload(changed))
        tampered_graphs.append(changed)
        changed = copy.deepcopy(graph)
        changed["nodes"][0]["identity"] = {"key": "tampered"}
        changed["graph_sha256"] = model.canonical_sha256(model._graph_payload(changed))
        tampered_graphs.append(changed)
        changed = copy.deepcopy(graph)
        changed["graph_sha256"] = "0" * 64
        tampered_graphs.append(changed)
        for index, changed in enumerate(tampered_graphs):
            with self.subTest(index=index):
                with self.assertRaises(model.EvidenceGraphError):
                    model.validate_graph(changed)

    def test_output_validation_protects_repository_data_and_requires_json(self) -> None:
        for protected in model.PROTECTED_OUTPUT_PARTS:
            with self.subTest(protected=protected):
                with self.assertRaises(model.EvidenceGraphError):
                    model.validate_output_path(
                        REPO_ROOT / protected / "graph.json",
                        repo_root=REPO_ROOT,
                    )
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_output_path(self.root / "graph.txt", repo_root=REPO_ROOT)
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_output_path(self.root / "nested" / ".." / "graph.json", repo_root=REPO_ROOT)

    @unittest.skipUnless(os.name == "nt", "Windows filesystem alias regression")
    def test_windows_protected_output_aliases_are_rejected(self) -> None:
        protected = self.root / "qa_runs"
        protected.mkdir()
        output_name = "__codex_protected_alias_probe__.json"

        for alias in ("qa_runs ", "QA_RUNS ", "qa_runs. "):
            with self.subTest(alias=alias):
                with self.assertRaisesRegex(
                    model.EvidenceGraphError,
                    "Windows-trimmed alias|alias or symlink|protected repository data",
                ):
                    model.validate_output_path(
                        self.root / alias / output_name,
                        repo_root=self.root,
                    )
                self.assertFalse((protected / output_name).exists())

    @unittest.skipUnless(os.name == "nt", "Windows filesystem alias regression")
    def test_windows_protected_output_aliases_are_rejected_before_directory_exists(
        self,
    ) -> None:
        fresh_root = self.root / "fresh-clone"
        fresh_root.mkdir()
        output_name = "__codex_absent_protected_alias_probe__.json"

        for alias in ("qa_runs ", "QA_RUNS ", "qa_runs. "):
            with self.subTest(alias=alias):
                self.assertFalse((fresh_root / "qa_runs").exists())
                with self.assertRaisesRegex(
                    model.EvidenceGraphError,
                    "Windows-trimmed alias",
                ):
                    model.validate_output_path(
                        fresh_root / alias / output_name,
                        repo_root=fresh_root,
                    )
                self.assertFalse((fresh_root / "qa_runs").exists())

    def test_cli_writes_only_explicit_output_and_round_trips(self) -> None:
        source = self.write_candidate("cli-input.json", self.minimal_candidate())
        output = self.root / "out" / "graph.json"
        script = EVIDENCE_DIR / "build_evidence_graph.py"
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                str(script),
                "--candidate",
                str(source),
                "--output",
                str(output),
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(output.is_file())
        graph = json.loads(output.read_text(encoding="utf-8"))
        model.validate_graph(graph)
        self.assertTrue(output.read_bytes().endswith(b"\n"))
        self.assertEqual(list(self.root.glob("**/*.tmp")), [])

    def test_failed_cli_does_not_create_output(self) -> None:
        invalid = self.root / "invalid.json"
        invalid.write_text("{}", encoding="utf-8")
        output = self.root / "out" / "graph.json"
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                str(EVIDENCE_DIR / "build_evidence_graph.py"),
                "--candidate",
                str(invalid),
                "--output",
                str(output),
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 2)
        self.assertFalse(output.exists())

    def test_cli_never_overwrites_a_candidate_input(self) -> None:
        source = self.write_candidate("same.json", self.minimal_candidate())
        before = source.read_bytes()
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                str(EVIDENCE_DIR / "build_evidence_graph.py"),
                "--candidate",
                str(source),
                "--output",
                str(source),
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(source.read_bytes(), before)

    @unittest.skipUnless(os.name == "nt", "Windows extended-path regression")
    def test_cli_never_overwrites_candidate_through_extended_namespace(self) -> None:
        source = self.write_candidate("extended-same.json", self.minimal_candidate())
        before = source.read_bytes()
        extended_output = "\\\\?\\" + str(source)
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                str(EVIDENCE_DIR / "build_evidence_graph.py"),
                "--candidate",
                str(source),
                "--output",
                extended_output,
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("must not overwrite", result.stderr)
        self.assertEqual(source.read_bytes(), before)

    def test_artifact_view_validates_source_identity_and_separate_status(self) -> None:
        artifact_id = model.derive_node_id("artifact", {"key": "artifact"})
        source_id = model.derive_node_id("claim", {"key": "claim"})
        identity = {
            "artifact_node_id": artifact_id,
            "source_graph_sha256": "a" * 64,
            "title": "Evidence dashboard",
            "view_kind": "site",
        }
        view = {
            "artifact_node_id": artifact_id,
            "schema_version": model.ARTIFACT_VIEW_SCHEMA_VERSION,
            "sections": [
                {
                    "id": "overview",
                    "rows": [
                        {
                            "cells": [
                                {
                                    "classification": "Observed",
                                    "source_node_ids": [source_id],
                                    "value": "42 observed",
                                }
                            ],
                            "classification": "Observed",
                            "id": "row-1",
                            "source_node_ids": [source_id],
                        }
                    ],
                    "source_node_ids": [source_id],
                    "title": "Overview",
                }
            ],
            "source_graph_sha256": "a" * 64,
            "title": "Evidence dashboard",
            "view_id": f"artifact-view:{model.canonical_sha256(identity)}",
            "view_kind": "site",
        }
        view["view_sha256"] = model.canonical_sha256(view)
        model.validate_artifact_view(view)
        changed = copy.deepcopy(view)
        changed["sections"][0]["rows"][0]["task_state"] = "complete"
        changed["view_sha256"] = model.canonical_sha256(
            {key: value for key, value in changed.items() if key != "view_sha256"}
        )
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_artifact_view(changed)

    def test_artifact_view_can_be_bound_to_exact_graph_and_existing_nodes(self) -> None:
        candidate_value = candidate(
            [
                node("claim", "claim", "claim", classification="Observed"),
                node("artifact", "artifact", "artifact"),
            ],
            [edge("renders", "artifact", "claim")],
        )
        graph = self.compile(self.write_candidate("view-graph.json", candidate_value))
        nodes_by_kind = {item["kind"]: item for item in graph["nodes"]}
        identity = {
            "artifact_node_id": nodes_by_kind["artifact"]["id"],
            "source_graph_sha256": graph["graph_sha256"],
            "title": "Bound dossier",
            "view_kind": "document",
        }
        view = {
            "artifact_node_id": identity["artifact_node_id"],
            "schema_version": model.ARTIFACT_VIEW_SCHEMA_VERSION,
            "sections": [
                {
                    "id": "claims",
                    "rows": [
                        {
                            "cells": [
                                {
                                    "source_node_ids": [nodes_by_kind["claim"]["id"]],
                                    "value": "Observed claim",
                                }
                            ],
                            "id": "claim-row",
                            "source_node_ids": [nodes_by_kind["claim"]["id"]],
                        }
                    ],
                    "source_node_ids": [nodes_by_kind["claim"]["id"]],
                    "title": "Claims",
                }
            ],
            "source_graph_sha256": identity["source_graph_sha256"],
            "title": identity["title"],
            "view_id": f"artifact-view:{model.canonical_sha256(identity)}",
            "view_kind": identity["view_kind"],
        }
        view["view_sha256"] = model.canonical_sha256(view)
        model.validate_artifact_view(view, source_graph=graph)

        missing_node = copy.deepcopy(view)
        missing = model.derive_node_id("claim", {"key": "absent"})
        missing_node["sections"][0]["source_node_ids"] = [missing]
        missing_node["sections"][0]["rows"][0]["source_node_ids"] = [missing]
        missing_node["sections"][0]["rows"][0]["cells"][0]["source_node_ids"] = [missing]
        missing_node["view_sha256"] = model.canonical_sha256(
            {key: value for key, value in missing_node.items() if key != "view_sha256"}
        )
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_artifact_view(missing_node, source_graph=graph)

        wrong_hash = copy.deepcopy(view)
        wrong_hash["source_graph_sha256"] = "f" * 64
        wrong_identity = dict(identity)
        wrong_identity["source_graph_sha256"] = "f" * 64
        wrong_hash["view_id"] = f"artifact-view:{model.canonical_sha256(wrong_identity)}"
        wrong_hash["view_sha256"] = model.canonical_sha256(
            {key: value for key, value in wrong_hash.items() if key != "view_sha256"}
        )
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_artifact_view(wrong_hash, source_graph=graph)

        contradictory = copy.deepcopy(view)
        contradictory["sections"][0]["rows"][0]["classification"] = "Passed"
        contradictory["sections"][0]["rows"][0]["cells"][0]["classification"] = "Passed"
        contradictory["view_sha256"] = model.canonical_sha256(
            {key: value for key, value in contradictory.items() if key != "view_sha256"}
        )
        with self.assertRaises(model.EvidenceGraphError):
            model.validate_artifact_view(contradictory, source_graph=graph)

    def test_schema_files_are_strict_json_and_pin_exact_enums(self) -> None:
        graph_schema = json.loads(
            (EVIDENCE_DIR / "schema" / "evidence-graph-1.0.0.schema.json").read_text(encoding="utf-8")
        )
        view_schema = json.loads(
            (EVIDENCE_DIR / "schema" / "artifact-view-1.0.0.schema.json").read_text(encoding="utf-8")
        )
        self.assertFalse(graph_schema["additionalProperties"])
        self.assertFalse(view_schema["additionalProperties"])
        self.assertEqual(
            graph_schema["properties"]["evidence_classifications"]["const"],
            list(model.EVIDENCE_CLASSIFICATIONS),
        )
        self.assertEqual(
            graph_schema["properties"]["task_states"]["const"],
            list(model.TASK_STATES),
        )


if __name__ == "__main__":
    unittest.main()
