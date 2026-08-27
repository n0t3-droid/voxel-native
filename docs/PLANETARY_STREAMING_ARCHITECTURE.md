# Planetary Streaming Architecture

Status: implementation contract, August 2026. This document describes the
replacement for distance-by-more-full-chunks. It is intentionally explicit
about limits and failure modes: a larger number in the render-distance setting
is not planetary streaming.

## Outcome

Voxel-Native must make a continuous world readable over kilometres while the
amount of CPU work, GPU geometry and resident memory remains bounded. The
player must be able to fly quickly, land, edit a voxel, leave the region and
return without holes, terrain popping into a different shape, or an edited
landmark disappearing from its distant silhouette.

No computer can display an infinite editable world at literally zero cost.
The attainable promise is stronger and testable: expanding the visual horizon
must not expand full-resolution voxel residency with the square of distance.
The engine spends a nearly constant budget and changes representation with
distance.

## Why the current pipeline cannot scale

The current streamer uses a horizontal disc of 16x16x16 voxel chunks and
loads every non-empty vertical slot in that disc. Distant chunks only skip
per-corner ambient occlusion. They still require:

- terrain generation for a full voxel volume;
- shared voxel storage;
- neighbour readiness and seam bookkeeping;
- greedy meshing and material buckets;
- Bevy mesh assets and render entities;
- unload, orphan, dirty and cache tracking.

Observed Astral QA runs held between 7,708 and 11,474 chunks while showing a
horizon measured in hundreds of metres. Increasing that disc to kilometres
would square the problem and multiply it again by vertical occupancy.

### Reference machine for the August 2026 baseline

The August 2026 baseline was measured on Windows 11 with an AMD Ryzen 7
5700G (8 cores / 16 threads), 31.3 GiB system RAM, and the integrated AMD
Radeon adapter reported by Windows. WMI exposes only 0.5 GiB of dedicated
adapter memory for this shared-memory GPU, so that number is recorded as a
system report rather than treated as the real usable graphics-memory ceiling.
The toolchain is Rust 1.92.0, Cargo 1.92.0 and LLVM 21.1.3 on
`x86_64-pc-windows-msvc`. Every performance result must state build mode,
profile, route distance and viewport; a result from this machine is evidence,
not a promise for every user's hardware.

## The World Continuum

The renderer and simulator use the same deterministic world fields but consume
them at different representations.

| Tier | Nominal range | Representation | Editable now | Hard residency |
| --- | ---: | --- | --- | ---: |
| Interaction + near voxel field | 0-256 m maximum | full 16^3 voxels, physics near the player, material meshes | yes | 2,400 requested/resident-plus-terrain-job cap |
| Midfield bricks | proposed 256 m-2 km | virtual hierarchy, occupancy/material/feature summaries | not integrated; promotion contract only | prototype cap 512 bricks |
| Far clipmap | fallback from 0 m, 15.36 km L-infinity axis half-extent (30.72 km full width) | one finest parent plus five height/material annuli | no, descriptive only | exactly 6 entities |
| Celestial field | beyond terrain horizon | analytic bodies and atmosphere | no | fixed body count |

Distances are quality-profile inputs, not assumptions baked into save data.
The key invariant is that each tier has a fixed maximum population.

## Far terrain: geometry clipmaps

The implemented far horizon is represented by one camera-centred finest parent
and five square annuli. Each level has the same possible vertex dimensions;
cell spacing doubles per level.

```text
level 0:  16 m samples, complete safety parent with irregular Near cutout
level 1:  32 m samples
level 2:  64 m samples
level 3: 128 m samples
level 4: 256 m samples
level 5: 512 m samples, 15.36 km L-infinity axis half-extent
         (30.72 km full width)
```

Each level owns a fixed 65x65 source window: 61x61 possible rendered vertices
plus a two-cell sampling halo. With no Near-coverage cutout, the current
terminal-L5-only topology installs exactly 23,286 vertices and 110,760 indices
across all six levels; Near coverage can only reduce those populations.
Extending the horizon changes sample spacing, not the hard 35,000/150,000
geometry caps. The terrain generator
already exposes deterministic surface height, biome and environment fields, so
far meshes do not allocate voxel volumes.

Clipmap rules:

1. Ring origins snap to their own cell spacing.
2. Movement updates only newly exposed rows or columns.
3. Adjacent levels share quantised samples at common coordinates.
4. A morph band blends fine heights toward the coarser parent before the seam.
5. Skirts close only the finite outer horizon. Inner skirts are forbidden
   because they become giant walls when the camera is inside an annulus.
6. A fixed 3,600-bit finest-grid stencil removes a parent cell only when all
   four underlying Near columns are visually covered. Expansion is time-stable;
   coverage loss restores the parent immediately.
7. A future water layer must be continuous so rivers and coastlines do not
   inherit terrain skirts.
8. Materials are a compact biome/surface palette plus slope, moisture and
   emission signals, not one draw call per voxel material.

## Midfield: virtual voxel bricks

The midfield preserves voxel character and major overhangs without keeping
every source cell. A brick covers a fixed world cube and stores:

- a conservative occupancy mask;
- minimum and maximum solid height per macro cell;
- dominant and secondary material with deterministic tie-breaking;
- exposed cavity and overhang flags;
- emissive energy, vegetation canopy density and structure presence;
- a parent/child error estimate used by the scheduler.

Brick levels use integer world coordinates and Euclidean division, including
negative positions. A level-L brick is reproducible from the generator and
the edit overlay; it is not a second authoritative world.

Only surface-crossing macro cells emit geometry. Completely solid interiors
and empty air produce no vertices. A brick close to the camera is replaced by
its children, then by full chunks, using a temporal cross-fade or geometric
morph instead of an abrupt pop.

## Sparse edits across all levels

Full edited chunk snapshots remain valid for compatibility, but distant
rendering needs summaries rather than all source chunks resident.

The new edit index is an overlay tree:

```text
world generator
  + sparse edited chunk delta
    -> level-0 surface/occupancy summary
      -> parent brick summary
        -> far-clipmap correction and landmark silhouette
```

An edit invalidates one leaf and at most one ancestor per level. Rebuilding a
summary is logarithmic in horizon scale and scheduled within a separate edit
budget. The authoritative voxel delta is never discarded. Summary files are
caches and may be recreated from the generator plus edits.

Required behaviours:

- removing a mountain top updates its far silhouette;
- building a large tower becomes visible in the correct macro tile;
- a one-voxel edit does not force a kilometre of full chunks into memory;
- old worlds load without migration and build summaries lazily;
- saving during an in-flight summary build cannot lose the edit;
- summary version mismatch invalidates only the cache, never the world.

## Streaming scheduler

Requests are prioritised by projected visual error, not distance alone.

```text
priority =
    screen_space_error
  + forward_view_bonus
  + predicted_flight_path_bonus
  + landing_corridor_bonus
  + active_edit_bonus
  + mission_landmark_bonus
  - occlusion_confidence_penalty
  - already_covered_parent_penalty
```

Inputs include camera frustum, angular velocity, shuttle velocity and braking
distance. The predicted anchor is clamped so a teleport or physics fault cannot
queue unbounded coordinates.

Every request carries a generation epoch. When the player changes world,
teleports, or outruns a request, stale work may finish on a worker but is
discarded before installation. Queue length, task count and upload count have
independent caps.

## Simulation is not render distance

Visual kilometres do not imply global high-frequency simulation.

- player/shuttle collision uses the interaction bubble;
- bots use explicit active districts and low-frequency summaries elsewhere;
- vegetation wind is vertex displacement only and never enters shuttle forces;
- fluids outside the interaction bubble use flow summaries, not per-cell ticks;
- combat and physics entities hibernate outside authored relevance zones;
- returning to a zone reconstructs deterministic state plus persisted events.

This separation is mandatory. Tying AI, fluids or physics to the far visual
horizon would simply move the scaling failure out of the renderer.

## Global content contract

All tiers sample the same sources:

- terrain height and geological masks;
- river and watershed fields;
- biome and ecology fields;
- road/transit graph;
- waypoint and settlement registry;
- authored hero precincts;
- sparse player/bot edits.

A special location may add detail, but it cannot use a different planet. A
river visible in the far clipmap must meet the same river at voxel range. A
remote waypoint must have a macro silhouette before its detailed voxels load.

## Budgets

Initial Balanced-profile targets, subject to measured tuning:

| Resource | Target bound |
| --- | ---: |
| full voxel chunks | <= 2,400 resident |
| full voxel mesh entities | exact current/peak telemetry; no hard rejection yet because a retained fallback is required before safe admission control |
| midfield bricks | <= 512 resident |
| far clipmap levels | 6 |
| far terrain vertices | <= 35,000 |
| far terrain indices | <= 150,000 |
| far generated mesh payload | <= 2,280,000 bytes |
| far coverage working set | 1,545 bytes, compile-time <= 2 KiB |
| terrain task installs | <= 4/frame |
| mesh/brick uploads | <= 3/frame |
| clipmap stripe updates | <= 2/frame |
| edit-summary work | <= 1.0 ms/frame |
| main-thread streaming p99 | <= 2.0 ms |

Fast and High profiles change radii and update cadence. They do not remove the
caps or switch back to square-law residency.

## Failure containment

- Coordinates use checked i64 intermediates before converting to renderer f32.
- Ring and brick keys use Euclidean division at negative world coordinates.
- A malformed cached summary is ignored and regenerated.
- Asset handles are explicitly released when a ring/brick slot is reused.
- A missing fine tile keeps its valid parent visible; it never creates a hole.
- A missing parent cannot erase an already valid fine tile.
- Generation errors are visible in Mission Control with tier and key.
- Teleports reset prediction history but preserve valid resident caches.
- Memory pressure reduces detail radius before it reduces horizon silhouette.

## Verification matrix

Automated tests:

- shared samples are identical on all clipmap seams;
- negative-coordinate ring shifts never skip or duplicate a stripe;
- residency stays bounded after a 100 km synthetic route;
- deterministic samples match across insertion and task-completion order;
- edits propagate through every parent level and undo/redo round-trips;
- stale generation epochs cannot install into a new world;
- malformed cache data is fail-closed;
- Natural and Astral profiles share infrastructure without sharing content.

Real-engine QA:

1. hover at one coordinate until all tiers settle;
2. fly at walking, shuttle and boost speeds across biome boundaries;
3. turn 180 degrees repeatedly to stress view-priority cancellation;
4. descend from far terrain into an editable landing site;
5. make a large edit, depart until only the far tier remains, and return;
6. teleport across positive and negative coordinates;
7. run at 320x480, 800x600, 1080p, ultrawide and 4K UI sizes;
8. record FPS, p95/p99 frame time, task queues, resident bytes and tier swaps;
9. reject any run with holes, divergent rivers, visible seam cracks, stale
   collision, runaway memory or a permanently backlogged queue.

The repeatable kilometre route is launched with
`scripts/planetary-streaming-qa.ps1`. It creates a new isolated QA world per
profile and run, drives an S-curve rather than one cache-friendly axis, and
records both final and peak chunk, mesh, terrain-task, mesh-task and dirty-queue
counts. The default matrix runs 8 km in Natural and Astral at 1280x720; distance
can be raised to 100 km without changing the representation budgets. A final
small resident count is not sufficient evidence if any peak grew with travelled
distance, so acceptance compares the peak fields and screenshot sequence.

## Delivery sequence

1. Add instrumentation for resident bytes and per-tier timing.
2. Implement far height/material clipmap behind a feature flag. **Implemented;
   Astral default, reversible, Natural available for QA.**
3. Reduce and hard-bound the full voxel radius while retaining the new horizon.
   **Implemented at <=2,400 requested/resident-plus-terrain jobs and <=16 chunk
   interaction radius.**
4. Add midfield bricks and parent fallback. **Pure hierarchy, implicit-volume
   and continuum prototypes exist; live renderer/authority integration remains.**
5. Add sparse edit summaries and invalidation.
6. Add landmark/vegetation/transit macro instances.
7. Connect predictive shuttle scheduling.
8. Validate long flights and enable by profile.

Each step must be visually compared in the real engine. A unit-test-only LOD
change is not accepted because wrong silhouettes, excessive haze and transition
pops are perceptual failures even when topology is mathematically closed.
