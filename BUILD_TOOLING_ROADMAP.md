# Professional Build Tooling Roadmap

This plan is for turning voxel-native's current build tools into a coherent, SketchUp-like direct-manipulation editor. It is intentionally phased so each pass makes the engine more usable before the next complex feature lands.

## Current Problem

The current toolset has useful pieces, but they do not feel like one professional system:

- Build/editor/toolbelt/weapons are separate state machines, so the player cannot always tell what mode is active.
- Inputs fail silently when the palette is open, cursor is not locked, raycast misses, or the wrong tool is live.
- Tool names are too short and mixed: `RECT`, `SCULPT`, `PLACE`, `CUT`, `FACE`, `ANIM` do not explain when to use them.
- The bottom toolbelt and hotbar compete for space and can look like two unrelated systems.
- There is no persistent selection workflow: draw/select/fill/move/rotate/scale/retexture should be one fluent loop.
- Localization does not exist; UI text is hardcoded across source files.

## Design Target

Press `F3` and enter a clear Build Studio:

- Weapons holster and stay disabled until Build Studio exits.
- A visible mode strip says what is active and what the mouse will do.
- Tools are named by job, not internal implementation.
- Every blocked input explains itself in the status strip.
- Direct tools work in the world: draw, select, fill, push/pull, move, rotate, scale, paint, duplicate.
- Undo/redo is always visible and always works across build operations.
- UI text comes from a localization layer, not hardcoded strings.

## Sprint 1: Stop The Confusion

Goal: make the current tools understandable and predictable before adding more power.

Files:

- `src/toolbelt.rs`
- `src/hud.rs`
- `src/menu.rs`
- `src/weapons.rs`
- `src/builder.rs`
- `src/commands.rs`

Changes:

1. Add a Build Studio mode strip.
   - Show active mode: `COMBAT`, `BUILD PICKER`, `BUILD LIVE`, `EDITOR`, `PAUSED`.
   - Show current tool full name and one-line action: `Rectangle Fill - drag LMB on a face`.
   - Show weapons state: `Weapons holstered` while building.

2. Remove silent failures.
   - If ESC/E is blocked by Build Studio, set status: `F3 closes Build Studio`.
   - If LMB cannot build because palette is open, show: `Choose a tool or press Tab to hide palette`.
   - If raycast misses, show: `No block face under crosshair`.
   - If 1-9 is pressed while building, show: `Weapon hotkeys disabled in Build Studio`.

3. Clean up F3/F7/Tab.
   - `F3`: enter/exit Build Studio.
   - `Tab`: show/hide tool picker while staying in Build Studio.
   - `F7`: either remove from the active mental model or make it a secondary alias for hiding/showing the picker, never a separate build state.

4. Rename and group tools.
   - `Navigate` -> `Navigate / Inspect`
   - `DrawRect` -> `Rectangle Fill`
   - `Sculpt` -> `Push Pull Face`
   - `BrushPlace` -> `Place Brush`
   - `BrushCut` -> `Cut Brush`
   - `CityRoad` -> `Road Tool`
   - `CityDistrict` -> `District Zone`
   - `CityBuilding` -> `Building Shell`
   - `CityFacade` -> `Facade Stamp`
   - `AnimationPick` -> `Animation Picker`

5. Redesign the toolbelt layout.
   - Group tools by category: Navigation, Shape, Edit, City, Animation.
   - Keep hotbar visually separate from build tools.
   - Add hover tooltips using existing `hint()` plus richer full names.
   - Use color category accents only as secondary signals.

Verification:

- Build with `cargo build --release --color never`.
- Start engine and press F3.
- Confirm weapons disappear and 1-9 cannot switch weapons.
- Confirm every blocked action updates the status strip.
- Confirm tool picker names are clear without guessing.

## Sprint 2: One Source Of Truth For Modes

Goal: stop build/editor/combat state from drifting apart.

Files:

- New: `src/mode.rs`
- `src/main.rs`
- `src/toolbelt.rs`
- `src/menu.rs`
- `src/weapons.rs`
- `src/editor.rs`
- `src/animation.rs`
- `src/city.rs`
- `src/hud.rs`

Add:

```rust
pub enum ActiveMode {
    Combat,
    BuildPicker { tool: ToolbeltTool },
    BuildLive { tool: ToolbeltTool },
    Editor { tab: EditorTab },
    Paused,
    CommandPalette,
}

pub struct ModeContext {
    pub mode: ActiveMode,
    pub last_mode: ActiveMode,
    pub status: String,
}
```

Rules:

- Weapons only run in `Combat`.
- Build tools only run in `BuildLive`.
- Tool UI runs in `BuildPicker` or `BuildLive` with picker visible.
- Editor UI runs in `Editor`.
- Menus run in `Paused`.

Migration:

1. Add `ModeContext` while keeping old states in sync.
2. Make `weapons.rs` read `mode.allows_weapons()`.
3. Make `menu.rs` use mode transitions instead of early returns.
4. Make `toolbelt.rs` transition modes instead of owning live/palette truth.
5. Make `city.rs` and `animation.rs` derive their active tool from mode, not from manual sync.

Verification:

- Log every mode transition once.
- Confirm there is no state where tool picker is visible and weapons fire.
- Confirm city and animation tools activate only when selected.

## Sprint 3: Selection First, Then Transform

Goal: make the core SketchUp loop real: select something, then manipulate it.

Files:

- `src/sculpt/state.rs`
- New: `src/sculpt/selection.rs`
- New: `src/sculpt/gizmo.rs`
- New: `src/sculpt/transform.rs`
- `src/sculpt/draw.rs`
- `src/sculpt/mod.rs`
- `src/builder.rs`
- `src/world.rs`

Features:

1. Rectangle Select.
   - Drag on a face to create a persistent selection, not just an immediate fill.
   - `Enter` or a Fill button fills it.
   - `Esc` clears active drag; second `Esc` exits Build Studio.
   - Selection outline stays visible after release.

2. Fill Selected.
   - Fill the selection with active block/material.
   - Record one undo batch through `BuilderHistory::record_external()`.

3. Move Gizmo.
   - Show red/green/blue axes at selection center.
   - Drag an axis to move selected voxels by integer blocks.
   - Preview while dragging; commit on release.

4. Rotate Gizmo.
   - Show X/Y/Z rings.
   - Rotation snaps to 90 degrees.
   - Commit through one undo batch.

5. Scale Gizmo.
   - Corner/axis handles scale selection by integer factors.
   - Start with simple nearest-neighbor up/down scale.

Data reuse:

- Use existing `SculptSelection::Aabb` and `VoxelBlob` in `src/sculpt/state.rs`.
- Use `VoxelWorld::edit_set_voxel_batched()` and `finish_edit_batch()`.
- Use `BuilderHistory::record_external()`.
- Use `dda_voxel()` and `ray_to_locked_plane()` patterns.

Verification:

- Select a 3x3 area, fill it, undo it.
- Select a block group, move it on X/Y/Z, undo it.
- Rotate a rectangular prism 90 degrees, undo/redo it.
- Scale a selection and verify no crash on chunk boundaries.

## Sprint 4: Material Paint And Retexture

Goal: let users change appearance without destroying block structure.

Files:

- New: `src/sculpt/paint.rs`
- `src/toolbelt.rs`
- `src/textures.rs`
- `src/world.rs`
- `src/editor.rs`

Features:

1. Add `Material Paint` tool.
2. Add compact material picker panel.
3. Paint modes:
   - Replace block and material.
   - Material only.
   - Erase to air.
4. Brush size via mouse wheel or small stepper buttons.
5. Retexture Selected button for any persistent selection.

Implementation notes:

- Prefer `edit_set_cell_batched()` for material-aware edits.
- Keep one undo batch per stroke.
- Preserve material in copy/duplicate blobs.

Verification:

- Paint a wall material without changing block type.
- Retexture a selected rectangle.
- Undo and redo material changes.
- Save/load and confirm materials persist.

## Sprint 5: Copy, Duplicate, History UI

Goal: fast high-detail building needs repetition tools.

Files:

- New: `src/sculpt/clipboard.rs`
- `src/sculpt/state.rs`
- `src/hud.rs`
- `src/builder.rs`

Features:

1. `Ctrl+C`: copy selection to `VoxelBlob` with materials and mask.
2. `Ctrl+V`: paste preview in front of camera.
3. `Ctrl+D`: duplicate selected voxels along last transform axis or camera-right.
4. Visible Undo/Redo buttons in Build Studio.
5. History label shows last action: `Moved Selection`, `Filled Rectangle`, `Painted Material`.

Verification:

- Copy/paste a detailed object with materials.
- Duplicate it multiple times.
- Undo each duplicate cleanly.

## Sprint 6: Localization

Goal: all UI text comes from language resources.

Files:

- New: `src/localization.rs`
- `src/settings.rs`
- `src/main.rs`
- `src/menu.rs`
- `src/editor.rs`
- `src/hud.rs`
- `src/toolbelt.rs`
- `src/commands.rs`
- `src/builder.rs`
- `src/city.rs`
- `src/animation.rs`

Approach:

- Start with `Language::{German, English}` and a central string table.
- Store selected language in save/settings.
- Add editor/system language picker.
- Convert UI strings in waves: toolbelt and mode strip first, then menu/editor, then command deck and status text.
- Keep fallback behavior: missing translation displays the key, making gaps obvious.

Initial language keys:

- `mode.combat`
- `mode.build_picker`
- `mode.build_live`
- `tool.rectangle_fill.name`
- `tool.rectangle_fill.hint`
- `tool.push_pull_face.name`
- `tool.push_pull_face.hint`
- `status.weapons_holstered`
- `status.no_target_face`
- `status.press_f3_to_exit_build`

Verification:

- Switch language in editor/system tab.
- Confirm toolbelt names, mode strip, and command deck update.
- Restart game and confirm language persists.

## Coding Principles

- Do not add new build features until mode/status feedback is reliable.
- Every live action must have: visible selected tool, preview, commit, cancel, undo, redo.
- No silent early returns in player-facing tools; update status when a user action cannot run.
- Use one edit batch per user action, not per frame.
- Keep chunk/voxel storage unchanged unless a feature absolutely requires it.
- Use existing world edit APIs and BuilderHistory.
- Add localization incrementally; do not block core build fixes on translating every string at once.

## First Implementation Slice

The next code slice should be Sprint 1 only:

1. Mode strip in HUD/toolbelt UI.
2. Clear tool names and grouped toolbelt.
3. F3/Tab/F7 cleanup.
4. Status feedback for blocked ESC/E/1-9/LMB.
5. Release build and run engine.

This gives the user an immediately more professional tool experience before deeper transform-gizmo work begins.
