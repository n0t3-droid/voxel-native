//! Direct rectangle drawing for live edit mode.
//!
//! This is the "I draw a square and it becomes blocks" tool the builder
//! needs before the heavier transform-gizmo phases are worth anything:
//! select RECT in the live toolbelt, hold LMB on a face, drag a rectangle
//! over the locked plane, release to fill it with the active builder block.
//! Esc cancels the active preview. Undo/redo uses the shared builder history.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::Voxel;
use crate::builder::{BuilderHistory, BuilderState};
use crate::mode::{BuildGestureLock, ModeContext};
use crate::player::Player;
use crate::sculpt::raycast::dda_voxel;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

const DRAW_REACH: f32 = 128.0;
const DRAW_CELL_CAP: usize = 16_384;
const MIN_SCREEN_AXIS_PX: f32 = 10.0;
const RECT_FILL_OWNER: &str = "Rectangle Fill";

#[derive(Resource, Default)]
pub struct RectDrawState {
    active: bool,
    click_finish: bool,
    start: IVec3,
    current: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    screen_u: Vec2,
    screen_v: Vec2,
    motion_accum: Vec2,
    motion_len: f32,
    voxel: Voxel,
    status_cells: usize,
}

fn shape_alt_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn draw_rect_active(mode: &ModeContext, keys: &ButtonInput<KeyCode>, draw: &RectDrawState) -> bool {
    if !mode.is_build_live() {
        return false;
    }
    match mode.build_tool() {
        Some(ToolbeltTool::DrawRect) => draw.active || !shape_alt_pressed(keys),
        Some(ToolbeltTool::Sculpt) => draw.active || shape_alt_pressed(keys),
        _ => false,
    }
}

pub fn rect_draw_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    mode: Res<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut draw: ResMut<RectDrawState>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    builder: Res<BuilderState>,
    cam_q: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
) {
    if !draw_rect_active(&mode, &keys, &draw) {
        if draw.active {
            draw.active = false;
            draw.click_finish = false;
        }
        gesture_lock.release(RECT_FILL_OWNER);
        motion_evr.clear();
        return;
    }

    if (keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right)) && draw.active
    {
        draw.active = false;
        draw.click_finish = false;
        gesture_lock.release(RECT_FILL_OWNER);
        toolbelt.status = "Rectangle Fill cancelled. LMB starts a new start point.".into();
        motion_evr.clear();
        return;
    }

    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Rectangle Fill could not find the player camera this frame.".into();
        }
        motion_evr.clear();
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();

    if mouse.just_pressed(MouseButton::Left) && draw.active && draw.click_finish {
        commit_rect_fill(&mut draw, &mut world, &mut history, &mut toolbelt);
        gesture_lock.release(RECT_FILL_OWNER);
        motion_evr.clear();
        return;
    }

    if mouse.just_pressed(MouseButton::Left) && !draw.active {
        let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) else {
            toolbelt.status =
                "Rectangle Fill needs a target face. Aim at a visible block face.".into();
            motion_evr.clear();
            return;
        };
        let normal = prev - hit;
        let Some((axis_u, axis_v)) = plane_axes(normal) else {
            toolbelt.status = "Rectangle Fill found an invalid target normal.".into();
            motion_evr.clear();
            return;
        };
        let anchor = prev.as_vec3() + Vec3::splat(0.5);
        let Some(screen_u) = project_axis(camera, cam_tf, anchor, axis_u.as_vec3()) else {
            toolbelt.status =
                "Rectangle Fill could not lock the screen axis. Move the camera slightly.".into();
            motion_evr.clear();
            return;
        };
        let Some(screen_v) = project_axis(camera, cam_tf, anchor, axis_v.as_vec3()) else {
            toolbelt.status =
                "Rectangle Fill could not lock the screen axis. Move the camera slightly.".into();
            motion_evr.clear();
            return;
        };
        draw.active = true;
        draw.start = prev;
        draw.current = prev;
        draw.normal = normal;
        draw.axis_u = axis_u;
        draw.axis_v = axis_v;
        draw.screen_u = stable_screen_axis(screen_u);
        draw.screen_v = stable_screen_axis(screen_v);
        draw.motion_accum = Vec2::ZERO;
        draw.motion_len = 0.0;
        draw.voxel = builder.block.into();
        draw.status_cells = 1;
        draw.click_finish = false;
        gesture_lock.lock(RECT_FILL_OWNER);
        toolbelt.status = if mode.build_tool() == Some(ToolbeltTool::Sculpt) {
            "Quick Fill start set. Keep Alt held while starting; drag to fill, LMB commits.".into()
        } else {
            "Rectangle Fill start set. Drag to fill now, or tap-release then move and LMB to finish.".into()
        };
    }

    if draw.active {
        gesture_lock.lock(RECT_FILL_OWNER);
        for ev in motion_evr.read() {
            draw.motion_accum += ev.delta;
            draw.motion_len += ev.delta.length();
        }
        let u = grid_steps_for_axis(draw.motion_accum, draw.screen_u);
        let v = grid_steps_for_axis(draw.motion_accum, draw.screen_v);
        draw.current = draw.start + draw.axis_u * u + draw.axis_v * v;
        let raw_cells = rect_cell_count(draw.start, draw.current, draw.normal);
        draw.status_cells = raw_cells.min(DRAW_CELL_CAP);
        toolbelt.status = if raw_cells > DRAW_CELL_CAP {
            format!(
                "Rectangle Fill preview capped: {} of {} cells. LMB finishes, RMB/Esc cancels.",
                DRAW_CELL_CAP, raw_cells
            )
        } else {
            format!(
                "Rectangle Fill preview: {} cells. LMB finishes, RMB/Esc cancels.",
                draw.status_cells
            )
        };
    } else {
        motion_evr.clear();
    }

    if mouse.just_released(MouseButton::Left) && draw.active && !draw.click_finish {
        if draw.motion_len < 4.0 && draw.status_cells <= 1 {
            draw.click_finish = true;
            toolbelt.status =
                "Rectangle Fill anchor set. Move to grow line/face, LMB commits, RMB/Esc cancels."
                    .into();
        } else {
            commit_rect_fill(&mut draw, &mut world, &mut history, &mut toolbelt);
            gesture_lock.release(RECT_FILL_OWNER);
        }
    }
}

fn commit_rect_fill(
    draw: &mut RectDrawState,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    toolbelt: &mut ToolbeltState,
) {
    let cells = rect_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP);
    let selected = cells.len();
    if cells.is_empty() {
        draw.active = false;
        draw.click_finish = false;
        return;
    }

    let mut batch = WorldEditBatch::default();
    let mut changes: Vec<(IVec3, Voxel, Voxel)> = Vec::with_capacity(cells.len());
    for pos in cells {
        if let Some((before, after)) =
            world.edit_set_voxel_batched(pos.x, pos.y, pos.z, draw.voxel, &mut batch)
        {
            changes.push((pos, before, after));
        }
    }
    world.finish_edit_batch(batch);
    let changed = changes.len();
    if changed > 0 {
        let label = format!("DrawRect {} cells", changed);
        history.record_external(label, changes);
        toolbelt.status = format!(
            "Rectangle Fill committed: {} selected, {} changed cells. Ctrl+Z undo, Ctrl+R redo.",
            selected, changed
        );
    } else {
        toolbelt.status = format!(
            "Rectangle Fill selected {} cells but made no changes because the area already matched the block.",
            selected
        );
    }
    draw.active = false;
    draw.click_finish = false;
}

fn normal_axis(normal: IVec3) -> Option<usize> {
    let abs_sum = normal.x.abs() + normal.y.abs() + normal.z.abs();
    if abs_sum != 1 {
        return None;
    }
    if normal.x != 0 {
        Some(0)
    } else if normal.y != 0 {
        Some(1)
    } else {
        Some(2)
    }
}

fn plane_axes(normal: IVec3) -> Option<(IVec3, IVec3)> {
    normal_axis(normal)?;
    if normal.x != 0 {
        Some((IVec3::Y, IVec3::Z))
    } else if normal.y != 0 {
        Some((IVec3::X, IVec3::Z))
    } else {
        Some((IVec3::X, IVec3::Y))
    }
}

fn project_axis(
    camera: &Camera,
    cam_tf: &GlobalTransform,
    anchor: Vec3,
    world_axis: Vec3,
) -> Option<Vec2> {
    let p0 = camera.world_to_viewport(cam_tf, anchor)?;
    let p1 = camera.world_to_viewport(cam_tf, anchor + world_axis)?;
    Some(p1 - p0)
}

fn stable_screen_axis(axis: Vec2) -> Vec2 {
    let len = axis.length();
    if len < 1e-4 {
        return Vec2::X * MIN_SCREEN_AXIS_PX;
    }
    if len < MIN_SCREEN_AXIS_PX {
        axis / len * MIN_SCREEN_AXIS_PX
    } else {
        axis
    }
}

fn grid_steps_for_axis(motion: Vec2, axis: Vec2) -> i32 {
    let len2 = axis.length_squared();
    if len2 < 1e-4 {
        return 0;
    }
    (motion.dot(axis) / len2).round() as i32
}

fn rect_cell_count(a: IVec3, b: IVec3, normal: IVec3) -> usize {
    let Some(axis) = normal_axis(normal) else {
        return 0;
    };
    let size = (b - a).abs() + IVec3::ONE;
    match axis {
        0 => (size.y * size.z) as usize,
        1 => (size.x * size.z) as usize,
        _ => (size.x * size.y) as usize,
    }
}

fn rect_cells(a: IVec3, b: IVec3, normal: IVec3, cap: usize) -> Vec<IVec3> {
    let Some(axis) = normal_axis(normal) else {
        return Vec::new();
    };
    let lo = IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z));
    let hi = IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z));
    let mut out = Vec::with_capacity(rect_cell_count(a, b, normal).min(cap));
    match axis {
        0 => {
            for y in lo.y..=hi.y {
                for z in lo.z..=hi.z {
                    out.push(IVec3::new(a.x, y, z));
                    if out.len() >= cap {
                        return out;
                    }
                }
            }
        }
        1 => {
            for x in lo.x..=hi.x {
                for z in lo.z..=hi.z {
                    out.push(IVec3::new(x, a.y, z));
                    if out.len() >= cap {
                        return out;
                    }
                }
            }
        }
        _ => {
            for x in lo.x..=hi.x {
                for y in lo.y..=hi.y {
                    out.push(IVec3::new(x, y, a.z));
                    if out.len() >= cap {
                        return out;
                    }
                }
            }
        }
    }
    out
}

pub fn draw_rect_gizmo(draw: Res<RectDrawState>, mut gizmos: Gizmos, time: Res<Time>) {
    if !draw.active {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 7.0).sin().abs();
    let color = Color::srgb(1.0, 0.05 + 0.35 * pulse, 1.0);
    let (lo, hi) = rect_bounds(draw.start, draw.current);
    let center = (lo.as_vec3() + hi.as_vec3()) * 0.5 + Vec3::splat(0.5);
    let mut scale = (hi - lo + IVec3::ONE).as_vec3();
    let normal_abs = draw.normal.abs().as_vec3();
    scale = scale * (Vec3::ONE - normal_abs) + normal_abs * 0.06;
    gizmos.cuboid(Transform::from_translation(center).with_scale(scale), color);
}

fn rect_bounds(a: IVec3, b: IVec3) -> (IVec3, IVec3) {
    (
        IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_cells_counts_horizontal_plane() {
        let a = IVec3::new(0, 10, 0);
        let b = IVec3::new(3, 10, 2);
        let cells = rect_cells(a, b, IVec3::Y, DRAW_CELL_CAP);
        assert_eq!(cells.len(), 12);
        assert!(cells.contains(&IVec3::new(0, 10, 0)));
        assert!(cells.contains(&IVec3::new(3, 10, 2)));
    }

    #[test]
    fn rect_cells_respects_cap() {
        let a = IVec3::new(0, 0, 0);
        let b = IVec3::new(99, 0, 99);
        let cells = rect_cells(a, b, IVec3::Y, 128);
        assert_eq!(cells.len(), 128);
    }

    #[test]
    fn grid_steps_use_locked_screen_axis() {
        let axis = Vec2::new(12.0, 0.0);
        assert_eq!(grid_steps_for_axis(Vec2::new(25.0, 4.0), axis), 2);
        assert_eq!(grid_steps_for_axis(Vec2::new(-18.0, 0.0), axis), -2);
    }
}
