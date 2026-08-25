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


SCHEMA_VERSION = "1.5.0"
GENERATOR_NAME = "voxel-native-evidence-manifest"
GENERATOR_VERSION = "1.5.0"
SELECTION_POLICY = "explicit_repo_contained_directories_only_no_latest_no_global_scan"
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


def _repository_relative_path(value: object) -> bool:
    if not _bounded_text(value, 4_096):
        return False
    text = str(value)
    if "\\" in text or text.startswith("/") or re.match(r"^[A-Za-z]:", text):
        return False
    return all(part not in {"", ".", ".."} for part in text.split("/"))


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
        "world_edit_store",
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
    _require(
        identity.get("terrain_grammar") in {"V1", "V2", "V3"},
        f"{scope} terrain grammar is invalid",
    )

    edit_store = observations["world_edit_store"]
    _require(
        edit_store.get("world_edit_store_status") == "compatible"
        and edit_store.get("world_edit_store_compatible") is True,
        f"{scope} world edit store is not compatible",
    )
    _require(
        _is_uint(edit_store.get("world_edit_store_edited_chunks")),
        f"{scope} world edit-store chunk count is invalid",
    )
    _require(
        edit_store.get("world_edit_store_block_reason_code") is None,
        f"{scope} compatible world edit store has a block reason",
    )
    for field, identity_field in (
        ("world_edit_store_seed", "world_seed"),
        ("world_edit_store_profile", "world_profile"),
        ("world_edit_store_scenery_quality", "scenery_quality"),
        ("world_edit_store_terrain_grammar", "terrain_grammar"),
    ):
        _require(
            edit_store.get(field) == identity.get(identity_field),
            f"{scope} world edit-store identity contradicts run identity",
        )

    viewport = observations["viewport"]
    for field in ("logical_width", "logical_height", "scale_factor", "dpi_percent"):
        _require(_is_finite_number(viewport.get(field), positive=True), f"{scope} viewport {field} is invalid")
    for field in ("physical_width", "physical_height"):
        _require(_is_uint(viewport.get(field)) and viewport[field] > 0, f"{scope} viewport {field} is invalid")

    route = observations["route"]
    supported_focuses = {"scenic", "waypoint", "streaming", "river", "lava", "near-far"}
    requested_focus = route.get("requested_route_focus")
    resolved_focus = route.get("resolved_route_focus")
    route_available = route.get("route_focus_available")
    unavailable_reason = route.get("route_focus_unavailable_reason")
    _require(requested_focus in supported_focuses, f"{scope} requested route focus is invalid")
    _require(resolved_focus in supported_focuses, f"{scope} resolved route focus is invalid")
    _require(type(route_available) is bool, f"{scope} route availability is invalid")
    _require(
        route_available is True
        and requested_focus == resolved_focus
        and unavailable_reason is None
        and route.get("route_focus_search_cap_exhausted") is False,
        f"{scope} does not contain an available requested route",
    )
    anchor = route.get("route_focus_anchor")
    _require(
        anchor is None
        or (
            type(anchor) is list
            and len(anchor) == 3
            and all(type(value) is int for value in anchor)
        ),
        f"{scope} route anchor is invalid",
    )
    anchored_focuses = {"waypoint", "river", "lava", "near-far"}
    compatible_world_profiles = {
        "waypoint": {"AstralFrontier"},
        "river": {"Natural"},
        "lava": {"AstralFrontier"},
        "near-far": {"Natural", "AstralFrontier"},
    }
    if requested_focus in anchored_focuses:
        _require(anchor is not None, f"{scope} available route focus is missing its anchor")
    expected_profiles = compatible_world_profiles.get(requested_focus)
    if expected_profiles is not None:
        _require(
            identity.get("world_profile") in expected_profiles,
            f"{scope} route focus is incompatible with its world profile",
        )
    for field in ("route_focus_search_visited_candidates", "route_focus_classification_queries"):
        _require(route.get(field) is None or _is_uint(route[field]), f"{scope} route {field} is invalid")
    for field in ("route_focus_search_candidate_cap", "route_focus_classification_query_cap"):
        _require(_is_uint(route.get(field)), f"{scope} route {field} is invalid")
    if route.get("route_focus_search_visited_candidates") is not None:
        _require(
            route["route_focus_search_visited_candidates"] <= route["route_focus_search_candidate_cap"],
            f"{scope} route candidate work exceeds its cap",
        )
    if route.get("route_focus_classification_queries") is not None:
        _require(
            route["route_focus_classification_queries"] <= route["route_focus_classification_query_cap"],
            f"{scope} route classification work exceeds its cap",
        )
    _require(route.get("camera_route_policy") == "preflight-v1", f"{scope} camera policy is not preflight-v1")
    applicable = route.get("camera_route_preflight_applicable")
    _require(type(applicable) is bool, f"{scope} camera applicability is invalid")
    _require(applicable is (requested_focus in {"river", "lava", "near-far"}), f"{scope} camera applicability contradicts focus")
    counter_fields = (
        "camera_route_variant_count", "camera_route_validation_samples",
        "camera_route_voxel_queries", "camera_route_voxel_query_cap",
        "camera_route_required_chunk_checks", "camera_route_loaded_chunk_checks",
        "camera_route_proven_air_chunk_checks", "camera_route_unloaded_chunk_checks",
        "camera_route_candidate_body_occlusions",
        "camera_route_candidate_los_occlusions", "camera_route_selected_clear_samples",
    )
    if applicable:
        _require(route.get("camera_route_available") is True, f"{scope} camera route is unavailable")
        _require(route.get("camera_route_unavailable_reason") is None, f"{scope} camera route has an unavailable reason")
        _require(type(route.get("camera_route_plan_hash")) is str and re.fullmatch(r"[0-9a-f]{16}", route["camera_route_plan_hash"]) is not None, f"{scope} camera plan hash is invalid")
        _require(route.get("camera_route_variant_count") == 8 and route.get("camera_route_validation_samples") == 16, f"{scope} camera validation contract is invalid")
        _require(route.get("camera_route_voxel_query_cap") == 153_600, f"{scope} camera query cap is invalid")
        variant = route.get("camera_route_variant_index")
        _require(_is_uint(variant) and variant < 8, f"{scope} selected camera variant is invalid")
        queries = route.get("camera_route_voxel_queries")
        required = route.get("camera_route_required_chunk_checks")
        loaded = route.get("camera_route_loaded_chunk_checks")
        proven_air = route.get("camera_route_proven_air_chunk_checks")
        unloaded = route.get("camera_route_unloaded_chunk_checks")
        _require(_is_uint(queries) and 0 < queries < 153_600, f"{scope} camera query work is invalid")
        _require(
            all(_is_uint(value) for value in (required, loaded, proven_air, unloaded)),
            f"{scope} camera chunk-check counters are invalid",
        )
        _require(
            required == queries == loaded + proven_air + unloaded,
            f"{scope} camera chunk-check accounting is inconsistent",
        )
        _require(unloaded == 0, f"{scope} camera route has unloaded chunk checks")
        for field in ("camera_route_candidate_body_occlusions", "camera_route_candidate_los_occlusions"):
            _require(_is_uint(route.get(field)) and route[field] <= 128, f"{scope} camera candidate diagnostic is invalid")
        _require(route.get("camera_route_selected_clear_samples") == 16, f"{scope} selected camera plan is not fully clear")
        _require(_is_uint(route.get("camera_route_minimum_clearance_voxels")) and route["camera_route_minimum_clearance_voxels"] > 0, f"{scope} camera route lacks positive clearance")
    else:
        _require(route.get("camera_route_available") is False and route.get("camera_route_unavailable_reason") is None, f"{scope} non-applicable camera state is invalid")
        _require(route.get("camera_route_plan_hash") is None and route.get("camera_route_variant_index") is None, f"{scope} non-applicable camera state has a plan")
        _require(all(route.get(field) == 0 for field in counter_fields), f"{scope} non-applicable camera counters are not zero")
        _require(route.get("camera_route_minimum_clearance_voxels") is None, f"{scope} non-applicable camera state has clearance")
    _require(route.get("camera_route_work_cap_exhausted") is False, f"{scope} camera route exhausted its work cap")
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
    _require(live.get("profile") in {"Natural", "AstralFrontier"}, f"{scope} planetary profile is invalid")
    _require(live.get("profile") == identity.get("world_profile"), f"{scope} planetary profile contradicts run identity")
    _require(
        telemetry.get("desired_terrain_grammar") == identity.get("terrain_grammar")
        and telemetry.get("active_terrain_grammar") == identity.get("terrain_grammar"),
        f"{scope} far-field grammar contradicts run identity",
    )
    for field in (
        "resident_entities", "resident_vertices", "resident_indices", "resident_mesh_bytes",
        "resident_fluid_entities", "resident_fluid_vertices", "resident_fluid_indices",
        "resident_fluid_mesh_bytes", "resident_water_indices", "resident_lava_indices",
        "resident_semantic_cohort_entities", "resident_semantic_cohort_vertices",
        "resident_semantic_cohort_indices", "resident_semantic_cohort_mesh_bytes",
        "resident_semantic_cohort_count", "live_sample_cache_windows", "live_sample_cache_bytes",
    ):
        _require(_is_uint(live.get(field)), f"{scope} planetary live {field} is invalid")
    for field in (
        "budget_entities", "budget_vertices", "budget_indices", "budget_mesh_bytes",
        "budget_sample_cache_bytes", "budget_fluid_entities", "budget_fluid_vertices",
        "budget_fluid_indices", "budget_fluid_mesh_bytes", "budget_hydro_atomic_ring_build_bytes",
        "budget_atomic_ring_build_bytes", "budget_semantic_cohort_entities",
        "budget_semantic_cohort_vertices", "budget_semantic_cohort_indices",
        "budget_semantic_cohort_mesh_bytes", "budget_semantic_cohort_hash_scans",
        "budget_semantic_cohort_height_queries", "budget_semantic_cohort_biome_queries",
    ):
        _require(_is_uint(budgets.get(field)), f"{scope} planetary budget {field} is invalid")
    for field in (
        "ring_vertices", "ring_indices", "fluid_ring_vertices", "fluid_ring_indices",
        "water_ring_indices", "lava_ring_indices",
    ):
        values = live.get(field)
        _require(
            type(values) is list and len(values) == 6 and all(_is_uint(value) for value in values),
            f"{scope} planetary {field} is not a six-level population",
        )
    _require(
        telemetry.get("surface_material_mode") in {"LegacyPalette", "BridgeV1", "BridgeV2"},
        f"{scope} surface material mode is invalid",
    )
    _require(telemetry.get("hydro_mode") in {"Disabled", "DescriptiveV1"}, f"{scope} hydro mode is invalid")
    _require(telemetry.get("semantic_cohort_mode") in {"Disabled", "SilhouettesV1"}, f"{scope} semantic cohort mode is invalid")
    for field in (
        "resident_fluid_observation_valid", "resident_fluid_kind_integrity_valid",
        "resident_semantic_cohort_observation_valid",
        "resident_semantic_cohort_payload_integrity_valid",
    ):
        _require(telemetry.get(field) is True, f"{scope} planetary {field} is invalid")
    for field in (
        "resident_fluid_entity_count_overflow", "resident_fluid_scheduler_mismatch",
        "resident_fluid_budget_exceeded", "resident_semantic_cohort_entity_count_overflow",
        "resident_semantic_cohort_scheduler_mismatch", "resident_semantic_cohort_budget_exceeded",
    ):
        _require(telemetry.get(field) is False, f"{scope} planetary {field} is invalid")
    for field in (
        "last_fluid_indices", "last_water_indices", "last_lava_indices",
        "scheduler_resident_water_indices", "scheduler_resident_lava_indices",
        "scheduler_resident_semantic_cohort_entities",
        "scheduler_resident_semantic_cohort_vertices",
        "scheduler_resident_semantic_cohort_indices",
        "scheduler_resident_semantic_cohort_mesh_bytes",
        "scheduler_resident_semantic_cohort_count",
        "last_semantic_cohort_candidates", "last_semantic_cohort_emitted",
        "last_semantic_cohort_vertices", "last_semantic_cohort_indices",
    ):
        _require(_is_uint(telemetry.get(field)), f"{scope} planetary {field} is invalid")
    for field in (
        "resident_semantic_cohort_kind_counts",
    ):
        _require(
            type(live.get(field)) is list and len(live[field]) == 6 and all(_is_uint(value) for value in live[field]),
            f"{scope} planetary {field} is not a six-kind population",
        )
    for field in (
        "scheduler_water_ring_indices", "scheduler_lava_ring_indices",
        "scheduler_resident_semantic_cohort_kind_counts", "last_semantic_cohort_kind_counts",
    ):
        _require(
            type(telemetry.get(field)) is list and len(telemetry[field]) == 6 and all(_is_uint(value) for value in telemetry[field]),
            f"{scope} planetary {field} is not a six-entry population",
        )
    _require(live["resident_water_indices"] + live["resident_lava_indices"] == live["resident_fluid_indices"], f"{scope} Hydro kind totals disagree")
    _require(live["resident_water_indices"] % 6 == live["resident_lava_indices"] % 6 == 0, f"{scope} Hydro kinds are not complete quads")
    _require(telemetry["last_water_indices"] + telemetry["last_lava_indices"] == telemetry["last_fluid_indices"], f"{scope} latest Hydro kind totals disagree")
    _require(telemetry["last_water_indices"] % 6 == telemetry["last_lava_indices"] % 6 == 0, f"{scope} latest Hydro kinds are not complete quads")
    for index in range(6):
        _require(live["water_ring_indices"][index] + live["lava_ring_indices"][index] == live["fluid_ring_indices"][index], f"{scope} Hydro ring kinds disagree")
        _require(live["water_ring_indices"][index] % 6 == live["lava_ring_indices"][index] % 6 == 0, f"{scope} Hydro ring kinds are not complete quads")
    cohort_count = live["resident_semantic_cohort_count"]
    _require(sum(live["resident_semantic_cohort_kind_counts"]) == cohort_count, f"{scope} cohort kinds disagree")
    _require(live["resident_semantic_cohort_vertices"] == cohort_count * 24, f"{scope} cohort vertices disagree")
    _require(live["resident_semantic_cohort_indices"] == cohort_count * 36, f"{scope} cohort indices disagree")
    _require(live["resident_semantic_cohort_mesh_bytes"] == cohort_count * (24 * 48 + 36 * 4), f"{scope} cohort bytes disagree")
    _require(live["resident_semantic_cohort_entities"] == int(cohort_count > 0), f"{scope} cohort entity population disagrees")
    _require(cohort_count <= 81, f"{scope} cohort count exceeds the fixed candidate cap")
    if live["profile"] == "Natural":
        _require(not any(live["resident_semantic_cohort_kind_counts"][3:]), f"{scope} Natural profile contains Astral cohort kinds")
        _require(not any(telemetry["last_semantic_cohort_kind_counts"][3:]), f"{scope} Natural latest work contains Astral cohort kinds")
    else:
        _require(not any(live["resident_semantic_cohort_kind_counts"][:3]), f"{scope} Astral profile contains Natural cohort kinds")
        _require(not any(telemetry["last_semantic_cohort_kind_counts"][:3]), f"{scope} Astral latest work contains Natural cohort kinds")
    _require(budgets["budget_hydro_atomic_ring_build_bytes"] == 653_008, f"{scope} Hydro atomic byte budget changed")
    _require(budgets["budget_atomic_ring_build_bytes"] == 757_984, f"{scope} combined atomic byte budget changed")
    expected_cohort_budgets = {
        "budget_semantic_cohort_entities": 1,
        "budget_semantic_cohort_vertices": 1_944,
        "budget_semantic_cohort_indices": 2_916,
        "budget_semantic_cohort_mesh_bytes": 104_976,
        "budget_semantic_cohort_hash_scans": 3_721,
        "budget_semantic_cohort_height_queries": 81,
        "budget_semantic_cohort_biome_queries": 81,
    }
    for field, expected in expected_cohort_budgets.items():
        _require(budgets[field] == expected, f"{scope} planetary {field} changed")
    scheduler_pairs = (
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
    for live_field, telemetry_field in scheduler_pairs:
        _require(live[live_field] == telemetry[telemetry_field], f"{scope} scheduler {telemetry_field} disagrees")
    for live_field, budget_field in (
        ("resident_semantic_cohort_entities", "budget_semantic_cohort_entities"),
        ("resident_semantic_cohort_vertices", "budget_semantic_cohort_vertices"),
        ("resident_semantic_cohort_indices", "budget_semantic_cohort_indices"),
        ("resident_semantic_cohort_mesh_bytes", "budget_semantic_cohort_mesh_bytes"),
    ):
        _require(live[live_field] <= budgets[budget_field], f"{scope} {live_field} exceeds budget")
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
    _require(_repository_relative_path(generator.get("source_path")), "generator source path is invalid")
    _require(SHA256_RE.fullmatch(str(generator.get("source_sha256", ""))) is not None, "generator source hash is invalid")

    inputs = data.get("inputs")
    _require(type(inputs) is dict, "manifest inputs are missing")
    assert isinstance(inputs, dict)
    _require(inputs.get("selection_policy") == SELECTION_POLICY, "manifest was not built from explicit runs")
    _require(_is_uint(inputs.get("argument_count")), "manifest argument_count is invalid")
    _require(_is_uint(inputs.get("accepted_run_count")), "manifest accepted_run_count is invalid")
    directories = inputs.get("qa_run_directories")
    _require(type(directories) is list and len(directories) <= MAX_RUNS, "manifest run directory list is invalid")
    _require(all(_repository_relative_path(item) for item in directories), "manifest contains an invalid run path")

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
        _require(_repository_relative_path(record_path), f"file_hashes[{index}].path is invalid")
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
