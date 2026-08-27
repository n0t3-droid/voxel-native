# Rendering Research Notes

These notes translate selected public rendering references into Voxel Native
engineering questions.
They are not implementation-complete and should not be treated as current
engine behavior.

## References

- Nick McDonald's high-performance voxel engine article:
  `https://nickmcd.me/2021/04/04/high-performance-voxel-engine/`
- LearnOpenGL instancing chapter:
  `https://learnopengl.com/Advanced-OpenGL/Instancing`
- TinyEngine example tree:
  `https://github.com/weigert/TinyEngine/tree/master/examples/0.0_Empty`

## Practical Translation For voxel-native

The current engine is Rust/Bevy/WGPU-oriented, so OpenGL examples should be
used as architecture pressure, not copied directly. The relevant ideas are:

- Keep greedy/chunk meshing for solid voxel terrain instead of instancing every
  cube. Instancing is useful for repeated props, particles, bot markers, and
  scenery objects, but per-voxel instancing would inflate draw counts and
  bandwidth for terrain.
- Reduce driver/API overhead by batching work: fewer mesh uploads, fewer tiny
  draw surfaces, and stable buffers for frequently changing editor previews.
- Move toward pooled or persistently reused mesh buffers for chunk rebuilds so
  flying/loading does not stall on allocation and upload churn.
- Add explicit render categories: terrain chunks, transparent/liquid/glass,
  editor previews/gizmos, bots/ships, particles, and scenery props. Each
  category should have a bounded update budget.
- Use LOD and impostor thinking for scenery beauty. Distant forests, city
  silhouettes, butterflies/particles, and skyline lights should not be full
  near-field geometry.
- Treat visual beauty as a streaming problem: the first loaded frame should
  show coherent terrain, sky, and silhouettes, then refine details without
  one-second hitches.

## Candidate Milestone

1. Instrument draw calls, mesh uploads, chunk rebuild queue length, and frame
   time around startup, fast flight, and bot proximity.
2. Cap per-frame mesh uploads and separate terrain rebuild budget from editor
   preview budget.
3. Pool chunk mesh buffers or introduce a staging allocator for rebuild output.
4. Add instanced rendering only for repeated non-terrain props where one mesh is
   reused many times.
5. Rework scenery generation to favor large readable silhouettes, color
   contrast, and low-cost detail layers before adding more geometry.

## Applied natural-world pass — 2026-08-09

This pass converts four additional references into bounded engine contracts:

- Virtual Horizon Method, DOI `10.26868/25222708.2025.1302`: eight azimuths,
  fixed multiscale radii, four seam-sharing chunk-corner sensors, and cached
  coalescing builds provide restrained macro terrain occlusion. This is an art
  lighting proxy, not validated solar irradiance; transparent vegetation,
  undercuts, and dynamic authored structures remain limitations.
  The publisher landing page identifies paper `bs2025_1302` and reports 91.69%
  mean annual-irradiance accuracy, while the linked PDF body carries a
  mismatched title/DOI header and reports 94.53%. Voxel-Native therefore uses
  the algorithmic idea only and does not repeat either percentage as settled
  evidence.
- Meyer/Neyret multiscale complex geometry: conifers preserve a visible
  trunk → bough → cone/needle-mass hierarchy. Broadleaf foliage uses a second
  multiscale contract: restrained texture pores in the base level converge
  toward a filled, stable aggregate in lower gamma-correct mip levels.
- Generative Adversarial Shaders (`arXiv:2306.04629`): the useful transferable
  constraint is a small, deterministic, temporally stable shader pipeline.
  Adversarial training was deliberately not added because voxel-native has no
  approved target corpus or measurable learned-style goal.
- CADIA Shaders and Visual Realism slides: lighting, material response,
  atmosphere, and motion are reviewed as one perceptual stack. The deck is
  instructional context and carries less evidential weight than the papers.

The corresponding world rules are habitat-driven tree cohorts with genuine
light wells, four bounded tree silhouettes, broad karst meadow/moss/limestone
masses, calmer world-anchored materials, local voxel AO plus macro horizon
depth, and foliage-only wind. Wind owns no collider, player, shuttle,
projectile, voxel-authority, save, or navigation state.

Visual QA is a rejection loop. A build fails even when tests pass if it shows
daytime star cards, squared/doubled albedo, checker or contour bands, regular
tree spacing, sealed cubic crowns, repeated cross-shaped shrubs, floating
foliage, or an attractive result at only one viewing distance.

### Rejected same-seed visual baseline and derived contracts

The same seed at 10.8 hours exposed three coupled failures that unit tests had
not represented: alternating bright top and dark side faces turned voxel
terraces into map-like contour lines; broad grass waves became blurred repeated
camouflage at walking height; and Lush tree crowns required a seven-voxel
margin inside a sixteen-voxel chunk, leaving almost only root coordinates 7/8.

The replacement separates scale and ownership:

- outdoor AO and directional vertex fill retain contact depth but bound the
  darkest face relative to the top; daylight ambient now represents stronger
  sky bounce while preserving a dominant key light;
- grass keeps hundreds of base-level fibre colours but deliberately aggregates
  to a small, bounded signature set in distant mips;
- gentle sheltered karst grades may carry broad meadow/moss skins, while steep
  cliffs remain continuous limestone;
- tree roots use jittered world ownership cells. Every target chunk replays a
  fixed 3x3 root halo and clips signed writes, allowing one connected crown to
  cross horizontal chunk seams without shared mutable generation state.

A replacement near/play/horizon inspection used the same seed and late-morning
light. Within that scoped historical review, the retained frames showed organic
grove placement, living gentle slopes, readable limestone masses, softer
terrace faces, and a stable daytime sky. The result was a coherent voxel
landscape; it is not presented as photorealistic botany or current release
evidence. No causal performance claim is inferred from that flight because
camera path, streaming state, and adaptive render distance were not controlled.

The replacement inspection also exposed a capture-state loophole: the Agent Control handoff/pause
state eventually returns to combat presentation, which can make the held item
reappear in a delayed screenshot. Hero evidence must therefore record the
capture timing and visible gameplay state, not merely the final file name.

## Hydrographic and riparian pass - 2026-08-09

### Baseline and candidate pressure

The previous terrain used independent local noise strongly enough that water
often read as ponds, coast fragments, or a painted low band instead of a course
with upstream/downstream continuity. Vegetation then sampled biome and a global
style roll, so a tree beside water did not communicate why it grew there.

Four approaches were considered:

- More unwarped threshold noise was the cheapest change, but it preserves the
  disconnected-island failure and supplies no stable tangent for bank layout.
- Authored river splines provide direct art control, but do not scale to every
  procedural seed and would make unexplored worlds depend on authored content.
- Priority-flood plus D8/D-infinity flow, or a shallow-water solver, provides a
  stronger physical drainage model. It was not selected for this pass because
  chunk-border accumulation, cache invalidation, persistence, and deterministic
  streaming need a dedicated architecture milestone rather than an incidental
  terrain patch.
- A warped continuous zero-contour field with a lowland/elevation envelope can
  produce connected courses, a stable local flow tangent, and bounded local
  evaluation without shared mutable chunk state. It is not a claim of hydraulic
  simulation, but it fits the current generator and streaming contract.

The selected implementation combines that connected contour with channel and
floodplain carving, bank soil/biome repair, and one `EnvironmentSample` contract
for river strength, flow direction, soil moisture, exposure, and habitat. The
same contract promotes only eligible broadleaf trees into riparian silhouettes,
aligns wide crowns with the flow tangent, adds bounded face-connected hanging
fringes, and strengthens bank understory without changing player, shuttle, bot,
or collision physics. Foliage wind remains a render-material vertex effect.

### Proof and rejected overclaims

Deterministic tests cover connected channel focus, two living banks, bounded
open-water exposure, flow-axis selection, habitat-gated riparian promotion,
connected hanging foliage, and the tree geometry cap. Fresh QA flights inspect
the same route at overview and play distance; queue telemetry is recorded
separately from stabilized frame rate. A coastal/confluence seed remains useful
for gallery-tree visibility but is not presented as the strongest inland-river
hero shot. Dynamic erosion, sediment transport, shallow-water dynamics, and
botanical photorealism are explicitly outside this pass.

The visual flight also found a cross-plugin startup defect unrelated to tree
geometry. Four companion robots appeared high in the sky. Their saved hub used
Y=87 even though the selected river focus was at or below Y=46. The bot plugin
had read `VoxelWorld` while a separate `OnEnter(InGame)` system was replacing
its bootstrap generator; plugin insertion happened to determine which seed won.
Fresh bot saves now derive grounding directly from the authoritative active
world seed, matching the independent ship-spawn contract. A regression test
uses the observed river seed and proves the hub cannot inherit the stale
bootstrap surface. Existing saves are left intact rather than silently migrated.

Finally, the urgent camera-neighbourhood mesh tier prevents far backlog from
starving visible chunks, and agent flight input uses a signed-square precision
curve plus a bounded remote speed envelope. These are causal fixes for visible
holes and unusably aggressive analog flight; neither changes manual player nor
shuttle speed contracts.

Historical local runs from 2026-08-09 exercised the 1280 x 720 mountain-river
route, 800 x 600 and 320 x 480 UI densities, and fresh companion grounding.
Their `qa_runs/` captures are intentionally excluded from the repository and
are not current release evidence. The checkpoint also omitted 1920-class,
ultrawide, and OS/text-scale captures. Those cells therefore remain open until
fresh reports and screenshots are explicitly selected, validated, and
inspected under the current evidence contract; no long-lived test total or
visual acceptance verdict is inferred from that historical run set.
