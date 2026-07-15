//! Direct-Manipulation voxel sculpting layer.
//!
//! SketchUp-style in-world editing: face-aware Push/Pull, Transform Gizmos
//! (translate / rotate-snap-90 / integer-scale), CSG slicing with
//! duplicate-and-drag, and a fluid paint-to-build 3D brush.
//!
//! Design rules:
//!   * Zero chat commands. All interaction is mouse + camera + modifier keys.
//!   * Minimal HUD: in-world gizmos via `Gizmos` immediate API; only a tiny
//!     status corner readout (extrusion delta, brush radius, snap mode).
//!   * Reuses existing chunk COW storage, greedy mesher, and
//!     [`crate::builder::BuilderHistory`] for undo/redo. We do **not** fork
//!     the chunk format or add per-voxel orientation metadata — rotations
//!     snap to 90° at commit, so all outputs land cleanly on the lattice.
//!
//! Phase status:
//!   * Phase 0 (this file): foundation — module skeleton, shared raycaster,
//!     core resource types, toolbelt entry, Classic-builder fallback toggle.
//!   * Phase 1: Push/Pull face extrusion (`face.rs`).
//!   * Phase 2: Transform Gizmo (`gizmo.rs`, `transform.rs`).
//!   * Phase 3: CSG Slice & Duplicate (`csg.rs`).
//!   * Phase 4: Paint-to-Build 3D brush (`paint.rs`, `radial.rs`).

use bevy::prelude::*;

pub mod draw;
pub mod face;
pub mod pushpull;
pub mod raycast;
pub mod smart;
pub mod state;
pub mod transform;

pub use raycast::dda_voxel;
pub use state::SculptState;

pub struct SculptPlugin;

impl Plugin for SculptPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SculptState>()
            .init_resource::<draw::RectDrawState>()
            .init_resource::<draw::SketchEditorPointerMarker>()
            .init_resource::<draw::SketchEditorScreenCursor>()
            .init_resource::<smart::SmartTowerState>()
            .init_resource::<pushpull::PushPullDrag>()
            .init_resource::<pushpull::PushPullReference>()
            .init_resource::<pushpull::HoverFace>()
            .init_resource::<transform::SemanticMoveDrag>()
            .init_resource::<transform::SemanticRotateDrag>()
            .init_resource::<transform::SemanticScaleDrag>()
            // Hover → face resolve → input → preview update → gizmo.
            // Order matters: drag-end must run AFTER update_drag so the
            // last applied preview is visible in `world.voxel_at` when
            // we snapshot `after` values.
            .add_systems(
                Update,
                (
                    pushpull::update_hover,
                    pushpull::resolve_hover_face,
                    pushpull::semantic_select_input,
                    transform::begin_move_drag,
                    transform::update_move_drag,
                    transform::end_move_drag,
                    transform::begin_rotate_drag,
                    transform::update_rotate_drag,
                    transform::end_rotate_drag,
                    transform::begin_scale_drag,
                    transform::update_scale_drag,
                    transform::end_scale_drag,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    pushpull::reference_input,
                    pushpull::begin_drag,
                    pushpull::update_drag,
                    pushpull::end_drag,
                    pushpull::universal_undo_input,
                    pushpull::draw_face_gizmo,
                    pushpull::draw_reference_gizmo,
                    transform::draw_move_gizmo,
                    transform::draw_rotate_gizmo,
                    transform::draw_scale_gizmo,
                    draw::rect_draw_input,
                    draw::draw_rect_gizmo,
                    smart::smart_tower_input,
                    smart::smart_tower_gizmo,
                )
                    .chain()
                    .after(transform::end_scale_drag),
            )
            .add_systems(
                Update,
                draw::refresh_editor_pointer_marker
                    .after(draw::rect_draw_input)
                    .before(draw::draw_rect_gizmo),
            )
            .add_systems(
                Update,
                draw::draw_editor_pointer_marker.after(draw::draw_rect_gizmo),
            )
            .add_systems(
                Update,
                draw::refresh_editor_screen_cursor.after(draw::refresh_editor_pointer_marker),
            )
            .add_systems(
                Update,
                draw::draw_editor_screen_cursor
                    .after(draw::refresh_editor_screen_cursor)
                    .after(draw::draw_editor_pointer_marker),
            );
    }
}
