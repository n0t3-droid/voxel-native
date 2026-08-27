#!/usr/bin/env node
/**
 * Build a custom 16:9 Voxel-Native evidence deck from one explicit manifest.
 *
 * The validation-only path deliberately does not import @oai/artifact-tool.
 * A real build also writes every slide render, every layout export, a montage,
 * and a bounded inspect snapshot to one explicit new QA directory.
 */

import crypto from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

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
const MAX_REFERENCE_IMAGE_BYTES = 64 * 1024 * 1024;
const MAX_DECK_RUNS = 4;
const MAX_DECK_SCREENSHOTS = MAX_DECK_RUNS;
const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const PNG_IEND = Buffer.from([0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]);
const JPEG_SIGNATURE = Buffer.from([0xff, 0xd8, 0xff]);
const JPEG_END = Buffer.from([0xff, 0xd9]);
const PROTECTED_OUTPUT_DIRS = ["saves", "qa_runs", "agent_runs"];

export const DECK_BLUEPRINT = Object.freeze([
  Object.freeze({ id: "01-opening", title: "Voxel-Native Evidence Command Center" }),
  Object.freeze({ id: "02-overview", title: "One manifest. Bounded truth." }),
  Object.freeze({ id: "03-architecture", title: "Evidence architecture, no hidden selection" }),
  Object.freeze({ id: "04-evidence", title: "What current evidence actually shows" }),
  Object.freeze({ id: "05-performance", title: "Observed route frame-time distribution" }),
  Object.freeze({ id: "06-limits", title: "Current limits are part of the evidence" }),
  Object.freeze({ id: "07-next-slice", title: "Hydro v1 evidence boundary" }),
]);

export const LAYOUT_CONTRACT = Object.freeze({
  opening: Object.freeze({
    title: Object.freeze({ top: 132, height: 252 }),
    subtitle: Object.freeze({ top: 410, height: 116 }),
  }),
  architectureCard: Object.freeze({
    top: 236,
    height: 252,
    titleOffsetTop: 18,
    titleHeight: 78,
    bodyOffsetTop: 112,
    bodyBottomInset: 20,
  }),
  performanceSamples: Object.freeze({
    label: Object.freeze({ top: 238, height: 84 }),
    value: Object.freeze({ top: 336, height: 58 }),
  }),
});

const SLIDE_SIZE = Object.freeze({ width: 1280, height: 720 });
// Artifact Tool shape fontSize values are pixels. These correspond to the
// Presentations skill's 16 pt body, 24 pt mid-level, 35 pt slide-title, and
// 50 pt deck-title floors at 96 px/in.
const TYPE_PX = Object.freeze({ body: 22, mid: 32, slideTitle: 48, deckTitle: 68 });
const COLORS = Object.freeze({
  ink: "#07111F",
  ink2: "#0C1B33",
  panel: "#102445",
  panel2: "#142B52",
  cyan: "#39D9FF",
  cyanSoft: "#A5F2FF",
  magenta: "#EF4DFF",
  violet: "#7A5CFF",
  orange: "#FF9B3D",
  green: "#5EE5A2",
  white: "#F6FBFF",
  muted: "#9BB2CE",
  line: "#26496F",
  transparent: "none",
});


function requireContract(condition, message) {
  if (!condition) throw new EvidenceContractError(message);
}


function parseArgs(argv) {
  const result = {
    repoRoot: DEFAULT_REPO_ROOT,
    checkOnly: false,
    qaDir: null,
  };
  const valueFlags = new Map([
    ["--evidence-manifest", "evidenceManifest"],
    ["--reference-image", "referenceImage"],
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
    if (index >= argv.length || argv[index].startsWith("--")) {
      throw new EvidenceContractError(`${token} requires a value`);
    }
    if (argv[index].length > 4096) throw new EvidenceContractError(`${token} exceeds the path length cap`);
    result[property] = argv[index];
  }
  if (!result.evidenceManifest) throw new EvidenceContractError("--evidence-manifest is required");
  if (!result.referenceImage) throw new EvidenceContractError("--reference-image is required");
  if (!result.output) throw new EvidenceContractError("--output is required");
  if (!result.checkOnly && !result.qaDir) {
    throw new EvidenceContractError("--qa-dir is required for a real deck build");
  }
  return result;
}


function isInside(candidate, parent) {
  const relative = path.relative(parent, candidate);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}


async function nearestExistingAncestor(candidate) {
  const missing = [];
  let cursor = path.resolve(candidate);
  for (;;) {
    try {
      await fs.lstat(cursor);
      return { existing: cursor, missing };
    } catch (error) {
      if (error.code !== "ENOENT") {
        throw new EvidenceContractError(`QA render ancestor is not safely inspectable: ${error.message}`);
      }
      const parent = path.dirname(cursor);
      requireContract(parent !== cursor, "QA render directory has no inspectable ancestor");
      missing.unshift(path.basename(cursor));
      cursor = parent;
    }
  }
}


async function canonicalProposedPath(candidate) {
  const { existing, missing } = await nearestExistingAncestor(candidate);
  let canonical;
  try {
    canonical = await fs.realpath(existing);
  } catch (error) {
    throw new EvidenceContractError(`QA render ancestor cannot be canonicalized: ${error.message}`);
  }
  return path.resolve(canonical, ...missing);
}


async function requireCanonicalOutsideProtected(candidate, repoRoot, label) {
  const root = path.resolve(repoRoot);
  const canonicalCandidate = await canonicalProposedPath(candidate);
  for (const directory of PROTECTED_OUTPUT_DIRS) {
    let protectedRoot = path.join(root, directory);
    try { protectedRoot = await fs.realpath(protectedRoot); } catch (error) {
      if (error.code !== "ENOENT") throw new EvidenceContractError(`protected directory is not safely inspectable: ${error.message}`);
    }
    requireContract(
      !isInside(path.resolve(candidate), path.join(root, directory))
        && !isInside(canonicalCandidate, protectedRoot),
      `${label} must not be inside protected directory '${directory}'`,
    );
  }
  return canonicalCandidate;
}


async function validateNewQaDirectory(qaDir, repoRoot) {
  if (!qaDir) return null;
  const destination = path.resolve(qaDir);
  const canonicalDestination = await requireCanonicalOutsideProtected(destination, repoRoot, "QA render directory");
  try {
    await fs.access(destination);
    throw new EvidenceContractError("QA render directory already exists; choose a new explicit path");
  } catch (error) {
    if (error instanceof EvidenceContractError) throw error;
    if (error.code !== "ENOENT") {
      throw new EvidenceContractError(`QA render directory is not safely inspectable: ${error.message}`);
    }
  }
  return canonicalDestination;
}


function probeReferenceBytes(payload, suffix, imagePath) {
  requireContract(payload.length > 0, "reference image is empty");
  requireContract(payload.length <= MAX_REFERENCE_IMAGE_BYTES, "reference image exceeds the fixed byte cap");
  if (suffix === ".png") {
    requireContract(
      payload.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)
        && payload.subarray(-PNG_IEND.length).equals(PNG_IEND),
      `reference image is not a complete PNG: ${imagePath}`,
    );
    return "image/png";
  }
  if (suffix === ".jpg" || suffix === ".jpeg") {
    requireContract(
      payload.subarray(0, JPEG_SIGNATURE.length).equals(JPEG_SIGNATURE)
        && payload.subarray(-JPEG_END.length).equals(JPEG_END),
      `reference image is not a complete JPEG: ${imagePath}`,
    );
    return "image/jpeg";
  }
  throw new EvidenceContractError("reference image must have a .png, .jpg, or .jpeg suffix");
}


async function validateReferenceImage(inputPath) {
  const imagePath = path.resolve(inputPath);
  let payload;
  try {
    const stat = await fs.stat(imagePath);
    requireContract(stat.isFile(), "reference image must be an explicit file");
    requireContract(stat.size > 0 && stat.size <= MAX_REFERENCE_IMAGE_BYTES, "reference image violates the fixed byte cap");
    payload = await fs.readFile(imagePath);
  } catch (error) {
    if (error instanceof EvidenceContractError) throw error;
    throw new EvidenceContractError(`reference image is not readable: ${error.message}`);
  }
  const contentType = probeReferenceBytes(payload, path.extname(imagePath).toLowerCase(), imagePath);
  return {
    path: imagePath,
    payload,
    contentType,
    sizeBytes: payload.length,
    sha256: crypto.createHash("sha256").update(payload).digest("hex"),
  };
}


function asArrayBuffer(payload) {
  return payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
}


function shortHash(value, length = 12) {
  return `${value.slice(0, length)}…`;
}


function unique(values) {
  return [...new Set(values)];
}


function formatInteger(value) {
  return new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 }).format(value);
}


function formatMs(value) {
  return `${Number(value).toFixed(2)} ms`;
}


function viewportLabel(viewport) {
  return `${viewport.physical_width}×${viewport.physical_height} @ ${viewport.dpi_percent}%`;
}


function runLabel(_run, index) {
  return `RUN ${index + 1}`;
}


function compactText(value, limit) {
  return value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;
}


function routeResolutionLabel(route) {
  return `${route.requested_route_focus} -> ${route.resolved_route_focus} · ${route.route_focus_available ? "available" : "unavailable"}`;
}


function optionalWork(value) {
  return value == null ? "not recorded" : formatInteger(value);
}


function routeWorkLabel(route) {
  return `candidate work ${optionalWork(route.route_focus_search_visited_candidates)}/${formatInteger(route.route_focus_search_candidate_cap)} · classification work ${optionalWork(route.route_focus_classification_queries)}/${formatInteger(route.route_focus_classification_query_cap)}`;
}


function generationIdentityLabel(run) {
  const identity = run.raw_observations.run_identity;
  return `${identity.world_name ?? "unrecorded"} · seed ${identity.world_seed ?? "not recorded"} · ${identity.world_profile ?? "unrecorded"}/${identity.scenery_quality ?? "unrecorded"}/${identity.terrain_grammar}`;
}


function editStoreLabel(run) {
  const editStore = run.raw_observations.world_edit_store;
  const editedChunks = editStore.world_edit_store_edited_chunks == null
    ? "not recorded"
    : formatInteger(editStore.world_edit_store_edited_chunks);
  return `${editStore.world_edit_store_status} · compatible ${editStore.world_edit_store_compatible ? "yes" : "no"} · ${editedChunks} edited chunks · reason ${editStore.world_edit_store_block_reason_code ?? "none"} · identity seed ${editStore.world_edit_store_seed ?? "not recorded"}/${editStore.world_edit_store_profile ?? "not recorded"}/${editStore.world_edit_store_scenery_quality ?? "not recorded"}/${editStore.world_edit_store_terrain_grammar ?? "not recorded"}`;
}


function farGrammarLabel(run) {
  const telemetry = run.raw_observations.planetary_streaming.telemetry;
  return `far grammar desired ${telemetry.desired_terrain_grammar} -> active ${telemetry.active_terrain_grammar ?? "not active"}`;
}


function layerSummary(run) {
  const planetary = run.raw_observations.planetary_streaming;
  const live = planetary.live;
  const telemetry = planetary.telemetry;
  return {
    hydro: `${telemetry.hydro_mode} · water ${formatInteger(live.resident_water_indices)} · lava ${formatInteger(live.resident_lava_indices)}`,
    cohorts: `${telemetry.semantic_cohort_mode} · ${formatInteger(live.resident_semantic_cohort_count)} cohorts · ${formatInteger(live.resident_semantic_cohort_vertices)} vertices`,
  };
}


function publicSourceLabel(sourcePath, repoRoot, fallback) {
  const absolute = path.resolve(sourcePath);
  const relative = path.relative(path.resolve(repoRoot), absolute);
  if (relative && !relative.startsWith("..") && !path.isAbsolute(relative)) {
    return relative.split(path.sep).join("/");
  }
  return `${fallback}/${path.basename(absolute)}`;
}


function addShape(slide, geometry, position, options = {}) {
  return slide.shapes.add({
    geometry,
    position,
    fill: options.fill ?? COLORS.transparent,
    line: options.line ?? { style: "solid", fill: COLORS.transparent, width: 0 },
    name: options.name,
    borderRadius: options.borderRadius,
    shadow: options.shadow,
    rotation: options.rotation,
  });
}


function addText(slide, text, position, style = {}, name = undefined) {
  const shape = slide.shapes.add({
    geometry: "textbox",
    position,
    fill: COLORS.transparent,
    line: { style: "solid", fill: COLORS.transparent, width: 0 },
    name,
  });
  shape.text = text;
  shape.text.style = {
    fontSize: style.fontSize ?? TYPE_PX.body,
    typeface: style.typeface ?? "Aptos",
    color: style.color ?? COLORS.white,
    bold: style.bold ?? false,
    alignment: style.alignment ?? "left",
    verticalAlignment: style.verticalAlignment ?? "top",
    autoFit: style.autoFit ?? "none",
    wrap: style.wrap ?? "square",
    lineSpacing: style.lineSpacing,
    insets: style.insets ?? { top: 0, right: 0, bottom: 0, left: 0 },
  };
  return shape;
}


function addSourceFooter(slide, label, slideNumber) {
  addShape(slide, "line", { left: 64, top: 676, width: 1152, height: 0 }, {
    fill: COLORS.transparent,
    line: { style: "solid", fill: COLORS.line, width: 1 },
    name: `source-rule-${slideNumber}`,
  });
  addText(slide, `SOURCE · ${label}`, { left: 64, top: 682, width: 1030, height: 30 }, {
    fontSize: 22,
    color: COLORS.muted,
  }, `source-label-${slideNumber}`);
  addText(slide, String(slideNumber).padStart(2, "0"), { left: 1140, top: 682, width: 76, height: 30 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.cyan,
    alignment: "right",
  }, `slide-number-${slideNumber}`);
}


function addNotes(slide, sourceLines, context = null) {
  const lines = ["[Sources]", ...sourceLines.map((line) => `- ${line}`)];
  if (context) lines.push("", "[Presenter context]", context);
  slide.speakerNotes.textFrame.setText(lines.join("\n"));
  slide.speakerNotes.setVisible(true);
}


function addBase(slide, eyebrow, title, subtitle, slideNumber) {
  slide.background.fill = COLORS.ink;
  addText(slide, eyebrow.toUpperCase(), { left: 64, top: 42, width: 620, height: 28 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.cyan,
  }, `eyebrow-${slideNumber}`);
  addText(slide, title, { left: 64, top: 82, width: 1152, height: 78 }, {
    fontSize: TYPE_PX.slideTitle,
    bold: true,
    color: COLORS.white,
    typeface: "Aptos Display",
  }, `title-${slideNumber}`);
  if (subtitle) {
    addText(slide, subtitle, { left: 66, top: 160, width: 1080, height: 62 }, {
      fontSize: 24,
      color: COLORS.muted,
    }, `subtitle-${slideNumber}`);
  }
}


function addMetricLabel(slide, label, value, position, accent, name) {
  addShape(slide, "line", { left: position.left, top: position.top + 2, width: position.width, height: 0 }, {
    line: { style: "solid", fill: accent, width: 3 },
    name: `${name}-rule`,
  });
  addText(slide, value, { left: position.left, top: position.top + 18, width: position.width, height: 58 }, {
    fontSize: 34,
    bold: true,
    color: COLORS.white,
    typeface: "Aptos Display",
  }, `${name}-value`);
  addText(slide, label.toUpperCase(), { left: position.left, top: position.top + 78, width: position.width, height: 30 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.muted,
  }, `${name}-label`);
}


function buildOpeningSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  slide.background.fill = COLORS.ink;
  slide.images.add({
    blob: asArrayBuffer(reference.payload),
    contentType: reference.contentType,
    alt: "User-provided visual direction: a luminous voxel world with shuttle, floating terrain, crystalline energy, cosmic sky, rails, and a distant command citadel.",
    fit: "cover",
    position: { left: 0, top: 0, width: 1280, height: 720 },
    name: "favorite-reference-hero",
  });
  addShape(slide, "rect", { left: 0, top: 0, width: 600, height: 720 }, {
    fill: COLORS.ink,
    line: { style: "solid", fill: COLORS.ink, width: 0 },
    name: "opening-copy-field",
  });
  addShape(slide, "line", { left: 598, top: 0, width: 0, height: 720 }, {
    line: { style: "solid", fill: COLORS.cyan, width: 4 },
    name: "opening-horizon",
  });
  addText(slide, "VOXEL-NATIVE", { left: 64, top: 74, width: 410, height: 32 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.cyan,
  }, "opening-eyebrow");
  addText(slide, "Evidence\nCommand\nCenter", { left: 64, top: LAYOUT_CONTRACT.opening.title.top, width: 486, height: LAYOUT_CONTRACT.opening.title.height }, {
    fontSize: TYPE_PX.deckTitle,
    bold: true,
    color: COLORS.white,
    typeface: "Aptos Display",
  }, "opening-title");
  addText(slide, "A manifest-bound view of the world we can prove today — and the global slice we can responsibly attempt next.", { left: 66, top: LAYOUT_CONTRACT.opening.subtitle.top, width: 462, height: LAYOUT_CONTRACT.opening.subtitle.height }, {
    fontSize: 28,
    color: COLORS.cyanSoft,
  }, "opening-subtitle");
  addText(slide, `CURRENT EVIDENCE · ${evidence.data.overall_classification.toUpperCase()}`, { left: 66, top: 556, width: 462, height: 32 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.orange,
  }, "opening-classification");
  addText(slide, `SOURCE · REFERENCE ${shortHash(reference.sha256)} · MANIFEST ${shortHash(evidence.manifestSha256)}`, { left: 66, top: 600, width: 486, height: 52 }, {
    fontSize: 22,
    color: COLORS.muted,
  }, "opening-source-label");
  addNotes(slide, [
    `Visual direction and embedded hero: explicit user-provided image ${sources.reference}; SHA-256 ${reference.sha256}; ${reference.sizeBytes} bytes.`,
    `Evidence classification and manifest identity: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
  ], "The hero is a visual target, not evidence that the current engine already matches the depicted world.");
}


function buildOverviewSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  addBase(
    slide,
    "Command overview",
    "One manifest. Bounded truth.",
    "Every visible metric is derived from explicit QA 2.6.0 current runs; absence remains absence.",
    2,
  );
  const runs = evidence.data.runs;
  const viewports = unique(runs.map((run) => viewportLabel(run.raw_observations.viewport)));
  const routes = unique(runs.map((run) => routeResolutionLabel(run.raw_observations.route)));
  const grammars = unique(runs.map((run) => run.raw_observations.run_identity.terrain_grammar));
  const compatibleStoreCount = runs.filter((run) => run.raw_observations.world_edit_store.world_edit_store_compatible).length;
  const editedChunkCount = runs.reduce((sum, run) => sum + run.raw_observations.world_edit_store.world_edit_store_edited_chunks, 0);

  addShape(slide, "ellipse", { left: 472, top: 236, width: 336, height: 336 }, {
    fill: COLORS.panel,
    line: { style: "solid", fill: COLORS.cyan, width: 3 },
    name: "overview-core",
  });
  addShape(slide, "ellipse", { left: 520, top: 284, width: 240, height: 240 }, {
    fill: COLORS.ink2,
    line: { style: "dashed", fill: COLORS.magenta, width: 2 },
    name: "overview-core-inner",
  });
  addText(slide, evidence.data.overall_classification.toUpperCase(), { left: 538, top: 338, width: 204, height: 46 }, {
    fontSize: 28,
    bold: true,
    color: COLORS.orange,
    alignment: "center",
  }, "overview-classification");
  addText(slide, "CANONICAL\nEVIDENCE", { left: 538, top: 398, width: 204, height: 82 }, {
    fontSize: 28,
    bold: true,
    color: COLORS.white,
    alignment: "center",
  }, "overview-core-label");

  addMetricLabel(slide, "Current runs", formatInteger(runs.length), { left: 72, top: 254, width: 250 }, COLORS.cyan, "overview-runs");
  addMetricLabel(slide, "Recorded viewports", formatInteger(viewports.length), { left: 76, top: 458, width: 250 }, COLORS.magenta, "overview-viewports");
  addMetricLabel(slide, "Route resolutions", formatInteger(routes.length), { left: 936, top: 254, width: 250 }, COLORS.orange, "overview-routes");
  addMetricLabel(slide, "Terrain grammars", compactText(grammars.join(" + "), 14), { left: 932, top: 458, width: 250 }, COLORS.violet, "overview-materials");

  addText(slide, `EDIT STORES · ${compatibleStoreCount}/${runs.length} COMPATIBLE\nEDITED CHUNKS · ${formatInteger(editedChunkCount)}`, { left: 510, top: 486, width: 260, height: 68 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.cyanSoft,
    alignment: "center",
  }, "overview-edit-store");

  addText(slide, "AUTOMATED TEST TOTAL", { left: 430, top: 584, width: 420, height: 40 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.muted,
    alignment: "center",
  }, "overview-tests-label");
  addText(slide, "Not represented by schema 1.6.0", { left: 360, top: 626, width: 560, height: 40 }, {
    fontSize: TYPE_PX.mid,
    color: COLORS.white,
    alignment: "center",
  }, "overview-tests-value");
  addSourceFooter(slide, `manifest ${shortHash(evidence.manifestSha256)}`, 2);
  addNotes(slide, [
    `Run count, viewport count, route-resolution count, terrain grammars, edit-store compatibility/counts, and classification: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
    ...runs.map((run, index) => `${runLabel(run, index)} generation ${generationIdentityLabel(run)}; edit store ${editStoreLabel(run)}; ${farGrammarLabel(run)}.`),
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ], "The manifest does not contain an independently hashed release-gate transcript, so no automated test total is shown.");
}


function architectureNode(slide, title, detail, position, accent, name) {
  const node = addShape(slide, "roundRect", position, {
    fill: COLORS.panel,
    line: { style: "solid", fill: accent, width: 2 },
    borderRadius: "rounded-xl",
    name,
  });
  addText(slide, title, { left: position.left + 20, top: position.top + LAYOUT_CONTRACT.architectureCard.titleOffsetTop, width: position.width - 40, height: LAYOUT_CONTRACT.architectureCard.titleHeight }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: accent,
  }, `${name}-title`);
  addText(slide, detail, { left: position.left + 20, top: position.top + LAYOUT_CONTRACT.architectureCard.bodyOffsetTop, width: position.width - 40, height: position.height - LAYOUT_CONTRACT.architectureCard.bodyOffsetTop - LAYOUT_CONTRACT.architectureCard.bodyBottomInset }, {
    fontSize: 22,
    color: COLORS.white,
  }, `${name}-detail`);
  return node;
}


function buildArchitectureSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  addBase(
    slide,
    "Evidence architecture",
    "Evidence architecture, no hidden selection",
    "Capture, classification, consumption, and presentation remain separate, inspectable stages.",
    3,
  );
  const card = LAYOUT_CONTRACT.architectureCard;
  const positions = [
    { left: 64, top: card.top, width: 250, height: card.height },
    { left: 364, top: card.top, width: 250, height: card.height },
    { left: 664, top: card.top, width: 250, height: card.height },
    { left: 964, top: card.top, width: 250, height: card.height },
  ];
  const nodes = [
    architectureNode(slide, "01 · EXPLICIT\nQA RUNS", "QA 2.6.0 report + PNG bytes.\nNo newest-run lookup.", positions[0], COLORS.cyan, "architecture-runs"),
    architectureNode(slide, "02 · EVIDENCE\nMANIFEST", "Schema 1.6.0 also binds viewport DPI provenance, terrain grammar, edit-store compatibility, and dense-residency budget proof to the run identity.", positions[1], COLORS.magenta, "architecture-manifest"),
    architectureNode(slide, "03 · STRICT\nCONSUMER", "Rejects stale, incomplete, changed, or unsafe evidence.", positions[2], COLORS.orange, "architecture-consumer"),
    architectureNode(slide, "04 · ARTIFACT\nLANES", "DOCX · PDF · XLSX · PPTX share one bounded truth.", positions[3], COLORS.violet, "architecture-artifacts"),
  ];
  for (let index = 0; index < nodes.length - 1; index += 1) {
    slide.shapes.connect(nodes[index], nodes[index + 1], {
      kind: "straight",
      fromSide: "right",
      toSide: "left",
      line: { style: "solid", fill: COLORS.cyanSoft, width: 2 },
      head: { type: "arrow", width: "med", length: "med" },
    });
  }
  addText(slide, "NO LATEST\nSCAN", { left: 72, top: 504, width: 250, height: 72 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.cyan }, "architecture-guard-1");
  addText(slide, "NO LEGACY\nSTATUS", { left: 366, top: 504, width: 250, height: 72 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.magenta }, "architecture-guard-2");
  addText(slide, "NO RESULT\nDEFAULTS", { left: 666, top: 504, width: 250, height: 72 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.orange }, "architecture-guard-3");
  addText(slide, "NO\nOVERWRITE", { left: 968, top: 504, width: 250, height: 72 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.violet }, "architecture-guard-4");
  addText(slide, `Generator ${evidence.data.generator.name} ${evidence.data.generator.version}`, { left: 64, top: 604, width: 700, height: 28 }, {
    fontSize: 22,
    color: COLORS.muted,
  }, "architecture-generator");
  addSourceFooter(slide, `manifest generator + consumer contract · ${shortHash(evidence.manifestSha256)}`, 3);
  addNotes(slide, [
    `Manifest generator identity, selection policy, classifications, and evidence records: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
    ...evidence.data.runs.map((run, index) => `${runLabel(run, index)} generation ${generationIdentityLabel(run)}; edit store ${editStoreLabel(run)}; ${farGrammarLabel(run)}.`),
    `Strict consumer implementation: tools/artifacts/evidence_manifest_consumer.mjs.`,
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ]);
}


async function addScreenshot(slide, shot, position, index, total) {
  const payload = await fs.readFile(shot.resolved);
  requireContract(payload.length === shot.record.size_bytes, `screenshot size changed before embedding: ${shot.display}`);
  requireContract(
    payload.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)
      && payload.subarray(-PNG_IEND.length).equals(PNG_IEND),
    `screenshot is no longer a complete PNG before embedding: ${shot.display}`,
  );
  requireContract(
    crypto.createHash("sha256").update(payload).digest("hex") === shot.record.sha256,
    `screenshot hash changed before embedding: ${shot.display}`,
  );
  const viewport = shot.run.raw_observations.viewport;
  const route = shot.run.raw_observations.route;
  const identity = shot.run.raw_observations.run_identity;
  const editStore = shot.run.raw_observations.world_edit_store;
  slide.images.add({
    blob: asArrayBuffer(payload),
    contentType: "image/png",
    alt: `Manifest-verified QA screenshot for requested route ${route.requested_route_focus}, resolved as ${route.resolved_route_focus}, at ${viewportLabel(viewport)}, under terrain grammar ${identity.terrain_grammar}. Edit store ${editStore.world_edit_store_status}, ${editStore.world_edit_store_edited_chunks} edited chunks. Byte integrity is Passed; visual acceptance is not implied.`,
    fit: "contain",
    position,
    geometry: "roundRect",
    borderRadius: "rounded-xl",
    name: `evidence-screenshot-${index + 1}`,
  });
  const label = total === 1
    ? `${compactText(routeResolutionLabel(route), 44)} · ${viewportLabel(viewport)}`
    : `RUN ${index + 1} · ${compactText(routeResolutionLabel(route), 28)}\n${viewportLabel(viewport)}`;
  addText(slide, label, {
    left: position.left,
    top: position.top + position.height + 10,
    width: position.width,
    height: total === 1 ? 30 : 58,
  }, { fontSize: 22, color: COLORS.cyanSoft }, `evidence-screenshot-label-${index + 1}`);
}


async function buildEvidenceSlide(presentation, evidence, reference, screenshots, sources) {
  const slide = presentation.slides.add();
  addBase(
    slide,
    "Visual evidence",
    "What current evidence actually shows",
    "Referenced PNGs are rehashed at use time. Their pixels remain evidence to inspect, not a visual verdict.",
    4,
  );
  const selected = screenshots.slice(0, MAX_DECK_SCREENSHOTS);
  requireContract(selected.length === evidence.data.runs.length, "deck must embed one verified screenshot per explicit run");
  const screenshotFrames = selected.length === 1
    ? [{ left: 64, top: 234, width: 760, height: 382 }]
    : selected.length === 2
      ? [
          { left: 64, top: 238, width: 368, height: 292 },
          { left: 448, top: 238, width: 368, height: 292 },
        ]
      : [
          { left: 64, top: 238, width: 368, height: 150 },
          { left: 448, top: 238, width: 368, height: 150 },
          { left: 64, top: 468, width: 368, height: 130 },
          { left: 448, top: 468, width: 368, height: 130 },
        ];
  for (const [index, shot] of selected.entries()) {
    await addScreenshot(slide, shot, screenshotFrames[index], index, selected.length);
  }
  const claimCounts = evidence.data.summary.claim_counts;
  const rightLeft = 866;
  const rightWidth = 350;
  addText(slide, "RECORDED", { left: rightLeft, top: 244, width: rightWidth, height: 28 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.orange,
  }, "evidence-recorded-label");
  addText(slide, `${formatInteger(claimCounts.Passed)} Passed\n${formatInteger(claimCounts.Observed)} Observed\nacross ${selected.length} run${selected.length === 1 ? "" : "s"}`, { left: rightLeft, top: 288, width: rightWidth, height: 112 }, {
    fontSize: 32,
    bold: true,
    color: COLORS.white,
  }, "evidence-claim-counts");
  addText(slide, "BYTE BOUNDARY", { left: rightLeft, top: 414, width: rightWidth, height: 28 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.cyan,
  }, "evidence-byte-label");
  addText(slide, "Size + signature + terminal IEND + SHA-256", { left: rightLeft, top: 454, width: rightWidth, height: 88 }, {
    fontSize: 22,
    color: COLORS.white,
  }, "evidence-byte-detail");
  addText(slide, "NOT IMPLIED", { left: rightLeft, top: 564, width: rightWidth, height: 26 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.magenta,
  }, "evidence-limit-label");
  addText(slide, "No clipping, overlap, motion, lighting, or terrain-quality approval.", { left: rightLeft, top: 600, width: rightWidth, height: 62 }, {
    fontSize: 22,
    color: COLORS.muted,
  }, "evidence-limit-detail");
  addSourceFooter(slide, `${selected.length} manifest-selected PNG${selected.length === 1 ? "" : "s"} · rehashed`, 4);
  addNotes(slide, [
    `Claims, classifications, screenshot selection, and file identities: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
    ...selected.map((shot, index) => `Embedded screenshot RUN ${index + 1}: screenshot-source/${path.basename(shot.display)}; SHA-256 ${shot.record.sha256}; ${shot.record.size_bytes} bytes.`),
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ], "The screenshot-integrity classification covers bytes only. A later human slide and engine review must assess perception and behavior.");
}


function buildPerformanceSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  addBase(
    slide,
    "Route telemetry",
    "Observed route frame-time distribution",
    "Quantiles describe each recorded route, build, and viewport. They are not a universal FPS target or causal uplift.",
    5,
  );
  const runs = evidence.data.runs;
  const series = runs.map((run, index) => {
    const frame = run.raw_observations.route_frame_times;
    const fills = [COLORS.cyan, COLORS.magenta, COLORS.orange, COLORS.violet, COLORS.green, COLORS.cyanSoft, "#FF6E76", "#B49AFF"];
    return {
      name: runLabel(run, index),
      values: [frame.median_ms, frame.p95_ms, frame.p99_ms, frame.max_ms],
      fill: fills[index],
    };
  });
  slide.charts.add("bar", {
    position: { left: 68, top: 244, width: 850, height: 362 },
    title: "Route frame time · lower is faster · milliseconds",
    titlePlacement: "aboveChart",
    titleTextStyle: { fontSize: TYPE_PX.mid, fill: COLORS.white, bold: true },
    categories: ["p50", "p95", "p99", "max"],
    series,
    hasLegend: true,
    legend: {
      position: "bottom",
      overlay: false,
      fill: COLORS.transparent,
      line: { style: "solid", fill: COLORS.transparent, width: 0 },
      textStyle: { fontSize: 22, fill: COLORS.white },
    },
    barOptions: { direction: "column", grouping: "clustered", gapWidth: 56 },
    xAxis: {
      visible: true,
      textStyle: { fontSize: 22, fill: COLORS.white },
      line: { style: "solid", fill: COLORS.line, width: 1 },
      majorGridlines: null,
    },
    yAxis: {
      visible: true,
      title: { text: "milliseconds", textStyle: { fontSize: 22, fill: COLORS.muted } },
      numberFormatCode: "0.00",
      textStyle: { fontSize: 22, fill: COLORS.muted },
      line: { style: "solid", fill: COLORS.line, width: 1 },
      majorGridlines: { style: "solid", fill: COLORS.line, width: 1 },
    },
    chartFill: COLORS.ink,
    chartLine: { style: "solid", fill: COLORS.transparent, width: 0 },
    plotAreaFill: COLORS.ink2,
    plotAreaLine: { style: "solid", fill: COLORS.line, width: 1 },
  });

  const totalSamples = runs.reduce((sum, run) => sum + run.raw_observations.route_frame_times.sample_count, 0);
  const p95Values = runs.map((run) => run.raw_observations.route_frame_times.p95_ms);
  const maxValues = runs.map((run) => run.raw_observations.route_frame_times.max_ms);
  addText(slide, "RECORDED\nSAMPLES", { left: 966, top: LAYOUT_CONTRACT.performanceSamples.label.top, width: 250, height: LAYOUT_CONTRACT.performanceSamples.label.height }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.cyan }, "performance-samples-label");
  addText(slide, formatInteger(totalSamples), { left: 964, top: LAYOUT_CONTRACT.performanceSamples.value.top, width: 252, height: LAYOUT_CONTRACT.performanceSamples.value.height }, { fontSize: 36, bold: true, color: COLORS.white }, "performance-samples");
  addText(slide, "P95 RANGE", { left: 966, top: 416, width: 250, height: 42 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.magenta }, "performance-p95-label");
  addText(slide, `${formatMs(Math.min(...p95Values))}\n→ ${formatMs(Math.max(...p95Values))}`, { left: 964, top: 462, width: 252, height: 58 }, { fontSize: 24, bold: true, color: COLORS.white }, "performance-p95-range");
  addText(slide, "MAX RANGE", { left: 966, top: 538, width: 250, height: 42 }, { fontSize: TYPE_PX.mid, bold: true, color: COLORS.orange }, "performance-max-label");
  addText(slide, `${formatMs(Math.min(...maxValues))}\n→ ${formatMs(Math.max(...maxValues))}`, { left: 964, top: 586, width: 252, height: 64 }, { fontSize: 24, bold: true, color: COLORS.white }, "performance-max-range");
  addText(slide, runs.map((run, index) => `${runLabel(run, index)} · ${compactText(routeResolutionLabel(run.raw_observations.route), 36)}`).join("  |  "), { left: 68, top: 618, width: 850, height: 34 }, {
    fontSize: 22,
    color: COLORS.muted,
  }, "performance-run-key");
  addSourceFooter(slide, `${runs.length} explicit route${runs.length === 1 ? "" : "s"} · quantiles complete`, 5);
  addNotes(slide, [
    `Route frame-time samples and quantiles: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ], "All displayed ranges are exact derivations over the explicit run set. No threshold, benchmark comparison, or causal performance claim is introduced.");
}


function buildLimitsSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  addBase(
    slide,
    "Readiness boundary",
    "Current limits are part of the evidence",
    "Integrity is necessary. Perceptual quality, responsiveness, provenance, and release readiness need additional evidence.",
    6,
  );
  addText(slide, "≠", { left: 82, top: 240, width: 260, height: 290 }, {
    fontSize: 190,
    bold: true,
    color: COLORS.magenta,
    alignment: "center",
    verticalAlignment: "middle",
  }, "limits-not-equal");
  addText(slide, "INTEGRITY\nIS NOT\nACCEPTANCE", { left: 72, top: 500, width: 280, height: 116 }, {
    fontSize: 28,
    bold: true,
    color: COLORS.white,
    alignment: "center",
  }, "limits-thesis");

  const statements = [
    ["PNG identity", "does not prove clipping, overlap, lighting, terrain quality, or motion."],
    ["One viewport per run", "does not complete the responsive and DPI matrix."],
    ["Route quantiles", "do not establish a universal threshold or causal improvement."],
    ["Manifest hashes", "do not prove authorship or unrecorded source correspondence."],
    ["No gate transcript", "means automated test totals remain absent from this deck."],
  ];
  statements.forEach(([lead, detail], index) => {
    const top = 236 + index * 78;
    addShape(slide, "ellipse", { left: 410, top: top + 7, width: 18, height: 18 }, {
      fill: index % 2 === 0 ? COLORS.cyan : COLORS.orange,
      line: { style: "solid", fill: COLORS.transparent, width: 0 },
      name: `limits-marker-${index + 1}`,
    });
    addText(slide, `${lead} —`, { left: 452, top, width: 322, height: 52 }, {
      fontSize: TYPE_PX.mid,
      bold: true,
      color: COLORS.white,
    }, `limits-lead-${index + 1}`);
    addText(slide, detail, { left: 790, top, width: 402, height: 64 }, {
      fontSize: 22,
      color: COLORS.muted,
    }, `limits-detail-${index + 1}`);
  });
  const issueCounts = evidence.data.summary.issue_counts;
  addText(slide, `MANIFEST ISSUES · ${formatInteger(Object.values(issueCounts).reduce((sum, value) => sum + value, 0))} RECORDED`, { left: 410, top: 626, width: 782, height: 30 }, {
    fontSize: 22,
    bold: true,
    color: COLORS.cyan,
  }, "limits-issue-count");
  addSourceFooter(slide, `manifest classifications + documented artifact boundary`, 6);
  addNotes(slide, [
    `Issue counts, screenshot identity, viewport scope, route quantiles, and file hashes: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}.`,
    `Artifact interpretation guardrails: docs/ARTIFACT_EVIDENCE_BUILDERS.md.`,
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ], "The word acceptance is used only to deny an unsupported conclusion. This slide does not record or imply a visual or release acceptance decision.");
}


function buildNextSliceSlide(presentation, evidence, reference, sources) {
  const slide = presentation.slides.add();
  const runs = evidence.data.runs;
  const hydroModes = unique(runs.map((run) => run.raw_observations.planetary_streaming.telemetry.hydro_mode));
  const cohortModes = unique(runs.map((run) => run.raw_observations.planetary_streaming.telemetry.semantic_cohort_mode));
  const waterIndices = runs.reduce((sum, run) => sum + run.raw_observations.planetary_streaming.live.resident_water_indices, 0);
  const lavaIndices = runs.reduce((sum, run) => sum + run.raw_observations.planetary_streaming.live.resident_lava_indices, 0);
  const cohortCount = runs.reduce((sum, run) => sum + run.raw_observations.planetary_streaming.live.resident_semantic_cohort_count, 0);
  const cohortVertices = runs.reduce((sum, run) => sum + run.raw_observations.planetary_streaming.live.resident_semantic_cohort_vertices, 0);
  addBase(
    slide,
    "IMPLEMENTED / RENDER-ONLY V1",
    "Hydro v1 evidence boundary",
    "Implemented render-only v1. Hydro-current telemetry is recorded. Semantic-cohort payloads are recorded separately. Human same-binary visual acceptance is pending.",
    7,
  );
  addShape(slide, "roundRect", { left: 74, top: 238, width: 1132, height: 282 }, {
    fill: COLORS.panel,
    line: { style: "solid", fill: COLORS.cyan, width: 2 },
    borderRadius: "rounded-xl",
    name: "next-slice-field",
  });
  const phases = [
    { left: 104, title: "HYDRO RECORDED", detail: `${hydroModes.join(" + ")} · run-sum water ${formatInteger(waterIndices)} · lava ${formatInteger(lavaIndices)} indices. No gameplay-water claim.`, accent: COLORS.cyan },
    { left: 490, title: "COHORTS RECORDED", detail: `${cohortModes.join(" + ")} · run-sum ${formatInteger(cohortCount)} cohorts · ${formatInteger(cohortVertices)} vertices. Render-only L5 layer.`, accent: COLORS.magenta },
    { left: 876, title: "PENDING", detail: "Human review of the same-binary captures and formal visual acceptance.", accent: COLORS.orange },
  ];
  const phaseNodes = phases.map((phase, index) => {
    const node = addShape(slide, "roundRect", { left: phase.left, top: 274, width: 300, height: 202 }, {
      fill: COLORS.ink2,
      line: { style: "solid", fill: phase.accent, width: 3 },
      borderRadius: "rounded-xl",
      name: `next-phase-${index + 1}`,
    });
    addText(slide, phase.title, { left: phase.left + 28, top: 302, width: 244, height: 38 }, {
      fontSize: TYPE_PX.mid,
      bold: true,
      color: phase.accent,
      alignment: "center",
    }, `next-phase-title-${index + 1}`);
    addText(slide, phase.detail, { left: phase.left + 30, top: 356, width: 240, height: 98 }, {
      fontSize: 22,
      color: COLORS.white,
      alignment: "center",
    }, `next-phase-detail-${index + 1}`);
    return node;
  });
  for (let index = 0; index < phaseNodes.length - 1; index += 1) {
    slide.shapes.connect(phaseNodes[index], phaseNodes[index + 1], {
      kind: "straight",
      fromSide: "right",
      toSide: "left",
      line: { style: "solid", fill: COLORS.cyanSoft, width: 3 },
      head: { type: "arrow", width: "med", length: "med" },
    });
  }
  addText(slide, "VISUAL ACCEPTANCE REMAINS EVIDENCE-BOUND", { left: 74, top: 552, width: 1132, height: 40 }, {
    fontSize: TYPE_PX.mid,
    bold: true,
    color: COLORS.white,
    alignment: "center",
  }, "next-slice-gate-title");
  addText(slide, "Current manifest + byte-verified captures are inputs · human visual review is the remaining gate", { left: 96, top: 606, width: 1088, height: 44 }, {
    fontSize: 22,
    color: COLORS.cyanSoft,
    alignment: "center",
  }, "next-slice-gate-detail");
  addSourceFooter(slide, `Hydro-current manifest recorded / human visual acceptance pending`, 7);
  addNotes(slide, [
    `Implementation source: src/planetary_streaming.rs; render-only Hydro v1 is implemented.`,
    `The manifest contains no independently hashed release-gate transcript, so no automated test total or nonvisual gate result is shown.`,
    `Hydro-current telemetry and byte-verified same-binary captures: ${sources.manifest}; SHA-256 ${evidence.manifestSha256}. Human visual review and formal visual acceptance remain pending.`,
    ...runs.map((run, index) => {
      const route = run.raw_observations.route;
      const layers = layerSummary(run);
      return `${runLabel(run, index)} generation ${generationIdentityLabel(run)}; edit store ${editStoreLabel(run)}; ${farGrammarLabel(run)}; route ${routeResolutionLabel(route)}; ${routeWorkLabel(route)}; Hydro ${layers.hydro}; cohorts ${layers.cohorts}.`;
    }),
    `Deck-wide visual direction: ${sources.reference}; SHA-256 ${reference.sha256}.`,
  ], "Implementation, Hydro-current telemetry, and capture byte identity are recorded. Human visual review and formal visual acceptance are not recorded by the manifest and remain pending.");
}


async function toBuffer(blob) {
  if (Buffer.isBuffer(blob)) return blob;
  if (blob instanceof Uint8Array) return Buffer.from(blob);
  if (blob && typeof blob.arrayBuffer === "function") return Buffer.from(await blob.arrayBuffer());
  throw new EvidenceContractError("artifact runtime returned an unsupported blob type");
}


async function writeBlobExclusive(outputPath, blob) {
  const payload = await toBuffer(blob);
  requireContract(payload.length > 0, `artifact runtime returned an empty render: ${outputPath}`);
  await fs.writeFile(outputPath, payload, { flag: "wx" });
}


async function buildDeck(evidence, reference, screenshots, output, qaDir, repoRoot) {
  const { Presentation, PresentationFile } = await import("@oai/artifact-tool");
  const presentation = Presentation.create({
    slideSize: { width: 1280, height: 720 },
  });
  presentation.theme.colorScheme = {
    name: "Voxel-Native Cosmic Command",
    themeColors: {
      accent1: COLORS.cyan,
      accent2: COLORS.magenta,
      accent3: COLORS.orange,
      accent4: COLORS.violet,
      accent5: COLORS.green,
      accent6: COLORS.cyanSoft,
      bg1: COLORS.ink,
      bg2: COLORS.ink2,
      tx1: COLORS.white,
      tx2: COLORS.muted,
      dk1: "#000000",
      dk2: COLORS.panel,
      lt1: COLORS.white,
      lt2: COLORS.cyanSoft,
      hlink: COLORS.cyan,
      folHlink: COLORS.magenta,
    },
  };
  const sources = {
    manifest: publicSourceLabel(evidence.manifestPath, repoRoot, "external-manifest"),
    reference: publicSourceLabel(reference.path, repoRoot, "user-reference"),
  };

  buildOpeningSlide(presentation, evidence, reference, sources);
  buildOverviewSlide(presentation, evidence, reference, sources);
  buildArchitectureSlide(presentation, evidence, reference, sources);
  await buildEvidenceSlide(presentation, evidence, reference, screenshots, sources);
  buildPerformanceSlide(presentation, evidence, reference, sources);
  buildLimitsSlide(presentation, evidence, reference, sources);
  buildNextSliceSlide(presentation, evidence, reference, sources);
  requireContract(presentation.slides.items.length === DECK_BLUEPRINT.length, "deck blueprint and slide count disagree");

  await fs.mkdir(path.dirname(qaDir), { recursive: true });
  await fs.mkdir(qaDir);
  for (const [index, slide] of presentation.slides.items.entries()) {
    const stem = DECK_BLUEPRINT[index].id;
    await writeBlobExclusive(
      path.join(qaDir, `${stem}.png`),
      await presentation.export({ slide, format: "png", scale: 1 }),
    );
    const layout = await slide.export({ format: "layout", scale: 1 });
    const layoutText = typeof layout.text === "function" ? await layout.text() : (await toBuffer(layout)).toString("utf8");
    requireContract(layoutText.length > 0, `layout export is empty: ${stem}`);
    await fs.writeFile(path.join(qaDir, `${stem}.layout.json`), layoutText, { encoding: "utf8", flag: "wx" });
  }
  await writeBlobExclusive(
    path.join(qaDir, "deck-montage.webp"),
    await presentation.export({
      format: "webp",
      montage: { format: "webp", width: 1800, slideWidth: 580, padding: 28, gap: 20, background: COLORS.ink, columns: 2 },
      scale: 1,
    }),
  );
  const inspect = await presentation.inspect({
    kind: "deck,slide,textbox,shape,image,chart,notes,layout",
    maxChars: 30000,
  });
  await fs.writeFile(
    path.join(qaDir, "inspect.ndjson"),
    inspect.ndjson ?? String(inspect),
    { encoding: "utf8", flag: "wx" },
  );
  await fs.writeFile(
    path.join(qaDir, "build-inputs.json"),
    `${JSON.stringify({
      evidence_manifest: sources.manifest,
      manifest_sha256: evidence.manifestSha256,
      reference_image: sources.reference,
      reference_sha256: reference.sha256,
      slide_ids: DECK_BLUEPRINT.map((item) => item.id),
      visual_acceptance: "not_recorded",
    }, null, 2)}\n`,
    { encoding: "utf8", flag: "wx" },
  );

  await fs.mkdir(path.dirname(output), { recursive: true });
  const temporary = path.join(
    path.dirname(output),
    `.${path.basename(output)}.${process.pid}.${crypto.randomUUID()}.partial.pptx`,
  );
  try {
    const pptx = await PresentationFile.exportPptx(presentation);
    await pptx.save(temporary);
    await publishNoClobber(temporary, output);
  } finally {
    try { await fs.unlink(temporary); } catch { /* exact task-owned temporary only */ }
  }
}


function deckValidationSummary(evidence, output, reference, qaDir, repoRoot) {
  return {
    ...validationSummary(evidence, output),
    artifact_kind: "pptx",
    slide_size: SLIDE_SIZE,
    slide_count: DECK_BLUEPRINT.length,
    slide_ids: DECK_BLUEPRINT.map((item) => item.id),
    reference_image: publicSourceLabel(reference.path, repoRoot, "user-reference"),
    reference_sha256: reference.sha256,
    reference_size_bytes: reference.sizeBytes,
    qa_dir: qaDir,
    visual_acceptance: "not_recorded",
  };
}


export async function main(argv = process.argv.slice(2)) {
  try {
    const args = parseArgs(argv);
    const repoRoot = path.resolve(args.repoRoot);
    const evidence = await loadCanonicalEvidence(args.evidenceManifest);
    requireContract(evidence.data.runs.length <= MAX_DECK_RUNS, `deck run count exceeds the fixed cap of ${MAX_DECK_RUNS}`);
    const output = await validateOutputPath(args.output, repoRoot, ".pptx");
    const canonicalOutput = await requireCanonicalOutsideProtected(output, repoRoot, "output");
    requireContract(path.extname(canonicalOutput).toLowerCase() === ".pptx", "canonical output must retain the .pptx suffix");
    const qaDir = await validateNewQaDirectory(args.qaDir, repoRoot);
    const reference = await validateReferenceImage(args.referenceImage);
    const screenshots = await verifiedScreenshots(evidence, repoRoot, MAX_DECK_SCREENSHOTS);
    if (!args.checkOnly) {
      const checkedCanonicalQa = await validateNewQaDirectory(qaDir, repoRoot);
      requireContract(qaDir === checkedCanonicalQa, "QA render directory changed before publication");
      const finalCanonicalOutput = await requireCanonicalOutsideProtected(output, repoRoot, "output");
      requireContract(canonicalOutput === finalCanonicalOutput, "output path changed before publication");
      await buildDeck(evidence, reference, screenshots, output, qaDir, repoRoot);
    }
    console.log(JSON.stringify(deckValidationSummary(evidence, output, reference, qaDir, repoRoot)));
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    console.error(`PPTX artifact rejected: ${message}`);
    return 2;
  }
}


if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main();
}
