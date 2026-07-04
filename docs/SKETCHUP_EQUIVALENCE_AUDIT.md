# SketchUp Equivalence Audit

This is the current source-level audit against the local
`SketchUp Equivalence Strategy for voxel-native.pdf` and SketchUp's documented
interaction model. It is intentionally strict: a type name existing in Rust does
not mean the feature behaves like SketchUp yet.

## Reference Behaviors

- SketchUp separates raw picking from inferenced input points. `PickHelper` is
  raw picking, while `InputPoint` adds inference/snapping behavior used by tools.
- SketchUp tools are event-driven state machines: activate, mouse move/down/up,
  wheel, text/measurement input, draw overlay, cursor, cancel/resume.
- Inference must support endpoint, midpoint, on-edge, on-face, axes,
  from-point/reference chaining, tooltip feedback, and lockable inference.

## Current Status

| Area | Status | Current evidence | Not exact yet |
|---|---|---|---|
| Document/entity model | Partial | `SketchDocument`, contexts, entities, materials, tags, styles, scenes, snapshots | No full SketchUp model graph, outliner UX, section scene binding, or import/export semantic parity |
| Selection and hit records | Partial | `SelectionSet`, `HitRecord`, instance paths, semantic hover hit, cell-level `SketchVoxelLinkIndex::hit_for_cell` fallback for drawn strokes, and preferred-cell hover resolution so large faces/strokes use the voxel under the cursor first | No crossing-window selection, hidden/locked filters, nested pick priority, soft/smooth surface selection |
| Raw picking | Started | `PickService` ranks raw `HitRecord`s by distance and hit kind without inference bias | Not yet wired as the universal live picking layer for every tool |
| Screen-space snap | Started | `project_world_to_screen`, `screen_space_inference_candidates`, and `best_screen_space_inference` project candidates through a view-projection matrix and rank by SketchUp-style kind priority plus screen distance/sticky boost | No BVH/octree broadphase yet; no depth-buffer occlusion test; not yet wired to every live tool overlay |
| Inference/InputPoint | Partial | `InferenceService::from_pick` now converts a raw pick into ranked endpoint/midpoint/face/on-edge/axis/from-point candidates; Pencil/Sketch Draw consumes semantic hover under the visible mouse cursor, stores live center-point start/current input points, shows colored markers, and reports explicit alignment text such as endpoint/midpoint/face-center plus axis/reference length | Missing full parallel/perpendicular/intersection solving, cursor-near tooltip parity, ambiguity resolution, and live lock UX everywhere |
| Inference locking | Started | `closest_point_on_locked_axis_from_ray` implements the skew-line projection needed for Shift/arrow axis locks; Pencil now supports Right=X, Left=Z, Up=Y height, Down=clear with visible axis guide | Not yet connected to every editor tool, and Shift pre-lock/reference chaining is still incomplete |
| Rectangle plane orientation | Started | `rectangle_plane_from_view_or_face` chooses locked axis, hovered face normal, or dominant view axis and returns an orthonormal drawing basis | Live Rectangle still needs full screen-space snap and measurement UI wiring |
| Tool controller | Partial | `ToolController` tracks active tool, phase, selection, inference lock, transaction label, and house guide | Not a complete SketchUp `Tool` event interface yet; no typed measurement parser for every tool |
| Components/instances | Partial | `ComponentDefinition`, `ComponentInstance`, transforms, definition snapshots, make-unique support | No production outliner, nested edit context UX, component browser, gluing/cut-opening behavior, or shared material inheritance parity |
| B-Rep / planar kernel | Started | `SketchBRepKernel` can store vertices/edges/faces, split coplanar faces, and push/pull simple face regions | Not yet the live authoring backend for all Pencil/Rectangle/Push/Pull/Opening edits |
| Push/Pull | Partial | Semantic `PushPullExtrusion`, simple B-Rep extrusion, voxel commit routing exists | No robust topology healing, repeat-depth workflow, through-cut coincidence detection, or live semantic face replacement everywhere |
| Move/Transform | Started | `src/sculpt/transform.rs` supports semantic selection voxel move with X/Y/Z arrow locks, Gizmo bounds preview, voxel commit, undo batch, `SketchDocument::move_selection`, and link-index translation | Move still needs grip-point-to-target inference, copy/array mode, typed deltas, crossing selection, and material/metadata-preserving voxel moves |
| UI/toolbox workflow | Partial | Mouse-first rail, compact contextual flyouts, STYLE drawer, no visible F-key workflow; primary rail now follows the core modeling order: Select, Line, Rectangle, Circle, Push/Pull, Move, Rotate, Scale, Material | Hover and cursor behavior still need visual playtesting; voxel-specific workflows such as Opening, Room, House, Road, Bots, and City stay in flyouts but still need deeper per-tool parity |

## New Source Changes In This Slice

- Added `PickService` as an explicit raw picking stage.
- Added `InferenceService::from_pick` as the inferenced `InputPoint`-style stage.
- Added reference-point axis/from-point candidates so the model can support
  SketchUp-like chained drawing from a previous point.
- Added tests proving picking does not let snap/inference bias override raw hit
  distance, and inference is applied only after a raw hit is chosen.
- Added Phase-1 math from the user's system specification: view-projection
  screen-space snapping, sticky candidate priority, skew-line axis locking, and
  dynamic Rectangle drawing planes.
- Wired the first live Pencil/Sketch Draw input-point path: endpoint/midpoint/
  face-center snaps now feed start/end cells, colored start/current markers are
  rendered with Gizmos, and Pencil uses arrow-key axis locks plus 3D line cells
  when drawing vertically or off the original face plane.
- Replaced the old hover-everything drawer behavior with compact contextual
  flyouts: `Draw`, `Edit Selected`, `Openings`, `House Builder`,
  `City Layout`, and `Scene`.
- Added cell-level semantic hit fallback and linked voxel translation so Pencil
  strokes and other non-face edits can be selected and moved instead of only
  perfect face-linked regions.
- Added the first live Move selection path: drag selected semantic voxels,
  preview bounds, axis-lock with arrow keys, and commit both voxel cells and
  `SketchDocument` geometry through shared undo.
- Added clearer Pencil/Rectangle alignment readouts for endpoint, midpoint,
  face-center, same-height axis lines, red X lines, green Z lines, equal-length
  and reference-length snaps.
- Slimmed the primary toolbox rail to the highest-value mouse-first tools,
  moving duplicate/advanced choices into hover context groups.
- Reordered the primary editor rail around SketchUp-style core modeling:
  Select, Line, Rectangle, Circle, Push/Pull, Move, Rotate, Scale, and Material.
  Opening, Room, House, Roads, Bots, city shell, landscape, skyline, and
  spacecraft remain available through hover flyouts/full drawers instead of
  competing with the first building gestures.
- Stabilized the hover flyout by retaining the last hovered group through the
  grace window, widening the invisible bridge from toolbox to flyout, and
  replacing terse labels such as `Mat`, `Box`, `Pull`, and `Window` with clearer
  `Paint`, `Rect`, `Push/Pull`, and `Opening`.
- Fixed a major cursor/alignment mismatch in the live editor path: when the
  cursor is visible/unlocked, Draw/Select-style semantic hover now uses the
  actual pointer ray instead of the camera crosshair ray.
- Fixed semantic hover on larger drawn regions to prefer the hovered voxel cell
  before falling back to a generic face/stroke hit. This prevents the blue
  preview/selection box from jumping to the first stored cell on the same
  semantic object.
- Updated Pencil semantic line records to store the same visible center points
  used by gizmo markers, avoiding the half-voxel offset between drawn strokes,
  later endpoint/midpoint inference, and selection/move previews.
- Added unit coverage for nearest endpoint preference, midpoint preference,
  semantic axis-lock projection, visible-pointer semantic hover, and preferred
  cell hover resolution.

## Next Highest-Value Work

1. Replace the remaining voxel-first hover helpers with `PickService`,
   screen-space snap, and `InferenceService::from_pick` in Push/Pull, Opening,
   Move, Scale, Rotate, and every advanced draw shape. Pencil/Rectangle now
   consume the first semantic hover path but are still not full SketchUp tools.
2. Add an overlay painter for green/cyan/red/blue inference points, edge highlights, axis locks,
   and tooltip text near the cursor.
3. Convert Pencil/Rectangle/Opening from direct voxel-first commits to semantic
   B-Rep/face edits first, then voxelize changed regions.
4. Add typed measurement parsing and a small measurements/status box for every
   drafting/transform tool.
5. Add selection window/crossing selection and component edit context before
   expanding more advanced tools.
6. Upgrade Move from screen-delta movement to true grip-point-to-inferenced
   target movement, including typed deltas, copy/array, and material-preserving
   voxel relocation.
