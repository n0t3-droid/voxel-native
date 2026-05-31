//! Direct rectangle drawing for live edit mode.
//!
//! This is the "I draw a square and it becomes blocks" tool the builder
//! needs before the heavier transform-gizmo phases are worth anything:
//! hold LMB on a face, drag the crosshair to a block endpoint on the locked
//! plane, release to fill it with the active builder block. In the default
//! smart builder, RMB does the same gesture as a cut. Esc cancels the active
//! preview. Undo/redo uses the shared builder history.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::{voxel_is_solid, Voxel, AIR};
use crate::builder::{BuilderHistory, BuilderState};
use crate::mode::{BuildGestureLock, ModeContext};
use crate::player::Player;
use crate::sculpt::raycast::dda_voxel;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

const DRAW_REACH: f32 = 128.0;
const DRAW_CELL_CAP: usize = 16_384;
const RECT_CUT_DEPTH_CAP: i32 = 16;
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
    motion_len: f32,
    action: RectDrawAction,
    button: RectDragButton,
    smart_gesture: bool,
    voxel: Voxel,
    status_cells: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectDrawAction {
    #[default]
    Fill,
    Cut,
}

impl RectDrawAction {
    fn label(self) -> &'static str {
        match self {
            Self::Fill => "Smart Build",
            Self::Cut => "Smart Cut",
        }
    }

    fn history_label(self) -> &'static str {
        match self {
            Self::Fill => "Smart endpoint build",
            Self::Cut => "Smart endpoint cut",
        }
    }

    fn preview_verb(self) -> &'static str {
        match self {
            Self::Fill => "build",
            Self::Cut => "cut",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectDragButton {
    #[default]
    Left,
    Right,
}

impl RectDragButton {
    fn just_released(self, mouse: &ButtonInput<MouseButton>) -> bool {
        match self {
            Self::Left => mouse.just_released(MouseButton::Left),
            Self::Right => mouse.just_released(MouseButton::Right),
        }
    }
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
        Some(ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut) => true,
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
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
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

    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    let smart_tool = matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    );

    if (keys.just_pressed(KeyCode::Escape)
        || (mouse.just_pressed(MouseButton::Right)
            && draw.active
            && draw.action == RectDrawAction::Fill))
        && draw.active
    {
        draw.active = false;
        draw.click_finish = false;
        gesture_lock.release(RECT_FILL_OWNER);
        toolbelt.status = "Smart Build cancelled. LMB starts a new snapped build point.".into();
        motion_evr.clear();
        return;
    }

    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);
    if smart_tool && !cursor_locked {
        if mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right) {
            toolbelt.status =
                "Smart Builder needs mouse capture. Click the game view once, then build.".into();
        }
        motion_evr.clear();
        return;
    }

    let Ok(cam_tf) = cam_q.get_single() else {
        if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Smart Build could not find the player camera this frame.".into();
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

    let start_cut = smart_tool
        && (mouse.just_pressed(MouseButton::Right)
            || (active_tool == ToolbeltTool::BrushCut && mouse.just_pressed(MouseButton::Left)));
    let start_fill = mouse.just_pressed(MouseButton::Left) && active_tool != ToolbeltTool::BrushCut;

    if (start_fill || start_cut) && !draw.active {
        let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) else {
            toolbelt.status =
                "Smart Build needs a target face. Aim at a visible block face.".into();
            motion_evr.clear();
            return;
        };
        let normal = prev - hit;
        let Some((axis_u, axis_v)) = plane_axes(normal) else {
            toolbelt.status = "Rectangle Fill found an invalid target normal.".into();
            motion_evr.clear();
            return;
        };
        let action = if start_cut {
            RectDrawAction::Cut
        } else {
            RectDrawAction::Fill
        };
        draw.active = true;
        let start = rect_start_cell(action, hit, prev);
        draw.start = start;
        draw.current = start;
        draw.normal = normal;
        draw.axis_u = axis_u;
        draw.axis_v = axis_v;
        draw.motion_len = 0.0;
        draw.action = action;
        draw.button = if start_cut && mouse.just_pressed(MouseButton::Right) {
            RectDragButton::Right
        } else {
            RectDragButton::Left
        };
        draw.smart_gesture = smart_tool;
        draw.voxel = if action == RectDrawAction::Cut {
            AIR
        } else {
            builder.block.into()
        };
        draw.status_cells = 1;
        draw.click_finish = false;
        gesture_lock.lock(RECT_FILL_OWNER);
        toolbelt.status = if smart_tool {
            format!(
                "{} start set. Drag to any block endpoint; release to {} the exact snapped length.",
                action.label(),
                action.preview_verb()
            )
        } else if mode.build_tool() == Some(ToolbeltTool::Sculpt) {
            "Quick Fill start set. Keep Alt held while starting; drag to fill, LMB commits.".into()
        } else {
            "Rectangle Fill start set. Drag to fill now, or tap-release then move and LMB to finish.".into()
        };
    }

    if draw.active {
        gesture_lock.lock(RECT_FILL_OWNER);
        for ev in motion_evr.read() {
            draw.motion_len += ev.delta.length();
        }
        if let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) {
            draw.current = snap_rect_endpoint_to_locked_plane(
                draw.start,
                draw.normal,
                draw.axis_u,
                draw.axis_v,
                hit,
                prev,
            );
        } else if let Some(endpoint) = snap_rect_endpoint_from_locked_plane_ray(
            draw.start,
            draw.normal,
            draw.axis_u,
            draw.axis_v,
            origin,
            dir,
        ) {
            draw.current = endpoint;
        }
        let raw_cells = rect_cell_count(draw.start, draw.current, draw.normal);
        draw.status_cells = raw_cells.min(DRAW_CELL_CAP);
        toolbelt.status = if raw_cells > DRAW_CELL_CAP {
            format!(
                "{} preview capped: {} of {} cells. Release commits, Esc cancels.",
                draw.action.label(),
                DRAW_CELL_CAP,
                raw_cells
            )
        } else {
            format!(
                "{} preview: {} cells snapped to endpoint. Release commits, Esc cancels.",
                draw.action.label(),
                draw.status_cells
            )
        };
    } else {
        motion_evr.clear();
    }

    if draw.button.just_released(&mouse) && draw.active && !draw.click_finish {
        if !draw.smart_gesture && draw.motion_len < 4.0 && draw.status_cells <= 1 {
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
    let cells = match draw.action {
        RectDrawAction::Fill => rect_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP),
        RectDrawAction::Cut => rect_cut_cells_through_solid(
            world,
            draw.start,
            draw.current,
            draw.normal,
            DRAW_CELL_CAP,
        ),
    };
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
        let label = format!("{} {} cells", draw.action.history_label(), changed);
        history.record_external(label, changes);
        toolbelt.status = format!(
            "{} committed: {} selected, {} changed cells. Ctrl+Z undo, Ctrl+R redo.",
            draw.action.label(),
            selected,
            changed
        );
    } else {
        toolbelt.status = format!(
            "{} selected {} cells but made no changes because the area already matched.",
            draw.action.label(),
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

fn rect_start_cell(action: RectDrawAction, hit: IVec3, adjacent: IVec3) -> IVec3 {
    match action {
        RectDrawAction::Fill => adjacent,
        RectDrawAction::Cut => hit,
    }
}

fn snap_rect_endpoint_to_locked_plane(
    start: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    hit: IVec3,
    adjacent: IVec3,
) -> IVec3 {
    let Some(plane_axis) = normal_axis(normal) else {
        return start;
    };
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return start;
    }

    let start_plane = component_by_index(start, plane_axis);
    let hit_plane_delta = (component_by_index(hit, plane_axis) - start_plane).abs();
    let adjacent_plane_delta = (component_by_index(adjacent, plane_axis) - start_plane).abs();
    let hovered = if adjacent_plane_delta <= hit_plane_delta {
        adjacent
    } else {
        hit
    };

    let mut snapped = start;
    set_component_by_axis(&mut snapped, axis_u, component_by_axis(hovered, axis_u));
    set_component_by_axis(&mut snapped, axis_v, component_by_axis(hovered, axis_v));
    set_component_by_index(&mut snapped, plane_axis, start_plane);
    snapped
}

fn snap_rect_endpoint_from_locked_plane_ray(
    start: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<IVec3> {
    let plane_axis = normal_axis(normal)?;
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return None;
    }
    let denom = vec_component_by_index(ray_dir, plane_axis);
    if denom.abs() < 1e-5 {
        return None;
    }

    let plane = component_by_index(start, plane_axis) as f32 + 0.5;
    let t = (plane - vec_component_by_index(ray_origin, plane_axis)) / denom;
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    let hit = ray_origin + ray_dir * t;
    if !hit.is_finite() {
        return None;
    }

    let mut snapped = start;
    set_component_by_axis(
        &mut snapped,
        axis_u,
        floor_to_i32_safe(vec_component_by_axis(hit, axis_u)),
    );
    set_component_by_axis(
        &mut snapped,
        axis_v,
        floor_to_i32_safe(vec_component_by_axis(hit, axis_v)),
    );
    set_component_by_index(
        &mut snapped,
        plane_axis,
        component_by_index(start, plane_axis),
    );
    Some(snapped)
}

fn is_cardinal_axis(axis: IVec3) -> bool {
    axis.x.abs() + axis.y.abs() + axis.z.abs() == 1
}

fn component_by_axis(v: IVec3, axis: IVec3) -> i32 {
    if axis.x != 0 {
        v.x
    } else if axis.y != 0 {
        v.y
    } else {
        v.z
    }
}

fn vec_component_by_axis(v: Vec3, axis: IVec3) -> f32 {
    if axis.x != 0 {
        v.x
    } else if axis.y != 0 {
        v.y
    } else {
        v.z
    }
}

fn vec_component_by_index(v: Vec3, index: usize) -> f32 {
    match index {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn floor_to_i32_safe(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.floor().clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

fn component_by_index(v: IVec3, index: usize) -> i32 {
    match index {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn set_component_by_axis(v: &mut IVec3, axis: IVec3, value: i32) {
    if axis.x != 0 {
        v.x = value;
    } else if axis.y != 0 {
        v.y = value;
    } else {
        v.z = value;
    }
}

fn set_component_by_index(v: &mut IVec3, index: usize, value: i32) {
    match index {
        0 => v.x = value,
        1 => v.y = value,
        _ => v.z = value,
    }
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

fn rect_cut_cells_through_solid(
    world: &VoxelWorld,
    a: IVec3,
    b: IVec3,
    normal: IVec3,
    cap: usize,
) -> Vec<IVec3> {
    if normal_axis(normal).is_none() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let inward = -normal;
    for surface in rect_cells(a, b, normal, cap) {
        let mut pos = surface;
        for _ in 0..RECT_CUT_DEPTH_CAP {
            if out.len() >= cap {
                return out;
            }
            if !voxel_is_solid(world.voxel_at(pos.x, pos.y, pos.z)) {
                break;
            }
            out.push(pos);
            pos += inward;
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
    use crate::blocks::BlockType;

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
    fn default_build_tool_accepts_smart_endpoint_fill() {
        let mode = ModeContext::default();
        let keys = ButtonInput::<KeyCode>::default();
        let draw = RectDrawState::default();

        assert!(draw_rect_active(&mode, &keys, &draw));
    }

    #[test]
    fn rect_endpoint_snaps_hovered_block_to_locked_floor_plane() {
        let start = IVec3::new(10, 64, 10);
        let hit = IVec3::new(18, 70, 14);
        let adjacent = IVec3::new(18, 71, 14);

        let snapped =
            snap_rect_endpoint_to_locked_plane(start, IVec3::Y, IVec3::X, IVec3::Z, hit, adjacent);

        assert_eq!(snapped, IVec3::new(18, 64, 14));
    }

    #[test]
    fn rect_endpoint_snaps_hovered_block_to_locked_wall_plane() {
        let start = IVec3::new(4, 10, 6);
        let hit = IVec3::new(20, 17, 15);
        let adjacent = IVec3::new(21, 17, 15);

        let snapped =
            snap_rect_endpoint_to_locked_plane(start, IVec3::X, IVec3::Y, IVec3::Z, hit, adjacent);

        assert_eq!(snapped, IVec3::new(4, 17, 15));
    }

    #[test]
    fn rect_endpoint_infers_locked_wall_plane_when_ray_hits_empty_space() {
        let start = IVec3::new(4, 10, 6);
        let ray_origin = Vec3::new(12.2, 13.8, -8.0);
        let ray_dir = Vec3::new(-1.0, 0.25, 2.0).normalize();

        let snapped = snap_rect_endpoint_from_locked_plane_ray(
            start,
            IVec3::X,
            IVec3::Y,
            IVec3::Z,
            ray_origin,
            ray_dir,
        )
        .expect("ray should intersect the locked wall plane");

        assert_eq!(snapped.x, start.x);
        assert!(
            snapped.y > start.y,
            "vertical endpoint should follow the inferred plane hit, got {snapped:?}"
        );
        assert!(
            snapped.z > start.z,
            "depth endpoint should continue beyond existing voxels on the locked plane, got {snapped:?}"
        );
    }

    #[test]
    fn rect_endpoint_infers_locked_floor_plane_for_free_ground_sketches() {
        let start = IVec3::new(10, 64, 10);
        let ray_origin = Vec3::new(4.5, 80.0, 3.0);
        let ray_dir = Vec3::new(0.42, -1.0, 0.55).normalize();

        let snapped = snap_rect_endpoint_from_locked_plane_ray(
            start,
            IVec3::Y,
            IVec3::X,
            IVec3::Z,
            ray_origin,
            ray_dir,
        )
        .expect("ray should intersect the locked floor plane");

        assert_eq!(snapped.y, start.y);
        assert!(
            snapped.x > start.x && snapped.z > start.z,
            "floor endpoint should grow from the fixed plane without needing a voxel hit, got {snapped:?}"
        );
    }

    #[test]
    fn cut_rectangle_drills_through_wall_thickness_for_windows_and_doors() {
        let mut world = VoxelWorld::new();
        for x in 0..=2 {
            for y in 0..=2 {
                for z in 0..=2 {
                    world.edit_set_voxel(x, y, z, Voxel::from(BlockType::Stone));
                }
            }
        }

        let cells = rect_cut_cells_through_solid(
            &world,
            IVec3::new(1, 1, 2),
            IVec3::new(2, 2, 2),
            IVec3::Z,
            DRAW_CELL_CAP,
        );

        assert_eq!(
            cells.len(),
            12,
            "2x2 opening should include all three solid wall layers"
        );
        assert!(cells.contains(&IVec3::new(1, 1, 2)));
        assert!(cells.contains(&IVec3::new(1, 1, 1)));
        assert!(cells.contains(&IVec3::new(1, 1, 0)));
    }

    #[test]
    fn cut_gesture_starts_on_hit_block_not_adjacent_air() {
        let hit = IVec3::new(8, 32, -4);
        let adjacent = IVec3::new(8, 33, -4);

        assert_eq!(
            rect_start_cell(RectDrawAction::Fill, hit, adjacent),
            adjacent
        );
        assert_eq!(rect_start_cell(RectDrawAction::Cut, hit, adjacent), hit);
    }
}
