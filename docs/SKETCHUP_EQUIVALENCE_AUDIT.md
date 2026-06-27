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
| Selection and hit records | Partial | `SelectionSet`, `HitRecord`, instance paths, semantic hover hit | No crossing-window selection, hidden/locked filters, nested pick priority, soft/smooth surface selection |
| Raw picking | Started | `PickService` ranks raw `HitRecord`s by distance and hit kind without inference bias | Not yet wired as the universal live picking layer for every tool |
| Screen-space snap | Started | `project_world_to_screen`, `screen_space_inference_candidates`, and `best_screen_space_inference` project candidates through a view-projection matrix and rank by SketchUp-style kind priority plus screen distance/sticky boost | No BVH/octree broadphase yet; no depth-buffer occlusion test; not yet wired to every live tool overlay |
| Inference/InputPoint | Partial | `InferenceService::from_pick` now converts a raw pick into ranked endpoint/midpoint/face/on-edge/axis/from-point candidates; Pencil/Sketch Draw stores live start/current input points and shows colored markers | Missing full parallel/perpendicular/intersection solving, visual tooltip parity, ambiguity resolution, and live lock UX everywhere |
| Inference locking | Started | `closest_point_on_locked_axis_from_ray` implements the skew-line projection needed for Shift/arrow axis locks; Pencil now supports Right=X, Left=Z, Up=Y height, Down=clear with visible axis guide | Not yet connected to every editor tool, and Shift pre-lock/reference chaining is still incomplete |
| Rectangle plane orientation | Started | `rectangle_plane_from_view_or_face` chooses locked axis, hovered face normal, or dominant view axis and returns an orthonormal drawing basis | Live Rectangle still needs full screen-space snap and measurement UI wiring |
| Tool controller | Partial | `ToolController` tracks active tool, phase, selection, inference lock, transaction label, and house guide | Not a complete SketchUp `Tool` event interface yet; no typed measurement parser for every tool |
| Components/instances | Partial | `ComponentDefinition`, `ComponentInstance`, transforms, definition snapshots, make-unique support | No production outliner, nested edit context UX, component browser, gluing/cut-opening behavior, or shared material inheritance parity |
| B-Rep / planar kernel | Started | `SketchBRepKernel` can store vertices/edges/faces, split coplanar faces, and push/pull simple face regions | Not yet the live authoring backend for all Pencil/Rectangle/Push/Pull/Opening edits |
| Push/Pull | Partial | Semantic `PushPullExtrusion`, simple B-Rep extrusion, voxel commit routing exists | No robust topology healing, repeat-depth workflow, through-cut coincidence detection, or live semantic face replacement everywhere |
| UI/toolbox workflow | Partial | Mouse-first rail, compact contextual flyouts, STYLE drawer, no visible F-key workflow | Hover and cursor behavior still need visual playtesting; tools need real per-tool capabilities instead of repeated cards |

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

## Next Highest-Value Work

1. Replace the remaining voxel-first hover helpers with `PickService`,
   screen-space snap, and `InferenceService::from_pick` in Rectangle,
   Push/Pull, Opening, Move, Scale, and Rotate.
2. Add an overlay painter for green/cyan/red/blue inference points, edge highlights, axis locks,
   and tooltip text near the cursor.
3. Convert Pencil/Rectangle/Opening from direct voxel-first commits to semantic
   B-Rep/face edits first, then voxelize changed regions.
4. Add typed measurement parsing and a small measurements/status box for every
   drafting/transform tool.
5. Add selection window/crossing selection and component edit context before
   expanding more advanced tools.
