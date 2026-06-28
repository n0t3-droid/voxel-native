use std::collections::{HashMap, HashSet};

use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;

use crate::blocks::{Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::mode::{BuildGestureLock, ModeContext};
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

const MOVE_OWNER: &str = "Sketch Move";
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
    sketch_links: Res<crate::sketch_model::SketchVoxelLinkIndex>,
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
    drag.selection = selection;
    drag.cells = cells;
    drag.tool_generation = toolbelt.selection_generation();
    gesture_lock.lock(MOVE_OWNER);
    toolbelt.status =
        "Move: drag selected voxels. ArrowRight=X, ArrowUp=height, ArrowLeft=Z, ArrowDown=free."
            .into();
}

pub fn update_move_drag(
    keys: Res<ButtonInput<KeyCode>>,
    mut motion_evr: EventReader<MouseMotion>,
    mut drag: ResMut<SemanticMoveDrag>,
    mut gesture_lock: ResMut<BuildGestureLock>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
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

    for ev in motion_evr.read() {
        drag.motion += ev.delta;
    }
    let next_delta = snapped_move_delta(drag.motion, drag.axis_lock);
    if next_delta != drag.delta {
        drag.delta = next_delta;
        let axis = drag.axis_lock.map(MoveAxisLock::label).unwrap_or("free");
        toolbelt.status = format!(
            "Move {axis}: {} cells selected, delta ({}, {}, {}). Release to commit.",
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
    drag.clear();
    gesture_lock.release(MOVE_OWNER);
}

pub fn draw_move_gizmo(drag: Res<SemanticMoveDrag>, mut gizmos: Gizmos, time: Res<Time>) {
    if !drag.active || drag.cells.is_empty() {
        return;
    }
    let pulse = 0.55 + 0.45 * (time.elapsed_seconds() * 5.0).sin().abs();
    let current = selection_bounds(&drag.cells, IVec3::ZERO);
    let target = selection_bounds(&drag.cells, drag.delta);
    if let Some((center, scale)) = current {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.04)),
            Color::srgba(0.20, 0.90, 1.00, 0.35),
        );
    }
    if let Some((center, scale)) = target {
        gizmos.cuboid(
            Transform::from_translation(center).with_scale(scale + Vec3::splat(0.14)),
            Color::srgba(1.00, 0.78, 0.18, 0.65 + pulse * 0.25),
        );
    }
    if drag.delta != IVec3::ZERO {
        let from = drag.cells[0].as_vec3() + Vec3::splat(0.5);
        let to = from + drag.delta.as_vec3();
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
    let mut sources = HashMap::<IVec3, Voxel>::new();
    for cell in cells {
        let voxel = world.voxel_at(cell.x, cell.y, cell.z);
        if voxel != AIR {
            sources.insert(cell, voxel);
        }
    }
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
        commit_selection_voxel_move, move_drag_should_cancel, snapped_move_delta, MoveAxisLock,
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
}
