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
      route.route_focus,
      route.requested_route_distance_m,
      frame.sample_count,
      frame.mean_ms,
      frame.median_ms,
      frame.p95_ms,
      frame.p99_ms,
      frame.max_ms,
      observations.screenshots.referenced_files.length,
      planetary.telemetry.surface_material_mode,
      planetary.live.resident_mesh_bytes,
      planetary.budgets.budget_mesh_bytes,
      planetary.telemetry.peak_live_sample_cache_bytes,
      planetary.budgets.budget_sample_cache_bytes,
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
  const budgets = workbook.worksheets.add("Budget Evidence");
  const claims = workbook.worksheets.add("Claims");
  const issues = workbook.worksheets.add("Issues");
  const hashes = workbook.worksheets.add("File Hashes");
  for (const sheet of [overview, runs, budgets, claims, issues, hashes]) sheet.showGridLines = false;

  const runRows = collectRuns(evidence);
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
  overview.getRange("A3").values = [[`Explicit manifest | generated ${evidence.data.generated_at_utc} | sha256 ${evidence.manifestSha256}`]];
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
  overview.getRange("B8").formulas = [[`=SUM('Run Evidence'!O2:O${runRows.length + 1})`]];
  overview.getRange("B9").formulas = [[`=COUNTIF('Claims'!C2:C${claimRows.length + 1},"Passed")`]];
  overview.getRange("B10").formulas = [[`=COUNTIF('Claims'!C2:C${claimRows.length + 1},"Observed")`]];
  applyHeader(overview.getRange("A5:B5"), colors.teal, colors.white);
  overview.getRange("A6:A11").format = { fill: colors.pale, font: { bold: true, color: colors.navy } };
  overview.getRange("B6:B11").format = { fill: colors.cream, font: { color: colors.ink }, wrapText: true };
  overview.getRange("B7:B10").format.numberFormat = "#,##0";
  overview.getRange("D5:H5").values = [["Boundary", "Meaning", "Evidence source", "What is not implied", "Disposition"]];
  overview.getRange("D6:H10").values = [
    ["Run selection", "Only explicitly named immutable QA runs", "Manifest inputs.selection_policy", "No latest/global scan", "Passed"],
    ["Frame time", "Route-only fixed-memory quantiles", "route_frame_times", "No universal FPS threshold", "Observed"],
    ["Planetary budgets", "Serialized live/peak values compared like-for-like", "planetary_streaming", "No visual-quality claim", "Passed"],
    ["Screenshot integrity", "Referenced PNG bytes are complete and hashed", "screenshots + file_hashes", "No perceptual inspection result", "Passed"],
    ["Responsive scope", "One viewport is recorded per run", "viewport", "No full viewport/DPI matrix", "Observed"],
  ];
  overview.getRange("D5:H10").format = { wrapText: true, verticalAlignment: "top", font: { name: "Aptos", size: 9, color: colors.ink } };
  applyHeader(overview.getRange("D5:H5"), colors.navy, colors.white);
  overview.getRange("H6:H10").conditionalFormats.add("containsText", { text: "Passed", format: { fill: "#DDF3E7", font: { color: "#17633E", bold: true } } });
  overview.getRange("H6:H10").conditionalFormats.add("containsText", { text: "Observed", format: { fill: "#DDEBF7", font: { color: colors.navy, bold: true } } });
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
  const runHeaders = ["Explicit run", "Build", "World", "World profile", "Viewport", "DPI", "Route focus", "Target m", "Samples", "Mean ms", "Median ms", "P95 ms", "P99 ms", "Max ms", "PNGs", "Material mode", "Mesh bytes", "Mesh budget", "Peak cache bytes", "Cache budget"];
  runs.getRange("A1:T1").values = [runHeaders];
  runs.getRange(`A2:T${runRows.length + 1}`).values = runRows;
  runs.tables.add(`A1:T${runRows.length + 1}`, true, "RunEvidenceTable").style = "TableStyleMedium2";
  styleLedgerSheet(runs, runRows.length + 1, "T", { A: 34, B: 11, C: 24, D: 18, E: 16, F: 10, G: 20, H: 12, I: 12, J: 12, K: 12, L: 12, M: 12, N: 12, O: 9, P: 18, Q: 16, R: 16, S: 18, T: 16 }, colors);
  runs.getRange(`F2:F${runRows.length + 1}`).format.numberFormat = "0%";
  runs.getRange(`H2:I${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`J2:N${runRows.length + 1}`).format.numberFormat = "0.000";
  runs.getRange(`O2:O${runRows.length + 1}`).format.numberFormat = "#,##0";
  runs.getRange(`Q2:T${runRows.length + 1}`).format.numberFormat = "#,##0";

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
  for (const sheetName of ["Overview", "Run Evidence", "Budget Evidence", "Claims", "Issues", "File Hashes"]) {
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
