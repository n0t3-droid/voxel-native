# Manifest-backed artifact builders

The DOCX, PDF, XLSX, and PPTX builders consume exactly one explicit JSON evidence
manifest produced by `tools/artifacts/build_evidence_manifest.py`. They do not
select a newest run, scan `qa_runs/`, open legacy `status.ron`, or accept a
human-entered verdict or test count.

## Safety and acceptance boundary

Every builder requires:

- schema version `1.0.0` from generator version `1.0.0`;
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
  --reference-image "C:\explicit\path\to\favorite-voxel-reference.jpg" `
  --output output/evidence/voxel-native-command-center.pptx `
  --check-only
```

The XLSX validation-only route intentionally does not import
`@oai/artifact-tool`. It can therefore test the evidence boundary before the
bundled artifact runtime is initialized. A real workbook build must use the
workspace-dependency runtime and a task-local `node_modules` junction described
by the Spreadsheets skill; system or repo-local packages are not supported.

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

- automated test totals are omitted because schema `1.0.0` has no separately
  hashed release-gate transcript;
- `Passed` is used only for manifest integrity or like-for-like hard-budget
  checks;
- frame-time values remain `Observed` and are not presented as a causal uplift
  or universal threshold;
- one viewport does not imply completion of the responsive/DPI matrix;
- PNG completion and hashes do not imply a human visual pass.

## Required final-artifact QA and known gaps

The builders are prepared for fresh canonical evidence, but this change does
not generate or approve final artifacts.

- DOCX: the evidence-boundary callout is a non-splittable Word table row whose
  sole paragraph is also marked keep-together. File-identity rows begin at a
  deterministic page boundary and are divided into balanced blocks of at most
  eight rows. Every block repeats the header, links all rows except the last
  with Word keep-next paragraph properties, and never creates a one-row
  multipage orphan. After a real build, render every page with the Documents
  skill's `render_docx.py`, inspect every PNG at full size, fix layout defects,
  and rerender. A single path can legally approach 4096 characters; if one
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
  `src/planetary_streaming.rs` and that current task evidence has green
  nonvisual gates. It does not invent a gate transcript or test total. Formal
  same-binary visual acceptance and current-manifest acceptance remain pending
  until QA/report/manifest Hydro telemetry produces a Hydro-current manifest
  whose captures are fully reviewed.
  The deck hard-fails above four explicit runs so chart labels cannot silently
  become unreadable.

## Presentation build and QA contract

A real presentation build requires a new explicit `--qa-dir`. The directory
must not exist and must not be under `saves/`, `qa_runs/`, or `agent_runs/`.
Like the workbook builder, it must run with the bundled workspace Node runtime
and task-local `node_modules` junction supplied by the artifact skills; the
dependency-free `--check-only` path does not require that runtime.
The builder renders all seven slides, exports every layout record, creates a
montage, records a bounded deck inspection and exact build inputs, then
publishes the PPTX without replacing an existing file:

```powershell
node tools/artifacts/build_elite_command_center_deck.mjs `
  --evidence-manifest output/evidence/planetary-manifest.json `
  --reference-image "C:\explicit\path\to\favorite-voxel-reference.jpg" `
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
4. Run the Presentations skill's `container_tools/slides_test.py` with the
   bundled workspace Python against the emitted PPTX. The system Python in this
   workspace currently lacks `pdf2image`, so the bundled runtime is required:

   ```powershell
   & $bundledPython `
     "C:\Users\ylber\.codex\plugins\cache\openai-primary-runtime\presentations\26.805.11740\skills\presentations\container_tools\slides_test.py" `
     output/evidence/voxel-native-command-center.pptx
   ```

5. Fix every reported overflow and every visually detected overlap or crop in
   the builder, generate a new uniquely named output/QA directory, and repeat
   the full pass. Never overwrite the prior artifact or treat `shrinkText` as a
   substitute for correcting a poor layout.

The QA-directory check canonicalizes the nearest existing ancestor and rejects
protected-directory containment through a junction or symlink as well as by
literal path. The destination is revalidated immediately before a real build.

Known presentation-runtime gap: this preparation intentionally runs only the
dependency-free `--check-only` and structural fixture tests. The real
`@oai/artifact-tool` export/render path, PowerPoint/LibreOffice fidelity,
speaker-note materialization, image-crop fidelity, chart legend behavior, and
the overlap/overflow pass remain unverified until fresh canonical evidence and
an explicit final output/QA directory are supplied. No final deck has been
generated by this preparation.

Run the deterministic no-output fixture suite with:

```powershell
python -B tools/artifacts/test_artifact_manifest_consumers.py
```
