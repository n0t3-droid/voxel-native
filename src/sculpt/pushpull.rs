//! Phase 1 — SketchUp-style Push/Pull face extrusion.
//!
//! When the SCULPT toolbelt slot is live and the player is in
//! first-person camera mode, this module:
//!
//! 1. Each frame, raycasts from the camera and floods the contiguous
//!    coplanar face under the crosshair (see [`super::face`]). The face
//!    boundary is drawn as a pulse-coloured outline via `Gizmos`.
//!
//! 2. On first click, locks that face into a [`PushPullDrag`] and records
//!    the screen-space direction the surface normal projects to. Until
//!    the second click, mouse-motion delta is projected onto that
//!    direction, divided by [`PIXELS_PER_VOXEL`], and rounded to an
//!    integer "extrusion distance" `d`.
//!
//! 3. Each frame while active, if `d` changed, the previous preview
//!    is reverted (write `before` voxels back) and a new preview is
//!    applied — extrude with `d > 0`, intrude (delete) with `d < 0`. We
//!    use [`VoxelWorld::edit_set_voxel_batched`] which only touches the
//!    voxel slot, leaving material data intact so revert is exact.
//!
//! 4. On the second click, the accumulated `(pos, before, after)` list is
//!    pushed onto the same undo timeline as the Classic builder via
//!    [`BuilderHistory::record_external`]. Ctrl+Z then rewinds the
//!    extrusion as a single batch.
//!
//! Anti-griefing rationale: the face flood is capped at
//! [`super::face::FACE_CELL_CAP`] = 16 384 cells, and the per-extrusion
//! voxel count is implicitly bounded by `cells * |d|`. A 4 096-cell face
//! pulled 32 voxels would stamp 131 072 voxels — well within the
//! existing batch system's working set.

use ahash::{AHashMap, AHashSet};
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::{voxel_is_solid, Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::mode::{BuildGestureLock, ModeContext};
use crate::player::Player;
use crate::sculpt::face::{collect_face, FaceRegion};
use crate::sculpt::raycast::dda_voxel;
use crate::sculpt::state::{HoverHit, SculptMode, SculptState};
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

/// Pixels of mouse travel that map to one voxel of extrusion. Chosen so
/// a deliberate ~14 px nudge always commits to one step (matches the
/// "snap to integer voxels" UX from the design doc — no free-angle
/// preview means we always quantise).
pub const PIXELS_PER_VOXEL: f32 = 14.0;

/// Maximum reach of the face-finding ray, in voxels.
pub const SCULPT_RAY_REACH: f32 = 96.0;
const PREVIEW_VOXEL_CAP: usize = 240_000;
const PUSH_PULL_OWNER: &str = "Push Pull Face";
const REFERENCE_POINT_SIZE: f32 = 0.16;
const HOVER_POINT_SIZE: f32 = 0.08;
const TAP_CLEANUP_FACE_CAP: usize = 768;
const TAP_CLEANUP_MAX_MOTION_PX: f32 = 4.0;

/// Floor for the screen-space normal projection magnitude: when the
/// camera looks edge-on at a face, the projected normal can collapse to
/// near-zero pixels, which would amplify any mouse jitter into huge
/// distances. We clamp the divisor here so the system stays usable even
/// at grazing angles.
const SCREEN_DIR_MIN_PX: f32 = 12.0;

/// In-flight push/pull drag. Lives only between LMB-down and LMB-up.
#[derive(Resource, Default)]
pub struct PushPullDrag {
    pub active: bool,
    /// Solid cells that compose the locked face.
    pub face_cells: Vec<IVec3>,
    /// Outward normal of the face (one component ±1).
    pub normal: IVec3,
    /// Source block type — what extrusion writes outward.
    pub voxel: Voxel,
    /// World-space anchor used to project the screen-space normal.
    pub anchor_world: Vec3,
    /// Cached unit vector in viewport space pointing along `+normal`.
    /// Length is the original projection magnitude (clamped to
    /// [`SCREEN_DIR_MIN_PX`]).
    pub screen_dir: Vec2,
    /// Sum of mouse-motion deltas since drag started (viewport pixels,
    /// y-down).
    pub motion_accum: Vec2,
    /// Total mouse travel during the gesture. Lets a clean tap mean
    /// "remove this leftover face/layer" while a failed drag still
    /// cancels safely.
    pub motion_len: f32,
    /// SketchUp-style click-move-click operation. The first LMB click locks
    /// the face; releasing it does not finish. The next LMB click commits.
    pub click_finish: bool,
    /// Toolbox selection generation that started this operation. If the
    /// mouse-first editor picks another tool, the live preview must revert
    /// instead of keeping ownership of clicks.
    pub tool_generation: u64,
    /// Most recently applied integer distance. `0` means the preview is
    /// empty and nothing has been written yet.
    pub last_d: i32,
    /// Initial signed layer distance seeded from an A/B reference, if
    /// the drag started with a completed reference measurement.
    pub reference_d: Option<i32>,
    /// `pos -> before-value` for every voxel the preview has touched.
    /// Reverting the preview means writing each `before` back; committing
    /// means computing `(pos, before, current_world_value)` triples.
    pub preview: AHashMap<IVec3, Voxel>,
}

impl PushPullDrag {
    fn clear(&mut self) {
        self.active = false;
        self.face_cells.clear();
        self.preview.clear();
        self.motion_accum = Vec2::ZERO;
        self.motion_len = 0.0;
        self.click_finish = false;
        self.tool_generation = 0;
        self.last_d = 0;
        self.reference_d = None;
    }
}

#[derive(Debug, Clone, Copy)]
pub enum InferenceSnapKind {
    Endpoint,
    Midpoint,
    FaceCenter,
}

impl InferenceSnapKind {
    fn label(self) -> &'static str {
        pushpull_inference_kind(self).tooltip()
    }
}

fn pushpull_inference_kind(kind: InferenceSnapKind) -> crate::sketch_model::InferenceKind {
    match kind {
        InferenceSnapKind::Endpoint => crate::sketch_model::InferenceKind::Endpoint,
        InferenceSnapKind::Midpoint => crate::sketch_model::InferenceKind::Midpoint,
        InferenceSnapKind::FaceCenter => crate::sketch_model::InferenceKind::FaceCenter,
    }
}

#[derive(Debug, Clone, Copy)]
struct InferenceCandidate {
    point: Vec3,
    kind: InferenceSnapKind,
}

#[derive(Debug, Clone, Copy)]
pub struct PushPullReferencePoint {
    pub point: Vec3,
    pub normal: IVec3,
    pub cell: IVec3,
    pub kind: InferenceSnapKind,
}

#[derive(Resource, Default)]
pub struct PushPullReference {
    pub start: Option<PushPullReferencePoint>,
    pub end: Option<PushPullReferencePoint>,
}

impl PushPullReference {
    fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }

    fn ready(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }

    fn delta(&self) -> Option<Vec3> {
        Some(self.end?.point - self.start?.point)
    }

    fn distance_for(&self, normal: IVec3) -> Option<i32> {
        let delta = self.delta()?;
        let distance = delta.dot(normal.as_vec3()).round() as i32;
        (distance != 0).then_some(distance)
    }
}

fn shape_alt_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)
}

/// Returns `true` while Push/Pull owns the current Shape gesture. Fill
/// and Push/Pull are paired: the selected tool is the default, while
/// Alt temporarily invokes the other one without changing toolbelt state.
fn sculpt_active(mode: &ModeContext, keys: &ButtonInput<KeyCode>, drag_active: bool) -> bool {
    if !mode.is_build_live() {
        return false;
    }
    match mode.build_tool() {
        Some(ToolbeltTool::Sculpt) => drag_active || !shape_alt_pressed(keys),
        Some(ToolbeltTool::DrawRect) => drag_active || shape_alt_pressed(keys),
        _ => false,
    }
}

/// Update [`SculptState::hover`] each frame from the camera ray. Runs
/// only when the sculpt tool is live; resets hover to `None` otherwise
/// so stale highlights don't bleed into other tools.
pub fn update_hover(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    drag: Res<PushPullDrag>,
    world: Res<VoxelWorld>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    mut state: ResMut<SculptState>,
) {
    // Don't re-flood while a drag is locked — the face cells are pinned
    // at click time and re-flooding mid-drag would chase the preview.
    if drag.active {
        return;
    }
    if !sculpt_active(&mode, &keys, drag.active) {
        if state.hover.is_some() {
            state.hover = None;
            state.mode = SculptMode::Idle;
        }
        return;
    }
    let Ok(cam_tf) = cam_q.get_single() else {
        state.hover = None;
        return;
    };

    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    if let Some((hit, prev)) = dda_voxel(&world, origin, dir, SCULPT_RAY_REACH) {
        state.hover = Some(HoverHit { voxel: hit, prev });
        state.mode = SculptMode::PushPull;
    } else {
        state.hover = None;
        state.mode = SculptMode::Idle;
    }
}

/// Resolve the face under the current hover (every frame, while not
/// dragging). Stored in a small per-frame resource so the gizmo system
/// can paint it without repeating the flood-fill.
#[derive(Resource, Default)]
pub struct HoverFace(pub Option<FaceRegion>);

pub fn resolve_hover_face(
    state: Res<SculptState>,
    drag: Res<PushPullDrag>,
    world: Res<VoxelWorld>,
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
    mut hover_face: ResMut<HoverFace>,
    mut semantic_hover: ResMut<crate::sketch_model::SemanticHoverHit>,
) {
    if drag.active {
        // Build a synthetic FaceRegion from the locked drag so the gizmo
        // keeps highlighting the same surface during extrusion.
        let region = FaceRegion {
            cells: drag.face_cells.clone(),
            voxel: drag.voxel,
            normal: drag.normal,
            clipped: false,
        };
        semantic_hover.0 = semantic_hover_hit_from_region(&region, &sketch_links);
        hover_face.0 = Some(region);
        return;
    }
    let Some(hit) = state.hover else {
        hover_face.0 = None;
        semantic_hover.0 = None;
        return;
    };
    let normal = hit.normal();
    hover_face.0 = collect_face(&world, hit.voxel, normal);
    semantic_hover.0 = hover_face
        .0
        .as_ref()
        .and_then(|region| semantic_hover_hit_from_region(region, &sketch_links));
}

fn semantic_hover_hit_from_region(
    region: &FaceRegion,
    sketch_links: &crate::sketch_model::SketchVoxelLinkIndex,
) -> Option<crate::sketch_model::HitRecord> {
    let cell = *region.cells.first()?;
    sketch_links.hit_for_face(
        cell,
        region.normal,
        Vec3::new(
            cell.x as f32 + 0.5,
            cell.y as f32 + 0.5,
            cell.z as f32 + 0.5,
        ),
        0.0,
    )
}

pub fn semantic_select_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    semantic_hover: Res<crate::sketch_model::SemanticHoverHit>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if !semantic_select_input_active(
        mode.is_build_live(),
        mode.build_tool(),
        tool_controller.active_tool(),
        mouse.just_pressed(MouseButton::Left),
        ui_focus
            .as_deref()
            .is_some_and(|focus| focus.pointer_over_editor_ui),
    ) {
        return;
    }

    let additive = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let update =
        apply_semantic_selection_click(&mut tool_controller, semantic_hover.0.as_ref(), additive);
    match update {
        crate::sketch_model::SelectionUpdate::Selected(id) => {
            toolbelt.status = format!(
                "Select: semantic entity {} selected. Shift-click adds to selection.",
                id.raw()
            );
        }
        crate::sketch_model::SelectionUpdate::Cleared => {
            toolbelt.status = "Select: semantic selection cleared.".into();
        }
        crate::sketch_model::SelectionUpdate::Ignored => {}
    }
}

fn semantic_select_input_active(
    build_live: bool,
    build_tool: Option<ToolbeltTool>,
    active_editor_tool: crate::sketch_model::EditorToolId,
    left_just_pressed: bool,
    pointer_over_editor_ui: bool,
) -> bool {
    build_live
        && left_just_pressed
        && !pointer_over_editor_ui
        && (build_tool == Some(ToolbeltTool::Navigate)
            || active_editor_tool == crate::sketch_model::EditorToolId::Select)
}

fn apply_semantic_selection_click(
    tool_controller: &mut crate::sketch_model::ToolController,
    hover: Option<&crate::sketch_model::HitRecord>,
    additive: bool,
) -> crate::sketch_model::SelectionUpdate {
    if let Some(hit) = hover {
        tool_controller.select_hit(hit, additive)
    } else if additive {
        crate::sketch_model::SelectionUpdate::Ignored
    } else {
        tool_controller.clear_selection()
    }
}

pub fn reference_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    drag: Res<PushPullDrag>,
    world: Res<VoxelWorld>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    mut reference: ResMut<PushPullReference>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if !sculpt_active(&mode, &keys, drag.active) || drag.active {
        return;
    }

    if keys.just_pressed(KeyCode::Escape) && reference.start.is_some() {
        reference.clear();
        toolbelt.status =
            "Push reference cleared. RMB sets corner A, RMB again sets target corner B.".into();
        return;
    }

    if !mouse.just_pressed(MouseButton::Right) {
        return;
    }

    let Ok(cam_tf) = cam_q.get_single() else {
        toolbelt.status = "Push reference could not find the player camera this frame.".into();
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some((hit, prev)) = dda_voxel(&world, origin, dir, SCULPT_RAY_REACH) else {
        toolbelt.status =
            "Push reference needs a visible block face/corner under the crosshair.".into();
        return;
    };
    let Some(point) = reference_point_from_hit(origin, dir, hit, prev) else {
        toolbelt.status =
            "Push reference could not snap this face. Aim at a clearer face corner.".into();
        return;
    };

    if reference.start.is_none() || reference.ready() {
        reference.start = Some(point);
        reference.end = None;
        toolbelt.status = format!(
            "Push reference A set at {} {}. RMB another endpoint/midpoint/face center for B.",
            point.kind.label(),
            fmt_point(point.point)
        );
    } else {
        reference.end = Some(point);
        let delta = reference.delta().unwrap_or(Vec3::ZERO);
        toolbelt.status = format!(
            "Push reference A->B locked to {} {}: dX {:+.1}, dY {:+.1}, dZ {:+.1}. Next Push/Pull snaps to selected face axis.",
            point.kind.label(),
            fmt_point(point.point),
            delta.x, delta.y, delta.z
        );
    }
}

fn reference_point_from_hit(
    origin: Vec3,
    dir: Vec3,
    hit: IVec3,
    prev: IVec3,
) -> Option<PushPullReferencePoint> {
    let normal = prev - hit;
    let axis = normal_axis(normal)?;
    let denom = vec_component(dir, axis);
    if denom.abs() < 1e-5 {
        return None;
    }
    let plane = if ivec_component(normal, axis) > 0 {
        ivec_component(hit, axis) as f32 + 1.0
    } else {
        ivec_component(hit, axis) as f32
    };
    let t = (plane - vec_component(origin, axis)) / denom;
    if !t.is_finite() || t < 0.0 {
        return None;
    }

    let raw = origin + dir * t;
    let candidates = inference_candidates(hit, normal)?;
    let Some(candidate) = nearest_candidate(raw, &candidates) else {
        return None;
    };

    Some(PushPullReferencePoint {
        point: candidate.point,
        normal,
        cell: hit,
        kind: candidate.kind,
    })
}

fn inference_candidates(hit: IVec3, normal: IVec3) -> Option<Vec<InferenceCandidate>> {
    let axis = normal_axis(normal)?;
    let plane = if ivec_component(normal, axis) > 0 {
        ivec_component(hit, axis) as f32 + 1.0
    } else {
        ivec_component(hit, axis) as f32
    };
    let axes: Vec<usize> = (0..3).filter(|component| *component != axis).collect();
    let u_axis = axes[0];
    let v_axis = axes[1];
    let u0 = ivec_component(hit, u_axis) as f32;
    let v0 = ivec_component(hit, v_axis) as f32;
    let u1 = u0 + 1.0;
    let v1 = v0 + 1.0;
    let um = u0 + 0.5;
    let vm = v0 + 0.5;

    let mut out = Vec::with_capacity(9);
    for u in [u0, u1] {
        for v in [v0, v1] {
            out.push(InferenceCandidate {
                point: face_point(axis, plane, u_axis, u, v_axis, v),
                kind: InferenceSnapKind::Endpoint,
            });
        }
    }
    for (u, v) in [(um, v0), (um, v1), (u0, vm), (u1, vm)] {
        out.push(InferenceCandidate {
            point: face_point(axis, plane, u_axis, u, v_axis, v),
            kind: InferenceSnapKind::Midpoint,
        });
    }
    out.push(InferenceCandidate {
        point: face_point(axis, plane, u_axis, um, v_axis, vm),
        kind: InferenceSnapKind::FaceCenter,
    });
    Some(out)
}

fn nearest_candidate(raw: Vec3, candidates: &[InferenceCandidate]) -> Option<InferenceCandidate> {
    candidates.iter().copied().min_by(|a, b| {
        raw.distance_squared(a.point)
            .partial_cmp(&raw.distance_squared(b.point))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn face_point(axis: usize, plane: f32, u_axis: usize, u: f32, v_axis: usize, v: f32) -> Vec3 {
    let mut point = Vec3::ZERO;
    point = set_vec_component(point, axis, plane);
    point = set_vec_component(point, u_axis, u);
    set_vec_component(point, v_axis, v)
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

fn vec_component(v: Vec3, axis: usize) -> f32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn ivec_component(v: IVec3, axis: usize) -> i32 {
    match axis {
        0 => v.x,
        1 => v.y,
        _ => v.z,
    }
}

fn set_vec_component(mut v: Vec3, axis: usize, value: f32) -> Vec3 {
    match axis {
        0 => v.x = value,
        1 => v.y = value,
        _ => v.z = value,
    }
    v
}

fn fmt_point(p: Vec3) -> String {
    format!("({:.1}, {:.1}, {:.1})", p.x, p.y, p.z)
}

/// Project `world_dir` from `anchor` to viewport space and return the
/// 2D delta vector. Returns `None` if either point projects behind the
/// camera (`world_to_viewport` failed).
fn project_screen_dir(
    camera: &Camera,
    cam_tf: &GlobalTransform,
    anchor: Vec3,
    world_dir: Vec3,
) -> Option<Vec2> {
    let p0 = camera.world_to_viewport(cam_tf, anchor)?;
    let p1 = camera.world_to_viewport(cam_tf, anchor + world_dir)?;
    Some(p1 - p0)
}

/// Begin a push/pull drag on LMB-down. Captures the locked face, the
/// click anchor, and the screen-space normal.
#[allow(clippy::too_many_arguments)]
pub fn begin_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    state: Res<SculptState>,
    hover_face: Res<HoverFace>,
    reference: Res<PushPullReference>,
    cam_q: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut drag: ResMut<PushPullDrag>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
) {
    if drag.active {
        return;
    }
    if !sculpt_active(&mode, &keys, drag.active) {
        return;
    }
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Some(face) = hover_face.0.as_ref() else {
        toolbelt.status = "Push Pull Face needs a flat target face under the crosshair.".into();
        return;
    };
    let Some(_hit) = state.hover else {
        toolbelt.status = "Push Pull Face has no voxel hit. Aim at a visible block face.".into();
        return;
    };
    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        toolbelt.status = "Push Pull Face could not find the player camera this frame.".into();
        return;
    };
    if face.cells.is_empty() {
        toolbelt.status = "Push Pull Face found an empty face region.".into();
        return;
    }

    // Anchor: centre of the hovered face on the air side. We use the
    // first cell as the visual anchor — exact choice doesn't matter for
    // the projection magnitude as long as we project the normal from
    // the same point we use throughout the drag.
    let anchor = face.cells[0].as_vec3() + Vec3::splat(0.5) + 0.5 * face.normal.as_vec3();

    let raw = match project_screen_dir(camera, cam_tf, anchor, face.normal.as_vec3()) {
        Some(v) if v.length() > 0.001 => v,
        _ => {
            toolbelt.status =
                "Push Pull Face is edge-on. Move the camera slightly, then drag again.".into();
            return;
        }
    };
    let len = raw.length();
    let screen_dir = if len < SCREEN_DIR_MIN_PX {
        raw / len * SCREEN_DIR_MIN_PX
    } else {
        raw
    };

    drag.active = true;
    drag.face_cells = face.cells.clone();
    drag.normal = face.normal;
    drag.voxel = face.voxel;
    drag.anchor_world = anchor;
    drag.screen_dir = screen_dir;
    let max_d = preview_distance_cap(face.cells.len());
    let reference_raw = reference.distance_for(face.normal);
    let reference_d = reference_raw.map(|d| d.clamp(-max_d, max_d));
    drag.motion_accum = reference_d
        .map(|d| screen_dir.normalize_or_zero() * d as f32 * PIXELS_PER_VOXEL)
        .unwrap_or(Vec2::ZERO);
    drag.motion_len = 0.0;
    drag.click_finish = true;
    drag.tool_generation = toolbelt.selection_generation();
    drag.last_d = 0;
    drag.reference_d = reference_d;
    drag.preview.clear();
    gesture_lock.lock(PUSH_PULL_OWNER);
    tool_controller.begin_transaction("Push/Pull preview");
    toolbelt.status = if mode.build_tool() == Some(ToolbeltTool::DrawRect) {
        format!(
            "Quick Push/Pull started: {} cells locked. Move to choose depth; click again to commit. RMB orbits.",
            drag.face_cells.len()
        )
    } else if let Some(d) = reference_d {
        if reference_raw != reference_d {
            format!(
                "Push Pull Face started with reference snap {d:+} layers (capped at {max_d}). Move to tune; click to commit."
            )
        } else {
            format!(
                "Push Pull Face started with reference snap {d:+} layers. Move to tune; click to commit."
            )
        }
    } else if face.clipped {
        format!(
            "Push Pull Face started: {} cells locked (face capped). Move to extrude/cut; click to commit; RMB orbits; Esc cancels.",
            drag.face_cells.len()
        )
    } else {
        format!(
            "Push Pull Face started: {} cells locked. Move to extrude/cut; click to commit; RMB orbits; Esc cancels.",
            drag.face_cells.len()
        )
    };
}

/// While LMB is held: accumulate motion, derive integer distance, and
/// re-apply the preview if it changed. Runs after [`begin_drag`].
pub fn update_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut motion_evr: EventReader<MouseMotion>,
    mode: Res<ModeContext>,
    mut drag: ResMut<PushPullDrag>,
    mut world: ResMut<VoxelWorld>,
    mut state: ResMut<SculptState>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
) {
    if !drag.active {
        // Drain the queue so events don't pile up between drags.
        motion_evr.clear();
        gesture_lock.release(PUSH_PULL_OWNER);
        return;
    }

    gesture_lock.lock(PUSH_PULL_OWNER);
    if pushpull_should_cancel_for_tool_selection(&drag, toolbelt.selection_generation()) {
        revert_preview(&mut world, &mut drag);
        state.status = "Sculpt: cancelled by toolbox switch.".into();
        toolbelt.status =
            "Push Pull Face cancelled. Toolbox switched tools; preview reverted.".into();
        tool_controller
            .cancel_active_operation(crate::sketch_model::EditorCancelReason::ToolboxClick);
        drag.clear();
        gesture_lock.release(PUSH_PULL_OWNER);
        motion_evr.clear();
        return;
    }

    if !sculpt_active(&mode, &keys, drag.active) {
        revert_preview(&mut world, &mut drag);
        state.status = "Sculpt: cancelled by tool switch.".into();
        toolbelt.status = "Push Pull Face cancelled by tool switch. Preview reverted.".into();
        tool_controller
            .cancel_active_operation(crate::sketch_model::EditorCancelReason::ToolSwitch);
        drag.clear();
        gesture_lock.release(PUSH_PULL_OWNER);
        motion_evr.clear();
        return;
    }

    if pushpull_drag_cancel_requested(
        keys.just_pressed(KeyCode::Escape),
        mouse.just_pressed(MouseButton::Right),
    ) {
        revert_preview(&mut world, &mut drag);
        state.status = "Sculpt: cancelled.".into();
        toolbelt.status = "Push Pull Face cancelled. Preview reverted.".into();
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        drag.clear();
        gesture_lock.release(PUSH_PULL_OWNER);
        motion_evr.clear();
        return;
    }

    if !pushpull_drag_accepts_motion(mouse.pressed(MouseButton::Right)) {
        state.status = "Sculpt: orbiting; Push/Pull depth held.".into();
        toolbelt.status =
            "Push Pull Face held while orbiting. Release RMB to keep tuning; click to commit."
                .into();
        motion_evr.clear();
        return;
    }

    for ev in motion_evr.read() {
        drag.motion_accum += ev.delta;
        drag.motion_len += ev.delta.length();
    }

    // Project motion onto stored screen direction.
    let dir_len2 = drag.screen_dir.length_squared();
    if dir_len2 < 1e-4 {
        return;
    }
    let signed_px = drag.motion_accum.dot(drag.screen_dir) / dir_len2.sqrt();
    // Note: motion accumulates in y-down screen space; screen_dir is in
    // the same convention, so a positive projection means "user moved
    // the cursor along the outward normal direction" → pull (d > 0).
    // BUT mouse-motion's positive y is downward in Bevy, while pulling
    // a face that points upward usually feels like "drag mouse up".
    // Empirically that means the *screen direction* of an upward normal
    // points up (negative y), and dragging up means motion.y is
    // negative, dot product is positive → d positive. Correct.
    let raw_d = (signed_px / PIXELS_PER_VOXEL).round() as i32;
    let max_d = preview_distance_cap(drag.face_cells.len());
    let d = raw_d.clamp(-max_d, max_d);
    let capped = d != raw_d;

    if d == drag.last_d {
        return;
    }

    // Revert previous preview, then apply new.
    revert_preview(&mut world, &mut drag);
    let mut batch = WorldEditBatch::default();

    // Split borrow: pull out the fields apply_preview needs immutably so
    // the &mut on `preview` doesn't conflict with `&face_cells`.
    let drag_ref = &mut *drag;
    apply_preview(
        &mut world,
        &mut batch,
        &drag_ref.face_cells,
        drag_ref.normal,
        drag_ref.voxel,
        d,
        &mut drag_ref.preview,
    );
    world.finish_edit_batch(batch);
    drag.last_d = d;

    // Status line for the HUD overlay.
    state.status = if d == 0 {
        "Sculpt: 0".into()
    } else if d > 0 {
        format!("Sculpt: +{d} (Pull)")
    } else {
        format!("Sculpt: {d} (Push)")
    };
    toolbelt.status = if capped {
        format!(
            "Push Pull Face preview capped at {} layers: {}. {}",
            max_d,
            state.status,
            pushpull_commit_hint(drag.click_finish)
        )
    } else {
        format!(
            "Push Pull Face preview: {}. {}",
            state.status,
            pushpull_commit_hint(drag.click_finish)
        )
    };
}

fn pushpull_drag_cancel_requested(escape_just: bool, _right_just: bool) -> bool {
    escape_just
}

fn pushpull_drag_accepts_motion(right_held: bool) -> bool {
    !right_held
}

fn pushpull_should_cancel_for_tool_selection(drag: &PushPullDrag, current_generation: u64) -> bool {
    drag.active && drag.tool_generation != current_generation
}

fn pushpull_commit_requested(
    click_finish: bool,
    left_just_pressed: bool,
    left_just_released: bool,
) -> bool {
    if click_finish {
        left_just_pressed
    } else {
        left_just_released
    }
}

fn pushpull_commit_hint(click_finish: bool) -> &'static str {
    if click_finish {
        "Click again to commit."
    } else {
        "Release LMB to commit."
    }
}

fn preview_distance_cap(face_cells: usize) -> i32 {
    let cells = face_cells.max(1);
    let by_voxels = (PREVIEW_VOXEL_CAP / cells).max(1);
    by_voxels.min(128) as i32
}

fn revert_preview(world: &mut VoxelWorld, drag: &mut PushPullDrag) {
    if drag.preview.is_empty() {
        return;
    }
    let mut batch = WorldEditBatch::default();
    for (&pos, &before) in drag.preview.iter() {
        world.edit_set_voxel_batched(pos.x, pos.y, pos.z, before, &mut batch);
    }
    drag.preview.clear();
    world.finish_edit_batch(batch);
}

/// Compute the (pos, target) writes for distance `d` and apply them via
/// `edit_set_voxel_batched`, recording each `before` value in
/// `preview`. Caller is responsible for reverting first if a previous
/// preview existed.
fn apply_preview(
    world: &mut VoxelWorld,
    batch: &mut WorldEditBatch,
    face_cells: &[IVec3],
    normal: IVec3,
    voxel: Voxel,
    d: i32,
    preview: &mut AHashMap<IVec3, Voxel>,
) {
    if d == 0 {
        return;
    }
    if d > 0 {
        // Pull: extrude `voxel` outward along +normal.
        for &c in face_cells {
            for i in 1..=d {
                let pos = c + normal * i;
                if let Some((before, _)) =
                    world.edit_set_voxel_batched(pos.x, pos.y, pos.z, voxel, batch)
                {
                    // First time we touch this cell in the preview —
                    // remember the original world value.
                    preview.entry(pos).or_insert(before);
                }
            }
        }
    } else {
        // Push: delete |d| layers starting at the face cell itself,
        // working inward along -normal. This matches SketchUp's "drag
        // face into the solid" intuition.
        let k = -d;
        for &c in face_cells {
            for j in 0..k {
                let pos = c - normal * j;
                if let Some((before, _)) =
                    world.edit_set_voxel_batched(pos.x, pos.y, pos.z, AIR, batch)
                {
                    preview.entry(pos).or_insert(before);
                }
            }
        }
    }
}

/// Commit on LMB-up: snapshot final world values into a history batch
/// and clear drag state. The preview voxels stay in place — the world
/// already reflects the final state.
pub fn end_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mut world: ResMut<VoxelWorld>,
    mut drag: ResMut<PushPullDrag>,
    mut history: ResMut<BuilderHistory>,
    mut state: ResMut<SculptState>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
) {
    if !drag.active {
        return;
    }
    if !pushpull_commit_requested(
        drag.click_finish,
        mouse.just_pressed(MouseButton::Left),
        mouse.just_released(MouseButton::Left),
    ) {
        return;
    }

    if drag.preview.is_empty() || drag.last_d == 0 {
        if !drag.click_finish && drag.motion_len <= TAP_CLEANUP_MAX_MOTION_PX {
            if commit_tap_cleanup(
                &mut world,
                &mut drag,
                &mut history,
                &mut state,
                &mut toolbelt,
                &mut tool_controller,
            ) {
                gesture_lock.release(PUSH_PULL_OWNER);
                return;
            }
        }
        drag.clear();
        gesture_lock.release(PUSH_PULL_OWNER);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        state.status = "Sculpt: cancelled.".into();
        toolbelt.status =
            "Push Pull Face cancelled: no extrusion distance. Tap small leftovers to clean them."
                .into();
        return;
    }

    let label = if drag.last_d > 0 {
        format!("Pull {} ({} Zellen)", drag.last_d, drag.face_cells.len())
    } else {
        format!(
            "Push {} ({} Zellen)",
            drag.last_d.abs(),
            drag.face_cells.len()
        )
    };

    let mut changes: Vec<(IVec3, Voxel, Voxel)> = Vec::with_capacity(drag.preview.len());
    for (&pos, &before) in drag.preview.iter() {
        let after = world.voxel_at(pos.x, pos.y, pos.z);
        if before != after {
            changes.push((pos, before, after));
        }
    }
    let n = changes.len();
    let changed_cells: Vec<_> = changes.iter().map(|(pos, _, _)| *pos).collect();
    history.record_external(&label, changes);
    if n > 0 {
        match record_pushpull_semantics(&drag, &mut sketch_doc) {
            Ok(records) => register_pushpull_semantic_links(
                &drag,
                &changed_cells,
                sketch_doc.active_context(),
                &records,
                &mut sketch_links,
            ),
            Err(error) => {
                warn!("sketch model: could not record push/pull semantic extrusion: {error}");
            }
        }
        tool_controller.begin_transaction(label.clone());
        let _ = tool_controller.commit_transaction();
    }
    state.status = format!("{label}: {n} Voxel committed.");
    toolbelt.status = state.status.clone();
    drag.clear();
    gesture_lock.release(PUSH_PULL_OWNER);
}

fn record_pushpull_semantics(
    drag: &PushPullDrag,
    sketch_doc: &mut crate::sketch_model::SketchDocument,
) -> Result<
    Vec<(
        crate::sketch_model::SketchId,
        crate::sketch_model::SketchVoxelLinkRole,
    )>,
    crate::sketch_model::SketchModelError,
> {
    if drag.face_cells.is_empty() || drag.last_d == 0 {
        return Ok(Vec::new());
    }
    let Some((origin, axis_u, axis_v)) = pushpull_semantic_face_axes(drag) else {
        return Ok(Vec::new());
    };
    let (face, extrusion) = sketch_doc.record_push_pull_face(
        sketch_doc.active_context(),
        origin,
        axis_u,
        axis_v,
        drag.last_d as f32,
        "Push/Pull semantic",
    )?;
    Ok(vec![
        (face, crate::sketch_model::SketchVoxelLinkRole::Face),
        (
            extrusion,
            crate::sketch_model::SketchVoxelLinkRole::Extrusion,
        ),
    ])
}

fn register_pushpull_semantic_links(
    drag: &PushPullDrag,
    changed_cells: &[IVec3],
    context: crate::sketch_model::SketchId,
    records: &[(
        crate::sketch_model::SketchId,
        crate::sketch_model::SketchVoxelLinkRole,
    )],
    sketch_links: &mut crate::sketch_model::SketchVoxelLinkIndex,
) {
    for (entity, role) in records {
        let link = crate::sketch_model::SketchVoxelLink::new(*entity, context, *role);
        match role {
            crate::sketch_model::SketchVoxelLinkRole::Face => {
                sketch_links.link_face_cells(drag.face_cells.iter().copied(), drag.normal, link);
            }
            crate::sketch_model::SketchVoxelLinkRole::Extrusion => {
                sketch_links.link_cells(changed_cells.iter().copied(), link);
            }
            _ => {
                sketch_links.link_cells(changed_cells.iter().copied(), link);
            }
        }
    }
}

fn pushpull_semantic_face_axes(drag: &PushPullDrag) -> Option<(Vec3, Vec3, Vec3)> {
    let normal_axis = normal_axis(drag.normal)?;
    let plane_axes: Vec<_> = (0..3).filter(|axis| *axis != normal_axis).collect();
    let u_axis = plane_axes[0];
    let v_axis = plane_axes[1];
    let normal_sign = ivec_component(drag.normal, normal_axis).signum();

    let first = *drag.face_cells.first()?;
    let mut min = first;
    let mut max = first;
    for cell in &drag.face_cells {
        min = min.min(*cell);
        max = max.max(*cell);
    }

    let plane = if normal_sign > 0 {
        ivec_component(max, normal_axis) as f32 + 1.0
    } else {
        ivec_component(min, normal_axis) as f32
    };
    let u0 = ivec_component(min, u_axis) as f32;
    let v0 = ivec_component(min, v_axis) as f32;
    let u_len = (ivec_component(max, u_axis) - ivec_component(min, u_axis) + 1) as f32;
    let v_len = (ivec_component(max, v_axis) - ivec_component(min, v_axis) + 1) as f32;

    let origin = face_point(normal_axis, plane, u_axis, u0, v_axis, v0);
    let mut axis_u = Vec3::ZERO;
    axis_u = set_vec_component(axis_u, u_axis, u_len);
    let mut axis_v = Vec3::ZERO;
    axis_v = set_vec_component(axis_v, v_axis, v_len);

    if axis_u.cross(axis_v).dot(drag.normal.as_vec3()) < 0.0 {
        std::mem::swap(&mut axis_u, &mut axis_v);
    }
    Some((origin, axis_u, axis_v))
}

fn commit_tap_cleanup(
    world: &mut VoxelWorld,
    drag: &mut PushPullDrag,
    history: &mut BuilderHistory,
    state: &mut SculptState,
    toolbelt: &mut ToolbeltState,
    tool_controller: &mut crate::sketch_model::ToolController,
) -> bool {
    if drag.face_cells.is_empty() {
        return false;
    }
    if drag.face_cells.len() > TAP_CLEANUP_FACE_CAP {
        state.status = "Sculpt: tap cleanup skipped; face is too large.".into();
        toolbelt.status = format!(
            "Tap cleanup protects large faces: {} cells > {}. Use Push/Pull click-move-click to edit this surface.",
            drag.face_cells.len(),
            TAP_CLEANUP_FACE_CAP
        );
        return false;
    }

    let mut batch = WorldEditBatch::default();
    let drag_ref = &mut *drag;
    apply_preview(
        world,
        &mut batch,
        &drag_ref.face_cells,
        drag_ref.normal,
        drag_ref.voxel,
        -1,
        &mut drag_ref.preview,
    );
    world.finish_edit_batch(batch);

    let mut changes: Vec<(IVec3, Voxel, Voxel)> = Vec::with_capacity(drag.preview.len());
    for (&pos, &before) in drag.preview.iter() {
        let after = world.voxel_at(pos.x, pos.y, pos.z);
        if before != after {
            changes.push((pos, before, after));
        }
    }
    if changes.is_empty() {
        drag.clear();
        state.status = "Sculpt: tap cleanup found nothing to remove.".into();
        toolbelt.status = state.status.clone();
        return true;
    }

    let changed = changes.len();
    let label = format!("Tap cleanup {} cells", changed);
    history.record_external(label.clone(), changes);
    tool_controller.begin_transaction(label);
    let _ = tool_controller.commit_transaction();
    drag.clear();
    state.status = format!("Tap cleanup removed {changed} leftover cells. Ctrl+Z undo.");
    toolbelt.status = state.status.clone();
    true
}

/// Universal Ctrl+Z / Ctrl+Y handler for the sculpt timeline. Runs even
/// when the editor (F3) panel is closed, so SketchUp-style undo Just
/// Works. Routes through [`BuilderHistory::pop_undo`] / `pop_redo` so
/// every batch — Classic builder, sculpt, future tools — shares one
/// undo stack.
pub fn universal_undo_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    mut state: ResMut<SculptState>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut mode: ResMut<ModeContext>,
    drag: Res<PushPullDrag>,
) {
    // Don't undo mid-drag — the preview owns the world's current state.
    if drag.active {
        return;
    }
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    if !ctrl {
        return;
    }
    if keys.just_pressed(KeyCode::KeyZ) {
        state.status = match history.pop_undo(&mut world) {
            Some((label, n)) => format!("Undo '{label}': {n} Voxel."),
            None => "Undo: nichts vorhanden.".into(),
        };
        toolbelt.status = state.status.clone();
        mode.status = state.status.clone();
    } else if keys.just_pressed(KeyCode::KeyY) || keys.just_pressed(KeyCode::KeyR) {
        state.status = match history.pop_redo(&mut world) {
            Some((label, n)) => format!("Redo '{label}': {n} Voxel."),
            None => "Redo: nichts vorhanden.".into(),
        };
        toolbelt.status = state.status.clone();
        mode.status = state.status.clone();
    }
}

/// Draw the boundary outline of the current hover face. We trace just
/// the cells that have at least one in-plane neighbour outside the face
/// set, so the highlight is a clean silhouette rather than a grid of
/// per-cell rectangles.
pub fn draw_face_gizmo(
    hover_face: Res<HoverFace>,
    drag: Res<PushPullDrag>,
    mut gizmos: Gizmos,
    time: Res<Time>,
) {
    let Some(face) = hover_face.0.as_ref() else {
        return;
    };
    if face.cells.is_empty() {
        return;
    }

    // Face-axis basis.
    let normal = face.normal.as_vec3();
    let (in1_iv, in2_iv) = if face.normal.x != 0 {
        (IVec3::Y, IVec3::Z)
    } else if face.normal.y != 0 {
        (IVec3::X, IVec3::Z)
    } else {
        (IVec3::X, IVec3::Y)
    };
    let in1 = in1_iv.as_vec3();
    let in2 = in2_iv.as_vec3();

    // Cells set for O(1) neighbour lookups.
    let cells_set: AHashSet<IVec3> = face.cells.iter().copied().collect();

    // Pulse colour: dragging = solid amber-ish, hover = breathing cyan.
    let base = if drag.active {
        Color::srgb(1.0, 0.78, 0.20)
    } else {
        let t = time.elapsed_seconds();
        let pulse = 0.6 + 0.4 * (t * 2.4).sin().abs();
        // Theme cyan (#32D7FF) → linear-ish floats.
        Color::srgb(0.20 * pulse, 0.85 * pulse, 1.0 * pulse)
    };

    // Push the outline a hair off the surface so it doesn't z-fight
    // with the chunk mesh.
    let outset = 0.02;

    for &c in &face.cells {
        let center = c.as_vec3() + Vec3::splat(0.5) + 0.5 * normal + outset * normal;

        // +in1 edge
        if !cells_set.contains(&(c + in1_iv)) {
            let a = center + 0.5 * in1 - 0.5 * in2;
            let b = center + 0.5 * in1 + 0.5 * in2;
            gizmos.line(a, b, base);
        }
        // -in1 edge
        if !cells_set.contains(&(c - in1_iv)) {
            let a = center - 0.5 * in1 - 0.5 * in2;
            let b = center - 0.5 * in1 + 0.5 * in2;
            gizmos.line(a, b, base);
        }
        // +in2 edge
        if !cells_set.contains(&(c + in2_iv)) {
            let a = center - 0.5 * in1 + 0.5 * in2;
            let b = center + 0.5 * in1 + 0.5 * in2;
            gizmos.line(a, b, base);
        }
        // -in2 edge
        if !cells_set.contains(&(c - in2_iv)) {
            let a = center - 0.5 * in1 - 0.5 * in2;
            let b = center + 0.5 * in1 - 0.5 * in2;
            gizmos.line(a, b, base);
        }
    }
}

pub fn draw_reference_gizmo(
    state: Res<SculptState>,
    reference: Res<PushPullReference>,
    drag: Res<PushPullDrag>,
    mut gizmos: Gizmos,
    time: Res<Time>,
) {
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let a_color = Color::srgb(1.0, 0.80, 0.15);
    let b_color = Color::srgb(0.15, 1.0, 0.55);
    let line_color = Color::srgb(0.15, 0.95 * pulse, 1.0);

    if let Some(a) = reference.start {
        let face_center = a.cell.as_vec3() + Vec3::splat(0.5) + a.normal.as_vec3() * 0.52;
        gizmos.cuboid(
            Transform::from_translation(a.point).with_scale(Vec3::splat(REFERENCE_POINT_SIZE)),
            a_color,
        );
        gizmos.line(face_center, a.point, a_color);
        gizmos.line(a.point, a.point + a.normal.as_vec3() * 0.45, a_color);
    }
    if let Some(b) = reference.end {
        let face_center = b.cell.as_vec3() + Vec3::splat(0.5) + b.normal.as_vec3() * 0.52;
        gizmos.cuboid(
            Transform::from_translation(b.point).with_scale(Vec3::splat(REFERENCE_POINT_SIZE)),
            b_color,
        );
        gizmos.line(face_center, b.point, b_color);
        gizmos.line(b.point, b.point + b.normal.as_vec3() * 0.45, b_color);
    }
    if let (Some(a), Some(b)) = (reference.start, reference.end) {
        gizmos.line(a.point, b.point, line_color);
    }

    if !drag.active {
        if let Some(hit) = state.hover {
            if let Some(points) = inference_candidates(hit.voxel, hit.normal()) {
                for candidate in points {
                    let color = match candidate.kind {
                        InferenceSnapKind::Endpoint => Color::srgb(1.0, 0.78, 0.18),
                        InferenceSnapKind::Midpoint => Color::srgb(0.10, 1.0, 0.55),
                        InferenceSnapKind::FaceCenter => Color::srgb(0.18, 0.85, 1.0),
                    };
                    let size = match candidate.kind {
                        InferenceSnapKind::Endpoint => HOVER_POINT_SIZE,
                        InferenceSnapKind::Midpoint => HOVER_POINT_SIZE * 0.85,
                        InferenceSnapKind::FaceCenter => HOVER_POINT_SIZE * 1.1,
                    };
                    gizmos.cuboid(
                        Transform::from_translation(candidate.point).with_scale(Vec3::splat(size)),
                        color,
                    );
                }
            }
        }
    }

    if drag.active {
        if let Some(d) = drag.reference_d {
            let a = drag.anchor_world;
            let b = a + drag.normal.as_vec3() * d as f32;
            gizmos.line(a, b, Color::srgb(1.0, 0.55, 0.08));
            gizmos.cuboid(
                Transform::from_translation(b).with_scale(Vec3::splat(REFERENCE_POINT_SIZE * 0.8)),
                Color::srgb(1.0, 0.55, 0.08),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockType;

    #[test]
    fn preview_distance_cap_scales_with_face_size() {
        assert_eq!(preview_distance_cap(1), 128);
        assert!(preview_distance_cap(16_384) < 32);
        assert!(preview_distance_cap(1_000_000) >= 1);
    }

    #[test]
    fn apply_preview_pulls_outward_layers() {
        let mut world = VoxelWorld::new();
        let stone = Voxel::from(BlockType::Stone);
        let mut seed = WorldEditBatch::default();
        world.edit_set_voxel_batched(0, 0, 0, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut batch = WorldEditBatch::default();
        let mut preview = AHashMap::new();
        apply_preview(
            &mut world,
            &mut batch,
            &[IVec3::ZERO],
            IVec3::Y,
            stone,
            2,
            &mut preview,
        );
        world.finish_edit_batch(batch);

        assert_eq!(world.voxel_at(0, 1, 0), stone);
        assert_eq!(world.voxel_at(0, 2, 0), stone);
        assert_eq!(preview.get(&IVec3::new(0, 1, 0)).copied(), Some(AIR));
        assert_eq!(preview.get(&IVec3::new(0, 2, 0)).copied(), Some(AIR));
    }

    #[test]
    fn apply_preview_pushes_inward_layers_to_air() {
        let mut world = VoxelWorld::new();
        let stone = Voxel::from(BlockType::Stone);
        let mut seed = WorldEditBatch::default();
        world.edit_set_voxel_batched(0, 0, 0, stone, &mut seed);
        world.edit_set_voxel_batched(0, -1, 0, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut batch = WorldEditBatch::default();
        let mut preview = AHashMap::new();
        apply_preview(
            &mut world,
            &mut batch,
            &[IVec3::ZERO],
            IVec3::Y,
            stone,
            -2,
            &mut preview,
        );
        world.finish_edit_batch(batch);

        assert_eq!(world.voxel_at(0, 0, 0), AIR);
        assert_eq!(world.voxel_at(0, -1, 0), AIR);
        assert_eq!(preview.get(&IVec3::new(0, 0, 0)).copied(), Some(stone));
        assert_eq!(preview.get(&IVec3::new(0, -1, 0)).copied(), Some(stone));
    }

    #[test]
    fn pushpull_commit_records_semantic_extrusion_with_undo() {
        let stone = Voxel::from(BlockType::Stone);
        let drag = PushPullDrag {
            active: true,
            face_cells: vec![IVec3::ZERO, IVec3::X],
            normal: IVec3::Y,
            voxel: stone,
            anchor_world: Vec3::ZERO,
            screen_dir: Vec2::X,
            motion_accum: Vec2::ZERO,
            motion_len: 24.0,
            click_finish: true,
            tool_generation: 0,
            last_d: 3,
            reference_d: None,
            preview: AHashMap::new(),
        };
        let mut sketch_doc = crate::sketch_model::SketchDocument::new();

        let records = record_pushpull_semantics(&drag, &mut sketch_doc)
            .expect("semantic push/pull should record");
        assert_eq!(records.len(), 2);
        let extrusion = records
            .iter()
            .find_map(|(entity, role)| {
                (*role == crate::sketch_model::SketchVoxelLinkRole::Extrusion).then_some(*entity)
            })
            .expect("positive push/pull distance should create extrusion");

        let ids = sketch_doc
            .context(sketch_doc.active_context())
            .unwrap()
            .entities
            .clone();
        assert_eq!(ids.len(), 2);
        let source_face = ids[0];
        assert_eq!(ids[1], extrusion);
        assert!(records.iter().any(|(entity, role)| {
            *entity == source_face && *role == crate::sketch_model::SketchVoxelLinkRole::Face
        }));
        assert!(matches!(
            &sketch_doc.entity(source_face).unwrap().kind,
            crate::sketch_model::SketchEntityKind::Face { vertices, normal }
                if vertices[0] == Vec3::new(0.0, 1.0, 0.0)
                    && vertices[2] == Vec3::new(2.0, 1.0, 1.0)
                    && *normal == Vec3::Y
        ));
        assert!(matches!(
            &sketch_doc.entity(extrusion).unwrap().kind,
            crate::sketch_model::SketchEntityKind::PushPullExtrusion { source_face: got, depth, .. }
                if *got == source_face && (*depth - 3.0).abs() < f32::EPSILON
        ));
        let mut sketch_links = crate::sketch_model::SketchVoxelLinkIndex::default();
        register_pushpull_semantic_links(
            &drag,
            &[IVec3::new(0, 1, 0), IVec3::new(1, 1, 0)],
            sketch_doc.active_context(),
            &records,
            &mut sketch_links,
        );
        assert!(sketch_links
            .links_for_face(IVec3::ZERO, IVec3::Y)
            .iter()
            .any(|link| {
                link.entity == source_face
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Face
            }));
        assert!(sketch_links
            .links_for_cell(IVec3::new(0, 1, 0))
            .iter()
            .any(|link| {
                link.entity == extrusion
                    && link.role == crate::sketch_model::SketchVoxelLinkRole::Extrusion
            }));

        let undone = sketch_doc
            .undo_last()
            .expect("undo pushpull semantic batch");
        assert_eq!(undone.label, "Push/Pull semantic");
        assert_eq!(undone.entity_count, 2);
        assert!(ids.iter().all(|id| sketch_doc.entity(*id).is_none()));

        let redone = sketch_doc
            .redo_last()
            .expect("redo pushpull semantic batch");
        assert_eq!(redone.entity_count, 2);
        assert!(ids.iter().all(|id| sketch_doc.entity(*id).is_some()));
    }

    #[test]
    fn semantic_hover_hit_uses_voxel_link_index() {
        let stone = Voxel::from(BlockType::Stone);
        let face = crate::sketch_model::SketchId::new_for_test(20);
        let context = crate::sketch_model::SketchId::new_for_test(2);
        let cell = IVec3::new(4, 5, 6);
        let normal = IVec3::Y;
        let mut links = crate::sketch_model::SketchVoxelLinkIndex::default();
        links.link_face_cell(
            cell,
            normal,
            crate::sketch_model::SketchVoxelLink::new(
                face,
                context,
                crate::sketch_model::SketchVoxelLinkRole::Face,
            ),
        );
        let region = FaceRegion {
            cells: vec![cell],
            voxel: stone,
            normal,
            clipped: false,
        };

        let hit = semantic_hover_hit_from_region(&region, &links).expect("semantic hover hit");

        assert_eq!(hit.entity, face);
        assert_eq!(hit.kind, crate::sketch_model::HitKind::Face);
        assert_eq!(hit.world_point, Vec3::new(4.5, 5.5, 6.5));
        assert_eq!(hit.normal, Some(Vec3::Y));
    }

    #[test]
    fn semantic_select_click_uses_hover_hit_and_respects_additive_selection() {
        let first = crate::sketch_model::SketchId::new_for_test(10);
        let second = crate::sketch_model::SketchId::new_for_test(11);
        let mut controller = crate::sketch_model::ToolController::default();
        let first_hit = crate::sketch_model::HitRecord::new(
            first,
            [],
            crate::sketch_model::HitKind::Face,
            Vec3::ZERO,
            0.0,
        );
        let second_hit = crate::sketch_model::HitRecord::new(
            second,
            [],
            crate::sketch_model::HitKind::Face,
            Vec3::X,
            0.0,
        );

        assert_eq!(
            apply_semantic_selection_click(&mut controller, Some(&first_hit), false),
            crate::sketch_model::SelectionUpdate::Selected(first)
        );
        assert_eq!(controller.selection().ordered(), &[first]);

        assert_eq!(
            apply_semantic_selection_click(&mut controller, Some(&second_hit), true),
            crate::sketch_model::SelectionUpdate::Selected(second)
        );
        assert_eq!(controller.selection().ordered(), &[first, second]);

        assert_eq!(
            apply_semantic_selection_click(&mut controller, None, false),
            crate::sketch_model::SelectionUpdate::Cleared
        );
        assert!(controller.selection().is_empty());
        assert_eq!(
            apply_semantic_selection_click(&mut controller, None, true),
            crate::sketch_model::SelectionUpdate::Ignored
        );
    }

    #[test]
    fn semantic_select_input_only_runs_for_mouse_first_select_surface() {
        assert!(semantic_select_input_active(
            true,
            Some(ToolbeltTool::Navigate),
            crate::sketch_model::EditorToolId::Rectangle,
            true,
            false
        ));
        assert!(semantic_select_input_active(
            true,
            Some(ToolbeltTool::DrawRect),
            crate::sketch_model::EditorToolId::Select,
            true,
            false
        ));
        assert!(!semantic_select_input_active(
            true,
            Some(ToolbeltTool::DrawRect),
            crate::sketch_model::EditorToolId::Rectangle,
            true,
            false
        ));
        assert!(!semantic_select_input_active(
            true,
            Some(ToolbeltTool::Navigate),
            crate::sketch_model::EditorToolId::Select,
            true,
            true
        ));
        assert!(!semantic_select_input_active(
            false,
            Some(ToolbeltTool::Navigate),
            crate::sketch_model::EditorToolId::Select,
            true,
            false
        ));
    }

    #[test]
    fn tap_cleanup_removes_small_leftover_face_layer() {
        let mut world = VoxelWorld::new();
        let stone = Voxel::from(BlockType::Stone);
        let mut seed = WorldEditBatch::default();
        world.edit_set_voxel_batched(0, 0, 0, stone, &mut seed);
        world.finish_edit_batch(seed);

        let mut drag = PushPullDrag {
            active: true,
            face_cells: vec![IVec3::ZERO],
            normal: IVec3::Y,
            voxel: stone,
            anchor_world: Vec3::ZERO,
            screen_dir: Vec2::X,
            motion_accum: Vec2::ZERO,
            motion_len: 0.0,
            click_finish: false,
            tool_generation: 0,
            last_d: 0,
            reference_d: None,
            preview: AHashMap::new(),
        };
        let mut history = BuilderHistory::default();
        let mut state = SculptState::default();
        let mut toolbelt = ToolbeltState::default();
        let mut tool_controller = crate::sketch_model::ToolController::default();

        assert!(commit_tap_cleanup(
            &mut world,
            &mut drag,
            &mut history,
            &mut state,
            &mut toolbelt,
            &mut tool_controller
        ));
        assert_eq!(world.voxel_at(0, 0, 0), AIR);
        assert_eq!(history.undo_len(), 1);
        assert_eq!(
            tool_controller.last_transaction_label(),
            Some("Tap cleanup 1 cells")
        );
    }

    #[test]
    fn reference_distance_uses_selected_face_normal() {
        let reference = PushPullReference {
            start: Some(PushPullReferencePoint {
                point: Vec3::ZERO,
                normal: IVec3::Z,
                cell: IVec3::ZERO,
                kind: InferenceSnapKind::Endpoint,
            }),
            end: Some(PushPullReferencePoint {
                point: Vec3::new(0.0, 0.0, 12.0),
                normal: IVec3::Z,
                cell: IVec3::Z,
                kind: InferenceSnapKind::Endpoint,
            }),
        };
        assert_eq!(reference.distance_for(IVec3::Z), Some(12));
        assert_eq!(reference.distance_for(-IVec3::Z), Some(-12));
        assert_eq!(reference.distance_for(IVec3::X), None);
    }

    #[test]
    fn reference_point_snaps_to_nearest_face_corner() {
        let origin = Vec3::new(0.02, 0.02, -5.0);
        let dir = Vec3::new(0.01, 0.01, 1.0).normalize();
        let point = reference_point_from_hit(origin, dir, IVec3::ZERO, IVec3::new(0, 0, -1))
            .expect("ray should intersect the front face");
        assert_eq!(point.normal, -IVec3::Z);
        assert!(matches!(point.kind, InferenceSnapKind::Endpoint));
        assert_eq!(point.point, Vec3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn reference_point_can_snap_to_face_center() {
        let origin = Vec3::new(0.50, 0.50, -5.0);
        let dir = Vec3::Z;
        let point = reference_point_from_hit(origin, dir, IVec3::ZERO, IVec3::new(0, 0, -1))
            .expect("ray should intersect the front face");
        assert!(matches!(point.kind, InferenceSnapKind::FaceCenter));
        assert_eq!(point.point, Vec3::new(0.5, 0.5, 0.0));
    }

    #[test]
    fn pushpull_reference_points_use_shared_inference_kinds_and_tooltips() {
        assert_eq!(
            pushpull_inference_kind(InferenceSnapKind::Endpoint),
            crate::sketch_model::InferenceKind::Endpoint
        );
        assert_eq!(
            pushpull_inference_kind(InferenceSnapKind::Midpoint).tooltip(),
            "Midpoint"
        );
        assert_eq!(
            pushpull_inference_kind(InferenceSnapKind::FaceCenter).tooltip(),
            "Face center"
        );
    }

    #[test]
    fn right_mouse_does_not_cancel_active_pushpull_drag() {
        assert!(
            !pushpull_drag_cancel_requested(false, true),
            "RMB during Push/Pull should orbit the camera, not revert the preview"
        );
        assert!(pushpull_drag_cancel_requested(true, false));
    }

    #[test]
    fn right_mouse_orbit_freezes_pushpull_depth_motion() {
        assert!(
            !pushpull_drag_accepts_motion(true),
            "RMB orbit should move the camera without also changing Push/Pull depth"
        );
        assert!(pushpull_drag_accepts_motion(false));
    }

    #[test]
    fn click_finish_pushpull_ignores_first_release_and_commits_on_second_click() {
        assert!(!pushpull_commit_requested(true, false, true));
        assert!(pushpull_commit_requested(true, true, false));
        assert!(pushpull_commit_requested(false, false, true));
        assert!(!pushpull_commit_requested(false, true, false));
    }

    #[test]
    fn active_pushpull_preview_cancels_when_toolbox_selection_changes() {
        let mut drag = PushPullDrag::default();
        drag.active = true;
        drag.tool_generation = 7;

        assert!(pushpull_should_cancel_for_tool_selection(&drag, 8));
        assert!(!pushpull_should_cancel_for_tool_selection(&drag, 7));
    }
}

// Keep a couple of imports referenced even when warnings are denied —
// `voxel_is_solid` is only used inside `face.rs` but Rust requires the
// public re-export here to stay sound; same for HoverHit.
#[allow(dead_code)]
fn _silence_unused(v: Voxel, h: HoverHit) -> (bool, IVec3) {
    (voxel_is_solid(v), h.normal())
}
