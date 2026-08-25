# World Continuum Integration v1

Status: pure, fixed-budget adapter implemented in
`src/world_continuum.rs`; compile-registered as a public pure module in
`main.rs`, but intentionally not installed as a runtime plugin and not
consumed by the live terrain, renderer, editor, physics, or save systems.

## Outcome and authority boundary

The adapter gives Continuum Morphogenesis, Virtual Voxel Hierarchy (VVH), and
the bounded implicit-shape prototype one common, versioned world identity. It
does **not** pretend that their payloads mean the same thing:

| Representation | Meaning retained by the adapter | Meaning it may not acquire |
| --- | --- | --- |
| Continuum Morphogenesis | descriptive height envelope, derived surface-family histogram, functional-guild histogram | block material, exact occupancy, collision, edit or placement permission |
| VVH observation | original `u16` material, quantized occupancy/error and exact `BrickStamp` | inferred macro ecology, terrain/save authority or an adapter-owned cache write |
| explicit implicit feature | conservative AABB classification for an explicitly identified sphere/ellipsoid | automatic biome-generated shape, block material, edit ownership or physics |

Correlation results therefore hold these values side by side. No averaging,
majority vote, or implicit conversion can silently turn one representation
into another representation's authority.

The implementation performs no I/O, has no Bevy dependency, launches no task,
and mutates no live world. Its only retained allocation is the fixed boxed slot
array created at construction; that slice has no capacity and cannot grow
during travel.

## Baseline read before design

The three existing APIs have materially different budgets and contracts:

| Existing module | Relevant measured/bounded baseline |
| --- | ---: |
| `continuum_morphogenesis` | 32 x 32 cells, 33 x 33 shared vertices, 8.192-km tile, 66,488-byte output, 60,552-byte scratch, <=785,453 accounted generation work units, zero retained generator bytes |
| Morphogenesis warm generation | p50 3.506 ms and p95 3.603 ms in its 32-sample acceptance record |
| `virtual_voxel_hierarchy` | four-byte summary cell, 2,048-byte 8^3 brick payload, 512 production-resident bricks, 1,093,632-byte accounted cache ceiling |
| `implicit_voxels` | one conservative sphere AABB classification around 25 ns; oriented ellipsoid around 137 ns in its 30-run acceptance record; adaptive microvoxel builds separately capped at 4,096 nodes |

Important semantic gaps were also observed:

- Morphogenesis has no authoritative voxel `MaterialId`; it has causal visual,
  soil, water, vegetation and guild fields.
- VVH `overlay_version` is per brick. It cannot be replaced with one invented
  global overlay number.
- `WorldVoxel` does not formally declare a metre conversion.
- implicit value types contain geometry but no world epoch, stable feature ID,
  or feature revision.
- an arbitrary implementation of `ConservativeImplicitVolume` is not required
  by that trait to have bounded work.

The adapter makes each missing contract explicit instead of guessing it.

## Candidate comparison

At least four adapter shapes were evaluated before implementation.

### 1. Borrow-only facade with regeneration on every miss

This would retain almost zero memory: generate a `ContinuumTile`, sample VVH,
classify a shape and immediately discard everything. It has a clean ownership
story, but the existing macro generator's p50 is about 3.5 ms. Repeating that
work for ordinary samples would be much more expensive than a summary lookup,
and a discarded result cannot provide a resident Far parent while Mid/Near
work completes.

Decision: rejected as the regular sampling path. Stateless generation remains
the reconstructible source behind a bounded cache miss.

### 2. One universal cell payload

This would pack height, one material, occupancy, one guild and an implicit sign
into one convenient struct. It is compact, but fundamentally lossy:

- one majority guild erases minority guilds;
- a heightfield cannot prove 3D volume occupancy;
- a VVH block material cannot be inferred from soil/moisture heuristics;
- an implicit `Inside` result is not permission to overwrite authored voxels.

Decision: rejected. Convenient representation unification is not semantic
world unification.

### 3. Fixed direct-mapped macro pyramid plus stamped sidecars (chosen)

Eight slots each contain fixed Far, Mid and Near arrays. A tile is first
published at Far, then may promote Mid and Near. Requests automatically fall
back only toward an already valid coarser parent. VVH and implicit data are
returned as separately typed, exactly stamped observations beside the macro
sample.

Strengths:

- exact compile-time bytes and reduction work;
- one slot probe per sample;
- no camera-distance growth;
- no independent per-band seed;
- no child-only eviction hole;
- read-only coupling to VVH lifecycle and no coupling to edit/cache methods;
- easy rollback because slots are reconstructible and unpersisted.

Trade-off: direct mapping is deliberately simple and deterministic, but two
coordinates can collide and evict one complete tile even when another slot is
unused. Eight slots are an integration prototype, not a claim of sufficient
live-world coverage.

### 4. Shared global octree/DAG or persistent content-addressed cache

This could deduplicate repeated summaries and give richer spatial lookup. Its
allocation count depends on world entropy and travel history; identity,
parent/child replacement, edit projection, garbage collection and save
migration would become part of the first adapter.

Decision: deferred. It may be investigated as a separately bounded cache, not
as world authority and not until the simple parent/fallback contract has live
evidence.

## Common world identity

Every accepted operation carries:

```text
(adapter_schema_version,
 morphogenesis_grammar_version,
 world_id,
 seed,
 morphogenesis_profile,
 world_epoch,
 source_revision,
 voxels_per_macro_cell)
```

`world_id` separates worlds that intentionally share a seed. `seed` is the
only macro-generation seed; callers cannot pass a separate Near, Mid or Far
seed. The profile preserves Natural/Astral causal behavior. The integer voxel
scale is explicit because the current VVH does not specify physical units.

Within one epoch, stable world fields may not change and the source revision
may only advance. An older epoch or revision is rejected. A different seed,
profile, world ID, grammar or scale under the same epoch is an identity
conflict. A newer epoch may establish a new world identity and logically
invalidates all eight slots in fixed work without rewriting their large stale
payload arrays.

An asynchronous worker receives a `MacroTileTicket`. The ticket privately owns
the exact identity and coordinate and exposes no extra seed parameter. Its
generated result has private fields. Finite-value validation and the full
66-KiB tile fingerprint happen once when that worker result is constructed;
staged Far/Mid/Near publication validates the immutable envelope rather than
rescanning it three times.

## Conservative MacroTile pyramid

All three bands reduce the same private `GeneratedMacroTile` fingerprint:

| Band | Stride | Summaries/tile | Source cells/summary | Fixed reduction work |
| --- | ---: | ---: | ---: | ---: |
| Far | 4 | 64 | 16 | 2,624 units |
| Mid | 2 | 256 | 4 | 3,328 units |
| Near | 1 | 1,024 | 1 | 5,120 units |

One work unit is one source-cell classification or one shared-height-vertex
read. It is an algorithmic bound, not a CPU-cycle claim.

Each 44-byte summary contains:

- minimum and maximum over every shared height vertex and every source-cell
  height in its region;
- deterministic area mean of the source-cell heights;
- counts for all five descriptive surface families;
- counts for all nine functional species guilds;
- the exact number of represented source cells.

The vertex envelope is conservative for a bilinear surface drawn through the
sampled shared lattice. The mean is descriptive, not a collision plane.
Histograms retain minority categories. Dominant-family and dominant-guild
helpers break equal counts by stable enum order, but consumers can always read
the complete counts.

### Descriptive surface classifier

Morphogenesis does not produce block materials. Version 1 therefore exposes a
clearly named, adapter-owned *descriptive surface family*, with fixed ordered
rules:

1. Astral profile -> `AstralCrystal`;
2. routed water >=0.8 or moisture >=0.82 -> `WetSediment`;
3. soil depth <=0.25 m or slope grade >=0.75 -> `ExposedSubstrate`;
4. vegetation potential >=0.45 with a non-bare/non-pioneer/non-alpine guild ->
   `BiogenicTopsoil`;
5. otherwise -> `Regolith`.

These are art-direction classifications tied to adapter schema v1. They are
not a geological survey and never replace `CellSummary.material`.

## Parent-first publication and pressure behavior

The only valid per-slot state transitions are:

```text
vacant -> Far -> Far+Mid -> Far+Mid+Near
```

Promoting Mid without Far fails. Promoting Near without Mid fails. A Near
request may receive Mid or Far; a Mid request may receive Far; no request is
served by a finer band than it requested. A direct-map collision replaces the
entire slot with the new Far result, so it cannot retain an orphaned Mid/Near
child from the evicted coordinate.

This is local data-layer continuity. Live geometry morph bands, cross-fades,
renderer residency, GPU upload completion and edit silhouette projection are
still separate Level-4 release gates.

## Fixed memory and travel proof

| Budget | Fixed value |
| --- | ---: |
| summary size | 44 bytes |
| summaries per slot | 1,344 |
| slot payload including metadata | 59,176 bytes |
| slots | 8 |
| adapter inline bookkeeping | 184 bytes |
| accounted adapter + boxed slots | 473,592 bytes |
| compile-time ceiling | 524,288 bytes |
| maximum reduction call | 5,120 work units |
| slot probes per macro sample | 1 |
| implicit classifications per correlation | 1 |
| ticket finite/fingerprint validation traffic | <=132,976 bytes (two fixed scans) |

Allocator bookkeeping is platform-specific and excluded, as it is in the VVH
budget. Construction allocates the exact boxed slice; there is no retained
capacity and no travel-time allocation path.

The route acceptance spans macro tile X `-1,250` through `+1,250`:

```text
2,500 tiles * 8.192 km/tile = 20,480 km
```

Sixty-five generation/publication samples across that route kept accounted
bytes unchanged, retained at most eight slots, and performed exactly 2,624 Far
reduction units per publication. Camera distance changes content, not the
ceiling.

## VVH correlation contract

The adapter deliberately does not call `reduce_child_bricks`,
`install_generated_base`, `install_resolved`, edit APIs, or cache residency
APIs. A caller supplies one already-produced `HierarchyObservation` containing
the original:

```text
(WorldVoxel, lod, configured_max_lod, BrickStamp, CellSummary)
```

The expected stamp and observed stamp must match exactly. Epoch and source
version must also equal the common world identity. The VVH caller remains
responsible for obtaining a current per-brick overlay stamp; the adapter does
not falsely treat it as a global world revision. Requested LOD must fit both
the copied hierarchy configuration and VVH's hard maximum. Negative voxel
positions use Euclidean division and the identity's explicit integer scale to
locate the corresponding MacroTile.

The returned correlation retains VVH material, occupancy and error unchanged.
Even an uncertain positive coarse occupancy is not rounded to empty or blended
with the 2D heightfield.

## Implicit correlation contract and explicit rejection

Automatic conversion from a macro guild, mountain, building hint, or material
into a sphere/ellipsoid was rejected. There is no stable feature identity,
shape parameter derivation, edit precedence or semantic guarantee that would
make such a conversion honest.

The safe bridge is an admission gate for an **already explicit** feature. A
caller supplies:

```text
(world_epoch, source_revision, feature_id, feature_revision,
 integer WorldVoxel anchor, conservative AABB, known bounded volume)
```

Expected and observed feature stamps must match exactly. Feature ID zero is
invalid. The adapter supports only the three known bounded implementations:
`SphereVolume`, `AxisAlignedEllipsoid`, and `OrientedEllipsoid`. It does not
accept an arbitrary trait implementation whose work might be unbounded. One
correlation performs exactly one conservative AABB classification; the worst
existing oriented case uses eight vertex samples and three interval axes.

The result keeps `Outside`, `Inside`, or `Surface` beside the macro summary. It
does not generate microvoxels, mutate occupancy, create a collider, edit a
block, or persist the feature.

## Focused verification and benchmark

The optimized focused harness and every result below use the real
Morphogenesis, VVH, and implicit modules plus the adapter.

Focused result:

```text
adapter focus: 10 passed; 0 failed; 1 ignored benchmark
all four pure modules: 42 passed; 0 failed; 1 ignored benchmark
```

Coverage includes fixed layout/work, one-ticket deterministic bands,
parent-first/fallback, direct-map whole-tile eviction, stale epoch/revision and
same-epoch reseed rejection, negative and `i64::MIN/MAX` macro identity, exact
VVH stamp/material retention, explicit implicit stamp/classification, and the
20,480-km bounded route.

The ignored optimized microbenchmark prepares generated tiles outside the
timed regions, then measures 100 staged publications and repeated resident
samples. It reports nearest-rank p50/p95/p99 rather than a best-only number.
Five consecutive real-module runs were recorded. The table reports the median
of the five independently calculated quantiles; the final column shows the
observed range of each run's p99, so desktop scheduling variance remains
visible.

| Hot path | median p50 | median p95 | median p99 | per-run p99 range |
| --- | ---: | ---: | ---: | ---: |
| Far publication, 64 summaries | 4.3 us | 5.3 us | 7.3 us | 6.9-11.1 us |
| Mid promotion, 256 summaries | 5.1 us | 5.6 us | 8.8 us | 5.8-9.0 us |
| Near promotion, 1,024 summaries | 11.4 us | 12.9 us | 17.0 us | 13.4-20.0 us |
| resident Near sample | 26 ns | 27 ns | 30 ns | 27-34 ns |

Measurement also caught two accidental hot-path costs in the first working
checkpoint: sampling copied an entire 59,176-byte slot by value, and every
promotion rescanned the immutable 66-KiB tile fingerprint/finite fields. That
checkpoint measured about 981 ns p50 per resident sample and 64.2 us p50 per
Far publication. Borrowing the slot and performing integrity scans once in the
private ticket result reduced those paths to the table above without weakening
identity checks. These are implementation-checkpoint comparisons, not a claim
about live engine frame time.

Verification commands and outcomes:

| Gate | Result |
| --- | --- |
| scoped `rustfmt --check` | pass |
| real-module native standalone library, `-D warnings` | pass |
| real-module Wasm standalone library, `-D warnings` | pass |
| optimized real-module focused tests | 42 passed, 0 failed, 1 ignored |
| five consecutive optimized benchmark runs | pass; distribution above |
| checkpoint registered native and Wasm Cargo checks | pass; compiler warnings remained |
| checkpoint registered workspace tests | pass; long-lived totals are deliberately not pinned here |

Native and Wasm Cargo checks prove compile registration and target
compatibility; they do not prove plugin installation or live consumption. The
standalone real-module harness is the adapter evidence. Runtime integration,
followed by repeated full native/Wasm Cargo gates, remains future work.

## Elite-standard position

Against `docs/ELITE_WORLD_SYSTEMS_STANDARD.md`:

- **Level 0, scoped pass:** new adapter/document only; no save, persisted
  format, deletion, reset, move, GUI or world mutation.
- **Level 1, scoped pass:** integer identity, explicit scale, Euclidean signed
  mapping, `i64` extreme MacroTiles through `i128`, finite generation envelope,
  and fail-closed schema/grammar/epoch/revision checks.
- **Level 2, scoped pass:** compile-time byte/work ceilings, fixed boxed slots,
  one-probe sampling, long-route invariants and an optimized distribution
  harness. Live frame, task, entity, GPU and queue budgets remain unmeasured.
- **Level 3, partial:** every band consumes one causal MacroTile and cannot
  reseed. The adapter does not improve the generator's bounded local hydrology
  horizon or claim global geology/ecology truth.
- **Level 4, local data pass only:** Far is published before Mid/Near and remains
  fallback-capable. No rendered seam/morph/edit test exists yet.
- **Levels 5-6:** not assessed by a nonvisual, noninteractive adapter.
- **Level 7, partial:** common identity and stale result rejection exist, but no
  agent command or live-engine capability is wired.
- **Level 8, scoped pass:** deterministic replay, collision pressure, stale
  work, negative/extreme coordinates, invalid scales/features, and long travel
  are covered. Device loss, task cancellation and corrupt persisted data do not
  apply to this unpersisted core.
- **Level 9:** not claimed. Registration, native/Wasm crate gates, live
  Natural/Astral routes and visual flight QA remain required.

## Known limitations and next integration gates

1. Eight direct-mapped slots are a proofable prototype, not enough evidence
   for a whole live camera footprint. A future scheduler may choose a different
   fixed placement policy only with new collision and timing evidence.
2. The explicit voxel/macro scale is not yet sourced from one engine-wide unit
   contract. Live wiring must define and test it rather than assume 256.
3. Morphogenesis surface families are descriptive. Actual voxel materials and
   authored edits remain separate even when correlated.
4. VVH per-brick overlay freshness must come from its own current request API.
   The adapter only exact-compares supplied stamps.
5. No macro output is converted to implicit geometry. Stable feature identity,
   parameters, edit precedence and save migration must exist first.
6. The adapter does not retain full `ContinuumTile` values. Promotion therefore
   needs the same immutable generated result again; losing it requires bounded
   regeneration from its ticket.
7. No renderer, mesh, GPU, collision, physics, vegetation, shuttle, editor,
   mission, save or agent system consumes this module.
8. No GUI was launched. Visual continuity, temporal stability and user-facing
   quality remain entirely unproven.

The next safe gate is minimal compile registration followed by native and Wasm
checks, then an isolated nonauthoritative scheduler with fixed request/install
budgets. Only after those gates should a visual Natural/Astral route compare
Far fallback, Mid/Near promotion, teleports, negative coordinates, edit
silhouettes and multiple window sizes. Live terrain and save authority should
remain unchanged throughout that experiment.
