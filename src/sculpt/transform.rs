use std::collections::{BTreeMap, HashMap, HashSet};

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::{BlockType, Voxel, AIR};
use crate::builder::{BuilderHistory, BuilderHistoryRecordOutcome, BuilderHistorySketchMeta};
use crate::mode::{BuildGestureLock, ModeContext};
use crate::sculpt::state::SculptState;
use crate::sketch_model::{EditorToolId, SketchVoxelEntityLinkSnapshot, ToolController};
use crate::toolbelt::ToolbeltState;
use crate::world::{VoxelWorld, WorldEditBatch};

const MOVE_OWNER: &str = "Sketch Move";
const MOVE_PIXELS_PER_VOXEL: f32 = 18.0;
const MOVE_DELTA_LIMIT: i32 = 256;
const ROTATE_OWNER: &str = "Sketch Rotate";
const ROTATE_PIXELS_PER_QUARTER: f32 = 42.0;
const SCALE_OWNER: &str = "Sketch Scale";
const SCALE_PIXELS_PER_STEP: f32 = 36.0;
const SCALE_FACTOR_MAX: i32 = 8;
const TRANSFORM_OUTPUT_LIMIT: usize = 250_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveAxisLock {
    X,
    Y,
    Z,
}

impl MoveAxisLock {
    pub fn from_key(key: KeyCode) -> Option<Self> {
        match key {
            KeyCode::ArrowRight => Some(Self::X),
            KeyCode::ArrowUp => Some(Self::Y),
            KeyCode::ArrowLeft => Some(Self::Z),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct SemanticMoveDrag {
    active: bool,
    motion: Vec2,
    delta: IVec3,
    axis_lock: Option<MoveAxisLock>,
    copy_mode: bool,
    copy_count: usize,
    grip_cell: Option<IVec3>,
    grip_point: Option<Vec3>,
    hover_snap_active: bool,
    selection: crate::sketch_model::SelectionSet,
    cells: Vec<IVec3>,
    tool_generation: u64,
}

impl Default for SemanticMoveDrag {
    fn default() -> Self {
        Self {
            active: false,
            motion: Vec2::ZERO,
            delta: IVec3::ZERO,
            axis_lock: None,
            copy_mode: false,
            copy_count: 1,
            grip_cell: None,
            grip_point: None,
            hover_snap_active: false,
            selection: crate::sketch_model::SelectionSet::default(),
            cells: Vec::new(),
            tool_generation: 0,
        }
    }
}

impl SemanticMoveDrag {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformAxis {
    X,
    Y,
    Z,
}

impl TransformAxis {
    pub fn from_key(key: KeyCode) -> Option<Self> {
        match key {
            KeyCode::ArrowRight => Some(Self::X),
            KeyCode::ArrowUp => Some(Self::Y),
            KeyCode::ArrowLeft => Some(Self::Z),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }

    fn vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }

    fn direction(self) -> Dir3 {
        match self {
            Self::X => Dir3::X,
            Self::Y => Dir3::Y,
            Self::Z => Dir3::Z,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct SemanticRotateDrag {
    active: bool,
    motion_x: f32,
    quarter_turns: i32,
    axis: TransformAxis,
    /// Twice the world-space pivot. Keeping this integral preserves exact
    /// half-voxel centers for even-sized selections.
    pivot_twice: IVec3,
    selection: crate::sketch_model::SelectionSet,
    cells: Vec<IVec3>,
    tool_generation: u64,
}

impl Default for SemanticRotateDrag {
    fn default() -> Self {
        Self {
            active: false,
            motion_x: 0.0,
            quarter_turns: 0,
            axis: TransformAxis::Y,
            pivot_twice: IVec3::ZERO,
            selection: crate::sketch_model::SelectionSet::default(),
            cells: Vec::new(),
            tool_generation: 0,
        }
    }
}

impl SemanticRotateDrag {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Resource, Debug, Clone)]
pub struct SemanticScaleDrag {
    active: bool,
    motion_x: f32,
    /// `1` is identity, `2..=8` expands, `-2..=-8` shrinks by the
    /// absolute divisor. A signed lattice ratio keeps the preview and commit
    /// paths exact without introducing floating-point voxel ownership.
    factor: i32,
    anchor_cell: IVec3,
    selection: crate::sketch_model::SelectionSet,
    cells: Vec<IVec3>,
    tool_generation: u64,
}

impl Default for SemanticScaleDrag {
    fn default() -> Self {
        Self {
            active: false,
            motion_x: 0.0,
            factor: 1,
            anchor_cell: IVec3::ZERO,
            selection: crate::sketch_model::SelectionSet::default(),
            cells: Vec::new(),
            tool_generation: 0,
        }
    }
}

impl SemanticScaleDrag {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

pub fn begin_move_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    tool_controller: Res<ToolController>,
    sketch_doc: Res<crate::sketch_model::SketchDocument>,
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
    semantic_hover: Res<crate::sketch_model::SemanticHoverHit>,
    state: Res<SculptState>,
    mut drag: ResMut<SemanticMoveDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if drag.active
        || !mouse.just_pressed(MouseButton::Left)
        || !move_tool_active(&mode, &tool_controller)
        || ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui)
    {
        return;
    }

    let selection = tool_controller.selection().clone();
    let cells = selection_cells(&sketch_links, &selection);
    if selection.is_empty() || cells.is_empty() {
        toolbelt.status =
            "Move: select a drawn line, face, room, road, or house part first.".into();
        return;
    }

    drag.active = true;
    drag.motion = Vec2::ZERO;
    drag.delta = IVec3::ZERO;
    drag.axis_lock = None;
    drag.copy_mode = false;
    drag.copy_count = 1;
    drag.grip_cell = move_grip_cell(&cells, state.hover.map(|hit| hit.voxel));
    drag.grip_point = move_grip_reference_point(
        &sketch_doc,
        &selection,
        semantic_hover.0.as_ref(),
        drag.grip_cell,
    );
    drag.hover_snap_active = false;
    drag.selection = selection;
    drag.cells = cells;
    drag.tool_generation = tool_controller.tool_generation();
    gesture_lock.lock(MOVE_OWNER);
    toolbelt.status =
        "Move: hover an endpoint, midpoint, face center, or voxel target. Ctrl copies; optional arrows lock X/Y/Z."
            .into();
}

pub fn update_move_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    mut drag: ResMut<SemanticMoveDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut tool_controller: ResMut<ToolController>,
    sketch_doc: Res<crate::sketch_model::SketchDocument>,
    semantic_hover: Res<crate::sketch_model::SemanticHoverHit>,
    state: Res<SculptState>,
) {
    if !drag.active {
        motion_evr.clear();
        gesture_lock.release(MOVE_OWNER);
        return;
    }

    gesture_lock.lock(MOVE_OWNER);
    let escape_pressed = keys.just_pressed(KeyCode::Escape);
    let tool_changed =
        move_should_cancel_for_tool_selection(&drag, tool_controller.tool_generation());
    if move_drag_should_cancel(escape_pressed, tool_changed) {
        drag.clear();
        gesture_lock.release(MOVE_OWNER);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        toolbelt.status = "Move cancelled.".into();
        motion_evr.clear();
        return;
    }

    if !move_drag_accepts_motion(mouse.pressed(MouseButton::Right)) {
        motion_evr.clear();
        toolbelt.status =
            "Move held while orbiting. Release RMB to resume exact endpoint snapping.".into();
        return;
    }

    for key in [KeyCode::ArrowRight, KeyCode::ArrowUp, KeyCode::ArrowLeft] {
        if keys.just_pressed(key) {
            drag.axis_lock = MoveAxisLock::from_key(key);
        }
    }
    if keys.just_pressed(KeyCode::ArrowDown) {
        drag.axis_lock = None;
    }

    for key in [
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ] {
        if keys.just_pressed(key) {
            drag.copy_mode = true;
            drag.copy_count = move_copy_count_from_key(key).unwrap_or(1);
        }
    }

    let motion_delta: Vec2 = motion_evr.read().map(|event| event.delta).sum();
    if motion_delta != Vec2::ZERO {
        drag.motion += motion_delta;
    }
    let reference_delta = move_delta_from_reference_hit(
        drag.grip_point,
        &sketch_doc,
        semantic_hover.0.as_ref(),
        drag.axis_lock,
    );
    let hover_delta = move_delta_from_hover_cell(
        drag.grip_cell,
        state.hover.map(|hit| hit.voxel),
        drag.axis_lock,
    );
    let mouse_delta = snapped_move_delta(drag.motion, drag.axis_lock);
    let mouse_delta = (mouse_delta != IVec3::ZERO).then_some(mouse_delta);
    let next_hover_snap_active = reference_delta.is_some() || hover_delta.is_some();
    let snap_delta = move_delta_from_snap_target(reference_delta, hover_delta, mouse_delta);
    let next_copy_mode = drag.copy_count > 1 || move_copy_modifier_pressed(&keys);
    let Some(next_delta) = snap_delta else {
        if drag.hover_snap_active || drag.delta != IVec3::ZERO || next_copy_mode != drag.copy_mode {
            drag.delta = IVec3::ZERO;
            drag.copy_mode = next_copy_mode;
            drag.hover_snap_active = false;
            let action = if drag.copy_mode {
                format!("Copy x{}", drag.copy_count.max(1))
            } else {
                "Move".to_string()
            };
            toolbelt.status = format!(
                "{action}: drag selected cells freely, or hover an endpoint/midpoint/face center for exact snap."
            );
        }
        return;
    };
    if next_hover_snap_active {
        drag.motion = Vec2::ZERO;
    }
    if next_delta != drag.delta
        || next_copy_mode != drag.copy_mode
        || next_hover_snap_active != drag.hover_snap_active
    {
        drag.delta = next_delta;
        drag.copy_mode = next_copy_mode;
        drag.hover_snap_active = next_hover_snap_active;
        let axis = drag.axis_lock.map(MoveAxisLock::label).unwrap_or("free");
        let snap = if drag.hover_snap_active {
            "target snap"
        } else {
            "screen drag"
        };
        let action = if drag.copy_mode {
            format!("Copy x{}", drag.copy_count.max(1))
        } else {
            "Move".to_string()
        };
        toolbelt.status = format!(
            "{action} {axis} {snap}: {} cells selected, delta ({}, {}, {}). Release to commit.",
            drag.cells.len(),
            drag.delta.x,
            drag.delta.y,
            drag.delta.z
        );
    }
}

pub fn end_move_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut drag: ResMut<SemanticMoveDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
) {
    if !drag.active || !mouse.just_released(MouseButton::Left) {
        return;
    }

    let delta = drag.delta;
    if drag.copy_mode {
        let copy_count = drag.copy_count.max(1);
        let copied = commit_selection_voxel_copy_array(
            &mut world,
            &mut history,
            &mut sketch_doc,
            &mut sketch_links,
            &drag.selection,
            delta,
            copy_count,
            format!("Copy selection x{copy_count}"),
        );
        if copied > 0 {
            toolbelt.status = format!(
                "Copy committed: {copied} voxels across {copy_count} snapped copies by ({}, {}, {}).",
                delta.x, delta.y, delta.z
            );
        } else {
            toolbelt.status = "Copy cancelled: no voxel step.".into();
        }
    } else {
        let moved = commit_selection_voxel_move(
            &mut world,
            &mut history,
            &mut sketch_doc,
            &mut sketch_links,
            &drag.selection,
            delta,
            "Move selection",
        );
        if moved > 0 {
            toolbelt.status = format!(
                "Move committed: {moved} voxels shifted by ({}, {}, {}).",
                delta.x, delta.y, delta.z
            );
        } else {
            toolbelt.status = "Move cancelled: no voxel step.".into();
        }
    }
    drag.clear();
    gesture_lock.release(MOVE_OWNER);
}

pub fn draw_move_gizmo(drag: Res<SemanticMoveDrag>, mut gizmos: Gizmos, time: Res<Time>) {
    if !drag.active || drag.cells.is_empty() {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let current = selection_bounds(&drag.cells, IVec3::ZERO);
    let target_count = if drag.copy_mode {
        drag.copy_count.max(1)
    } else {
        1
    };
    if let Some((center, scale)) = current {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.04)),
            Color::srgba(0.20, 0.90, 1.00, 0.35),
        );
    }
    for step in 1..=target_count {
        if let Some((center, scale)) = selection_bounds(&drag.cells, drag.delta * step as i32) {
            let alpha = if drag.copy_mode {
                (0.35 + pulse * 0.18).min(0.70)
            } else {
                0.65 + pulse * 0.25
            };
            gizmos.cuboid(
                Transform::from_translation(center).with_scale(scale + Vec3::splat(0.14)),
                Color::srgba(1.00, 0.78, 0.18, alpha),
            );
        }
    }
    if drag.delta != IVec3::ZERO {
        let from = drag.cells[0].as_vec3() + Vec3::splat(0.5);
        let to = from + (drag.delta * target_count as i32).as_vec3();
        gizmos.line(from, to, Color::srgb(1.0, 0.78, 0.18));
    }
}

pub fn begin_rotate_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    tool_controller: Res<ToolController>,
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
    mut drag: ResMut<SemanticRotateDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if drag.active
        || !mouse.just_pressed(MouseButton::Left)
        || !rotate_tool_active(&mode, &tool_controller)
        || ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui)
    {
        return;
    }

    let selection = tool_controller.selection().clone();
    let cells = selection_cells(&sketch_links, &selection);
    let Some(pivot_twice) = selection_pivot_twice(&cells) else {
        toolbelt.status = "Rotate: select a drawn object first.".into();
        return;
    };

    drag.active = true;
    drag.motion_x = 0.0;
    drag.quarter_turns = 0;
    drag.axis = TransformAxis::Y;
    drag.pivot_twice = pivot_twice;
    drag.selection = selection;
    drag.cells = cells;
    drag.tool_generation = tool_controller.tool_generation();
    gesture_lock.lock(ROTATE_OWNER);
    toolbelt.status =
        "Rotate: drag horizontally for exact 90 degree steps. Optional arrows choose X/Y/Z; RMB orbits."
            .into();
}

pub fn update_rotate_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    mut drag: ResMut<SemanticRotateDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut tool_controller: ResMut<ToolController>,
) {
    if !drag.active {
        motion_evr.clear();
        gesture_lock.release(ROTATE_OWNER);
        return;
    }

    gesture_lock.lock(ROTATE_OWNER);
    let tool_changed = drag.tool_generation != tool_controller.tool_generation();
    if transform_drag_should_cancel(keys.just_pressed(KeyCode::Escape), tool_changed) {
        drag.clear();
        gesture_lock.release(ROTATE_OWNER);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        toolbelt.status = "Rotate cancelled.".into();
        motion_evr.clear();
        return;
    }

    if !transform_drag_accepts_motion(mouse.pressed(MouseButton::Right)) {
        motion_evr.clear();
        toolbelt.status = "Rotate held while orbiting. Release RMB to resume.".into();
        return;
    }

    for key in [KeyCode::ArrowRight, KeyCode::ArrowUp, KeyCode::ArrowLeft] {
        if keys.just_pressed(key) {
            drag.axis = TransformAxis::from_key(key).unwrap_or(TransformAxis::Y);
        }
    }
    if keys.just_pressed(KeyCode::ArrowDown) {
        drag.axis = TransformAxis::Y;
    }

    drag.motion_x += motion_evr.read().map(|event| event.delta.x).sum::<f32>();
    let next_turns = snapped_quarter_turns(drag.motion_x);
    if next_turns != drag.quarter_turns {
        drag.quarter_turns = next_turns;
        toolbelt.status = format!(
            "Rotate {}: {} degrees around exact selection center. Release to commit.",
            drag.axis.label(),
            drag.quarter_turns * 90
        );
    }
}

pub fn end_rotate_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut drag: ResMut<SemanticRotateDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
) {
    if !drag.active || !mouse.just_released(MouseButton::Left) {
        return;
    }

    let rotated = commit_selection_voxel_rotate(
        &mut world,
        &mut history,
        &mut sketch_doc,
        &mut sketch_links,
        &drag.selection,
        drag.pivot_twice,
        drag.axis,
        drag.quarter_turns,
        "Rotate selection",
    );
    if rotated > 0 {
        toolbelt.status = format!(
            "Rotate committed: {rotated} voxels, {} degrees around {}.",
            normalized_quarter_turns(drag.quarter_turns) * 90,
            drag.axis.label()
        );
    } else {
        toolbelt.status = "Rotate cancelled: no rotation step.".into();
    }
    drag.clear();
    gesture_lock.release(ROTATE_OWNER);
}

pub fn draw_rotate_gizmo(drag: Res<SemanticRotateDrag>, mut gizmos: Gizmos, time: Res<Time>) {
    if !drag.active || drag.cells.is_empty() {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let pivot = drag.pivot_twice.as_vec3() * 0.5;
    let target: Vec<_> = drag
        .cells
        .iter()
        .filter_map(|cell| {
            rotate_cell_quarter(*cell, drag.pivot_twice, drag.axis, drag.quarter_turns)
        })
        .collect();
    if let Some((center, scale)) = selection_bounds(&drag.cells, IVec3::ZERO) {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.04)),
            Color::srgba(0.20, 0.90, 1.00, 0.32),
        );
    }
    if let Some((center, scale)) = selection_bounds(&target, IVec3::ZERO) {
        let radius = scale.max_element().max(1.0) * 0.65;
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.14)),
            Color::srgba(1.00, 0.72, 0.18, 0.58 + pulse * 0.25),
        );
        gizmos.circle(
            pivot,
            drag.axis.direction(),
            radius,
            Color::srgba(1.00, 0.72, 0.18, 0.72 + pulse * 0.22),
        );
        gizmos.line(
            pivot - drag.axis.vector() * radius,
            pivot + drag.axis.vector() * radius,
            Color::srgb(1.00, 0.82, 0.28),
        );
    }
}

pub fn begin_scale_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    tool_controller: Res<ToolController>,
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
    mut drag: ResMut<SemanticScaleDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if drag.active
        || !mouse.just_pressed(MouseButton::Left)
        || !scale_tool_active(&mode, &tool_controller)
        || ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui)
    {
        return;
    }

    let selection = tool_controller.selection().clone();
    let cells = selection_cells(&sketch_links, &selection);
    let Some(anchor_cell) = selection_min_cell(&cells) else {
        toolbelt.status = "Scale: select a drawn object first.".into();
        return;
    };

    drag.active = true;
    drag.motion_x = 0.0;
    drag.factor = 1;
    drag.anchor_cell = anchor_cell;
    drag.selection = selection;
    drag.cells = cells;
    drag.tool_generation = tool_controller.tool_generation();
    gesture_lock.lock(SCALE_OWNER);
    toolbelt.status =
        "Scale: drag right to enlarge x2..x8 or left to shrink to 1/2..1/8. RMB orbits; Escape cancels."
            .into();
}

pub fn update_scale_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    mut drag: ResMut<SemanticScaleDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut tool_controller: ResMut<ToolController>,
) {
    if !drag.active {
        motion_evr.clear();
        gesture_lock.release(SCALE_OWNER);
        return;
    }

    gesture_lock.lock(SCALE_OWNER);
    let tool_changed = drag.tool_generation != tool_controller.tool_generation();
    if transform_drag_should_cancel(keys.just_pressed(KeyCode::Escape), tool_changed) {
        drag.clear();
        gesture_lock.release(SCALE_OWNER);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        toolbelt.status = "Scale cancelled.".into();
        motion_evr.clear();
        return;
    }

    if !transform_drag_accepts_motion(mouse.pressed(MouseButton::Right)) {
        motion_evr.clear();
        toolbelt.status = "Scale held while orbiting. Release RMB to resume.".into();
        return;
    }

    drag.motion_x += motion_evr.read().map(|event| event.delta.x).sum::<f32>();
    let next_factor = snapped_scale_factor(drag.motion_x);
    if next_factor != drag.factor {
        drag.factor = next_factor;
        toolbelt.status = format!(
            "Scale {}: {} source cells, anchored to the minimum voxel corner. Release to commit.",
            scale_factor_label(drag.factor),
            drag.cells.len()
        );
    }
}

pub fn end_scale_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut drag: ResMut<SemanticScaleDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
) {
    if !drag.active || !mouse.just_released(MouseButton::Left) {
        return;
    }

    let scaled = commit_selection_voxel_scale(
        &mut world,
        &mut history,
        &mut sketch_doc,
        &mut sketch_links,
        &drag.selection,
        drag.anchor_cell,
        drag.factor,
        "Scale selection",
    );
    if scaled > 0 {
        toolbelt.status = format!(
            "Scale committed: {} voxels at {}.",
            scaled,
            scale_factor_label(drag.factor)
        );
    } else {
        toolbelt.status = "Scale cancelled: factor unchanged or output too large.".into();
    }
    drag.clear();
    gesture_lock.release(SCALE_OWNER);
}

pub fn draw_scale_gizmo(drag: Res<SemanticScaleDrag>, mut gizmos: Gizmos, time: Res<Time>) {
    if !drag.active || drag.cells.is_empty() {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let target =
        scale_destination_cells(&drag.cells, drag.anchor_cell, drag.factor).unwrap_or_default();
    if let Some((center, scale)) = selection_bounds(&drag.cells, IVec3::ZERO) {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.04)),
            Color::srgba(0.20, 0.90, 1.00, 0.32),
        );
    }
    if let Some((center, scale)) = selection_bounds(&target, IVec3::ZERO) {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.14)),
            Color::srgba(0.42, 1.00, 0.48, 0.55 + pulse * 0.28),
        );
    }
    gizmos.sphere(
        drag.anchor_cell.as_vec3(),
        Quat::IDENTITY,
        0.18 + pulse * 0.09,
        Color::srgb(0.42, 1.00, 0.48),
    );
}

fn rotate_tool_active(mode: &ModeContext, tool_controller: &ToolController) -> bool {
    mode.is_build_live() && tool_controller.active_tool() == EditorToolId::Rotate
}

fn scale_tool_active(mode: &ModeContext, tool_controller: &ToolController) -> bool {
    mode.is_build_live() && tool_controller.active_tool() == EditorToolId::Scale
}

fn transform_drag_accepts_motion(right_held: bool) -> bool {
    !right_held
}

fn transform_drag_should_cancel(escape_pressed: bool, tool_changed: bool) -> bool {
    escape_pressed || tool_changed
}

fn move_tool_active(mode: &ModeContext, tool_controller: &ToolController) -> bool {
    mode.is_build_live() && tool_controller.active_tool() == EditorToolId::Move
}

fn move_should_cancel_for_tool_selection(drag: &SemanticMoveDrag, generation: u64) -> bool {
    drag.active && drag.tool_generation != generation
}

fn move_drag_should_cancel(escape_pressed: bool, tool_changed: bool) -> bool {
    escape_pressed || tool_changed
}

fn move_copy_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight)
}

fn move_copy_count_from_key(key: KeyCode) -> Option<usize> {
    match key {
        KeyCode::Digit2 => Some(2),
        KeyCode::Digit3 => Some(3),
        KeyCode::Digit4 => Some(4),
        KeyCode::Digit5 => Some(5),
        KeyCode::Digit6 => Some(6),
        KeyCode::Digit7 => Some(7),
        KeyCode::Digit8 => Some(8),
        KeyCode::Digit9 => Some(9),
        _ => None,
    }
}

fn move_grip_cell(cells: &[IVec3], hovered: Option<IVec3>) -> Option<IVec3> {
    let first = cells.first().copied()?;
    if let Some(hovered) = hovered {
        if cells.iter().any(|cell| *cell == hovered) {
            return Some(hovered);
        }
    }
    Some(first)
}

fn move_grip_reference_point(
    sketch_doc: &crate::sketch_model::SketchDocument,
    selection: &crate::sketch_model::SelectionSet,
    hover: Option<&crate::sketch_model::HitRecord>,
    fallback_cell: Option<IVec3>,
) -> Option<Vec3> {
    if let Some(hit) = hover.filter(|hit| selection.contains(hit.entity)) {
        return move_reference_point_from_hit(sketch_doc, hit).or(Some(hit.world_point));
    }
    fallback_cell.map(cell_center)
}

fn move_delta_from_reference_hit(
    grip_point: Option<Vec3>,
    sketch_doc: &crate::sketch_model::SketchDocument,
    hover: Option<&crate::sketch_model::HitRecord>,
    axis_lock: Option<MoveAxisLock>,
) -> Option<IVec3> {
    let target = move_reference_point_from_hit(sketch_doc, hover?)?;
    move_delta_from_reference_points(grip_point, Some(target), axis_lock)
}

fn move_reference_point_from_hit(
    sketch_doc: &crate::sketch_model::SketchDocument,
    hit: &crate::sketch_model::HitRecord,
) -> Option<Vec3> {
    let mut candidates = sketch_doc.entity_inference_candidates(hit.entity).ok()?;
    candidates.extend(
        crate::sketch_model::InferenceService::from_pick(sketch_doc, hit, Some(hit.world_point))
            .ok()?,
    );
    candidates
        .into_iter()
        .filter(|candidate| move_reference_kind_bias(candidate.kind).is_some())
        .filter(|candidate| candidate.point.is_finite())
        .filter_map(|candidate| {
            let distance = candidate.point.distance(hit.world_point);
            (distance <= 2.0).then_some((
                candidate.point,
                distance + move_reference_kind_bias(candidate.kind).unwrap_or(1.0),
            ))
        })
        .min_by(|(_, score_a), (_, score_b)| score_a.total_cmp(score_b))
        .map(|(point, _)| point)
}

fn move_reference_kind_bias(kind: crate::sketch_model::InferenceKind) -> Option<f32> {
    match kind {
        crate::sketch_model::InferenceKind::Endpoint => Some(0.0),
        crate::sketch_model::InferenceKind::Midpoint => Some(0.08),
        crate::sketch_model::InferenceKind::FaceCenter => Some(0.16),
        crate::sketch_model::InferenceKind::OnEdge => Some(0.22),
        crate::sketch_model::InferenceKind::OnFace => Some(0.34),
        _ => None,
    }
}

fn move_delta_from_reference_points(
    grip: Option<Vec3>,
    target: Option<Vec3>,
    axis_lock: Option<MoveAxisLock>,
) -> Option<IVec3> {
    let raw = target? - grip?;
    if !raw.is_finite() {
        return None;
    }
    let delta = IVec3::new(
        round_move_delta_component(raw.x),
        round_move_delta_component(raw.y),
        round_move_delta_component(raw.z),
    );
    let delta = apply_move_axis_lock(delta, axis_lock);
    (delta != IVec3::ZERO).then_some(delta)
}

fn move_delta_from_hover_cell(
    grip: Option<IVec3>,
    hover: Option<IVec3>,
    axis_lock: Option<MoveAxisLock>,
) -> Option<IVec3> {
    let delta = apply_move_axis_lock(hover? - grip?, axis_lock);
    (delta != IVec3::ZERO).then_some(delta)
}

fn move_delta_from_snap_target(
    reference_delta: Option<IVec3>,
    hover_delta: Option<IVec3>,
    mouse_delta: Option<IVec3>,
) -> Option<IVec3> {
    reference_delta.or(hover_delta).or(mouse_delta)
}

fn move_drag_accepts_motion(right_held: bool) -> bool {
    !right_held
}

fn apply_move_axis_lock(delta: IVec3, axis_lock: Option<MoveAxisLock>) -> IVec3 {
    match axis_lock {
        Some(MoveAxisLock::X) => IVec3::new(delta.x, 0, 0),
        Some(MoveAxisLock::Y) => IVec3::new(0, delta.y, 0),
        Some(MoveAxisLock::Z) => IVec3::new(0, 0, delta.z),
        None => delta,
    }
}

pub fn snapped_move_delta(motion: Vec2, axis_lock: Option<MoveAxisLock>) -> IVec3 {
    let step_x = snapped_steps(motion.x);
    let step_y = snapped_steps(-motion.y);
    match axis_lock {
        Some(MoveAxisLock::X) => IVec3::new(step_x, 0, 0),
        Some(MoveAxisLock::Y) => IVec3::new(0, step_y, 0),
        Some(MoveAxisLock::Z) => IVec3::new(0, 0, step_x),
        None if motion.y.abs() > motion.x.abs() * 1.2 => IVec3::new(0, step_y, 0),
        None => IVec3::new(step_x, 0, 0),
    }
}

fn snapped_steps(pixels: f32) -> i32 {
    (pixels / MOVE_PIXELS_PER_VOXEL)
        .round()
        .clamp(-(MOVE_DELTA_LIMIT as f32), MOVE_DELTA_LIMIT as f32) as i32
}

fn round_move_delta_component(value: f32) -> i32 {
    value
        .round()
        .clamp(-(MOVE_DELTA_LIMIT as f32), MOVE_DELTA_LIMIT as f32) as i32
}

fn cell_center(cell: IVec3) -> Vec3 {
    cell.as_vec3() + Vec3::splat(0.5)
}

fn selection_cells(
    links: &crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
) -> Vec<IVec3> {
    let mut cells = HashSet::new();
    for entity in selection.ordered() {
        cells.extend(links.cells_for_entity(*entity));
    }
    let mut cells: Vec<_> = cells.into_iter().collect();
    cells.sort_unstable_by_key(|cell| (cell.x, cell.y, cell.z));
    cells
}

fn selection_source_voxels(
    world: &VoxelWorld,
    links: &crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
) -> HashMap<IVec3, Voxel> {
    let mut sources = HashMap::<IVec3, Voxel>::new();
    for cell in selection_cells(links, selection) {
        let voxel = world.voxel_at(cell.x, cell.y, cell.z);
        if voxel != AIR {
            sources.insert(cell, voxel);
        }
    }
    sources
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticDeleteSummary {
    voxel_count: usize,
    shared_voxel_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticDeleteError {
    NoSelection,
    LockedOrHidden,
    NoLinkedGeometry,
    TooLarge,
    AlreadyHidden,
    ModelRejected,
    HistoryRejected,
}

fn selection_entities_are_editable(
    sketch_doc: &crate::sketch_model::SketchDocument,
    selection: &crate::sketch_model::SelectionSet,
) -> bool {
    !selection.is_empty()
        && selection.ordered().iter().all(|entity| {
            sketch_doc.entity(*entity).is_some_and(|record| {
                !record.locked
                    && sketch_doc
                        .entity_effective_visible(*entity)
                        .unwrap_or(false)
            })
        })
}

fn commit_semantic_selection_delete(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    label: &str,
) -> Result<SemanticDeleteSummary, SemanticDeleteError> {
    if selection.is_empty() {
        return Err(SemanticDeleteError::NoSelection);
    }
    if !selection_entities_are_editable(sketch_doc, selection) {
        return Err(SemanticDeleteError::LockedOrHidden);
    }

    let before_links = sketch_links.snapshot_entities(selection.ordered().iter().copied());
    if before_links
        .iter()
        .all(|snapshot| snapshot.cell_links.is_empty() && snapshot.face_links.is_empty())
    {
        return Err(SemanticDeleteError::NoLinkedGeometry);
    }

    let selected: HashSet<_> = selection.ordered().iter().copied().collect();
    let mut shared_voxel_count = 0_usize;
    let mut changes = Vec::new();
    for cell in selection_cells(sketch_links, selection) {
        let exclusively_selected = sketch_links
            .links_for_cell(cell)
            .iter()
            .all(|link| selected.contains(&link.entity));
        if !exclusively_selected {
            shared_voxel_count += 1;
            continue;
        }
        let before = world.voxel_at(cell.x, cell.y, cell.z);
        if before != AIR {
            changes.push((cell, before, AIR));
        }
    }
    if changes.len() > TRANSFORM_OUTPUT_LIMIT {
        return Err(SemanticDeleteError::TooLarge);
    }

    let after_links = before_links
        .iter()
        .map(|snapshot| SketchVoxelEntityLinkSnapshot {
            entity: snapshot.entity,
            cell_links: Vec::new(),
            face_links: Vec::new(),
        })
        .collect();
    let document_before = sketch_doc.clone();
    let summary = sketch_doc
        .set_selection_visible(selection, false, label.to_string())
        .map_err(|_| SemanticDeleteError::ModelRejected)?;
    if summary.entity_count == 0 {
        *sketch_doc = document_before;
        return Err(SemanticDeleteError::AlreadyHidden);
    }
    let outcome = history.record_external_with_sketch_meta_checked(
        label,
        changes.clone(),
        Some(BuilderHistorySketchMeta::SketchTransform {
            before_links,
            after_links,
            document_steps: 1,
        }),
    );
    if !matches!(outcome, BuilderHistoryRecordOutcome::Recorded { .. }) {
        *sketch_doc = document_before;
        return Err(SemanticDeleteError::HistoryRejected);
    }

    apply_voxel_transform_changes(world, &changes, true);
    sketch_links.remove_entities(selection.ordered().iter().copied());
    Ok(SemanticDeleteSummary {
        voxel_count: changes.len(),
        shared_voxel_count,
    })
}

pub fn delete_semantic_selection_input(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
    mut tool_controller: ResMut<ToolController>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if !mode.is_build_live()
        || tool_controller.active_tool() != EditorToolId::Select
        || ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui)
        || !(keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace))
    {
        return;
    }

    let selection = tool_controller.selection().clone();
    if selection.is_empty() {
        toolbelt.status = if tool_controller.edit_object_active() {
            "EDIT OBJECT: select a part before pressing Delete.".into()
        } else {
            "Select an object before pressing Delete.".into()
        };
        return;
    }

    let delete_label = if tool_controller.edit_object_active() {
        "Delete object part"
    } else {
        "Delete object"
    };
    let summary = match commit_semantic_selection_delete(
        &mut world,
        &mut history,
        &mut sketch_doc,
        &mut sketch_links,
        &selection,
        delete_label,
    ) {
        Ok(summary) => summary,
        Err(SemanticDeleteError::LockedOrHidden) => {
            toolbelt.status =
                "Delete held: locked or hidden object parts are protected from editing.".into();
            return;
        }
        Err(SemanticDeleteError::TooLarge | SemanticDeleteError::HistoryRejected) => {
            toolbelt.status =
                "Delete cancelled safely because the complete edit did not fit undo history."
                    .into();
            return;
        }
        Err(SemanticDeleteError::AlreadyHidden) => {
            toolbelt.status = "Delete held: the selected object was already hidden.".into();
            return;
        }
        Err(SemanticDeleteError::ModelRejected) => {
            toolbelt.status =
                "Delete cancelled safely because the object model rejected it.".into();
            return;
        }
        Err(SemanticDeleteError::NoSelection | SemanticDeleteError::NoLinkedGeometry) => {
            toolbelt.status = "Delete held: the selection has no linked object geometry.".into();
            return;
        }
    };
    let removed = summary.voxel_count;
    if tool_controller.edit_object_active() {
        tool_controller.remove_selection_from_edit_object();
        toolbelt.status = if tool_controller.edit_object_active() {
            format!(
                "EDIT OBJECT: removed {removed} exclusive voxels and detached {} shared voxels as one undoable part edit. Select another part or press Escape.",
                summary.shared_voxel_count
            )
        } else {
            "EDIT OBJECT: the final part was removed as one undoable edit. Select another object."
                .into()
        };
    } else {
        tool_controller.clear_selection();
        toolbelt.status =
            format!("Deleted the complete object ({removed} voxels) as one undoable edit.");
    }
}

pub fn draw_semantic_selection_gizmo(
    mode: Res<ModeContext>,
    tool_controller: Res<ToolController>,
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
    time: Res<Time>,
    mut gizmos: Gizmos,
) {
    if !mode.is_build_live() {
        return;
    }
    let pulse = 0.72 + 0.28 * (time.elapsed_seconds() * 4.0).sin().abs();
    if let Some(members) = tool_controller.edit_object_members() {
        let object_cells = selection_cells(&sketch_links, members);
        if let Some((center, scale)) = selection_bounds(&object_cells, IVec3::ZERO) {
            gizmos.cuboid(
                Transform::from_translation(center).with_scale(scale + Vec3::splat(0.08)),
                Color::srgba(0.18, 0.86, 1.0, 0.28),
            );
        }
        let part_cells = selection_cells(&sketch_links, tool_controller.selection());
        if let Some((center, scale)) = selection_bounds(&part_cells, IVec3::ZERO) {
            gizmos.cuboid(
                Transform::from_translation(center).with_scale(scale + Vec3::splat(0.18)),
                Color::srgba(1.0, 0.70, 0.16, pulse),
            );
        }
        return;
    }

    let object_cells = selection_cells(&sketch_links, tool_controller.selection());
    if let Some((center, scale)) = selection_bounds(&object_cells, IVec3::ZERO) {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.14)),
            Color::srgba(0.20, 0.92, 1.0, pulse),
        );
    }
}

fn normalized_quarter_turns(turns: i32) -> i32 {
    match turns.rem_euclid(4) {
        0 => 0,
        1 => 1,
        2 => 2,
        3 => -1,
        _ => unreachable!(),
    }
}

fn snapped_quarter_turns(motion_x: f32) -> i32 {
    (motion_x / ROTATE_PIXELS_PER_QUARTER)
        .round()
        .clamp(-4.0, 4.0) as i32
}

fn snapped_scale_factor(motion_x: f32) -> i32 {
    let steps = (motion_x / SCALE_PIXELS_PER_STEP).round().clamp(
        -((SCALE_FACTOR_MAX - 1) as f32),
        (SCALE_FACTOR_MAX - 1) as f32,
    ) as i32;
    match steps.cmp(&0) {
        std::cmp::Ordering::Less => steps - 1,
        std::cmp::Ordering::Equal => 1,
        std::cmp::Ordering::Greater => steps + 1,
    }
}

fn scale_factor_label(factor: i32) -> String {
    if factor < -1 {
        format!("1/{}", factor.unsigned_abs())
    } else {
        format!("x{factor}")
    }
}

fn selection_pivot_twice(cells: &[IVec3]) -> Option<IVec3> {
    let first = *cells.first()?;
    let mut min = first;
    let mut max = first;
    for cell in cells.iter().copied().skip(1) {
        min = min.min(cell);
        max = max.max(cell);
    }
    checked_ivec3(
        i64::from(min.x) + i64::from(max.x) + 1,
        i64::from(min.y) + i64::from(max.y) + 1,
        i64::from(min.z) + i64::from(max.z) + 1,
    )
}

fn selection_min_cell(cells: &[IVec3]) -> Option<IVec3> {
    let first = *cells.first()?;
    Some(
        cells
            .iter()
            .copied()
            .skip(1)
            .fold(first, |min, cell| min.min(cell)),
    )
}

fn checked_ivec3(x: i64, y: i64, z: i64) -> Option<IVec3> {
    Some(IVec3::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        i32::try_from(z).ok()?,
    ))
}

fn checked_add_ivec3(a: IVec3, b: IVec3) -> Option<IVec3> {
    checked_ivec3(
        i64::from(a.x) + i64::from(b.x),
        i64::from(a.y) + i64::from(b.y),
        i64::from(a.z) + i64::from(b.z),
    )
}

fn rotate_vector_quarter_i64(vector: [i64; 3], axis: TransformAxis, turns: i32) -> [i64; 3] {
    let mut rotated = vector;
    for _ in 0..turns.rem_euclid(4) {
        rotated = match axis {
            TransformAxis::X => [rotated[0], -rotated[2], rotated[1]],
            TransformAxis::Y => [rotated[2], rotated[1], -rotated[0]],
            TransformAxis::Z => [-rotated[1], rotated[0], rotated[2]],
        };
    }
    rotated
}

fn rotation_lattice_offset_twice(pivot_twice: IVec3, axis: TransformAxis, turns: i32) -> IVec3 {
    if normalized_quarter_turns(turns) == 0 {
        return IVec3::ZERO;
    }

    // Every voxel centre has odd doubled coordinates. For a fixed pivot and
    // quarter turn, all transformed centres share the same parity per axis,
    // so one uniform half-cell correction preserves the complete shape.
    let pivot = [
        i64::from(pivot_twice.x),
        i64::from(pivot_twice.y),
        i64::from(pivot_twice.z),
    ];
    let sample_center = [1_i64, 1_i64, 1_i64];
    let delta = [
        sample_center[0] - pivot[0],
        sample_center[1] - pivot[1],
        sample_center[2] - pivot[2],
    ];
    let rotated = rotate_vector_quarter_i64(delta, axis, turns);
    let raw = [
        pivot[0] + rotated[0],
        pivot[1] + rotated[1],
        pivot[2] + rotated[2],
    ];
    IVec3::new(
        i32::from(raw[0].rem_euclid(2) == 0),
        i32::from(raw[1].rem_euclid(2) == 0),
        i32::from(raw[2].rem_euclid(2) == 0),
    )
}

fn rotate_cell_quarter(
    cell: IVec3,
    pivot_twice: IVec3,
    axis: TransformAxis,
    turns: i32,
) -> Option<IVec3> {
    if normalized_quarter_turns(turns) == 0 {
        return Some(cell);
    }
    let pivot = [
        i64::from(pivot_twice.x),
        i64::from(pivot_twice.y),
        i64::from(pivot_twice.z),
    ];
    let center = [
        i64::from(cell.x) * 2 + 1,
        i64::from(cell.y) * 2 + 1,
        i64::from(cell.z) * 2 + 1,
    ];
    let delta = [
        center[0] - pivot[0],
        center[1] - pivot[1],
        center[2] - pivot[2],
    ];
    let rotated = rotate_vector_quarter_i64(delta, axis, turns);
    let correction = rotation_lattice_offset_twice(pivot_twice, axis, turns);
    let corrected_center = [
        pivot[0] + rotated[0] + i64::from(correction.x),
        pivot[1] + rotated[1] + i64::from(correction.y),
        pivot[2] + rotated[2] + i64::from(correction.z),
    ];
    if corrected_center
        .iter()
        .any(|coordinate| coordinate.rem_euclid(2) != 1)
    {
        return None;
    }
    checked_ivec3(
        (corrected_center[0] - 1) / 2,
        (corrected_center[1] - 1) / 2,
        (corrected_center[2] - 1) / 2,
    )
}

fn rotate_face_normal_quarter(normal: IVec3, axis: TransformAxis, turns: i32) -> Option<IVec3> {
    let rotated = rotate_vector_quarter_i64(
        [
            i64::from(normal.x),
            i64::from(normal.y),
            i64::from(normal.z),
        ],
        axis,
        turns,
    );
    checked_ivec3(rotated[0], rotated[1], rotated[2])
}

fn scale_cell_base(cell: IVec3, anchor: IVec3, factor: i32) -> Option<IVec3> {
    if factor < 1 {
        return None;
    }
    checked_ivec3(
        i64::from(anchor.x) + (i64::from(cell.x) - i64::from(anchor.x)) * i64::from(factor),
        i64::from(anchor.y) + (i64::from(cell.y) - i64::from(anchor.y)) * i64::from(factor),
        i64::from(anchor.z) + (i64::from(cell.z) - i64::from(anchor.z)) * i64::from(factor),
    )
}

fn expanded_scale_cells(cell: IVec3, anchor: IVec3, factor: i32) -> Option<Vec<IVec3>> {
    let base = scale_cell_base(cell, anchor, factor)?;
    let side = usize::try_from(factor).ok()?;
    let output_count = side.checked_mul(side)?.checked_mul(side)?;
    let mut output = Vec::with_capacity(output_count);
    for y in 0..factor {
        for z in 0..factor {
            for x in 0..factor {
                output.push(checked_add_ivec3(base, IVec3::new(x, y, z))?);
            }
        }
    }
    Some(output)
}

fn shrink_scale_cell(cell: IVec3, anchor: IVec3, divisor: i32) -> Option<IVec3> {
    if !(2..=SCALE_FACTOR_MAX).contains(&divisor) {
        return None;
    }
    let divisor = i64::from(divisor);
    checked_ivec3(
        i64::from(anchor.x) + (i64::from(cell.x) - i64::from(anchor.x)).div_euclid(divisor),
        i64::from(anchor.y) + (i64::from(cell.y) - i64::from(anchor.y)).div_euclid(divisor),
        i64::from(anchor.z) + (i64::from(cell.z) - i64::from(anchor.z)).div_euclid(divisor),
    )
}

fn shrink_divisor(factor: i32) -> Option<i32> {
    (-SCALE_FACTOR_MAX..=-2)
        .contains(&factor)
        .then_some(factor.unsigned_abs() as i32)
}

fn scale_factor_is_valid(factor: i32) -> bool {
    (2..=SCALE_FACTOR_MAX).contains(&factor) || shrink_divisor(factor).is_some()
}

fn scale_destination_cells(cells: &[IVec3], anchor: IVec3, factor: i32) -> Option<Vec<IVec3>> {
    if !scale_factor_is_valid(factor) {
        return None;
    }
    let expected = if factor > 1 {
        let factor = usize::try_from(factor).ok()?;
        cells
            .len()
            .checked_mul(factor.checked_mul(factor)?.checked_mul(factor)?)?
    } else {
        cells.len()
    };
    if expected > TRANSFORM_OUTPUT_LIMIT {
        return None;
    }

    let mut output = HashSet::with_capacity(expected);
    for cell in cells {
        if factor > 1 {
            output.extend(expanded_scale_cells(*cell, anchor, factor)?);
        } else {
            output.insert(shrink_scale_cell(*cell, anchor, shrink_divisor(factor)?)?);
        }
        if output.len() > TRANSFORM_OUTPUT_LIMIT {
            return None;
        }
    }
    let mut output: Vec<_> = output.into_iter().collect();
    output.sort_unstable_by_key(|cell| (cell.x, cell.y, cell.z));
    Some(output)
}

fn selection_is_transformable(
    sketch_doc: &crate::sketch_model::SketchDocument,
    sketch_links: &crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    cells: &[IVec3],
) -> bool {
    let selected: HashSet<_> = selection.ordered().iter().copied().collect();
    if selected.is_empty()
        || selection.ordered().iter().any(|entity| {
            sketch_doc.entity(*entity).is_none_or(|record| {
                record.locked
                    || !sketch_doc
                        .entity_effective_visible(*entity)
                        .unwrap_or(false)
                    || matches!(
                        record.kind,
                        crate::sketch_model::SketchEntityKind::Group { .. }
                    )
            })
        })
    {
        return false;
    }

    cells.iter().all(|cell| {
        sketch_links
            .links_for_cell(*cell)
            .iter()
            .all(|link| selected.contains(&link.entity))
    })
}

fn destination_cells_are_available(
    sketch_links: &crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    cells: impl IntoIterator<Item = IVec3>,
) -> bool {
    let selected: HashSet<_> = selection.ordered().iter().copied().collect();
    cells.into_iter().all(|cell| {
        sketch_links
            .links_for_cell(cell)
            .iter()
            .all(|link| selected.contains(&link.entity))
    })
}

fn sort_and_dedup_cell_links(links: &mut Vec<(IVec3, crate::sketch_model::SketchVoxelLink)>) {
    links.sort_unstable_by_key(|(cell, link)| (cell.x, cell.y, cell.z, *link));
    links.dedup();
}

fn sort_and_dedup_face_links(
    links: &mut Vec<(IVec3, IVec3, crate::sketch_model::SketchVoxelLink)>,
) {
    links.sort_unstable_by_key(|(cell, normal, link)| {
        (cell.x, cell.y, cell.z, normal.x, normal.y, normal.z, *link)
    });
    links.dedup();
}

fn rotate_link_snapshots(
    snapshots: &[SketchVoxelEntityLinkSnapshot],
    pivot_twice: IVec3,
    axis: TransformAxis,
    turns: i32,
) -> Option<Vec<SketchVoxelEntityLinkSnapshot>> {
    let mut transformed = Vec::with_capacity(snapshots.len());
    let mut entry_count = 0_usize;
    for snapshot in snapshots {
        let mut cell_links = Vec::with_capacity(snapshot.cell_links.len());
        for (cell, link) in &snapshot.cell_links {
            cell_links.push((rotate_cell_quarter(*cell, pivot_twice, axis, turns)?, *link));
        }
        sort_and_dedup_cell_links(&mut cell_links);

        let mut face_links = Vec::with_capacity(snapshot.face_links.len());
        for (cell, normal, link) in &snapshot.face_links {
            face_links.push((
                rotate_cell_quarter(*cell, pivot_twice, axis, turns)?,
                rotate_face_normal_quarter(*normal, axis, turns)?,
                *link,
            ));
        }
        sort_and_dedup_face_links(&mut face_links);
        entry_count = entry_count
            .checked_add(cell_links.len())?
            .checked_add(face_links.len())?;
        if entry_count > TRANSFORM_OUTPUT_LIMIT {
            return None;
        }
        transformed.push(SketchVoxelEntityLinkSnapshot {
            entity: snapshot.entity,
            cell_links,
            face_links,
        });
    }
    Some(transformed)
}

fn translate_link_snapshots(
    snapshots: &[SketchVoxelEntityLinkSnapshot],
    delta: IVec3,
) -> Option<Vec<SketchVoxelEntityLinkSnapshot>> {
    let mut transformed = Vec::with_capacity(snapshots.len());
    let mut entry_count = 0_usize;
    for snapshot in snapshots {
        let mut cell_links = Vec::with_capacity(snapshot.cell_links.len());
        for (cell, link) in &snapshot.cell_links {
            cell_links.push((checked_add_ivec3(*cell, delta)?, *link));
        }
        let mut face_links = Vec::with_capacity(snapshot.face_links.len());
        for (cell, normal, link) in &snapshot.face_links {
            face_links.push((checked_add_ivec3(*cell, delta)?, *normal, *link));
        }
        entry_count = entry_count
            .checked_add(cell_links.len())?
            .checked_add(face_links.len())?;
        if entry_count > TRANSFORM_OUTPUT_LIMIT {
            return None;
        }
        transformed.push(SketchVoxelEntityLinkSnapshot {
            entity: snapshot.entity,
            cell_links,
            face_links,
        });
    }
    Some(transformed)
}

fn scaled_face_cells(cell: IVec3, normal: IVec3, anchor: IVec3, factor: i32) -> Option<Vec<IVec3>> {
    if !matches!(
        normal,
        IVec3::X | IVec3::NEG_X | IVec3::Y | IVec3::NEG_Y | IVec3::Z | IVec3::NEG_Z
    ) {
        return None;
    }
    let base = scale_cell_base(cell, anchor, factor)?;
    let mut cells = Vec::with_capacity(usize::try_from(factor.checked_mul(factor)?).ok()?);
    for y in 0..factor {
        for z in 0..factor {
            for x in 0..factor {
                let on_face = (normal == IVec3::X && x == factor - 1)
                    || (normal == IVec3::NEG_X && x == 0)
                    || (normal == IVec3::Y && y == factor - 1)
                    || (normal == IVec3::NEG_Y && y == 0)
                    || (normal == IVec3::Z && z == factor - 1)
                    || (normal == IVec3::NEG_Z && z == 0);
                if on_face {
                    cells.push(checked_add_ivec3(base, IVec3::new(x, y, z))?);
                }
            }
        }
    }
    Some(cells)
}

fn scale_link_snapshots(
    snapshots: &[SketchVoxelEntityLinkSnapshot],
    anchor: IVec3,
    factor: i32,
) -> Option<Vec<SketchVoxelEntityLinkSnapshot>> {
    let mut transformed = Vec::with_capacity(snapshots.len());
    let mut entry_count = 0_usize;
    for snapshot in snapshots {
        let mut cell_links = Vec::new();
        for (cell, link) in &snapshot.cell_links {
            if factor > 1 {
                for target in expanded_scale_cells(*cell, anchor, factor)? {
                    cell_links.push((target, *link));
                }
            } else {
                cell_links.push((
                    shrink_scale_cell(*cell, anchor, shrink_divisor(factor)?)?,
                    *link,
                ));
            }
        }
        sort_and_dedup_cell_links(&mut cell_links);

        let mut face_links = Vec::new();
        for (cell, normal, link) in &snapshot.face_links {
            if factor > 1 {
                for target in scaled_face_cells(*cell, *normal, anchor, factor)? {
                    face_links.push((target, *normal, *link));
                }
            } else {
                face_links.push((
                    shrink_scale_cell(*cell, anchor, shrink_divisor(factor)?)?,
                    *normal,
                    *link,
                ));
            }
        }
        sort_and_dedup_face_links(&mut face_links);
        entry_count = entry_count
            .checked_add(cell_links.len())?
            .checked_add(face_links.len())?;
        if entry_count > TRANSFORM_OUTPUT_LIMIT {
            return None;
        }
        transformed.push(SketchVoxelEntityLinkSnapshot {
            entity: snapshot.entity,
            cell_links,
            face_links,
        });
    }
    Some(transformed)
}

fn planned_voxel_transform_changes(
    world: &VoxelWorld,
    sources: &HashMap<IVec3, Voxel>,
    destinations: &HashMap<IVec3, Voxel>,
) -> Option<Vec<(IVec3, Voxel, Voxel)>> {
    let mut final_values = HashMap::with_capacity(sources.len() + destinations.len());
    for cell in sources.keys() {
        final_values.insert(*cell, AIR);
    }
    for (cell, voxel) in destinations {
        final_values.insert(*cell, *voxel);
    }
    if final_values.len() > TRANSFORM_OUTPUT_LIMIT {
        return None;
    }

    let mut final_values: Vec<_> = final_values.into_iter().collect();
    final_values.sort_unstable_by_key(|(cell, _)| (cell.x, cell.y, cell.z));
    let changes: Vec<_> = final_values
        .into_iter()
        .filter_map(|(cell, after)| {
            let before = world.voxel_at(cell.x, cell.y, cell.z);
            (before != after).then_some((cell, before, after))
        })
        .collect();
    (changes.len() <= TRANSFORM_OUTPUT_LIMIT).then_some(changes)
}

fn apply_voxel_transform_changes(
    world: &mut VoxelWorld,
    changes: &[(IVec3, Voxel, Voxel)],
    forward: bool,
) {
    let mut batch = WorldEditBatch::default();
    for (cell, before, after) in changes {
        let voxel = if forward { *after } else { *before };
        world.edit_set_voxel_batched(cell.x, cell.y, cell.z, voxel, &mut batch);
    }
    world.finish_edit_batch(batch);
}

fn rotate_voxel_destinations(
    sources: &HashMap<IVec3, Voxel>,
    pivot_twice: IVec3,
    axis: TransformAxis,
    turns: i32,
) -> Option<HashMap<IVec3, Voxel>> {
    let mut destinations = HashMap::with_capacity(sources.len());
    for (cell, voxel) in sources {
        let target = rotate_cell_quarter(*cell, pivot_twice, axis, turns)?;
        if destinations.insert(target, *voxel).is_some() {
            return None;
        }
    }
    Some(destinations)
}

fn scale_voxel_destinations(
    sources: &HashMap<IVec3, Voxel>,
    anchor: IVec3,
    factor: i32,
) -> Option<HashMap<IVec3, Voxel>> {
    if factor < -1 {
        let divisor = shrink_divisor(factor)?;
        let mut grouped = HashMap::<IVec3, Vec<(IVec3, Voxel)>>::new();
        let mut ordered_sources: Vec<_> = sources
            .iter()
            .map(|(cell, voxel)| (*cell, *voxel))
            .collect();
        ordered_sources.sort_unstable_by_key(|(cell, voxel)| (cell.x, cell.y, cell.z, *voxel));
        for (cell, voxel) in ordered_sources {
            let target = shrink_scale_cell(cell, anchor, divisor)?;
            grouped.entry(target).or_default().push((cell, voxel));
        }
        if grouped.len() > TRANSFORM_OUTPUT_LIMIT {
            return None;
        }
        let mut destinations = HashMap::with_capacity(grouped.len());
        for (target, candidates) in grouped {
            destinations.insert(
                target,
                choose_shrink_voxel(&candidates, target, anchor, divisor)?,
            );
        }
        return Some(destinations);
    }

    let factor_usize = usize::try_from(factor).ok()?;
    let expected = sources.len().checked_mul(
        factor_usize
            .checked_mul(factor_usize)?
            .checked_mul(factor_usize)?,
    )?;
    if expected > TRANSFORM_OUTPUT_LIMIT {
        return None;
    }
    let mut destinations = HashMap::with_capacity(expected);
    for (cell, voxel) in sources {
        for target in expanded_scale_cells(*cell, anchor, factor)? {
            if destinations.insert(target, *voxel).is_some() {
                return None;
            }
        }
    }
    Some(destinations)
}

fn choose_shrink_voxel(
    candidates: &[(IVec3, Voxel)],
    target: IVec3,
    anchor: IVec3,
    divisor: i32,
) -> Option<Voxel> {
    let divisor_i64 = i64::from(divisor);
    let bucket_min = checked_ivec3(
        i64::from(anchor.x) + (i64::from(target.x) - i64::from(anchor.x)) * divisor_i64,
        i64::from(anchor.y) + (i64::from(target.y) - i64::from(anchor.y)) * divisor_i64,
        i64::from(anchor.z) + (i64::from(target.z) - i64::from(anchor.z)) * divisor_i64,
    )?;
    let center_twice = [
        i64::from(bucket_min.x) * 2 + i64::from(divisor),
        i64::from(bucket_min.y) * 2 + i64::from(divisor),
        i64::from(bucket_min.z) * 2 + i64::from(divisor),
    ];
    let mut aggregates = BTreeMap::<Voxel, (usize, i128)>::new();
    for (cell, voxel) in candidates {
        let point_twice = [
            i64::from(cell.x) * 2 + 1,
            i64::from(cell.y) * 2 + 1,
            i64::from(cell.z) * 2 + 1,
        ];
        let distance = point_twice
            .iter()
            .zip(center_twice)
            .map(|(point, center)| (i128::from(*point) - i128::from(center)).pow(2))
            .sum();
        aggregates
            .entry(*voxel)
            .and_modify(|aggregate| {
                aggregate.0 += 1;
                aggregate.1 = aggregate.1.min(distance);
            })
            .or_insert((1, distance));
    }

    aggregates
        .into_iter()
        .max_by(
            |(voxel_a, (count_a, distance_a)), (voxel_b, (count_b, distance_b))| {
                shrink_voxel_priority(*voxel_a)
                    .cmp(&shrink_voxel_priority(*voxel_b))
                    .then_with(|| count_a.cmp(count_b))
                    .then_with(|| distance_b.cmp(distance_a))
                    .then_with(|| voxel_b.cmp(voxel_a))
            },
        )
        .map(|(voxel, _)| voxel)
}

fn shrink_voxel_priority(voxel: Voxel) -> u8 {
    match BlockType::from_voxel(voxel) {
        BlockType::Wood | BlockType::Bamboo => 4,
        block if block.is_solid() => 3,
        BlockType::Leaves | BlockType::JungleLeaves | BlockType::BlossomLeaves => 2,
        _ => 1,
    }
}

pub fn commit_selection_voxel_rotate(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    pivot_twice: IVec3,
    axis: TransformAxis,
    turns: i32,
    label: &str,
) -> usize {
    let turns = normalized_quarter_turns(turns);
    if selection.is_empty() || turns == 0 {
        return 0;
    }
    let cells = selection_cells(sketch_links, selection);
    if cells.is_empty() || !selection_is_transformable(sketch_doc, sketch_links, selection, &cells)
    {
        return 0;
    }
    let sources = selection_source_voxels(world, sketch_links, selection);
    if sources.is_empty() {
        return 0;
    }
    let Some(destinations) = rotate_voxel_destinations(&sources, pivot_twice, axis, turns) else {
        return 0;
    };
    if !destination_cells_are_available(sketch_links, selection, destinations.keys().copied()) {
        return 0;
    }
    let Some(changes) = planned_voxel_transform_changes(world, &sources, &destinations) else {
        return 0;
    };
    let before_links = sketch_links.snapshot_entities(selection.ordered().iter().copied());
    let Some(after_links) = rotate_link_snapshots(&before_links, pivot_twice, axis, turns) else {
        return 0;
    };

    let document_before = sketch_doc.clone();
    let links_before = sketch_links.clone();
    let pivot = pivot_twice.as_vec3() * 0.5;
    let offset = rotation_lattice_offset_twice(pivot_twice, axis, turns).as_vec3() * 0.5;
    let rotation = Quat::from_axis_angle(axis.vector(), turns as f32 * std::f32::consts::FRAC_PI_2);
    let Ok(summary) = sketch_doc.rotate_selection_about_pivot_with_offset(
        selection,
        pivot,
        rotation,
        offset,
        label.to_string(),
    ) else {
        return 0;
    };
    if summary.entity_count == 0 {
        *sketch_doc = document_before;
        return 0;
    }

    apply_voxel_transform_changes(world, &changes, true);
    sketch_links.restore_entity_snapshots(&after_links);
    let outcome = history.record_external_with_sketch_meta_checked(
        label.to_string(),
        changes.clone(),
        Some(BuilderHistorySketchMeta::SketchTransform {
            before_links,
            after_links,
            document_steps: 1,
        }),
    );
    if !matches!(outcome, BuilderHistoryRecordOutcome::Recorded { .. }) {
        apply_voxel_transform_changes(world, &changes, false);
        *sketch_doc = document_before;
        *sketch_links = links_before;
        return 0;
    }
    sources.len()
}

pub fn commit_selection_voxel_scale(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    anchor: IVec3,
    factor: i32,
    label: &str,
) -> usize {
    if selection.is_empty() || !scale_factor_is_valid(factor) {
        return 0;
    }
    let cells = selection_cells(sketch_links, selection);
    if cells.is_empty() || !selection_is_transformable(sketch_doc, sketch_links, selection, &cells)
    {
        return 0;
    }
    let sources = selection_source_voxels(world, sketch_links, selection);
    if sources.is_empty() {
        return 0;
    }
    let Some(destinations) = scale_voxel_destinations(&sources, anchor, factor) else {
        return 0;
    };
    if !destination_cells_are_available(sketch_links, selection, destinations.keys().copied()) {
        return 0;
    }
    let Some(changes) = planned_voxel_transform_changes(world, &sources, &destinations) else {
        return 0;
    };
    let before_links = sketch_links.snapshot_entities(selection.ordered().iter().copied());
    let Some(after_links) = scale_link_snapshots(&before_links, anchor, factor) else {
        return 0;
    };

    let document_before = sketch_doc.clone();
    let links_before = sketch_links.clone();
    let semantic_factor = if factor > 1 {
        factor as f32
    } else {
        1.0 / shrink_divisor(factor).expect("validated shrink factor") as f32
    };
    let Ok(summary) = sketch_doc.scale_selection_about_pivot(
        selection,
        anchor.as_vec3(),
        Vec3::splat(semantic_factor),
        label.to_string(),
    ) else {
        return 0;
    };
    if summary.entity_count == 0 {
        *sketch_doc = document_before;
        return 0;
    }

    apply_voxel_transform_changes(world, &changes, true);
    sketch_links.restore_entity_snapshots(&after_links);
    let outcome = history.record_external_with_sketch_meta_checked(
        label.to_string(),
        changes.clone(),
        Some(BuilderHistorySketchMeta::SketchTransform {
            before_links,
            after_links,
            document_steps: 1,
        }),
    );
    if !matches!(outcome, BuilderHistoryRecordOutcome::Recorded { .. }) {
        apply_voxel_transform_changes(world, &changes, false);
        *sketch_doc = document_before;
        *sketch_links = links_before;
        return 0;
    }
    destinations.len()
}

pub fn commit_selection_voxel_move(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    delta: IVec3,
    label: &str,
) -> usize {
    if selection.is_empty() || delta == IVec3::ZERO {
        return 0;
    }

    let cells = selection_cells(sketch_links, selection);
    if cells.is_empty() || !selection_is_transformable(sketch_doc, sketch_links, selection, &cells)
    {
        return 0;
    }

    let sources = selection_source_voxels(world, sketch_links, selection);
    if sources.is_empty() || sources.len() > TRANSFORM_OUTPUT_LIMIT {
        return 0;
    }

    let mut destinations = HashMap::<IVec3, Voxel>::new();
    for (cell, voxel) in &sources {
        let Some(target) = checked_add_ivec3(*cell, delta) else {
            return 0;
        };
        destinations.insert(target, *voxel);
    }
    if !destination_cells_are_available(sketch_links, selection, destinations.keys().copied()) {
        return 0;
    }
    let Some(changes) = planned_voxel_transform_changes(world, &sources, &destinations) else {
        return 0;
    };
    let before_links = sketch_links.snapshot_entities(selection.ordered().iter().copied());
    let Some(after_links) = translate_link_snapshots(&before_links, delta) else {
        return 0;
    };

    let document_before = sketch_doc.clone();
    let links_before = sketch_links.clone();
    let Ok(summary) = sketch_doc.move_selection(selection, delta.as_vec3(), label.to_string())
    else {
        return 0;
    };
    if summary.entity_count == 0 {
        *sketch_doc = document_before;
        return 0;
    }

    apply_voxel_transform_changes(world, &changes, true);
    sketch_links.restore_entity_snapshots(&after_links);
    let moved = sources.len();
    let outcome = history.record_external_with_sketch_meta_checked(
        label.to_string(),
        changes.clone(),
        Some(BuilderHistorySketchMeta::SketchTransform {
            before_links,
            after_links,
            document_steps: 1,
        }),
    );
    if !matches!(outcome, BuilderHistoryRecordOutcome::Recorded { .. }) {
        apply_voxel_transform_changes(world, &changes, false);
        *sketch_doc = document_before;
        *sketch_links = links_before;
        return 0;
    }
    moved
}

pub fn commit_selection_voxel_copy_array(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    delta: IVec3,
    copy_count: usize,
    label: impl Into<String>,
) -> usize {
    if selection.is_empty() || delta == IVec3::ZERO || copy_count == 0 {
        return 0;
    }

    let sources = selection_source_voxels(world, sketch_links, selection);
    if sources.is_empty() {
        return 0;
    }

    let label = label.into();
    let Ok(copied_entities) = sketch_doc.copy_selection_linear_array(
        selection,
        delta.as_vec3(),
        copy_count,
        label.clone(),
    ) else {
        return 0;
    };

    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::with_capacity(sources.len() * copy_count);
    for step in 1..=copy_count {
        let step_delta = delta * step as i32;
        for (cell, voxel) in &sources {
            let dest = *cell + step_delta;
            if let Some((before, after)) =
                world.edit_set_voxel_batched(dest.x, dest.y, dest.z, *voxel, &mut batch)
            {
                changes.push((dest, before, after));
            }
        }
    }
    world.finish_edit_batch(batch);
    link_linear_array_copies(sketch_links, selection, &copied_entities, delta, copy_count);
    let link_snapshots = sketch_links.snapshot_entities(copied_entities.iter().copied());
    history.record_external_with_sketch_meta(
        label,
        changes,
        Some(BuilderHistorySketchMeta::SketchCreated {
            link_snapshots,
            document_steps: 1,
        }),
    );
    sources.len() * copy_count
}

fn link_linear_array_copies(
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
    selection: &crate::sketch_model::SelectionSet,
    copied_entities: &[crate::sketch_model::SketchId],
    delta: IVec3,
    copy_count: usize,
) {
    let selected = selection.ordered();
    if selected.is_empty() || copied_entities.len() < selected.len() * copy_count {
        return;
    }
    let source_cells: Vec<_> = selected
        .iter()
        .map(|entity| (*entity, sketch_links.cells_for_entity(*entity)))
        .collect();
    for step in 1..=copy_count {
        let step_delta = delta * step as i32;
        for (entity_index, (source_entity, cells)) in source_cells.iter().enumerate() {
            let copy_entity = copied_entities[(step - 1) * selected.len() + entity_index];
            for source_cell in cells {
                let dest = *source_cell + step_delta;
                for link in sketch_links
                    .links_for_cell(*source_cell)
                    .into_iter()
                    .filter(|link| link.entity == *source_entity)
                {
                    sketch_links.link_cell(
                        dest,
                        crate::sketch_model::SketchVoxelLink {
                            entity: copy_entity,
                            ..link
                        },
                    );
                }
                for normal in [
                    IVec3::X,
                    IVec3::NEG_X,
                    IVec3::Y,
                    IVec3::NEG_Y,
                    IVec3::Z,
                    IVec3::NEG_Z,
                ] {
                    for link in sketch_links
                        .links_for_face(*source_cell, normal)
                        .into_iter()
                        .filter(|link| link.entity == *source_entity)
                    {
                        sketch_links.link_face_cell(
                            dest,
                            normal,
                            crate::sketch_model::SketchVoxelLink {
                                entity: copy_entity,
                                ..link
                            },
                        );
                    }
                }
            }
        }
    }
}

fn selection_bounds(cells: &[IVec3], delta: IVec3) -> Option<(Vec3, Vec3)> {
    let first = *cells.first()?;
    let mut min = first + delta;
    let mut max = first + delta;
    for cell in cells.iter().copied().skip(1) {
        let moved = cell + delta;
        min = min.min(moved);
        max = max.max(moved);
    }
    let center = (min.as_vec3() + max.as_vec3()) * 0.5 + Vec3::splat(0.5);
    let scale = (max - min + IVec3::ONE).as_vec3();
    Some((center, scale))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bevy::prelude::*;

    use crate::blocks::{BlockType, AIR};
    use crate::builder::BuilderHistory;
    use crate::mode::{ActiveMode, ModeContext};
    use crate::sketch_model::{
        EditorToolId, SelectionSet, SketchDocument, SketchVoxelLink, SketchVoxelLinkIndex,
        SketchVoxelLinkRole, ToolController,
    };
    use crate::toolbelt::ToolbeltTool;
    use crate::world::{VoxelWorld, WorldEditBatch};

    use super::{
        commit_selection_voxel_copy_array, commit_selection_voxel_move,
        commit_selection_voxel_rotate, commit_selection_voxel_scale,
        commit_semantic_selection_delete, expanded_scale_cells, move_copy_count_from_key,
        move_delta_from_hover_cell, move_delta_from_reference_points, move_delta_from_snap_target,
        move_drag_accepts_motion, move_drag_should_cancel, move_grip_cell,
        move_grip_reference_point, move_reference_point_from_hit, move_tool_active,
        normalized_quarter_turns, rotate_cell_quarter, rotate_face_normal_quarter,
        rotate_tool_active, rotation_lattice_offset_twice, scale_destination_cells,
        scale_tool_active, scale_voxel_destinations, selection_is_transformable,
        selection_pivot_twice, shrink_scale_cell, snapped_move_delta, snapped_quarter_turns,
        snapped_scale_factor, MoveAxisLock, TransformAxis,
    };

    #[test]
    fn legacy_mode_tool_cannot_override_the_canonical_transform_selection() {
        let mut mode = ModeContext::default();
        mode.set(
            ActiveMode::BuildLive {
                tool: ToolbeltTool::TransformMove,
            },
            "legacy move mode",
        );
        let mut controller = ToolController::default();
        controller.activate(EditorToolId::Pencil);

        assert!(!move_tool_active(&mode, &controller));
        assert!(!rotate_tool_active(&mode, &controller));
        assert!(!scale_tool_active(&mode, &controller));

        controller.activate(EditorToolId::Move);
        assert!(move_tool_active(&mode, &controller));
        controller.activate(EditorToolId::Rotate);
        assert!(rotate_tool_active(&mode, &controller));
        controller.activate(EditorToolId::Scale);
        assert!(scale_tool_active(&mode, &controller));
    }

    #[test]
    fn move_axis_lock_maps_arrow_keys_to_world_axes() {
        assert_eq!(
            MoveAxisLock::from_key(KeyCode::ArrowRight),
            Some(MoveAxisLock::X)
        );
        assert_eq!(
            MoveAxisLock::from_key(KeyCode::ArrowUp),
            Some(MoveAxisLock::Y)
        );
        assert_eq!(
            MoveAxisLock::from_key(KeyCode::ArrowLeft),
            Some(MoveAxisLock::Z)
        );
        assert_eq!(MoveAxisLock::from_key(KeyCode::KeyA), None);
    }

    #[test]
    fn move_array_count_maps_digit_keys_to_copy_counts() {
        assert_eq!(move_copy_count_from_key(KeyCode::Digit2), Some(2));
        assert_eq!(move_copy_count_from_key(KeyCode::Digit5), Some(5));
        assert_eq!(move_copy_count_from_key(KeyCode::Digit9), Some(9));
        assert_eq!(move_copy_count_from_key(KeyCode::Digit1), None);
        assert_eq!(move_copy_count_from_key(KeyCode::KeyA), None);
    }

    #[test]
    fn snapped_move_delta_uses_locked_axis_and_voxel_steps() {
        assert_eq!(
            snapped_move_delta(Vec2::new(37.0, 0.0), None),
            IVec3::new(2, 0, 0)
        );
        assert_eq!(
            snapped_move_delta(Vec2::new(0.0, -42.0), None),
            IVec3::new(0, 2, 0)
        );
        assert_eq!(
            snapped_move_delta(Vec2::new(37.0, 0.0), Some(MoveAxisLock::X)),
            IVec3::new(2, 0, 0)
        );
        assert_eq!(
            snapped_move_delta(Vec2::new(0.0, -37.0), Some(MoveAxisLock::Y)),
            IVec3::new(0, 2, 0)
        );
        assert_eq!(
            snapped_move_delta(Vec2::new(37.0, 0.0), Some(MoveAxisLock::Z)),
            IVec3::new(0, 0, 2)
        );
    }

    #[test]
    fn move_grip_cell_prefers_the_hovered_selected_voxel() {
        let cells = vec![
            IVec3::new(10, 4, -3),
            IVec3::new(11, 4, -3),
            IVec3::new(12, 4, -3),
        ];

        assert_eq!(
            move_grip_cell(&cells, Some(IVec3::new(11, 4, -3))),
            Some(IVec3::new(11, 4, -3))
        );
        assert_eq!(
            move_grip_cell(&cells, Some(IVec3::new(99, 4, -3))),
            Some(IVec3::new(10, 4, -3))
        );
        assert_eq!(move_grip_cell(&[], Some(IVec3::ZERO)), None);
    }

    #[test]
    fn move_delta_from_hover_cell_uses_target_minus_grip() {
        assert_eq!(
            move_delta_from_hover_cell(
                Some(IVec3::new(10, 4, -3)),
                Some(IVec3::new(15, 7, -8)),
                None
            ),
            Some(IVec3::new(5, 3, -5))
        );
    }

    #[test]
    fn move_delta_from_hover_cell_applies_axis_locks_after_target_snap() {
        let grip = Some(IVec3::new(10, 4, -3));
        let hover = Some(IVec3::new(15, 7, -8));

        assert_eq!(
            move_delta_from_hover_cell(grip, hover, Some(MoveAxisLock::X)),
            Some(IVec3::new(5, 0, 0))
        );
        assert_eq!(
            move_delta_from_hover_cell(grip, hover, Some(MoveAxisLock::Y)),
            Some(IVec3::new(0, 3, 0))
        );
        assert_eq!(
            move_delta_from_hover_cell(grip, hover, Some(MoveAxisLock::Z)),
            Some(IVec3::new(0, 0, -5))
        );
    }

    #[test]
    fn move_delta_from_hover_cell_ignores_zero_delta_for_mouse_fallback() {
        let grip = Some(IVec3::new(10, 4, -3));

        assert_eq!(move_delta_from_hover_cell(grip, grip, None), None);
        assert_eq!(
            move_delta_from_hover_cell(grip, Some(IVec3::new(10, 7, -3)), Some(MoveAxisLock::X)),
            None
        );
    }

    #[test]
    fn move_delta_from_snap_target_uses_screen_drag_only_after_inference_targets() {
        assert_eq!(
            move_delta_from_snap_target(
                Some(IVec3::new(5, 0, 0)),
                Some(IVec3::new(1, 0, 0)),
                Some(IVec3::new(9, 0, 0)),
            ),
            Some(IVec3::new(5, 0, 0)),
            "Exact inference targets should win over coarse voxel hover."
        );
        assert_eq!(
            move_delta_from_snap_target(None, Some(IVec3::new(0, 3, 0)), Some(IVec3::new(9, 0, 0)),),
            Some(IVec3::new(0, 3, 0)),
            "Voxel hover is still valid when no semantic endpoint/midpoint is hit."
        );
        assert_eq!(
            move_delta_from_snap_target(None, None, Some(IVec3::new(2, 0, 0))),
            Some(IVec3::new(2, 0, 0)),
            "Free mouse drag should be usable when no snap target is under the pointer."
        );
        assert_eq!(
            move_delta_from_snap_target(None, None, None),
            None,
            "Move still waits until either a snap target or an intentional mouse drag exists."
        );
    }

    #[test]
    fn move_delta_from_reference_points_preserves_exact_endpoint_alignment() {
        assert_eq!(
            move_delta_from_reference_points(
                Some(Vec3::new(2.0, 4.5, 8.5)),
                Some(Vec3::new(12.0, 9.25, 8.25)),
                Some(MoveAxisLock::X),
            ),
            Some(IVec3::new(10, 0, 0)),
            "Move should align selected geometry by the exact endpoint coordinate, not by voxel-center bias"
        );
    }

    #[test]
    fn move_reference_point_prefers_selected_endpoint_near_cursor() {
        let mut doc = SketchDocument::default();
        let edge = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(0.5, 4.5, 0.5),
                Vec3::new(8.5, 4.5, 0.5),
            )
            .expect("edge entity");
        let mut selection = SelectionSet::default();
        selection.select(edge);
        let hit = crate::sketch_model::HitRecord::new(
            edge,
            [],
            crate::sketch_model::HitKind::Edge,
            Vec3::new(8.3, 4.5, 0.5),
            0.0,
        );

        let grip = move_grip_reference_point(&doc, &selection, Some(&hit), None)
            .expect("selected endpoint grip");

        assert_eq!(grip, Vec3::new(8.5, 4.5, 0.5));
        assert_eq!(
            move_reference_point_from_hit(&doc, &hit),
            Some(Vec3::new(8.5, 4.5, 0.5))
        );
    }

    #[test]
    fn move_release_frame_is_not_cancelled_before_commit() {
        assert!(!move_drag_should_cancel(false, false));
        assert!(move_drag_should_cancel(true, false));
        assert!(move_drag_should_cancel(false, true));
    }

    #[test]
    fn right_mouse_orbit_never_drives_selected_object_motion() {
        assert!(!move_drag_accepts_motion(true));
        assert!(move_drag_accepts_motion(false));
    }

    #[test]
    fn transform_axis_and_drag_snapping_are_deterministic() {
        assert_eq!(
            TransformAxis::from_key(KeyCode::ArrowRight),
            Some(TransformAxis::X)
        );
        assert_eq!(
            TransformAxis::from_key(KeyCode::ArrowUp),
            Some(TransformAxis::Y)
        );
        assert_eq!(
            TransformAxis::from_key(KeyCode::ArrowLeft),
            Some(TransformAxis::Z)
        );
        assert_eq!(TransformAxis::from_key(KeyCode::KeyA), None);

        assert_eq!(snapped_quarter_turns(0.0), 0);
        assert_eq!(snapped_quarter_turns(41.0), 1);
        assert_eq!(snapped_quarter_turns(-90.0), -2);
        assert_eq!(normalized_quarter_turns(3), -1);
        assert_eq!(normalized_quarter_turns(4), 0);
        assert_eq!(snapped_scale_factor(0.0), 1);
        assert_eq!(snapped_scale_factor(37.0), 2);
        assert_eq!(snapped_scale_factor(10_000.0), 8);
        assert_eq!(snapped_scale_factor(-37.0), -2);
        assert_eq!(snapped_scale_factor(-72.0), -3);
        assert_eq!(snapped_scale_factor(-10_000.0), -8);
    }

    #[test]
    fn quarter_turn_preserves_mixed_parity_selection_without_shape_drift() {
        let cells = vec![IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)];
        let pivot_twice = selection_pivot_twice(&cells).expect("selection pivot");
        assert_eq!(pivot_twice, IVec3::new(4, 5, 7));
        assert_eq!(
            rotation_lattice_offset_twice(pivot_twice, TransformAxis::Y, 1),
            IVec3::new(1, 0, 1)
        );

        let mut rotated: Vec<_> = cells
            .into_iter()
            .map(|cell| {
                rotate_cell_quarter(cell, pivot_twice, TransformAxis::Y, 1)
                    .expect("rotated voxel cell")
            })
            .collect();
        rotated.sort_unstable_by_key(|cell| (cell.x, cell.y, cell.z));
        assert_eq!(rotated, vec![IVec3::new(2, 2, 3), IVec3::new(2, 2, 4)]);
        assert_eq!(
            rotate_face_normal_quarter(IVec3::X, TransformAxis::Y, 1),
            Some(IVec3::NEG_Z)
        );
    }

    #[test]
    fn integer_scale_expands_voxels_without_gaps_or_duplicates() {
        let anchor = IVec3::new(1, 2, 3);
        let expanded = expanded_scale_cells(anchor, anchor, 2).expect("expanded voxel");
        assert_eq!(expanded.len(), 8);
        assert!(expanded.contains(&IVec3::new(1, 2, 3)));
        assert!(expanded.contains(&IVec3::new(2, 3, 4)));

        let destinations =
            scale_destination_cells(&[IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], anchor, 2)
                .expect("scaled selection");
        assert_eq!(destinations.len(), 16);
        assert_eq!(destinations.first(), Some(&IVec3::new(1, 2, 3)));
        assert_eq!(destinations.last(), Some(&IVec3::new(4, 3, 4)));
    }

    #[test]
    fn half_scale_reduces_a_four_cube_to_a_dense_two_cube() {
        let source: Vec<_> = (0..4)
            .flat_map(|y| (0..4).flat_map(move |z| (0..4).map(move |x| IVec3::new(x, y, z))))
            .collect();

        let destinations =
            scale_destination_cells(&source, IVec3::ZERO, -2).expect("half-scale lattice");
        let mut expected: Vec<_> = (0..2)
            .flat_map(|y| (0..2).flat_map(move |z| (0..2).map(move |x| IVec3::new(x, y, z))))
            .collect();
        expected.sort_unstable_by_key(|cell| (cell.x, cell.y, cell.z));

        assert_eq!(destinations, expected);
    }

    #[test]
    fn shrink_buckets_use_euclidean_division_and_fail_closed_for_invalid_extremes() {
        assert_eq!(
            shrink_scale_cell(IVec3::splat(-1), IVec3::ZERO, 2),
            Some(IVec3::splat(-1)),
            "the cell immediately below a zero anchor belongs to the negative bucket"
        );
        assert_eq!(
            shrink_scale_cell(IVec3::splat(-2), IVec3::ZERO, 2),
            Some(IVec3::splat(-1))
        );
        assert_eq!(
            shrink_scale_cell(IVec3::splat(-3), IVec3::ZERO, 2),
            Some(IVec3::splat(-2))
        );
        assert_eq!(
            shrink_scale_cell(IVec3::splat(i32::MIN), IVec3::splat(i32::MAX), 2),
            Some(IVec3::splat(-1)),
            "i64 intermediates keep opposite i32 extremes defined"
        );
        assert_eq!(shrink_scale_cell(IVec3::ZERO, IVec3::ZERO, i32::MIN), None);
        assert_eq!(
            scale_destination_cells(&[IVec3::ZERO], IVec3::ZERO, i32::MIN),
            None,
            "a signed factor must be validated before taking its absolute value"
        );
    }

    #[test]
    fn shrink_material_reduction_is_insertion_order_independent_and_preserves_wood() {
        let wood = BlockType::Wood as u16;
        let leaves = BlockType::Leaves as u16;
        let mut ordered_cells: Vec<_> = (0..2)
            .flat_map(|y| (0..2).flat_map(move |z| (0..2).map(move |x| IVec3::new(x, y, z))))
            .collect();
        let mut forward = HashMap::new();
        for cell in &ordered_cells {
            forward.insert(*cell, leaves);
        }
        forward.insert(IVec3::new(1, 1, 1), wood);

        ordered_cells.reverse();
        let mut reverse = HashMap::new();
        for cell in ordered_cells {
            reverse.insert(
                cell,
                if cell == IVec3::new(1, 1, 1) {
                    wood
                } else {
                    leaves
                },
            );
        }

        let forward_result =
            scale_voxel_destinations(&forward, IVec3::ZERO, -2).expect("forward shrink");
        let reverse_result =
            scale_voxel_destinations(&reverse, IVec3::ZERO, -2).expect("reverse shrink");

        assert_eq!(forward_result, reverse_result);
        assert_eq!(forward_result.len(), 1);
        assert_eq!(forward_result.get(&IVec3::ZERO), Some(&wood));
    }

    #[test]
    fn rotate_commit_and_undo_redo_keep_voxels_links_and_document_atomic() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Face);
        assert_eq!(
            links.link_face_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], IVec3::X, link,),
            2
        );
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let pivot_twice = selection_pivot_twice(&[IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)])
            .expect("selection pivot");
        let mut history = BuilderHistory::default();

        let rotated = commit_selection_voxel_rotate(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            pivot_twice,
            TransformAxis::Y,
            1,
            "Rotate selection Y 90",
        );

        assert_eq!(rotated, 2);
        assert_eq!(world.voxel_at(1, 2, 3), AIR);
        assert_eq!(world.voxel_at(2, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 4), stone);
        assert_eq!(
            links.primary_face_link(IVec3::new(2, 2, 3), IVec3::NEG_Z),
            Some(link)
        );
        assert_eq!(
            links.primary_face_link(IVec3::new(2, 2, 4), IVec3::NEG_Z),
            Some(link)
        );
        assert_eq!(history.undo_len(), 1);
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if a.distance(Vec3::new(2.0, 2.0, 5.0)) < 1.0e-5
                    && b.distance(Vec3::new(2.0, 2.0, 3.0)) < 1.0e-5
        ));

        let undo = history
            .pop_undo_detailed(&mut world)
            .expect("voxel undo step");
        assert_eq!(
            undo.apply_sketch_undo(&mut doc, &mut links).unwrap().label,
            "Rotate selection Y 90"
        );
        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 4), AIR);
        assert_eq!(
            links.primary_face_link(IVec3::new(1, 2, 3), IVec3::X),
            Some(link)
        );
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(1.0, 2.0, 3.0) && *b == Vec3::new(3.0, 2.0, 3.0)
        ));

        let redo = history
            .pop_redo_detailed(&mut world)
            .expect("voxel redo step");
        assert_eq!(
            redo.apply_sketch_redo(&mut doc, &mut links).unwrap().label,
            "Rotate selection Y 90"
        );
        assert_eq!(world.voxel_at(1, 2, 3), AIR);
        assert_eq!(world.voxel_at(2, 2, 4), stone);
        assert_eq!(
            links.primary_face_link(IVec3::new(2, 2, 4), IVec3::NEG_Z),
            Some(link)
        );
    }

    #[test]
    fn scale_commit_and_undo_redo_keep_voxels_links_and_document_atomic() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], link);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let scaled = commit_selection_voxel_scale(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::new(1, 2, 3),
            2,
            "Scale selection 2x",
        );

        assert_eq!(scaled, 16);
        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(4, 3, 4), stone);
        assert_eq!(links.primary_cell_link(IVec3::new(4, 3, 4)), Some(link));
        assert_eq!(history.undo_len(), 1);
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(1.0, 2.0, 3.0) && *b == Vec3::new(5.0, 2.0, 3.0)
        ));

        let undo = history
            .pop_undo_detailed(&mut world)
            .expect("voxel undo step");
        assert_eq!(
            undo.apply_sketch_undo(&mut doc, &mut links).unwrap().label,
            "Scale selection 2x"
        );
        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 3), stone);
        assert_eq!(world.voxel_at(4, 3, 4), AIR);
        assert_eq!(links.primary_cell_link(IVec3::new(1, 2, 3)), Some(link));
        assert!(links.primary_cell_link(IVec3::new(4, 3, 4)).is_none());
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(1.0, 2.0, 3.0) && *b == Vec3::new(3.0, 2.0, 3.0)
        ));

        let redo = history
            .pop_redo_detailed(&mut world)
            .expect("voxel redo step");
        assert_eq!(
            redo.apply_sketch_redo(&mut doc, &mut links).unwrap().label,
            "Scale selection 2x"
        );
        assert_eq!(world.voxel_at(4, 3, 4), stone);
        assert_eq!(links.primary_cell_link(IVec3::new(4, 3, 4)), Some(link));
    }

    #[test]
    fn shrink_commit_and_undo_redo_keep_voxels_links_and_document_atomic() {
        let source_cells: Vec<_> = (0..4)
            .flat_map(|y| (0..4).flat_map(move |z| (0..4).map(move |x| IVec3::new(x, y, z))))
            .collect();
        let stone = BlockType::Stone as u16;
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        for cell in &source_cells {
            world.edit_set_voxel_batched(cell.x, cell.y, cell.z, stone, &mut seed);
        }
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0))
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells(source_cells.iter().copied(), link);
        assert_eq!(links.cells_for_entity(entity).len(), 64);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let scaled = commit_selection_voxel_scale(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::ZERO,
            -2,
            "Scale selection 1/2",
        );

        assert_eq!(scaled, 8);
        assert_eq!(links.cells_for_entity(entity).len(), 8);
        assert_eq!(world.voxel_at(1, 1, 1), stone);
        assert_eq!(world.voxel_at(2, 2, 2), AIR);
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::ZERO && *b == Vec3::new(2.0, 0.0, 0.0)
        ));

        let undo = history
            .pop_undo_detailed(&mut world)
            .expect("shrink undo step");
        assert_eq!(
            undo.apply_sketch_undo(&mut doc, &mut links).unwrap().label,
            "Scale selection 1/2"
        );
        assert!(source_cells
            .iter()
            .all(|cell| world.voxel_at(cell.x, cell.y, cell.z) == stone));
        assert_eq!(links.cells_for_entity(entity).len(), 64);
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::ZERO && *b == Vec3::new(4.0, 0.0, 0.0)
        ));

        let redo = history
            .pop_redo_detailed(&mut world)
            .expect("shrink redo step");
        assert_eq!(
            redo.apply_sketch_redo(&mut doc, &mut links).unwrap().label,
            "Scale selection 1/2"
        );
        assert_eq!(world.voxel_at(1, 1, 1), stone);
        assert_eq!(world.voxel_at(2, 2, 2), AIR);
        assert_eq!(links.cells_for_entity(entity).len(), 8);
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::ZERO && *b == Vec3::new(2.0, 0.0, 0.0)
        ));
    }

    #[test]
    fn move_and_scale_reject_destinations_linked_to_foreign_geometry() {
        let stone = BlockType::Stone as u16;

        {
            let mut world = VoxelWorld::new();
            let mut seed = WorldEditBatch::default();
            world.edit_set_voxel_batched(IVec3::ZERO.x, 0, 0, stone, &mut seed);
            world.finish_edit_batch(seed);

            let mut doc = SketchDocument::default();
            let selected_entity = doc
                .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X)
                .expect("selected edge");
            let foreign_entity = doc
                .draw_pencil_line(
                    doc.active_context(),
                    Vec3::new(2.0, 0.0, 0.0),
                    Vec3::new(3.0, 0.0, 0.0),
                )
                .expect("foreign edge");
            let selected_link = SketchVoxelLink::new(
                selected_entity,
                doc.active_context(),
                SketchVoxelLinkRole::Stroke,
            );
            let foreign_link = SketchVoxelLink::new(
                foreign_entity,
                doc.active_context(),
                SketchVoxelLinkRole::Stroke,
            );
            let mut links = SketchVoxelLinkIndex::default();
            links.link_cell(IVec3::ZERO, selected_link);
            links.link_cell(IVec3::new(2, 0, 0), foreign_link);
            let mut selection = SelectionSet::default();
            selection.select(selected_entity);
            let mut history = BuilderHistory::default();
            let document_undo_before = doc.undo_count();

            assert_eq!(
                commit_selection_voxel_move(
                    &mut world,
                    &mut history,
                    &mut doc,
                    &mut links,
                    &selection,
                    IVec3::new(2, 0, 0),
                    "Blocked move",
                ),
                0
            );
            assert_eq!(world.voxel_at(0, 0, 0), stone);
            assert_eq!(world.voxel_at(2, 0, 0), AIR);
            assert_eq!(links.primary_cell_link(IVec3::ZERO), Some(selected_link));
            assert_eq!(
                links.primary_cell_link(IVec3::new(2, 0, 0)),
                Some(foreign_link)
            );
            assert_eq!(history.undo_len(), 0);
            assert_eq!(doc.undo_count(), document_undo_before);
        }

        {
            let source = IVec3::new(1, 0, 0);
            let blocked_destination = IVec3::new(3, 0, 0);
            let mut world = VoxelWorld::new();
            let mut seed = WorldEditBatch::default();
            world.edit_set_voxel_batched(source.x, source.y, source.z, stone, &mut seed);
            world.finish_edit_batch(seed);

            let mut doc = SketchDocument::default();
            let selected_entity = doc
                .draw_pencil_line(doc.active_context(), Vec3::X, Vec3::new(2.0, 0.0, 0.0))
                .expect("selected edge");
            let foreign_entity = doc
                .draw_pencil_line(
                    doc.active_context(),
                    blocked_destination.as_vec3(),
                    Vec3::new(4.0, 0.0, 0.0),
                )
                .expect("foreign edge");
            let selected_link = SketchVoxelLink::new(
                selected_entity,
                doc.active_context(),
                SketchVoxelLinkRole::Stroke,
            );
            let foreign_link = SketchVoxelLink::new(
                foreign_entity,
                doc.active_context(),
                SketchVoxelLinkRole::Stroke,
            );
            let mut links = SketchVoxelLinkIndex::default();
            links.link_cell(source, selected_link);
            links.link_cell(blocked_destination, foreign_link);
            let mut selection = SelectionSet::default();
            selection.select(selected_entity);
            let mut history = BuilderHistory::default();
            let document_undo_before = doc.undo_count();

            assert_eq!(
                commit_selection_voxel_scale(
                    &mut world,
                    &mut history,
                    &mut doc,
                    &mut links,
                    &selection,
                    IVec3::ZERO,
                    2,
                    "Blocked scale",
                ),
                0
            );
            assert_eq!(world.voxel_at(source.x, source.y, source.z), stone);
            assert_eq!(
                links.primary_cell_link(blocked_destination),
                Some(foreign_link)
            );
            assert_eq!(history.undo_len(), 0);
            assert_eq!(doc.undo_count(), document_undo_before);
        }
    }

    #[test]
    fn transform_rejects_cells_shared_with_unselected_semantic_geometry() {
        let mut doc = SketchDocument::default();
        let selected_entity = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X)
            .expect("selected edge");
        let other_entity = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::Y)
            .expect("other edge");
        let mut links = SketchVoxelLinkIndex::default();
        let selected_link = SketchVoxelLink::new(
            selected_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Stroke,
        );
        let other_link = SketchVoxelLink::new(
            other_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Stroke,
        );
        links.link_cell(IVec3::ZERO, selected_link);
        links.link_cell(IVec3::ZERO, other_link);
        let mut selection = SelectionSet::default();
        selection.select(selected_entity);

        assert!(!selection_is_transformable(
            &doc,
            &links,
            &selection,
            &[IVec3::ZERO]
        ));
    }

    #[test]
    fn move_of_a_source_shared_with_an_unselected_part_is_an_atomic_no_op() {
        let source = IVec3::new(4, 5, 6);
        let destination = source + IVec3::X;
        let stone = BlockType::Stone as u16;
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        world.edit_set_voxel_batched(source.x, source.y, source.z, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let selected_entity = doc
            .draw_pencil_line(
                doc.active_context(),
                source.as_vec3(),
                source.as_vec3() + Vec3::X,
            )
            .expect("selected edge");
        let sharing_entity = doc
            .draw_pencil_line(
                doc.active_context(),
                source.as_vec3(),
                source.as_vec3() + Vec3::Y,
            )
            .expect("sharing edge");
        let selected_link = SketchVoxelLink::new(
            selected_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Stroke,
        );
        let sharing_link = SketchVoxelLink::new(
            sharing_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Stroke,
        );
        let mut links = SketchVoxelLinkIndex::default();
        links.link_cell(source, selected_link);
        links.link_cell(source, sharing_link);
        let links_before = links.links_for_cell(source);
        let mut selection = SelectionSet::default();
        selection.select(selected_entity);
        let mut history = BuilderHistory::default();
        let document_undo_before = doc.undo_count();

        let moved = commit_selection_voxel_move(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::X,
            "Move shared source",
        );

        assert_eq!(moved, 0);
        assert_eq!(world.voxel_at(source.x, source.y, source.z), stone);
        assert_eq!(
            world.voxel_at(destination.x, destination.y, destination.z),
            AIR
        );
        assert_eq!(links.links_for_cell(source), links_before);
        assert!(links.links_for_cell(destination).is_empty());
        assert_eq!(history.undo_len(), 0);
        assert_eq!(doc.undo_count(), document_undo_before);
        assert!(matches!(
            &doc.entity(selected_entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == source.as_vec3() && *b == source.as_vec3() + Vec3::X
        ));
    }

    #[test]
    fn fully_shared_part_delete_is_metadata_only_and_undo_redo_atomic() {
        let cell = IVec3::new(7, 8, 9);
        let wood = BlockType::Wood as u16;
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        world.edit_set_voxel_batched(cell.x, cell.y, cell.z, wood, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let selected_entity = doc
            .draw_pencil_line(
                doc.active_context(),
                cell.as_vec3(),
                cell.as_vec3() + Vec3::Y,
            )
            .expect("selected part");
        let sharing_entity = doc
            .draw_pencil_line(
                doc.active_context(),
                cell.as_vec3(),
                cell.as_vec3() + Vec3::X,
            )
            .expect("sharing part");
        let selected_link = SketchVoxelLink::new(
            selected_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Shape,
        );
        let sharing_link = SketchVoxelLink::new(
            sharing_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Shape,
        );
        let mut links = SketchVoxelLinkIndex::default();
        links.link_cell(cell, selected_link);
        links.link_cell(cell, sharing_link);
        let mut selection = SelectionSet::default();
        selection.select(selected_entity);
        let mut history = BuilderHistory::default();

        let summary = commit_semantic_selection_delete(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            "Delete shared part",
        )
        .expect("metadata-only delete should be recorded");

        assert_eq!(summary.voxel_count, 0);
        assert_eq!(summary.shared_voxel_count, 1);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), wood);
        assert!(!doc.entity_effective_visible(selected_entity).unwrap());
        assert!(doc.entity_effective_visible(sharing_entity).unwrap());
        assert!(!links.links_for_cell(cell).contains(&selected_link));
        assert!(links.links_for_cell(cell).contains(&sharing_link));
        assert_eq!(history.undo_len(), 1);

        let undo = history
            .undo_with_sketch(&mut world, &mut doc, &mut links)
            .expect("metadata undo is applicable")
            .expect("metadata undo step");
        assert_eq!(undo.label, "Delete shared part");
        assert_eq!(undo.voxel_count, 0);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), wood);
        assert!(doc.entity_effective_visible(selected_entity).unwrap());
        assert!(links.links_for_cell(cell).contains(&selected_link));
        assert!(links.links_for_cell(cell).contains(&sharing_link));

        let redo = history
            .redo_with_sketch(&mut world, &mut doc, &mut links)
            .expect("metadata redo is applicable")
            .expect("metadata redo step");
        assert_eq!(redo.label, "Delete shared part");
        assert_eq!(redo.voxel_count, 0);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), wood);
        assert!(!doc.entity_effective_visible(selected_entity).unwrap());
        assert!(!links.links_for_cell(cell).contains(&selected_link));
        assert!(links.links_for_cell(cell).contains(&sharing_link));
    }

    #[test]
    fn commit_selection_voxel_move_moves_selected_cells_links_and_document() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], link);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let moved = commit_selection_voxel_move(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::new(0, 3, 0),
            "Move selection",
        );

        assert_eq!(moved, 2);
        assert_eq!(world.voxel_at(1, 2, 3), AIR);
        assert_eq!(world.voxel_at(2, 2, 3), AIR);
        assert_eq!(world.voxel_at(1, 5, 3), stone);
        assert_eq!(world.voxel_at(2, 5, 3), stone);
        assert_eq!(links.primary_cell_link(IVec3::new(1, 5, 3)), Some(link));
        assert!(links.primary_cell_link(IVec3::new(1, 2, 3)).is_none());
        assert_eq!(history.undo_len(), 1);

        let moved_edge = doc.entity(entity).expect("moved edge still exists");
        assert!(format!("{moved_edge:?}").contains("5.0"));
    }

    #[test]
    fn undo_redo_selection_voxel_move_restores_cells_links_and_document() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], link);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let delta = IVec3::new(0, 3, 0);
        commit_selection_voxel_move(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            delta,
            "Move selection",
        );

        let undo = history
            .pop_undo_detailed(&mut world)
            .expect("voxel undo step");
        assert_eq!(
            undo.apply_sketch_undo(&mut doc, &mut links).unwrap().label,
            "Move selection"
        );

        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 3), stone);
        assert_eq!(world.voxel_at(1, 5, 3), AIR);
        assert_eq!(world.voxel_at(2, 5, 3), AIR);
        assert_eq!(links.primary_cell_link(IVec3::new(1, 2, 3)), Some(link));
        assert!(links.primary_cell_link(IVec3::new(1, 5, 3)).is_none());
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(1.0, 2.0, 3.0) && *b == Vec3::new(3.0, 2.0, 3.0)
        ));

        let redo = history
            .pop_redo_detailed(&mut world)
            .expect("voxel redo step");
        assert_eq!(
            redo.apply_sketch_redo(&mut doc, &mut links).unwrap().label,
            "Move selection"
        );

        assert_eq!(world.voxel_at(1, 2, 3), AIR);
        assert_eq!(world.voxel_at(2, 2, 3), AIR);
        assert_eq!(world.voxel_at(1, 5, 3), stone);
        assert_eq!(world.voxel_at(2, 5, 3), stone);
        assert_eq!(links.primary_cell_link(IVec3::new(1, 5, 3)), Some(link));
        assert!(links.primary_cell_link(IVec3::new(1, 2, 3)).is_none());
        assert!(matches!(
            &doc.entity(entity).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(1.0, 5.0, 3.0) && *b == Vec3::new(3.0, 5.0, 3.0)
        ));
    }

    #[test]
    fn stale_selection_move_aborts_without_voxel_or_link_history_changes() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let stale_entity = crate::sketch_model::SketchId::new_for_test(99);
        let mut links = SketchVoxelLinkIndex::default();
        let stale_link = SketchVoxelLink::new(
            stale_entity,
            doc.active_context(),
            SketchVoxelLinkRole::Stroke,
        );
        links.link_cell(IVec3::new(1, 2, 3), stale_link);
        let mut selection = SelectionSet::default();
        selection.select(stale_entity);
        let mut history = BuilderHistory::default();

        let moved = commit_selection_voxel_move(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::new(0, 3, 0),
            "Move stale selection",
        );

        assert_eq!(moved, 0);
        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(1, 5, 3), AIR);
        assert_eq!(
            links.primary_cell_link(IVec3::new(1, 2, 3)),
            Some(stale_link)
        );
        assert!(links.primary_cell_link(IVec3::new(1, 5, 3)).is_none());
        assert_eq!(history.undo_len(), 0);
        assert_eq!(doc.undo_count(), 0);
    }

    #[test]
    fn commit_selection_voxel_copy_array_keeps_original_and_links_copies() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], link);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let copied = commit_selection_voxel_copy_array(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::new(0, 3, 0),
            2,
            "Copy selection x2",
        );

        assert_eq!(copied, 4);
        assert_eq!(world.voxel_at(1, 2, 3), stone);
        assert_eq!(world.voxel_at(2, 2, 3), stone);
        assert_eq!(world.voxel_at(1, 5, 3), stone);
        assert_eq!(world.voxel_at(2, 5, 3), stone);
        assert_eq!(world.voxel_at(1, 8, 3), stone);
        assert_eq!(world.voxel_at(2, 8, 3), stone);
        assert_eq!(history.undo_len(), 1);
        assert_eq!(doc.undo_count(), 2);

        let first_copy = links
            .links_for_cell(IVec3::new(1, 5, 3))
            .into_iter()
            .find(|link| link.entity != entity)
            .expect("first copied cell should have a copied semantic link");
        let second_copy = links
            .links_for_cell(IVec3::new(1, 8, 3))
            .into_iter()
            .find(|link| link.entity != entity && link.entity != first_copy.entity)
            .expect("second copied cell should have its own copied semantic link");
        assert_eq!(first_copy.role, SketchVoxelLinkRole::Stroke);
        assert_eq!(second_copy.role, SketchVoxelLinkRole::Stroke);
        assert!(doc.entity(first_copy.entity).is_some());
        assert!(doc.entity(second_copy.entity).is_some());
        assert_eq!(links.primary_cell_link(IVec3::new(1, 2, 3)), Some(link));
    }

    #[test]
    fn undo_redo_selection_copy_array_restores_cells_links_and_document() {
        let mut world = VoxelWorld::new();
        let mut seed = WorldEditBatch::default();
        let stone = BlockType::Stone as u16;
        world.edit_set_voxel_batched(1, 2, 3, stone, &mut seed);
        world.edit_set_voxel_batched(2, 2, 3, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut doc = SketchDocument::default();
        let entity = doc
            .draw_pencil_line(
                doc.active_context(),
                Vec3::new(1.0, 2.0, 3.0),
                Vec3::new(3.0, 2.0, 3.0),
            )
            .expect("edge entity");
        let mut links = SketchVoxelLinkIndex::default();
        let link = SketchVoxelLink::new(entity, doc.active_context(), SketchVoxelLinkRole::Stroke);
        links.link_cells([IVec3::new(1, 2, 3), IVec3::new(2, 2, 3)], link);
        let mut selection = SelectionSet::default();
        selection.select(entity);
        let mut history = BuilderHistory::default();

        let copied = commit_selection_voxel_copy_array(
            &mut world,
            &mut history,
            &mut doc,
            &mut links,
            &selection,
            IVec3::new(0, 3, 0),
            2,
            "Copy selection x2",
        );
        assert_eq!(copied, 4);

        let first_copy = links
            .links_for_cell(IVec3::new(1, 5, 3))
            .into_iter()
            .find(|copy| copy.entity != entity)
            .expect("first copied semantic link");
        let second_copy = links
            .links_for_cell(IVec3::new(1, 8, 3))
            .into_iter()
            .find(|copy| copy.entity != entity && copy.entity != first_copy.entity)
            .expect("second copied semantic link");

        let undo = history
            .pop_undo_detailed(&mut world)
            .expect("voxel undo step");
        assert_eq!(
            undo.apply_sketch_undo(&mut doc, &mut links).unwrap().label,
            "Copy selection x2"
        );
        assert_eq!(world.voxel_at(1, 5, 3), AIR);
        assert_eq!(world.voxel_at(2, 5, 3), AIR);
        assert_eq!(world.voxel_at(1, 8, 3), AIR);
        assert_eq!(world.voxel_at(2, 8, 3), AIR);
        assert!(doc.entity(first_copy.entity).is_none());
        assert!(doc.entity(second_copy.entity).is_none());
        assert!(links.primary_cell_link(IVec3::new(1, 5, 3)).is_none());
        assert!(links.primary_cell_link(IVec3::new(1, 8, 3)).is_none());
        assert_eq!(links.primary_cell_link(IVec3::new(1, 2, 3)), Some(link));

        let redo = history
            .pop_redo_detailed(&mut world)
            .expect("voxel redo step");
        assert_eq!(
            redo.apply_sketch_redo(&mut doc, &mut links).unwrap().label,
            "Copy selection x2"
        );
        assert_eq!(world.voxel_at(1, 5, 3), stone);
        assert_eq!(world.voxel_at(2, 5, 3), stone);
        assert_eq!(world.voxel_at(1, 8, 3), stone);
        assert_eq!(world.voxel_at(2, 8, 3), stone);
        assert!(doc.entity(first_copy.entity).is_some());
        assert!(doc.entity(second_copy.entity).is_some());
        assert_eq!(
            links.primary_cell_link(IVec3::new(1, 5, 3)),
            Some(first_copy)
        );
        assert_eq!(
            links.primary_cell_link(IVec3::new(1, 8, 3)),
            Some(second_copy)
        );
    }
}
