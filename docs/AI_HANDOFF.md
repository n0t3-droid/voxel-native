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

## Important Remaining Gaps

- SketchUp equivalence is not complete. Circle, polygon, arc, and freehand
  exist semantically in `sketch_model`, but still need full toolbox exposure,
  voxel preview/commit routing, and polished UI feedback.
- The builder still needs stronger endpoint/midpoint/face-center inference for
  all drafting tools, not only rectangle/pencil and Push/Pull.
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
