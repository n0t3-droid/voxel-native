//! Direct rectangle drawing for live edit mode.
//!
//! This is the "I draw a square and it becomes blocks" tool the builder
//! needs before the heavier transform-gizmo phases are worth anything:
//! hold LMB on a face, drag the crosshair to a block endpoint on the locked
//! plane, release to fill it with the active builder block. In the default
//! sketch builder, RMB is reserved for camera orbit; Ctrl+LMB cuts an
//! opening and Shift+LMB clears room depth. Esc cancels the active preview.
//! Undo/redo uses the shared builder history.

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
const RECT_ROOM_CUT_DEPTH_CAP: i32 = 32;
const RECT_ROOM_CUT_MIN_DEPTH: i32 = 6;
const RECT_FILL_OWNER: &str = "Sketch Draw";
const RECT_AXIS_JITTER: i32 = 1;
const RECT_AXIS_RATIO: f32 = 3.0;
const RECT_EQUAL_LENGTH_TOLERANCE: i32 = 2;

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
    room_cut: bool,
    inference: RectEndpointInference,
    voxel: Voxel,
    status_cells: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectDrawAction {
    #[default]
    Fill,
    Cut,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectEndpointInference {
    #[default]
    None,
    Axis,
    EqualLength,
}

impl RectEndpointInference {
    fn status_suffix(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Axis => " Axis lock.",
            Self::EqualLength => " Equal-length snap.",
        }
    }
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RectStartIntent {
    fill: bool,
    cut: bool,
    room_cut: bool,
    button: RectDragButton,
}

fn rect_start_intent(
    active_tool: ToolbeltTool,
    left_just: bool,
    right_just: bool,
    ctrl: bool,
    shift: bool,
    room_workflow: bool,
) -> RectStartIntent {
    let smart_tool = matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    );
    let sketch_tool = active_tool == ToolbeltTool::DrawRect;
    let room_cut = sketch_tool && left_just && !ctrl && (shift || room_workflow);
    let modifier_cut = sketch_tool && left_just && (ctrl || shift || room_workflow);
    let brush_cut = active_tool == ToolbeltTool::BrushCut && left_just;
    let smart_right_cut = smart_tool && right_just;
    let cut = modifier_cut || brush_cut || smart_right_cut;
    let fill = left_just && !cut && active_tool != ToolbeltTool::BrushCut;
    let button = if smart_right_cut {
        RectDragButton::Right
    } else {
        RectDragButton::Left
    };
    RectStartIntent {
        fill,
        cut,
        room_cut,
        button,
    }
}

fn shape_alt_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

fn shift_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

fn ctrl_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
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

    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    let smart_tool = matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    );

    if keys.just_pressed(KeyCode::Escape) && draw.active {
        draw.active = false;
        draw.click_finish = false;
        gesture_lock.release(RECT_FILL_OWNER);
        toolbelt.status = "Smart Build cancelled. LMB starts a new snapped build point.".into();
        motion_evr.clear();
        return;
    }

    let window = windows.get_single().ok();
    let cursor_locked = window.map(crate::mode::cursor_is_captured).unwrap_or(false);
    if smart_tool && !cursor_locked {
        if mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right) {
            toolbelt.status =
                "Smart Builder needs mouse capture. Click the game view once, then build.".into();
        }
        motion_evr.clear();
        return;
    }

    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Smart Build could not find the player camera this frame.".into();
        }
        motion_evr.clear();
        return;
    };
    let Some((origin, dir)) = draw_input_ray(active_tool, cursor_locked, window, camera, cam_tf)
    else {
        if mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right) {
            toolbelt.status =
                "Sketch Draw needs the pointer inside the game window to pick endpoints.".into();
        }
        motion_evr.clear();
        return;
    };

    let start_intent = rect_start_intent(
        active_tool,
        mouse.just_pressed(MouseButton::Left),
        mouse.just_pressed(MouseButton::Right),
        ctrl_pressed(&keys),
        shift_pressed(&keys),
        toolbelt.room_workflow_active(),
    );

    if start_intent.fill && draw.active && draw.click_finish {
        commit_rect_fill(&mut draw, &mut world, &mut history, &mut toolbelt);
        gesture_lock.release(RECT_FILL_OWNER);
        motion_evr.clear();
        return;
    }

    if (start_intent.fill || start_intent.cut) && !draw.active {
        let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) else {
            toolbelt.status =
                "Smart Build needs a target face. Aim at a visible block face.".into();
            motion_evr.clear();
            return;
        };
        let normal = prev - hit;
        let Some((axis_u, axis_v)) = plane_axes(normal) else {
            toolbelt.status = "Sketch Draw found an invalid target normal.".into();
            motion_evr.clear();
            return;
        };
        let action = if start_intent.cut {
            RectDrawAction::Cut
        } else {
            RectDrawAction::Fill
        };
        draw.active = true;
        let start = rect_start_cell_from_ray(action, hit, prev, axis_u, axis_v, origin, dir);
        draw.start = start;
        draw.current = start;
        draw.normal = normal;
        draw.axis_u = axis_u;
        draw.axis_v = axis_v;
        draw.motion_len = 0.0;
        draw.action = action;
        draw.button = start_intent.button;
        draw.smart_gesture = smart_tool;
        draw.room_cut = action == RectDrawAction::Cut && start_intent.room_cut;
        draw.inference = RectEndpointInference::None;
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
        } else if draw.room_cut {
            "Smart Room Hollow start set. Drag the wall/floor face; release clears a livable volume behind it.".into()
        } else if mode.build_tool() == Some(ToolbeltTool::Sculpt) {
            "Quick Fill start set. Keep Alt held while starting; drag to fill, LMB commits.".into()
        } else {
            "Sketch Draw start set. Drag endpoint, release to build. Hold RMB to orbit; Ctrl+LMB cuts, Shift+LMB hollows.".into()
        };
    }

    if draw.active {
        gesture_lock.lock(RECT_FILL_OWNER);
        if rect_draw_endpoint_updates(draw.smart_gesture, mouse.pressed(MouseButton::Right)) {
            for ev in motion_evr.read() {
                draw.motion_len += ev.delta.length();
            }
            if let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) {
                let endpoint = snap_rect_endpoint_to_locked_plane_from_ray(
                    draw.start,
                    draw.normal,
                    draw.axis_u,
                    draw.axis_v,
                    hit,
                    prev,
                    origin,
                    dir,
                );
                let (endpoint, inference) =
                    infer_rect_endpoint(draw.start, endpoint, draw.axis_u, draw.axis_v);
                draw.current = endpoint;
                draw.inference = inference;
            } else if let Some(endpoint) = snap_rect_endpoint_from_locked_plane_ray(
                draw.start,
                draw.normal,
                draw.axis_u,
                draw.axis_v,
                origin,
                dir,
            ) {
                let (endpoint, inference) =
                    infer_rect_endpoint(draw.start, endpoint, draw.axis_u, draw.axis_v);
                draw.current = endpoint;
                draw.inference = inference;
            }
            let raw_cells = rect_cell_count(draw.start, draw.current, draw.normal);
            draw.status_cells = raw_cells.min(DRAW_CELL_CAP);
            let action_label = if draw.room_cut {
                "Smart Room Hollow"
            } else {
                draw.action.label()
            };
            toolbelt.status = if raw_cells > DRAW_CELL_CAP {
                format!(
                    "{} preview capped: {} of {} cells.{} Release commits, Esc cancels.",
                    action_label,
                    DRAW_CELL_CAP,
                    raw_cells,
                    draw.inference.status_suffix()
                )
            } else {
                format!(
                    "{} preview: {} cells snapped to endpoint.{} Release commits, Esc cancels.",
                    action_label,
                    draw.status_cells,
                    draw.inference.status_suffix()
                )
            };
        } else {
            motion_evr.clear();
            toolbelt.status =
                "Sketch Draw orbiting: endpoint held. Release RMB to continue snapping, LMB release commits."
                    .into();
        }
    } else {
        motion_evr.clear();
    }

    if draw.button.just_released(&mouse) && draw.active && !draw.click_finish {
        if !draw.smart_gesture && draw.motion_len < 4.0 && draw.status_cells <= 1 {
            draw.click_finish = true;
            toolbelt.status =
                "Sketch Draw anchor set. Move to grow line/face, LMB commits, RMB orbits, Esc cancels."
                    .into();
        } else {
            commit_rect_fill(&mut draw, &mut world, &mut history, &mut toolbelt);
            gesture_lock.release(RECT_FILL_OWNER);
        }
    }
}

fn draw_input_ray(
    active_tool: ToolbeltTool,
    cursor_locked: bool,
    window: Option<&bevy::window::Window>,
    camera: &Camera,
    camera_tf: &GlobalTransform,
) -> Option<(Vec3, Vec3)> {
    if matches!(active_tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt) && !cursor_locked {
        if let Some(ray) = window
            .and_then(|window| window.cursor_position())
            .and_then(|cursor| camera.viewport_to_world(camera_tf, cursor))
        {
            return Some((ray.origin, *ray.direction));
        }
    }
    Some((camera_tf.translation(), camera_tf.forward().as_vec3()))
}

fn rect_draw_endpoint_updates(smart_gesture: bool, right_held: bool) -> bool {
    smart_gesture || !right_held
}

fn commit_rect_fill(
    draw: &mut RectDrawState,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    toolbelt: &mut ToolbeltState,
) {
    let cells = match draw.action {
        RectDrawAction::Fill => rect_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP),
        RectDrawAction::Cut if draw.room_cut => rect_room_cut_cells_through_solid(
            world,
            draw.start,
            draw.current,
            draw.normal,
            DRAW_CELL_CAP,
        ),
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
        let label = if draw.room_cut {
            format!("Smart room hollow {} cells", changed)
        } else {
            format!("{} {} cells", draw.action.history_label(), changed)
        };
        history.record_external(label, changes);
        toolbelt.status = format!(
            "{} committed: {} selected, {} changed cells. Ctrl+Z undo, Ctrl+Y redo.",
            if draw.room_cut {
                "Smart Room Hollow"
            } else {
                draw.action.label()
            },
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

fn rect_start_cell_from_ray(
    action: RectDrawAction,
    hit: IVec3,
    adjacent: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> IVec3 {
    let mut start = rect_start_cell(action, hit, adjacent);
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return start;
    }
    if let Some(face_hit) = ray_face_hit_point(ray_origin, ray_dir, hit, adjacent) {
        set_component_by_axis(
            &mut start,
            axis_u,
            round_to_i32_safe(vec_component_by_axis(face_hit, axis_u)),
        );
        set_component_by_axis(
            &mut start,
            axis_v,
            round_to_i32_safe(vec_component_by_axis(face_hit, axis_v)),
        );
    }
    start
}

fn snap_rect_endpoint_to_locked_plane_from_ray(
    start: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    hit: IVec3,
    adjacent: IVec3,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> IVec3 {
    let mut snapped =
        snap_rect_endpoint_to_locked_plane(start, normal, axis_u, axis_v, hit, adjacent);
    let Some(plane_axis) = normal_axis(normal) else {
        return snapped;
    };
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return snapped;
    }
    if let Some(face_hit) = ray_face_hit_point(ray_origin, ray_dir, hit, adjacent) {
        set_component_by_axis(
            &mut snapped,
            axis_u,
            round_to_i32_safe(vec_component_by_axis(face_hit, axis_u)),
        );
        set_component_by_axis(
            &mut snapped,
            axis_v,
            round_to_i32_safe(vec_component_by_axis(face_hit, axis_v)),
        );
        set_component_by_index(
            &mut snapped,
            plane_axis,
            component_by_index(start, plane_axis),
        );
    }
    snapped
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

fn ray_face_hit_point(
    ray_origin: Vec3,
    ray_dir: Vec3,
    hit: IVec3,
    adjacent: IVec3,
) -> Option<Vec3> {
    let normal = adjacent - hit;
    let axis = normal_axis(normal)?;
    let denom = vec_component_by_index(ray_dir, axis);
    if denom.abs() < 1e-5 {
        return None;
    }
    let plane = if component_by_index(normal, axis) > 0 {
        component_by_index(hit, axis) as f32 + 1.0
    } else {
        component_by_index(hit, axis) as f32
    };
    let t = (plane - vec_component_by_index(ray_origin, axis)) / denom;
    if !t.is_finite() || t < 0.0 {
        return None;
    }
    let face_hit = ray_origin + ray_dir * t;
    face_hit.is_finite().then_some(face_hit)
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
        round_to_i32_safe(vec_component_by_axis(hit, axis_u)),
    );
    set_component_by_axis(
        &mut snapped,
        axis_v,
        round_to_i32_safe(vec_component_by_axis(hit, axis_v)),
    );
    set_component_by_index(
        &mut snapped,
        plane_axis,
        component_by_index(start, plane_axis),
    );
    Some(snapped)
}

fn infer_rect_endpoint(
    start: IVec3,
    raw: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
) -> (IVec3, RectEndpointInference) {
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return (raw, RectEndpointInference::None);
    }
    let du = component_by_axis(raw, axis_u) - component_by_axis(start, axis_u);
    let dv = component_by_axis(raw, axis_v) - component_by_axis(start, axis_v);
    let au = du.abs();
    let av = dv.abs();
    if au == 0 && av == 0 {
        return (raw, RectEndpointInference::None);
    }

    let mut inferred = raw;
    if au > 0 && av > 0 && (av <= RECT_AXIS_JITTER || au as f32 >= av as f32 * RECT_AXIS_RATIO) {
        set_component_by_axis(&mut inferred, axis_v, component_by_axis(start, axis_v));
        return (inferred, RectEndpointInference::Axis);
    }
    if av > 0 && au > 0 && (au <= RECT_AXIS_JITTER || av as f32 >= au as f32 * RECT_AXIS_RATIO) {
        set_component_by_axis(&mut inferred, axis_u, component_by_axis(start, axis_u));
        return (inferred, RectEndpointInference::Axis);
    }

    if au >= 2 && av >= 2 && (au - av).abs() <= RECT_EQUAL_LENGTH_TOLERANCE {
        let span = au.max(av);
        set_component_by_axis(
            &mut inferred,
            axis_u,
            component_by_axis(start, axis_u) + du.signum() * span,
        );
        set_component_by_axis(
            &mut inferred,
            axis_v,
            component_by_axis(start, axis_v) + dv.signum() * span,
        );
        return (inferred, RectEndpointInference::EqualLength);
    }

    (raw, RectEndpointInference::None)
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

fn round_to_i32_safe(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32
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

fn rect_room_cut_cells_through_solid(
    world: &VoxelWorld,
    a: IVec3,
    b: IVec3,
    normal: IVec3,
    cap: usize,
) -> Vec<IVec3> {
    if normal_axis(normal).is_none() {
        return Vec::new();
    }
    let (span_u, span_v) = rect_plane_spans(a, b, normal);
    let depth = smart_room_cut_depth(span_u, span_v);
    let inward = -normal;
    let mut out = Vec::new();
    for surface in rect_cells(a, b, normal, cap) {
        for layer in 0..depth {
            if out.len() >= cap {
                return out;
            }
            let pos = surface + inward * layer;
            if voxel_is_solid(world.voxel_at(pos.x, pos.y, pos.z)) {
                out.push(pos);
            }
        }
    }
    out
}

fn rect_plane_spans(a: IVec3, b: IVec3, normal: IVec3) -> (i32, i32) {
    let size = (b - a).abs() + IVec3::ONE;
    match normal_axis(normal) {
        Some(0) => (size.y, size.z),
        Some(1) => (size.x, size.z),
        Some(2) => (size.x, size.y),
        _ => (0, 0),
    }
}

fn smart_room_cut_depth(span_u: i32, span_v: i32) -> i32 {
    let broad = span_u.max(span_v).max(1);
    (broad * 2 / 3).clamp(RECT_ROOM_CUT_MIN_DEPTH, RECT_ROOM_CUT_DEPTH_CAP)
}

pub fn draw_rect_gizmo(draw: Res<RectDrawState>, mut gizmos: Gizmos, time: Res<Time>) {
    if !draw.active {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 7.0).sin().abs();
    let color = match draw.action {
        RectDrawAction::Fill => Color::srgb(0.15 + 0.25 * pulse, 0.95, 1.0),
        RectDrawAction::Cut => Color::srgb(1.0, 0.15 + 0.25 * pulse, 0.05),
    };
    let (lo, hi) = rect_bounds(draw.start, draw.current);
    let center = (lo.as_vec3() + hi.as_vec3()) * 0.5 + Vec3::splat(0.5);
    let mut scale = (hi - lo + IVec3::ONE).as_vec3();
    let normal_abs = draw.normal.abs().as_vec3();
    scale = scale * (Vec3::ONE - normal_abs) + normal_abs * 0.10;
    gizmos.cuboid(Transform::from_translation(center).with_scale(scale), color);
    gizmos.cuboid(
        Transform::from_translation(center).with_scale(scale + Vec3::splat(0.06)),
        Color::srgba(1.0, 1.0, 1.0, 0.65),
    );
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
    fn sketch_right_mouse_is_reserved_for_orbit_not_cut() {
        let intent = rect_start_intent(ToolbeltTool::DrawRect, false, true, false, false, false);

        assert!(
            !intent.cut && !intent.fill,
            "RMB in Sketch Draw should not remove blocks; it is camera orbit"
        );
    }

    #[test]
    fn sketch_right_mouse_orbit_freezes_endpoint_drag_updates() {
        assert!(
            !rect_draw_endpoint_updates(false, true),
            "RMB orbit in Sketch Draw should hold the current endpoint instead of distorting the preview"
        );
        assert!(rect_draw_endpoint_updates(false, false));
    }

    #[test]
    fn smart_right_mouse_cut_keeps_endpoint_tracking() {
        assert!(
            rect_draw_endpoint_updates(true, true),
            "classic smart RMB cut gestures still need endpoint tracking while held"
        );
    }

    #[test]
    fn sketch_modifier_left_mouse_selects_cut_and_room_cut() {
        let cut = rect_start_intent(ToolbeltTool::DrawRect, true, false, true, false, false);
        assert!(cut.cut);
        assert!(!cut.room_cut);
        assert_eq!(cut.button, RectDragButton::Left);

        let room = rect_start_intent(ToolbeltTool::DrawRect, true, false, false, true, false);
        assert!(room.cut);
        assert!(room.room_cut);
        assert_eq!(room.button, RectDragButton::Left);
    }

    #[test]
    fn room_workflow_left_mouse_hollows_without_modifier() {
        let room = rect_start_intent(ToolbeltTool::DrawRect, true, false, false, false, true);

        assert!(room.cut);
        assert!(room.room_cut);
        assert_eq!(room.button, RectDragButton::Left);
    }

    #[test]
    fn ctrl_left_mouse_cuts_openings_even_inside_room_workflow() {
        let cut = rect_start_intent(ToolbeltTool::DrawRect, true, false, true, false, true);

        assert!(cut.cut);
        assert!(!cut.room_cut);
        assert_eq!(cut.button, RectDragButton::Left);
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
    fn rect_start_snaps_to_nearest_block_corner_on_hit_face() {
        let start = rect_start_cell_from_ray(
            RectDrawAction::Fill,
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
            IVec3::X,
            IVec3::Z,
            Vec3::new(10.82, 5.0, 14.18),
            Vec3::NEG_Y,
        );

        assert_eq!(
            start,
            IVec3::new(11, 1, 14),
            "start point should snap to a real voxel grid corner, not only the hit cell center"
        );
    }

    #[test]
    fn rect_endpoint_snaps_to_nearest_block_corner_from_ray_hit() {
        let snapped = snap_rect_endpoint_to_locked_plane_from_ray(
            IVec3::new(0, 1, 0),
            IVec3::Y,
            IVec3::X,
            IVec3::Z,
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
            Vec3::new(10.82, 5.0, 14.18),
            Vec3::NEG_Y,
        );

        assert_eq!(snapped, IVec3::new(11, 1, 14));
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
    fn rect_endpoint_inference_axis_locks_small_hand_jitter() {
        let (snapped, inference) = infer_rect_endpoint(
            IVec3::new(10, 64, 10),
            IVec3::new(31, 64, 11),
            IVec3::X,
            IVec3::Z,
        );

        assert_eq!(snapped, IVec3::new(31, 64, 10));
        assert_eq!(inference, RectEndpointInference::Axis);
    }

    #[test]
    fn rect_endpoint_inference_snaps_near_square_to_equal_lengths() {
        let (snapped, inference) = infer_rect_endpoint(
            IVec3::new(0, 64, 0),
            IVec3::new(7, 64, 5),
            IVec3::X,
            IVec3::Z,
        );

        assert_eq!(snapped, IVec3::new(7, 64, 7));
        assert_eq!(inference, RectEndpointInference::EqualLength);
    }

    #[test]
    fn rect_endpoint_inference_preserves_deliberate_rectangles() {
        let (snapped, inference) = infer_rect_endpoint(
            IVec3::new(0, 64, 0),
            IVec3::new(16, 64, 10),
            IVec3::X,
            IVec3::Z,
        );

        assert_eq!(snapped, IVec3::new(16, 64, 10));
        assert_eq!(inference, RectEndpointInference::None);
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
    fn smart_room_cut_clears_livable_depth_behind_drawn_wall_face() {
        let mut world = VoxelWorld::new();
        for x in 0..=7 {
            for y in 0..=7 {
                for z in 0..=7 {
                    world.edit_set_voxel(x, y, z, Voxel::from(BlockType::Stone));
                }
            }
        }

        let cells = rect_room_cut_cells_through_solid(
            &world,
            IVec3::new(1, 1, 7),
            IVec3::new(6, 6, 7),
            IVec3::Z,
            DRAW_CELL_CAP,
        );

        assert!(cells.contains(&IVec3::new(3, 3, 7)));
        assert!(cells.contains(&IVec3::new(3, 3, 2)));
        assert!(
            !cells.contains(&IVec3::new(0, 3, 7)),
            "room cut should preserve wall shell outside the drawn face"
        );
        assert!(
            cells.len() > 6 * 6 * 3,
            "room cut should clear a usable volume, not only a shallow hole"
        );
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
