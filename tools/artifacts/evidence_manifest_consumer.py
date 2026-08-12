#!/usr/bin/env python3
"""Strict, bounded helpers for evidence-backed artifact builders.

This module consumes only the explicit JSON manifest produced by
``build_evidence_manifest.py``.  It never selects QA runs, scans ``qa_runs`` or
opens legacy ``status.ron`` files.  Builders fail closed when the manifest is
not a complete current-schema, Observed evidence set.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import math
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = "1.0.0"
GENERATOR_NAME = "voxel-native-evidence-manifest"
GENERATOR_VERSION = "1.0.0"
SELECTION_POLICY = "explicit_cli_directories_only_no_latest_no_global_scan"
CLASSIFICATIONS = ("Passed", "Observed", "Rejected", "Planned", "Blocked")
PROTECTED_OUTPUT_DIRS = ("saves", "qa_runs", "agent_runs")

MAX_MANIFEST_BYTES = 8 * 1024 * 1024
MAX_RUNS = 100
MAX_CLAIMS = 4_000
MAX_ISSUES = 4_000
MAX_FILE_HASHES = 2_000
MAX_SCREENSHOTS_PER_RUN = 128
MAX_SCREENSHOT_BYTES = 64 * 1024 * 1024
MAX_EMBEDDED_SCREENSHOTS = 8
MAX_EMBEDDED_SCREENSHOT_BYTES = 128 * 1024 * 1024
HASH_CHUNK_BYTES = 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
PNG_IEND = b"\x00\x00\x00\x00IEND\xaeB`\x82"


class EvidenceContractError(ValueError):
    """Raised when a manifest or artifact destination violates the contract."""


@dataclass(frozen=True)
class CanonicalEvidence:
    manifest_path: Path
    manifest_sha256: str
    manifest_size_bytes: int
    generated_at: dt.datetime
    data: dict[str, Any]

    @property
    def runs(self) -> list[dict[str, Any]]:
        return self.data["runs"]


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise EvidenceContractError(message)


def _is_uint(value: object) -> bool:
    return type(value) is int and value >= 0


def _is_finite_number(value: object, *, positive: bool = False) -> bool:
    if type(value) not in (int, float):
        return False
    number = float(value)
    if not math.isfinite(number):
        return False
    return number > 0 if positive else number >= 0


def _bounded_text(value: object, limit: int = 16_384) -> bool:
    return type(value) is str and 0 < len(value) <= limit and all(
        character.isprintable() for character in value
    )


def _parse_generated_at(value: object) -> dt.datetime:
    _require(_bounded_text(value, 64), "generated_at_utc must be bounded text")
    text = str(value)
    _require(text.endswith("Z"), "generated_at_utc must be UTC with a Z suffix")
    try:
        parsed = dt.datetime.fromisoformat(text[:-1] + "+00:00")
    except ValueError as error:
        raise EvidenceContractError("generated_at_utc is not RFC3339-compatible") from error
    _require(parsed.tzinfo is not None, "generated_at_utc must include a timezone")
    return parsed.astimezone(dt.timezone.utc)


def _validate_claim_or_issue(
    item: object, *, kind: str, index: int, require_evidence: bool
) -> str:
    _require(type(item) is dict, f"{kind}[{index}] must be an object")
    assert isinstance(item, dict)
    classification = item.get("classification")
    _require(
        classification in CLASSIFICATIONS,
        f"{kind}[{index}].classification is invalid",
    )
    if require_evidence:
        _require(_bounded_text(item.get("id"), 4_096), f"{kind}[{index}].id is invalid")
        _require(
            _bounded_text(item.get("statement"), 16_384),
            f"{kind}[{index}].statement is invalid",
        )
        evidence = item.get("evidence")
        _require(type(evidence) is list and len(evidence) <= 256, f"{kind}[{index}].evidence is invalid")
        _require(
            all(_bounded_text(path, 4_096) for path in evidence),
            f"{kind}[{index}].evidence contains an invalid path",
        )
    else:
        for field, limit in (("code", 256), ("field", 4_096), ("message", 16_384)):
            _require(
                _bounded_text(item.get(field), limit),
                f"{kind}[{index}].{field} is invalid",
            )
    return str(classification)


def _validate_claim_set(
    claims: object, issues: object, *, scope: str
) -> tuple[dict[str, int], dict[str, int]]:
    _require(type(claims) is list, f"{scope}.claims must be an array")
    _require(type(issues) is list, f"{scope}.issues must be an array")
    assert isinstance(claims, list) and isinstance(issues, list)
    _require(len(claims) <= MAX_CLAIMS, f"{scope}.claims exceeds the fixed cap")
    _require(len(issues) <= MAX_ISSUES, f"{scope}.issues exceeds the fixed cap")
    claim_counts = {classification: 0 for classification in CLASSIFICATIONS}
    issue_counts = {classification: 0 for classification in CLASSIFICATIONS}
    claim_ids: set[str] = set()
    for index, item in enumerate(claims):
        classification = _validate_claim_or_issue(
            item, kind=f"{scope}.claims", index=index, require_evidence=True
        )
        assert isinstance(item, dict)
        claim_id = str(item["id"])
        _require(claim_id not in claim_ids, f"duplicate claim id: {claim_id}")
        claim_ids.add(claim_id)
        claim_counts[classification] += 1
    for index, item in enumerate(issues):
        classification = _validate_claim_or_issue(
            item, kind=f"{scope}.issues", index=index, require_evidence=False
        )
        issue_counts[classification] += 1
    return claim_counts, issue_counts


def _require_claim(run: dict[str, Any], suffix: str, classification: str) -> dict[str, Any]:
    matches = [
        item
        for item in run["claims"]
        if type(item) is dict
        and str(item.get("id", "")).endswith(suffix)
        and item.get("classification") == classification
    ]
    _require(len(matches) == 1, f"{run['input_path']} must contain one {classification} {suffix} claim")
    return matches[0]


def _validate_run(
    run: object,
    *,
    index: int,
    file_hashes: dict[tuple[str, str], dict[str, Any]],
) -> tuple[dict[str, int], dict[str, int]]:
    scope = f"runs[{index}]"
    _require(type(run) is dict, f"{scope} must be an object")
    assert isinstance(run, dict)
    _require(_bounded_text(run.get("input_path"), 4_096), f"{scope}.input_path is invalid")
    _require(run.get("report_schema_variant") == "current", f"{scope} is not current-schema evidence")
    _require(run.get("overall_classification") == "Observed", f"{scope} is not an Observed evidence set")
    claim_counts, issue_counts = _validate_claim_set(
        run.get("claims"), run.get("issues"), scope=scope
    )
    _require(
        claim_counts["Rejected"] == claim_counts["Blocked"] == claim_counts["Planned"] == 0,
        f"{scope} contains non-publishable claims",
    )
    _require(
        issue_counts["Rejected"] == issue_counts["Blocked"] == issue_counts["Planned"] == 0,
        f"{scope} contains non-publishable issues",
    )

    observations = run.get("raw_observations")
    _require(type(observations) is dict, f"{scope}.raw_observations must be an object")
    assert isinstance(observations, dict)
    for field in (
        "run_identity",
        "viewport",
        "route",
        "route_frame_times",
        "planetary_streaming",
        "screenshots",
    ):
        _require(type(observations.get(field)) is dict, f"{scope}.{field} is required")

    identity = observations["run_identity"]
    _require(_bounded_text(identity.get("package_version"), 160), f"{scope} package_version is missing")
    _require(identity.get("build_profile") in {"debug", "release"}, f"{scope} build_profile is invalid")

    viewport = observations["viewport"]
    for field in ("logical_width", "logical_height", "scale_factor", "dpi_percent"):
        _require(_is_finite_number(viewport.get(field), positive=True), f"{scope} viewport {field} is invalid")
    for field in ("physical_width", "physical_height"):
        _require(_is_uint(viewport.get(field)) and viewport[field] > 0, f"{scope} viewport {field} is invalid")

    route = observations["route"]
    _require(_bounded_text(route.get("route_focus"), 160), f"{scope} route_focus is missing")
    for field in (
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
    ):
        _require(_is_finite_number(route.get(field)), f"{scope} route {field} is invalid")

    frame_times = observations["route_frame_times"]
    _require(frame_times.get("measurement_valid") is True, f"{scope} frame-time measurement is invalid")
    _require(frame_times.get("quantiles_complete") is True, f"{scope} frame-time quantiles are incomplete")
    _require(_is_uint(frame_times.get("sample_count")) and frame_times["sample_count"] > 0, f"{scope} has no route samples")
    for field in ("mean_ms", "median_ms", "p95_ms", "p99_ms", "max_ms"):
        _require(_is_finite_number(frame_times.get(field)), f"{scope} frame-time {field} is invalid")

    planetary = observations["planetary_streaming"]
    for group in ("live", "budgets", "telemetry"):
        _require(type(planetary.get(group)) is dict, f"{scope} planetary {group} is missing")
    live = planetary["live"]
    budgets = planetary["budgets"]
    telemetry = planetary["telemetry"]
    _require(live.get("enabled") is True, f"{scope} planetary streaming is disabled")
    _require(_bounded_text(live.get("profile"), 160), f"{scope} planetary profile is missing")
    for field in (
        "resident_entities",
        "resident_vertices",
        "resident_indices",
        "resident_mesh_bytes",
        "live_sample_cache_windows",
        "live_sample_cache_bytes",
    ):
        _require(_is_uint(live.get(field)), f"{scope} planetary live {field} is invalid")
    for field in (
        "budget_entities",
        "budget_vertices",
        "budget_indices",
        "budget_mesh_bytes",
        "budget_sample_cache_bytes",
    ):
        _require(_is_uint(budgets.get(field)), f"{scope} planetary budget {field} is invalid")
    for field in ("ring_vertices", "ring_indices"):
        values = live.get(field)
        _require(
            type(values) is list and len(values) == 6 and all(_is_uint(value) for value in values),
            f"{scope} planetary {field} is not a six-level population",
        )
    _require(
        telemetry.get("surface_material_mode") in {"LegacyPalette", "BridgeV1", "BridgeV2"},
        f"{scope} surface material mode is invalid",
    )
    for field in (
        "desired_material_detail",
        "resident_material_detail",
    ):
        values = telemetry.get(field)
        _require(type(values) is list and len(values) == 6, f"{scope} {field} is not a six-level state")
    for field in (
        "last_build_ms",
        "max_build_ms",
    ):
        _require(_is_finite_number(telemetry.get(field)), f"{scope} planetary {field} is invalid")
    for field in (
        "last_material_slope_queries",
        "last_bridge_v2_cell_reuses",
        "peak_live_sample_cache_windows",
        "peak_live_sample_cache_bytes",
    ):
        _require(_is_uint(telemetry.get(field)), f"{scope} planetary {field} is invalid")

    screenshots = observations["screenshots"]
    referenced = screenshots.get("referenced_files")
    actual = screenshots.get("actual_files")
    _require(
        type(referenced) is list and 0 < len(referenced) <= MAX_SCREENSHOTS_PER_RUN,
        f"{scope} must contain bounded referenced screenshots",
    )
    _require(type(actual) is list and len(actual) <= MAX_SCREENSHOTS_PER_RUN, f"{scope} actual screenshots are invalid")
    actual_by_path: dict[str, dict[str, Any]] = {}
    for shot_index, record in enumerate(actual):
        _require(type(record) is dict, f"{scope} actual screenshot {shot_index} is invalid")
        assert isinstance(record, dict)
        path = record.get("path")
        _require(_bounded_text(path, 4_096), f"{scope} screenshot path is invalid")
        _require(path not in actual_by_path, f"{scope} has a duplicate screenshot path")
        _require(record.get("classification") == "Passed", f"{scope} screenshot is not Passed")
        _require(record.get("png_complete") is True, f"{scope} screenshot is not complete")
        _require(SHA256_RE.fullmatch(str(record.get("sha256", ""))) is not None, f"{scope} screenshot hash is invalid")
        _require(_is_uint(record.get("size_bytes")), f"{scope} screenshot size is invalid")
        actual_by_path[str(path)] = record
    for path in referenced:
        _require(_bounded_text(path, 4_096), f"{scope} referenced screenshot path is invalid")
        _require(path in actual_by_path, f"{scope} referenced screenshot is absent from actual_files")
        record = actual_by_path[path]
        hash_record = file_hashes.get(("screenshot", str(path)))
        _require(hash_record is not None, f"{scope} screenshot is absent from file_hashes")
        _require(
            hash_record["sha256"] == record["sha256"]
            and hash_record["size_bytes"] == record["size_bytes"],
            f"{scope} screenshot hash records disagree",
        )

    report_claim = _require_claim(run, ":report_integrity", "Passed")
    _require(len(report_claim["evidence"]) == 1, f"{scope} report claim must cite one report")
    report_path = report_claim["evidence"][0]
    _require(("report", report_path) in file_hashes, f"{scope} report hash is missing")
    screenshot_claim = _require_claim(run, ":screenshot_integrity", "Passed")
    _require(sorted(screenshot_claim["evidence"]) == sorted(referenced), f"{scope} screenshot claim disagrees")
    _require_claim(run, ":planetary_budgets", "Passed")
    return claim_counts, issue_counts


def load_canonical_evidence(path: Path | str) -> CanonicalEvidence:
    manifest_path = Path(path).resolve(strict=False)
    _require(manifest_path.suffix.lower() == ".json", "evidence manifest must have a .json suffix")
    try:
        size = manifest_path.stat().st_size
    except OSError as error:
        raise EvidenceContractError(f"evidence manifest is not readable: {error}") from error
    _require(0 < size <= MAX_MANIFEST_BYTES, "evidence manifest violates the fixed byte cap")
    try:
        payload = manifest_path.read_bytes()
        data = json.loads(payload.decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError, RecursionError) as error:
        raise EvidenceContractError(f"evidence manifest is not strict UTF-8 JSON: {error}") from error
    _require(type(data) is dict, "evidence manifest root must be an object")
    assert isinstance(data, dict)
    _require(data.get("schema_version") == SCHEMA_VERSION, "unsupported evidence manifest schema_version")
    _require(data.get("claim_classifications") == list(CLASSIFICATIONS), "claim classification contract changed")
    _require(data.get("overall_classification") == "Observed", "manifest is not an Observed evidence set")
    generated_at = _parse_generated_at(data.get("generated_at_utc"))

    generator = data.get("generator")
    _require(type(generator) is dict, "generator metadata is missing")
    assert isinstance(generator, dict)
    _require(generator.get("name") == GENERATOR_NAME, "unexpected manifest generator")
    _require(generator.get("version") == GENERATOR_VERSION, "unsupported manifest generator version")
    _require(_bounded_text(generator.get("source_path"), 4_096), "generator source path is invalid")
    _require(SHA256_RE.fullmatch(str(generator.get("source_sha256", ""))) is not None, "generator source hash is invalid")

    inputs = data.get("inputs")
    _require(type(inputs) is dict, "manifest inputs are missing")
    assert isinstance(inputs, dict)
    _require(inputs.get("selection_policy") == SELECTION_POLICY, "manifest was not built from explicit runs")
    _require(_is_uint(inputs.get("argument_count")), "manifest argument_count is invalid")
    _require(_is_uint(inputs.get("accepted_run_count")), "manifest accepted_run_count is invalid")
    directories = inputs.get("qa_run_directories")
    _require(type(directories) is list and len(directories) <= MAX_RUNS, "manifest run directory list is invalid")
    _require(all(_bounded_text(item, 4_096) for item in directories), "manifest contains an invalid run path")

    hashes = data.get("file_hashes")
    _require(type(hashes) is list and len(hashes) <= MAX_FILE_HASHES, "manifest file_hashes exceed the fixed cap")
    assert isinstance(hashes, list)
    file_hashes: dict[tuple[str, str], dict[str, Any]] = {}
    for index, record in enumerate(hashes):
        _require(type(record) is dict, f"file_hashes[{index}] must be an object")
        assert isinstance(record, dict)
        kind = record.get("kind")
        record_path = record.get("path")
        _require(kind in {"report", "screenshot", "generator_source"}, f"file_hashes[{index}].kind is invalid")
        _require(_bounded_text(record_path, 4_096), f"file_hashes[{index}].path is invalid")
        _require(SHA256_RE.fullmatch(str(record.get("sha256", ""))) is not None, f"file_hashes[{index}].sha256 is invalid")
        _require(_is_uint(record.get("size_bytes")), f"file_hashes[{index}].size_bytes is invalid")
        key = (str(kind), str(record_path))
        _require(key not in file_hashes, f"duplicate file hash record: {key}")
        file_hashes[key] = record
    source_key = ("generator_source", str(generator["source_path"]))
    _require(source_key in file_hashes, "generator source hash record is missing")
    _require(file_hashes[source_key]["sha256"] == generator["source_sha256"], "generator source hashes disagree")

    top_claim_counts, top_issue_counts = _validate_claim_set(
        data.get("claims"), data.get("issues"), scope="manifest"
    )
    _require(
        top_claim_counts["Rejected"] == top_claim_counts["Blocked"] == top_claim_counts["Planned"] == 0,
        "manifest contains non-publishable claims",
    )
    _require(
        top_issue_counts["Rejected"] == top_issue_counts["Blocked"] == top_issue_counts["Planned"] == 0,
        "manifest contains non-publishable issues",
    )

    runs = data.get("runs")
    _require(type(runs) is list and 0 < len(runs) <= MAX_RUNS, "manifest must contain bounded current runs")
    assert isinstance(runs, list)
    _require(len(directories) == len(runs), "manifest run directory and run counts disagree")
    _require(inputs["accepted_run_count"] == len(runs), "accepted_run_count disagrees with runs")
    _require(inputs["argument_count"] >= len(runs), "argument_count is below accepted_run_count")
    observed_paths: list[str] = []
    claim_counts = top_claim_counts.copy()
    issue_counts = top_issue_counts.copy()
    for index, run in enumerate(runs):
        run_claims, run_issues = _validate_run(run, index=index, file_hashes=file_hashes)
        assert isinstance(run, dict)
        observed_paths.append(str(run["input_path"]))
        for classification in CLASSIFICATIONS:
            claim_counts[classification] += run_claims[classification]
            issue_counts[classification] += run_issues[classification]
    _require(observed_paths == directories, "run order or paths disagree with manifest inputs")
    _require(len(set(observed_paths)) == len(observed_paths), "manifest contains duplicate runs")

    summary = data.get("summary")
    _require(type(summary) is dict, "manifest summary is missing")
    assert isinstance(summary, dict)
    _require(summary.get("run_count") == len(runs), "summary.run_count disagrees with runs")
    _require(summary.get("file_hash_count") == len(hashes), "summary.file_hash_count disagrees")
    _require(summary.get("claim_counts") == claim_counts, "summary.claim_counts disagrees")
    _require(summary.get("issue_counts") == issue_counts, "summary.issue_counts disagrees")

    return CanonicalEvidence(
        manifest_path=manifest_path,
        manifest_sha256=hashlib.sha256(payload).hexdigest(),
        manifest_size_bytes=len(payload),
        generated_at=generated_at,
        data=data,
    )


def validate_output_path(output: Path | str, repo_root: Path | str, suffix: str) -> Path:
    root = Path(repo_root).resolve(strict=False)
    destination = Path(output).resolve(strict=False)
    _require(destination.suffix.lower() == suffix.lower(), f"output must have a {suffix} suffix")
    _require(not destination.exists(), "output already exists; choose a new explicit path")
    for directory in PROTECTED_OUTPUT_DIRS:
        protected = (root / directory).resolve(strict=False)
        try:
            destination.relative_to(protected)
        except ValueError:
            continue
        raise EvidenceContractError(f"output must not be inside protected directory {directory!r}")
    return destination


def publish_no_clobber(temporary: Path, destination: Path) -> None:
    """Publish a sibling temporary file without ever replacing an existing file."""

    try:
        os.link(temporary, destination)
    except FileExistsError as error:
        raise EvidenceContractError("output appeared during publication; nothing was replaced") from error
    finally:
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass


def _resolve_evidence_path(display_path: str, repo_root: Path) -> Path:
    candidate = Path(display_path.replace("/", os.sep))
    _require(".." not in candidate.parts, "evidence path contains parent traversal")
    return (candidate if candidate.is_absolute() else repo_root / candidate).resolve(strict=False)


def _hash_and_probe_png(path: Path, expected_size: int) -> str:
    _require(expected_size <= MAX_SCREENSHOT_BYTES, "screenshot exceeds the artifact byte cap")
    digest = hashlib.sha256()
    first = b""
    tail = b""
    total = 0
    try:
        with path.open("rb") as source:
            while True:
                block = source.read(HASH_CHUNK_BYTES)
                if not block:
                    break
                total += len(block)
                _require(total <= MAX_SCREENSHOT_BYTES, "screenshot exceeds the artifact byte cap")
                if not first:
                    first = block[: len(PNG_SIGNATURE)]
                tail = (tail + block)[-len(PNG_IEND) :]
                digest.update(block)
    except OSError as error:
        raise EvidenceContractError(f"screenshot is not readable: {path}: {error}") from error
    _require(total == expected_size, f"screenshot size changed after manifest generation: {path}")
    _require(first == PNG_SIGNATURE and tail == PNG_IEND, f"screenshot is no longer a complete PNG: {path}")
    return digest.hexdigest()


def verified_screenshots(
    evidence: CanonicalEvidence,
    repo_root: Path | str,
    *,
    limit: int = MAX_EMBEDDED_SCREENSHOTS,
) -> list[tuple[dict[str, Any], str, Path, dict[str, Any]]]:
    """Return and re-hash a bounded, deterministic screenshot selection.

    One referenced screenshot per run is selected first; remaining slots are
    filled in manifest order.  This avoids a "latest" or filename heuristic.
    """

    _require(0 < limit <= MAX_EMBEDDED_SCREENSHOTS, "screenshot selection limit is invalid")
    root = Path(repo_root).resolve(strict=False)
    candidates: list[tuple[dict[str, Any], str, dict[str, Any]]] = []
    extras: list[tuple[dict[str, Any], str, dict[str, Any]]] = []
    for run in evidence.runs:
        screenshots = run["raw_observations"]["screenshots"]
        records = {record["path"]: record for record in screenshots["actual_files"]}
        referenced = screenshots["referenced_files"]
        candidates.append((run, referenced[0], records[referenced[0]]))
        extras.extend((run, path, records[path]) for path in referenced[1:])
    selected = (candidates + extras)[:limit]
    output: list[tuple[dict[str, Any], str, Path, dict[str, Any]]] = []
    total_bytes = 0
    for run, display, record in selected:
        total_bytes += int(record["size_bytes"])
        _require(total_bytes <= MAX_EMBEDDED_SCREENSHOT_BYTES, "selected screenshots exceed the total byte cap")
        resolved = _resolve_evidence_path(display, root)
        run_root = _resolve_evidence_path(str(run["input_path"]), root)
        try:
            resolved.relative_to(run_root)
        except ValueError as error:
            raise EvidenceContractError(
                f"screenshot no longer resolves inside its explicit run: {display}"
            ) from error
        digest = _hash_and_probe_png(resolved, int(record["size_bytes"]))
        _require(digest == record["sha256"], f"screenshot hash changed after manifest generation: {display}")
        output.append((run, display, resolved, record))
    return output


def iter_claims(evidence: CanonicalEvidence) -> Iterable[tuple[str, dict[str, Any]]]:
    for item in evidence.data["claims"]:
        yield "manifest", item
    for run in evidence.runs:
        for item in run["claims"]:
            yield run["input_path"], item


def iter_issues(evidence: CanonicalEvidence) -> Iterable[tuple[str, dict[str, Any]]]:
    for item in evidence.data["issues"]:
        yield "manifest", item
    for run in evidence.runs:
        for item in run["issues"]:
            yield run["input_path"], item


def validation_summary(evidence: CanonicalEvidence, output: Path) -> dict[str, Any]:
    return {
        "evidence_manifest": str(evidence.manifest_path),
        "manifest_sha256": evidence.manifest_sha256,
        "output": str(output),
        "overall_classification": evidence.data["overall_classification"],
        "run_count": len(evidence.runs),
        "schema_version": evidence.data["schema_version"],
    }
