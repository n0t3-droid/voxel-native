# Voxel-Native Evidence Manifest Schema

Status: schema `1.0.0`

Generator: `tools/artifacts/build_evidence_manifest.py`

## Purpose

The evidence manifest is the canonical, machine-readable boundary between raw
QA runs and presentation artifacts such as the engineering dossier, PDF,
workbook, evidence dashboard, and release notes. It preserves what was actually
observed, exposes missing or unsafe evidence, and prevents downstream builders
from copying hardcoded test counts, FPS values, screenshots, or budget claims.

The manifest is evidence indexing, not release certification. A valid Debug FPS
measurement is still classified as `Observed`; it does not become a Release
performance promise merely because its input files are intact.

## Input and output contract

Every QA run must be supplied explicitly:

```powershell
python -B tools/artifacts/build_evidence_manifest.py `
  --qa-run qa_runs/run_1786329313 `
  --qa-run qa_runs/run_1786329471 `
  --qa-run qa_runs/run_1786329490 `
  --output output/evidence/planetary-manifest.json
```

The command:

- inspects only the directories passed through `--qa-run`;
- requires one direct `report.ron` per accepted directory;
- enumerates only direct `.png` children of each explicit directory, plus safe
  screenshot paths explicitly named by that run's report;
- never searches for the newest run;
- rejects a directory literally named `latest`;
- never recursively scans the repository-wide `qa_runs` tree;
- canonicalizes and deduplicates run arguments;
- rejects run aliases resolving to a directory named `latest` and refuses a
  `report.ron` symlink that escapes its explicit run directory;
- requires an explicit `.json` output path;
- rejects output beneath repository `saves/`, `qa_runs/`, or `agent_runs/`;
- writes through a same-directory temporary file and atomic replacement.

Generated manifests belong to the artifact/evidence workflow. Repository
staging remains governed by `scripts/elite-release-gates.ps1`; generating a file
under `output/` does not authorize staging `output/`.

### Exit codes

| Code | Meaning |
|---:|---|
| `0` | Manifest was written and its overall classification is `Observed`. |
| `1` | Unsafe output configuration or an I/O failure prevented a trustworthy manifest write. |
| `2` | Manifest was written, but at least one accepted input is `Blocked`, `Rejected`, or explicitly `Planned`. |

Exit code `2` is intentional: downstream automation can retain the diagnostic
manifest while refusing to publish it as accepted evidence.

## Determinism

For identical files, explicit inputs, generator source, repository location,
and a fixed timestamp, serialized output is byte-stable:

- JSON object keys are sorted;
- runs are sorted by canonical path;
- claims, issues, file hashes, screenshot paths, and enumerated files use stable
  sorting;
- duplicate run arguments are removed and reported;
- JSON is emitted as UTF-8 with a final newline;
- `NaN` and infinity are prohibited by `allow_nan=False`;
- no filesystem modification time is used as evidence or as a sort key.

`generated_at_utc` is the only intentionally changing value during normal CLI
generation. It is an RFC 3339 UTC timestamp at whole-second precision.

## Top-level structure

```text
schema_version
generated_at_utc
generator
claim_classifications
inputs
overall_classification
summary
claims[]
issues[]
file_hashes[]
runs[]
```

### `generator`

| Field | Type | Meaning |
|---|---|---|
| `name` | string | Stable generator identity. |
| `version` | string | Generator contract version. |
| `source_path` | string | Repository-relative path when possible. |
| `source_sha256` | string | SHA-256 of the exact Python source that generated the manifest. |

### `inputs`

| Field | Type | Meaning |
|---|---|---|
| `argument_count` | integer | Number of raw `--qa-run` arguments. |
| `accepted_run_count` | integer | Canonical, unique, non-`latest`, non-traversing run paths processed. |
| `qa_run_directories` | string array | Stable list of accepted explicit run paths. |
| `selection_policy` | string | Fixed declaration that selection was explicit and non-scanning. |

### `file_hashes[]`

Every successfully read evidence file has:

| Field | Type | Meaning |
|---|---|---|
| `kind` | enum | `report`, `screenshot`, or `generator_source`. |
| `path` | string | Repository-relative when inside the repository, otherwise canonical absolute path. |
| `sha256` | string | Lowercase SHA-256 hexadecimal digest. |
| `size_bytes` | integer | Bytes streamed through the hash calculation. |

Hashing uses fixed 1 MiB read chunks. A hash proves byte identity, not authorship,
visual quality, semantic correctness, or correspondence with an unrecorded Git
revision. Reports are hashed and captured for parsing in one bounded streaming
pass. Screenshots are hashed and checked for PNG signature/terminal `IEND` over
the same byte stream, avoiding a hash-to-parser or hash-to-probe race.

## Claim classifications

The only legal classifications are:

| Classification | Meaning |
|---|---|
| `Passed` | A structural, integrity, or explicit hard-budget check passed. |
| `Observed` | A measured value is faithfully recorded without asserting a threshold or release promise. |
| `Rejected` | Evidence is unsafe, contradictory, invalid, missing after being referenced, or explicitly marked invalid by the runtime. |
| `Planned` | A future evidence item was explicitly declared as planned by an authoritative upstream workflow. The current generator never invents planned work. |
| `Blocked` | The input can be inspected, but required evidence is absent, typically because the report uses a legacy schema. |

Priority for aggregate status is `Rejected` > `Blocked` > `Planned` >
`Observed`/`Passed`. A run containing only passed integrity claims and observed
measurements aggregates to `Observed`, not `Passed`. Both claims and issues
participate in per-run and top-level aggregation; an isolated rejected path or
I/O issue can therefore never be hidden by otherwise valid claims.

Each claim contains:

- `id`: stable run-qualified identity;
- `classification`: one value from the table above;
- `statement`: narrowly scoped claim;
- `evidence`: sorted source paths supporting that claim.

Each issue contains:

- `code`: stable machine-readable failure name;
- `classification`;
- `field`: source field or path;
- `message`: bounded explanatory text without fabricated replacement values.

`summary.claim_counts` and `summary.issue_counts` report the two grains
separately for every legal classification.

## Per-run structure

```text
input_path
report_schema_variant
overall_classification
claims[]
issues[]
raw_observations
  run_identity
  viewport
  route
  route_frame_times
  planetary_streaming
  screenshots
```

`report_schema_variant` is:

- `current`: `qa_report_schema_version` is exactly `2.0.0`, route-only
  frame-time evidence is present, and the derived build profile is present;
- `legacy`: the report parsed, but one or both current contracts are absent;
- `unavailable`: the report was missing, malformed, oversized, or contained a
  non-finite number.

### `raw_observations.run_identity`

Known fields are copied without inventing defaults:

- `package_version`
- `build_profile`
- `instance_label`
- `world_name`
- `world_seed`
- `world_profile`
- `scenery_quality`
- `git_sha`
- `git_dirty`
- `source_fingerprint`
- `executable_hash`
- `toolchain`
- `hardware`

`package_version` and derived `build_profile` are required for the current
identity claim. The remaining provenance fields are optional, but any value
that is present must have the serialized type promised by `src/qa.rs`.

A legacy report without `build_profile` is `Blocked`; the generator never
guesses Debug or Release from FPS, paths, timestamps, or naming conventions.

### `raw_observations.viewport`

The following values must all be finite and positive:

- `logical_width`
- `logical_height`
- `physical_width`
- `physical_height`
- `scale_factor`
- `dpi_percent`

Physical width/height must retain their serialized unsigned-integer shape.
Logical size multiplied by scale factor must agree with physical size within
the one-pixel rounding boundary, and `dpi_percent` must agree with
`scale_factor * 100`.

The manifest records one viewport per run. It does not infer completion of the
full responsive viewport/DPI matrix.

### `raw_observations.route`

The route observation preserves:

- `route_focus`
- `requested_route_distance_m`
- `max_horizontal_displacement_m`
- `requested_duration_seconds`
- `duration_seconds`
- `warmup_seconds`
- `write_tail_seconds`
- `frames`
- `average_fps`
- `max_frame_ms`
- `final_smoothed_fps`

All numeric values must be finite and non-negative. `average_fps` and
`final_smoothed_fps` remain observations. The manifest defines no hidden FPS
acceptance threshold.

Legacy reports that do not separate requested duration and screenshot write-tail
time are retained but `Blocked` for current route-timing provenance.

### `raw_observations.route_frame_times`

Current reports expose the fixed-memory route-only accumulator contract:

- scope and quantile method;
- accepted route sample count;
- excluded warmup and write-tail frame counts;
- rejection counts by cause;
- histogram bucket width/range and overflow count;
- mean, median, p95, p99, and maximum frame milliseconds;
- accuracy bounds;
- accumulator byte and scan-work caps;
- `quantiles_complete` and `measurement_valid`.

Validation includes:

- count fields are non-negative and fixed memory/work/histogram bounds are
  positive;
- statistics are null or finite and non-negative;
- the aggregate rejection count equals the sum of its serialized causes;
- histogram overflow cannot exceed the accepted sample count;
- bucket count matches exact range, width, and one overflow bucket;
- the quantile scan cap covers the whole histogram;
- the exact route-only scope, nearest-rank method, and conservative-upper-bound
  interpretation are serialized;
- median <= p95 <= p99 <= max when all exist;
- top-level `frames` equals `sample_count`;
- top-level `average_fps` agrees with `1000 / mean_ms` within a small
  serialization tolerance;
- top-level `max_frame_ms` agrees with the route-only maximum;
- `measurement_valid=true` requires samples, zero rejected samples, complete
  quantiles, and non-null mean/median/p95/p99/max;
- `measurement_valid=false` is `Rejected`, never silently upgraded.

The current 1 ms histogram reports conservative bucket upper bounds. Quantiles
have a stated maximum bucket error of 1 ms when they do not land in the overflow
bucket. The mean uses per-sample microsecond rounding with a stated 0.0005 ms
rounding bound. An overflow quantile is null and invalid rather than falsely
reported at 1 ms accuracy.

Legacy reports without `route_frame_times` are `Blocked`; the generator does not
reconstruct median, p95, or p99 from average FPS, maximum frame time, or stalls.

### `raw_observations.planetary_streaming`

The observation separates:

- `live`: enabled/profile, near/far coverage, resident geometry, mesh payload,
  and sample-cache values;
- `budgets`: entity, vertex, index, mesh-byte, build-job, ring-result,
  sample-cache, and coverage-work limits;
- `telemetry`: scheduler, rebuild, query, cache, clamp, and camera coordinates.

QA report schema `2.0.0` is the first current contract with Far Hydro evidence.
Reports without that exact version remain readable but are classified `legacy`
and `Blocked`; field presence or zero defaults are never used to reinterpret a
pre-Hydro report as current evidence.

The `resident_*` and `ring_*` live fields are post-`apply_deferred` Bevy ECS
observations. Matching `scheduler_resident_*` and `scheduler_ring_*` fields
retain the streamer's bookkeeping separately. A current evidence run is
rejected unless both representations match exactly and the bounded observer
reports: valid, no seventh-entity overflow, no duplicate/out-of-range levels,
no budget violation, and zero rejection episodes.

Far Hydro truth is separate from the established terrain truth:

- `hydro_mode` is exactly `Disabled` or `DescriptiveV1`;
- `resident_fluid_*` and `fluid_ring_*` are post-`apply_deferred` ECS values;
- `scheduler_resident_fluid_*` and `scheduler_fluid_ring_*` are independent
  scheduler bookkeeping and must match the observed values exactly;
- `resident_fluid_observation_valid` must be true, while fluid overflow,
  duplicate-slot, out-of-range, scheduler-mismatch, budget-exceeded and
  rejection counters must all report the clean state;
- `budget_fluid_*`, `budget_fluid_ring_build_bytes`, and
  `budget_atomic_ring_build_bytes` retain the separate fluid and paired worker
  ceilings;
- `last_fluid_classification_queries`, `last_fluid_biome_queries`,
  `last_fluid_vertices`, and `last_fluid_indices` record bounded latest-work
  observations, not a performance or visual-quality promise.

When `hydro_mode` is `Disabled`, all fluid ECS/scheduler populations, per-ring
arrays, and latest-work counters must be zero. This fail-closed relationship
prevents an off/on transition or stale result from being mislabeled as a clean
rollback run.

Material-transition evidence is deliberately redundant and cross-checked:

- `material_detail` is the desired L0/global-policy summary;
- `desired_material_detail` contains exactly six `Detailed`/`Reduced` LOD states;
- `resident_material_detail` contains exactly six nullable installed LOD states;
- `resident_detailed_levels` and `resident_reduced_levels` must exactly match
  those installed states;
- `surface_material_mode` identifies the legacy or versioned bridge algorithm;
- `last_material_slope_queries` keeps expensive material-only height work
  separate from geometry height work;
- `last_bridge_v2_cell_reuses` records the bounded absolute-cell memo reuse;
- live/peak sample-cache window and byte counters prove both steady-state and
  transient residency, with peak values required to be monotonic.

A mixed transition can therefore never be reported as fully detailed merely
because the desired global policy already changed.

Hard-budget claims compare only serialized like-for-like pairs:

- resident entities <= entity budget;
- resident vertices <= vertex budget;
- resident indices <= index budget;
- resident mesh bytes <= mesh-byte budget;
- observed and scheduler fluid entities/vertices/indices/mesh bytes <= their
  corresponding fluid budgets;
- live sample-cache bytes <= sample-cache budget;
- peak sample-cache bytes <= sample-cache budget;
- live and peak sample-cache windows <= the six fixed far-field levels, and
  each serialized peak must be at least its corresponding live value;
- runtime budget rejection count equals zero.

All serialized planetary budget, live-state, and telemetry fields in the
current `src/qa.rs` contract are type checked. Timing values must be finite and
non-negative, telemetry counters must be non-negative integers, signed cache
shifts/camera coordinates must remain integers, and state strings/booleans must
retain their declared types. If planetary streaming was disabled, serialized
caps are retained but the budget claim is `Blocked`, not promoted to `Passed`.

The generator does not invent absent budgets and does not turn a passed byte cap
into a visual-quality claim.

### `raw_observations.screenshots`

The screenshot observation contains:

- `reported_paths`: paths exactly serialized by the report;
- `referenced_files`: safely resolved canonical paths;
- `actual_files`: hashed PNG files present in the explicit run directory or
  explicitly and safely referenced beneath it;
- `unreferenced_files`: direct PNG children not named by the report.

Every reported path must resolve inside that explicit run directory. Absolute
paths, Windows-style relative paths, and repository-relative run paths are
accepted only when their canonical target remains inside the run. Parent
traversal, symlink escape, duplicate references, missing files, read failures,
invalid PNG signatures, and missing terminal `IEND` chunks are rejected.

PNG inspection proves only basic container completion. It does not decode the
image or replace perceptual inspection for clipping, UI overlap, terrain holes,
lighting defects, repeated structures, or other visual regressions.

## Bounded RON parsing

The generator includes a non-executing parser for the serde-generated subset of
RON used by QA reports. It does not use `eval`, invoke Rust, or shell out.

Fixed limits:

- report size: 4 MiB;
- nesting depth: 128;
- parsed nodes: 100,000;
- individual decoded string: 16,384 characters.

Supported structures include named-field tuples, tuples, lists, maps, strings,
booleans, integers, floats, `Some`, `None`, and unit enum identifiers. Duplicate
fields and unsupported syntax are rejected. Any parsed `NaN` or infinity rejects
the report before observations reach strict JSON serialization.

This is intentionally not a general-purpose RON implementation. A future QA
schema that uses unsupported RON features must update the parser and fixtures
before its evidence can be accepted.

## Required fixture tests

Run:

```powershell
python -B tools/artifacts/test_build_evidence_manifest.py
```

The suite covers:

- a current report with report/screenshot hashes and extracted budgets;
- legacy report blocking without reconstructed statistics;
- malformed RON with retained report byte hash;
- screenshot parent traversal that is never read or hashed;
- missing reported screenshot;
- unreferenced direct screenshot hashing;
- report symlink escape that is never read or hashed (when platform symlinks
  are available);
- unreferenced direct screenshot symlink escape and overall rejection;
- non-finite report values and strict JSON safety;
- malformed serialized Git provenance;
- route-frame rejection-count contradictions;
- active-route duration contaminated by screenshot write-tail time;
- negative current route timing;
- viewport physical-dimension type drift;
- missing planetary budget and telemetry fields;
- missing or stale QA report schema version, invalid Hydro modes, Hydro-disabled
  nonzero work, fluid scheduler/ECS mismatches, invalid observations, and fluid
  budget overflow;
- inconsistent desired/resident per-LOD material transition state and an
  unsupported surface-material mode;
- duplicate canonical run inputs;
- rejected input issue dominance beside an otherwise valid run;
- explicit input paths containing `..`;
- rejection of a directory named `latest`;
- protected output directories;
- successful atomic CLI write to one explicit safe output path;
- byte-stable output for fixed timestamp and identical explicit inputs.

## Downstream rules

DOCX, PDF, workbook, Sites, Visualize, and GitHub release workflows should:

1. consume one explicit manifest rather than scanning QA directories;
2. retain report and screenshot hashes in citations or evidence tables;
3. use raw observations as source values;
4. retain claim classifications and issues near affected claims;
5. distinguish Debug observations from Release gates;
6. refuse to turn `Blocked`, `Rejected`, or missing fields into zeros;
7. avoid averaging averages across runs;
8. never infer test totals not present in a separately hashed gate transcript;
9. rerender and visually inspect final artifacts on their delivery surface.

Test and gate transcripts are intentionally not synthesized by schema `1.0.0`.
They require an explicit, hashed input contract in a future schema revision.
