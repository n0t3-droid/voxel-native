# Manifest-backed artifact builders

The DOCX, PDF, XLSX, and PPTX builders consume exactly one explicit JSON evidence
manifest produced by `tools/artifacts/build_evidence_manifest.py`. They do not
select a newest run, scan `qa_runs/`, open legacy `status.ron`, or accept a
human-entered verdict or test count.

## Source-first technical atlas

`tools/artifacts/build_codex_engineering_atlas.py` is deliberately separate from
the manifest-backed builders below. It produces the project-authored technical
atlas from immutable single-read snapshots of a bounded allowlist: documentation
contracts, the authoritative chunk/city/bot/Agent Control Rust sources, its own
generator identity, and eight original SVG diagrams. The current declaration is
30 unique files. It does not inspect `qa_runs/`, select runtime screenshots,
consume a canonical manifest, or emit a runtime release verdict. Its runtime
gallery remains explicitly pending the separate visual acceptance route.

Validate the declared inputs, dependencies, fingerprint, and destination without
writing output:

```powershell
python -B tools/artifacts/build_codex_engineering_atlas.py --check-only
```

That route performs no builder-authored filesystem writes. It validates exact
source anchors and subjects every bounded, passive SVG to a chunked start-event
preflight that constructs no element tree and aborts as soon as the fixed 8,192
node, 64-level, or 65,536-attribute ceiling is crossed. Only a structurally
bounded input is then fully parsed from its captured bytes after a fixed
fail-closed math-glyph-to-ASCII normalization; an unmapped
non-ASCII character is rejected. Resolved XML text, tails, and attributes reject
forbidden C0/C1/DEL and invalid Unicode scalars. SVG style blocks use a strict
class-rule grammar, an explicit property allowlist, bounded finite numeric
values, internal-only references, and only the base input family names Helvetica
or Courier; CSS escapes, Base-14 variant names as input families, font sources,
fallback families, workstation paths, and environment-dependent functions fail
closed before `svg2rlg`. Font weight is
restricted to `normal` or `bold`; project styles use explicit family, size, and
weight declarations because svglib does not preserve CSS `font` shorthand
semantics. A complete base-family/normal-or-bold/normal-or-italic-or-oblique
matrix must convert only to the eight expected PDF Base-14 output faces. The
converter also tests all eight SVGs with dynamic font registration,
TrueType loading, fontconfig, and subprocess paths disabled. It is not a
substring-only XML check. Formula multiplication, subscripts, superscripts, and
norm labels are authored as single plain-ASCII text nodes; `<tspan>` is rejected
because svglib can fragment its runs and visually collide independently
positioned glyphs. The flagship budget label is exactly
`Delta_l = 16 * 2^l m | ||(x,z)||_inf <= R_l = 30 * Delta_l`. Literal `*`
remains multiplication, while middle-dot normalization is reserved for prose
separators. Run it
with `-B` as shown so dependency imports cannot create Python bytecode caches.
The sorted toolchain identity binds Python, ReportLab, svglib, pypdf, compiled
and runtime zlib,
lxml, cssselect2, tinycss2, and lxml's compiled and runtime libxml2/libxslt versions;
missing or `unknown` version identities fail closed before any build.
The same escape/environment-function guard applies to every resolved XML
attribute, while direct paint/filter/marker presentation attributes accept only
canonical colors or internal fragment references.

The CLI first captures its own source through one bounded, stable regular-file
descriptor, rejects a changed/reparse identity, and executes exactly those bytes
in a fresh inner pass. The builder input snapshot is injected from that immutable
byte string; its canonical repository path and component chain are revalidated,
but its content is never reread. Executed generator semantics and recorded
builder size/SHA-256 therefore share one identity, and renamed/copied invocations
fail closed. Normal module import remains available only for pure helper tests;
`main()` fails without the byte-bound CLI snapshot.
Because check-only never publishes, an existing regular PDF at the destination
does not block validation; no-clobber is enforced again on every real build.

Run the no-output adversarial builder suite with:

```powershell
python -B tools/artifacts/test_build_codex_engineering_atlas.py
```

Once a separately approved release copy exists at the single canonical path
`docs/releases/technical-preview/voxel-native-codex-engineering-atlas.pdf`, CI
can validate those exact bytes against the current 30 inputs and toolchain with:

```powershell
python -B tools/artifacts/build_codex_engineering_atlas.py --validate-release
```

This route is strictly read-only: it neither selects a build destination nor
enters any generation, temporary-file, or publication path. It accepts only the
fixed canonical release location, uses a no-follow regular-file descriptor with
before/after identity checks and a 64 MiB cap, then applies the same SVG,
object-graph, text, font, link, metadata, fingerprint, and document-ID validation
as generation. A missing, empty, oversized, reparse-backed, changing, or invalid
release fails closed. `--validate-release` is mutually exclusive with
`--force`, `--no-clobber`, `--check-only`, and `--verify-determinism`, and rejects
a nondefault `--output`.

Build the default ignored working artifact with no-clobber publication:

```powershell
python -B tools/artifacts/build_codex_engineering_atlas.py
```

For a release-candidate build, require two byte-identical in-memory builds before
the single final publication step:

```powershell
python -B tools/artifacts/build_codex_engineering_atlas.py --verify-determinism
```

The default destination is
`output/pdf/voxel-native-codex-engineering-atlas.pdf`. `--force` is required for
an explicit atomic replacement; `--no-clobber` restates the safer default and is
mutually exclusive with `--force`. Output is confined to this repository's
`output/pdf/` or `tmp/` trees. Lexical traversal, symlink/junction/reparse
components, non-portable path names, and resolved containment changes are
rejected before publication.
The builder validates exact A4 page count and headings, a strict PDF envelope,
canonical metadata and document IDs, compressed page streams, the complete
source/generator hashes, passive text, the exact HTTPS URI allowlist, and the
bounded object graph's absence of active PDF features before writing the
same-directory temporary file. Trailer IDs are compared using pypdf's
authoritative original byte strings rather than lossy re-encoding of its
PDFDocEncoding text view. Only an object whose concrete type is exactly the
`TextStringObject` class imported from the active pypdf dependency may expose
that recovery property; plain strings, forged subclasses or metadata
lookalikes, unsupported values, and non-byte-preserving values fail closed.
Every font object must be an explicitly allowed
PDF Type1 Base-14 face with WinAnsi encoding; Symbol, ArialUnicode, TrueType,
embedded font programs, and ToUnicode fallback resources fail closed. The
bounded global object walk rejects `/FontFile`, `/FontFile2`, and `/FontFile3`
at every reachable depth, including inside direct or indirect font descriptors;
a canonical-looking outer Base-14 font dictionary cannot hide a font program.
The
active-object walk rejects named and 3D-view
actions, PostScript XObjects, page transitions/durations, scripts, embedded
files, outlines, rich media, forms, launch/remote actions, and the other
enumerated active keys, types, and actions in the builder contract. Every URI
action in the global object graph independently requires one scalar allowlisted
HTTPS target and forbids action chaining. The invariant canvas removes
ReportLab's implicit empty page-transition dictionaries before serialization.

Before a release copy is placed under `docs/releases/`, render every final page
from the exact published bytes, inspect each image at full size, extract text,
verify page order and metadata, reject replacement glyphs and absolute
workstation paths, and compare a second build byte-for-byte. Capture Poppler's
stderr and render a minimal one-page Base-14 control PDF with the exact same
binary and environment. Any missing-font diagnostic emitted only for the atlas
fails the release. If the control and atlas emit the same `Symbol` or
`ArialUnicode` startup diagnostic while the atlas's complete reachable font
object graph contains neither resource, record it as a renderer-installation
diagnostic rather than an atlas-font defect; retain both raw logs and never hide
the comparison behind quiet mode. A different or additional atlas diagnostic
still fails closed.
Long formula and rollback labels must retain their authored token boundaries in
the rendered pages; source-level wrapping checks do not replace visual review.
This technical
atlas QA proves document integrity and presentation only; it cannot promote a
runtime screenshot or convert pending visual acceptance into a pass.

## Safety and acceptance boundary

Every builder requires:

- schema version `1.6.0` from generator version `1.6.0`;
- exact current QA report schema `2.6.0`, including distinct effective and
  OS/window-backend viewport scale factors and the 2,400-slot combined
  resident-plus-in-flight total and independently observed peak;
- the explicit-run selection policy recorded by the manifest generator;
- an aggregate `Observed` classification;
- at least one unique run whose `report_schema_variant` is `current` and whose
  run classification is `Observed`;
- valid route-only quantiles with `measurement_valid=true` and
  `quantiles_complete=true`;
- enabled planetary streaming with six-level geometry/material state and the
  current live, peak, and hard-budget fields;
- one or more manifest-referenced, Passed screenshots per run;
- internally consistent claim, issue, summary, report, screenshot, and
  generator-source hash records.

Before a screenshot is embedded, the builder reopens only the bounded paths
selected by the manifest, verifies the size, PNG signature and terminal IEND,
and recomputes SHA-256. This is a time-of-use identity check. It is not a visual
quality judgment.

Manifest input is capped at 8 MiB. Runs, claims, issues, hashes, screenshots,
individual screenshot bytes, and total embedded screenshot bytes all have
fixed caps. Output is explicit, must use the artifact's expected extension,
must not exist, and must not be under `saves/`, `qa_runs/`, or `agent_runs/`.
Publication uses a same-directory temporary file and a no-clobber hard link;
an existing destination is never replaced.

## Validation-only commands

These commands validate the manifest, destination, and referenced screenshot
bytes without creating an artifact:

```powershell
$referenceImage = '<absolute path to the selected local PNG or JPEG>'

python -B tools/artifacts/build_elite_visual_report.py `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --output output/evidence/voxel-native-evidence.docx `
  --check-only

python -B tools/artifacts/build_elite_visual_pdf.py `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --output output/evidence/voxel-native-evidence.pdf `
  --check-only

node tools/artifacts/build_elite_qa_workbook.mjs `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --output output/evidence/voxel-native-evidence.xlsx `
  --check-only

node tools/artifacts/build_elite_command_center_deck.mjs `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --reference-image $referenceImage `
  --output output/evidence/voxel-native-command-center.pptx `
  --check-only
```

The XLSX validation-only route intentionally does not import
`@oai/artifact-tool`, so its evidence boundary is reproducible from a clean
checkout with Node.js. Real XLSX and PPTX rendering currently requires the
bundled artifact runtime distributed with Codex desktop; that dependency and a
clean-checkout bootstrap are not distributed by this repository. Treat those
two real-build routes as optional Codex operator tooling until a public runtime
is documented. Never commit a workstation-specific package path or junction.

For an actual XLSX build, pass an explicit new `--qa-dir` to retain one render
of every sheet plus bounded workbook inspection and formula-error scan output:

```powershell
node tools/artifacts/build_elite_qa_workbook.mjs `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --output output/evidence/voxel-native-evidence.xlsx `
  --qa-dir output/evidence/workbook-render-qa
```

## Content contract

The artifacts render only manifest observations, claims, issues, budgets, and
file identities. Stable explanatory text describes the evidence contract, not
unrecorded engine results. In particular:

- automated test totals are omitted because schema `1.6.0` intentionally does
  not synthesize test or gate transcripts, and this manifest has no separately
  hashed release-gate transcript;
- `Passed` is used only for manifest integrity or like-for-like hard-budget
  checks;
- frame-time values remain `Observed` and are not presented as a causal uplift
  or universal threshold;
- one viewport does not imply completion of the responsive/DPI matrix;
- PNG completion and hashes do not imply a human visual pass.

## Required final-artifact QA and known gaps

The builders accept only fresh canonical evidence. The repository does not
include or imply approved final evidence artifacts.

- DOCX: the evidence-boundary callout is a non-splittable Word table row whose
  sole paragraph is also marked keep-together. File-identity rows begin at a
  deterministic page boundary and are divided into balanced blocks of at most
  eight rows. Every block repeats the header, links all rows except the last
  with Word keep-next paragraph properties, and never creates a one-row
  multipage orphan. After a real build, render every page to PNG with a
  compatible Word or LibreOffice renderer, inspect every PNG at full size, fix
  layout defects, and rerender. A single path can legally approach 4096
  characters; if one
  physical row is taller than a page, Word cannot honor any keep-together
  constraint, so final-run rendering remains mandatory. Word page-number
  fields may also require a field-update/materialization step in renderers that
  do not update fields automatically.
- PDF: the run matrix uses a fixed 174-mm column plan with a dedicated 16-mm
  PNG-count column so its header cannot collapse into one letter per line. The
  six live/peak hard-budget observations are compacted into one complete row
  per explicit run. Those rows are planned into balanced, indivisible groups
  of at most eight; multi-page groups differ by at most one row and never end
  in a singleton orphan page. Claim rows use the same current-style A4 capacity
  of eight rows and are emitted as balanced, indivisible tables with a repeated
  header, preventing a short continuation page after a maximally filled page.
  The semantic `generator_source` kind remains unchanged in the manifest and is
  shown as the nonbreaking human label `generator source` in a widened 30-mm
  Kind column. Render every page with Poppler and inspect at full size. Claim
  statements and evidence paths are bounded but variable-height content; an
  individual row taller than a page cannot be kept intact by ReportLab. Long
  paths and 64-character hashes likewise remain identity-heavy content, so
  wrapping must still be checked on the final run set rather than assumed from
  fixture output.
- XLSX: a real build renders every sheet and can retain those images via
  `--qa-dir`, but a human still has to inspect all of them. The artifact-tool
  inspection and formula-error scan are preserved as QA evidence; the current
  facade does not provide a single documented boolean "all layouts and formulas
  passed" result, so those records must be reviewed rather than summarized by
  an invented pass flag.
- PPTX: `tools/artifacts/build_elite_command_center_deck.mjs` creates a custom
  seven-slide, 1280×720 cosmic command-center deck from scratch. It does not
  reuse a bundled template. The visual direction is one explicit PNG or JPEG,
  capped at 64 MiB, signature-checked, hashed, byte-embedded, and cited in
  speaker notes. Manifest-selected PNGs retain descriptive alternative text.
  One manifest-referenced screenshot per explicit run is shown; aggregate claim
  counts are visibly labeled with the same run count. Deck speaker notes use
  repository-relative or neutral source labels plus full hashes, never absolute
  workstation paths. Fixed-layout text does not auto-shrink below its authored
  size, and the authored pixel sizes encode the skill's point-size floors.
  The slides cover overview, evidence architecture, visual evidence, observed
  route performance, current evidence limits, and the Hydro v1 evidence
  boundary. Slide 7 records that render-only Hydro v1 is implemented in
  `src/planetary_streaming.rs`; it deliberately reports no automated test total
  or nonvisual gate result because schema `1.6.0` carries neither transcript.
  Formal same-binary visual acceptance and current-manifest acceptance remain
  pending until QA/report/manifest Hydro telemetry produces a Hydro-current
  manifest whose captures are fully reviewed.
  The deck hard-fails above four explicit runs so chart labels cannot silently
  become unreadable.

## Presentation build and QA contract

A real presentation build requires a new explicit `--qa-dir`. The directory
must not exist and must not be under `saves/`, `qa_runs/`, or `agent_runs/`.
Like the workbook builder, the real export currently requires Codex desktop's
bundled artifact runtime; the dependency-free `--check-only` path does not.
The builder renders all seven slides, exports every layout record, creates a
montage, records a bounded deck inspection and exact build inputs, then
publishes the PPTX without replacing an existing file:

```powershell
$referenceImage = '<absolute path to the selected local PNG or JPEG>'

node tools/artifacts/build_elite_command_center_deck.mjs `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --reference-image $referenceImage `
  --output output/evidence/voxel-native-command-center.pptx `
  --qa-dir output/evidence/command-center-render-qa
```

That build is still not a visual acceptance decision. Before sharing the deck:

1. Open every `01-opening.png` through `07-next-slice.png` at full size. Check
   title wrapping, body wrapping, chart labels, screenshot crops, connector
   routing, footer clearance, contrast, and the 16:9 safe area.
2. Inspect `deck-montage.webp` for hierarchy, pacing, repeated composition,
   inconsistent margins, and slides that are visually too dense or too empty.
3. Review every `*.layout.json` plus `inspect.ndjson` for text or objects outside
   the 1280×720 canvas and for unintended element intersections. Layout records
   are diagnostic evidence, not a synthesized pass flag.
4. Run an independent PPTX structural validator and record its exact name and
   version with the evidence. Codex operators may additionally use the current
   bundled slide validator, but its installation path is environment-owned and
   must never be copied into repository instructions or public artifacts. A
   clean checkout does not yet include an equivalent validator; that is a
   declared reproducibility gap, not an implicit pass.

5. Fix every reported overflow and every visually detected overlap or crop in
   the builder, generate a new uniquely named output/QA directory, and repeat
   the full pass. Never overwrite the prior artifact or treat `shrinkText` as a
   substitute for correcting a poor layout.

The QA-directory check canonicalizes the nearest existing ancestor and rejects
protected-directory containment through a junction or symlink as well as by
literal path. The destination is revalidated immediately before a real build.

Known presentation-runtime gap: a clean checkout supports only the
dependency-free `--check-only` and structural fixture tests. The real
`@oai/artifact-tool` export/render path, independent PPTX validation,
PowerPoint/LibreOffice fidelity, speaker-note materialization, image-crop
fidelity, chart legend behavior, and the overlap/overflow pass remain
unverified until fresh canonical evidence and an explicit final output/QA
directory are supplied. No final deck is implied by this documentation.

Run the deterministic no-output fixture suite with:

```powershell
python -B tools/artifacts/test_artifact_manifest_consumers.py
```
