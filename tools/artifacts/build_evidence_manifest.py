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


SCHEMA_VERSION = "1.0.0"
GENERATOR_VERSION = "1.0.0"
CURRENT_QA_REPORT_SCHEMA_VERSION = "2.0.0"
CLASSIFICATIONS = ("Passed", "Observed", "Rejected", "Planned", "Blocked")
PROTECTED_OUTPUT_DIRS = ("saves", "qa_runs", "agent_runs")
MAX_REPORT_BYTES = 4 * 1024 * 1024
MAX_RON_DEPTH = 128
MAX_RON_NODES = 100_000
MAX_RON_STRING_CHARS = 16_384
HASH_CHUNK_BYTES = 1024 * 1024
EXPECTED_ROUTE_FRAME_SCOPE = "active_route_only_warmup_and_write_tail_excluded"
EXPECTED_QUANTILE_METHOD = "nearest_rank_conservative_bucket_upper_bound"

RUN_IDENTITY_FIELDS = (
    "package_version",
    "build_profile",
    "instance_label",
    "world_name",
    "world_seed",
    "world_profile",
    "scenery_quality",
    "git_sha",
    "git_dirty",
    "source_fingerprint",
    "executable_hash",
    "toolchain",
    "hardware",
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
    "fluid_ring_vertices",
    "fluid_ring_indices",
    "resident_fluid_mesh_bytes",
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
    "budget_atomic_ring_build_bytes",
)
PLANETARY_TELEMETRY_FIELDS = (
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
    "scheduler_ring_vertices",
    "scheduler_ring_indices",
    "scheduler_fluid_ring_vertices",
    "scheduler_fluid_ring_indices",
    "resident_observation_valid",
    "resident_entity_count_overflow",
    "resident_duplicate_levels",
    "resident_out_of_range_levels",
    "resident_scheduler_mismatch",
    "resident_budget_exceeded",
    "resident_observation_rejections",
    "resident_fluid_observation_valid",
    "resident_fluid_entity_count_overflow",
    "resident_fluid_duplicate_slots",
    "resident_fluid_out_of_range_levels",
    "resident_fluid_scheduler_mismatch",
    "resident_fluid_budget_exceeded",
    "resident_fluid_observation_rejections",
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
        return resolved.as_posix()


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
    report: dict[str, Any], run_key: str
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


def validate_route(
    report: dict[str, Any], run_key: str
) -> tuple[dict[str, Any], dict[str, Any], list[dict[str, str]]]:
    output = copy_known_fields(report, ROUTE_FIELDS)
    problems: list[dict[str, str]] = []
    invalid = False
    blocked = False

    if not is_bounded_text(report.get("route_focus"), 64):
        invalid = True
        problems.append(
            issue(
                "invalid_route_focus",
                "Rejected",
                "route_focus",
                "route focus must be non-empty text",
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
        "resident_fluid_mesh_bytes",
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
        "resident_duplicate_levels",
        "resident_out_of_range_levels",
        "resident_observation_rejections",
        "resident_fluid_duplicate_slots",
        "resident_fluid_out_of_range_levels",
        "resident_fluid_observation_rejections",
        "last_biome_queries",
        "last_fluid_classification_queries",
        "last_fluid_biome_queries",
        "last_fluid_vertices",
        "last_fluid_indices",
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
        "resident_fluid_entity_count_overflow",
        "resident_fluid_scheduler_mismatch",
        "resident_fluid_budget_exceeded",
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
    for field in ("material_detail", "surface_material_mode", "hydro_mode", "last_cache_update"):
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
        )
        disabled_fluid_arrays = (
            "fluid_ring_vertices",
            "fluid_ring_indices",
            "scheduler_fluid_ring_vertices",
            "scheduler_fluid_ring_indices",
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
        schema_variant = (
            "current"
            if report.get("qa_report_schema_version")
            == CURRENT_QA_REPORT_SCHEMA_VERSION
            and isinstance(report.get("route_frame_times"), dict)
            and isinstance(report.get("run_identity"), dict)
            and report["run_identity"].get("build_profile") in {"debug", "release"}
            else "legacy"
        )
        if report.get("qa_report_schema_version") != CURRENT_QA_REPORT_SCHEMA_VERSION:
            run_issues.append(
                issue(
                    "legacy_missing_current_qa_report_schema",
                    "Blocked",
                    "qa_report_schema_version",
                    f"current evidence requires QA report schema {CURRENT_QA_REPORT_SCHEMA_VERSION}",
                )
            )

        identity, identity_claim, identity_issues = validate_run_identity(report, run_key)
        viewport, viewport_claim, viewport_issues = validate_viewport(report, run_key)
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
                "viewport": viewport,
            }
        )
        run_claims.extend(
            [identity_claim, viewport_claim, route_claim, frame_claim, planetary_claim]
        )
        run_issues.extend(
            identity_issues
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
        display_raw = path.as_posix()
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
            "selection_policy": "explicit_cli_directories_only_no_latest_no_global_scan",
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
