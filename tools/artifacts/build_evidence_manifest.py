#!/usr/bin/env python3
"""Build a deterministic, fail-closed Voxel-Native QA evidence manifest.

Only QA run directories named explicitly on the command line are inspected.
The builder never searches for a "latest" run and never recursively scans the
repository's qa_runs directory.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any, Iterable, Sequence


SCHEMA_VERSION = "1.5.0"
GENERATOR_VERSION = "1.5.0"
CURRENT_QA_REPORT_SCHEMA_VERSION = "2.5.0"
LEGACY_QA_REPORT_SCHEMA_VERSIONS = frozenset(
    {"2.0.0", "2.1.0", "2.2.0", "2.3.0", "2.4.0"}
)
CLASSIFICATIONS = ("Passed", "Observed", "Rejected", "Planned", "Blocked")
PROTECTED_OUTPUT_DIRS = ("saves", "qa_runs", "agent_runs")
MAX_REPORT_BYTES = 4 * 1024 * 1024
MAX_RON_DEPTH = 128
MAX_RON_NODES = 100_000
MAX_RON_STRING_CHARS = 16_384
HASH_CHUNK_BYTES = 1024 * 1024
EXPECTED_ROUTE_FRAME_SCOPE = "active_route_only_warmup_and_write_tail_excluded"
EXPECTED_QUANTILE_METHOD = "nearest_rank_conservative_bucket_upper_bound"
EXPECTED_CAMERA_ROUTE_POLICY = "preflight-v1"
EXPECTED_CAMERA_ROUTE_VARIANTS = 8
EXPECTED_CAMERA_ROUTE_VALIDATION_SAMPLES = 16
EXPECTED_CAMERA_ROUTE_VOXEL_QUERY_CAP = 153_600
EXPECTED_DENSE_CHUNK_BUDGET = 2_400
PREFLIGHT_CAMERA_FOCUSES = frozenset({"river", "lava", "near-far"})
OBSOLETE_CAMERA_COLUMN_FIELDS = frozenset(
    {
        "camera_route_required_columns",
        "camera_route_loaded_columns",
        "camera_route_unloaded_columns",
    }
)
OBSOLETE_CAMERA_ROUTE_FIELDS = OBSOLETE_CAMERA_COLUMN_FIELDS | frozenset(
    {
        "camera_route_body_occlusions",
        "camera_route_los_occlusions",
        "camera_route_required_chunks",
        "camera_route_loaded_chunks",
        "camera_route_unloaded_chunks",
    }
)

RUN_IDENTITY_FIELDS = (
    "package_version",
    "build_profile",
    "instance_label",
    "world_name",
    "world_seed",
    "world_profile",
    "scenery_quality",
    "terrain_grammar",
    "git_sha",
    "git_dirty",
    "source_fingerprint",
    "executable_hash",
    "toolchain",
    "hardware",
)
WORLD_EDIT_STORE_FIELDS = (
    "world_edit_store_status",
    "world_edit_store_compatible",
    "world_edit_store_seed",
    "world_edit_store_profile",
    "world_edit_store_scenery_quality",
    "world_edit_store_terrain_grammar",
    "world_edit_store_edited_chunks",
    "world_edit_store_block_reason_code",
)
VIEWPORT_FIELDS = (
    "logical_width",
    "logical_height",
    "physical_width",
    "physical_height",
    "scale_factor",
    "dpi_percent",
)
ROUTE_FIELDS = (
    "requested_route_focus",
    "resolved_route_focus",
    "route_focus_available",
    "route_focus_unavailable_reason",
    "route_focus_anchor",
    "route_focus_search_visited_candidates",
    "route_focus_classification_queries",
    "route_focus_search_candidate_cap",
    "route_focus_classification_query_cap",
    "route_focus_search_cap_exhausted",
    "camera_route_policy",
    "camera_route_preflight_applicable",
    "camera_route_plan_hash",
    "camera_route_available",
    "camera_route_unavailable_reason",
    "camera_route_variant_index",
    "camera_route_variant_count",
    "camera_route_validation_samples",
    "camera_route_voxel_queries",
    "camera_route_voxel_query_cap",
    "camera_route_required_chunk_checks",
    "camera_route_loaded_chunk_checks",
    "camera_route_proven_air_chunk_checks",
    "camera_route_unloaded_chunk_checks",
    "camera_route_candidate_body_occlusions",
    "camera_route_candidate_los_occlusions",
    "camera_route_selected_clear_samples",
    "camera_route_minimum_clearance_voxels",
    "camera_route_work_cap_exhausted",
    "requested_route_distance_m",
    "max_horizontal_displacement_m",
    "requested_duration_seconds",
    "duration_seconds",
    "warmup_seconds",
    "write_tail_seconds",
    "frames",
    "average_fps",
    "max_frame_ms",
    "final_smoothed_fps",
    "loaded_chunks",
    "pending_terrain",
    "pending_meshes",
    "dirty_chunks",
    "dense_chunks",
    "dense_chunk_budget",
    "dense_chunk_budget_exceeded",
    "frontier_complete",
    "peak_loaded_chunks",
    "peak_pending_terrain",
    "peak_dense_chunks",
)
ROUTE_FRAME_TIME_FIELDS = (
    "scope",
    "sample_count",
    "excluded_warmup_sample_count",
    "excluded_write_tail_sample_count",
    "rejected_sample_count",
    "rejected_non_finite_sample_count",
    "rejected_non_positive_sample_count",
    "rejected_huge_sample_count",
    "rejected_arithmetic_overflow_sample_count",
    "histogram_overflow_sample_count",
    "histogram_bucket_count",
    "histogram_bucket_width_ms",
    "histogram_exact_max_ms",
    "accepted_sample_max_ms",
    "quantile_method",
    "quantile_values_are_bucket_upper_bounds",
    "quantile_max_error_ms",
    "mean_sample_rounding_max_error_ms",
    "quantiles_complete",
    "measurement_valid",
    "mean_ms",
    "median_ms",
    "p95_ms",
    "p99_ms",
    "max_ms",
    "accumulator_bytes",
    "quantile_scan_work_cap",
)
PLANETARY_LIVE_FIELDS = (
    "enabled",
    "profile",
    "interaction_radius_metres",
    "confirmed_near_extent_metres",
    "near_coverage_ready_columns",
    "near_coverage_hidden_cells",
    "far_radius_metres",
    "resident_entities",
    "resident_vertices",
    "resident_indices",
    "ring_vertices",
    "ring_indices",
    "resident_mesh_bytes",
    "resident_fluid_entities",
    "resident_fluid_vertices",
    "resident_fluid_indices",
    "resident_water_indices",
    "resident_lava_indices",
    "fluid_ring_vertices",
    "fluid_ring_indices",
    "water_ring_indices",
    "lava_ring_indices",
    "resident_fluid_mesh_bytes",
    "resident_semantic_cohort_entities",
    "resident_semantic_cohort_vertices",
    "resident_semantic_cohort_indices",
    "resident_semantic_cohort_mesh_bytes",
    "resident_semantic_cohort_count",
    "resident_semantic_cohort_kind_counts",
    "live_sample_cache_windows",
    "live_sample_cache_bytes",
)
PLANETARY_BUDGET_FIELDS = (
    "budget_entities",
    "budget_vertices",
    "budget_indices",
    "budget_mesh_bytes",
    "budget_build_jobs",
    "budget_ring_build_bytes",
    "budget_sample_cache_bytes",
    "budget_coverage_work_bytes",
    "budget_fluid_entities",
    "budget_fluid_vertices",
    "budget_fluid_indices",
    "budget_fluid_mesh_bytes",
    "budget_fluid_ring_build_bytes",
    "budget_hydro_atomic_ring_build_bytes",
    "budget_atomic_ring_build_bytes",
    "budget_semantic_cohort_entities",
    "budget_semantic_cohort_vertices",
    "budget_semantic_cohort_indices",
    "budget_semantic_cohort_mesh_bytes",
    "budget_semantic_cohort_hash_scans",
    "budget_semantic_cohort_height_queries",
    "budget_semantic_cohort_biome_queries",
)
PLANETARY_TELEMETRY_FIELDS = (
    "desired_terrain_grammar",
    "active_terrain_grammar",
    "pending_rebuilds",
    "dirty_mask",
    "build_in_flight",
    "update_cadence_frames",
    "material_detail",
    "desired_material_detail",
    "resident_material_detail",
    "resident_detailed_levels",
    "resident_reduced_levels",
    "surface_material_mode",
    "hydro_mode",
    "semantic_cohort_mode",
    "scheduler_deferred_frames",
    "completed_rebuilds",
    "stale_builds_discarded",
    "budget_rejections",
    "last_build_ms",
    "max_build_ms",
    "last_height_queries",
    "last_material_slope_queries",
    "last_bridge_v2_cell_reuses",
    "last_fluid_classification_queries",
    "last_fluid_biome_queries",
    "last_fluid_vertices",
    "last_fluid_indices",
    "last_water_indices",
    "last_lava_indices",
    "peak_live_sample_cache_windows",
    "peak_live_sample_cache_bytes",
    "scheduler_resident_entities",
    "scheduler_resident_vertices",
    "scheduler_resident_indices",
    "scheduler_resident_mesh_bytes",
    "scheduler_resident_fluid_entities",
    "scheduler_resident_fluid_vertices",
    "scheduler_resident_fluid_indices",
    "scheduler_resident_fluid_mesh_bytes",
    "scheduler_resident_water_indices",
    "scheduler_resident_lava_indices",
    "scheduler_resident_semantic_cohort_entities",
    "scheduler_resident_semantic_cohort_vertices",
    "scheduler_resident_semantic_cohort_indices",
    "scheduler_resident_semantic_cohort_mesh_bytes",
    "scheduler_resident_semantic_cohort_count",
    "scheduler_ring_vertices",
    "scheduler_ring_indices",
    "scheduler_fluid_ring_vertices",
    "scheduler_fluid_ring_indices",
    "scheduler_water_ring_indices",
    "scheduler_lava_ring_indices",
    "scheduler_resident_semantic_cohort_kind_counts",
    "resident_observation_valid",
    "resident_entity_count_overflow",
    "resident_duplicate_levels",
    "resident_out_of_range_levels",
    "resident_scheduler_mismatch",
    "resident_budget_exceeded",
    "resident_observation_rejections",
    "resident_fluid_observation_valid",
    "resident_fluid_kind_integrity_valid",
    "resident_fluid_entity_count_overflow",
    "resident_fluid_duplicate_slots",
    "resident_fluid_out_of_range_levels",
    "resident_fluid_scheduler_mismatch",
    "resident_fluid_budget_exceeded",
    "resident_fluid_observation_rejections",
    "resident_semantic_cohort_observation_valid",
    "resident_semantic_cohort_payload_integrity_valid",
    "resident_semantic_cohort_entity_count_overflow",
    "resident_semantic_cohort_scheduler_mismatch",
    "resident_semantic_cohort_budget_exceeded",
    "resident_semantic_cohort_observation_rejections",
    "last_semantic_cohort_hash_scans",
    "last_semantic_cohort_height_queries",
    "last_semantic_cohort_biome_queries",
    "last_semantic_cohort_candidates",
    "last_semantic_cohort_emitted",
    "last_semantic_cohort_vertices",
    "last_semantic_cohort_indices",
    "last_semantic_cohort_kind_counts",
    "last_biome_queries",
    "last_reused_height_samples",
    "last_reused_biome_samples",
    "last_cache_shift_x_cells",
    "last_cache_shift_z_cells",
    "last_cache_update",
    "incremental_strip_rebuilds",
    "full_cache_rebuilds",
    "teleport_fallbacks",
    "last_clamped_queries",
    "camera_world_x",
    "camera_world_z",
)
PLANETARY_FAR_FIELD_LEVELS = 6
PLANETARY_SEMANTIC_COHORT_KIND_COUNT = 6
PLANETARY_SEMANTIC_COHORT_VERTICES_PER_COHORT = 24
PLANETARY_SEMANTIC_COHORT_INDICES_PER_COHORT = 36
PLANETARY_VERTEX_BYTES = 48
PLANETARY_INDEX_BYTES = 4
PLANETARY_EXPECTED_HYDRO_ATOMIC_RING_BUILD_BYTES = 653_008
PLANETARY_EXPECTED_ATOMIC_RING_BUILD_BYTES = 757_984
PLANETARY_EXPECTED_SEMANTIC_COHORT_BUDGETS = {
    "budget_semantic_cohort_entities": 1,
    "budget_semantic_cohort_vertices": 1_944,
    "budget_semantic_cohort_indices": 2_916,
    "budget_semantic_cohort_mesh_bytes": 104_976,
    "budget_semantic_cohort_hash_scans": 3_721,
    "budget_semantic_cohort_height_queries": 81,
    "budget_semantic_cohort_biome_queries": 81,
}

_NUMBER_RE = re.compile(
    r"[+-]?(?:\d(?:_?\d)*(?:\.\d(?:_?\d)*)?(?:[eE][+-]?\d(?:_?\d)*)?)"
)
_IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


class EvidenceManifestError(ValueError):
    """Raised for unsafe CLI/output conditions that prevent any manifest."""


class RonParseError(ValueError):
    """Raised when a report is outside the supported, bounded RON subset."""


class RonParser:
    """Small non-executing RON parser for serde-generated QA reports."""

    def __init__(self, text: str) -> None:
        self.text = text
        self.pos = 0
        self.nodes = 0

    def parse(self) -> Any:
        value = self._value(0)
        self._skip_trivia()
        if self.pos != len(self.text):
            self._fail("unexpected trailing input")
        return value

    def _value(self, depth: int) -> Any:
        if depth > MAX_RON_DEPTH:
            self._fail("maximum nesting depth exceeded")
        self.nodes += 1
        if self.nodes > MAX_RON_NODES:
            self._fail("maximum parsed node count exceeded")
        self._skip_trivia()
        if self.pos >= len(self.text):
            self._fail("expected value")

        char = self.text[self.pos]
        if char == '"':
            return self._string()
        if char == "(":
            return self._paren(depth + 1)
        if char == "[":
            return self._list(depth + 1)
        if char == "{":
            return self._map(depth + 1)
        if self.text.startswith("-inf", self.pos):
            self.pos += 4
            return float("-inf")
        if char in "+-." or char.isdigit():
            return self._number()
        if char.isalpha() or char == "_":
            return self._identifier_value(depth + 1)
        self._fail(f"unsupported value starting with {char!r}")

    def _paren(self, depth: int) -> Any:
        self._expect("(")
        self._skip_trivia()
        if self._take(")"):
            return []

        saved = self.pos
        field_name = self._try_field_name()
        if field_name is not None:
            self._skip_trivia()
        is_struct = field_name is not None and self._take(":")
        self.pos = saved

        if is_struct:
            result: dict[str, Any] = {}
            while True:
                name = self._field_name()
                self._skip_trivia()
                self._expect(":")
                if name in result:
                    self._fail(f"duplicate field {name!r}")
                result[name] = self._value(depth)
                self._skip_trivia()
                if self._take(")"):
                    return result
                self._expect(",")
                self._skip_trivia()
                if self._take(")"):
                    return result

        values: list[Any] = []
        while True:
            values.append(self._value(depth))
            self._skip_trivia()
            if self._take(")"):
                return values
            self._expect(",")
            self._skip_trivia()
            if self._take(")"):
                return values

    def _list(self, depth: int) -> list[Any]:
        self._expect("[")
        result: list[Any] = []
        self._skip_trivia()
        if self._take("]"):
            return result
        while True:
            result.append(self._value(depth))
            self._skip_trivia()
            if self._take("]"):
                return result
            self._expect(",")
            self._skip_trivia()
            if self._take("]"):
                return result

    def _map(self, depth: int) -> dict[str, Any]:
        self._expect("{")
        result: dict[str, Any] = {}
        self._skip_trivia()
        if self._take("}"):
            return result
        while True:
            key = self._value(depth)
            if not isinstance(key, (str, int)) or isinstance(key, bool):
                self._fail("map key must be a string or integer")
            key_text = str(key)
            self._skip_trivia()
            self._expect(":")
            if key_text in result:
                self._fail(f"duplicate map key {key_text!r}")
            result[key_text] = self._value(depth)
            self._skip_trivia()
            if self._take("}"):
                return result
            self._expect(",")
            self._skip_trivia()
            if self._take("}"):
                return result

    def _identifier_value(self, depth: int) -> Any:
        identifier = self._identifier()
        lowered = identifier.lower()
        if identifier == "true":
            return True
        if identifier == "false":
            return False
        if identifier == "None":
            return None
        if lowered == "nan":
            return float("nan")
        if lowered in {"inf", "+inf"}:
            return float("inf")

        self._skip_trivia()
        if not self._take("("):
            return identifier

        self._skip_trivia()
        if self._take(")"):
            payload: Any = []
        else:
            values: list[Any] = []
            while True:
                values.append(self._value(depth))
                self._skip_trivia()
                if self._take(")"):
                    break
                self._expect(",")
                self._skip_trivia()
                if self._take(")"):
                    break
            payload = values[0] if len(values) == 1 else values

        if identifier == "Some":
            if isinstance(payload, list) and not payload:
                self._fail("Some requires exactly one value")
            return payload
        return {"__ron_variant__": identifier, "value": payload}

    def _string(self) -> str:
        self._expect('"')
        result: list[str] = []
        while self.pos < len(self.text):
            char = self.text[self.pos]
            self.pos += 1
            if char == '"':
                value = "".join(result)
                if len(value) > MAX_RON_STRING_CHARS:
                    self._fail("maximum string length exceeded")
                return value
            if char != "\\":
                if ord(char) < 0x20:
                    self._fail("literal control character in string")
                result.append(char)
                continue
            if self.pos >= len(self.text):
                self._fail("unterminated escape")
            escape = self.text[self.pos]
            self.pos += 1
            simple = {
                '"': '"',
                "\\": "\\",
                "n": "\n",
                "r": "\r",
                "t": "\t",
                "0": "\0",
            }
            if escape in simple:
                result.append(simple[escape])
            elif escape == "x":
                digits = self.text[self.pos : self.pos + 2]
                if len(digits) != 2 or not all(c in "0123456789abcdefABCDEF" for c in digits):
                    self._fail("invalid hexadecimal string escape")
                self.pos += 2
                result.append(chr(int(digits, 16)))
            elif escape == "u":
                self._expect("{")
                end = self.text.find("}", self.pos)
                if end < 0:
                    self._fail("unterminated Unicode escape")
                digits = self.text[self.pos : end]
                if not 1 <= len(digits) <= 6 or not all(
                    c in "0123456789abcdefABCDEF" for c in digits
                ):
                    self._fail("invalid Unicode escape")
                self.pos = end + 1
                result.append(chr(int(digits, 16)))
            else:
                self._fail(f"unsupported string escape \\{escape}")
        self._fail("unterminated string")

    def _number(self) -> int | float:
        match = _NUMBER_RE.match(self.text, self.pos)
        if match is None:
            self._fail("invalid number")
        token = match.group(0)
        self.pos = match.end()
        clean = token.replace("_", "")
        try:
            if any(marker in clean for marker in ".eE"):
                return float(clean)
            return int(clean, 10)
        except ValueError as error:
            raise RonParseError(f"invalid number {token!r} at offset {self.pos}") from error

    def _field_name(self) -> str:
        name = self._try_field_name()
        if name is None:
            self._fail("expected field name")
        return name

    def _try_field_name(self) -> str | None:
        self._skip_trivia()
        if self.pos >= len(self.text):
            return None
        if self.text[self.pos] == '"':
            return self._string()
        match = _IDENTIFIER_RE.match(self.text, self.pos)
        if match is None:
            return None
        self.pos = match.end()
        return match.group(0)

    def _identifier(self) -> str:
        match = _IDENTIFIER_RE.match(self.text, self.pos)
        if match is None:
            self._fail("expected identifier")
        self.pos = match.end()
        return match.group(0)

    def _skip_trivia(self) -> None:
        while True:
            while self.pos < len(self.text) and self.text[self.pos].isspace():
                self.pos += 1
            if self.text.startswith("//", self.pos):
                newline = self.text.find("\n", self.pos + 2)
                self.pos = len(self.text) if newline < 0 else newline + 1
                continue
            if self.text.startswith("/*", self.pos):
                end = self.text.find("*/", self.pos + 2)
                if end < 0:
                    self._fail("unterminated block comment")
                self.pos = end + 2
                continue
            return

    def _expect(self, token: str) -> None:
        self._skip_trivia()
        if not self.text.startswith(token, self.pos):
            self._fail(f"expected {token!r}")
        self.pos += len(token)

    def _take(self, token: str) -> bool:
        if self.text.startswith(token, self.pos):
            self.pos += len(token)
            return True
        return False

    def _fail(self, message: str) -> None:
        raise RonParseError(f"{message} at offset {self.pos}")


def utc_now_text() -> str:
    return (
        dt.datetime.now(dt.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def display_path(path: Path, repo_root: Path) -> str:
    try:
        resolved = path.resolve(strict=False)
    except (OSError, RuntimeError):
        resolved = Path(os.path.abspath(path))
    try:
        return resolved.relative_to(repo_root.resolve()).as_posix()
    except (OSError, RuntimeError, ValueError):
        return "external"


def path_is_within(path: Path, parent: Path) -> bool:
    try:
        path.resolve(strict=False).relative_to(parent.resolve(strict=False))
        return True
    except (OSError, RuntimeError, ValueError):
        return False


def lexical_path_is_within(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def validate_output_path(output: Path, repo_root: Path) -> Path:
    lexical = Path(os.path.abspath(output))
    try:
        resolved = output.resolve(strict=False)
    except (OSError, RuntimeError) as error:
        raise EvidenceManifestError(f"output path could not be resolved safely: {error}") from error
    for directory in PROTECTED_OUTPUT_DIRS:
        lexical_protected = Path(os.path.abspath(repo_root / directory))
        try:
            protected = (repo_root / directory).resolve(strict=False)
        except (OSError, RuntimeError) as error:
            raise EvidenceManifestError(
                f"protected output boundary {directory!r} could not be resolved safely: {error}"
            ) from error
        if lexical_path_is_within(lexical, lexical_protected) or path_is_within(
            resolved, protected
        ):
            raise EvidenceManifestError(
                f"output path must not be inside protected directory {directory!r}"
            )
    if resolved.exists() and resolved.is_dir():
        raise EvidenceManifestError("output path names an existing directory")
    if resolved.suffix.lower() != ".json":
        raise EvidenceManifestError("output path must have a .json suffix")
    return resolved


def hash_file(path: Path) -> dict[str, Any]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while True:
            chunk = source.read(HASH_CHUNK_BYTES)
            if not chunk:
                break
            size += len(chunk)
            digest.update(chunk)
    return {"sha256": digest.hexdigest(), "size_bytes": size}


def hash_and_capture_file(
    path: Path, *, capture_limit: int
) -> tuple[dict[str, Any], bytes | None]:
    """Hash one byte stream while retaining at most capture_limit bytes."""

    digest = hashlib.sha256()
    size = 0
    captured: bytearray | None = bytearray()
    with path.open("rb") as source:
        while True:
            chunk = source.read(HASH_CHUNK_BYTES)
            if not chunk:
                break
            size += len(chunk)
            digest.update(chunk)
            if captured is not None:
                if size <= capture_limit:
                    captured.extend(chunk)
                else:
                    captured = None
    return {
        "sha256": digest.hexdigest(),
        "size_bytes": size,
    }, bytes(captured) if captured is not None else None


def hash_and_probe_png(path: Path) -> tuple[dict[str, Any], bool]:
    """Hash and perform bounded PNG boundary checks over the same byte stream."""

    digest = hashlib.sha256()
    size = 0
    signature = bytearray()
    tail = bytearray()
    with path.open("rb") as source:
        while True:
            chunk = source.read(HASH_CHUNK_BYTES)
            if not chunk:
                break
            size += len(chunk)
            digest.update(chunk)
            if len(signature) < 8:
                signature.extend(chunk[: 8 - len(signature)])
            tail.extend(chunk)
            if len(tail) > 12:
                del tail[:-12]
    complete = (
        size >= 20
        and bytes(signature) == b"\x89PNG\r\n\x1a\n"
        and bytes(tail[:8]) == b"\x00\x00\x00\x00IEND"
    )
    return {"sha256": digest.hexdigest(), "size_bytes": size}, complete


def issue(
    code: str,
    classification: str,
    field: str,
    message: str,
) -> dict[str, str]:
    if classification not in CLASSIFICATIONS:
        raise AssertionError(f"unsupported classification: {classification}")
    return {
        "classification": classification,
        "code": code,
        "field": field,
        "message": message,
    }


def claim(
    claim_id: str,
    classification: str,
    statement: str,
    evidence: Iterable[str] = (),
) -> dict[str, Any]:
    if classification not in CLASSIFICATIONS:
        raise AssertionError(f"unsupported classification: {classification}")
    return {
        "classification": classification,
        "evidence": sorted(set(evidence)),
        "id": claim_id,
        "statement": statement,
    }


def classification_priority(classification: str) -> int:
    return {
        "Observed": 0,
        "Passed": 0,
        "Planned": 1,
        "Blocked": 2,
        "Rejected": 3,
    }[classification]


def aggregate_classification(classifications: Iterable[str]) -> str:
    values = list(classifications)
    if not values:
        return "Blocked"
    worst = max(values, key=classification_priority)
    if worst in {"Rejected", "Blocked", "Planned"}:
        return worst
    return "Observed"


def find_non_finite(value: Any, path: str = "$") -> list[str]:
    found: list[str] = []
    if isinstance(value, float) and not math.isfinite(value):
        found.append(path)
    elif isinstance(value, dict):
        for key in sorted(value):
            found.extend(find_non_finite(value[key], f"{path}.{key}"))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            found.extend(find_non_finite(item, f"{path}[{index}]"))
    return found


def is_finite_number(value: Any, *, minimum: float | None = None) -> bool:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return False
    if not math.isfinite(float(value)):
        return False
    return minimum is None or float(value) >= minimum


def is_non_negative_integer(value: Any) -> bool:
    return type(value) is int and value >= 0


def is_bounded_text(value: Any, max_chars: int) -> bool:
    return (
        isinstance(value, str)
        and bool(value.strip())
        and len(value) <= max_chars
        and all(character.isprintable() for character in value)
    )


def is_provenance_token(value: Any, max_chars: int) -> bool:
    return is_bounded_text(value, max_chars) and all(
        character.isascii()
        and (character.isalnum() or character in "-_.:")
        for character in value
    )


def copy_known_fields(source: dict[str, Any], fields: Sequence[str]) -> dict[str, Any]:
    return {field: source.get(field) for field in fields}


def validate_run_identity(
    report: dict[str, Any], run_key: str, *, require_current: bool
) -> tuple[dict[str, Any] | None, dict[str, Any], list[dict[str, str]]]:
    problems: list[dict[str, str]] = []
    identity = report.get("run_identity")
    if not isinstance(identity, dict):
        problems.append(
            issue(
                "missing_run_identity",
                "Blocked",
                "run_identity",
                "report does not contain a structured run identity",
            )
        )
        return None, claim(
            f"{run_key}:run_identity",
            "Blocked",
            "Run provenance is incomplete.",
        ), problems

    output = copy_known_fields(identity, RUN_IDENTITY_FIELDS)
    invalid = False
    blocked = False
    package_version = identity.get("package_version")
    if not is_bounded_text(package_version, 128):
        blocked = True
        problems.append(
            issue(
                "missing_package_version",
                "Blocked",
                "run_identity.package_version",
                "package version is required",
            )
        )
    build_profile = identity.get("build_profile")
    if build_profile is None:
        blocked = True
        problems.append(
            issue(
                "legacy_missing_build_profile",
                "Blocked",
                "run_identity.build_profile",
                "legacy report cannot prove whether the executable was debug or release",
            )
        )
    elif build_profile not in {"debug", "release"}:
        invalid = True
        problems.append(
            issue(
                "invalid_build_profile",
                "Rejected",
                "run_identity.build_profile",
                "build profile must be derived as debug or release",
            )
        )

    for field in ("instance_label", "world_name", "world_profile", "scenery_quality"):
        value = identity.get(field)
        if value is not None and not is_bounded_text(value, 256):
            invalid = True
            problems.append(
                issue(
                    "invalid_identity_field",
                    "Rejected",
                    f"run_identity.{field}",
                    "optional identity text must be a string when present",
                )
            )
    if identity.get("world_seed") is not None and not is_non_negative_integer(
        identity.get("world_seed")
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_world_seed",
                "Rejected",
                "run_identity.world_seed",
                "world seed must be a non-negative integer",
            )
        )
    terrain_grammar = identity.get("terrain_grammar")
    if terrain_grammar is None:
        blocked = not require_current
        invalid = invalid or require_current
        problems.append(
            issue(
                "missing_terrain_grammar",
                "Rejected" if require_current else "Blocked",
                "run_identity.terrain_grammar",
                "current evidence must bind the procedural world to an explicit V1, V2, or V3 terrain grammar",
            )
        )
    elif terrain_grammar not in {"V1", "V2", "V3"}:
        invalid = True
        problems.append(
            issue(
                "invalid_terrain_grammar",
                "Rejected",
                "run_identity.terrain_grammar",
                "terrain grammar must be exactly V1, V2, or V3",
            )
        )
    if identity.get("git_dirty") is not None and type(identity.get("git_dirty")) is not bool:
        invalid = True
        problems.append(
            issue(
                "invalid_git_dirty",
                "Rejected",
                "run_identity.git_dirty",
                "git dirty provenance must be a boolean",
            )
        )
    git_sha = identity.get("git_sha")
    if git_sha is not None and (
        not isinstance(git_sha, str)
        or re.fullmatch(r"[0-9A-Fa-f]{7,64}", git_sha) is None
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_git_sha",
                "Rejected",
                "run_identity.git_sha",
                "git SHA must contain 7 to 64 hexadecimal characters",
            )
        )
    for field in ("source_fingerprint", "executable_hash"):
        value = identity.get(field)
        if value is not None and not is_provenance_token(value, 128):
            invalid = True
            problems.append(
                issue(
                    "invalid_provenance_field",
                    "Rejected",
                    f"run_identity.{field}",
                    "fingerprint must be a bounded ASCII provenance token",
                )
            )
    for field, max_chars in (("toolchain", 160), ("hardware", 320)):
        value = identity.get(field)
        if value is not None and not is_bounded_text(value, max_chars):
            invalid = True
            problems.append(
                issue(
                    "invalid_provenance_text",
                    "Rejected",
                    f"run_identity.{field}",
                    "provenance text is empty, contains control characters, or exceeds its cap",
                )
            )

    classification = "Rejected" if invalid else "Blocked" if blocked else "Passed"
    statement = (
        "Run identity and build profile are structurally valid."
        if classification == "Passed"
        else "Run identity cannot support a current provenance claim."
    )
    return output, claim(f"{run_key}:run_identity", classification, statement), problems


def validate_world_edit_store(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, str]]]:
    output = copy_known_fields(report, WORLD_EDIT_STORE_FIELDS)
    problems: list[dict[str, str]] = []
    invalid = False
    blocked = False

    identity = report.get("run_identity")
    expected = {
        "world_edit_store_seed": identity.get("world_seed") if isinstance(identity, dict) else None,
        "world_edit_store_profile": identity.get("world_profile") if isinstance(identity, dict) else None,
        "world_edit_store_scenery_quality": identity.get("scenery_quality")
        if isinstance(identity, dict)
        else None,
        "world_edit_store_terrain_grammar": identity.get("terrain_grammar")
        if isinstance(identity, dict)
        else None,
    }
    status = report.get("world_edit_store_status")
    compatible = report.get("world_edit_store_compatible")
    edited_chunks = report.get("world_edit_store_edited_chunks")
    reason_code = report.get("world_edit_store_block_reason_code")

    if status not in {"unchecked", "compatible", "blocked"} or type(compatible) is not bool:
        invalid = True
        problems.append(
            issue(
                "invalid_world_edit_store_status",
                "Rejected",
                "world_edit_store_status",
                "edit-store status must use the closed unchecked/compatible/blocked contract with an explicit compatibility boolean",
            )
        )
    elif status == "compatible":
        if compatible is not True or not is_non_negative_integer(edited_chunks) or reason_code is not None:
            invalid = True
            problems.append(
                issue(
                    "inconsistent_compatible_world_edit_store",
                    "Rejected",
                    "world_edit_store_status",
                    "a compatible edit store requires compatible=true, a non-negative edited-chunk count, and no block reason",
                )
            )
    elif status == "blocked":
        blocked = True
        if compatible is not False or edited_chunks is not None or not is_provenance_token(reason_code, 64):
            invalid = True
            problems.append(
                issue(
                    "inconsistent_blocked_world_edit_store",
                    "Rejected",
                    "world_edit_store_status",
                    "a blocked edit store requires compatible=false, no edited-chunk count, and one bounded reason code",
                )
            )
    elif status == "unchecked":
        blocked = True
        if compatible is not False or edited_chunks is not None or reason_code is not None:
            invalid = True
            problems.append(
                issue(
                    "inconsistent_unchecked_world_edit_store",
                    "Rejected",
                    "world_edit_store_status",
                    "an unchecked edit store requires an empty fail-closed sentinel",
                )
            )

    for field, expected_value in expected.items():
        actual = report.get(field)
        if status == "unchecked":
            if actual is not None:
                invalid = True
                problems.append(
                    issue(
                        "unchecked_world_edit_store_has_identity",
                        "Rejected",
                        field,
                        "an unchecked edit store must not claim a generation identity",
                    )
                )
        elif actual != expected_value or expected_value is None:
            invalid = True
            problems.append(
                issue(
                    "world_edit_store_identity_mismatch",
                    "Rejected",
                    field,
                    "edit-store generation identity must exactly match the immutable run identity",
                )
            )

    classification = "Rejected" if invalid else "Blocked" if blocked else "Passed"
    statement = (
        "Edited voxel snapshots are bound to the exact world-generation identity."
        if classification == "Passed"
        else "Edited voxel snapshot compatibility does not authorize current evidence."
    )
    return output, claim(f"{run_key}:world_edit_store", classification, statement), problems


def validate_viewport(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any] | None, dict[str, Any], list[dict[str, str]]]:
    viewport = report.get("viewport")
    if not isinstance(viewport, dict):
        problem = issue(
            "missing_viewport",
            "Blocked",
            "viewport",
            "viewport evidence is absent or not structured",
        )
        return None, claim(
            f"{run_key}:viewport", "Blocked", "Viewport coverage cannot be established."
        ), [problem]

    output = copy_known_fields(viewport, VIEWPORT_FIELDS)
    invalid_fields = [
        field
        for field in ("logical_width", "logical_height", "scale_factor", "dpi_percent")
        if not is_finite_number(viewport.get(field), minimum=0.000001)
    ]
    invalid_fields.extend(
        field
        for field in ("physical_width", "physical_height")
        if type(viewport.get(field)) is not int or viewport[field] <= 0
    )
    if invalid_fields:
        problems = [
            issue(
                "invalid_viewport_field",
                "Rejected",
                f"viewport.{field}",
                "viewport dimensions and scale must retain their positive finite numeric types",
            )
            for field in invalid_fields
        ]
        return output, claim(
            f"{run_key}:viewport", "Rejected", "Viewport evidence is invalid."
        ), problems

    logical_width = float(viewport["logical_width"])
    logical_height = float(viewport["logical_height"])
    scale_factor = float(viewport["scale_factor"])
    dpi_percent = float(viewport["dpi_percent"])
    inconsistent_fields: list[str] = []
    if abs(logical_width * scale_factor - viewport["physical_width"]) > 1.5:
        inconsistent_fields.append("physical_width")
    if abs(logical_height * scale_factor - viewport["physical_height"]) > 1.5:
        inconsistent_fields.append("physical_height")
    if abs(scale_factor * 100.0 - dpi_percent) > 0.01:
        inconsistent_fields.append("dpi_percent")
    if inconsistent_fields:
        return output, claim(
            f"{run_key}:viewport", "Rejected", "Viewport evidence is internally inconsistent."
        ), [
            issue(
                "inconsistent_viewport_geometry",
                "Rejected",
                f"viewport.{field}",
                "logical, physical, scale-factor, and DPI values do not describe one viewport",
            )
            for field in inconsistent_fields
        ]
    return output, claim(
        f"{run_key}:viewport", "Passed", "Viewport dimensions and DPI are present and valid."
    ), []


def validate_camera_route(
    report: dict[str, Any], run_key: str
) -> tuple[bool, bool, list[dict[str, str]]]:
    """Validate the schema-2.3 camera-route preflight as an acceptance binding.

    Candidate occlusion counters are diagnostics across rejected variants.
    Acceptance is bound separately to all selected-route samples being clear.
    """

    invalid = False
    blocked = False
    problems: list[dict[str, str]] = []
    policy = report.get("camera_route_policy")
    requested_focus = report.get("requested_route_focus")
    requires_plan = requested_focus in PREFLIGHT_CAMERA_FOCUSES
    preflight_applicable = report.get("camera_route_preflight_applicable")
    available = report.get("camera_route_available")
    reason = report.get("camera_route_unavailable_reason")
    plan_hash = report.get("camera_route_plan_hash")
    variant_index = report.get("camera_route_variant_index")
    minimum_clearance = report.get("camera_route_minimum_clearance_voxels")
    work_cap_exhausted = report.get("camera_route_work_cap_exhausted")
    counter_names = (
        "camera_route_variant_count",
        "camera_route_validation_samples",
        "camera_route_voxel_queries",
        "camera_route_voxel_query_cap",
        "camera_route_required_chunk_checks",
        "camera_route_loaded_chunk_checks",
        "camera_route_proven_air_chunk_checks",
        "camera_route_unloaded_chunk_checks",
        "camera_route_candidate_body_occlusions",
        "camera_route_candidate_los_occlusions",
        "camera_route_selected_clear_samples",
    )
    counters = {name: report.get(name) for name in counter_names}

    for name in sorted(OBSOLETE_CAMERA_ROUTE_FIELDS.intersection(report)):
        invalid = True
        problems.append(
            issue(
                "obsolete_camera_route_field",
                "Rejected",
                name,
                "schema 2.3 rejects obsolete column-only or ambiguous occlusion telemetry names",
            )
        )

    if policy not in {EXPECTED_CAMERA_ROUTE_POLICY, "legacy"}:
        invalid = True
        problems.append(
            issue(
                "invalid_camera_route_policy",
                "Rejected",
                "camera_route_policy",
                "schema 2.3 camera policy must be preflight-v1 or the explicit blocked legacy policy",
            )
        )
    if (
        type(preflight_applicable) is not bool
        or type(available) is not bool
        or type(work_cap_exhausted) is not bool
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_camera_route_state",
                "Rejected",
                "camera_route_available",
                "camera-route applicability, availability, and work-cap exhaustion must be booleans",
            )
        )
    allowed_reasons = {
        "camera-route-focus-unavailable",
        "camera-route-chunks-unloaded",
        "camera-route-body-occluded",
        "camera-route-los-occluded",
        "camera-route-work-cap",
        "camera-route-coordinate-range",
    }
    if reason is not None and reason not in allowed_reasons:
        invalid = True
        problems.append(
            issue(
                "invalid_camera_route_unavailable_reason",
                "Rejected",
                "camera_route_unavailable_reason",
                "camera-route unavailability must use a closed schema-2.3 reason",
            )
        )
    for name, value in counters.items():
        if not is_non_negative_integer(value):
            invalid = True
            problems.append(
                issue(
                    "invalid_camera_route_counter",
                    "Rejected",
                    name,
                    "camera-route work and validation counters must be non-negative integers",
                )
            )
    if variant_index is not None and not is_non_negative_integer(variant_index):
        invalid = True
        problems.append(
            issue(
                "invalid_camera_route_variant",
                "Rejected",
                "camera_route_variant_index",
                "camera-route variant must be null or a non-negative integer",
            )
        )
    if minimum_clearance is not None and not is_non_negative_integer(minimum_clearance):
        invalid = True
        problems.append(
            issue(
                "invalid_camera_route_clearance",
                "Rejected",
                "camera_route_minimum_clearance_voxels",
                "minimum camera clearance must be null or a non-negative integer",
            )
        )

    if policy == "legacy":
        blocked = True
        problems.append(
            issue(
                "legacy_camera_route_policy",
                "Blocked",
                "camera_route_policy",
                "legacy camera motion has no visibility-aware route-plan binding",
            )
        )
        return invalid, blocked, problems
    if policy != EXPECTED_CAMERA_ROUTE_POLICY:
        return invalid, blocked, problems

    if type(preflight_applicable) is bool and preflight_applicable is not requires_plan:
        invalid = True
        problems.append(
            issue(
                "camera_route_applicability_mismatch",
                "Rejected",
                "camera_route_preflight_applicable",
                "camera preflight applies exactly to river, lava, and near-far route focuses",
            )
        )

    if not requires_plan:
        sentinel_valid = (
            preflight_applicable is False
            and available is False
            and reason is None
            and plan_hash is None
            and variant_index is None
            and minimum_clearance is None
            and all(value == 0 for value in counters.values() if value is not None)
            and work_cap_exhausted is False
        )
        if not sentinel_valid:
            invalid = True
            problems.append(
                issue(
                    "unexpected_camera_preflight_for_legacy_route_shape",
                    "Rejected",
                    "camera_route_plan_hash",
                    "scenic, waypoint, and streaming routes must serialize the schema-2.3 non-preflight sentinel exactly",
                )
            )
        return invalid, blocked, problems

    if available is False and reason == "camera-route-focus-unavailable":
        sentinel_valid = (
            preflight_applicable is True
            and report.get("route_focus_available") is False
            and plan_hash is None
            and variant_index is None
            and minimum_clearance is None
            and all(value == 0 for value in counters.values() if value is not None)
            and work_cap_exhausted is False
        )
        if not sentinel_valid:
            invalid = True
            problems.append(
                issue(
                    "contradictory_focus_unavailable_camera_route",
                    "Rejected",
                    "camera_route_unavailable_reason",
                    "focus-unavailable camera state requires an unavailable focus and exact zero-work sentinel",
                )
            )
        else:
            blocked = True
            problems.append(
                issue(
                    "camera_route_focus_unavailable",
                    "Blocked",
                    "camera_route_available",
                    "camera preflight could not start because the requested spatial focus was unavailable",
                )
            )
        return invalid, blocked, problems

    variants = counters["camera_route_variant_count"]
    samples = counters["camera_route_validation_samples"]
    queries = counters["camera_route_voxel_queries"]
    query_cap = counters["camera_route_voxel_query_cap"]
    required = counters["camera_route_required_chunk_checks"]
    loaded = counters["camera_route_loaded_chunk_checks"]
    proven_air = counters["camera_route_proven_air_chunk_checks"]
    unloaded = counters["camera_route_unloaded_chunk_checks"]
    body_occlusions = counters["camera_route_candidate_body_occlusions"]
    los_occlusions = counters["camera_route_candidate_los_occlusions"]
    selected_clear_samples = counters["camera_route_selected_clear_samples"]
    if all(is_non_negative_integer(value) for value in counters.values()):
        if (
            variants != EXPECTED_CAMERA_ROUTE_VARIANTS
            or samples != EXPECTED_CAMERA_ROUTE_VALIDATION_SAMPLES
            or query_cap != EXPECTED_CAMERA_ROUTE_VOXEL_QUERY_CAP
        ):
            invalid = True
            problems.append(
                issue(
                    "camera_route_contract_mismatch",
                    "Rejected",
                    "camera_route_variant_count",
                    "preflight-v1 requires exactly 8 variants, 16 samples, and a 153600-query cap",
                )
            )
        if queries > query_cap:
            invalid = True
            problems.append(
                issue(
                    "camera_route_work_cap_exceeded",
                    "Rejected",
                    "camera_route_voxel_queries",
                    "camera-route voxel work exceeds its serialized hard cap",
                )
            )
        if required != loaded + proven_air + unloaded or queries != required:
            invalid = True
            problems.append(
                issue(
                    "camera_route_chunk_accounting_mismatch",
                    "Rejected",
                    "camera_route_required_chunk_checks",
                    "required chunk checks must equal loaded plus proven-air plus unloaded checks and voxel queries",
                )
            )
        if body_occlusions > variants * samples or los_occlusions > variants * samples:
            invalid = True
            problems.append(
                issue(
                    "camera_route_candidate_occlusion_count_exceeded",
                    "Rejected",
                    "camera_route_candidate_body_occlusions",
                    "candidate occlusion diagnostics cannot exceed the bounded attempted-pose population",
                )
            )
        if type(work_cap_exhausted) is bool and work_cap_exhausted is not (queries >= query_cap):
            invalid = True
            problems.append(
                issue(
                    "camera_route_work_cap_state_mismatch",
                    "Rejected",
                    "camera_route_work_cap_exhausted",
                    "work-cap exhaustion must exactly match reaching the serialized query cap",
                )
            )

    hash_valid = type(plan_hash) is str and re.fullmatch(r"[0-9a-f]{16}", plan_hash) is not None
    variant_valid = (
        is_non_negative_integer(variant_index)
        and is_non_negative_integer(variants)
        and variant_index < variants
    )
    if available is True:
        acceptance_valid = (
            preflight_applicable is True
            and hash_valid
            and variant_valid
            and is_non_negative_integer(queries)
            and 0 < queries < EXPECTED_CAMERA_ROUTE_VOXEL_QUERY_CAP
            and all(
                is_non_negative_integer(value)
                for value in (required, loaded, proven_air, unloaded)
            )
            and required == queries
            and loaded + proven_air == required
            and unloaded == 0
            and selected_clear_samples == samples == EXPECTED_CAMERA_ROUTE_VALIDATION_SAMPLES
            and is_non_negative_integer(minimum_clearance)
            and minimum_clearance > 0
            and reason is None
            and work_cap_exhausted is False
        )
        if not acceptance_valid:
            invalid = True
            problems.append(
                issue(
                    "camera_route_acceptance_invariant_failed",
                    "Rejected",
                    "camera_route_available",
                    "an available route requires a bound 16-hex plan, exact loaded/proven-air accounting, 16 selected clear samples, positive clearance, and work strictly below cap",
                )
            )
    elif available is False:
        blocked = True
        absence_valid = (
            reason in allowed_reasons
            and plan_hash is None
            and variant_index is None
            and minimum_clearance is None
            and selected_clear_samples == 0
            and (
                reason == "camera-route-work-cap"
                and work_cap_exhausted is True
                or reason != "camera-route-work-cap"
                and work_cap_exhausted is False
            )
        )
        if not absence_valid:
            invalid = True
            problems.append(
                issue(
                    "contradictory_unavailable_camera_route",
                    "Rejected",
                    "camera_route_unavailable_reason",
                    "an unavailable preflight route needs one exact reason, no plan, and a consistent work-cap state",
                )
            )
        else:
            problems.append(
                issue(
                    "camera_route_unavailable",
                    "Blocked",
                    "camera_route_available",
                    "visibility-aware camera preflight did not produce a publishable route plan",
                )
            )
    return invalid, blocked, problems


def validate_route(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, str]]]:
    output = copy_known_fields(report, ROUTE_FIELDS)
    problems: list[dict[str, str]] = []
    invalid = False
    blocked = False

    supported_focuses = {"scenic", "waypoint", "streaming", "river", "lava", "near-far"}
    anchored_focuses = {"waypoint", "river", "lava", "near-far"}
    compatible_world_profiles = {
        "waypoint": {"AstralFrontier"},
        "river": {"Natural"},
        "lava": {"AstralFrontier"},
        "near-far": {"Natural", "AstralFrontier"},
    }
    requested_focus = report.get("requested_route_focus")
    resolved_focus = report.get("resolved_route_focus")
    focus_available = report.get("route_focus_available")
    unavailable_reason = report.get("route_focus_unavailable_reason")
    focus_anchor = report.get("route_focus_anchor")
    visited_candidates = report.get("route_focus_search_visited_candidates")
    classification_queries = report.get("route_focus_classification_queries")
    candidate_cap = report.get("route_focus_search_candidate_cap")
    classification_cap = report.get("route_focus_classification_query_cap")
    cap_exhausted = report.get("route_focus_search_cap_exhausted")
    identity = report.get("run_identity")
    world_profile = identity.get("world_profile") if isinstance(identity, dict) else None
    camera_invalid, camera_blocked, camera_problems = validate_camera_route(report, run_key)
    invalid = invalid or camera_invalid
    blocked = blocked or camera_blocked
    problems.extend(camera_problems)

    for field, value in (
        ("requested_route_focus", requested_focus),
        ("resolved_route_focus", resolved_focus),
    ):
        if not is_bounded_text(value, 64) or value not in supported_focuses:
            invalid = True
            problems.append(
                issue(
                    "invalid_route_focus",
                    "Rejected",
                    field,
                    "route focus must name a supported real route",
                )
            )
    if type(focus_available) is not bool or type(cap_exhausted) is not bool:
        invalid = True
        problems.append(
            issue(
                "invalid_route_resolution_state",
                "Rejected",
                "route_focus_available",
                "route availability and cap-exhaustion states must be booleans",
            )
        )
    if unavailable_reason is not None and not is_bounded_text(unavailable_reason, 512):
        invalid = True
        problems.append(
            issue(
                "invalid_route_unavailable_reason",
                "Rejected",
                "route_focus_unavailable_reason",
                "an unavailable reason must be null or bounded non-empty text",
            )
        )
    if focus_anchor is not None and (
        type(focus_anchor) is not list
        or len(focus_anchor) != 3
        or any(type(value) is not int for value in focus_anchor)
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_route_focus_anchor",
                "Rejected",
                "route_focus_anchor",
                "route anchor must be null or exactly three signed integers",
            )
        )
    for field, value in (
        ("route_focus_search_visited_candidates", visited_candidates),
        ("route_focus_classification_queries", classification_queries),
    ):
        if value is not None and not is_non_negative_integer(value):
            invalid = True
            problems.append(
                issue(
                    "invalid_route_search_count",
                    "Rejected",
                    field,
                    "optional route-search work must be a non-negative integer",
                )
            )
    for field, value in (
        ("route_focus_search_candidate_cap", candidate_cap),
        ("route_focus_classification_query_cap", classification_cap),
    ):
        if not is_non_negative_integer(value):
            invalid = True
            problems.append(
                issue(
                    "invalid_route_search_cap",
                    "Rejected",
                    field,
                    "route-search caps must be non-negative integers",
                )
            )
    if (
        is_non_negative_integer(visited_candidates)
        and is_non_negative_integer(candidate_cap)
        and visited_candidates > candidate_cap
    ) or (
        is_non_negative_integer(classification_queries)
        and is_non_negative_integer(classification_cap)
        and classification_queries > classification_cap
    ):
        invalid = True
        problems.append(
            issue(
                "route_search_cap_exceeded",
                "Rejected",
                "route_focus_search_visited_candidates",
                "observed route-search work exceeds its serialized hard cap",
            )
        )

    reached_cap = (
        is_non_negative_integer(visited_candidates)
        and is_non_negative_integer(candidate_cap)
        and visited_candidates == candidate_cap
    ) or (
        is_non_negative_integer(classification_queries)
        and is_non_negative_integer(classification_cap)
        and classification_queries == classification_cap
    )
    if focus_available is True:
        if requested_focus != resolved_focus or unavailable_reason is not None or cap_exhausted is not False:
            invalid = True
            problems.append(
                issue(
                    "contradictory_available_route_resolution",
                    "Rejected",
                    "route_focus_available",
                    "an available focus must resolve to itself without a reason or exhausted search",
                )
            )
        if requested_focus in anchored_focuses and focus_anchor is None:
            invalid = True
            problems.append(
                issue(
                    "available_route_focus_missing_anchor",
                    "Rejected",
                    "route_focus_anchor",
                    "an available spatial route focus must serialize its resolved world anchor",
                )
            )
        expected_profiles = compatible_world_profiles.get(requested_focus)
        if expected_profiles is not None and world_profile not in expected_profiles:
            invalid = True
            problems.append(
                issue(
                    "route_focus_profile_mismatch",
                    "Rejected",
                    "run_identity.world_profile",
                    "the available route focus is incompatible with the serialized world profile",
                )
            )
    elif focus_available is False:
        blocked = True
        if requested_focus == resolved_focus:
            invalid = True
            problems.append(
                issue(
                    "contradictory_unavailable_route_resolution",
                    "Rejected",
                    "resolved_route_focus",
                    "an unavailable requested focus must name the actual fallback route",
                )
            )
        if not is_bounded_text(unavailable_reason, 512) or cap_exhausted is not reached_cap:
            invalid = True
            problems.append(
                issue(
                    "contradictory_unavailable_route_resolution",
                    "Rejected",
                    "route_focus_unavailable_reason",
                    "an unavailable focus needs a reason and exact bounded-search exhaustion state",
                )
            )
        else:
            problems.append(
                issue(
                    "requested_route_focus_unavailable",
                    "Blocked",
                    "route_focus_available",
                    "the requested evidence focus was unavailable; metrics describe the explicit resolved fallback route",
                )
            )
    numeric_fields = (
        "requested_route_distance_m",
        "max_horizontal_displacement_m",
        "duration_seconds",
        "warmup_seconds",
        "average_fps",
        "max_frame_ms",
        "final_smoothed_fps",
    )
    for field in numeric_fields:
        if not is_finite_number(report.get(field), minimum=0.0):
            invalid = True
            problems.append(
                issue(
                    "invalid_route_metric",
                    "Rejected",
                    field,
                    "route metric must be finite and non-negative",
                )
            )
    if not is_non_negative_integer(report.get("frames")):
        invalid = True
        problems.append(
            issue(
                "invalid_frame_count",
                "Rejected",
                "frames",
                "frame count must be a non-negative integer",
            )
        )

    dense_integer_fields = (
        "loaded_chunks",
        "pending_terrain",
        "pending_meshes",
        "dirty_chunks",
        "dense_chunks",
        "dense_chunk_budget",
        "peak_loaded_chunks",
        "peak_pending_terrain",
        "peak_dense_chunks",
    )
    for field in dense_integer_fields:
        if not is_non_negative_integer(report.get(field)):
            invalid = True
            problems.append(
                issue(
                    "invalid_dense_chunk_budget_evidence",
                    "Rejected",
                    field,
                    "dense residency evidence must be a non-negative integer",
                )
            )
    loaded_chunks = report.get("loaded_chunks")
    pending_terrain = report.get("pending_terrain")
    dense_chunks = report.get("dense_chunks")
    peak_loaded_chunks = report.get("peak_loaded_chunks")
    peak_pending_terrain = report.get("peak_pending_terrain")
    peak_dense_chunks = report.get("peak_dense_chunks")
    dense_budget = report.get("dense_chunk_budget")
    if dense_budget != EXPECTED_DENSE_CHUNK_BUDGET:
        invalid = True
        problems.append(
            issue(
                "dense_chunk_budget_identity_drift",
                "Rejected",
                "dense_chunk_budget",
                f"current evidence requires the exact {EXPECTED_DENSE_CHUNK_BUDGET}-chunk budget",
            )
        )
    if report.get("dense_chunk_budget_exceeded") is not False:
        invalid = True
        problems.append(
            issue(
                "dense_chunk_budget_exceeded",
                "Rejected",
                "dense_chunk_budget_exceeded",
                "a current QA run cannot publish after any dense residency budget violation",
            )
        )
    if (
        is_non_negative_integer(loaded_chunks)
        and is_non_negative_integer(pending_terrain)
        and is_non_negative_integer(dense_chunks)
        and dense_chunks != loaded_chunks + pending_terrain
    ):
        invalid = True
        problems.append(
            issue(
                "dense_chunk_total_mismatch",
                "Rejected",
                "dense_chunks",
                "dense_chunks must equal loaded_chunks plus pending_terrain",
            )
        )
    if (
        is_non_negative_integer(peak_dense_chunks)
        and is_non_negative_integer(dense_chunks)
        and is_non_negative_integer(peak_loaded_chunks)
        and is_non_negative_integer(peak_pending_terrain)
        and (
            peak_dense_chunks > EXPECTED_DENSE_CHUNK_BUDGET
            or peak_dense_chunks < dense_chunks
            or peak_dense_chunks < peak_loaded_chunks
            or peak_dense_chunks < peak_pending_terrain
        )
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_peak_dense_chunk_budget_evidence",
                "Rejected",
                "peak_dense_chunks",
                "peak dense residency must remain within budget and dominate every component observation",
            )
        )
    if (
        report.get("pending_terrain") != 0
        or report.get("pending_meshes") != 0
        or report.get("dirty_chunks") != 0
        or report.get("frontier_complete") is not True
    ):
        invalid = True
        problems.append(
            issue(
                "near_field_not_settled",
                "Rejected",
                "frontier_complete",
                "current evidence requires zero near-field queues and a complete frontier",
            )
        )

    for field in ("requested_duration_seconds", "write_tail_seconds"):
        if field not in report:
            blocked = True
            problems.append(
                issue(
                    f"legacy_missing_{field}",
                    "Blocked",
                    field,
                    "legacy report does not separate requested route time from write-tail time",
                )
            )
        elif not is_finite_number(report.get(field), minimum=0.0):
            invalid = True
            problems.append(
                issue(
                    "invalid_route_timing",
                    "Rejected",
                    field,
                    "route timing must be finite and non-negative",
                )
            )

    requested_duration = report.get("requested_duration_seconds")
    actual_duration = report.get("duration_seconds")
    if is_finite_number(requested_duration, minimum=0.0) and is_finite_number(
        actual_duration, minimum=0.0
    ):
        duration_tolerance = max(0.001, float(requested_duration) * 0.000001)
        if float(actual_duration) > float(requested_duration) + duration_tolerance:
            invalid = True
            problems.append(
                issue(
                    "route_duration_includes_tail",
                    "Rejected",
                    "duration_seconds",
                    "active route duration exceeds requested duration; "
                    "write-tail time must remain separate",
                )
            )
        elif float(actual_duration) + duration_tolerance < float(requested_duration):
            blocked = True
            problems.append(
                issue(
                    "incomplete_route_duration",
                    "Blocked",
                    "duration_seconds",
                    "active route ended before the requested duration",
                )
            )
    classification = "Rejected" if invalid else "Blocked" if blocked else "Observed"
    return output, claim(
        f"{run_key}:route_observation",
        classification,
        "Route and frame-rate values are preserved as observations, not performance promises.",
    ), problems


def validate_route_frame_times(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any] | None, dict[str, Any], list[dict[str, str]]]:
    frame_times = report.get("route_frame_times")
    if not isinstance(frame_times, dict):
        problem = issue(
            "legacy_missing_route_frame_times",
            "Blocked",
            "route_frame_times",
            "legacy report lacks route-only median, p95, p99, and rejection telemetry",
        )
        return None, claim(
            f"{run_key}:route_frame_times",
            "Blocked",
            "Route-only frame-time distribution is unavailable.",
        ), [problem]

    output = copy_known_fields(frame_times, ROUTE_FRAME_TIME_FIELDS)
    problems: list[dict[str, str]] = []
    invalid = False
    count_fields = (
        "sample_count",
        "excluded_warmup_sample_count",
        "excluded_write_tail_sample_count",
        "rejected_sample_count",
        "rejected_non_finite_sample_count",
        "rejected_non_positive_sample_count",
        "rejected_huge_sample_count",
        "rejected_arithmetic_overflow_sample_count",
        "histogram_overflow_sample_count",
    )
    positive_bound_fields = (
        "histogram_bucket_count",
        "histogram_bucket_width_ms",
        "histogram_exact_max_ms",
        "accepted_sample_max_ms",
        "accumulator_bytes",
        "quantile_scan_work_cap",
    )
    for field in count_fields:
        if not is_non_negative_integer(frame_times.get(field)):
            invalid = True
            problems.append(
                issue(
                    "invalid_route_frame_count",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "frame-time count or bound must be a non-negative integer",
                )
            )
    for field in positive_bound_fields:
        if not is_non_negative_integer(frame_times.get(field)) or frame_times.get(field) == 0:
            invalid = True
            problems.append(
                issue(
                    "invalid_route_frame_bound",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "frame-time memory, work, and histogram bounds must be positive integers",
                )
            )
    for field in ("quantile_max_error_ms", "mean_sample_rounding_max_error_ms"):
        if not is_finite_number(frame_times.get(field), minimum=0.0):
            invalid = True
            problems.append(
                issue(
                    "invalid_frame_time_error_bound",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "accuracy bound must be finite and non-negative",
                )
            )
    for field in ("mean_ms", "median_ms", "p95_ms", "p99_ms", "max_ms"):
        value = frame_times.get(field)
        if value is not None and not is_finite_number(value, minimum=0.0):
            invalid = True
            problems.append(
                issue(
                    "invalid_frame_time_statistic",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "frame-time statistic must be null or finite and non-negative",
                )
            )
    for field in (
        "quantile_values_are_bucket_upper_bounds",
        "quantiles_complete",
        "measurement_valid",
    ):
        if type(frame_times.get(field)) is not bool:
            invalid = True
            problems.append(
                issue(
                    "invalid_frame_time_flag",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "frame-time validity flags must be booleans",
                )
            )
    for field in ("scope", "quantile_method"):
        if not is_bounded_text(frame_times.get(field), 160):
            invalid = True
            problems.append(
                issue(
                    "invalid_frame_time_contract",
                    "Rejected",
                    f"route_frame_times.{field}",
                    "frame-time scope and quantile method must be explicit text",
                )
            )

    if frame_times.get("scope") != EXPECTED_ROUTE_FRAME_SCOPE:
        invalid = True
        problems.append(
            issue(
                "unexpected_route_frame_scope",
                "Rejected",
                "route_frame_times.scope",
                "frame-time scope does not prove active-route-only sampling "
                "with warmup and write-tail exclusion",
            )
        )
    if frame_times.get("quantile_method") != EXPECTED_QUANTILE_METHOD:
        invalid = True
        problems.append(
            issue(
                "unexpected_quantile_method",
                "Rejected",
                "route_frame_times.quantile_method",
                "quantile method is not the current conservative nearest-rank contract",
            )
        )
    if frame_times.get("quantile_values_are_bucket_upper_bounds") is not True:
        invalid = True
        problems.append(
            issue(
                "unbounded_quantile_interpretation",
                "Rejected",
                "route_frame_times.quantile_values_are_bucket_upper_bounds",
                "current quantiles must be explicitly serialized as conservative "
                "bucket upper bounds",
            )
        )

    sample_count = frame_times.get("sample_count")
    rejected_count = frame_times.get("rejected_sample_count")
    complete = frame_times.get("quantiles_complete")
    measurement_valid = frame_times.get("measurement_valid")
    statistic_fields = ("mean_ms", "median_ms", "p95_ms", "p99_ms", "max_ms")
    statistics = [frame_times.get(field) for field in statistic_fields]
    quantiles = [frame_times.get(field) for field in ("median_ms", "p95_ms", "p99_ms")]
    if measurement_valid is True and (
        not is_non_negative_integer(sample_count)
        or sample_count == 0
        or rejected_count != 0
        or complete is not True
        or any(value is None for value in statistics)
    ):
        invalid = True
        problems.append(
            issue(
                "contradictory_measurement_validity",
                "Rejected",
                "route_frame_times.measurement_valid",
                "valid measurement contradicts its sample, rejection, or statistic fields",
            )
        )

    if all(
        is_finite_number(value, minimum=0.0)
        for value in (*quantiles, frame_times.get("max_ms"))
    ):
        median, p95, p99 = (float(value) for value in quantiles)
        maximum = float(frame_times["max_ms"])
        if not median <= p95 <= p99 <= maximum:
            invalid = True
            problems.append(
                issue(
                    "unordered_quantiles",
                    "Rejected",
                    "route_frame_times",
                    "expected median <= p95 <= p99 <= max",
                )
            )

    rejection_components = (
        "rejected_non_finite_sample_count",
        "rejected_non_positive_sample_count",
        "rejected_huge_sample_count",
        "rejected_arithmetic_overflow_sample_count",
    )
    if is_non_negative_integer(rejected_count) and all(
        is_non_negative_integer(frame_times.get(field)) for field in rejection_components
    ):
        component_sum = sum(frame_times[field] for field in rejection_components)
        if rejected_count != component_sum:
            invalid = True
            problems.append(
                issue(
                    "rejection_count_mismatch",
                    "Rejected",
                    "route_frame_times.rejected_sample_count",
                    "aggregate rejected sample count does not equal its serialized causes",
                )
            )

    overflow_count = frame_times.get("histogram_overflow_sample_count")
    if (
        is_non_negative_integer(overflow_count)
        and is_non_negative_integer(sample_count)
        and overflow_count > sample_count
    ):
        invalid = True
        problems.append(
            issue(
                "histogram_overflow_count_exceeds_samples",
                "Rejected",
                "route_frame_times.histogram_overflow_sample_count",
                "histogram overflow count cannot exceed accepted sample count",
            )
        )

    bucket_count = frame_times.get("histogram_bucket_count")
    bucket_width = frame_times.get("histogram_bucket_width_ms")
    exact_max = frame_times.get("histogram_exact_max_ms")
    accepted_max = frame_times.get("accepted_sample_max_ms")
    scan_cap = frame_times.get("quantile_scan_work_cap")
    if all(
        is_non_negative_integer(value) and value > 0
        for value in (bucket_count, bucket_width, exact_max, accepted_max, scan_cap)
    ):
        expected_bucket_count = (exact_max + bucket_width - 1) // bucket_width + 1
        if bucket_count != expected_bucket_count:
            invalid = True
            problems.append(
                issue(
                    "histogram_geometry_mismatch",
                    "Rejected",
                    "route_frame_times.histogram_bucket_count",
                    "bucket count does not match exact range, width, and overflow bucket",
                )
            )
        if accepted_max < exact_max:
            invalid = True
            problems.append(
                issue(
                    "invalid_histogram_range",
                    "Rejected",
                    "route_frame_times.accepted_sample_max_ms",
                    "accepted sample maximum cannot be below the exact histogram range",
                )
            )
        if scan_cap < bucket_count:
            invalid = True
            problems.append(
                issue(
                    "quantile_scan_cap_too_small",
                    "Rejected",
                    "route_frame_times.quantile_scan_work_cap",
                    "quantile scan work cap is smaller than the serialized histogram",
                )
            )

    if is_non_negative_integer(sample_count) and report.get("frames") != sample_count:
        invalid = True
        problems.append(
            issue(
                "legacy_frame_count_mismatch",
                "Rejected",
                "frames",
                "top-level frames must equal route_frame_times.sample_count",
            )
        )
    mean_ms = frame_times.get("mean_ms")
    average_fps = report.get("average_fps")
    if is_finite_number(mean_ms, minimum=0.000001) and is_finite_number(
        average_fps, minimum=0.0
    ):
        expected_fps = 1_000.0 / float(mean_ms)
        tolerance = max(0.01, expected_fps * 0.001)
        if abs(float(average_fps) - expected_fps) > tolerance:
            invalid = True
            problems.append(
                issue(
                    "average_fps_mismatch",
                    "Rejected",
                    "average_fps",
                    "top-level average_fps is inconsistent with route mean_ms",
                )
            )

    top_level_max = report.get("max_frame_ms")
    frame_max = frame_times.get("max_ms")
    if is_finite_number(top_level_max, minimum=0.0) and is_finite_number(
        frame_max, minimum=0.0
    ):
        if abs(float(top_level_max) - float(frame_max)) > 0.001:
            invalid = True
            problems.append(
                issue(
                    "max_frame_time_mismatch",
                    "Rejected",
                    "max_frame_ms",
                    "top-level max_frame_ms is inconsistent with route_frame_times.max_ms",
                )
            )

    if measurement_valid is not True:
        invalid = True
        problems.append(
            issue(
                "route_frame_measurement_invalid",
                "Rejected",
                "route_frame_times.measurement_valid",
                "runtime marked the route-only frame-time measurement invalid",
            )
        )

    classification = "Rejected" if invalid else "Observed"
    return output, claim(
        f"{run_key}:route_frame_times",
        classification,
        "Route-only frame-time distribution is recorded with bounded accuracy.",
    ), problems


def validate_planetary_streaming(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any] | None, dict[str, Any], list[dict[str, str]]]:
    planetary = report.get("planetary_streaming")
    if not isinstance(planetary, dict):
        problem = issue(
            "missing_planetary_streaming",
            "Blocked",
            "planetary_streaming",
            "planetary live values and budgets are absent",
        )
        return None, claim(
            f"{run_key}:planetary_budgets",
            "Blocked",
            "Planetary budget compliance cannot be evaluated.",
        ), [problem]

    output = {
        "budgets": copy_known_fields(planetary, PLANETARY_BUDGET_FIELDS),
        "live": copy_known_fields(planetary, PLANETARY_LIVE_FIELDS),
        "telemetry": copy_known_fields(planetary, PLANETARY_TELEMETRY_FIELDS),
    }
    problems: list[dict[str, str]] = []
    invalid = False
    run_identity = report.get("run_identity")
    run_grammar = (
        run_identity.get("terrain_grammar") if isinstance(run_identity, dict) else None
    )
    run_profile = (
        run_identity.get("world_profile") if isinstance(run_identity, dict) else None
    )
    if planetary.get("profile") != run_profile:
        invalid = True
        problems.append(
            issue(
                "planetary_world_profile_mismatch",
                "Rejected",
                "planetary_streaming.profile",
                "far-field profile must exactly equal the immutable run identity profile",
            )
        )
    desired_grammar = planetary.get("desired_terrain_grammar")
    active_grammar = planetary.get("active_terrain_grammar")
    if desired_grammar not in {"V1", "V2", "V3"} or desired_grammar != run_grammar:
        invalid = True
        problems.append(
            issue(
                "planetary_desired_terrain_grammar_mismatch",
                "Rejected",
                "planetary_streaming.desired_terrain_grammar",
                "far-field desired grammar must exactly equal the run's immutable terrain grammar",
            )
        )
    planetary_enabled = planetary.get("enabled")
    if planetary_enabled is True and active_grammar != desired_grammar:
        invalid = True
        problems.append(
            issue(
                "planetary_active_terrain_grammar_mismatch",
                "Rejected",
                "planetary_streaming.active_terrain_grammar",
                "an enabled far field must be resident under the exact desired terrain grammar",
            )
        )
    elif planetary_enabled is False and active_grammar is not None:
        invalid = True
        problems.append(
            issue(
                "disabled_planetary_has_active_terrain_grammar",
                "Rejected",
                "planetary_streaming.active_terrain_grammar",
                "a disabled far field must not retain an active worker grammar",
            )
        )
    for field in PLANETARY_BUDGET_FIELDS:
        if not is_non_negative_integer(planetary.get(field)):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_budget",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary budget must be a non-negative integer",
                )
            )
    for field, expected in PLANETARY_EXPECTED_SEMANTIC_COHORT_BUDGETS.items():
        if planetary.get(field) != expected:
            invalid = True
            problems.append(
                issue(
                    "unexpected_semantic_cohort_budget",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    f"semantic-cohort v1 contract requires the exact serialized budget {expected}",
                )
            )
    if (
        planetary.get("budget_hydro_atomic_ring_build_bytes")
        != PLANETARY_EXPECTED_HYDRO_ATOMIC_RING_BUILD_BYTES
        or planetary.get("budget_atomic_ring_build_bytes")
        != PLANETARY_EXPECTED_ATOMIC_RING_BUILD_BYTES
    ):
        invalid = True
        problems.append(
            issue(
                "unexpected_atomic_build_budget",
                "Rejected",
                "planetary_streaming.budget_atomic_ring_build_bytes",
                "Hydro-only and combined atomic worker-result byte ceilings must remain separately exact",
            )
        )
    integer_live = (
        "interaction_radius_metres",
        "confirmed_near_extent_metres",
        "near_coverage_ready_columns",
        "near_coverage_hidden_cells",
        "far_radius_metres",
        "resident_entities",
        "resident_vertices",
        "resident_indices",
        "resident_mesh_bytes",
        "resident_fluid_entities",
        "resident_fluid_vertices",
        "resident_fluid_indices",
        "resident_water_indices",
        "resident_lava_indices",
        "resident_fluid_mesh_bytes",
        "resident_semantic_cohort_entities",
        "resident_semantic_cohort_vertices",
        "resident_semantic_cohort_indices",
        "resident_semantic_cohort_mesh_bytes",
        "resident_semantic_cohort_count",
        "live_sample_cache_windows",
        "live_sample_cache_bytes",
    )
    for field in integer_live:
        if not is_non_negative_integer(planetary.get(field)):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_live_value",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary live value must be a non-negative integer",
                )
            )
    for field, total_field in (
        ("ring_vertices", "resident_vertices"),
        ("ring_indices", "resident_indices"),
        ("scheduler_ring_vertices", "scheduler_resident_vertices"),
        ("scheduler_ring_indices", "scheduler_resident_indices"),
        ("fluid_ring_vertices", "resident_fluid_vertices"),
        ("fluid_ring_indices", "resident_fluid_indices"),
        ("scheduler_fluid_ring_vertices", "scheduler_resident_fluid_vertices"),
        ("scheduler_fluid_ring_indices", "scheduler_resident_fluid_indices"),
        ("water_ring_indices", "resident_water_indices"),
        ("lava_ring_indices", "resident_lava_indices"),
        ("scheduler_water_ring_indices", "scheduler_resident_water_indices"),
        ("scheduler_lava_ring_indices", "scheduler_resident_lava_indices"),
    ):
        values = planetary.get(field)
        if (
            type(values) is not list
            or len(values) != PLANETARY_FAR_FIELD_LEVELS
            or any(not is_non_negative_integer(value) for value in values)
        ):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_ring_population",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    f"ring population must contain exactly {PLANETARY_FAR_FIELD_LEVELS} non-negative integers",
                )
            )
        elif sum(values) != planetary.get(total_field):
            invalid = True
            problems.append(
                issue(
                    "planetary_ring_population_total_mismatch",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    f"ring population must sum exactly to {total_field}",
                )
            )
    if type(planetary.get("enabled")) is not bool:
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_enabled",
                "Rejected",
                "planetary_streaming.enabled",
                "planetary enabled state must be a boolean",
            )
        )

    fluid_total = planetary.get("resident_fluid_indices")
    water_total = planetary.get("resident_water_indices")
    lava_total = planetary.get("resident_lava_indices")
    scheduler_fluid_total = planetary.get("scheduler_resident_fluid_indices")
    scheduler_water_total = planetary.get("scheduler_resident_water_indices")
    scheduler_lava_total = planetary.get("scheduler_resident_lava_indices")
    fluid_kind_contracts = (
        (water_total, lava_total, fluid_total),
        (scheduler_water_total, scheduler_lava_total, scheduler_fluid_total),
        (
            planetary.get("last_water_indices"),
            planetary.get("last_lava_indices"),
            planetary.get("last_fluid_indices"),
        ),
    )
    ring_kind_contracts = (
        (planetary.get("water_ring_indices"), planetary.get("lava_ring_indices"), planetary.get("fluid_ring_indices")),
        (planetary.get("scheduler_water_ring_indices"), planetary.get("scheduler_lava_ring_indices"), planetary.get("scheduler_fluid_ring_indices")),
    )
    fluid_kind_invalid = any(
        not all(is_non_negative_integer(value) for value in (water, lava, fluid))
        or water % 6 != 0
        or lava % 6 != 0
        or water + lava != fluid
        for water, lava, fluid in fluid_kind_contracts
    )
    for water, lava, fluid in ring_kind_contracts:
        if (
            type(water) is not list
            or type(lava) is not list
            or type(fluid) is not list
            or len(water) != PLANETARY_FAR_FIELD_LEVELS
            or len(lava) != PLANETARY_FAR_FIELD_LEVELS
            or len(fluid) != PLANETARY_FAR_FIELD_LEVELS
            or any(
                not all(is_non_negative_integer(value) for value in (water[index], lava[index], fluid[index]))
                or water[index] % 6 != 0
                or lava[index] % 6 != 0
                or water[index] + lava[index] != fluid[index]
                for index in range(PLANETARY_FAR_FIELD_LEVELS)
            )
        ):
            fluid_kind_invalid = True
    if fluid_kind_invalid:
        invalid = True
        problems.append(
            issue(
                "planetary_fluid_kind_integrity_mismatch",
                "Rejected",
                "planetary_streaming.resident_water_indices",
                "water and lava counts must be complete quads whose exact sum equals fluid indices per ring and in total",
            )
        )
    if not is_bounded_text(planetary.get("profile"), 160):
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_profile",
                "Rejected",
                "planetary_streaming.profile",
                "planetary profile must be non-empty text",
            )
        )
    elif planetary.get("profile") not in {"Natural", "AstralFrontier"}:
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_profile",
                "Rejected",
                "planetary_streaming.profile",
                "planetary profile is not a supported generator contract",
            )
        )

    telemetry_unsigned_integer_fields = (
        "pending_rebuilds",
        "dirty_mask",
        "update_cadence_frames",
        "scheduler_deferred_frames",
        "completed_rebuilds",
        "stale_builds_discarded",
        "budget_rejections",
        "resident_detailed_levels",
        "resident_reduced_levels",
        "last_height_queries",
        "last_material_slope_queries",
        "last_bridge_v2_cell_reuses",
        "peak_live_sample_cache_windows",
        "peak_live_sample_cache_bytes",
        "scheduler_resident_entities",
        "scheduler_resident_vertices",
        "scheduler_resident_indices",
        "scheduler_resident_mesh_bytes",
        "scheduler_resident_fluid_entities",
        "scheduler_resident_fluid_vertices",
        "scheduler_resident_fluid_indices",
        "scheduler_resident_fluid_mesh_bytes",
        "scheduler_resident_water_indices",
        "scheduler_resident_lava_indices",
        "scheduler_resident_semantic_cohort_entities",
        "scheduler_resident_semantic_cohort_vertices",
        "scheduler_resident_semantic_cohort_indices",
        "scheduler_resident_semantic_cohort_mesh_bytes",
        "scheduler_resident_semantic_cohort_count",
        "resident_duplicate_levels",
        "resident_out_of_range_levels",
        "resident_observation_rejections",
        "resident_fluid_duplicate_slots",
        "resident_fluid_out_of_range_levels",
        "resident_fluid_observation_rejections",
        "resident_semantic_cohort_observation_rejections",
        "last_biome_queries",
        "last_fluid_classification_queries",
        "last_fluid_biome_queries",
        "last_fluid_vertices",
        "last_fluid_indices",
        "last_water_indices",
        "last_lava_indices",
        "last_semantic_cohort_hash_scans",
        "last_semantic_cohort_height_queries",
        "last_semantic_cohort_biome_queries",
        "last_semantic_cohort_candidates",
        "last_semantic_cohort_emitted",
        "last_semantic_cohort_vertices",
        "last_semantic_cohort_indices",
        "last_reused_height_samples",
        "last_reused_biome_samples",
        "incremental_strip_rebuilds",
        "full_cache_rebuilds",
        "teleport_fallbacks",
        "last_clamped_queries",
    )
    for field in telemetry_unsigned_integer_fields:
        if not is_non_negative_integer(planetary.get(field)):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_telemetry_count",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary telemetry count must be a non-negative integer",
                )
            )

    cohort_kind_counts = planetary.get("resident_semantic_cohort_kind_counts")
    scheduler_cohort_kind_counts = planetary.get(
        "scheduler_resident_semantic_cohort_kind_counts"
    )
    last_cohort_kind_counts = planetary.get("last_semantic_cohort_kind_counts")
    cohort_count = planetary.get("resident_semantic_cohort_count")
    scheduler_cohort_count = planetary.get("scheduler_resident_semantic_cohort_count")
    last_cohort_count = planetary.get("last_semantic_cohort_emitted")
    cohort_contracts = (
        (
            cohort_kind_counts,
            cohort_count,
            planetary.get("resident_semantic_cohort_vertices"),
            planetary.get("resident_semantic_cohort_indices"),
            planetary.get("resident_semantic_cohort_mesh_bytes"),
        ),
        (
            scheduler_cohort_kind_counts,
            scheduler_cohort_count,
            planetary.get("scheduler_resident_semantic_cohort_vertices"),
            planetary.get("scheduler_resident_semantic_cohort_indices"),
            planetary.get("scheduler_resident_semantic_cohort_mesh_bytes"),
        ),
        (
            last_cohort_kind_counts,
            last_cohort_count,
            planetary.get("last_semantic_cohort_vertices"),
            planetary.get("last_semantic_cohort_indices"),
            None,
        ),
    )
    cohort_payload_invalid = False
    for kind_counts, count, vertices, indices, mesh_bytes in cohort_contracts:
        if (
            type(kind_counts) is not list
            or len(kind_counts) != PLANETARY_SEMANTIC_COHORT_KIND_COUNT
            or any(not is_non_negative_integer(value) for value in kind_counts)
            or not all(is_non_negative_integer(value) for value in (count, vertices, indices))
        ):
            cohort_payload_invalid = True
            continue
        expected_vertices = count * PLANETARY_SEMANTIC_COHORT_VERTICES_PER_COHORT
        expected_indices = count * PLANETARY_SEMANTIC_COHORT_INDICES_PER_COHORT
        expected_bytes = (
            expected_vertices * PLANETARY_VERTEX_BYTES
            + expected_indices * PLANETARY_INDEX_BYTES
        )
        if (
            sum(kind_counts) != count
            or vertices != expected_vertices
            or indices != expected_indices
            or (mesh_bytes is not None and mesh_bytes != expected_bytes)
        ):
            cohort_payload_invalid = True
    last_candidates = planetary.get("last_semantic_cohort_candidates")
    if (
        is_non_negative_integer(last_cohort_count)
        and is_non_negative_integer(last_candidates)
        and last_cohort_count > last_candidates
    ):
        cohort_payload_invalid = True
    expected_cohort_entities = (
        int(cohort_count > 0) if is_non_negative_integer(cohort_count) else None
    )
    scheduler_expected_cohort_entities = (
        int(scheduler_cohort_count > 0)
        if is_non_negative_integer(scheduler_cohort_count)
        else None
    )
    if (
        planetary.get("resident_semantic_cohort_entities")
        != expected_cohort_entities
        or planetary.get("scheduler_resident_semantic_cohort_entities")
        != scheduler_expected_cohort_entities
        or (
            is_non_negative_integer(cohort_count)
            and cohort_count
            > PLANETARY_EXPECTED_SEMANTIC_COHORT_BUDGETS[
                "budget_semantic_cohort_height_queries"
            ]
        )
        or (
            is_non_negative_integer(last_candidates)
            and last_candidates
            > PLANETARY_EXPECTED_SEMANTIC_COHORT_BUDGETS[
                "budget_semantic_cohort_height_queries"
            ]
        )
    ):
        cohort_payload_invalid = True
    profile = planetary.get("profile")
    for kind_counts in (
        cohort_kind_counts,
        scheduler_cohort_kind_counts,
        last_cohort_kind_counts,
    ):
        if type(kind_counts) is list and len(kind_counts) == 6:
            if (profile == "Natural" and any(kind_counts[3:])) or (
                profile == "AstralFrontier" and any(kind_counts[:3])
            ):
                cohort_payload_invalid = True
    if cohort_payload_invalid:
        invalid = True
        problems.append(
            issue(
                "planetary_semantic_cohort_payload_mismatch",
                "Rejected",
                "planetary_streaming.resident_semantic_cohort_kind_counts",
                "cohort kind totals, fixed 24/36 geometry, mesh bytes, and candidate/emission counts must agree exactly",
            )
        )
    for field in (
        "last_cache_shift_x_cells",
        "last_cache_shift_z_cells",
        "camera_world_x",
        "camera_world_z",
    ):
        if type(planetary.get(field)) is not int:
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_coordinate",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary cache shifts and camera coordinates must be integers",
                )
            )
    for field in (
        "build_in_flight",
        "resident_observation_valid",
        "resident_entity_count_overflow",
        "resident_scheduler_mismatch",
        "resident_budget_exceeded",
        "resident_fluid_observation_valid",
        "resident_fluid_kind_integrity_valid",
        "resident_fluid_entity_count_overflow",
        "resident_fluid_scheduler_mismatch",
        "resident_fluid_budget_exceeded",
        "resident_semantic_cohort_observation_valid",
        "resident_semantic_cohort_payload_integrity_valid",
        "resident_semantic_cohort_entity_count_overflow",
        "resident_semantic_cohort_scheduler_mismatch",
        "resident_semantic_cohort_budget_exceeded",
    ):
        if type(planetary.get(field)) is not bool:
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_build_state",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary scheduler and observation states must be booleans",
                )
            )
    for field in (
        "material_detail",
        "surface_material_mode",
        "hydro_mode",
        "semantic_cohort_mode",
        "last_cache_update",
    ):
        if not is_bounded_text(planetary.get(field), 160):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_telemetry_text",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary telemetry state must be bounded non-empty text",
                )
            )
    if planetary.get("surface_material_mode") not in {
        "LegacyPalette",
        "BridgeV1",
        "BridgeV2",
    }:
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_surface_material_mode",
                "Rejected",
                "planetary_streaming.surface_material_mode",
                "surface material mode is not a supported evidence value",
            )
        )
    if planetary.get("hydro_mode") not in {"Disabled", "DescriptiveV1"}:
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_hydro_mode",
                "Rejected",
                "planetary_streaming.hydro_mode",
                "far hydro mode is not a supported evidence value",
            )
        )
    if planetary.get("semantic_cohort_mode") not in {"Disabled", "SilhouettesV1"}:
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_semantic_cohort_mode",
                "Rejected",
                "planetary_streaming.semantic_cohort_mode",
                "far semantic-cohort mode is not a supported evidence value",
            )
        )

    for field in (
        "resident_semantic_cohort_kind_counts",
        "scheduler_resident_semantic_cohort_kind_counts",
        "last_semantic_cohort_kind_counts",
    ):
        values = planetary.get(field)
        if (
            type(values) is not list
            or len(values) != PLANETARY_SEMANTIC_COHORT_KIND_COUNT
            or any(not is_non_negative_integer(value) for value in values)
        ):
            invalid = True
            problems.append(
                issue(
                    "invalid_semantic_cohort_kind_population",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    f"cohort kind population must contain exactly {PLANETARY_SEMANTIC_COHORT_KIND_COUNT} non-negative integers",
                )
            )

    desired_detail = planetary.get("desired_material_detail")
    resident_detail = planetary.get("resident_material_detail")
    valid_detail_values = {"Detailed", "Reduced"}
    if (
        type(desired_detail) is not list
        or len(desired_detail) != PLANETARY_FAR_FIELD_LEVELS
        or any(type(value) is not str or value not in valid_detail_values for value in desired_detail)
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_desired_material_detail",
                "Rejected",
                "planetary_streaming.desired_material_detail",
                f"desired detail must contain exactly {PLANETARY_FAR_FIELD_LEVELS} Detailed/Reduced entries",
            )
        )
    elif planetary.get("material_detail") != desired_detail[0]:
        invalid = True
        problems.append(
            issue(
                "planetary_material_detail_summary_mismatch",
                "Rejected",
                "planetary_streaming.material_detail",
                "material-detail summary must equal desired L0 detail",
            )
        )

    if (
        type(resident_detail) is not list
        or len(resident_detail) != PLANETARY_FAR_FIELD_LEVELS
        or any(value is not None and (type(value) is not str or value not in valid_detail_values) for value in resident_detail)
    ):
        invalid = True
        problems.append(
            issue(
                "invalid_planetary_resident_material_detail",
                "Rejected",
                "planetary_streaming.resident_material_detail",
                f"resident detail must contain exactly {PLANETARY_FAR_FIELD_LEVELS} null/Detailed/Reduced entries",
            )
        )
    else:
        detailed_count = sum(value == "Detailed" for value in resident_detail)
        reduced_count = sum(value == "Reduced" for value in resident_detail)
        if (
            planetary.get("resident_detailed_levels") != detailed_count
            or planetary.get("resident_reduced_levels") != reduced_count
        ):
            invalid = True
            problems.append(
                issue(
                    "planetary_resident_material_counts_mismatch",
                    "Rejected",
                    "planetary_streaming.resident_material_detail",
                    "resident detail counters must exactly match the per-LOD states",
                )
            )

    observation_fault = (
        planetary.get("resident_observation_valid") is not True
        or planetary.get("resident_entity_count_overflow") is not False
        or planetary.get("resident_duplicate_levels") != 0
        or planetary.get("resident_out_of_range_levels") != 0
        or planetary.get("resident_scheduler_mismatch") is not False
        or planetary.get("resident_budget_exceeded") is not False
        or planetary.get("resident_observation_rejections") != 0
    )
    if observation_fault:
        invalid = True
        problems.append(
            issue(
                "planetary_residency_observation_rejected",
                "Rejected",
                "planetary_streaming.resident_observation_valid",
                "post-deferred ECS residency observation reported overflow, invalid levels, mismatch, budget failure, or a prior rejection episode",
            )
        )

    fluid_observation_fault = (
        planetary.get("resident_fluid_observation_valid") is not True
        or planetary.get("resident_fluid_kind_integrity_valid") is not True
        or planetary.get("resident_fluid_entity_count_overflow") is not False
        or planetary.get("resident_fluid_duplicate_slots") != 0
        or planetary.get("resident_fluid_out_of_range_levels") != 0
        or planetary.get("resident_fluid_scheduler_mismatch") is not False
        or planetary.get("resident_fluid_budget_exceeded") is not False
        or planetary.get("resident_fluid_observation_rejections") != 0
    )
    if fluid_observation_fault:
        invalid = True
        problems.append(
            issue(
                "planetary_fluid_residency_observation_rejected",
                "Rejected",
                "planetary_streaming.resident_fluid_observation_valid",
                "post-deferred fluid ECS residency reported overflow, duplicate slots, invalid levels, scheduler mismatch, budget failure, or a prior rejection episode",
            )
        )

    cohort_observation_fault = (
        planetary.get("resident_semantic_cohort_observation_valid") is not True
        or planetary.get("resident_semantic_cohort_payload_integrity_valid") is not True
        or planetary.get("resident_semantic_cohort_entity_count_overflow") is not False
        or planetary.get("resident_semantic_cohort_scheduler_mismatch") is not False
        or planetary.get("resident_semantic_cohort_budget_exceeded") is not False
        or planetary.get("resident_semantic_cohort_observation_rejections") != 0
    )
    if cohort_observation_fault:
        invalid = True
        problems.append(
            issue(
                "planetary_semantic_cohort_observation_rejected",
                "Rejected",
                "planetary_streaming.resident_semantic_cohort_observation_valid",
                "post-deferred semantic-cohort ECS residency reported invalid payload, overflow, scheduler mismatch, budget failure, or a prior rejection episode",
            )
        )

    scheduler_pairs = (
        ("resident_entities", "scheduler_resident_entities"),
        ("resident_vertices", "scheduler_resident_vertices"),
        ("resident_indices", "scheduler_resident_indices"),
        ("resident_mesh_bytes", "scheduler_resident_mesh_bytes"),
        ("ring_vertices", "scheduler_ring_vertices"),
        ("ring_indices", "scheduler_ring_indices"),
        ("resident_fluid_entities", "scheduler_resident_fluid_entities"),
        ("resident_fluid_vertices", "scheduler_resident_fluid_vertices"),
        ("resident_fluid_indices", "scheduler_resident_fluid_indices"),
        ("resident_fluid_mesh_bytes", "scheduler_resident_fluid_mesh_bytes"),
        ("fluid_ring_vertices", "scheduler_fluid_ring_vertices"),
        ("fluid_ring_indices", "scheduler_fluid_ring_indices"),
        ("resident_water_indices", "scheduler_resident_water_indices"),
        ("resident_lava_indices", "scheduler_resident_lava_indices"),
        ("water_ring_indices", "scheduler_water_ring_indices"),
        ("lava_ring_indices", "scheduler_lava_ring_indices"),
        ("resident_semantic_cohort_entities", "scheduler_resident_semantic_cohort_entities"),
        ("resident_semantic_cohort_vertices", "scheduler_resident_semantic_cohort_vertices"),
        ("resident_semantic_cohort_indices", "scheduler_resident_semantic_cohort_indices"),
        ("resident_semantic_cohort_mesh_bytes", "scheduler_resident_semantic_cohort_mesh_bytes"),
        ("resident_semantic_cohort_count", "scheduler_resident_semantic_cohort_count"),
        ("resident_semantic_cohort_kind_counts", "scheduler_resident_semantic_cohort_kind_counts"),
    )
    if any(planetary.get(observed) != planetary.get(scheduled) for observed, scheduled in scheduler_pairs):
        invalid = True
        problems.append(
            issue(
                "planetary_scheduler_observation_mismatch",
                "Rejected",
                "planetary_streaming.scheduler_resident_entities",
                "scheduler bookkeeping must exactly match post-deferred ECS residency at evidence capture",
            )
        )
    for field in ("last_build_ms", "max_build_ms"):
        if not is_finite_number(planetary.get(field), minimum=0.0):
            invalid = True
            problems.append(
                issue(
                    "invalid_planetary_timing",
                    "Rejected",
                    f"planetary_streaming.{field}",
                    "planetary build timing must be finite and non-negative",
                )
            )
    if is_finite_number(planetary.get("last_build_ms"), minimum=0.0) and is_finite_number(
        planetary.get("max_build_ms"), minimum=0.0
    ) and float(planetary["last_build_ms"]) > float(planetary["max_build_ms"]):
        invalid = True
        problems.append(
            issue(
                "planetary_timing_order_invalid",
                "Rejected",
                "planetary_streaming.max_build_ms",
                "maximum build time cannot be below the last build time",
            )
        )

    comparisons = (
        ("resident_entities", "budget_entities"),
        ("resident_vertices", "budget_vertices"),
        ("resident_indices", "budget_indices"),
        ("resident_mesh_bytes", "budget_mesh_bytes"),
        ("scheduler_resident_entities", "budget_entities"),
        ("scheduler_resident_vertices", "budget_vertices"),
        ("scheduler_resident_indices", "budget_indices"),
        ("scheduler_resident_mesh_bytes", "budget_mesh_bytes"),
        ("resident_fluid_entities", "budget_fluid_entities"),
        ("resident_fluid_vertices", "budget_fluid_vertices"),
        ("resident_fluid_indices", "budget_fluid_indices"),
        ("resident_fluid_mesh_bytes", "budget_fluid_mesh_bytes"),
        ("scheduler_resident_fluid_entities", "budget_fluid_entities"),
        ("scheduler_resident_fluid_vertices", "budget_fluid_vertices"),
        ("scheduler_resident_fluid_indices", "budget_fluid_indices"),
        ("scheduler_resident_fluid_mesh_bytes", "budget_fluid_mesh_bytes"),
        ("resident_semantic_cohort_entities", "budget_semantic_cohort_entities"),
        ("resident_semantic_cohort_vertices", "budget_semantic_cohort_vertices"),
        ("resident_semantic_cohort_indices", "budget_semantic_cohort_indices"),
        ("resident_semantic_cohort_mesh_bytes", "budget_semantic_cohort_mesh_bytes"),
        ("scheduler_resident_semantic_cohort_entities", "budget_semantic_cohort_entities"),
        ("scheduler_resident_semantic_cohort_vertices", "budget_semantic_cohort_vertices"),
        ("scheduler_resident_semantic_cohort_indices", "budget_semantic_cohort_indices"),
        ("scheduler_resident_semantic_cohort_mesh_bytes", "budget_semantic_cohort_mesh_bytes"),
        ("last_semantic_cohort_hash_scans", "budget_semantic_cohort_hash_scans"),
        ("last_semantic_cohort_height_queries", "budget_semantic_cohort_height_queries"),
        ("last_semantic_cohort_biome_queries", "budget_semantic_cohort_biome_queries"),
        ("last_semantic_cohort_vertices", "budget_semantic_cohort_vertices"),
        ("last_semantic_cohort_indices", "budget_semantic_cohort_indices"),
        ("live_sample_cache_bytes", "budget_sample_cache_bytes"),
        ("peak_live_sample_cache_bytes", "budget_sample_cache_bytes"),
    )
    for live_field, budget_field in comparisons:
        live = planetary.get(live_field)
        budget = planetary.get(budget_field)
        if is_non_negative_integer(live) and is_non_negative_integer(budget) and live > budget:
            invalid = True
            problems.append(
                issue(
                    "planetary_budget_exceeded",
                    "Rejected",
                    f"planetary_streaming.{live_field}",
                    f"live value exceeds {budget_field}",
                )
            )

    live_cache_windows = planetary.get("live_sample_cache_windows")
    peak_cache_windows = planetary.get("peak_live_sample_cache_windows")
    live_cache_bytes = planetary.get("live_sample_cache_bytes")
    peak_cache_bytes = planetary.get("peak_live_sample_cache_bytes")
    if (
        is_non_negative_integer(live_cache_windows)
        and live_cache_windows > PLANETARY_FAR_FIELD_LEVELS
    ) or (
        is_non_negative_integer(peak_cache_windows)
        and peak_cache_windows > PLANETARY_FAR_FIELD_LEVELS
    ):
        invalid = True
        problems.append(
            issue(
                "planetary_cache_window_cap_exceeded",
                "Rejected",
                "planetary_streaming.live_sample_cache_windows",
                f"current and peak cache windows must remain at or below {PLANETARY_FAR_FIELD_LEVELS}",
            )
        )
    if (
        is_non_negative_integer(live_cache_windows)
        and is_non_negative_integer(peak_cache_windows)
        and peak_cache_windows < live_cache_windows
    ) or (
        is_non_negative_integer(live_cache_bytes)
        and is_non_negative_integer(peak_cache_bytes)
        and peak_cache_bytes < live_cache_bytes
    ):
        invalid = True
        problems.append(
            issue(
                "planetary_cache_peak_below_live",
                "Rejected",
                "planetary_streaming.peak_live_sample_cache_windows",
                "cache peak populations must not be below the corresponding live populations",
            )
        )
    budget_rejections = planetary.get("budget_rejections")
    if is_non_negative_integer(budget_rejections) and budget_rejections > 0:
        invalid = True
        problems.append(
            issue(
                "runtime_budget_rejections",
                "Rejected",
                "planetary_streaming.budget_rejections",
                "runtime reported one or more budget rejections",
            )
        )

    hydro_mode = planetary.get("hydro_mode")
    if hydro_mode == "Disabled":
        disabled_fluid_fields = (
            "resident_fluid_entities",
            "resident_fluid_vertices",
            "resident_fluid_indices",
            "resident_fluid_mesh_bytes",
            "scheduler_resident_fluid_entities",
            "scheduler_resident_fluid_vertices",
            "scheduler_resident_fluid_indices",
            "scheduler_resident_fluid_mesh_bytes",
            "last_fluid_classification_queries",
            "last_fluid_biome_queries",
            "last_fluid_vertices",
            "last_fluid_indices",
            "resident_water_indices",
            "resident_lava_indices",
            "scheduler_resident_water_indices",
            "scheduler_resident_lava_indices",
            "last_water_indices",
            "last_lava_indices",
        )
        disabled_fluid_arrays = (
            "fluid_ring_vertices",
            "fluid_ring_indices",
            "scheduler_fluid_ring_vertices",
            "scheduler_fluid_ring_indices",
            "water_ring_indices",
            "lava_ring_indices",
            "scheduler_water_ring_indices",
            "scheduler_lava_ring_indices",
        )
        if any(planetary.get(field) != 0 for field in disabled_fluid_fields) or any(
            planetary.get(field) != [0] * PLANETARY_FAR_FIELD_LEVELS
            for field in disabled_fluid_arrays
        ):
            invalid = True
            problems.append(
                issue(
                    "planetary_disabled_hydro_has_live_work",
                    "Rejected",
                    "planetary_streaming.hydro_mode",
                    "Disabled far hydro must report zero fluid residency, scheduler payload, latest fluid work, and per-ring populations",
                )
            )

    semantic_cohort_mode = planetary.get("semantic_cohort_mode")
    if semantic_cohort_mode == "Disabled":
        disabled_cohort_fields = (
            "resident_semantic_cohort_entities",
            "resident_semantic_cohort_vertices",
            "resident_semantic_cohort_indices",
            "resident_semantic_cohort_mesh_bytes",
            "resident_semantic_cohort_count",
            "scheduler_resident_semantic_cohort_entities",
            "scheduler_resident_semantic_cohort_vertices",
            "scheduler_resident_semantic_cohort_indices",
            "scheduler_resident_semantic_cohort_mesh_bytes",
            "scheduler_resident_semantic_cohort_count",
            "last_semantic_cohort_hash_scans",
            "last_semantic_cohort_height_queries",
            "last_semantic_cohort_biome_queries",
            "last_semantic_cohort_candidates",
            "last_semantic_cohort_emitted",
            "last_semantic_cohort_vertices",
            "last_semantic_cohort_indices",
        )
        disabled_cohort_arrays = (
            "resident_semantic_cohort_kind_counts",
            "scheduler_resident_semantic_cohort_kind_counts",
            "last_semantic_cohort_kind_counts",
        )
        if any(planetary.get(field) != 0 for field in disabled_cohort_fields) or any(
            planetary.get(field) != [0] * PLANETARY_SEMANTIC_COHORT_KIND_COUNT
            for field in disabled_cohort_arrays
        ):
            invalid = True
            problems.append(
                issue(
                    "planetary_disabled_semantic_cohorts_have_live_work",
                    "Rejected",
                    "planetary_streaming.semantic_cohort_mode",
                    "Disabled semantic cohorts must report zero live, scheduler, latest-work, and per-kind populations",
                )
            )
    elif semantic_cohort_mode == "SilhouettesV1":
        hash_scans = planetary.get("last_semantic_cohort_hash_scans")
        height_queries = planetary.get("last_semantic_cohort_height_queries")
        biome_queries = planetary.get("last_semantic_cohort_biome_queries")
        candidates = planetary.get("last_semantic_cohort_candidates")
        if (
            hash_scans
            not in {
                0,
                PLANETARY_EXPECTED_SEMANTIC_COHORT_BUDGETS[
                    "budget_semantic_cohort_hash_scans"
                ],
            }
            or (hash_scans == 0 and any(
                planetary.get(field) != 0
                for field in (
                    "last_semantic_cohort_height_queries",
                    "last_semantic_cohort_biome_queries",
                    "last_semantic_cohort_candidates",
                    "last_semantic_cohort_emitted",
                    "last_semantic_cohort_vertices",
                    "last_semantic_cohort_indices",
                )
            ))
            or (hash_scans != 0 and (height_queries != candidates or biome_queries != candidates))
        ):
            invalid = True
            problems.append(
                issue(
                    "semantic_cohort_latest_work_scope_mismatch",
                    "Rejected",
                    "planetary_streaming.last_semantic_cohort_hash_scans",
                    "SilhouettesV1 latest L5 work must scan the fixed lattice and classify each admitted candidate exactly once",
                )
            )

    blocked = planetary.get("enabled") is False
    if blocked:
        problems.append(
            issue(
                "planetary_streaming_disabled",
                "Blocked",
                "planetary_streaming.enabled",
                "serialized budgets are present, but planetary streaming was disabled",
            )
        )
    classification = "Rejected" if invalid else "Blocked" if blocked else "Passed"
    return output, claim(
        f"{run_key}:planetary_budgets",
        classification,
        "Recorded planetary live values are within their serialized hard budgets.",
    ), problems


def normalize_reported_path(value: str) -> Path:
    normalized = value.replace("\\", os.sep).replace("/", os.sep)
    return Path(normalized)


def resolve_reported_screenshot(
    reported: str, run_dir: Path, repo_root: Path
) -> tuple[Path | None, str | None]:
    candidate_path = normalize_reported_path(reported)
    if ".." in candidate_path.parts:
        return None, "path traversal is not allowed"
    candidates: list[Path]
    if candidate_path.is_absolute():
        candidates = [candidate_path]
    else:
        candidates = [run_dir / candidate_path, repo_root / candidate_path]

    safe_candidates: list[Path] = []
    resolution_failed = False
    for candidate in candidates:
        try:
            resolved = candidate.resolve(strict=False)
        except (OSError, RuntimeError):
            resolution_failed = True
            continue
        if path_is_within(resolved, run_dir):
            safe_candidates.append(resolved)
    if not safe_candidates:
        if resolution_failed:
            return None, "reported screenshot path could not be resolved safely"
        return None, "reported screenshot resolves outside the explicit run directory"
    for candidate in safe_candidates:
        if candidate.is_file():
            return candidate, None
    return safe_candidates[0], None


def inspect_screenshot_file(
    path: Path, repo_root: Path
) -> tuple[dict[str, Any], dict[str, Any] | None, list[dict[str, str]]]:
    display = display_path(path, repo_root)
    problems: list[dict[str, str]] = []
    try:
        digest, complete = hash_and_probe_png(path)
    except OSError as error:
        problems.append(
            issue(
                "screenshot_read_failed",
                "Rejected",
                display,
                f"screenshot could not be read: {error.strerror or type(error).__name__}",
            )
        )
        return {
            "classification": "Rejected",
            "path": display,
        }, None, problems

    classification = "Passed" if complete else "Rejected"
    if not complete:
        problems.append(
            issue(
                "invalid_png",
                "Rejected",
                display,
                "screenshot lacks a valid PNG signature or terminal IEND chunk",
            )
        )
    file_record = {
        "classification": classification,
        "path": display,
        "png_complete": complete,
        "sha256": digest["sha256"],
        "size_bytes": digest["size_bytes"],
    }
    hash_record = {
        "kind": "screenshot",
        "path": display,
        "sha256": digest["sha256"],
        "size_bytes": digest["size_bytes"],
    }
    return file_record, hash_record, problems


def inspect_screenshots(
    report: dict[str, Any] | None,
    run_dir: Path,
    repo_root: Path,
    run_key: str,
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, str]], list[dict[str, Any]]]:
    problems: list[dict[str, str]] = []
    hashes: list[dict[str, Any]] = []
    files_by_path: dict[str, dict[str, Any]] = {}

    try:
        direct_children = sorted(run_dir.iterdir(), key=lambda path: path.name.casefold())
    except OSError as error:
        direct_children = []
        problems.append(
            issue(
                "run_directory_scan_failed",
                "Rejected",
                display_path(run_dir, repo_root),
                "explicit run directory could not be enumerated: "
                f"{error.strerror or type(error).__name__}",
            )
        )

    for child in direct_children:
        if child.suffix.lower() != ".png" or not child.is_file():
            continue
        try:
            resolved = child.resolve(strict=False)
        except (OSError, RuntimeError) as error:
            problems.append(
                issue(
                    "screenshot_path_resolution_failed",
                    "Rejected",
                    child.name,
                    f"direct screenshot path could not be resolved safely: {error}",
                )
            )
            continue
        if not path_is_within(resolved, run_dir):
            problems.append(
                issue(
                    "screenshot_symlink_escape",
                    "Rejected",
                    child.name,
                    "direct screenshot resolves outside the explicit run directory",
                )
            )
            continue
        record, hash_record, file_problems = inspect_screenshot_file(resolved, repo_root)
        files_by_path[record["path"]] = record
        if hash_record is not None:
            hashes.append(hash_record)
        problems.extend(file_problems)

    reported_value = report.get("screenshots") if isinstance(report, dict) else None
    reported_paths: list[str] = []
    referenced_files: list[str] = []
    seen_resolved: set[str] = set()
    reference_invalid = False
    if not isinstance(reported_value, list):
        problems.append(
            issue(
                "missing_screenshot_list",
                "Blocked",
                "screenshots",
                "report does not contain a screenshot list",
            )
        )
    else:
        for index, value in enumerate(reported_value):
            field = f"screenshots[{index}]"
            if not isinstance(value, str) or not value.strip():
                reference_invalid = True
                problems.append(
                    issue(
                        "invalid_screenshot_reference",
                        "Rejected",
                        field,
                        "screenshot reference must be non-empty text",
                    )
                )
                continue
            if len(value) > 4_096 or not all(character.isprintable() for character in value):
                reference_invalid = True
                problems.append(
                    issue(
                        "unsafe_screenshot_reference_text",
                        "Rejected",
                        field,
                        "screenshot path contains control characters or exceeds 4096 characters",
                    )
                )
                continue
            reported_paths.append(value)
            resolved, resolve_error = resolve_reported_screenshot(value, run_dir, repo_root)
            if resolve_error is not None or resolved is None:
                reference_invalid = True
                problems.append(
                    issue(
                        "unsafe_screenshot_reference",
                        "Rejected",
                        field,
                        resolve_error or "screenshot reference could not be resolved safely",
                    )
                )
                continue
            display = display_path(resolved, repo_root)
            if display in seen_resolved:
                reference_invalid = True
                problems.append(
                    issue(
                        "duplicate_screenshot_reference",
                        "Rejected",
                        field,
                        "multiple report entries resolve to the same screenshot",
                    )
                )
                continue
            seen_resolved.add(display)
            referenced_files.append(display)
            if not resolved.is_file():
                reference_invalid = True
                problems.append(
                    issue(
                        "missing_screenshot",
                        "Rejected",
                        field,
                        "reported screenshot does not exist",
                    )
                )
                continue
            if display not in files_by_path:
                record, hash_record, file_problems = inspect_screenshot_file(resolved, repo_root)
                files_by_path[display] = record
                if hash_record is not None:
                    hashes.append(hash_record)
                problems.extend(file_problems)
            if files_by_path[display]["classification"] != "Passed":
                reference_invalid = True

    unreferenced = sorted(set(files_by_path) - set(referenced_files))
    if unreferenced:
        problems.append(
            issue(
                "unreferenced_screenshots",
                "Observed",
                "screenshots",
                f"{len(unreferenced)} direct PNG file(s) are present but not "
                "referenced by the report",
            )
        )

    if reported_value is None or not isinstance(reported_value, list):
        classification = "Blocked"
    elif reference_invalid or any(
        record["classification"] == "Rejected" for record in files_by_path.values()
    ):
        classification = "Rejected"
    elif not reported_paths:
        classification = "Blocked"
        problems.append(
            issue(
                "empty_screenshot_list",
                "Blocked",
                "screenshots",
                "report contains no screenshot evidence",
            )
        )
    else:
        classification = "Passed"

    observation = {
        "actual_files": [files_by_path[path] for path in sorted(files_by_path)],
        "referenced_files": sorted(referenced_files),
        "reported_paths": sorted(reported_paths),
        "unreferenced_files": unreferenced,
    }
    screenshot_claim = claim(
        f"{run_key}:screenshot_integrity",
        classification,
        "Every reported screenshot resolves inside the explicit run directory "
        "and passes file integrity checks.",
        referenced_files,
    )
    return observation, screenshot_claim, problems, hashes


def read_report(
    report_path: Path, repo_root: Path, run_dir: Path
) -> tuple[dict[str, Any] | None, dict[str, Any] | None, list[dict[str, str]]]:
    display = display_path(report_path, repo_root)
    problems: list[dict[str, str]] = []
    if not path_is_within(report_path, run_dir):
        problems.append(
            issue(
                "report_symlink_escape",
                "Rejected",
                display,
                "report.ron resolves outside the explicit QA run directory",
            )
        )
        return None, None, problems
    if not report_path.is_file():
        problems.append(
            issue(
                "missing_report",
                "Rejected",
                display,
                "explicit run directory does not contain report.ron",
            )
        )
        return None, None, problems

    try:
        digest, report_bytes = hash_and_capture_file(
            report_path, capture_limit=MAX_REPORT_BYTES
        )
    except OSError as error:
        problems.append(
            issue(
                "report_read_failed",
                "Rejected",
                display,
                f"report could not be read: {error.strerror or type(error).__name__}",
            )
        )
        return None, None, problems
    hash_record = {
        "kind": "report",
        "path": display,
        "sha256": digest["sha256"],
        "size_bytes": digest["size_bytes"],
    }
    if report_bytes is None:
        problems.append(
            issue(
                "report_too_large",
                "Rejected",
                display,
                f"report exceeds the {MAX_REPORT_BYTES}-byte parser cap",
            )
        )
        return None, hash_record, problems

    try:
        text = report_bytes.decode("utf-8")
        parsed = RonParser(text).parse()
    except (OSError, UnicodeError, RonParseError) as error:
        problems.append(
            issue(
                "malformed_report",
                "Rejected",
                display,
                f"report could not be parsed safely: {error}",
            )
        )
        return None, hash_record, problems
    if not isinstance(parsed, dict):
        problems.append(
            issue(
                "invalid_report_root",
                "Rejected",
                display,
                "report root must be a named-field RON structure",
            )
        )
        return None, hash_record, problems
    non_finite = find_non_finite(parsed)
    if non_finite:
        problems.append(
            issue(
                "non_finite_report_value",
                "Rejected",
                non_finite[0],
                f"report contains {len(non_finite)} non-finite numeric value(s)",
            )
        )
        return None, hash_record, problems
    return parsed, hash_record, problems


def process_run(
    run_dir: Path, repo_root: Path
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    run_dir = run_dir.resolve(strict=False)
    run_key = display_path(run_dir, repo_root)
    run_issues: list[dict[str, str]] = []
    run_claims: list[dict[str, Any]] = []
    file_hashes: list[dict[str, Any]] = []
    observations: dict[str, Any] = {}

    if not run_dir.is_dir():
        run_issues.append(
            issue(
                "missing_run_directory",
                "Rejected",
                run_key,
                "explicit QA run path is not a directory",
            )
        )
        run_claims.append(
            claim(
                f"{run_key}:report_integrity",
                "Rejected",
                "No readable report.ron exists in the explicit run directory.",
            )
        )
        return {
            "claims": run_claims,
            "input_path": run_key,
            "issues": run_issues,
            "raw_observations": observations,
            "overall_classification": "Rejected",
            "report_schema_variant": "unavailable",
        }, file_hashes

    report_path = run_dir / "report.ron"
    report, report_hash, report_issues = read_report(report_path, repo_root, run_dir)
    run_issues.extend(report_issues)
    if report_hash is not None:
        file_hashes.append(report_hash)
    if report is None:
        run_claims.append(
            claim(
                f"{run_key}:report_integrity",
                "Rejected",
                "report.ron is missing, malformed, oversized, or contains non-finite values.",
                [report_hash["path"]] if report_hash else [],
            )
        )
        schema_variant = "unavailable"
    else:
        run_claims.append(
            claim(
                f"{run_key}:report_integrity",
                "Passed",
                "report.ron parsed within fixed size, depth, and node limits.",
                [report_hash["path"]] if report_hash else [],
            )
        )
        report_schema_version = report.get("qa_report_schema_version")
        if report_schema_version == CURRENT_QA_REPORT_SCHEMA_VERSION:
            schema_variant = "current"
        elif report_schema_version is None or report_schema_version in LEGACY_QA_REPORT_SCHEMA_VERSIONS:
            schema_variant = "legacy"
        else:
            schema_variant = "unsupported"

        if schema_variant == "legacy":
            run_issues.append(
                issue(
                    "legacy_missing_current_qa_report_schema",
                    "Blocked",
                    "qa_report_schema_version",
                    f"current evidence requires QA report schema {CURRENT_QA_REPORT_SCHEMA_VERSION}",
                )
            )
        elif schema_variant == "unsupported":
            run_issues.append(
                issue(
                    "unsupported_qa_report_schema",
                    "Rejected",
                    "qa_report_schema_version",
                    f"report schema {report_schema_version!r} is not explicitly supported; current evidence requires exact {CURRENT_QA_REPORT_SCHEMA_VERSION}",
                )
            )

        identity, identity_claim, identity_issues = validate_run_identity(
            report, run_key, require_current=schema_variant == "current"
        )
        viewport, viewport_claim, viewport_issues = validate_viewport(report, run_key)
        if schema_variant == "legacy":
            world_edit_store = copy_known_fields(report, WORLD_EDIT_STORE_FIELDS)
            world_edit_store_claim = claim(
                f"{run_key}:world_edit_store",
                "Blocked",
                "Legacy QA evidence predates the schema-2.3 edit-store identity contract.",
            )
            world_edit_store_issues: list[dict[str, str]] = []
            route = copy_known_fields(
                report,
                (
                    "route_focus",
                    "requested_route_distance_m",
                    "max_horizontal_displacement_m",
                    "requested_duration_seconds",
                    "duration_seconds",
                    "warmup_seconds",
                    "write_tail_seconds",
                    "frames",
                    "average_fps",
                    "max_frame_ms",
                    "final_smoothed_fps",
                ),
            )
            route_claim = claim(
                f"{run_key}:route_observation",
                "Blocked",
                "Legacy QA route observations predate schema-2.3 terrain-grammar binding.",
            )
            route_issues: list[dict[str, str]] = []
            frame_times = copy_known_fields(
                report.get("route_frame_times", {}), ROUTE_FRAME_TIME_FIELDS
            )
            frame_claim = claim(
                f"{run_key}:route_frame_times",
                "Blocked",
                "Legacy frame-time data is historical and not publishable under manifest 1.2.",
            )
            frame_issues: list[dict[str, str]] = []
            legacy_planetary = report.get("planetary_streaming")
            planetary = (
                {
                    "budgets": copy_known_fields(legacy_planetary, PLANETARY_BUDGET_FIELDS),
                    "live": copy_known_fields(legacy_planetary, PLANETARY_LIVE_FIELDS),
                    "telemetry": copy_known_fields(legacy_planetary, PLANETARY_TELEMETRY_FIELDS),
                }
                if isinstance(legacy_planetary, dict)
                else None
            )
            planetary_claim = claim(
                f"{run_key}:planetary_budgets",
                "Blocked",
                "Legacy planetary observations are not publishable under the current QA contract.",
            )
            planetary_issues: list[dict[str, str]] = []
        else:
            world_edit_store, world_edit_store_claim, world_edit_store_issues = (
                validate_world_edit_store(report, run_key)
            )
            route, route_claim, route_issues = validate_route(report, run_key)
            frame_times, frame_claim, frame_issues = validate_route_frame_times(report, run_key)
            planetary, planetary_claim, planetary_issues = validate_planetary_streaming(
                report, run_key
            )
        observations.update(
            {
                "planetary_streaming": planetary,
                "route": route,
                "route_frame_times": frame_times,
                "run_identity": identity,
                "world_edit_store": world_edit_store,
                "viewport": viewport,
            }
        )
        run_claims.extend(
            [
                identity_claim,
                world_edit_store_claim,
                viewport_claim,
                route_claim,
                frame_claim,
                planetary_claim,
            ]
        )
        run_issues.extend(
            identity_issues
            + world_edit_store_issues
            + viewport_issues
            + route_issues
            + frame_issues
            + planetary_issues
        )

    screenshot_observation, screenshot_claim, screenshot_issues, screenshot_hashes = (
        inspect_screenshots(report, run_dir, repo_root, run_key)
    )
    observations["screenshots"] = screenshot_observation
    run_claims.append(screenshot_claim)
    run_issues.extend(screenshot_issues)
    file_hashes.extend(screenshot_hashes)

    run_claims.sort(key=lambda item: item["id"])
    run_issues.sort(key=lambda item: (item["classification"], item["code"], item["field"]))
    overall_inputs = [item["classification"] for item in run_claims]
    overall_inputs.extend(item["classification"] for item in run_issues)
    overall = aggregate_classification(overall_inputs)
    return {
        "claims": run_claims,
        "input_path": run_key,
        "issues": run_issues,
        "raw_observations": observations,
        "overall_classification": overall,
        "report_schema_variant": schema_variant,
    }, file_hashes


def build_manifest(
    qa_run_directories: Sequence[Path | str],
    *,
    repo_root: Path | str,
    generated_at_utc: str | None = None,
) -> dict[str, Any]:
    try:
        root = Path(repo_root).resolve(strict=False)
    except (OSError, RuntimeError) as error:
        raise EvidenceManifestError(
            f"repository root could not be resolved safely: {error}"
        ) from error
    top_issues: list[dict[str, str]] = []
    top_claims: list[dict[str, Any]] = []
    accepted_inputs: dict[str, Path] = {}
    raw_argument_count = len(qa_run_directories)
    duplicate_count = 0

    for raw_path in qa_run_directories:
        path = Path(raw_path)
        display_raw = display_path(path, root)
        if path.name.casefold() == "latest":
            top_issues.append(
                issue(
                    "implicit_latest_forbidden",
                    "Rejected",
                    display_raw,
                    "a directory named latest is not an explicit immutable QA run",
                )
            )
            continue
        if ".." in path.parts:
            top_issues.append(
                issue(
                    "run_path_traversal",
                    "Rejected",
                    display_raw,
                    "QA run arguments must not contain parent traversal",
                )
            )
            continue
        try:
            resolved = path.resolve(strict=False)
        except (OSError, RuntimeError) as error:
            top_issues.append(
                issue(
                    "run_path_resolution_failed",
                    "Rejected",
                    display_raw,
                    f"QA run path could not be resolved safely: {error}",
                )
            )
            continue
        if resolved.name.casefold() == "latest":
            top_issues.append(
                issue(
                    "implicit_latest_forbidden",
                    "Rejected",
                    display_path(resolved, root),
                    "a path resolving to latest is not an explicit immutable QA run",
                )
            )
            continue
        if not path_is_within(resolved, root):
            top_issues.append(
                issue(
                    "run_outside_repository",
                    "Rejected",
                    "external",
                    "public evidence runs must resolve inside the repository",
                )
            )
            continue
        canonical = os.path.normcase(str(resolved))
        if canonical in accepted_inputs:
            duplicate_count += 1
            top_issues.append(
                issue(
                    "duplicate_run_input",
                    "Rejected",
                    display_path(resolved, root),
                    "the same canonical QA run was supplied more than once",
                )
            )
            continue
        accepted_inputs[canonical] = resolved

    if duplicate_count:
        top_claims.append(
            claim(
                "manifest:input_uniqueness",
                "Rejected",
                f"{duplicate_count} duplicate QA run argument(s) were removed.",
            )
        )
    else:
        top_claims.append(
            claim(
                "manifest:input_uniqueness",
                "Passed",
                "Every accepted QA run input is canonical and unique.",
            )
        )
    if not accepted_inputs:
        top_claims.append(
            claim(
                "manifest:explicit_inputs",
                "Rejected",
                "No safe explicit QA run directory was provided.",
            )
        )
    else:
        top_claims.append(
            claim(
                "manifest:explicit_inputs",
                "Passed",
                "Only explicitly named QA run directories were inspected.",
            )
        )

    runs: list[dict[str, Any]] = []
    file_hashes: list[dict[str, Any]] = []
    for run_dir in sorted(accepted_inputs.values(), key=lambda path: str(path).casefold()):
        run, hashes = process_run(run_dir, root)
        runs.append(run)
        file_hashes.extend(hashes)

    generator_path = Path(__file__).resolve(strict=False)
    generator_hash = hash_file(generator_path)
    generator_source = {
        "kind": "generator_source",
        "path": display_path(generator_path, root),
        "sha256": generator_hash["sha256"],
        "size_bytes": generator_hash["size_bytes"],
    }
    file_hashes.append(generator_source)

    deduplicated_hashes: dict[tuple[str, str], dict[str, Any]] = {}
    for record in file_hashes:
        deduplicated_hashes[(record["kind"], record["path"])] = record
    sorted_hashes = sorted(
        deduplicated_hashes.values(), key=lambda item: (item["path"], item["kind"])
    )

    top_claims.sort(key=lambda item: item["id"])
    top_issues.sort(key=lambda item: (item["classification"], item["code"], item["field"]))
    overall_inputs = [item["classification"] for item in top_claims]
    overall_inputs.extend(item["classification"] for item in top_issues)
    overall_inputs.extend(run["overall_classification"] for run in runs)
    overall = aggregate_classification(overall_inputs)
    classification_counts = {classification: 0 for classification in CLASSIFICATIONS}
    issue_counts = {classification: 0 for classification in CLASSIFICATIONS}
    for item in top_claims:
        classification_counts[item["classification"]] += 1
    for item in top_issues:
        issue_counts[item["classification"]] += 1
    for run in runs:
        for item in run["claims"]:
            classification_counts[item["classification"]] += 1
        for item in run["issues"]:
            issue_counts[item["classification"]] += 1

    manifest = {
        "claim_classifications": list(CLASSIFICATIONS),
        "claims": top_claims,
        "file_hashes": sorted_hashes,
        "generated_at_utc": generated_at_utc or utc_now_text(),
        "generator": {
            "name": "voxel-native-evidence-manifest",
            "source_path": generator_source["path"],
            "source_sha256": generator_source["sha256"],
            "version": GENERATOR_VERSION,
        },
        "inputs": {
            "accepted_run_count": len(accepted_inputs),
            "argument_count": raw_argument_count,
            "qa_run_directories": [run["input_path"] for run in runs],
            "selection_policy": "explicit_repo_contained_directories_only_no_latest_no_global_scan",
        },
        "issues": top_issues,
        "overall_classification": overall,
        "runs": runs,
        "schema_version": SCHEMA_VERSION,
        "summary": {
            "claim_counts": classification_counts,
            "file_hash_count": len(sorted_hashes),
            "issue_counts": issue_counts,
            "run_count": len(runs),
        },
    }
    # Enforce JSON-number safety before returning data to any writer.
    json.dumps(manifest, allow_nan=False, ensure_ascii=False, sort_keys=True)
    return manifest


def manifest_json(manifest: dict[str, Any]) -> str:
    return json.dumps(
        manifest,
        allow_nan=False,
        ensure_ascii=False,
        indent=2,
        sort_keys=True,
    ) + "\n"


def write_manifest(manifest: dict[str, Any], output: Path, repo_root: Path) -> Path:
    destination = validate_output_path(output, repo_root)
    destination.parent.mkdir(parents=True, exist_ok=True)
    text = manifest_json(manifest)
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            newline="\n",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary.write(text)
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


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--qa-run",
        action="append",
        required=True,
        metavar="DIRECTORY",
        help="explicit QA run directory; repeat for multiple runs",
    )
    parser.add_argument(
        "--output",
        required=True,
        metavar="MANIFEST.json",
        help="explicit output JSON path outside saves/qa_runs/agent_runs",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    repo_root = Path(__file__).resolve().parents[2]
    try:
        output = validate_output_path(Path(args.output), repo_root)
        manifest = build_manifest(
            [Path(path) for path in args.qa_run],
            repo_root=repo_root,
        )
        write_manifest(manifest, output, repo_root)
    except EvidenceManifestError as error:
        print(f"evidence manifest rejected: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"evidence manifest I/O failure: {error}", file=sys.stderr)
        return 1

    print(
        f"wrote {output} with {manifest['summary']['run_count']} run(s); "
        f"classification={manifest['overall_classification']}"
    )
    return 0 if manifest["overall_classification"] == "Observed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
