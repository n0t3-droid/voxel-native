# AI Handoff

This branch contains the current source-only engine update for the in-game
Sketch Editor, bot-city control, startup stability, and low-end performance
work. The local worktree also contains many generated `saves/` and runtime
world files from playtesting; those are intentionally not part of the source
handoff unless a later task explicitly needs a reproducible save.

## Current Branch

- Branch: `feature/dev-setup`
- Remote: `origin` -> `https://github.com/n0t3-droid/voxel-native.git`
- Source files changed under `src/` plus the new semantic editor spine
  `src/sketch_model.rs`.
- Last verified source snapshot before this handoff note: `f90781df`
  (`Fix sketch inference reference alignment`).

## 2026-07-04 External AI Review Brief

This project still does not behave like SketchUp. The latest user testing
shows several user-visible failures that should be treated as blockers before
adding more decorative tools:

- Normal drawing should infer endpoints, midpoints, same-height references,
  and parallel/axis alignment from mouse hover without forcing arrow-key locks.
  Arrow keys should be optional hard locks, not the only reliable way to align.
- Pencil/Line can still show a cyan preview cage or axis guide that does not
  match the visible mouse pointer's intended block. The recent pointer-ray fix
  reduced the worst mismatch, but the live tool is still too cell/grid driven
  and not enough screen-space/inference driven.
- Endpoint, midpoint, face-center, and edge markers must be obvious in the
  world: green endpoint/start/end, cyan midpoint, red edge/axis, blue face.
  The status text alone is not enough.
- Select -> Move/Rotate/Scale is not a SketchUp-grade workflow yet. The user
  expects to click a drawn object/component, choose a grip point, drag it to an
  inferenced target, rotate/scale from handles, and keep voxel-to-voxel snaps.
- Undo/redo is not trustworthy enough. Voxel edits, semantic entities,
  link-index changes, and transform operations currently pass through multiple
  partial history paths. The next AI should inspect `BuilderHistory`,
  `SketchDocument` snapshots, `ToolController` transaction labels, and
  transform commits, then converge them into one command timeline.
- The toolbox still has duplicate-feeling tools. Primary tools should be the
  highest-frequency SketchUp-like actions; specialized voxel/game workflows
  belong in categorized flyouts with stable hover behavior.

Treat the next milestone as "SketchUp interaction correctness", not "more
icons". The highest-value implementation slice is:

1. Route Pencil, Rectangle, Select, Move, Rotate, Scale, Push/Pull, Opening,
   and Room through the same `PickService -> InferenceService -> ToolState`
   pipeline every frame.
2. Make hover inference visual first: draw colored snap points and axis/edge
   guide lines at the inferenced world point under the actual OS cursor.
3. Make arrow keys only set or clear explicit locks; natural hover inference
   should already pick useful endpoints, midpoints, same-height references,
   and parallel/perpendicular guides.
4. Replace direct per-tool undo batches with a unified sketch command history
   that records voxel diffs, semantic deltas, link-index updates, selection,
   and transform metadata together.
5. Upgrade Select/Move before adding new modeling tools: selection must create
   a manipulable component-like object with exact grip-to-target snapping.

Files to inspect first:

- `src/sculpt/draw.rs`
- `src/sculpt/transform.rs`
- `src/sketch_model.rs`
- `src/sculpt/pushpull.rs`
- `src/builder.rs`
- `src/toolbelt.rs`
- `src/selection.rs`
- `src/sculpt/mod.rs`

## Implemented Direction

- Build/editor UI has been moving away from visible F-key switching toward a
  mouse-first Sketch Editor toolbox and status bar.
- The Sketch Editor toolbox now has a smaller primary rail ordered around core
  modeling first: Select, Line, Rectangle, Circle, Push/Pull, Move, Rotate,
  Scale, and Material. Opening, Room, House, Roads, Bots, city shell, landscape,
  skyline, and spacecraft remain available through compact contextual flyouts
  instead of competing on the first-level rail.
- Each visible workflow button shows a simple label plus an inference cue such
  as Point, Corner, Face, Path, Area, Axis, Plane, or Volume so the player can
  tell what the tool snaps to before clicking it.
- The contextual flyout has a wider bridge zone and longer grace hold while
  the cursor moves from the rail into the flyout. The full workflow/material
  catalog is now the explicit `STYLE` drawer only. Cursor policy is also
  UI-aware: right mouse only becomes world orbit when the pointer is not over
  the Sketch Editor UI, so the toolbox should not make the mouse disappear
  while selecting tools.
- The latest toolbox pass keeps the last hovered context group alive during
  the grace window, so moving from the rail into the flyout should no longer
  collapse or swap the panel mid-travel. Primary labels are now plainer:
  Select, Line, Rect, Circle, Push/Pull, Move, Rotate, Scale, and Paint.
- `sketch_model` is now the semantic spine for editor tools, transactions,
  selection, inference, components, rectangle/pencil semantics, room/opening
  semantics, and Push/Pull-style operations.
- Rectangle, Pencil, Room, Opening, Push/Pull, Road, Bot Area, House, and city
  workflows are routed through shared workflow state instead of isolated HUD
  shortcuts.
- Right mouse is reserved for orbit during Sketch Draw and Push/Pull; it should
  not delete or cut blocks while drawing.
- Startup/runtime budget code was tightened to throttle render distance,
  terrain jobs, mesh jobs, shadow radius, and effects while the world catches
  up.
- Bot-city work was changed toward manual area/command control, lower startup
  visual load, and fewer high-detail idle bot rigs.
- The current follow-up adds toolbox exposure and voxel preview/commit routing
  for Circle, Polygon, Arc, and Freehand drafting workflows. These route through
  `ToolController` and write semantic sketch entities instead of being only
  placeholder catalog entries.
- Startup pressure now scans a much smaller dirty-mesh candidate window, and
  crowded bot saves scan project queues through a smaller rotating window.
- The CAD Copilot note from the user is now represented in
  `src/sketch_model.rs` as `SketchCadCommand` / `SketchCadTool` plus
  `SketchDocument::execute_cad_command`. Bots or another AI can emit a strict
  semantic command recipe for `ROAD`, `ROOM`, `PENCIL`, `RECTANGLE`,
  `PUSH_PULL`, `OPENING`, `CIRCLE`, `POLYGON`, `ARC`, `FREEHAND`, and
  `BOT_AREA` without placing raw voxels or relying on live cloud AI.

## CAD Copilot Command Contract

The command layer is serializable and intentionally close to the user's
provided JSON recipe. Example shape:

```json
[
  {
    "tool": "ROAD",
    "material": "GlowStone",
    "width": 3.0,
    "points": [
      { "x": -15.2, "y": 4.0, "z": 22.1 },
      { "x": -5.0, "y": 4.0, "z": 10.5 },
      { "x": 12.4, "y": 4.5, "z": -2.3 }
    ]
  },
  {
    "tool": "ROOM",
    "material": "Limestone",
    "height": 4.0,
    "width": 0.35,
    "points": [
      { "x": -7.0, "y": 4.0, "z": 12.0 },
      { "x": -3.0, "y": 4.0, "z": 12.0 },
      { "x": -3.0, "y": 4.0, "z": 8.0 },
      { "x": -7.0, "y": 4.0, "z": 8.0 }
    ]
  }
]
```

Each command creates semantic `SketchEntity` records and one undo batch per
bot/user intent. Roads are stored as semantic freehand curves with CAD
metadata, rooms create both a shell face and a room entity, and targeted
`PUSH_PULL` / `OPENING` commands operate against a semantic face id.

The next bridge is also started: `SketchVoxelLinkIndex` maps committed voxel
cells/faces back to semantic `SketchId`s. Sketch Draw and Push/Pull now register
their committed cells into that index, and Push/Pull hover resolution publishes
`SemanticHoverHit` when the hovered voxel face has a semantic link. Select /
Navigate clicks now consume that hover record and update
`ToolController.selection`, which is the base for material assignment,
transforms, openings, and bot edits that target existing semantic faces instead
of flood-filling anonymous voxels.

The PDF-required "pick first, infer second" split is now represented in the
semantic layer. `PickService` ranks raw `HitRecord`s by distance and hit-kind
tiebreaks without applying snap bias, while `InferenceService::from_pick`
converts the chosen raw hit into InputPoint-style endpoint, midpoint,
face-center, on-face, axis, and from-point candidates. This is still not full
SketchUp inference parity, but it prevents the next tools from mixing raycast
selection and snapping into one opaque helper.

The user's later R93G system specification adds the exact Phase-1 math layer:
`project_world_to_screen`, `screen_space_inference_candidates`,
`best_screen_space_inference`, `closest_point_on_locked_axis_from_ray`, and
`rectangle_plane_from_view_or_face`. These cover view-projection/NDC candidate
projection, SketchUp-style screen-space priority plus sticky inference,
skew-line axis locking for Shift/arrow constraints, and dynamic Rectangle plane
selection from locked axis, hovered face normal, or dominant view axis.

The first live Pencil/Sketch Draw wiring is now in `src/sculpt/draw.rs`.
Start and hover input points store their exact world marker, snap kind, and
discrete voxel endpoint. Endpoint, midpoint, and face-center snaps now affect
the drawn start/end cell instead of being status text only. The Draw gizmo
renders small colored input-point markers plus an axis guide, and Pencil can
use arrow-key axis locks: Right = X, Left = Z, Up = Y height, Down = clear.
When Pencil leaves the original face plane through an axis lock, its voxel
stroke uses a 3D line path instead of collapsing back onto the old plane.
The latest cursor-alignment pass also makes unlocked mouse mode feed Draw and
Select semantic hover from the real pointer ray instead of the center
crosshair. Pencil semantic edges now store the same center points shown by the
gizmos, and semantic hover prefers the exact voxel cell under the cursor before
using a broader face/stroke fallback. This directly targets the reported issue
where the visible mouse pointer sat on one block while the blue alignment
preview attached to another block.
The status readout now names the active reference explicitly, for example
`Endpoint | same height line Y 8 -> 13`, `Midpoint | red X line 4 -> 9`, or
`Face center | equal length`, so users can see whether the current point is
really aligned before committing.

Selection and Move are now no longer just semantic placeholders. `SketchVoxelLinkIndex`
can resolve cell-level hits for Pencil strokes and translate linked cell/face
records when selected entities move. `src/sculpt/transform.rs` adds the first
voxel-snapped Move drag: select a semantic stroke/face/component, drag, use
Right/Up/Left arrows for X/Y/Z locks, preview the target bounds with Gizmos,
and release to commit voxel edits plus semantic document/link translation in a
single undo batch.

The first lightweight B-Rep/vector layer now lives in `src/sketch_model.rs` as
`SketchBRepKernel`. It stores linked vertices, oriented edges, loop faces, plane
equations, and supports the first PDF-required operations: coplanar face split
and Push/Pull extrusion into a top face plus side faces. `SketchDocument` can
export an existing semantic face into a B-Rep kernel, which is the bridge toward
SketchUp-style face editing before voxel rasterization.

The SketchUp inference/transform video follow-up has also started at the
semantic layer. `SketchDocument` now has undoable `scale_selection_about_pivot`
and `flip_selection_across_plane` operations. These support exact scale factors,
component-instance transforms, arbitrary mirror planes from inferred axes/faces,
and geometry/bounds updates for faces, curves, openings, rooms, and extrusions.

## Important Remaining Gaps

- SketchUp equivalence is not complete. Circle, polygon, arc, and freehand now
  have toolbox routing and voxel commits, but arcs/freehand are still simple
  first-pass raster tools and need real curve editing, face splitting, and
  component-aware geometry.
- The builder still needs stronger endpoint/midpoint/face-center inference for
  all drafting tools. Pencil/Sketch Draw now has first live input-point markers,
  pointer-ray semantic hover, preferred-cell hover, and arrow locks, but
  Push/Pull, Move, Scale, Rotate, Opening, and every advanced draw tool still
  need the same universal pipeline.
- The new screen-space snap and skew-line locking math is tested in
  `sketch_model`, but only the first Pencil/Sketch Draw path consumes axis
  locking live. The remaining tools still need to consume it per frame and draw
  green/cyan/red/blue inference overlays near the cursor.
- The next major architecture step should wire the new `SketchBRepKernel` into
  live Pencil/Rectangle/Push/Pull/Openings, then voxelize B-Rep previews/commits
  through the existing batch edit and undo paths. Semantic material/transform/
  opening actions should operate on `ToolController.selection`.
- Move is now wired to live mouse drag for semantic selections. Scale and
  Rotate remain first-class editor tool IDs with semantic operations, but still
  need real handle UX, local-axis pivots, and preview/commit parity.
- The Rendering/Scenery research from Nick McDonald's high-performance voxel
  article, LearnOpenGL instancing, and TinyEngine should be translated later
  into Bevy/WGPU-native batching work: persistent/pooled mesh buffers, fewer
  per-chunk uploads, indirect/dense draw grouping where practical, and scenery
  LOD. Do not copy the OpenGL examples directly into Rust; use them as design
  pressure against the current chunk mesh/update pipeline.
- `docs/SKETCHUP_EQUIVALENCE_AUDIT.md` tracks which PDF/SketchUp capabilities
  are actual, partial, or missing. Keep it honest; do not mark a feature exact
  just because a similarly named Rust type exists.
- Startup can still inherit huge generated save/edit/bot state locally. Keep
  source commits separate from generated `saves/` unless deliberately testing a
  specific world.
- Bot proximity lag should be profiled around project scanning, companion
  target updates, and visible bot rigs before adding new visual complexity.
- Sky visuals are camera-centered by design, but the reported "stars move with
  mouse" complaint should be checked visually before changing the two-camera
  sky pass.

## Suggested Next Test Targets

- `cargo test toolbelt::tests`
- `cargo test sculpt::draw::tests`
- `cargo test sculpt::pushpull::tests`
- `cargo test sketch_model::tests`
- `cargo test bots::tests`
- `cargo test neurocore::tests`
- `cargo test`
- `cargo build`

## Publishing Scope

Commit source and docs explicitly. Do not use `git add -A` in this worktree:
generated saves, `.codex_tmp`, screenshots, and local playtest artifacts are
mixed into the working tree.
