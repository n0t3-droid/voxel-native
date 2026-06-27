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

## Implemented Direction

- Build/editor UI has been moving away from visible F-key switching toward a
  mouse-first Sketch Editor toolbox and status bar.
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

The first lightweight B-Rep/vector layer now lives in `src/sketch_model.rs` as
`SketchBRepKernel`. It stores linked vertices, oriented edges, loop faces, plane
equations, and supports the first PDF-required operations: coplanar face split
and Push/Pull extrusion into a top face plus side faces. `SketchDocument` can
export an existing semantic face into a B-Rep kernel, which is the bridge toward
SketchUp-style face editing before voxel rasterization.

## Important Remaining Gaps

- SketchUp equivalence is not complete. Circle, polygon, arc, and freehand now
  have toolbox routing and voxel commits, but arcs/freehand are still simple
  first-pass raster tools and need real curve editing, face splitting, and
  component-aware geometry.
- The builder still needs stronger endpoint/midpoint/face-center inference for
  all drafting tools, not only rectangle/pencil and Push/Pull.
- The next major architecture step should wire the new `SketchBRepKernel` into
  live Pencil/Rectangle/Push/Pull/Openings, then voxelize B-Rep previews/commits
  through the existing batch edit and undo paths. Semantic material/transform/
  opening actions should operate on `ToolController.selection`.
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
