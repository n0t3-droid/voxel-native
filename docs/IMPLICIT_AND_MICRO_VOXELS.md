# Implicit Shapes and Bounded Microvoxels

Status: the pure prototype is implemented in `src/implicit_voxels.rs` and is
compiled by the application, but no live ECS or renderer system consumes it
yet. It has no Bevy, renderer, physics, save, editor, or world-streaming
dependency. That makes the first implementation measurable, reversible, and
safe to integrate behind a feature gate later.

## Outcome

The prototype adds five things that the current block grid cannot express
reliably with a single center sample:

1. conservative whole-cell classification for solid spheres;
2. conservative whole-cell classification for axis-aligned and oriented
   ellipsoids;
3. hollow shells and intersections with up to six clipping half-spaces;
4. deterministic Morton-ordered microvoxel subdivision of surface cells only;
5. fixed production depth, node, stack, and payload ceilings with fail-closed
   behavior at floating-point or allocation-pressure boundaries.

The core safety rule is deliberately asymmetric:

> `Outside` must be proven. Any unresolved numerical, geometric, budget, or
> precision case remains `Surface`; it is never silently converted to empty
> space.

That rule prevents a thin shell, grazing sphere, rotated ellipsoid, or clipped
cap from disappearing merely because no cell center happens to land inside it.

### Elite-standard position

Against `docs/ELITE_WORLD_SYSTEMS_STANDARD.md`, the pure module currently has
evidence for Level 0, Level 1, Level 2, and the local/adversarial parts of
Levels 4 and 8:

- **Level 0:** new cache/prototype files only; no saves, persisted formats,
  deletion, reset, move, or GUI run;
- **Level 1:** signed negative grid coordinates, checked extremes, subnormal
  radii, finite rotation/clip validation, and fail-closed `Surface` fallback;
- **Level 2:** compile-time byte, node, output, stack, and classification caps,
  plus a 30-run optimized benchmark distribution;
- **Level 4, local contract only:** complete child groups and parent retention
  prevent a local refinement hole; live Near/Mid/Far continuity is not yet
  claimed because no live scheduler, mesher, or renderer consumes the module;
- **Level 8, pure geometry scope:** adversarial known-inside properties,
  pressure fallback, order independence, extreme coordinates, and precision
  exhaustion are covered deterministically;
- **Levels 3 and 5:** not applicable to this isolated data layer; causal world
  generation and visual/temporal fidelity require live integration;
- **Level 9, scoped only:** the focused tests, warning-denied standalone build,
  registered native application check, and registered Wasm application check
  pass. `main.rs` compile-registers the module, but no runtime system consumes
  it. The integrated full-suite result belongs to the Root verification pass
  after all concurrent work lands; real-engine routes remain a later release
  gate.

This is an evidence boundary, not a claim that the feature is already shipped.

## Evidence policy and supplied graph audit

The German Wikipedia pages for
[Kugel](https://de.wikipedia.org/wiki/Kugel),
[Ellipsoid](https://de.wikipedia.org/wiki/Ellipsoid), and
[Voxel](https://de.wikipedia.org/wiki/Voxel) were used as vocabulary and
discovery entry points, not as implementation authority. The wider supplied
graph was parsed in full from:

```text
C:\Users\ylber\.codex\attachments\cf64a876-6452-4b6b-bd70-887f8f891608\pasted-text.txt
```

Its observed data shape is:

| Measurement | Observed value |
| --- | ---: |
| parent pages | 118 |
| parent-child rows | 8,129 |
| unique child URLs | 5,527 |
| exact duplicate parent-child pairs | 4 |
| parents linking to `Voxel` | 117 |

The last row is a warning, not a ranking result. Link degree is dominated by
article size, templates, navigation, citations, and the way the graph was
collected. A frequently linked page is not automatically an important engine
algorithm. Parent-child direction also does not prove causality or technical
dependence.

Relevant bridges were selected by mathematical or engine meaning, then checked
against primary papers or official standards. The graph exposed useful routes
from voxelization to `Octree`, `Marching Cubes`, `Quadrik`, `Kugel`,
`Rotationsellipsoid`, `Delaunay-Triangulation`, anisotropy, tomography,
segmentation, leaf venation, plant motion, shells, and 3D materials.

The following graph regions were explicitly rejected as engineering evidence:

- game catalog, store, rating, publisher, and platform links;
- user drafts, discussion pages, deletion archives, portal lists, and quality
  assurance archives;
- medical conditions and acquisition modalities that do not transfer a
  spatial representation principle;
- unrelated biographical, language, identifier, and web-archive links;
- visual examples with no first-party algorithm or reproducible measurement.

The graph is still valuable: it revealed neighboring fields and their search
terms. It does not override the current engine baseline, a hard budget, a proof,
or a measured test.

## Baseline and failure reproduction

The tempting baseline is center sampling:

```text
occupied(cell) = q(center(cell)) <= 1
```

For a shell it becomes:

```text
inner^2 <= q(center(cell)) <= outer^2
```

This is fast, but it has no coverage guarantee. A shell can cross a cell while
the center lies in its cavity. A small sphere, oblique leaf, narrow vein,
grazing ellipsoid, or clipped cap can similarly pass between all center
samples. Sampling the eight corners does not solve the general problem either:
a closed surface can enter and leave through a face while all corners have the
same sign.

The focused regression creates a spherical shell with outer radius `1.0` and
inner radius `0.99`. The root cell center is in the empty cavity, yet the
whole-cell classifier returns `Surface` and the adaptive pass reaches the
configured minimum surface depth. This is the concrete correctness gap the new
prototype closes.

An optimized standalone benchmark on the current Windows host measured center
sampling at roughly 7 ns per query. It is retained only as a performance
baseline; it is rejected as an occupancy authority because the missing-shell
counterexample is deterministic.

## Candidate comparison

| Candidate | Coverage / topology | Cost and memory | Decision |
| --- | --- | --- | --- |
| center or corner samples | no whole-cell guarantee; thin features can vanish | cheapest; fixed | rejected as authority, benchmark only |
| analytic quadratic bounds over an AABB | exact range for sphere and axis-aligned ellipsoid; conservative lower bound for an oriented ellipsoid | constant work, no allocation | chosen for the bounded prototype |
| generic interval or affine arithmetic over an arbitrary implicit program | conservative when every operation has outward rounding; supports richer CSG | wider bounds, more operations, difficult SIMD/GPU contract | defer until real non-quadric shapes justify it |
| signed-distance Lipschitz bounds | very useful for sphere tracing and adaptive distance fields | requires a genuine distance/Lipschitz contract; an ellipsoid quadratic is not an SDF | future companion field, not claimed here |
| dense uniform subvoxel grid | simple and predictable at one local resolution | memory grows cubically everywhere | rejected |
| unbounded octree or DAG as the shape authority | sparse in uniform space; potential sharing | entropy-dependent memory, update and crack complexity | rejected for this local prototype |
| fixed surface-only octants with parent fallback | conservative, deterministic, bounded | quantized and may stop coarse under pressure | chosen |

The selected implementation is reversible. Nothing in a save file depends on
it, its public production budget has no user-sized cache knob, and current
world generation remains unchanged until explicit integration work is
authorized and visually tested.

## Mathematical contract

### Cell and coordinate domain

Every query classifies a closed axis-aligned box

```text
B = [l_x, u_x] x [l_y, u_y] x [l_z, u_z].
```

`Aabb3d::from_grid` accepts signed integer origins and positive integer edge
lengths. It uses checked `i64` addition and permits only endpoints through
`2^53` in magnitude, the range in which every integer is exactly representable
by binary64. Negative cells therefore do not alias positive cells or use
truncating division conventions. An overflowing endpoint is an error.

A subdivision also fails if a binary64 midpoint is no longer strictly between
distinct endpoints. The caller receives a coarse `Surface` parent instead of a
degenerate child set.

### Sphere and axis-aligned ellipsoid

For center `c` and positive semi-axes `r = (r_x, r_y, r_z)`:

```text
q(p) = sum_i ((p_i - c_i) / r_i)^2.
```

A sphere is the special case `r_x = r_y = r_z`. For each cell axis, define:

```text
d_i = distance(c_i, [l_i, u_i])
D_i = max(abs(l_i - c_i), abs(u_i - c_i)).
```

The exact real-arithmetic extrema are separable:

```text
q_min = sum_i (d_i / r_i)^2
q_max = sum_i (D_i / r_i)^2.
```

For a solid, the occupied set is `q <= 1`. For a similar ellipsoidal shell
with inner ratio `rho`, it is `rho^2 <= q <= 1`. A spherical shell accepts an
absolute inner radius and internally derives `rho`.

The classifier is:

```text
Outside  if q_min > 1                         (beyond outer surface)
Outside  if rho > 0 and q_max < rho^2         (entirely in cavity)
Inside   if q_max < 1 and (rho = 0 or q_min > rho^2)
Surface  otherwise.
```

Strict comparisons plus a scale-aware numerical guard turn near-boundary
roundoff into `Surface`. The guard accounts for both quadratic evaluation and
coordinate cancellation relative to the smallest radius.

Membership is evaluated in normalized coordinates. The implementation does
not square the physical radius, so valid solids beyond `sqrt(f64::MAX)` and
subnormal solid radii can still be queried without radius-square overflow.
For a shell, the derived or supplied positive inner ratio must also have a
representable positive square. If `rho > 0` but `rho^2` underflows to zero, the
constructor returns `UnrepresentableInnerSurface` instead of collapsing the
cavity into a solid centre. This is an explicit fail-closed precision boundary,
not a promise that every mathematically positive shell thickness is
representable in binary64.

Axis-aligned boxes are valid only when every maximum is strictly greater than
its matching minimum. Their volume is checked and returns an error if the
finite extents multiply beyond `f64`; callers cannot obtain a non-finite public
volume from finite coordinates.

### Oriented ellipsoid

Let the columns of right-handed orthonormal rotation `R` be principal axes
`a_j`. The implicit quadratic is:

```text
q(p) = sum_j ((a_j dot (p - c)) / r_j)^2.
```

For cell center `m` and half-extents `h`, projection onto principal axis `a_j`
is bounded by:

```text
t_j = a_j dot (m - c)
s_j = abs(a_j) dot h
I_j = [t_j - s_j, t_j + s_j].
```

The minimum absolute value in `I_j` is

```text
e_j = max(abs(t_j) - s_j, 0).
```

The implementation uses

```text
L = sum_j (e_j / r_j)^2
```

as a lower bound. It relaxes the correlation between the three projected
intervals, so `L` can be lower than the true minimum but cannot be higher in
exact arithmetic. Consequently it may conservatively label extra cells as
`Surface`, but it cannot use this relaxation to falsely reject an intersecting
cell as `Outside`.

The quadratic is convex, so its maximum over a box occurs at a box vertex. The
implementation evaluates all eight vertices for `q_max`. This exact maximum
in real arithmetic also makes cavity rejection and complete-inside proofs
safe. Projection and quadratic guards widen uncertain floating-point cases.

This is intentionally not advertised as the exact minimum-distance algorithm
for an oriented ellipsoid. Exact box-constrained quadratic minimization could
tighten surface counts, but its branches and solve cost were not justified by
the present measurements.

### Shells, cuts, and caps

`SphereVolume` supports an absolute inner radius. Both ellipsoid types support
a similar concentric inner ellipsoid through `inner_ratio`. This is a bounded
shell model, not a generic Boolean expression tree.

Up to six normalized clip planes retain half-spaces:

```text
n dot p <= d.
```

For a box, plane projection is bounded exactly in real arithmetic by:

```text
n dot m +/- abs(n) dot h.
```

A box wholly outside any retained half-space is `Outside`. A box straddling a
plane is `Surface`. Intersecting a solid or shell with these planes produces
the classification required for hemispheres, sliced domes, windows, portal
cuts, and flat caps. A future mesher still has to emit the cap face; this module
only proves spatial classification.

Plane normalization checks for non-finite overflow. An overflowing point-plane
dot product fails membership rather than comparing `infinity <= infinity`.

## Bounded adaptive microvoxels

`AdaptiveMicroVoxelizer` performs deterministic depth-first subdivision. It
splits a node only when all of these are true:

```text
classification == Surface
depth < 8
and (
    depth < 2
    or cell_diagonal > detail_scale * 0.25
)
```

The shape detail scale includes both curvature and shell thickness:

- solid sphere: outer radius;
- spherical shell: `min(outer, inner, outer - inner)`;
- solid ellipsoid: conservative smallest curvature-radius scale
  `r_min^2 / r_max`;
- ellipsoidal shell: the minimum of outer curvature scale, inner scaled
  curvature, and minimum-axis shell thickness.

The curvature rule is a scheduling heuristic, not a geometric proof. Safety
comes from conservative parent classification and parent retention when the
heuristic or budget stops refinement.

Every split is all-or-none: eight children are classified before any is
committed. If the next complete sibling group would exceed the node limit, the
unsplit parent remains a leaf and `budget_limited` is set. If midpoint
precision is exhausted, the parent remains and `precision_limited` is set.
Neither condition makes a hole.

Children use a Morton prefix. They are pushed in reverse octant order so DFS
output is spatially Morton ordered. `MortonPath::spatial_key` left-aligns
mixed-depth prefixes at the fixed hard depth; comparing the raw `(code, depth)`
pair is not a valid mixed-depth spatial ordering.

### Hard production limits

| Limit | Production value | Consequence |
| --- | ---: | --- |
| maximum depth | 8 | smallest edge is root edge / 256 |
| reserved node/classification cap | 4,096 | capacity ceiling; complete octant groups only |
| maximum reachable/visited nodes | 4,089 | `1 + 8 * 511`; also the maximum shape classifications |
| maximum split nodes | 511 | every split commits exactly eight children |
| maximum returned leaves | 3,578 | `1 + 7 * 511`; result vector still reserves the fixed 4,096 slots |
| maximum pending DFS nodes | 57 | one active path plus seven siblings per depth |
| minimum forced surface depth | 2 | all unresolved root-surface regions receive an initial local audit |
| curvature ratio | 0.25 | refinement continues while cell diagonal exceeds one quarter detail scale |
| `MicroVoxelLeaf` size on tested target | 64 bytes | pinned by test |
| retained result vector payload | 262,144 bytes | `4,096 * 64`; allocator metadata excluded |
| maximum simultaneous result + DFS vector payload | 265,792 bytes | adds `57 * 64`; allocator metadata and stack-local child array excluded |
| clip planes | 6 | fixed-size `ClipSet`, no heap allocation |

For the most expensive current shape/clip combination, those node limits also
pin the primitive work per build:

| Work primitive | Compile-time maximum |
| --- | ---: |
| shape AABB classifications | 4,089 |
| oriented-ellipsoid vertex quadratic evaluations | 32,712 (`4,089 * 8`) |
| oriented principal-axis interval evaluations | 12,267 (`4,089 * 3`) |
| clip-plane AABB tests | 24,534 (`4,089 * 6`) |

The public constructor exposes these compile-time limits and no capacity
parameter. Private test-only budgets exercise pressure behavior at smaller
limits. The result vector reserves its complete fixed payload, so it cannot
silently grow beyond the stated leaf capacity.

## What is guaranteed, and what is not

### Implemented guarantees

- sphere and axis-aligned ellipsoid AABB extrema are exact in real arithmetic;
- oriented-ellipsoid `Outside` uses a conservative relaxed lower bound;
- oriented-ellipsoid maximum uses all eight vertices;
- uncertain floating-point cases become `Surface`;
- shell cavities and outer boundaries are treated independently;
- clip order cannot change classification or adaptive output;
- negative integer grid cells and Morton order are deterministic;
- complete octants preserve a partition when pressure stops refinement;
- depth, node, retained payload, and DFS payload have hard ceilings;
- no hash iteration participates in geometry or order;
- the module has no physics API and cannot affect shuttle forces.

### Explicit non-guarantees

- `q` for an ellipsoid is an implicit quadratic, not Euclidean signed distance;
- the module does not perform ray marching, meshing, collision response, or
  continuous collision detection;
- conservative cell coverage alone is not yet a declared 6/18/26-connectivity
  theorem for the assembled live chunk grid;
- the classifier does not promise a minimal surface voxel set;
- clipping is intersection with fixed half-spaces, not arbitrary union,
  subtraction, or a general CSG tree;
- no material mixture, opacity integral, density, or normal is stored in a
  microvoxel leaf yet;
- a budget-limited `Surface` leaf may be visibly coarse until a later consumer
  selects an appropriate representation;
- benchmark timings do not include ECS scheduling, chunk generation, meshing,
  GPU upload, draw submission, or save reconstruction.

## Cross-domain synthesis from the knowledge graph

### Conservative and topological voxelization

Schwarz and Seidel distinguish thin and conservative surface voxelization.
Laine's intersection-target formulation makes connectivity, separability, and
effective input dimension explicit. The important transfer is that visual
coverage, solid connectivity, and background separability are different
contracts.

This prototype solves analytic whole-cell coverage. Before live collision,
water containment, navigation, or room sealing uses it, integration must choose
and test a digital adjacency pair. Diagonal walls, tangent contacts, one-cell
necks, chunk seams, and clipped cavities need explicit 6/18/26-neighborhood
property tests. No global topology claim is inferred from attractive images.

Decision: adopt conservative `Outside` as a local invariant; defer a declared
global connectivity mode until the chunk assembler is part of the test.

### SDFs, ADFs, octrees, and sparse volumes

Frisken et al.'s adaptively sampled distance fields justify spending samples
where detail demands them. Laine and Karras, and later VDB-family work, show
why sparse hierarchical storage is valuable. These references do not justify
an unbounded recursive structure in a game world.

The chosen transfer is narrow: bounded local subdivision only where the
analytic classifier says the surface may cross. A future exact sphere SDF or
conservative ellipsoid-distance bound may add gradients and ray stepping, but
the current quadratic must not be mislabeled as an SDF.

Decision: adopt fixed surface-only subdivision; reject a second global dense
or unbounded voxel database.

### Marching Cubes versus Dual Contouring

Lorensen and Cline's Marching Cubes is a natural consumer for sampled scalar
fields, but original lookup cases have ambiguity issues. Corrected MC33 work
shows that topological correctness depends on the interpolant and disambiguation,
not merely on using a familiar algorithm name.

Dual Contouring consumes Hermite edge intersections and normals, places a
vertex through a quadratic-error solve, preserves sharp features better, and
can operate on adaptive octrees. It also requires careful rank handling,
cell-transition ownership, and crack tests.

Decision: neither mesher is hidden inside this classifier. First integrate the
bounded field and classification telemetry. Then compare corrected MC-style
smooth output and Dual Contouring on the same isolated implicit test corpus.
Authored block buildings keep their block-preserving path.

### Tomography, segmentation, anisotropy, and partial volume

The official DICOM image-plane contract stores pixel spacing, slice thickness,
orientation, and position separately. This supports a useful engine principle:
voxel index is not physical distance, and axis spacing cannot be assumed equal.
The explicit ellipsoid radii and world-space AABBs follow that principle.

Medical partial-volume research models boundary voxels as mixtures rather than
forcing one tissue label. The safe transfer is conceptual: `Surface` is an
honest unresolved boundary state, not a majority-vote material. A future
renderer may attach bounded material fractions or coverage to surface leaves.
This prototype does not simulate medical acquisition and does not claim a
medical segmentation algorithm.

Decision: preserve physical scale and uncertainty; reject binary majority as
the only future boundary-material representation.

### Leaves, veins, shells, and growth

Leaf research treats thin blades as shell structures; procedural venation work
separates a growing vein network from the blade domain. Fowler, Meinhardt, and
Prusinkiewicz model seashells by combining a helico-spiral sweep with a surface
pattern process. These are stronger generators than hand-placing more static
voxel cubes.

The future architecture should therefore separate:

- a semantic growth graph or aperture sweep;
- an implicit thin shell for coverage and local editing;
- a vein/rib graph for stiffness and material detail;
- bounded surface microvoxels near cuts, edges, curvature, and damage;
- a render-only wind deformation derived from the same stable plant seed.

Visual vegetation wind remains isolated from shuttle aerodynamics. This module
contains no force, velocity, mass, collider, or rigid-body state.

Decision: current shells are a geometry primitive for later leaf/dome/shell
generators, not a claim that a complete plant or mollusk generator is done.

### Material cells, crystals, Voronoi, and Delaunay

Worley's cellular basis partitions space by distances to deterministic feature
points and is useful for rock, ice, cells, crater, and crystal-like material
variation. Weighted crystal-growth Voronoi work adds different propagation
speeds. Voronoi/Delaunay structure may later provide region adjacency, fracture
seeds, or crystal material cells.

It is rejected as the geometry authority for this prototype: discontinuous
nearest-feature identity does not by itself give a conservative shell or a
watertight editable solid. It is a candidate material/growth field sampled
inside a proven geometry cell. Seeds must be world-coordinate deterministic
and neighborhood enumeration must be bounded before integration.

## Focused verification

The optimized standalone test binary contains fourteen tests:

1. sphere and axis-ellipsoid reflection symmetry;
2. analytic sphere and ellipsoid volume bracketing at two resolutions, with a
   shrinking uncertain band;
3. a `0.01`-thick shell whose root center lies in its cavity;
4. positive shell cavities across extreme and subnormal inner/outer ratios;
   ratios whose square cannot be represented are rejected instead of turning
   the centre solid;
5. conservative half-space cuts and cap cells;
6. identity equivalence of oriented and axis-aligned ellipsoids plus a rotated
   principal-axis surface point;
7. sampled falsification search: no sampled occupied point may occur inside a
   cell classified `Outside` for a rotated ellipsoidal shell;
8. 1,024 deterministic adversarial known-inside cases spanning anisotropic
   radii, random axis-angle rotations, solid/thick/thin shells, zero to three
   clip planes, reversed clip order, local/`1e6`/`+/-1e12` coordinates, and
   repeatable pressure-limited adaptive builds;
9. large coordinates, huge and subnormal radii, strict/non-degenerate boxes,
   checked box-volume and grid overflow, unrepresentable center/radius pairs,
   and clip normalization/dot overflow;
10. deterministic negative-coordinate subdivision and mixed-depth Morton order;
11. clip insertion-order independence;
12. hard node pressure with complete eight-child groups and parent fallback;
13. enum/path/leaf byte pins plus node, work, and payload ceilings;
14. optimized center, sphere-AABB, oriented-AABB, and adaptive-build benchmark.

The volume test brackets the known analytic volumes:

```text
sphere    V = 4*pi/3
ellipsoid V = 4*pi*a*b*c/3, with (a,b,c) = (1, 0.75, 0.5)
```

Both the `12^3` and `30^3` grids contain the exact volume between the sum of
fully-inside cells and the sum of inside-plus-surface cells. The fine uncertain
band is required to be narrower than the coarse one. The deterministic bounds
from the verified run were:

| Shape | Exact | `12^3` lower / upper | `30^3` lower / upper |
| --- | ---: | ---: | ---: |
| unit sphere | 4.188790 | 2.777778 / 5.925926 | 3.553185 / 4.842667 |
| ellipsoid `(1, 0.75, 0.5)` | 1.570796 | 1.041667 / 2.222222 | 1.332444 / 1.816000 |

## Benchmark protocol and result

The module is compiled directly so unrelated live-engine work cannot mask its
warnings:

```powershell
rustc --edition=2021 -D warnings --crate-type lib src\implicit_voxels.rs
rustc --edition=2021 -D warnings --test -O src\implicit_voxels.rs
```

The measured environment was:

| Item | Value |
| --- | --- |
| Rust compiler | `rustc 1.92.0 (ded5c06cf 2025-12-08)` |
| Cargo | `cargo 1.92.0 (344c4567c 2025-10-21)` |
| LLVM | 21.1.3 |
| host target | `x86_64-pc-windows-msvc` |
| OS | Windows 11 Professional, build 26200, 64-bit |
| CPU | AMD Ryzen 7 5700G, 8 cores / 16 logical processors |
| installed memory | 31.3 GiB |
| focused correctness flags | edition 2021, `-D warnings` |
| benchmark flags | edition 2021, `-D warnings`, `-O` (optimized standalone test binary) |
| Cargo linker/profile context | `rust-lld.exe`; dev root `opt-level=1`, dependencies `opt-level=3`; release `opt-level=3`, thin LTO, one codegen unit |

There is no repository `rust-toolchain.toml`; the active installed toolchain is
therefore recorded explicitly so later measurements can detect drift. The
standalone benchmark uses the direct `rustc -O` flags above, not the Cargo dev
profile.

The optimized benchmark performs:

- 5,000,000 center membership samples;
- 2,000,000 sphere AABB classifications;
- 500,000 oriented-ellipsoid AABB classifications;
- repeated adaptive builds with a private test cap of 1,025 visited nodes.

Thirty consecutive full focused-test runs on the current host measured the
following nearest-rank distribution. With only 30 samples, p99 equals the
maximum; it is reported honestly rather than interpolated into false precision.

| Hot path | Minimum | Median | p95 | p99 / maximum |
| --- | ---: | ---: | ---: | ---: |
| center membership baseline | 7.07 ns | 7.27 ns | 8.79 ns | 8.83 ns |
| conservative sphere AABB | 25.39 ns | 25.74 ns | 27.69 ns | 29.70 ns |
| conservative oriented-ellipsoid AABB | 119.84 ns | 121.38 ns | 131.64 ns | 149.04 ns |
| adaptive build, average 1,025 visited nodes | 41.76 us | 42.90 us | 46.03 us | 47.20 us |
| leaf payload | 64 bytes | 64 bytes | 64 bytes | 64 bytes |

This compares useful CPU kernels, not equal correctness. The center path is
faster because it does less work and is known to miss shells. The oriented
path evaluates interval projections and eight box vertices. Repeated-run data
should be refreshed after integration and compiler/profile changes.

### Verification transcript and release boundary

The final scoped audit on 2026-08-09 used the following commands. Build output
is reconstructible; no save, QA world, personal media, or user-owned dirty path
was read as an input or changed by this work.

| Command | Result |
| --- | --- |
| `rustfmt --edition 2021 --check src\implicit_voxels.rs` | pass |
| `rustc --edition=2021 -D warnings --crate-type lib src\implicit_voxels.rs` | pass |
| `rustc --edition=2021 -D warnings --test -O src\implicit_voxels.rs` followed by `--nocapture --test-threads=1` | 14 passed, 0 failed |
| `rustc --edition=2021 -D warnings --target wasm32-unknown-unknown --crate-type lib src\implicit_voxels.rs` | pass |
| `cargo test --bin voxel-native implicit_voxels::tests --quiet` | 14 passed, 0 failed; 1,023 filtered out in the current registered binary |
| `cargo check --bin voxel-native` | pass; 31 pre-existing/unrelated warnings |
| `cargo check --target wasm32-unknown-unknown --bin voxel-native` | pass; 90 pre-existing/unrelated warnings |
| `cargo test --workspace --quiet` | pending Root's integrated full-suite run after concurrent owned patches merge; no stale total is claimed here |
| `cargo fmt --all -- --check` | pending Root's repository-wide gate; the owned Rust file passes the scoped rustfmt check |

The standalone Wasm command compiles the module itself. The Cargo Wasm command
proves compile registration in the application target, not runtime scheduling,
rendering, or a visual route. Neither fact is presented as live Level-9 route
evidence. A clean repository-wide native suite, Natural/Astral engine routes,
and path-curated staging remain explicit release work.

The owned paths comply with the root `AGENTS.md` contract: they use checked
arithmetic and signed coordinates, deterministic ordering, compile-time work
and memory caps, a baseline/candidate/reject record, repeated measurements, and
fail-closed uncertainty. They do not register engine systems, alter authority,
touch saves, launch GPU QA, or claim a fallback as a completed live feature.

## Integration handoff and gates

No live integration is proposed until the pure contract is reviewed. When it
is accepted, a reversible first integration should follow this order:

| Tier | Consumes | May use from this module | Authority and fallback contract |
| --- | --- | --- | --- |
| Interaction | stable shape id/version, world epoch, exact user edits, local signed-integer cell requests | conservative cell class; bounded microcells only around the active tool/contact region | authored voxels, semantic object state, and edit log stay authoritative; an unresolved analytic cell is not editable empty space |
| Near | requested full chunk cells and current shape parameters | `Inside` fast fill, `Outside` proven skip, `Surface` mesh/micro work | keep the coarse `Surface` owner/render proxy until every promoted child result is installed; stale epoch/version work is discarded |
| Mid | stable analytic parameters plus reconstructible summary-brick requests | conservative occupancy/error hint or analytic silhouette bound | never retain thousands of per-shape microtrees; evict only reconstructible summaries and keep sparse edits separately |
| Far | macro feature seed, bounds, material family, shape id | analytic sphere/ellipsoid bound or impostor input | no caves, collision, edit, or exact shell thickness authority; a valid far parent remains while Near/Mid data changes |
| Celestial | double/integer world anchor plus analytic body parameters | direct analytic body/shell evaluation at local render coordinates | never instantiate planet-scale dense cells; interaction patches resolve only near a landing/edit region |

The integration owner must add `world_epoch`, stable shape identity, monotonic
shape/source version, task nonce, and stale-result rejection around this pure
API. Those concepts are intentionally not faked inside a geometry value type.
Near authored edits override regenerated analytic material, and their sparse
authoritative log must survive every summary or mesh eviction.

The current hard budget is **per adaptive build**. Live integration must add a
separate compile-time per-frame admission cap for requested builds, completed
installs, generated vertices/instances, and queued bytes. Until that scheduler
budget and its pressure telemetry exist, this module does not claim a bounded
frame cost merely because one build is bounded.

Recommended sequence:

1. wire the compile-registered module behind an Astral-first runtime/rollout
   gate;
2. add one isolated analytic-shape entity that owns stable world-space shape
   parameters rather than generated voxels;
3. classify only cells requested by the existing near/mid scheduler;
4. keep `Surface` parent coverage until every child representation is ready;
5. emit telemetry for inside/outside/surface counts, guard fallbacks, budget
   stops, precision stops, build time, and generated triangle/instance count;
6. compare block-preserving, corrected MC-style, and Dual Contouring consumers
   only on a controlled corpus;
7. add save-version rules for shape parameters and deterministic regeneration;
8. test chunk seams, negative coordinates, teleports, scale extremes, cuts,
   shells, and edits;
9. perform real-engine visual QA at close, midfield, horizon, multiple window
   sizes, Natural and Astral profiles, and moving-camera conditions;
10. prove that vegetation presentation and shape LOD do not change shuttle
    acceleration, collision, or flight input.

Whole-world use must remain layered. A planet-sized sphere should stay an
analytic celestial/macro primitive at distance, become bounded local surface
cells near the player, and produce exact editable near voxels only where
interaction requires them. This module is not permission to allocate
planet-scale dense microvoxels.

### Known impossibilities and deliberate limits

- Finite hardware cannot retain depth-8 microvoxels over every surface cell of
  a planet, every building, and every leaf while also keeping fixed memory and
  frame time. Distance- and interaction-dependent representations are required.
- A conservative classifier can avoid false empty cells, but it cannot
  simultaneously guarantee a minimal voxel set, a chosen global digital
  topology, perfect smooth shading, and zero transition work without a
  topology-aware assembler and mesher.
- The ellipsoid quadratic is not exact Euclidean signed distance. Using it as a
  sphere-tracing step would be an unsupported and potentially unsafe leap.
- Six half-spaces cannot represent arbitrary Boolean CSG. General unions,
  differences, self-intersections, and edited topology require a versioned CSG
  or authored-voxel authority with separate complexity limits.
- The pure module cannot prove visual quality, temporal stability, collision
  parity, save migration, or registered-engine integration merely by compiling
  for Wasm. Those properties require wiring into live systems and the
  corresponding Level 4/5/9 routes.

## Primary and official sources

Discovery pages are listed above. Engine decisions were anchored in these
primary papers or official documents:

- Michael Schwarz and Hans-Peter Seidel, *Fast Parallel Surface and Solid
  Voxelization on GPUs*:
  https://michael-schwarz.com/research/publ/2010/vox/
- Samuli Laine, *A Topological Approach to Voxelization*:
  https://research.nvidia.com/sites/default/files/pubs/2013-06_A-Topological-Approach/laine2013egsr_paper.pdf
- Samuli Laine and Tero Karras, *Efficient Sparse Voxel Octrees*:
  https://research.nvidia.com/publication/2010-02_efficient-sparse-voxel-octrees
- Sarah Frisken, Ronald Perry, Alyn Rockwood, and Thouis Jones, *Adaptively
  Sampled Distance Fields*:
  https://www.ronaldperry.org/sig2000_ADFs_Paper.pdf
- Tao Ju, Frank Losasso, Scott Schaefer, and Joe Warren, *Dual Contouring of
  Hermite Data*:
  https://www.cs.rice.edu/~jwarren/papers/dualcontour.pdf
- William Lorensen and Harvey Cline, *Marching Cubes: A High Resolution 3D
  Surface Construction Algorithm*:
  https://dl.acm.org/doi/10.1145/37401.37422
- Lis Custodio, Tiago Etiene, Sinesio Pesco, and Claudio Silva, *Practical
  Considerations on Marching Cubes 33 Topological Correctness*:
  https://www.sci.utah.edu/~etiene/pdf/mc33.pdf
- John Hart, *Sphere Tracing: A Geometric Method for the Antialiased Ray
  Tracing of Implicit Surfaces*:
  https://doi.org/10.1007/s003710050084
- Alan Barr, *Superquadrics and Angle-Preserving Transformations*:
  https://authors.library.caltech.edu/records/rtr62-f2882
- Ken Museth, *VDB: High-Resolution Sparse Volumes with Dynamic Topology*:
  https://museth.org/Ken/Publications_files/Museth_TOG13.pdf
- DICOM PS3.3, Image Plane Module, physical spacing/orientation/position:
  https://dicom.nema.org/MEDICAL/DICOM/current/output/chtml/part03/sect_C.7.6.2.html
- Xiaoping Yang et al., *A Theoretical Solution to MAP-EM Partial Volume
  Segmentation of Medical Images*:
  https://pubmed.ncbi.nlm.nih.gov/19768123/
- Adam Runions et al., *Modeling and Visualization of Leaf Venation Patterns*:
  https://citeseerx.ist.psu.edu/document?doi=66403382a0ac4be8076c6b67fbce73cf1edfb691&repid=rep1&type=pdf
- Bruno Moulia, *Leaves as Shell Structures*:
  https://doi.org/10.1007/s003440000004
- Deborah Fowler, Hans Meinhardt, and Przemyslaw Prusinkiewicz, *Modeling
  Seashells*:
  https://algorithmicbotany.org/papers/shells.sig92.pdf
- Steven Worley, *A Cellular Texture Basis Function*:
  https://cedric.cnam.fr/~cubaud/PROCEDURAL/worley.pdf
- Kei Kobayashi and Kokichi Sugihara, *Crystal Voronoi Diagram and Its
  Applications*:
  https://doi.org/10.1016/S0167-739X(02)00033-X
- G. M. Morton, *A Computer Oriented Geodetic Data Base and a New Technique in
  File Sequencing*:
  https://dominoweb.draco.res.ibm.com/0dabf9473b9c86d48525779800566a39.html
