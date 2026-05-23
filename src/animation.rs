//! Animation Studio — CodeWalker-style in-game tooling for capturing
//! voxel structures from the world, animating them on a timeline, and
//! either looping them forever as kinetic decoration or baking the new
//! pose back into the voxel grid.
//!
//! Workflow (also documented in the editor's ANIM tab):
//!
//! 1. Press **F4** (or tick *Picker aktiv* in the editor) to enter
//!    picker mode. The reticle now highlights the voxel under your
//!    crosshair via a neon gizmo box.
//! 2. **Left-click** to add the highlighted block to the selection.
//!    **Right-click** to remove. The whole selection is outlined.
//! 3. Click *Erfassen* in the editor to snapshot the selection into a
//!    new clip. The source voxels are LIFTED out of the world (set to
//!    air), and a visual replica spawns at their original position so
//!    the world keeps a clean hole while the animation plays.
//! 4. Add keyframes (offset, yaw, scale, time). The clip plays in an
//!    infinite loop by default, so it acts as living decoration —
//!    rotating gates, hovering platforms, flapping wings, drifting
//!    debris, anything you can stage from voxels.
//! 5. *Bake* writes the clip's blocks back into the world at the
//!    currently displayed pose (snapped to the voxel grid). *Restore*
//!    puts them back at the original anchor. *Verwerfen* deletes the
//!    clip and discards the captured volume.
//!
//! All state lives in [`AnimationStudio`] and is reset between runs;
//! it does NOT persist to RON yet (the captured voxel volumes can grow
//! arbitrarily large and we don't want to bloat the save). A future
//! pass can serialize clips into `./animations/*.ron` prefab files.

use bevy::prelude::*;

use crate::blocks::{voxel_is_solid, BlockType, Voxel, AIR};
use crate::menu::GameState;
use crate::player::Player;
use crate::world::VoxelWorld;

// ---------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------

/// Easing / interpolation mode applied on the outgoing side of a
/// keyframe. The curve is evaluated between key `i` (this one) and key
/// `i+1`. All presets map a normalized `t ∈ [0,1]` through a Penner-
/// style formula, then the engine lerps the channels with that eased
/// `t`. Non-linear easings only affect the **shape** of the motion,
/// never the endpoints — the clip still passes through every key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interp {
    #[default]
    Linear,
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    Bounce,
    Elastic,
    Back,
    Step,
}

impl Interp {
    pub fn label(self) -> &'static str {
        match self {
            Interp::Linear => "Linear",
            Interp::Hold => "Hold",
            Interp::EaseIn => "EaseIn",
            Interp::EaseOut => "EaseOut",
            Interp::EaseInOut => "EaseInOut",
            Interp::Bounce => "Bounce",
            Interp::Elastic => "Elastic",
            Interp::Back => "Back",
            Interp::Step => "Step",
        }
    }

    pub fn all() -> [Interp; 9] {
        [
            Interp::Linear,
            Interp::Hold,
            Interp::EaseIn,
            Interp::EaseOut,
            Interp::EaseInOut,
            Interp::Bounce,
            Interp::Elastic,
            Interp::Back,
            Interp::Step,
        ]
    }

    /// Map `t ∈ [0,1]` → eased `t ∈ [0,1]` (but some presets overshoot
    /// slightly, which is desirable for Back / Elastic snap). Source:
    /// standard Robert-Penner easing equations.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Interp::Linear => t,
            // Hold clamps to 0 until the very end, producing a discrete
            // step to the next key — useful for "snap to pose" frames.
            Interp::Hold => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Interp::EaseIn => t * t,
            Interp::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Interp::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - (u * u) * 0.5
                }
            }
            Interp::Bounce => {
                // easeOutBounce — classic Penner.
                let n1 = 7.5625;
                let d1 = 2.75;
                if t < 1.0 / d1 {
                    n1 * t * t
                } else if t < 2.0 / d1 {
                    let t = t - 1.5 / d1;
                    n1 * t * t + 0.75
                } else if t < 2.5 / d1 {
                    let t = t - 2.25 / d1;
                    n1 * t * t + 0.9375
                } else {
                    let t = t - 2.625 / d1;
                    n1 * t * t + 0.984375
                }
            }
            Interp::Elastic => {
                // easeOutElastic — damped-sine overshoot.
                if t <= 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else {
                    let c4 = std::f32::consts::TAU / 3.0;
                    (2.0_f32).powf(-10.0 * t) * ((t * 10.0 - 0.75) * c4).sin() + 1.0
                }
            }
            Interp::Back => {
                // easeOutBack — overshoots slightly, then settles.
                let c1 = 1.70158_f32;
                let c3 = c1 + 1.0;
                let u = t - 1.0;
                1.0 + c3 * u * u * u + c1 * u * u
            }
            Interp::Step => {
                if t < 0.5 {
                    0.0
                } else {
                    1.0
                }
            }
        }
    }
}

/// Single keyframe on a clip's timeline. The outgoing easing mode is
/// stored here so the interpolator between key `i` and key `i+1` can
/// pick the right curve without the UI having to maintain a parallel
/// array. Rotation is yaw-only on purpose: voxel structures snap best
/// when spun around the world's vertical axis, and yaw alone covers
/// >90% of "kinetic decoration" use cases.
#[derive(Debug, Clone, Copy)]
pub struct KeyFrame {
    /// Time in seconds from the start of the loop.
    pub time: f32,
    /// World-space translation offset added to the clip's anchor.
    pub offset: Vec3,
    /// Yaw rotation around the world Y axis, in degrees.
    pub yaw_deg: f32,
    /// Uniform scale multiplier (1.0 = original size).
    pub scale: f32,
    /// Easing applied between this key and the next.
    pub interp: Interp,
}

impl KeyFrame {
    pub fn identity(time: f32) -> Self {
        Self {
            time,
            offset: Vec3::ZERO,
            yaw_deg: 0.0,
            scale: 1.0,
            interp: Interp::Linear,
        }
    }
}

/// One captured clip — a frozen voxel volume plus a looping timeline.
#[derive(Debug, Clone)]
pub struct AnimClip {
    pub name: String,
    /// Captured voxels stored as offsets from `anchor`. Empty voxels
    /// are filtered out at capture time so we never animate "holes".
    pub blocks: Vec<(IVec3, Voxel)>,
    /// World-space anchor (the min corner of the captured AABB). All
    /// keyframe offsets are relative to this; restoring the clip puts
    /// the blocks back at their original world position.
    pub anchor: IVec3,
    pub keys: Vec<KeyFrame>,
    /// Playback head in seconds. Wraps when `looping` is true.
    pub t: f32,
    pub playing: bool,
    pub looping: bool,
    /// Playback rate multiplier (1.0 = realtime).
    pub speed: f32,
    /// Spawned visual root entity, if currently materialised.
    /// `None` while the clip is fully baked (no visual).
    pub root: Option<Entity>,
}

impl AnimClip {
    fn duration(&self) -> f32 {
        self.keys
            .iter()
            .map(|k| k.time)
            .fold(0.0, f32::max)
            .max(0.001)
    }

    /// Sample the timeline at the current playhead, returning
    /// (offset, yaw_deg, scale).
    fn sample(&self) -> (Vec3, f32, f32) {
        if self.keys.is_empty() {
            return (Vec3::ZERO, 0.0, 1.0);
        }
        if self.keys.len() == 1 {
            let k = self.keys[0];
            return (k.offset, k.yaw_deg, k.scale);
        }
        // Find the bracketing pair. Keys are kept sorted on insert.
        let dur = self.duration();
        let t = if self.looping {
            self.t.rem_euclid(dur)
        } else {
            self.t.clamp(0.0, dur)
        };
        for w in self.keys.windows(2) {
            let a = w[0];
            let b = w[1];
            if t >= a.time && t <= b.time {
                let span = (b.time - a.time).max(1e-4);
                let raw = ((t - a.time) / span).clamp(0.0, 1.0);
                let f = a.interp.apply(raw);
                return (
                    a.offset.lerp(b.offset, f),
                    a.yaw_deg + (b.yaw_deg - a.yaw_deg) * f,
                    a.scale + (b.scale - a.scale) * f,
                );
            }
        }
        let last = *self.keys.last().unwrap();
        (last.offset, last.yaw_deg, last.scale)
    }
}

/// Marker for the visual root spawned per [`AnimClip`]. The `clip`
/// field stores the original clip-list index at spawn time; the studio
/// re-binds entities to clips by `Entity` identity in `drive_animation`,
/// so the index here is informational only and survives clip removal
/// without becoming a footgun.
#[derive(Component)]
pub struct AnimRoot {
    #[allow(dead_code)]
    pub clip: usize,
}

/// Marker for the per-block child meshes under an [`AnimRoot`].
#[derive(Component)]
pub struct AnimChild;

/// Central studio resource — selection state, clip list, and pending
/// commands the editor UI dispatches via boolean flags. Using simple
/// flags keeps the UI free of `Commands` / `World` access and lets all
/// world mutation happen in one ordered system.
#[derive(Resource, Default)]
pub struct AnimationStudio {
    /// When true, the picker raycast is active and left/right click
    /// add/remove the highlighted voxel from `selection`.
    pub picking: bool,
    /// Voxels currently flagged for capture (world-space coords).
    pub selection: Vec<IVec3>,
    /// All captured clips. The active one (if any) is the editing
    /// target for keyframe edits.
    pub clips: Vec<AnimClip>,
    pub active: Option<usize>,
    /// Editor-side request: turn the current `selection` into a clip.
    pub pending_capture: bool,
    /// Editor-side request: bake clip[i] back into the world at the
    /// currently sampled pose (rounded to integer voxel coords).
    pub pending_bake: Option<usize>,
    /// Editor-side request: restore clip[i] back to its anchor and
    /// discard it (also despawns the visual).
    pub pending_restore: Option<usize>,
    /// Editor-side request: remove clip[i] without restoring (the
    /// blocks stay missing from the world; useful when you've baked
    /// the new pose and just want to drop the clip).
    pub pending_discard: Option<usize>,
    /// Last status string for the UI.
    pub status: String,
    /// Persistent target counter for unique clip names.
    next_id: u32,
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AnimationStudio>().add_systems(
            Update,
            (
                toggle_picker_keybind.run_if(in_state(GameState::InGame)),
                pick_input.run_if(in_state(GameState::InGame)),
                draw_picker_gizmos,
                process_capture,
                process_bake,
                process_restore_or_discard,
                drive_animation,
            )
                .chain(),
        );
    }
}

// ---------------------------------------------------------------------
// Keybinds + picker raycast
// ---------------------------------------------------------------------

fn toggle_picker_keybind(
    keys: Res<ButtonInput<KeyCode>>,
    mut studio: ResMut<AnimationStudio>,
    mut toolbelt: ResMut<crate::toolbelt::ToolbeltState>,
    mut mode: ResMut<crate::mode::ModeContext>,
) {
    if keys.just_pressed(KeyCode::F4) {
        studio.picking = !studio.picking;
        toolbelt.tool = crate::toolbelt::ToolbeltTool::AnimationPick;
        studio.status = if studio.picking {
            "Picker AN — Linksklick waehlt, Rechtsklick entfernt.".into()
        } else {
            "Picker AUS.".into()
        };
        if studio.picking {
            mode.set(
                crate::mode::ActiveMode::BuildLive {
                    tool: crate::toolbelt::ToolbeltTool::AnimationPick,
                },
                "Build Live: Animation Picker. LMB/RMB pick voxels for animation authoring.",
            );
        } else {
            mode.set(crate::mode::ActiveMode::Combat, "Animation Picker off.");
        }
        info!(
            "Animation picker {}",
            if studio.picking { "ON" } else { "OFF" }
        );
    }
}

/// Forward DDA voxel raycast from the player camera. Returns the first
/// solid voxel hit within `max_dist`. Mirrored from `weapons::dda_voxel`
/// (duplicated rather than re-exported to keep `weapons.rs` private —
/// the function is ~30 lines and keeping the modules decoupled wins).
fn pick_raycast(world: &VoxelWorld, origin: Vec3, dir: Vec3, max_dist: f32) -> Option<IVec3> {
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
    let nb = |p: f32, step: i32| -> f32 {
        if step > 0 {
            p.floor() + 1.0 - p
        } else if step < 0 {
            p - p.floor()
        } else {
            f32::INFINITY
        }
    };
    let mut tmx = nb(origin.x, step_x) * t_delta_x;
    let mut tmy = nb(origin.y, step_y) * t_delta_y;
    let mut tmz = nb(origin.z, step_z) * t_delta_z;
    let mut steps = 0;
    while steps < 4_096 {
        let t = tmx.min(tmy).min(tmz);
        if t > max_dist {
            return None;
        }
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
            return Some(IVec3::new(x, y, z));
        }
        steps += 1;
    }
    None
}

fn pick_input(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    world: Res<VoxelWorld>,
    mut studio: ResMut<AnimationStudio>,
) {
    if !studio.picking {
        return;
    }
    // Only react when the cursor is locked, so picker clicks don't
    // collide with editor UI clicks.
    let cursor_locked = windows
        .get_single()
        .map(|w| w.cursor.grab_mode == bevy::window::CursorGrabMode::Locked)
        .unwrap_or(false);
    if !cursor_locked {
        return;
    }
    let Ok(cam_tf) = cam_q.get_single() else {
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some(hit) = pick_raycast(&world, origin, dir, 80.0) else {
        return;
    };
    if mouse.just_pressed(MouseButton::Left) {
        if !studio.selection.contains(&hit) {
            studio.selection.push(hit);
            studio.status = format!("Auswahl: {} Bloecke", studio.selection.len());
        }
    } else if mouse.just_pressed(MouseButton::Right) {
        if let Some(i) = studio.selection.iter().position(|&p| p == hit) {
            studio.selection.swap_remove(i);
            studio.status = format!("Auswahl: {} Bloecke", studio.selection.len());
        }
    }
}

/// Cyan reticle around the currently hovered voxel + magenta outlines
/// around every selected voxel. Cheap because Gizmos batches per-frame
/// line draws into a single mesh.
fn draw_picker_gizmos(
    mut gizmos: Gizmos,
    studio: Res<AnimationStudio>,
    world: Res<VoxelWorld>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
) {
    let outline = Color::srgb(1.0, 0.25, 0.7);
    for &p in &studio.selection {
        let center = p.as_vec3() + Vec3::splat(0.5);
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(Vec3::splat(1.02)),
            outline,
        );
    }
    if !studio.picking {
        return;
    }
    if let Ok(cam_tf) = cam_q.get_single() {
        let origin = cam_tf.translation();
        let dir = cam_tf.forward().as_vec3();
        if let Some(hit) = pick_raycast(&world, origin, dir, 80.0) {
            let center = hit.as_vec3() + Vec3::splat(0.5);
            let cyan = Color::srgb(0.0, 0.95, 1.0);
            gizmos.cuboid(
                Transform::from_translation(center).with_scale(Vec3::splat(1.06)),
                cyan,
            );
        }
    }
}

// ---------------------------------------------------------------------
// Capture / bake / restore commands
// ---------------------------------------------------------------------

fn process_capture(
    mut studio: ResMut<AnimationStudio>,
    mut world: ResMut<VoxelWorld>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !studio.pending_capture {
        return;
    }
    studio.pending_capture = false;
    if studio.selection.is_empty() {
        studio.status = "Auswahl leer — nichts erfasst.".into();
        return;
    }

    // Compute anchor (min corner) so block offsets are >= 0.
    let mut min = IVec3::splat(i32::MAX);
    for &p in &studio.selection {
        min = min.min(p);
    }

    // Snapshot voxels and lift them out of the world.
    let mut blocks = Vec::with_capacity(studio.selection.len());
    let selection = std::mem::take(&mut studio.selection);
    for p in &selection {
        let v = world.voxel_at(p.x, p.y, p.z);
        if v == AIR {
            continue;
        }
        blocks.push((*p - min, v));
        world.edit_set_voxel(p.x, p.y, p.z, AIR);
    }

    if blocks.is_empty() {
        studio.status = "Auswahl enthielt nur Luft — verworfen.".into();
        return;
    }

    studio.next_id = studio.next_id.wrapping_add(1);
    let id = studio.next_id;
    let mut clip = AnimClip {
        name: format!("Clip {id}"),
        blocks,
        anchor: min,
        keys: vec![
            KeyFrame::identity(0.0),
            KeyFrame {
                time: 4.0,
                offset: Vec3::Y * 2.0,
                yaw_deg: 360.0,
                scale: 1.0,
                interp: Interp::EaseInOut,
            },
        ],
        t: 0.0,
        playing: true,
        looping: true,
        speed: 1.0,
        root: None,
    };

    // Spawn the visual replica.
    let root = spawn_clip_visual(
        &mut commands,
        &mut meshes,
        &mut materials,
        &clip,
        studio.clips.len(),
    );
    clip.root = Some(root);

    studio.status = format!(
        "Erfasst: {} Bloecke @ {:?}.",
        clip.blocks.len(),
        clip.anchor.to_array()
    );
    studio.active = Some(studio.clips.len());
    studio.clips.push(clip);
}

fn spawn_clip_visual(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    clip: &AnimClip,
    clip_idx: usize,
) -> Entity {
    // Shared 1×1×1 cube + per-block-type material. Clip volumes are
    // typically <1k blocks, so per-instance material lookup is fine.
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let mut mat_for: ahash::AHashMap<Voxel, Handle<StandardMaterial>> = Default::default();
    let anchor = clip.anchor.as_vec3();
    let root = commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(anchor),
                ..default()
            },
            AnimRoot { clip: clip_idx },
        ))
        .id();
    for (offset, v) in &clip.blocks {
        let mat = mat_for
            .entry(*v)
            .or_insert_with(|| {
                let bt = BlockType::from_voxel(*v);
                let c = bt.color();
                materials.add(StandardMaterial {
                    base_color: c,
                    perceptual_roughness: 0.85,
                    metallic: 0.05,
                    ..default()
                })
            })
            .clone();
        let child = commands
            .spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: mat,
                    transform: Transform::from_translation(offset.as_vec3() + Vec3::splat(0.5)),
                    ..default()
                },
                AnimChild,
            ))
            .id();
        commands.entity(root).add_child(child);
    }
    root
}

fn despawn_recursive_if_exists(commands: &mut Commands, entity: Entity) {
    if let Some(entity_commands) = commands.get_entity(entity) {
        entity_commands.despawn_recursive();
    }
}

fn process_bake(
    mut studio: ResMut<AnimationStudio>,
    mut world: ResMut<VoxelWorld>,
    mut commands: Commands,
) {
    let Some(idx) = studio.pending_bake.take() else {
        return;
    };
    if idx >= studio.clips.len() {
        return;
    }
    let clip = studio.clips[idx].clone();
    let (offset, _yaw, _scale) = clip.sample();
    // Snap to integer voxel coords. Yaw/scale are intentionally ignored
    // here — baking voxels with arbitrary rotation would require a full
    // resample pass and is a future enhancement. Translation alone
    // covers "place this captured prefab at a new spot" perfectly.
    let snap = IVec3::new(
        offset.x.round() as i32,
        offset.y.round() as i32,
        offset.z.round() as i32,
    );
    let mut n = 0;
    for (off, v) in &clip.blocks {
        let p = clip.anchor + *off + snap;
        if world.edit_set_voxel(p.x, p.y, p.z, *v) {
            n += 1;
        }
    }
    if let Some(root) = clip.root {
        despawn_recursive_if_exists(&mut commands, root);
    }
    let removed = studio.clips.remove(idx);
    studio.status = format!(
        "Gebacken: {} Bloecke an Offset {:?} ({}).",
        n,
        snap.to_array(),
        removed.name
    );
    fix_active_index(&mut studio, idx);
}

fn process_restore_or_discard(
    mut studio: ResMut<AnimationStudio>,
    mut world: ResMut<VoxelWorld>,
    mut commands: Commands,
) {
    if let Some(idx) = studio.pending_restore.take() {
        if idx < studio.clips.len() {
            let clip = studio.clips[idx].clone();
            let mut n = 0;
            for (off, v) in &clip.blocks {
                let p = clip.anchor + *off;
                if world.edit_set_voxel(p.x, p.y, p.z, *v) {
                    n += 1;
                }
            }
            if let Some(root) = clip.root {
                despawn_recursive_if_exists(&mut commands, root);
            }
            let removed = studio.clips.remove(idx);
            studio.status = format!("Wiederhergestellt: {} Bloecke ({}).", n, removed.name);
            fix_active_index(&mut studio, idx);
        }
    }
    if let Some(idx) = studio.pending_discard.take() {
        if idx < studio.clips.len() {
            if let Some(root) = studio.clips[idx].root {
                despawn_recursive_if_exists(&mut commands, root);
            }
            let removed = studio.clips.remove(idx);
            studio.status = format!("Verworfen: {}.", removed.name);
            fix_active_index(&mut studio, idx);
        }
    }
}

fn fix_active_index(studio: &mut AnimationStudio, removed_idx: usize) {
    studio.active = match studio.active {
        Some(a) if a == removed_idx => None,
        Some(a) if a > removed_idx => Some(a - 1),
        other => other,
    };
    // AnimRoot.clip indices on still-living entities also need to shift,
    // but since `process_bake` / `process_restore` always despawn the
    // root they removed, the surviving roots' indices are now stale.
    // We re-anchor them in `drive_animation` by ignoring the stored
    // index and matching by entity identity instead.
}

// ---------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------

fn drive_animation(
    time: Res<Time>,
    mut studio: ResMut<AnimationStudio>,
    mut roots: Query<(Entity, &mut Transform), With<AnimRoot>>,
) {
    let dt = time.delta_seconds();
    // Build a quick map from root Entity -> clip index. Indices on the
    // AnimRoot component can become stale after a clip is removed, so we
    // rebuild by matching the `root` field on each clip.
    for (i, clip) in studio.clips.iter_mut().enumerate() {
        if clip.playing {
            clip.t += dt * clip.speed;
            if clip.looping {
                let dur = clip.duration();
                if clip.t > dur {
                    clip.t = clip.t.rem_euclid(dur);
                }
            } else {
                clip.t = clip.t.min(clip.duration());
            }
        }
        let (offset, yaw, scale) = clip.sample();
        let target_translation = clip.anchor.as_vec3() + offset;
        let target_rot = Quat::from_rotation_y(yaw.to_radians());
        let target_scale = Vec3::splat(scale.max(0.001));
        if let Some(root_entity) = clip.root {
            if let Ok((_, mut tf)) = roots.get_mut(root_entity) {
                tf.translation = target_translation;
                tf.rotation = target_rot;
                tf.scale = target_scale;
            }
            // If the root no longer exists (despawned externally), drop
            // the dangling reference so nothing tries to bake later.
            if !roots.contains(root_entity) {
                clip.root = None;
            }
        }
        // `i` is intentionally unused for the index-rebind comment but
        // kept to make a future "label clips with their index in HUD"
        // pass trivial.
        let _ = i;
    }
}
