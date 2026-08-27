# Natural River Bank Grammar V3

## Decision

Terrain grammar V3 changes only the Natural-profile hydrographic cross-section.
It replaces V2's single continuous bank envelope with three ordered vertical
zones driven by the existing `HydrographicField`:

1. a submerged bed at `WATER_LEVEL - 2` blocks;
2. a sediment shelf at `WATER_LEVEL + 1` blocks after the canonical shoreline
   snap;
3. a low living cap beginning at `WATER_LEVEL + 3` blocks.

V1 and V2 remain separate byte-established code paths. Astral continues to use
the V1 hydrographic carve. Reverting a world identity to V2 is the complete
rollback boundary; no migration, cache reinterpretation, or edit-store rewrite
is permitted.

## Baseline and observed V2 failure

The release comparison is the Natural/Lush world at seed `12,345`, river focus
`(-64, 64)`, with Hydro and Far Cohorts disabled so those systems cannot hide
the near-terrain silhouette. The real engine inspection found a repeated,
immediate bank face along the channel: V2 could satisfy a transverse average
slope gate while adjacent tangent slices still aligned their steepest voxel
edge into a palisade.

V2 has one target envelope:

```text
bed + 6 blocks * (1 - channel_blend)
```

and blends that target with `sqrt(corridor)`. It has no explicit low shelf.
Consequently a rounded cross-section can move directly from water to its living
bank even though the underlying floating-point function is continuous.

The fixed-anchor programmatic replay makes that deficit concrete: across 33
tangent slices and both sides, V2 exposes only 1 to 2 consecutive sandy shelf
voxels. V3 exposes 4 to 5. The old maximum-adjacent-rise check alone cannot
distinguish those silhouettes, which is why the shelf-width gate is now an
independent release condition.

The V3 release gate therefore evaluates all 33 recentered tangent slices at
offsets `-16..=16`, both banks independently. Every slice must expose a bounded
sediment shelf before outer relief, and no immediate transition may rise by
three or more blocks.

## Selected formula

All heights below are in vertical voxel blocks. `corridor` and `channel` are
dimensionless weights clamped to `[0, 1]`. These are authored visual units, not
metres and not outputs of an erosion or shallow-water simulation.

```text
bed = WATER_LEVEL - 2 blocks
living_to_shelf = smoothstep(0.26, 0.50, channel)
shelf_to_bed    = smoothstep(0.66, 0.90, channel)

target = bed
       + 3 blocks * (1 - shelf_to_bed)
       + 2 blocks * (1 - living_to_shelf)

envelope = min(pre_carve_height, target)
output   = lerp(pre_carve_height, envelope, fourth_root(corridor))
```

The thresholds are ordered, so increasing channel strength can never raise the
target. The interval from channel weight `0.50` through `0.66` is an exact
three-block sediment shelf before the outer two-block living cap. The existing
shoreline authority maps its exposed surface to `WATER_LEVEL + 1`, which keeps
the shelf sandy through the established Beach palette. Both transitions are
smooth before integer rounding; there is no hard height quantization in the new
function.

At the living cap, V3 gives the riparian moisture classification priority over
Natural regional rock palettes. This is deliberately grammar-scoped: a Karst
region can no longer repaint the first V3 shoulder as limestone, while V1, V2,
and every Astral branch retain their established palette order.

V3 also groups the *focus score* (not its rejection gates) into four-block
context-relief bands. The new bed rounding had otherwise made one probe differ
by one block and moved the deterministic QA camera to an equally valid opposite
river bend. Exact maximum-height and relief caps remain unquantized; the score
band keeps seed `12,345` anchored at `(-64, 64)` for the bounded visual
comparison. The camera-route plan hash intentionally includes terrain grammar,
and the preflight may choose a different safe route variant when the generated
bank geometry changes; therefore V2/V3 is not a pixel-identical camera A/B.
The fresh Release runs must report the exact anchor and selected variant, and
the visual verdict must disclose any variant change rather than treating the
hashes as equal.

## Candidate selection

Three bounded candidates were considered:

- Stronger corridor easing alone was rejected. It widens V2's influence but
  cannot guarantee a sediment shelf because the cross-section still has only
  one continuous target.
- Hard height terraces were rejected. They produce a shelf locally but turn an
  iso-channel contour into a long, authored retaining wall—the same failure in
  a different place.
- Nested smooth envelopes were selected. They add the missing topological zone
  while retaining monotonicity, totality, seam stability, and constant work.

The morphology is intentionally modest. NRCS stream-restoration guidance
describes cross-sections as ordered bed, toe, bank, overbank, transitional, and
upland zones, while USGS monitoring guidance distinguishes the streambed,
low-lying sand or gravel bars, banks, and nearby terraces. Those references
support the zone ordering, not the project's authored voxel dimensions:

- [NRCS National Engineering Handbook, Stream Restoration Design Process,
  figure 4-6](https://directives.nrcs.usda.gov/sites/default/files2/1712931087/7328.pdf)
- [USGS Reconfigured Channel Monitoring and Assessment Program](https://www.usgs.gov/centers/colorado-water-science-center/science/reconfigured-channel-monitoring-and-assessment)

## Fixed budgets and failure mode

V3 adds:

- zero noise or terrain queries;
- zero allocations;
- zero retained state;
- two `smoothstep` evaluations and one additional square root per affected
  column;
- constant `O(1)` work and the existing world-height clamp.

Non-finite hydro weights fail closed to zero influence. A non-finite pre-carve
height returns the finite bed height for the existing bounded integer
conversion. For every finite input, the output is finite and never exceeds the
pre-carve height.

The expected failure mode is over-broad shelf exposure where the existing
channel field changes unusually slowly. The pure acceptance gate therefore
bounds shelf width before outer relief in addition to enforcing a minimum
multi-voxel shelf. A failed bound rejects V3; it must not be repaired by adding
neighbor queries or retained cross-section state.

## Deterministic acceptance contract

The colocated `terrain` tests are authoritative and must cover:

- totality, replay, output bounds, and monotonicity in both hydro weights;
- exact V1 formula replay and exact V2 formula replay;
- distinct V1, V2, and V3 Natural chunk bytes;
- seed `12,345` at the fixed `(-64, 64)` anchor;
- all 33 recentered tangent slices and both banks, with no immediate rise of
  three or more blocks;
- exactly 4 to 5 sandy shelf voxels per bank at that anchor before a green
  living cap, compared with the recorded V2 range of 1 to 2;
- multiple seeds, signed chunk seams, reversed query order, and coordinates at
  both `i32` extremes.

Passing pure tests does not replace real visual QA. The final release decision
still requires a fresh engine run and inspection of every captured frame; V3
must remain isolated from any later far-horizon skirt experiment so attribution
stays exact.

## Local pure evidence

Validated on 2026-08-21 with:

```text
cargo test --bin voxel-native terrain::tests -- --nocapture
74 passed; 0 failed
```

The fixed Natural/Off chunk at `ChunkPos(-4, 3, 4)` replays FNV-1a checksums
`0xbca76b20990e392e` for V1 and `0x064918f3e974c9ab` for V2. The seed `12,345`
V3 anchor remains `(-64, 64)`; its 33 tangent slices produce 66 checked bank
sides, each with a 4-to-5-voxel sand shelf, first outer cap at
`WATER_LEVEL + 3`, and maximum adjacent rise below 3 blocks.
