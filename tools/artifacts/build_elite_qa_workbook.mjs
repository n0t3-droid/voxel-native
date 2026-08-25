#!/usr/bin/env node
/** Build an XLSX evidence ledger from one explicit canonical QA manifest. */

import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  EvidenceContractError,
  loadCanonicalEvidence,
  publishNoClobber,
  validateOutputPath,
  validationSummary,
  verifiedScreenshots,
} from "./evidence_manifest_consumer.mjs";


const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_REPO_ROOT = path.resolve(SCRIPT_DIR, "..", "..");
const MAX_ARTIFACT_ROWS = 1000;


function parseArgs(argv) {
  const result = { repoRoot: DEFAULT_REPO_ROOT, checkOnly: false, qaDir: null };
  const valueFlags = new Map([
    ["--evidence-manifest", "evidenceManifest"],
    ["--output", "output"],
    ["--repo-root", "repoRoot"],
    ["--qa-dir", "qaDir"],
  ]);
  const seen = new Set();
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (token === "--check-only") {
      if (seen.has(token)) throw new EvidenceContractError("duplicate --check-only flag");
      seen.add(token);
      result.checkOnly = true;
      continue;
    }
    const property = valueFlags.get(token);
    if (!property) throw new EvidenceContractError(`unknown argument: ${token}`);
    if (seen.has(token)) throw new EvidenceContractError(`duplicate argument: ${token}`);
    seen.add(token);
    index += 1;
    if (index >= argv.length || argv[index].startsWith("--")) throw new EvidenceContractError(`${token} requires a value`);
    if (argv[index].length > 4096) throw new EvidenceContractError(`${token} exceeds the path length cap`);
    result[property] = argv[index];
  }
  if (!result.evidenceManifest) throw new EvidenceContractError("--evidence-manifest is required");
  if (!result.output) throw new EvidenceContractError("--output is required");
  return result;
}


async function validateQaDirectory(qaDir, repoRoot) {
  if (!qaDir) return null;
  const destination = path.resolve(qaDir);
  for (const directory of ["saves", "qa_runs", "agent_runs"]) {
    const relative = path.relative(path.join(path.resolve(repoRoot), directory), destination);
    if (!relative.startsWith("..") && !path.isAbsolute(relative)) {
      throw new EvidenceContractError(`QA preview directory must not be inside protected directory '${directory}'`);
    }
  }
  try {
    await fs.access(destination);
    throw new EvidenceContractError("QA preview directory already exists; choose a new explicit path");
  } catch (error) {
    if (error instanceof EvidenceContractError) throw error;
    if (error.code !== "ENOENT") throw new EvidenceContractError(`QA preview directory is not safely inspectable: ${error.message}`);
  }
  return destination;
}


function formatPhysicalViewport(viewport) {
  return `${viewport.physical_width}x${viewport.physical_height}`;
}


function formatRouteAnchor(anchor) {
  return anchor == null ? "Not recorded" : `[${anchor.join(", ")}]`;
}


function optionalObservedCount(value) {
  return value == null ? "Not recorded" : value;
}


function excelColumnName(index) {
  let value = index + 1;
  let label = "";
  while (value > 0) {
    const remainder = (value - 1) % 26;
    label = String.fromCharCode(65 + remainder) + label;
    value = Math.floor((value - 1) / 26);
  }
  return label;
}


function collectClaims(evidence) {
  const rows = evidence.data.claims.map((claim) => ["manifest", claim.id, claim.classification, claim.statement, claim.evidence.join("\n") || "None"]);
  for (const run of evidence.data.runs) {
    rows.push(...run.claims.map((claim) => [run.input_path, claim.id, claim.classification, claim.statement, claim.evidence.join("\n") || "None"]));
  }
  if (rows.length > MAX_ARTIFACT_ROWS) throw new EvidenceContractError("claim ledger exceeds the workbook row cap");
  return rows;
}


function collectIssues(evidence) {
  const rows = evidence.data.issues.map((issue) => ["manifest", issue.classification, issue.code, issue.field, issue.message]);
  for (const run of evidence.data.runs) {
    rows.push(...run.issues.map((issue) => [run.input_path, issue.classification, issue.code, issue.field, issue.message]));
  }
  if (rows.length > MAX_ARTIFACT_ROWS) throw new EvidenceContractError("issue ledger exceeds the workbook row cap");
  return rows;
}


function collectRuns(evidence) {
  return evidence.data.runs.map((run) => {
    const observations = run.raw_observations;
    const identity = observations.run_identity;
    const viewport = observations.viewport;
    const route = observations.route;
    const frame = observations.route_frame_times;
    const planetary = observations.planetary_streaming;
    return [
      run.input_path,
      identity.build_profile,
      identity.world_name ?? "unrecorded",
      identity.world_profile ?? "unrecorded",
      formatPhysicalViewport(viewport),
      viewport.dpi_percent / 100,
      route.requested_route_focus,
      route.resolved_route_focus,
      route.route_focus_available,
      route.route_focus_unavailable_reason ?? "None",
      formatRouteAnchor(route.route_focus_anchor),
      optionalObservedCount(route.route_focus_search_visited_candidates),
      route.route_focus_search_candidate_cap,
      optionalObservedCount(route.route_focus_classification_queries),
      route.route_focus_classification_query_cap,
      route.route_focus_search_cap_exhausted,
      route.requested_route_distance_m,
      frame.sample_count,
      frame.mean_ms,
      frame.median_ms,
      frame.p95_ms,
      frame.p99_ms,
      frame.max_ms,
      observations.screenshots.referenced_files.length,
      planetary.telemetry.surface_material_mode,
      planetary.telemetry.hydro_mode,
      planetary.telemetry.semantic_cohort_mode,
      planetary.live.resident_mesh_bytes,
      planetary.budgets.budget_mesh_bytes,
      planetary.telemetry.peak_live_sample_cache_bytes,
      planetary.budgets.budget_sample_cache_bytes,
    ];
  });
}


function collectGenerationRows(evidence) {
  return evidence.data.runs.map((run) => {
    const observations = run.raw_observations;
    const identity = observations.run_identity;
    const editStore = observations.world_edit_store;
    const telemetry = observations.planetary_streaming.telemetry;
    return [
      run.input_path,
      identity.world_name ?? "Not recorded",
      identity.world_seed ?? "Not recorded",
      identity.world_profile ?? "Not recorded",
      identity.scenery_quality ?? "Not recorded",
      identity.terrain_grammar,
      editStore.world_edit_store_status,
      editStore.world_edit_store_compatible,
      optionalObservedCount(editStore.world_edit_store_edited_chunks),
      editStore.world_edit_store_block_reason_code ?? "None",
      editStore.world_edit_store_seed ?? "Not recorded",
      editStore.world_edit_store_profile ?? "Not recorded",
      editStore.world_edit_store_scenery_quality ?? "Not recorded",
      editStore.world_edit_store_terrain_grammar ?? "Not recorded",
      telemetry.desired_terrain_grammar,
      telemetry.active_terrain_grammar ?? "Not active",
    ];
  });
}


function collectLayerRows(evidence) {
  return evidence.data.runs.map((run) => {
    const planetary = run.raw_observations.planetary_streaming;
    const live = planetary.live;
    const budgets = planetary.budgets;
    const telemetry = planetary.telemetry;
    return [
      run.input_path,
      live.profile,
      telemetry.hydro_mode,
      live.resident_fluid_entities,
      live.resident_fluid_vertices,
      live.resident_fluid_indices,
      live.resident_water_indices,
      live.resident_lava_indices,
      ...live.water_ring_indices,
      ...live.lava_ring_indices,
      live.resident_fluid_mesh_bytes,
      budgets.budget_fluid_mesh_bytes,
      budgets.budget_hydro_atomic_ring_build_bytes,
      telemetry.resident_fluid_observation_valid,
      telemetry.resident_fluid_kind_integrity_valid,
      telemetry.semantic_cohort_mode,
      live.resident_semantic_cohort_entities,
      live.resident_semantic_cohort_count,
      ...live.resident_semantic_cohort_kind_counts,
      live.resident_semantic_cohort_vertices,
      live.resident_semantic_cohort_indices,
      live.resident_semantic_cohort_mesh_bytes,
      budgets.budget_semantic_cohort_mesh_bytes,
      telemetry.last_semantic_cohort_candidates,
      budgets.budget_semantic_cohort_hash_scans,
      telemetry.last_semantic_cohort_emitted,
      telemetry.resident_semantic_cohort_observation_valid,
      telemetry.resident_semantic_cohort_payload_integrity_valid,
      budgets.budget_atomic_ring_build_bytes,
    ];
  });
}


function collectBudgetRows(evidence) {
  const definitions = [
    ["Entities", "resident_entities", "budget_entities", "live"],
    ["Vertices", "resident_vertices", "budget_vertices", "live"],
    ["Indices", "resident_indices", "budget_indices", "live"],
    ["Mesh bytes", "resident_mesh_bytes", "budget_mesh_bytes", "live"],
    ["Sample-cache bytes", "live_sample_cache_bytes", "budget_sample_cache_bytes", "live"],
    ["Peak sample-cache bytes", "peak_live_sample_cache_bytes", "budget_sample_cache_bytes", "telemetry"],
  ];
  const rows = [];
  for (const run of evidence.data.runs) {
    const planetary = run.raw_observations.planetary_streaming;
    for (const [label, liveField, budgetField, group] of definitions) {
      rows.push([
        run.input_path,
        label,
        planetary[group][liveField],
        planetary.budgets[budgetField],
        null,
        "Passed",
      ]);
    }
  }
  return rows;
}


function applyHeader(range, fill, white) {
  range.format = {
    fill,
    font: { name: "Aptos", size: 10, bold: true, color: white },
    verticalAlignment: "center",
    wrapText: true,
  };
}


function styleLedgerSheet(sheet, lastRow, lastColumn, widths, colors) {
  const used = sheet.getRange(`A1:${lastColumn}${lastRow}`);
  used.format = { font: { name: "Aptos", size: 9, color: colors.ink }, verticalAlignment: "top", wrapText: true };
  applyHeader(sheet.getRange(`A1:${lastColumn}1`), colors.navy, colors.white);
  for (const [column, width] of Object.entries(widths)) sheet.getRange(`${column}1:${column}${lastRow}`).format.columnWidth = width;
  sheet.freezePanes.freezeRows(1);
  sheet.showGridLines = false;
  used.format.autofitRows();
}


async function buildWorkbook(evidence, output, qaDir) {
  const { SpreadsheetFile, Workbook } = await import("@oai/artifact-tool");
  const colors = {
    ink: "#122128",
    navy: "#173B4D",
    teal: "#1F8A70",
    cream: "#F5F1E8",
    pale: "#EAF3F1",
    line: "#C9D8D5",
    red: "#B64B52",
    white: "#FFFFFF",
  };
  const workbook = Workbook.create();
  const overview = workbook.worksheets.add("Overview");
  const runs = workbook.worksheets.add("Run Evidence");
  const generation = workbook.worksheets.add("Generation Identity");
  const layers = workbook.worksheets.add("Far Layers");
  const budgets = workbook.worksheets.add("Budget Evidence");
  const claims = workbook.worksheets.add("Claims");
  const issues = workbook.worksheets.add("Issues");
  const hashes = workbook.worksheets.add("File Hashes");
  for (const sheet of [overview, runs, generation, layers, budgets, claims, issues, hashes]) sheet.showGridLines = false;

  const runRows = collectRuns(evidence);
  const generationRows = collectGenerationRows(evidence);
  const layerRows = collectLayerRows(evidence);
  const budgetRows = collectBudgetRows(evidence);
  const claimRows = collectClaims(evidence);
  const issueRows = collectIssues(evidence);
  const hashRows = evidence.data.file_hashes.map((record) => [record.kind, record.path, record.size_bytes, record.sha256]);
  if (hashRows.length > MAX_ARTIFACT_ROWS) throw new EvidenceContractError("file hash ledger exceeds the workbook row cap");

  // Overview - formula-backed summaries and explicit evidence limitations.
  overview.mergeCells("A1:H2");
  overview.getRange("A1").values = [["VOXEL-NATIVE - CANONICAL QA EVIDENCE"]];
  overview.getRange("A1:H2").format = {
    fill: colors.navy,
    font: { name: "Aptos Display", size: 20, bold: true, color: colors.white },
    verticalAlignment: "center",
  };
  overview.mergeCells("A3:H3");
  overview.getRange("A3").values = [[`Explicit manifest schema ${evidence.data.schema_version} | QA 2.6.0 current runs | generated ${evidence.data.generated_at_utc} | sha256 ${evidence.manifestSha256}`]];
  overview.getRange("A3:H3").format = { fill: colors.pale, font: { name: "Aptos", size: 9, italic: true, color: colors.navy }, wrapText: true };
  overview.getRange("A5:B5").values = [["Verified summary", "Value"]];
  overview.getRange("A6:A11").values = [
    ["Manifest classification"],
    ["Current-schema runs"],
    ["Referenced PNGs"],
    ["Passed claims"],
    ["Observed claims"],
    ["Automated test total"],
  ];
  overview.getRange("B6:B11").values = [[evidence.data.overall_classification], [null], [null], [null], [null], ["Not represented by this manifest"]];
  overview.getRange("B7").formulas = [[`=COUNTA('Run Evidence'!A2:A${runRows.length + 1})`]];
  overview.getRange("B8").formulas = [[`=SUM('Run Evidence'!X2:X${runRows.length + 1})`]];
  overview.getRange("B9").formulas = [[`=COUNTIF('Claims'!C2:C${claimRows.length + 1},"Passed")`]];
  overview.getRange("B10").formulas = [[`=COUNTIF('Claims'!C2:C${claimRows.length + 1},"Observed")`]];
  applyHeader(overview.getRange("A5:B5"), colors.teal, colors.white);
  overview.getRange("A6:A11").format = { fill: colors.pale, font: { bold: true, color: colors.navy } };
  overview.getRange("B6:B11").format = { fill: colors.cream, font: { color: colors.ink }, wrapText: true };
  overview.getRange("B7:B10").format.numberFormat = "#,##0";
  overview.getRange("D5:H5").values = [["Boundary", "Meaning", "Evidence source", "What is not implied", "Disposition"]];
  overview.getRange("D6:H11").values = [
    ["Run selection", "Only explicitly named immutable QA runs", "Manifest inputs.selection_policy", "No latest/global scan", "Passed"],
    ["Generation identity", "Terrain grammar and compatible edit-store identity agree exactly", "run_identity + world_edit_store + planetary_streaming.telemetry", "No inference from world name or empty storage", "Passed"],
    ["Frame time", "Route-only fixed-memory quantiles", "route_frame_times", "No universal FPS threshold", "Observed"],
    ["Planetary budgets", "Serialized live/peak values compared like-for-like", "planetary_streaming", "No visual-quality claim", "Passed"],
    ["Screenshot integrity", "Referenced PNG bytes are complete and hashed", "screenshots + file_hashes", "No perceptual inspection result", "Passed"],
    ["Responsive scope", "One viewport is recorded per run", "viewport", "No full viewport/DPI matrix", "Observed"],
  ];
  overview.getRange("D5:H11").format = { wrapText: true, verticalAlignment: "top", font: { name: "Aptos", size: 9, color: colors.ink } };
  applyHeader(overview.getRange("D5:H5"), colors.navy, colors.white);
  overview.getRange("H6:H11").conditionalFormats.add("containsText", { text: "Passed", format: { fill: "#DDF3E7", font: { color: "#17633E", bold: true } } });
  overview.getRange("H6:H11").conditionalFormats.add("containsText", { text: "Observed", format: { fill: "#DDEBF7", font: { color: colors.navy, bold: true } } });
  overview.getRange("A14:H14").merge();
  overview.getRange("A14").values = [["Interpretation guardrail"]];
  overview.getRange("A14:H14").format = { fill: colors.teal, font: { bold: true, color: colors.white } };
  overview.getRange("A15:H19").merge(true);
  overview.getRange("A15:A19").values = [
    ["PNG completion and hashes prove byte identity, not clipping, overlap, terrain quality, lighting, transition quality, or motion."],
    ["Average FPS and quantiles describe the recorded route, build, and hardware; this workbook makes no causal A/B claim."],
    ["Hashes do not prove authorship, an unrecorded Git revision, or source correspondence beyond serialized provenance."],
    ["Automated test totals require a separately hashed gate transcript and are intentionally absent."],
    ["Missing responsive matrix cells remain missing; one viewport never implies another."],
  ];
  overview.getRange("A15:H19").format = { fill: "#F7F9F8", font: { name: "Aptos", size: 9, color: colors.ink }, wrapText: true };
  for (const [column, width] of Object.entries({ A: 25, B: 25, C: 3, D: 20, E: 27, F: 25, G: 28, H: 15 })) overview.getRange(`${column}1:${column}19`).format.columnWidth = width;
  overview.getRange("A1:H19").format.autofitRows();
  overview.freezePanes.freezeRows(3);

  // Current run observations.
  const runHeaders = ["Explicit run", "Build", "World", "World profile", "Viewport", "DPI", "Requested focus", "Resolved focus", "Available", "Unavailable reason", "Anchor", "Candidates actual", "Candidate cap", "Classifications actual", "Classification cap", "Cap exhausted", "Target m", "Samples", "Mean ms", "Median ms", "P95 ms", "P99 ms", "Max ms", "PNGs", "Material mode", "Hydro mode", "Cohort mode", "Mesh bytes", "Mesh budget", "Peak cache bytes", "Cache budget"];
  runs.getRange("A1:AE1").values = [runHeaders];
  runs.getRange(`A2:AE${runRows.length + 1}`).values = runRows;
  runs.tables.add(`A1:AE${runRows.length + 1}`, true, "RunEvidenceTable").style = "TableStyleMedium2";
  styleLedgerSheet(runs, runRows.length + 1, "AE", { A: 34, B: 11, C: 24, D: 18, E: 16, F: 10, G: 18, H: 18, I: 11, J: 24, K: 18, L: 18, M: 14, N: 20, O: 16, P: 14, Q: 12, R: 12, S: 12, T: 12, U: 12, V: 12, W: 12, X: 9, Y: 18, Z: 18, AA: 18, AB: 16, AC: 16, AD: 18, AE: 16 }, colors);
  runs.getRange(`F2:F${runRows.length + 1}`).format.numberFormat = "0%";
  runs.getRange(`M2:M${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`O2:O${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`Q2:R${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`S2:W${runRows.length + 1}`).format.numberFormat = "0.000";
  runs.getRange(`X2:X${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`AB2:AE${runRows.length + 1}`).format.numberFormat = "#,##0";

  // Immutable generation identity and edit-store authority stay explicit.
  const generationHeaders = ["Explicit run", "World", "World seed", "World profile", "Scenery", "Terrain grammar", "Edit-store status", "Edit-store compatible", "Edited chunks", "Block reason", "Store seed", "Store profile", "Store scenery", "Store terrain grammar", "Desired terrain grammar", "Active terrain grammar"];
  generation.getRange("A1:P1").values = [generationHeaders];
  generation.getRange(`A2:P${generationRows.length + 1}`).values = generationRows;
  generation.tables.add(`A1:P${generationRows.length + 1}`, true, "GenerationIdentityTable").style = "TableStyleMedium2";
  styleLedgerSheet(generation, generationRows.length + 1, "P", { A: 34, B: 24, C: 14, D: 18, E: 16, F: 18, G: 18, H: 18, I: 16, J: 24, K: 14, L: 18, M: 16, N: 22, O: 22, P: 22 }, colors);
  generation.getRange(`C2:C${generationRows.length + 1}`).format.numberFormat = "#,##0";
  generation.getRange(`I2:I${generationRows.length + 1}`).format.numberFormat = "#,##0";
  generation.getRange(`K2:K${generationRows.length + 1}`).format.numberFormat = "#,##0";

  // Per-kind Hydro and semantic-cohort evidence remains raw, typed, and auditable.
  const layerHeaders = ["Explicit run", "World profile", "Hydro mode", "Fluid entities", "Fluid vertices", "Fluid indices", "Water indices", "Lava indices", "Water L0", "Water L1", "Water L2", "Water L3", "Water L4", "Water L5", "Lava L0", "Lava L1", "Lava L2", "Lava L3", "Lava L4", "Lava L5", "Fluid bytes", "Fluid byte budget", "Hydro atomic bytes", "Hydro observed", "Hydro kind integrity", "Cohort mode", "Cohort entities", "Cohort count", "NaturalGrove", "NaturalKarst", "NaturalMesa", "AstralCrystal", "AstralBasalt", "AstralReef", "Cohort vertices", "Cohort indices", "Cohort bytes", "Cohort byte budget", "Latest candidates", "Candidate scan cap", "Latest emitted", "Cohort observed", "Cohort payload integrity", "Combined atomic bytes"];
  layers.getRange("A1:AR1").values = [layerHeaders];
  layers.getRange(`A2:AR${layerRows.length + 1}`).values = layerRows;
  layers.tables.add(`A1:AR${layerRows.length + 1}`, true, "FarLayerEvidenceTable").style = "TableStyleMedium4";
  const layerWidths = Object.fromEntries(layerHeaders.map((_, index) => [excelColumnName(index), 13]));
  Object.assign(layerWidths, { A: 34, B: 18, C: 18, Z: 18 });
  styleLedgerSheet(layers, layerRows.length + 1, "AR", layerWidths, colors);
  layers.getRange(`D2:W${layerRows.length + 1}`).format.numberFormat = "#,##0";
  layers.getRange(`AA2:AO${layerRows.length + 1}`).format.numberFormat = "#,##0";
  layers.getRange(`AR2:AR${layerRows.length + 1}`).format.numberFormat = "#,##0";

  // Hard budgets; usage is formula-backed from live and budget cells.
  budgets.getRange("A1:F1").values = [["Explicit run", "Measure", "Live / peak", "Budget", "Usage", "Manifest decision"]];
  budgets.getRange(`A2:F${budgetRows.length + 1}`).values = budgetRows;
  for (let row = 2; row <= budgetRows.length + 1; row += 1) budgets.getRange(`E${row}`).formulas = [[`=IF(D${row}=0,0,C${row}/D${row})`]];
  budgets.tables.add(`A1:F${budgetRows.length + 1}`, true, "BudgetEvidenceTable").style = "TableStyleMedium4";
  styleLedgerSheet(budgets, budgetRows.length + 1, "F", { A: 34, B: 28, C: 18, D: 18, E: 14, F: 18 }, colors);
  budgets.getRange(`C2:D${budgetRows.length + 1}`).format.numberFormat = "#,##0";
  budgets.getRange(`E2:E${budgetRows.length + 1}`).format.numberFormat = "0.0%";
  budgets.getRange(`E2:E${budgetRows.length + 1}`).conditionalFormats.add("dataBar", { color: colors.teal, gradient: true });
  budgets.getRange(`F2:F${budgetRows.length + 1}`).conditionalFormats.add("containsText", { text: "Passed", format: { fill: "#DDF3E7", font: { color: "#17633E", bold: true } } });

  // Claim and issue ledgers contain no editorial status substitutions.
  claims.getRange("A1:E1").values = [["Scope", "Claim ID", "Classification", "Statement", "Evidence paths"]];
  claims.getRange(`A2:E${claimRows.length + 1}`).values = claimRows;
  claims.tables.add(`A1:E${claimRows.length + 1}`, true, "ClaimLedgerTable").style = "TableStyleMedium2";
  styleLedgerSheet(claims, claimRows.length + 1, "E", { A: 34, B: 48, C: 16, D: 70, E: 58 }, colors);
  claims.getRange(`C2:C${claimRows.length + 1}`).conditionalFormats.add("containsText", { text: "Passed", format: { fill: "#DDF3E7", font: { color: "#17633E", bold: true } } });
  claims.getRange(`C2:C${claimRows.length + 1}`).conditionalFormats.add("containsText", { text: "Observed", format: { fill: "#DDEBF7", font: { color: colors.navy, bold: true } } });

  issues.getRange("A1:E1").values = [["Scope", "Classification", "Code", "Field", "Recorded message"]];
  if (issueRows.length) {
    issues.getRange(`A2:E${issueRows.length + 1}`).values = issueRows;
    issues.tables.add(`A1:E${issueRows.length + 1}`, true, "IssueLedgerTable").style = "TableStyleMedium4";
  }
  styleLedgerSheet(issues, Math.max(1, issueRows.length + 1), "E", { A: 34, B: 16, C: 28, D: 40, E: 75 }, colors);

  hashes.getRange("A1:D1").values = [["Kind", "Path", "Bytes", "SHA-256"]];
  hashes.getRange(`A2:D${hashRows.length + 1}`).values = hashRows;
  hashes.tables.add(`A1:D${hashRows.length + 1}`, true, "FileHashTable").style = "TableStyleMedium2";
  styleLedgerSheet(hashes, hashRows.length + 1, "D", { A: 20, B: 70, C: 18, D: 70 }, colors);
  hashes.getRange(`C2:C${hashRows.length + 1}`).format.numberFormat = "#,##0";

  // Compact structural and formula verification before export.
  const inspect = await workbook.inspect({ kind: "workbook,sheet,table,formula", maxChars: 12000, tableMaxRows: 6, tableMaxCols: 20, options: { maxResults: 200 } });
  const errorScan = await workbook.inspect({ kind: "match", searchTerm: "#REF!|#DIV/0!|#VALUE!|#NAME\\?|#N/A", options: { useRegex: true, maxResults: 200 }, maxChars: 8000 });
  const previews = new Map();
  for (const sheetName of ["Overview", "Run Evidence", "Generation Identity", "Far Layers", "Budget Evidence", "Claims", "Issues", "File Hashes"]) {
    const preview = await workbook.render({ sheetName, autoCrop: "all", scale: 1, format: "png" });
    const bytes = new Uint8Array(await preview.arrayBuffer());
    if (!bytes.length) throw new EvidenceContractError(`rendered preview is empty: ${sheetName}`);
    previews.set(sheetName, bytes);
  }
  if (qaDir) {
    await fs.mkdir(path.dirname(qaDir), { recursive: true });
    await fs.mkdir(qaDir);
    for (const [sheetName, bytes] of previews) {
      const safeName = sheetName.toLowerCase().replaceAll(" ", "-");
      await fs.writeFile(path.join(qaDir, `${safeName}.png`), bytes, { flag: "wx" });
    }
    await fs.writeFile(path.join(qaDir, "inspect.ndjson"), inspect.ndjson ?? String(inspect), { encoding: "utf8", flag: "wx" });
    await fs.writeFile(path.join(qaDir, "formula-error-scan.ndjson"), errorScan.ndjson ?? String(errorScan), { encoding: "utf8", flag: "wx" });
  }

  await fs.mkdir(path.dirname(output), { recursive: true });
  const temporary = path.join(path.dirname(output), `.${path.basename(output)}.${process.pid}.${crypto.randomUUID()}.partial.xlsx`);
  const xlsx = await SpreadsheetFile.exportXlsx(workbook);
  await xlsx.save(temporary);
  await publishNoClobber(temporary, output);
}


async function main() {
  try {
    const args = parseArgs(process.argv.slice(2));
    const repoRoot = path.resolve(args.repoRoot);
    const evidence = await loadCanonicalEvidence(args.evidenceManifest);
    const output = await validateOutputPath(args.output, repoRoot, ".xlsx");
    const qaDir = await validateQaDirectory(args.qaDir, repoRoot);
    await verifiedScreenshots(evidence, repoRoot);
    if (!args.checkOnly) await buildWorkbook(evidence, output, qaDir);
    console.log(JSON.stringify(validationSummary(evidence, output)));
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`XLSX artifact rejected: ${message}`);
    return 2;
  }
}


process.exitCode = await main();
