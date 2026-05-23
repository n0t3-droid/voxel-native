//! Selection + single-key C/V clipboard + mirror planes.
//!
//! This is the "modelling studio" backbone added on top of
//! [`crate::builder`]. It turns the editor into something that *feels*
//! like MagicaVoxel / Blender without any dialog-box plumbing:
//!
//! Workflow
//! --------
//! * Press **`B`** while looking at a voxel → first marquee corner A is
//!   captured. The next press of `B` captures corner B and the box is
//!   locked in. Third `B` starts over.
//! * Press **`C`** → the selected AABB is copied into
//!   [`crate::builder::BuilderClipboard`] (non-selection air included,
//!   just like MagicaVoxel's Cut).
//! * Press **`V`** → the clipboard becomes a live *ghost* that follows
//!   the crosshair. Mouse-wheel rotates it 90° around Y. `LMB` or
//!   `Enter` commits the paste. Holding `Shift` at commit keeps the
//!   ghost up for stamp mode — every subsequent click drops another
//!   copy until `Esc` cancels.
//! * Press **`M`** / **`Shift+M`** / **`Alt+M`** → toggle the X / Y / Z
//!   mirror planes. While armed, every place / remove / paste is
//!   duplicated across the planes through the builder pipeline.
//! * Press **`Esc`** → cancels the active ghost, clears the picking
//!   cursor, but keeps the box around so the user can re-`C` it.
//!
//! Design notes
//! ------------
//! * All gameplay input is gated on `EditorState::open` so the F3 panel
//!   behaves as a modal — inside the editor these hotkeys win, outside
//!   nothing changes.
//! * The DDA raycast is duplicated from [`crate::weapons`] (see the
//!   animation module for the same pattern) — we keep modules decoupled
//!   and the function is ~30 lines.
//! * Selection / ghost rendering uses Bevy 0.14 `Gizmos` immediate API:
//!   no meshes allocated, no shader work, one batched line-list per
//!   frame.
//! * The phosphor colour + pulse animation picks up the live
//!   [`crate::theme::ThemeSettings`] variant so the look stays
//!   consistent with the hacker theme.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;

use crate::blocks::voxel_is_solid;
use crate::builder::{BuildAction, BuilderClipboard, BuilderState};
use crate::editor::EditorState;
use crate::player::Player;
use crate::settings::WorldSettings;
use crate::world::VoxelWorld;

// ---------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------

/// Marquee selection state machine.
///
/// Three logical states packed into the two `Option`s:
///   * `a = None, b = None` — nothing selected.
///   * `a = Some, b = None` — first corner picked, awaiting second.
///   * `a = Some, b = Some` — a valid AABB is selected.
#[derive(Resource, Default, Debug, Clone)]
pub struct SelectionState {
    pub a: Option<IVec3>,
    pub b: Option<IVec3>,
    /// `true` after `V`: a paste ghost is following the crosshair and
    /// awaits either a click / Enter (commit) or Esc (cancel).
    pub ghosting: bool,
    /// Ghost position in world cells (origin of the clipboard AABB).
    /// Updated every frame from the crosshair ray.
    pub ghost_origin: IVec3,
    /// Stamp mode: after a commit, if Shift was held the ghost stays
    /// and every subsequent click drops another copy. Counter shown
    /// in the editor status bar.
    pub stamp: bool,
    pub stamp_count: u32,
}

impl SelectionState {
    /// Return the inclusive AABB `[min, max]` if both corners are set.
    pub fn aabb(&self) -> Option<(IVec3, IVec3)> {
        let (a, b) = (self.a?, self.b?);
        let lo = IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
        let hi = IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z));
        Some((lo, hi))
    }

    pub fn clear(&mut self) {
        self.a = None;
        self.b = None;
    }
}

/// Mirror planes — when any axis is armed, every edit queued by
/// [`crate::builder::apply_build_actions`] is duplicated across the
/// plane. The origin defaults to (0,0,0) but follows the selection
/// center whenever a full AABB is selected.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct MirrorState {
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub origin: IVec3,
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

pub struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SelectionState::default())
            .insert_resource(MirrorState::default())
            .add_systems(Update, (selection_input, draw_selection_gizmos));
    }
}

// ---------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn selection_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: EventReader<MouseWheel>,
    editor: Res<EditorState>,
    mut sel: ResMut<SelectionState>,
    mut mirror: ResMut<MirrorState>,
    mut builder: ResMut<BuilderState>,
    clipboard: Res<BuilderClipboard>,
    world: Res<VoxelWorld>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
) {
    // Only active while the F3 editor is open AND the user has opted
    // into the legacy precision-cuboid builder — otherwise these keys
    // stay free for the new direct-manipulation system and for gameplay.
    if !editor.open || !editor.show_classic_builder {
        // Still drain the wheel reader so it doesn't fill up.
        wheel.clear();
        return;
    }

    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);

    let Ok(cam_tf) = cam_q.get_single() else {
        wheel.clear();
        return;
    };

    if ctrl && !alt && keys.just_pressed(KeyCode::KeyZ) {
        if shift {
            builder.pending.push(BuildAction::Redo);
        } else {
            builder.pending.push(BuildAction::Undo);
        }
        wheel.clear();
        return;
    }
    if ctrl && !shift && !alt && keys.just_pressed(KeyCode::KeyY) {
        builder.pending.push(BuildAction::Redo);
        wheel.clear();
        return;
    }

    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    // Two flavours of "looked-at cell":
    //   * `hit` — the solid cell the ray entered (used for selection).
    //   * `adj` — the empty cell just before `hit` (used for paste
    //     so the clipboard lands on top of surfaces, not inside them).
    // When the ray misses everything, fall back to 8 cells in front.
    let picked = raycast_voxel(&world, origin, dir, 80.0).unwrap_or_else(|| {
        let fwd = origin + dir * 8.0;
        let c = IVec3::new(
            fwd.x.floor() as i32,
            fwd.y.floor() as i32,
            fwd.z.floor() as i32,
        );
        (c, c)
    });
    let (hit, adj) = picked;

    // --- Selection: B -------------------------------------------------
    if !ctrl && !shift && !alt && keys.just_pressed(KeyCode::KeyB) {
        match (sel.a.is_some(), sel.b.is_some()) {
            (false, _) => {
                sel.a = Some(hit);
                sel.b = None;
                builder.status = format!(
                    "Auswahl A @ {},{},{} — B-Taste fuer zweite Ecke.",
                    hit.x, hit.y, hit.z
                );
            }
            (true, false) => {
                sel.b = Some(hit);
                if let Some((lo, hi)) = sel.aabb() {
                    // Mirror builder.a/b so the BAUEN tab shows the box.
                    builder.a = lo;
                    builder.b = hi;
                    // Move mirror origin to the selection center so
                    // mirrored edits feel natural ("mirror around what
                    // I just selected").
                    mirror.origin = (lo + hi) / 2;
                    let size = hi - lo + IVec3::ONE;
                    builder.status = format!(
                        "Auswahl: {}x{}x{} — C=Kopieren, V=Einfuegen-Geist.",
                        size.x, size.y, size.z
                    );
                }
            }
            _ => {
                // Already had a full box — restart.
                sel.a = Some(hit);
                sel.b = None;
                builder.status = format!("Neue Auswahl A @ {},{},{}.", hit.x, hit.y, hit.z);
            }
        }
    }

    // --- Copy: C ------------------------------------------------------
    if !ctrl && !shift && !alt && keys.just_pressed(KeyCode::KeyC) {
        if let Some((lo, hi)) = sel.aabb() {
            builder.a = lo;
            builder.b = hi;
            builder.pending.push(BuildAction::Copy);
        } else {
            builder.status = "Nichts ausgewaehlt. B + B setzt eine Box.".into();
        }
    }
    // --- Cut: Ctrl+X -------------------------------------------------
    if ctrl && !shift && !alt && keys.just_pressed(KeyCode::KeyX) {
        if let Some((lo, hi)) = sel.aabb() {
            builder.a = lo;
            builder.b = hi;
            builder.pending.push(BuildAction::Copy);
            builder.pending.push(BuildAction::ClearBox);
        }
    }

    // --- Paste-ghost: V ----------------------------------------------
    if !ctrl && !shift && !alt && keys.just_pressed(KeyCode::KeyV) {
        if clipboard.is_empty() {
            builder.status = "Clipboard leer. Erst C druecken.".into();
        } else {
            sel.ghosting = true;
            sel.ghost_origin = adj;
            sel.stamp = false;
            sel.stamp_count = 0;
            builder.status = "Geist aktiv — Klick/Enter plaziert, Shift-Klick stempelt.".into();
        }
    }

    // --- Ghost manipulation ------------------------------------------
    if sel.ghosting {
        // Follow the crosshair every frame.
        sel.ghost_origin = adj;

        // Wheel → rotate clipboard 90° around Y.
        let wheel_ticks: f32 = wheel.read().map(|w| w.y).sum();
        if wheel_ticks.abs() > 0.01 {
            builder.pending.push(BuildAction::RotateClipboardY);
        }

        if keys.just_pressed(KeyCode::KeyR) {
            builder.pending.push(BuildAction::RotateClipboardY);
        }
        if keys.just_pressed(KeyCode::KeyX) && !ctrl {
            builder.pending.push(BuildAction::FlipClipboardX);
        }
        if keys.just_pressed(KeyCode::KeyY) {
            builder.pending.push(BuildAction::FlipClipboardY);
        }
        if keys.just_pressed(KeyCode::KeyZ) && !ctrl {
            builder.pending.push(BuildAction::FlipClipboardZ);
        }

        // Commit: Enter or LMB. Shift-held = stay in stamp mode.
        let commit = keys.just_pressed(KeyCode::Enter) || mouse.just_pressed(MouseButton::Left);
        if commit {
            builder.paste_origin = sel.ghost_origin;
            builder.pending.push(BuildAction::Paste);
            sel.stamp_count += 1;
            if shift {
                sel.stamp = true;
                builder.status = format!(
                    "Stempel {} — Shift+Klick = weiter, Esc = beenden.",
                    sel.stamp_count
                );
            } else {
                sel.ghosting = false;
                sel.stamp = false;
                builder.status = format!(
                    "Eingefuegt @ {},{},{}.",
                    sel.ghost_origin.x, sel.ghost_origin.y, sel.ghost_origin.z
                );
            }
        }

        // Cancel.
        if keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right) {
            sel.ghosting = false;
            sel.stamp = false;
            builder.status = "Geist abgebrochen.".into();
        }
    } else {
        // Drain wheel events outside ghost mode so they don't pile up
        // and cause a rotation when the ghost finally re-opens.
        wheel.clear();

        // Esc outside ghost mode clears the marquee but keeps the
        // clipboard intact — users can re-paste later.
        if keys.just_pressed(KeyCode::Escape) {
            if sel.a.is_some() || sel.b.is_some() {
                sel.clear();
                builder.status = "Auswahl geleert.".into();
            }
        }
    }

    // --- Mirror toggles: M / Shift+M / Alt+M -------------------------
    if keys.just_pressed(KeyCode::KeyM) {
        if alt {
            mirror.z = !mirror.z;
        } else if shift {
            mirror.y = !mirror.y;
        } else {
            mirror.x = !mirror.x;
        }
        builder.status = format!(
            "Spiegel X={} Y={} Z={}  (Origin {},{},{})",
            on_off(mirror.x),
            on_off(mirror.y),
            on_off(mirror.z),
            mirror.origin.x,
            mirror.origin.y,
            mirror.origin.z
        );
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "AN"
    } else {
        "AUS"
    }
}

// ---------------------------------------------------------------------
// Gizmo rendering (3D visualization)
// ---------------------------------------------------------------------

fn draw_selection_gizmos(
    time: Res<Time>,
    editor: Res<EditorState>,
    sel: Res<SelectionState>,
    mirror: Res<MirrorState>,
    clipboard: Res<BuilderClipboard>,
    settings: Res<WorldSettings>,
    mut gizmos: Gizmos,
) {
    if !editor.open || !editor.show_classic_builder {
        return;
    }

    // Phosphor primary colour pulses at 2 Hz between 50%..100% so the
    // eye is drawn to live selections without being irritating.
    let phase = (time.elapsed_seconds() * 2.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let pulse = 0.5 + 0.5 * phase;

    let p = settings.theme.color.primary();
    let primary = Color::srgba(
        p.r() as f32 / 255.0,
        p.g() as f32 / 255.0,
        p.b() as f32 / 255.0,
        1.0,
    );
    let pulsed = Color::srgba(
        p.r() as f32 / 255.0 * pulse,
        p.g() as f32 / 255.0 * pulse,
        p.b() as f32 / 255.0 * pulse,
        1.0,
    );

    // --- First-corner pin ---
    if let Some(a) = sel.a {
        let center = a.as_vec3() + Vec3::splat(0.5);
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(Vec3::splat(1.02)),
            primary,
        );
    }

    // --- Full AABB ---
    if let Some((lo, hi)) = sel.aabb() {
        let center = ((lo + hi).as_vec3() + Vec3::splat(1.0)) * 0.5;
        let size = (hi - lo).as_vec3() + Vec3::splat(1.0);
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(size + Vec3::splat(0.04)),
            pulsed,
        );
    }

    // --- Paste ghost ---
    if sel.ghosting && !clipboard.is_empty() {
        let lo = sel.ghost_origin.as_vec3();
        let size = clipboard.size.as_vec3();
        let center = lo + size * 0.5;
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(size + Vec3::splat(0.04)),
            primary,
        );
        // Axis cross at the ghost origin so it's visible even if the
        // cuboid is partially clipped by terrain.
        let o = lo + Vec3::new(0.5, 0.5, 0.5);
        let axis_len = 2.0_f32;
        gizmos.line(o, o + Vec3::X * axis_len, Color::srgb(1.0, 0.2, 0.2));
        gizmos.line(o, o + Vec3::Y * axis_len, Color::srgb(0.2, 1.0, 0.2));
        gizmos.line(o, o + Vec3::Z * axis_len, Color::srgb(0.2, 0.4, 1.0));
    }

    // --- Mirror planes ---
    // Drawn as translucent "plane" hinted by a pair of crossed axis
    // lines + a hollow square. Cheap (<20 line segments) and enough
    // to see where the plane lives.
    let mo = mirror.origin.as_vec3() + Vec3::splat(0.5);
    let pl_size: f32 = 32.0;
    let plane_col = Color::srgba(1.0, 1.0, 1.0, 0.5);
    if mirror.x {
        let q = [
            mo + Vec3::new(0.0, -pl_size, -pl_size),
            mo + Vec3::new(0.0, pl_size, -pl_size),
            mo + Vec3::new(0.0, pl_size, pl_size),
            mo + Vec3::new(0.0, -pl_size, pl_size),
        ];
        gizmos.linestrip([q[0], q[1], q[2], q[3], q[0]], plane_col);
        gizmos.line(q[0], q[2], plane_col);
        gizmos.line(q[1], q[3], plane_col);
    }
    if mirror.y {
        let q = [
            mo + Vec3::new(-pl_size, 0.0, -pl_size),
            mo + Vec3::new(pl_size, 0.0, -pl_size),
            mo + Vec3::new(pl_size, 0.0, pl_size),
            mo + Vec3::new(-pl_size, 0.0, pl_size),
        ];
        gizmos.linestrip([q[0], q[1], q[2], q[3], q[0]], plane_col);
        gizmos.line(q[0], q[2], plane_col);
        gizmos.line(q[1], q[3], plane_col);
    }
    if mirror.z {
        let q = [
            mo + Vec3::new(-pl_size, -pl_size, 0.0),
            mo + Vec3::new(pl_size, -pl_size, 0.0),
            mo + Vec3::new(pl_size, pl_size, 0.0),
            mo + Vec3::new(-pl_size, pl_size, 0.0),
        ];
        gizmos.linestrip([q[0], q[1], q[2], q[3], q[0]], plane_col);
        gizmos.line(q[0], q[2], plane_col);
        gizmos.line(q[1], q[3], plane_col);
    }
}

// ---------------------------------------------------------------------
// DDA voxel raycast (Amanatides-Woo)
// ---------------------------------------------------------------------
//
// Returns `(hit, adjacent_empty)` where `hit` is the first solid cell
// the ray enters and `adjacent_empty` is the cell *just before* that
// entry — useful as a paste anchor so the clipboard lands on top of a
// surface, not inside it.
//
// Duplicated from [`crate::weapons::dda_voxel`] (the animation module
// does the same) to keep module boundaries clean.

fn raycast_voxel(
    world: &VoxelWorld,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<(IVec3, IVec3)> {
    if dir.length_squared() < 1e-6 {
        return None;
    }
    let mut x = origin.x.floor() as i32;
    let mut y = origin.y.floor() as i32;
    let mut z = origin.z.floor() as i32;
    let step_x = dir.x.signum() as i32;
    let step_y = dir.y.signum() as i32;
    let step_z = dir.z.signum() as i32;
    let t_delta_x = if dir.x != 0.0 {
        (1.0 / dir.x).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_y = if dir.y != 0.0 {
        (1.0 / dir.y).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if dir.z != 0.0 {
        (1.0 / dir.z).abs()
    } else {
        f32::INFINITY
    };
    let nb = |p: f32, s: i32| -> f32 {
        if s > 0 {
            p.floor() + 1.0 - p
        } else if s < 0 {
            p - p.floor()
        } else {
            f32::INFINITY
        }
    };
    let mut tmx = nb(origin.x, step_x) * t_delta_x;
    let mut tmy = nb(origin.y, step_y) * t_delta_y;
    let mut tmz = nb(origin.z, step_z) * t_delta_z;
    // `prev` is assigned just before each step, which runs at least
    // once before any solid-hit return — no init needed.
    let mut prev: IVec3;
    for _ in 0..4_096 {
        let t = tmx.min(tmy).min(tmz);
        if t > max_dist {
            return None;
        }
        prev = IVec3::new(x, y, z);
        if tmx <= tmy && tmx <= tmz {
            x += step_x;
            tmx += t_delta_x;
        } else if tmy <= tmz {
            y += step_y;
            tmy += t_delta_y;
        } else {
            z += step_z;
            tmz += t_delta_z;
        }
        if voxel_is_solid(world.voxel_at(x, y, z)) {
            return Some((IVec3::new(x, y, z), prev));
        }
    }
    None
}
