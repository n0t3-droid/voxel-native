//! World Builder / Editor — precision voxel construction inside the
//! running world.
//!
//! Lives alongside `editor.rs` (the F3 cyberpunk panel). `editor.rs` owns
//! the UI widgets for the BAUEN (builder) tab; this module owns the
//! *state machine* and the *execution* of build operations so that the
//! actual chunk edits and the async mesher interact cleanly.
//!
//! Features:
//!   * Any-size cuboid brush (1..=32 on each axis).
//!   * Place / Remove / Fill-between-points / Copy / Paste / Clear-to-air.
//!   * Capture A / B corner coordinates from the player's current
//!     position so you never have to guess world coordinates by hand.
//!   * Named prefabs saved to `./prefabs/<name>.ron` — load them later
//!     (same session or next launch) and paste them anywhere in the
//!     world.
//!   * Every edit marks affected chunks dirty so the existing async
//!     mesher picks the change up within a frame or two.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::blocks::{BlockType, Voxel, AIR};
use crate::director::UnifiedTelemetry;
use crate::player::Player;
use crate::sketch_model::{
    SketchDocument, SketchEditSummary, SketchId, SketchVoxelEntityLinkSnapshot,
    SketchVoxelLinkIndex,
};
use crate::world::{VoxelWorld, WorldEditBatch};

/// Root resource for the in-game builder. The editor UI reads/writes
/// this; `apply_build_actions` drains `pending` and mutates the world.
#[derive(Resource)]
pub struct BuilderState {
    pub block: BlockType,
    /// Cuboid brush size (blocks). 1 = single block. Anchored at its
    /// minus-corner so "place at feet" puts the brush above ground level.
    pub brush: IVec3,
    /// Corner A / B for Fill, Copy. Edited by the UI, populated by the
    /// "Capture from player" buttons.
    pub a: IVec3,
    pub b: IVec3,
    /// Paste anchor — minus-corner of where the clipboard lands next.
    pub paste_origin: IVec3,
    pub prefab_name: String,
    /// Actions queued by the UI this frame.
    pub pending: Vec<BuildAction>,
    live_flow: LiveBrushFlow,
    /// Last status line rendered beneath the builder UI.
    pub status: String,
}

impl Default for BuilderState {
    fn default() -> Self {
        Self {
            block: BlockType::Stone,
            brush: IVec3::new(1, 1, 1),
            a: IVec3::ZERO,
            b: IVec3::ZERO,
            paste_origin: IVec3::ZERO,
            prefab_name: "haus01".into(),
            pending: Vec::new(),
            live_flow: LiveBrushFlow::default(),
            status: "Bereit.".into(),
        }
    }
}

const LIVE_BRUSH_MIN_INTERVAL_SECONDS: f32 = 0.035;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveBrushAction {
    Place,
    Cut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LiveBrushStamp {
    action: LiveBrushAction,
    origin: IVec3,
    brush: IVec3,
    voxel: Voxel,
}

impl LiveBrushStamp {
    fn new(action: LiveBrushAction, origin: IVec3, brush: IVec3, voxel: Voxel) -> Self {
        Self {
            action,
            origin,
            brush,
            voxel,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct LiveBrushFlow {
    last: Option<LiveBrushStamp>,
    cooldown_s: f32,
}

fn live_brush_should_stamp(
    flow: &mut LiveBrushFlow,
    candidate: Option<LiveBrushStamp>,
    dt: f32,
    just_pressed: bool,
) -> bool {
    flow.cooldown_s = (flow.cooldown_s - dt.max(0.0)).max(0.0);
    let Some(candidate) = candidate else {
        flow.last = None;
        flow.cooldown_s = 0.0;
        return false;
    };

    if just_pressed {
        flow.last = Some(candidate);
        flow.cooldown_s = LIVE_BRUSH_MIN_INTERVAL_SECONDS;
        return true;
    }
    if flow.last == Some(candidate) {
        return false;
    }
    if flow.cooldown_s <= 0.0 {
        flow.last = Some(candidate);
        flow.cooldown_s = LIVE_BRUSH_MIN_INTERVAL_SECONDS;
        return true;
    }
    false
}

/// Copy/paste clipboard for an arbitrary axis-aligned region.
#[derive(Resource, Default, Clone)]
pub struct BuilderClipboard {
    pub size: IVec3,
    /// Voxels, laid out as `x + z*size.x + y*size.x*size.z`.
    pub voxels: Vec<Voxel>,
}

#[derive(Resource, Default)]
pub struct BuilderHistory {
    undo: Vec<EditHistoryBatch>,
    redo: Vec<EditHistoryBatch>,
}

/// Result of attempting to append one external edit to builder history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuilderHistoryRecordOutcome {
    Recorded { voxel_count: usize },
    SkippedNoChanges,
    RejectedTooManyChanges { voxel_count: usize, limit: usize },
}

impl BuilderHistory {
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// External edits (e.g. the SketchUp-style sculpt system in
    /// [`crate::sculpt`]) push their per-voxel changes onto the same
    /// undo stack via this method so Ctrl+Z works uniformly across
    /// builder tools. `changes` is a list of `(pos, before, after)`
    /// tuples. Duplicate positions are folded into a deterministic net
    /// change using the first `before` and final `after`; net no-ops are
    /// filtered so an empty drag does not clutter the stack.
    ///
    /// The redo stack is cleared on every new edit, matching common
    /// undo-history semantics. The per-batch cap protects low-end PCs
    /// from a single giant operation; the stack cap is intentionally
    /// high so mouse-first Sketch sessions do not lose early precise
    /// edits after only a few dozen strokes.
    pub fn record_external(
        &mut self,
        label: impl Into<String>,
        changes: Vec<(IVec3, Voxel, Voxel)>,
    ) {
        self.record_external_with_sketch_meta(label, changes, None);
    }

    /// Checked form of [`Self::record_external`].
    #[must_use]
    pub fn record_external_checked(
        &mut self,
        label: impl Into<String>,
        changes: Vec<(IVec3, Voxel, Voxel)>,
    ) -> BuilderHistoryRecordOutcome {
        self.record_external_with_sketch_meta_checked(label, changes, None)
    }

    pub fn record_external_with_sketch_meta(
        &mut self,
        label: impl Into<String>,
        changes: Vec<(IVec3, Voxel, Voxel)>,
        sketch_meta: Option<BuilderHistorySketchMeta>,
    ) {
        let label = label.into();
        let outcome =
            self.record_external_with_sketch_meta_checked(label.clone(), changes, sketch_meta);
        if let BuilderHistoryRecordOutcome::RejectedTooManyChanges { voxel_count, limit } = outcome
        {
            warn!(
                "Undo history rejected '{label}': {voxel_count} net voxel changes exceed the {limit}-change limit"
            );
        }
    }

    /// Records the complete canonical batch or rejects it without retaining
    /// voxel changes or semantic metadata.
    #[must_use]
    pub fn record_external_with_sketch_meta_checked(
        &mut self,
        label: impl Into<String>,
        changes: Vec<(IVec3, Voxel, Voxel)>,
        sketch_meta: Option<BuilderHistorySketchMeta>,
    ) -> BuilderHistoryRecordOutcome {
        let changes = canonicalize_voxel_changes(
            changes
                .into_iter()
                .map(|(pos, before, after)| VoxelChange { pos, before, after }),
        );
        let voxel_count = changes.len();
        if voxel_count > UNDO_CHANGE_LIMIT {
            self.redo.clear();
            return BuilderHistoryRecordOutcome::RejectedTooManyChanges {
                voxel_count,
                limit: UNDO_CHANGE_LIMIT,
            };
        }
        if changes.is_empty() && sketch_meta.is_none() {
            return BuilderHistoryRecordOutcome::SkippedNoChanges;
        }

        self.undo.push(EditHistoryBatch {
            label: label.into(),
            changes,
            sketch_meta,
        });
        if self.undo.len() > UNDO_STACK_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        BuilderHistoryRecordOutcome::Recorded { voxel_count }
    }

    /// Pop the most recent batch off the undo stack, apply its `before`
    /// values to `world`, push the batch onto the redo stack, and return
    /// the (label, voxel-count). Used by [`crate::sculpt`] for the
    /// universal Ctrl+Z handler. Returns `None` when the stack is empty.
    pub fn pop_undo(&mut self, world: &mut VoxelWorld) -> Option<(String, usize)> {
        self.pop_undo_detailed(world)
            .map(|step| (step.label, step.voxel_count))
    }

    pub fn pop_undo_detailed(&mut self, world: &mut VoxelWorld) -> Option<BuilderHistoryStep> {
        let batch = self.undo.pop()?;
        let n = apply_history_batch(world, &batch, true);
        let step = BuilderHistoryStep {
            label: batch.label.clone(),
            voxel_count: n,
            sketch_meta: batch.sketch_meta.clone(),
        };
        self.redo.push(batch);
        Some(step)
    }

    /// Mirror of [`Self::pop_undo`] for redo.
    pub fn pop_redo(&mut self, world: &mut VoxelWorld) -> Option<(String, usize)> {
        self.pop_redo_detailed(world)
            .map(|step| (step.label, step.voxel_count))
    }

    pub fn pop_redo_detailed(&mut self, world: &mut VoxelWorld) -> Option<BuilderHistoryStep> {
        let batch = self.redo.pop()?;
        let n = apply_history_batch(world, &batch, false);
        let step = BuilderHistoryStep {
            label: batch.label.clone(),
            voxel_count: n,
            sketch_meta: batch.sketch_meta.clone(),
        };
        self.undo.push(batch);
        Some(step)
    }

    pub fn undo_with_sketch(
        &mut self,
        world: &mut VoxelWorld,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Result<Option<BuilderHistoryStep>, BuilderHistoryStep> {
        let Some(step) = self.pop_undo_detailed(world) else {
            return Ok(None);
        };
        if step.sketch_meta.is_some() && step.apply_sketch_undo(doc, links).is_none() {
            let rollback = self.pop_redo_detailed(world);
            debug_assert_eq!(
                rollback.as_ref().map(|entry| entry.label.as_str()),
                Some(step.label.as_str())
            );
            return Err(step);
        }
        Ok(Some(step))
    }

    pub fn redo_with_sketch(
        &mut self,
        world: &mut VoxelWorld,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Result<Option<BuilderHistoryStep>, BuilderHistoryStep> {
        let Some(step) = self.pop_redo_detailed(world) else {
            return Ok(None);
        };
        if step.sketch_meta.is_some() && step.apply_sketch_redo(doc, links).is_none() {
            let rollback = self.pop_undo_detailed(world);
            debug_assert_eq!(
                rollback.as_ref().map(|entry| entry.label.as_str()),
                Some(step.label.as_str())
            );
            return Err(step);
        }
        Ok(Some(step))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuilderHistoryStep {
    pub label: String,
    pub voxel_count: usize,
    pub sketch_meta: Option<BuilderHistorySketchMeta>,
}

impl BuilderHistoryStep {
    pub fn label_and_voxels(&self) -> (String, usize) {
        (self.label.clone(), self.voxel_count)
    }

    pub fn apply_sketch_undo(
        &self,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Option<SketchEditSummary> {
        self.sketch_meta.as_ref()?.apply_undo(doc, links)
    }

    pub fn apply_sketch_redo(
        &self,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Option<SketchEditSummary> {
        self.sketch_meta.as_ref()?.apply_redo(doc, links)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuilderHistorySketchMeta {
    SketchMove {
        entities: Vec<SketchId>,
        delta: IVec3,
    },
    SketchCreated {
        link_snapshots: Vec<SketchVoxelEntityLinkSnapshot>,
        document_steps: usize,
    },
    SketchTransform {
        before_links: Vec<SketchVoxelEntityLinkSnapshot>,
        after_links: Vec<SketchVoxelEntityLinkSnapshot>,
        document_steps: usize,
    },
}

impl BuilderHistorySketchMeta {
    fn apply_undo(
        &self,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Option<SketchEditSummary> {
        match self {
            Self::SketchMove { entities, delta } => {
                let summary = doc.undo_last()?;
                links.translate_entities(entities.iter().copied(), -*delta);
                Some(summary)
            }
            Self::SketchCreated {
                link_snapshots,
                document_steps,
            } => {
                let summary = apply_sketch_document_steps(doc, *document_steps, true)?;
                links.remove_entities(link_snapshots.iter().map(|snapshot| snapshot.entity));
                Some(summary)
            }
            Self::SketchTransform {
                before_links,
                after_links,
                document_steps,
            } => {
                let summary = apply_sketch_document_steps(doc, *document_steps, true)?;
                restore_transform_link_snapshots(links, before_links, after_links, before_links);
                Some(summary)
            }
        }
    }

    fn apply_redo(
        &self,
        doc: &mut SketchDocument,
        links: &mut SketchVoxelLinkIndex,
    ) -> Option<SketchEditSummary> {
        match self {
            Self::SketchMove { entities, delta } => {
                let summary = doc.redo_last()?;
                links.translate_entities(entities.iter().copied(), *delta);
                Some(summary)
            }
            Self::SketchCreated {
                link_snapshots,
                document_steps,
            } => {
                let summary = apply_sketch_document_steps(doc, *document_steps, false)?;
                links.restore_entity_snapshots(link_snapshots);
                Some(summary)
            }
            Self::SketchTransform {
                before_links,
                after_links,
                document_steps,
            } => {
                let summary = apply_sketch_document_steps(doc, *document_steps, false)?;
                restore_transform_link_snapshots(links, before_links, after_links, after_links);
                Some(summary)
            }
        }
    }
}

fn restore_transform_link_snapshots(
    links: &mut SketchVoxelLinkIndex,
    before: &[SketchVoxelEntityLinkSnapshot],
    after: &[SketchVoxelEntityLinkSnapshot],
    target: &[SketchVoxelEntityLinkSnapshot],
) {
    links.remove_entities(before.iter().chain(after).map(|snapshot| snapshot.entity));
    links.restore_entity_snapshots(target);
}

fn apply_sketch_document_steps(
    doc: &mut SketchDocument,
    steps: usize,
    undo: bool,
) -> Option<SketchEditSummary> {
    if steps == 0 {
        return None;
    }
    let before = doc.clone();
    let mut labels = Vec::with_capacity(steps);
    let mut entity_count = 0usize;
    for _ in 0..steps {
        let Some(summary) = (if undo {
            doc.undo_last()
        } else {
            doc.redo_last()
        }) else {
            *doc = before;
            return None;
        };
        labels.push(summary.label);
        entity_count += summary.entity_count;
    }
    labels.dedup();
    Some(SketchEditSummary {
        label: labels.join(" + "),
        entity_count,
    })
}

#[derive(Clone)]
struct EditHistoryBatch {
    label: String,
    changes: Vec<VoxelChange>,
    sketch_meta: Option<BuilderHistorySketchMeta>,
}

#[derive(Clone, Copy)]
struct VoxelChange {
    pos: IVec3,
    before: Voxel,
    after: Voxel,
}

fn canonicalize_voxel_changes(changes: impl IntoIterator<Item = VoxelChange>) -> Vec<VoxelChange> {
    let changes = changes.into_iter();
    let mut by_position =
        std::collections::HashMap::<IVec3, VoxelChange>::with_capacity(changes.size_hint().0);
    for change in changes {
        by_position
            .entry(change.pos)
            .and_modify(|net| net.after = change.after)
            .or_insert(change);
    }

    let mut canonical: Vec<_> = by_position
        .into_values()
        .filter(|change| change.before != change.after)
        .collect();
    // Hash iteration order is unstable, so history uses world-coordinate order.
    canonical.sort_unstable_by_key(|change| (change.pos.x, change.pos.y, change.pos.z));
    canonical
}

impl BuilderClipboard {
    pub fn is_empty(&self) -> bool {
        self.voxels.is_empty()
    }

    fn idx(&self, x: i32, y: i32, z: i32) -> usize {
        (x + z * self.size.x + y * self.size.x * self.size.z) as usize
    }
}

#[derive(Clone, Debug)]
pub enum BuildAction {
    /// Stamp the current block in a `brush` cuboid anchored at `origin`.
    PlaceBrush {
        origin: IVec3,
    },
    /// Stamp AIR in a `brush` cuboid anchored at `origin`.
    RemoveBrush {
        origin: IVec3,
    },
    /// Fill the inclusive axis-aligned box [a,b] with the current block.
    FillBox,
    /// Fill the inclusive axis-aligned box [a,b] with AIR.
    ClearBox,
    /// Copy the inclusive axis-aligned box [a,b] into the clipboard.
    Copy,
    /// Paste the clipboard with its minus-corner at `paste_origin`. Air
    /// voxels in the clipboard are preserved as air (do not overwrite).
    Paste,
    /// Paste the clipboard destructively, including AIR cells.
    PasteIncludingAir,
    /// Turn the selected box into a shell: current block on boundary,
    /// AIR inside.
    HollowBox,
    /// One-click editor tools that create useful playable structures
    /// from player position or A/B anchors.
    SmartPlatform,
    SmartShelter,
    SmartBridge,
    SmartRamp,
    SmartTunnel,
    /// Save the clipboard to `./prefabs/<name>.ron`.
    SavePrefab,
    /// Load `./prefabs/<name>.ron` into the clipboard.
    LoadPrefab,
    /// Rotate the clipboard 90° clockwise around the Y axis (top-down view).
    /// After rotation the new size is (old.z, old.y, old.x).
    RotateClipboardY,
    /// Mirror the clipboard along one axis. Useful for symmetric
    /// buildings — build one half, copy, flip, paste adjacent.
    FlipClipboardX,
    FlipClipboardY,
    FlipClipboardZ,
    Undo,
    Redo,
}

/// On-disk prefab format.
#[derive(Serialize, Deserialize)]
struct PrefabFile {
    version: u32,
    size: [i32; 3],
    voxels: Vec<Voxel>,
}

pub struct BuilderPlugin;

impl Plugin for BuilderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(BuilderState::default())
            .insert_resource(BuilderClipboard::default())
            .insert_resource(BuilderHistory::default())
            .add_systems(
                Update,
                (
                    live_builder_input.run_if(in_state(crate::menu::GameState::InGame)),
                    apply_build_actions,
                )
                    .chain(),
            );
    }
}

const PREFAB_DIR: &str = "prefabs";

fn apply_build_actions(
    mut state: ResMut<BuilderState>,
    mut clipboard: ResMut<BuilderClipboard>,
    mut history: ResMut<BuilderHistory>,
    mut world: ResMut<VoxelWorld>,
    mut telemetry: ResMut<UnifiedTelemetry>,
    player_q: Query<&Transform, With<Player>>,
    mirror: Res<crate::selection::MirrorState>,
) {
    if state.pending.is_empty() {
        return;
    }
    let queued: Vec<BuildAction> = state.pending.drain(..).collect();
    // Mirror pre-pass: every place / remove spawns reflected twins
    // across the armed planes. We generate the reflections once (per
    // armed axis combination) and flatten into one action list so
    // downstream code doesn't change. Non-spatial actions (Copy,
    // Paste, SavePrefab, RotateClipboard*, …) pass through unchanged;
    // clipboard-space mirroring is handled via the FlipClipboard*
    // actions instead.
    let queued = mirror_expand(queued, *mirror, state.brush);
    let player_transform = player_q.get_single().ok();
    let player_pos = player_transform
        .map(|t| t.translation)
        .unwrap_or(Vec3::ZERO);
    let player_forward = player_transform.map(|t| *t.forward()).unwrap_or(Vec3::X);

    for action in queued {
        match action {
            BuildAction::PlaceBrush { origin } => {
                let label = format!(
                    "Place {:?} {}x{}x{}",
                    state.block, state.brush.x, state.brush.y, state.brush.z
                );
                let (n, note) = stamp_cuboid(
                    &mut world,
                    &mut history,
                    label,
                    origin,
                    state.brush,
                    state.block.into(),
                );
                state.status = format!(
                    "Platziert: {:?}  Groesse {}x{}x{}  ({} Bloecke). {}",
                    state.block, state.brush.x, state.brush.y, state.brush.z, n, note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::RemoveBrush { origin } => {
                let label = format!(
                    "Remove {}x{}x{}",
                    state.brush.x, state.brush.y, state.brush.z
                );
                let (n, note) =
                    stamp_cuboid(&mut world, &mut history, label, origin, state.brush, AIR);
                state.status = format!(
                    "Entfernt Groesse {}x{}x{}  ({} Bloecke). {}",
                    state.brush.x, state.brush.y, state.brush.z, n, note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::FillBox => {
                let (lo, hi) = minmax(state.a, state.b);
                let label = format!(
                    "Fill {:?} {:?}->{:?}",
                    state.block,
                    lo.to_array(),
                    hi.to_array()
                );
                let (n, note) =
                    fill_box(&mut world, &mut history, label, lo, hi, state.block.into());
                state.status = format!(
                    "Box gefuellt {:?} -> {:?}  ({} Bloecke). {}",
                    lo.to_array(),
                    hi.to_array(),
                    n,
                    note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::ClearBox => {
                let (lo, hi) = minmax(state.a, state.b);
                let label = format!("Clear {:?}->{:?}", lo.to_array(), hi.to_array());
                let (n, note) = fill_box(&mut world, &mut history, label, lo, hi, AIR);
                state.status = format!(
                    "Box geleert {:?} -> {:?}  ({} Bloecke). {}",
                    lo.to_array(),
                    hi.to_array(),
                    n,
                    note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::HollowBox => {
                let (lo, hi) = minmax(state.a, state.b);
                let label = format!("Hollow {:?}->{:?}", lo.to_array(), hi.to_array());
                let (n, note) =
                    hollow_box(&mut world, &mut history, label, lo, hi, state.block.into());
                state.status = format!(
                    "Box gehoehlt {:?} -> {:?}  ({} Aenderungen). {}",
                    lo.to_array(),
                    hi.to_array(),
                    n,
                    note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::SmartPlatform => {
                let center = IVec3::new(
                    player_pos.x.floor() as i32,
                    player_pos.y.floor() as i32,
                    player_pos.z.floor() as i32,
                );
                let (n, note) = smart_platform(
                    &mut world,
                    &mut history,
                    "Smart platform".into(),
                    center,
                    state.block.into(),
                );
                state.status = format!(
                    "Smart-Plattform um Spieler gebaut ({} Aenderungen). {}",
                    n, note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::SmartShelter => {
                let center = IVec3::new(
                    player_pos.x.floor() as i32,
                    player_pos.y.floor() as i32,
                    player_pos.z.floor() as i32,
                );
                let (n, note) = smart_shelter(
                    &mut world,
                    &mut history,
                    "Smart shelter".into(),
                    center,
                    player_forward,
                    state.block.into(),
                );
                state.status = format!(
                    "Smart-Basis als Rohbau mit Tuer, Licht und freiem Innenraum gebaut ({} Aenderungen). Fenster bitte per Live-Cut selbst schneiden. {}",
                    n, note
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
            }
            BuildAction::SmartBridge => {
                let (n, note) = smart_path_build(
                    &mut world,
                    &mut history,
                    "Smart bridge".into(),
                    state.a,
                    state.b,
                    state.block.into(),
                    SmartPathKind::Bridge,
                );
                state.status = format!("Smart-Bruecke A -> B gebaut ({} Aenderungen). {}", n, note);
            }
            BuildAction::SmartRamp => {
                let (n, note) = smart_path_build(
                    &mut world,
                    &mut history,
                    "Smart ramp".into(),
                    state.a,
                    state.b,
                    state.block.into(),
                    SmartPathKind::Ramp,
                );
                state.status = format!(
                    "Smart-Rampe/Treppe A -> B gebaut ({} Aenderungen). {}",
                    n, note
                );
            }
            BuildAction::SmartTunnel => {
                let (n, note) = smart_path_build(
                    &mut world,
                    &mut history,
                    "Smart tunnel".into(),
                    state.a,
                    state.b,
                    state.block.into(),
                    SmartPathKind::Tunnel,
                );
                state.status = format!(
                    "Smart-Tunnel A -> B freigelegt ({} Aenderungen). {}",
                    n, note
                );
            }
            BuildAction::Copy => {
                let (lo, hi) = minmax(state.a, state.b);
                let size = hi - lo + IVec3::ONE;
                if size.x <= 0 || size.y <= 0 || size.z <= 0 || size.x * size.y * size.z > 4_000_000
                {
                    state.status = "Kopieren abgebrochen: Box ungueltig / zu gross.".into();
                    continue;
                }
                let mut voxels = vec![AIR; (size.x * size.y * size.z) as usize];
                for y in 0..size.y {
                    for z in 0..size.z {
                        for x in 0..size.x {
                            voxels[(x + z * size.x + y * size.x * size.z) as usize] =
                                world.voxel_at(lo.x + x, lo.y + y, lo.z + z);
                        }
                    }
                }
                *clipboard = BuilderClipboard { size, voxels };
                state.paste_origin = lo;
                state.status = format!(
                    "Kopiert {}x{}x{}  ({} Bloecke).",
                    size.x,
                    size.y,
                    size.z,
                    clipboard.voxels.len()
                );
            }
            act @ (BuildAction::Paste | BuildAction::PasteIncludingAir) => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer.".into();
                    continue;
                }
                let origin = if state.paste_origin == IVec3::ZERO {
                    IVec3::new(
                        player_pos.x as i32,
                        player_pos.y as i32,
                        player_pos.z as i32,
                    )
                } else {
                    state.paste_origin
                };
                let include_air = matches!(act, BuildAction::PasteIncludingAir);
                let label = if include_air {
                    format!("Paste+Air {:?}", origin.to_array())
                } else {
                    format!("Paste {:?}", origin.to_array())
                };
                let (n, note) = paste_clipboard(
                    &mut world,
                    &mut history,
                    label,
                    &clipboard,
                    origin,
                    include_air,
                );
                state.status = format!(
                    "Eingefuegt bei {:?}  ({} Bloecke, Air={}). {}",
                    origin.to_array(),
                    n,
                    if include_air { "ja" } else { "nein" },
                    note
                );
            }
            BuildAction::SavePrefab => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer. Erst Kopieren.".into();
                    continue;
                }
                match save_prefab(&state.prefab_name, &clipboard) {
                    Ok(path) => state.status = format!("Prefab gespeichert: {}", path.display()),
                    Err(e) => state.status = format!("Speichern fehlgeschlagen: {e}"),
                }
            }
            BuildAction::LoadPrefab => match load_prefab(&state.prefab_name) {
                Ok(cb) => {
                    state.status = format!(
                        "Prefab '{}' geladen: {}x{}x{}",
                        state.prefab_name, cb.size.x, cb.size.y, cb.size.z
                    );
                    *clipboard = cb;
                }
                Err(e) => state.status = format!("Laden fehlgeschlagen: {e}"),
            },
            BuildAction::RotateClipboardY => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer.".into();
                    continue;
                }
                *clipboard = rotate_y_cw(clipboard.as_ref());
                state.status = format!(
                    "Rotiert 90° Y: {}x{}x{}",
                    clipboard.size.x, clipboard.size.y, clipboard.size.z
                );
            }
            BuildAction::FlipClipboardX => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer.".into();
                    continue;
                }
                *clipboard = flip_axis(clipboard.as_ref(), 0);
                state.status = "Gespiegelt: X-Achse.".into();
            }
            BuildAction::FlipClipboardY => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer.".into();
                    continue;
                }
                *clipboard = flip_axis(clipboard.as_ref(), 1);
                state.status = "Gespiegelt: Y-Achse.".into();
            }
            BuildAction::FlipClipboardZ => {
                if clipboard.is_empty() {
                    state.status = "Clipboard leer.".into();
                    continue;
                }
                *clipboard = flip_axis(clipboard.as_ref(), 2);
                state.status = "Gespiegelt: Z-Achse.".into();
            }
            BuildAction::Undo => {
                let Some((label, n)) = history.pop_undo(&mut world) else {
                    state.status = "Undo: nichts vorhanden.".into();
                    continue;
                };
                state.status = format!("Undo '{}': {} Bloecke.", label, n);
            }
            BuildAction::Redo => {
                let Some((label, n)) = history.pop_redo(&mut world) else {
                    state.status = "Redo: nichts vorhanden.".into();
                    continue;
                };
                state.status = format!("Redo '{}': {} Bloecke.", label, n);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn live_builder_input(
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    mode: Res<crate::mode::ModeContext>,
    mut toolbelt: ResMut<crate::toolbelt::ToolbeltState>,
    mut state: ResMut<BuilderState>,
    mut history: ResMut<BuilderHistory>,
    mut world: ResMut<VoxelWorld>,
    mirror: Res<crate::selection::MirrorState>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
) {
    use crate::toolbelt::ToolbeltTool;

    let user_action =
        mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right);
    if !mode.is_build() {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        return;
    }
    if !mode.is_build_live() {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        if user_action {
            toolbelt.status = build_drawer_blocked_status().into();
        }
        return;
    }
    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    if matches!(
        active_tool,
        ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut
    ) {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        return;
    }
    let place_tool = active_tool == ToolbeltTool::BrushPlace;
    let cut_tool = active_tool == ToolbeltTool::BrushCut;
    if !place_tool && !cut_tool {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        return;
    }
    let place_held = place_tool && mouse.pressed(MouseButton::Left);
    let place_just = place_tool && mouse.just_pressed(MouseButton::Left);
    let cut_held = (place_tool && mouse.pressed(MouseButton::Right))
        || (cut_tool && (mouse.pressed(MouseButton::Left) || mouse.pressed(MouseButton::Right)));
    let cut_just = (place_tool && mouse.just_pressed(MouseButton::Right))
        || (cut_tool
            && (mouse.just_pressed(MouseButton::Left) || mouse.just_pressed(MouseButton::Right)));
    if !place_held && !cut_held {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        return;
    }
    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);
    if !cursor_locked {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        if user_action {
            toolbelt.status = build_cursor_capture_status().into();
        }
        return;
    }
    let Ok(cam_tf) = cam_q.get_single() else {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        if user_action {
            toolbelt.status = build_camera_missing_status().into();
        }
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some((hit, adj)) = live_raycast_voxel(&world, origin, dir, 100.0) else {
        live_brush_should_stamp(&mut state.live_flow, None, time.delta_seconds(), false);
        if user_action {
            toolbelt.status = "No target face under crosshair. Aim at a visible block face.".into();
        }
        return;
    };

    let normal = adj - hit;
    let brush = oriented_live_brush(state.brush, normal);
    let block_voxel: Voxel = state.block.into();
    let (action, brush_origin, voxel, just_pressed) = if cut_held {
        (
            LiveBrushAction::Cut,
            live_brush_origin(hit, adj, brush, normal, true),
            AIR,
            cut_just,
        )
    } else {
        (
            LiveBrushAction::Place,
            live_brush_origin(hit, adj, brush, normal, false),
            block_voxel,
            place_just,
        )
    };
    let stamp = LiveBrushStamp::new(action, brush_origin, brush, voxel);
    if !live_brush_should_stamp(
        &mut state.live_flow,
        Some(stamp),
        time.delta_seconds(),
        just_pressed,
    ) {
        return;
    }

    match action {
        LiveBrushAction::Place => {
            let (n, note) = live_stamp_mirrored(
                &mut world,
                &mut history,
                "Power brush place".into(),
                brush_origin,
                brush,
                block_voxel,
                *mirror,
            );
            state.status = format!(
                "SMART BUILD {:?} {}x{}x{} ({} Bloecke). LMB endpoint builds; RMB cuts. {}",
                state.block, brush.x, brush.y, brush.z, n, note
            );
            toolbelt.status = state.status.clone();
        }
        LiveBrushAction::Cut => {
            let (n, note) = live_stamp_mirrored(
                &mut world,
                &mut history,
                "Power brush cut".into(),
                brush_origin,
                brush,
                AIR,
                *mirror,
            );
            state.status = format!(
                "SMART CUT {}x{}x{} ({} Bloecke). RMB endpoint cuts; LMB builds. {}",
                brush.x, brush.y, brush.z, n, note
            );
            toolbelt.status = state.status.clone();
        }
    }
}

fn build_drawer_blocked_status() -> &'static str {
    "Sketch Editor drawer is open. Pick a Toolbox action or close the drawer before drawing."
}

fn build_cursor_capture_status() -> &'static str {
    "Sketch Editor needs the game view. Click the game view once, then use the Toolbox action."
}

fn build_camera_missing_status() -> &'static str {
    "Sketch Editor game view could not find the player camera this frame."
}

fn oriented_live_brush(base: IVec3, normal: IVec3) -> IVec3 {
    let base = IVec3::new(base.x.max(1), base.y.max(1), base.z.max(1));
    if normal.x != 0 {
        IVec3::new(base.z.max(1), base.y, base.x.max(1))
    } else {
        base
    }
}

fn live_brush_origin(hit: IVec3, adj: IVec3, size: IVec3, normal: IVec3, cut: bool) -> IVec3 {
    let mut origin = IVec3::new(hit.x - size.x / 2, hit.y, hit.z - size.z / 2);
    if normal.x != 0 {
        origin.x = if cut {
            if normal.x > 0 {
                hit.x - (size.x - 1)
            } else {
                hit.x
            }
        } else if normal.x > 0 {
            adj.x
        } else {
            adj.x - (size.x - 1)
        };
        origin.z = hit.z - size.z / 2;
    } else if normal.z != 0 {
        origin.z = if cut {
            if normal.z > 0 {
                hit.z - (size.z - 1)
            } else {
                hit.z
            }
        } else if normal.z > 0 {
            adj.z
        } else {
            adj.z - (size.z - 1)
        };
        origin.x = hit.x - size.x / 2;
    } else if normal.y != 0 {
        origin.y = if cut {
            if normal.y > 0 {
                hit.y - (size.y - 1)
            } else {
                hit.y
            }
        } else if normal.y > 0 {
            adj.y
        } else {
            adj.y - (size.y - 1)
        };
    }
    origin
}

fn live_stamp_mirrored(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    origin: IVec3,
    brush: IVec3,
    voxel: Voxel,
    mirror: crate::selection::MirrorState,
) -> (usize, String) {
    let xs: &[bool] = if mirror.x { &[false, true] } else { &[false] };
    let ys: &[bool] = if mirror.y { &[false, true] } else { &[false] };
    let zs: &[bool] = if mirror.z { &[false, true] } else { &[false] };
    let mut total = 0usize;
    let mut last_note = String::new();
    for &fx in xs {
        for &fy in ys {
            for &fz in zs {
                let stamped_origin = reflect_origin(origin, brush, mirror.origin, (fx, fy, fz));
                let (n, note) =
                    stamp_cuboid(world, history, label.clone(), stamped_origin, brush, voxel);
                total += n;
                if !note.is_empty() {
                    last_note = note;
                }
            }
        }
    }
    (total, last_note)
}

fn live_raycast_voxel(
    world: &VoxelWorld,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<(IVec3, IVec3)> {
    // Delegates to the shared sculpt raycaster — single source of truth
    // for "what voxel is the camera looking at?" since Phase 0 of the
    // direct-manipulation overhaul. Behaviour is byte-identical to the
    // previous inline implementation.
    crate::sculpt::dda_voxel(world, origin, dir, max_dist)
}

const UNDO_CHANGE_LIMIT: usize = 250_000;
const UNDO_STACK_LIMIT: usize = 4096;

fn stamp_cuboid(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    origin: IVec3,
    size: IVec3,
    v: Voxel,
) -> (usize, String) {
    let size = size.max(IVec3::ONE);
    let mut n = 0;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;
    for dy in 0..size.y {
        for dz in 0..size.z {
            for dx in 0..size.x {
                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(origin.x + dx, origin.y + dy, origin.z + dz),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }
    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn fill_box(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    lo: IVec3,
    hi: IVec3,
    v: Voxel,
) -> (usize, String) {
    let mut n = 0;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(x, y, z),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }
    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn hollow_box(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    lo: IVec3,
    hi: IVec3,
    shell: Voxel,
) -> (usize, String) {
    let mut n = 0;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let boundary =
                    x == lo.x || x == hi.x || y == lo.y || y == hi.y || z == lo.z || z == hi.z;
                let v = if boundary { shell } else { AIR };
                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(x, y, z),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }
    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn paste_clipboard(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    clipboard: &BuilderClipboard,
    origin: IVec3,
    include_air: bool,
) -> (usize, String) {
    let mut n = 0usize;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;
    let sz = clipboard.size;
    for y in 0..sz.y {
        for z in 0..sz.z {
            for x in 0..sz.x {
                let v = clipboard.voxels[clipboard.idx(x, y, z)];
                if v == AIR && !include_air {
                    continue;
                }
                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(origin.x + x, origin.y + y, origin.z + z),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }
    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

#[derive(Clone, Copy)]
enum SmartPathKind {
    Bridge,
    Ramp,
    Tunnel,
}

fn smart_platform(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    center: IVec3,
    deck: Voxel,
) -> (usize, String) {
    let floor_y = center.y - 1;
    let radius = 8;
    let mut n = 0usize;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;

    for z in center.z - radius..=center.z + radius {
        for x in center.x - radius..=center.x + radius {
            if set_recorded(
                world,
                &mut batch,
                &mut changes,
                &mut overflow,
                IVec3::new(x, floor_y, z),
                deck,
            ) {
                n += 1;
            }

            for y in floor_y + 1..=floor_y + 5 {
                let edge = x == center.x - radius
                    || x == center.x + radius
                    || z == center.z - radius
                    || z == center.z + radius;
                let gate = (x - center.x).abs() <= 1 || (z - center.z).abs() <= 1;
                let v = if edge && y == floor_y + 1 && !gate {
                    deck
                } else {
                    AIR
                };
                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(x, y, z),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }

    for (x, z) in [
        (center.x - radius, center.z - radius),
        (center.x + radius, center.z - radius),
        (center.x - radius, center.z + radius),
        (center.x + radius, center.z + radius),
    ] {
        for y in floor_y + 1..=floor_y + 3 {
            if set_recorded(
                world,
                &mut batch,
                &mut changes,
                &mut overflow,
                IVec3::new(x, y, z),
                BlockType::GlowSand.into(),
            ) {
                n += 1;
            }
        }
    }

    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn smart_shelter(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    center: IVec3,
    forward: Vec3,
    wall: Voxel,
) -> (usize, String) {
    let floor_y = center.y - 1;
    let lo = IVec3::new(center.x - 6, floor_y, center.z - 6);
    let hi = IVec3::new(center.x + 6, floor_y + 6, center.z + 6);
    let door_dir = cardinal_xz(forward);
    let mut n = 0usize;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;

    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let boundary =
                    x == lo.x || x == hi.x || y == lo.y || y == hi.y || z == lo.z || z == hi.z;
                let mut v = if boundary { wall } else { AIR };

                let local = IVec3::new(x - center.x, y - floor_y, z - center.z);
                if is_door_cell(local, door_dir) {
                    v = AIR;
                } else if is_light_cell(local) {
                    v = BlockType::GlowSand.into();
                }

                if set_recorded(
                    world,
                    &mut batch,
                    &mut changes,
                    &mut overflow,
                    IVec3::new(x, y, z),
                    v,
                ) {
                    n += 1;
                }
            }
        }
    }

    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn smart_path_build(
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    label: String,
    a: IVec3,
    b: IVec3,
    deck: Voxel,
    kind: SmartPathKind,
) -> (usize, String) {
    let delta = b - a;
    let steps = delta.x.abs().max(delta.y.abs()).max(delta.z.abs());
    if steps <= 0 {
        return (0, "A und B sind gleich.".into());
    }
    if steps > 384 {
        return (
            0,
            "Abgebrochen: A/B-Strecke ist zu lang fuer einen Sofortbau.".into(),
        );
    }

    let mut n = 0usize;
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut overflow = false;
    let major_x = delta.x.abs() >= delta.z.abs();
    let perp = if major_x {
        IVec3::new(0, 0, 1)
    } else {
        IVec3::new(1, 0, 0)
    };
    let flat_steps = delta.x.abs().max(delta.z.abs()).max(1);

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = lerp_i32(a.x, b.x, t);
        let z = lerp_i32(a.z, b.z, t);
        let y = match kind {
            SmartPathKind::Bridge => a.y,
            SmartPathKind::Ramp | SmartPathKind::Tunnel => {
                let ft = (i.min(flat_steps)) as f32 / flat_steps as f32;
                lerp_i32(a.y, b.y, ft)
            }
        };
        let p = IVec3::new(x, y, z);

        match kind {
            SmartPathKind::Bridge | SmartPathKind::Ramp => {
                for w in -1..=1 {
                    let q = p + perp * w;
                    if set_recorded(world, &mut batch, &mut changes, &mut overflow, q, deck) {
                        n += 1;
                    }
                    for clear_y in q.y + 1..=q.y + 4 {
                        if set_recorded(
                            world,
                            &mut batch,
                            &mut changes,
                            &mut overflow,
                            IVec3::new(q.x, clear_y, q.z),
                            AIR,
                        ) {
                            n += 1;
                        }
                    }
                }
                for side in [-2, 2] {
                    let rail = p + perp * side + IVec3::Y;
                    if set_recorded(world, &mut batch, &mut changes, &mut overflow, rail, deck) {
                        n += 1;
                    }
                    if i % 5 == 0 {
                        let lamp = rail + IVec3::Y;
                        if set_recorded(
                            world,
                            &mut batch,
                            &mut changes,
                            &mut overflow,
                            lamp,
                            BlockType::GlowSand.into(),
                        ) {
                            n += 1;
                        }
                    }
                }
            }
            SmartPathKind::Tunnel => {
                let floor = p - IVec3::Y;
                for w in -2..=2 {
                    let base = floor + perp * w;
                    if set_recorded(world, &mut batch, &mut changes, &mut overflow, base, deck) {
                        n += 1;
                    }
                    for h in 1..=4 {
                        if set_recorded(
                            world,
                            &mut batch,
                            &mut changes,
                            &mut overflow,
                            base + IVec3::Y * h,
                            AIR,
                        ) {
                            n += 1;
                        }
                    }
                }
                for side in [-3, 3] {
                    for h in 0..=3 {
                        if set_recorded(
                            world,
                            &mut batch,
                            &mut changes,
                            &mut overflow,
                            floor + perp * side + IVec3::Y * h,
                            deck,
                        ) {
                            n += 1;
                        }
                    }
                }
                if i % 8 == 0 {
                    if set_recorded(
                        world,
                        &mut batch,
                        &mut changes,
                        &mut overflow,
                        floor + IVec3::Y * 5,
                        BlockType::GlowSand.into(),
                    ) {
                        n += 1;
                    }
                }
            }
        }
    }

    world.finish_edit_batch(batch);
    let note = commit_history(history, label, changes, overflow);
    (n, note)
}

fn cardinal_xz(forward: Vec3) -> IVec3 {
    if forward.x.abs() >= forward.z.abs() {
        IVec3::new(if forward.x >= 0.0 { 1 } else { -1 }, 0, 0)
    } else {
        IVec3::new(0, 0, if forward.z >= 0.0 { 1 } else { -1 })
    }
}

fn is_door_cell(local: IVec3, dir: IVec3) -> bool {
    let on_face = if dir.x != 0 {
        local.x == 6 * dir.x && local.z.abs() <= 1
    } else {
        local.z == 6 * dir.z && local.x.abs() <= 1
    };
    on_face && (1..=3).contains(&local.y)
}

fn is_light_cell(local: IVec3) -> bool {
    local.y == 6 && ((local.x == 0 && local.z == 0) || (local.x.abs() == 4 && local.z.abs() == 4))
}

fn lerp_i32(a: i32, b: i32, t: f32) -> i32 {
    (a as f32 + (b - a) as f32 * t).round() as i32
}

fn set_recorded(
    world: &mut VoxelWorld,
    batch: &mut WorldEditBatch,
    changes: &mut Vec<VoxelChange>,
    overflow: &mut bool,
    pos: IVec3,
    v: Voxel,
) -> bool {
    let Some((before, after)) = world.edit_set_voxel_batched(pos.x, pos.y, pos.z, v, batch) else {
        return false;
    };
    if !*overflow {
        if changes.len() < UNDO_CHANGE_LIMIT {
            changes.push(VoxelChange { pos, before, after });
        } else {
            changes.clear();
            *overflow = true;
        }
    }
    true
}

fn commit_history(
    history: &mut BuilderHistory,
    label: String,
    changes: Vec<VoxelChange>,
    overflow: bool,
) -> String {
    if overflow {
        history.redo.clear();
        return "Undo ausgelassen: Region zu gross.".into();
    }
    if changes.is_empty() {
        return "Keine Aenderung.".into();
    }
    history.undo.push(EditHistoryBatch {
        label,
        changes,
        sketch_meta: None,
    });
    if history.undo.len() > UNDO_STACK_LIMIT {
        history.undo.remove(0);
    }
    history.redo.clear();
    "Undo bereit.".into()
}

fn apply_history_batch(world: &mut VoxelWorld, batch_src: &EditHistoryBatch, undo: bool) -> usize {
    let mut batch = WorldEditBatch::default();
    let mut n = 0usize;
    let changes = canonicalize_voxel_changes(batch_src.changes.iter().copied());
    for change in &changes {
        let v = if undo { change.before } else { change.after };
        if world
            .edit_set_voxel_batched(change.pos.x, change.pos.y, change.pos.z, v, &mut batch)
            .is_some()
        {
            n += 1;
        }
    }
    world.finish_edit_batch(batch);
    n
}

fn minmax(a: IVec3, b: IVec3) -> (IVec3, IVec3) {
    (
        IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

// ---------------------------------------------------------------------
// Mirror pre-pass
// ---------------------------------------------------------------------
//
// When any axis of `MirrorState` is armed, every `PlaceBrush` /
// `RemoveBrush` action is duplicated across the armed planes. One
// armed axis = 2 twins, two = 4, three = 8. Reflected origin accounts
// for the brush extent so the mirrored cuboid is exactly on the other
// side of the plane, not off by the brush size.

fn reflect_origin(origin: IVec3, brush: IVec3, mo: IVec3, axes: (bool, bool, bool)) -> IVec3 {
    IVec3::new(
        if axes.0 {
            2 * mo.x - origin.x - (brush.x - 1)
        } else {
            origin.x
        },
        if axes.1 {
            2 * mo.y - origin.y - (brush.y - 1)
        } else {
            origin.y
        },
        if axes.2 {
            2 * mo.z - origin.z - (brush.z - 1)
        } else {
            origin.z
        },
    )
}

fn mirror_expand(
    queued: Vec<BuildAction>,
    m: crate::selection::MirrorState,
    brush: IVec3,
) -> Vec<BuildAction> {
    if !m.x && !m.y && !m.z {
        return queued;
    }
    // Slices let us iterate the armed-axis cross-product cleanly.
    let xs: &[bool] = if m.x { &[false, true] } else { &[false] };
    let ys: &[bool] = if m.y { &[false, true] } else { &[false] };
    let zs: &[bool] = if m.z { &[false, true] } else { &[false] };
    let mut out = Vec::with_capacity(queued.len() * 8);
    for act in queued {
        match act {
            BuildAction::PlaceBrush { origin } => {
                for &fx in xs {
                    for &fy in ys {
                        for &fz in zs {
                            let twin = reflect_origin(origin, brush, m.origin, (fx, fy, fz));
                            out.push(BuildAction::PlaceBrush { origin: twin });
                        }
                    }
                }
            }
            BuildAction::RemoveBrush { origin } => {
                for &fx in xs {
                    for &fy in ys {
                        for &fz in zs {
                            let twin = reflect_origin(origin, brush, m.origin, (fx, fy, fz));
                            out.push(BuildAction::RemoveBrush { origin: twin });
                        }
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

fn save_prefab(name: &str, cb: &BuilderClipboard) -> std::io::Result<std::path::PathBuf> {
    let _ = std::fs::create_dir_all(PREFAB_DIR);
    let path = std::path::Path::new(PREFAB_DIR).join(format!("{}.ron", sanitize(name)));
    let file = PrefabFile {
        version: 1,
        size: cb.size.to_array(),
        voxels: cb.voxels.clone(),
    };
    let s = ron::ser::to_string_pretty(&file, ron::ser::PrettyConfig::default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&path, s)?;
    Ok(path)
}

fn load_prefab(name: &str) -> std::io::Result<BuilderClipboard> {
    let path = std::path::Path::new(PREFAB_DIR).join(format!("{}.ron", sanitize(name)));
    let s = std::fs::read_to_string(&path)?;
    let file: PrefabFile =
        ron::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    Ok(BuilderClipboard {
        size: IVec3::from_array(file.size),
        voxels: file.voxels,
    })
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Scan `./prefabs/` and return the base names (without extension) of
/// every `.ron` file found. Cheap — one directory read per call,
/// intended for the prefab browser in the BAUEN tab.
pub fn list_prefabs() -> Vec<String> {
    let mut out = Vec::new();
    let dir = std::path::Path::new(PREFAB_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) == Some("ron") {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Rotate the clipboard 90° clockwise around the Y axis. Produces a
/// new clipboard whose size is (old.z, old.y, old.x).
fn rotate_y_cw(cb: &BuilderClipboard) -> BuilderClipboard {
    let sx = cb.size.x;
    let sy = cb.size.y;
    let sz = cb.size.z;
    let new_size = IVec3::new(sz, sy, sx);
    let mut voxels = vec![AIR; (new_size.x * new_size.y * new_size.z) as usize];
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                // CW around Y (looking down +Y): (x,z) -> (z, sx-1-x)
                let nx = z;
                let nz = sx - 1 - x;
                let src = cb.voxels[(x + z * sx + y * sx * sz) as usize];
                let dst_idx = (nx + nz * new_size.x + y * new_size.x * new_size.z) as usize;
                voxels[dst_idx] = src;
            }
        }
    }
    BuilderClipboard {
        size: new_size,
        voxels,
    }
}

/// Mirror along axis 0/1/2 = X/Y/Z. Size is preserved.
fn flip_axis(cb: &BuilderClipboard, axis: u8) -> BuilderClipboard {
    let sx = cb.size.x;
    let sy = cb.size.y;
    let sz = cb.size.z;
    let mut voxels = vec![AIR; (sx * sy * sz) as usize];
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let (nx, ny, nz) = match axis {
                    0 => (sx - 1 - x, y, z),
                    1 => (x, sy - 1 - y, z),
                    _ => (x, y, sz - 1 - z),
                };
                let src = cb.voxels[(x + z * sx + y * sx * sz) as usize];
                voxels[(nx + nz * sx + ny * sx * sz) as usize] = src;
            }
        }
    }
    BuilderClipboard {
        size: cb.size,
        voxels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_undo_rolls_world_back_when_semantic_history_is_incomplete() {
        let mut world = VoxelWorld::new();
        let cell = IVec3::new(2, 3, 4);
        let stone = Voxel::from(BlockType::Stone);
        assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, stone));

        let mut doc = SketchDocument::new();
        let edge = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X)
            .expect("semantic edge");
        let mut links = SketchVoxelLinkIndex::default();
        let mut history = BuilderHistory::default();
        history.record_external_with_sketch_meta(
            "incomplete semantic batch",
            vec![(cell, AIR, stone)],
            Some(BuilderHistorySketchMeta::SketchCreated {
                link_snapshots: Vec::new(),
                document_steps: 2,
            }),
        );

        let failed = history
            .undo_with_sketch(&mut world, &mut doc, &mut links)
            .expect_err("semantic mismatch must block the whole undo");

        assert_eq!(failed.label, "incomplete semantic batch");
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), stone);
        assert!(doc.entity(edge).is_some());
        assert_eq!(history.undo_len(), 1);
        assert_eq!(history.redo_len(), 0);
    }

    #[test]
    fn transform_history_restores_document_voxels_and_links_atomically() {
        let mut world = VoxelWorld::new();
        let stone = Voxel::from(BlockType::Stone);
        for cell in [IVec3::X, IVec3::X * 2] {
            assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, stone));
        }

        let mut doc = SketchDocument::new();
        let edge = doc
            .draw_pencil_line(doc.active_context(), Vec3::X, Vec3::X * 3.0)
            .expect("semantic edge");
        let link = crate::sketch_model::SketchVoxelLink::new(
            edge,
            doc.active_context(),
            crate::sketch_model::SketchVoxelLinkRole::Stroke,
        );
        let mut links = SketchVoxelLinkIndex::default();
        links.link_cells([IVec3::X, IVec3::X * 2], link);
        assert!(links.link_face_cell(IVec3::X, IVec3::X, link));
        let before_links = links.snapshot_entities([edge]);

        let mut selection = crate::sketch_model::SelectionSet::default();
        selection.select(edge);
        doc.rotate_selection_about_pivot(
            &selection,
            Vec3::ZERO,
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            "Rotate exact",
        )
        .expect("semantic rotation");
        links.remove_entity(edge);
        links.link_cells([IVec3::Y, IVec3::Y * 2], link);
        assert!(links.link_face_cell(IVec3::Y, IVec3::Y, link));
        let after_links = links.snapshot_entities([edge]);

        for cell in [IVec3::X, IVec3::X * 2] {
            assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, AIR));
        }
        for cell in [IVec3::Y, IVec3::Y * 2] {
            assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, stone));
        }
        let changes = vec![
            (IVec3::X, stone, AIR),
            (IVec3::X * 2, stone, AIR),
            (IVec3::Y, AIR, stone),
            (IVec3::Y * 2, AIR, stone),
        ];
        let mut history = BuilderHistory::default();
        history.record_external_with_sketch_meta(
            "Rotate exact",
            changes,
            Some(BuilderHistorySketchMeta::SketchTransform {
                before_links: before_links.clone(),
                after_links: after_links.clone(),
                document_steps: 1,
            }),
        );

        history
            .undo_with_sketch(&mut world, &mut doc, &mut links)
            .expect("undo remains atomic")
            .expect("undo step");
        assert_eq!(world.voxel_at(1, 0, 0), stone);
        assert_eq!(world.voxel_at(0, 1, 0), AIR);
        assert_eq!(links.snapshot_entities([edge]), before_links);
        assert!(matches!(
            &doc.entity(edge).expect("restored edge").kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if *a == Vec3::X && *b == Vec3::X * 3.0
        ));

        history
            .redo_with_sketch(&mut world, &mut doc, &mut links)
            .expect("redo remains atomic")
            .expect("redo step");
        assert_eq!(world.voxel_at(1, 0, 0), AIR);
        assert_eq!(world.voxel_at(0, 1, 0), stone);
        assert_eq!(links.snapshot_entities([edge]), after_links);
        assert!(matches!(
            &doc.entity(edge).expect("rotated edge").kind,
            crate::sketch_model::SketchEntityKind::Edge { a, b }
                if a.distance(Vec3::Y) < 1.0e-5 && b.distance(Vec3::Y * 3.0) < 1.0e-5
        ));
    }

    #[test]
    fn failed_transform_history_keeps_link_index_unchanged() {
        let mut world = VoxelWorld::new();
        let cell = IVec3::new(4, 5, 6);
        let stone = Voxel::from(BlockType::Stone);
        assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, stone));

        let mut doc = SketchDocument::new();
        let edge = doc
            .draw_pencil_line(doc.active_context(), Vec3::ZERO, Vec3::X)
            .expect("semantic edge");
        let link = crate::sketch_model::SketchVoxelLink::new(
            edge,
            doc.active_context(),
            crate::sketch_model::SketchVoxelLinkRole::Stroke,
        );
        let mut links = SketchVoxelLinkIndex::default();
        links.link_cell(cell, link);
        let original_links = links.clone();
        let snapshots = links.snapshot_entities([edge]);
        let mut history = BuilderHistory::default();
        history.record_external_with_sketch_meta(
            "incomplete transform",
            vec![(cell, AIR, stone)],
            Some(BuilderHistorySketchMeta::SketchTransform {
                before_links: snapshots.clone(),
                after_links: snapshots,
                document_steps: 2,
            }),
        );

        history
            .undo_with_sketch(&mut world, &mut doc, &mut links)
            .expect_err("semantic mismatch must reject transform undo");

        assert_eq!(links, original_links);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), stone);
        assert!(doc.entity(edge).is_some());
        assert_eq!(history.undo_len(), 1);
        assert_eq!(history.redo_len(), 0);
    }

    #[test]
    fn external_history_canonicalizes_overlapping_positions_for_atomic_undo_redo() {
        let mut world = VoxelWorld::new();
        let a = IVec3::new(3, 4, 5);
        let b = IVec3::new(4, 4, 5);
        let c = IVec3::new(5, 4, 5);
        let stone = Voxel::from(BlockType::Stone);
        let limestone = Voxel::from(BlockType::Limestone);
        assert!(world.edit_set_voxel(a.x, a.y, a.z, limestone));
        assert!(world.edit_set_voxel(c.x, c.y, c.z, stone));

        let mut history = BuilderHistory::default();
        let outcome = history.record_external_checked(
            "overlapping transform",
            vec![
                (b, AIR, stone),
                (a, stone, AIR),
                (c, AIR, limestone),
                (a, AIR, limestone),
                (b, stone, AIR),
                (c, limestone, stone),
            ],
        );

        assert_eq!(
            outcome,
            BuilderHistoryRecordOutcome::Recorded { voxel_count: 2 }
        );
        assert_eq!(history.undo[0].changes.len(), 2);
        assert_eq!(
            history.undo[0]
                .changes
                .iter()
                .map(|change| change.pos)
                .collect::<Vec<_>>(),
            vec![a, c]
        );

        let undone = history.pop_undo_detailed(&mut world).expect("undo step");
        assert_eq!(undone.voxel_count, 2);
        assert_eq!(world.voxel_at(a.x, a.y, a.z), stone);
        assert_eq!(world.voxel_at(b.x, b.y, b.z), AIR);
        assert_eq!(world.voxel_at(c.x, c.y, c.z), AIR);
        assert_eq!((history.undo_len(), history.redo_len()), (0, 1));

        let redone = history.pop_redo_detailed(&mut world).expect("redo step");
        assert_eq!(redone.voxel_count, 2);
        assert_eq!(world.voxel_at(a.x, a.y, a.z), limestone);
        assert_eq!(world.voxel_at(b.x, b.y, b.z), AIR);
        assert_eq!(world.voxel_at(c.x, c.y, c.z), stone);
        assert_eq!((history.undo_len(), history.redo_len()), (1, 0));
    }

    #[test]
    fn legacy_duplicate_history_uses_first_before_and_final_after() {
        let mut world = VoxelWorld::new();
        let cell = IVec3::new(7, 8, 9);
        let stone = Voxel::from(BlockType::Stone);
        let limestone = Voxel::from(BlockType::Limestone);
        assert!(world.edit_set_voxel(cell.x, cell.y, cell.z, limestone));

        let mut history = BuilderHistory::default();
        history.undo.push(EditHistoryBatch {
            label: "legacy overlap".into(),
            changes: vec![
                VoxelChange {
                    pos: cell,
                    before: AIR,
                    after: stone,
                },
                VoxelChange {
                    pos: cell,
                    before: stone,
                    after: limestone,
                },
            ],
            sketch_meta: None,
        });

        let undone = history.pop_undo_detailed(&mut world).expect("legacy undo");
        assert_eq!(undone.voxel_count, 1);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), AIR);

        let redone = history.pop_redo_detailed(&mut world).expect("legacy redo");
        assert_eq!(redone.voxel_count, 1);
        assert_eq!(world.voxel_at(cell.x, cell.y, cell.z), limestone);
    }

    #[test]
    fn external_history_accepts_cap_and_rejects_cap_plus_one_atomically() {
        let stone = Voxel::from(BlockType::Stone);
        let make_changes = |count: usize| {
            (0..count)
                .map(|x| (IVec3::new(x as i32, 0, 0), AIR, stone))
                .collect::<Vec<_>>()
        };

        let mut at_cap = make_changes(UNDO_CHANGE_LIMIT);
        at_cap.push((IVec3::ZERO, stone, stone));
        let mut history = BuilderHistory::default();
        assert_eq!(
            history.record_external_checked("at cap after deduplication", at_cap),
            BuilderHistoryRecordOutcome::Recorded {
                voxel_count: UNDO_CHANGE_LIMIT
            }
        );
        assert_eq!(history.undo_len(), 1);
        assert_eq!(history.undo[0].changes.len(), UNDO_CHANGE_LIMIT);

        let rejected = history.record_external_with_sketch_meta_checked(
            "cap plus one transform",
            make_changes(UNDO_CHANGE_LIMIT + 1),
            Some(BuilderHistorySketchMeta::SketchTransform {
                before_links: Vec::new(),
                after_links: Vec::new(),
                document_steps: 1,
            }),
        );
        assert_eq!(
            rejected,
            BuilderHistoryRecordOutcome::RejectedTooManyChanges {
                voxel_count: UNDO_CHANGE_LIMIT + 1,
                limit: UNDO_CHANGE_LIMIT,
            }
        );
        assert_eq!(history.undo_len(), 1);
        assert_eq!(history.undo[0].label, "at cap after deduplication");
        assert!(history.undo.iter().all(|batch| batch.sketch_meta.is_none()));
        assert_eq!(history.redo_len(), 0);
    }

    #[test]
    fn live_brush_flow_stamps_once_per_new_target_while_held() {
        let mut flow = LiveBrushFlow::default();
        let brush = IVec3::new(2, 2, 2);
        let a = LiveBrushStamp::new(
            LiveBrushAction::Place,
            IVec3::new(0, 4, 0),
            brush,
            BlockType::Stone.into(),
        );
        let b = LiveBrushStamp::new(
            LiveBrushAction::Place,
            IVec3::new(1, 4, 0),
            brush,
            BlockType::Stone.into(),
        );
        let c = LiveBrushStamp::new(
            LiveBrushAction::Place,
            IVec3::new(2, 4, 0),
            brush,
            BlockType::Stone.into(),
        );

        assert!(live_brush_should_stamp(&mut flow, Some(a), 0.016, true));
        assert!(!live_brush_should_stamp(&mut flow, Some(a), 0.200, false));
        assert!(live_brush_should_stamp(
            &mut flow,
            Some(b),
            LIVE_BRUSH_MIN_INTERVAL_SECONDS,
            false
        ));
        assert!(!live_brush_should_stamp(&mut flow, Some(c), 0.0, false));
        assert!(!live_brush_should_stamp(&mut flow, None, 0.016, false));
        assert!(live_brush_should_stamp(&mut flow, Some(c), 0.0, true));
    }

    #[test]
    fn builder_statuses_are_toolbox_first_not_old_build_live() {
        for status in [
            build_drawer_blocked_status(),
            build_cursor_capture_status(),
            build_camera_missing_status(),
        ] {
            assert!(!status.contains("Build Live"));
            assert!(!status.contains("Tab"));
            assert!(status.contains("Toolbox") || status.contains("game view"));
        }
    }

    #[test]
    fn builder_history_keeps_long_sketch_sessions_beyond_old_32_step_cap() {
        let mut history = BuilderHistory::default();

        for x in 0..96 {
            history.record_external(
                format!("Pencil line {x}"),
                vec![(IVec3::new(x, 0, 0), AIR, Voxel::from(BlockType::Limestone))],
            );
        }

        assert_eq!(
            history.undo_len(),
            96,
            "SketchUp-style building sessions need long undo history; dropping edits after 32 actions makes precise construction feel broken"
        );
    }
}
