import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";


export const SCHEMA_VERSION = "1.6.0";
const GENERATOR_NAME = "voxel-native-evidence-manifest";
const GENERATOR_VERSION = "1.6.0";
const SELECTION_POLICY = "explicit_repo_contained_directories_only_no_latest_no_global_scan";
const CLASSIFICATIONS = ["Passed", "Observed", "Rejected", "Planned", "Blocked"];
const PROTECTED_OUTPUT_DIRS = ["saves", "qa_runs", "agent_runs"];
const MAX_MANIFEST_BYTES = 8 * 1024 * 1024;
const MAX_RUNS = 100;
const MAX_CLAIMS = 4000;
const MAX_ISSUES = 4000;
const MAX_FILE_HASHES = 2000;
const MAX_SCREENSHOTS_PER_RUN = 128;
const MAX_SCREENSHOT_BYTES = 64 * 1024 * 1024;
const MAX_EMBEDDED_SCREENSHOTS = 8;
const MAX_EMBEDDED_SCREENSHOT_BYTES = 128 * 1024 * 1024;
const SHA256_RE = /^[0-9a-f]{64}$/;
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const PNG_IEND = Buffer.from([0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]);


export class EvidenceContractError extends Error {
  constructor(message) {
    super(message);
    this.name = "EvidenceContractError";
  }
}


function requireContract(condition, message) {
  if (!condition) throw new EvidenceContractError(message);
}


function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}


function isUInt(value) {
  return Number.isInteger(value) && value >= 0;
}


function isFiniteNumber(value, positive = false) {
  return typeof value === "number" && Number.isFinite(value) && (positive ? value > 0 : value >= 0);
}


function boundedText(value, limit = 16384) {
  return typeof value === "string" && value.length > 0 && value.length <= limit && !/[\u0000-\u001f\u007f]/u.test(value);
}


function repositoryRelativePath(value) {
  return boundedText(value, 4096)
    && !value.includes("\\")
    && !value.startsWith("/")
    && !/^[A-Za-z]:/u.test(value)
    && value.split("/").every((part) => part !== "" && part !== "." && part !== "..");
}


function emptyCounts() {
  return Object.fromEntries(CLASSIFICATIONS.map((classification) => [classification, 0]));
}


function addCounts(target, source) {
  for (const classification of CLASSIFICATIONS) target[classification] += source[classification];
}


function countsEqual(actual, expected) {
  return isObject(actual) && CLASSIFICATIONS.every((classification) => actual[classification] === expected[classification]);
}


function validateClaimOrIssue(item, { kind, index, requireEvidence }) {
  requireContract(isObject(item), `${kind}[${index}] must be an object`);
  requireContract(CLASSIFICATIONS.includes(item.classification), `${kind}[${index}].classification is invalid`);
  if (requireEvidence) {
    requireContract(boundedText(item.id, 4096), `${kind}[${index}].id is invalid`);
    requireContract(boundedText(item.statement), `${kind}[${index}].statement is invalid`);
    requireContract(Array.isArray(item.evidence) && item.evidence.length <= 256, `${kind}[${index}].evidence is invalid`);
    requireContract(item.evidence.every((entry) => boundedText(entry, 4096)), `${kind}[${index}].evidence contains an invalid path`);
  } else {
    requireContract(boundedText(item.code, 256), `${kind}[${index}].code is invalid`);
    requireContract(boundedText(item.field, 4096), `${kind}[${index}].field is invalid`);
    requireContract(boundedText(item.message), `${kind}[${index}].message is invalid`);
  }
  return item.classification;
}


function validateClaimSet(claims, issues, scope) {
  requireContract(Array.isArray(claims) && claims.length <= MAX_CLAIMS, `${scope}.claims violates the fixed cap`);
  requireContract(Array.isArray(issues) && issues.length <= MAX_ISSUES, `${scope}.issues violates the fixed cap`);
  const claimCounts = emptyCounts();
  const issueCounts = emptyCounts();
  const ids = new Set();
  claims.forEach((item, index) => {
    const classification = validateClaimOrIssue(item, { kind: `${scope}.claims`, index, requireEvidence: true });
    requireContract(!ids.has(item.id), `duplicate claim id: ${item.id}`);
    ids.add(item.id);
    claimCounts[classification] += 1;
  });
  issues.forEach((item, index) => {
    const classification = validateClaimOrIssue(item, { kind: `${scope}.issues`, index, requireEvidence: false });
    issueCounts[classification] += 1;
  });
  return { claimCounts, issueCounts };
}


function requireClaim(run, suffix, classification) {
  const matches = run.claims.filter((item) => item.id.endsWith(suffix) && item.classification === classification);
  requireContract(matches.length === 1, `${run.input_path} must contain one ${classification} ${suffix} claim`);
  return matches[0];
}


function validateRun(run, index, fileHashes) {
  const scope = `runs[${index}]`;
  requireContract(isObject(run), `${scope} must be an object`);
  requireContract(boundedText(run.input_path, 4096), `${scope}.input_path is invalid`);
  requireContract(run.report_schema_variant === "current", `${scope} is not current-schema evidence`);
  requireContract(run.overall_classification === "Observed", `${scope} is not an Observed evidence set`);
  const counts = validateClaimSet(run.claims, run.issues, scope);
  for (const classification of ["Rejected", "Blocked", "Planned"]) {
    requireContract(counts.claimCounts[classification] === 0, `${scope} contains non-publishable claims`);
    requireContract(counts.issueCounts[classification] === 0, `${scope} contains non-publishable issues`);
  }

  const observations = run.raw_observations;
  requireContract(isObject(observations), `${scope}.raw_observations must be an object`);
  for (const field of ["run_identity", "world_edit_store", "viewport", "route", "route_frame_times", "planetary_streaming", "screenshots"]) {
    requireContract(isObject(observations[field]), `${scope}.${field} is required`);
  }
  const identity = observations.run_identity;
  requireContract(boundedText(identity.package_version, 160), `${scope} package_version is missing`);
  requireContract(["debug", "release"].includes(identity.build_profile), `${scope} build_profile is invalid`);
  requireContract(["V1", "V2", "V3"].includes(identity.terrain_grammar), `${scope} terrain grammar is invalid`);

  const editStore = observations.world_edit_store;
  requireContract(editStore.world_edit_store_status === "compatible" && editStore.world_edit_store_compatible === true, `${scope} world edit store is not compatible`);
  requireContract(isUInt(editStore.world_edit_store_edited_chunks), `${scope} world edit-store chunk count is invalid`);
  requireContract(editStore.world_edit_store_block_reason_code == null, `${scope} compatible world edit store has a block reason`);
  for (const [field, identityField] of [
    ["world_edit_store_seed", "world_seed"],
    ["world_edit_store_profile", "world_profile"],
    ["world_edit_store_scenery_quality", "scenery_quality"],
    ["world_edit_store_terrain_grammar", "terrain_grammar"],
  ]) requireContract(editStore[field] === identity[identityField], `${scope} world edit-store identity contradicts run identity`);

  const viewport = observations.viewport;
  for (const field of ["logical_width", "logical_height", "scale_factor", "base_scale_factor", "dpi_percent"]) {
    requireContract(isFiniteNumber(viewport[field], true), `${scope} viewport ${field} is invalid`);
  }
  for (const field of ["physical_width", "physical_height"]) {
    requireContract(isUInt(viewport[field]) && viewport[field] > 0, `${scope} viewport ${field} is invalid`);
  }
  requireContract(
    Math.abs(viewport.logical_width * viewport.scale_factor - viewport.physical_width) <= 1.5
      && Math.abs(viewport.logical_height * viewport.scale_factor - viewport.physical_height) <= 1.5,
    `${scope} viewport effective scale is inconsistent`,
  );
  requireContract(
    Math.abs(viewport.base_scale_factor * 100 - viewport.dpi_percent) <= 0.01,
    `${scope} viewport base scale and DPI are inconsistent`,
  );

  const route = observations.route;
  const supportedFocuses = new Set(["scenic", "waypoint", "streaming", "river", "lava", "near-far"]);
  requireContract(supportedFocuses.has(route.requested_route_focus), `${scope} requested route focus is invalid`);
  requireContract(supportedFocuses.has(route.resolved_route_focus), `${scope} resolved route focus is invalid`);
  requireContract(route.route_focus_available === true && route.requested_route_focus === route.resolved_route_focus && route.route_focus_unavailable_reason == null && route.route_focus_search_cap_exhausted === false, `${scope} does not contain an available requested route`);
  requireContract(route.route_focus_anchor == null || (Array.isArray(route.route_focus_anchor) && route.route_focus_anchor.length === 3 && route.route_focus_anchor.every(Number.isInteger)), `${scope} route anchor is invalid`);
  const anchoredFocuses = new Set(["waypoint", "river", "lava", "near-far"]);
  const compatibleWorldProfiles = {
    waypoint: new Set(["AstralFrontier"]),
    river: new Set(["Natural"]),
    lava: new Set(["AstralFrontier"]),
    "near-far": new Set(["Natural", "AstralFrontier"]),
  };
  if (anchoredFocuses.has(route.requested_route_focus)) requireContract(route.route_focus_anchor != null, `${scope} available route focus is missing its anchor`);
  const expectedProfiles = compatibleWorldProfiles[route.requested_route_focus];
  if (expectedProfiles) requireContract(expectedProfiles.has(identity.world_profile), `${scope} route focus is incompatible with its world profile`);
  for (const field of ["route_focus_search_visited_candidates", "route_focus_classification_queries"]) requireContract(route[field] == null || isUInt(route[field]), `${scope} route ${field} is invalid`);
  for (const field of ["route_focus_search_candidate_cap", "route_focus_classification_query_cap"]) requireContract(isUInt(route[field]), `${scope} route ${field} is invalid`);
  if (route.route_focus_search_visited_candidates != null) requireContract(route.route_focus_search_visited_candidates <= route.route_focus_search_candidate_cap, `${scope} route candidate work exceeds its cap`);
  if (route.route_focus_classification_queries != null) requireContract(route.route_focus_classification_queries <= route.route_focus_classification_query_cap, `${scope} route classification work exceeds its cap`);
  requireContract(route.camera_route_policy === "preflight-v1", `${scope} camera policy is not preflight-v1`);
  const applicable = route.camera_route_preflight_applicable;
  requireContract(typeof applicable === "boolean", `${scope} camera applicability is invalid`);
  requireContract(applicable === new Set(["river", "lava", "near-far"]).has(route.requested_route_focus), `${scope} camera applicability contradicts focus`);
  const cameraCounters = ["camera_route_variant_count", "camera_route_validation_samples", "camera_route_voxel_queries", "camera_route_voxel_query_cap", "camera_route_required_chunk_checks", "camera_route_loaded_chunk_checks", "camera_route_proven_air_chunk_checks", "camera_route_unloaded_chunk_checks", "camera_route_candidate_body_occlusions", "camera_route_candidate_los_occlusions", "camera_route_selected_clear_samples"];
  if (applicable) {
    requireContract(route.camera_route_available === true && route.camera_route_unavailable_reason == null, `${scope} camera route is unavailable`);
    requireContract(typeof route.camera_route_plan_hash === "string" && /^[0-9a-f]{16}$/.test(route.camera_route_plan_hash), `${scope} camera plan hash is invalid`);
    requireContract(route.camera_route_variant_count === 8 && route.camera_route_validation_samples === 16, `${scope} camera validation contract is invalid`);
    requireContract(route.camera_route_voxel_query_cap === 153600, `${scope} camera query cap is invalid`);
    requireContract(isUInt(route.camera_route_variant_index) && route.camera_route_variant_index < 8, `${scope} selected camera variant is invalid`);
    requireContract(isUInt(route.camera_route_voxel_queries) && route.camera_route_voxel_queries > 0 && route.camera_route_voxel_queries < 153600, `${scope} camera query work is invalid`);
    const chunkCounters = ["camera_route_required_chunk_checks", "camera_route_loaded_chunk_checks", "camera_route_proven_air_chunk_checks", "camera_route_unloaded_chunk_checks"];
    requireContract(chunkCounters.every(field => Number.isSafeInteger(route[field]) && route[field] >= 0), `${scope} camera chunk-check counters are invalid`);
    requireContract(route.camera_route_required_chunk_checks === route.camera_route_voxel_queries, `${scope} camera query/required chunk-check accounting is inconsistent`);
    let remainingCameraChecks = route.camera_route_required_chunk_checks;
    requireContract(route.camera_route_loaded_chunk_checks <= remainingCameraChecks, `${scope} camera loaded chunk-check accounting is inconsistent`);
    remainingCameraChecks -= route.camera_route_loaded_chunk_checks;
    requireContract(route.camera_route_proven_air_chunk_checks <= remainingCameraChecks, `${scope} camera proven-air chunk-check accounting is inconsistent`);
    remainingCameraChecks -= route.camera_route_proven_air_chunk_checks;
    requireContract(route.camera_route_unloaded_chunk_checks === remainingCameraChecks, `${scope} camera unloaded chunk-check accounting is inconsistent`);
    requireContract(route.camera_route_unloaded_chunk_checks === 0, `${scope} camera route has unloaded chunk checks`);
    for (const field of ["camera_route_candidate_body_occlusions", "camera_route_candidate_los_occlusions"]) requireContract(isUInt(route[field]) && route[field] <= 128, `${scope} camera candidate diagnostic is invalid`);
    requireContract(route.camera_route_selected_clear_samples === 16, `${scope} selected camera plan is not fully clear`);
    requireContract(isUInt(route.camera_route_minimum_clearance_voxels) && route.camera_route_minimum_clearance_voxels > 0, `${scope} camera route lacks positive clearance`);
  } else {
    requireContract(route.camera_route_available === false && route.camera_route_unavailable_reason == null, `${scope} non-applicable camera state is invalid`);
    requireContract(route.camera_route_plan_hash == null && route.camera_route_variant_index == null, `${scope} non-applicable camera state has a plan`);
    requireContract(cameraCounters.every(field => route[field] === 0), `${scope} non-applicable camera counters are not zero`);
    requireContract(route.camera_route_minimum_clearance_voxels == null, `${scope} non-applicable camera state has clearance`);
  }
  requireContract(route.camera_route_work_cap_exhausted === false, `${scope} camera route exhausted its work cap`);
  for (const field of [
    "requested_route_distance_m", "max_horizontal_displacement_m", "requested_duration_seconds",
    "duration_seconds", "warmup_seconds", "write_tail_seconds", "frames", "average_fps",
    "max_frame_ms", "final_smoothed_fps",
  ]) requireContract(isFiniteNumber(route[field]), `${scope} route ${field} is invalid`);

  const frame = observations.route_frame_times;
  requireContract(frame.measurement_valid === true, `${scope} frame-time measurement is invalid`);
  requireContract(frame.quantiles_complete === true, `${scope} frame-time quantiles are incomplete`);
  requireContract(isUInt(frame.sample_count) && frame.sample_count > 0, `${scope} has no route samples`);
  for (const field of ["mean_ms", "median_ms", "p95_ms", "p99_ms", "max_ms"]) {
    requireContract(isFiniteNumber(frame[field]), `${scope} frame-time ${field} is invalid`);
  }

  const planetary = observations.planetary_streaming;
  for (const group of ["live", "budgets", "telemetry"]) requireContract(isObject(planetary[group]), `${scope} planetary ${group} is missing`);
  const live = planetary.live;
  const budgets = planetary.budgets;
  const telemetry = planetary.telemetry;
  requireContract(live.enabled === true, `${scope} planetary streaming is disabled`);
  requireContract(["Natural", "AstralFrontier"].includes(live.profile), `${scope} planetary profile is invalid`);
  requireContract(live.profile === identity.world_profile, `${scope} planetary profile contradicts run identity`);
  requireContract(telemetry.desired_terrain_grammar === identity.terrain_grammar && telemetry.active_terrain_grammar === identity.terrain_grammar, `${scope} far-field grammar contradicts run identity`);
  for (const field of ["resident_entities", "resident_vertices", "resident_indices", "resident_mesh_bytes", "resident_fluid_entities", "resident_fluid_vertices", "resident_fluid_indices", "resident_fluid_mesh_bytes", "resident_water_indices", "resident_lava_indices", "resident_semantic_cohort_entities", "resident_semantic_cohort_vertices", "resident_semantic_cohort_indices", "resident_semantic_cohort_mesh_bytes", "resident_semantic_cohort_count", "live_sample_cache_windows", "live_sample_cache_bytes"]) {
    requireContract(isUInt(live[field]), `${scope} planetary live ${field} is invalid`);
  }
  for (const field of ["budget_entities", "budget_vertices", "budget_indices", "budget_mesh_bytes", "budget_sample_cache_bytes", "budget_fluid_entities", "budget_fluid_vertices", "budget_fluid_indices", "budget_fluid_mesh_bytes", "budget_hydro_atomic_ring_build_bytes", "budget_atomic_ring_build_bytes", "budget_semantic_cohort_entities", "budget_semantic_cohort_vertices", "budget_semantic_cohort_indices", "budget_semantic_cohort_mesh_bytes", "budget_semantic_cohort_hash_scans", "budget_semantic_cohort_height_queries", "budget_semantic_cohort_biome_queries"]) {
    requireContract(isUInt(budgets[field]), `${scope} planetary budget ${field} is invalid`);
  }
  for (const field of ["ring_vertices", "ring_indices", "fluid_ring_vertices", "fluid_ring_indices", "water_ring_indices", "lava_ring_indices"]) {
    requireContract(Array.isArray(live[field]) && live[field].length === 6 && live[field].every(isUInt), `${scope} planetary ${field} is not a six-level population`);
  }
  requireContract(["LegacyPalette", "BridgeV1", "BridgeV2"].includes(telemetry.surface_material_mode), `${scope} surface material mode is invalid`);
  requireContract(["Disabled", "DescriptiveV1"].includes(telemetry.hydro_mode), `${scope} hydro mode is invalid`);
  requireContract(["Disabled", "SilhouettesV1"].includes(telemetry.semantic_cohort_mode), `${scope} semantic cohort mode is invalid`);
  for (const field of ["resident_fluid_observation_valid", "resident_fluid_kind_integrity_valid", "resident_semantic_cohort_observation_valid", "resident_semantic_cohort_payload_integrity_valid"]) requireContract(telemetry[field] === true, `${scope} planetary ${field} is invalid`);
  for (const field of ["resident_fluid_entity_count_overflow", "resident_fluid_scheduler_mismatch", "resident_fluid_budget_exceeded", "resident_semantic_cohort_entity_count_overflow", "resident_semantic_cohort_scheduler_mismatch", "resident_semantic_cohort_budget_exceeded"]) requireContract(telemetry[field] === false, `${scope} planetary ${field} is invalid`);
  for (const field of ["last_fluid_indices", "last_water_indices", "last_lava_indices", "scheduler_resident_water_indices", "scheduler_resident_lava_indices", "scheduler_resident_semantic_cohort_entities", "scheduler_resident_semantic_cohort_vertices", "scheduler_resident_semantic_cohort_indices", "scheduler_resident_semantic_cohort_mesh_bytes", "scheduler_resident_semantic_cohort_count", "last_semantic_cohort_candidates", "last_semantic_cohort_emitted", "last_semantic_cohort_vertices", "last_semantic_cohort_indices"]) requireContract(isUInt(telemetry[field]), `${scope} planetary ${field} is invalid`);
  requireContract(Array.isArray(live.resident_semantic_cohort_kind_counts) && live.resident_semantic_cohort_kind_counts.length === 6 && live.resident_semantic_cohort_kind_counts.every(isUInt), `${scope} planetary cohort kinds are invalid`);
  for (const field of ["scheduler_water_ring_indices", "scheduler_lava_ring_indices", "scheduler_resident_semantic_cohort_kind_counts", "last_semantic_cohort_kind_counts"]) requireContract(Array.isArray(telemetry[field]) && telemetry[field].length === 6 && telemetry[field].every(isUInt), `${scope} planetary ${field} is invalid`);
  requireContract(live.resident_water_indices + live.resident_lava_indices === live.resident_fluid_indices && live.resident_water_indices % 6 === 0 && live.resident_lava_indices % 6 === 0, `${scope} Hydro kind totals disagree`);
  requireContract(telemetry.last_water_indices + telemetry.last_lava_indices === telemetry.last_fluid_indices && telemetry.last_water_indices % 6 === 0 && telemetry.last_lava_indices % 6 === 0, `${scope} latest Hydro kind totals disagree`);
  for (let index = 0; index < 6; index += 1) requireContract(live.water_ring_indices[index] + live.lava_ring_indices[index] === live.fluid_ring_indices[index] && live.water_ring_indices[index] % 6 === 0 && live.lava_ring_indices[index] % 6 === 0, `${scope} Hydro ring kinds disagree`);
  const cohortCount = live.resident_semantic_cohort_count;
  requireContract(live.resident_semantic_cohort_kind_counts.reduce((total, value) => total + value, 0) === cohortCount, `${scope} cohort kinds disagree`);
  requireContract(live.resident_semantic_cohort_vertices === cohortCount * 24 && live.resident_semantic_cohort_indices === cohortCount * 36 && live.resident_semantic_cohort_mesh_bytes === cohortCount * (24 * 48 + 36 * 4), `${scope} cohort payload disagrees`);
  requireContract(live.resident_semantic_cohort_entities === Number(cohortCount > 0) && cohortCount <= 81, `${scope} cohort entity/count population disagrees`);
  if (live.profile === "Natural") {
    requireContract(!live.resident_semantic_cohort_kind_counts.slice(3).some(Boolean), `${scope} Natural profile contains Astral cohort kinds`);
    requireContract(!telemetry.last_semantic_cohort_kind_counts.slice(3).some(Boolean), `${scope} Natural latest work contains Astral cohort kinds`);
  } else {
    requireContract(!live.resident_semantic_cohort_kind_counts.slice(0, 3).some(Boolean), `${scope} Astral profile contains Natural cohort kinds`);
    requireContract(!telemetry.last_semantic_cohort_kind_counts.slice(0, 3).some(Boolean), `${scope} Astral latest work contains Natural cohort kinds`);
  }
  requireContract(budgets.budget_hydro_atomic_ring_build_bytes === 653008 && budgets.budget_atomic_ring_build_bytes === 757984, `${scope} atomic byte budgets changed`);
  const expectedCohortBudgets = { budget_semantic_cohort_entities: 1, budget_semantic_cohort_vertices: 1944, budget_semantic_cohort_indices: 2916, budget_semantic_cohort_mesh_bytes: 104976, budget_semantic_cohort_hash_scans: 3721, budget_semantic_cohort_height_queries: 81, budget_semantic_cohort_biome_queries: 81 };
  for (const [field, expected] of Object.entries(expectedCohortBudgets)) requireContract(budgets[field] === expected, `${scope} planetary ${field} changed`);
  for (const [liveField, telemetryField] of [["resident_water_indices", "scheduler_resident_water_indices"], ["resident_lava_indices", "scheduler_resident_lava_indices"], ["water_ring_indices", "scheduler_water_ring_indices"], ["lava_ring_indices", "scheduler_lava_ring_indices"], ["resident_semantic_cohort_entities", "scheduler_resident_semantic_cohort_entities"], ["resident_semantic_cohort_vertices", "scheduler_resident_semantic_cohort_vertices"], ["resident_semantic_cohort_indices", "scheduler_resident_semantic_cohort_indices"], ["resident_semantic_cohort_mesh_bytes", "scheduler_resident_semantic_cohort_mesh_bytes"], ["resident_semantic_cohort_count", "scheduler_resident_semantic_cohort_count"], ["resident_semantic_cohort_kind_counts", "scheduler_resident_semantic_cohort_kind_counts"]]) requireContract(JSON.stringify(live[liveField]) === JSON.stringify(telemetry[telemetryField]), `${scope} scheduler ${telemetryField} disagrees`);
  for (const [liveField, budgetField] of [["resident_semantic_cohort_entities", "budget_semantic_cohort_entities"], ["resident_semantic_cohort_vertices", "budget_semantic_cohort_vertices"], ["resident_semantic_cohort_indices", "budget_semantic_cohort_indices"], ["resident_semantic_cohort_mesh_bytes", "budget_semantic_cohort_mesh_bytes"]]) requireContract(live[liveField] <= budgets[budgetField], `${scope} ${liveField} exceeds budget`);
  for (const field of ["desired_material_detail", "resident_material_detail"]) {
    requireContract(Array.isArray(telemetry[field]) && telemetry[field].length === 6, `${scope} ${field} is not a six-level state`);
  }
  for (const field of ["last_build_ms", "max_build_ms"]) requireContract(isFiniteNumber(telemetry[field]), `${scope} planetary ${field} is invalid`);
  for (const field of ["last_material_slope_queries", "last_bridge_v2_cell_reuses", "peak_live_sample_cache_windows", "peak_live_sample_cache_bytes"]) {
    requireContract(isUInt(telemetry[field]), `${scope} planetary ${field} is invalid`);
  }

  const screenshots = observations.screenshots;
  requireContract(Array.isArray(screenshots.referenced_files) && screenshots.referenced_files.length > 0 && screenshots.referenced_files.length <= MAX_SCREENSHOTS_PER_RUN, `${scope} must contain bounded referenced screenshots`);
  requireContract(Array.isArray(screenshots.actual_files) && screenshots.actual_files.length <= MAX_SCREENSHOTS_PER_RUN, `${scope} actual screenshots are invalid`);
  const actualByPath = new Map();
  screenshots.actual_files.forEach((record, shotIndex) => {
    requireContract(isObject(record), `${scope} actual screenshot ${shotIndex} is invalid`);
    requireContract(boundedText(record.path, 4096), `${scope} screenshot path is invalid`);
    requireContract(!actualByPath.has(record.path), `${scope} has a duplicate screenshot path`);
    requireContract(record.classification === "Passed" && record.png_complete === true, `${scope} screenshot is not Passed and complete`);
    requireContract(SHA256_RE.test(record.sha256), `${scope} screenshot hash is invalid`);
    requireContract(isUInt(record.size_bytes), `${scope} screenshot size is invalid`);
    actualByPath.set(record.path, record);
  });
  for (const screenshotPath of screenshots.referenced_files) {
    requireContract(boundedText(screenshotPath, 4096), `${scope} referenced screenshot path is invalid`);
    requireContract(actualByPath.has(screenshotPath), `${scope} referenced screenshot is absent from actual_files`);
    const actualRecord = actualByPath.get(screenshotPath);
    const hashRecord = fileHashes.get(`screenshot\0${screenshotPath}`);
    requireContract(hashRecord, `${scope} screenshot is absent from file_hashes`);
    requireContract(hashRecord.sha256 === actualRecord.sha256 && hashRecord.size_bytes === actualRecord.size_bytes, `${scope} screenshot hash records disagree`);
  }

  const reportClaim = requireClaim(run, ":report_integrity", "Passed");
  requireContract(reportClaim.evidence.length === 1, `${scope} report claim must cite one report`);
  requireContract(fileHashes.has(`report\0${reportClaim.evidence[0]}`), `${scope} report hash is missing`);
  const screenshotClaim = requireClaim(run, ":screenshot_integrity", "Passed");
  requireContract(JSON.stringify([...screenshotClaim.evidence].sort()) === JSON.stringify([...screenshots.referenced_files].sort()), `${scope} screenshot claim disagrees`);
  requireClaim(run, ":planetary_budgets", "Passed");
  return counts;
}


export async function loadCanonicalEvidence(inputPath) {
  const manifestPath = path.resolve(inputPath);
  requireContract(path.extname(manifestPath).toLowerCase() === ".json", "evidence manifest must have a .json suffix");
  let payload;
  try {
    const stat = await fs.stat(manifestPath);
    requireContract(stat.isFile() && stat.size > 0 && stat.size <= MAX_MANIFEST_BYTES, "evidence manifest violates the fixed byte cap");
    payload = await fs.readFile(manifestPath);
  } catch (error) {
    if (error instanceof EvidenceContractError) throw error;
    throw new EvidenceContractError(`evidence manifest is not readable: ${error.message}`);
  }
  let data;
  try {
    data = JSON.parse(payload.toString("utf8"));
  } catch (error) {
    throw new EvidenceContractError(`evidence manifest is not strict UTF-8 JSON: ${error.message}`);
  }
  requireContract(isObject(data), "evidence manifest root must be an object");
  requireContract(data.schema_version === SCHEMA_VERSION, "unsupported evidence manifest schema_version");
  requireContract(JSON.stringify(data.claim_classifications) === JSON.stringify(CLASSIFICATIONS), "claim classification contract changed");
  requireContract(data.overall_classification === "Observed", "manifest is not an Observed evidence set");
  requireContract(boundedText(data.generated_at_utc, 64) && data.generated_at_utc.endsWith("Z") && Number.isFinite(Date.parse(data.generated_at_utc)), "generated_at_utc is invalid");

  const generator = data.generator;
  requireContract(isObject(generator) && generator.name === GENERATOR_NAME && generator.version === GENERATOR_VERSION, "unexpected manifest generator");
  requireContract(repositoryRelativePath(generator.source_path) && SHA256_RE.test(generator.source_sha256), "generator source identity is invalid");

  const inputs = data.inputs;
  requireContract(isObject(inputs) && inputs.selection_policy === SELECTION_POLICY, "manifest was not built from explicit runs");
  requireContract(isUInt(inputs.argument_count) && isUInt(inputs.accepted_run_count), "manifest input counts are invalid");
  requireContract(Array.isArray(inputs.qa_run_directories) && inputs.qa_run_directories.length <= MAX_RUNS && inputs.qa_run_directories.every((entry) => repositoryRelativePath(entry)), "manifest run directory list is invalid");

  requireContract(Array.isArray(data.file_hashes) && data.file_hashes.length <= MAX_FILE_HASHES, "manifest file_hashes exceed the fixed cap");
  const fileHashes = new Map();
  data.file_hashes.forEach((record, index) => {
    requireContract(isObject(record), `file_hashes[${index}] must be an object`);
    requireContract(["report", "screenshot", "generator_source"].includes(record.kind), `file_hashes[${index}].kind is invalid`);
    requireContract(repositoryRelativePath(record.path) && SHA256_RE.test(record.sha256) && isUInt(record.size_bytes), `file_hashes[${index}] identity is invalid`);
    const key = `${record.kind}\0${record.path}`;
    requireContract(!fileHashes.has(key), `duplicate file hash record: ${key}`);
    fileHashes.set(key, record);
  });
  const sourceRecord = fileHashes.get(`generator_source\0${generator.source_path}`);
  requireContract(sourceRecord && sourceRecord.sha256 === generator.source_sha256, "generator source hash record is missing or inconsistent");

  const topCounts = validateClaimSet(data.claims, data.issues, "manifest");
  for (const classification of ["Rejected", "Blocked", "Planned"]) {
    requireContract(topCounts.claimCounts[classification] === 0 && topCounts.issueCounts[classification] === 0, "manifest contains non-publishable claims or issues");
  }

  requireContract(Array.isArray(data.runs) && data.runs.length > 0 && data.runs.length <= MAX_RUNS, "manifest must contain bounded current runs");
  requireContract(data.runs.length === inputs.qa_run_directories.length && inputs.accepted_run_count === data.runs.length && inputs.argument_count >= data.runs.length, "manifest run counts disagree");
  const claimCounts = { ...topCounts.claimCounts };
  const issueCounts = { ...topCounts.issueCounts };
  const paths = [];
  data.runs.forEach((run, index) => {
    const counts = validateRun(run, index, fileHashes);
    addCounts(claimCounts, counts.claimCounts);
    addCounts(issueCounts, counts.issueCounts);
    paths.push(run.input_path);
  });
  requireContract(JSON.stringify(paths) === JSON.stringify(inputs.qa_run_directories), "run order or paths disagree with manifest inputs");
  requireContract(new Set(paths).size === paths.length, "manifest contains duplicate runs");

  const summary = data.summary;
  requireContract(isObject(summary), "manifest summary is missing");
  requireContract(summary.run_count === data.runs.length && summary.file_hash_count === data.file_hashes.length, "manifest summary counts disagree");
  requireContract(countsEqual(summary.claim_counts, claimCounts), "summary.claim_counts disagrees");
  requireContract(countsEqual(summary.issue_counts, issueCounts), "summary.issue_counts disagrees");
  return {
    data,
    generatedAt: new Date(data.generated_at_utc),
    manifestPath,
    manifestSha256: crypto.createHash("sha256").update(payload).digest("hex"),
    manifestSizeBytes: payload.length,
  };
}


export async function validateOutputPath(outputPath, repoRoot, suffix) {
  const root = path.resolve(repoRoot);
  const destination = path.resolve(outputPath);
  requireContract(path.extname(destination).toLowerCase() === suffix.toLowerCase(), `output must have a ${suffix} suffix`);
  try {
    await fs.access(destination);
    throw new EvidenceContractError("output already exists; choose a new explicit path");
  } catch (error) {
    if (error instanceof EvidenceContractError) throw error;
    if (error.code !== "ENOENT") throw new EvidenceContractError(`output path is not safely inspectable: ${error.message}`);
  }
  for (const directory of PROTECTED_OUTPUT_DIRS) {
    const relative = path.relative(path.join(root, directory), destination);
    requireContract(relative.startsWith("..") || path.isAbsolute(relative), `output must not be inside protected directory '${directory}'`);
  }
  return destination;
}


function resolveEvidencePath(displayPath, repoRoot) {
  const normalized = displayPath.replace(/[\\/]/gu, path.sep);
  requireContract(!normalized.split(path.sep).includes(".."), "evidence path contains parent traversal");
  return path.resolve(path.isAbsolute(normalized) ? normalized : path.join(repoRoot, normalized));
}


async function hashAndProbePng(filePath, expectedSize) {
  requireContract(expectedSize <= MAX_SCREENSHOT_BYTES, "screenshot exceeds the artifact byte cap");
  let payload;
  try {
    payload = await fs.readFile(filePath);
  } catch (error) {
    throw new EvidenceContractError(`screenshot is not readable: ${filePath}: ${error.message}`);
  }
  requireContract(payload.length === expectedSize, `screenshot size changed after manifest generation: ${filePath}`);
  requireContract(payload.length <= MAX_SCREENSHOT_BYTES, "screenshot exceeds the artifact byte cap");
  requireContract(payload.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE) && payload.subarray(-PNG_IEND.length).equals(PNG_IEND), `screenshot is no longer a complete PNG: ${filePath}`);
  return crypto.createHash("sha256").update(payload).digest("hex");
}


export async function verifiedScreenshots(evidence, repoRoot, limit = MAX_EMBEDDED_SCREENSHOTS) {
  requireContract(Number.isInteger(limit) && limit > 0 && limit <= MAX_EMBEDDED_SCREENSHOTS, "screenshot selection limit is invalid");
  const primary = [];
  const extras = [];
  for (const run of evidence.data.runs) {
    const screenshots = run.raw_observations.screenshots;
    const records = new Map(screenshots.actual_files.map((record) => [record.path, record]));
    primary.push({ run, display: screenshots.referenced_files[0], record: records.get(screenshots.referenced_files[0]) });
    extras.push(...screenshots.referenced_files.slice(1).map((display) => ({ run, display, record: records.get(display) })));
  }
  const selected = [...primary, ...extras].slice(0, limit);
  let totalBytes = 0;
  const output = [];
  for (const item of selected) {
    totalBytes += item.record.size_bytes;
    requireContract(totalBytes <= MAX_EMBEDDED_SCREENSHOT_BYTES, "selected screenshots exceed the total byte cap");
    const resolved = resolveEvidencePath(item.display, path.resolve(repoRoot));
    const runRoot = resolveEvidencePath(item.run.input_path, path.resolve(repoRoot));
    const relative = path.relative(runRoot, resolved);
    requireContract(!relative.startsWith("..") && !path.isAbsolute(relative), `screenshot no longer resolves inside its explicit run: ${item.display}`);
    const digest = await hashAndProbePng(resolved, item.record.size_bytes);
    requireContract(digest === item.record.sha256, `screenshot hash changed after manifest generation: ${item.display}`);
    output.push({ ...item, resolved });
  }
  return output;
}


export function validationSummary(evidence, output) {
  return {
    evidence_manifest: evidence.manifestPath,
    manifest_sha256: evidence.manifestSha256,
    output,
    overall_classification: evidence.data.overall_classification,
    run_count: evidence.data.runs.length,
    schema_version: evidence.data.schema_version,
  };
}


export async function publishNoClobber(temporary, destination) {
  try {
    await fs.link(temporary, destination);
  } catch (error) {
    if (error.code === "EEXIST") throw new EvidenceContractError("output appeared during publication; nothing was replaced");
    throw error;
  } finally {
    try { await fs.unlink(temporary); } catch { /* exact task-owned temporary only */ }
  }
}
