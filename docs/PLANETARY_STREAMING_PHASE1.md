# Planetary Streaming Phase 1B: Bounded Toroidal Far Field

## Problem, metric, and measured baseline

Voxel-Native currently represents view distance with complete `16 x 16 x 16`
chunks. The X/Z streaming frontier is a circular set of columns, and the only
existing `Macro` detail tier disables ambient occlusion while retaining the
same voxel and mesh representation. This means kilometres of visibility cannot
be obtained by raising render distance: work and memory still grow with area.

Two local Debug diagnostics captured before this phase observed:

| Effective render distance | Resident full chunks |
| ---: | ---: |
| 16 chunks (256 m nominal radius) | 7,204 |
| 23 chunks (368 m nominal radius) | 8,193 |

Those artifacts are not distributed and therefore cannot be verified from a
clean checkout; the values are retained only as a historical design input, not
release evidence. They predate the now-implemented dense-near hard ceiling. The
current streamer caps full chunks at 2,400, the interaction radius at 16
chunks, terrain tasks at 96, and mesh tasks at 64.

At the configured default render distance of 50, the integer circle contains
7,845 X/Z columns. With eight configured vertical slots, the current frontier
can consider as many as 62,760 full chunk slots before empty-column rejection.
At the shipping 15.36 km far radius, the same representation would contain
2,895,185 X/Z columns (up to 23,161,480 configured vertical slots) and is
therefore not a viable far-field representation.

Phase 1 succeeds when camera travel changes *which* terrain is sampled but not
the representation budget. Phase 1B additionally succeeds when an ordinary
lattice crossing samples only the entering strip instead of rebuilding every
procedural source sample in a ring. Far-field meshes must add no collider,
voxel storage, gameplay simulation, or per-cell ticking.

### Inputs, outputs, and measured rebuild baseline

The deterministic geometry/material key includes seed, world profile, scenery
quality, terrain grammar, far-surface material mode, hydro mode, semantic
cohort mode, and L0 topology mode. Signed 64-bit camera X/Z, the bounded
interaction radius, and the automatic pressure signal drive bounded scheduling
and coverage. The terrain output is exactly one complete finest fallback grid
plus five render-only annuli extending to 15.36 km; enabled hydro and semantic
cohort layers have separate entity and payload ceilings.
Authoritative voxels, collision, edits, and simulation remain in the near/mid
tiers; this module produces only descriptive horizon geometry and colour.

Before the toroidal change, nine independent native optimized-test runs of the
six-ring real terrain benchmark measured:

| Metric | Result |
| --- | ---: |
| Minimum | 112.016 ms |
| Median (p50) | 113.197 ms |
| Mean | 113.972 ms |
| p95 / maximum (nearest-rank, n=9) | 116.764 ms |
| Geometry per run | 30,358 vertices / 120,504 indices |
| Procedural work per run | 36,246 height / 2,469 biome queries |

All nine runs produced identical counts. Timing was measured on the checked-in
optimized test profile and is machine/load dependent; query counts and caps are
the structural regression metrics. Success requires byte-identical target
meshes from cold and incremental construction, a one-cell axial shift of only
one fixed entering strip, bounded teleport fallback, and no retained-state
growth across a 20,000 km synthetic route.

## Candidate approaches

### A. Nested geometry clipmaps (chosen)

One camera-centred finest parent and five square annuli double their sample
spacing at every level.
The topology is fixed, each level snaps on a world-aligned integer lattice, and
only a level whose lattice anchor changed is rebuilt. Coalescing dirty levels
means high-speed travel cannot create an unbounded queue. Outer-edge height
morphing, two-cell overlap, and outer-horizon skirts contain resolution seams.
The finest parent is complete while near meshes are late, then an exact fixed
finest-grid stencil removes each 16 m parent cell only after its one matching
16 m chunk column has current voxel data and settled or already-visible mesh
coverage. New removals wait for a 0.5 second stable observation so independently
finishing chunks coalesce; any loss of near coverage restores the parent
immediately. This asymmetric handshake prevents both a sky hole during boost
travel and coarse triangles cutting through settled voxel relief. It keeps CPU,
GPU, entity, stencil, and queue budgets explicit while reusing the exact
deterministic terrain height sampler. The cost is that distant caves,
overhangs, authored vertical structures, and edits are not represented yet.

### B. Screen-space-error quadtree

A quadtree can spend triangles only on visible high-relief areas and may render
fewer triangles in flat terrain. It was rejected for Phase 1 because node count,
allocation, and update work vary with camera direction and terrain complexity;
hysteresis and neighbour balancing are needed to prevent popping and T-junctions.
It is a useful later refinement behind the same fixed global budget, not the
safest first proof of bounded travel.

### C. GPU analytic displacement

A static grid displaced entirely in a shader has exceptionally low CPU update
cost. It was rejected because the current terrain generator includes authored
regional grammar and many CPU noise layers that do not exist in shader form.
Maintaining two generators would make near and far terrain disagree. A future
compute-generated height cache could revisit this without duplicating rules.

### D. Sparse voxel DAG / brick impostors

Hierarchical bricks can preserve caves, overhangs, structures, and edit
summaries much farther away than a height field. They are the intended
mid-field representation, but require persistent brick summaries, edit
propagation, material aggregation, and a dedicated mesher. Starting there
would couple too many systems before a constant-budget horizon is proven.

### Phase 1B update-path candidates

The `novel-solutions` review compared four update mechanisms against the
measured 112–117 ms whole-ring path:

1. **Toroidal GPU vertex/index remapping.** This can update only strips all the
   way through the GPU upload. It was deferred because Bevy's current `Mesh`
   asset path replaces complete attributes, and a shader/index indirection
   would introduce a second topology contract, seam risk, and renderer changes
   outside this module. It remains the next step if whole-attribute upload is
   proven to dominate after source sampling is removed.
2. **Fixed toroidal CPU source window (chosen).** Each level owns a fixed
   `65 x 65` height/biome window: 61 visible samples plus a two-cell halo for
   coarse morph and palette lookups. The cache moves into the sole worker (not
   cloned), rotates its logical origin in-place, and overwrites only samples
   entering the window. Mesh assembly remains deterministic and atomic. This
   is the smallest reversible change targeting the measured procedural-query
   bottleneck without touching renderer or terrain-generator ownership.
3. **Patch entities / sector meshlets.** Small patch meshes would permit true
   partial asset replacement, but either violate the six-entity invariant or
   require a new instanced/indirect render pipeline. More entities also expand
   culling and lifecycle state. Deferred until renderer-level meshlets have an
   explicit global budget.
4. **World-keyed LRU height tiles.** Cross-ring and revisit reuse could be high,
   but an LRU adds hash lookup, eviction order, and travel-history state. Even a
   nominal cap makes determinism and stale-world invalidation harder than the
   per-level fixed window. Rejected for this phase because adjacent-strip reuse
   already captures the common case with zero travel growth.

## Chosen representation and invariants

The incremental dataflow is:

`64-bit target anchor -> move level cache into worker -> i128-safe shift ->`
`rotate toroidal origin -> sample entering strips -> assemble local-f32 mesh ->`
`world/spec stale gate -> atomic mesh+cache install`.

The old mesh is never removed while work is pending. A stale visible result is
discarded before asset mutation; a stale cache may be retained only when its
world key still matches, because it is non-visible deterministic source data
for the next coalesced target. A seed/profile mismatch discards both. A large
teleport refills the same fixed window instead of allocating travel history.

| Hard cap | Value | Enforcement |
| --- | ---: | --- |
| Far entities | 6 | one complete finest parent plus five annuli |
| Resident vertices / indices | 35,000 / 150,000 | pre-install rejection |
| Resident generated mesh payload | 2,280,000 bytes | explicit payload check |
| One ring result | 6,000 vertices / 25,000 indices / 388,000 bytes | pre-install rejection plus topology sweep |
| Builds in flight | 1 | single task slot and six-bit dirty coalescer |
| Sample windows | exactly 6 across resident storage and the sole worker | cache ownership moves to the worker; incompatible retargets refill the same allocation in place |
| Sample-cache payload budget | 512 KiB | compile-time type-size assertion and telemetry |
| Near-coverage working set | 1,545 bytes | 33 x 33 readiness bits-as-bools plus 3,600-bit parent mask; compile-time <= 2 KiB assertion |

The mesh byte figures count generated position, normal, colour, UV, and `u32`
index payloads. Engine allocator metadata and renderer-owned copies are not
claimed as zero; GPU-residency profiling remains part of live QA.

- Six levels use 60 x 60 cells (61 x 61 possible top vertices).
- Shipping level spacing is `16, 32, 64, 128, 256, 512` metres.
- Their outer extents are `0.48, 0.96, 1.92, 3.84, 7.68, 15.36` km.
- The finest level is a complete fallback until the near streamer proves
  individual current-request columns. A 3,600-bit stencil hides a 16 m parent
  cell only when its exactly corresponding 16 m column is covered. Missing data,
  coordinates outside the fixed 33 x 33 window, and unrepresentable coordinates
  fail closed to a visible parent. Newly covered cells publish only after 0.5
  stable seconds; lost coverage publishes immediately. Each later ring overlaps
  the previous level by two of its own cells.
- Integer world anchors and Euclidean snapping keep negative coordinates
  deterministic and make the mesh compatible with a future floating origin.
  Mesh vertices remain local to their ring entity instead of embedding large
  world coordinates in vertex buffers.
- Fine outer vertices morph toward bilinear samples of the next coarser global
  lattice. Skirts exist only on the finite outer perimeter; an inner-hole skirt
  would become a kilometre-scale wall in front of a camera inside the annulus.
- Authored biome swatches are treated as sRGB art values and converted to
  linear albedo before PBR lighting. A 0.5 swatch therefore contributes about
  0.214 linear energy instead of clipping the daylight landscape toward white.
- One async build exists at a time. Dirty requests are a six-bit coalescing
  mask, so even extreme flight speed cannot allocate a growing work queue.
- Each level has one fixed 65 x 65 toroidal source window. A one-cell axial
  move issues exactly 65 new height queries; a diagonal move issues 129; all
  other 4,160 or 4,096 samples are reused. A shift of 65 cells or more takes a
  full 4,225-sample fallback in the same allocation. Ownership moves between
  resident storage and the sole worker: even an incompatible world/spec refill
  reuses that window in place, so there is no transient seventh allocation.
- Runtime pressure never shortens the horizon. Once all rings are resident,
  automatic pressure policy first changes refresh cadence from one to two or
  four frames and skips optional biome-colour queries on necessary rebuilds.
  Old silhouettes remain visible while refresh work waits; no user tuning is
  required. Telemetry exposes the cadence, palette tier, dirty mask, backlog,
  and deferred-frame count so Mission Control can prove that policy live.
  Cache mode, cell shift, new/reused height and biome samples, mesh bytes, and
  current/peak cache population are also exposed for live proof rather than
  inferred. The exact fields are `live_sample_cache_windows`,
  `live_sample_cache_bytes`, `peak_live_sample_cache_windows`, and
  `peak_live_sample_cache_bytes`; both current and peak values are checked
  against six windows and 512 KiB. `last_bridge_v2_cell_reuses` separately
  proves how many visible material vertices reused an already sampled fixed
  cell in the latest build.
- The far field has PBR mesh components only: no collider, rigid body, voxel
  chunk, navigation node, vegetation simulation, or shuttle-force component.
- Default rollout is Astral-only and can be disabled with
  `VOXEL_NATIVE_PLANETARY_STREAMING=off`; `all` enables the Natural profile.
  This keeps established Natural visuals reversible until visual QA passes.

### Far-surface material bridge and rollback contract

`BridgeV2` is the shipping default for new far-ring builds. Each visible top
vertex maps by Euclidean absolute world coordinates to a fixed 64 m material
cell. The first vertex in a cell resolves the biome and its canonical base
block family; neighbouring vertices in that cell reuse the result. This is
independent of clipmap level, anchor, and retarget, performs no material-slope
height queries, and introduces no map, heap, entity, task, or cache window.

The categorical speedup is deliberately not described as exact near-surface
parity. A representative can be 32 m away along either axis, so transitions
can become broader or visibly blocky and slope-specific Dirt, Stone, and
Limestone accents are omitted. Geometry normals still carry relief into PBR
lighting, but do not change the selected family. Natural and Astral visual A/B
inspection across anchor/LOD transitions is therefore still a release gate.

The non-persisted environment gate is
`VOXEL_NATIVE_FAR_SURFACE_MATERIAL`:

| Mode | Accepted values | Purpose |
| --- | --- | --- |
| `BridgeV2` (default) | unset, `1`, `on`, `bridge`, `bridge-v2`, `bridge_v2`, `v2`, `fast`, and unknown values | fixed-cell, slope-query-free shipping path |
| `BridgeV1` | `bridge-v1`, `bridge_v1`, `v1`, `exact`, `exact-slope` | exact one-metre near-terrain slope-family diagnostic/reference path |
| `LegacyPalette` | `0`, `off`, `legacy`, `legacy-palette`, `legacy_palette` | former interpolated-palette visual rollback |

The selected mode participates in the world key, so switching it cannot reuse
a mesh or sample window authored under another material interpretation. It is
not written to save data.

### Shipping 16 m refinement, rationale, and rollback boundary

The current patch halves the finest terrain spacing from the legacy 32 m value
to 16 m and halves the BridgeV2 material-cell quantum from 128 m to 64 m. It
does **not** add a ring or enlarge a lattice: all six 60 x 60-cell levels, the
61 x 61 sample grids, geometry ceilings, six cache windows, single worker, and
coalesced job limits remain unchanged. The consequence is a 15.36 km
L-infinity axis half-extent instead of the legacy 30.72 km half-extent.

This trade is intentional. A legacy L0 cell crossed four independently ready
Near columns, so one late column could retain a coarse triangle over three
ready columns. Matching the 16 m Near-chunk footprint makes the handoff 1:1:
one proven column removes exactly one fallback cell, including across negative
coordinates through Euclidean division. The 64 m material quantum preserves
the same four-finest-cell classification cadence while reducing the maximum
distance to the sampled cell centre. The patch targets visible near/far walls
and over-broad categorical transitions; it is not claimed to increase the
established population or work budgets.

The compile-time rollback boundary is this refinement patch as one unit: the
base-step/material-cell constants, L0 handoff tests, and L5 semantic-alignment
assertions must stay mutually consistent. Reverting the unit restores the
legacy reach but also restores the four-column L0 handoff and therefore
requires a fresh build plus full structural and visual validation. Operational
rollback remains `VOXEL_NATIVE_PLANETARY_STREAMING=off`; neither path changes
save authority or serialized world data. Native Natural/Astral screenshots and `report.ron`
evidence for the refined shipping geometry are still pending, so the patch is
structurally verified but not yet visually accepted.

## Proof and validation plan

All four checked-in ignored benchmarks were run locally in the optimized test
profile before the 16 m shipping refinement. The distributions below remain
historical evidence for the bounded toroidal algorithm; they are not current
performance acceptance for the refined metric spacing. Nine post-toroidal,
pre-refinement cold six-ring runs measured:

| Metric | Before | Toroidal source window | Change |
| --- | ---: | ---: | ---: |
| Minimum | 112.016 ms | 84.434 ms | -24.6% |
| p50 | 113.197 ms | 85.913 ms | -24.1% |
| Mean | 113.972 ms | 85.968 ms | -24.6% |
| p95 / max | 116.764 ms | 88.029 ms | -24.6% |
| Height queries | 36,246 | 25,350 | -30.1% |
| Biome queries | 2,469 | 2,469 | unchanged cold |

This cold result is faster because the halo window deduplicates coarse-morph
height queries even when no previous cache exists. Geometry remains exactly
30,358 vertices / 120,504 indices. The earlier palette optimization lineage is
also retained: the first exact-colour version took 168.612 ms and 22,326 biome
queries before the bounded bilinear palette reduced that to 2,469.

The final material-mode comparison used 25 cold, optimized level-1 builds per
mode. Build order rotated each iteration to avoid consistently giving one mode
the warmer instruction/data-cache position:

| Far-surface mode | p50 | p95 | Interpretation |
| --- | ---: | ---: | --- |
| `LegacyPalette` | 5.270 ms | 5.859 ms | former interpolated-palette rollback baseline |
| `BridgeV1` | 18.834 ms | 20.685 ms | exact one-metre slope-family diagnostic; too costly as the default |
| `BridgeV2` | 5.364 ms | 5.944 ms | legacy fixed-cell categorical path; shipping quantum is now 64 m |

These are CPU construction distributions on the tested machine, not universal
frame-time promises. BridgeV2 is within 1.8% of the Legacy p50 and 1.5% of its
p95 in this distribution while removing all material-slope queries. The
structural result is the fixed query/work contract; the small timing difference
can vary with host load. Performance does not approve appearance: the broader
fixed-cell transitions still require the Natural/Astral visual A/B below.

The legacy one-L0-cell diagonal route produced 94 incremental ring jobs per run.
Across nine complete benchmark invocations every run issued exactly 8,158 new
height and 11,935 biome queries. Per-job distributions stayed within:

| Incremental job metric | Nine-run observed range |
| --- | ---: |
| Minimum | 0.662–0.685 ms |
| p50 | 0.815–0.874 ms |
| p95 | 1.792–1.927 ms |
| Maximum | 2.792–3.675 ms |

Three teleport benchmark invocations (nine six-ring, 20,000-km-class batches
per invocation) measured p50 82.231–84.973 ms and p95/max 88.684–89.397 ms.
Every batch used the same 25,350 height-query full-window fallback and retained
the then-current pre-refinement extent. Teleport fallback is intentionally
slower than a strip, but fixed: it allocates no path history and cannot enqueue
more than one job.

These figures are CPU worker construction times, not render-frame or GPU upload
times. Runtime performs builds one-at-a-time off the main thread. Whole mesh
attribute assembly/upload still occurs after strip-only source sampling; live
profiling must determine whether renderer-level partial updates are worthwhile.

An exhaustive topology sweep across every supported first-ring hole size found
at most 31,062 resident vertices and 121,104 resident indices, below the hard
35,000 / 150,000 limits. At the shipping 15.36 km radius, a full-chunk circle
would contain 2,895,185 horizontal chunk columns (up to 23,161,480 configured
vertical slots), while the chosen representation remains six render entities.

The focused planetary-streaming suite pins the topology and byte budgets,
cold/incremental byte equality, exact axial and diagonal strip counts,
deterministic replay, signed/extreme coordinates,
fail-closed stale tokens, pressure policy, profile gate, morph continuity,
coalesced queue bound, exact irregular coverage, expansion batching with
immediate loss recovery, 1-km multi-level reuse, bounded teleport fallback, and
fixed allocation/work over a 20,000-km route. The suite also pins all three
material-mode aliases/defaults, BridgeV1's exact query ceiling, BridgeV2's
fixed-cell LOD/anchor stability and reuse counts, byte-identical incremental
rebuilds, incompatible in-place cache refill, and current/peak six-window
telemetry under the 512 KiB cap. A headless compile/test run validates the Bevy
integration.

### Unbundled historical diagnostics

Earlier local Debug captures helped expose coarse parent sheets crossing voxel
relief and showed that time-based coverage stabilization reduced discarded
work. They predate the current BridgeV2 material path and 16 m refinement, and
their screenshots and reports are not distributed. Consequently they are
historical observations—not reproducible proof, current visual acceptance, an
FPS promise, or evidence suitable for a public artifact.

The old captures also rejected a stronger art claim: their pre-refinement
surface was visibly pastel and smooth beside detailed voxel mountains. Whether
the current shipping surface resolves that objection must be decided from fresh
same-binary Natural and Astral evidence. Before broad rollout:

1. Inspect all six compass directions and a top-down view at 0, 5, and 25 km.
2. Fly across multiple 16/32/64 m anchor transitions and look specifically for
   cracks, z-fighting, horizon pumping, and dark skirts.
3. Record resident far entities/vertices, build latency, queue mask, full chunks,
   FPS, and frame stalls during a 30 km high-speed route.
4. Repeat at Astral dawn/noon/night and Natural only when explicitly enabled.

Known Phase-1 limits are intentional: the far field is a terrain surface with
optional bounded Hydro and semantic presentation layers, not a replacement for
mid-field bricks, sparse edit summaries, authored distant structures, detailed
shorelines, vegetation LODs, or simulation bubbles. Those systems should layer
onto this constant-budget horizon instead of expanding the full-chunk frontier.

## Primary technical references

- Asirvatham and Hoppe, [Terrain Rendering Using GPU-Based Geometry
  Clipmaps](https://hhoppe.com/proj/gpugcm/): nested regular grids,
  incremental viewer-relative shifts, and outer-boundary morphing. The paper
  motivates the structure; Voxel-Native's current implementation intentionally
  keeps terrain generation on CPU so near/far rules do not diverge.
- Bevy 0.14.2, [`Mesh` API](https://docs.rs/bevy/0.14.2/bevy/render/mesh/struct.Mesh.html):
  the current asset integration inserts complete position, normal, colour, UV,
  and index arrays. This is why partial GPU-buffer mutation remains a measured
  follow-up rather than an assumption in Phase 1B.
