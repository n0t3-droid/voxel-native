# Dense Near-Field Streaming Baseline and Budget

## Scope and success metric

This note covers only the editable, collidable `16 x 16 x 16` chunk
representation in `src/world.rs`. It does not claim that kilometre-scale
terrain is free. The success metric is stricter and measurable: increasing
visual distance or distance travelled must not increase the number of full
chunks, full-chunk requests, or asynchronous chunk jobs beyond fixed limits.
The visual horizon is served by the separate bounded far-field hierarchy.

The invariant is:

```text
requested full chunks <= 2,400
resident full chunks + in-flight terrain chunks <= 2,400
in-flight terrain tasks <= 96
in-flight mesh tasks <= 64
near-field candidate radius <= 16 chunks (256 m)
```

`StreamingTelemetry` and `StreamingGovernor` expose requested, resident,
in-flight, current/peak mesh-bucket entity, evicted, cancelled, stale-result,
epoch, selected-column, hard-cap, and cap-reason values. The cap is automatic
and deterministic on every machine; a fast PC does not silently allocate an
unbounded dense frontier.

## Measured baseline

The old frontier enumerated every X/Z column in a render-distance circle and
then considered every configured vertical chunk. The exact structural counts
are:

| Render distance | X/Z columns | Slots at 8 vertical chunks | Slots at 16 vertical chunks | Dense voxel + material arrays at V8* |
| ---: | ---: | ---: | ---: | ---: |
| 16 | 797 | 6,376 | 12,752 | 99.6 MiB |
| 23 | 1,653 | 13,224 | 26,448 | 206.6 MiB |
| 50 | 7,845 | 62,760 | 125,520 | 980.6 MiB |
| 64 | 12,853 | 102,824 | 205,648 | 1,606.6 MiB |

\* Array estimate is `slots x 4096 x (u16 voxel + u16 material)`. It excludes
hash maps, `Arc`/chunk metadata, mesh CPU/GPU buffers, material buckets,
entities, tasks, and edit snapshots, so it is a lower bound rather than a RAM
forecast.

The latest pre-change real-engine evidence already recorded 7,204 resident
chunks at effective RD16 and 8,193 at RD23. Those numbers are below the
structural worst case only because terrain-height rejection skips guaranteed
air; they still grow with the visible disc and travelled frontier.

The baseline world unit suite contained 11 tests and completed successfully.
The unbounded formula, rather than test failure, was the defect.

## Candidate approaches

### A. Radius-only clamp

Clamp full chunks to a smaller circle derived from the worst configured
vertical height. This is simple and safe, but wastes capacity in low terrain,
does not prioritise the flight direction, and still needs a second check for
edited towers and future taller profiles.

### B. Exact ranked, column-complete interaction plan (chosen)

Examine a constant 16-chunk candidate disc, rank columns core-first, then by
nearby edits, inferred travel direction, camera direction, and distance. Admit
only whole conservative vertical envelopes until 2,400 slots are consumed.
Actual terrain ceilings are resolved lazily for admitted columns as the normal
per-frame scheduler reaches them; known-air reservations allocate no chunk or
task. The exact request set becomes the authority for residency, tasks, dirty
work, and eviction. This prevents half a tower or collision column from being
cut at the boundary and remains small enough to rebuild deterministically after
a chunk crossing or teleport.

### C. Travel LRU

Keep the most recently used 2,400 chunks irrespective of an exact current
bubble. It is bounded, but spends memory behind the player, does not guarantee
a complete collision core, and makes replay depend on traversal history. It
was rejected for the authoritative near field.

### D. Sparse voxel DAG / compressed bricks everywhere

A hierarchy can preserve distant caves and edits more efficiently, but using
it as the first interaction representation would require new collision,
editing, meshing, and persistence semantics simultaneously. It belongs in the
mid/far hierarchy behind this near-field contract, not as a prerequisite for
bounding the existing hot path.

## Chosen implementation

- A four-chunk core is always distance-first so steering prediction cannot
  evict collision/editor space immediately around the player.
- Outside the core, eight-sector quantised camera and inferred movement hints
  bias the deterministic column order without reacting to tiny mouse jitter.
- Sparse edited snapshots are never deleted by streaming. Nearby edited
  columns receive priority inside the already-conservative vertical envelope;
  distant edits remain persisted until the player returns.
- Candidate planning does not synchronously sample terrain heights. The first
  measured version performed 25 height samples for each of up to 797 columns
  and took 45.065 ms on a cold teleport. Reserving the vertical envelope and
  resolving only admitted column tops incrementally reduced the same cold plan
  rebuild to 0.457 ms (98.99% lower) without weakening the hard bound.
- Every request-plan rebuild increments a monotonic epoch. Still-valid
  deterministic jobs are retagged; jobs outside the exact new set are dropped,
  and a result must match both epoch and request membership before install.
- Column-top and horizon caches follow the fixed candidate disc, so a world
  tour cannot accumulate path-length metadata.
- NeuroCore pressure automatically contracts dense radius to 11 chunks when
  throttled and 8 when critical. Nominal pressure can use 12 or 9 before a
  critical state. The minimum four-chunk interaction core is retained. This
  requires no settings ritual from the user and does not shorten the separate
  far-field horizon.
- Mesh-bucket entity count and lifetime peak are telemetry only for now. There
  is intentionally no hard 1,800-entity rejection until a parent/fallback
  representation can replace rejected near meshes without creating holes.

## Proof after the change

The world suite now contains 17 passing tests. New invariant tests cover:

- a requested visual RD of 10,000 still produces exactly 2,400 unique dense
  requests and a 16-chunk candidate radius;
- a 320-step synthetic route jumping approximately 2.2 km per step keeps
  requested and settled resident peaks below 2,400 and terrain jobs at or
  below 96;
- a roughly 141,000-chunk-space diagonal teleport cancels all 96 old jobs and rejects an old
  epoch result;
- dense eviction removes a loaded edited chunk while preserving its persisted
  `EditedChunkOverride`;
- automatic pressure states resolve to deterministic 16/11/8-chunk caps and
  handle non-finite pressure conservatively.
- current mesh-bucket entities and their lifetime peak remain exact telemetry
  even after the current set falls back to zero.

The focused suite completed in 0.05 seconds after compilation on the current
Windows/Rust 1.92.0 development environment. These are structural correctness
and bounded-work measurements, not a frame-rate claim.

## Known limits and required real-engine QA

- A dropped Bevy task handle guarantees that its result cannot be installed;
  work already executing on a worker may finish before cancellation is
  observed. The 96/64 handle and install bounds still hold.
- Dense coverage radius varies with terrain height because columns are kept
  complete. The far-field clipmap must visually cover the transition.
- Full-chunk storage is bounded, but mesh/material complexity, persistent edit
  snapshots, vegetation, water, bots, and far-field work have their own
  separate budgets.
- Current/peak mesh-bucket entity telemetry must be reviewed before an exact
  entity admission cap is enabled.
- Synthetic tests do not replace visual proof. The prepared route is:

```powershell
.\scripts\planetary-streaming-qa.ps1 -Profile both -Seed 12345 -Scenery lush -DistanceKm 30 -Seconds 60 -ScreenshotInterval 6 -Configuration Release -Build
```

Run it only with the shared GPU slot available. Accept only if full-chunk
current and peak residency stay bounded throughout the route, stale epochs do
not install after jumps, edited terrain returns correctly, the dense/far seam
has no holes, and screenshots show no popping, cracks, z-fighting, or black
horizon bands.
