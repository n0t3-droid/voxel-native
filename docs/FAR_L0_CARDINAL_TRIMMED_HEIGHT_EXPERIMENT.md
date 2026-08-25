# Far L0 cardinal-trimmed height experiment

Status: bounded diagnostic experiment. `Point16V1` remains the default and
shipping rollback. `CardinalTrimmed8V1` is non-persistent, non-authoritative,
and cannot produce publishable release evidence under the current manifest
schema.

## Decision boundary and baseline

The current blocker is no longer an unidentified generic far-terrain artifact.
The retained provenance and culling diagnostics established that the dominant
Natural and Astral walls are **front-facing L0 top surfaces**. They are not Near
terrain, terminal skirts, back faces, or a material-colour illusion. This
experiment may therefore change only the displayed height of existing L0 top
vertices. It may not change voxel authority, the terrain grammar, the Near
coverage handshake, ring topology, Hydro classification, material identity,
or any save.

Two finer-topology experiments have already been rejected by controlled native
A/B inspection and rolled back:

- the seven-ring experiment with an inner 8 m level did not remove the
  identified wall sufficiently;
- the bounded `K = 112` adaptive half-step refinement saturated on the tested
  routes and changed facets without materially reducing the wall.

Those failures narrow the next question. The new hypothesis is that an exact
16 m L0 centre can occasionally be the sole axial height outlier among its four
8 m cardinal neighbours. One such sample then controls several large existing
triangles and can create a large projected slab. Replacing only that isolated
display extreme may reduce the slab without increasing geometric population.
This is a falsifiable rendering hypothesis, not a statement that the centre is
geologically wrong.

The 8 m probe spacing is an **engineering diagnostic scale** derived exactly as
half of the existing 16 m L0 step. It is not a physical, surveying, hydrologic,
or realism standard. World heights and horizontal distances in this contract
are authored voxel metres unless stated otherwise.

This boundary follows the render-cache and authority rules in
[ELITE_WORLD_SYSTEMS_STANDARD.md](ELITE_WORLD_SYSTEMS_STANDARD.md), the retained
six-ring and handoff contract in
[PLANETARY_STREAMING_PHASE1.md](PLANETARY_STREAMING_PHASE1.md), and the
cross-system inspection requirements in
[RESPONSIVE_VISUAL_QA.md](RESPONSIVE_VISUAL_QA.md).

## Exact candidate algorithm

The runtime modes are:

| Mode | Environment value | Meaning |
| --- | --- | --- |
| `Point16V1` | unset, `point-16-v1`, `point_16_v1`, `default`, or an unknown value | retained exact 16 m point display; default and rollback |
| `CardinalTrimmed8V1` | `cardinal-trimmed-8-v1` or `cardinal_trimmed_8_v1` | explicit L0-only diagnostic |

The non-persisted gate is `VOXEL_NATIVE_FAR_L0_HEIGHT_MODE`. On WebAssembly the
mode is always `Point16V1`. Only the explicit diagnostic spelling may enable
the candidate; a typo must fail closed to the retained point mode.

Let the L0 anchor be `(ax, az)` in signed integer world metres. The existing
centre cache remains a 65 by 65 plane with logical `gx, gz = -32..=32`:

```text
C[gx, gz] = h(ax + 16*gx, az + 16*gz)
```

The candidate adds two optional fixed planes only to the L0 cache:

```text
X[ex, gz] = h(ax + 16*ex + 8, az + 16*gz)
             ex = -33..=32, gz = -32..=32    # 66 * 65 = 4,290

Z[gx, ez] = h(ax + 16*gx, az + 16*ez + 8)
             gx = -32..=32, ez = -33..=32    # 65 * 66 = 4,290
```

All multiply/add operations use the existing checked signed-integer coordinate
path. A coordinate that cannot be represented follows the existing clamped or
rejected-query accounting; it must not wrap, alias another world position, or
grow a sparse coordinate map.

For each of the existing 61 by 61 visible L0 top vertices, form exactly this
five-value local sample:

```text
v = [
    C[gx, gz],
    X[gx - 1, gz],
    X[gx,     gz],
    Z[gx, gz - 1],
    Z[gx, gz],
]
```

If every value is finite, sort the five `f32` values deterministically with
`f32::total_cmp` and display:

```text
lower   = v_sorted[1]
upper   = v_sorted[3]
display = clamp(C[gx, gz], lower, upper)
```

Including the centre in the five-value sort is deliberate. It winsorizes at
most one strict local extreme in either direction; it is not an average,
median filter, erosion pass, or resampling of the mesh. Ties are stable under
the total ordering. If any of the five inputs is non-finite, the diagnostic
falls back to the exact centre and leaves the pre-existing mesh validation to
accept or reject the result; it must not invent a replacement height.

The existing parent-morph sample remains the exact, untrimmed 16 m centre
value. The current L0-to-L1 morph therefore blends a candidate L0 display
height toward the same exact 32 m parent representation as `Point16V1`. This
preserves the established 16/32 handoff and makes any new transition kink or
pop directly attributable and rejectable.

Only the terrain vertex position uses `display`. Hydro classification, fluid
height/category, BridgeV1, BridgeV2, reduced material detail, biome identity,
Near coverage, and terrain-generation authority continue to use the exact
centre `C[gx, gz]`. Normals are recomputed from the unchanged triangle list.
This separation is required by
[FAR_HYDROGRAPHIC_CONTINUITY_V1.md](FAR_HYDROGRAPHIC_CONTINUITY_V1.md): the
diagnostic must not silently turn a display filter into new hydrographic truth.

## Fixed topology, entity, and payload budgets

The candidate adds no vertex, index, triangle, mesh entity, collider, task,
coverage cell, ring, skirt, or material. With no Near cutout, the complete
six-ring terrain population must remain exactly:

| Quantity | Exact resident population | Existing public ceiling |
| --- | ---: | ---: |
| Terrain entities | 6 | 6 |
| Terrain vertices | 23,286 | 35,000 |
| Terrain indices | 110,760 | 150,000 |
| Terrain vertex/index payload | 1,560,768 B | 2,280,000 B |

L0 remains a 61 by 61 top lattice: 3,721 vertices, 21,600 triangle-list indices,
and 265,008 B of generated vertex/index payload before any conservative Near
cutout. Only L5 retains the terminal skirt. Per-ring ceilings remain 6,000
vertices, 25,000 indices, and 388,000 B. The paired A/B evidence must show
identical per-ring and global vertex/index populations for every matched
coverage state, not merely values below the ceilings.

Hydro and semantic-cohort mesh ceilings also remain independent and unchanged:

- terrain plus Hydro is at most 653,008 B in the one atomic worker result;
- terrain plus Hydro plus the optional L5 cohort payload is at most 757,984 B;
- the maximum fully enabled far render population remains 13 entities;
- build jobs in flight remain capped at one.

The new CPU source planes are sample-cache payload, not mesh payload, and must
not be hidden inside the atomic mesh-byte counters.

## Query, cache, and work budgets

### Geometry-height queries

The exact cold or teleport-fallback L0 geometry-height budget is:

| Source plane | Samples |
| --- | ---: |
| Existing centre, 65 by 65 | 4,225 |
| Half-X, 66 by 65 | 4,290 |
| Half-Z, 65 by 66 | 4,290 |
| **Candidate L0 total** | **12,805** |

The other five rings remain at 4,225 centre samples each, so a cold candidate
six-ring population performs exactly `12,805 + 5*4,225 = 33,930`
geometry-height queries. A one-cell 16 m axial L0 retarget samples exactly 196
new heights across the three toroidal planes. A one-cell diagonal retarget
samples exactly 389: 129 centre values plus 130 values in each half plane.
Teleport and incompatible fallback may refill the fixed planes but may not
exceed the 12,805 L0 ceiling.

With the default BridgeV2 and worst-case Hydro classification, the L0 cold-job
counted-operation ceiling is 20,503:

```text
12,805 geometry-height queries
   256 BridgeV2 fixed-material-cell biome queries
 3,721 Hydro vertex classifications
 3,721 conditional Hydro biome queries
------
20,503 counted operations
```

The BridgeV1 reference path retains its separate 14,884 one-metre
material-slope probes and 3,721 material-family/biome queries. Including the
same worst-case Hydro work gives a candidate L0 ceiling of 38,852 counted
operations. These are work ceilings, not timing or visual-quality promises.

### Cache bytes

The accounting is compile-time `size_of`-derived. On the native x86_64 QA
target, adding the two optional box fields makes the fixed cache header 112 B.
The ordinary centre/material cache payload remains `4,225 * 9 = 38,025 B`, so
an ordinary ring accounts for 38,137 B. An enabled L0 adds exactly
`2 * 4,290 * 4 = 34,320 B` of `f32` probes:

```text
ordinary ring cache          =  38,137 B
candidate L0 cache           =  72,457 B
candidate six-window maximum =  72,457 + 5*38,137
                             = 263,142 B
```

The established public ceiling remains 524,288 B across exactly six resident
plus in-flight windows. A diagnostic report above 263,142 B fails this
experiment even if it remains below the broader public ceiling. The target
build must also retain the compile-time assertion against the public ceiling;
the x86_64 arithmetic is evidence-host accounting, not permission to assume a
pointer size on every target.

### Other work

Display evaluation performs at most 3,721 fixed five-value sorts per L0 build.
It uses a five-element local array and no heap, candidate list, hash map,
neighbour graph, or adaptive queue. The existing 1,545 B Near-coverage work
budget, six cache windows, one in-flight build, coalesced six-bit dirty mask,
and constant travel population are unchanged.

## Identity and report contract

`FarFieldL0HeightMode` must participate in the complete far-world key, build
request, cache compatibility, asynchronous result identity, and L0 resident
state. Changing the mode invalidates incompatible cached work. A result built
under one mode may never install after the other mode is desired; stale work
is discarded and counted through the existing fail-closed path.

The QA report must expose the mode instead of asking a reviewer to infer it
from screenshots or query counts. The `planetary_streaming` observation
contains:

- `desired_l0_height_mode`, `active_l0_height_mode`, and
  `resident_l0_height_mode`;
- `l0_probe_spacing_metres` and `budget_l0_height_queries`;
- `last_l0_center_queries`, `last_l0_half_x_queries`, and
  `last_l0_half_z_queries`;
- `last_l0_trimmed_vertices`, `last_l0_trimmed_up_vertices`, and
  `last_l0_trimmed_down_vertices`;
- `last_l0_max_abs_adjustment_metres`;
- the existing live/peak cache population, ring population, scheduler/ECS
  agreement, stale-discard, and budget-rejection fields.

For this wall-occupancy comparison, both arms use the diagnostic
`LodProvenanceV1` surface material so the analyzer can identify the exact L0
red mask. The point arm is therefore diagnostic too; it does not use the
canonical point/BridgeV2 report identity. The same binary must emit:

```text
Point16V1:
qa_report_schema_version = "2.6.0-diagnostic-lod-provenance-v1"
evidence_disposition = "diagnostic-lod-provenance-only-non-publishable"

CardinalTrimmed8V1:
qa_report_schema_version = "2.6.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1"
evidence_disposition = "diagnostic-l0-height-and-lod-provenance-only-non-publishable"
```

The analyzer also names the archived 2.5 diagnostic identities explicitly for
reproducibility. A historical four-arm cohort must be entirely 2.5, omit
`base_scale_factor`, and retain `dpi_percent == scale_factor * 100`; a current
cohort must be entirely 2.6, include `base_scale_factor`, and bind DPI to that
OS/window-backend scale. Mixed schema generations fail closed.

Those diagnostic schema strings are intentionally unsupported by the canonical
manifest rules in
[EVIDENCE_MANIFEST_SCHEMA.md](EVIDENCE_MANIFEST_SCHEMA.md). The manifest must
reject them until a future, explicit schema and validator upgrade. Renaming
either report to `2.6.0`, omitting its disposition, or copying its visual verdict
into a canonical claim is forbidden. Passing this experiment permits only a
follow-up design decision; it does not promote the filter or establish release
readiness.

The paired occupancy ledger is also diagnostic evidence. For every captured
frame it records the profile, capture index/camera pose, physical viewport
pixel count, L0-provenance wall-mask pixel count, baseline occupancy,
candidate occupancy, and reduction. It must retain the report and executable
hashes that bind the pair. A prose-only estimate or a differently thresholded
candidate mask is not acceptable evidence.

## One-binary Natural/Astral A/B procedure

Build the optimized Windows x86_64 executable once. Use that exact executable,
source fingerprint, toolchain/hardware identity, and executable hash for all
four native runs:

1. Natural `Point16V1` baseline;
2. Natural `CardinalTrimmed8V1` candidate;
3. Astral `Point16V1` baseline;
4. Astral `CardinalTrimmed8V1` candidate.

The analyzer contract fixes seed `12,345`, terrain grammar V3, Lush scenery,
the `LodProvenanceV1` surface-material mode, Hydro off, semantic cohorts off,
and a physical `1920 x 1080` viewport. Natural uses the resolved `river` focus;
Astral uses `lava`. Logical viewport, scale factor, and DPI must also match
across all four arms. Within each profile pair, additionally fix hour,
camera-plan hash and variant, capture indices and schedule, and camera poses.
Use fresh isolated QA world and run names; never reuse, clean, or overwrite user
data. Any BridgeV2 or enabled-layer route is secondary evidence and cannot
replace this isolated provenance pair because the red wall mask would no longer
have the analyzer's declared identity.

The QA runner selects `-SurfaceMaterial lod-provenance-v1`, `-Hydro off`,
`-Cohorts off`, `-Width 1920`, `-Height 1080`, and either
`-L0HeightMode point-16-v1` or
`-L0HeightMode cardinal-trimmed-8-v1`. The L0 option sets
`VOXEL_NATIVE_FAR_L0_HEIGHT_MODE` for the child process. The analyzer consumes
the four explicit run directories through `--natural-point`,
`--natural-cardinal`, `--astral-point`, and `--astral-cardinal`. A set with a
different executable hash, source fingerprint, seed, route plan, camera pose,
viewport, capture index/name, or scheduled capture time is invalid rather than
"close enough". Inspect every image and all four `report.ron` files. Mean FPS
alone cannot accept the candidate.

The screen-occupancy measurement uses the analyzer's exact L0 provenance mask,
`(R > 200) and (G < 10) and (B < 30)`, on the fixed matched frames. For a frame,
let `W` be the number of pixels in the largest eight-connected non-background
component of that mask, and let `P = 1920 * 1080` be the physical viewport pixel
count. The screen fraction is `O = W / P`. The same mask construction and
component rule must be applied to baseline and candidate; the candidate may not
win by recolouring, changing the camera, excluding difficult components, or
changing the denominator.

## Stop test

`CardinalTrimmed8V1` is rejected and rolled back immediately if any condition
below fails:

1. In **every matched frame**, candidate wall occupancy is at most half the
   baseline occupancy: `O_candidate <= 0.50 * O_baseline`. If the baseline mask
   is empty, the candidate mask must also be empty.
2. Candidate wall occupancy is at most 5% of the physical viewport in every
   matched frame: `O_candidate <= 0.05`.
3. Human inspection of all Natural and Astral captures and route motion finds
   no new crack, sky hole, missing cell, L0/L1 handoff seam, silhouette
   flattening, or temporal pop.
4. Desired, active, and resident modes converge; scheduler and observed ECS
   populations agree; no stale result is installed; every observation remains
   valid; and budget rejection/overflow flags remain clear. Discarding an
   actually stale result is permitted and must remain counted, but stale
   installation or rebuild thrash is a failure.
5. Live and peak sample-cache accounting never exceeds 263,142 B for the
   candidate six-window population.
6. Per-ring and total entities, vertices, indices, topology, and mesh bytes are
   identical to the matched point-mode state. For a complete no-cutout
   population this is exactly 6 terrain entities, 23,286 vertices, 110,760
   indices, and 1,560,768 B.

Meeting all six conditions still yields only non-publishable diagnostic
evidence. Promotion would require a new explicit release decision, canonical
schema support, full native/WASM/non-visual gates, the required viewport
matrix, and fresh release evidence.

## Alternatives not selected

### Uniform 8 m L0 over the full 960 m footprint

A uniform 8 m replacement would use 120 by 120 cells, 121 by 121 vertices,
14,641 L0 vertices, 86,400 L0 indices, and 1,048,368 B for L0 alone. Replacing
the current L0 would produce 34,206 terrain vertices, 175,560 indices, and
2,344,128 B across the six rings. It would exceed the 6,000/25,000/388,000
per-ring ceilings, the 150,000 global index ceiling, and the 2,280,000 B global
mesh ceiling. It also changes topology and upload size while testing the
height-sampling hypothesis, so a visual result would have poor causal
attribution. It is rejected for this experiment.

### `K = 1,300` adaptive half-step refinement

Expanding the rejected `K = 112` conforming refinement to `K = 1,300` could add
up to 6,500 vertices, 39,000 indices, and 468,000 B to L0. The resulting
six-ring worst case would reach 29,786 vertices and 149,760 indices, leaving
only 240 indices below the global ceiling while exceeding the existing
per-ring ceilings. It would reintroduce adaptive selection, transition masks,
saturation policy, topology changes, and additional scratch/query work after
the smaller version already failed its visual purpose. It is therefore not the
smallest high-information next test.

The cardinal-trimmed candidate is selected because it isolates display-height
estimation while preserving every geometric population and handoff identity.

## Expected failure mode and rollback

The filter cannot remove a broad wall whose centre and cardinal probes are all
part of the same extreme surface. It can also suppress a legitimate isolated
peak or pit, flatten an authored silhouette, or create a visible contrast where
trimmed L0 approaches the exact parent-morph height. Those are expected,
observable failure modes, not cases to conceal with a wider kernel, looser mask,
extra topology, or post-hoc recolouring. Any such failure stops the experiment.

Runtime rollback is immediate and non-persistent:

```text
VOXEL_NATIVE_FAR_L0_HEIGHT_MODE=point-16-v1
```

Unset and unknown values also select `Point16V1`. Code rollback removes the
render-only mode, optional half-plane boxes, counters, and diagnostic runner
surface as one unit. No save record, terrain-grammar byte, procedural centre
height, authored edit, Hydro category, collider, navigation state, or voxel
authority changes under either mode, so rollback requires no migration.

## Structural verification required before native A/B

Focused tests must prove:

- default, explicit, alias, unknown-value, and WebAssembly mode selection;
- deterministic five-value ordering, ties, trim-up, trim-down, unchanged
  centre, and non-finite fallback;
- exact half-plane coordinates across negative anchors and checked integer
  extremes;
- exact cold/teleport 12,805, axial 196, diagonal 389, and six-ring 33,930
  geometry-query counts;
- exact 38,137 B ordinary, 72,457 B candidate L0, and 263,142 B six-window
  native cache accounting under the unchanged 524,288 B public cap;
- byte-identical vertex/index topology and exact untrimmed parent-morph
  samples in both modes;
- exact-centre Hydro and material classification in candidate mode;
- mode-bound cache invalidation, stale-result rejection, pressure saturation,
  order independence, and deterministic replay;
- accurate report schema, disposition, identity, query, trim, cache, and
  scheduler/ECS fields;
- native tests, formatting, lint, and `wasm32-unknown-unknown` compilation.

Only after those checks pass may one release binary be used for the controlled
Natural/Astral procedure above.

## External context, with deliberately narrow applicability

The following authoritative sources provide general context only:

- [USGS Lidar Base Specification, Appendix 2: Hydro-Flattening
  Reference](https://www.usgs.gov/ngp-standards-and-specifications/lidar-base-specification-appendix-2-hydro-flattening-reference)
- [USGS Elevation-Derived Hydrography Acquisition Specification,
  Techniques and Methods 11-B11](https://pubs.usgs.gov/publication/tm11B11)
- [Hoppe, *Smooth View-Dependent Level-of-Detail Control and its Application
  to Terrain Rendering*](https://www.microsoft.com/en-us/research/publication/smooth-view-dependent-level-of-detail-control-and-its-application-to-terrain-rendering/)

The USGS references motivate treating terrain feature boundaries and
hydrographic breaklines explicitly; this experiment does **not** implement a
USGS breakline or hydro-flattening product. Hoppe provides context for terrain
LOD error control and temporally coherent geomorphing; it does **not** validate
this five-sample winsorization rule. The candidate preserves the engine's
existing morph and exists only to test the measured L0 wall hypothesis.
