# Virtual Voxel Hierarchy

Status: implemented as a pure, reversible data layer in
`src/virtual_voxel_hierarchy.rs` and compile-registered by `src/main.rs`. No
live world scheduler, Bevy system, mesher, renderer, physics path, or save path
consumes it yet. Compilation is therefore evidence for the API and invariants,
not evidence that the feature is visible in an engine route.

## Decision and measurable baseline

The engine already has the two ends of planetary rendering:

- full `16^3` chunks for interaction, editing, physics and near rendering;
- six fixed-topology height/material clipmap rings for the distant horizon.

The missing middle is not another copy of either representation. A raw full
chunk currently carries 4,096 `u16` voxels and 4,096 `u16` material ids, or
16,384 payload bytes before `Arc`, chunk and mesh overhead. Extending that
dense representation to kilometres grows with every X/Z column and occupied Y
chunk. The far clipmap stays constant, but cannot preserve caves, overhangs,
structures or sparse edits.

The implemented middle layer uses an `8^3` brick of four-byte summaries:

| Measurement | Result |
| --- | ---: |
| summary cell | 4 bytes (`u16` material + `u8` occupancy + `u8` error) |
| brick payload | 2,048 bytes |
| production resident cap | 512 bricks |
| fixed accounted resident ceiling | 1,093,632 bytes |
| active generation ticket cap | 128 tickets |
| fixed inline ticket storage | 7,168 bytes |
| represented source volume at LOD 2 | `32^3` voxels per brick, 64x raw-payload reduction |
| represented source volume at LOD 4 | `128^3` voxels per brick, 4,096x raw-payload reduction |

“Accounted resident” includes every reserved clock slot, every reserved sorted
lookup entry and the maximum fixed brick payload allocations. Ticket storage
is a separate fixed inline array and is reported separately rather than hidden
inside the cache number. Allocator bookkeeping and the hierarchy value's small
fixed control fields are platform-specific and are not included. Sparse
authoritative edit records are save data and intentionally have a separate,
edit-count-proportional budget; cache pressure never owns or evicts them. The
public checked byte-accounting function returns `None` on multiplication
overflow instead of wrapping.

## Candidate comparison

Four approaches were evaluated against determinism, edit survival, bounded
travel and integration risk.

### 1. Expand full chunks to the visual horizon

This reuses every existing gameplay path and preserves perfect detail. It was
rejected because kilometres of dense chunks make memory, generation, meshing,
entities and physics candidate work scale with explored area and vertical
occupancy. A larger render-distance setting would still be a larger memory
setting.

### 2. Pointer-rich sparse voxel octree or global DAG

An octree can be very compact in uniform terrain and a DAG can deduplicate
identical subtrees. Both are attractive long-term storage experiments. They
were rejected for this first middle layer because allocation count and memory
pressure depend on world entropy; updates need path copying or complicated
reference/version management; and deterministic persistent edits become part
of the tree's ownership model. Those properties make the first integration
hard to budget and hard to roll back.

### 3. Fixed summary bricks plus deterministic clock residency (chosen)

Every brick has the same 2,048-byte payload. Integer keys identify level and
world brick coordinate. A sorted fixed-capacity lookup and second-chance clock
choose victims deterministically. The public constructor always uses the
compile-time cap of 512 bricks; players cannot turn a visual option into an
unbounded cache. Bricks are reconstructible from generator data plus the
separate edit overlay.

The trade-off is deliberate quantisation. A coarse cell keeps dominant
material and conservative error, not every source block. Scheduler error 255
on a partially edited coarse cell forces refinement instead of pretending the
coarse result is exact.

### 4. GPU-only occupancy/material atlas

A toroidal 3D texture could make sampling and eviction cheap on the render
side. It was rejected as the authoritative middle data layer because CPU
scheduling, edit propagation, deterministic tests and save reconstruction
would need a second representation or GPU readback. A future renderer may
upload these exact bricks into such an atlas without changing their ownership
contract.

The chosen module is reversible: it has no Bevy dependency and, despite compile
registration in `main.rs`, has no system or plugin wiring and no changes to the
current chunk or clipmap authority. Runtime integration can be feature-gated
and removed without migrating world saves because summary bricks are caches,
not world authority.

## Coordinate and reduction contract

- Level 0 has one source voxel per macro cell and an eight-voxel brick edge.
- Every next level doubles cell span on every axis.
- The production hierarchy accepts LOD 0 through configured LOD 12; the format
  hard limit is 30. Address creation checks the hard limit, and every hierarchy
  generation, installation, overlay and sampling path also checks the
  configured limit.
- All world and span arithmetic uses checked `i64` intermediates.
- Brick coordinates remain `i32`; an out-of-range conversion returns an error.
- Euclidean division and remainder make negative boundaries exact. World `-1`
  maps to brick `-1`, local cell `7`; world `-8` maps to brick `-1`, cell `0`.
- Child-to-parent reduction recovers octants from keys, so asynchronous child
  completion order cannot change output.
- Public scalar reduction has only two bounded shapes: exactly eight child
  summaries or exactly 512 brick summaries. Both use fixed stack storage; no
  public caller can request an arbitrarily large reducer allocation or loop.
- Material votes are occupancy-weighted. Equal weight always selects the lower
  material id. The reducer uses only fixed stack storage and never iterates a
  hash map.
- `CellSummary::is_empty()` means *known empty*: occupancy and refinement error
  must both be zero. Any positive mass is conservatively quantised to at least
  occupancy 1 at every reduction/overlay LOD, and an error-only cell is not
  treated as empty.
- Error is at least the largest inherited error, occupancy range, or secondary
  material weight. It is a refinement signal, not geometric distance in metres.

The fixed cell index matches chunk-friendly X-contiguous order:

```text
index = x + z * 8 + y * 64
```

## Version and epoch safety

Every brick carries:

```text
world epoch + source version + per-brick overlay version
```

The epoch changes when the active world changes. Cache lookup, generated-task
installation and edit writes reject a stale epoch. Source version is a single
strictly monotonic hierarchy authority rather than a travel-key map: advancing
it clears reconstructible residency and active tasks while retaining sparse
edits. This keeps freshness storage bounded and prevents eviction from erasing
the knowledge required to reject an old generator result.

`begin_generation` issues a `GenerationTicket` containing key, full stamp and a
monotonic task nonce. At most 128 keys may have active work. Reissuing a key
replaces its old nonce; edits cancel affected key/ancestor tickets; source and
epoch changes cancel all tickets. Installation consumes the exact active
ticket, so a result cannot become acceptable merely because its prior resident
brick was evicted.

Resident stamp comparison is component-wise. A candidate with either a lower
source or overlay version is stale; crossed components fail closed. Equal stamp
plus equal payload is idempotent, while equal stamp plus different payload is a
conflict and preserves the resident value. An edit increments its monotonic
per-brick overlay version and invalidates the affected brick at every configured
level, so a pre-edit result cannot overwrite it.

## Persistent sparse edit overlay

`SparseEditOverlay` owns exact replacement records independently of residency:

```text
(world voxel, original before, latest after, epoch, version)
```

The original `before` value survives an edit chain. This lets a regenerated
coarse base adjust occupancy mass by the final delta after its old summary has
been evicted. Each record is indexed into one macro cell at every active level.

At LOD 0 the edited summary replaces the exact source voxel. At coarser levels:

- occupancy changes by the edit's before/after delta divided by macro-cell
  volume;
- any positive occupancy mass is clamped to quantised occupancy 1 instead of
  rounding to known-empty at a coarse LOD;
- an edit into an empty base supplies a deterministic dominant material;
- an existing coarse dominant material is preserved until finer data exists;
- error becomes 255, forcing refinement around uncertain local structure.

`snapshot()` returns records in deterministic version/coordinate order.
`from_snapshot()` rebuilds every derived index, so the index and all resident
bricks may be discarded and reconstructed without losing an edit. The engine's
save layer still needs to serialize this snapshot alongside its existing full
edited-chunk compatibility data.

## Self-budgeted residency and pressure behaviour

The cache and overlay are private implementation details; external callers can
only install through hierarchy-owned tickets. The public hierarchy constructor
does not accept a capacity. It always reserves exactly 512 clock slots and 512
sorted lookup entries, plus the inline 128-ticket table. Under pressure:

1. a lookup marks its slot referenced;
2. the clock clears referenced slots during the first pass;
3. the first unreferenced slot on the deterministic hand path is reused;
4. only the `SummaryBrick` is dropped;
5. authoritative edits remain in `SparseEditOverlay` and can reconstruct the
   brick when requested again.

Ticket saturation returns `ActiveTaskLimitReached` rather than allocating more
work. Replacing a ticket for the same key does not consume another slot.

Camera distance and travel duration therefore change cache contents, never the
resident ceiling. The invariant test travels 20,000 km, from -10,000 km to
+10,000 km, through 20,001 generated requests with a deliberately tiny
31-brick test cap; resident count and accounted bytes stay unchanged at the cap.

## Pure integration API

Near generation should use this sequence:

1. When the deterministic generator/grammar changes, call
   `advance_source_version` once with a strictly greater global version.
2. Call `begin_generation(key)` before launching work. Keep the returned
   `GenerationTicket`; its stamp is the only valid payload stamp.
3. Convert full voxel/material data into exactly 512 `CellSummary` values and
   return `SummaryBrick::from_cells(ticket.key(), ticket.stamp(), cells)`.
4. Call `install_generated_base(ticket, brick)`. It validates current
   epoch/source/overlay/nonce and exact key/stamp, applies authoritative edits,
   installs the reconstructible cache value, then consumes the ticket.

Parent generation calls `begin_generation(parent_key)` and then
`reduce_and_install_parent(ticket, children)`. The hierarchy requires one
complete eight-child spatial group and validates every child against the
current epoch, global source, that child's authoritative overlay version, and
the exact current resident payload before reduction. A newer sibling cannot
mask a stale edited sibling. There is deliberately no public unvalidated
resolved-brick installation path.

Far and renderer consumers call `sample(world_voxel, lod, epoch)`. A cache miss
means “request or keep the valid parent/far fallback,” never “empty world.”

Edit integration calls `record_edit(EditRecord)`. It invalidates only impacted
resident keys and returns those keys so a future scheduler can prioritise their
rebuild. `edit_snapshot()` supplies deterministic save data.

## Verification and measured hot paths

The module compiles standalone with `rustc --edition=2021 -D warnings`; its
optimized test binary contains eighteen focused tests covering:

- four-byte payload, checked byte accounting, fixed cache bytes and fixed task
  saturation;
- Euclidean negative coordinates, checked integer extremes and configured LOD
  rejection on every public hierarchy path;
- deterministic fixed-eight and fixed-512 reduction, including positive-mass
  and error-only cells never becoming known-empty through all LODs;
- pure parent order independence plus hierarchy-owned complete-child/current-
  resident validation and stale-sibling masking rejection;
- component-wise stamp ordering, equal-stamp idempotence/payload conflict,
  replay rejection after ticket replacement, eviction, edits, source changes
  and epoch changes;
- repeatable clock victims and a 20,001-request, 20,000-km bounded-residency
  route;
- edit survival across eviction, reconstruction and snapshot replay, plus
  conservative coarse occupancy/error behavior;
- hot sampling, fixed-eight reduction and worst-case fixed-512 reduction with
  checksums.

Thirty consecutive optimized runs on the current Windows host measured. Each
run performed 5,000,000 resident samples, 2,500,000 eight-cell reductions, and
2,000 worst-case 512-cell reductions with 512 distinct positive materials:

| Hot path | Minimum | Median | p95 | p99 / maximum |
| --- | ---: | ---: | ---: | ---: |
| resident address + lookup + sample | 14.32 ns | 14.44 ns | 14.78 ns | 15.53 ns |
| reduction of eight summaries | 30.88 ns | 31.01 ns | 31.34 ns | 31.47 ns |
| worst-case reduction of 512 summaries | 1,898.55 ns | 1,912.28 ns | 1,958.90 ns | 1,997.45 ns |

These are microbenchmarks, not a frame-time guarantee. They run one resident
brick and stack-resident fixed-shape reduction input in an optimized standalone
test binary. Engine scheduling, generator sampling, meshing, GPU upload and
cold-cache behaviour must be measured separately after runtime integration.

### Verification transcript and release boundary

| Command | Result |
| --- | --- |
| `rustfmt --edition 2021 --check src\virtual_voxel_hierarchy.rs` | pass |
| `rustc --edition=2021 -D warnings --crate-type lib src\virtual_voxel_hierarchy.rs` | pass |
| `rustc --edition=2021 -D warnings --test -O src\virtual_voxel_hierarchy.rs` followed by `--nocapture --test-threads=1` | 18 passed, 0 failed |
| `cargo test --bin voxel-native virtual_voxel_hierarchy::tests --quiet` | 18 passed, 0 failed; remaining tests filtered |
| `cargo check --bin voxel-native` | pass; known repository dead-code warnings remain |
| `cargo check --target wasm32-unknown-unknown --bin voxel-native` | pass; known target-specific warnings remain |
| `cargo test --workspace --quiet` | pass at the documented local verification checkpoint; a long-lived total is deliberately not pinned here |

Native and Wasm Cargo checks prove that the public module compiles as part of
the application. They do not prove runtime scheduling, renderer consumption,
visual continuity, save compatibility, or a Level-9 engine route.

## Explicit integration gaps

The pure data layer is compile-registered but intentionally runtime-disconnected
until the rollout, authority, and evidence gates below are implemented.
Remaining work is therefore visible and bounded:

1. Wire the module behind an Astral-first runtime feature/rollout gate.
2. Add a near-chunk reduction worker and connect its authoritative grammar
   revision to `advance_source_version`.
3. Add a midfield scheduler that requests bricks by projected error while
   preserving parent coverage during child promotion.
4. Add a brick surface/cavity mesher or GPU atlas consumer.
5. Feed large edited structures into far-clipmap silhouette correction.
6. Serialize `edit_snapshot()` atomically with existing edited chunk saves and
   restore it before accepting generated summaries.
7. Export cache hit/miss, eviction, stale-rejection, edit-index and build-time
   telemetry to Mission Control.
8. Perform real-engine flight QA for parent/child morphing, overhangs, edits,
   negative coordinates, teleports and high-speed traversal.

Vegetation wind and every other far/midfield presentation effect remain
render-only. This hierarchy exposes no force, collider, rigid body or shuttle
physics API, so extending visual distance cannot influence flight mechanics.
