use std::collections::{HashMap, HashSet};

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::{Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::mode::{BuildGestureLock, ModeContext};
use crate::sculpt::state::SculptState;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

const MOVE_OWNER: &str = "Sketch Move";
#[cfg(test)]
const MOVE_PIXELS_PER_VOXEL: f32 = 18.0;
const MOVE_DELTA_LIMIT: i32 = 256;

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

pub fn begin_move_drag(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    tool_controller: Res<crate::sketch_model::ToolController>,
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
    drag.tool_generation = toolbelt.selection_generation();
    gesture_lock.lock(MOVE_OWNER);
    toolbelt.status =
        "Move: hover an endpoint, midpoint, face center, or voxel target. Ctrl copies; optional arrows lock X/Y/Z."
            .into();
}

pub fn update_move_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mut motion_evr: EventReader<MouseMotion>,
    mut drag: ResMut<SemanticMoveDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
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
        move_should_cancel_for_tool_selection(&drag, toolbelt.selection_generation());
    if move_drag_should_cancel(escape_pressed, tool_changed) {
        drag.clear();
        gesture_lock.release(MOVE_OWNER);
        tool_controller.cancel_active_operation(crate::sketch_model::EditorCancelReason::Escape);
        toolbelt.status = "Move cancelled.".into();
        motion_evr.clear();
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
    let snap_delta = move_delta_from_snap_target(reference_delta, hover_delta);
    motion_evr.clear();
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
                "{action}: hover a real endpoint, midpoint, face center, or voxel target; no unsnapped mouse drift."
            );
        }
        return;
    };
    let next_hover_snap_active = true;
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
            "waiting"
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

fn move_tool_active(
    mode: &ModeContext,
    tool_controller: &crate::sketch_model::ToolController,
) -> bool {
    mode.is_build_live()
        && (mode.build_tool() == Some(ToolbeltTool::TransformMove)
            || tool_controller.active_tool() == crate::sketch_model::EditorToolId::Move)
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
) -> Option<IVec3> {
    reference_delta.or(hover_delta)
}

fn apply_move_axis_lock(delta: IVec3, axis_lock: Option<MoveAxisLock>) -> IVec3 {
    match axis_lock {
        Some(MoveAxisLock::X) => IVec3::new(delta.x, 0, 0),
        Some(MoveAxisLock::Y) => IVec3::new(0, delta.y, 0),
        Some(MoveAxisLock::Z) => IVec3::new(0, 0, delta.z),
        None => delta,
    }
}

#[cfg(test)]
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

#[cfg(test)]
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
    cells.into_iter().collect()
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

    let sources = selection_source_voxels(world, sketch_links, selection);
    if sources.is_empty() {
        return 0;
    }

    let mut destinations = HashMap::<IVec3, Voxel>::new();
    for (cell, voxel) in &sources {
        destinations.insert(*cell + delta, *voxel);
    }
    let destination_cells: HashSet<IVec3> = destinations.keys().copied().collect();

    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::with_capacity(sources.len() + destinations.len());
    for cell in sources.keys().copied() {
        if destination_cells.contains(&cell) {
            continue;
        }
        if let Some((before, after)) =
            world.edit_set_voxel_batched(cell.x, cell.y, cell.z, AIR, &mut batch)
        {
            changes.push((cell, before, after));
        }
    }
    for (cell, voxel) in destinations {
        if let Some((before, after)) =
            world.edit_set_voxel_batched(cell.x, cell.y, cell.z, voxel, &mut batch)
        {
            changes.push((cell, before, after));
        }
    }
    world.finish_edit_batch(batch);
    let moved = sources.len();
    history.record_external(label.to_string(), changes);
    let _ = sketch_doc.move_selection(selection, delta.as_vec3(), label.to_string());
    sketch_links.translate_entities(selection.ordered().iter().copied(), delta);
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
    history.record_external(label, changes);
    link_linear_array_copies(sketch_links, selection, &copied_entities, delta, copy_count);
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
    use bevy::prelude::*;

    use crate::blocks::{BlockType, AIR};
    use crate::builder::BuilderHistory;
    use crate::sketch_model::{
        SelectionSet, SketchDocument, SketchVoxelLink, SketchVoxelLinkIndex, SketchVoxelLinkRole,
    };
    use crate::world::{VoxelWorld, WorldEditBatch};

    use super::{
        commit_selection_voxel_copy_array, commit_selection_voxel_move, move_copy_count_from_key,
        move_delta_from_hover_cell, move_delta_from_reference_points, move_delta_from_snap_target,
        move_drag_should_cancel, move_grip_cell, move_grip_reference_point,
        move_reference_point_from_hit, snapped_move_delta, MoveAxisLock,
    };

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
    fn move_delta_from_snap_target_never_invents_mouse_motion_delta() {
        assert_eq!(
            move_delta_from_snap_target(Some(IVec3::new(5, 0, 0)), Some(IVec3::new(1, 0, 0))),
            Some(IVec3::new(5, 0, 0)),
            "Exact inference targets should win over coarse voxel hover."
        );
        assert_eq!(
            move_delta_from_snap_target(None, Some(IVec3::new(0, 3, 0))),
            Some(IVec3::new(0, 3, 0)),
            "Voxel hover is still valid when no semantic endpoint/midpoint is hit."
        );
        assert_eq!(
            move_delta_from_snap_target(None, None),
            None,
            "Move must wait for a real snap target instead of falling back to raw mouse pixels."
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
}
