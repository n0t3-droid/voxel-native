# Far-World Rendering: Link-Graph Synthesis and Architecture Decision

## Decision in one paragraph

Voxel-Native should keep the current constant-budget height-field clipmap as
the permanent horizon backbone, then add two *bounded supplements* instead of
expanding full chunks: a sparse semantic/voxel-brick mid-field for genuinely
three-dimensional silhouettes and a point/surfel landmark layer for remote
vegetation and structures. Material detail should use a portable virtual-page
atlas fed from the same semantic pyramid. A Voxel Space screen-column renderer,
a full-world sparse-voxel ray caster, Marching Cubes over the horizon, and
trained 3D Gaussian scenes are rejected as the primary world representation.
They either lose required 3D/edit behavior, duplicate the renderer, or move the
cost from a fixed geometry budget into view-dependent traversal, overdraw,
training, or remeshing work.

This is an architecture decision, not a claim that every layer is implemented.
The checked-in Phase-1 clipmap and its fixed toroidal strip-sampling cache are
the measured baseline. Semantic pyramids, virtual pages, sparse bricks, and
surfel landmarks below remain staged follow-up work with explicit acceptance
gates.

## How the supplied link graph was used

The canonical corpus contains 8,129 parent-child rows from 118 parent pages and
5,527 unique child URLs. Four exact duplicate edges were found; none is on a
far-world route. The duplicate rows were ignored. Parent degree was **not** used
as a quality or importance score because it is strongly biased by article size,
navigation templates, and list-heavy pages. Instead, the study followed named
routes and inspected their actual outgoing edges: GIS, height fields,
clipmaps/virtual texturing, point clouds/splatting, sparse volumes/ray casting,
and the Voxel Space/flight-simulation branch.

The corpus is a discovery graph made largely of Wikipedia links. Wikipedia was
used only to identify terminology, direct bridges, and neighbouring techniques;
all technical conclusions below come from original papers, author project
pages, standards, official API specifications, or official project manuals.

| Link-graph cluster | What was retained | What was not treated as evidence |
| --- | --- | --- |
| `Höhenfeld`, `Voxel Space`, `Raycasting` | 2.5D terrain representations, front-to-back visibility, distance-dependent sampling | Wikipedia descriptions and game anecdotes |
| `GRASS GIS`, point clouds | explicit extent/resolution, raster bands, no-data masks, overviews, point hierarchies | an assumption that a desktop GIS belongs in the runtime |
| `Octree`, voxel grids, volume graphics, texture-based volume rendering | sparse 3D residency, hierarchical empty-space skipping, ray-guided requests, volume-specific effects | an assumption that all terrain should become a volume |
| `Splatting`, point clouds | surfels and hierarchical point LOD for disconnected remote detail | treating point samples as editable/collidable world truth |
| `Marching Cubes`, segmentation, tomography | surface extraction from scalar volumes and the need for semantic reducer rules | medical workload or image quality numbers as game-runtime evidence |
| `MegaTexture`, compositing, transparency | material-page virtualization and blending risks | a single monolithic texture as a world database |
| plants, leaves, maples, shells, pixels | future visual-reference and vegetation/material vocabulary | far-world streaming conclusions |
| Comanche, Delta Force, Outcast, Teardown, Space Engineers, other games | inspiration and search leads | reverse-engineered behavior as an authoritative design specification |
| Wikipedia discussions, user pages, portals, deletion/quality pages | nothing | all were excluded from technical reasoning |

### Cross-domain bridges found in the canonical rows

These graph edges explain *why* the primary-source research was grouped as it
was. They are discovery evidence, not performance evidence.

| Route in the supplied graph | Useful bridge | Consequence for Voxel-Native |
| --- | --- | --- |
| `MegaTexture -> Clipmap / Virtual Texturing / Texturestreaming` | geometry and material LOD have the same residency problem | pair the geometry rings with a separately bounded material-page cache |
| `MegaTexture -> Octree / Sparse Voxel Octree / Raycasting` | virtual pages generalize from 2D texels to sparse 3D bricks | reuse page IDs, coarse fallback, request coalescing, and eviction telemetry across 2D and 3D caches |
| `Punktwolke -> Digitales Höhenmodell / Georeferenzierung` | point observations and 2D elevation surfaces are related products, not interchangeable truths | make both derivatives of one world transform/version contract |
| `Punktwolke -> Level of Detail / Out-of-Core / Octree` | massive disconnected detail needs a hierarchy and residency policy | remote landmarks require fixed point/node/page budgets, not a flat point list |
| `Punktwolke -> Marching Cubes` | points/volumes may be converted to surfaces | meshing is a local cache-generation option, not the streaming architecture |
| `Höhenfeld -> Displacement Mapping / Dreiecksnetz / Skalarfeld` | a height field is a sampled scalar displacement over regular surface geometry | retain clipmap rings for land, but do not ask them to represent multiple heights per X/Z |
| `Raycasting -> Höhenfeld / Voxel Space` | the flight-game route is an image-order 2.5D visibility solution | preserve it as a cheap/debug alternative; do not duplicate the main PBR renderer |
| `Raycasting -> Marching Cubes / Splatting / Volumengrafik` | rays, extracted meshes, and splats are alternative views of volumetric data | select representation per layer instead of forcing one renderer on every world feature |
| `Volumengrafik -> Gaussian Splatting` | modern radiance splats sit next to classical volume/point methods | evaluate them only for non-authoritative vistas, not deterministic editable terrain |
| `Voxel Space / Comanche / NovaLogic -> Flugsimulation` | the original motivation is high-speed, long-view terrain | visual tests must prioritize horizon stability, high-speed traversal, and altitude changes |
| `Voxel Space -> Z-Buffer / Polygon / Grafik-Engine` | even the historic route meets conventional visibility/geometry systems | composition with the existing depth/fog pipeline matters as much as raw sampling speed |

The strongest meaningful cross-route intersections were not raw popularity
counts: virtual texturing met sparse 3D through octrees/ray casting; point
clouds met sparse 3D through octrees, out-of-core LOD, and surface extraction;
height fields met ray methods through displacement; and the flight branch met
ray casting through Voxel Space and visibility buffers. Generic shared children
such as “Voxel,” “Algorithmus,” or “Computergrafik” were not interpreted as
architectural evidence.

The medical-imaging branch is useful for understanding scalar volumes,
classification, isosurfaces, and transfer functions, but its priorities differ:
it often permits expensive view-dependent integration to preserve measured
density. Voxel-Native needs deterministic edits, opaque terrain, bounded frame
cost, physics separation, and graceful low-end behavior. The shared vocabulary
does not make the workloads interchangeable.

## Measured current baseline

### Dense interaction representation

The current near streamer owns complete `16 x 16 x 16` chunks and now exposes
hard ceilings independent of visual horizon distance:

| Near-field item | Current hard bound |
| --- | ---: |
| Resident full chunks | 2,400 |
| Interaction radius | 16 chunks / 256 m nominal X/Z radius |
| Terrain tasks in flight | 96 |
| Mesh tasks in flight | 64 |

Before that bound was introduced, real-engine QA recorded 7,204 resident full
chunks at render distance 16 and 8,193 at render distance 23. A render-distance
50 integer disc contains 7,845 horizontal columns; at eight vertical slots it
can nominate 62,760 full chunk positions before empty-column rejection. Merely
scaling that representation to 30.72 km would require 11,581,133 horizontal
columns and up to 92,649,064 configured vertical slots. This is the baseline
failure the far representation must avoid.

### Implemented Phase-1 horizon

The current `planetary_streaming` module renders six nested square annuli with
spacings `32, 64, 128, 256, 512, 1024` metres. Their outer radii are `0.96,
1.92, 3.84, 7.68, 15.36, 30.72` km.

| Far-field item | Fixed bound / measured result |
| --- | ---: |
| Render entities | 6 maximum |
| Hard vertex budget | 35,000 |
| Exhaustive worst topology | 31,062 vertices |
| Hard index budget | 150,000 |
| Exhaustive worst topology | 121,104 indices |
| Example six-ring build | 30,358 vertices / 120,504 indices |
| Build tasks in flight | 1 |
| Pending request storage | one six-bit dirty mask; no growing queue |
| Completed mesh installs | at most one ring per frame |
| Sample cache | exactly 6 fixed windows across residency and the sole worker; 512 KiB hard cap |
| Example cold six-ring sampling | 25,350 height + 2,469 biome queries |
| Example cold six-ring CPU build p50 / p95 | 85.913 / 88.029 ms across nine optimized runs |

The example benchmark is a local CPU measurement, not a universal frame-time
promise. Runtime performs one build at a time off the native main thread. The
same benchmark originally took 168.612 ms and 22,326 biome queries; a bounded
biome lattice reduced recorded time by 30.9–33.6% and biome classifications by
88.9% while leaving positions, indices, silhouette, radius, and entity count
unchanged. Timing varies with machine load; the query-count reduction is the
structural result.

Tests rebuild the representation after simulated camera travel of 0, 1, 10,
100, and 1,000 km and pin identical per-level entity/vertex/index counts.
Negative coordinates use Euclidean integer snapping. Mesh vertices remain
render-local `f32`; absolute X/Z sampling coordinates remain `i64`, ready for a
future floating origin.

### Automatic pressure contract

Horizon extent is not a pressure knob. Initial fill always proceeds at cadence
1. Once all six rings exist, pressure first changes rebuild cadence from 1 to 2
or 4 frames and removes optional biome queries from newly rebuilt rings. Old
ring silhouettes remain visible while work is deferred. Telemetry exposes
resident and budget counts, per-ring topology, dirty mask, backlog, in-flight
state, cadence, material tier, stale results, rejected builds, sample counts,
and build latency.

This satisfies “no manual optimization” for extent and now retains one fixed
`65 x 65` toroidal source window per level. A one-cell axial shift samples 65
new heights and a diagonal shift samples 129; incompatible targets refill the
same allocation in place, so there is no transient seventh window. Mesh
attribute assembly/upload still replaces the complete changed ring, however:
the current optimization is strip-only *procedural sampling*, not partial GPU
buffer mutation. Telemetry reports current and peak cache windows/bytes under
the six-window / 512 KiB cap.

Far-surface material classification defaults to `BridgeV2`: vertices share a
categorical biome/base-family lookup inside fixed absolute 128 m cells, with no
material-slope queries. `BridgeV1` remains the exact one-metre slope-family
diagnostic and `LegacyPalette` the rollback. Across 25 optimized cold level-1
builds per mode, Legacy measured 5.270/5.859 ms p50/p95, BridgeV1
18.834/20.685 ms, and BridgeV2 5.364/5.944 ms. BridgeV2 can broaden categorical
transitions, so same-seed Natural/Astral visual A/B remains pending.

## What the primary sources actually imply

### Height fields and geometry clipmaps

Losasso and Hoppe's original geometry clipmap stores nested, viewer-centred,
power-of-two regular grids, refills them incrementally, uses morph transition
regions, and keeps tessellation complexity independent of local roughness. The
paper also states the important limits: this is a height-field representation;
needle-like features can appear late; buildings and vegetation are separate
LOD systems. The later GPU implementation stores elevation in textures,
reuses constant grid footprints, accesses windows toroidally, and generally
updates only a small L-shaped region after coherent camera motion.

That directly validates the current horizon topology and directly identifies
the next CPU-work reduction: persistent per-level sample windows plus bounded
toroidal strip updates. It does **not** validate representing caves, floating
islands, towers, trees, or authored edits as a single height value.

The Voxel Space reconstruction reaches a similar 2.5D conclusion by a very
different renderer: sample a height map and colour map along view-distance
lines, project heights into screen columns, and use a per-column visibility
buffer. Its own documentation says one height per map coordinate cannot
represent complex buildings or trees. It is admirably cheap, but adopting it
as the main far renderer would duplicate Bevy/wgpu's raster pipeline, change
PBR/fog/shadow integration, and make geometry unavailable to normal depth
passes. It remains useful as a retro/debug map view, not world truth.

### GIS layers are a data contract, not a runtime dependency

GDAL defines a dataset as co-registered raster bands with a shared size,
coordinate transform, coordinate system, metadata, block size, optional
no-data mask, and reduced-resolution overviews covering the same geographic
region. GRASS makes extent and resolution explicit, distinguishes NULL from
zero, and resamples inputs to a computational region. USGS 3DEP demonstrates a
practical division between original lidar point clouds and derived seamless
DEMs at several resolutions. These are strong models for procedural world data
even if Voxel-Native never embeds GDAL or GRASS.

Voxel-Native should therefore expose a deterministic **semantic world
pyramid**, not just `height(x,z)`:

| Band / summary | Required reducer for the next coarser level | Reason |
| --- | --- | --- |
| Surface height | filtered mean/residual **plus min and max** | mean gives stable geometry; min/max protect culling and tall silhouettes |
| Biome/material | top two material IDs + normalized coverage | majority alone erases narrow biomes and creates abrupt colour swaps |
| Occupancy/structure | logical OR + coverage/density | a tower must not disappear because its footprint is a minority |
| Emissive/crystal | maximum + coverage | preserves rare distant glints without making the whole cell emissive |
| Water | surface min/max, coverage, flow class | averaging land and water heights invents sloped water |
| Vegetation | species class, canopy height max, coverage, motion phase seed | separates silhouette from near-instance simulation |
| Edit state | max generation, content hash, dirty bit | stale far pages can be detected and rebuilt deterministically |
| Signed-distance/volume | sign-aware min-absolute summary + occupancy bounds | arithmetic averaging can erase thin surfaces or flip topology |

Normals should be derived from each level's filtered height signal instead of
averaging near-field normals. Every band needs a declared world transform,
sample convention, no-data/default behavior, seed/profile/version, reducer,
and invalidation rule. That prevents near/far disagreement from becoming an
untraceable collection of ad-hoc samplers.

### Virtual texturing and MegaTexture

Texture clipmaps virtualize an arbitrarily large mip pyramid by retaining a
finite, view-relevant cache. Modern sparse-resource APIs separate a large
virtual address range from physically committed pages, but they also require
feature discovery, page-granularity rules, explicit residency changes, and
synchronization. Vulkan's specification explicitly permits non-contiguous,
rebindable resource memory; the Khronos OpenGL sparse-texture extension exposes
page commitment for on-demand loading and application-controlled LOD.

For Voxel-Native, “MegaTexture” should mean a bounded material-page system, not
one baked world image:

1. A portable fixed-size physical atlas and page table are the baseline.
2. Pages contain material weights/parameters, not only final baked RGB, so
   day/night, wetness, damage, and biome transitions still work.
3. Coarse fallback pages are always resident; a missing fine page can reduce
   detail but cannot create a checkerboard hole.
4. Requests are coalesced by `(level, page_x, page_z, band_version)` and capped.
5. Pressure reduces anisotropy, normal/detail layers, and request cadence before
   it removes the coarse albedo/material identity or horizon extent.
6. Hardware sparse residency is an optional backend optimization only after the
   current Bevy/wgpu path proves support; correctness must not depend on it.

The current Phase-1 clipmap uses vertex colours, so virtual material pages are
not a prerequisite for its constant geometry proof. They become valuable when
the semantic band pyramid exists and the horizon must stop looking like broad
palette interpolation.

### Point clouds, surfels, and splatting

Surface Splatting and Surfels show that point samples can carry depth, colour,
normal, radius, and other attributes without explicit mesh connectivity. Potree
shows a practical hierarchy: a low-resolution root, progressively denser child
levels, frustum culling, and lower resolution at distance. OGC 3D Tiles likewise
standardizes a hierarchical delivery structure for massive 3D content while
leaving visualization policy to the client.

This is a strong **supplemental** representation for distant trees, crystal
clusters, skyline architecture, floating-island edges, and authored landmarks:
it preserves disconnected and vertical detail that a height field cannot. A
fixed point/splat budget can select the most screen-important nodes while a
coarse parent always remains available.

It is not suitable as interaction truth. Sparse points need footprint filters
to avoid holes; oversized splats cause silhouette swelling and alpha overdraw;
thin geometry changes with view and density; connectivity, interior volume,
collision, and part editing are absent. 3D Gaussian Splatting is rejected for
the mutable procedural core because the original method optimizes anisotropic
Gaussians from captured multi-view scenes. Its capture/training/static-scene
assumptions conflict with deterministic seeds, live voxel edits, and semantic
object selection. It could later serve as an optional captured skybox or
non-editable vista, never authoritative world state.

### Sparse volumes, ray casting, and Marching Cubes

GigaVoxels demonstrates ray-guided production and streaming of sparse octree
bricks: rendering requests only the resolution visible in the final image and
uses view/occlusion feedback. NVIDIA's Sparse Voxel Octree work demonstrates a
compact GPU octree and ray casting with per-voxel attributes. Amanatides and
Woo give the foundational constant-step grid traversal rule. These techniques
can preserve overhangs, caves, floating masses, volumetric vegetation, and
vertical structures that a height field necessarily loses.

They do not make full-world 3D free. Cost becomes pixels times traversal/brick
lookups and depends on empty-space hierarchy quality, opacity, view direction,
screen resolution, cache misses, and edit churn. Fixed-step volume marching is
especially wasteful; transparent texture-slice volume rendering also suffers
high fragment blending and cannot use normal opaque early-depth rejection.
This is why sparse volumes belong in a bounded mid-field or in local effects,
not under every horizon pixel on every supported machine.

Marching Cubes extracts an isosurface mesh from a sampled 3D scalar grid. It is
valuable inside already-resident sparse bricks, especially for smooth local
materials, but does not solve residency or LOD by itself. Running it across a
kilometre-scale horizon would turn camera travel and edits into large remeshing
jobs, introduce cross-brick topology/seam obligations, and allocate variable
triangle counts. It is rejected as the far-horizon representation.

Direct volume rendering remains appropriate for fog, clouds, dust, fire, and
other media where colour/opacity integration is the desired image. It should
not replace opaque surface rendering for ordinary land.

## Four candidates and disposition

| Candidate | Fixed visible cost | Full 3D silhouette | Deterministic edit path | Integration risk | Disposition |
| --- | --- | --- | --- | --- | --- |
| A. Semantic geometry/material clipmap | Yes, by ring/page caps | No | Strong for surface edits | Low to medium | **Adopt as horizon backbone** |
| B. Ray-guided sparse voxel bricks | Only with strict page/step/request caps | Yes | Strong but invalidation is complex | High | **Prototype as bounded mid-field supplement** |
| C. Hierarchical surfel/splat landmarks | Yes, by point/node cap | Surface samples only | Rebuild from semantic object summaries | Medium | **Adopt later for remote structures/vegetation** |
| D. Voxel Space screen-column ray caster | Pixel/distance bounded | No | Height edits only | High because it duplicates renderer | **Reject as main renderer; debug/retro view only** |

Virtual material paging is cross-cutting infrastructure for A–C rather than a
fifth world representation. Marching Cubes is a local meshing option inside B,
not a streaming architecture. Texture-based volume rendering is a local media
renderer, not terrain.

## Recommended layered world contract

```text
authoritative seed + edits + authored object graph
                    |
          deterministic semantic pyramid
       height/material/occupancy/water/edit/version
          /                  |                 \
 near full chunks      sparse 3D mid-field     far clipmap
 physics/edit tools    caves/vertical shells   30.72 km surface
          \                  |                 /
             surfel landmark + virtual material pages
                    |
        raster depth/fog/atmosphere composition
```

The authoritative world is not any render LOD. Near chunks, far height rings,
sparse bricks, surfels, and material pages are reproducible caches derived from
the same seed/edit/object versions. A cache miss changes fidelity, not world
meaning. Physics and shuttle behavior remain tied to the near interaction
bubble; far layers have no collider or simulation tick.

## Staged implementation with non-negotiable gates

### Stage 1 — bounded clipmap update model (implemented; visual gate open)

- Keep six entities, 30.72 km extent, one task, and current hard topology caps.
- Retain persistent per-level sample windows and use toroidal entering-strip
  source updates; refill incompatible targets in place.
- Keep source-cache ownership at exactly six windows and 512 KiB while
  retaining old resident geometry when the coalesced update backlog grows.
- Prioritize outward silhouette continuity over material/normal freshness.
- Expose update kind, shifts, new/reused samples, current/peak cache population,
  desired/resident material detail, and fixed-cell material reuse telemetry.

Gate: a measured high-speed 30 km flight must show constant entities,
vertices/indices, sample-cache bytes, request slots, and task count; frame
pressure may age fine data but may not shorten the visible horizon. Natural and
Astral visual A/B must also reject objectionable BridgeV2 transition broadening.

### Stage 2 — semantic raster pyramid and material pages

- Define the bands/reducers/version contract above in one public module.
- Make the far clipmap consume level-appropriate cached height/material data,
  with exact procedural fallback for missing pages.
- Add a fixed page atlas, fixed request table, always-resident coarse fallback,
  and explicit hit/miss/eviction telemetry.

Gate: the same world coordinates must produce compatible near and far height,
biome identity, water state, and edit generation across negative coordinates,
profile changes, saves, and repeated runs. Page pressure may remove fine normal
or detail layers first, never base material identity or extent.

### Stage 3 — bounded 3D silhouette bricks

- Summarize only regions whose occupancy cannot be represented by a height
  envelope: caves at visible mouths, floating islands, arches, towers, cliffs
  with meaningful undercuts, and large authored vehicles/structures.
- Use a fixed brick/page pool and fixed request queue. Store parent occupancy,
  min/max bounds, material summary, edit generation, and coarse fallback.
- Compare rasterized brick meshes against ray-guided brick rendering on the
  target low-end GPU before committing to a renderer.

Gate: overload must evict fine bricks to parent summaries without holes; ray
step/page/triangle limits must be explicit and visible in telemetry. No collider
or simulation is created outside the interaction bubble.

### Stage 4 — surfel landmarks and vegetation canopy

- Derive stable surfels from semantic object IDs, not from camera history.
- Use deterministic blue-noise/area sampling, per-node error, conservative
  bounds, and a fixed screen-space point budget.
- Use opaque/depth-writing coverage where possible; tightly cap transparent
  overdraw and retain parent silhouettes.

Gate: orbit tests must show no holes, explosive splat growth, identity swaps,
or temporal shimmer at LOD boundaries. Selection/edit tools must still resolve
the authoritative object, never the splat itself.

## Candid current limitations and rejection triggers

1. **No authored far objects yet.** Current rings contain procedural surface
   height and broad colour only; cities, shuttles, vegetation, crystals, water,
   caves, floating-island undersides, and user edits need derived layers.
2. **Whole-ring GPU uploads remain.** Entering-strip procedural sampling and
   fixed memory are implemented, but the changed ring still receives complete
   CPU attribute assembly and asset upload. Rapid motion can still produce stale
   discarded builds and visible lag.
3. **No material-page system.** Vertex colours cannot deliver the reference
   image's close-to-far material richness, night emissives, roads, or edit masks.
4. **No far shadows.** Rings are explicitly non-shadow-casting/receiving. This
   avoids a large shadow cost but can disconnect distant relief from lighting.
5. **Needle and vertical-feature loss.** Height filtering can delay or erase
   towers, arches, canopies, and narrow peaks unless min/max and object summaries
   feed a separate silhouette layer.
6. **No proven planetary coordinate range.** X/Z anchors are `i64`, but the
   existing terrain sampler accepts narrower coordinates and explicitly clamps
   extreme queries. Floating-origin readiness is not infinite-world proof.
7. **Double-sided, uncullable rings.** This prevents temporary underside holes,
   but costs more pixels. The six-entity cap bounds geometry, not fill rate.
8. **Astral-only default.** Natural remains gated until visual QA proves that
   fog, colour, water, vegetation, and terrain transitions are acceptable.
9. **Sparse-volume rejection trigger.** Do not adopt B globally if worst-case
   ray/page work cannot be capped without holes on the lowest supported GPU.
10. **Surfel rejection trigger.** Do not expand C beyond landmarks if temporal
    stability requires enough alpha overdraw to dominate the far-field cost.
11. **Virtual-texture rejection trigger.** Do not depend on hardware sparse
    features until every supported backend has a correct coarse fallback and
    synchronization path.

## Primary and official sources

### Terrain, height fields, and clipmaps

- Frank Losasso and Hugues Hoppe, *Geometry Clipmaps: Terrain Rendering Using
  Nested Regular Grids* (SIGGRAPH 2004 author PDF):
  https://hhoppe.com/geomclipmap.pdf
- Arul Asirvatham and Hugues Hoppe, *Terrain Rendering Using GPU-Based Geometry
  Clipmaps* (GPU Gems 2, NVIDIA Developer):
  https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-2-terrain-rendering-using-gpu-based-geometry
- Hugues Hoppe project page and implementation materials:
  https://hhoppe.com/proj/gpugcm/
- Microsoft official geometry-clipmap demonstration:
  https://www.microsoft.com/en-us/download/details.aspx?id=52336
- s-macke, Voxel Space reference implementation and documented limits:
  https://github.com/s-macke/VoxelSpace

### GIS, raster pyramids, and geospatial delivery

- GDAL raster data model and overviews:
  https://gdal.org/en/stable/user/raster_data_model.html
- GRASS GIS raster model, region/resolution, NULL, and resampling behavior:
  https://grass.osgeo.org/grass-stable/manuals/rasterintro.html
- USGS 3DEP products: lidar point clouds and multi-resolution/seamless DEMs:
  https://www.usgs.gov/3d-elevation-program/about-3dep-products-services
- USGS 3DEP lidar point-cloud data collection:
  https://data.usgs.gov/datacatalog/data/USGS%3Ab7e353d2-325f-4fc6-8d95-01254705638a
- OGC 3D Tiles 1.1 standard page and specification:
  https://www.ogc.org/standards/3DTiles/
  https://docs.ogc.org/cs/22-025r4/22-025r4.pdf

### Texture virtualization

- Christopher Tanner, Christopher Migdal, and Michael Jones, *The Clipmap: A
  Virtual Mipmap* (SIGGRAPH 1998 author/archival copy):
  https://www.graphicon.ru/oldgr/library/siggraph/98/papers/tanner/tanner.pdf
- Khronos `ARB_sparse_texture` specification:
  https://registry.khronos.org/OpenGL/extensions/ARB/ARB_sparse_texture.txt
- Vulkan sparse-resource specification and official guide:
  https://docs.vulkan.org/spec/latest/chapters/sparsemem.html
  https://docs.vulkan.org/guide/latest/sparse_resources.html

### Point clouds and splatting

- Matthias Zwicker et al., *Surface Splatting* (SIGGRAPH 2001 author PDF):
  https://cgl.ethz.ch/Downloads/Publications/Papers/2001/Zwi01a/Zwi01a.pdf
- Hanspeter Pfister et al., *Surfels: Surface Elements as Rendering Primitives*:
  https://cgl.ethz.ch/Downloads/Publications/Papers/2000/p_Pfi00.pdf
- Markus Schütz, *Potree: Rendering Large Point Clouds in Web Browsers*:
  https://www.cg.tuwien.ac.at/research/publications/2016/SCHUETZ-2016-POT/
- Bernhard Kerbl et al., *3D Gaussian Splatting for Real-Time Radiance Field
  Rendering* (INRIA project and paper):
  https://repo-sam.inria.fr/fungraph/3d-gaussian-splatting/

### Sparse voxels, ray casting, volume rendering, and isosurfaces

- Cyril Crassin et al., *GigaVoxels: Ray-Guided Streaming for Efficient and
  Detailed Voxel Rendering* (author version):
  https://www-sop.inria.fr/reves/Basilic/2009/CNLE09/CNLE09.pdf
- Samuli Laine and Tero Karras, *Efficient Sparse Voxel Octrees* (NVIDIA
  Research page and paper):
  https://research.nvidia.com/publication/2010-02_efficient-sparse-voxel-octrees
- John Amanatides and Andrew Woo, *A Fast Voxel Traversal Algorithm for Ray
  Tracing* (Eurographics archive):
  https://diglib.eg.org/items/60c72224-00f3-416d-9952-ee41e8c408da
- Marc Levoy, *Display of Surfaces from Volume Data* (author page/paper):
  https://graphics.stanford.edu/papers/volume-cga88/
- Marc Levoy, *Efficient Ray Tracing of Volume Data* (author page):
  https://graphics.stanford.edu/papers/Levoy-hpscans/raytrace-tog90/INDEX.HTM
- NVIDIA GPU Gems, *Volume Rendering Techniques*:
  https://developer.nvidia.com/gpugems/gpugems/part-vi-beyond-triangles/chapter-39-volume-rendering-techniques
- William Lorensen and Harvey Cline, *Marching Cubes: A High Resolution 3D
  Surface Construction Algorithm* (original DOI):
  https://doi.org/10.1145/37402.37422
