//! Direct rectangle drawing for live edit mode.
//!
//! This is the "I draw a square and it becomes blocks" tool the builder
//! needs before the heavier transform-gizmo phases are worth anything:
//! click a face to set a snapped start point, move the cursor to preview the
//! endpoint on the locked plane, then click again to commit. In the default
//! sketch builder, RMB is reserved for camera orbit; dedicated toolbox
//! workflows handle openings and room hollowing without forcing key chords.
//! Esc cancels the active preview.
//! Undo/redo uses the shared builder history.

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::blocks::{voxel_is_solid, Voxel, AIR};
use crate::builder::{BuilderHistory, BuilderHistorySketchMeta, BuilderState};
use crate::mode::{BuildGestureLock, ModeContext};
use crate::player::Player;
use crate::sculpt::raycast::dda_voxel;
use crate::toolbelt::{BuildWorkflowPreset, ToolbeltState, ToolbeltTool};
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
const RECT_FACE_SNAP_RADIUS: f32 = 0.30;
const SEMANTIC_DRAW_POINT_RADIUS: f32 = 1.25;
const SEMANTIC_DRAW_SCREEN_RADIUS: f32 = 22.0;
const RECT_ACQUISITION_FEEDBACK_SECONDS: f32 = 0.24;
const RECT_COMMIT_FEEDBACK_SECONDS: f32 = 0.34;

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
    pencil_line: bool,
    shape_workflow: SketchShapeWorkflow,
    inference: RectEndpointInference,
    snap_kind: Option<RectFaceSnapKind>,
    start_snap_kind: Option<RectFaceSnapKind>,
    axis_lock: Option<RectAxisLock>,
    start_point: Vec3,
    current_point: Vec3,
    tool_generation: u64,
    reference_span: IVec2,
    voxel: Voxel,
    status_cells: usize,
    pointer_valid: bool,
    visual_acquisition: Option<RectVisualAcquisition>,
    visual_feedback: RectVisualFeedback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RectVisualAcquisition {
    Snap(RectFaceSnapKind, IVec3),
    Axis(RectAxisLock),
    Inference(RectEndpointInference),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectVisualFeedbackKind {
    #[default]
    None,
    Acquisition,
    Commit,
}

#[derive(Debug, Clone, Copy, Default)]
struct RectVisualFeedback {
    kind: RectVisualFeedbackKind,
    point: Vec3,
    normal: IVec3,
    snap_kind: Option<RectFaceSnapKind>,
    remaining: f32,
    duration: f32,
}

impl RectVisualFeedback {
    fn begin(
        &mut self,
        kind: RectVisualFeedbackKind,
        point: Vec3,
        normal: IVec3,
        snap_kind: Option<RectFaceSnapKind>,
    ) {
        let duration = match kind {
            RectVisualFeedbackKind::Acquisition => RECT_ACQUISITION_FEEDBACK_SECONDS,
            RectVisualFeedbackKind::Commit => RECT_COMMIT_FEEDBACK_SECONDS,
            RectVisualFeedbackKind::None => 0.0,
        };
        *self = Self {
            kind,
            point,
            normal,
            snap_kind,
            remaining: duration,
            duration,
        };
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct SketchEditorPointerMarker {
    active: bool,
    drawing: bool,
    point: Vec3,
    normal: IVec3,
    cell: IVec3,
    snap_kind: Option<RectFaceSnapKind>,
}

impl SketchEditorPointerMarker {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn set(
        &mut self,
        point: Vec3,
        normal: IVec3,
        cell: IVec3,
        snap_kind: Option<RectFaceSnapKind>,
        drawing: bool,
    ) {
        self.active = true;
        self.drawing = drawing;
        self.point = point;
        self.normal = normal;
        self.cell = cell;
        self.snap_kind = snap_kind;
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct SketchEditorScreenCursor {
    active: bool,
    cursor: Vec2,
    target: Option<Vec2>,
    snap_kind: Option<RectFaceSnapKind>,
    drawing: bool,
    over_ui: bool,
}

impl SketchEditorScreenCursor {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn set(
        &mut self,
        cursor: Vec2,
        target: Option<Vec2>,
        snap_kind: Option<RectFaceSnapKind>,
        drawing: bool,
        over_ui: bool,
    ) {
        self.active = true;
        self.cursor = cursor;
        self.target = target;
        self.snap_kind = snap_kind;
        self.drawing = drawing;
        self.over_ui = over_ui;
    }
}

fn visible_screen_cursor_position(
    cursor_visible: bool,
    cursor_position: Option<Vec2>,
) -> Option<Vec2> {
    cursor_visible.then_some(cursor_position).flatten()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectDrawAction {
    #[default]
    Fill,
    Cut,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SketchShapeWorkflow {
    #[default]
    Rectangle,
    Circle,
    Polygon,
    Arc,
    Freehand,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum RectEndpointInference {
    #[default]
    None,
    Axis,
    EqualLength,
    ReferenceLength,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RectFaceSnapKind {
    Endpoint,
    Midpoint,
    FaceCenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RectAxisLock {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RectFaceInputPoint {
    point: Vec3,
    kind: Option<RectFaceSnapKind>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SemanticDrawInputPoint {
    cell: IVec3,
    point: Vec3,
    kind: RectFaceSnapKind,
}

impl RectEndpointInference {
    fn status_suffix(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Axis => " Axis lock.",
            Self::EqualLength => " Equal-length snap.",
            Self::ReferenceLength => " Reference length snap.",
        }
    }

    fn readout_label(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Axis => Some("axis line"),
            Self::EqualLength => Some("equal length"),
            Self::ReferenceLength => Some("reference length"),
        }
    }
}

impl RectFaceSnapKind {
    fn status_suffix(self) -> &'static str {
        match rect_face_snap_inference_kind(self) {
            crate::sketch_model::InferenceKind::Endpoint => " Endpoint snap.",
            crate::sketch_model::InferenceKind::Midpoint => " Midpoint snap.",
            crate::sketch_model::InferenceKind::FaceCenter => " Face center snap.",
            _ => " Snap.",
        }
    }

    fn readout_label(self) -> &'static str {
        match self {
            Self::Endpoint => "Endpoint",
            Self::Midpoint => "Midpoint",
            Self::FaceCenter => "Face center",
        }
    }
}

fn screen_cursor_label(snap_kind: Option<RectFaceSnapKind>, drawing: bool) -> &'static str {
    match snap_kind {
        Some(kind) => kind.readout_label(),
        None if drawing => "Cursor",
        None => "Pointer",
    }
}

fn screen_cursor_color(
    snap_kind: Option<RectFaceSnapKind>,
    drawing: bool,
    over_ui: bool,
) -> egui::Color32 {
    let [r, g, b] = match snap_kind {
        Some(RectFaceSnapKind::Endpoint) => [68, 255, 112],
        Some(RectFaceSnapKind::Midpoint) => [68, 230, 255],
        Some(RectFaceSnapKind::FaceCenter) => [82, 145, 255],
        None if drawing => [255, 215, 72],
        None => [255, 190, 54],
    };
    let alpha = if over_ui { 150 } else { 238 };
    egui::Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

fn rect_face_snap_inference_kind(
    snap_kind: RectFaceSnapKind,
) -> crate::sketch_model::InferenceKind {
    match snap_kind {
        RectFaceSnapKind::Endpoint => crate::sketch_model::InferenceKind::Endpoint,
        RectFaceSnapKind::Midpoint => crate::sketch_model::InferenceKind::Midpoint,
        RectFaceSnapKind::FaceCenter => crate::sketch_model::InferenceKind::FaceCenter,
    }
}

fn rect_face_snap_from_inference_kind(
    kind: crate::sketch_model::InferenceKind,
) -> Option<RectFaceSnapKind> {
    match kind {
        crate::sketch_model::InferenceKind::Endpoint => Some(RectFaceSnapKind::Endpoint),
        crate::sketch_model::InferenceKind::Midpoint => Some(RectFaceSnapKind::Midpoint),
        crate::sketch_model::InferenceKind::FaceCenter => Some(RectFaceSnapKind::FaceCenter),
        _ => None,
    }
}

impl RectAxisLock {
    fn axis(self) -> IVec3 {
        match self {
            Self::X => IVec3::X,
            Self::Y => IVec3::Y,
            Self::Z => IVec3::Z,
        }
    }

    fn axis_vec3(self) -> Vec3 {
        self.axis().as_vec3()
    }

    fn status_suffix(self) -> &'static str {
        match self {
            Self::X => " Red X axis lock.",
            Self::Y => " Blue vertical height lock.",
            Self::Z => " Green depth axis lock.",
        }
    }

    fn readout_label(self, start: IVec3, current: IVec3) -> String {
        let from = component_by_axis(start, self.axis());
        let to = component_by_axis(current, self.axis());
        match self {
            Self::X => format!("red X line {from} -> {to}"),
            Self::Y => format!("blue vertical height line {from} -> {to}"),
            Self::Z => format!("green depth line {from} -> {to}"),
        }
    }

    fn color(self) -> Color {
        match self {
            Self::X => Color::srgb(1.0, 0.18, 0.16),
            Self::Y => Color::srgb(0.18, 0.55, 1.0),
            Self::Z => Color::srgb(0.12, 1.0, 0.30),
        }
    }
}

fn rect_status_suffix(
    snap_kind: Option<RectFaceSnapKind>,
    inference: RectEndpointInference,
    axis_lock: Option<RectAxisLock>,
) -> String {
    let mut suffix = String::new();
    if let Some(snap_kind) = snap_kind {
        suffix.push_str(snap_kind.status_suffix());
    }
    if let Some(axis_lock) = axis_lock {
        suffix.push_str(axis_lock.status_suffix());
    } else {
        suffix.push_str(inference.status_suffix());
    }
    suffix
}

fn rect_alignment_readout(
    start: IVec3,
    current: IVec3,
    snap_kind: Option<RectFaceSnapKind>,
    inference: RectEndpointInference,
    axis_lock: Option<RectAxisLock>,
) -> String {
    let mut parts = Vec::new();
    if let Some(snap_kind) = snap_kind {
        parts.push(snap_kind.readout_label().to_string());
    }
    if let Some(axis_lock) = axis_lock {
        parts.push(axis_lock.readout_label(start, current));
    } else if let Some(label) = inference.readout_label() {
        parts.push(label.to_string());
    }
    parts.join(" | ")
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

impl SketchShapeWorkflow {
    fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Circle => "Circle",
            Self::Polygon => "Polygon",
            Self::Arc => "Arc",
            Self::Freehand => "Freehand",
        }
    }

    fn preview_label(self) -> &'static str {
        match self {
            Self::Rectangle => "Smart Build",
            Self::Circle => "Circle",
            Self::Polygon => "Polygon",
            Self::Arc => "Arc",
            Self::Freehand => "Freehand",
        }
    }

    fn history_label(self) -> &'static str {
        match self {
            Self::Rectangle => "Smart endpoint build",
            Self::Circle => "Circle face",
            Self::Polygon => "Polygon face",
            Self::Arc => "Arc curve",
            Self::Freehand => "Freehand stroke",
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
    opening_workflow: bool,
    room_workflow: bool,
) -> RectStartIntent {
    let smart_tool = matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    );
    let sketch_tool = active_tool == ToolbeltTool::DrawRect;
    let room_cut =
        sketch_tool && left_just && !ctrl && !opening_workflow && (shift || room_workflow);
    let opening_cut = sketch_tool && left_just && opening_workflow;
    let modifier_cut = sketch_tool && left_just && (ctrl || shift || room_workflow || opening_cut);
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

fn rect_action_for_start_intent(
    intent: RectStartIntent,
    _active_tool: ToolbeltTool,
    _normal: IVec3,
) -> RectDrawAction {
    if intent.cut {
        return RectDrawAction::Cut;
    }
    RectDrawAction::Fill
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

fn update_rect_axis_lock(
    keys: &ButtonInput<KeyCode>,
    current: Option<RectAxisLock>,
) -> Option<RectAxisLock> {
    if keys.just_pressed(KeyCode::ArrowDown) {
        return None;
    }
    if keys.just_pressed(KeyCode::ArrowRight) {
        return toggle_rect_axis_lock(current, RectAxisLock::X);
    }
    if keys.just_pressed(KeyCode::ArrowLeft) {
        return toggle_rect_axis_lock(current, RectAxisLock::Y);
    }
    if keys.just_pressed(KeyCode::ArrowUp) {
        return toggle_rect_axis_lock(current, RectAxisLock::Z);
    }
    current
}

fn toggle_rect_axis_lock(
    current: Option<RectAxisLock>,
    requested: RectAxisLock,
) -> Option<RectAxisLock> {
    if current == Some(requested) {
        None
    } else {
        Some(requested)
    }
}

fn sketch_tool_uses_click_finish(active_tool: ToolbeltTool, smart_tool: bool) -> bool {
    !smart_tool && matches!(active_tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt)
}

fn active_shape_workflow(toolbelt: &ToolbeltState) -> SketchShapeWorkflow {
    match toolbelt.drafting_shape_workflow() {
        Some(BuildWorkflowPreset::Circle) => SketchShapeWorkflow::Circle,
        Some(BuildWorkflowPreset::Polygon) => SketchShapeWorkflow::Polygon,
        Some(BuildWorkflowPreset::Arc) => SketchShapeWorkflow::Arc,
        Some(BuildWorkflowPreset::Freehand) => SketchShapeWorkflow::Freehand,
        _ => SketchShapeWorkflow::Rectangle,
    }
}

fn rect_should_commit_on_start_intent(intent: RectStartIntent, draw: &RectDrawState) -> bool {
    draw.active
        && draw.click_finish
        && draw.pointer_valid
        && matches!(intent.button, RectDragButton::Left)
        && (intent.fill || intent.cut)
}

fn rect_should_commit_on_release(draw: &RectDrawState) -> bool {
    draw.active && !draw.click_finish
}

fn rect_should_cancel_for_tool_selection(draw: &RectDrawState, current_generation: u64) -> bool {
    draw.active && draw.tool_generation != current_generation
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RectPointerLossDisposition {
    None,
    SuspendForOrbit,
    Cancel,
}

fn rect_pointer_loss_disposition(
    draw: &RectDrawState,
    active_tool: ToolbeltTool,
    pointer_ray_available: bool,
    cursor_locked: bool,
    right_held: bool,
) -> RectPointerLossDisposition {
    if !draw.active || !editor_pointer_tool_requires_cursor(active_tool) || pointer_ray_available {
        return RectPointerLossDisposition::None;
    }
    if cursor_locked && right_held {
        RectPointerLossDisposition::SuspendForOrbit
    } else {
        RectPointerLossDisposition::Cancel
    }
}

fn rect_should_ignore_world_click_for_editor_ui(
    pointer_over_editor_ui: bool,
    left_just: bool,
    right_just: bool,
) -> bool {
    pointer_over_editor_ui && (left_just || right_just)
}

fn clear_rect_preview(draw: &mut RectDrawState) {
    draw.active = false;
    draw.click_finish = false;
    draw.pencil_line = false;
    draw.shape_workflow = SketchShapeWorkflow::Rectangle;
    draw.snap_kind = None;
    draw.start_snap_kind = None;
    draw.axis_lock = None;
    draw.inference = RectEndpointInference::None;
    draw.pointer_valid = false;
    draw.visual_acquisition = None;
    draw.visual_feedback.clear();
}

fn sync_pointer_marker_from_active_draw(
    marker: &mut SketchEditorPointerMarker,
    draw: &RectDrawState,
) -> bool {
    if !draw.active {
        return false;
    }
    marker.set(
        draw.current_point,
        draw.normal,
        draw.current,
        draw.snap_kind,
        true,
    );
    true
}

fn sync_pointer_marker_from_hover(
    marker: &mut SketchEditorPointerMarker,
    world: &VoxelWorld,
    active_tool: ToolbeltTool,
    pencil_workflow: bool,
    origin: Vec3,
    dir: Vec3,
) -> bool {
    if !matches!(active_tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt) {
        return false;
    }
    let Some((hit, prev)) = dda_voxel(world, origin, dir, DRAW_REACH) else {
        return false;
    };
    let normal = prev - hit;
    let Some((axis_u, axis_v)) = plane_axes(normal) else {
        return false;
    };
    let input = rect_face_input_point(origin, dir, hit, prev);
    let action = if active_tool == ToolbeltTool::Sculpt {
        rect_action_for_start_intent(
            RectStartIntent {
                fill: true,
                cut: false,
                room_cut: false,
                button: RectDragButton::Left,
            },
            active_tool,
            normal,
        )
    } else {
        RectDrawAction::Fill
    };
    let mut cell = if pencil_workflow {
        pencil_anchor_cell_from_ray(hit, prev, axis_u, axis_v, origin, dir)
    } else {
        rect_start_cell_from_ray(action, hit, prev, axis_u, axis_v, origin, dir)
    };
    if let Some(input) = input {
        cell = apply_face_input_point_to_cell(cell, input, axis_u, axis_v);
    }
    let point = if pencil_workflow {
        input
            .map(|input| project_draw_input_point_to_locked_plane(input.point, cell, normal, true))
            .unwrap_or_else(|| pencil_cell_marker_point(cell))
    } else {
        input
            .map(|input| input.point)
            .unwrap_or_else(|| cell.as_vec3())
    };
    marker.set(
        point,
        normal,
        cell,
        input.and_then(|input| input.kind),
        false,
    );
    true
}

fn sync_pointer_marker(
    marker: &mut SketchEditorPointerMarker,
    draw: &RectDrawState,
    world: &VoxelWorld,
    active_tool: ToolbeltTool,
    pencil_workflow: bool,
    origin: Vec3,
    dir: Vec3,
) {
    if sync_pointer_marker_from_active_draw(marker, draw) {
        return;
    }
    if sync_pointer_marker_from_hover(marker, world, active_tool, pencil_workflow, origin, dir) {
        return;
    }
    marker.clear();
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
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    semantic_hover: Res<crate::sketch_model::SemanticHoverHit>,
    mut draw: ResMut<RectDrawState>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
    builder: Res<BuilderState>,
    mut view_q: ParamSet<(
        Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
        Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
    )>,
) {
    if !draw_rect_active(&mode, &keys, &draw) {
        if draw.active {
            clear_rect_preview(&mut draw);
            tool_controller
                .cancel_active_operation(crate::sketch_model::EditorCancelReason::ToolSwitch);
        }
        gesture_lock.release(RECT_FILL_OWNER);
        motion_evr.clear();
        return;
    }

    if rect_should_cancel_for_tool_selection(&draw, toolbelt.selection_generation()) {
        clear_rect_preview(&mut draw);
        tool_controller
            .cancel_active_operation(crate::sketch_model::EditorCancelReason::ToolboxClick);
        gesture_lock.release(RECT_FILL_OWNER);
        toolbelt.status =
            "Sketch operation cancelled. Toolbox switched tools; click a new snapped start point."
                .into();
        motion_evr.clear();
        return;
    }

    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    let smart_tool = matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    );
    if rect_should_ignore_world_click_for_editor_ui(
        ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui),
        mouse.just_pressed(MouseButton::Left),
        mouse.just_pressed(MouseButton::Right),
    ) {
        motion_evr.clear();
        return;
    }

    if keys.just_pressed(KeyCode::Escape) && draw.active {
        clear_rect_preview(&mut draw);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        gesture_lock.release(RECT_FILL_OWNER);
        toolbelt.status =
            "Smart Build cancelled. Click a snapped start point to draw again.".into();
        motion_evr.clear();
        return;
    }

    let (cursor_locked, cursor_visible, cursor_position) = {
        let window_q = view_q.p0();
        let window = window_q.get_single().ok();
        (
            window.map(crate::mode::cursor_is_captured).unwrap_or(false),
            window.map(|window| window.cursor.visible).unwrap_or(false),
            window.and_then(|window| window.cursor_position()),
        )
    };
    if smart_tool && !cursor_locked {
        if mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right) {
            toolbelt.status =
                "Smart Builder needs mouse capture. Click the game view once, then build.".into();
        }
        motion_evr.clear();
        return;
    }

    let cam_q = view_q.p1();
    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        if draw.active {
            clear_rect_preview(&mut draw);
            tool_controller
                .cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
            gesture_lock.release(RECT_FILL_OWNER);
            toolbelt.status =
                "Sketch preview cancelled because the player camera became unavailable.".into();
        } else if mouse.just_pressed(MouseButton::Left) {
            toolbelt.status = "Smart Build could not find the player camera this frame.".into();
        }
        motion_evr.clear();
        return;
    };
    let screen_snap = if cursor_locked
        && !editor_pointer_ray_available(
            active_tool,
            cursor_locked,
            cursor_visible,
            cursor_position,
        ) {
        None
    } else {
        cursor_position
            .zip(camera.logical_viewport_size())
            .map(|(cursor, viewport)| {
                let view_projection = camera.clip_from_view() * cam_tf.compute_matrix().inverse();
                (cursor, view_projection, viewport)
            })
    };
    let input_ray = draw_input_ray(
        active_tool,
        cursor_locked,
        cursor_visible,
        cursor_position,
        camera,
        cam_tf,
    );
    match rect_pointer_loss_disposition(
        &draw,
        active_tool,
        input_ray.is_some(),
        cursor_locked,
        mouse.pressed(MouseButton::Right),
    ) {
        RectPointerLossDisposition::SuspendForOrbit => {
            draw.pointer_valid = false;
            toolbelt.status =
                "Sketch Draw orbiting: endpoint held. Release RMB to reacquire the pointer.".into();
            motion_evr.clear();
            return;
        }
        RectPointerLossDisposition::Cancel => {
            clear_rect_preview(&mut draw);
            tool_controller
                .cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
            gesture_lock.release(RECT_FILL_OWNER);
            toolbelt.status =
                "Sketch preview cancelled because the pointer left the game window. Click a new start point."
                    .into();
            motion_evr.clear();
            return;
        }
        RectPointerLossDisposition::None => {}
    }
    let Some((origin, dir)) = input_ray else {
        if mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right) {
            toolbelt.status =
                "Sketch Draw needs the pointer inside the game window to pick endpoints.".into();
        }
        motion_evr.clear();
        return;
    };

    let pencil_workflow = toolbelt.pencil_workflow_active();
    let start_intent = rect_start_intent(
        active_tool,
        mouse.just_pressed(MouseButton::Left),
        mouse.just_pressed(MouseButton::Right),
        ctrl_pressed(&keys),
        shift_pressed(&keys),
        toolbelt.opening_workflow_active(),
        toolbelt.room_workflow_active(),
    );

    if rect_should_commit_on_start_intent(start_intent, &draw) {
        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );
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
        let pencil_line = active_tool == ToolbeltTool::DrawRect
            && pencil_workflow
            && start_intent.fill
            && !start_intent.cut;
        let shape_workflow = if pencil_line || start_intent.cut {
            SketchShapeWorkflow::Rectangle
        } else {
            active_shape_workflow(&toolbelt)
        };
        let action = if pencil_line {
            RectDrawAction::Fill
        } else {
            rect_action_for_start_intent(start_intent, active_tool, normal)
        };
        draw.active = true;
        let start_input = rect_face_input_point(origin, dir, hit, prev);
        let mut start = if pencil_line {
            pencil_anchor_cell_from_ray(hit, prev, axis_u, axis_v, origin, dir)
        } else {
            rect_start_cell_from_ray(action, hit, prev, axis_u, axis_v, origin, dir)
        };
        if let Some(input) = start_input {
            start = apply_face_input_point_to_cell(start, input, axis_u, axis_v);
        }
        let semantic_start = screen_snap
            .and_then(|(cursor, view_projection, viewport)| {
                semantic_draw_screen_space_input_point(
                    &sketch_doc,
                    semantic_hover.0.as_ref(),
                    start_input.map(|input| input.point),
                    start,
                    normal,
                    axis_u,
                    axis_v,
                    pencil_line,
                    None,
                    cursor,
                    view_projection,
                    viewport,
                )
            })
            .or_else(|| {
                semantic_draw_input_point(
                    &sketch_doc,
                    semantic_hover.0.as_ref(),
                    start_input.map(|input| input.point),
                    start,
                    normal,
                    axis_u,
                    axis_v,
                    pencil_line,
                    None,
                )
            });
        if let Some(input) = semantic_start {
            start = input.cell;
        }
        draw.start = start;
        draw.current = start;
        draw.start_point = semantic_start.map(|input| input.point).unwrap_or_else(|| {
            if pencil_line {
                start_input
                    .map(|input| {
                        project_draw_input_point_to_locked_plane(input.point, start, normal, true)
                    })
                    .unwrap_or_else(|| pencil_cell_marker_point(start))
            } else {
                start_input
                    .map(|input| input.point)
                    .unwrap_or_else(|| start.as_vec3())
            }
        });
        draw.current_point = draw.start_point;
        draw.normal = normal;
        draw.axis_u = axis_u;
        draw.axis_v = axis_v;
        draw.motion_len = 0.0;
        draw.action = action;
        draw.button = start_intent.button;
        draw.smart_gesture = smart_tool;
        draw.room_cut = action == RectDrawAction::Cut && start_intent.room_cut;
        draw.pencil_line = pencil_line;
        draw.shape_workflow = shape_workflow;
        draw.inference = RectEndpointInference::None;
        draw.snap_kind = semantic_start
            .map(|input| input.kind)
            .or_else(|| start_input.and_then(|input| input.kind));
        draw.start_snap_kind = draw.snap_kind;
        draw.axis_lock = None;
        draw.tool_generation = toolbelt.selection_generation();
        draw.voxel = if action == RectDrawAction::Cut {
            AIR
        } else {
            builder.block.into()
        };
        draw.status_cells = 1;
        draw.pointer_valid = true;
        draw.click_finish = sketch_tool_uses_click_finish(active_tool, smart_tool);
        gesture_lock.lock(RECT_FILL_OWNER);
        tool_controller.begin_transaction(rect_preview_transaction_label(&draw));
        let start_status_suffix =
            rect_status_suffix(draw.start_snap_kind, RectEndpointInference::None, None);
        toolbelt.status = if smart_tool {
            format!(
                "{} start set.{} Drag to any block endpoint; release to {} the exact snapped length.",
                action.label(),
                start_status_suffix,
                action.preview_verb()
            )
        } else if draw.pencil_line {
            format!(
                "Pencil start set.{} Move to Endpoint/Midpoint/Face Center. Right locks X, Left locks Y, Up locks Z, Down returns to relative inference. RMB orbits.",
                start_status_suffix
            )
        } else if draw.shape_workflow != SketchShapeWorkflow::Rectangle {
            format!(
                "{} start set.{} Move to the snapped endpoint on this locked plane, click again to commit. RMB orbits.",
                draw.shape_workflow.label(),
                start_status_suffix
            )
        } else if draw.room_cut {
            format!(
                "Room start set.{} Move to size the wall/floor face, click again to hollow a livable volume.",
                start_status_suffix
            )
        } else if mode.build_tool() == Some(ToolbeltTool::Sculpt) {
            format!(
                "Push/Pull start set.{} Move on the locked face plane, click again to commit.",
                start_status_suffix
            )
        } else {
            format!(
                "Rectangle start set.{} Move to Endpoint/Midpoint/Face Center. Right locks X, Left locks Y, Up locks Z, Down returns to relative inference. RMB orbits.",
                start_status_suffix
            )
        };
    }

    if draw.active {
        gesture_lock.lock(RECT_FILL_OWNER);
        draw.pointer_valid = true;
        draw.axis_lock = update_rect_axis_lock(&keys, draw.axis_lock);
        if rect_draw_endpoint_updates(draw.smart_gesture, mouse.pressed(MouseButton::Right)) {
            for ev in motion_evr.read() {
                draw.motion_len += ev.delta.length();
            }
            if draw.pencil_line && draw.axis_lock.is_some() {
                let semantic_reference =
                    dda_voxel(&world, origin, dir, DRAW_REACH).and_then(|(hit, prev)| {
                        rect_face_input_point(origin, dir, hit, prev).map(|input| input.point)
                    });
                let semantic_input = screen_snap
                    .and_then(|(cursor, view_projection, viewport)| {
                        semantic_draw_screen_space_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            semantic_reference,
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            true,
                            draw.axis_lock,
                            cursor,
                            view_projection,
                            viewport,
                        )
                    })
                    .or_else(|| {
                        semantic_draw_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            semantic_reference,
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            true,
                            draw.axis_lock,
                        )
                    });
                if let (Some(axis_lock), Some(input)) = (draw.axis_lock, semantic_input) {
                    let (endpoint, point) = semantic_axis_locked_endpoint(
                        draw.start,
                        draw.start_point,
                        input,
                        axis_lock,
                    );
                    draw.current = endpoint;
                    draw.current_point = point;
                    draw.inference = RectEndpointInference::Axis;
                    draw.snap_kind = Some(input.kind);
                } else if let Some((endpoint, point)) =
                    snap_pencil_axis_endpoint_and_marker_from_ray(
                        draw.start,
                        draw.start_point,
                        draw.axis_lock,
                        origin,
                        dir,
                    )
                {
                    draw.current = endpoint;
                    draw.current_point = point;
                    draw.inference = RectEndpointInference::Axis;
                    draw.snap_kind = None;
                }
            } else if let Some((hit, prev)) = dda_voxel(&world, origin, dir, DRAW_REACH) {
                let input = rect_face_input_point(origin, dir, hit, prev);
                let semantic_input = screen_snap
                    .and_then(|(cursor, view_projection, viewport)| {
                        semantic_draw_screen_space_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            input.map(|input| input.point),
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            draw.pencil_line,
                            None,
                            cursor,
                            view_projection,
                            viewport,
                        )
                    })
                    .or_else(|| {
                        semantic_draw_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            input.map(|input| input.point),
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            draw.pencil_line,
                            None,
                        )
                    });
                draw.snap_kind = semantic_input
                    .map(|input| input.kind)
                    .or_else(|| input.and_then(|input| input.kind));
                let endpoint = if draw.pencil_line {
                    snap_pencil_endpoint_to_locked_plane_from_ray(
                        draw.start,
                        draw.normal,
                        draw.axis_u,
                        draw.axis_v,
                        hit,
                        prev,
                        origin,
                        dir,
                    )
                } else {
                    snap_rect_endpoint_to_locked_plane_from_ray(
                        draw.start,
                        draw.normal,
                        draw.axis_u,
                        draw.axis_v,
                        hit,
                        prev,
                        origin,
                        dir,
                    )
                };
                let endpoint = input
                    .map(|input| {
                        apply_face_input_point_to_cell(endpoint, input, draw.axis_u, draw.axis_v)
                    })
                    .unwrap_or(endpoint);
                let semantic_snap = semantic_input.is_some();
                let endpoint = semantic_input.map(|input| input.cell).unwrap_or(endpoint);
                let (endpoint, inference) = resolve_rect_endpoint_after_snap(
                    draw.start,
                    endpoint,
                    semantic_snap,
                    draw.axis_lock,
                    draw.axis_u,
                    draw.axis_v,
                    draw.reference_span,
                );
                draw.current = endpoint;
                draw.inference = inference;
                draw.current_point = if draw.pencil_line {
                    pencil_display_point_for_endpoint(
                        endpoint,
                        semantic_input,
                        input,
                        draw.start,
                        draw.start_point,
                        draw.normal,
                        inference,
                        draw.axis_lock,
                    )
                } else {
                    semantic_input
                        .filter(|input| {
                            input.cell == endpoint && inference == RectEndpointInference::None
                        })
                        .map(|input| input.point)
                        .or_else(|| {
                            input.map(|input| {
                                project_face_point_to_locked_plane(
                                    input.point,
                                    draw.start,
                                    draw.normal,
                                )
                            })
                        })
                        .unwrap_or_else(|| endpoint.as_vec3())
                };
            } else if let Some(endpoint) = snap_rect_endpoint_from_locked_plane_ray(
                draw.start,
                draw.normal,
                draw.axis_u,
                draw.axis_v,
                origin,
                dir,
            ) {
                let semantic_input = screen_snap
                    .and_then(|(cursor, view_projection, viewport)| {
                        semantic_draw_screen_space_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            Some(pencil_cell_marker_point(endpoint)),
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            draw.pencil_line,
                            None,
                            cursor,
                            view_projection,
                            viewport,
                        )
                    })
                    .or_else(|| {
                        semantic_draw_input_point(
                            &sketch_doc,
                            semantic_hover.0.as_ref(),
                            Some(pencil_cell_marker_point(endpoint)),
                            draw.start,
                            draw.normal,
                            draw.axis_u,
                            draw.axis_v,
                            draw.pencil_line,
                            None,
                        )
                    });
                let semantic_snap = semantic_input.is_some();
                let endpoint = semantic_input.map(|input| input.cell).unwrap_or(endpoint);
                let (endpoint, inference) = resolve_rect_endpoint_after_snap(
                    draw.start,
                    endpoint,
                    semantic_snap,
                    draw.axis_lock,
                    draw.axis_u,
                    draw.axis_v,
                    draw.reference_span,
                );
                draw.current = endpoint;
                draw.inference = inference;
                draw.current_point = semantic_input
                    .filter(|input| {
                        input.cell == endpoint && inference == RectEndpointInference::None
                    })
                    .map(|input| input.point)
                    .unwrap_or_else(|| {
                        if draw.pencil_line {
                            pencil_cell_marker_point(endpoint)
                        } else {
                            endpoint.as_vec3()
                        }
                    });
                draw.snap_kind = semantic_input.map(|input| input.kind);
            }
            let raw_cells = draw_preview_cell_count(&draw);
            draw.status_cells = raw_cells.min(DRAW_CELL_CAP);
            let status_suffix = rect_status_suffix(draw.snap_kind, draw.inference, draw.axis_lock);
            let readout = rect_alignment_readout(
                draw.start,
                draw.current,
                draw.snap_kind,
                draw.inference,
                draw.axis_lock,
            );
            let readout_suffix = if readout.is_empty() {
                String::new()
            } else {
                format!(" [{readout}]")
            };
            let action_label = if draw.pencil_line {
                "Pencil"
            } else if draw.room_cut {
                "Smart Room Hollow"
            } else if draw.action == RectDrawAction::Fill {
                draw.shape_workflow.preview_label()
            } else {
                draw.action.label()
            };
            toolbelt.status = if raw_cells > DRAW_CELL_CAP {
                format!(
                    "{} preview capped: {} of {} snapped cells.{}{} {}",
                    action_label,
                    DRAW_CELL_CAP,
                    raw_cells,
                    status_suffix,
                    readout_suffix,
                    rect_commit_hint(&draw)
                )
            } else {
                format!(
                    "{} preview: {} snapped cells.{}{} {}",
                    action_label,
                    draw.status_cells,
                    status_suffix,
                    readout_suffix,
                    rect_commit_hint(&draw)
                )
            };
        } else {
            motion_evr.clear();
            toolbelt.status =
                "Sketch Draw orbiting: endpoint held. Release RMB to continue snapping; click commits."
                    .into();
        }
    } else {
        motion_evr.clear();
    }

    if draw.button.just_released(&mouse) && rect_should_commit_on_release(&draw) {
        if !draw.smart_gesture && draw.motion_len < 4.0 && draw.status_cells <= 1 {
            draw.click_finish = true;
            toolbelt.status =
                "Sketch Draw anchor set. Move to grow line/face, click commits, RMB orbits, Esc cancels."
                    .into();
        } else {
            commit_rect_fill(
                &mut draw,
                &mut world,
                &mut history,
                &mut toolbelt,
                &mut tool_controller,
                &mut sketch_doc,
                &mut sketch_links,
            );
            gesture_lock.release(RECT_FILL_OWNER);
        }
    }
}

fn rect_commit_hint(draw: &RectDrawState) -> &'static str {
    if draw.click_finish {
        "Click again to commit, Esc cancels."
    } else {
        "Release commits, Esc cancels."
    }
}

fn rect_preview_transaction_label(draw: &RectDrawState) -> &'static str {
    if draw.pencil_line {
        "Pencil preview"
    } else if draw.room_cut {
        "Room preview"
    } else if draw.action == RectDrawAction::Cut {
        "Opening preview"
    } else if draw.shape_workflow != SketchShapeWorkflow::Rectangle {
        draw.shape_workflow.history_label()
    } else {
        "Rectangle preview"
    }
}

fn draw_input_ray(
    active_tool: ToolbeltTool,
    cursor_locked: bool,
    cursor_visible: bool,
    cursor_position: Option<Vec2>,
    camera: &Camera,
    camera_tf: &GlobalTransform,
) -> Option<(Vec3, Vec3)> {
    if editor_pointer_ray_available(active_tool, cursor_locked, cursor_visible, cursor_position) {
        if let Some(ray) =
            cursor_position.and_then(|cursor| camera.viewport_to_world(camera_tf, cursor))
        {
            return Some((ray.origin, *ray.direction));
        }
    }
    if editor_pointer_tool_requires_cursor(active_tool) {
        return None;
    }
    Some((camera_tf.translation(), camera_tf.forward().as_vec3()))
}

fn editor_pointer_tool_requires_cursor(active_tool: ToolbeltTool) -> bool {
    matches!(active_tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt)
}

fn editor_pointer_ray_available(
    active_tool: ToolbeltTool,
    _cursor_locked: bool,
    cursor_visible: bool,
    cursor_position: Option<Vec2>,
) -> bool {
    editor_pointer_tool_requires_cursor(active_tool) && cursor_visible && cursor_position.is_some()
}

fn rect_draw_endpoint_updates(smart_gesture: bool, right_held: bool) -> bool {
    smart_gesture || !right_held
}

fn commit_rect_fill(
    draw: &mut RectDrawState,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    toolbelt: &mut ToolbeltState,
    tool_controller: &mut crate::sketch_model::ToolController,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
) {
    let should_chain_pencil = draw.pencil_line && draw.action == RectDrawAction::Fill;
    let chain_start = draw.current;
    let chain_start_point = if draw.current_point.is_finite() {
        draw.current_point
    } else {
        pencil_cell_marker_point(chain_start)
    };
    let commit_normal = draw.normal;
    let commit_snap_kind = draw.snap_kind;
    let next_reference_span =
        rect_reference_span(draw.start, draw.current, draw.axis_u, draw.axis_v);
    let cells = match draw.action {
        RectDrawAction::Fill if draw.pencil_line => {
            pencil_line_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP)
        }
        RectDrawAction::Fill => sketch_shape_cells(
            draw.shape_workflow,
            draw.start,
            draw.current,
            draw.normal,
            DRAW_CELL_CAP,
        ),
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
        clear_rect_preview(draw);
        return;
    }

    let mut batch = WorldEditBatch::default();
    let mut changes: Vec<(IVec3, Voxel, Voxel)> = Vec::with_capacity(cells.len());
    for &pos in &cells {
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
        } else if draw.pencil_line {
            format!("Pencil line {} cells", changed)
        } else if draw.action == RectDrawAction::Fill
            && draw.shape_workflow != SketchShapeWorkflow::Rectangle
        {
            format!("{} {} cells", draw.shape_workflow.history_label(), changed)
        } else {
            format!("{} {} cells", draw.action.history_label(), changed)
        };
        let sketch_meta = record_rect_semantics_for_history(draw, &cells, sketch_doc, sketch_links);
        history.record_external_with_sketch_meta(label.clone(), changes, sketch_meta);
        tool_controller.begin_transaction(label);
        let _ = tool_controller.commit_transaction();
        toolbelt.status = if should_chain_pencil {
            format!(
                "Pencil line committed: {} selected, {} changed cells. Next endpoint starts from {},{},{}.",
                selected, changed, chain_start.x, chain_start.y, chain_start.z
            )
        } else {
            format!(
                "{} committed: {} selected, {} changed cells. Ctrl+Z undo, Ctrl+Y redo.",
                if draw.pencil_line {
                    "Pencil line"
                } else if draw.room_cut {
                    "Smart Room Hollow"
                } else if draw.action == RectDrawAction::Fill {
                    draw.shape_workflow.preview_label()
                } else {
                    draw.action.label()
                },
                selected,
                changed
            )
        };
    } else {
        let label = if should_chain_pencil {
            format!("Pencil connection {} cells", selected)
        } else if draw.room_cut {
            format!("Smart room hollow selection {} cells", selected)
        } else if draw.pencil_line {
            format!("Pencil selection {} cells", selected)
        } else if draw.action == RectDrawAction::Fill
            && draw.shape_workflow != SketchShapeWorkflow::Rectangle
        {
            format!(
                "{} selection {} cells",
                draw.shape_workflow.history_label(),
                selected
            )
        } else {
            format!(
                "{} selection {} cells",
                draw.action.history_label(),
                selected
            )
        };
        let sketch_meta = record_rect_semantics_for_history(draw, &cells, sketch_doc, sketch_links);
        history.record_external_with_sketch_meta(label.clone(), Vec::new(), sketch_meta);
        tool_controller.begin_transaction(label.clone());
        let _ = tool_controller.commit_transaction();

        if should_chain_pencil {
            toolbelt.status = format!(
                "Pencil connected existing cells. Next endpoint starts from {},{},{}.",
                chain_start.x, chain_start.y, chain_start.z
            );
        } else {
            toolbelt.status = format!(
                "{} already matched {} cells; recorded selectable sketch object without voxel edits.",
                if draw.pencil_line {
                    "Pencil line"
                } else if draw.room_cut {
                    "Smart Room Hollow"
                } else if draw.action == RectDrawAction::Fill {
                    draw.shape_workflow.preview_label()
                } else {
                    draw.action.label()
                },
                selected
            );
        }
    }
    if should_chain_pencil {
        draw.active = true;
        draw.click_finish = true;
        draw.start = chain_start;
        draw.current = chain_start;
        draw.start_point = chain_start_point;
        draw.current_point = draw.start_point;
        draw.motion_len = 0.0;
        draw.status_cells = 1;
        draw.pointer_valid = true;
        draw.inference = RectEndpointInference::None;
        draw.snap_kind = Some(RectFaceSnapKind::Endpoint);
        draw.start_snap_kind = Some(RectFaceSnapKind::Endpoint);
    } else {
        clear_rect_preview(draw);
    }
    if next_reference_span != IVec2::ZERO {
        draw.reference_span = next_reference_span;
    }
    begin_rect_visual_feedback(
        draw,
        RectVisualFeedbackKind::Commit,
        chain_start_point,
        commit_normal,
        commit_snap_kind,
    );
}

fn record_rect_semantics_for_history(
    draw: &RectDrawState,
    cells: &[IVec3],
    sketch_doc: &mut crate::sketch_model::SketchDocument,
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
) -> Option<BuilderHistorySketchMeta> {
    let records = match record_rect_semantics(draw, sketch_doc) {
        Ok(records) => records,
        Err(error) => {
            warn!("sketch model: could not record draw semantic entity: {error}");
            return None;
        }
    };
    if records.is_empty() {
        return None;
    }
    register_rect_semantic_links(
        draw,
        cells,
        sketch_doc.active_context(),
        &records,
        sketch_links,
    );
    Some(BuilderHistorySketchMeta::SketchCreated {
        link_snapshots: sketch_links.snapshot_entities(records.iter().map(|(entity, _)| *entity)),
    })
}

fn record_rect_semantics(
    draw: &RectDrawState,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
) -> Result<
    Vec<(
        crate::sketch_model::SketchId,
        crate::sketch_model::SketchVoxelLinkRole,
    )>,
    crate::sketch_model::SketchModelError,
> {
    if draw.pencil_line && draw.action == RectDrawAction::Fill {
        let edge = sketch_doc.draw_pencil_line(
            sketch_doc.active_context(),
            draw.start_point,
            draw.current_point,
        )?;
        Ok(vec![(
            edge,
            crate::sketch_model::SketchVoxelLinkRole::Stroke,
        )])
    } else if draw.action == RectDrawAction::Fill
        && draw.shape_workflow != SketchShapeWorkflow::Rectangle
    {
        record_shape_semantics(draw, sketch_doc)
    } else {
        let (origin, axis_u, axis_v) = semantic_rect_axes(draw);
        let face_label = if draw.action == RectDrawAction::Cut {
            "Opening face"
        } else {
            "Rectangle face"
        };
        let face = match sketch_doc.draw_rectangle_face(
            sketch_doc.active_context(),
            origin,
            axis_u,
            axis_v,
            face_label,
        ) {
            Ok(face) => face,
            Err(error) => {
                warn!("sketch model: could not record rectangle semantic face: {error}");
                return Err(error);
            }
        };
        let mut records = vec![(face, crate::sketch_model::SketchVoxelLinkRole::Face)];
        if draw.action == RectDrawAction::Cut {
            if draw.room_cut {
                let depth = smart_room_cut_depth(
                    (component_by_axis(draw.current, draw.axis_u)
                        - component_by_axis(draw.start, draw.axis_u))
                    .abs(),
                    (component_by_axis(draw.current, draw.axis_v)
                        - component_by_axis(draw.start, draw.axis_v))
                    .abs(),
                ) as f32;
                let room = sketch_doc.create_hollow_room(face, 1.0, depth)?;
                records.push((room, crate::sketch_model::SketchVoxelLinkRole::Room));
            } else {
                let center = origin + (axis_u + axis_v) * 0.5;
                let size = axis_u.abs() + axis_v.abs();
                let opening = sketch_doc.cut_opening_through_face(
                    face,
                    center,
                    size,
                    RECT_CUT_DEPTH_CAP as f32,
                )?;
                records.push((opening, crate::sketch_model::SketchVoxelLinkRole::Opening));
            }
        }
        Ok(records)
    }
}

fn record_shape_semantics(
    draw: &RectDrawState,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
) -> Result<
    Vec<(
        crate::sketch_model::SketchId,
        crate::sketch_model::SketchVoxelLinkRole,
    )>,
    crate::sketch_model::SketchModelError,
> {
    let context = sketch_doc.active_context();
    let center = ivec3_as_vec3(draw.start);
    let normal = draw.normal.as_vec3();
    let radius = sketch_shape_radius(draw.start, draw.current, draw.axis_u, draw.axis_v) as f32;
    let (entity, role) = match draw.shape_workflow {
        SketchShapeWorkflow::Circle => sketch_doc
            .draw_circle_face(context, center, normal, radius, 24, "Circle face")
            .map(|entity| (entity, crate::sketch_model::SketchVoxelLinkRole::Shape)),
        SketchShapeWorkflow::Polygon => sketch_doc
            .draw_polygon_face(context, center, normal, radius, 6, "Polygon face")
            .map(|entity| (entity, crate::sketch_model::SketchVoxelLinkRole::Shape)),
        SketchShapeWorkflow::Arc => sketch_doc
            .draw_arc_curve(
                context,
                center,
                normal,
                radius,
                draw.axis_u.as_vec3(),
                std::f32::consts::FRAC_PI_2,
                16,
                "Arc curve",
            )
            .map(|entity| (entity, crate::sketch_model::SketchVoxelLinkRole::Stroke)),
        SketchShapeWorkflow::Freehand => sketch_doc
            .draw_freehand_curve(
                context,
                [ivec3_as_vec3(draw.start), ivec3_as_vec3(draw.current)],
                "Freehand stroke",
            )
            .map(|entity| (entity, crate::sketch_model::SketchVoxelLinkRole::Stroke)),
        SketchShapeWorkflow::Rectangle => {
            return Ok(Vec::new());
        }
    }?;
    Ok(vec![(entity, role)])
}

fn register_rect_semantic_links(
    draw: &RectDrawState,
    changed_cells: &[IVec3],
    context: crate::sketch_model::SketchId,
    records: &[(
        crate::sketch_model::SketchId,
        crate::sketch_model::SketchVoxelLinkRole,
    )],
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
) {
    let expose_face = draw.action == RectDrawAction::Fill;
    for (entity, role) in records {
        let link = crate::sketch_model::SketchVoxelLink::new(*entity, context, *role);
        if expose_face {
            sketch_links.link_face_cells(changed_cells.iter().copied(), draw.normal, link);
        } else {
            sketch_links.link_cells(changed_cells.iter().copied(), link);
        }
    }
}

fn semantic_rect_axes(draw: &RectDrawState) -> (Vec3, Vec3, Vec3) {
    let origin = ivec3_as_vec3(draw.start);
    let span_u =
        component_by_axis(draw.current, draw.axis_u) - component_by_axis(draw.start, draw.axis_u);
    let span_v =
        component_by_axis(draw.current, draw.axis_v) - component_by_axis(draw.start, draw.axis_v);
    (
        origin,
        draw.axis_u.as_vec3() * span_u as f32,
        draw.axis_v.as_vec3() * span_v as f32,
    )
}

fn ivec3_as_vec3(value: IVec3) -> Vec3 {
    Vec3::new(value.x as f32, value.y as f32, value.z as f32)
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

fn pencil_anchor_cell(hit: IVec3, adjacent: IVec3) -> IVec3 {
    let normal = adjacent - hit;
    if normal == IVec3::Y {
        adjacent
    } else {
        hit
    }
}

fn pencil_anchor_cell_from_ray(
    hit: IVec3,
    adjacent: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> IVec3 {
    let mut cell = pencil_anchor_cell(hit, adjacent);
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return cell;
    }
    if let Some(face_hit) = ray_face_hit_point(ray_origin, ray_dir, hit, adjacent) {
        let fallback_u = component_by_axis(cell, axis_u);
        let fallback_v = component_by_axis(cell, axis_v);
        set_component_by_axis(
            &mut cell,
            axis_u,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_u), fallback_u),
        );
        set_component_by_axis(
            &mut cell,
            axis_v,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_v), fallback_v),
        );
    }
    cell
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
        let fallback_u = component_by_axis(start, axis_u);
        let fallback_v = component_by_axis(start, axis_v);
        set_component_by_axis(
            &mut start,
            axis_u,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_u), fallback_u),
        );
        set_component_by_axis(
            &mut start,
            axis_v,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_v), fallback_v),
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
        let fallback_u = component_by_axis(snapped, axis_u);
        let fallback_v = component_by_axis(snapped, axis_v);
        set_component_by_axis(
            &mut snapped,
            axis_u,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_u), fallback_u),
        );
        set_component_by_axis(
            &mut snapped,
            axis_v,
            face_axis_component_to_cell(vec_component_by_axis(face_hit, axis_v), fallback_v),
        );
        set_component_by_index(
            &mut snapped,
            plane_axis,
            component_by_index(start, plane_axis),
        );
    }
    snapped
}

fn snap_pencil_endpoint_to_locked_plane_from_ray(
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
        pencil_anchor_cell_from_ray(hit, adjacent, axis_u, axis_v, ray_origin, ray_dir);
    if let Some(plane_axis) = normal_axis(normal) {
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

#[cfg(test)]
fn snap_pencil_endpoint_to_axis_from_ray(
    start: IVec3,
    axis_lock: Option<RectAxisLock>,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<IVec3> {
    snap_pencil_axis_endpoint_and_marker_from_ray(
        start,
        pencil_cell_marker_point(start),
        axis_lock,
        ray_origin,
        ray_dir,
    )
    .map(|(endpoint, _)| endpoint)
}

fn snap_pencil_axis_endpoint_and_marker_from_ray(
    start: IVec3,
    start_marker: Vec3,
    axis_lock: Option<RectAxisLock>,
    ray_origin: Vec3,
    ray_dir: Vec3,
) -> Option<(IVec3, Vec3)> {
    let axis_lock = axis_lock?;
    let locked_point = crate::sketch_model::closest_point_on_locked_axis_from_ray(
        ray_origin,
        ray_dir,
        start_marker,
        axis_lock.axis_vec3(),
    )?;
    let mut endpoint = start;
    set_component_by_axis(
        &mut endpoint,
        axis_lock.axis(),
        center_axis_component_to_cell(
            vec_component_by_axis(locked_point, axis_lock.axis()),
            component_by_axis(start, axis_lock.axis()),
        ),
    );
    let mut marker = start_marker;
    set_vec_component_by_axis(
        &mut marker,
        axis_lock.axis(),
        vec_component_by_axis(locked_point, axis_lock.axis()),
    );
    Some((endpoint, marker))
}

fn apply_axis_lock_to_endpoint(
    start: IVec3,
    endpoint: IVec3,
    axis_lock: Option<RectAxisLock>,
) -> IVec3 {
    let Some(axis_lock) = axis_lock else {
        return endpoint;
    };
    let mut locked = start;
    set_component_by_axis(
        &mut locked,
        axis_lock.axis(),
        component_by_axis(endpoint, axis_lock.axis()),
    );
    locked
}

fn resolve_rect_endpoint_after_snap(
    start: IVec3,
    endpoint: IVec3,
    semantic_snap: bool,
    axis_lock: Option<RectAxisLock>,
    axis_u: IVec3,
    axis_v: IVec3,
    reference_span: IVec2,
) -> (IVec3, RectEndpointInference) {
    let endpoint = apply_axis_lock_to_endpoint(start, endpoint, axis_lock);
    if axis_lock.is_some() {
        return (endpoint, RectEndpointInference::Axis);
    }
    if semantic_snap {
        return (endpoint, RectEndpointInference::None);
    }
    infer_rect_endpoint_with_reference(start, endpoint, axis_u, axis_v, reference_span)
}

#[cfg(test)]
fn infer_rect_endpoint(
    start: IVec3,
    raw: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
) -> (IVec3, RectEndpointInference) {
    infer_rect_endpoint_with_reference(start, raw, axis_u, axis_v, IVec2::ZERO)
}

fn infer_rect_endpoint_with_reference(
    start: IVec3,
    raw: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    reference_span: IVec2,
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
        return snap_endpoint_to_reference_length(
            start,
            inferred,
            axis_u,
            axis_v,
            reference_span,
            RectEndpointInference::Axis,
        );
    }
    if av > 0 && au > 0 && (au <= RECT_AXIS_JITTER || av as f32 >= au as f32 * RECT_AXIS_RATIO) {
        set_component_by_axis(&mut inferred, axis_u, component_by_axis(start, axis_u));
        return snap_endpoint_to_reference_length(
            start,
            inferred,
            axis_u,
            axis_v,
            reference_span,
            RectEndpointInference::Axis,
        );
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

    snap_endpoint_to_reference_length(
        start,
        raw,
        axis_u,
        axis_v,
        reference_span,
        RectEndpointInference::None,
    )
}

fn snap_endpoint_to_reference_length(
    start: IVec3,
    raw: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    reference_span: IVec2,
    fallback: RectEndpointInference,
) -> (IVec3, RectEndpointInference) {
    let mut snapped = raw;
    let mut used_reference = false;
    let references = [reference_span.x.abs(), reference_span.y.abs()];
    for axis in [axis_u, axis_v] {
        let delta = component_by_axis(snapped, axis) - component_by_axis(start, axis);
        let span = delta.abs();
        let reference = references
            .iter()
            .copied()
            .filter(|reference| *reference > 0)
            .filter(|reference| {
                span > 0 && (span - *reference).abs() <= RECT_EQUAL_LENGTH_TOLERANCE
            })
            .min_by_key(|reference| (span - *reference).abs());
        if let Some(reference) = reference {
            set_component_by_axis(
                &mut snapped,
                axis,
                component_by_axis(start, axis) + delta.signum() * reference,
            );
            used_reference = true;
        }
    }
    if used_reference {
        (snapped, RectEndpointInference::ReferenceLength)
    } else {
        (raw, fallback)
    }
}

fn rect_reference_span(start: IVec3, current: IVec3, axis_u: IVec3, axis_v: IVec3) -> IVec2 {
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return IVec2::ZERO;
    }
    IVec2::new(
        (component_by_axis(current, axis_u) - component_by_axis(start, axis_u)).abs(),
        (component_by_axis(current, axis_v) - component_by_axis(start, axis_v)).abs(),
    )
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

fn face_point_by_indices(
    axis: usize,
    plane: f32,
    u_axis: usize,
    u: f32,
    v_axis: usize,
    v: f32,
) -> Vec3 {
    let mut components = [0.0; 3];
    components[axis] = plane;
    components[u_axis] = u;
    components[v_axis] = v;
    Vec3::new(components[0], components[1], components[2])
}

fn rect_face_input_point(
    ray_origin: Vec3,
    ray_dir: Vec3,
    hit: IVec3,
    adjacent: IVec3,
) -> Option<RectFaceInputPoint> {
    let face_hit = ray_face_hit_point(ray_origin, ray_dir, hit, adjacent)?;
    Some(
        nearest_rect_face_input_point(face_hit, hit, adjacent).unwrap_or(RectFaceInputPoint {
            point: face_hit,
            kind: None,
        }),
    )
}

fn nearest_rect_face_input_point(
    face_hit: Vec3,
    hit: IVec3,
    adjacent: IVec3,
) -> Option<RectFaceInputPoint> {
    let normal = adjacent - hit;
    let axis = normal_axis(normal)?;
    let plane = if component_by_index(normal, axis) > 0 {
        component_by_index(hit, axis) as f32 + 1.0
    } else {
        component_by_index(hit, axis) as f32
    };
    let axes: Vec<usize> = (0..3).filter(|component| *component != axis).collect();
    let u_axis = axes[0];
    let v_axis = axes[1];
    let u0 = component_by_index(hit, u_axis) as f32;
    let v0 = component_by_index(hit, v_axis) as f32;
    let u1 = u0 + 1.0;
    let v1 = v0 + 1.0;
    let um = u0 + 0.5;
    let vm = v0 + 0.5;

    let mut best: Option<(RectFaceSnapKind, Vec3, f32)> = None;
    for (u, v, kind) in [
        (u0, v0, RectFaceSnapKind::Endpoint),
        (u0, v1, RectFaceSnapKind::Endpoint),
        (u1, v0, RectFaceSnapKind::Endpoint),
        (u1, v1, RectFaceSnapKind::Endpoint),
        (um, v0, RectFaceSnapKind::Midpoint),
        (um, v1, RectFaceSnapKind::Midpoint),
        (u0, vm, RectFaceSnapKind::Midpoint),
        (u1, vm, RectFaceSnapKind::Midpoint),
        (um, vm, RectFaceSnapKind::FaceCenter),
    ] {
        let point = face_point_by_indices(axis, plane, u_axis, u, v_axis, v);
        let distance = face_hit.distance_squared(point);
        if best.is_none_or(|(_, _, best_distance)| distance < best_distance) {
            best = Some((kind, point, distance));
        }
    }
    best.and_then(|(kind, point, distance)| {
        (distance <= RECT_FACE_SNAP_RADIUS * RECT_FACE_SNAP_RADIUS).then_some(RectFaceInputPoint {
            point,
            kind: Some(kind),
        })
    })
}

fn apply_face_input_point_to_cell(
    mut cell: IVec3,
    input: RectFaceInputPoint,
    axis_u: IVec3,
    axis_v: IVec3,
) -> IVec3 {
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return cell;
    }
    let fallback_u = component_by_axis(cell, axis_u);
    let fallback_v = component_by_axis(cell, axis_v);
    set_component_by_axis(
        &mut cell,
        axis_u,
        face_axis_component_to_cell(vec_component_by_axis(input.point, axis_u), fallback_u),
    );
    set_component_by_axis(
        &mut cell,
        axis_v,
        face_axis_component_to_cell(vec_component_by_axis(input.point, axis_v), fallback_v),
    );
    cell
}

fn semantic_candidate_kind_bias(kind: RectFaceSnapKind) -> f32 {
    match kind {
        RectFaceSnapKind::Endpoint => 0.0,
        RectFaceSnapKind::Midpoint => 0.08,
        RectFaceSnapKind::FaceCenter => 0.16,
    }
}

fn best_semantic_draw_candidate(
    sketch_doc: &crate::sketch_model::SketchDocument,
    hover: &crate::sketch_model::HitRecord,
    reference_point: Vec3,
) -> Option<(crate::sketch_model::InferenceCandidate, RectFaceSnapKind)> {
    let mut candidates = sketch_doc.entity_inference_candidates(hover.entity).ok()?;
    candidates.extend(
        crate::sketch_model::InferenceService::from_pick(sketch_doc, hover, Some(reference_point))
            .ok()?,
    );
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let kind = rect_face_snap_from_inference_kind(candidate.kind)?;
            candidate.point.is_finite().then_some((candidate, kind))
        })
        .filter_map(|(candidate, kind)| {
            let distance = candidate.point.distance(reference_point);
            (distance <= SEMANTIC_DRAW_POINT_RADIUS).then_some((candidate, kind, distance))
        })
        .min_by(|(_, kind_a, distance_a), (_, kind_b, distance_b)| {
            let score_a = *distance_a + semantic_candidate_kind_bias(*kind_a);
            let score_b = *distance_b + semantic_candidate_kind_bias(*kind_b);
            score_a.total_cmp(&score_b)
        })
        .map(|(candidate, kind, _)| (candidate, kind))
}

fn project_draw_input_point_to_locked_plane(
    point: Vec3,
    start: IVec3,
    normal: IVec3,
    pencil_line: bool,
) -> Vec3 {
    let Some(plane_axis) = normal_axis(normal) else {
        return point;
    };
    let mut projected = point;
    let plane = component_by_index(start, plane_axis) as f32 + if pencil_line { 0.5 } else { 0.0 };
    match plane_axis {
        0 => projected.x = plane,
        1 => projected.y = plane,
        _ => projected.z = plane,
    }
    projected
}

fn semantic_draw_point_to_cell(
    start: IVec3,
    point: Vec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
) -> IVec3 {
    let mut cell = start;
    if !is_cardinal_axis(axis_u) || !is_cardinal_axis(axis_v) {
        return cell;
    }
    set_component_by_axis(
        &mut cell,
        axis_u,
        center_axis_component_to_cell(
            vec_component_by_axis(point, axis_u),
            component_by_axis(start, axis_u),
        ),
    );
    set_component_by_axis(
        &mut cell,
        axis_v,
        center_axis_component_to_cell(
            vec_component_by_axis(point, axis_v),
            component_by_axis(start, axis_v),
        ),
    );
    if let Some(plane_axis) = normal_axis(normal) {
        set_component_by_index(&mut cell, plane_axis, component_by_index(start, plane_axis));
    }
    cell
}

fn semantic_draw_input_point(
    sketch_doc: &crate::sketch_model::SketchDocument,
    hover: Option<&crate::sketch_model::HitRecord>,
    reference_point: Option<Vec3>,
    start: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    pencil_line: bool,
    axis_lock: Option<RectAxisLock>,
) -> Option<SemanticDrawInputPoint> {
    let hover = hover?;
    let reference_point = reference_point.unwrap_or(hover.world_point);
    let (candidate, kind) = best_semantic_draw_candidate(sketch_doc, hover, reference_point)?;
    let point = project_semantic_draw_candidate_point(
        candidate.point,
        start,
        normal,
        pencil_line,
        axis_lock,
    );
    let cell = semantic_draw_point_to_cell(start, point, normal, axis_u, axis_v);
    Some(SemanticDrawInputPoint { cell, point, kind })
}

fn semantic_draw_screen_space_input_point(
    sketch_doc: &crate::sketch_model::SketchDocument,
    hover: Option<&crate::sketch_model::HitRecord>,
    reference_point: Option<Vec3>,
    start: IVec3,
    normal: IVec3,
    axis_u: IVec3,
    axis_v: IVec3,
    pencil_line: bool,
    axis_lock: Option<RectAxisLock>,
    cursor_screen: Vec2,
    view_projection: Mat4,
    viewport_size: Vec2,
) -> Option<SemanticDrawInputPoint> {
    let mut candidates = sketch_doc.active_context_inference_candidates().ok()?;
    if let Some(hover) = hover {
        let hover_reference = reference_point.unwrap_or(hover.world_point);
        candidates.extend(
            crate::sketch_model::InferenceService::from_pick(
                sketch_doc,
                hover,
                Some(hover_reference),
            )
            .ok()?,
        );
    }
    let chosen = crate::sketch_model::best_screen_space_inference(
        candidates,
        cursor_screen,
        view_projection,
        viewport_size,
        crate::sketch_model::ScreenSpaceSnapSettings {
            radius_pixels: SEMANTIC_DRAW_SCREEN_RADIUS,
            ..Default::default()
        },
        None,
    )?;
    let kind = rect_face_snap_from_inference_kind(chosen.inference.kind)?;
    let point = project_semantic_draw_candidate_point(
        chosen.inference.point,
        start,
        normal,
        pencil_line,
        axis_lock,
    );
    let cell = semantic_draw_point_to_cell(start, point, normal, axis_u, axis_v);
    Some(SemanticDrawInputPoint { cell, point, kind })
}

fn project_semantic_draw_candidate_point(
    candidate_point: Vec3,
    start: IVec3,
    normal: IVec3,
    pencil_line: bool,
    axis_lock: Option<RectAxisLock>,
) -> Vec3 {
    let mut point =
        project_draw_input_point_to_locked_plane(candidate_point, start, normal, pencil_line);
    if pencil_line {
        if let Some(axis_lock) = axis_lock {
            set_vec_component_by_axis(
                &mut point,
                axis_lock.axis(),
                vec_component_by_axis(candidate_point, axis_lock.axis()),
            );
        }
    }
    point
}

fn semantic_axis_locked_endpoint(
    start: IVec3,
    start_marker: Vec3,
    input: SemanticDrawInputPoint,
    axis_lock: RectAxisLock,
) -> (IVec3, Vec3) {
    let mut endpoint = start;
    set_component_by_axis(
        &mut endpoint,
        axis_lock.axis(),
        center_axis_component_to_cell(
            vec_component_by_axis(input.point, axis_lock.axis()),
            component_by_axis(start, axis_lock.axis()),
        ),
    );
    let mut marker = start_marker;
    set_vec_component_by_axis(
        &mut marker,
        axis_lock.axis(),
        vec_component_by_axis(input.point, axis_lock.axis()),
    );
    (endpoint, marker)
}

fn pencil_display_point_for_endpoint(
    endpoint: IVec3,
    semantic_input: Option<SemanticDrawInputPoint>,
    face_input: Option<RectFaceInputPoint>,
    start: IVec3,
    start_marker: Vec3,
    normal: IVec3,
    inference: RectEndpointInference,
    axis_lock: Option<RectAxisLock>,
) -> Vec3 {
    if inference == RectEndpointInference::Axis {
        if let Some(axis_lock) = axis_lock {
            if let Some(input) = semantic_input {
                return pencil_axis_locked_marker_from_point_with_start(
                    start,
                    start_marker,
                    normal,
                    axis_lock,
                    input.point,
                );
            }
            if let Some(input) = face_input {
                return pencil_axis_locked_marker_from_point_with_start(
                    start,
                    start_marker,
                    normal,
                    axis_lock,
                    input.point,
                );
            }
        }
    }
    if inference == RectEndpointInference::None {
        if let Some(input) = semantic_input.filter(|input| input.cell == endpoint) {
            return input.point;
        }
        if let Some(input) = face_input {
            return project_draw_input_point_to_locked_plane(input.point, start, normal, true);
        }
    }
    pencil_cell_marker_point(endpoint)
}

fn pencil_axis_locked_marker_from_point_with_start(
    start: IVec3,
    start_marker: Vec3,
    normal: IVec3,
    axis_lock: RectAxisLock,
    point: Vec3,
) -> Vec3 {
    let projected =
        project_semantic_draw_candidate_point(point, start, normal, true, Some(axis_lock));
    let mut marker = start_marker;
    set_vec_component_by_axis(
        &mut marker,
        axis_lock.axis(),
        vec_component_by_axis(projected, axis_lock.axis()),
    );
    marker
}

fn project_face_point_to_locked_plane(point: Vec3, start: IVec3, normal: IVec3) -> Vec3 {
    let Some(plane_axis) = normal_axis(normal) else {
        return point;
    };
    let mut projected = point;
    match plane_axis {
        0 => projected.x = start.x as f32,
        1 => projected.y = start.y as f32,
        _ => projected.z = start.z as f32,
    }
    projected
}

fn pencil_cell_marker_point(cell: IVec3) -> Vec3 {
    cell.as_vec3() + Vec3::splat(0.5)
}

#[cfg(test)]
fn classify_rect_face_snap(
    face_hit: Vec3,
    hit: IVec3,
    adjacent: IVec3,
) -> Option<RectFaceSnapKind> {
    nearest_rect_face_input_point(face_hit, hit, adjacent).and_then(|input| input.kind)
}

fn face_axis_component_to_cell(value: f32, face_cell_component: i32) -> i32 {
    if !value.is_finite() {
        return face_cell_component;
    }
    let min = face_cell_component as f32;
    let max = min + 1.0 - 0.0001;
    value.clamp(min, max).floor() as i32
}

fn round_to_i32_safe(value: f32) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    value.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

fn center_axis_component_to_cell(value: f32, fallback: i32) -> i32 {
    if !value.is_finite() {
        return fallback;
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

fn set_vec_component_by_axis(v: &mut Vec3, axis: IVec3, value: f32) {
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

fn draw_preview_cell_count(draw: &RectDrawState) -> usize {
    if draw.pencil_line {
        pencil_line_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP).len()
    } else if draw.action == RectDrawAction::Fill
        && draw.shape_workflow != SketchShapeWorkflow::Rectangle
    {
        sketch_shape_cells(
            draw.shape_workflow,
            draw.start,
            draw.current,
            draw.normal,
            DRAW_CELL_CAP,
        )
        .len()
    } else {
        rect_cell_count(draw.start, draw.current, draw.normal)
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

fn pencil_line_cells(a: IVec3, b: IVec3, normal: IVec3, cap: usize) -> Vec<IVec3> {
    let Some((axis_u, axis_v)) = plane_axes(normal) else {
        return Vec::new();
    };
    if normal_axis(normal)
        .is_some_and(|axis| component_by_index(a, axis) != component_by_index(b, axis))
    {
        return pencil_line_cells_3d(a, b, cap);
    }
    if cap == 0 {
        return Vec::new();
    }

    let au = component_by_axis(a, axis_u);
    let av = component_by_axis(a, axis_v);
    let bu = component_by_axis(b, axis_u);
    let bv = component_by_axis(b, axis_v);
    let mut u = au;
    let mut v = av;
    let du = (bu - au).abs();
    let dv = -(bv - av).abs();
    let su = (bu - au).signum();
    let sv = (bv - av).signum();
    let mut err = du + dv;
    let expected = (du.max(-dv) as usize + 1).min(cap);
    let mut out = Vec::with_capacity(expected);

    loop {
        let mut p = a;
        set_component_by_axis(&mut p, axis_u, u);
        set_component_by_axis(&mut p, axis_v, v);
        out.push(p);
        if out.len() >= cap || (u == bu && v == bv) {
            break;
        }
        let e2 = err * 2;
        if e2 >= dv {
            err += dv;
            u += su;
        }
        if e2 <= du {
            err += du;
            v += sv;
        }
    }

    out
}

fn pencil_line_cells_3d(a: IVec3, b: IVec3, cap: usize) -> Vec<IVec3> {
    if cap == 0 {
        return Vec::new();
    }
    let delta = b - a;
    let steps = delta.x.abs().max(delta.y.abs()).max(delta.z.abs());
    if steps == 0 {
        return vec![a];
    }
    let mut out = Vec::with_capacity((steps as usize + 1).min(cap));
    for index in 0..=steps {
        let t = index as f32 / steps as f32;
        let point = Vec3::new(
            a.x as f32 + delta.x as f32 * t,
            a.y as f32 + delta.y as f32 * t,
            a.z as f32 + delta.z as f32 * t,
        );
        let cell = IVec3::new(
            round_to_i32_safe(point.x),
            round_to_i32_safe(point.y),
            round_to_i32_safe(point.z),
        );
        if out.last().copied() != Some(cell) {
            out.push(cell);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

fn sketch_shape_cells(
    shape: SketchShapeWorkflow,
    a: IVec3,
    b: IVec3,
    normal: IVec3,
    cap: usize,
) -> Vec<IVec3> {
    match shape {
        SketchShapeWorkflow::Rectangle => rect_cells(a, b, normal, cap),
        SketchShapeWorkflow::Circle => circle_disc_cells(a, b, normal, cap),
        SketchShapeWorkflow::Polygon => hex_polygon_cells(a, b, normal, cap),
        SketchShapeWorkflow::Arc => arc_trace_cells(a, b, normal, cap),
        SketchShapeWorkflow::Freehand => pencil_line_cells(a, b, normal, cap),
    }
}

fn sketch_shape_radius(a: IVec3, b: IVec3, axis_u: IVec3, axis_v: IVec3) -> i32 {
    let du = (component_by_axis(b, axis_u) - component_by_axis(a, axis_u)).abs();
    let dv = (component_by_axis(b, axis_v) - component_by_axis(a, axis_v)).abs();
    du.max(dv).max(1)
}

fn capped_shape_radius(a: IVec3, b: IVec3, axis_u: IVec3, axis_v: IVec3, cap: usize) -> i32 {
    let preview_limit = (cap as f32).sqrt().ceil() as i32 + 1;
    sketch_shape_radius(a, b, axis_u, axis_v)
        .min(preview_limit.max(1))
        .min(96)
}

fn circle_disc_cells(center: IVec3, edge: IVec3, normal: IVec3, cap: usize) -> Vec<IVec3> {
    let Some((axis_u, axis_v)) = plane_axes(normal) else {
        return Vec::new();
    };
    if cap == 0 {
        return Vec::new();
    }
    let radius = capped_shape_radius(center, edge, axis_u, axis_v, cap);
    let r2 = radius * radius;
    let approx = (radius as usize)
        .saturating_mul(radius as usize)
        .saturating_mul(4)
        .max(1)
        .min(cap);
    let mut out = Vec::with_capacity(approx);
    for u in -radius..=radius {
        for v in -radius..=radius {
            if u * u + v * v > r2 {
                continue;
            }
            let mut p = center;
            set_component_by_axis(&mut p, axis_u, component_by_axis(center, axis_u) + u);
            set_component_by_axis(&mut p, axis_v, component_by_axis(center, axis_v) + v);
            out.push(p);
            if out.len() >= cap {
                return out;
            }
        }
    }
    out
}

fn hex_polygon_cells(center: IVec3, edge: IVec3, normal: IVec3, cap: usize) -> Vec<IVec3> {
    let Some((axis_u, axis_v)) = plane_axes(normal) else {
        return Vec::new();
    };
    if cap == 0 {
        return Vec::new();
    }
    let radius = capped_shape_radius(center, edge, axis_u, axis_v, cap);
    let approx = (radius as usize)
        .saturating_mul(radius as usize)
        .saturating_mul(3)
        .max(1)
        .min(cap);
    let mut out = Vec::with_capacity(approx);
    for u in -radius..=radius {
        for v in -radius..=radius {
            if u.abs() > radius || v.abs() > radius || (u + v).abs() > radius {
                continue;
            }
            let mut p = center;
            set_component_by_axis(&mut p, axis_u, component_by_axis(center, axis_u) + u);
            set_component_by_axis(&mut p, axis_v, component_by_axis(center, axis_v) + v);
            out.push(p);
            if out.len() >= cap {
                return out;
            }
        }
    }
    out
}

fn arc_trace_cells(center: IVec3, edge: IVec3, normal: IVec3, cap: usize) -> Vec<IVec3> {
    let Some((axis_u, axis_v)) = plane_axes(normal) else {
        return Vec::new();
    };
    if cap == 0 {
        return Vec::new();
    }
    let radius = capped_shape_radius(center, edge, axis_u, axis_v, cap) as f32;
    let samples = ((radius as usize) * 4).clamp(8, 48);
    let cu = component_by_axis(center, axis_u);
    let cv = component_by_axis(center, axis_v);
    let mut out = Vec::with_capacity(samples.min(cap));
    for idx in 0..=samples {
        let t = idx as f32 / samples as f32;
        let angle = t * std::f32::consts::FRAC_PI_2;
        let u = cu + round_to_i32_safe(angle.cos() * radius);
        let v = cv + round_to_i32_safe(angle.sin() * radius);
        let mut p = center;
        set_component_by_axis(&mut p, axis_u, u);
        set_component_by_axis(&mut p, axis_v, v);
        if !out.contains(&p) {
            out.push(p);
            if out.len() >= cap {
                break;
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

fn rect_snap_marker_color(kind: Option<RectFaceSnapKind>) -> Color {
    match kind {
        Some(RectFaceSnapKind::Endpoint) => Color::srgb(0.2, 1.0, 0.28),
        Some(RectFaceSnapKind::Midpoint) => Color::srgb(0.1, 0.9, 1.0),
        Some(RectFaceSnapKind::FaceCenter) => Color::srgb(0.22, 0.48, 1.0),
        None => Color::srgb(1.0, 0.85, 0.18),
    }
}

fn rect_snap_marker_radius(kind: Option<RectFaceSnapKind>, current: bool) -> f32 {
    let base = match kind {
        Some(RectFaceSnapKind::Endpoint) => 0.16,
        Some(RectFaceSnapKind::Midpoint) => 0.13,
        Some(RectFaceSnapKind::FaceCenter) => 0.105,
        None => 0.085,
    };
    let focus = if current { 0.035 } else { 0.0 };
    base + focus
}

fn rect_snap_marker_halo_radius(kind: Option<RectFaceSnapKind>) -> f32 {
    match kind {
        Some(RectFaceSnapKind::Endpoint) => 0.48,
        Some(RectFaceSnapKind::Midpoint) => 0.42,
        Some(RectFaceSnapKind::FaceCenter) => 0.36,
        None => 0.30,
    }
}

fn rect_snap_marker_plane_basis(normal: IVec3) -> (Vec3, Vec3) {
    match normal_axis(normal).unwrap_or(1) {
        0 => (Vec3::Y, Vec3::Z),
        1 => (Vec3::X, Vec3::Z),
        _ => (Vec3::X, Vec3::Y),
    }
}

fn rect_snap_marker_normal_dir(normal: IVec3) -> Dir3 {
    match normal_axis(normal).unwrap_or(1) {
        0 => Dir3::X,
        1 => Dir3::Y,
        _ => Dir3::Z,
    }
}

fn draw_input_point_marker(
    gizmos: &mut Gizmos,
    point: Vec3,
    normal: IVec3,
    kind: Option<RectFaceSnapKind>,
    current: bool,
    color: Color,
) {
    let offset = normal.as_vec3() * 0.06;
    let center = point + offset;
    let marker_radius = rect_snap_marker_radius(kind, current);
    let halo_radius = rect_snap_marker_halo_radius(kind);

    gizmos.circle(
        center,
        rect_snap_marker_normal_dir(normal),
        halo_radius,
        color.with_alpha(if current { 0.55 } else { 0.35 }),
    );
    gizmos.cuboid(
        Transform::from_translation(center).with_scale(Vec3::splat(marker_radius * 1.35)),
        color.with_alpha(if current { 0.95 } else { 0.70 }),
    );
}

fn draw_rect_input_point_gizmos(draw: &RectDrawState, gizmos: &mut Gizmos) {
    draw_input_point_marker(
        gizmos,
        draw.start_point,
        draw.normal,
        draw.start_snap_kind,
        false,
        rect_snap_marker_color(draw.start_snap_kind),
    );
    draw_input_point_marker(
        gizmos,
        draw.current_point,
        draw.normal,
        draw.snap_kind,
        true,
        rect_snap_marker_color(draw.snap_kind),
    );
    gizmos.line(
        draw.start_point + draw.normal.as_vec3() * 0.12,
        draw.current_point + draw.normal.as_vec3() * 0.12,
        rect_snap_marker_color(draw.snap_kind).with_alpha(0.78),
    );
    if let Some(axis_lock) = draw.axis_lock {
        let axis = axis_lock.axis_vec3();
        let start = draw.start_point + draw.normal.as_vec3() * 0.10;
        let reach = 120.0;
        gizmos.line(
            start - axis * reach,
            start + axis * reach,
            axis_lock.color(),
        );
    }
}

fn rect_visual_acquisition(draw: &RectDrawState) -> Option<RectVisualAcquisition> {
    if !draw.active {
        return None;
    }
    if let Some(kind) = draw.snap_kind {
        return Some(RectVisualAcquisition::Snap(kind, draw.current));
    }
    if let Some(axis_lock) = draw.axis_lock {
        return Some(RectVisualAcquisition::Axis(axis_lock));
    }
    (draw.inference != RectEndpointInference::None)
        .then_some(RectVisualAcquisition::Inference(draw.inference))
}

fn begin_rect_visual_feedback(
    draw: &mut RectDrawState,
    kind: RectVisualFeedbackKind,
    point: Vec3,
    normal: IVec3,
    snap_kind: Option<RectFaceSnapKind>,
) {
    if kind == RectVisualFeedbackKind::Commit {
        draw.visual_acquisition = rect_visual_acquisition(draw);
    }
    draw.visual_feedback.begin(kind, point, normal, snap_kind);
}

fn tick_rect_visual_feedback(
    draw: &mut RectDrawState,
    delta_seconds: f32,
) -> Option<(RectVisualFeedback, f32)> {
    if draw.visual_feedback.kind != RectVisualFeedbackKind::None {
        draw.visual_feedback.remaining =
            (draw.visual_feedback.remaining - delta_seconds.max(0.0)).max(0.0);
        if draw.visual_feedback.remaining <= 0.0 {
            draw.visual_feedback.clear();
        }
    }

    let acquisition = rect_visual_acquisition(draw);
    if draw.visual_feedback.kind != RectVisualFeedbackKind::Commit
        && acquisition != draw.visual_acquisition
    {
        draw.visual_acquisition = acquisition;
        if acquisition.is_some() {
            let point = draw.current_point;
            let normal = draw.normal;
            let snap_kind = draw.snap_kind;
            begin_rect_visual_feedback(
                draw,
                RectVisualFeedbackKind::Acquisition,
                point,
                normal,
                snap_kind,
            );
        } else if draw.visual_feedback.kind == RectVisualFeedbackKind::Acquisition {
            draw.visual_feedback.clear();
        }
    }

    let feedback = draw.visual_feedback;
    if feedback.kind == RectVisualFeedbackKind::None || feedback.duration <= 0.0 {
        return None;
    }
    let progress = (1.0 - feedback.remaining / feedback.duration).clamp(0.0, 1.0);
    Some((feedback, progress))
}

fn rect_visual_feedback_radius(
    kind: RectVisualFeedbackKind,
    progress: f32,
    reduce_motion: bool,
) -> f32 {
    if reduce_motion {
        return match kind {
            RectVisualFeedbackKind::Acquisition => 0.54,
            RectVisualFeedbackKind::Commit => 0.68,
            RectVisualFeedbackKind::None => 0.0,
        };
    }
    let progress = progress.clamp(0.0, 1.0);
    match kind {
        RectVisualFeedbackKind::Acquisition => 0.28 + 0.34 * progress,
        RectVisualFeedbackKind::Commit => 0.34 + 0.62 * progress,
        RectVisualFeedbackKind::None => 0.0,
    }
}

fn draw_rect_visual_feedback(
    gizmos: &mut Gizmos,
    feedback: RectVisualFeedback,
    progress: f32,
    reduce_motion: bool,
) {
    let radius = rect_visual_feedback_radius(feedback.kind, progress, reduce_motion);
    if radius <= 0.0 {
        return;
    }
    let alpha = if reduce_motion {
        0.78
    } else {
        (1.0 - progress).clamp(0.0, 1.0) * 0.90
    };
    let color = match feedback.kind {
        RectVisualFeedbackKind::Acquisition => rect_snap_marker_color(feedback.snap_kind),
        RectVisualFeedbackKind::Commit => Color::srgb(1.0, 0.92, 0.24),
        RectVisualFeedbackKind::None => return,
    };
    let center = feedback.point + feedback.normal.as_vec3() * 0.08;
    let normal = rect_snap_marker_normal_dir(feedback.normal);
    gizmos.circle(center, normal, radius, color.with_alpha(alpha));
    if feedback.kind == RectVisualFeedbackKind::Commit {
        gizmos.circle(
            center,
            normal,
            radius * 0.68,
            color.with_alpha(alpha * 0.72),
        );
    }
}

pub fn draw_rect_gizmo(
    mut draw: ResMut<RectDrawState>,
    mut gizmos: Gizmos,
    time: Res<Time>,
    settings: Option<Res<crate::settings::WorldSettings>>,
) {
    let reduce_motion = settings
        .as_deref()
        .is_some_and(|settings| settings.reduce_motion);
    if let Some((feedback, progress)) = tick_rect_visual_feedback(&mut draw, time.delta_seconds()) {
        draw_rect_visual_feedback(&mut gizmos, feedback, progress, reduce_motion);
    }
    if !draw.active {
        return;
    }
    let color = match draw.action {
        RectDrawAction::Fill => Color::srgb(0.32, 0.95, 1.0),
        RectDrawAction::Cut => Color::srgb(1.0, 0.32, 0.05),
    };
    draw_rect_input_point_gizmos(&draw, &mut gizmos);
    if draw.pencil_line || draw.shape_workflow != SketchShapeWorkflow::Rectangle {
        let cells = if draw.pencil_line {
            pencil_line_cells(draw.start, draw.current, draw.normal, 768)
        } else {
            sketch_shape_cells(
                draw.shape_workflow,
                draw.start,
                draw.current,
                draw.normal,
                768,
            )
        };
        for cell in cells {
            let center = cell.as_vec3() + Vec3::splat(0.5);
            gizmos.cuboid(
                Transform::from_translation(center).with_scale(Vec3::splat(1.04)),
                color,
            );
        }
        return;
    }
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

pub fn refresh_editor_pointer_marker(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    toolbelt: Res<ToolbeltState>,
    draw: Res<RectDrawState>,
    world: Res<VoxelWorld>,
    mut marker: ResMut<SketchEditorPointerMarker>,
    mut view_q: ParamSet<(
        Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
        Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
    )>,
) {
    if !draw_rect_active(&mode, &keys, &draw) {
        marker.clear();
        return;
    }
    if sync_pointer_marker_from_active_draw(&mut marker, &draw) {
        return;
    }

    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    let (cursor_locked, cursor_visible, cursor_position) = {
        let window_q = view_q.p0();
        let window = window_q.get_single().ok();
        (
            window.map(crate::mode::cursor_is_captured).unwrap_or(false),
            window.map(|window| window.cursor.visible).unwrap_or(false),
            window.and_then(|window| window.cursor_position()),
        )
    };
    let cam_q = view_q.p1();
    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        marker.clear();
        return;
    };
    let Some((origin, dir)) = draw_input_ray(
        active_tool,
        cursor_locked,
        cursor_visible,
        cursor_position,
        camera,
        cam_tf,
    ) else {
        marker.clear();
        return;
    };

    sync_pointer_marker(
        &mut marker,
        &draw,
        &world,
        active_tool,
        toolbelt.pencil_workflow_active(),
        origin,
        dir,
    );
}

pub fn draw_editor_pointer_marker(marker: Res<SketchEditorPointerMarker>, mut gizmos: Gizmos) {
    if !marker.active {
        return;
    }
    let base_color = rect_snap_marker_color(marker.snap_kind);
    let color = if marker.drawing {
        base_color
    } else {
        Color::srgb(1.0, 0.86, 0.20)
    };
    draw_input_point_marker(
        &mut gizmos,
        marker.point,
        marker.normal,
        marker.snap_kind,
        true,
        color,
    );

    let normal = marker.normal.as_vec3();
    let (u, v) = rect_snap_marker_plane_basis(marker.normal);
    let center = marker.point + normal * 0.14;
    let cross_radius = if marker.drawing { 0.42 } else { 0.34 };
    gizmos.line(
        center - u * cross_radius,
        center + u * cross_radius,
        color.with_alpha(0.92),
    );
    gizmos.line(
        center - v * cross_radius,
        center + v * cross_radius,
        color.with_alpha(0.92),
    );
    gizmos.line(center, center + normal * 0.82, color.with_alpha(0.72));
}

pub fn refresh_editor_screen_cursor(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    toolbelt: Res<ToolbeltState>,
    draw: Res<RectDrawState>,
    marker: Res<SketchEditorPointerMarker>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    mut screen_cursor: ResMut<SketchEditorScreenCursor>,
    window_q: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    camera_q: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
) {
    if !draw_rect_active(&mode, &keys, &draw) {
        screen_cursor.clear();
        return;
    }

    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    if !matches!(active_tool, ToolbeltTool::DrawRect | ToolbeltTool::Sculpt) {
        screen_cursor.clear();
        return;
    }

    let Some(window) = window_q.get_single().ok() else {
        screen_cursor.clear();
        return;
    };
    let Some(cursor) =
        visible_screen_cursor_position(window.cursor.visible, window.cursor_position())
    else {
        screen_cursor.clear();
        return;
    };

    let target = camera_q
        .get_single()
        .ok()
        .and_then(|(camera, cam_tf)| marker_screen_target(&marker, camera, cam_tf));
    screen_cursor.set(
        cursor,
        target,
        marker.snap_kind,
        draw.active || marker.drawing,
        ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui),
    );
}

fn marker_screen_target(
    marker: &SketchEditorPointerMarker,
    camera: &Camera,
    cam_tf: &GlobalTransform,
) -> Option<Vec2> {
    if !marker.active {
        return None;
    }
    camera.world_to_viewport(cam_tf, marker.point)
}

pub fn draw_editor_screen_cursor(
    mut contexts: EguiContexts,
    screen_cursor: Res<SketchEditorScreenCursor>,
) {
    if !screen_cursor.active {
        return;
    }

    let ctx = contexts.ctx_mut();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("sketch_editor_screen_cursor"),
    ));
    let pos = egui::pos2(screen_cursor.cursor.x, screen_cursor.cursor.y);
    let color = screen_cursor_color(
        screen_cursor.snap_kind,
        screen_cursor.drawing,
        screen_cursor.over_ui,
    );
    let radius = if screen_cursor.drawing { 9.5 } else { 8.0 };
    let stroke = egui::Stroke::new(1.6, color);

    painter.circle_stroke(pos, radius, stroke);
    painter.line_segment(
        [pos + egui::vec2(-12.0, 0.0), pos + egui::vec2(-4.0, 0.0)],
        stroke,
    );
    painter.line_segment(
        [pos + egui::vec2(4.0, 0.0), pos + egui::vec2(12.0, 0.0)],
        stroke,
    );
    painter.line_segment(
        [pos + egui::vec2(0.0, -12.0), pos + egui::vec2(0.0, -4.0)],
        stroke,
    );
    painter.line_segment(
        [pos + egui::vec2(0.0, 4.0), pos + egui::vec2(0.0, 12.0)],
        stroke,
    );

    if let Some(target) = screen_cursor.target {
        let target_pos = egui::pos2(target.x, target.y);
        if pos.distance(target_pos) > 8.0 {
            painter.line_segment(
                [pos, target_pos],
                egui::Stroke::new(1.0, color.gamma_multiply(0.45)),
            );
            painter.circle_stroke(target_pos, 6.5, egui::Stroke::new(1.2, color));
        }
    }

    let label = screen_cursor_label(screen_cursor.snap_kind, screen_cursor.drawing);
    let label_pos = pos + egui::vec2(16.0, 16.0);
    let label_rect =
        egui::Rect::from_min_size(label_pos + egui::vec2(-8.0, -9.0), egui::vec2(98.0, 21.0));
    painter.rect_filled(
        label_rect,
        5.0,
        egui::Color32::from_rgba_unmultiplied(4, 18, 28, 210),
    );
    painter.rect_stroke(label_rect, 5.0, egui::Stroke::new(1.0, color));
    painter.text(
        label_pos,
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(12.0),
        color,
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
    fn pencil_line_cells_draws_single_voxel_line_on_floor_plane() {
        let cells = pencil_line_cells(
            IVec3::new(0, 10, 0),
            IVec3::new(4, 10, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );

        assert_eq!(
            cells,
            vec![
                IVec3::new(0, 10, 0),
                IVec3::new(1, 10, 0),
                IVec3::new(2, 10, 0),
                IVec3::new(3, 10, 0),
                IVec3::new(4, 10, 0),
            ]
        );
    }

    #[test]
    fn pencil_line_cells_stays_on_locked_wall_plane() {
        let cells = pencil_line_cells(
            IVec3::new(3, 4, 8),
            IVec3::new(3, 7, 12),
            IVec3::X,
            DRAW_CELL_CAP,
        );

        assert_eq!(cells.first().copied(), Some(IVec3::new(3, 4, 8)));
        assert_eq!(cells.last().copied(), Some(IVec3::new(3, 7, 12)));
        assert!(cells.iter().all(|p| p.x == 3));
        assert!(cells.len() >= 5);
    }

    #[test]
    fn circle_cells_create_disc_on_locked_floor_plane() {
        let center = IVec3::new(0, 10, 0);
        let cells = sketch_shape_cells(
            SketchShapeWorkflow::Circle,
            center,
            IVec3::new(3, 10, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );

        assert!(cells.contains(&center));
        assert!(cells.contains(&IVec3::new(3, 10, 0)));
        assert!(!cells.contains(&IVec3::new(4, 10, 0)));
        assert!(cells.iter().all(|p| p.y == 10));
        assert!(cells.len() > 20);
    }

    #[test]
    fn polygon_cells_create_hex_footprint_on_locked_floor_plane() {
        let center = IVec3::new(0, 10, 0);
        let cells = sketch_shape_cells(
            SketchShapeWorkflow::Polygon,
            center,
            IVec3::new(4, 10, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );

        assert!(cells.contains(&center));
        assert!(cells.iter().all(|p| p.y == 10));
        assert!(cells.len() > 24);
        assert!(
            cells.len() < 64,
            "hex footprint should not become a filled square"
        );
    }

    #[test]
    fn arc_and_freehand_cells_stay_on_locked_plane_without_filling_area() {
        let center = IVec3::new(0, 10, 0);
        let arc = sketch_shape_cells(
            SketchShapeWorkflow::Arc,
            center,
            IVec3::new(4, 10, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );
        let freehand = sketch_shape_cells(
            SketchShapeWorkflow::Freehand,
            center,
            IVec3::new(4, 10, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );

        assert!(arc.iter().all(|p| p.y == 10));
        assert!(freehand.iter().all(|p| p.y == 10));
        assert!(arc.len() > 4);
        assert_eq!(
            freehand,
            pencil_line_cells(center, IVec3::new(4, 10, 0), IVec3::Y, DRAW_CELL_CAP)
        );
        assert!(arc.len() < 20, "arc should trace a curve, not fill a face");
    }

    #[test]
    fn sketchup_style_pencil_waits_for_second_click_after_start() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = sketch_tool_uses_click_finish(ToolbeltTool::DrawRect, false);
        draw.pointer_valid = true;

        assert!(!rect_should_commit_on_release(&draw));

        let second_click = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(rect_should_commit_on_start_intent(second_click, &draw));
    }

    #[test]
    fn opening_workflow_second_click_commits_cut_not_only_fill() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pointer_valid = true;

        let second_click = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            true,
            false,
        );

        assert!(second_click.cut);
        assert!(rect_should_commit_on_start_intent(second_click, &draw));
    }

    #[test]
    fn right_mouse_orbit_never_finishes_active_sketch_preview() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;

        let orbit = rect_start_intent(
            ToolbeltTool::DrawRect,
            false,
            true,
            false,
            false,
            false,
            false,
        );

        assert!(!rect_should_commit_on_start_intent(orbit, &draw));
        assert!(!orbit.cut);
    }

    #[test]
    fn stale_preview_cannot_commit_after_pointer_loss() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pointer_valid = false;
        draw.current = IVec3::new(40, 12, -7);

        let second_click = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            false,
            false,
        );

        assert!(!rect_should_commit_on_start_intent(second_click, &draw));
        assert_eq!(
            rect_pointer_loss_disposition(&draw, ToolbeltTool::DrawRect, false, false, false,),
            RectPointerLossDisposition::Cancel
        );
        clear_rect_preview(&mut draw);
        assert!(!draw.active);
        assert!(!draw.pointer_valid);
    }

    #[test]
    fn captured_rmb_orbit_suspends_instead_of_committing_or_cancelling() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.pointer_valid = true;

        assert_eq!(
            rect_pointer_loss_disposition(&draw, ToolbeltTool::DrawRect, false, true, true,),
            RectPointerLossDisposition::SuspendForOrbit
        );
        assert_eq!(
            rect_pointer_loss_disposition(&draw, ToolbeltTool::DrawRect, true, true, false,),
            RectPointerLossDisposition::None
        );
    }

    #[test]
    fn active_sketch_preview_cancels_when_toolbox_selection_changes() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.tool_generation = 4;

        assert!(rect_should_cancel_for_tool_selection(&draw, 5));
        assert!(!rect_should_cancel_for_tool_selection(&draw, 4));
    }

    #[test]
    fn smart_brush_gestures_keep_hold_release_commit() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = sketch_tool_uses_click_finish(ToolbeltTool::BrushPlace, true);

        assert!(rect_should_commit_on_release(&draw));
    }

    #[test]
    fn committed_pencil_line_chains_from_last_endpoint_like_sketchup_line_tool() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = true;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 0);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Limestone);
        draw.start_point = pencil_cell_marker_point(draw.start);
        draw.current_point = pencil_cell_marker_point(draw.current);

        let mut world = VoxelWorld::new();
        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert!(draw.active, "pencil should remain armed for the next edge");
        assert!(draw.click_finish);
        assert_eq!(draw.start, IVec3::new(3, 4, 0));
        assert_eq!(draw.current, IVec3::new(3, 4, 0));
        assert!(toolbelt.status.contains("Next endpoint"));
        assert!(tool_controller
            .last_transaction_label()
            .is_some_and(|label| label.starts_with("Pencil line")));
        let semantic_edge = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic pencil edge");
        assert!(matches!(
            &sketch_doc.entity(semantic_edge).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(0.5, 4.5, 0.5) && *b == Vec3::new(3.5, 4.5, 0.5)
        ));
        assert!(sketch_links
            .links_for_face(IVec3::new(0, 4, 0), IVec3::Y)
            .iter()
            .any(|link| {
                link.entity == semantic_edge
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Stroke
            }));
    }

    #[test]
    fn committed_pencil_line_chains_from_exact_snap_point_not_cell_center() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = true;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 0);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Limestone);
        draw.start_point = Vec3::new(0.0, 4.5, 0.5);
        let exact_endpoint = Vec3::new(3.0, 4.5, 0.5);
        draw.current_point = exact_endpoint;

        let mut world = VoxelWorld::new();
        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert!(draw.active, "pencil should remain armed for the next edge");
        assert_eq!(draw.visual_feedback.kind, RectVisualFeedbackKind::Commit);
        assert_eq!(draw.visual_feedback.point, exact_endpoint);
        assert_eq!(draw.start, IVec3::new(3, 4, 0));
        assert_eq!(
            draw.start_point, exact_endpoint,
            "SketchUp-style chained pencil lines must continue from the visible snap point, not the committed voxel cell center"
        );
        assert_eq!(draw.current_point, exact_endpoint);
        let semantic_edge = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic pencil edge");
        assert!(matches!(
            &sketch_doc.entity(semantic_edge).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(0.0, 4.5, 0.5) && *b == exact_endpoint
        ));
    }

    #[test]
    fn pencil_connection_records_semantic_edge_even_when_voxels_already_exist() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = true;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 0);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Limestone);
        draw.start_point = pencil_cell_marker_point(draw.start);
        draw.current_point = pencil_cell_marker_point(draw.current);

        let mut world = VoxelWorld::new();
        for pos in pencil_line_cells(draw.start, draw.current, draw.normal, DRAW_CELL_CAP) {
            assert!(world.edit_set_voxel(pos.x, pos.y, pos.z, draw.voxel));
        }
        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert_eq!(
            history.undo_len(),
            1,
            "existing-voxel connection still needs a semantic undo step for the selectable line"
        );
        assert!(draw.active, "pencil remains armed after connecting");
        assert_eq!(draw.start, IVec3::new(3, 4, 0));
        assert!(toolbelt.status.contains("connected existing cells"));
        assert!(tool_controller
            .last_transaction_label()
            .is_some_and(|label| label.starts_with("Pencil connection")));
        let semantic_edge = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic connection edge");
        assert!(matches!(
            &sketch_doc.entity(semantic_edge).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::new(0.5, 4.5, 0.5) && *b == Vec3::new(3.5, 4.5, 0.5)
        ));
        assert!(sketch_links
            .links_for_face(IVec3::new(1, 4, 0), IVec3::Y)
            .iter()
            .any(|link| link.entity == semantic_edge));

        let undo_step = history
            .pop_undo_detailed(&mut world)
            .expect("semantic-only pencil undo step");
        assert_eq!(
            undo_step.voxel_count, 0,
            "semantic-only undo must not rewrite already-matching voxels"
        );
        undo_step
            .apply_sketch_undo(&mut sketch_doc, &mut sketch_links)
            .expect("semantic pencil undo");
        assert!(sketch_doc.entity(semantic_edge).is_none());
        assert!(
            !sketch_links
                .links_for_face(IVec3::new(1, 4, 0), IVec3::Y)
                .iter()
                .any(|link| link.entity == semantic_edge),
            "undo must remove stale selectable links"
        );
        assert_eq!(
            world.voxel_at(
                IVec3::new(1, 4, 0).x,
                IVec3::new(1, 4, 0).y,
                IVec3::new(1, 4, 0).z
            ),
            draw.voxel,
            "semantic-only undo must leave the existing blocks in place"
        );

        let redo_step = history
            .pop_redo_detailed(&mut world)
            .expect("semantic-only pencil redo step");
        assert_eq!(redo_step.voxel_count, 0);
        redo_step
            .apply_sketch_redo(&mut sketch_doc, &mut sketch_links)
            .expect("semantic pencil redo");
        assert!(sketch_doc.entity(semantic_edge).is_some());
        assert!(sketch_links
            .links_for_face(IVec3::new(1, 4, 0), IVec3::Y)
            .iter()
            .any(|link| link.entity == semantic_edge));
    }

    #[test]
    fn rectangle_records_selectable_face_even_when_voxels_already_match() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = false;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 2);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Stone);
        draw.start_point = pencil_cell_marker_point(draw.start);
        draw.current_point = pencil_cell_marker_point(draw.current);

        let mut world = VoxelWorld::new();
        for pos in sketch_shape_cells(
            SketchShapeWorkflow::Rectangle,
            draw.start,
            draw.current,
            draw.normal,
            DRAW_CELL_CAP,
        ) {
            assert!(world.edit_set_voxel(pos.x, pos.y, pos.z, draw.voxel));
        }
        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert_eq!(
            history.undo_len(),
            1,
            "no-op voxel writes still need a semantic undo step for the selectable face"
        );
        let semantic_face = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic rectangle face");
        assert!(sketch_links
            .links_for_face(IVec3::new(2, 4, 1), IVec3::Y)
            .iter()
            .any(|link| {
                link.entity == semantic_face
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Face
            }));
        assert!(toolbelt.status.contains("selectable"));

        let undo_step = history
            .pop_undo_detailed(&mut world)
            .expect("semantic-only rectangle undo step");
        assert_eq!(undo_step.voxel_count, 0);
        undo_step
            .apply_sketch_undo(&mut sketch_doc, &mut sketch_links)
            .expect("semantic rectangle undo");
        assert!(sketch_doc.entity(semantic_face).is_none());
        assert!(!sketch_links
            .links_for_face(IVec3::new(2, 4, 1), IVec3::Y)
            .iter()
            .any(|link| link.entity == semantic_face));
        assert_eq!(world.voxel_at(2, 4, 1), draw.voxel);

        let redo_step = history
            .pop_redo_detailed(&mut world)
            .expect("semantic-only rectangle redo step");
        assert_eq!(redo_step.voxel_count, 0);
        redo_step
            .apply_sketch_redo(&mut sketch_doc, &mut sketch_links)
            .expect("semantic rectangle redo");
        assert!(sketch_doc.entity(semantic_face).is_some());
        assert!(sketch_links
            .links_for_face(IVec3::new(2, 4, 1), IVec3::Y)
            .iter()
            .any(|link| link.entity == semantic_face));
    }

    #[test]
    fn rectangle_links_prefilled_and_new_cells_to_one_semantic_face() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = false;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 2);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Stone);
        draw.start_point = pencil_cell_marker_point(draw.start);
        draw.current_point = pencil_cell_marker_point(draw.current);

        let mut world = VoxelWorld::new();
        let prefilled = IVec3::new(2, 4, 1);
        assert!(world.edit_set_voxel(prefilled.x, prefilled.y, prefilled.z, draw.voxel));
        let new_cell = IVec3::new(0, 4, 0);
        assert_eq!(world.voxel_at(new_cell.x, new_cell.y, new_cell.z), AIR);

        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert_eq!(
            history.undo_len(),
            1,
            "partial-overlap rectangle should still create one voxel undo batch for the newly written cells"
        );
        let semantic_face = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic rectangle face");
        for cell in [prefilled, new_cell] {
            assert!(
                sketch_links
                    .links_for_face(cell, IVec3::Y)
                    .iter()
                    .any(|link| {
                        link.entity == semantic_face
                            && link.role == crate::sketch_model::SketchVoxelLinkRole::Face
                    }),
                "both already-existing and newly-written cells need semantic face links for stable select/move"
            );
        }

        let undo_step = history
            .pop_undo_detailed(&mut world)
            .expect("partial rectangle undo step");
        assert!(
            undo_step.voxel_count > 0,
            "partial-overlap undo should rewind newly-written cells"
        );
        undo_step
            .apply_sketch_undo(&mut sketch_doc, &mut sketch_links)
            .expect("partial rectangle semantic undo");
        assert_eq!(
            world.voxel_at(new_cell.x, new_cell.y, new_cell.z),
            AIR,
            "undo should remove newly-written cells"
        );
        assert_eq!(
            world.voxel_at(prefilled.x, prefilled.y, prefilled.z),
            draw.voxel,
            "undo must keep pre-existing cells"
        );
        assert!(sketch_doc.entity(semantic_face).is_none());
        for cell in [prefilled, new_cell] {
            assert!(
                !sketch_links
                    .links_for_face(cell, IVec3::Y)
                    .iter()
                    .any(|link| link.entity == semantic_face),
                "undo must remove all stale links for the semantic face"
            );
        }

        let redo_step = history
            .pop_redo_detailed(&mut world)
            .expect("partial rectangle redo step");
        assert!(redo_step.voxel_count > 0);
        redo_step
            .apply_sketch_redo(&mut sketch_doc, &mut sketch_links)
            .expect("partial rectangle semantic redo");
        assert_eq!(
            world.voxel_at(new_cell.x, new_cell.y, new_cell.z),
            draw.voxel
        );
        assert!(sketch_doc.entity(semantic_face).is_some());
        for cell in [prefilled, new_cell] {
            assert!(sketch_links
                .links_for_face(cell, IVec3::Y)
                .iter()
                .any(|link| link.entity == semantic_face));
        }
    }

    #[test]
    fn committed_rectangle_finishes_operation_instead_of_chaining() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.click_finish = true;
        draw.pencil_line = false;
        draw.action = RectDrawAction::Fill;
        draw.start = IVec3::new(0, 4, 0);
        draw.current = IVec3::new(3, 4, 2);
        draw.normal = IVec3::Y;
        draw.axis_u = IVec3::X;
        draw.axis_v = IVec3::Z;
        draw.voxel = Voxel::from(BlockType::Stone);

        let mut world = VoxelWorld::new();
        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut draw,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        assert!(!draw.active);
        assert!(!draw.click_finish);
        assert!(tool_controller
            .last_transaction_label()
            .is_some_and(|label| label.starts_with("Smart endpoint build")));
        let semantic_face = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .last()
            .copied()
            .expect("semantic rectangle face");
        assert!(matches!(
            &sketch_doc.entity(semantic_face).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Face { vertices, normal }
                if vertices == &vec![
                    Vec3::new(0.0, 4.0, 0.0),
                    Vec3::new(3.0, 4.0, 0.0),
                    Vec3::new(3.0, 4.0, 2.0),
                    Vec3::new(0.0, 4.0, 2.0),
                ] && *normal == Vec3::NEG_Y
        ));
        assert!(sketch_links
            .links_for_face(IVec3::new(0, 4, 0), IVec3::Y)
            .iter()
            .any(|link| {
                link.entity == semantic_face
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Face
            }));
    }

    #[test]
    fn committed_opening_and_room_write_semantic_house_entities() {
        let mut world = VoxelWorld::new();
        for x in 0..=4 {
            for y in 0..=3 {
                world.edit_set_voxel(x, y, 0, Voxel::from(BlockType::Stone));
            }
        }
        let mut opening = RectDrawState::default();
        opening.active = true;
        opening.click_finish = true;
        opening.action = RectDrawAction::Cut;
        opening.start = IVec3::new(1, 1, 0);
        opening.current = IVec3::new(2, 2, 0);
        opening.normal = IVec3::Z;
        opening.axis_u = IVec3::X;
        opening.axis_v = IVec3::Y;
        opening.voxel = AIR;

        let mut history = BuilderHistory::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();

        commit_rect_fill(
            &mut opening,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        let ids = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .clone();
        assert!(ids.iter().any(|id| matches!(
            &sketch_doc.entity(*id).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Opening { through_depth, .. }
                if (*through_depth - RECT_CUT_DEPTH_CAP as f32).abs() < f32::EPSILON
        )));
        let opening_id = ids
            .iter()
            .copied()
            .find(|id| {
                matches!(
                    &sketch_doc.entity(*id).unwrap().kind,
                    crate::sketch_model::SketchEntityKind::Opening { .. }
                )
            })
            .expect("semantic opening id");
        assert!(sketch_links
            .links_for_cell(IVec3::new(1, 1, 0))
            .iter()
            .any(|link| {
                link.entity == opening_id
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Opening
            }));

        let mut room = RectDrawState::default();
        room.active = true;
        room.click_finish = true;
        room.action = RectDrawAction::Cut;
        room.room_cut = true;
        room.start = IVec3::new(0, 0, 0);
        room.current = IVec3::new(4, 3, 0);
        room.normal = IVec3::Z;
        room.axis_u = IVec3::X;
        room.axis_v = IVec3::Y;
        room.voxel = AIR;

        commit_rect_fill(
            &mut room,
            &mut world,
            &mut history,
            &mut toolbelt,
            &mut tool_controller,
            &mut sketch_doc,
            &mut sketch_links,
        );

        let ids = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .clone();
        assert!(ids.iter().any(|id| matches!(
            &sketch_doc.entity(*id).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Room { wall_thickness, .. }
                if (*wall_thickness - 1.0).abs() < f32::EPSILON
        )));
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
        let intent = rect_start_intent(
            ToolbeltTool::DrawRect,
            false,
            true,
            false,
            false,
            false,
            false,
        );

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
    fn sketch_draw_ignores_world_clicks_over_editor_toolbox() {
        assert!(rect_should_ignore_world_click_for_editor_ui(
            true, true, false
        ));
        assert!(rect_should_ignore_world_click_for_editor_ui(
            true, false, true
        ));
        assert!(!rect_should_ignore_world_click_for_editor_ui(
            false, true, false
        ));
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
        let cut = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            true,
            false,
            false,
            false,
        );
        assert!(cut.cut);
        assert!(!cut.room_cut);
        assert_eq!(cut.button, RectDragButton::Left);

        let room = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            true,
            false,
            false,
        );
        assert!(room.cut);
        assert!(room.room_cut);
        assert_eq!(room.button, RectDragButton::Left);
    }

    #[test]
    fn plain_sketch_left_mouse_draws_on_vertical_wall_faces() {
        let intent = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            false,
            false,
        );

        assert_eq!(
            rect_action_for_start_intent(intent, ToolbeltTool::DrawRect, IVec3::X),
            RectDrawAction::Fill
        );
    }

    #[test]
    fn plain_sketch_left_mouse_still_builds_on_floor_faces() {
        let intent = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            false,
            false,
        );

        assert_eq!(
            rect_action_for_start_intent(intent, ToolbeltTool::DrawRect, IVec3::Y),
            RectDrawAction::Fill
        );
    }

    #[test]
    fn room_workflow_left_mouse_hollows_without_modifier() {
        let room = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            false,
            true,
        );

        assert!(room.cut);
        assert!(room.room_cut);
        assert_eq!(room.button, RectDragButton::Left);
    }

    #[test]
    fn opening_workflow_left_mouse_cuts_without_modifier() {
        let opening = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            false,
            false,
            true,
            false,
        );

        assert!(opening.cut);
        assert!(!opening.fill);
        assert!(!opening.room_cut);
        assert_eq!(opening.button, RectDragButton::Left);
    }

    #[test]
    fn ctrl_left_mouse_cuts_openings_even_inside_room_workflow() {
        let cut = rect_start_intent(
            ToolbeltTool::DrawRect,
            true,
            false,
            true,
            false,
            false,
            true,
        );

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
    fn rect_start_stays_inside_hovered_face_cell_from_ray_hit() {
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
            IVec3::new(10, 1, 14),
            "surface clicks should not jump into the next voxel when the cursor is still over this face"
        );
    }

    #[test]
    fn rect_endpoint_stays_inside_hovered_face_cell_from_ray_hit() {
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

        assert_eq!(snapped, IVec3::new(10, 1, 14));
    }

    #[test]
    fn rect_face_hit_classifies_endpoint_midpoint_and_face_center_targets() {
        let endpoint = classify_rect_face_snap(
            Vec3::new(10.03, 1.0, 14.04),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        )
        .expect("endpoint snap");
        assert_eq!(endpoint, RectFaceSnapKind::Endpoint);

        let midpoint = classify_rect_face_snap(
            Vec3::new(10.50, 1.0, 14.03),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        )
        .expect("midpoint snap");
        assert_eq!(midpoint, RectFaceSnapKind::Midpoint);

        let center = classify_rect_face_snap(
            Vec3::new(10.50, 1.0, 14.50),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        )
        .expect("face center snap");
        assert_eq!(center, RectFaceSnapKind::FaceCenter);
    }

    #[test]
    fn rect_face_hit_does_not_report_snap_when_between_reference_points() {
        let snap = classify_rect_face_snap(
            Vec3::new(10.24, 1.0, 14.31),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        );

        assert_eq!(
            snap, None,
            "snap labels should appear only near an endpoint, midpoint, or face center"
        );
    }

    #[test]
    fn rect_status_suffix_reports_snap_target_and_inference() {
        assert_eq!(
            rect_status_suffix(
                Some(RectFaceSnapKind::Endpoint),
                RectEndpointInference::None,
                None
            ),
            " Endpoint snap."
        );
        assert_eq!(
            rect_status_suffix(
                Some(RectFaceSnapKind::Midpoint),
                RectEndpointInference::Axis,
                None
            ),
            " Midpoint snap. Axis lock."
        );
        assert_eq!(
            rect_status_suffix(
                Some(RectFaceSnapKind::FaceCenter),
                RectEndpointInference::EqualLength,
                None
            ),
            " Face center snap. Equal-length snap."
        );
        assert_eq!(
            rect_status_suffix(
                Some(RectFaceSnapKind::Endpoint),
                RectEndpointInference::EqualLength,
                Some(RectAxisLock::Y)
            ),
            " Endpoint snap. Blue vertical height lock."
        );
    }

    #[test]
    fn rect_alignment_readout_names_reference_points_and_axis_lines() {
        assert_eq!(
            rect_alignment_readout(
                IVec3::new(4, 8, 2),
                IVec3::new(4, 13, 2),
                Some(RectFaceSnapKind::Endpoint),
                RectEndpointInference::Axis,
                Some(RectAxisLock::Y),
            ),
            "Endpoint | blue vertical height line 8 -> 13"
        );
        assert_eq!(
            rect_alignment_readout(
                IVec3::new(4, 8, 2),
                IVec3::new(9, 8, 2),
                Some(RectFaceSnapKind::Midpoint),
                RectEndpointInference::Axis,
                Some(RectAxisLock::X),
            ),
            "Midpoint | red X line 4 -> 9"
        );
        assert_eq!(
            rect_alignment_readout(
                IVec3::new(4, 8, 2),
                IVec3::new(9, 8, 7),
                Some(RectFaceSnapKind::FaceCenter),
                RectEndpointInference::EqualLength,
                None,
            ),
            "Face center | equal length"
        );
    }

    #[test]
    fn face_input_point_returns_exact_midpoint_marker_and_cell_snap() {
        let input = nearest_rect_face_input_point(
            Vec3::new(10.50, 1.0, 14.03),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        )
        .expect("midpoint input point");

        assert_eq!(input.kind, Some(RectFaceSnapKind::Midpoint));
        assert_eq!(input.point, Vec3::new(10.5, 1.0, 14.0));

        let cell = apply_face_input_point_to_cell(IVec3::new(10, 1, 14), input, IVec3::X, IVec3::Z);
        assert_eq!(
            cell,
            IVec3::new(10, 1, 14),
            "the discrete voxel endpoint stays in the hovered face cell instead of jumping to a neighbor"
        );
    }

    #[test]
    fn face_input_point_on_far_edge_does_not_jump_to_neighbor_cell() {
        let input = nearest_rect_face_input_point(
            Vec3::new(11.0, 1.0, 15.0),
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
        )
        .expect("endpoint input point");

        assert_eq!(input.kind, Some(RectFaceSnapKind::Endpoint));

        let cell = apply_face_input_point_to_cell(IVec3::new(10, 1, 14), input, IVec3::X, IVec3::Z);
        assert_eq!(
            cell,
            IVec3::new(10, 1, 14),
            "exact block-edge clicks must select the visible block cell, not x+1/z+1"
        );
    }

    #[test]
    fn pencil_side_face_anchor_uses_hit_cell_to_connect_existing_blocks() {
        let cell = pencil_anchor_cell_from_ray(
            IVec3::new(20, 10, 6),
            IVec3::new(21, 10, 6),
            IVec3::Y,
            IVec3::Z,
            Vec3::new(30.0, 10.25, 6.75),
            Vec3::NEG_X,
        );

        assert_eq!(
            cell,
            IVec3::new(20, 10, 6),
            "Pencil side-face clicks should reference the visible block, not the outside adjacent cell"
        );
    }

    #[test]
    fn pencil_top_face_anchor_still_builds_on_surface() {
        let cell = pencil_anchor_cell_from_ray(
            IVec3::new(10, 0, 14),
            IVec3::new(10, 1, 14),
            IVec3::X,
            IVec3::Z,
            Vec3::new(10.25, 8.0, 14.75),
            Vec3::NEG_Y,
        );

        assert_eq!(
            cell,
            IVec3::new(10, 1, 14),
            "Pencil top-face clicks should still place on top of terrain/floors"
        );
    }

    #[test]
    fn pencil_endpoint_keeps_start_plane_but_uses_target_side_cell_reference() {
        let endpoint = snap_pencil_endpoint_to_locked_plane_from_ray(
            IVec3::new(20, 10, 6),
            IVec3::X,
            IVec3::Y,
            IVec3::Z,
            IVec3::new(20, 2, 6),
            IVec3::new(21, 2, 6),
            Vec3::new(30.0, 2.25, 6.75),
            Vec3::NEG_X,
        );

        assert_eq!(
            endpoint,
            IVec3::new(20, 2, 6),
            "Pencil previews should align with the block under the cursor on the locked plane"
        );
    }

    #[test]
    fn arrow_keys_toggle_sketchup_style_axis_locks() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowRight);
        assert_eq!(update_rect_axis_lock(&keys, None), Some(RectAxisLock::X));

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowLeft);
        assert_eq!(update_rect_axis_lock(&keys, None), Some(RectAxisLock::Y));

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowUp);
        assert_eq!(update_rect_axis_lock(&keys, None), Some(RectAxisLock::Z));

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowDown);
        assert_eq!(update_rect_axis_lock(&keys, Some(RectAxisLock::X)), None);
        assert_eq!(update_rect_axis_lock(&keys, None), None);
        keys.press(KeyCode::ArrowRight);
        assert_eq!(
            update_rect_axis_lock(&keys, Some(RectAxisLock::Z)),
            None,
            "Down is the explicit relative-mode fallback"
        );
    }

    #[test]
    fn pencil_axis_lock_projects_endpoint_from_camera_ray() {
        let endpoint = snap_pencil_endpoint_to_axis_from_ray(
            IVec3::ZERO,
            Some(RectAxisLock::X),
            Vec3::new(0.5, 10.5, 0.5),
            Vec3::new(1.0, -1.0, 0.0).normalize(),
        )
        .expect("locked endpoint");

        assert_eq!(endpoint, IVec3::new(10, 0, 0));
    }

    #[test]
    fn pencil_axis_lock_marker_tracks_projected_cursor_coordinate() {
        let (endpoint, marker) = snap_pencil_axis_endpoint_and_marker_from_ray(
            IVec3::ZERO,
            Vec3::splat(0.5),
            Some(RectAxisLock::X),
            Vec3::new(0.5, 10.5, 0.5),
            Vec3::new(1.0, -1.0, 0.0).normalize(),
        )
        .expect("locked endpoint and marker");

        assert_eq!(endpoint, IVec3::new(10, 0, 0));
        assert_eq!(
            marker,
            Vec3::new(10.5, 0.5, 0.5),
            "the visible Pencil marker should stay under the cursor-projected lock point instead of falling back to the committed voxel center"
        );
    }

    #[test]
    fn pencil_axis_lock_preserves_exact_chained_start_marker() {
        let start = IVec3::new(3, 4, 0);
        let exact_start_marker = Vec3::new(3.0, 4.5, 0.5);
        let (endpoint, marker) = snap_pencil_axis_endpoint_and_marker_from_ray(
            start,
            exact_start_marker,
            Some(RectAxisLock::X),
            Vec3::new(3.0, 10.5, 0.5),
            Vec3::new(1.0, -1.0, 0.0).normalize(),
        )
        .expect("locked endpoint and marker");

        assert_eq!(endpoint, IVec3::new(9, 4, 0));
        assert_eq!(
            marker,
            Vec3::new(9.0, 4.5, 0.5),
            "axis-locked chained Pencil previews must preserve the exact visible start point on non-locked axes"
        );
    }

    #[test]
    fn pencil_axis_lock_uses_cell_center_thresholds_not_corner_rounding() {
        let endpoint = snap_pencil_endpoint_to_axis_from_ray(
            IVec3::ZERO,
            Some(RectAxisLock::Y),
            Vec3::new(0.5, 7.85, 10.5),
            Vec3::NEG_Z,
        )
        .expect("locked endpoint");

        assert_eq!(
            endpoint,
            IVec3::new(0, 7, 0),
            "locked Pencil endpoints should not jump to the next voxel before the cursor crosses that cell boundary"
        );
    }

    #[test]
    fn pencil_line_cells_allows_vertical_axis_locked_line() {
        let cells = pencil_line_cells(
            IVec3::new(0, 10, 0),
            IVec3::new(0, 14, 0),
            IVec3::Y,
            DRAW_CELL_CAP,
        );

        assert_eq!(
            cells,
            vec![
                IVec3::new(0, 10, 0),
                IVec3::new(0, 11, 0),
                IVec3::new(0, 12, 0),
                IVec3::new(0, 13, 0),
                IVec3::new(0, 14, 0),
            ]
        );
    }

    #[test]
    fn rect_face_snap_uses_shared_inference_kinds_and_tooltips() {
        assert_eq!(
            rect_face_snap_inference_kind(RectFaceSnapKind::Endpoint),
            crate::sketch_model::InferenceKind::Endpoint
        );
        assert_eq!(
            rect_face_snap_inference_kind(RectFaceSnapKind::Midpoint).tooltip(),
            "Midpoint"
        );
        assert_eq!(
            rect_face_snap_inference_kind(RectFaceSnapKind::FaceCenter).tooltip(),
            "Face center"
        );
    }

    #[test]
    fn semantic_draw_snap_uses_nearest_edge_endpoint_not_first_endpoint() {
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let edge = sketch_doc
            .draw_pencil_line(
                sketch_doc.active_context(),
                Vec3::new(0.5, 4.5, 0.5),
                Vec3::new(8.5, 4.5, 0.5),
            )
            .expect("edge");
        let hit = crate::sketch_model::HitRecord::new(
            edge,
            [],
            crate::sketch_model::HitKind::Edge,
            Vec3::new(7.8, 4.5, 0.5),
            0.0,
        );

        let input = semantic_draw_input_point(
            &sketch_doc,
            Some(&hit),
            Some(Vec3::new(7.8, 4.5, 0.5)),
            IVec3::new(0, 4, 0),
            IVec3::Y,
            IVec3::X,
            IVec3::Z,
            true,
            None,
        )
        .expect("semantic endpoint");

        assert_eq!(input.kind, RectFaceSnapKind::Endpoint);
        assert_eq!(input.cell, IVec3::new(8, 4, 0));
        assert_eq!(input.point, Vec3::new(8.5, 4.5, 0.5));
    }

    #[test]
    fn semantic_draw_snap_reports_midpoint_when_cursor_is_near_edge_center() {
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let edge = sketch_doc
            .draw_pencil_line(
                sketch_doc.active_context(),
                Vec3::new(0.5, 4.5, 0.5),
                Vec3::new(8.5, 4.5, 0.5),
            )
            .expect("edge");
        let hit = crate::sketch_model::HitRecord::new(
            edge,
            [],
            crate::sketch_model::HitKind::Edge,
            Vec3::new(4.5, 4.5, 0.5),
            0.0,
        );

        let input = semantic_draw_input_point(
            &sketch_doc,
            Some(&hit),
            Some(Vec3::new(4.5, 4.5, 0.5)),
            IVec3::new(0, 4, 0),
            IVec3::Y,
            IVec3::X,
            IVec3::Z,
            true,
            None,
        )
        .expect("semantic midpoint");

        assert_eq!(input.kind, RectFaceSnapKind::Midpoint);
        assert_eq!(input.cell, IVec3::new(4, 4, 0));
        assert_eq!(input.point, Vec3::new(4.5, 4.5, 0.5));
    }

    #[test]
    fn semantic_draw_screen_space_snap_prefers_cursor_endpoint_over_hover_cell() {
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();
        let _near_hover_edge = sketch_doc
            .draw_pencil_line(
                sketch_doc.active_context(),
                Vec3::new(0.5, 4.5, 0.5),
                Vec3::new(2.5, 4.5, 0.5),
            )
            .expect("near hover edge");
        let target_edge = sketch_doc
            .draw_pencil_line(
                sketch_doc.active_context(),
                Vec3::new(10.5, 4.5, 0.5),
                Vec3::new(12.5, 4.5, 0.5),
            )
            .expect("target edge");
        let hover = crate::sketch_model::HitRecord::new(
            target_edge,
            [],
            crate::sketch_model::HitKind::Edge,
            Vec3::new(10.5, 4.5, 0.5),
            0.0,
        );
        let view_projection = Mat4::IDENTITY;
        let viewport = Vec2::new(100.0, 100.0);
        let target_screen = crate::sketch_model::project_world_to_screen(
            Vec3::new(12.5, 4.5, 0.5),
            view_projection,
            viewport,
        )
        .expect("screen projection")
        .screen;

        let input = semantic_draw_screen_space_input_point(
            &sketch_doc,
            Some(&hover),
            Some(Vec3::new(10.5, 4.5, 0.5)),
            IVec3::new(0, 4, 0),
            IVec3::Y,
            IVec3::X,
            IVec3::Z,
            true,
            None,
            target_screen,
            view_projection,
            viewport,
        )
        .expect("screen-space endpoint");

        assert_eq!(
            input.point,
            Vec3::new(12.5, 4.5, 0.5),
            "Pencil should follow the endpoint nearest the visible cursor, not the raw hover cell"
        );
        assert_eq!(input.kind, RectFaceSnapKind::Endpoint);
        assert_eq!(input.cell, IVec3::new(12, 4, 0));
    }

    #[test]
    fn static_snap_markers_keep_endpoint_priority_without_animation() {
        let endpoint = rect_snap_marker_radius(Some(RectFaceSnapKind::Endpoint), true);
        let midpoint = rect_snap_marker_radius(Some(RectFaceSnapKind::Midpoint), true);
        let face = rect_snap_marker_radius(Some(RectFaceSnapKind::FaceCenter), true);
        let fallback = rect_snap_marker_radius(None, true);

        assert!(endpoint > midpoint);
        assert!(midpoint > face);
        assert!(face > fallback);
    }

    #[test]
    fn snap_acquisition_feedback_expires_and_does_not_restart_perpetually() {
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.current = IVec3::new(3, 4, 5);
        draw.current_point = Vec3::new(3.5, 4.5, 5.5);
        draw.normal = IVec3::Y;
        draw.snap_kind = Some(RectFaceSnapKind::Endpoint);

        let (feedback, progress) =
            tick_rect_visual_feedback(&mut draw, 0.0).expect("initial acquisition feedback");
        assert_eq!(feedback.kind, RectVisualFeedbackKind::Acquisition);
        assert_eq!(progress, 0.0);

        assert!(
            tick_rect_visual_feedback(&mut draw, RECT_ACQUISITION_FEEDBACK_SECONDS + 0.01,)
                .is_none()
        );
        assert!(
            tick_rect_visual_feedback(&mut draw, 0.0).is_none(),
            "an unchanged snap must not restart a perpetual pulse"
        );

        draw.current = IVec3::new(4, 4, 5);
        assert!(tick_rect_visual_feedback(&mut draw, 0.0).is_some());
    }

    #[test]
    fn commit_feedback_is_finite_and_reduce_motion_keeps_it_stationary() {
        let mut draw = RectDrawState::default();
        begin_rect_visual_feedback(
            &mut draw,
            RectVisualFeedbackKind::Commit,
            Vec3::new(2.0, 3.0, 4.0),
            IVec3::Y,
            Some(RectFaceSnapKind::Endpoint),
        );

        let (feedback, progress) =
            tick_rect_visual_feedback(&mut draw, 0.0).expect("commit feedback");
        assert_eq!(feedback.kind, RectVisualFeedbackKind::Commit);
        assert_eq!(
            rect_visual_feedback_radius(feedback.kind, progress, true),
            rect_visual_feedback_radius(feedback.kind, 0.9, true),
            "reduced-motion feedback should not expand"
        );
        assert_ne!(
            rect_visual_feedback_radius(feedback.kind, progress, false),
            rect_visual_feedback_radius(feedback.kind, 0.9, false)
        );
        assert!(
            tick_rect_visual_feedback(&mut draw, RECT_COMMIT_FEEDBACK_SECONDS + 0.01,).is_none()
        );
    }

    #[test]
    fn editor_pointer_marker_tracks_exact_active_draw_point_for_screenshots() {
        let mut marker = SketchEditorPointerMarker::default();
        let mut draw = RectDrawState::default();
        draw.active = true;
        draw.current = IVec3::new(7, 8, 9);
        draw.current_point = Vec3::new(7.25, 8.5, 9.75);
        draw.normal = IVec3::Y;
        draw.snap_kind = Some(RectFaceSnapKind::Midpoint);

        assert!(sync_pointer_marker_from_active_draw(&mut marker, &draw));

        assert!(marker.active);
        assert!(marker.drawing);
        assert_eq!(marker.cell, draw.current);
        assert_eq!(marker.point, draw.current_point);
        assert_eq!(marker.snap_kind, Some(RectFaceSnapKind::Midpoint));
    }

    #[test]
    fn screen_cursor_label_reports_snap_kind_for_screenshots() {
        assert_eq!(
            screen_cursor_label(Some(RectFaceSnapKind::Endpoint), false),
            "Endpoint"
        );
        assert_eq!(
            screen_cursor_label(Some(RectFaceSnapKind::Midpoint), true),
            "Midpoint"
        );
        assert_eq!(
            screen_cursor_label(Some(RectFaceSnapKind::FaceCenter), false),
            "Face center"
        );
        assert_eq!(screen_cursor_label(None, true), "Cursor");
        assert_eq!(screen_cursor_label(None, false), "Pointer");
    }

    #[test]
    fn screen_cursor_resource_keeps_mouse_and_snap_target_separate() {
        let mut cursor = SketchEditorScreenCursor::default();

        cursor.set(
            Vec2::new(1180.0, 620.0),
            Some(Vec2::new(840.0, 410.0)),
            Some(RectFaceSnapKind::Midpoint),
            true,
            false,
        );

        assert!(cursor.active);
        assert_eq!(cursor.cursor, Vec2::new(1180.0, 620.0));
        assert_eq!(cursor.target, Some(Vec2::new(840.0, 410.0)));
        assert_ne!(
            Some(cursor.cursor),
            cursor.target,
            "screenshot overlay must preserve the user's real cursor separately from the snapped voxel target"
        );
        assert_eq!(
            screen_cursor_label(cursor.snap_kind, cursor.drawing),
            "Midpoint"
        );
    }

    #[test]
    fn hidden_windows_cursor_clears_stale_screen_cursor_position() {
        let stale_position = Some(Vec2::new(1510.0, 690.0));

        assert_eq!(
            visible_screen_cursor_position(true, stale_position),
            stale_position
        );
        assert_eq!(
            visible_screen_cursor_position(false, stale_position),
            None,
            "the editor overlay must not draw from a stale Windows cursor while orbit/navigation has hidden it"
        );
        assert_eq!(visible_screen_cursor_position(true, None), None);
    }

    #[test]
    fn editor_pointer_marker_clears_when_no_draw_or_hover_target_exists() {
        let mut marker = SketchEditorPointerMarker::default();
        marker.set(
            Vec3::new(4.0, 5.0, 6.0),
            IVec3::Y,
            IVec3::new(4, 5, 6),
            Some(RectFaceSnapKind::Endpoint),
            false,
        );
        let draw = RectDrawState::default();
        let world = VoxelWorld::new();

        sync_pointer_marker(
            &mut marker,
            &draw,
            &world,
            ToolbeltTool::DrawRect,
            true,
            Vec3::ZERO,
            Vec3::Z,
        );

        assert!(!marker.active);
    }

    #[test]
    fn draw_rect_prefers_visible_cursor_when_windows_confines_pointer() {
        assert!(
            editor_pointer_ray_available(
                ToolbeltTool::DrawRect,
                true,
                true,
                Some(Vec2::new(1420.0, 730.0))
            ),
            "Sketch drawing must use the visible mouse position even when Windows reports a confined cursor"
        );
    }

    #[test]
    fn pointer_editor_tools_reject_hidden_captured_cursor_even_with_stale_position() {
        assert!(
            !editor_pointer_ray_available(
                ToolbeltTool::DrawRect,
                true,
                false,
                Some(Vec2::new(1420.0, 730.0))
            ),
            "Hidden orbit cursor positions are stale on Windows and must not drive the Sketch preview"
        );
    }

    #[test]
    fn pointer_editor_tools_do_not_fall_back_to_crosshair_without_cursor() {
        assert!(editor_pointer_tool_requires_cursor(ToolbeltTool::DrawRect));
        assert!(editor_pointer_tool_requires_cursor(ToolbeltTool::Sculpt));
        assert!(!editor_pointer_tool_requires_cursor(
            ToolbeltTool::BrushPlace
        ));

        assert!(
            !editor_pointer_ray_available(ToolbeltTool::DrawRect, false, true, None),
            "Pencil/Rectangle must not build from the camera center when Windows drops the pointer position"
        );
        assert!(
            !editor_pointer_ray_available(ToolbeltTool::Sculpt, true, true, None),
            "Push/Pull must wait for a real editor pointer instead of cutting or pulling under the crosshair"
        );
    }

    #[test]
    fn semantic_axis_lock_projects_target_coordinate_onto_locked_line() {
        let input = SemanticDrawInputPoint {
            cell: IVec3::new(12, 9, 8),
            point: Vec3::new(12.5, 9.5, 8.5),
            kind: RectFaceSnapKind::Endpoint,
        };

        let (endpoint, marker) = semantic_axis_locked_endpoint(
            IVec3::new(2, 4, 8),
            pencil_cell_marker_point(IVec3::new(2, 4, 8)),
            input,
            RectAxisLock::X,
        );

        assert_eq!(endpoint, IVec3::new(12, 4, 8));
        assert_eq!(marker, Vec3::new(12.5, 4.5, 8.5));
    }

    #[test]
    fn semantic_axis_lock_marker_keeps_exact_target_coordinate_not_cell_center() {
        let input = SemanticDrawInputPoint {
            cell: IVec3::new(12, 9, 8),
            point: Vec3::new(12.0, 9.25, 8.25),
            kind: RectFaceSnapKind::Endpoint,
        };

        let (endpoint, marker) = semantic_axis_locked_endpoint(
            IVec3::new(2, 4, 8),
            pencil_cell_marker_point(IVec3::new(2, 4, 8)),
            input,
            RectAxisLock::X,
        );

        assert_eq!(endpoint, IVec3::new(12, 4, 8));
        assert_eq!(
            marker,
            Vec3::new(12.0, 4.5, 8.5),
            "the visual axis-lock marker should sit on the exact endpoint coordinate, not on the voxel center"
        );
    }

    #[test]
    fn pencil_display_marker_uses_face_midpoint_when_commit_cell_stays_quantized() {
        let endpoint = IVec3::new(10, 1, 14);
        let face_input = RectFaceInputPoint {
            point: Vec3::new(10.5, 1.0, 14.0),
            kind: Some(RectFaceSnapKind::Midpoint),
        };

        let marker = pencil_display_point_for_endpoint(
            endpoint,
            None,
            Some(face_input),
            endpoint,
            pencil_cell_marker_point(endpoint),
            IVec3::Y,
            RectEndpointInference::None,
            None,
        );

        assert_eq!(
            marker,
            Vec3::new(10.5, 1.5, 14.0),
            "Pencil should show the hovered midpoint while committing to the correct voxel cell"
        );
    }

    #[test]
    fn pencil_axis_lock_display_marker_uses_face_cursor_coordinate() {
        let endpoint = IVec3::new(12, 4, 8);
        let face_input = RectFaceInputPoint {
            point: Vec3::new(12.0, 9.25, 8.25),
            kind: Some(RectFaceSnapKind::Endpoint),
        };

        let marker = pencil_display_point_for_endpoint(
            endpoint,
            None,
            Some(face_input),
            IVec3::new(2, 4, 8),
            pencil_cell_marker_point(IVec3::new(2, 4, 8)),
            IVec3::Y,
            RectEndpointInference::Axis,
            Some(RectAxisLock::X),
        );

        assert_eq!(
            marker,
            Vec3::new(12.0, 4.5, 8.5),
            "axis-locked Pencil previews should show the exact hovered endpoint coordinate on the locked axis"
        );
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
    fn rect_endpoint_inference_reuses_previous_drawn_length() {
        let (snapped, inference) = infer_rect_endpoint_with_reference(
            IVec3::new(0, 64, 0),
            IVec3::new(11, 64, 1),
            IVec3::X,
            IVec3::Z,
            IVec2::new(12, 0),
        );

        assert_eq!(snapped, IVec3::new(12, 64, 0));
        assert_eq!(inference, RectEndpointInference::ReferenceLength);
    }

    #[test]
    fn rect_endpoint_inference_reuses_previous_height_as_new_width() {
        let (snapped, inference) = infer_rect_endpoint_with_reference(
            IVec3::new(0, 64, 0),
            IVec3::new(11, 64, 0),
            IVec3::X,
            IVec3::Z,
            IVec2::new(0, 12),
        );

        assert_eq!(snapped, IVec3::new(12, 64, 0));
        assert_eq!(inference, RectEndpointInference::ReferenceLength);
    }

    #[test]
    fn rect_endpoint_inference_keeps_deliberate_new_length() {
        let (snapped, inference) = infer_rect_endpoint_with_reference(
            IVec3::new(0, 64, 0),
            IVec3::new(17, 64, 0),
            IVec3::X,
            IVec3::Z,
            IVec2::new(12, 0),
        );

        assert_eq!(snapped, IVec3::new(17, 64, 0));
        assert_eq!(inference, RectEndpointInference::None);
    }

    #[test]
    fn semantic_endpoint_snap_wins_over_rect_auto_inference() {
        let start = IVec3::new(0, 64, 0);
        let semantic_endpoint = IVec3::new(11, 64, 1);

        let (snapped, inference) = resolve_rect_endpoint_after_snap(
            start,
            semantic_endpoint,
            true,
            None,
            IVec3::X,
            IVec3::Z,
            IVec2::new(12, 0),
        );

        assert_eq!(
            snapped, semantic_endpoint,
            "a visible endpoint/midpoint/face-center snap must keep the exact hovered cell"
        );
        assert_eq!(
            inference,
            RectEndpointInference::None,
            "semantic snaps should not be relabeled as hidden reference-length inference"
        );
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
