# Voxel-Native Evidence Manifest Schema

Status: schema `1.6.0`

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

Every QA run must be supplied explicitly. The path variables below are
placeholders for newly generated, reviewed runs; the repository does not ship
the historical local `qa_runs/` tree:

```powershell
$runA = '<path to accepted run A>'
$runB = '<path to accepted run B>'
$runC = '<path to accepted run C>'

python -B tools/artifacts/build_evidence_manifest.py `
  --qa-run $runA `
  --qa-run $runB `
  --qa-run $runC `
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
| `qa_run_directories` | string array | Stable repository-relative list of accepted explicit run paths. |
| `selection_policy` | string | Fixed declaration that selection was explicit and non-scanning. |

### `file_hashes[]`

Every successfully read evidence file has:

| Field | Type | Meaning |
|---|---|---|
| `kind` | enum | `report`, `screenshot`, or `generator_source`. |
| `path` | string | Repository-relative path. External runs are rejected before inspection so public manifests cannot serialize workstation paths. |
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
  world_edit_store
  viewport
  route
  route_frame_times
  planetary_streaming
  screenshots
```

`report_schema_variant` is:

- `current`: `qa_report_schema_version` is exactly `2.6.0`; all missing or
  contradictory current fields then fail through their own explicit checks;
- `legacy`: the report has no schema identity or is exactly `2.0.0`, `2.1.0`,
  `2.2.0`, `2.3.0`, `2.4.0`, or `2.5.0`; its
  historical observations remain inspectable, but every publishable claim is
  `Blocked` and no current field is inferred;
- `unsupported`: the report names any other schema version, including a future
  version; the run is `Rejected` until the parser and validators are explicitly
  upgraded;
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
- `terrain_grammar`
- `git_sha`
- `git_dirty`
- `source_fingerprint`
- `executable_hash`
- `toolchain`
- `hardware`

`package_version`, derived `build_profile`, and `terrain_grammar` are required
for the current identity claim. Terrain grammar is exactly `V1`, `V2`, or `V3`
and is part of the immutable generation identity alongside seed, world profile,
and scenery quality. The remaining provenance fields are optional, but any value
that is present must have the serialized type promised by `src/qa.rs`.

A legacy report without `build_profile` is `Blocked`; the generator never
guesses Debug or Release from FPS, paths, timestamps, or naming conventions.

### `raw_observations.world_edit_store`

QA schema 2.4 records the edited-voxel snapshot authority separately from the
world metadata. The observation preserves:

- `world_edit_store_status`: exactly `unchecked`, `compatible`, or `blocked`;
- `world_edit_store_compatible`;
- `world_edit_store_seed`;
- `world_edit_store_profile`;
- `world_edit_store_scenery_quality`;
- `world_edit_store_terrain_grammar`;
- `world_edit_store_edited_chunks`;
- `world_edit_store_block_reason_code`.

A `compatible` store requires `compatible=true`, a non-negative edited-chunk
count, no block reason, and an exact four-field identity match to
`run_identity`: seed, profile, scenery quality, and terrain grammar. A
`blocked` store requires `compatible=false`, no edited-chunk count, one bounded
reason code, and the same exact identity match. An `unchecked` store is the
closed empty sentinel: compatibility is false; count and reason are null; and
all four store-identity fields are null. Contradictory combinations are
`Rejected`; coherent blocked or unchecked state remains `Blocked`.

Artifact consumers accept only a current `compatible` store and repeat its
identity and edited-chunk count instead of inferring safety from an empty
directory, a world name, or a grammar default.

### `raw_observations.viewport`

The following values must all be finite and positive:

- `logical_width`
- `logical_height`
- `physical_width`
- `physical_height`
- `scale_factor`
- `base_scale_factor`
- `dpi_percent`

Physical width/height must retain their serialized unsigned-integer shape.
Logical size multiplied by the effective `scale_factor` must agree with
physical size within the one-pixel rounding boundary. For current 2.6 reports,
`base_scale_factor` is required and records the OS/window-backend ratio before
any application override; `dpi_percent` must agree with
`base_scale_factor * 100`. This allows exact-pixel evidence to use an effective
scale of 1.0 on a 200% desktop without falsifying the OS DPI as 100%.

Exact legacy 2.5 reports retain their historical
`dpi_percent == scale_factor * 100` interpretation and may omit
`base_scale_factor`. That compatibility is confined to the legacy path: the
run remains `Blocked` and cannot be relabeled or inferred as current 2.6
evidence.

The manifest records one viewport per run. It does not infer completion of the
full responsive viewport/DPI matrix.

### `raw_observations.route`

The route observation preserves:

- `requested_route_focus`
- `resolved_route_focus`
- `route_focus_available`
- nullable `route_focus_unavailable_reason`
- nullable signed-integer `[x, y, z]` `route_focus_anchor`
- nullable actual `route_focus_search_visited_candidates` and
  `route_focus_classification_queries`
- non-negative candidate/classification hard caps
- `route_focus_search_cap_exhausted`
- `camera_route_policy` and `camera_route_preflight_applicable`
- nullable camera plan hash, selected variant, unavailable reason, and minimum
  clearance
- camera availability, variant/sample/query limits, actual query work, exact
  XYZ request-resolution accounting split into loaded, proven-air, and
  unavailable checks, candidate occlusion diagnostics, selected-clear samples,
  and work-cap exhaustion
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

An available requested focus must resolve to itself, have no unavailable
reason, and report no exhausted search. An unavailable focus is `Blocked`, must
name a real fallback route and bounded reason, and may claim exhaustion only
when a known actual counter exactly reached its cap. Optional actual counters
may be null when the upstream terrain API does not yet expose that work; null
is preserved rather than replaced by zero. Any known actual count above its
serialized cap is `Rejected`.

Available `waypoint`, `river`, `lava`, and `near-far` focuses are spatial
claims and therefore require a non-null three-integer anchor. Their requested
focus must also agree with `run_identity.world_profile`: `river` requires
`Natural`; `waypoint` and `lava` require `AstralFrontier`; and `near-far`
accepts either supported profile. Missing anchors, missing profiles, and
incompatible profiles are `Rejected`, so a consumer cannot publish an
otherwise well-shaped but impossible route/world pairing.

Schema 2.4 distinguishes camera-preflight applicability from success. It
applies exactly to requested `river`, `lava`, and `near-far` routes. For those
routes, the policy is `preflight-v1`, applicability and availability are true,
the plan hash is exactly 16 lowercase hexadecimal digits, the selected variant
is in `0..7`, and v1 is exactly eight variants, sixteen validation samples, and
a 153,600 voxel-query cap. Actual query work is positive and strictly below
that cap. Each voxel query checks its exact owning XYZ chunk against the current
streaming request. The result is either resident
(`camera_route_loaded_chunk_checks`), scheduler-proven procedural air
(`camera_route_proven_air_chunk_checks`), or unavailable
(`camera_route_unloaded_chunk_checks`). Proven air is not reported as resident:
it requires the streamer's conservative cached column ceiling above the queried
chunk and no edit override for that exact chunk. The invariant is
`required == voxel_queries == loaded + proven_air + unloaded`; an available
route additionally requires `unloaded == 0`. These counters count checks, not
unique chunk IDs.

`camera_route_selected_clear_samples == camera_route_validation_samples == 16`
binds the selected plan to all clear samples. Candidate body/LOS occlusion
counters cover rejected alternatives and are diagnostic unsigned integers;
each is bounded by `variant_count * validation_samples` (128) but need not be
zero. Minimum clearance is positive, no unavailable reason exists, and the
work cap is not exhausted. A coherent unavailable applicable route is
`Blocked`; contradictory state is `Rejected`.

If focus resolution fails before preflight starts, an applicable route may use
the closed reason `camera-route-focus-unavailable` with an unavailable focus,
no plan/index/clearance, the complete zero-work camera sentinel, and no exhausted
work cap. That state is `Blocked`, never Observed camera evidence.

For `scenic`, `waypoint`, and `streaming`, applicability and availability are
false, plan hash/index/clearance/reason are null, voxel queries and all four
required/loaded/proven-air/unloaded counters (as well as the remaining preflight
counters and caps) are zero, and work-cap exhaustion is false. This exact
sentinel is valid current Observed evidence, but it is not camera-preflight
proof. Generic Python and JavaScript consumers accept it without promoting it.
Obsolete `*_columns`, ambiguous `*_chunks`, and old unqualified
body/LOS-occlusion field names are rejected rather than silently
reinterpreted.

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

QA report schema `2.6.0` is the current contract with distinct effective and
OS/window-backend viewport scale factors, immutable terrain-grammar identity,
edit-store compatibility, route-resolution truth, visibility-aware camera-plan
evidence for applicable Hydro routes, per-kind Far Hydro evidence, Far Semantic
Cohorts v1, and the exact combined resident-plus-in-flight dense chunk budget.
Exact `2.5.0`, `2.4.0`, `2.3.0`, `2.2.0`, `2.1.0`, and `2.0.0`
reports remain readable historical evidence but are classified `legacy` and
`Blocked`; field presence or zero defaults never reinterpret them as current.
Any other named version is unsupported and rejected fail-closed.

`desired_terrain_grammar` must be exactly `V1`, `V2`, or `V3` and equal
`run_identity.terrain_grammar`. When planetary streaming is enabled,
`active_terrain_grammar` must equal that desired grammar; when disabled, active
grammar must be null. The far-field `profile` likewise equals the immutable
run identity profile. This prevents a worker or cached ring built under one
grammar from being reported as evidence for another.

Near-field evidence is current only when `dense_chunks` equals
`loaded_chunks + pending_terrain`, `dense_chunk_budget` is exactly `2400`, the
current and peak totals remain at or below that limit, and
`dense_chunk_budget_exceeded` is false. The final frontier must be complete
with zero pending terrain, pending meshes, and dirty chunks. Independent peak
component values are not substituted for `peak_dense_chunks`, because their
maxima may occur on different frames.

The `resident_*` and `ring_*` live fields are post-`apply_deferred` Bevy ECS
observations. Matching `scheduler_resident_*` and `scheduler_ring_*` fields
retain the streamer's bookkeeping separately. A current evidence run is
rejected unless both representations match exactly and the bounded observer
reports: valid, no seventh-entity overflow, no duplicate/out-of-range levels,
no budget violation, and zero rejection episodes.

Far Hydro truth is separate from the established terrain truth:

- `hydro_mode` is exactly `Disabled` or `DescriptiveV1`;
- `resident_fluid_*` and `fluid_ring_*` are post-`apply_deferred` ECS values;
- `resident_water_indices` plus `resident_lava_indices` must equal total fluid
  indices; the same identity holds per LOD and for scheduler copies;
- every water/lava count is divisible by six, proving complete top-face quads;
- `scheduler_resident_fluid_*` and `scheduler_fluid_ring_*` are independent
  scheduler bookkeeping and must match the observed values exactly;
- `resident_fluid_observation_valid` must be true, while fluid overflow,
  duplicate-slot, out-of-range, scheduler-mismatch, budget-exceeded and
  rejection counters must all report the clean state;
- `resident_fluid_kind_integrity_valid` must independently be true;
- `budget_fluid_*`, `budget_fluid_ring_build_bytes`, and
  `budget_hydro_atomic_ring_build_bytes=653008` retain the Hydro-only worker
  contract; `budget_atomic_ring_build_bytes=757984` is the larger combined
  terrain + Hydro + optional-cohort ceiling;
- `last_fluid_classification_queries`, `last_fluid_biome_queries`,
  `last_fluid_vertices`, and `last_fluid_indices` record bounded latest-work
  observations, not a performance or visual-quality promise.

When `hydro_mode` is `Disabled`, all fluid ECS/scheduler populations, per-ring
arrays, water/lava categories, and latest-work counters must be zero. This fail-closed relationship
prevents an off/on transition or stale result from being mislabeled as a clean
rollback run.

Far Semantic Cohorts are a separately gated render-only L5 layer:

- `semantic_cohort_mode` is `Disabled` or `SilhouettesV1`;
- the exact v1 budgets are one entity, 81 candidates, 1,944 vertices, 2,916
  indices, 104,976 mesh bytes, 3,721 hash scans, 81 height queries, and 81 biome
  queries;
- six-element kind arrays have the fixed order `NaturalGrove`, `NaturalKarst`,
  `NaturalMesa`, `AstralCrystal`, `AstralBasalt`, `AstralReef`;
- kind counts sum exactly to cohort count; each emitted cohort contributes 24
  vertices and 36 indices; mesh bytes are recomputed as `vertices * 48 +
  indices * 4`;
- observed populations and per-kind counts equal scheduler copies exactly;
- `resident_semantic_cohort_observation_valid` and
  `resident_semantic_cohort_payload_integrity_valid` must be true, while
  overflow, mismatch, budget, and rejection indicators remain clean;
- the latest emitted count cannot exceed latest candidates, and latest work is
  bounded by its serialized caps;
- `Disabled` requires every live, scheduler, latest-work, and per-kind value to
  be zero. `SilhouettesV1` permits a zero latest record before L5 is first
  built; a completed L5 record scans exactly 3,721 cells and performs one
  height plus one biome query per candidate.

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
- exact 2.5/2.4/2.3/2.2/2.1/2.0 legacy blocking and rejection of unsupported
  older/future schemas;
- exact current/peak dense-residency accounting, the fixed 2,400-chunk budget,
  overflow/rejection truth, and final near-field settlement;
- required V1/V2/V3 terrain grammar, exact edit-store identity/status invariants,
  and desired/active far-grammar agreement;
- contradictory requested/resolved route truth, unavailable-route blocking,
  optional search work, and cap overflow;
- current camera applicability, exact non-applicable sentinel acceptance,
  missing/invalid plan binding, selected-clear-sample truth, exact XYZ
  chunk-check accounting, candidate-occlusion bounds, and obsolete field
  rejection;
- water/lava total and per-ring integrity, cohort kind/geometry/byte integrity,
  exact v1 budgets, disabled-mode zero work, and Python/JavaScript consumer
  tamper rejection;
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

Test and gate transcripts are intentionally not synthesized by schema `1.6.0`.
They require an explicit, hashed input contract in a future schema revision.
