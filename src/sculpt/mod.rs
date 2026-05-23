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

pub use raycast::dda_voxel;
pub use state::SculptState;

pub struct SculptPlugin;

impl Plugin for SculptPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SculptState>()
            .init_resource::<draw::RectDrawState>()
            .init_resource::<smart::SmartTowerState>()
            .init_resource::<pushpull::PushPullDrag>()
            .init_resource::<pushpull::PushPullReference>()
            .init_resource::<pushpull::HoverFace>()
            // Hover → face resolve → input → preview update → gizmo.
            // Order matters: drag-end must run AFTER update_drag so the
            // last applied preview is visible in `world.voxel_at` when
            // we snapshot `after` values.
            .add_systems(
                Update,
                (
                    pushpull::update_hover,
                    pushpull::resolve_hover_face,
                    pushpull::reference_input,
                    pushpull::begin_drag,
                    pushpull::update_drag,
                    pushpull::end_drag,
                    pushpull::universal_undo_input,
                    pushpull::draw_face_gizmo,
                    pushpull::draw_reference_gizmo,
                    draw::rect_draw_input,
                    draw::draw_rect_gizmo,
                    smart::smart_tower_input,
                    smart::smart_tower_gizmo,
                )
                    .chain(),
            );
    }
}
