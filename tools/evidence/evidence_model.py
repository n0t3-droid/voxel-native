#!/usr/bin/env python3
"""Dependency-free, fail-closed Evidence Graph compiler and validators.

The graph is a normalized truth boundary.  Candidate aliases exist only while
compiling edges; authoritative node IDs are always derived from typed identity
payloads and cannot be supplied by a caller.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import tempfile
from collections import Counter
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


GRAPH_SCHEMA_VERSION = "evidence-graph/1.0.0"
CANDIDATE_SCHEMA_VERSION = "evidence-graph-candidate/1.0.0"
ARTIFACT_VIEW_SCHEMA_VERSION = "artifact-view/1.0.0"
GENERATOR_NAME = "voxel-native-evidence-graph"
GENERATOR_VERSION = "1.0.0"

MAX_GRAPH_BYTES = 16 * 1024 * 1024
MAX_INPUTS = 64
MAX_NODES = 12_000
MAX_EDGES = 32_000
MAX_TASKS = 512
MAX_AGENTS = 48
MAX_DEPENDENCIES = 2_048
MAX_EXTERNAL_ASSETS = 32
MAX_ARTIFACTS = 32
MAX_ID_CHARS = 256
MAX_PATH_CHARS = 4_096
MAX_STRING_CHARS = 16_384
MAX_JSON_DEPTH = 64
READ_CHUNK_BYTES = 1024 * 1024

NODE_KINDS = (
    "source_file",
    "qa_run",
    "observation",
    "claim",
    "issue",
    "gate_run",
    "visual_review",
    "task",
    "agent",
    "artifact",
    "external_asset",
    "github_check",
)
EDGE_RELATIONS = (
    "derived_from",
    "supports",
    "contradicts",
    "generated_by",
    "validated_by",
    "assigned_to",
    "depends_on",
    "blocks",
    "renders",
    "published_as",
    "references",
)
EVIDENCE_CLASSIFICATIONS = (
    "Passed",
    "Observed",
    "Rejected",
    "Planned",
    "Blocked",
)
TASK_STATES = (
    "planned",
    "ready",
    "running",
    "blocked",
    "review",
    "complete",
    "cancelled",
)
EVIDENCE_CLASSIFIABLE_KINDS = frozenset(
    {
        "source_file",
        "qa_run",
        "observation",
        "claim",
        "issue",
        "gate_run",
        "visual_review",
        "external_asset",
        "github_check",
    }
)
PROTECTED_OUTPUT_PARTS = frozenset({"saves", "qa_runs", "agent_runs"})

_ALIAS_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,255}$")
_SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
_NODE_ID_RE = re.compile(
    rf"^(?:{'|'.join(re.escape(kind) for kind in NODE_KINDS)}):[0-9a-f]{{64}}$"
)


class EvidenceGraphError(ValueError):
    """Raised when input cannot produce a trustworthy bounded graph."""


class _DuplicateJsonKey(EvidenceGraphError):
    pass


def _reject_constant(value: str) -> None:
    raise EvidenceGraphError(f"non-finite JSON number is unsupported: {value}")


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJsonKey(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_json_bytes(payload: bytes, *, source: str) -> Any:
    try:
        text = payload.decode("utf-8")
        return json.loads(
            text,
            object_pairs_hook=_strict_object,
            parse_constant=_reject_constant,
        )
    except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise EvidenceGraphError(f"{source} is not supported strict UTF-8 JSON: {error}") from error


def canonical_json_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            allow_nan=False,
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode("utf-8")
    except (TypeError, ValueError, RecursionError) as error:
        raise EvidenceGraphError(f"value cannot be canonically serialized: {error}") from error


def canonical_sha256(value: Any) -> str:
    return hashlib.sha256(canonical_json_bytes(value)).hexdigest()


def derive_node_id(kind: str, identity: Mapping[str, Any]) -> str:
    if kind not in NODE_KINDS:
        raise EvidenceGraphError(f"unsupported node kind: {kind!r}")
    return f"{kind}:{canonical_sha256(identity)}"


def _require_exact_keys(
    value: Mapping[str, Any],
    *,
    required: Iterable[str],
    optional: Iterable[str] = (),
    scope: str,
) -> None:
    required_set = set(required)
    allowed = required_set | set(optional)
    missing = sorted(required_set - set(value))
    unknown = sorted(set(value) - allowed)
    if missing:
        raise EvidenceGraphError(f"{scope} is missing required field(s): {', '.join(missing)}")
    if unknown:
        raise EvidenceGraphError(f"{scope} has unsupported field(s): {', '.join(unknown)}")


def _require_string(
    value: Any,
    *,
    scope: str,
    maximum: int = MAX_STRING_CHARS,
    pattern: re.Pattern[str] | None = None,
) -> str:
    if type(value) is not str or not value or len(value) > maximum:
        raise EvidenceGraphError(f"{scope} must be a non-empty string of at most {maximum} characters")
    if pattern is not None and pattern.fullmatch(value) is None:
        raise EvidenceGraphError(f"{scope} has an unsupported machine-readable format")
    return value


def _is_path_key(key: str) -> bool:
    folded = key.casefold()
    return folded == "path" or folded.endswith("_path") or folded.endswith("_paths")


def _validate_json_value(value: Any, *, scope: str, depth: int = 0, path_context: bool = False) -> None:
    if depth > MAX_JSON_DEPTH:
        raise EvidenceGraphError(f"{scope} exceeds the {MAX_JSON_DEPTH}-level nesting cap")
    if value is None or type(value) is bool:
        return
    if type(value) is int:
        if not -(2**63) <= value <= 2**63 - 1:
            raise EvidenceGraphError(f"{scope} integer is outside signed 64-bit portability bounds")
        return
    if type(value) is float:
        if not math.isfinite(value):
            raise EvidenceGraphError(f"{scope} contains a non-finite number")
        return
    if type(value) is str:
        maximum = MAX_PATH_CHARS if path_context else MAX_STRING_CHARS
        if len(value) > maximum:
            raise EvidenceGraphError(f"{scope} exceeds the {maximum}-character cap")
        return
    if type(value) is list:
        for index, item in enumerate(value):
            _validate_json_value(
                item,
                scope=f"{scope}[{index}]",
                depth=depth + 1,
                path_context=path_context,
            )
        return
    if type(value) is dict:
        for key, item in value.items():
            if type(key) is not str or not key or len(key) > MAX_STRING_CHARS:
                raise EvidenceGraphError(f"{scope} contains an invalid object key")
            _validate_json_value(
                item,
                scope=f"{scope}.{key}",
                depth=depth + 1,
                path_context=_is_path_key(key),
            )
        return
    raise EvidenceGraphError(f"{scope} contains unsupported JSON type {type(value).__name__}")


def _display_path(path: Path, repo_root: Path) -> str:
    try:
        display = path.relative_to(repo_root).as_posix()
    except ValueError:
        display = path.as_posix()
    if not display or len(display) > MAX_PATH_CHARS:
        raise EvidenceGraphError(f"canonical input path exceeds {MAX_PATH_CHARS} characters")
    return display


def _read_bounded_file(path: Path, *, maximum: int) -> bytes:
    """Capture at most ``maximum`` bytes plus one rejection byte.

    This does not trust a prior stat size: an explicitly selected file can grow
    concurrently, and the fixed input-memory budget must still hold.
    """

    if maximum < 0:
        raise EvidenceGraphError("shared input byte budget is exhausted")
    captured = bytearray()
    try:
        with path.open("rb") as handle:
            while True:
                remaining_with_sentinel = maximum - len(captured) + 1
                if remaining_with_sentinel <= 0:
                    raise EvidenceGraphError(
                        f"explicit candidate bytes exceed the shared {MAX_GRAPH_BYTES}-byte cap"
                    )
                chunk = handle.read(min(READ_CHUNK_BYTES, remaining_with_sentinel))
                if not chunk:
                    break
                captured.extend(chunk)
                if len(captured) > maximum:
                    raise EvidenceGraphError(
                        f"explicit candidate bytes exceed the shared {MAX_GRAPH_BYTES}-byte cap"
                    )
    except OSError as error:
        raise EvidenceGraphError(f"candidate cannot be read: {path}: {error}") from error
    return bytes(captured)


def _normalize_candidate_node(raw: Any, *, scope: str) -> tuple[str, dict[str, Any]]:
    if type(raw) is not dict:
        raise EvidenceGraphError(f"{scope} must be an object")
    _require_exact_keys(
        raw,
        required=("alias", "kind", "identity", "title"),
        optional=("classification", "task_state", "description", "data"),
        scope=scope,
    )
    alias = _require_string(raw["alias"], scope=f"{scope}.alias", maximum=MAX_ID_CHARS, pattern=_ALIAS_RE)
    kind = _require_string(raw["kind"], scope=f"{scope}.kind", maximum=MAX_ID_CHARS)
    if kind not in NODE_KINDS:
        raise EvidenceGraphError(f"{scope}.kind is unsupported: {kind!r}")
    identity = raw["identity"]
    if type(identity) is not dict or not identity:
        raise EvidenceGraphError(f"{scope}.identity must be a non-empty object")
    _validate_json_value(identity, scope=f"{scope}.identity")
    title = _require_string(raw["title"], scope=f"{scope}.title")

    normalized: dict[str, Any] = {
        "data": raw.get("data", {}),
        "id": derive_node_id(kind, identity),
        "identity": identity,
        "kind": kind,
        "title": title,
    }
    if type(normalized["data"]) is not dict:
        raise EvidenceGraphError(f"{scope}.data must be an object")
    _validate_json_value(normalized["data"], scope=f"{scope}.data")
    if "description" in raw:
        normalized["description"] = _require_string(raw["description"], scope=f"{scope}.description")

    classification = raw.get("classification")
    task_state = raw.get("task_state")
    if kind == "task":
        if classification is not None:
            raise EvidenceGraphError(f"{scope} task classification is forbidden; use task_state")
        if task_state not in TASK_STATES:
            raise EvidenceGraphError(f"{scope}.task_state must be one of {TASK_STATES}")
        normalized["task_state"] = task_state
    else:
        if task_state is not None:
            raise EvidenceGraphError(f"{scope}.task_state is legal only for task nodes")
        if classification is not None:
            if kind not in EVIDENCE_CLASSIFIABLE_KINDS:
                raise EvidenceGraphError(f"{scope}.classification is not legal for {kind} nodes")
            if classification not in EVIDENCE_CLASSIFICATIONS:
                raise EvidenceGraphError(
                    f"{scope}.classification must be one of {EVIDENCE_CLASSIFICATIONS}"
                )
            normalized["classification"] = classification
        elif kind in {"claim", "issue"}:
            raise EvidenceGraphError(f"{scope}.classification is required for {kind} nodes")
    return alias, normalized


def _normalize_candidate_edge(raw: Any, *, scope: str) -> dict[str, Any]:
    if type(raw) is not dict:
        raise EvidenceGraphError(f"{scope} must be an object")
    _require_exact_keys(
        raw,
        required=("relation", "from", "to"),
        optional=("data",),
        scope=scope,
    )
    relation = _require_string(raw["relation"], scope=f"{scope}.relation", maximum=MAX_ID_CHARS)
    if relation not in EDGE_RELATIONS:
        raise EvidenceGraphError(f"{scope}.relation is unsupported: {relation!r}")
    source_alias = _require_string(raw["from"], scope=f"{scope}.from", maximum=MAX_ID_CHARS, pattern=_ALIAS_RE)
    target_alias = _require_string(raw["to"], scope=f"{scope}.to", maximum=MAX_ID_CHARS, pattern=_ALIAS_RE)
    data = raw.get("data", {})
    if type(data) is not dict:
        raise EvidenceGraphError(f"{scope}.data must be an object")
    _validate_json_value(data, scope=f"{scope}.data")
    return {"data": data, "from_alias": source_alias, "relation": relation, "to_alias": target_alias}


def _task_cycle(nodes_by_id: Mapping[str, Mapping[str, Any]], edges: Sequence[Mapping[str, Any]]) -> list[str] | None:
    task_ids = sorted(node_id for node_id, node in nodes_by_id.items() if node["kind"] == "task")
    adjacency = {node_id: [] for node_id in task_ids}
    for edge in edges:
        if edge["relation"] == "depends_on":
            adjacency[edge["from"]].append(edge["to"])
    for targets in adjacency.values():
        targets.sort()

    state: dict[str, int] = {}
    stack: list[str] = []
    stack_index: dict[str, int] = {}

    def visit(node_id: str) -> list[str] | None:
        state[node_id] = 1
        stack_index[node_id] = len(stack)
        stack.append(node_id)
        for target in adjacency[node_id]:
            target_state = state.get(target, 0)
            if target_state == 0:
                cycle = visit(target)
                if cycle is not None:
                    return cycle
            elif target_state == 1:
                return stack[stack_index[target] :] + [target]
        stack.pop()
        stack_index.pop(node_id, None)
        state[node_id] = 2
        return None

    for node_id in task_ids:
        if state.get(node_id, 0) == 0:
            cycle = visit(node_id)
            if cycle is not None:
                return cycle
    return None


def _validate_caps(nodes: Sequence[Mapping[str, Any]], edges: Sequence[Mapping[str, Any]]) -> None:
    if len(nodes) > MAX_NODES:
        raise EvidenceGraphError(f"node count {len(nodes)} exceeds fixed cap {MAX_NODES}")
    if len(edges) > MAX_EDGES:
        raise EvidenceGraphError(f"edge count {len(edges)} exceeds fixed cap {MAX_EDGES}")
    kinds = Counter(node["kind"] for node in nodes)
    relations = Counter(edge["relation"] for edge in edges)
    bounded = (
        ("task", kinds["task"], MAX_TASKS),
        ("agent", kinds["agent"], MAX_AGENTS),
        ("external_asset", kinds["external_asset"], MAX_EXTERNAL_ASSETS),
        ("artifact", kinds["artifact"], MAX_ARTIFACTS),
        ("depends_on", relations["depends_on"], MAX_DEPENDENCIES),
    )
    for label, actual, maximum in bounded:
        if actual > maximum:
            raise EvidenceGraphError(f"{label} count {actual} exceeds fixed cap {maximum}")


def _summary(
    source_inputs: Sequence[Mapping[str, Any]],
    nodes: Sequence[Mapping[str, Any]],
    edges: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    node_counts = Counter(node["kind"] for node in nodes)
    relation_counts = Counter(edge["relation"] for edge in edges)
    classification_counts = Counter(
        node["classification"] for node in nodes if "classification" in node
    )
    task_state_counts = Counter(node["task_state"] for node in nodes if node["kind"] == "task")
    return {
        "classification_counts": {
            classification: classification_counts[classification]
            for classification in EVIDENCE_CLASSIFICATIONS
        },
        "edge_count": len(edges),
        "node_count": len(nodes),
        "node_kind_counts": {kind: node_counts[kind] for kind in NODE_KINDS},
        "relation_counts": {relation: relation_counts[relation] for relation in EDGE_RELATIONS},
        "source_input_count": len(source_inputs),
        "task_state_counts": {state: task_state_counts[state] for state in TASK_STATES},
    }


def _graph_payload(graph: Mapping[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in graph.items() if key != "graph_sha256"}


def validate_graph(graph: Any) -> None:
    if type(graph) is not dict:
        raise EvidenceGraphError("evidence graph root must be an object")
    _require_exact_keys(
        graph,
        required=(
            "schema_version",
            "generator",
            "evidence_classifications",
            "task_states",
            "source_inputs",
            "nodes",
            "edges",
            "summary",
            "graph_sha256",
        ),
        scope="graph",
    )
    if graph["schema_version"] != GRAPH_SCHEMA_VERSION:
        raise EvidenceGraphError("unsupported evidence graph schema_version")
    if graph["evidence_classifications"] != list(EVIDENCE_CLASSIFICATIONS):
        raise EvidenceGraphError("evidence classification contract changed")
    if graph["task_states"] != list(TASK_STATES):
        raise EvidenceGraphError("task state contract changed")
    generator = graph["generator"]
    if type(generator) is not dict:
        raise EvidenceGraphError("graph.generator must be an object")
    _require_exact_keys(generator, required=("name", "version"), scope="graph.generator")
    if generator != {"name": GENERATOR_NAME, "version": GENERATOR_VERSION}:
        raise EvidenceGraphError("unsupported evidence graph generator")

    source_inputs = graph["source_inputs"]
    nodes = graph["nodes"]
    edges = graph["edges"]
    if type(source_inputs) is not list or not 0 < len(source_inputs) <= MAX_INPUTS:
        raise EvidenceGraphError(f"source_inputs must contain 1..{MAX_INPUTS} records")
    if type(nodes) is not list or type(edges) is not list:
        raise EvidenceGraphError("graph nodes and edges must be arrays")
    if source_inputs != sorted(source_inputs, key=lambda item: item.get("path", "")):
        raise EvidenceGraphError("source_inputs are not in canonical path order")
    seen_input_paths: set[str] = set()
    for index, record in enumerate(source_inputs):
        scope = f"graph.source_inputs[{index}]"
        if type(record) is not dict:
            raise EvidenceGraphError(f"{scope} must be an object")
        _require_exact_keys(record, required=("path", "sha256", "size_bytes"), scope=scope)
        path = _require_string(record["path"], scope=f"{scope}.path", maximum=MAX_PATH_CHARS)
        if "\\" in path or any(
            part == ".." or part.casefold() == "latest" for part in path.split("/")
        ):
            raise EvidenceGraphError(f"{scope}.path is not a canonical explicit source path")
        if path in seen_input_paths:
            raise EvidenceGraphError("graph contains duplicate source input paths")
        seen_input_paths.add(path)
        if type(record["size_bytes"]) is not int or not 0 <= record["size_bytes"] <= MAX_GRAPH_BYTES:
            raise EvidenceGraphError(f"{scope}.size_bytes is outside the bounded input contract")
        if type(record["sha256"]) is not str or _SHA256_RE.fullmatch(record["sha256"]) is None:
            raise EvidenceGraphError(f"{scope}.sha256 is invalid")

    node_ids: set[str] = set()
    normalized_nodes: list[dict[str, Any]] = []
    for index, raw_node in enumerate(nodes):
        scope = f"graph.nodes[{index}]"
        if type(raw_node) is not dict:
            raise EvidenceGraphError(f"{scope} must be an object")
        required = {"id", "kind", "identity", "title", "data"}
        allowed = required | {"classification", "task_state", "description"}
        _require_exact_keys(raw_node, required=required, optional=allowed - required, scope=scope)
        node_id = _require_string(raw_node["id"], scope=f"{scope}.id", maximum=MAX_ID_CHARS)
        if _NODE_ID_RE.fullmatch(node_id) is None:
            raise EvidenceGraphError(f"{scope}.id has an unsupported authoritative format")
        if node_id in node_ids:
            raise EvidenceGraphError(f"duplicate authoritative node ID: {node_id}")
        node_ids.add(node_id)
        kind = raw_node["kind"]
        if kind not in NODE_KINDS:
            raise EvidenceGraphError(f"{scope}.kind is unsupported")
        identity = raw_node["identity"]
        if type(identity) is not dict or not identity:
            raise EvidenceGraphError(f"{scope}.identity must be a non-empty object")
        if derive_node_id(kind, identity) != node_id:
            raise EvidenceGraphError(f"{scope}.id does not match its canonical typed identity")
        # Reuse candidate validation so task/evidence separation has one implementation.
        synthetic = {key: value for key, value in raw_node.items() if key != "id"}
        synthetic["alias"] = "validation-alias"
        _, normalized = _normalize_candidate_node(synthetic, scope=scope)
        normalized_nodes.append(normalized)
    expected_node_order = sorted(normalized_nodes, key=lambda item: (item["kind"], item["id"]))
    if normalized_nodes != expected_node_order:
        raise EvidenceGraphError("nodes are not in canonical kind/ID order")

    nodes_by_id = {node["id"]: node for node in normalized_nodes}
    normalized_edges: list[dict[str, Any]] = []
    seen_edges: set[tuple[str, str, str]] = set()
    for index, edge in enumerate(edges):
        scope = f"graph.edges[{index}]"
        if type(edge) is not dict:
            raise EvidenceGraphError(f"{scope} must be an object")
        _require_exact_keys(edge, required=("relation", "from", "to", "data"), scope=scope)
        relation = edge["relation"]
        if relation not in EDGE_RELATIONS:
            raise EvidenceGraphError(f"{scope}.relation is unsupported")
        source_id = edge["from"]
        target_id = edge["to"]
        if source_id not in nodes_by_id or target_id not in nodes_by_id:
            raise EvidenceGraphError(f"{scope} has a missing endpoint")
        if type(edge["data"]) is not dict:
            raise EvidenceGraphError(f"{scope}.data must be an object")
        _validate_json_value(edge["data"], scope=f"{scope}.data")
        key = (relation, source_id, target_id)
        if key in seen_edges:
            raise EvidenceGraphError(f"duplicate edge: {relation} {source_id} -> {target_id}")
        seen_edges.add(key)
        source_kind = nodes_by_id[source_id]["kind"]
        target_kind = nodes_by_id[target_id]["kind"]
        if relation == "depends_on":
            if source_kind != "task" or target_kind != "task":
                raise EvidenceGraphError("depends_on endpoints must both be task nodes")
            if source_id == target_id:
                raise EvidenceGraphError("task self-dependency is forbidden")
        if relation == "assigned_to" and (source_kind, target_kind) != ("task", "agent"):
            raise EvidenceGraphError("assigned_to must connect task -> agent")
        normalized_edges.append(dict(edge))
    expected_edge_order = sorted(
        normalized_edges,
        key=lambda item: (item["relation"], item["from"], item["to"], canonical_sha256(item["data"])),
    )
    if normalized_edges != expected_edge_order:
        raise EvidenceGraphError("edges are not in canonical relation/endpoint order")

    _validate_caps(normalized_nodes, normalized_edges)
    cycle = _task_cycle(nodes_by_id, normalized_edges)
    if cycle is not None:
        raise EvidenceGraphError(f"task dependency cycle is forbidden: {' -> '.join(cycle)}")
    expected_summary = _summary(source_inputs, normalized_nodes, normalized_edges)
    if graph["summary"] != expected_summary:
        raise EvidenceGraphError("graph summary does not exactly match normalized contents")
    expected_hash = canonical_sha256(_graph_payload(graph))
    if graph["graph_sha256"] != expected_hash:
        raise EvidenceGraphError("graph_sha256 does not match canonical graph payload")
    if len(canonical_json_bytes(graph)) > MAX_GRAPH_BYTES:
        raise EvidenceGraphError(f"canonical graph exceeds the {MAX_GRAPH_BYTES}-byte cap")


def _read_candidate_files(
    candidate_paths: Sequence[Path | str], *, repo_root: Path
) -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    if not 0 < len(candidate_paths) <= MAX_INPUTS:
        raise EvidenceGraphError(f"exactly 1..{MAX_INPUTS} explicit candidate paths are required")
    accepted: dict[str, Path] = {}
    raw_by_canonical: dict[str, Path | str] = {}
    for raw in candidate_paths:
        path = Path(raw)
        if ".." in path.parts:
            raise EvidenceGraphError(f"candidate path must not contain parent traversal: {path}")
        if path.suffix.casefold() != ".json":
            raise EvidenceGraphError(f"candidate path must end in .json: {path}")
        if path.stem.casefold() == "latest" or any(part.casefold() == "latest" for part in path.parts):
            raise EvidenceGraphError(f"implicit latest candidate is forbidden: {path}")
        try:
            resolved = path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise EvidenceGraphError(f"candidate path cannot be resolved: {path}: {error}") from error
        if any(part.casefold() == "latest" for part in resolved.parts):
            raise EvidenceGraphError(f"candidate path resolves through forbidden latest state: {path}")
        if not resolved.is_file():
            raise EvidenceGraphError(f"candidate path is not a file: {path}")
        canonical = _path_comparison_key(resolved)
        if canonical in accepted:
            raise EvidenceGraphError(f"duplicate canonical candidate input: {raw_by_canonical[canonical]}")
        accepted[canonical] = resolved
        raw_by_canonical[canonical] = raw

    source_inputs: list[dict[str, Any]] = []
    candidate_nodes: list[dict[str, Any]] = []
    candidate_edges: list[dict[str, Any]] = []
    total_input_bytes = 0
    for resolved in sorted(accepted.values(), key=_path_comparison_key):
        payload = _read_bounded_file(
            resolved,
            maximum=MAX_GRAPH_BYTES - total_input_bytes,
        )
        total_input_bytes += len(payload)
        parsed = parse_json_bytes(payload, source=str(resolved))
        if type(parsed) is not dict:
            raise EvidenceGraphError(f"candidate root must be an object: {resolved}")
        _require_exact_keys(
            parsed,
            required=("schema_version", "nodes", "edges"),
            scope=f"candidate {resolved}",
        )
        if parsed["schema_version"] != CANDIDATE_SCHEMA_VERSION:
            raise EvidenceGraphError(f"unsupported candidate schema_version: {parsed['schema_version']!r}")
        if type(parsed["nodes"]) is not list or type(parsed["edges"]) is not list:
            raise EvidenceGraphError(f"candidate nodes and edges must be arrays: {resolved}")
        display = _display_path(resolved, repo_root)
        source_inputs.append(
            {"path": display, "sha256": hashlib.sha256(payload).hexdigest(), "size_bytes": len(payload)}
        )
        candidate_nodes.extend(parsed["nodes"])
        candidate_edges.extend(parsed["edges"])
    source_inputs.sort(key=lambda item: item["path"])
    return source_inputs, candidate_nodes, candidate_edges


def compile_candidate_files(
    candidate_paths: Sequence[Path | str], *, repo_root: Path | str
) -> dict[str, Any]:
    root = Path(repo_root).resolve(strict=False)
    source_inputs, raw_nodes, raw_edges = _read_candidate_files(candidate_paths, repo_root=root)
    if len(raw_nodes) > MAX_NODES:
        raise EvidenceGraphError(f"candidate node count exceeds fixed cap {MAX_NODES}")
    if len(raw_edges) > MAX_EDGES:
        raise EvidenceGraphError(f"candidate edge count exceeds fixed cap {MAX_EDGES}")

    aliases: dict[str, str] = {}
    nodes_by_id: dict[str, dict[str, Any]] = {}
    for index, raw_node in enumerate(raw_nodes):
        alias, node = _normalize_candidate_node(raw_node, scope=f"candidate.nodes[{index}]")
        if alias in aliases:
            raise EvidenceGraphError(f"duplicate candidate alias: {alias}")
        if node["id"] in nodes_by_id:
            raise EvidenceGraphError(f"duplicate canonical node identity: {node['id']}")
        aliases[alias] = node["id"]
        nodes_by_id[node["id"]] = node

    edges: list[dict[str, Any]] = []
    seen_edges: set[tuple[str, str, str]] = set()
    for index, raw_edge in enumerate(raw_edges):
        candidate = _normalize_candidate_edge(raw_edge, scope=f"candidate.edges[{index}]")
        try:
            source_id = aliases[candidate["from_alias"]]
            target_id = aliases[candidate["to_alias"]]
        except KeyError as error:
            raise EvidenceGraphError(f"candidate.edges[{index}] references missing alias {error.args[0]!r}") from error
        edge = {
            "data": candidate["data"],
            "from": source_id,
            "relation": candidate["relation"],
            "to": target_id,
        }
        key = (edge["relation"], edge["from"], edge["to"])
        if key in seen_edges:
            raise EvidenceGraphError(f"duplicate edge: {key[0]} {key[1]} -> {key[2]}")
        seen_edges.add(key)
        edges.append(edge)

    nodes = sorted(nodes_by_id.values(), key=lambda item: (item["kind"], item["id"]))
    edges.sort(key=lambda item: (item["relation"], item["from"], item["to"], canonical_sha256(item["data"])))
    _validate_caps(nodes, edges)

    graph: dict[str, Any] = {
        "edges": edges,
        "evidence_classifications": list(EVIDENCE_CLASSIFICATIONS),
        "generator": {"name": GENERATOR_NAME, "version": GENERATOR_VERSION},
        "nodes": nodes,
        "schema_version": GRAPH_SCHEMA_VERSION,
        "source_inputs": source_inputs,
        "summary": _summary(source_inputs, nodes, edges),
        "task_states": list(TASK_STATES),
    }
    graph["graph_sha256"] = canonical_sha256(graph)
    validate_graph(graph)
    return graph


def graph_json(graph: Mapping[str, Any]) -> str:
    validate_graph(graph)
    try:
        text = json.dumps(
            graph,
            allow_nan=False,
            ensure_ascii=False,
            indent=2,
            sort_keys=True,
        ) + "\n"
    except (TypeError, ValueError, RecursionError) as error:
        raise EvidenceGraphError(f"graph cannot be serialized safely: {error}") from error
    if len(text.encode("utf-8")) > MAX_GRAPH_BYTES:
        raise EvidenceGraphError(f"serialized graph exceeds the {MAX_GRAPH_BYTES}-byte cap")
    return text


def _path_comparison_key(path: Path) -> str:
    """Return one case/namespace-normalized spelling for containment checks."""

    value = os.path.normpath(str(path))
    if os.name == "nt":
        # pathlib may return an extended-length spelling only for an aliased
        # operand.  Strip the namespace marker so both operands are compared in
        # the same Windows path namespace; preserve UNC semantics.
        if value.casefold().startswith("\\\\?\\unc\\"):
            value = "\\\\" + value[8:]
        elif value.startswith("\\\\?\\"):
            value = value[4:]
    return os.path.normcase(value)


def validate_output_path(output: Path | str, *, repo_root: Path | str) -> Path:
    raw = Path(output)
    if ".." in raw.parts:
        raise EvidenceGraphError("output path must not contain parent traversal")
    if os.name == "nt":
        # Win32 trims trailing spaces and periods from ordinary path
        # components when it creates them.  Reject those spellings before any
        # filesystem resolution so a fresh clone without (for example)
        # ``qa_runs`` cannot turn ``qa_runs.`` into protected repository data
        # only after validation has completed.
        for part in raw.parts:
            if part.rstrip(" .") != part:
                raise EvidenceGraphError(
                    "output path contains a Windows-trimmed alias component"
                )
    if raw.suffix.casefold() != ".json":
        raise EvidenceGraphError("output path must end in .json")
    try:
        lexical = Path(os.path.abspath(raw))
        destination = raw.resolve(strict=False)
        root = Path(repo_root).resolve(strict=False)
    except (OSError, RuntimeError) as error:
        raise EvidenceGraphError(f"output path cannot be resolved: {error}") from error
    if _path_comparison_key(lexical) != _path_comparison_key(destination):
        raise EvidenceGraphError("output path resolves through an alias or symlink")
    if len(destination.as_posix()) > MAX_PATH_CHARS:
        raise EvidenceGraphError(f"output path exceeds {MAX_PATH_CHARS} characters")
    try:
        comparable_destination = Path(_path_comparison_key(destination))
        comparable_root = Path(_path_comparison_key(root))
        relative_parts = {
            part.casefold()
            for part in comparable_destination.relative_to(comparable_root).parts
        }
    except ValueError:
        relative_parts = set()
    forbidden = sorted(relative_parts & PROTECTED_OUTPUT_PARTS)
    if forbidden:
        raise EvidenceGraphError(f"output path is inside protected repository data: {forbidden[0]}")
    return destination


def write_graph(graph: Mapping[str, Any], output: Path | str, *, repo_root: Path | str) -> Path:
    destination = validate_output_path(output, repo_root=repo_root)
    text = graph_json(graph)
    encoded = text.encode("utf-8")
    if len(encoded) > MAX_GRAPH_BYTES:
        raise EvidenceGraphError(f"serialized graph exceeds the {MAX_GRAPH_BYTES}-byte cap")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "wb",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary.write(encoded)
            temporary.flush()
            os.fsync(temporary.fileno())
            temporary_name = temporary.name
        os.replace(temporary_name, destination)
        temporary_name = None
    finally:
        if temporary_name is not None:
            try:
                Path(temporary_name).unlink()
            except FileNotFoundError:
                pass
    return destination


def validate_artifact_view(
    view: Any, *, source_graph: Mapping[str, Any] | None = None
) -> None:
    """Validate the bounded renderer-facing schema without producing artifacts."""

    if type(view) is not dict:
        raise EvidenceGraphError("artifact view root must be an object")
    _require_exact_keys(
        view,
        required=(
            "schema_version",
            "view_id",
            "view_sha256",
            "source_graph_sha256",
            "artifact_node_id",
            "view_kind",
            "title",
            "sections",
        ),
        scope="artifact_view",
    )
    if view["schema_version"] != ARTIFACT_VIEW_SCHEMA_VERSION:
        raise EvidenceGraphError("unsupported artifact view schema_version")
    if type(view["source_graph_sha256"]) is not str or _SHA256_RE.fullmatch(view["source_graph_sha256"]) is None:
        raise EvidenceGraphError("artifact view source_graph_sha256 is invalid")
    artifact_id = view["artifact_node_id"]
    if type(artifact_id) is not str or re.fullmatch(r"artifact:[0-9a-f]{64}", artifact_id) is None:
        raise EvidenceGraphError("artifact view artifact_node_id is invalid")
    if view["view_kind"] not in {"document", "pdf", "spreadsheet", "presentation", "site", "visualization", "github"}:
        raise EvidenceGraphError("artifact view kind is unsupported")
    _require_string(view["title"], scope="artifact_view.title")
    expected_view_id = f"artifact-view:{canonical_sha256({'source_graph_sha256': view['source_graph_sha256'], 'artifact_node_id': artifact_id, 'view_kind': view['view_kind'], 'title': view['title']})}"
    if view["view_id"] != expected_view_id:
        raise EvidenceGraphError("artifact view ID does not match its canonical identity")
    sections = view["sections"]
    if type(sections) is not list or len(sections) > MAX_TASKS:
        raise EvidenceGraphError(f"artifact view sections exceed fixed cap {MAX_TASKS}")
    total_rows = 0
    section_ids: set[str] = set()
    projected_node_ids: set[str] = set()
    projected_statuses: list[tuple[str, str, str, list[str]]] = []
    for section_index, section in enumerate(sections):
        scope = f"artifact_view.sections[{section_index}]"
        if type(section) is not dict:
            raise EvidenceGraphError(f"{scope} must be an object")
        _require_exact_keys(section, required=("id", "title", "source_node_ids", "rows"), scope=scope)
        section_id = _require_string(
            section["id"], scope=f"{scope}.id", maximum=MAX_ID_CHARS, pattern=_ALIAS_RE
        )
        if section_id in section_ids:
            raise EvidenceGraphError(f"duplicate artifact view section ID: {section_id}")
        section_ids.add(section_id)
        _require_string(section["title"], scope=f"{scope}.title")
        _validate_source_node_ids(section["source_node_ids"], scope=f"{scope}.source_node_ids")
        section_sources = set(section["source_node_ids"])
        projected_node_ids.update(section_sources)
        if type(section["rows"]) is not list:
            raise EvidenceGraphError(f"{scope}.rows must be an array")
        total_rows += len(section["rows"])
        if total_rows > MAX_NODES:
            raise EvidenceGraphError(f"artifact view rows exceed fixed cap {MAX_NODES}")
        row_ids: set[str] = set()
        for row_index, row in enumerate(section["rows"]):
            row_scope = f"{scope}.rows[{row_index}]"
            if type(row) is not dict:
                raise EvidenceGraphError(f"{row_scope} must be an object")
            _require_exact_keys(
                row,
                required=("id", "source_node_ids", "cells"),
                optional=("classification", "task_state"),
                scope=row_scope,
            )
            row_id = _require_string(
                row["id"], scope=f"{row_scope}.id", maximum=MAX_ID_CHARS, pattern=_ALIAS_RE
            )
            if row_id in row_ids:
                raise EvidenceGraphError(
                    f"duplicate artifact view row ID within section {section_id}: {row_id}"
                )
            row_ids.add(row_id)
            _validate_source_node_ids(row["source_node_ids"], scope=f"{row_scope}.source_node_ids")
            row_sources = set(row["source_node_ids"])
            if not row_sources <= section_sources:
                raise EvidenceGraphError(
                    f"{row_scope}.source_node_ids must be represented by its section"
                )
            projected_node_ids.update(row_sources)
            _validate_projection_status(row, scope=row_scope)
            for status_field in ("classification", "task_state"):
                if status_field in row:
                    projected_statuses.append(
                        (row_scope, status_field, row[status_field], row["source_node_ids"])
                    )
            cells = row["cells"]
            if type(cells) is not list or not 0 < len(cells) <= MAX_EXTERNAL_ASSETS:
                raise EvidenceGraphError(f"{row_scope}.cells must contain 1..{MAX_EXTERNAL_ASSETS} cells")
            for cell_index, cell in enumerate(cells):
                cell_scope = f"{row_scope}.cells[{cell_index}]"
                if type(cell) is not dict:
                    raise EvidenceGraphError(f"{cell_scope} must be an object")
                _require_exact_keys(
                    cell,
                    required=("value", "source_node_ids"),
                    optional=("classification", "task_state"),
                    scope=cell_scope,
                )
                if type(cell["value"]) not in {str, int, float, bool, type(None)}:
                    raise EvidenceGraphError(f"{cell_scope}.value must be a JSON scalar")
                _validate_json_value(cell["value"], scope=f"{cell_scope}.value")
                _validate_source_node_ids(cell["source_node_ids"], scope=f"{cell_scope}.source_node_ids")
                cell_sources = set(cell["source_node_ids"])
                if not cell_sources <= row_sources:
                    raise EvidenceGraphError(
                        f"{cell_scope}.source_node_ids must be represented by its row"
                    )
                projected_node_ids.update(cell_sources)
                _validate_projection_status(cell, scope=cell_scope)
                for status_field in ("classification", "task_state"):
                    if status_field in cell:
                        projected_statuses.append(
                            (
                                cell_scope,
                                status_field,
                                cell[status_field],
                                cell["source_node_ids"],
                            )
                        )
    expected_hash = canonical_sha256({key: value for key, value in view.items() if key != "view_sha256"})
    if view["view_sha256"] != expected_hash:
        raise EvidenceGraphError("artifact view hash does not match canonical payload")
    if len(canonical_json_bytes(view)) > MAX_GRAPH_BYTES:
        raise EvidenceGraphError(f"artifact view exceeds the {MAX_GRAPH_BYTES}-byte cap")
    if source_graph is not None:
        validate_graph(source_graph)
        if view["source_graph_sha256"] != source_graph["graph_sha256"]:
            raise EvidenceGraphError(
                "artifact view source_graph_sha256 does not identify the supplied graph"
            )
        graph_nodes = {node["id"]: node for node in source_graph["nodes"]}
        artifact = graph_nodes.get(artifact_id)
        if artifact is None or artifact["kind"] != "artifact":
            raise EvidenceGraphError(
                "artifact view artifact_node_id does not identify an artifact in the supplied graph"
            )
        missing = sorted(projected_node_ids - set(graph_nodes))
        if missing:
            raise EvidenceGraphError(
                f"artifact view references {len(missing)} node(s) absent from the supplied graph"
            )
        for scope, status_field, projected_status, source_ids in projected_statuses:
            source_statuses = {
                graph_nodes[node_id][status_field]
                for node_id in source_ids
                if status_field in graph_nodes[node_id]
            }
            if not source_statuses:
                raise EvidenceGraphError(
                    f"{scope}.{status_field} has no matching status-bearing source node"
                )
            if source_statuses != {projected_status}:
                raise EvidenceGraphError(
                    f"{scope}.{status_field} contradicts its referenced source node status"
                )


def _validate_source_node_ids(value: Any, *, scope: str) -> None:
    if type(value) is not list or not value or len(value) > MAX_NODES:
        raise EvidenceGraphError(f"{scope} must be a non-empty bounded node-ID array")
    if value != sorted(set(value)):
        raise EvidenceGraphError(f"{scope} must be unique and canonically sorted")
    for node_id in value:
        if type(node_id) is not str or _NODE_ID_RE.fullmatch(node_id) is None:
            raise EvidenceGraphError(f"{scope} contains an invalid node ID")


def _validate_projection_status(value: Mapping[str, Any], *, scope: str) -> None:
    if "classification" in value and "task_state" in value:
        raise EvidenceGraphError(
            f"{scope} must not collapse evidence classification and task state"
        )
    if "classification" in value and value["classification"] not in EVIDENCE_CLASSIFICATIONS:
        raise EvidenceGraphError(f"{scope}.classification is invalid")
    if "task_state" in value and value["task_state"] not in TASK_STATES:
        raise EvidenceGraphError(f"{scope}.task_state is invalid")
