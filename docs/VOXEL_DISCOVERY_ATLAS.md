# Voxel-Native Discovery Atlas

Status: living study; source intake and relationship mapping remain open.

The raised whole-engine acceptance ladder is maintained in
[`ELITE_WORLD_SYSTEMS_STANDARD.md`](ELITE_WORLD_SYSTEMS_STANDARD.md). Research
prototypes become project features only after they satisfy the applicable
safety, boundedness, coherence, continuity, visual, interaction and release
levels defined there.

This document is the shared research map for the engine. It is intentionally
not a feature wish list. Every idea must be assigned to a world layer, bounded,
tested, and either adopted, prototyped, deferred, or rejected.

## Evidence policy

Wikipedia is useful here as a concept graph: it exposes vocabulary, historic
systems, neighboring sciences, and references. It is not implementation
authority. A production decision requires all of the following:

1. a primary paper, official technical publication, patent, or first-party
   developer account;
2. a measured baseline in the current Voxel-Native code path;
3. two to four credible candidates, including the current approach;
4. an explicit memory, work, latency, and failure budget;
5. deterministic unit/property tests before a graphical run;
6. real-engine visual and mechanical evidence at several distances and window
   sizes;
7. a whole-world check: the result may not work only at one hero location.

No research result authorizes deleting saves, personal files, or unrelated
workspace changes.

## The central model: one world, several representations

Trying to store every visible detail as an equally dense voxel is the wrong
problem formulation. Voxel-Native should preserve one authoritative world
identity while selecting a bounded representation for each scale.

| Layer | Intended authority | Representation | What it must never do |
|---|---|---|---|
| Interaction | editing, collisions, destruction, exact materials | current full voxel chunks and semantic object links | silently approximate a user edit |
| Near | local playable terrain and authored structures | hard-capped full chunks and meshes | grow with distance traveled |
| Mid | recognizable silhouettes and persistent sparse edits | fixed-capacity multiresolution bricks | own or evict the authoritative edit log |
| Far | horizon, macro-biomes, rivers, routes, mountains | constant-count geometry clipmap, optional bounded splats | pretend to support caves or exact collisions |
| Celestial | planets, moons, orbital bodies, sky phenomena | analytic quadrics, impostors, atmosphere shells | allocate planet-scale dense chunks |
| Micro | bevels, shells, cuts, high-curvature local detail | adaptive surface-only subcells with fixed depth | become a second global dense voxel grid |

The same logical feature can cross layers. A tree, for example, is an editable
semantic object near the player, a reduced silhouette brick farther away, and
possibly a wind-animated splat at the horizon. Its stable object identity and
species seed remain the same.

## Studied concept clusters

### 1. Kugel, Ellipsoid, quadrics, shells, and implicit solids

The sphere and ellipsoid links lead to a useful analytic family rather than
merely two new primitives.

For a sphere centered at `c` with radius `r`:

```text
f(p) = dot(p - c, p - c) - r^2
```

For an oriented ellipsoid with symmetric positive-definite matrix `A`:

```text
q(p) = (p - c)^T A (p - c) - 1
```

These expressions provide exact inside/outside queries in continuous space.
Voxel generation must still classify an entire cell conservatively. Sampling
only the cell center can miss thin shells and grazing intersections. The safe
contract is: an `Outside` classification must prove the complete cell is
outside; uncertainty becomes a surface cell and may be subdivided locally.

Candidate uses:

- planets, moons, domes, observatories, pressure vessels, caves and tunnels;
- bounded shell bands between two implicit surfaces;
- constructive caps/cuts without prebuilding a dense volume;
- curvature-triggered microvoxels only where a coarse cell cannot resolve the
  surface;
- analytic broad-phase collision and ray intersection before voxel detail.

Decision: **prototype now**, but only as a pure bounded module. Live world
integration waits for false-outside, symmetry, extreme-coordinate, shell,
rotation, volume-convergence, and maximum-subcell tests.

### 2. Conservative and topological voxelization

The crucial question is not only whether a triangle or implicit surface looks
filled. It is whether the discrete solid has the promised connectivity and
whether gaps can leak through it. Schwarz and Seidel distinguish conservative
surface voxelization from thin, separating variants; Laine formalizes
topological guarantees. This directly affects rooms, water containment,
selection, navigation, destruction, and collision.

Adopted rule for future voxelizers:

- declare the intended digital connectivity pair explicitly (for example,
  solid 6-connectivity versus background 26-connectivity);
- never infer topology from one center sample;
- property-test rotations, diagonal walls, one-cell necks, cavities, and chunk
  boundaries;
- treat topology and appearance as separate acceptance criteria.

Decision: **adopt as an invariant**, not as a single renderer.

### 3. Octrees, sparse voxel hierarchies, DAGs, NanoVDB, and ADFs

These links all exploit sparsity, but they solve different problems:

- an octree skips homogeneous regions by recursively subdividing space;
- a sparse voxel octree is useful for traversal and multiresolution occupancy;
- a DAG additionally deduplicates identical subtrees, which is powerful for
  static repetition but expensive to update;
- NanoVDB is a portable read-optimized sparse volume layout;
- an adaptively sampled distance field concentrates samples where local detail
  or curvature requires them.

Voxel-Native already has a pure fixed-budget 8x8x8 summary-brick hierarchy
prototype. Its authoritative sparse edit store is separate, so reconstructible
resident summaries may be evicted without losing user work.

Decision:

- **adopt** fixed-capacity summary bricks for the mid layer after live
  integration tests;
- **prototype** adaptive distance/microvoxel data only at local surfaces;
- **defer** DAG deduplication until edit invalidation and rebuild cost are
  measured;
- **reject** an unbounded recursive tree as the only world database.

### 4. Marching Cubes, Dual Contouring, and block-preserving meshing

Marching Cubes extracts an isosurface from scalar samples and is excellent for
smooth density data, but its lookup cases and interpolated vertices do not by
themselves preserve authored block edges, semantic faces, or all topological
intent. Dual Contouring is attractive where sharp features and adaptive cells
matter, but it introduces Hermite-data and crack-management requirements.

Proposed dual visual modes:

- `Block`: current voxel-native face language, exact authored cells;
- `Surface`: optional smooth terrain/implicit material layer, feature-aware and
  never used to reinterpret semantic buildings.

Decision: **prototype later** on an isolated scalar-field terrain sample.
Neither algorithm replaces the authoritative voxel/object model.

### 5. Ray casting, ray tracing, volume rendering, and splatting

These are complementary orders of work rather than one quality ladder:

- ray casting/tracing is image-order traversal from pixels into a scene;
- texture-based volume rendering samples slices or a 3D texture;
- splatting projects volume or point samples into image space;
- surface splatting adds elliptical weighted filtering for irregular point
  samples without mesh connectivity.

Useful bounded roles:

- occupancy rays for editing, visibility, and selective lighting;
- a low-resolution global occupancy summary for secondary rays;
- short, bounded volume marches for fog, nebulae, crystal interiors and engine
  exhaust;
- far vegetation/crystal cohorts represented by screen-sized filtered splats;
- analytic sphere/ellipsoid intersection before local voxel traversal.

Decision:

- **adopt** hybrid traversal and analytic broad phases;
- **prototype** splats for far vegetation, never for collisions;
- **prototype** short local volume passes with strict step/coverage budgets;
- **reject** full-resolution whole-world path tracing as a baseline requirement
  for the target integrated-GPU hardware class.

### 6. Voxel Space, heightfields, clipmaps, GIS, and watersheds

NovaLogic's Voxel Space family demonstrates how a height/color field can render
large outdoor vistas with very small state. Its primary limitation is equally
important: one elevation per horizontal coordinate cannot represent caves,
overhangs, stacked buildings, or arbitrary six-degree-of-freedom solids.

Therefore the transferable idea is a far-field representation, not a world
authority. The current planetary clipmap uses six constant-count rings and a
64-bit world anchor with local floating-point meshes. It retains the horizon
under pressure by reducing update cadence and expensive material queries rather
than shrinking visible distance.

GRASS GIS contributes a second idea: terrain is not one noise function. It is a
stack of derived world layers such as drainage direction, flow accumulation,
basins, stream networks, slope, wetness, and obstacles. Multiple-flow-direction
watershed analysis distributes flow among lower neighbors rather than forcing
every cell into one artificial direction.

Decision:

- **adopt** constant-budget geometry clipmaps for macro-relief;
- **prototype** deterministic offline/tile-cached watershed fields with halos;
- **adopt** layer-based biome grammar: geology -> relief -> water -> soil ->
  vegetation -> settlement/route constraints;
- **reject** a pure heightfield for the near interactive world.

### 7. Pixels, MegaTexture, clipmaps, and virtual material pages

The Clipmap paper demonstrates that a huge logical texture can be represented
by a finite moving cache centered around a focus. This principle transfers to
material detail, decals, erosion masks, and semantic overlay fields.

Decision: **prototype after geometry streaming**. The first version should use
a fixed physical page pool, deterministic fallback mip, bounded requests per
frame, and visible missing-page telemetry. It must not embed authoritative
voxel edits only in transient texture pages.

### 8. Tomography, segmentation, point clouds, and image-based meshing

Medical imaging links are valuable as algorithmic neighbors, not as a reason to
simulate medical devices:

- tomography reconstructs an interior field from indirect measurements;
- segmentation labels coherent regions/objects in image or volume data;
- multiplanar views expose one volume through several orthogonal slices;
- point-cloud reconstruction converts incomplete samples into surfaces or
  occupancy evidence;
- diffusion-tensor methods represent direction-dependent structure.

Transferable Voxel-Native uses:

- import scans/point clouds as uncertain evidence rather than immediately
  trusted solid voxels;
- semantic object segmentation with authored `object_id` as authority and
  capped connectivity only as a legacy fallback;
- slice/debug views through terrain, buildings, edit histories and streaming
  tiers;
- tensor/directional fields for wood grain, rock strata, leaf orientation,
  wind response and anisotropic material behavior;
- multi-view visual QA that asks whether an apparent surface is truly solid.

Decision: **adopt the debugging and semantic principles**; **defer** scan
reconstruction until an actual import workflow exists. Medical correctness is
outside the product scope.

### 9. Lucas-Kanade and temporal visual QA

Sparse optical flow estimates image motion from local spatial and temporal
gradients under small-motion/brightness assumptions. It is not an engine
physics model. It can, however, make automated visual review much better.

Candidate QA uses:

- detect unexpected frozen vegetation while the wind field is active;
- distinguish camera motion from mesh popping or teleporting chunks;
- identify unstable outlines, temporal shimmer, and repeated UI shifts;
- compare expected shuttle motion against screen-space evidence.

Decision: **prototype in offline QA**, with explicit uncertainty in low-texture,
transparent, emissive, and large-motion regions. Never grant it simulation
authority.

### 10. Leaves, maples, shells, water content, diffusivity, and composites

These links point toward living and layered material grammars rather than a
larger library of static voxel sculptures.

Vegetation should have:

- a species/development graph (L-system or equivalent graph grammar);
- trunk, branch, twig, petiole, and leaf hierarchy;
- per-part stiffness, length, crown exposure and flutter phase;
- phyllotaxis/branching constraints and controlled variation;
- moisture, exposure, season and damage state;
- a reduced far representation derived from the same seed.

Wind should be deliberately decoupled:

- a visual wind field drives trunk sway, branch bending and leaf flutter on
  vegetation vertices/instances;
- local wake/gust sources may affect vegetation presentation;
- the shuttle flight model receives only aerodynamic/weather forces from its
  own explicitly configured physical field;
- changing a bush's visual amplitude can never change shuttle acceleration.

Shell growth offers a reusable morphospace: a generating aperture expands,
rotates and translates along an axis. This can generate shells, alien flora,
conduits, towers, tunnels, armor and ornament without storing a unique dense
model for every variation.

Composite materials and diffusion suggest slow ecology/material fields, but a
stable bounded discretization and gameplay purpose are required before adding
them.

Decision:

- **adopt** hierarchical GPU vegetation motion and physical-field isolation;
- **prototype** L-system/species graphs and shell-growth morphospace;
- **defer** moisture/diffusion simulation until it drives visible ecology or
  material behavior with a fixed update budget.

### 11. First-party voxel game studies

Games in the recorded discovery graph are comparative case studies, not blueprints.

- Teardown's first-party material describes small movable voxel volumes,
  palette-sized material storage, voxel ray traversal and technology-shaped
  gameplay. This is relevant to destructible local objects, not a proof that
  one dense representation can cover a planet.
- Voxel Space/Comanche validates an extremely cheap far heightfield but also
  reveals the cave/overhang and camera constraints of 2.5D terrain.
- Space Engineers, Enshrouded, Vintage Story, Hytale, Boundless, Luanti,
  7 Days to Die, PixARK, Castle Story, Cube World, and related titles should be
  evaluated for interaction contracts, streaming behavior, edit persistence,
  building feel, and visual hierarchy only when first-party technical evidence
  or direct repeatable observation exists.
- A screenshot or marketing statement is evidence of a target experience, not
  evidence of the underlying algorithm.

Decision: maintain a comparison matrix with `observed`, `first-party stated`,
and `inferred` kept as separate columns.

### 12. 2024-2025 sparse-volume and filtering refresh

The newer literature strengthens the layered architecture, but it does not
remove the edit-authority problem.

**Aokana (PACMCGIT 2025)** divides a large prebuilt scene into multiple shallow
SVDAG chunks, streams only a subset into VRAM, and uses GPU chunk selection,
screen-tile selection, previous/current Hi-Z correction passes, ray marching
and a 64-bit visibility buffer. The paper reports two-to-four-times faster
rendering than HashDAG above 32K scene resolution and about five percent of the
complete scene resident in VRAM during navigation. This is directly relevant
to a future opaque Mid/Far render path. It is not a replacement for the current
authoritative edit layer: the published implementation does not support runtime
voxel modification, transparent voxels, collision or navigation integration.
Decision: **prototype only after the CPU brick continuum is live**, using
multiple shallow fixed-pool regions and retained parent coverage; reject it as
Near/edit authority.

**Sparse Voxels Rasterization (CVPR 2025)** explicitly allocates adaptive
sparse voxel LOD and uses ray-direction-dependent Morton order so rasterized
voxels resolve in depth order rather than exhibiting common splat popping. Its
reported problem is high-fidelity radiance-field novel-view synthesis, not a
mutable simulation. Decision: **study the ordering and visibility-buffer idea
for a render-only voxel proxy**; do not import learned appearance or treat its
image metric as gameplay correctness.

**NeuralVDB (TOG 2024)** replaces lower sparse-tree nodes with hierarchical
neural topology and value encoders and reports roughly 10x to above 100x
compression over already compressed VDB inputs with controllable error. That
is promising for offline, read-mostly smoke, cloud or archival volumes. It is a
poor current fit for deterministic per-voxel editing: decompression is
approximate, compression has training cost, and exact authority would still
need a separate representation. Decision: **defer to offline visual-volume
experiments**, never save or collision authority.

**Filtering After Shading (I3D 2024)** shows that stochastic evaluation of a
texture filter after BSDF shading can improve accuracy and work with sparse or
compressed 3D data. It exchanges deterministic filter cost for sampling noise
that needs moderate sample counts or spatiotemporal denoising. Decision:
**candidate for a measured material/volume experiment after temporal QA exists**;
reject as a default shader change until it beats current filtering without
foliage shimmer or ghosting on the reference integrated GPU.

This refresh therefore changes priorities, not authority: connect the proven
CPU continuum first; then benchmark a fixed-pool GPU proxy patterned after
shallow regions and visibility-buffer feedback. Neural or stochastic methods
remain optional visual caches whose failure reveals a conservative parent.

## Candidate portfolio

| Candidate | Whole-world role | Expected benefit | Hard risk | Current gate |
|---|---|---|---|---|
| Conservative implicit quadrics | celestial + local architecture | exact huge shapes with sparse surface work | thin-shell false negatives | pure tests and fixed subcell cap |
| Fixed near bubble | near authority | edit/collision correctness with bounded RAM/jobs | holes if admission is wrong | resident+pending invariant and teleport tests |
| Six-ring geometry clipmap | far | 15.36 km L-infinity axis half-extent (30.72 km full width) with constant entities | visible seams/popping | Natural/Astral km visual route |
| Summary-brick cache | mid | silhouette/edit continuity without full chunks | stale epochs or lost edits | separate edit log and replay tests |
| EWA-style far splats | far vegetation/crystals | richer silhouettes at small geometry cost | transparency/order/shimmer | capped prototype and motion QA |
| Virtual material pages | all rendered layers | nonrepeating detail with fixed pool | thrash/missing pages | deterministic fallback and request cap |
| Hierarchical vegetation wind | near/mid rendering | believable bush/tree motion | synchrony, clipping, physics leakage | visual-only field isolation test |
| Watershed/soil/ecology layers | world grammar | coherent rivers/biomes/routes | preprocessing cost and tile seams | deterministic tiled bake with halos |
| Optical-flow QA | test system | finds popping/frozen/shimmering motion | false confidence under violations | uncertainty masks + known-motion tests |
| Dual surface mode | local smooth terrain | optional organic surfaces | cracks and semantic erosion | isolated scalar-field prototype |

## Explicit impossibilities and non-goals

The following cannot all be true at once on ordinary user hardware:

- every kilometer has near-player voxel resolution;
- every voxel is resident, simulated, ray traced and editable every frame;
- memory and work remain negligible;
- no quality is reduced with distance.

The achievable top-tier goal is stronger and more useful: every logical place
is addressable and persistent; exact edits survive; visible representation is
selected by scale and importance; budgets remain constant; transitions are
stable; and the player can approach any distant feature until it resolves into
interactive detail.

## Reproducible study protocol

Every research contribution should record:

1. source URLs and an evidence classification;
2. the exact engine baseline it measured;
3. candidate algorithms and why alternatives were rejected;
4. state ownership and deletion/eviction semantics;
5. fixed memory/work/latency limits;
6. deterministic tests, including extreme coordinates and stale async work;
7. real-engine screenshots/telemetry only after the nonvisual proof is green;
8. an integration note explaining which world layers change.

Mission Control should flag a feed whose capability schema or shared power
profile differs from the current fleet profile. A later knowledge-manifest
version will similarly expose whether an agent has consumed the current study
atlas.

## Primary and official sources studied so far

- Schwarz and Seidel, *Fast Parallel Surface and Solid Voxelization on GPUs*:
  https://michael-schwarz.com/research/publ/2010/vox/
- Laine, *A Topological Approach to Voxelization*:
  https://research.nvidia.com/sites/default/files/pubs/2013-06_A-Topological-Approach/laine2013egsr_paper.pdf
- Laine and Karras, *Efficient Sparse Voxel Octrees*:
  https://research.nvidia.com/publication/2010-02_efficient-sparse-voxel-octrees
- Museth, *NanoVDB: A GPU-Friendly and Portable VDB Data Structure*:
  https://research.nvidia.com/labs/prl/nanovdb/
- Frisken et al., *Adaptively Sampled Distance Fields*:
  https://www.ronaldperry.org/sig2000_ADFs_Paper.pdf
- Ju et al., *Dual Contouring of Hermite Data* (author research index):
  https://people.engr.tamu.edu/schaefer/research/index.html
- Lorensen and Cline, *Marching Cubes*:
  https://dl.acm.org/doi/10.1145/37401.37422
- Tanner, Migdal and Jones, *The Clipmap: A Virtual Mipmap*:
  https://doi.org/10.1145/280814.280855
- Zwicker et al., *Surface Splatting*:
  https://www.cs.umd.edu/~zwicker/publications/SurfaceSplatting-SIG01.pdf
- GRASS GIS `r.watershed` official manual:
  https://grass.osgeo.org/grass84/manuals/r.watershed.html
- Prusinkiewicz and Lindenmayer, *The Algorithmic Beauty of Plants*:
  https://algorithmicbotany.org/papers/
- Zioma, *GPU-Generated Procedural Wind Animations for Trees*:
  https://developer.nvidia.com/gpugems/gpugems3/part-i-geometry/chapter-6-gpu-generated-procedural-wind-animations-trees
- Sousa, *Vegetation Procedural Animation and Shading in Crysis*:
  https://developer.nvidia.com/gpugems/gpugems3/part-iii-rendering/chapter-16-vegetation-procedural-animation-and-shading-crysis
- Lucas and Kanade, *An Iterative Image Registration Technique with an
  Application to Stereo Vision*:
  https://cseweb.ucsd.edu/classes/sp02/cse252/lucaskanade81.pdf
- Perona and Malik, *Scale-Space and Edge Detection Using Anisotropic
  Diffusion*: https://doi.org/10.1109/34.56205
- Raup, *The Geometry of Coiling in Gastropods*:
  https://pmc.ncbi.nlm.nih.gov/articles/PMC221494/
- NovaLogic Voxel Space terrain patent US6020893A:
  https://patents.google.com/patent/US6020893A/en
- Tuxedo Labs technical archive by Dennis Gustafsson:
  https://blog.voxagon.se/
- Graphics Programming Conference 2025, *Raytracing Voxels in Teardown and
  Beyond* (first-party talk/slides):
  https://graphicsprogrammingconference.com/archive/2025/
- Fang, Wang, and Wang, *Aokana: A GPU-Driven Voxel Rendering Framework for
  Open World Games*, PACMCGIT 2025: https://arxiv.org/abs/2505.02017
- Sun et al., *Sparse Voxels Rasterization*, CVPR 2025:
  https://research.nvidia.com/labs/twn/publication/cvpr_2025_svraster/
- Kim, Lee, and Museth, *NeuralVDB*, ACM TOG 2024:
  https://research.nvidia.com/labs/prl/publication/neuralvdb/
- Pharr et al., *Filtering After Shading with Stochastic Texture Filtering*,
  I3D 2024:
  https://research.nvidia.com/labs/rtr/publication/pharr2024stochtex/

## Discovery-link intake map

The recorded Wikipedia discovery graph has been inventoried under these
routes. Incoming sublinks should be appended to the matching route rather than
creating disconnected feature ideas.

- **Foundations:** Voxel, voxel grid, pixel, computer graphics, image synthesis,
  graphics engines/libraries, geometric modeling, numerical simulation,
  transparency, compositing, stereodisplay and volume display.
- **Spatial data:** octree, point cloud, heightfield, GRASS GIS, image-based
  meshing, segmentation, digital image analysis, computer vision.
- **Rendering:** ray tracing, ray casting, Marching Cubes, Voxel Space, volume
  graphics, splatting, texture-based volume rendering, MegaTexture and line
  rasterization.
- **Reconstruction/science:** tomography, CT, MRI, OCT, filtered
  back-projection, multiplanar reformats, diffusion tensors, spectroscopy,
  microscopy, angiography, morphometry and reconstructed radiographs.
- **Natural/material grammar:** leaves, maples, shells, particle board, water
  content, diffusivity and isotropy.
- **Temporal analysis:** Lucas-Kanade optical flow.
- **Historic engines/games:** Build Engine, Shadow Warrior, NovaLogic,
  Comanche, Delta Force, Outcast, Blade Runner, Second Reality and related
  software-rendering history.
- **Modern voxel experiences:** Teardown, Enshrouded, Space Engineers 1/2,
  Vintage Story, Hytale, Luanti, Boundless, PixARK, 7 Days to Die, Castle
  Story, Cube World, The Sandbox, 3D Dot Game Heroes, Urbek City Builder,
  Cloudpunk and Donkey Kong Bananza.
- **Low-authority discovery only:** discussion pages, user drafts, portals,
  deletion/quality-assurance archives and uncited catalog pages. These may
  reveal vocabulary but cannot support an engine decision.
