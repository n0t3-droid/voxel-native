# Far Hydrographic Continuity v1

Status: implemented behind a reversible runtime environment gate; native visual acceptance is still required.

## Problem and release boundary

The six-ring planetary height field previously described only solid relief. Near chunks already fill low Natural columns with water and low `VolcanicWaste` columns with lava, so losing those categories beyond the chunk bubble broke geographic continuity and could read as empty land or a colour void.

Hydrographic Continuity v1 adds a render-only description of those two existing column-fill rules. It is not a fluid solver, navigation authority, edit layer, collider, save format, or promise that every coarse surface is physically connected. The feature never writes world data and does not alter near terrain generation.

Rollback is immediate and non-persistent:

```text
VOXEL_NATIVE_FAR_HYDROGRAPHY=off
```

The default and explicit enabled spellings are `descriptive-v1`, `v1`, `on`, and `1`. Unknown values resolve to v1 so misspelling cannot create a third undocumented interpretation. The mode is part of the far-world/cache key: an on/off change invalidates old work and stale results fail closed.

## Ground truth and dimensional contract

These are authored voxel-world rules, not claims about real hydrology:

- Near Natural water occupies voxel columns where `surface < y <= WATER_LEVEL`, with `WATER_LEVEL = 48` voxel metres.
- Near `VolcanicWaste` lava occupies columns where `surface < y <= 52` voxel metres and takes precedence over water.
- A voxel top is rendered at `y + 1 m`. Far terrain already uses `+0.94 m` to avoid coincident surfaces, so Hydro v1 uses the same `0.94 m` top offset and a `0.02 m * LOD` depth bias.
- Absolute X/Z sampling remains integer `i64`. Conversion to the generator domain uses the established explicit 4096-block margin and clamped `i32` coordinates. Local mesh vertices become `f32` only after subtracting the render origin.

All displayed colours come from `BlockType::Water` and `BlockType::Lava`. Bevy's exact sRGB-to-linear conversion is applied once. Alpha is then forced to 1 because this release deliberately uses opaque PBR.

## Chosen algorithm

Every existing ring worker already owns a fixed 65 by 65 toroidal height cache. Hydro v1 performs one classification at each of the visible 61 by 61 lattice vertices:

1. A non-finite or surface height at/above 52 is dry.
2. Otherwise query the exact biome at the same absolute coordinate.
3. `VolcanicWaste` below 52 is lava.
4. A non-volcanic surface below 48 is water; all other vertices are dry.
5. Emit a cell only when its four corners agree on one fluid category.

The four-corner rule is conservative: one isolated coarse wet sample cannot flood an outer-ring cell. It is deterministic, independent of build order, requires no flood fill, and has no hidden connectivity queue. Water and lava share one vertex-coloured mesh and therefore at most one extra entity per LOD.

The terrain and fluid payloads are produced by the same bounded worker result. The worker can now also carry the independently gated Far Semantic Cohorts v1 payload on L5. Installation checks the request's complete world identity, LOD, anchor, near-coverage mask and material detail, then validates every enabled CPU payload before creating any new asset. A stale, malformed or over-budget result publishes none of its new meshes. Existing terrain remains visible while a rejected request is coalesced for retry.

Hydro's historical atomic contract remains explicit: terrain plus fluid is at most 653,008 B. Enabling the optional L5 cohort layer adds at most 104,976 B and raises the fully enabled terrain + fluid + cohort result ceiling to 757,984 B. Hydro evidence must report both fields and must not silently reinterpret the larger combined ceiling as Hydro-only cost.

## Alternatives evaluated

1. A transparent refractive plane was rejected for v1. Global MSAA is off, and alpha blending would introduce depth-order artifacts while visually promising fluid optics that the render-only tier does not simulate.
2. One giant plane per ring was rejected because it would cover land and recreate the blue-void failure in another form.
3. Per-cell entities were rejected because entity/draw count would scale to 3,600 per LOD.
4. A flood-fill or flow graph was rejected because it adds connectivity state and work without authoritative water simulation.
5. The chosen shared-lattice mesh preserves one entity per wet LOD, deterministic absolute samples, and a compile-time maximum independent of flight distance or world size.

## Compile-time budgets

| Quantity | Per ring | All six rings |
|---|---:|---:|
| Fluid entities | 1 | 6 |
| Hydro-only far render entities (terrain + fluid) | 2 | 12 |
| Fluid classification queries | 3,721 | 22,326 per full rebuild |
| Fluid biome queries | <= 3,721 | <= 22,326 per full rebuild |
| Fluid vertices | <= 3,721 | <= 22,326 |
| Fluid indices | <= 21,600 | <= 129,600 |
| Fluid vertex/index payload | <= 265,008 B | <= 1,590,048 B |
| Hydro-only atomic terrain + fluid worker payload | <= 653,008 B | one worker only |
| Optional cohort payload on L5 | <= 104,976 B | one combined cohort entity |
| Fully enabled L5 terrain + fluid + cohort worker payload | <= 757,984 B | one worker only |
| Maximum far render entities with optional cohorts | 3 on L5 | 13 total |
| Build jobs in flight | shared | 1 |
| Sample-cache windows | shared | 6 |

The fluid layer adds no task, cache window, hash map, connectivity graph, collider, material per cell, save record, or authoritative voxel. Dry rings create no fluid entity. The six established terrain entities and their actual-ECS telemetry retain their original meaning; fluid residency has a separate bounded post-deferred observer and separate scheduler-vs-ECS truth fields.

Hydro telemetry preserves water and lava independently at three levels: per-ring actual ECS indices, per-ring scheduler indices, and last-build indices. `resident_fluid_kind_integrity_valid` independently proves that Water + Lava equals total fluid indices globally and per ring, and that both category counts are divisible by the six indices emitted per quad. This reason is separate from population/budget failure and scheduler mismatch, so a category-corrupt payload cannot hide inside otherwise matching totals. The QA report also exports `budget_hydro_atomic_ring_build_bytes = 653008` separately from the optional-layer-inclusive `budget_atomic_ring_build_bytes = 757984`.

## Measured CPU/query distribution

The manual benchmark alternates enabled/disabled execution order and performs 25 cold LOD-1 builds per mode with the same seed, anchor, profile, detail tier and empty sample-cache input. On 2026-08-12, `cargo test --release --bin voxel-native planetary_streaming::tests::benchmark_hydrography_distribution -- --ignored --nocapture` produced:

| Host/toolchain | Hydro off | Descriptive v1 | Increment |
|---|---:|---:|---:|
| AMD Ryzen 7 5700G, rustc 1.92.0, x86_64-pc-windows-msvc, release | p50 4.231 ms; p95 4.947 ms | p50 6.055 ms; p95 6.845 ms | p50 +1.824 ms (+43.1%); p95 +1.898 ms (+38.4%) |

Every enabled sample issued exactly 3,721 fluid classifications and 835 conditional biome queries. The benchmark passed the exact query ceilings on every iteration. This is a single-machine cold-build microbenchmark, not frame-time, GPU or visual-acceptance evidence; its value is the paired baseline and stable bounded distribution.

## Failure modes and rollback boundaries

- A conservative four-corner cell can underrepresent a narrow river at coarse LOD. Expanding wet samples is intentionally forbidden in v1 because it can manufacture kilometre-wide water.
- There are no side walls, waves, transparency, refraction, foam, shoreline wetness, flow direction, depth, buoyancy, or physics. Those require separate evidence and cannot be inferred from this layer.
- A biome query at a lava-eligible low vertex is deliberate. Because water/lava precedence depends on `VolcanicWaste`, removing it would mislabel lava basins as water.
- The far layer describes procedural terrain. User edits remain near-world authority and are never copied into this mesh.
- Disabling the Hydro gate is its rollback. Legacy, Bridge-v1, Bridge-v2 and the separate semantic-cohort gate remain independently selectable and can be A/B tested without changing Hydro's 653,008 B contract.

## Verification contract

Automated tests cover:

- default/explicit rollback parsing;
- Natural water and Astral lava classification;
- negative and generator-edge coordinates;
- dry, NaN and infinite height rejection;
- exact vertex/index/query/entity/byte ceilings;
- repeated pressure/dirty-mask saturation;
- hydro mode as cache and stale-result identity;
- per-ring and total Water/Lava scheduler and actual-ECS counts;
- exact Water + Lava conservation, six-index quad divisibility, and an independently reported category-integrity failure;
- post-deferred fluid ECS truth, duplicate-slot and over-budget rejection;
- separate 653,008 B Hydro-only and 757,984 B fully enabled atomic worker ceilings;
- preservation of the pre-existing planetary suite.

Before release acceptance, run the same release binary and fixed seed/route with Hydro v1 on and off for Natural and Astral profiles. Inspect screenshots and `report.ron`; do not infer visual success from mean FPS. Acceptance requires no blue plane over land, no visible terrain/fluid half-install, no near/far water-level jump at the transition, no lava-to-water category error, stable horizon depth ordering, exact telemetry agreement, and no budget rejection. Transparent water is explicitly a future experiment, not part of this release claim.
