#!/usr/bin/env python3
"""Fixture tests for build_evidence_manifest.py."""

from __future__ import annotations

import base64
import contextlib
import hashlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import build_evidence_manifest as evidence


PNG_1X1 = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUB"
    "AScY42YAAAAASUVORK5CYII="
)
FIXED_TIME = "2026-08-12T00:00:00Z"


def modern_report(screenshot: str = "shot_0000.png") -> str:
    return f'''(
    qa_report_schema_version: "2.6.0",
    run_identity: (
        package_version: "0.1.0",
        build_profile: "debug",
        instance_label: Some("fixture"),
        world_name: Some("fixture_world"),
        world_seed: Some(12345),
        world_profile: Some("AstralFrontier"),
        scenery_quality: Some("Lush"),
        terrain_grammar: Some("V3"),
        git_sha: Some("abcdef1234567"),
        git_dirty: Some(false),
        source_fingerprint: Some("sha256:source"),
        executable_hash: Some("sha256:executable"),
        toolchain: Some("rustc fixture"),
        hardware: Some("fixture hardware"),
    ),
    world_edit_store_status: "compatible",
    world_edit_store_compatible: true,
    world_edit_store_seed: Some(12345),
    world_edit_store_profile: Some("AstralFrontier"),
    world_edit_store_scenery_quality: Some("Lush"),
    world_edit_store_terrain_grammar: Some("V3"),
    world_edit_store_edited_chunks: Some(0),
    world_edit_store_block_reason_code: None,
    viewport: Some((
        logical_width: 1280.0,
        logical_height: 720.0,
        physical_width: 1280,
        physical_height: 720,
        scale_factor: 1.0,
        base_scale_factor: 1.0,
        dpi_percent: 100.0,
    )),
    planetary_streaming: Some((
        enabled: true,
        profile: "AstralFrontier",
        desired_terrain_grammar: Some("V3"),
        active_terrain_grammar: Some("V3"),
        interaction_radius_metres: 256,
        confirmed_near_extent_metres: 64,
        near_coverage_ready_columns: 32,
        near_coverage_hidden_cells: 0,
        far_radius_metres: 30720,
        resident_entities: 6,
        resident_vertices: 28000,
        resident_indices: 117000,
        ring_vertices: [5000, 5000, 5000, 5000, 4000, 4000],
        ring_indices: [20000, 20000, 20000, 20000, 20000, 17000],
        resident_mesh_bytes: 1800000,
        resident_fluid_entities: 6,
        resident_fluid_vertices: 2100,
        resident_fluid_indices: 6300,
        resident_water_indices: 4200,
        resident_lava_indices: 2100,
        fluid_ring_vertices: [100, 200, 300, 400, 500, 600],
        fluid_ring_indices: [300, 600, 900, 1200, 1500, 1800],
        water_ring_indices: [300, 600, 600, 900, 900, 900],
        lava_ring_indices: [0, 0, 300, 300, 600, 900],
        resident_fluid_mesh_bytes: 100800,
        resident_semantic_cohort_entities: 1,
        resident_semantic_cohort_vertices: 48,
        resident_semantic_cohort_indices: 72,
        resident_semantic_cohort_mesh_bytes: 2592,
        resident_semantic_cohort_count: 2,
        resident_semantic_cohort_kind_counts: [0, 0, 0, 1, 1, 0],
        scheduler_resident_entities: 6,
        scheduler_resident_vertices: 28000,
        scheduler_resident_indices: 117000,
        scheduler_ring_vertices: [5000, 5000, 5000, 5000, 4000, 4000],
        scheduler_ring_indices: [20000, 20000, 20000, 20000, 20000, 17000],
        scheduler_resident_mesh_bytes: 1800000,
        scheduler_resident_fluid_entities: 6,
        scheduler_resident_fluid_vertices: 2100,
        scheduler_resident_fluid_indices: 6300,
        scheduler_resident_water_indices: 4200,
        scheduler_resident_lava_indices: 2100,
        scheduler_fluid_ring_vertices: [100, 200, 300, 400, 500, 600],
        scheduler_fluid_ring_indices: [300, 600, 900, 1200, 1500, 1800],
        scheduler_water_ring_indices: [300, 600, 600, 900, 900, 900],
        scheduler_lava_ring_indices: [0, 0, 300, 300, 600, 900],
        scheduler_resident_fluid_mesh_bytes: 100800,
        scheduler_resident_semantic_cohort_entities: 1,
        scheduler_resident_semantic_cohort_vertices: 48,
        scheduler_resident_semantic_cohort_indices: 72,
        scheduler_resident_semantic_cohort_mesh_bytes: 2592,
        scheduler_resident_semantic_cohort_count: 2,
        scheduler_resident_semantic_cohort_kind_counts: [0, 0, 0, 1, 1, 0],
        resident_observation_valid: true,
        resident_entity_count_overflow: false,
        resident_duplicate_levels: 0,
        resident_out_of_range_levels: 0,
        resident_scheduler_mismatch: false,
        resident_budget_exceeded: false,
        resident_observation_rejections: 0,
        resident_fluid_observation_valid: true,
        resident_fluid_entity_count_overflow: false,
        resident_fluid_duplicate_slots: 0,
        resident_fluid_out_of_range_levels: 0,
        resident_fluid_scheduler_mismatch: false,
        resident_fluid_budget_exceeded: false,
        resident_fluid_kind_integrity_valid: true,
        resident_fluid_observation_rejections: 0,
        resident_semantic_cohort_observation_valid: true,
        resident_semantic_cohort_entity_count_overflow: false,
        resident_semantic_cohort_scheduler_mismatch: false,
        resident_semantic_cohort_budget_exceeded: false,
        resident_semantic_cohort_payload_integrity_valid: true,
        resident_semantic_cohort_observation_rejections: 0,
        live_sample_cache_windows: 6,
        live_sample_cache_bytes: 152000,
        budget_entities: 6,
        budget_vertices: 35000,
        budget_indices: 150000,
        budget_mesh_bytes: 2280000,
        budget_build_jobs: 1,
        budget_ring_build_bytes: 388000,
        budget_sample_cache_bytes: 524288,
        budget_coverage_work_bytes: 1545,
        budget_fluid_entities: 6,
        budget_fluid_vertices: 22326,
        budget_fluid_indices: 129600,
        budget_fluid_mesh_bytes: 1590048,
        budget_fluid_ring_build_bytes: 265008,
        budget_hydro_atomic_ring_build_bytes: 653008,
        budget_atomic_ring_build_bytes: 757984,
        budget_semantic_cohort_entities: 1,
        budget_semantic_cohort_vertices: 1944,
        budget_semantic_cohort_indices: 2916,
        budget_semantic_cohort_mesh_bytes: 104976,
        budget_semantic_cohort_hash_scans: 3721,
        budget_semantic_cohort_height_queries: 81,
        budget_semantic_cohort_biome_queries: 81,
        pending_rebuilds: 0,
        dirty_mask: 0,
        build_in_flight: false,
        update_cadence_frames: 1,
        material_detail: "Detailed",
        desired_material_detail: ["Detailed", "Detailed", "Detailed", "Detailed", "Detailed", "Detailed"],
        resident_material_detail: [Some("Detailed"), Some("Detailed"), Some("Detailed"), Some("Detailed"), Some("Detailed"), Some("Detailed")],
        resident_detailed_levels: 6,
        resident_reduced_levels: 0,
        surface_material_mode: "BridgeV2",
        hydro_mode: "DescriptiveV1",
        semantic_cohort_mode: "SilhouettesV1",
        scheduler_deferred_frames: 0,
        completed_rebuilds: 10,
        stale_builds_discarded: 1,
        budget_rejections: 0,
        last_build_ms: 1.25,
        max_build_ms: 16.0,
        last_height_queries: 0,
        last_material_slope_queries: 0,
        last_bridge_v2_cell_reuses: 3800,
        last_fluid_classification_queries: 3721,
        last_fluid_biome_queries: 835,
        last_fluid_vertices: 600,
        last_fluid_indices: 1800,
        last_water_indices: 1200,
        last_lava_indices: 600,
        last_semantic_cohort_hash_scans: 3721,
        last_semantic_cohort_height_queries: 2,
        last_semantic_cohort_biome_queries: 2,
        last_semantic_cohort_candidates: 2,
        last_semantic_cohort_emitted: 2,
        last_semantic_cohort_vertices: 48,
        last_semantic_cohort_indices: 72,
        last_semantic_cohort_kind_counts: [0, 0, 0, 1, 1, 0],
        peak_live_sample_cache_windows: 6,
        peak_live_sample_cache_bytes: 152000,
        last_biome_queries: 0,
        last_reused_height_samples: 100,
        last_reused_biome_samples: 100,
        last_cache_shift_x_cells: 0,
        last_cache_shift_z_cells: 0,
        last_cache_update: "IncrementalStrip",
        incremental_strip_rebuilds: 9,
        full_cache_rebuilds: 1,
        teleport_fallbacks: 0,
        last_clamped_queries: 0,
        camera_world_x: 0,
        camera_world_z: 0,
    )),
    requested_route_focus: "lava",
    resolved_route_focus: "lava",
    route_focus_available: true,
    route_focus_unavailable_reason: None,
    route_focus_anchor: Some([1520, 52, -2320]),
    route_focus_search_candidate_cap: 0,
    route_focus_search_visited_candidates: None,
    route_focus_classification_query_cap: 0,
    route_focus_classification_queries: None,
    route_focus_search_cap_exhausted: false,
    camera_route_policy: "preflight-v1",
    camera_route_preflight_applicable: true,
    camera_route_plan_hash: Some("0000000000000001"),
    camera_route_available: true,
    camera_route_unavailable_reason: None,
    camera_route_variant_index: Some(0),
    camera_route_variant_count: 8,
    camera_route_validation_samples: 16,
    camera_route_voxel_queries: 12,
    camera_route_voxel_query_cap: 153600,
    camera_route_required_chunk_checks: 12,
    camera_route_loaded_chunk_checks: 9,
    camera_route_proven_air_chunk_checks: 3,
    camera_route_unloaded_chunk_checks: 0,
    camera_route_candidate_body_occlusions: 2,
    camera_route_candidate_los_occlusions: 3,
    camera_route_selected_clear_samples: 16,
    camera_route_minimum_clearance_voxels: Some(1),
    camera_route_work_cap_exhausted: false,
    requested_route_distance_m: 8000.0,
    max_horizontal_displacement_m: 8000.0,
    requested_duration_seconds: 12.0,
    duration_seconds: 12.0,
    warmup_seconds: 3.0,
    write_tail_seconds: 0.25,
    frames: 3,
    average_fps: 62.5,
    max_frame_ms: 18.0,
    route_frame_times: (
        scope: "active_route_only_warmup_and_write_tail_excluded",
        sample_count: 3,
        excluded_warmup_sample_count: 2,
        excluded_write_tail_sample_count: 1,
        rejected_sample_count: 0,
        rejected_non_finite_sample_count: 0,
        rejected_non_positive_sample_count: 0,
        rejected_huge_sample_count: 0,
        rejected_arithmetic_overflow_sample_count: 0,
        histogram_overflow_sample_count: 0,
        histogram_bucket_count: 1001,
        histogram_bucket_width_ms: 1,
        histogram_exact_max_ms: 1000,
        accepted_sample_max_ms: 60000,
        quantile_method: "nearest_rank_conservative_bucket_upper_bound",
        quantile_values_are_bucket_upper_bounds: true,
        quantile_max_error_ms: 1.0,
        mean_sample_rounding_max_error_ms: 0.0005,
        quantiles_complete: true,
        measurement_valid: true,
        mean_ms: Some(16.0),
        median_ms: Some(16.0),
        p95_ms: Some(18.0),
        p99_ms: Some(18.0),
        max_ms: Some(18.0),
        accumulator_bytes: 8096,
        quantile_scan_work_cap: 1024,
    ),
    final_smoothed_fps: 60.0,
    loaded_chunks: 100,
    mesh_entities: 80,
    pending_terrain: 0,
    pending_meshes: 0,
    dirty_chunks: 0,
    dense_chunks: 100,
    dense_chunk_budget: 2400,
    dense_chunk_budget_exceeded: false,
    frontier_complete: true,
    render_distance: 36,
    peak_loaded_chunks: 120,
    peak_dense_chunks: 121,
    peak_mesh_entities: 90,
    peak_pending_terrain: 1,
    peak_pending_meshes: 1,
    peak_dirty_chunks: 2,
    screenshots: ["{screenshot}"],
    stalls: [],
)'''


def legacy_report() -> str:
    report = modern_report()
    report = report.replace('    qa_report_schema_version: "2.6.0",\n', "")
    report = report.replace('        build_profile: "debug",\n', "")
    report = report.replace('        git_sha: Some("abcdef1234567"),\n', "")
    report = report.replace('        git_dirty: Some(false),\n', "")
    report = report.replace('        source_fingerprint: Some("sha256:source"),\n', "")
    report = report.replace('        executable_hash: Some("sha256:executable"),\n', "")
    report = report.replace('        toolchain: Some("rustc fixture"),\n', "")
    report = report.replace('        hardware: Some("fixture hardware"),\n', "")
    report = report.replace("    requested_duration_seconds: 12.0,\n", "")
    report = report.replace("    write_tail_seconds: 0.25,\n", "")
    start = report.index("    route_frame_times: (")
    end = report.index("    final_smoothed_fps:", start)
    return report[:start] + report[end:]


class EvidenceManifestTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name) / "repo"
        self.root.mkdir(parents=True)
        (self.root / "qa_runs").mkdir()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def make_run(
        self,
        name: str,
        report_text: str,
        *,
        screenshots: tuple[str, ...] = ("shot_0000.png",),
    ) -> Path:
        run = self.root / "qa_runs" / name
        run.mkdir()
        (run / "report.ron").write_text(report_text, encoding="utf-8")
        for screenshot in screenshots:
            path = run / screenshot
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(PNG_1X1)
        return run

    def build(self, *runs: Path) -> dict:
        return evidence.build_manifest(
            list(runs), repo_root=self.root, generated_at_utc=FIXED_TIME
        )

    def test_current_report_hashes_explicit_report_and_existing_screenshot(self) -> None:
        run = self.make_run("run_current", modern_report())
        manifest = self.build(run)

        self.assertEqual(manifest["schema_version"], "1.6.0")
        self.assertEqual(manifest["generator"]["version"], "1.6.0")
        self.assertEqual(manifest["overall_classification"], "Observed")
        self.assertEqual(manifest["inputs"]["accepted_run_count"], 1)
        self.assertEqual(manifest["runs"][0]["report_schema_variant"], "current")
        observations = manifest["runs"][0]["raw_observations"]
        self.assertEqual(observations["run_identity"]["build_profile"], "debug")
        self.assertEqual(observations["world_edit_store"]["world_edit_store_status"], "compatible")
        self.assertTrue(observations["world_edit_store"]["world_edit_store_compatible"])
        self.assertEqual(observations["world_edit_store"]["world_edit_store_edited_chunks"], 0)
        self.assertEqual(observations["viewport"]["physical_width"], 1280)
        self.assertEqual(observations["route_frame_times"]["p99_ms"], 18.0)
        self.assertEqual(
            observations["planetary_streaming"]["budgets"]["budget_vertices"],
            35000,
        )
        planetary = observations["planetary_streaming"]
        self.assertEqual(planetary["telemetry"]["hydro_mode"], "DescriptiveV1")
        self.assertEqual(planetary["live"]["resident_fluid_vertices"], 2100)
        self.assertEqual(planetary["budgets"]["budget_fluid_vertices"], 22326)
        screenshot = observations["screenshots"]["actual_files"][0]
        self.assertEqual(screenshot["sha256"], hashlib.sha256(PNG_1X1).hexdigest())
        self.assertEqual(screenshot["size_bytes"], len(PNG_1X1))
        self.assertTrue(screenshot["png_complete"])
        self.assertTrue(
            any(record["kind"] == "report" for record in manifest["file_hashes"])
        )
        report_bytes = (run / "report.ron").read_bytes()
        report_hash = next(
            record for record in manifest["file_hashes"] if record["kind"] == "report"
        )
        self.assertEqual(report_hash["sha256"], hashlib.sha256(report_bytes).hexdigest())
        self.assertEqual(report_hash["size_bytes"], len(report_bytes))
        self.assertTrue(
            any(record["kind"] == "screenshot" for record in manifest["file_hashes"])
        )

    def test_current_terrain_grammar_identity_is_required_and_matches_far_worker(self) -> None:
        mutations = (
            (
                "missing_run_grammar",
                '        terrain_grammar: Some("V3"),\n',
                "",
                "missing_terrain_grammar",
            ),
            (
                "unknown_run_grammar",
                '        terrain_grammar: Some("V3"),\n',
                '        terrain_grammar: Some("V4"),\n',
                "invalid_terrain_grammar",
            ),
            (
                "desired_grammar_mismatch",
                '        desired_terrain_grammar: Some("V3"),\n',
                '        desired_terrain_grammar: Some("V1"),\n',
                "planetary_desired_terrain_grammar_mismatch",
            ),
            (
                "active_grammar_mismatch",
                '        active_terrain_grammar: Some("V3"),\n',
                '        active_terrain_grammar: Some("V1"),\n',
                "planetary_active_terrain_grammar_mismatch",
            ),
        )
        for name, old, new, expected_code in mutations:
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(expected_code, {item["code"] for item in record["issues"]})

        for grammar in ("V1", "V2", "V3"):
            with self.subTest(accepted_grammar=grammar):
                report = modern_report().replace('Some("V3")', f'Some("{grammar}")')
                record = self.build(
                    self.make_run(f"run_accepted_{grammar.lower()}", report)
                )["runs"][0]
                self.assertEqual(record["report_schema_variant"], "current")
                self.assertEqual(record["overall_classification"], "Observed")

    def test_current_edit_store_identity_is_exact_and_blocked_state_stays_blocked(self) -> None:
        mutations = (
            (
                "store_grammar_mismatch",
                '    world_edit_store_terrain_grammar: Some("V3"),\n',
                '    world_edit_store_terrain_grammar: Some("V1"),\n',
                "world_edit_store_identity_mismatch",
            ),
            (
                "store_seed_missing",
                "    world_edit_store_seed: Some(12345),\n",
                "    world_edit_store_seed: None,\n",
                "world_edit_store_identity_mismatch",
            ),
            (
                "compatible_without_count",
                "    world_edit_store_edited_chunks: Some(0),\n",
                "    world_edit_store_edited_chunks: None,\n",
                "inconsistent_compatible_world_edit_store",
            ),
        )
        for name, old, new, expected_code in mutations:
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(expected_code, {item["code"] for item in record["issues"]})

        blocked_report = modern_report()
        blocked_report = blocked_report.replace(
            '    world_edit_store_status: "compatible",\n',
            '    world_edit_store_status: "blocked",\n',
        ).replace(
            "    world_edit_store_compatible: true,\n",
            "    world_edit_store_compatible: false,\n",
        ).replace(
            "    world_edit_store_edited_chunks: Some(0),\n",
            "    world_edit_store_edited_chunks: None,\n",
        ).replace(
            "    world_edit_store_block_reason_code: None,\n",
            '    world_edit_store_block_reason_code: Some("manifest-mismatch"),\n',
        )
        blocked = self.build(self.make_run("run_store_blocked", blocked_report))["runs"][0]
        self.assertEqual(blocked["overall_classification"], "Blocked")
        store_claim = next(
            item for item in blocked["claims"] if item["id"].endswith(":world_edit_store")
        )
        self.assertEqual(store_claim["classification"], "Blocked")

    def test_legacy_report_is_blocked_without_invented_route_statistics(self) -> None:
        run = self.make_run("run_legacy", legacy_report())
        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["report_schema_variant"], "legacy")
        self.assertEqual(record["overall_classification"], "Blocked")
        self.assertTrue(
            all(
                value is None
                for value in record["raw_observations"]["route_frame_times"].values()
            )
        )
        codes = {item["code"] for item in record["issues"]}
        self.assertIn("legacy_missing_build_profile", codes)
        self.assertIn("legacy_missing_current_qa_report_schema", codes)
        self.assertNotIn("p95_ms", json.dumps(record["raw_observations"]["route"]))

    def test_malformed_report_is_rejected_but_its_bytes_are_hashed(self) -> None:
        run = self.make_run("run_malformed", "(run_identity: (")
        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("malformed_report", {item["code"] for item in record["issues"]})
        self.assertTrue(
            any(item["kind"] == "report" for item in manifest["file_hashes"])
        )

    def test_reported_screenshot_traversal_is_rejected_and_never_hashed(self) -> None:
        outside = self.root / "qa_runs" / "outside.png"
        outside.write_bytes(PNG_1X1 + b"outside sentinel")
        run = self.make_run(
            "run_traversal", modern_report("../outside.png"), screenshots=()
        )
        manifest = self.build(run)
        record = manifest["runs"][0]
        outside_hash = hashlib.sha256(outside.read_bytes()).hexdigest()

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn(
            "unsafe_screenshot_reference", {item["code"] for item in record["issues"]}
        )
        self.assertNotIn(outside_hash, json.dumps(manifest))

    def test_missing_reported_screenshot_is_rejected(self) -> None:
        run = self.make_run("run_missing_shot", modern_report(), screenshots=())
        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("missing_screenshot", {item["code"] for item in record["issues"]})

    def test_unreferenced_direct_screenshot_is_still_hashed_and_observed(self) -> None:
        run = self.make_run(
            "run_extra_shot",
            modern_report(),
            screenshots=("shot_0000.png", "shot_extra.png"),
        )
        manifest = self.build(run)
        screenshots = manifest["runs"][0]["raw_observations"]["screenshots"]

        self.assertEqual(len(screenshots["actual_files"]), 2)
        self.assertEqual(len(screenshots["unreferenced_files"]), 1)
        self.assertTrue(screenshots["unreferenced_files"][0].endswith("shot_extra.png"))
        screenshot_hashes = [
            item for item in manifest["file_hashes"] if item["kind"] == "screenshot"
        ]
        self.assertEqual(len(screenshot_hashes), 2)

    def test_route_frame_contract_contradictions_are_rejected(self) -> None:
        report = modern_report().replace(
            "rejected_sample_count: 0", "rejected_sample_count: 1"
        )
        run = self.make_run("run_bad_frame_contract", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("contradictory_measurement_validity", codes)
        self.assertIn("rejection_count_mismatch", codes)

    def test_route_duration_cannot_include_screenshot_write_tail(self) -> None:
        report = modern_report().replace(
            "    duration_seconds: 12.0,\n", "    duration_seconds: 12.25,\n"
        )
        run = self.make_run("run_tail_in_duration", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn(
            "route_duration_includes_tail", {item["code"] for item in record["issues"]}
        )

    def test_negative_current_route_timing_is_rejected(self) -> None:
        report = modern_report().replace(
            "    write_tail_seconds: 0.25,\n", "    write_tail_seconds: -0.25,\n"
        )
        run = self.make_run("run_negative_tail", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("invalid_route_timing", {item["code"] for item in record["issues"]})

    def test_dense_chunk_budget_evidence_fails_closed(self) -> None:
        cases = {
            "missing_total": ("    dense_chunks: 100,\n", ""),
            "total_mismatch": ("    dense_chunks: 100,", "    dense_chunks: 101,"),
            "budget_drift": (
                "    dense_chunk_budget: 2400,",
                "    dense_chunk_budget: 2399,",
            ),
            "budget_exceeded": (
                "    dense_chunk_budget_exceeded: false,",
                "    dense_chunk_budget_exceeded: true,",
            ),
            "peak_over_budget": (
                "    peak_dense_chunks: 121,",
                "    peak_dense_chunks: 2401,",
            ),
            "near_field_pending": (
                "    pending_meshes: 0,",
                "    pending_meshes: 1,",
            ),
        }
        for name, (old, new) in cases.items():
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_dense_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertTrue(
                    {
                        "invalid_dense_chunk_budget_evidence",
                        "dense_chunk_total_mismatch",
                        "dense_chunk_budget_identity_drift",
                        "dense_chunk_budget_exceeded",
                        "invalid_peak_dense_chunk_budget_evidence",
                        "near_field_not_settled",
                    }.intersection(item["code"] for item in record["issues"])
                )

    def test_viewport_physical_dimensions_retain_integer_type(self) -> None:
        report = modern_report().replace(
            "        physical_width: 1280,\n", "        physical_width: 1280.5,\n"
        )
        run = self.make_run("run_bad_viewport_type", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("invalid_viewport_field", {item["code"] for item in record["issues"]})

    def test_current_viewport_separates_effective_scale_from_base_dpi(self) -> None:
        report = modern_report().replace(
            "        base_scale_factor: 1.0,\n",
            "        base_scale_factor: 2.0,\n",
        ).replace(
            "        dpi_percent: 100.0,\n",
            "        dpi_percent: 200.0,\n",
        )
        record = self.build(self.make_run("run_exact_scale_override", report))["runs"][0]

        self.assertEqual(record["report_schema_variant"], "current")
        self.assertEqual(record["overall_classification"], "Observed")
        viewport = record["raw_observations"]["viewport"]
        self.assertEqual(viewport["scale_factor"], 1.0)
        self.assertEqual(viewport["base_scale_factor"], 2.0)
        self.assertEqual(viewport["dpi_percent"], 200.0)

    def test_current_viewport_requires_consistent_base_scale_factor(self) -> None:
        cases = {
            "missing": (
                "        base_scale_factor: 1.0,\n",
                "",
                "invalid_viewport_field",
            ),
            "dpi_mismatch": (
                "        base_scale_factor: 1.0,\n",
                "        base_scale_factor: 2.0,\n",
                "inconsistent_viewport_geometry",
            ),
        }
        for name, (old, new, expected_code) in cases.items():
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_base_scale_{name}", report))["runs"][0]
                self.assertEqual(record["report_schema_variant"], "current")
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(expected_code, {item["code"] for item in record["issues"]})

    def test_missing_planetary_budget_and_telemetry_are_rejected(self) -> None:
        report = modern_report().replace("        budget_vertices: 35000,\n", "")
        report = report.replace("        budget_rejections: 0,\n", "")
        run = self.make_run("run_missing_planetary_fields", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("invalid_planetary_budget", codes)
        self.assertIn("invalid_planetary_telemetry_count", codes)

    def test_planetary_material_transition_must_be_internally_consistent(self) -> None:
        report = modern_report().replace(
            '        desired_material_detail: ["Detailed", "Detailed", "Detailed", "Detailed", "Detailed", "Detailed"],\n',
            '        desired_material_detail: ["Reduced", "Detailed", "Detailed", "Detailed", "Detailed", "Detailed"],\n',
        )
        report = report.replace(
            "        resident_detailed_levels: 6,\n",
            "        resident_detailed_levels: 5,\n",
        )
        report = report.replace(
            '        surface_material_mode: "BridgeV2",\n',
            '        surface_material_mode: "UnknownBridge",\n',
        )
        report = report.replace(
            "        ring_vertices: [5000, 5000, 5000, 5000, 4000, 4000],\n",
            "        ring_vertices: [5001, 5000, 5000, 5000, 4000, 4000],\n",
        )
        run = self.make_run("run_inconsistent_material_transition", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("planetary_material_detail_summary_mismatch", codes)
        self.assertIn("planetary_resident_material_counts_mismatch", codes)
        self.assertIn("invalid_planetary_surface_material_mode", codes)
        self.assertIn("planetary_ring_population_total_mismatch", codes)

    def test_planetary_cache_current_and_peak_must_respect_hard_caps(self) -> None:
        report = modern_report().replace(
            "        live_sample_cache_windows: 6,\n",
            "        live_sample_cache_windows: 7,\n",
        )
        report = report.replace(
            "        peak_live_sample_cache_bytes: 152000,\n",
            "        peak_live_sample_cache_bytes: 151999,\n",
        )
        run = self.make_run("run_invalid_cache_population", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("planetary_cache_window_cap_exceeded", codes)
        self.assertIn("planetary_cache_peak_below_live", codes)

    def test_planetary_ecs_observation_must_match_scheduler_truth(self) -> None:
        report = modern_report().replace(
            "        resident_observation_valid: true,\n",
            "        resident_observation_valid: false,\n",
        )
        report = report.replace(
            "        resident_observation_rejections: 0,\n",
            "        resident_observation_rejections: 1,\n",
        )
        report = report.replace(
            "        scheduler_resident_vertices: 28000,\n",
            "        scheduler_resident_vertices: 27999,\n",
        )
        run = self.make_run("run_invalid_ecs_observation", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("planetary_residency_observation_rejected", codes)
        self.assertIn("planetary_scheduler_observation_mismatch", codes)

    def test_planetary_fluid_observation_and_scheduler_truth_fail_closed(self) -> None:
        report = modern_report().replace(
            "        resident_fluid_observation_valid: true,\n",
            "        resident_fluid_observation_valid: false,\n",
        )
        report = report.replace(
            "        resident_fluid_observation_rejections: 0,\n",
            "        resident_fluid_observation_rejections: 1,\n",
        )
        report = report.replace(
            "        scheduler_resident_fluid_vertices: 2100,\n",
            "        scheduler_resident_fluid_vertices: 2099,\n",
        )
        run = self.make_run("run_invalid_fluid_ecs_observation", report)
        record = self.build(run)["runs"][0]
        codes = {item["code"] for item in record["issues"]}

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("planetary_fluid_residency_observation_rejected", codes)
        self.assertIn("planetary_scheduler_observation_mismatch", codes)

    def test_planetary_fluid_budget_overflow_is_rejected(self) -> None:
        report = modern_report().replace(
            "        budget_fluid_vertices: 22326,\n",
            "        budget_fluid_vertices: 2099,\n",
        )
        run = self.make_run("run_fluid_budget_overflow", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn(
            "planetary_budget_exceeded", {item["code"] for item in record["issues"]}
        )

    def test_disabled_hydro_requires_zero_live_and_latest_work(self) -> None:
        report = modern_report().replace(
            '        hydro_mode: "DescriptiveV1",\n',
            '        hydro_mode: "Disabled",\n',
        )
        run = self.make_run("run_disabled_hydro_with_live_work", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn(
            "planetary_disabled_hydro_has_live_work",
            {item["code"] for item in record["issues"]},
        )

    def test_missing_schema_version_is_legacy_and_blocked_with_complete_fields(self) -> None:
        report = modern_report().replace('    qa_report_schema_version: "2.6.0",\n', "")
        run = self.make_run("run_pre_hydro_schema", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["report_schema_variant"], "legacy")
        self.assertEqual(record["overall_classification"], "Blocked")
        self.assertIn(
            "legacy_missing_current_qa_report_schema",
            {item["code"] for item in record["issues"]},
        )

    def test_exact_qa_25_through_20_are_legacy_blocked_and_other_versions_are_unsupported(self) -> None:
        qa_25 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.5.0"',
        )
        legacy_25 = self.build(self.make_run("run_exact_25", qa_25))["runs"][0]
        self.assertEqual(legacy_25["report_schema_variant"], "legacy")
        self.assertEqual(legacy_25["overall_classification"], "Blocked")
        self.assertIn(
            "legacy_missing_current_qa_report_schema",
            {item["code"] for item in legacy_25["issues"]},
        )
        legacy_25_claims = {item["id"].rsplit(":", 1)[-1]: item for item in legacy_25["claims"]}
        self.assertEqual(
            legacy_25_claims["world_edit_store"]["statement"],
            "Legacy QA edit-store observations cannot authorize current compatibility.",
        )
        self.assertEqual(
            legacy_25_claims["route_observation"]["statement"],
            "Legacy QA route observations cannot authorize current publication.",
        )
        self.assertEqual(
            legacy_25_claims["route_frame_times"]["statement"],
            "Legacy frame-time data is historical and not publishable under the current manifest contract.",
        )

        historical_25 = qa_25.replace("        base_scale_factor: 1.0,\n", "")
        historical_25_record = self.build(
            self.make_run("run_historical_25_effective_dpi", historical_25)
        )["runs"][0]
        historical_25_codes = {
            item["code"] for item in historical_25_record["issues"]
        }
        self.assertEqual(historical_25_record["report_schema_variant"], "legacy")
        self.assertEqual(historical_25_record["overall_classification"], "Blocked")
        self.assertNotIn("invalid_viewport_field", historical_25_codes)
        self.assertNotIn("inconsistent_viewport_geometry", historical_25_codes)

        qa_24 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.4.0"',
        )
        legacy_24 = self.build(self.make_run("run_exact_24", qa_24))["runs"][0]
        self.assertEqual(legacy_24["report_schema_variant"], "legacy")
        self.assertEqual(legacy_24["overall_classification"], "Blocked")

        qa_23 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.3.0"',
        )
        legacy_23 = self.build(self.make_run("run_exact_23", qa_23))["runs"][0]
        self.assertEqual(legacy_23["report_schema_variant"], "legacy")
        self.assertEqual(legacy_23["overall_classification"], "Blocked")

        qa_22 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.2.0"',
        )
        legacy_22 = self.build(self.make_run("run_exact_22", qa_22))["runs"][0]
        self.assertEqual(legacy_22["report_schema_variant"], "legacy")
        self.assertEqual(legacy_22["overall_classification"], "Blocked")

        qa_21 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.1.0"',
        )
        legacy_21 = self.build(self.make_run("run_exact_21", qa_21))["runs"][0]
        self.assertEqual(legacy_21["report_schema_variant"], "legacy")
        self.assertEqual(legacy_21["overall_classification"], "Blocked")

        qa_20 = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.0.0"',
        )
        legacy = self.build(self.make_run("run_exact_20", qa_20))["runs"][0]
        self.assertEqual(legacy["report_schema_variant"], "legacy")
        self.assertEqual(legacy["overall_classification"], "Blocked")

        future = modern_report().replace(
            'qa_report_schema_version: "2.6.0"',
            'qa_report_schema_version: "2.7.0"',
        )
        unsupported = self.build(self.make_run("run_future", future))["runs"][0]
        self.assertEqual(unsupported["report_schema_variant"], "unsupported")
        self.assertEqual(unsupported["overall_classification"], "Rejected")
        self.assertIn(
            "unsupported_qa_report_schema",
            {item["code"] for item in unsupported["issues"]},
        )

    def test_non_bare_qa_schema_tokens_are_rejected_without_aborting_manifest(self) -> None:
        for name, token in (
            ("current_option", 'Some("2.6.0")'),
            ("legacy_option", 'Some("2.5.0")'),
            ("list", "[]"),
            ("map", '{"unexpected": "2.6.0"}'),
        ):
            with self.subTest(name=name):
                report = modern_report().replace(
                    'qa_report_schema_version: "2.6.0"',
                    f"qa_report_schema_version: {token}",
                )
                record = self.build(
                    self.make_run(f"run_structured_schema_{name}", report)
                )["runs"][0]

                self.assertEqual(record["report_schema_variant"], "unsupported")
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(
                    "unsupported_qa_report_schema",
                    {item["code"] for item in record["issues"]},
                )

    def test_route_resolution_and_bounded_search_truth_fail_closed(self) -> None:
        contradictions = (
            (
                "available_fallback",
                '    resolved_route_focus: "lava",\n',
                '    resolved_route_focus: "scenic",\n',
                "contradictory_available_route_resolution",
            ),
            (
                "candidate_overflow",
                "    route_focus_search_visited_candidates: None,\n",
                "    route_focus_search_visited_candidates: Some(1),\n",
                "route_search_cap_exceeded",
            ),
        )
        for name, old, new, code in contradictions:
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(code, {item["code"] for item in record["issues"]})

        unavailable = modern_report().replace(
            '    resolved_route_focus: "lava",\n',
            '    resolved_route_focus: "scenic",\n',
        ).replace(
            "    route_focus_available: true,\n",
            "    route_focus_available: false,\n",
        ).replace(
            "    route_focus_unavailable_reason: None,\n",
            '    route_focus_unavailable_reason: Some("no bounded focus found"),\n',
        )
        record = self.build(self.make_run("run_unavailable_route", unavailable))["runs"][0]
        self.assertEqual(record["overall_classification"], "Blocked")
        self.assertIn(
            "requested_route_focus_unavailable",
            {item["code"] for item in record["issues"]},
        )

        focus_unavailable_camera = (
            unavailable
            .replace('    camera_route_plan_hash: Some("0000000000000001"),\n', "    camera_route_plan_hash: None,\n")
            .replace("    camera_route_available: true,\n", "    camera_route_available: false,\n")
            .replace(
                "    camera_route_unavailable_reason: None,\n",
                '    camera_route_unavailable_reason: Some("camera-route-focus-unavailable"),\n',
            )
            .replace("    camera_route_variant_index: Some(0),\n", "    camera_route_variant_index: None,\n")
            .replace("    camera_route_variant_count: 8,\n", "    camera_route_variant_count: 0,\n")
            .replace("    camera_route_validation_samples: 16,\n", "    camera_route_validation_samples: 0,\n")
            .replace("    camera_route_voxel_queries: 12,\n", "    camera_route_voxel_queries: 0,\n")
            .replace("    camera_route_voxel_query_cap: 153600,\n", "    camera_route_voxel_query_cap: 0,\n")
            .replace("    camera_route_required_chunk_checks: 12,\n", "    camera_route_required_chunk_checks: 0,\n")
            .replace("    camera_route_loaded_chunk_checks: 9,\n", "    camera_route_loaded_chunk_checks: 0,\n")
            .replace("    camera_route_proven_air_chunk_checks: 3,\n", "    camera_route_proven_air_chunk_checks: 0,\n")
            .replace("    camera_route_candidate_body_occlusions: 2,\n", "    camera_route_candidate_body_occlusions: 0,\n")
            .replace("    camera_route_candidate_los_occlusions: 3,\n", "    camera_route_candidate_los_occlusions: 0,\n")
            .replace("    camera_route_selected_clear_samples: 16,\n", "    camera_route_selected_clear_samples: 0,\n")
            .replace("    camera_route_minimum_clearance_voxels: Some(1),\n", "    camera_route_minimum_clearance_voxels: None,\n")
        )
        record = self.build(
            self.make_run("run_focus_unavailable_camera", focus_unavailable_camera)
        )["runs"][0]
        self.assertEqual(record["overall_classification"], "Blocked")
        self.assertIn("camera_route_focus_unavailable", {item["code"] for item in record["issues"]})

    def test_schema_26_camera_preflight_truth_fails_closed(self) -> None:
        mutations = (
            (
                "missing_plan_hash",
                '    camera_route_plan_hash: Some("0000000000000001"),\n',
                "",
                "camera_route_acceptance_invariant_failed",
            ),
            (
                "unloaded_chunk",
                "    camera_route_loaded_chunk_checks: 9,\n    camera_route_proven_air_chunk_checks: 3,\n    camera_route_unloaded_chunk_checks: 0,\n",
                "    camera_route_loaded_chunk_checks: 8,\n    camera_route_proven_air_chunk_checks: 3,\n    camera_route_unloaded_chunk_checks: 1,\n",
                "camera_route_acceptance_invariant_failed",
            ),
            (
                "proven_air_accounting_mismatch",
                "    camera_route_proven_air_chunk_checks: 3,\n",
                "    camera_route_proven_air_chunk_checks: 4,\n",
                "camera_route_chunk_accounting_mismatch",
            ),
            (
                "missing_proven_air_counter",
                "    camera_route_proven_air_chunk_checks: 3,\n",
                "",
                "invalid_camera_route_counter",
            ),
            (
                "selected_route_not_clear",
                "    camera_route_selected_clear_samples: 16,\n",
                "    camera_route_selected_clear_samples: 15,\n",
                "camera_route_acceptance_invariant_failed",
            ),
            (
                "candidate_occlusion_overflow",
                "    camera_route_candidate_los_occlusions: 3,\n",
                "    camera_route_candidate_los_occlusions: 129,\n",
                "camera_route_candidate_occlusion_count_exceeded",
            ),
            (
                "work_cap",
                "    camera_route_voxel_queries: 12,\n    camera_route_voxel_query_cap: 153600,\n    camera_route_required_chunk_checks: 12,\n    camera_route_loaded_chunk_checks: 9,\n    camera_route_proven_air_chunk_checks: 3,\n",
                "    camera_route_voxel_queries: 153600,\n    camera_route_voxel_query_cap: 153600,\n    camera_route_required_chunk_checks: 153600,\n    camera_route_loaded_chunk_checks: 120000,\n    camera_route_proven_air_chunk_checks: 33600,\n",
                "camera_route_acceptance_invariant_failed",
            ),
            (
                "obsolete_columns",
                "    camera_route_required_chunk_checks: 12,\n",
                "    camera_route_required_columns: 12,\n",
                "obsolete_camera_route_field",
            ),
        )
        for name, old, new, expected_code in mutations:
            with self.subTest(name=name):
                report = modern_report().replace(old, new)
                record = self.build(self.make_run(f"run_camera_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(expected_code, {item["code"] for item in record["issues"]})

        unavailable = (
            modern_report()
            .replace('    camera_route_plan_hash: Some("0000000000000001"),\n', "    camera_route_plan_hash: None,\n")
            .replace("    camera_route_available: true,\n", "    camera_route_available: false,\n")
            .replace(
                "    camera_route_unavailable_reason: None,\n",
                '    camera_route_unavailable_reason: Some("camera-route-los-occluded"),\n',
            )
            .replace("    camera_route_variant_index: Some(0),\n", "    camera_route_variant_index: None,\n")
            .replace(
                "    camera_route_minimum_clearance_voxels: Some(1),\n",
                "    camera_route_minimum_clearance_voxels: None,\n",
            )
            .replace(
                "    camera_route_selected_clear_samples: 16,\n",
                "    camera_route_selected_clear_samples: 0,\n",
            )
        )
        record = self.build(self.make_run("run_camera_unavailable", unavailable))["runs"][0]
        self.assertEqual(record["overall_classification"], "Blocked")
        self.assertIn("camera_route_unavailable", {item["code"] for item in record["issues"]})

    def test_schema_26_non_applicable_camera_sentinel_is_current_observed(self) -> None:
        report = (
            modern_report()
            .replace('    requested_route_focus: "lava",\n', '    requested_route_focus: "streaming",\n')
            .replace('    resolved_route_focus: "lava",\n', '    resolved_route_focus: "streaming",\n')
            .replace("    route_focus_anchor: Some([1520, 52, -2320]),\n", "    route_focus_anchor: None,\n")
            .replace("    camera_route_preflight_applicable: true,\n", "    camera_route_preflight_applicable: false,\n")
            .replace('    camera_route_plan_hash: Some("0000000000000001"),\n', "    camera_route_plan_hash: None,\n")
            .replace("    camera_route_available: true,\n", "    camera_route_available: false,\n")
            .replace("    camera_route_variant_index: Some(0),\n", "    camera_route_variant_index: None,\n")
            .replace("    camera_route_variant_count: 8,\n", "    camera_route_variant_count: 0,\n")
            .replace("    camera_route_validation_samples: 16,\n", "    camera_route_validation_samples: 0,\n")
            .replace("    camera_route_voxel_queries: 12,\n", "    camera_route_voxel_queries: 0,\n")
            .replace("    camera_route_voxel_query_cap: 153600,\n", "    camera_route_voxel_query_cap: 0,\n")
            .replace("    camera_route_required_chunk_checks: 12,\n", "    camera_route_required_chunk_checks: 0,\n")
            .replace("    camera_route_loaded_chunk_checks: 9,\n", "    camera_route_loaded_chunk_checks: 0,\n")
            .replace("    camera_route_proven_air_chunk_checks: 3,\n", "    camera_route_proven_air_chunk_checks: 0,\n")
            .replace("    camera_route_candidate_body_occlusions: 2,\n", "    camera_route_candidate_body_occlusions: 0,\n")
            .replace("    camera_route_candidate_los_occlusions: 3,\n", "    camera_route_candidate_los_occlusions: 0,\n")
            .replace("    camera_route_selected_clear_samples: 16,\n", "    camera_route_selected_clear_samples: 0,\n")
            .replace("    camera_route_minimum_clearance_voxels: Some(1),\n", "    camera_route_minimum_clearance_voxels: None,\n")
        )
        record = self.build(self.make_run("run_camera_not_applicable", report))["runs"][0]
        self.assertEqual(record["report_schema_variant"], "current")
        self.assertEqual(record["overall_classification"], "Observed")

    def test_available_spatial_route_requires_anchor_and_compatible_profile(self) -> None:
        cases = (
            (
                "waypoint_without_anchor",
                "waypoint",
                "AstralFrontier",
                "None",
                "available_route_focus_missing_anchor",
            ),
            (
                "river_in_astral_world",
                "river",
                "AstralFrontier",
                "Some([1, 2, 3])",
                "route_focus_profile_mismatch",
            ),
            (
                "lava_in_natural_world",
                "lava",
                "Natural",
                "Some([1, 2, 3])",
                "route_focus_profile_mismatch",
            ),
            (
                "near_far_in_unknown_world",
                "near-far",
                "Unknown",
                "Some([1, 2, 3])",
                "route_focus_profile_mismatch",
            ),
        )
        for name, focus, world_profile, anchor, code in cases:
            with self.subTest(name=name):
                report = (
                    modern_report()
                    .replace(
                        '        world_profile: Some("AstralFrontier"),\n',
                        f'        world_profile: Some("{world_profile}"),\n',
                    )
                    .replace(
                        '    requested_route_focus: "lava",\n',
                        f'    requested_route_focus: "{focus}",\n',
                    )
                    .replace(
                        '    resolved_route_focus: "lava",\n',
                        f'    resolved_route_focus: "{focus}",\n',
                    )
                    .replace(
                        "    route_focus_anchor: Some([1520, 52, -2320]),\n",
                        f"    route_focus_anchor: {anchor},\n",
                    )
                )
                record = self.build(self.make_run(f"run_{name}", report))["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(code, {item["code"] for item in record["issues"]})

        anchored_waypoint = modern_report().replace(
            '    requested_route_focus: "lava",\n',
            '    requested_route_focus: "waypoint",\n',
        ).replace(
            '    resolved_route_focus: "lava",\n',
            '    resolved_route_focus: "waypoint",\n',
        ).replace(
            "    route_focus_anchor: Some([1520, 52, -2320]),\n",
            "    route_focus_anchor: Some([1, 2, 3]),\n",
        )
        record = self.build(
            self.make_run("run_anchored_waypoint", anchored_waypoint)
        )["runs"][0]
        self.assertEqual(record["overall_classification"], "Rejected")

    def test_hydro_kind_and_semantic_cohort_invariants_fail_closed(self) -> None:
        mutations = (
            (
                "hydro_total",
                "        resident_water_indices: 4200,\n",
                "        resident_water_indices: 4194,\n",
                "planetary_fluid_kind_integrity_mismatch",
            ),
            (
                "cohort_kind_sum",
                "        resident_semantic_cohort_kind_counts: [0, 0, 0, 1, 1, 0],\n",
                "        resident_semantic_cohort_kind_counts: [0, 0, 0, 1, 0, 0],\n",
                "planetary_semantic_cohort_payload_mismatch",
            ),
            (
                "cohort_budget",
                "        budget_semantic_cohort_vertices: 1944,\n",
                "        budget_semantic_cohort_vertices: 1945,\n",
                "unexpected_semantic_cohort_budget",
            ),
        )
        for name, old, new, code in mutations:
            with self.subTest(name=name):
                record = self.build(
                    self.make_run(f"run_{name}", modern_report().replace(old, new))
                )["runs"][0]
                self.assertEqual(record["overall_classification"], "Rejected")
                self.assertIn(code, {item["code"] for item in record["issues"]})

    def test_disabled_semantic_cohorts_require_zero_population_and_work(self) -> None:
        report = modern_report().replace(
            '        semantic_cohort_mode: "SilhouettesV1",\n',
            '        semantic_cohort_mode: "Disabled",\n',
        )
        record = self.build(self.make_run("run_disabled_cohorts_with_work", report))["runs"][0]
        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn(
            "planetary_disabled_semantic_cohorts_have_live_work",
            {item["code"] for item in record["issues"]},
        )

    def test_pre_hydro_report_without_fluid_contract_is_rejected(self) -> None:
        report = modern_report().replace('    qa_report_schema_version: "2.6.0",\n', "")
        hydro_lines = (
            "        resident_fluid_entities: 6,\n",
            "        resident_fluid_vertices: 2100,\n",
            "        resident_fluid_indices: 6300,\n",
            "        fluid_ring_vertices: [100, 200, 300, 400, 500, 600],\n",
            "        fluid_ring_indices: [300, 600, 900, 1200, 1500, 1800],\n",
            "        resident_fluid_mesh_bytes: 100800,\n",
            "        scheduler_resident_fluid_entities: 6,\n",
            "        scheduler_resident_fluid_vertices: 2100,\n",
            "        scheduler_resident_fluid_indices: 6300,\n",
            "        scheduler_fluid_ring_vertices: [100, 200, 300, 400, 500, 600],\n",
            "        scheduler_fluid_ring_indices: [300, 600, 900, 1200, 1500, 1800],\n",
            "        scheduler_resident_fluid_mesh_bytes: 100800,\n",
            "        resident_fluid_observation_valid: true,\n",
            "        resident_fluid_entity_count_overflow: false,\n",
            "        resident_fluid_duplicate_slots: 0,\n",
            "        resident_fluid_out_of_range_levels: 0,\n",
            "        resident_fluid_scheduler_mismatch: false,\n",
            "        resident_fluid_budget_exceeded: false,\n",
            "        resident_fluid_observation_rejections: 0,\n",
            "        budget_fluid_entities: 6,\n",
            "        budget_fluid_vertices: 22326,\n",
            "        budget_fluid_indices: 129600,\n",
            "        budget_fluid_mesh_bytes: 1590048,\n",
            "        budget_fluid_ring_build_bytes: 265008,\n",
            "        budget_atomic_ring_build_bytes: 653008,\n",
            '        hydro_mode: "DescriptiveV1",\n',
            "        last_fluid_classification_queries: 3721,\n",
            "        last_fluid_biome_queries: 835,\n",
            "        last_fluid_vertices: 600,\n",
            "        last_fluid_indices: 1800,\n",
        )
        for line in hydro_lines:
            report = report.replace(line, "")
        run = self.make_run("run_true_pre_hydro_schema", report)
        record = self.build(run)["runs"][0]

        self.assertEqual(record["report_schema_variant"], "legacy")
        self.assertEqual(record["overall_classification"], "Blocked")
        codes = {item["code"] for item in record["issues"]}
        self.assertIn("legacy_missing_current_qa_report_schema", codes)
        self.assertNotIn("invalid_planetary_hydro_mode", codes)
        self.assertNotIn("invalid_planetary_budget", codes)

    def test_report_symlink_escape_is_rejected_without_hashing_target(self) -> None:
        outside_report = self.root / "outside-report.ron"
        outside_report.write_text(modern_report(), encoding="utf-8")
        outside_hash = hashlib.sha256(outside_report.read_bytes()).hexdigest()
        run = self.root / "qa_runs" / "run_report_escape"
        run.mkdir()
        try:
            os.symlink(outside_report, run / "report.ron")
        except OSError as error:
            self.skipTest(f"file symlinks unavailable: {error}")

        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("report_symlink_escape", {item["code"] for item in record["issues"]})
        self.assertNotIn(outside_hash, json.dumps(manifest))

    def test_unreferenced_screenshot_symlink_escape_rejects_run(self) -> None:
        outside_screenshot = self.root / "outside-shot.png"
        outside_screenshot.write_bytes(PNG_1X1 + b"outside screenshot sentinel")
        outside_hash = hashlib.sha256(outside_screenshot.read_bytes()).hexdigest()
        run = self.make_run("run_direct_symlink_escape", modern_report())
        try:
            os.symlink(outside_screenshot, run / "unreferenced-escape.png")
        except OSError as error:
            self.skipTest(f"file symlinks unavailable: {error}")

        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("screenshot_symlink_escape", {item["code"] for item in record["issues"]})
        self.assertNotIn(outside_hash, json.dumps(manifest))

    def test_non_finite_value_rejects_report_and_json_remains_strict(self) -> None:
        report = modern_report().replace("average_fps: 62.5", "average_fps: NaN")
        run = self.make_run("run_non_finite", report)
        manifest = self.build(run)

        self.assertEqual(manifest["runs"][0]["overall_classification"], "Rejected")
        self.assertIn(
            "non_finite_report_value",
            {item["code"] for item in manifest["runs"][0]["issues"]},
        )
        json.dumps(manifest, allow_nan=False)

    def test_invalid_serialized_provenance_is_rejected_instead_of_trusted(self) -> None:
        report = modern_report().replace(
            'git_sha: Some("abcdef1234567")', 'git_sha: Some("not-a-sha")'
        )
        run = self.make_run("run_bad_provenance", report)
        manifest = self.build(run)
        record = manifest["runs"][0]

        self.assertEqual(record["overall_classification"], "Rejected")
        self.assertIn("invalid_git_sha", {item["code"] for item in record["issues"]})

    def test_duplicate_runs_are_deduplicated_and_rejected_at_input_grain(self) -> None:
        run = self.make_run("run_duplicate", modern_report())
        manifest = evidence.build_manifest(
            [run, run.resolve()], repo_root=self.root, generated_at_utc=FIXED_TIME
        )

        self.assertEqual(manifest["overall_classification"], "Rejected")
        self.assertEqual(manifest["inputs"]["argument_count"], 2)
        self.assertEqual(manifest["inputs"]["accepted_run_count"], 1)
        self.assertEqual(len(manifest["runs"]), 1)
        self.assertIn("duplicate_run_input", {item["code"] for item in manifest["issues"]})

    def test_input_parent_traversal_and_latest_alias_are_rejected_without_scan(self) -> None:
        run = self.make_run("run_safe", modern_report())
        latest = self.root / "qa_runs" / "latest"
        latest.mkdir()
        traversing = run / ".." / "run_safe"
        manifest = evidence.build_manifest(
            [traversing, latest], repo_root=self.root, generated_at_utc=FIXED_TIME
        )

        self.assertEqual(manifest["overall_classification"], "Rejected")
        self.assertEqual(manifest["inputs"]["accepted_run_count"], 0)
        self.assertEqual(manifest["runs"], [])
        codes = {item["code"] for item in manifest["issues"]}
        self.assertEqual(codes, {"implicit_latest_forbidden", "run_path_traversal"})

    def test_external_run_is_rejected_without_serializing_workstation_path(self) -> None:
        external_run = Path(self.temporary.name) / "private-owner" / "run_external"
        external_run.mkdir(parents=True)
        (external_run / "report.ron").write_text(modern_report(), encoding="utf-8")
        (external_run / "shot_0000.png").write_bytes(PNG_1X1)

        manifest = evidence.build_manifest(
            [external_run], repo_root=self.root, generated_at_utc=FIXED_TIME
        )
        serialized = json.dumps(manifest)

        self.assertEqual(manifest["overall_classification"], "Rejected")
        self.assertEqual(manifest["inputs"]["accepted_run_count"], 0)
        self.assertEqual(manifest["runs"], [])
        self.assertEqual(
            {item["code"] for item in manifest["issues"]},
            {"run_outside_repository"},
        )
        self.assertNotIn("private-owner", serialized)
        self.assertNotIn(str(external_run), serialized)

    def test_rejected_input_issue_dominates_an_observed_valid_run(self) -> None:
        run = self.make_run("run_valid_beside_bad_input", modern_report())
        traversing = run / ".." / run.name
        manifest = evidence.build_manifest(
            [run, traversing], repo_root=self.root, generated_at_utc=FIXED_TIME
        )

        self.assertEqual(manifest["inputs"]["accepted_run_count"], 1)
        self.assertEqual(manifest["runs"][0]["overall_classification"], "Observed")
        self.assertEqual(manifest["overall_classification"], "Rejected")
        self.assertEqual(manifest["summary"]["issue_counts"]["Rejected"], 1)

    def test_protected_output_paths_fail_before_writing(self) -> None:
        for directory in evidence.PROTECTED_OUTPUT_DIRS:
            with self.subTest(directory=directory):
                with self.assertRaises(evidence.EvidenceManifestError):
                    evidence.validate_output_path(
                        self.root / directory / "manifest.json", self.root
                    )

    def test_cli_writes_only_the_explicit_safe_output(self) -> None:
        run = self.make_run("run_cli", modern_report())
        output = self.root / "generated" / "manifest.json"
        stdout = io.StringIO()

        synthetic_script = self.root / "tools" / "artifacts" / "build_evidence_manifest.py"
        synthetic_script.parent.mkdir(parents=True)
        synthetic_script.write_text("# fixture generator\n", encoding="utf-8")
        with contextlib.redirect_stdout(stdout), mock.patch.object(
            evidence, "__file__", str(synthetic_script)
        ):
            exit_code = evidence.main(
                ["--qa-run", str(run), "--output", str(output)]
            )

        self.assertEqual(exit_code, 0)
        self.assertTrue(output.is_file())
        parsed = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(parsed["overall_classification"], "Observed")
        self.assertEqual(parsed["inputs"]["accepted_run_count"], 1)
        self.assertFalse(any(output.parent.glob(f".{output.name}.*.tmp")))

    def test_manifest_is_stable_when_timestamp_and_inputs_are_identical(self) -> None:
        run_b = self.make_run("run_b", modern_report())
        run_a = self.make_run("run_a", modern_report())
        first = self.build(run_b, run_a)
        second = self.build(run_a, run_b)

        self.assertEqual(evidence.manifest_json(first), evidence.manifest_json(second))
        self.assertEqual(
            first["inputs"]["qa_run_directories"],
            sorted(first["inputs"]["qa_run_directories"]),
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
