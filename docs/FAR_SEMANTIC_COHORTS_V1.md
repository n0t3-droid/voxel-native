# Far Semantic Cohorts v1

Status: implemented behind a default-off, reversible runtime gate. Determinism and fixed-budget tests exist; native Natural/Astral visual acceptance is still pending.

## Problem, metric, and release boundary

The shipping six-ring planetary far field reaches 15,360 m, but its outer rings previously described only terrain and optional hydrographic surfaces. At kilometre scale that preserved land shape while omitting the sparse vertical landmarks that make a world legible as a place rather than a height map.

Semantic Cohorts v1 adds bounded, profile-aware silhouettes to the outermost L5 ring. The success criteria for this release are deliberately narrower than "reference-quality world art":

- absolute placement replays identically for the same grammar version, seed, profile, and signed world coordinates;
- admission and CPU/mesh work have exact, world-size-independent ceilings;
- the layer cannot become voxel, edit, collision, navigation, resource, vegetation, physics, or save authority;
- enabling or disabling it cannot reinterpret an old in-flight result;
- actual ECS residency can be checked independently of scheduler claims.

No frame-time, GPU-time, visual-quality, or player-satisfaction win is claimed yet. This is a deterministic and reversible presentation primitive whose visual value must still be proven on native routes.

## Authority boundary

The six stable categories are `NaturalGrove`, `NaturalKarst`, `NaturalMesa`, `AstralCrystal`, `AstralBasalt`, and `AstralReef`. Profile and the exact procedural biome choose the category only after a cell passes stateless admission. Ocean and beach cells emit nothing, and a centre classified as Natural water or `VolcanicWaste` lava emits nothing.

Each emitted cohort is currently one opaque, vertex-coloured tapered frustum with 24 vertices and 36 indices. All cohorts are packed into at most one combined L5 mesh and one ECS entity. That entity has no collider, casts and receives no shadow, creates no voxel or vegetation record, and is never serialized. Terrain generation, near chunks, edits, saves, resources, flight, and simulation do not consume its category or geometry.

Consequently, this layer may describe a distant skyline but can never certify that a tree, crystal, mesa, building, cave, or traversable object exists. Near-world authority remains unchanged.

## Deterministic selector and proof

Shipping terrain L5 samples a fixed 61 by 61 lattice at a 512 m step. Semantic identity deliberately remains on an independent absolute 1,024 m grid: only L5 points whose world X and Z are both divisible by 1,024 reach the selector. For each such Euclidean kilometre cell `(cell_x, cell_z)`, the selector derives:

1. a stable ID from the grammar version, seed, profile, and both signed cell coordinates;
2. `super_x = cell_x.div_euclid(8)` and `super_z = cell_z.div_euclid(8)`;
3. one hashed local X in `[0, 7]` and one hashed local Z in `[0, 7]` for that absolute supertile;
4. admission only when both Euclidean remainders equal those selected locals;
5. a stateless 4-bit shape variant from the stable ID.

This admits exactly one cell in every complete Euclidean 8 by 8 supertile, including negative coordinates. Euclidean division is essential: truncation toward zero would split the rule at the world origin and make the negative side asymmetric.

A selector window of at most 61 consecutive semantic cells can intersect at most nine 8-cell supertiles on either axis. One selected cell per intersected supertile therefore preserves the public Cartesian ceiling `9 * 9 = 81` candidates. The refined shipping L5 window inspects `61 * 61 = 3,721` terrain-lattice positions but contains only 30 or 31 aligned semantic coordinates per axis; its tighter geometric admission bound is therefore 25. Runtime allocation and validation intentionally retain the established `<= 81` compatibility ceiling, and height and biome queries are issued only for admitted candidates. Checked multiplication and addition skip unrepresentable samples at `i64` extremes rather than wrapping them; skipping cannot increase either bound.

Absolute admission does not depend on the camera, terrain LOD spacing, or build order. Retargeting the L5 anchor can change only whether an already admitted absolute cell falls inside the separate moving near-handoff exclusion.

## Conservative near-authority handoff

L5 uses floor snapping at its 512 m terrain quantum. For every camera block sharing an L5 anchor, each axis lies in the closed integer interval from `anchor` through `anchor + 511`. The handoff computes each absolute semantic centre's Euclidean distance to that camera interval with squared `i128` arithmetic and emits the centre only when that distance is strictly greater than the public 512 m exclusion radius.

This is an exact conservative promise for the whole snapped camera cell: a centre at 512 m distance is excluded, while one beyond the radius can pass. It does not relabel the guarantee as 1,024 m merely because semantic centres use a 1,024 m grid, and the arithmetic remains defined at `i64` extremes.

This is a render handoff, not proof of complete near-world coverage at 512 m. It also does not cross-fade a landmark. A cell can intentionally disappear from the far layer when L5 retargeting moves the camera interval within the exclusion radius, so transition behaviour remains a required visual gate.

## Chosen algorithm and alternatives

The chosen design is a two-phase stateless supertile grammar: inspect every fixed L5 lattice point, hash only points aligned to the independent semantic grid without terrain queries, then sample height and biome only for the bounded candidate set. All accepted shapes are batched into one mesh carried by the existing ring worker.

Alternatives were rejected for explicit reasons:

1. Independent random probability per kilometre cell was rejected because it has a distribution, not an exact population ceiling; an unlucky window can exceed a nominal density target.
2. Height and biome classification at all 3,721 cells was rejected because it spends expensive generator queries before knowing whether a landmark can be admitted.
3. One ECS entity or draw per cohort was rejected because the draw/entity count could grow to 81 for a single outer ring.
4. Per-cohort async jobs, retained maps, or edit-aware authority were rejected because they add queues, cache ownership, stale-result combinations, or false simulation authority to a render-only feature.
5. GPU generation or procedural instancing remains a possible later experiment, but v1 prefers a small CPU mesh whose exact payload and post-deferred ECS state can be inspected with the existing QA stack.

The selected approach is bounded and reversible, but it is not presented as a measured performance victory: no paired runtime benchmark has yet been accepted for the cohort layer.

Keeping the semantic cell at 1,024 m while terrain L5 moves to 512 m is a versioning boundary, not an accidental mismatch. Halving the semantic quantum would create up to four times as many candidate sites and reinterpret stable world IDs, skyline placement, and categories. Such a change requires a new semantic grammar version and separate budgets; the Far-LOD refinement is not allowed to make it implicitly.

## Exact compile-time budgets

| Quantity | Exact ceiling |
|---|---:|
| L5 lattice inspections (`hash_scans` telemetry) when enabled | 3,721 |
| Candidate records | <= 81 public compatibility ceiling (shipping geometry is tighter) |
| Height queries | <= 81 |
| Biome queries | <= 81 |
| Vertices per emitted cohort | 24 |
| Indices per emitted cohort | 36 |
| Total cohort vertices | 1,944 |
| Total cohort indices | 2,916 |
| Generated cohort attribute/index payload | 104,976 B |
| Cohort ECS entities | 1 |
| Cohort observer scan | 1 admissible entity + 1 overflow sentinel |
| Total far render entities with terrain, Hydro v1, and cohorts | 13 |
| Existing async ring jobs in flight | 1 shared; no additional cohort task |
| Hydro-only terrain + fluid atomic worker payload | 653,008 B |
| Fully enabled L5 terrain + fluid + cohort atomic worker payload | 757,984 B |

The 104,976 B cohort payload is exact at the ceiling: `1,944 * 48 B` for position, normal, colour, and UV attributes plus `2,916 * 4 B` for `u32` indices. Allocator capacity, Bevy asset metadata, renderer copies, and driver memory are not included in that generated-payload contract. The cohort layer creates no sample-cache window, task, per-cell material, map, collider, or save record.

## Mode, identity, and rollback

The feature is enabled only by one of these explicit values:

```text
VOXEL_NATIVE_FAR_SEMANTIC_COHORTS=silhouettes-v1
VOXEL_NATIVE_FAR_SEMANTIC_COHORTS=v1
VOXEL_NATIVE_FAR_SEMANTIC_COHORTS=on
VOXEL_NATIVE_FAR_SEMANTIC_COHORTS=1
```

`true` and `silhouettes_v1` are also accepted. Missing, unknown, `off`, `0`, `false`, `disabled`, and `none` values resolve to disabled. WebAssembly also defaults to disabled. This fail-closed parsing is intentional while visual acceptance is pending.

Rollback is non-persistent: launch without the variable or set it to `off`. The mode is part of the far-world and sample-cache identity, alongside seed, profile, scenery, surface-material mode, and hydro mode. Changing it invalidates incompatible cached interpretation and makes an older async request stale rather than publishing it under a new mode.

The 16 m terrain refinement does not alter the 1,024 m semantic identity. A deliberate source rollback must restore the matching L5 alignment path while retaining that semantic quantum; it must not silently reinterpret the grammar as 512 m. Disabling cohorts removes their presentation mesh immediately without changing terrain, Near authority, saves, or the stable selector grammar.

## Async installation and observed truth

Cohort generation runs inside the existing single bounded ring worker. A completed result is rejected before asset or ECS mutation when its world identity, LOD, anchor, detail, or relevant near-coverage identity is stale. A same-world stale terrain sample cache may seed the coalesced retry, but its visible meshes do not publish; a different world identity is not retained.

Before any of the terrain, fluid, or cohort payloads is added to render assets, installation validates all enabled payloads together. Cohort validation requires:

- L5-only scope and zero cohort work everywhere else;
- exactly 3,721 L5 lattice inspections (`hash_scans` telemetry) when cohorts are enabled;
- candidate, height-query, and biome-query agreement;
- emitted count no greater than candidate count;
- checked sum of all six category counts equal to emitted count;
- exact `emitted * 24` vertices and `emitted * 36` indices without arithmetic overflow;
- per-layer and fully combined byte/population ceilings.

The post-deferred observer then measures the actual ECS, not just scheduler bookkeeping. It uses a one-entity ceiling plus one sentinel, compares entity/vertex/index/byte/count/category totals against scheduler state, and independently reports population overflow, scheduler mismatch, budget excess, payload-integrity failure, overall observation validity, and rejection transitions. Overflow uses fail-closed sentinel values rather than understating unseen entities.

## Verification already encoded in tests

The current automated contract covers:

- exact replay and exactly one admitted cell per complete Euclidean 8 by 8 supertile across seeds, both profiles, negative coordinates, and representable `i64` edge supertiles;
- the <=81 selector compatibility ceiling, no duplicate supertiles, and the refined L5 alignment filter across negative and extreme starts;
- default-off and L5-only behaviour;
- exact scan/query/geometry/byte ceilings and six-category conservation;
- suppression over water and `VolcanicWaste` lava centres;
- anchor-overlap stability and the explicit moving handoff;
- the exact strict 512 m camera-interval exclusion using overflow-safe squared distance;
- checked extreme-anchor behaviour with finite generated geometry;
- cohort mode as stale-result identity;
- post-deferred scheduler/ECS agreement and independent payload-integrity failure;
- impossible-shape and arithmetic-overflow rejection;
- QA report mapping of mode, budgets, live/scheduler counts, category counts, integrity flags, and last-build work.

These tests establish deterministic and bounded mechanics. They do not establish native visual quality.

## Known limitations and pending visual gates

- The present frusta are semantic placeholders, not reference-level voxel trees, crystals, cities, towers, reefs, arches, or authored buildings.
- One centre height and biome query grounds a narrow silhouette; it does not prove that a broad footprint conforms to slopes, cliffs, caves, edits, or structures.
- There is no per-cohort LOD, cross-fade, animation, wind response, shadow contribution, local interaction, destruction, or edit-object integration.
- The one-per-supertile rule guarantees a ceiling, not aesthetically ideal composition, skyline rhythm, clustering, biome transitions, or sightline framing.
- The moving 512 m exclusion and L5-only scope can still expose popping, occlusion, scale, horizon, or near/far continuity defects.
- CPU and GPU distributions for representative flights have not yet been benchmarked against cohorts off.

Visual acceptance therefore remains open. The same release binary and fixed seeds must be run with cohorts off and `silhouettes-v1` for both Natural and Astral routes, including documented viewport/DPI cases and a near/far transition route. Screenshots and `report.ron` must be inspected together for composition, density, grounding, scale, category fit, occlusion, pop, horizon stability, telemetry agreement, and zero budget rejection. Passing unit tests or average FPS alone must never be recorded as visual acceptance.
