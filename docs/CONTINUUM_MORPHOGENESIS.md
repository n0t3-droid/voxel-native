# Continuum Morphogenesis v1

Status: pure, compile-registered grammar and acceptance record, with no ECS or
renderer registration, 2026-08-09.

Implementation: `src/continuum_morphogenesis.rs`

Invariant and benchmark harness: `tests/continuum_morphogenesis.rs`

This module is part of the ordinary crate build so API drift cannot silently
rot it, and the pure `world_continuum` adapter consumes its output in registered
source. No ECS system or renderer calls it at runtime, it does not read or write
a save, and it does not replace terrain, collision, edits, fluids, or semantic
object authority. It is a bounded macro-world grammar whose output can become
a reconstructible cache after a separate runtime integration gate. It
intentionally does not allocate dense microvoxels.

## Outcome and present baseline

Before this prototype there was no isolated, versioned
geology-to-settlement macro generator with a comparable benchmark. Existing
live terrain is not a controlled before-baseline and was not changed, so this
record does not claim an engine frame-time improvement.

The current measured baseline is:

| Measure | Result | Hard contract |
| --- | ---: | ---: |
| Macro core | 32 x 32 cells | compile-time fixed |
| Cell scale | 256 m | art-direction scale, not survey resolution |
| Tile span | 8.192 km | derived from the two values above |
| Shared vertex lattice | 33 x 33 | exact adjacent edge samples |
| Hydrology halo | 13 cells | radius 4 + 8 flow steps + 1 guard |
| Scratch lattice | 58 x 58 | compile-time fixed |
| Output bytes per tile | 66,488 B | <= 73,728 B |
| Scratch bytes per generation | 60,552 B | <= 65,536 B |
| Generator retained state | 0 B | must remain zero in this pure core |
| Maximum accounted work | 785,453 units | compile-time cap |
| 20,447.232-km route peak work | 759,227 units | <= cap at every sample |
| Route samples | 65 | output and scratch population stayed constant |
| Focused acceptance tests | 11 passed | 0 failed |

One work unit is one macro sample, one D8 neighbour inspection, or one
bounded flow-trace visit. It is an algorithmic accounting unit, not a CPU
cycle.

### Benchmark distribution

The acceptance run used:

- Windows x86_64 MSVC;
- `rustc 1.92.0 (ded5c06cf 2025-12-08)`, LLVM 21.1.3;
- repository test profile reported by Cargo as `optimized + debuginfo`
  (`[profile.dev] opt-level = 1`, line-table debug information);
- AMD Ryzen 7 5700G, 8 cores / 16 logical processors, 31.3 GiB RAM;
- one unmeasured warm-up tile, then 32 seeds and signed coordinates;
- ordinary desktop scheduling, with no core pinning or process isolation.

| Statistic | Tile generation time |
| --- | ---: |
| Minimum | 2.951 ms |
| Median (p50) | 3.506 ms |
| p95 | 3.603 ms |
| Maximum | 3.668 ms |
| Mean | 3.451 ms |

This is a small in-test distribution, not Criterion, a frame-time trace, or a
universal hardware promise. Timing is evidence for the isolated CPU grammar
only. Correctness, memory, output, and work ceilings are hard regression gates;
the wall-clock numbers are not yet a release threshold.

## Inputs, outputs, and authority

### Inputs

The complete identity of a generated tile is:

```text
(grammar_version: u32, seed: u64, tile_x: i64, tile_z: i64, profile: enum)
```

`generate` selects the current grammar version. `generate_versioned` accepts an
explicit version and fails closed with `UnsupportedGrammarVersion` for any
unknown value. Version 1 is therefore reproducible without silently interpreting
future data as an older grammar.

Signed tile coordinates are promoted to `i128` before multiplication and local
offsets. Lattice noise uses Euclidean division and remainder. `i64::MIN` and
`i64::MAX` tile pairs are acceptance cases, not undefined edge behaviour.

Profiles are causal parameter sets rather than colour palettes:

- Natural: Temperate Basins, Arid Plateaus, Alpine Rifts, Volcanic
  Archipelago.
- Astral: Astral Crystalline, including different relief, uplift, rainfall,
  weathering, temperature/aridity constraints, and functional guild outcomes.

### Outputs

`ContinuumTile` contains only fixed arrays and reports:

- shared vertex elevation, uplift, and strata phase;
- cell elevation, uplift, strata phase, slope and downhill drop;
- D8 receiver direction, bounded local flow accumulation, and one-step routed
  surface water;
- soil depth and moisture potential;
- vegetation potential and a functional species-guild label;
- local route and settlement suitability;
- explicit incoming and outgoing flux for four edges and four diagonal
  corners;
- water and generation accounting telemetry.

These values are descriptive. They may guide meshing, scattering, colour,
far-field summaries, or planning candidates. They are not permission to place
gameplay objects and are never collision, fluid, edit, or save authority.

## Required invariants and success metrics

1. The same version, seed, coordinate, and profile produces byte-equivalent
   scalar fields and the same fingerprint.
2. Request order, worker completion order, camera direction, and travel history
   cannot affect output because the generator has no mutable state.
3. Core world indexing is signed integer indexing. Planetary identity never
   depends on a floating render position.
4. Adjacent tiles share bit-identical 33-sample vertex edges.
5. Every outgoing edge/corner flux is bit-identical to the matching incoming
   port of its neighbour.
6. A non-sink D8 receiver has strictly positive downhill drop. A sink is
   represented by `(dx, dz) = (0, 0)` and zero drop.
7. One-step water is conserved locally and after cross-tile inflow. Internal
   flux cancels in a generated region.
8. Every public scalar is finite; normalized descriptive fields stay in
   `[0, 1]`.
9. Output, scratch, and accounted work never exceed compile-time caps.
10. Travel distance cannot grow generator state. The 20,000-km gate must show
    the same per-request output and scratch sizes at every sample.
11. Natural and Astral profiles must differ in causal fields and functional
    guild results, not just their presentation palette.

## Candidate decision record

| Candidate | Strength | Cost / failure mode | Decision |
| --- | --- | --- | --- |
| Independent per-chunk fractal noise | Cheap and trivially parallel | No drainage causality; fragile seams; Near/Mid/Far can disagree | Rejected |
| Global erosion, climate, and weather PDE | Potentially rich long-term landforms | Runtime and resident state grow with domain and simulated time; difficult teleport and migration semantics | Rejected for the pure core; future offline bake only |
| Full global watershed or MFD solve | Better large-catchment accumulation and divergence | Requires a global ordering/graph, larger boundary state, and a bounded incremental update design | Deferred behind a watershed-summary prototype |
| Translation-invariant macro tile with fixed halo, D8, ports, and stateless replay | Exact caps, deterministic borders, local causality, easy rollback | Only a bounded hydrology horizon; no long river history | Chosen for v1 |
| Persistent sparse VDB/clipmap cache | Efficient sparse storage and useful visual continuity | Cache residency, invalidation, epochs, parent coverage, and edit projection must be proven | Deferred to integration; never authority |

The chosen solution is deliberately simple where simplicity proves the real
metric. It does not label a fixed noise function as a geological simulation.

## Causal pipeline

The stages are executed in this order and `GenerationReport` records five
completed stages.

### 1. Geology, strata, and uplift

Global integer coordinates feed several differently scaled value-noise fields.
Their weighted relief, a ridge-shaped uplift response, and a quantized strata
phase produce elevation. Every sample is a pure function of global coordinate,
seed, version, and profile, so the halo and neighbour tile see the same source.

This is a landform grammar. It does not simulate plate mechanics, lithology,
deposition, compaction, or erosion through time.

### 2. Hydrology and bounded accumulation

Rainfall is a bounded profile-scaled forcing in arbitrary mass units. Each
scratch cell selects the steepest positive D8 receiver; deterministic D8 order
breaks equal grades. Local accumulation asks whether each source in a 9 x 9
window reaches a target within at most eight receiver steps. Therefore:

```text
maximum local accumulation
  = 9 * 9 source cells * 2.0 mass/cell
  = 162.0 mass units
```

Core rainfall is routed one step. Water that crosses a boundary is emitted to
an edge/corner port. The same source is sampled by the adjacent tile's halo and
is accepted through the corresponding incoming port into the destination cell.

For each tile `T`:

```text
M_initial(T) + F_in(T) = M_core_after(T) + F_out(T)
```

For east/west neighbours `A` and `B`:

```text
F_out_east(A)[z] = F_in_west(B)[z]
```

The tests also generate a 2 x 2 region, compare every internal edge and central
diagonal port, remove those equal internal terms, and verify the external
regional equation. The report uses `f64`; visualization storage is `f32`, so
the rendered-cell sum has a documented `1e-3` tolerance while the report
equation uses `1e-9`.

The GRASS `r.watershed` manual distinguishes SFD/D8 from MFD, exposes drainage
and accumulation, and warns that an analysis region can underestimate incoming
runoff. This prototype takes those facts as constraints for explicit halo and
boundary ports; it does not claim equivalence to GRASS's full watershed solve.

### 3. Soil and moisture

Soil depth combines profile weathering, local regolith variation, and a
slope-retention term. Moisture combines bounded rainfall, log-normalized local
accumulation, soil retention, slope loss, and profile aridity. Thus geology
constrains slope; slope and uplift constrain drainage; drainage constrains soil
and moisture.

No soil horizons, groundwater table, infiltration PDE, sediment transport, or
seasonal water balance is simulated.

### 4. Vegetation potential and guild

Vegetation is a product of bounded temperature, water, soil, dry-air, and slope
limiting scalars. NASA MOD17 similarly documents multiplicative minimum-
temperature and vapour-pressure-deficit attenuation scalars for productivity.
The resemblance stops at the limiting-scalar pattern: this implementation does
not compute APAR, GPP, NPP, carbon, biomass, or a MOD17 product.

Functional guilds are coarse art-direction categories. Moisture, temperature,
aridity, wetness, and potential select Bare, Pioneer Grass, Shrubland, Closed
Canopy, Riparian, Alpine, Xeric Scrub, Crystal Pioneer, or Luminous Grove. The
plant-functional-trait literature supports treating environmental gradients as
filters on functional composition, but these thresholds are not calibrated
botany and the Astral guilds are explicitly fictional.

### 5. Route and settlement suitability

Route suitability prefers gentle, unflooded, traversable cells. Settlement
suitability additionally considers soil, moderate moisture, water access, and
uplift/flood penalties. The fields are local preferences only.

GRASS `r.walk` combines elevation with friction and models anisotropic uphill
and downhill travel cost. A future global route solver may consume this tile's
fields in that spirit. The current code does not run least-cost search and must
not be presented as a solved road or settlement plan.

## Border and coordinate contract

The core is the half-open global cell rectangle:

```text
[tile_x * 32, tile_x * 32 + 32)
x
[tile_z * 32, tile_z * 32 + 32)
```

The vertex lattice is inclusive on both ends, so the east vertex `x = 32` of
one tile is exactly the west vertex `x = 0` of its eastern neighbour. The same
rule applies north/south. Core cell fields across a boundary are adjacent cells,
not duplicate cells, and are not expected to be equal.

The 13-cell halo is translation invariant. It is large enough for a source
radius of four, an eight-step trace, and one guard cell. Changing any of those
constants without updating the compile-time halo assertion is rejected.

Coordinates are multiplied in `i128`, and lattice partitioning uses
`div_euclid`/`rem_euclid`. Negative coordinates therefore have the same phase
and neighbourhood semantics as positive coordinates. No float-to-world-index
conversion exists in this module.

## Fixed resource budget

The pure generator contains no `Vec`, `Box`, hash map, cache, task queue, file
handle, or retained tile list. Its only generation scratch is a fixed stack
structure.

The maximum work expression includes:

- geology and rainfall samples over 58 x 58;
- eight D8 comparisons per scratch cell;
- the 33 x 33 shared vertices;
- all 1,024 targets, all 81 local sources, and all nine possible trace visits;
- four post-hydrology cell stages;
- 128 edge and four corner incoming sources.

Compile-time assertions fail the build if output exceeds 72 KiB, scratch
exceeds 64 KiB, the halo is too small, or the Mid/Far strides no longer divide
the tile. A caller that generates concurrently must still cap the number of
worker stacks; a fixed per-call allocation is not a free unlimited-concurrency
claim.

## One-world Near / Mid / Far feed contract

`LOD_FEED_CONTRACT` is a code-level integration contract, not live integration.
Every visual tier must derive from the same versioned `ContinuumTile`. It may
not reseed an independent terrain function.

| Tier | Macro stride | Intended role |
| --- | ---: | --- |
| Near | 1 | detailed terrain/scatter guidance around interaction authority |
| Mid | 2 | 2 x 2 conservative aggregates |
| Far | 4 | 4 x 4 conservative aggregates and silhouette bounds |

Reduction rules are semantic:

- extensive water-like quantities use sums;
- intensive moisture, soil, vegetation, route, and settlement fields use
  area-weighted means;
- elevation retains minimum, mean, and maximum, not one decimated sample;
- categorical guilds retain a histogram, not an unstable winner;
- shared vertices define the stitch boundary at every tier.

The future live consumer must also prove these Level 4 obligations before it
can ship:

1. a coarse parent is resident before a finer child disappears;
2. promotion/demotion uses a stable morph band or cross-fade;
3. stale asynchronous children cannot replace a newer epoch;
4. sparse edits invalidate a bounded ancestor chain and appear in distant
   summaries;
5. proxies remain descriptive unless they prove conservative collision/edit
   coverage;
6. Interaction and Celestial tiers receive compatible quantization and stable
   feature identity.

None of those live streaming obligations is implemented by this pure file.

## Regression evidence

The focused integration test proves:

| Test | Evidence |
| --- | --- |
| `deterministic_replay_is_independent_of_request_order` | repeat and reversed request order produce equal tiles |
| `grammar_version_is_reproducible_and_unknown_versions_fail_closed` | version 1 replay and unknown-version rejection |
| `signed_extreme_coordinates_are_finite_and_do_not_panic` | negative axes and all `i64::MIN/MAX` pairs |
| `neighboring_tiles_share_exact_vertices_and_flux_ports` | bit-identical east/west, north/south, and diagonal contracts |
| `two_by_two_region_cancels_internal_flux_and_conserves_mass` | internal ports cancel and regional boundary mass balances |
| `bounded_water_routes_downhill_and_conserves_one_step_mass` | D8 downhill/sink rule, local cap, inflow/outflow mass accounting |
| `all_public_scalar_fields_are_finite_and_normalized_fields_are_bounded` | no NaN/Inf and normalized ranges for every profile |
| `output_scratch_and_work_have_compile_time_caps` | exact layout accounting, zero state, work cap, LOD contract |
| `profiles_produce_distinct_macro_grammars` | five distinct fingerprints and Natural/Astral functional outcomes |
| `twenty_thousand_kilometre_route_does_not_grow_generator_state` | 20,447.232 km, 65 samples, constant bytes and bounded work |
| `benchmark_fixed_macro_tile_generation` | warm distribution with min/p50/p95/max, not a best-only sample |

A transient earlier test exposed that incoming boundary mass was reported but
not inserted into the destination tile's routed-water field. The correction
was causal: incoming halo sources now feed the destination cell, the report
separates inflow and post-route core mass, and the 2 x 2 regional proof was
added. The acceptance was not obtained by merely weakening a tolerance.

## Elite acceptance ladder: honest status

Against `docs/ELITE_WORLD_SYSTEMS_STANDARD.md`:

- Level 0: passes for this pure prototype. It performs no I/O and touches no
  saves. There is no persisted format yet.
- Level 1: passes the scoped numeric contract: signed coordinates, Euclidean
  division, extreme-coordinate tests, finite outputs, and fail-closed versions.
- Level 2: passes for isolated generation: compile-time bytes/work, zero
  retained state, benchmark distribution, and 20,000-km route. It does not
  prove live scheduler/task/entity budgets.
- Level 3: passes the bounded macro-grammar scope: causal stage ordering,
  exact shared borders/ports, sinks/downhill/conservation, order independence,
  versioning, and causal Natural/Astral differences. It does not compute full
  continental watersheds or time-evolving climate.
- Level 4: contract only. The reducer semantics are explicit; no Near/Mid/Far
  renderer consumes them yet, and edits/identity are not projected.
- Levels 5-7: not assessed by this non-visual, non-interactive prototype.
- Level 8: partial adversarial evidence for extreme coordinates, request order,
  NaN avoidance, and route length. No async cancellation, device pressure,
  corrupt persistence, or live recovery exists here.
- Level 9: not claimed. No GUI/GPU QA was launched and no Natural/Astral flight
  was recorded for this isolated grammar. The source is compile-registered, but
  it still has no ECS, renderer, or save-authority route.

## Known limits and invalidating assumptions

- This is macro grammar, not a full erosion, weather, climate, groundwater,
  ocean, soil, ecology, or plate-tectonic simulation.
- Local flow accumulation sees a 9 x 9 source window and eight downhill steps.
  A river longer than that is not represented by upstream history.
- Rainfall and water are bounded art units, not SI volume or discharge.
- D8 sends all mass to one receiver. MFD, flat resolution, depression filling,
  evaporation, infiltration, and time integration are deferred.
- Soil and guild thresholds are uncalibrated heuristics. Fictional Astral
  biology has no real-world ground truth.
- Route and settlement fields are suitability hints, not global plans,
  ownership, navmesh, economy, or mission authority.
- Output and scratch are stack values. A live job system needs a separate hard
  concurrency cap.
- There are no persistent landmark IDs, edit summaries, cache epochs, or
  migrations beyond rejecting unsupported grammar versions.
- Benchmark results become invalid if compiler, profile, CPU power state,
  constants, or the generation workload changes.

## Primary sources and how they constrain this prototype

- [GRASS GIS `r.watershed` manual](https://grass.osgeo.org/grass85/manuals/r.watershed.html): primary project documentation for drainage direction, accumulation, SFD/D8 versus MFD, sinks/depressions, memory controls, and boundary underestimation. Used as a hydrology contract reference, not copied as an implementation.
- [NASA MOD17 User's Guide v3](https://modis-land.gsfc.nasa.gov/pdf/MOD17UsersGuide2015v3.pdf): documents multiplicative minimum-temperature and VPD attenuation scalars. Used only to justify bounded limiting factors; no GPP/NPP claim is made.
- [GRASS GIS `r.walk` manual](https://grass.osgeo.org/grass-stable/manuals/r.walk.html): documents anisotropic cumulative movement cost from elevation and friction. Used to constrain the meaning of local route suitability and to make clear that it is not a solved path.
- [Global patterns of plant functional traits and their relationships to climate](https://www.nature.com/articles/s42003-024-06777-3): primary research relating functional traits and environmental gradients. Used to prefer coarse functional guild constraints over pretending to generate taxonomically valid species.
- [Geometry clipmaps](https://hhoppe.com/proj/geomclipmap/): primary project/paper page for nested regular-grid LOD, visual continuity, throttling, and graceful degradation. Used for the future representation contract, not for macro causality.
- [VDB: High-resolution sparse volumes with dynamic topology](https://museth.org/Ken/Publications_files/Museth_TOG13.pdf): primary paper showing why dense volume memory grows with embedding volume and how sparse topology changes that relationship. VDB is a deferred cache candidate, not part of v1 and never edit authority.

The recorded discovery graph was used only to route research. Its 8,129 link
rows are not independent evidence: universal voxel links and large medical and
botanical fan-outs bias degree counts, four duplicate pairs exist, and link
degree does not measure relevance or scientific quality. Claims above therefore
point to primary sources rather than treating the graph as an authority.

## Rollback and next gate

Rollback requires removing the compile registrations together with the grammar,
pure adapter, tests, and documentation; no save format depends on them. Runtime
integration should begin with an ECS/renderer boundary that:

1. requests versioned macro tiles through a bounded, epoch-aware queue;
2. materializes conservative Mid/Far reducers and tests parent-first coverage;
3. compares the same Natural/Astral route at Near/Mid/Far boundaries;
4. carries edit summaries and stable feature IDs without making macro output
   authoritative;
5. repeats memory, work, queue, frame-time, teleport, and visual QA before any
   live terrain switch.

Until that gate is green, Continuum Morphogenesis v1 is evidence-backed world
grammar research, not a shipped world system.
