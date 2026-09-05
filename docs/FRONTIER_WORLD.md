# The Frontier World

Every world this engine generates is the same planet: a neon frontier of
banded canyon mesas under a nebula sky, with glowing channels in the
canyon floors, elevated highways bridging them, islands hanging in the
air and docking platforms above those.

This document covers how that is built and, more usefully, *why* each
piece is built the way it is. The bot city planner has its own maths
write-up in [`CITY_PLANNER_MATH.md`](CITY_PLANNER_MATH.md).

## Provinces

`TerrainGenerator::region` classifies every world column into one of nine
macro provinces using three orthogonal very-low-frequency noise channels
(`a`, `b`, `c`, all around 0.0002 Hz per block, so a province is hundreds
of chunks across). Each province owns a point in `(a, b, c)` space; the
nearest point wins.

| Province | What it makes |
|---|---|
| Canyon (two centres) | Banded mesa tables and deep slot canyons |
| CrystalSpires | Hex-prism spire fields on a glow-sand floor |
| VolcanicWaste | Basalt flats cut by lava channels |
| GlacierShards | Razor ice ridges and crevasse bowls |
| AlienReef | Bioluminescent moss hills and bone pillars |
| Karst | Vertical limestone pillars over jungle floor |
| Highland | Alpine ridge silhouettes |
| Plateau | Vast tablelands |

Canyon gets two centres because banded mesa country is the terrain the
player crosses between the rarer set-pieces. It should be the connective
tissue of the map, not one province in nine.

Province strength is the squared-distance margin over the runner-up,
scaled by 7 and clamped to 1. Nine points sit closer together than the
five earth-like provinces this replaced, so without the steeper ramp the
entire map would sit in a permanently half-blended boundary state. Below
strength 0.10 the column falls back to mixed green country, which is what
gives the frontier its transitional valleys.

## Strata

`frontier::strata_block` maps a world Y to one of violet, brick, ochre,
buff or terracotta on a 34-block cycle.

It is a pure function of Y and nothing else. That is the whole point: two
adjacent columns, generated in different chunks on different threads,
must agree on the band at a given height, or cliff faces dissolve into
noise instead of resolving into the horizontal stripes that define every
rock face in the reference art.

Biomes with a strong material identity of their own — crystal, basalt,
ice, bone — keep a solid core so their silhouettes stay readable.

## Skyline compression

Crystal spikes and reef pillars naturally stack past 260 blocks. The
streamer only loads chunk `y` in `[0, vertical_chunks)`, so anything
above that ceiling is not *tall*, it is decapitated: the player sees a
spire sheared off into a flat table.

Rather than clamp — which produces exactly that — `surface_height`
compresses everything above a knee at y=118 by a factor of 0.24. Relief
below the knee is untouched, and the tallest hero silhouettes taper into
the sky.

The default vertical budget is 10 chunks (160 blocks). A test asserts
that no sampled peak reaches it.

## Lattice-anchored landmarks

Four features cannot be expressed as a height field, and all four
straddle chunk boundaries. They live in `frontier.rs` and are **pure
functions of world coordinates** anchored to a coarse lattice, so a
feature produces identical voxels regardless of which chunk is generated
first, in which order, or on which thread. No cross-chunk bookkeeping, no
deferred population pass.

| Feature | Lattice | Notes |
|---|---|---|
| Crystal clusters | 96 blocks | Tilted tapering shards, diamond cross-section, rooted on the true surface |
| Sky islands | 208 blocks | Wobbled slab, green cap, tapering crystal root |
| Sky stations | 512 blocks | Disc, tapered hull underside, holo-windowed tower, docking arms, mast |

Sky islands lift by `30 + radius + jitter`. The radius term matters: a
wide island grows a proportionally longer root, and a fixed lift would
plant that root in the dirt.

Airborne features clamp into `[86, 138]` (islands) and `[112, 132]`
(stations) so the whole silhouette, root and mast included, fits inside
the streamed slab. A landmark above the ceiling is generated, meshed and
never seen.

## Skyways

Roads follow the **zero-contour of a low-frequency noise field**. Contours
of a smooth field wind endlessly, never dead-end, and never need any
global route planning.

The naive version balloons: wherever the field flattens, the band
`|value| < threshold` covers a huge area. So the value is normalised by
the gradient:

```
dist ≈ |f(x, z)| / |∇f(x, z)|
```

which is the first-order distance to the contour in blocks, giving a
constant-width carriageway. The gradient costs four extra noise samples,
but an early-out at `|f| > 0.05` keeps 19 columns in 20 down to a single
sample.

Deck height rides `TerrainGenerator::macro_height` — continentalness and
erosion only, no hills, no ridges, no province modifiers. That is the
surface the terrain *would* have if it were sanded flat. A deck offset
from it stays level while the real ground heaves eighty blocks up into a
mesa (the road becomes a cutting) or drops away into a canyon (the road
becomes a bridge on pylons). Offsetting from the true surface instead
would give a road that bucks over every ridge it crosses.

Decks win against terrain, and their headroom is force-cleared, so a
route is always drivable.

## Energy rivers

Two independent contour networks, not one with a hot/cold mask: the key
art has an orange lava river and a blue plasma river threading the same
canyon system, and a single field can only ever give one colour per
region. Where the two cross, the hotter one wins the bed.

The channel profile is parabolic — deepest at the centreline, feathering
to nothing at the banks — so a cut never leaves a vertical wall.

The carve happens inside `surface_height`, not in `generate`. Every
consumer of the height field (spawn search, bot siting, ship landing,
collision) has to agree the channel is there, or things get placed inside
rivers.

Fluid level is measured **down from the natural ground line**, not up
from the bed, so the surface is flat right across the channel. Filling a
fixed depth above the bed would give a parabolic river climbing its own
banks.

## Palette and light

Two failure modes govern the colour work, and both are locked down by
tests in `blocks.rs`:

**Ground must survive linear conversion.** sRGB values in the 0.1–0.3
range look like reasonable colours in a swatch and land at 0.01–0.06 in
linear light. At that level every rock and grass surface renders as a
black silhouette while the emissive blocks blow out around it.
`walkable_ground_materials_survive_conversion_to_linear_light` enforces a
minimum linear luminance on everything the player stands on.

**Structure must stay under the bloom threshold.** Decks, plating and
lane paint frame the neon; they must not out-glare it. Bright plating
turns every skyway into a glare streak.
`structural_surfaces_stay_below_the_bloom_threshold` pins their peak
linear channel below 0.40.

Prop and speck densities are tuned against the lit palette. Densities are
per-candidate with 24 candidates per chunk, so the numbers look smaller
than they are: 0.02 averages half a prop per chunk, while anything near
0.05 puts an outpost in every chunk, which at render distance reads as a
field of glowing litter rather than an inhabited frontier.

The sky splits its work. `daynight.rs` drives one flat dome colour, which
is what keeps the fog and the sky matched and hides the streaming edge
for free — but a flat sky is the one thing the key art never has. It
therefore keeps only a hint of sunset, and `sky.rs` adds a latitude
gradient on a shell outside the nebula: warm at dusk, violet at night,
cool at noon, fading out well before the zenith. It blends additively, so
it can only brighten and can never open a seam between the sky and the
fogged horizon.

## Capturing a frame

The QA autopilot flies a deterministic route and saves screenshots:

```bash
VOXEL_NATIVE_QA=1 \
VOXEL_NATIVE_QA_HOUR=17.9 \
VOXEL_NATIVE_QA_RENDER_DISTANCE=32 \
cargo run --release
```

`VOXEL_NATIVE_QA_RENDER_DISTANCE` pins the horizon and disables adaptive
streaming. Without it the governor throttles the render distance down on
a slow or software renderer, and the run captures a picture of fog rather
than a picture of the world.
