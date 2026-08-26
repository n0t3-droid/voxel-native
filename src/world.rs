//! World plugin — owns the chunk map, streams chunks around the player,
//! and schedules terrain + meshing work on background threads via
//! `AsyncComputeTaskPool` so a render distance of 20+ chunks stays snappy
//! even on modest hardware.
//!
//! Port target: `lib/voxel/ChunkManager.ts` + `lib/voxel/worker.ts`.

use ahash::{AHashMap, AHashSet};
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::tasks::AsyncComputeTaskPool;
use bevy::tasks::Task;
use futures_lite::future;
use serde::{Deserialize, Serialize};
use std::collections::HashMap as StdHashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::blocks::{
    effective_material_for_voxel, normalize_material_for_voxel, voxel_is_solid, BlockType,
    MaterialId, Voxel, AIR, DEFAULT_MATERIAL,
};
use crate::chunk::{
    world_to_chunk, Chunk, ChunkPos, SharedMaterials, SharedVoxels, CHUNK_SIZE_I, CHUNK_VOLUME,
};
use crate::horizon::SharedHorizonCache;
use crate::mesher::{build_mesh_buckets_budgeted_with_horizon, MeshBucketKey, MeshRenderClass};
use crate::neurocore::{QualityState, RuntimeBudget, RuntimeIntent, RuntimeProfile};
#[cfg(not(target_arch = "wasm32"))]
use crate::settings::TerrainGrammarVersion;
use crate::settings::{WorldGenerationIdentity, WorldSettings};
use crate::terrain::TerrainGenerator;
use crate::vegetation::VegetationSpecies;
use crate::voxel_budget::{VoxelDetailTier, WorldQualityBudget};

pub struct WorldPlugin;

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub enum WorldSet {
    NeuroCore,
    Stream,
    Mesh,
}

/// Dense 16³ chunks are the interaction representation, not the horizon
/// representation. Keeping this ceiling independent of the user's visual
/// render distance is what makes a kilometre flight a bounded-memory
/// operation. Far-field clipmaps/bricks can extend the view without raising
/// this number.
pub const MAX_FULL_CHUNK_RESIDENT: usize = 2_400;
/// The near-field candidate disc is deliberately small and constant. The
/// exact request planner below usually reaches the resident budget before the
/// rim in tall terrain, but never scans an RD=64-sized disc.
pub const MAX_INTERACTION_RADIUS_CHUNKS: i32 = 16;
pub const MAX_IN_FLIGHT_TERRAIN_TASKS: usize = 96;
pub const MAX_IN_FLIGHT_MESH_TASKS: usize = 64;
pub const FULL_CHUNK_CAP_REASON: &str =
    "Dense chunks are reserved for interaction; the visual horizon uses bounded far-field LOD";
const GUARANTEED_INTERACTION_CORE_CHUNKS: i32 = 4;
const PREDICTIVE_LEAD_CHUNKS: i32 = 4;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(VoxelWorld::new())
            .insert_resource(ChunkStreamer::default())
            .insert_resource(StreamingGovernor::default())
            .insert_resource(crate::textures::MaterialLibrary::default())
            .configure_sets(
                Update,
                (WorldSet::NeuroCore, WorldSet::Stream, WorldSet::Mesh).chain(),
            )
            .add_systems(Startup, init_world)
            .add_systems(Update, reload_material_library)
            .add_systems(
                OnEnter(crate::menu::GameState::InGame),
                reinit_world_for_active,
            )
            .add_systems(
                Update,
                stream_chunks
                    .in_set(WorldSet::Stream)
                    .run_if(in_state(crate::menu::GameState::InGame)),
            )
            .add_systems(
                Update,
                mesh_dirty_chunks
                    .in_set(WorldSet::Mesh)
                    .run_if(in_state(crate::menu::GameState::InGame)),
            );
    }
}

fn reload_material_library(
    mut library: ResMut<crate::textures::MaterialLibrary>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut vegetation_materials: ResMut<Assets<crate::vegetation::VegetationMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut world: ResMut<VoxelWorld>,
) {
    if !library.reload_requested {
        return;
    }
    library.rebuild(&mut materials, &mut vegetation_materials, &mut images);
    let mut dirty = Vec::new();
    for chunk in world.chunks.values_mut() {
        chunk.dirty = true;
        dirty.push(chunk.pos);
    }
    for pos in dirty {
        world.edit_dirty_chunks.insert(pos);
    }
}

/// When the player enters a world (via main menu / load), rebuild the
/// generator with the chosen seed and drop any stale chunks. Skipped when
/// returning from Pause/Options so mid-play tweaks don't reset the world.
fn reinit_world_for_active(
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    mut meshes: ResMut<Assets<Mesh>>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    mut pending: ResMut<crate::menu::PendingWorldLoad>,
    mut pending_edits: ResMut<PendingEditedOverrideStore>,
    mut next: ResMut<NextState<crate::menu::GameState>>,
    mut commands: Commands,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active.as_deref() else {
        error!("world edits: pending world load has no immutable ActiveWorld authority");
        pending.0 = false;
        pending_edits.clear();
        next.set(crate::menu::GameState::MainMenu);
        return;
    };
    let identity = active.meta.generation_identity();
    let loaded = pending_edits.take(&active.meta.name, identity).map_or_else(
        || load_edited_overrides_for_world(&active.meta.name, identity),
        |(overrides, manifest)| EditedOverrideStoreLoad::Compatible {
            overrides,
            manifest,
        },
    );
    let (overrides, manifest) = match loaded {
        EditedOverrideStoreLoad::Compatible {
            overrides,
            manifest,
        } => (overrides, manifest),
        EditedOverrideStoreLoad::Blocked { reason } => {
            error!(
                "world edits: refusing to open '{}' because its edit authority is blocked: {reason}",
                active.meta.name
            );
            world.edit_store_status = WorldEditStoreStatus::Blocked {
                generation_identity: identity,
                reason_code: "authority_validation_failed",
                detail: reason,
            };
            pending.0 = false;
            pending_edits.clear();
            commands.remove_resource::<crate::settings::ActiveWorld>();
            next.set(crate::menu::GameState::MainMenu);
            return;
        }
    };

    world.generator = TerrainGenerator::from_identity(identity);
    world.clear_chunks();
    world.edited_overrides.clear();
    world.column_top_cy.clear();
    world.edit_dirty_chunks.clear();
    world.edit_save_dirty = false;
    world.edit_save_revision = 0;
    world.last_repair_report = None;
    if !overrides.is_empty() {
        info!(
            "world edits: loaded {} edited chunks for '{}'",
            manifest.edited_chunks, active.meta.name
        );
    }
    world.edited_overrides = overrides;
    world.edit_store_status = WorldEditStoreStatus::Compatible {
        generation_identity: identity,
        edited_chunks: manifest.edited_chunks,
    };
    streamer.pending_terrain.clear();
    streamer.pending_meshes.clear();
    streamer.dirty_queue.clear();
    streamer.mesh_candidates_scratch.clear();
    streamer.load_offsets.clear();
    streamer.load_offsets_rd = -1;
    streamer.load_cursor = 0;
    streamer.last_vertical_chunks = 0;
    streamer.frontier_complete = false;
    streamer.last_anchor_cxz = None;
    streamer.requested_chunks.clear();
    streamer.request_epoch = streamer.request_epoch.wrapping_add(1).max(1);
    streamer.last_priority_heading = (0, 0);
    streamer.last_motion_hint = (0, 0);
    streamer.telemetry = StreamingTelemetry {
        request_epoch: streamer.request_epoch,
        ..default()
    };
    streamer.needs_orphan_scan = true;
    for (_, group) in streamer.entities.drain() {
        for entry in group {
            if let Some(entity_commands) = commands.get_entity(entry.entity) {
                entity_commands.despawn_recursive();
            }
            let _ = meshes.remove(&entry.handle);
        }
    }
}

#[derive(Resource)]
pub struct VoxelWorld {
    pub chunks: AHashMap<ChunkPos, Chunk>,
    /// Per-column loaded chunk counts. Player physics and bots ask
    /// "is this column ready?" every frame; scanning `chunks.keys()` at
    /// RD=50 turns that into thousands of hash visits per query.
    pub loaded_column_counts: AHashMap<(i32, i32), usize>,
    pub generator: TerrainGenerator,
    /// Cached macro-scale terrain horizons, shared by every vertical chunk
    /// in an X/Z column. This affects rendering only and never simulation.
    horizon_cache: SharedHorizonCache,
    /// Full chunk snapshots for chunks touched by editor/build tools. These
    /// stay resident even when the render streamer unloads the chunk, then
    /// re-apply when terrain streams back in.
    pub edited_overrides: AHashMap<ChunkPos, EditedChunkOverride>,
    /// Per (cx, cz) column: the maximum surface height within that column,
    /// quantised to a chunk-y index. Chunks strictly above this index are
    /// guaranteed air and don't need terrain-gen / meshing work. Populated
    /// lazily as columns are first visited so RD=50 stays cheap.
    pub column_top_cy: AHashMap<(i32, i32), i32>,
    /// Chunks dirtied by direct voxel edits (builder, city, animation,
    /// weapons). The mesher drains this into its priority queue once per
    /// frame, which keeps every editing subsystem from needing a direct
    /// dependency on [`ChunkStreamer`].
    pub edit_dirty_chunks: AHashSet<ChunkPos>,
    /// Dense slots currently reserved by terrain tasks. The streamer refreshes
    /// this after every scheduling pass so direct editors cannot materialise a
    /// chunk beyond the exact resident-plus-in-flight ceiling between systems.
    reserved_async_dense_slots: usize,
    /// True once direct edits changed `edited_overrides` since the last
    /// save request. Autosave uses this to avoid serialising every edit
    /// chunk every 30 seconds when nothing changed.
    pub edit_save_dirty: bool,
    /// Monotonic in-memory content revision for `edited_overrides`. Save
    /// receipts may clear [`Self::edit_save_dirty`] only when they still
    /// describe this exact revision.
    edit_save_revision: u64,
    /// Read-only runtime truth for QA and every persistence caller. Only an
    /// exact `Compatible` identity authorizes writes to the world's edit or
    /// companion stores.
    pub edit_store_status: WorldEditStoreStatus,
    /// Last explicit visual-repair result, shown in the pause menu so the
    /// repair action never feels like a silent no-op.
    pub last_repair_report: Option<WorldRepairReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditedChunkOverride {
    pub voxels: Vec<Voxel>,
    #[serde(default)]
    pub materials: Vec<MaterialId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorldRepairReport {
    pub scanned_chunks: usize,
    pub removed_chunks: usize,
    pub refreshed_loaded_chunks: usize,
    pub kept_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorldEditStoreStatus {
    #[default]
    Unchecked,
    Compatible {
        generation_identity: WorldGenerationIdentity,
        edited_chunks: usize,
    },
    Blocked {
        generation_identity: WorldGenerationIdentity,
        /// Stable, bounded evidence value. The detailed diagnostic remains
        /// available for logs/UI but is intentionally not an evidence key.
        reason_code: &'static str,
        detail: String,
    },
}

impl WorldEditStoreStatus {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Compatible { .. } => "compatible",
            Self::Blocked { .. } => "blocked",
        }
    }

    pub const fn edited_chunks(&self) -> Option<usize> {
        match self {
            Self::Compatible { edited_chunks, .. } => Some(*edited_chunks),
            Self::Unchecked | Self::Blocked { .. } => None,
        }
    }

    pub const fn generation_identity(&self) -> Option<WorldGenerationIdentity> {
        match self {
            Self::Compatible {
                generation_identity,
                ..
            }
            | Self::Blocked {
                generation_identity,
                ..
            } => Some(*generation_identity),
            Self::Unchecked => None,
        }
    }

    pub const fn reason_code(&self) -> Option<&'static str> {
        match self {
            Self::Blocked { reason_code, .. } => Some(*reason_code),
            Self::Unchecked | Self::Compatible { .. } => None,
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Blocked { detail, .. } => Some(detail),
            Self::Unchecked | Self::Compatible { .. } => None,
        }
    }

    pub fn is_compatible_with(&self, identity: WorldGenerationIdentity) -> bool {
        matches!(
            self,
            Self::Compatible {
                generation_identity,
                ..
            } if *generation_identity == identity
        )
    }
}

impl EditedChunkOverride {
    fn from_chunk(chunk: &Chunk) -> Self {
        Self {
            voxels: chunk.voxels_vec(),
            materials: chunk.materials_vec(),
        }
    }

    fn into_shared(self) -> Option<(SharedVoxels, SharedMaterials)> {
        if self.voxels.len() != CHUNK_VOLUME {
            return None;
        }
        let mut voxels = [AIR; CHUNK_VOLUME];
        voxels.copy_from_slice(&self.voxels);

        let mut materials = [DEFAULT_MATERIAL; CHUNK_VOLUME];
        if self.materials.len() == CHUNK_VOLUME {
            materials.copy_from_slice(&self.materials);
        }

        Some((std::sync::Arc::new(voxels), std::sync::Arc::new(materials)))
    }
}

/// Persisted edit snapshots are intentionally bounded. A snapshot is a full
/// dense 16³ chunk, so sharing the same ceiling as dense interaction
/// residency prevents a corrupt save folder from allocating without limit.
pub const MAX_EDITED_OVERRIDE_RECORDS: usize = MAX_FULL_CHUNK_RESIDENT;
#[cfg(not(target_arch = "wasm32"))]
const MAX_EDITED_OVERRIDE_FILE_BYTES: u64 = 512 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MAX_EDITED_OVERRIDE_STORE_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const MAX_EDITED_OVERRIDE_MANIFEST_BYTES: u64 = 1024 * 1024;
#[cfg(not(target_arch = "wasm32"))]
const EDITED_OVERRIDE_STORE_SCHEMA_V2: u32 = 2;
#[cfg(not(target_arch = "wasm32"))]
const EDITED_OVERRIDE_STORE_SCHEMA_V3: u32 = 3;
#[cfg(not(target_arch = "wasm32"))]
static EDIT_STORE_TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EditStoreSaveOrderKey {
    storage_scope: String,
    world_claim: String,
    generation_identity: WorldGenerationIdentity,
}

#[derive(Debug, Default)]
struct EditStoreSaveOrderState {
    next_capture_token: u64,
    latest_capture_token: u64,
    latest_committed_token: u64,
}

static EDIT_STORE_SAVE_ORDER: OnceLock<
    Mutex<StdHashMap<EditStoreSaveOrderKey, Arc<Mutex<EditStoreSaveOrderState>>>>,
> = OnceLock::new();

#[derive(Debug)]
pub enum EditedOverrideStoreLoad {
    Compatible {
        overrides: AHashMap<ChunkPos, EditedChunkOverride>,
        manifest: crate::settings::WorldEditManifest,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditedOverrideSaveOutcome {
    Saved(crate::settings::WorldEditManifest),
    Blocked { reason: String },
}

#[derive(Debug)]
enum EditedOverrideCapturePayload {
    Snapshot(AHashMap<ChunkPos, EditedChunkOverride>),
    ValidateExisting,
}

/// Immutable edit snapshot plus the monotonically ordered token reserved at
/// capture time. A background worker must carry this value intact rather than
/// cloning the live world again later.
#[derive(Debug)]
pub struct EditedOverrideSaveCapture {
    token: u64,
    world_name: String,
    generation_identity: WorldGenerationIdentity,
    world_revision: u64,
    payload: EditedOverrideCapturePayload,
    order: Arc<Mutex<EditStoreSaveOrderState>>,
    #[cfg(not(target_arch = "wasm32"))]
    saves_root: PathBuf,
}

/// Proof that the newest accepted capture reached the edit-store publication
/// boundary and every dependent write supplied by the caller also succeeded.
#[derive(Debug, Clone)]
pub struct EditedOverrideSaveReceipt {
    pub manifest: crate::settings::WorldEditManifest,
    token: u64,
    world_revision: u64,
    order: Arc<Mutex<EditStoreSaveOrderState>>,
}

impl EditedOverrideSaveReceipt {
    pub fn token(&self) -> u64 {
        self.token
    }

    pub fn is_latest_confirmed(&self) -> bool {
        let order = self
            .order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        order.latest_capture_token == self.token && order.latest_committed_token == self.token
    }
}

#[derive(Debug)]
pub enum OrderedEditedOverrideSaveOutcome {
    Committed(EditedOverrideSaveReceipt),
    Superseded {
        capture_token: u64,
        latest_capture_token: u64,
    },
    AuthorityBlocked {
        reason: String,
    },
    DependentWriteFailed {
        manifest: crate::settings::WorldEditManifest,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg(not(target_arch = "wasm32"))]
#[serde(deny_unknown_fields)]
struct EditedChunkFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation_identity: Option<WorldGenerationIdentity>,
    pos: ChunkPos,
    data: EditedChunkOverride,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg(not(target_arch = "wasm32"))]
#[serde(deny_unknown_fields)]
struct EditedChunkStoreManifestVersioned {
    schema: u32,
    generation_identity: WorldGenerationIdentity,
    edited_chunks: usize,
    last_saved_epoch: u64,
    records: Vec<EditedChunkStoreRecordVersioned>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg(not(target_arch = "wasm32"))]
#[serde(deny_unknown_fields)]
struct EditedChunkStoreRecordVersioned {
    pos: ChunkPos,
    file_name: String,
    byte_len: u64,
    content_checksum_fnv1a64: u64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
struct VersionedEditStoreSpec {
    grammar: TerrainGrammarVersion,
    schema: u32,
    namespace: &'static str,
}

#[cfg(not(target_arch = "wasm32"))]
impl VersionedEditStoreSpec {
    const V2: Self = Self {
        grammar: TerrainGrammarVersion::V2,
        schema: EDITED_OVERRIDE_STORE_SCHEMA_V2,
        namespace: "grammar_v2",
    };
    const V3: Self = Self {
        grammar: TerrainGrammarVersion::V3,
        schema: EDITED_OVERRIDE_STORE_SCHEMA_V3,
        namespace: "grammar_v3",
    };

    const fn for_grammar(grammar: TerrainGrammarVersion) -> Option<Self> {
        match grammar {
            TerrainGrammarVersion::V1 => None,
            TerrainGrammarVersion::V2 => Some(Self::V2),
            TerrainGrammarVersion::V3 => Some(Self::V3),
        }
    }

    const fn version_label(self) -> &'static str {
        match self.grammar {
            TerrainGrammarVersion::V1 => "V1",
            TerrainGrammarVersion::V2 => "V2",
            TerrainGrammarVersion::V3 => "V3",
        }
    }
}

/// The main-menu preflight owns the loaded authority until `OnEnter(InGame)`.
/// This prevents a second filesystem read from observing a different snapshot
/// after the menu has already approved the transition.
#[derive(Resource, Default)]
pub struct PendingEditedOverrideStore {
    world_name: Option<String>,
    generation_identity: Option<WorldGenerationIdentity>,
    compatible: Option<(
        AHashMap<ChunkPos, EditedChunkOverride>,
        crate::settings::WorldEditManifest,
    )>,
}

impl PendingEditedOverrideStore {
    pub fn clear(&mut self) {
        self.world_name = None;
        self.generation_identity = None;
        self.compatible = None;
    }

    pub fn prepare(
        &mut self,
        world_name: &str,
        generation_identity: WorldGenerationIdentity,
        load: EditedOverrideStoreLoad,
    ) -> Result<(), String> {
        self.clear();
        match load {
            EditedOverrideStoreLoad::Compatible {
                overrides,
                manifest,
            } => {
                self.world_name = Some(world_name.to_owned());
                self.generation_identity = Some(generation_identity);
                self.compatible = Some((overrides, manifest));
                Ok(())
            }
            EditedOverrideStoreLoad::Blocked { reason } => Err(reason),
        }
    }

    fn take(
        &mut self,
        world_name: &str,
        generation_identity: WorldGenerationIdentity,
    ) -> Option<(
        AHashMap<ChunkPos, EditedChunkOverride>,
        crate::settings::WorldEditManifest,
    )> {
        if self.world_name.as_deref() != Some(world_name)
            || self.generation_identity != Some(generation_identity)
        {
            self.clear();
            return None;
        }
        self.world_name = None;
        self.generation_identity = None;
        self.compatible.take()
    }
}

/// Prepare the immutable metadata and edit authority before a non-menu entry
/// point is allowed to insert `ActiveWorld` or request `InGame`.
///
/// A fresh name publishes metadata and one empty grammar-matched snapshot.
/// An existing claim is either loaded exactly (when explicitly allowed) or
/// rejected; it is never overwritten with an empty snapshot.
pub fn prepare_programmatic_world_entry(
    proposed: &crate::settings::WorldMeta,
    allow_existing_exact: bool,
    pending: &mut PendingEditedOverrideStore,
) -> Result<crate::settings::WorldMeta, String> {
    pending.clear();
    let claim_key = crate::settings::world_storage_claim_key(&proposed.name);
    let claimed = crate::settings::reserved_world_storage_stems().contains(&claim_key);
    if claimed {
        if !allow_existing_exact {
            return Err(format!(
                "world storage identity '{}' is already reserved",
                proposed.name
            ));
        }
        let existing = crate::settings::list_worlds()
            .into_iter()
            .find(|meta| {
                meta.name == proposed.name
                    && meta.generation_identity() == proposed.generation_identity()
            })
            .ok_or_else(|| {
                format!(
                    "reserved world '{}' is not an exact decodable generation identity",
                    proposed.name
                )
            })?;
        let identity = existing.generation_identity();
        let load = load_edited_overrides_for_world(&existing.name, identity);
        pending.prepare(&existing.name, identity, load)?;
        return Ok(existing);
    }

    crate::settings::save_world(proposed)?;
    match save_edited_overrides_snapshot(
        &proposed.name,
        proposed.generation_identity(),
        AHashMap::new(),
    ) {
        EditedOverrideSaveOutcome::Saved(manifest) if manifest.edited_chunks == 0 => {}
        EditedOverrideSaveOutcome::Saved(manifest) => {
            return Err(format!(
                "fresh world edit authority unexpectedly contains {} chunks",
                manifest.edited_chunks
            ));
        }
        EditedOverrideSaveOutcome::Blocked { reason } => return Err(reason),
    }
    let identity = proposed.generation_identity();
    let load = load_edited_overrides_for_world(&proposed.name, identity);
    pending.prepare(&proposed.name, identity, load)?;
    Ok(proposed.clone())
}

pub fn save_edited_overrides_for_world(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world: &VoxelWorld,
) -> EditedOverrideSaveOutcome {
    let capture = capture_edited_overrides_for_world(world_name, generation_identity, world);
    ordered_outcome_to_legacy(commit_edited_override_capture_with(capture, |_| Ok(())))
}

pub fn save_edited_overrides_snapshot(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> EditedOverrideSaveOutcome {
    let capture = capture_edited_overrides_snapshot(world_name, generation_identity, overrides);
    ordered_outcome_to_legacy(commit_edited_override_capture_with(capture, |_| Ok(())))
}

/// Capture a full live-world edit snapshot and reserve its publication order
/// before any asynchronous work begins.
pub fn capture_edited_overrides_for_world(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world: &VoxelWorld,
) -> EditedOverrideSaveCapture {
    capture_edited_overrides(
        world_name,
        generation_identity,
        world.edit_save_revision,
        EditedOverrideCapturePayload::Snapshot(world.edited_overrides.clone()),
    )
}

/// Reserve the same ordering boundary while only revalidating an existing
/// authority. Bot-only saves use this so an old journal cannot pass a newer
/// edit capture and publish afterward.
pub fn capture_existing_edited_override_authority(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world: &VoxelWorld,
) -> EditedOverrideSaveCapture {
    capture_edited_overrides(
        world_name,
        generation_identity,
        world.edit_save_revision,
        EditedOverrideCapturePayload::ValidateExisting,
    )
}

fn capture_edited_overrides_snapshot(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> EditedOverrideSaveCapture {
    capture_edited_overrides(
        world_name,
        generation_identity,
        0,
        EditedOverrideCapturePayload::Snapshot(overrides),
    )
}

fn capture_edited_overrides(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world_revision: u64,
    payload: EditedOverrideCapturePayload,
) -> EditedOverrideSaveCapture {
    #[cfg(target_arch = "wasm32")]
    let storage_scope = "browser-local-storage".to_owned();

    #[cfg(not(target_arch = "wasm32"))]
    let (storage_scope, saves_root) = {
        let saves_root = PathBuf::from(crate::settings::SAVES_DIR);
        (native_edit_store_scope(&saves_root), saves_root)
    };

    capture_edited_overrides_in_scope(
        storage_scope,
        world_name,
        generation_identity,
        world_revision,
        payload,
        #[cfg(not(target_arch = "wasm32"))]
        saves_root,
    )
}

#[allow(clippy::too_many_arguments)]
fn capture_edited_overrides_in_scope(
    storage_scope: String,
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world_revision: u64,
    payload: EditedOverrideCapturePayload,
    #[cfg(not(target_arch = "wasm32"))] saves_root: PathBuf,
) -> EditedOverrideSaveCapture {
    let key = EditStoreSaveOrderKey {
        storage_scope,
        world_claim: crate::settings::world_storage_claim_key(world_name),
        generation_identity,
    };
    let order = {
        let registry = EDIT_STORE_SAVE_ORDER.get_or_init(|| Mutex::new(StdHashMap::new()));
        let mut registry = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            registry
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(EditStoreSaveOrderState::default()))),
        )
    };
    let token = {
        let mut state = order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_capture_token = state
            .next_capture_token
            .checked_add(1)
            .expect("edit-store capture token exhausted");
        state.latest_capture_token = state.next_capture_token;
        state.next_capture_token
    };
    EditedOverrideSaveCapture {
        token,
        world_name: world_name.to_owned(),
        generation_identity,
        world_revision,
        payload,
        order,
        #[cfg(not(target_arch = "wasm32"))]
        saves_root,
    }
}

/// Commit a captured snapshot and keep the same per-world publication gate
/// held while the caller writes dependent journal/metadata files. Captures
/// made later are therefore either final, or make this one return
/// `Superseded` before it mutates authority.
pub fn commit_edited_override_capture_with(
    capture: EditedOverrideSaveCapture,
    dependent_write: impl FnOnce(&crate::settings::WorldEditManifest) -> Result<(), String>,
) -> OrderedEditedOverrideSaveOutcome {
    let EditedOverrideSaveCapture {
        token,
        world_name,
        generation_identity,
        world_revision,
        payload,
        order,
        #[cfg(not(target_arch = "wasm32"))]
        saves_root,
    } = capture;
    let mut state = order
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if token != state.latest_capture_token {
        return OrderedEditedOverrideSaveOutcome::Superseded {
            capture_token: token,
            latest_capture_token: state.latest_capture_token,
        };
    }

    let edit_outcome = match payload {
        EditedOverrideCapturePayload::Snapshot(overrides) => {
            #[cfg(target_arch = "wasm32")]
            {
                match validate_override_snapshot(&overrides, generation_identity) {
                    Ok(()) => {
                        EditedOverrideSaveOutcome::Saved(crate::settings::WorldEditManifest {
                            edited_chunks: overrides.len(),
                            last_saved_epoch: now_epoch(),
                        })
                    }
                    Err(reason) => EditedOverrideSaveOutcome::Blocked { reason },
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                save_edited_overrides_snapshot_at_unordered(
                    &saves_root,
                    &world_name,
                    generation_identity,
                    overrides,
                )
            }
        }
        EditedOverrideCapturePayload::ValidateExisting => {
            #[cfg(target_arch = "wasm32")]
            {
                validate_edited_override_store_for_world(&world_name, generation_identity)
                    .map(EditedOverrideSaveOutcome::Saved)
                    .unwrap_or_else(|reason| EditedOverrideSaveOutcome::Blocked { reason })
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                match load_edited_overrides_at(&saves_root, &world_name, generation_identity) {
                    EditedOverrideStoreLoad::Compatible { manifest, .. } => {
                        EditedOverrideSaveOutcome::Saved(manifest)
                    }
                    EditedOverrideStoreLoad::Blocked { reason } => {
                        EditedOverrideSaveOutcome::Blocked { reason }
                    }
                }
            }
        }
    };
    let manifest = match edit_outcome {
        EditedOverrideSaveOutcome::Saved(manifest) => manifest,
        EditedOverrideSaveOutcome::Blocked { reason } => {
            return OrderedEditedOverrideSaveOutcome::AuthorityBlocked { reason }
        }
    };
    state.latest_committed_token = token;
    if let Err(reason) = dependent_write(&manifest) {
        return OrderedEditedOverrideSaveOutcome::DependentWriteFailed { manifest, reason };
    }
    drop(state);
    OrderedEditedOverrideSaveOutcome::Committed(EditedOverrideSaveReceipt {
        manifest,
        token,
        world_revision,
        order,
    })
}

fn ordered_outcome_to_legacy(
    outcome: OrderedEditedOverrideSaveOutcome,
) -> EditedOverrideSaveOutcome {
    match outcome {
        OrderedEditedOverrideSaveOutcome::Committed(receipt) => {
            EditedOverrideSaveOutcome::Saved(receipt.manifest)
        }
        OrderedEditedOverrideSaveOutcome::Superseded {
            capture_token,
            latest_capture_token,
        } => EditedOverrideSaveOutcome::Blocked {
            reason: format!(
                "edit snapshot capture {capture_token} was superseded by capture {latest_capture_token}"
            ),
        },
        OrderedEditedOverrideSaveOutcome::AuthorityBlocked { reason }
        | OrderedEditedOverrideSaveOutcome::DependentWriteFailed { reason, .. } => {
            EditedOverrideSaveOutcome::Blocked { reason }
        }
    }
}

pub fn load_edited_overrides_for_world(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
) -> EditedOverrideStoreLoad {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (world_name, generation_identity);
        return EditedOverrideStoreLoad::Compatible {
            overrides: AHashMap::new(),
            manifest: crate::settings::WorldEditManifest::default(),
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        load_edited_overrides_at(
            Path::new(crate::settings::SAVES_DIR),
            world_name,
            generation_identity,
        )
    }
}

/// Revalidate the on-disk edit authority without modifying it. Persistence
/// systems use this before writing adjacent world/bot journals when no edit
/// snapshot itself needs to be rewritten.
pub fn validate_edited_override_store_for_world(
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
) -> Result<crate::settings::WorldEditManifest, String> {
    match load_edited_overrides_for_world(world_name, generation_identity) {
        EditedOverrideStoreLoad::Compatible { manifest, .. } => Ok(manifest),
        EditedOverrideStoreLoad::Blocked { reason } => Err(reason),
    }
}

fn validate_override_snapshot(
    overrides: &AHashMap<ChunkPos, EditedChunkOverride>,
    _generation_identity: WorldGenerationIdentity,
) -> Result<(), String> {
    if overrides.len() > MAX_EDITED_OVERRIDE_RECORDS {
        return Err(format!(
            "edit snapshot has {} chunks; hard limit is {MAX_EDITED_OVERRIDE_RECORDS}",
            overrides.len()
        ));
    }
    for (pos, data) in overrides {
        if data.voxels.len() != CHUNK_VOLUME {
            return Err(format!(
                "edit chunk {:?} has {} voxels; expected {CHUNK_VOLUME}",
                pos,
                data.voxels.len()
            ));
        }
        if !data.materials.is_empty() && data.materials.len() != CHUNK_VOLUME {
            return Err(format!(
                "edit chunk {:?} has {} materials; expected 0 or {CHUNK_VOLUME}",
                pos,
                data.materials.len()
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn save_edited_overrides_snapshot_at(
    saves_root: &Path,
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> EditedOverrideSaveOutcome {
    let capture = capture_edited_overrides_at(
        saves_root,
        world_name,
        generation_identity,
        0,
        EditedOverrideCapturePayload::Snapshot(overrides),
    );
    ordered_outcome_to_legacy(commit_edited_override_capture_with(capture, |_| Ok(())))
}

#[cfg(not(target_arch = "wasm32"))]
fn capture_edited_overrides_at(
    saves_root: &Path,
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    world_revision: u64,
    payload: EditedOverrideCapturePayload,
) -> EditedOverrideSaveCapture {
    capture_edited_overrides_in_scope(
        native_edit_store_scope(saves_root),
        world_name,
        generation_identity,
        world_revision,
        payload,
        saves_root.to_path_buf(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn native_edit_store_scope(saves_root: &Path) -> String {
    let absolute = if saves_root.is_absolute() {
        saves_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(saves_root))
            .unwrap_or_else(|_| saves_root.to_path_buf())
    };
    let scope = absolute.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        scope.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        scope
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_edited_overrides_snapshot_at_unordered(
    saves_root: &Path,
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> EditedOverrideSaveOutcome {
    if let Err(reason) = validate_override_snapshot(&overrides, generation_identity) {
        return EditedOverrideSaveOutcome::Blocked { reason };
    }

    let edits_root = edited_override_root_at(saves_root, world_name);
    let final_dir =
        edited_chunk_dir_at(saves_root, world_name, generation_identity.terrain_grammar);
    if let Err(reason) = ensure_existing_path_is_safe(saves_root, "saves root")
        .and_then(|()| ensure_existing_path_is_safe(&edits_root, "world edit root"))
        .and_then(|()| reject_transaction_debris(&edits_root, generation_identity.terrain_grammar))
    {
        return EditedOverrideSaveOutcome::Blocked { reason };
    }

    if final_dir.exists() {
        let current = load_edited_overrides_at(saves_root, world_name, generation_identity);
        if let EditedOverrideStoreLoad::Blocked { reason } = current {
            return EditedOverrideSaveOutcome::Blocked {
                reason: format!("existing edit authority is blocked: {reason}"),
            };
        }
    } else if let Some(spec) =
        VersionedEditStoreSpec::for_grammar(generation_identity.terrain_grammar)
    {
        let versioned_root = edited_versioned_root_at(saves_root, world_name, spec);
        if versioned_root.exists() {
            return EditedOverrideSaveOutcome::Blocked {
                reason: format!(
                    "{} edit namespace exists without its chunks authority",
                    spec.version_label()
                ),
            };
        }
    }

    if let Err(reason) = cleanup_retired_transaction_snapshot_before_publish(
        &edits_root,
        generation_identity.terrain_grammar,
    ) {
        return EditedOverrideSaveOutcome::Blocked { reason };
    }

    if let Err(e) = fs::create_dir_all(&edits_root) {
        return EditedOverrideSaveOutcome::Blocked {
            reason: format!("could not create world edit root: {e}"),
        };
    }
    if let Err(reason) = ensure_existing_path_is_safe(&edits_root, "world edit root") {
        return EditedOverrideSaveOutcome::Blocked { reason };
    }

    let epoch = now_epoch();
    let summary = crate::settings::WorldEditManifest {
        edited_chunks: overrides.len(),
        last_saved_epoch: epoch,
    };
    let result = match generation_identity.terrain_grammar {
        TerrainGrammarVersion::V1 => {
            write_v1_snapshot_transaction(&edits_root, &final_dir, generation_identity, overrides)
        }
        TerrainGrammarVersion::V2 | TerrainGrammarVersion::V3 => {
            let spec = VersionedEditStoreSpec::for_grammar(generation_identity.terrain_grammar)
                .expect("V2/V3 grammar has a versioned edit-store specification");
            write_versioned_snapshot_transaction(
                &edits_root,
                &final_dir,
                generation_identity,
                overrides,
                epoch,
                spec,
            )
        }
    };
    match result {
        Ok(()) => EditedOverrideSaveOutcome::Saved(summary),
        Err(reason) => EditedOverrideSaveOutcome::Blocked { reason },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_edited_overrides_at(
    saves_root: &Path,
    world_name: &str,
    generation_identity: WorldGenerationIdentity,
) -> EditedOverrideStoreLoad {
    let edits_root = edited_override_root_at(saves_root, world_name);
    if let Err(reason) = ensure_existing_path_is_safe(saves_root, "saves root")
        .and_then(|()| ensure_existing_path_is_safe(&edits_root, "world edit root"))
        .and_then(|()| reject_transaction_debris(&edits_root, generation_identity.terrain_grammar))
    {
        return EditedOverrideStoreLoad::Blocked { reason };
    }
    let result = match generation_identity.terrain_grammar {
        TerrainGrammarVersion::V1 => {
            let dir = edited_chunk_dir_at(saves_root, world_name, TerrainGrammarVersion::V1);
            load_v1_chunk_dir(&dir)
        }
        TerrainGrammarVersion::V2 | TerrainGrammarVersion::V3 => {
            let spec = VersionedEditStoreSpec::for_grammar(generation_identity.terrain_grammar)
                .expect("V2/V3 grammar has a versioned edit-store specification");
            let root = edited_versioned_root_at(saves_root, world_name, spec);
            load_versioned_store_root(&root, generation_identity, spec)
        }
    };
    match result {
        Ok((overrides, manifest)) => EditedOverrideStoreLoad::Compatible {
            overrides,
            manifest,
        },
        Err(reason) => EditedOverrideStoreLoad::Blocked { reason },
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn load_v1_chunk_dir(
    dir: &Path,
) -> Result<
    (
        AHashMap<ChunkPos, EditedChunkOverride>,
        crate::settings::WorldEditManifest,
    ),
    String,
> {
    ensure_existing_path_is_safe(dir, "V1 edit chunk directory")?;
    let metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                AHashMap::new(),
                crate::settings::WorldEditManifest::default(),
            ));
        }
        Err(error) => {
            return Err(format!(
                "could not inspect V1 edit chunk directory: {error}"
            ));
        }
    };
    if !metadata.file_type().is_dir() {
        return Err("V1 edit chunk path is not a directory".to_owned());
    }

    let mut paths = exact_directory_files(dir, None)?;
    if paths.len() > MAX_EDITED_OVERRIDE_RECORDS {
        return Err(format!(
            "V1 edit store has {} records; hard limit is {MAX_EDITED_OVERRIDE_RECORDS}",
            paths.len()
        ));
    }
    paths.sort();
    let mut total_bytes = 0_u64;
    let mut out = AHashMap::with_capacity(paths.len());
    for path in paths {
        let bytes =
            read_bounded_regular_file(&path, MAX_EDITED_OVERRIDE_FILE_BYTES, "V1 edit chunk")?;
        total_bytes = checked_store_bytes(total_bytes, bytes.len() as u64)?;
        let record: EditedChunkFile = ron::de::from_bytes(&bytes)
            .map_err(|e| format!("could not parse V1 edit chunk {}: {e}", path.display()))?;
        if record.schema.is_some() || record.generation_identity.is_some() {
            return Err(format!(
                "V1 edit chunk {} carries non-legacy provenance",
                path.display()
            ));
        }
        validate_loaded_record(&path, &record, TerrainGrammarVersion::V1)?;
        if out.insert(record.pos, record.data).is_some() {
            return Err(format!("duplicate V1 edit chunk position {:?}", record.pos));
        }
    }
    let edited_chunks = out.len();
    Ok((
        out,
        crate::settings::WorldEditManifest {
            edited_chunks,
            last_saved_epoch: 0,
        },
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_versioned_store_root(
    root: &Path,
    expected_identity: WorldGenerationIdentity,
    spec: VersionedEditStoreSpec,
) -> Result<
    (
        AHashMap<ChunkPos, EditedChunkOverride>,
        crate::settings::WorldEditManifest,
    ),
    String,
> {
    ensure_existing_path_is_safe(root, "versioned edit store")?;
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} edit store manifest is missing",
                spec.version_label()
            ));
        }
        Err(error) => {
            return Err(format!(
                "could not inspect {} edit store: {error}",
                spec.version_label()
            ));
        }
    };
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "{} edit store path is not a directory",
            spec.version_label()
        ));
    }
    validate_versioned_root_entries(root, spec)?;

    let manifest_path = root.join("manifest.ron");
    let manifest_bytes = read_bounded_regular_file(
        &manifest_path,
        MAX_EDITED_OVERRIDE_MANIFEST_BYTES,
        "versioned edit store manifest",
    )?;
    let manifest: EditedChunkStoreManifestVersioned = ron::de::from_bytes(&manifest_bytes)
        .map_err(|e| {
            format!(
                "could not parse {} edit store manifest: {e}",
                spec.version_label()
            )
        })?;
    validate_versioned_manifest(&manifest, expected_identity, spec)?;

    let chunks_dir = root.join("chunks");
    ensure_existing_path_is_safe(&chunks_dir, "versioned edit chunk directory")?;
    let actual_paths = exact_directory_files(&chunks_dir, None)?;
    if actual_paths.len() != manifest.records.len() {
        return Err(format!(
            "{} edit record set mismatch: manifest={}, directory={}",
            spec.version_label(),
            manifest.records.len(),
            actual_paths.len()
        ));
    }
    let actual_names: AHashSet<String> = actual_paths
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let expected_names: AHashSet<String> = manifest
        .records
        .iter()
        .map(|record| record.file_name.clone())
        .collect();
    if actual_names != expected_names {
        return Err(format!(
            "{} edit record paths do not match the manifest",
            spec.version_label()
        ));
    }

    let mut total_bytes = 0_u64;
    let mut out = AHashMap::with_capacity(manifest.records.len());
    for expected in &manifest.records {
        let path = chunks_dir.join(&expected.file_name);
        let bytes = read_bounded_regular_file(
            &path,
            MAX_EDITED_OVERRIDE_FILE_BYTES,
            "versioned edit chunk",
        )?;
        if bytes.len() as u64 != expected.byte_len {
            return Err(format!(
                "{} edit chunk {} byte length does not match manifest",
                spec.version_label(),
                expected.file_name
            ));
        }
        if fnv1a64(&bytes) != expected.content_checksum_fnv1a64 {
            return Err(format!(
                "{} edit chunk {} checksum does not match manifest",
                spec.version_label(),
                expected.file_name
            ));
        }
        total_bytes = checked_store_bytes(total_bytes, bytes.len() as u64)?;
        let record: EditedChunkFile = ron::de::from_bytes(&bytes).map_err(|e| {
            format!(
                "could not parse {} edit chunk {}: {e}",
                spec.version_label(),
                expected.file_name
            )
        })?;
        if record.schema != Some(spec.schema) {
            return Err(format!(
                "{} edit chunk {} has an unsupported schema",
                spec.version_label(),
                expected.file_name
            ));
        }
        if record.generation_identity != Some(expected_identity) {
            return Err(format!(
                "{} edit chunk {} belongs to a different generation identity",
                spec.version_label(),
                expected.file_name
            ));
        }
        if record.pos != expected.pos {
            return Err(format!(
                "{} edit chunk {} position does not match manifest",
                spec.version_label(),
                expected.file_name
            ));
        }
        validate_loaded_record(&path, &record, spec.grammar)?;
        if out.insert(record.pos, record.data).is_some() {
            return Err(format!(
                "duplicate {} edit chunk position {:?}",
                spec.version_label(),
                record.pos
            ));
        }
    }
    Ok((
        out,
        crate::settings::WorldEditManifest {
            edited_chunks: manifest.edited_chunks,
            last_saved_epoch: manifest.last_saved_epoch,
        },
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_versioned_manifest(
    manifest: &EditedChunkStoreManifestVersioned,
    expected_identity: WorldGenerationIdentity,
    spec: VersionedEditStoreSpec,
) -> Result<(), String> {
    if manifest.schema != spec.schema {
        return Err(format!(
            "unsupported {} edit store schema {}",
            spec.version_label(),
            manifest.schema
        ));
    }
    if manifest.generation_identity != expected_identity {
        return Err(format!(
            "{} edit store belongs to a different generation identity",
            spec.version_label()
        ));
    }
    if manifest.edited_chunks != manifest.records.len() {
        return Err(format!(
            "{} edit manifest count does not match its record list",
            spec.version_label()
        ));
    }
    if manifest.records.len() > MAX_EDITED_OVERRIDE_RECORDS {
        return Err(format!(
            "{} edit store has {} records; hard limit is {MAX_EDITED_OVERRIDE_RECORDS}",
            spec.version_label(),
            manifest.records.len()
        ));
    }
    let mut previous = None::<(i32, i32, i32)>;
    let mut names = AHashSet::with_capacity(manifest.records.len());
    for record in &manifest.records {
        let key = chunk_pos_key(record.pos);
        if previous.is_some_and(|prior| key <= prior) {
            return Err(format!(
                "{} edit manifest positions are duplicate or not canonical",
                spec.version_label()
            ));
        }
        previous = Some(key);
        let canonical = edited_chunk_file_name(record.pos);
        if record.file_name != canonical || !is_single_normal_path_component(&record.file_name) {
            return Err(format!(
                "{} edit manifest contains a non-canonical record path {}",
                spec.version_label(),
                record.file_name
            ));
        }
        if !names.insert(record.file_name.clone()) {
            return Err(format!(
                "{} edit manifest contains a duplicate record path",
                spec.version_label()
            ));
        }
        if record.byte_len > MAX_EDITED_OVERRIDE_FILE_BYTES {
            return Err(format!(
                "{} edit chunk {} exceeds the per-record byte limit",
                spec.version_label(),
                record.file_name
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn write_v1_snapshot_transaction(
    transaction_parent: &Path,
    final_dir: &Path,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> Result<(), String> {
    debug_assert_eq!(
        generation_identity.terrain_grammar,
        TerrainGrammarVersion::V1
    );
    publish_directory_transaction(transaction_parent, final_dir, "chunks", |stage| {
        fs::create_dir(stage)
            .map_err(|e| format!("could not create V1 edit staging directory: {e}"))?;
        let mut entries: Vec<_> = overrides.into_iter().collect();
        entries.sort_by_key(|(pos, _)| chunk_pos_key(*pos));
        for (pos, data) in entries {
            let record = EditedChunkFile {
                schema: None,
                generation_identity: None,
                pos,
                data,
            };
            let text = ron::ser::to_string_pretty(&record, ron::ser::PrettyConfig::default())
                .map_err(|e| format!("could not serialize V1 edit chunk {:?}: {e}", pos))?;
            if text.len() as u64 > MAX_EDITED_OVERRIDE_FILE_BYTES {
                return Err(format!(
                    "serialized V1 edit chunk {:?} exceeds byte limit",
                    pos
                ));
            }
            write_new_text(&stage.join(edited_chunk_file_name(pos)), &text)?;
        }
        load_v1_chunk_dir(stage).map(|_| ())
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn write_versioned_snapshot_transaction(
    transaction_parent: &Path,
    final_chunks_dir: &Path,
    generation_identity: WorldGenerationIdentity,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
    last_saved_epoch: u64,
    spec: VersionedEditStoreSpec,
) -> Result<(), String> {
    debug_assert_eq!(generation_identity.terrain_grammar, spec.grammar);
    let final_root = final_chunks_dir.parent().ok_or_else(|| {
        format!(
            "{} edit store has no namespace parent",
            spec.version_label()
        )
    })?;
    publish_directory_transaction(
        transaction_parent,
        final_root,
        spec.namespace,
        |stage_root| {
            let stage_chunks = stage_root.join("chunks");
            fs::create_dir_all(&stage_chunks).map_err(|e| {
                format!(
                    "could not create {} edit staging directory: {e}",
                    spec.version_label()
                )
            })?;
            let mut entries: Vec<_> = overrides.into_iter().collect();
            entries.sort_by_key(|(pos, _)| chunk_pos_key(*pos));
            let mut records = Vec::with_capacity(entries.len());
            let mut total_bytes = 0_u64;
            for (pos, data) in entries {
                let record = EditedChunkFile {
                    schema: Some(spec.schema),
                    generation_identity: Some(generation_identity),
                    pos,
                    data,
                };
                let text = ron::ser::to_string_pretty(&record, ron::ser::PrettyConfig::default())
                    .map_err(|e| {
                    format!(
                        "could not serialize {} edit chunk {:?}: {e}",
                        spec.version_label(),
                        pos
                    )
                })?;
                if text.len() as u64 > MAX_EDITED_OVERRIDE_FILE_BYTES {
                    return Err(format!(
                        "serialized {} edit chunk {:?} exceeds byte limit",
                        spec.version_label(),
                        pos
                    ));
                }
                total_bytes = checked_store_bytes(total_bytes, text.len() as u64)?;
                let file_name = edited_chunk_file_name(pos);
                write_new_text(&stage_chunks.join(&file_name), &text)?;
                records.push(EditedChunkStoreRecordVersioned {
                    pos,
                    file_name,
                    byte_len: text.len() as u64,
                    content_checksum_fnv1a64: fnv1a64(text.as_bytes()),
                });
            }
            let manifest = EditedChunkStoreManifestVersioned {
                schema: spec.schema,
                generation_identity,
                edited_chunks: records.len(),
                last_saved_epoch,
                records,
            };
            let text = ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::default())
                .map_err(|e| {
                    format!(
                        "could not serialize {} edit manifest: {e}",
                        spec.version_label()
                    )
                })?;
            if text.len() as u64 > MAX_EDITED_OVERRIDE_MANIFEST_BYTES {
                return Err(format!(
                    "serialized {} edit manifest exceeds byte limit",
                    spec.version_label()
                ));
            }
            write_new_text(&stage_root.join("manifest.ron"), &text)?;
            load_versioned_store_root(stage_root, generation_identity, spec).map(|_| ())
        },
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn publish_directory_transaction(
    transaction_parent: &Path,
    final_dir: &Path,
    label: &str,
    build: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let id = EDIT_STORE_TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
    let nonce = format!("{}-{id}", std::process::id());
    let stage = transaction_parent.join(format!(".{label}.stage-{nonce}"));
    let previous = transaction_parent.join(format!(".{label}.previous-{nonce}"));
    if stage.exists() || previous.exists() {
        return Err("edit-store transaction paths already exist".to_owned());
    }

    if let Err(reason) = build(&stage) {
        remove_owned_transaction_dir(&stage);
        return Err(reason);
    }
    sync_directory_best_effort(&stage);

    let had_previous = final_dir.exists();
    if had_previous {
        if let Err(error) = fs::rename(final_dir, &previous) {
            remove_owned_transaction_dir(&stage);
            return Err(format!("could not park previous edit snapshot: {error}"));
        }
    }
    if let Err(e) = fs::rename(&stage, final_dir) {
        if had_previous {
            let _ = fs::rename(&previous, final_dir);
        }
        remove_owned_transaction_dir(&stage);
        return Err(format!("could not publish edit snapshot: {e}"));
    }
    sync_directory_best_effort(transaction_parent);

    if had_previous {
        if let Err(error) = fs::remove_dir_all(&previous) {
            // The new, fully validated directory is already authoritative at
            // this point. Reporting Blocked would falsely imply that no bytes
            // were published. A single exact `.previous-*` directory is
            // therefore treated as bounded retired debris by the loader; a
            // second one still fails closed.
            warn!(
                "published edit snapshot but could not retire old snapshot '{}': {error}",
                previous.display()
            );
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn remove_owned_transaction_dir(path: &Path) {
    if path.exists() {
        let _ = fs::remove_dir_all(path);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_new_text(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    file.sync_all()
        .map_err(|e| format!("could not sync {}: {e}", path.display()))
}

#[cfg(not(target_arch = "wasm32"))]
fn sync_directory_best_effort(path: &Path) {
    if let Ok(dir) = fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_loaded_record(
    path: &Path,
    record: &EditedChunkFile,
    grammar: TerrainGrammarVersion,
) -> Result<(), String> {
    let expected_name = edited_chunk_file_name(record.pos);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err(format!(
            "edit chunk path {} does not match its position {:?}",
            path.display(),
            record.pos
        ));
    }
    validate_override_snapshot(
        &AHashMap::from([(record.pos, record.data.clone())]),
        WorldGenerationIdentity {
            seed: 0,
            world_profile: crate::settings::WorldProfile::Natural,
            scenery_quality: crate::settings::SceneryQuality::Off,
            terrain_grammar: grammar,
        },
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn exact_directory_files(
    dir: &Path,
    allowed_non_file: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    let read = fs::read_dir(dir)
        .map_err(|e| format!("could not enumerate edit directory {}: {e}", dir.display()))?;
    let mut paths = Vec::new();
    for entry in read {
        let entry = entry
            .map_err(|e| format!("could not enumerate edit directory {}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| "edit store contains a non-Unicode path".to_owned())?;
        let kind = entry
            .file_type()
            .map_err(|e| format!("could not inspect edit path {}: {e}", path.display()))?;
        if allowed_non_file == Some(name) {
            continue;
        }
        if !kind.is_file()
            || kind.is_symlink()
            || path.extension().and_then(|e| e.to_str()) != Some("ron")
        {
            return Err(format!("unexpected edit-store path {}", path.display()));
        }
        ensure_existing_path_is_safe(&path, "edit record")?;
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_versioned_root_entries(
    root: &Path,
    spec: VersionedEditStoreSpec,
) -> Result<(), String> {
    let mut saw_manifest = false;
    let mut saw_chunks = false;
    let read = fs::read_dir(root).map_err(|e| {
        format!(
            "could not enumerate {} edit store: {e}",
            spec.version_label()
        )
    })?;
    for entry in read {
        let entry = entry.map_err(|e| {
            format!(
                "could not enumerate {} edit store: {e}",
                spec.version_label()
            )
        })?;
        let path = entry.path();
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| {
                format!(
                    "{} edit store contains a non-Unicode path",
                    spec.version_label()
                )
            })?
            .to_owned();
        let kind = entry.file_type().map_err(|e| {
            format!(
                "could not inspect {} edit path {}: {e}",
                spec.version_label(),
                path.display()
            )
        })?;
        match name.as_str() {
            "manifest.ron" if kind.is_file() && !kind.is_symlink() => saw_manifest = true,
            "chunks" if kind.is_dir() && !kind.is_symlink() => saw_chunks = true,
            _ => {
                return Err(format!(
                    "unexpected {} edit-store path {}",
                    spec.version_label(),
                    path.display()
                ))
            }
        }
        ensure_existing_path_is_safe(&path, "versioned edit-store entry")?;
    }
    if !saw_manifest || !saw_chunks {
        return Err(format!(
            "{} edit store requires exactly manifest.ron and chunks/",
            spec.version_label()
        ));
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn read_bounded_regular_file(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>, String> {
    ensure_existing_path_is_safe(path, label)?;
    let metadata = fs::metadata(path)
        .map_err(|e| format!("could not inspect {label} {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{label} {} is not a regular file", path.display()));
    }
    if metadata.len() > max_bytes {
        return Err(format!("{label} {} exceeds byte limit", path.display()));
    }
    let bytes =
        fs::read(path).map_err(|e| format!("could not read {label} {}: {e}", path.display()))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("{label} {} changed while reading", path.display()));
    }
    Ok(bytes)
}

#[cfg(not(target_arch = "wasm32"))]
fn checked_store_bytes(current: u64, additional: u64) -> Result<u64, String> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| "edit-store byte accounting overflowed".to_owned())?;
    if total > MAX_EDITED_OVERRIDE_STORE_BYTES {
        return Err(format!(
            "edit store exceeds {} byte hard limit",
            MAX_EDITED_OVERRIDE_STORE_BYTES
        ));
    }
    Ok(total)
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_existing_path_is_safe(path: &Path, label: &str) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not inspect {label} {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
        return Err(format!(
            "{label} {} is a symlink or reparse point",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn reject_transaction_debris(
    edits_root: &Path,
    grammar: TerrainGrammarVersion,
) -> Result<(), String> {
    ensure_existing_path_is_safe(edits_root, "edit root")?;
    let root_metadata = match fs::symlink_metadata(edits_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect edit root: {error}")),
    };
    if !root_metadata.file_type().is_dir() {
        return Err(format!(
            "edit root {} is not a directory",
            edits_root.display()
        ));
    }
    let label = match grammar {
        TerrainGrammarVersion::V1 => "chunks",
        TerrainGrammarVersion::V2 => "grammar_v2",
        TerrainGrammarVersion::V3 => "grammar_v3",
    };
    let stage_prefix = format!(".{label}.stage-");
    let previous_prefix = format!(".{label}.previous-");
    let final_dir = match grammar {
        TerrainGrammarVersion::V1 => edits_root.join("chunks"),
        TerrainGrammarVersion::V2 => edits_root.join("grammar_v2"),
        TerrainGrammarVersion::V3 => edits_root.join("grammar_v3"),
    };
    let mut retired_previous = 0usize;
    let read = fs::read_dir(edits_root)
        .map_err(|e| format!("could not inspect edit transaction state: {e}"))?;
    for entry in read {
        let entry = entry.map_err(|e| format!("could not inspect edit transaction state: {e}"))?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "edit root contains a non-Unicode path".to_owned())?
            .to_owned();
        reject_casefold_edit_namespace_alias(&name)?;
        if name.starts_with(&stage_prefix) {
            return Err(format!(
                "unfinished {label} edit-store transaction is present"
            ));
        }
        if name.starts_with(&previous_prefix) {
            let path = entry.path();
            ensure_existing_path_is_safe(&path, "retired edit snapshot")?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "could not inspect retired edit snapshot {}: {error}",
                    path.display()
                )
            })?;
            if !metadata.file_type().is_dir() {
                return Err(format!(
                    "retired edit snapshot {} is not a directory",
                    path.display()
                ));
            }
            retired_previous = retired_previous.saturating_add(1);
        }
    }
    if retired_previous > 0 && !final_dir.exists() {
        return Err(format!(
            "retired {label} edit snapshot exists without a published authority"
        ));
    }
    if retired_previous > 1 {
        return Err(format!(
            "multiple retired {label} edit snapshots exceed the bounded recovery contract"
        ));
    }
    if retired_previous == 1 {
        warn!(
            "using validated {label} edit authority with one bounded retired snapshot pending cleanup"
        );
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn reject_casefold_edit_namespace_alias(name: &str) -> Result<(), String> {
    for namespace in ["chunks", "grammar_v2", "grammar_v3"] {
        if name.eq_ignore_ascii_case(namespace) && name != namespace {
            return Err(format!(
                "edit namespace '{name}' is a case-only alias of '{namespace}'"
            ));
        }
        for marker in [
            format!(".{namespace}.stage-"),
            format!(".{namespace}.previous-"),
        ] {
            if name.to_ascii_lowercase().starts_with(&marker) && !name.starts_with(&marker) {
                return Err(format!(
                    "edit transaction path '{name}' is a non-canonical case alias"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn cleanup_retired_transaction_snapshot_before_publish(
    edits_root: &Path,
    grammar: TerrainGrammarVersion,
) -> Result<(), String> {
    let label = match grammar {
        TerrainGrammarVersion::V1 => "chunks",
        TerrainGrammarVersion::V2 => "grammar_v2",
        TerrainGrammarVersion::V3 => "grammar_v3",
    };
    let previous_prefix = format!(".{label}.previous-");
    let metadata = match fs::symlink_metadata(edits_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not inspect edit root: {error}")),
    };
    if !metadata.file_type().is_dir() {
        return Err("edit root is not a directory".to_owned());
    }
    let mut retired = None::<PathBuf>;
    for entry in fs::read_dir(edits_root)
        .map_err(|error| format!("could not inspect retired edit snapshots: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("could not inspect retired edit snapshots: {error}"))?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| "edit root contains a non-Unicode path".to_owned())?
            .to_owned();
        if !name.starts_with(&previous_prefix) {
            continue;
        }
        if retired.is_some() {
            return Err(format!(
                "multiple retired {label} snapshots block a new transaction"
            ));
        }
        let path = entry.path();
        ensure_existing_path_is_safe(&path, "retired edit snapshot")?;
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "could not inspect retired edit snapshot {}: {error}",
                path.display()
            )
        })?;
        if !metadata.file_type().is_dir() {
            return Err(format!(
                "retired edit snapshot {} is not a directory",
                path.display()
            ));
        }
        retired = Some(path);
    }
    if let Some(path) = retired {
        fs::remove_dir_all(&path).map_err(|error| {
            format!(
                "could not retire validated edit snapshot {} before publication: {error}",
                path.display()
            )
        })?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err(format!(
                "retired edit snapshot {} still exists after cleanup",
                path.display()
            ));
        }
        sync_directory_best_effort(edits_root);
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_override_root_at(saves_root: &Path, world_name: &str) -> PathBuf {
    saves_root.join(format!(
        "{}_edits",
        crate::settings::world_storage_stem(world_name)
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_versioned_root_at(
    saves_root: &Path,
    world_name: &str,
    spec: VersionedEditStoreSpec,
) -> PathBuf {
    edited_override_root_at(saves_root, world_name).join(spec.namespace)
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_v2_root_at(saves_root: &Path, world_name: &str) -> PathBuf {
    edited_versioned_root_at(saves_root, world_name, VersionedEditStoreSpec::V2)
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_v3_root_at(saves_root: &Path, world_name: &str) -> PathBuf {
    edited_versioned_root_at(saves_root, world_name, VersionedEditStoreSpec::V3)
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_chunk_dir_at(
    saves_root: &Path,
    world_name: &str,
    grammar: TerrainGrammarVersion,
) -> PathBuf {
    match grammar {
        TerrainGrammarVersion::V1 => edited_override_root_at(saves_root, world_name).join("chunks"),
        TerrainGrammarVersion::V2 => edited_v2_root_at(saves_root, world_name).join("chunks"),
        TerrainGrammarVersion::V3 => edited_v3_root_at(saves_root, world_name).join("chunks"),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_chunk_file_name(pos: ChunkPos) -> String {
    format!("{}_{}_{}.ron", pos.x, pos.y, pos.z)
}

#[cfg(not(target_arch = "wasm32"))]
fn is_single_normal_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

#[cfg(not(target_arch = "wasm32"))]
fn chunk_pos_key(pos: ChunkPos) -> (i32, i32, i32) {
    (pos.x, pos.y, pos.z)
}

#[cfg(not(target_arch = "wasm32"))]
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn override_looks_like_visual_artifact(
    pos: ChunkPos,
    data: &EditedChunkOverride,
    generator: &TerrainGenerator,
) -> bool {
    if data.voxels.len() != CHUNK_VOLUME {
        return false;
    }

    let wx = pos.x * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    let wz = pos.z * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    let biome = generator.biome_at(wx, wz);
    if biome.is_showcase_terrain() {
        return false;
    }

    let mut non_air = 0usize;
    let mut showcase = 0usize;
    let mut cold = 0usize;
    for &voxel in &data.voxels {
        if voxel == AIR {
            continue;
        }
        non_air += 1;
        match BlockType::from_voxel(voxel) {
            BlockType::Crystal
            | BlockType::LuminiteCrystal
            | BlockType::MagnetiteOre
            | BlockType::IridiumVein
            | BlockType::AlienMoss
            | BlockType::BoneRock
            | BlockType::GlowSand
            | BlockType::Basalt
            | BlockType::Lava => showcase += 1,
            BlockType::Snow | BlockType::Ice => cold += 1,
            _ => {}
        }
    }

    if non_air < 64 {
        return false;
    }

    let showcase_ratio = showcase as f32 / non_air as f32;
    let cold_ratio = cold as f32 / non_air as f32;
    showcase_ratio >= 0.35
        || (cold_ratio >= 0.72
            && !matches!(
                biome,
                crate::terrain::Biome::SnowyMountains
                    | crate::terrain::Biome::Tundra
                    | crate::terrain::Biome::Ocean
            ))
}

fn now_epoch() -> u64 {
    crate::platform::now_epoch()
}

/// Live render-distance governor. `WorldSettings::render_distance` stays
/// the player's desired horizon; this resource tracks the distance the
/// machine can currently afford without stalling chunk generation,
/// meshing, or GPU uploads.
#[derive(Resource, Debug, Clone)]
pub struct StreamingGovernor {
    pub enabled: bool,
    pub profile: RuntimeProfile,
    pub intent: RuntimeIntent,
    pub quality: QualityState,
    pub target_render_distance: i32,
    pub effective_render_distance: i32,
    pub smoothed_fps: f32,
    pub frame_ms: f32,
    pub frame_pressure: f32,
    pub queue_pressure: f32,
    pub congestion: usize,
    pub chunks_per_frame: u32,
    pub meshes_per_frame: u32,
    pub mesh_applies_per_frame: u32,
    pub max_in_flight_terrain: u32,
    pub max_in_flight_meshes: u32,
    pub shadow_radius: i32,
    pub weather_fx_scale: f32,
    pub weapon_fx_scale: f32,
    pub update_cadence: f32,
    pub status: String,
    /// Exact dense-near-field telemetry. These values are intentionally
    /// separate from the visual render distance: distant terrain must use a
    /// cheaper representation instead of silently growing this budget.
    pub requested_chunks: usize,
    pub resident_chunks: usize,
    pub inflight_terrain: usize,
    pub inflight_meshes: usize,
    pub mesh_bucket_entities: usize,
    pub peak_mesh_bucket_entities: usize,
    pub evicted_chunks_total: u64,
    pub cancelled_tasks_total: u64,
    pub request_epoch: u64,
    pub interaction_radius_chunks: i32,
    pub full_chunk_budget: usize,
    pub full_chunk_cap_reason: String,
}

impl Default for StreamingGovernor {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: RuntimeProfile::Auto,
            intent: RuntimeIntent::Explore,
            quality: QualityState::Nominal,
            target_render_distance: 0,
            effective_render_distance: 0,
            smoothed_fps: 0.0,
            frame_ms: 0.0,
            frame_pressure: 0.0,
            queue_pressure: 0.0,
            congestion: 0,
            chunks_per_frame: 0,
            meshes_per_frame: 0,
            mesh_applies_per_frame: 0,
            max_in_flight_terrain: 0,
            max_in_flight_meshes: 0,
            shadow_radius: 0,
            weather_fx_scale: 1.0,
            weapon_fx_scale: 1.0,
            update_cadence: 0.5,
            status: "warming up".into(),
            requested_chunks: 0,
            resident_chunks: 0,
            inflight_terrain: 0,
            inflight_meshes: 0,
            mesh_bucket_entities: 0,
            peak_mesh_bucket_entities: 0,
            evicted_chunks_total: 0,
            cancelled_tasks_total: 0,
            request_epoch: 0,
            interaction_radius_chunks: 0,
            full_chunk_budget: MAX_FULL_CHUNK_RESIDENT,
            full_chunk_cap_reason: FULL_CHUNK_CAP_REASON.to_string(),
        }
    }
}

impl StreamingGovernor {
    pub fn active_render_distance(&self, target: u32) -> i32 {
        let target = target as i32;
        if !self.enabled || self.effective_render_distance <= 0 {
            target
        } else {
            self.effective_render_distance.clamp(2, target.max(2))
        }
    }
}

impl VoxelWorld {
    pub fn new() -> Self {
        Self {
            chunks: AHashMap::new(),
            loaded_column_counts: AHashMap::new(),
            generator: TerrainGenerator::new(12345),
            horizon_cache: SharedHorizonCache::default(),
            edited_overrides: AHashMap::new(),
            column_top_cy: AHashMap::new(),
            edit_dirty_chunks: AHashSet::new(),
            reserved_async_dense_slots: 0,
            edit_save_dirty: false,
            edit_save_revision: 0,
            edit_store_status: WorldEditStoreStatus::Unchecked,
            last_repair_report: None,
        }
    }

    pub fn clear_chunks(&mut self) {
        self.chunks.clear();
        self.loaded_column_counts.clear();
        self.horizon_cache.clear();
        self.reserved_async_dense_slots = 0;
    }

    fn mark_edit_snapshot_dirty(&mut self) {
        self.edit_save_revision = self
            .edit_save_revision
            .checked_add(1)
            .expect("in-memory edit revision exhausted");
        self.edit_save_dirty = true;
    }

    /// Clear dirty state only when this receipt is both the newest capture for
    /// the authority and still describes the current in-memory edit revision.
    pub fn confirm_edited_override_save(&mut self, receipt: &EditedOverrideSaveReceipt) -> bool {
        if receipt.world_revision != self.edit_save_revision || !receipt.is_latest_confirmed() {
            return false;
        }
        self.edit_save_dirty = false;
        true
    }

    /// Remove saved edit chunks that are overwhelmingly old showcase /
    /// ice-artifact material in normal terrain, then regenerate any
    /// currently loaded chunks from terrain. This is intentionally
    /// conservative: ordinary stone/wood/road/building edits stay.
    pub fn repair_visual_artifact_overrides(&mut self) -> WorldRepairReport {
        let mut report = WorldRepairReport {
            scanned_chunks: self.edited_overrides.len(),
            ..default()
        };
        let to_remove: Vec<ChunkPos> = self
            .edited_overrides
            .iter()
            .filter_map(|(pos, data)| {
                if override_looks_like_visual_artifact(*pos, data, &self.generator) {
                    Some(*pos)
                } else {
                    None
                }
            })
            .collect();

        report.removed_chunks = to_remove.len();
        report.kept_chunks = report.scanned_chunks.saturating_sub(report.removed_chunks);

        for pos in to_remove {
            self.edited_overrides.remove(&pos);
            self.column_top_cy.remove(&(pos.x, pos.z));
            if self.chunks.contains_key(&pos) {
                let mut regenerated = Chunk::new(pos);
                self.generator.generate(&mut regenerated);
                regenerated.dirty = true;
                self.insert_chunk(pos, regenerated);
                report.refreshed_loaded_chunks += 1;
            }
            self.mark_chunk_family_dirty(pos);
        }

        if report.removed_chunks > 0 {
            self.mark_edit_snapshot_dirty();
        }
        self.last_repair_report = Some(report);
        report
    }

    fn mark_chunk_family_dirty(&mut self, cp: ChunkPos) {
        self.edit_dirty_chunks.insert(cp);
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let n = ChunkPos::new(cp.x + dx, cp.y + dy, cp.z + dz);
            if let Some(c) = self.chunks.get_mut(&n) {
                c.dirty = true;
                self.edit_dirty_chunks.insert(n);
            }
        }
    }

    pub fn insert_chunk(&mut self, pos: ChunkPos, chunk: Chunk) -> Option<Chunk> {
        let previous = self.chunks.insert(pos, chunk);
        if previous.is_none() {
            *self.loaded_column_counts.entry((pos.x, pos.z)).or_insert(0) += 1;
        }
        previous
    }

    pub fn remove_chunk(&mut self, pos: &ChunkPos) -> Option<Chunk> {
        let removed = self.chunks.remove(pos);
        if removed.is_some() {
            let col = (pos.x, pos.z);
            if let Some(count) = self.loaded_column_counts.get_mut(&col) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.loaded_column_counts.remove(&col);
                }
            }
        }
        removed
    }

    /// Look up a voxel in world-space. Returns AIR if that chunk isn't loaded.
    #[inline]
    pub fn voxel_at(&self, wx: i32, wy: i32, wz: i32) -> Voxel {
        let (cp, lx, ly, lz) = world_to_chunk(wx, wy, wz);
        match self.chunks.get(&cp) {
            Some(chunk) => chunk.get(lx, ly, lz),
            None => AIR,
        }
    }

    /// Whether the exact chunk owning this world-space voxel is resident.
    ///
    /// This is intentionally stronger than [`Self::is_column_loaded`]. A
    /// visual or safety probe must not treat an absent vertical chunk as AIR
    /// merely because another chunk in the same X/Z column happens to exist.
    #[inline]
    pub fn is_voxel_chunk_loaded(&self, wx: i32, wy: i32, wz: i32) -> bool {
        let (cp, _, _, _) = world_to_chunk(wx, wy, wz);
        self.chunks.contains_key(&cp)
    }

    /// Resolve a voxel without pretending an absent chunk is resident.
    ///
    /// Loaded chunks return their authoritative voxel. An absent slot is
    /// resolved as AIR only when the streamer has already cached a
    /// conservative procedural column ceiling, the queried chunk is above it,
    /// and no restored edit override exists for that exact chunk. Every other
    /// absent slot remains unresolved. Callers that require current streaming
    /// coverage must additionally bind this result to the streamer's exact
    /// request set. This lets safety/QA probes distinguish deliberately
    /// unmaterialized air from missing world data.
    #[inline]
    pub fn voxel_at_if_resolved(&self, wx: i32, wy: i32, wz: i32) -> Option<Voxel> {
        let (cp, lx, ly, lz) = world_to_chunk(wx, wy, wz);
        if let Some(chunk) = self.chunks.get(&cp) {
            return Some(chunk.get(lx, ly, lz));
        }
        if self.edited_overrides.contains_key(&cp) {
            return None;
        }
        self.column_top_cy
            .get(&(cp.x, cp.z))
            .is_some_and(|top_cy| cp.y > *top_cy)
            .then_some(AIR)
    }

    #[inline]
    #[allow(dead_code)]
    pub fn material_at(&self, wx: i32, wy: i32, wz: i32) -> MaterialId {
        let (cp, lx, ly, lz) = world_to_chunk(wx, wy, wz);
        match self.chunks.get(&cp) {
            Some(chunk) => chunk.get_material(lx, ly, lz),
            None => DEFAULT_MATERIAL,
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn effective_material_at(&self, wx: i32, wy: i32, wz: i32) -> MaterialId {
        let voxel = self.voxel_at(wx, wy, wz);
        effective_material_for_voxel(voxel, self.material_at(wx, wy, wz))
    }

    /// Is this world-space block solid (for collision)? Unloaded chunks
    /// count as non-solid so the player can keep moving while terrain
    /// streams in.
    #[inline]
    pub fn is_solid(&self, wx: i32, wy: i32, wz: i32) -> bool {
        voxel_is_solid(self.voxel_at(wx, wy, wz))
    }

    /// Biome at a world (x, z) column — for HUD / editor display.
    pub fn biome_at(&self, wx: i32, wz: i32) -> crate::terrain::Biome {
        self.generator.biome_at(wx, wz)
    }

    /// Shared continuous ecology sample for bounded simulation and visual
    /// presentation systems. This is deterministic and never mutates terrain.
    pub fn environment_sample_at(&self, wx: i32, wz: i32) -> crate::terrain::EnvironmentSample {
        self.generator.environment_sample_at(wx, wz)
    }

    /// Is at least one chunk in the vertical column at (wx, wz) loaded?
    /// Used by the player to know when physics can safely take over.
    pub fn is_column_loaded(&self, wx: i32, wz: i32) -> bool {
        let cx = wx.div_euclid(crate::chunk::CHUNK_SIZE as i32);
        let cz = wz.div_euclid(crate::chunk::CHUNK_SIZE as i32);
        self.loaded_column_counts.contains_key(&(cx, cz))
    }

    /// Builder / editor hook: set a voxel at a world-space coordinate,
    /// creating the owning chunk if necessary, and marking it plus any
    /// neighbours touched by the change as dirty so the mesher re-runs.
    /// Returns `true` iff the voxel actually changed.
    pub fn edit_set_voxel(&mut self, wx: i32, wy: i32, wz: i32, v: Voxel) -> bool {
        let mut batch = WorldEditBatch::default();
        let changed = self
            .edit_set_voxel_batched(wx, wy, wz, v, &mut batch)
            .is_some();
        self.finish_edit_batch(batch);
        changed
    }

    /// Batched variant of [`Self::edit_set_voxel`]. Call this repeatedly
    /// for large editor operations, then call [`Self::finish_edit_batch`]
    /// once. This avoids recomputing uniform flags and queueing neighbours
    /// for every single voxel in a 100k+ block edit.
    pub fn edit_set_voxel_batched(
        &mut self,
        wx: i32,
        wy: i32,
        wz: i32,
        v: Voxel,
        batch: &mut WorldEditBatch,
    ) -> Option<(Voxel, Voxel)> {
        let (cp, lx, ly, lz) = crate::chunk::world_to_chunk(wx, wy, wz);
        if v == AIR && !self.chunks.contains_key(&cp) {
            return None;
        }
        let prev = self
            .chunks
            .get(&cp)
            .map_or(AIR, |chunk| chunk.get(lx, ly, lz));
        if prev == v {
            return None;
        }
        if let Err(reason) = self.direct_edit_admission(cp, batch) {
            batch.reject(cp, reason);
            return None;
        }
        if !self.chunks.contains_key(&cp) {
            self.insert_chunk(cp, crate::chunk::Chunk::new(cp));
        }
        let chunk = self
            .chunks
            .get_mut(&cp)
            .expect("admitted direct edit chunk must be resident");
        chunk.set(lx, ly, lz, v);
        batch.mark(cp, lx, ly, lz);
        Some((prev, v))
    }

    #[allow(dead_code)]
    pub fn edit_set_cell_batched(
        &mut self,
        wx: i32,
        wy: i32,
        wz: i32,
        v: Voxel,
        material: MaterialId,
        batch: &mut WorldEditBatch,
    ) -> Option<((Voxel, MaterialId), (Voxel, MaterialId))> {
        let (cp, lx, ly, lz) = crate::chunk::world_to_chunk(wx, wy, wz);
        if v == AIR && !self.chunks.contains_key(&cp) {
            return None;
        }
        let material = normalize_material_for_voxel(v, material);
        let prev = self
            .chunks
            .get(&cp)
            .map_or((AIR, DEFAULT_MATERIAL), |chunk| {
                (chunk.get(lx, ly, lz), chunk.get_material(lx, ly, lz))
            });
        let next = (v, material);
        if prev == next {
            return None;
        }
        if let Err(reason) = self.direct_edit_admission(cp, batch) {
            batch.reject(cp, reason);
            return None;
        }
        if !self.chunks.contains_key(&cp) {
            self.insert_chunk(cp, crate::chunk::Chunk::new(cp));
        }
        let chunk = self
            .chunks
            .get_mut(&cp)
            .expect("admitted direct cell edit chunk must be resident");
        chunk.set_cell(lx, ly, lz, v, material);
        batch.mark(cp, lx, ly, lz);
        Some((prev, next))
    }

    #[allow(dead_code)]
    pub fn edit_set_material_batched(
        &mut self,
        wx: i32,
        wy: i32,
        wz: i32,
        material: MaterialId,
        batch: &mut WorldEditBatch,
    ) -> Option<(MaterialId, MaterialId)> {
        let (cp, lx, ly, lz) = crate::chunk::world_to_chunk(wx, wy, wz);
        let chunk = self.chunks.get(&cp)?;
        let voxel = chunk.get(lx, ly, lz);
        if voxel == AIR {
            return None;
        }
        let material = normalize_material_for_voxel(voxel, material);
        let prev = chunk.get_material(lx, ly, lz);
        if prev == material {
            return None;
        }
        if let Err(reason) = self.direct_edit_admission(cp, batch) {
            batch.reject(cp, reason);
            return None;
        }
        let chunk = self
            .chunks
            .get_mut(&cp)
            .expect("material edit chunk was checked resident");
        chunk.set_material(lx, ly, lz, material);
        batch.mark(cp, lx, ly, lz);
        Some((prev, material))
    }

    fn direct_edit_admission(
        &self,
        cp: ChunkPos,
        batch: &mut WorldEditBatch,
    ) -> Result<(), DirectEditAdmissionRejection> {
        if !self.chunks.contains_key(&cp)
            && self
                .chunks
                .len()
                .saturating_add(self.reserved_async_dense_slots)
                >= MAX_FULL_CHUNK_RESIDENT
        {
            return Err(DirectEditAdmissionRejection::DenseSlots);
        }
        if !self.edited_overrides.contains_key(&cp)
            && !batch.new_override_chunks.contains(&cp)
            && self
                .edited_overrides
                .len()
                .saturating_add(batch.new_override_chunks.len())
                >= MAX_EDITED_OVERRIDE_RECORDS
        {
            return Err(DirectEditAdmissionRejection::OverrideRecords);
        }
        if !self.edited_overrides.contains_key(&cp) {
            batch.new_override_chunks.insert(cp);
        }
        Ok(())
    }

    /// Finalise a direct-edit batch and publish all touched chunks to the
    /// mesher queue. Safe to call with an empty batch.
    pub fn finish_edit_batch(&mut self, batch: WorldEditBatch) {
        if !batch.dense_slot_rejections.is_empty() || !batch.override_record_rejections.is_empty() {
            warn!(
                "world edit admission rejected {} dense-slot chunk(s) and {} new override-record chunk(s); hard limits are {MAX_FULL_CHUNK_RESIDENT} resident-plus-in-flight chunks and {MAX_EDITED_OVERRIDE_RECORDS} persisted edit chunks",
                batch.dense_slot_rejections.len(),
                batch.override_record_rejections.len()
            );
        }
        if batch.modified_chunks.is_empty() {
            return;
        }

        // Any edit can change the topmost-non-air row for a column;
        // invalidate the fast vertical terrain cull for touched columns.
        for col in batch.dirty_columns {
            self.column_top_cy.remove(&col);
        }

        // Recompute uniform/empty flags once per modified chunk. This is
        // the expensive O(4096) scan that used to run once per edited voxel.
        let mut snapshot_changed = false;
        for cp in &batch.modified_chunks {
            if let Some(c) = self.chunks.get_mut(cp) {
                c.finalize_uniform_flags();
                c.dirty = true;
                self.edited_overrides
                    .insert(*cp, EditedChunkOverride::from_chunk(c));
                snapshot_changed = true;
            }
        }
        if snapshot_changed {
            self.mark_edit_snapshot_dirty();
        }
        debug_assert!(self.edited_overrides.len() <= MAX_EDITED_OVERRIDE_RECORDS);

        // Queue modified chunks plus boundary neighbours so face culling
        // updates across chunk edges.
        for cp in batch.dirty_chunks {
            if let Some(c) = self.chunks.get_mut(&cp) {
                c.dirty = true;
                self.edit_dirty_chunks.insert(cp);
            }
        }
    }

    /// Terrain surface height (block y of the topmost solid block) at a
    /// world (x, z) column.
    pub fn surface_height_at(&self, wx: i32, wz: i32) -> i32 {
        self.generator.surface_height_at(wx, wz)
    }

    /// Highest chunk-y index that could contain non-air terrain for the
    /// given chunk column, cached. Probes the terrain on a 4×4 grid AND
    /// clamps to `WATER_LEVEL` so oceans keep their surface. Adds +2
    /// chunks of headroom for trees, mountain peaks that fall between
    /// grid points, and future decorations.
    pub fn column_top_cy_cached(&mut self, cx: i32, cz: i32) -> i32 {
        if let Some(v) = self.column_top_cy.get(&(cx, cz)) {
            return *v;
        }
        let s = CHUNK_SIZE_I;
        let wx0 = cx * s;
        let wz0 = cz * s;
        // 4×4 = 16 samples inside the chunk column. Cheap (each is a few
        // noise evals) and dense enough to catch mountain peaks that the
        // old 5-point probe missed.
        let step = s / 4;
        let mut max_block_y = crate::terrain::WATER_LEVEL;
        for iz in 0..=4 {
            for ix in 0..=4 {
                let wx = wx0 + (ix * step).min(s - 1);
                let wz = wz0 + (iz * step).min(s - 1);
                let h = self.generator.surface_height_at(wx, wz);
                if h > max_block_y {
                    max_block_y = h;
                }
            }
        }
        if let Some(feature_top) = self.generator.decorative_top_hint_for_chunk(cx, cz) {
            max_block_y = max_block_y.max(feature_top);
        }
        // +2 chunks of safety: covers trees (+6 blocks), tall features,
        // and mountain peaks that might still fall between samples.
        let top_cy = (max_block_y / s) + 2;
        self.column_top_cy.insert((cx, cz), top_cy);
        top_cy
    }
}

/// Accumulator for a large direct voxel edit. Public because editor-like
/// modules build a batch, but fields stay private so all callers go through
/// [`VoxelWorld::edit_set_voxel_batched`].
#[derive(Default)]
pub struct WorldEditBatch {
    modified_chunks: AHashSet<ChunkPos>,
    dirty_chunks: AHashSet<ChunkPos>,
    dirty_columns: AHashSet<(i32, i32)>,
    new_override_chunks: AHashSet<ChunkPos>,
    dense_slot_rejections: AHashSet<ChunkPos>,
    override_record_rejections: AHashSet<ChunkPos>,
}

#[derive(Clone, Copy)]
enum DirectEditAdmissionRejection {
    DenseSlots,
    OverrideRecords,
}

impl WorldEditBatch {
    fn reject(&mut self, cp: ChunkPos, reason: DirectEditAdmissionRejection) {
        match reason {
            DirectEditAdmissionRejection::DenseSlots => {
                self.dense_slot_rejections.insert(cp);
            }
            DirectEditAdmissionRejection::OverrideRecords => {
                self.override_record_rejections.insert(cp);
            }
        }
    }

    fn mark(&mut self, cp: ChunkPos, lx: usize, ly: usize, lz: usize) {
        self.modified_chunks.insert(cp);
        self.dirty_chunks.insert(cp);
        self.dirty_columns.insert((cp.x, cp.z));

        let s = CHUNK_SIZE_I as usize;
        if lx == 0 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x - 1, cp.y, cp.z));
        }
        if lx == s - 1 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x + 1, cp.y, cp.z));
        }
        if ly == 0 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x, cp.y - 1, cp.z));
        }
        if ly == s - 1 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x, cp.y + 1, cp.z));
        }
        if lz == 0 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x, cp.y, cp.z - 1));
        }
        if lz == s - 1 {
            self.dirty_chunks
                .insert(ChunkPos::new(cp.x, cp.y, cp.z + 1));
        }
    }
}

/// One-frame and lifetime counters for the dense interaction bubble. Keeping
/// this on the streamer makes it available to the HUD, Agent Control, QA, and
/// future Mission Control feeds without scraping log text.
#[derive(Debug, Clone, Copy)]
pub struct StreamingTelemetry {
    pub requested_chunks: usize,
    pub resident_chunks: usize,
    pub inflight_terrain: usize,
    pub inflight_meshes: usize,
    pub mesh_bucket_entities: usize,
    pub peak_mesh_bucket_entities: usize,
    pub evicted_this_frame: usize,
    pub evicted_chunks_total: u64,
    pub cancelled_this_frame: usize,
    pub cancelled_tasks_total: u64,
    pub stale_results_total: u64,
    pub request_epoch: u64,
    pub interaction_radius_chunks: i32,
    pub selected_columns: usize,
    pub hard_resident_budget: usize,
    pub cap_reason: &'static str,
}

impl Default for StreamingTelemetry {
    fn default() -> Self {
        Self {
            requested_chunks: 0,
            resident_chunks: 0,
            inflight_terrain: 0,
            inflight_meshes: 0,
            mesh_bucket_entities: 0,
            peak_mesh_bucket_entities: 0,
            evicted_this_frame: 0,
            evicted_chunks_total: 0,
            cancelled_this_frame: 0,
            cancelled_tasks_total: 0,
            stale_results_total: 0,
            request_epoch: 0,
            interaction_radius_chunks: 0,
            selected_columns: 0,
            hard_resident_budget: MAX_FULL_CHUNK_RESIDENT,
            cap_reason: FULL_CHUNK_CAP_REASON,
        }
    }
}

/// Tracks which chunk entities are currently spawned so we can despawn them
/// when they stream out of range. Also keeps the async terrain and mesh
/// task handles so we can poll them each frame without blocking.
#[derive(Resource, Default)]
pub struct ChunkStreamer {
    /// Spawned mesh entity + its mesh asset handle per chunk. Keeping the
    /// handle lets us explicitly free the GPU buffer via `meshes.remove()`
    /// when a chunk re-meshes or unloads, instead of waiting for Bevy's
    /// asset-GC sweep (which caused long-session memory drift).
    pub entities: AHashMap<ChunkPos, Vec<ChunkMeshEntity>>,
    pub material: Option<Handle<StandardMaterial>>,
    /// In-flight terrain-generation tasks (one per chunk position).
    pub pending_terrain: AHashMap<ChunkPos, (u64, Task<(ChunkPos, SharedVoxels)>)>,
    /// In-flight meshing tasks (one per chunk position). `None` mesh =
    /// chunk is empty / uniform-solid and needs no geometry.
    pub pending_meshes: AHashMap<ChunkPos, (u64, Task<(ChunkPos, Vec<(MeshBucketKey, Mesh)>)>)>,
    /// Dirty-chunk set so the mesh scheduler doesn't walk the entire
    /// chunk hashmap every frame AND so a given chunk cannot end up in
    /// the work list 20× per frame. Before this was a `Vec<ChunkPos>`
    /// which accumulated duplicates from (a) each newly-loaded chunk
    /// pushing itself + 6 neighbours without dedup, and (b) the
    /// re-queue logic in `mesh_dirty_chunks` for chunks that couldn't
    /// be scheduled this frame. After long sessions the vec could
    /// contain the same ChunkPos thousands of times, causing a slow
    /// per-frame drift as the scheduler iterated the same duplicates.
    pub dirty_queue: AHashSet<ChunkPos>,
    /// Scratch buffer for the mesh scheduler's priority-sorted
    /// candidate list. Reused across frames to avoid a 10 KB+
    /// allocation per frame at RD=50.
    pub mesh_candidates_scratch: Vec<(i32, ChunkPos)>,
    /// Render-distance disc offsets sorted near-to-far. Reused while the
    /// player stays at the same render distance so we do not rebuild and
    /// sort an 8k-column frontier list whenever the player crosses a
    /// chunk boundary.
    pub load_offsets: Vec<(i32, i32, i32)>,
    pub load_offsets_rd: i32,
    /// Cursor into `load_offsets` for the incremental frontier pass.
    /// This spreads the RD=50 scan across frames while still scanning
    /// enough already-loaded columns each frame to find the new edge
    /// quickly after movement.
    pub load_cursor: usize,
    pub last_vertical_chunks: i32,
    /// Flips true when at least one chunk unloads (mesh entities might
    /// now be orphaned). Cleared after the orphan-scan pass. Stops the
    /// mesh system from walking the entire entities map every frame at
    /// RD=50 (≈2500+ entries) when nothing has actually changed.
    pub needs_orphan_scan: bool,
    /// True once a full RD sweep has confirmed every chunk inside the
    /// render radius is loaded or in-flight. At RD=50 the sweep itself
    /// is ~80,000 slot checks per frame — at ~30 ns per HashMap lookup
    /// that's 5 ms of pure waste while standing still. We invalidate
    /// this flag only when the player crosses a chunk boundary (new
    /// chunks might be needed) or a chunk unloads (slot opened up).
    pub frontier_complete: bool,
    /// Last anchor chunk position we scanned from. When this changes, a
    /// new frontier sweep is required.
    pub last_anchor_cxz: Option<(i32, i32)>,
    /// Exact full-chunk request set for the current interaction bubble.
    /// Both resident chunks and terrain tasks must be members, so their
    /// combined count can never exceed [`MAX_FULL_CHUNK_RESIDENT`].
    pub requested_chunks: AHashSet<ChunkPos>,
    /// Monotonic generation of the request plan. Async jobs are tagged with
    /// this value; a teleport replaces the plan and stale work is cancelled
    /// before its result can be installed.
    pub request_epoch: u64,
    pub last_priority_heading: (i8, i8),
    pub last_motion_hint: (i8, i8),
    pub telemetry: StreamingTelemetry,
}

#[derive(Clone)]
pub struct ChunkMeshEntity {
    pub entity: Entity,
    pub handle: Handle<Mesh>,
    pub bucket: MeshBucketKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MeshMaterialRoute {
    Standard,
    Vegetation(VegetationSpecies),
    WaterOptics,
}

/// Shader-family policy is owned by the voxel category, never inferred from
/// editable material presentation data. A custom material on Water therefore
/// remains a distinct edit/bucket identity but cannot opt the voxel out of the
/// one canonical water presentation; its custom base texture (including the
/// unresolved sentinel) is intentionally suppressed. Conversely, a solid
/// borrowing Water's material id cannot opt in. Vegetation follows the same
/// authority rule with one of four voxel-derived species presets; nonmatching
/// or custom base textures are suppressed rather than allocating an unbounded
/// material-by-species cross-product. This preserves the fixed one-Water plus
/// four-Vegetation material asset budget and makes the policy explicit.
fn mesh_material_route(bucket: MeshBucketKey) -> MeshMaterialRoute {
    match bucket.render_class {
        MeshRenderClass::Standard => MeshMaterialRoute::Standard,
        MeshRenderClass::Vegetation(species) => MeshMaterialRoute::Vegetation(species),
        MeshRenderClass::Water => MeshMaterialRoute::WaterOptics,
    }
}

fn init_world(
    mut streamer: ResMut<ChunkStreamer>,
    mut world: ResMut<VoxelWorld>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut vegetation_materials: ResMut<Assets<crate::vegetation::VegetationMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut material_library: ResMut<crate::textures::MaterialLibrary>,
    settings: Res<WorldSettings>,
) {
    world.generator = TerrainGenerator::from_identity(settings.generation_identity());
    material_library.rebuild(&mut materials, &mut vegetation_materials, &mut images);

    // Bake the procedural surface-grain texture once. 128×128 is the
    // sweet spot: still crisp at arm's length under `Repeat` sampling,
    // but generates in <50 ms on an iGPU (vs ~250 ms at 256×256 with
    // the 6-octave + warp + Worley + strata + sparkle pipeline). Users
    // who drop a real 512²/1024² PNG in ./textures/universal_grain.png
    // get photorealistic detail for free via the override path.
    let grain_size = match settings.graphics {
        crate::settings::GraphicsMode::Fast => 64,
        crate::settings::GraphicsMode::Balanced => 128,
        crate::settings::GraphicsMode::High => 256,
    };
    let grain = images.add(crate::textures::universal_grain_or_override(grain_size));

    streamer.material = Some(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(grain.clone()),
        perceptual_roughness: 1.0,
        reflectance: 0.05,
        ..default()
    }));
}

/// Player marker — looked up by the streamer to decide which chunks to load.
#[derive(Component)]
pub struct ChunkAnchor;

/// Shove a chunk priority toward the camera forward direction so chunks
/// the player is looking at get meshed first. Returns a score where
/// smaller = higher priority.
#[inline]
fn priority_score(dx: i32, dz: i32, forward: Vec2) -> i32 {
    let d2 = dx * dx + dz * dz;
    let dot = forward.x * dx as f32 + forward.y * dz as f32;
    // Chunks in front of the camera get a bonus (negative penalty); chunks
    // behind get a mild penalty. The weight stays smaller than the
    // distance² term so proximity still dominates for close chunks.
    let bias = (-dot * 3.0) as i32;
    d2 + bias
}

#[derive(Debug, Clone, Copy)]
struct ColumnDemand {
    top_cy: i32,
    has_edits: bool,
}

#[derive(Debug)]
struct InteractionRequestPlan {
    radius: i32,
    columns: Vec<(i32, i32, i32)>,
    chunks: Vec<ChunkPos>,
}

/// Collapse a continuously rotating camera/motion vector into one of eight
/// stable sectors. The hysteresis-by-quantisation prevents request churn from
/// tiny mouse jitter while still giving the scheduler a useful frustum hint.
fn quantized_direction(direction: Vec2) -> (i8, i8) {
    let direction = direction.normalize_or_zero();
    if direction == Vec2::ZERO {
        return (0, 0);
    }
    let quantize = |axis: f32| {
        if axis > 0.382_683_43 {
            1
        } else if axis < -0.382_683_43 {
            -1
        } else {
            0
        }
    };
    (quantize(direction.x), quantize(direction.y))
}

/// Build an exact, column-complete dense-chunk reservation plan. This is the key bounded
/// representation switch: arbitrary visual RD affects far-field systems, but
/// this planner examines at most a 16-chunk radius and admits at most 2,400
/// full chunks. A four-chunk core is unconditionally distance-first; outside
/// it, edited columns and predicted/frustum direction receive priority.
fn build_interaction_request_plan<F>(
    pcx: i32,
    pcz: i32,
    visual_render_distance: i32,
    vertical_chunks: i32,
    heading: (i8, i8),
    motion: (i8, i8),
    max_chunks: usize,
    mut demand_for_column: F,
) -> InteractionRequestPlan
where
    F: FnMut(i32, i32) -> ColumnDemand,
{
    let radius = visual_render_distance
        .max(2)
        .min(MAX_INTERACTION_RADIUS_CHUNKS);
    if vertical_chunks <= 0 || max_chunks == 0 {
        return InteractionRequestPlan {
            radius,
            columns: Vec::new(),
            chunks: Vec::new(),
        };
    }

    let radius2 = radius * radius;
    let core2 = GUARANTEED_INTERACTION_CORE_CHUNKS * GUARANTEED_INTERACTION_CORE_CHUNKS;
    let mut candidates = Vec::with_capacity(((radius * 2 + 1).pow(2)) as usize);
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let d2 = dx * dx + dz * dz;
            if d2 > radius2 {
                continue;
            }
            let demand = demand_for_column(pcx + dx, pcz + dz);
            let top_cy = demand.top_cy.clamp(0, vertical_chunks - 1);
            let column_chunks = (top_cy + 1) as usize;
            let predicted_dx = dx - i32::from(motion.0) * PREDICTIVE_LEAD_CHUNKS;
            let predicted_dz = dz - i32::from(motion.1) * PREDICTIVE_LEAD_CHUNKS;
            let predicted_d2 = predicted_dx * predicted_dx + predicted_dz * predicted_dz;
            let forward_dot = dx * i32::from(heading.0) + dz * i32::from(heading.1);
            let motion_dot = dx * i32::from(motion.0) + dz * i32::from(motion.1);
            let score = d2 * 2_048 + predicted_d2 * 256 - forward_dot * 96 - motion_dot * 192;
            let tier = if d2 <= core2 {
                0
            } else if demand.has_edits {
                1
            } else {
                2
            };
            candidates.push((tier, score, d2, dx, dz, column_chunks));
        }
    }
    candidates.sort_unstable_by_key(|(tier, score, d2, dx, dz, _)| {
        (*tier, *score, *d2, dx.abs() + dz.abs(), *dz, *dx)
    });

    let mut columns = Vec::with_capacity(candidates.len());
    let mut chunks = Vec::with_capacity(max_chunks);
    for (_tier, score, _d2, dx, dz, column_chunks) in candidates {
        // Admit whole columns only. Splitting a column at the budget boundary
        // creates floating collision holes and makes an edited tower vanish
        // halfway up. Skipping at most `vertical_chunks - 1` slots is the
        // deterministic, safer tradeoff.
        if chunks.len().saturating_add(column_chunks) > max_chunks {
            continue;
        }
        columns.push((score, dx, dz));
        for cy in 0..column_chunks as i32 {
            chunks.push(ChunkPos::new(pcx + dx, cy, pcz + dz));
        }
    }

    InteractionRequestPlan {
        radius,
        columns,
        chunks,
    }
}

fn retarget_epoch_jobs<T>(
    jobs: &mut AHashMap<ChunkPos, (u64, T)>,
    requested: &AHashSet<ChunkPos>,
    epoch: u64,
) -> usize {
    let before = jobs.len();
    jobs.retain(|pos, (job_epoch, _)| {
        if requested.contains(pos) {
            // Terrain generation is deterministic for a chunk. A mesh job is
            // also safe to retag only when none of its captured neighbour
            // snapshots changed; the mesh-specific wrapper below enforces
            // that additional condition.
            *job_epoch = epoch;
            true
        } else {
            false
        }
    });
    before.saturating_sub(jobs.len())
}

const CARDINAL_CHUNK_OFFSETS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

#[inline]
fn checked_chunk_offset(pos: ChunkPos, dx: i32, dy: i32, dz: i32) -> Option<ChunkPos> {
    Some(ChunkPos::new(
        pos.x.checked_add(dx)?,
        pos.y.checked_add(dy)?,
        pos.z.checked_add(dz)?,
    ))
}

/// Find retained mesh centres whose six-neighbour snapshot changes when the
/// exact request authority evicts `to_drop`. Diagonals do not share faces;
/// checked arithmetic makes the boundary behavior defined at i32 extremes.
fn retained_mesh_neighbours_after_eviction(
    world: &VoxelWorld,
    requested: &AHashSet<ChunkPos>,
    to_drop: &[ChunkPos],
) -> AHashSet<ChunkPos> {
    let mut affected = AHashSet::new();
    for dropped in to_drop {
        for (dx, dy, dz) in CARDINAL_CHUNK_OFFSETS {
            let Some(neighbour) = checked_chunk_offset(*dropped, dx, dy, dz) else {
                continue;
            };
            if requested.contains(&neighbour) && world.chunks.contains_key(&neighbour) {
                affected.insert(neighbour);
            }
        }
    }
    affected
}

fn retarget_mesh_epoch_jobs<T>(
    jobs: &mut AHashMap<ChunkPos, (u64, T)>,
    requested: &AHashSet<ChunkPos>,
    invalidated_centres: &AHashSet<ChunkPos>,
    epoch: u64,
) -> usize {
    let before = jobs.len();
    jobs.retain(|pos, (job_epoch, _)| {
        if requested.contains(pos) && !invalidated_centres.contains(pos) {
            *job_epoch = epoch;
            true
        } else {
            false
        }
    });
    before.saturating_sub(jobs.len())
}

#[inline]
fn task_result_is_current(
    task_epoch: u64,
    current_epoch: u64,
    requested: &AHashSet<ChunkPos>,
    pos: ChunkPos,
) -> bool {
    task_epoch == current_epoch && requested.contains(&pos)
}

#[inline]
fn has_unrequested_resident_chunk(
    world: &VoxelWorld,
    requested_chunks: &AHashSet<ChunkPos>,
) -> bool {
    world
        .chunks
        .keys()
        .any(|pos| !requested_chunks.contains(pos))
}

#[inline]
fn terrain_task_limit(runtime_limit: usize, resident: usize, requested: usize) -> usize {
    runtime_limit
        .min(MAX_IN_FLIGHT_TERRAIN_TASKS)
        .min(requested.saturating_sub(resident))
}

#[inline]
fn mesh_task_limit(runtime_limit: usize) -> usize {
    runtime_limit.min(MAX_IN_FLIGHT_MESH_TASKS)
}

/// Choose the next dense interaction radius without feeding the queues back
/// into their own request authority. Queue pressure may slow scheduling and
/// lower NeuroCore's *expansion target*, but it never contracts an existing
/// exact request plan. Expansion is one radius step and only after the current
/// plan has fully converged. User/profile ceilings and genuine frame-pressure
/// emergencies remain immediate safety contractions.
fn stable_interaction_radius(
    current_radius: i32,
    user_visual_radius: i32,
    effective_visual_radius: i32,
    profile: RuntimeProfile,
    frame_pressure: f32,
    plan_quiescent: bool,
) -> i32 {
    let user_ceiling = user_visual_radius
        .max(GUARANTEED_INTERACTION_CORE_CHUNKS)
        .min(MAX_INTERACTION_RADIUS_CHUNKS);
    let profile_ceiling = match profile {
        RuntimeProfile::LowSpec => 11,
        RuntimeProfile::Auto
        | RuntimeProfile::Balanced
        | RuntimeProfile::Cinematic
        | RuntimeProfile::Benchmark => MAX_INTERACTION_RADIUS_CHUNKS,
    };
    let stable_ceiling = user_ceiling.min(profile_ceiling);
    let emergency_ceiling = if !frame_pressure.is_finite() {
        8
    } else if profile == RuntimeProfile::Benchmark {
        MAX_INTERACTION_RADIUS_CHUNKS
    } else if frame_pressure >= 0.85 {
        8
    } else if frame_pressure >= 0.65 {
        11
    } else {
        MAX_INTERACTION_RADIUS_CHUNKS
    };
    let hard_ceiling = stable_ceiling.min(emergency_ceiling);
    let expansion_target = effective_visual_radius
        .max(GUARANTEED_INTERACTION_CORE_CHUNKS)
        .min(hard_ceiling);

    // `-1` is the explicit new-world sentinel; accepting any value below the
    // guaranteed core also makes a default-constructed test/runtime safe.
    if current_radius < GUARANTEED_INTERACTION_CORE_CHUNKS {
        return expansion_target;
    }
    if current_radius > hard_ceiling {
        return hard_ceiling;
    }
    if plan_quiescent && current_radius < expansion_target {
        return current_radius.saturating_add(1).min(expansion_target);
    }
    current_radius
}

fn publish_streaming_telemetry(
    streamer: &mut ChunkStreamer,
    resident_chunks: usize,
    governor: &mut StreamingGovernor,
) {
    let telemetry = &mut streamer.telemetry;
    telemetry.requested_chunks = streamer.requested_chunks.len();
    telemetry.resident_chunks = resident_chunks;
    telemetry.inflight_terrain = streamer.pending_terrain.len();
    telemetry.inflight_meshes = streamer.pending_meshes.len();
    telemetry.request_epoch = streamer.request_epoch;
    telemetry.hard_resident_budget = MAX_FULL_CHUNK_RESIDENT;

    governor.requested_chunks = telemetry.requested_chunks;
    governor.resident_chunks = telemetry.resident_chunks;
    governor.inflight_terrain = telemetry.inflight_terrain;
    governor.inflight_meshes = telemetry.inflight_meshes;
    governor.mesh_bucket_entities = telemetry.mesh_bucket_entities;
    governor.peak_mesh_bucket_entities = telemetry.peak_mesh_bucket_entities;
    governor.evicted_chunks_total = telemetry.evicted_chunks_total;
    governor.cancelled_tasks_total = telemetry.cancelled_tasks_total;
    governor.request_epoch = telemetry.request_epoch;
    governor.interaction_radius_chunks = telemetry.interaction_radius_chunks;
    governor.full_chunk_budget = telemetry.hard_resident_budget;
    governor.full_chunk_cap_reason = telemetry.cap_reason.to_string();
    governor.status = format!(
        "{} // dense near {}/{} resident, {} requested, epoch {}",
        governor.status,
        telemetry.resident_chunks,
        telemetry.hard_resident_budget,
        telemetry.requested_chunks,
        telemetry.request_epoch
    );
}

fn refresh_mesh_entity_telemetry(streamer: &mut ChunkStreamer) {
    let current = streamer.entities.values().map(Vec::len).sum();
    streamer.telemetry.mesh_bucket_entities = current;
    streamer.telemetry.peak_mesh_bucket_entities =
        streamer.telemetry.peak_mesh_bucket_entities.max(current);
}

#[inline]
fn biome_stream_bonus(generator: &TerrainGenerator, cx: i32, cz: i32) -> i32 {
    let wx = cx * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    let wz = cz * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    crate::daynight::BiomeArtProfile::for_biome(generator.biome_at(wx, wz)).streaming_bonus
}

fn rebuild_interaction_plan(
    world: &mut VoxelWorld,
    streamer: &mut ChunkStreamer,
    pcx: i32,
    pcz: i32,
    visual_render_distance: i32,
    vertical_chunks: i32,
    heading: (i8, i8),
    motion: (i8, i8),
) {
    let radius = visual_render_distance
        .max(2)
        .min(MAX_INTERACTION_RADIUS_CHUNKS);
    let radius2 = i64::from(radius) * i64::from(radius);

    // Sparse edits never become mandatory global residents. Within the
    // interaction candidate disc their columns receive priority; outside it
    // their persisted snapshots remain safe in `edited_overrides` until the
    // player returns.
    let mut edited_columns: AHashSet<(i32, i32)> = AHashSet::new();
    for pos in world.edited_overrides.keys() {
        let dx = i64::from(pos.x) - i64::from(pcx);
        let dz = i64::from(pos.z) - i64::from(pcz);
        if dx * dx + dz * dz <= radius2 && pos.y >= 0 && pos.y < vertical_chunks {
            edited_columns.insert((pos.x, pos.z));
        }
    }

    let plan = build_interaction_request_plan(
        pcx,
        pcz,
        visual_render_distance,
        vertical_chunks,
        heading,
        motion,
        MAX_FULL_CHUNK_RESIDENT,
        |cx, cz| {
            let has_edits = edited_columns.contains(&(cx, cz));
            ColumnDemand {
                // Reserve the configured vertical envelope without probing
                // 25 expensive terrain samples for every candidate column on
                // the main thread. The scheduler resolves actual terrain tops
                // lazily for only admitted columns, spread across frames.
                top_cy: vertical_chunks - 1,
                has_edits,
            }
        },
    );

    streamer.request_epoch = streamer.request_epoch.wrapping_add(1);
    if streamer.request_epoch == 0 {
        streamer.request_epoch = 1;
    }
    streamer.requested_chunks.clear();
    streamer.requested_chunks.extend(plan.chunks);
    streamer.load_offsets = plan.columns;
    streamer.load_offsets_rd = plan.radius;
    streamer.load_cursor = 0;
    streamer.frontier_complete = false;
    streamer.last_priority_heading = heading;
    streamer.last_motion_hint = motion;

    // Exact-set eviction changes the AIR/solid boundary sampled by retained
    // cardinal neighbours. Discover those centres before removing storage so
    // stale mesh snapshots can be cancelled and rebuilt symmetrically with
    // the existing neighbour-dirtying path used on chunk insertion.
    let to_drop: Vec<ChunkPos> = world
        .chunks
        .keys()
        .filter(|pos| !streamer.requested_chunks.contains(pos))
        .copied()
        .collect();
    let invalidated_mesh_centres =
        retained_mesh_neighbours_after_eviction(world, &streamer.requested_chunks, &to_drop);

    let cancelled_terrain = {
        let requested = &streamer.requested_chunks;
        retarget_epoch_jobs(
            &mut streamer.pending_terrain,
            requested,
            streamer.request_epoch,
        )
    };
    let cancelled_meshes = {
        let requested = &streamer.requested_chunks;
        retarget_mesh_epoch_jobs(
            &mut streamer.pending_meshes,
            requested,
            &invalidated_mesh_centres,
            streamer.request_epoch,
        )
    };
    let cancelled = cancelled_terrain.saturating_add(cancelled_meshes);
    streamer.telemetry.cancelled_this_frame = streamer
        .telemetry
        .cancelled_this_frame
        .saturating_add(cancelled);
    streamer.telemetry.cancelled_tasks_total = streamer
        .telemetry
        .cancelled_tasks_total
        .saturating_add(cancelled as u64);

    // Exact-set eviction is stronger than a retention radius: neither a
    // multi-kilometre flight nor an extreme visual RD can leave dense chunks
    // behind. `edited_overrides` is deliberately untouched.
    for pos in &to_drop {
        world.remove_chunk(pos);
    }
    for pos in invalidated_mesh_centres {
        if let Some(chunk) = world.chunks.get_mut(&pos) {
            chunk.dirty = true;
            streamer.dirty_queue.insert(pos);
        }
    }
    if !to_drop.is_empty() {
        streamer.needs_orphan_scan = true;
    }
    streamer.telemetry.evicted_this_frame = streamer
        .telemetry
        .evicted_this_frame
        .saturating_add(to_drop.len());
    streamer.telemetry.evicted_chunks_total = streamer
        .telemetry
        .evicted_chunks_total
        .saturating_add(to_drop.len() as u64);

    // Caches follow the constant candidate disc rather than travelled
    // distance, preventing a world tour from accumulating hidden metadata.
    world.column_top_cy.retain(|(cx, cz), _| {
        let dx = i64::from(*cx) - i64::from(pcx);
        let dz = i64::from(*cz) - i64::from(pcz);
        dx * dx + dz * dz <= radius2
    });
    world.horizon_cache.retain_within(pcx, pcz, radius);
    {
        let requested = &streamer.requested_chunks;
        streamer.dirty_queue.retain(|pos| requested.contains(pos));
    }
    streamer.telemetry.interaction_radius_chunks = plan.radius;
    streamer.telemetry.selected_columns = streamer.load_offsets.len();
    streamer.telemetry.request_epoch = streamer.request_epoch;
}

#[inline]
fn chunk_slot_known_air(world: &mut VoxelWorld, pos: ChunkPos, vertical_chunks: i32) -> bool {
    if pos.y < 0 || pos.y >= vertical_chunks {
        return true;
    }
    pos.y > world.column_top_cy_cached(pos.x, pos.z)
}

#[inline]
fn chunk_slot_loaded_or_known_air(
    world: &mut VoxelWorld,
    pos: ChunkPos,
    vertical_chunks: i32,
) -> bool {
    world.chunks.contains_key(&pos) || chunk_slot_known_air(world, pos, vertical_chunks)
}

/// Resolve a mesh-neighbour dependency against the exact request authority.
///
/// A slot outside `requested_chunks` is deliberately absent for the current
/// request epoch. `ChunkSnapshot` samples that absence as AIR, so it is a
/// stable boundary condition rather than work that can ever complete while
/// the camera remains stationary. If the slot enters a later request plan,
/// installing it marks every resident cardinal neighbour dirty again.
#[inline]
fn mesh_neighbour_resolved_for_request(
    world: &mut VoxelWorld,
    requested_chunks: &AHashSet<ChunkPos>,
    pos: ChunkPos,
    vertical_chunks: i32,
) -> bool {
    !requested_chunks.contains(&pos) || chunk_slot_loaded_or_known_air(world, pos, vertical_chunks)
}

#[inline]
fn uniform_chunk_slot_matches(
    world: &mut VoxelWorld,
    pos: ChunkPos,
    vertical_chunks: i32,
    expected: Voxel,
) -> bool {
    if let Some(chunk) = world.chunks.get(&pos) {
        return (chunk.is_empty || chunk.is_uniform_solid) && chunk.uniform_voxel == expected;
    }
    chunk_slot_known_air(world, pos, vertical_chunks) && expected == AIR
}

fn uniform_chunk_is_trivially_invisible(
    world: &mut VoxelWorld,
    pos: ChunkPos,
    vertical_chunks: i32,
) -> bool {
    let expected = {
        let Some(center) = world.chunks.get(&pos) else {
            return false;
        };
        if !(center.is_empty || center.is_uniform_solid) {
            return false;
        }
        center.uniform_voxel
    };
    [
        ChunkPos::new(pos.x + 1, pos.y, pos.z),
        ChunkPos::new(pos.x - 1, pos.y, pos.z),
        ChunkPos::new(pos.x, pos.y, pos.z + 1),
        ChunkPos::new(pos.x, pos.y, pos.z - 1),
        ChunkPos::new(pos.x, pos.y + 1, pos.z),
        ChunkPos::new(pos.x, pos.y - 1, pos.z),
    ]
    .into_iter()
    .all(|n| uniform_chunk_slot_matches(world, n, vertical_chunks, expected))
}

fn sync_streaming_governor(
    governor: &mut StreamingGovernor,
    budget: &RuntimeBudget,
    streamer: &ChunkStreamer,
) -> i32 {
    governor.enabled = budget.enabled;
    governor.profile = budget.profile;
    governor.intent = budget.intent;
    governor.quality = budget.quality;
    governor.target_render_distance = budget.target_render_distance;
    governor.effective_render_distance = budget.render_distance;
    governor.smoothed_fps = budget.fps;
    governor.frame_ms = budget.frame_ms;
    governor.frame_pressure = budget.frame_pressure;
    governor.queue_pressure = budget.queue_pressure;
    governor.congestion =
        streamer.pending_terrain.len() + streamer.pending_meshes.len() + streamer.dirty_queue.len();
    governor.chunks_per_frame = budget.chunks_per_frame;
    governor.meshes_per_frame = budget.meshes_per_frame;
    governor.mesh_applies_per_frame = budget.mesh_applies_per_frame;
    governor.max_in_flight_terrain = budget.max_in_flight_terrain;
    governor.max_in_flight_meshes = budget.max_in_flight_meshes;
    governor.shadow_radius = budget.shadow_radius;
    governor.weather_fx_scale = budget.weather_fx_scale;
    governor.weapon_fx_scale = budget.weapon_fx_scale;
    governor.update_cadence = budget.update_cadence;
    governor.status = budget.status.clone();
    budget.render_distance.max(2)
}

/// Load chunks inside `render_distance` of the player (measured in chunks
/// on the X/Z plane) and unload any that drift outside retention range.
/// Terrain generation runs on the async compute task pool.
fn stream_chunks(
    anchors: Query<&Transform, With<ChunkAnchor>>,
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    celestial_travel: Option<Res<crate::celestial::CelestialTravel>>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    mut governor: ResMut<StreamingGovernor>,
) {
    let Ok(transform) = anchors.get_single() else {
        return;
    };
    // Direct edit systems run in the same Update schedule but do not borrow
    // `ChunkStreamer`. Publish the current asynchronous reservations into the
    // world before yielding so an edit that runs later in the frame still
    // enforces the combined resident-plus-in-flight ceiling.
    world.reserved_async_dense_slots = streamer.pending_terrain.len();

    // During orbital transit the ground is no longer the relevant
    // destination. Holding the current frontier avoids turning every
    // boost frame into unload/generate/mesh churn as the carrier crosses
    // thousands of blocks per second. Existing terrain remains resident
    // for a seamless return; streaming resumes automatically on approach.
    let _governed_rd = sync_streaming_governor(&mut governor, &budget, &streamer);
    streamer.telemetry.evicted_this_frame = 0;
    streamer.telemetry.cancelled_this_frame = 0;
    if celestial_travel
        .as_deref()
        .is_some_and(crate::celestial::CelestialTravel::suspends_ground_streaming)
    {
        governor.status = "Orbital transit // ground frontier held".to_string();
        publish_streaming_telemetry(&mut streamer, world.chunks.len(), &mut governor);
        return;
    }

    let (px, _py, pz) = (
        crate::chunk::to_i32_safe(transform.translation.x),
        crate::chunk::to_i32_safe(transform.translation.y),
        crate::chunk::to_i32_safe(transform.translation.z),
    );
    let pcx = px.div_euclid(CHUNK_SIZE_I);
    let pcz = pz.div_euclid(CHUNK_SIZE_I);

    let vertical = settings.vertical_chunks as i32;
    let vertical_changed = streamer.last_vertical_chunks != vertical;
    let cur_anchor = (pcx, pcz);
    let previous_anchor = streamer.last_anchor_cxz;
    let moved = previous_anchor != Some(cur_anchor);
    let forward = transform.forward();
    let heading = quantized_direction(Vec2::new(forward.x, forward.z));
    let motion = if let Some((old_x, old_z)) = previous_anchor.filter(|_| moved) {
        quantized_direction(Vec2::new((pcx - old_x) as f32, (pcz - old_z) as f32))
    } else if heading != streamer.last_priority_heading {
        // A stationary camera turn invalidates stale velocity prediction.
        (0, 0)
    } else {
        streamer.last_motion_hint
    };
    let plan_quiescent = streamer.frontier_complete
        && streamer.pending_terrain.is_empty()
        && streamer.pending_meshes.is_empty()
        && streamer.dirty_queue.is_empty()
        && world.edit_dirty_chunks.is_empty();
    let interaction_radius = stable_interaction_radius(
        streamer.load_offsets_rd,
        settings.render_distance as i32,
        budget.render_distance,
        budget.profile,
        budget.frame_pressure,
        plan_quiescent,
    );
    // A direct editor can legally create a sparse override outside the current
    // interaction plan. Its dense working chunk is temporary: the persisted
    // snapshot survives, while the next streaming pass must rebuild/evict even
    // if the mesh system already drained its dirty marker (for example after
    // orbital ground-stream suspension).
    let resident_outside_plan = has_unrequested_resident_chunk(&world, &streamer.requested_chunks);
    let plan_changed = streamer.requested_chunks.is_empty()
        || streamer.load_offsets_rd != interaction_radius
        || moved
        || vertical_changed
        || resident_outside_plan;
    if plan_changed {
        rebuild_interaction_plan(
            &mut world,
            &mut streamer,
            pcx,
            pcz,
            interaction_radius,
            vertical,
            heading,
            motion,
        );
    }
    streamer.last_vertical_chunks = vertical;
    streamer.last_anchor_cxz = Some(cur_anchor);

    // An editor operation can materialise a requested chunk while its terrain
    // task is still running. Prefer the edited resident chunk and cancel the
    // duplicate generator job so `resident + in-flight` remains a real memory
    // bound rather than double-counting the same request position.
    let terrain_before_dedup = streamer.pending_terrain.len();
    streamer
        .pending_terrain
        .retain(|pos, _| !world.chunks.contains_key(pos));
    let duplicate_jobs = terrain_before_dedup.saturating_sub(streamer.pending_terrain.len());
    if duplicate_jobs > 0 {
        streamer.telemetry.cancelled_this_frame = streamer
            .telemetry
            .cancelled_this_frame
            .saturating_add(duplicate_jobs);
        streamer.telemetry.cancelled_tasks_total = streamer
            .telemetry
            .cancelled_tasks_total
            .saturating_add(duplicate_jobs as u64);
    }

    // 2. Poll finished terrain tasks and fold them back into the world.
    // Cap installs too: terrain generation finishes on worker threads in
    // waves, and installing every completed chunk in one frame causes the
    // one-second hitch the player sees while flying at max distance.
    let terrain_apply_cap = (budget.chunks_per_frame.max(1) as usize).min(6);
    let mut applied_terrain = 0usize;
    let mut done: Vec<ChunkPos> = Vec::new();
    let mut finished_terrain: Vec<(u64, ChunkPos, SharedVoxels)> = Vec::new();
    let mut newly_loaded: Vec<ChunkPos> = Vec::new();
    for (pos, (task_epoch, task)) in streamer.pending_terrain.iter_mut() {
        if applied_terrain >= terrain_apply_cap {
            break;
        }
        if let Some((cp, voxels)) = future::block_on(future::poll_once(task)) {
            finished_terrain.push((*task_epoch, cp, voxels));
            done.push(*pos);
            applied_terrain += 1;
        }
    }
    for p in done {
        streamer.pending_terrain.remove(&p);
    }
    for (task_epoch, cp, voxels) in finished_terrain {
        if task_result_is_current(
            task_epoch,
            streamer.request_epoch,
            &streamer.requested_chunks,
            cp,
        ) && world.chunks.len() < MAX_FULL_CHUNK_RESIDENT
        {
            let mut chunk = Chunk::new(cp);
            chunk.install_voxels(voxels);
            if let Some(edited) = world.edited_overrides.get(&cp).cloned() {
                if let Some((voxels, materials)) = edited.into_shared() {
                    chunk.install_voxels_and_materials(voxels, materials);
                }
            }
            world.insert_chunk(cp, chunk);
            newly_loaded.push(cp);
        } else {
            // A teleport/replan happened after this task started. The dense
            // result is intentionally discarded; persistent edit snapshots
            // live separately and are not affected.
            streamer.telemetry.stale_results_total =
                streamer.telemetry.stale_results_total.saturating_add(1);
        }
    }
    // Enqueue every newly-loaded chunk + its 6 neighbours so seams get
    // remeshed cleanly without a whole-world scan.
    for cp in newly_loaded {
        streamer.dirty_queue.insert(cp);
        for (dx, dy, dz) in [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ] {
            let n = ChunkPos::new(cp.x + dx, cp.y + dy, cp.z + dz);
            if let Some(c) = world.chunks.get_mut(&n) {
                c.dirty = true;
                streamer.dirty_queue.insert(n);
            }
        }
    }

    // 3. Queue new terrain jobs for nearby chunks, camera-priority first,
    //    up to `max_in_flight_terrain` tasks total across threads.
    //
    //    Fast-path: if the bounded frontier is fully loaded AND every
    //    pending task slot is busy (or no slots matter because there's
    //    nothing to schedule), skip the entire sweep.
    let max_in_flight = terrain_task_limit(
        budget.max_in_flight_terrain as usize,
        world.chunks.len(),
        streamer.requested_chunks.len(),
    );
    if streamer.frontier_complete || streamer.pending_terrain.len() >= max_in_flight {
        // Nothing to do — frontier already saturated or no task slots.
    } else if streamer.pending_terrain.len() < max_in_flight {
        #[cfg(not(target_arch = "wasm32"))]
        let pool = AsyncComputeTaskPool::get();
        let spawn_budget = budget.chunks_per_frame.max(1) as usize;
        let scan_budget = (spawn_budget * 192).min(streamer.load_offsets.len()).max(1);
        let mut spawned = 0usize;
        let mut scanned = 0usize;

        while streamer.load_cursor < streamer.load_offsets.len()
            && scanned < scan_budget
            && streamer.pending_terrain.len() < max_in_flight
            && spawned < spawn_budget
        {
            scanned += 1;
            let (_, dx, dz) = streamer.load_offsets[streamer.load_cursor];
            let cx = pcx + dx;
            let cz = pcz + dz;
            let terrain_top = world.column_top_cy_cached(cx, cz).clamp(0, vertical - 1);
            let mut column_complete = true;

            for cy in 0..vertical {
                let cp = ChunkPos::new(cx, cy, cz);
                if !streamer.requested_chunks.contains(&cp) {
                    continue;
                }
                if cy > terrain_top && !world.edited_overrides.contains_key(&cp) {
                    // The slot is conservatively reserved in the hard budget
                    // but proven air for this column. Resolving this lazily
                    // avoids a cold 25-sample probe for all 797 candidates.
                    continue;
                }
                if world.chunks.contains_key(&cp) || streamer.pending_terrain.contains_key(&cp) {
                    continue;
                }
                if streamer.pending_terrain.len() >= max_in_flight || spawned >= spawn_budget {
                    column_complete = false;
                    break;
                }
                if world
                    .chunks
                    .len()
                    .saturating_add(streamer.pending_terrain.len())
                    >= streamer.requested_chunks.len().min(MAX_FULL_CHUNK_RESIDENT)
                {
                    column_complete = false;
                    break;
                }
                // Async near-terrain work must inherit the already active
                // immutable authority. Reconstructing from mutable settings,
                // or from seed/profile alone, would silently regenerate V1
                // worlds with CURRENT bytes while Far remains V1.
                let gen = terrain_generator_for_chunk_worker(&world);
                #[cfg(target_arch = "wasm32")]
                {
                    let mut chunk = Chunk::new(cp);
                    gen.generate(&mut chunk);
                    if let Some(edited) = world.edited_overrides.get(&cp).cloned() {
                        if let Some((voxels, materials)) = edited.into_shared() {
                            chunk.install_voxels_and_materials(voxels, materials);
                        }
                    }
                    world.insert_chunk(cp, chunk);
                    mark_chunk_and_neighbours_dirty(&mut world, &mut streamer, cp);
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let task = pool.spawn(async move {
                        let mut chunk = Chunk::new(cp);
                        gen.generate(&mut chunk);
                        (cp, chunk.voxels_shared())
                    });
                    let request_epoch = streamer.request_epoch;
                    streamer.pending_terrain.insert(cp, (request_epoch, task));
                }
                spawned += 1;
            }

            if column_complete {
                streamer.load_cursor += 1;
            } else {
                break;
            }
        }

        if streamer.load_cursor >= streamer.load_offsets.len() {
            streamer.frontier_complete = true;
        }
    }

    world.reserved_async_dense_slots = streamer.pending_terrain.len();
    debug_assert!(streamer.requested_chunks.len() <= MAX_FULL_CHUNK_RESIDENT);
    debug_assert!(world.chunks.len() <= MAX_FULL_CHUNK_RESIDENT);
    debug_assert!(
        world
            .chunks
            .len()
            .saturating_add(world.reserved_async_dense_slots)
            <= MAX_FULL_CHUNK_RESIDENT
    );
    publish_streaming_telemetry(&mut streamer, world.chunks.len(), &mut governor);
}

fn terrain_generator_for_chunk_worker(world: &VoxelWorld) -> TerrainGenerator {
    TerrainGenerator::from_identity(world.generator.generation_identity())
}

/// Re-mesh every chunk marked dirty. Meshing runs on background threads
/// using cheap `Arc`-clone snapshots of the chunk + 6 cardinal neighbours
/// so the main thread doesn't touch chunk storage while the mesher works.
fn mesh_dirty_chunks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    material_library: Res<crate::textures::MaterialLibrary>,
    water_surface: Res<crate::water::WaterSurfaceLibrary>,
    anchors: Query<&Transform, With<ChunkAnchor>>,
) {
    {
        // Destructure the resource so Rust can prove these are disjoint field
        // borrows; cloning a 2,400-entry request set every frame would defeat
        // the bounded streamer's allocation goal.
        let streamer = &mut *streamer;
        let requested = &streamer.requested_chunks;
        let dirty_queue = &mut streamer.dirty_queue;
        if !world.edit_dirty_chunks.is_empty() {
            dirty_queue.extend(
                world
                    .edit_dirty_chunks
                    .drain()
                    .filter(|pos| requested.contains(pos)),
            );
        }
        dirty_queue.retain(|pos| requested.contains(pos));
    }

    // Player's current chunk — used both for shadow-cull tagging on
    // newly-spawned mesh entities and for camera-priority scheduling.
    let (pcx, pcz) = anchors
        .get_single()
        .map(|t| {
            (
                crate::chunk::to_i32_safe(t.translation.x).div_euclid(CHUNK_SIZE_I),
                crate::chunk::to_i32_safe(t.translation.z).div_euclid(CHUNK_SIZE_I),
            )
        })
        .unwrap_or((0, 0));
    let visual_budget = WorldQualityBudget::resolve(
        settings.graphics,
        settings.scenery_quality,
        budget.profile,
        budget.quality,
    );

    // 1. Poll finished meshing tasks. Cap how many we actually *apply*
    //    (spawn entities for) per frame so a flood of finished tasks
    //    can't spike the frame budget with mesh.add() + commands.spawn().
    let spawn_cap = budget.mesh_applies_per_frame as usize;
    let mut applied = 0usize;
    let mut done_keys: Vec<ChunkPos> = Vec::new();
    let mut finished: Vec<(u64, ChunkPos, Vec<(MeshBucketKey, Mesh)>)> = Vec::new();

    for (pos, (task_epoch, task)) in streamer.pending_meshes.iter_mut() {
        if applied >= spawn_cap {
            break;
        }
        if let Some((cp, mesh)) = future::block_on(future::poll_once(task)) {
            finished.push((*task_epoch, cp, mesh));
            done_keys.push(*pos);
            applied += 1;
        }
    }
    for p in done_keys {
        streamer.pending_meshes.remove(&p);
    }
    for (task_epoch, pos, buckets) in finished {
        if !task_result_is_current(
            task_epoch,
            streamer.request_epoch,
            &streamer.requested_chunks,
            pos,
        ) || !world.chunks.contains_key(&pos)
        {
            streamer.telemetry.stale_results_total =
                streamer.telemetry.stale_results_total.saturating_add(1);
            continue;
        }
        let mut previous = streamer.entities.remove(&pos).unwrap_or_default();
        if buckets.is_empty() {
            for entry in previous {
                if let Some(entity_commands) = commands.get_entity(entry.entity) {
                    entity_commands.despawn_recursive();
                }
                let _ = meshes.remove(&entry.handle);
            }
            continue;
        }

        let (ox, oy, oz) = pos.origin();
        let transform = Transform::from_xyz(ox as f32, oy as f32, oz as f32);
        let shadow_radius = budget.shadow_radius.max(2);
        let shadow_r2 = shadow_radius * shadow_radius;
        let dx = pos.x - pcx;
        let dz = pos.z - pcz;
        let far = dx * dx + dz * dz > shadow_r2;
        let mut next_entries = Vec::with_capacity(buckets.len());

        for (bucket, mesh) in buckets {
            let material_id = bucket.material;
            let route = mesh_material_route(bucket);
            let water_material = match route {
                MeshMaterialRoute::WaterOptics => {
                    water_surface.handle_for(BlockType::Water as MaterialId)
                }
                MeshMaterialRoute::Standard | MeshMaterialRoute::Vegetation(_) => None,
            };
            let vegetation_material = match route {
                MeshMaterialRoute::Vegetation(species) => {
                    material_library.vegetation_handle_for_species(species)
                }
                MeshMaterialRoute::Standard | MeshMaterialRoute::WaterOptics => None,
            };
            let Some(material_handle) = material_library
                .handle_for(material_id)
                .or_else(|| streamer.material.clone())
            else {
                continue;
            };
            let culling_margin = if matches!(route, MeshMaterialRoute::Vegetation(_)) {
                crate::vegetation::MAX_VEGETATION_DISPLACEMENT_VOXELS
            } else {
                0.0
            };
            let aabb = bevy::render::primitives::Aabb::from_min_max(
                Vec3::splat(-culling_margin),
                Vec3::splat(CHUNK_SIZE_I as f32 + culling_margin),
            );

            if let Some(idx) = previous.iter().position(|entry| entry.bucket == bucket) {
                let mut entry = previous.swap_remove(idx);
                if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                    if let Some(water_material) = water_material.clone() {
                        entity_commands.insert(water_material);
                    } else if let Some(vegetation_material) = vegetation_material.clone() {
                        entity_commands.insert(vegetation_material);
                    } else {
                        entity_commands.insert(material_handle.clone());
                    }
                    entity_commands.insert(aabb);
                }
                if let Some(slot) = meshes.get_mut(&entry.handle) {
                    *slot = mesh;
                } else {
                    let new_handle = meshes.add(mesh);
                    if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                        entity_commands.insert(new_handle.clone());
                    }
                    entry.handle = new_handle;
                }
                next_entries.push(entry);
                continue;
            }

            let handle = meshes.add(mesh);
            let entity = if let Some(water_material) = water_material {
                if far {
                    commands
                        .spawn((
                            MaterialMeshBundle {
                                mesh: handle.clone(),
                                material: water_material,
                                transform,
                                ..default()
                            },
                            aabb,
                            bevy::pbr::NotShadowCaster,
                        ))
                        .id()
                } else {
                    commands
                        .spawn((
                            MaterialMeshBundle {
                                mesh: handle.clone(),
                                material: water_material,
                                transform,
                                ..default()
                            },
                            aabb,
                        ))
                        .id()
                }
            } else if let Some(vegetation_material) = vegetation_material {
                if far {
                    commands
                        .spawn((
                            MaterialMeshBundle {
                                mesh: handle.clone(),
                                material: vegetation_material,
                                transform,
                                ..default()
                            },
                            aabb,
                            bevy::pbr::NotShadowCaster,
                        ))
                        .id()
                } else {
                    commands
                        .spawn((
                            MaterialMeshBundle {
                                mesh: handle.clone(),
                                material: vegetation_material,
                                transform,
                                ..default()
                            },
                            aabb,
                        ))
                        .id()
                }
            } else if far {
                commands
                    .spawn((
                        PbrBundle {
                            mesh: handle.clone(),
                            material: material_handle,
                            transform,
                            ..default()
                        },
                        aabb,
                        bevy::pbr::NotShadowCaster,
                    ))
                    .id()
            } else {
                commands
                    .spawn((
                        PbrBundle {
                            mesh: handle.clone(),
                            material: material_handle,
                            transform,
                            ..default()
                        },
                        aabb,
                    ))
                    .id()
            };
            next_entries.push(ChunkMeshEntity {
                entity,
                handle,
                bucket,
            });
        }

        for entry in previous {
            if let Some(entity_commands) = commands.get_entity(entry.entity) {
                entity_commands.despawn_recursive();
            }
            let _ = meshes.remove(&entry.handle);
        }

        if !next_entries.is_empty() {
            streamer.entities.insert(pos, next_entries);
        }
    }

    // 2. Queue new mesh jobs. Camera-priority first.
    let max_in_flight = mesh_task_limit(budget.max_in_flight_meshes as usize);
    if streamer.pending_meshes.len() >= max_in_flight {
        refresh_mesh_entity_telemetry(&mut streamer);
        return;
    }

    let forward = anchors
        .get_single()
        .map(|t| {
            let f = t.forward();
            Vec2::new(f.x, f.z).normalize_or_zero()
        })
        .unwrap_or(Vec2::ZERO);

    // Drain a bounded window of the dirty set into a candidate list.
    // Retain anything we can't schedule this frame (no slots / missing
    // neighbours) so it is picked up next frame. We deliberately avoid
    // sorting the whole dirty backlog: bot cities, road edits, and dense
    // scenic biomes can mark thousands of chunks dirty at once, while the
    // runtime budget may only allow a handful of mesh jobs this frame.
    let mut candidates = std::mem::take(&mut streamer.mesh_candidates_scratch);
    candidates.clear();
    let dirty_total = streamer.dirty_queue.len();
    let schedule_budget = budget.meshes_per_frame.max(1) as usize;
    let scan_budget = dirty_mesh_candidate_scan_budget(
        dirty_total,
        schedule_budget,
        max_in_flight,
        budget.queue_pressure.max(budget.frame_pressure),
    );
    // AHashSet iteration is deliberately unordered. Sampling only an
    // arbitrary bounded window can therefore starve a nearby never-meshed
    // chunk behind thousands of distant dirty entries, leaving a visible
    // 16x16 hole even though camera-priority sorting happens afterwards.
    // Pull a small deterministic camera neighbourhood first; the remaining
    // global backlog keeps the old bounded scan and cannot monopolise a frame.
    const URGENT_MESH_RADIUS: i32 = 8;
    const URGENT_PRIORITY_BONUS: i32 = 1_000_000;
    let urgent = take_urgent_mesh_candidates(
        &mut streamer.dirty_queue,
        pcx,
        pcz,
        settings.vertical_chunks as i32,
        URGENT_MESH_RADIUS,
    );
    candidates.reserve((scan_budget + urgent.len()).min(dirty_total));
    for p in urgent {
        let Some(c) = world.chunks.get(&p) else {
            continue;
        };
        if !c.dirty {
            continue;
        }
        if streamer.pending_meshes.contains_key(&p) {
            streamer.dirty_queue.insert(p);
            continue;
        }
        let scenic = biome_stream_bonus(&world.generator, p.x, p.z);
        candidates.push((
            priority_score(p.x - pcx, p.z - pcz, forward) + scenic - URGENT_PRIORITY_BONUS,
            p,
        ));
    }
    let queue: AHashSet<ChunkPos> = std::mem::take(&mut streamer.dirty_queue);
    let mut scanned = 0usize;
    let mut queue_iter = queue.into_iter();
    while scanned < scan_budget {
        let Some(p) = queue_iter.next() else {
            break;
        };
        scanned += 1;
        let Some(c) = world.chunks.get(&p) else {
            continue;
        };
        if !c.dirty {
            continue;
        }
        if streamer.pending_meshes.contains_key(&p) {
            // A mesh task is already in-flight for this chunk based on
            // potentially stale neighbour data. We must NOT drop the
            // dirty flag here — put it back in the set so the next
            // frame (after the stale task finishes and drains out of
            // pending_meshes) re-enqueues a fresh task with the current
            // neighbour data. Dropping it here caused permanent dark
            // patches whenever a neighbour streamed in while the chunk
            // was already meshing.
            streamer.dirty_queue.insert(p);
            continue;
        }
        let scenic = biome_stream_bonus(&world.generator, p.x, p.z);
        candidates.push((priority_score(p.x - pcx, p.z - pcz, forward) + scenic, p));
    }
    streamer.dirty_queue.extend(queue_iter);
    candidates.sort_unstable_by_key(|(s, _)| *s);

    #[cfg(not(target_arch = "wasm32"))]
    let pool = AsyncComputeTaskPool::get();
    let horizon_cache = world.horizon_cache.clone();
    let horizon_generator = std::sync::Arc::new(world.generator.clone());
    let mut slots = max_in_flight - streamer.pending_meshes.len();
    let mut scheduled_this_frame = 0usize;

    for (_s, pos) in candidates.drain(..) {
        if slots == 0 || scheduled_this_frame >= schedule_budget {
            // Put back into the dirty set for next frame.
            streamer.dirty_queue.insert(pos);
            continue;
        }
        // Seam avoidance: require all 6 cardinal neighbours to be loaded
        // or proven-air before meshing. Horizontal neighbours prevent XZ seams; vertical
        // neighbours close a nasty streaming race where a chunk meshed
        // with its top neighbour missing would sample AIR above, cache
        // the top faces, and then — if the newly-loaded top chunk's
        // dirty re-queue happened while this chunk was already in
        // pending_meshes — never get re-meshed, leaving visible holes /
        // dark patches scattered across the terrain at high altitudes.
        // Terrain slots above the cached column ceiling are treated as
        // implicit AIR, so the streamer does not need placeholder chunks.
        let vertical_chunks = settings.vertical_chunks as i32;
        let neighbours_needed = [
            ChunkPos::new(pos.x + 1, pos.y, pos.z),
            ChunkPos::new(pos.x - 1, pos.y, pos.z),
            ChunkPos::new(pos.x, pos.y, pos.z + 1),
            ChunkPos::new(pos.x, pos.y, pos.z - 1),
            ChunkPos::new(pos.x, pos.y + 1, pos.z),
            ChunkPos::new(pos.x, pos.y - 1, pos.z),
        ];
        let all_neighbours_ready = neighbours_needed.into_iter().all(|n| {
            mesh_neighbour_resolved_for_request(
                &mut world,
                &streamer.requested_chunks,
                n,
                vertical_chunks,
            )
        });
        if !all_neighbours_ready {
            // Neighbours haven't streamed in yet; try again next frame.
            streamer.dirty_queue.insert(pos);
            continue;
        }

        // Safe fast-skip: a uniform chunk is invisible iff every one of
        // its 6 neighbours is the same uniform voxel, or the neighbour
        // slot is known implicit AIR and this chunk is AIR too.
        let fast_skip = uniform_chunk_is_trivially_invisible(&mut world, pos, vertical_chunks);
        if fast_skip {
            if let Some(previous) = streamer.entities.remove(&pos) {
                for entry in previous {
                    if let Some(entity_commands) = commands.get_entity(entry.entity) {
                        entity_commands.despawn_recursive();
                    }
                    let _ = meshes.remove(&entry.handle);
                }
            }
            if let Some(c) = world.chunks.get_mut(&pos) {
                c.dirty = false;
            }
            continue;
        }

        let snap = ChunkSnapshot::build(&world, pos);

        // Mark clean BEFORE the task starts so a second mutation during
        // meshing will still re-flag the chunk as dirty.
        if let Some(c) = world.chunks.get_mut(&pos) {
            c.dirty = false;
        }

        // LOD: chunks further than `lod_radius` from the player skip
        // per-corner ambient occlusion. Visually indistinguishable
        // through fog at that distance; mesher runs ~3× faster and
        // emits ~40% fewer triangles (greedy merge no longer breaks
        // on AO seams). Threshold chosen so the nearest ~60% of the
        // visible disc keeps full-quality AO.
        let dx = pos.x - pcx;
        let dz = pos.z - pcz;
        let use_ao = visual_budget.detail_tier(dx, dz) != VoxelDetailTier::Macro;
        let emission_budget = visual_budget.emission;
        let column_horizon_cache = horizon_cache.clone();
        let column_horizon_generator = std::sync::Arc::clone(&horizon_generator);

        #[cfg(target_arch = "wasm32")]
        {
            let horizon = column_horizon_cache
                .get_or_build(pos, |x, z| column_horizon_generator.surface_height_at(x, z));
            let buckets = build_mesh_buckets_budgeted_with_horizon(
                pos,
                |wx, wy, wz| snap.sample_with_material(wx, wy, wz),
                use_ao,
                emission_budget,
                Some(&horizon),
            );
            apply_mesh_buckets_now(
                &mut commands,
                &mut meshes,
                &mut streamer,
                &material_library,
                &water_surface,
                &budget,
                pcx,
                pcz,
                pos,
                buckets,
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let task = pool.spawn(async move {
                let horizon = column_horizon_cache
                    .get_or_build(pos, |x, z| column_horizon_generator.surface_height_at(x, z));
                let buckets = build_mesh_buckets_budgeted_with_horizon(
                    pos,
                    |wx, wy, wz| snap.sample_with_material(wx, wy, wz),
                    use_ao,
                    emission_budget,
                    Some(&horizon),
                );
                (pos, buckets)
            });
            let request_epoch = streamer.request_epoch;
            streamer.pending_meshes.insert(pos, (request_epoch, task));
        }
        slots -= 1;
        scheduled_this_frame += 1;
    }

    // Return the scratch buffer to the resource so it keeps its
    // allocated capacity for next frame.
    streamer.mesh_candidates_scratch = candidates;

    // 3. Clean up orphaned mesh entities whose chunk has streamed out.
    //    Free the GPU buffer too so long sessions don't accumulate.
    //    Only runs on frames where `stream_chunks` actually unloaded
    //    at least one chunk \u2014 otherwise nothing can be newly orphaned
    //    and walking `entities` (\u22482500+ entries at RD=50) is pure
    //    per-frame waste.
    if streamer.needs_orphan_scan {
        let mut orphaned = Vec::new();
        for (pos, group) in streamer.entities.iter() {
            if !world.chunks.contains_key(pos) {
                orphaned.push((*pos, group.clone()));
            }
        }
        for (pos, group) in orphaned {
            for entry in group {
                if let Some(entity_commands) = commands.get_entity(entry.entity) {
                    entity_commands.despawn_recursive();
                }
                let _ = meshes.remove(&entry.handle);
            }
            streamer.entities.remove(&pos);
        }
        streamer.needs_orphan_scan = false;
    }
    refresh_mesh_entity_telemetry(&mut streamer);
}

#[cfg(target_arch = "wasm32")]
fn mark_chunk_and_neighbours_dirty(
    world: &mut VoxelWorld,
    streamer: &mut ChunkStreamer,
    cp: ChunkPos,
) {
    streamer.dirty_queue.insert(cp);
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        let n = ChunkPos::new(cp.x + dx, cp.y + dy, cp.z + dz);
        if let Some(c) = world.chunks.get_mut(&n) {
            c.dirty = true;
            streamer.dirty_queue.insert(n);
        }
    }
}

fn dirty_mesh_candidate_scan_budget(
    dirty_total: usize,
    schedule_budget: usize,
    max_in_flight: usize,
    pressure: f32,
) -> usize {
    if dirty_total == 0 {
        return 0;
    }
    let pressure = pressure.clamp(0.0, 1.25);
    let multiplier = if pressure >= 0.85 {
        20
    } else if pressure >= 0.55 {
        48
    } else {
        96
    };
    let floor = if pressure >= 0.85 {
        32
    } else {
        max_in_flight.saturating_mul(2).max(64)
    };
    let budget = schedule_budget.max(1).saturating_mul(multiplier).max(floor);
    budget.min(dirty_total)
}

/// Remove dirty chunks in a deterministic camera-centred cylinder so they can
/// be sorted ahead of an unordered global backlog. This is bounded by
/// `(2r+1)^2 * vertical_chunks` and performs no whole-queue walk.
fn take_urgent_mesh_candidates(
    dirty: &mut AHashSet<ChunkPos>,
    pcx: i32,
    pcz: i32,
    vertical_chunks: i32,
    radius: i32,
) -> Vec<ChunkPos> {
    let radius = radius.max(0);
    let vertical_chunks = vertical_chunks.max(0);
    let side = radius.saturating_mul(2).saturating_add(1) as usize;
    let mut urgent = Vec::with_capacity(
        dirty.len().min(
            side.saturating_mul(side)
                .saturating_mul(vertical_chunks as usize),
        ),
    );
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dz * dz > radius * radius {
                continue;
            }
            for cy in 0..vertical_chunks {
                let pos = ChunkPos::new(pcx + dx, cy, pcz + dz);
                if dirty.remove(&pos) {
                    urgent.push(pos);
                }
            }
        }
    }
    urgent
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn apply_mesh_buckets_now(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    streamer: &mut ChunkStreamer,
    material_library: &crate::textures::MaterialLibrary,
    water_surface: &crate::water::WaterSurfaceLibrary,
    budget: &RuntimeBudget,
    pcx: i32,
    pcz: i32,
    pos: ChunkPos,
    buckets: Vec<(MeshBucketKey, Mesh)>,
) {
    let mut previous = streamer.entities.remove(&pos).unwrap_or_default();
    if buckets.is_empty() {
        for entry in previous {
            if let Some(entity_commands) = commands.get_entity(entry.entity) {
                entity_commands.despawn_recursive();
            }
            let _ = meshes.remove(&entry.handle);
        }
        return;
    }

    let (ox, oy, oz) = pos.origin();
    let transform = Transform::from_xyz(ox as f32, oy as f32, oz as f32);
    let shadow_radius = budget.shadow_radius.max(2);
    let shadow_r2 = shadow_radius * shadow_radius;
    let dx = pos.x - pcx;
    let dz = pos.z - pcz;
    let far = dx * dx + dz * dz > shadow_r2;
    let mut next_entries = Vec::with_capacity(buckets.len());

    for (bucket, mesh) in buckets {
        let material_id = bucket.material;
        let route = mesh_material_route(bucket);
        let water_material = match route {
            MeshMaterialRoute::WaterOptics => {
                water_surface.handle_for(BlockType::Water as MaterialId)
            }
            MeshMaterialRoute::Standard | MeshMaterialRoute::Vegetation(_) => None,
        };
        let vegetation_material = match route {
            MeshMaterialRoute::Vegetation(species) => {
                material_library.vegetation_handle_for_species(species)
            }
            MeshMaterialRoute::Standard | MeshMaterialRoute::WaterOptics => None,
        };
        let Some(material_handle) = material_library
            .handle_for(material_id)
            .or_else(|| streamer.material.clone())
        else {
            continue;
        };
        let culling_margin = if matches!(route, MeshMaterialRoute::Vegetation(_)) {
            crate::vegetation::MAX_VEGETATION_DISPLACEMENT_VOXELS
        } else {
            0.0
        };
        let aabb = bevy::render::primitives::Aabb::from_min_max(
            Vec3::splat(-culling_margin),
            Vec3::splat(CHUNK_SIZE_I as f32 + culling_margin),
        );

        if let Some(idx) = previous.iter().position(|entry| entry.bucket == bucket) {
            let mut entry = previous.swap_remove(idx);
            if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                if let Some(water_material) = water_material.clone() {
                    entity_commands.insert(water_material);
                } else if let Some(vegetation_material) = vegetation_material.clone() {
                    entity_commands.insert(vegetation_material);
                } else {
                    entity_commands.insert(material_handle.clone());
                }
                entity_commands.insert(aabb);
            }
            if let Some(slot) = meshes.get_mut(&entry.handle) {
                *slot = mesh;
            } else {
                let new_handle = meshes.add(mesh);
                if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                    entity_commands.insert(new_handle.clone());
                }
                entry.handle = new_handle;
            }
            next_entries.push(entry);
            continue;
        }

        let handle = meshes.add(mesh);
        let entity = if let Some(water_material) = water_material {
            if far {
                commands
                    .spawn((
                        MaterialMeshBundle {
                            mesh: handle.clone(),
                            material: water_material,
                            transform,
                            ..default()
                        },
                        aabb,
                        bevy::pbr::NotShadowCaster,
                    ))
                    .id()
            } else {
                commands
                    .spawn((
                        MaterialMeshBundle {
                            mesh: handle.clone(),
                            material: water_material,
                            transform,
                            ..default()
                        },
                        aabb,
                    ))
                    .id()
            }
        } else if let Some(vegetation_material) = vegetation_material {
            if far {
                commands
                    .spawn((
                        MaterialMeshBundle {
                            mesh: handle.clone(),
                            material: vegetation_material,
                            transform,
                            ..default()
                        },
                        aabb,
                        bevy::pbr::NotShadowCaster,
                    ))
                    .id()
            } else {
                commands
                    .spawn((
                        MaterialMeshBundle {
                            mesh: handle.clone(),
                            material: vegetation_material,
                            transform,
                            ..default()
                        },
                        aabb,
                    ))
                    .id()
            }
        } else if far {
            commands
                .spawn((
                    PbrBundle {
                        mesh: handle.clone(),
                        material: material_handle,
                        transform,
                        ..default()
                    },
                    aabb,
                    bevy::pbr::NotShadowCaster,
                ))
                .id()
        } else {
            commands
                .spawn((
                    PbrBundle {
                        mesh: handle.clone(),
                        material: material_handle,
                        transform,
                        ..default()
                    },
                    aabb,
                ))
                .id()
        };
        next_entries.push(ChunkMeshEntity {
            entity,
            handle,
            bucket,
        });
    }

    for entry in previous {
        if let Some(entity_commands) = commands.get_entity(entry.entity) {
            entity_commands.despawn_recursive();
        }
        let _ = meshes.remove(&entry.handle);
    }

    if !next_entries.is_empty() {
        streamer.entities.insert(pos, next_entries);
    }
}

/// Immutable snapshot of a chunk + its 6 cardinal neighbours, used by
/// the off-thread mesher. All storage is `Arc`-shared so the snapshot is
/// an O(1) refcount bump instead of 7 × 4 KB memcpy on the main thread.
struct ChunkSnapshot {
    pos: ChunkPos,
    center: SharedVoxels,
    center_materials: SharedMaterials,
    neighbours: [Option<SharedVoxels>; 6],
    neighbour_materials: [Option<SharedMaterials>; 6],
}

impl ChunkSnapshot {
    fn build(world: &VoxelWorld, pos: ChunkPos) -> Self {
        let center = world
            .chunks
            .get(&pos)
            .map(|c| c.voxels_shared())
            .unwrap_or_else(|| std::sync::Arc::new([AIR; CHUNK_VOLUME]));
        let center_materials = world
            .chunks
            .get(&pos)
            .map(|c| c.materials_shared())
            .unwrap_or_else(|| std::sync::Arc::new([DEFAULT_MATERIAL; CHUNK_VOLUME]));
        let dirs = [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ];
        let neighbours = std::array::from_fn(|i| {
            let (dx, dy, dz) = dirs[i];
            world
                .chunks
                .get(&ChunkPos::new(pos.x + dx, pos.y + dy, pos.z + dz))
                .map(|c| c.voxels_shared())
        });
        let neighbour_materials = std::array::from_fn(|i| {
            let (dx, dy, dz) = dirs[i];
            world
                .chunks
                .get(&ChunkPos::new(pos.x + dx, pos.y + dy, pos.z + dz))
                .map(|c| c.materials_shared())
        });
        Self {
            pos,
            center,
            center_materials,
            neighbours,
            neighbour_materials,
        }
    }

    #[inline]
    fn sample_with_material(&self, wx: i32, wy: i32, wz: i32) -> (Voxel, MaterialId) {
        let (cp, lx, ly, lz) = world_to_chunk(wx, wy, wz);
        let dx = cp.x - self.pos.x;
        let dy = cp.y - self.pos.y;
        let dz = cp.z - self.pos.z;
        let idx = Chunk::index(lx, ly, lz);
        if (dx, dy, dz) == (0, 0, 0) {
            return (self.center[idx], self.center_materials[idx]);
        }
        let ni = match (dx, dy, dz) {
            (1, 0, 0) => 0,
            (-1, 0, 0) => 1,
            (0, 1, 0) => 2,
            (0, -1, 0) => 3,
            (0, 0, 1) => 4,
            (0, 0, -1) => 5,
            _ => return (AIR, DEFAULT_MATERIAL),
        };
        let voxel = self.neighbours[ni].as_ref().map(|v| v[idx]).unwrap_or(AIR);
        let material = self.neighbour_materials[ni]
            .as_ref()
            .map(|m| m[idx])
            .unwrap_or(DEFAULT_MATERIAL);
        (voxel, material)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::BlockType;

    #[test]
    fn mesh_shader_route_uses_voxel_class_and_species_not_editable_material_id() {
        let water_material = BlockType::Water as MaterialId;
        let leaves_material = BlockType::Leaves as MaterialId;
        let stone_with_water_material =
            MeshBucketKey::new(BlockType::Stone as Voxel, water_material);
        let stone_with_leaves_material =
            MeshBucketKey::new(BlockType::Stone as Voxel, leaves_material);
        let custom_water = MeshBucketKey::new(
            BlockType::Water as Voxel,
            crate::blocks::CUSTOM_MATERIAL_BASE,
        );
        let custom_leaves = MeshBucketKey::new(
            BlockType::Leaves as Voxel,
            crate::blocks::CUSTOM_MATERIAL_BASE,
        );
        let custom_sakura = MeshBucketKey::new(
            BlockType::SakuraPetals as Voxel,
            crate::blocks::CUSTOM_MATERIAL_BASE,
        );
        let leaves_with_sakura_material = MeshBucketKey::new(
            BlockType::Leaves as Voxel,
            BlockType::SakuraPetals as MaterialId,
        );

        assert_eq!(
            mesh_material_route(stone_with_water_material),
            MeshMaterialRoute::Standard
        );
        assert_eq!(
            mesh_material_route(custom_water),
            MeshMaterialRoute::WaterOptics
        );
        assert_eq!(
            mesh_material_route(stone_with_leaves_material),
            MeshMaterialRoute::Standard
        );
        assert_eq!(
            mesh_material_route(custom_leaves),
            MeshMaterialRoute::Vegetation(VegetationSpecies::Leaves)
        );
        assert_eq!(
            mesh_material_route(custom_sakura),
            MeshMaterialRoute::Vegetation(VegetationSpecies::SakuraPetals)
        );
        assert_eq!(
            mesh_material_route(leaves_with_sakura_material),
            MeshMaterialRoute::Vegetation(VegetationSpecies::Leaves),
            "voxel species, not a borrowed foliage material id, owns the preset"
        );
        assert_ne!(stone_with_water_material, custom_water);
        assert_ne!(custom_leaves, custom_sakura);
    }

    #[test]
    fn near_chunk_worker_preserves_the_complete_v1_generation_identity() {
        let identity = WorldGenerationIdentity {
            seed: 0xA11C_E551,
            world_profile: crate::settings::WorldProfile::Natural,
            scenery_quality: crate::settings::SceneryQuality::Lush,
            terrain_grammar: TerrainGrammarVersion::V1,
        };
        let mut world = VoxelWorld::new();
        world.generator = TerrainGenerator::from_identity(identity);

        let worker = terrain_generator_for_chunk_worker(&world);

        assert_eq!(worker.generation_identity(), identity);
        assert_eq!(world.generator.generation_identity(), identity);
    }

    #[test]
    fn mesh_neighbour_resolution_uses_the_exact_request_boundary() {
        let mut world = VoxelWorld::new();
        let mut requested = AHashSet::new();
        let neighbour = ChunkPos::new(1, 0, 0);
        world.column_top_cy.insert((neighbour.x, neighbour.z), 0);

        requested.insert(neighbour);
        assert!(
            !mesh_neighbour_resolved_for_request(&mut world, &requested, neighbour, 8),
            "a requested below-ceiling slot must wait for its terrain"
        );

        requested.clear();
        assert!(
            mesh_neighbour_resolved_for_request(&mut world, &requested, neighbour, 8),
            "an unrequested slot is a terminal boundary, not pending work"
        );

        let uncached_boundary = ChunkPos::new(-7, 0, 11);
        assert!(!world
            .column_top_cy
            .contains_key(&(uncached_boundary.x, uncached_boundary.z)));
        assert!(mesh_neighbour_resolved_for_request(
            &mut world,
            &requested,
            uncached_boundary,
            8
        ));
        assert!(
            !world
                .column_top_cy
                .contains_key(&(uncached_boundary.x, uncached_boundary.z)),
            "outside-plan resolution must short-circuit without generator/cache work"
        );

        requested.insert(neighbour);
        world
            .chunks
            .insert(neighbour, crate::chunk::Chunk::new(neighbour));
        assert!(mesh_neighbour_resolved_for_request(
            &mut world, &requested, neighbour, 8
        ));

        let proven_air = ChunkPos::new(2, 3, 0);
        world.column_top_cy.insert((proven_air.x, proven_air.z), 0);
        requested.insert(proven_air);
        assert!(
            mesh_neighbour_resolved_for_request(&mut world, &requested, proven_air, 8),
            "a requested slot above the exact column ceiling is resolved AIR"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    struct EditStoreTestRoot(PathBuf);

    #[cfg(not(target_arch = "wasm32"))]
    impl EditStoreTestRoot {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "voxel-native-edit-store-{label}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create isolated edit-store test root");
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl Drop for EditStoreTestRoot {
        fn drop(&mut self) {
            let temp = std::env::temp_dir();
            assert!(
                self.0.starts_with(&temp)
                    && self
                        .0
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("voxel-native-edit-store-")),
                "refuse broad test cleanup"
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn edit_store_identity(grammar: TerrainGrammarVersion, seed: u32) -> WorldGenerationIdentity {
        WorldGenerationIdentity {
            seed,
            world_profile: crate::settings::WorldProfile::Natural,
            scenery_quality: crate::settings::SceneryQuality::Lush,
            terrain_grammar: grammar,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn saved_manifest(outcome: EditedOverrideSaveOutcome) -> crate::settings::WorldEditManifest {
        match outcome {
            EditedOverrideSaveOutcome::Saved(manifest) => manifest,
            EditedOverrideSaveOutcome::Blocked { reason } => {
                panic!("expected compatible edit save, blocked: {reason}")
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn compatible_overrides(
        load: EditedOverrideStoreLoad,
    ) -> AHashMap<ChunkPos, EditedChunkOverride> {
        match load {
            EditedOverrideStoreLoad::Compatible { overrides, .. } => overrides,
            EditedOverrideStoreLoad::Blocked { reason } => {
                panic!("expected compatible edit load, blocked: {reason}")
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn tree_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            base: &Path,
            current: &Path,
            out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            let mut entries = fs::read_dir(current)
                .expect("read test tree")
                .map(|entry| entry.expect("read test entry"))
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let kind = entry.file_type().expect("read test file type");
                if kind.is_dir() {
                    visit(base, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(base)
                            .expect("relative test path")
                            .to_owned(),
                        fs::read(path).expect("read test file"),
                    );
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        if root.exists() {
            visit(root, root, &mut out);
        }
        out
    }

    fn cache_air_columns(world: &mut VoxelWorld, top_cy: i32) {
        for cx in -1..=1 {
            for cz in -1..=1 {
                world.column_top_cy.insert((cx, cz), top_cy);
            }
        }
    }

    fn solid_chunk(pos: ChunkPos, voxel: Voxel) -> Chunk {
        let mut chunk = Chunk::new(pos);
        for y in 0..crate::chunk::CHUNK_SIZE {
            for z in 0..crate::chunk::CHUNK_SIZE {
                for x in 0..crate::chunk::CHUNK_SIZE {
                    chunk.set(x, y, z, voxel);
                }
            }
        }
        chunk.finalize_uniform_flags();
        chunk
    }

    fn override_chunk(voxel: Voxel) -> EditedChunkOverride {
        EditedChunkOverride {
            voxels: vec![voxel; CHUNK_VOLUME],
            materials: Vec::new(),
        }
    }

    fn synthetic_column_demand(cx: i32, cz: i32) -> ColumnDemand {
        let hash = i64::from(cx)
            .wrapping_mul(73_856_093)
            .wrapping_add(i64::from(cz).wrapping_mul(19_349_663));
        ColumnDemand {
            top_cy: 2 + (hash.unsigned_abs() % 12) as i32,
            has_edits: hash.rem_euclid(97) == 0,
        }
    }

    #[test]
    fn exact_voxel_chunk_residency_does_not_infer_missing_vertical_chunks() {
        let mut world = VoxelWorld::new();
        let lower = ChunkPos::new(-1, -1, 2);
        let upper = ChunkPos::new(-1, 0, 2);
        let world_x = -1;
        let world_z = 2 * crate::chunk::CHUNK_SIZE_I + 3;

        world.insert_chunk(lower, Chunk::new(lower));
        assert!(world.is_column_loaded(world_x, world_z));
        assert!(world.is_voxel_chunk_loaded(world_x, -1, world_z));
        assert!(!world.is_voxel_chunk_loaded(world_x, 0, world_z));
        assert_eq!(world.voxel_at(world_x, 0, world_z), AIR);

        world.insert_chunk(upper, Chunk::new(upper));
        assert!(world.is_voxel_chunk_loaded(world_x, 0, world_z));

        world.remove_chunk(&upper);
        assert!(world.is_column_loaded(world_x, world_z));
        assert!(!world.is_voxel_chunk_loaded(world_x, 0, world_z));

        world.remove_chunk(&lower);
        assert!(!world.is_column_loaded(world_x, world_z));
    }

    #[test]
    fn direct_edits_share_the_dense_residency_cap_with_async_reservations() {
        let mut world = VoxelWorld::new();
        world.reserved_async_dense_slots = MAX_FULL_CHUNK_RESIDENT - 1;

        assert!(world.edit_set_voxel(0, 0, 0, BlockType::Stone.into()));
        assert_eq!(world.chunks.len(), 1);
        assert_eq!(world.loaded_column_counts.get(&(0, 0)), Some(&1));
        assert_eq!(
            world
                .chunks
                .len()
                .saturating_add(world.reserved_async_dense_slots),
            MAX_FULL_CHUNK_RESIDENT
        );

        let rejected = ChunkPos::new(1, 0, 0);
        assert!(!world.edit_set_voxel(CHUNK_SIZE_I, 0, 0, BlockType::Limestone.into()));
        assert!(!world.chunks.contains_key(&rejected));
        assert!(!world.edited_overrides.contains_key(&rejected));
        assert!(!world.loaded_column_counts.contains_key(&(1, 0)));
        assert_eq!(world.chunks.len(), 1);
    }

    #[test]
    fn direct_edit_creation_updates_exact_column_residency() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(-2, 3, 4);
        let wx = pos.x * CHUNK_SIZE_I;
        let wy = pos.y * CHUNK_SIZE_I;
        let wz = pos.z * CHUNK_SIZE_I;

        assert!(!world.is_column_loaded(wx, wz));
        assert!(world.edit_set_voxel(wx, wy, wz, BlockType::Stone.into()));

        assert!(world.chunks.contains_key(&pos));
        assert_eq!(world.loaded_column_counts.get(&(pos.x, pos.z)), Some(&1));
        assert!(world.is_column_loaded(wx, wz));
    }

    #[test]
    fn direct_edits_reject_only_new_records_at_the_override_cap() {
        let mut world = VoxelWorld::new();
        let existing = ChunkPos::new(0, 0, 0);
        for x in 0..MAX_EDITED_OVERRIDE_RECORDS {
            world.edited_overrides.insert(
                ChunkPos::new(x as i32, 50, 0),
                EditedChunkOverride {
                    voxels: Vec::new(),
                    materials: Vec::new(),
                },
            );
        }
        world.edited_overrides.insert(
            existing,
            EditedChunkOverride {
                voxels: Vec::new(),
                materials: Vec::new(),
            },
        );
        world.edited_overrides.remove(&ChunkPos::new(
            (MAX_EDITED_OVERRIDE_RECORDS - 1) as i32,
            50,
            0,
        ));
        assert_eq!(world.edited_overrides.len(), MAX_EDITED_OVERRIDE_RECORDS);

        let new_record = ChunkPos::new(-5, 0, 0);
        world.insert_chunk(new_record, solid_chunk(new_record, BlockType::Stone.into()));
        assert!(!world.edit_set_voxel(
            new_record.x * CHUNK_SIZE_I,
            0,
            0,
            BlockType::Limestone.into()
        ));
        assert_eq!(
            world
                .chunks
                .get(&new_record)
                .expect("new-record test chunk remains resident")
                .get(0, 0, 0),
            Voxel::from(BlockType::Stone)
        );
        assert!(!world.edited_overrides.contains_key(&new_record));

        world.insert_chunk(existing, solid_chunk(existing, BlockType::Stone.into()));
        assert!(world.edit_set_voxel(0, 0, 0, BlockType::Limestone.into()));
        assert_eq!(world.edited_overrides.len(), MAX_EDITED_OVERRIDE_RECORDS);
        assert_eq!(
            world
                .chunks
                .get(&existing)
                .expect("existing-record test chunk remains resident")
                .get(0, 0, 0),
            Voxel::from(BlockType::Limestone)
        );
    }

    #[test]
    fn one_batch_cannot_reserve_more_new_override_records_than_remain() {
        let mut world = VoxelWorld::new();
        for x in 0..(MAX_EDITED_OVERRIDE_RECORDS - 1) {
            world.edited_overrides.insert(
                ChunkPos::new(x as i32, 80, 0),
                EditedChunkOverride {
                    voxels: Vec::new(),
                    materials: Vec::new(),
                },
            );
        }
        let admitted = ChunkPos::new(-10, 0, 0);
        let rejected = ChunkPos::new(-11, 0, 0);
        world.insert_chunk(admitted, solid_chunk(admitted, BlockType::Stone.into()));
        world.insert_chunk(rejected, solid_chunk(rejected, BlockType::Stone.into()));
        let mut batch = WorldEditBatch::default();

        assert!(world
            .edit_set_voxel_batched(
                admitted.x * CHUNK_SIZE_I,
                0,
                0,
                BlockType::Limestone.into(),
                &mut batch,
            )
            .is_some());
        assert!(world
            .edit_set_voxel_batched(
                rejected.x * CHUNK_SIZE_I,
                0,
                0,
                BlockType::Limestone.into(),
                &mut batch,
            )
            .is_none());
        assert!(batch.override_record_rejections.contains(&rejected));
        world.finish_edit_batch(batch);

        assert_eq!(world.edited_overrides.len(), MAX_EDITED_OVERRIDE_RECORDS);
        assert!(world.edited_overrides.contains_key(&admitted));
        assert!(!world.edited_overrides.contains_key(&rejected));
        assert_eq!(
            world
                .chunks
                .get(&rejected)
                .expect("rejected batch chunk remains resident")
                .get(0, 0, 0),
            Voxel::from(BlockType::Stone)
        );
    }

    #[test]
    fn remote_resident_chunks_force_a_plan_rebuild_after_dirty_drain() {
        let mut world = VoxelWorld::new();
        let remote = ChunkPos::new(500, 2, -700);
        world.insert_chunk(remote, Chunk::new(remote));
        world.edit_dirty_chunks.clear();
        let mut requested = AHashSet::new();

        assert!(has_unrequested_resident_chunk(&world, &requested));
        requested.insert(remote);
        assert!(!has_unrequested_resident_chunk(&world, &requested));
    }

    #[test]
    fn resolved_voxel_distinguishes_cached_air_from_unloaded_edits() {
        let mut world = VoxelWorld::new();
        let world_x = -3;
        let world_y = 5 * crate::chunk::CHUNK_SIZE_I + 2;
        let world_z = 2 * crate::chunk::CHUNK_SIZE_I + 7;
        let (pos, _, _, _) = world_to_chunk(world_x, world_y, world_z);

        assert_eq!(world.voxel_at_if_resolved(world_x, world_y, world_z), None);
        world.column_top_cy.insert((pos.x, pos.z), pos.y - 1);
        assert_eq!(
            world.voxel_at_if_resolved(world_x, world_y, world_z),
            Some(AIR)
        );

        world.edited_overrides.insert(pos, override_chunk(AIR));
        assert_eq!(world.voxel_at_if_resolved(world_x, world_y, world_z), None);

        let mut loaded = Chunk::new(pos);
        loaded.set(0, 0, 0, Voxel::from(BlockType::Stone));
        world.insert_chunk(pos, loaded);
        assert!(world
            .voxel_at_if_resolved(world_x, world_y, world_z)
            .is_some());
    }

    #[test]
    fn extreme_visual_distance_cannot_expand_dense_request_budget() {
        let plan = build_interaction_request_plan(
            0,
            0,
            10_000,
            16,
            (1, 0),
            (1, 0),
            MAX_FULL_CHUNK_RESIDENT,
            |_cx, _cz| ColumnDemand {
                top_cy: 15,
                has_edits: false,
            },
        );
        let unique: AHashSet<_> = plan.chunks.iter().copied().collect();

        assert_eq!(plan.radius, MAX_INTERACTION_RADIUS_CHUNKS);
        assert_eq!(plan.chunks.len(), MAX_FULL_CHUNK_RESIDENT);
        assert_eq!(unique.len(), plan.chunks.len());
        assert_eq!(plan.columns.len(), MAX_FULL_CHUNK_RESIDENT / 16);
        assert!(plan.columns.len() < 797, "only a bounded subset is dense");

        // The collision/edit core wins before predictive/frustum bias.
        for dx in -GUARANTEED_INTERACTION_CORE_CHUNKS..=GUARANTEED_INTERACTION_CORE_CHUNKS {
            for dz in -GUARANTEED_INTERACTION_CORE_CHUNKS..=GUARANTEED_INTERACTION_CORE_CHUNKS {
                if dx * dx + dz * dz
                    <= GUARANTEED_INTERACTION_CORE_CHUNKS * GUARANTEED_INTERACTION_CORE_CHUNKS
                {
                    assert!(unique.contains(&ChunkPos::new(dx, 0, dz)));
                    assert!(unique.contains(&ChunkPos::new(dx, 15, dz)));
                }
            }
        }
    }

    #[test]
    fn many_kilometre_flight_keeps_requests_residency_and_tasks_bounded() {
        let mut resident = AHashSet::<ChunkPos>::new();
        let mut terrain_jobs = AHashMap::<ChunkPos, (u64, ())>::new();
        let mut epoch = 0_u64;
        let mut peak_requested = 0_usize;
        let mut peak_resident = 0_usize;
        let mut peak_tasks = 0_usize;

        // Each step jumps 2.2 km in X and varies Z. Total traversed distance
        // is far beyond a normal play session, yet no collection is allowed
        // to scale with the path length.
        for step in 0_i32..320 {
            epoch += 1;
            let pcx = step.saturating_mul(137);
            let pcz = (step % 17 - 8).saturating_mul(61);
            let motion = if step % 2 == 0 { (1, 0) } else { (1, 1) };
            let plan = build_interaction_request_plan(
                pcx,
                pcz,
                64,
                16,
                motion,
                motion,
                MAX_FULL_CHUNK_RESIDENT,
                synthetic_column_demand,
            );
            let requested: AHashSet<_> = plan.chunks.iter().copied().collect();
            peak_requested = peak_requested.max(requested.len());

            resident.retain(|pos| requested.contains(pos));
            retarget_epoch_jobs(&mut terrain_jobs, &requested, epoch);
            let task_limit = terrain_task_limit(usize::MAX, resident.len(), requested.len());
            for pos in &plan.chunks {
                if terrain_jobs.len() >= task_limit {
                    break;
                }
                if !resident.contains(pos) && !terrain_jobs.contains_key(pos) {
                    terrain_jobs.insert(*pos, (epoch, ()));
                }
            }
            peak_resident = peak_resident.max(resident.len());
            peak_tasks = peak_tasks.max(terrain_jobs.len());

            assert!(requested.len() <= MAX_FULL_CHUNK_RESIDENT);
            assert!(resident.len() <= MAX_FULL_CHUNK_RESIDENT);
            assert!(terrain_jobs.len() <= MAX_IN_FLIGHT_TERRAIN_TASKS);
            assert!(resident.len() + terrain_jobs.len() <= MAX_FULL_CHUNK_RESIDENT);
            assert!(terrain_jobs
                .iter()
                .all(|(pos, (job_epoch, _))| requested.contains(pos) && *job_epoch == epoch));

            // Complete a bounded wave, mimicking the real apply cap without
            // allocating 16³ arrays in this invariant test.
            let completed: Vec<_> = terrain_jobs.keys().take(24).copied().collect();
            for pos in completed {
                terrain_jobs.remove(&pos);
                resident.insert(pos);
            }

            // Exercise the fully settled state too. The next kilometre jump
            // then runs exact-set eviction over the largest legal resident
            // collection rather than only a partially loaded frontier.
            terrain_jobs.clear();
            resident = requested;
            peak_resident = peak_resident.max(resident.len());
        }

        assert!(
            peak_requested > 2_000,
            "test must meaningfully exercise the cap"
        );
        assert!(
            peak_resident > 2_000,
            "settled residency must meaningfully exercise the cap"
        );
        assert_eq!(peak_tasks, MAX_IN_FLIGHT_TERRAIN_TASKS);
        println!(
            "synthetic km route peaks: requested={peak_requested}, resident={peak_resident}, terrain_jobs={peak_tasks}"
        );
    }

    #[test]
    fn teleport_cancels_old_epoch_and_rejects_stale_results() {
        let old_plan = build_interaction_request_plan(
            0,
            0,
            64,
            8,
            (1, 0),
            (1, 0),
            MAX_FULL_CHUNK_RESIDENT,
            synthetic_column_demand,
        );
        let new_plan = build_interaction_request_plan(
            100_000,
            -100_000,
            64,
            8,
            (0, -1),
            (0, -1),
            MAX_FULL_CHUNK_RESIDENT,
            synthetic_column_demand,
        );
        let new_requested: AHashSet<_> = new_plan.chunks.iter().copied().collect();
        let mut jobs: AHashMap<_, _> = old_plan
            .chunks
            .iter()
            .take(MAX_IN_FLIGHT_TERRAIN_TASKS)
            .map(|pos| (*pos, (41_u64, ())))
            .collect();
        let stale_pos = old_plan.chunks[0];

        let cancelled = retarget_epoch_jobs(&mut jobs, &new_requested, 42);

        assert_eq!(cancelled, MAX_IN_FLIGHT_TERRAIN_TASKS);
        assert!(jobs.is_empty());
        assert!(!task_result_is_current(41, 42, &new_requested, stale_pos));
        assert!(task_result_is_current(
            42,
            42,
            &new_requested,
            new_plan.chunks[0]
        ));
    }

    #[test]
    fn evicting_dense_chunk_never_deletes_persisted_edit_snapshot() {
        let mut world = VoxelWorld::new();
        let edited_pos = ChunkPos::new(0, 2, 0);
        world.insert_chunk(edited_pos, solid_chunk(edited_pos, BlockType::Stone.into()));
        assert!(world.edit_set_voxel(0, 32, 0, BlockType::Limestone.into()));
        assert!(world.edited_overrides.contains_key(&edited_pos));
        let mut streamer = ChunkStreamer::default();

        let started = std::time::Instant::now();
        rebuild_interaction_plan(
            &mut world,
            &mut streamer,
            50_000,
            50_000,
            64,
            8,
            (1, 0),
            (1, 0),
        );
        println!(
            "cold teleport interaction-plan rebuild: {:.3} ms",
            started.elapsed().as_secs_f64() * 1_000.0
        );

        assert!(!world.chunks.contains_key(&edited_pos));
        assert!(world.edited_overrides.contains_key(&edited_pos));
        assert_eq!(streamer.telemetry.evicted_this_frame, 1);
    }

    #[test]
    fn interaction_radius_is_queue_independent_and_expands_only_when_quiescent() {
        // NeuroCore may move its effective target between 8 and 11 as queues
        // fill and drain. That signal can delay expansion but cannot contract
        // the already-authoritative radius.
        assert_eq!(
            stable_interaction_radius(11, 64, 8, RuntimeProfile::Auto, 0.0, false),
            11
        );
        assert_eq!(
            stable_interaction_radius(8, 64, 11, RuntimeProfile::Auto, 0.0, false),
            8
        );
        assert_eq!(
            stable_interaction_radius(8, 64, 11, RuntimeProfile::Auto, 0.0, true),
            9
        );
        assert_eq!(
            stable_interaction_radius(9, 64, 16, RuntimeProfile::Auto, 0.0, true),
            10
        );

        // Explicit ceilings and hard frame-pressure safety remain immediate.
        assert_eq!(
            stable_interaction_radius(16, 7, 16, RuntimeProfile::Auto, 0.0, false),
            7
        );
        assert_eq!(
            stable_interaction_radius(16, 64, 16, RuntimeProfile::LowSpec, 0.0, false),
            11
        );
        assert_eq!(
            stable_interaction_radius(16, 64, 16, RuntimeProfile::Auto, 0.85, false),
            8
        );
        assert_eq!(
            stable_interaction_radius(16, 64, 16, RuntimeProfile::Auto, f32::NAN, false),
            8
        );
        assert_eq!(
            stable_interaction_radius(16, 64, 16, RuntimeProfile::Benchmark, 1.0, false),
            MAX_INTERACTION_RADIUS_CHUNKS
        );
        assert_eq!(
            stable_interaction_radius(16, 64, 16, RuntimeProfile::Benchmark, f32::NAN, false),
            8
        );

        // New-world initialization is explicit and always preserves the
        // collision/edit core even when the visual setting is smaller.
        assert_eq!(
            stable_interaction_radius(-1, 2, 2, RuntimeProfile::Auto, 0.0, false),
            GUARANTEED_INTERACTION_CORE_CHUNKS
        );
    }

    #[test]
    fn eviction_invalidates_only_retained_cardinal_mesh_snapshots() {
        let mut world = VoxelWorld::new();
        let dropped_a = ChunkPos::new(0, 0, 0);
        let dropped_b = ChunkPos::new(0, 1, 0);
        let retained_x = ChunkPos::new(1, 0, 0);
        let retained_y = ChunkPos::new(0, 2, 0);
        let diagonal = ChunkPos::new(1, 1, 1);
        for pos in [dropped_a, dropped_b, retained_x, retained_y, diagonal] {
            world.insert_chunk(pos, solid_chunk(pos, BlockType::Stone.into()));
        }
        let requested = AHashSet::from_iter([retained_x, retained_y, diagonal]);
        let affected =
            retained_mesh_neighbours_after_eviction(&world, &requested, &[dropped_a, dropped_b]);

        assert_eq!(affected, AHashSet::from_iter([retained_x, retained_y]));

        let unaffected = ChunkPos::new(4, 0, 4);
        let mut jobs = AHashMap::from_iter([
            (retained_x, (7_u64, ())),
            (retained_y, (7_u64, ())),
            (diagonal, (7_u64, ())),
            (unaffected, (7_u64, ())),
        ]);
        let cancelled = retarget_mesh_epoch_jobs(&mut jobs, &requested, &affected, 8);
        assert_eq!(cancelled, 3);
        assert_eq!(jobs.get(&diagonal).map(|job| job.0), Some(8));
    }

    #[test]
    fn eviction_neighbour_discovery_is_deduplicated_and_overflow_safe() {
        let mut world = VoxelWorld::new();
        let retained = ChunkPos::new(i32::MAX - 1, 0, 0);
        let dropped = ChunkPos::new(i32::MAX, 0, 0);
        world.insert_chunk(retained, solid_chunk(retained, BlockType::Stone.into()));
        world.insert_chunk(dropped, solid_chunk(dropped, BlockType::Stone.into()));
        let requested = AHashSet::from_iter([retained]);
        let affected =
            retained_mesh_neighbours_after_eviction(&world, &requested, &[dropped, dropped]);
        assert_eq!(affected, AHashSet::from_iter([retained]));
        assert!(checked_chunk_offset(dropped, 1, 0, 0).is_none());
        assert!(checked_chunk_offset(ChunkPos::new(0, i32::MIN, 0), 0, -1, 0).is_none());
    }

    #[test]
    fn mesh_bucket_entity_telemetry_tracks_current_and_lifetime_peak() {
        let mut streamer = ChunkStreamer::default();
        let pos = ChunkPos::new(0, 0, 0);
        streamer.entities.insert(
            pos,
            vec![
                ChunkMeshEntity {
                    entity: Entity::from_raw(1),
                    handle: Handle::default(),
                    bucket: MeshBucketKey {
                        render_class: MeshRenderClass::Standard,
                        material: DEFAULT_MATERIAL,
                    },
                },
                ChunkMeshEntity {
                    entity: Entity::from_raw(2),
                    handle: Handle::default(),
                    bucket: MeshBucketKey {
                        render_class: MeshRenderClass::Standard,
                        material: DEFAULT_MATERIAL,
                    },
                },
            ],
        );
        refresh_mesh_entity_telemetry(&mut streamer);
        assert_eq!(streamer.telemetry.mesh_bucket_entities, 2);
        assert_eq!(streamer.telemetry.peak_mesh_bucket_entities, 2);

        streamer.entities.remove(&pos);
        refresh_mesh_entity_telemetry(&mut streamer);
        assert_eq!(streamer.telemetry.mesh_bucket_entities, 0);
        assert_eq!(streamer.telemetry.peak_mesh_bucket_entities, 2);
    }

    #[test]
    fn known_air_slots_do_not_need_placeholder_chunks() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(4, 5, -3);
        world.column_top_cy.insert((4, -3), 2);

        assert!(chunk_slot_known_air(&mut world, pos, 8));
        assert!(chunk_slot_loaded_or_known_air(&mut world, pos, 8));
        assert!(!world.chunks.contains_key(&pos));
    }

    #[test]
    fn chunk_slots_below_column_ceiling_still_wait_for_real_chunks() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(2, 2, 2);
        world.column_top_cy.insert((2, 2), 2);

        assert!(!chunk_slot_known_air(&mut world, pos, 8));
        assert!(!chunk_slot_loaded_or_known_air(&mut world, pos, 8));
    }

    #[test]
    fn uniform_air_chunk_fast_skip_accepts_implicit_air_neighbours() {
        let mut world = VoxelWorld::new();
        cache_air_columns(&mut world, 0);
        let center = ChunkPos::new(0, 2, 0);
        world.insert_chunk(center, Chunk::new(center));

        assert!(uniform_chunk_is_trivially_invisible(&mut world, center, 4));
        assert_eq!(world.chunks.len(), 1);
    }

    #[test]
    fn uniform_solid_chunk_does_not_treat_implicit_air_as_hidden() {
        let mut world = VoxelWorld::new();
        cache_air_columns(&mut world, 0);
        let center = ChunkPos::new(0, 2, 0);
        world.insert_chunk(center, solid_chunk(center, BlockType::Stone.into()));

        assert!(!uniform_chunk_is_trivially_invisible(&mut world, center, 4));
    }

    #[test]
    fn dirty_mesh_candidate_scan_budget_scans_small_queues_fully() {
        assert_eq!(dirty_mesh_candidate_scan_budget(24, 4, 80, 0.0), 24);
    }

    #[test]
    fn dirty_mesh_candidate_scan_budget_caps_large_backlogs() {
        let budget = dirty_mesh_candidate_scan_budget(10_000, 4, 80, 1.0);

        assert!(budget < 10_000);
        assert_eq!(budget, 80);
    }

    #[test]
    fn dirty_mesh_candidate_scan_budget_stays_tiny_under_startup_pressure() {
        let budget = dirty_mesh_candidate_scan_budget(25_000, 4, 80, 1.0);

        assert!(
            budget <= 80,
            "startup pressure should not spend the frame scanning {budget} dirty chunks"
        );
    }

    #[test]
    fn dirty_mesh_candidate_scan_budget_expands_when_stable() {
        let pressured = dirty_mesh_candidate_scan_budget(10_000, 4, 80, 1.0);
        let stable = dirty_mesh_candidate_scan_budget(10_000, 4, 80, 0.0);

        assert!(stable > pressured);
        assert_eq!(stable, 384);
    }

    #[test]
    fn urgent_mesh_candidates_cannot_hide_behind_unordered_global_backlog() {
        let near_surface = ChunkPos::new(4, 3, -2);
        let near_subsurface = ChunkPos::new(-5, 1, 1);
        let outside_radius = ChunkPos::new(9, 3, 0);
        let outside_vertical_range = ChunkPos::new(0, 8, 0);
        let mut dirty = AHashSet::from([
            near_surface,
            near_subsurface,
            outside_radius,
            outside_vertical_range,
        ]);

        let urgent = take_urgent_mesh_candidates(&mut dirty, 0, 0, 8, 8);

        assert!(urgent.contains(&near_surface));
        assert!(urgent.contains(&near_subsurface));
        assert!(!urgent.contains(&outside_radius));
        assert!(!urgent.contains(&outside_vertical_range));
        assert!(dirty.contains(&outside_radius));
        assert!(dirty.contains(&outside_vertical_range));
    }

    #[test]
    fn visual_artifact_repair_removes_showcase_override_and_regenerates_loaded_chunk() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(0, 3, 0);
        let alien_moss: Voxel = BlockType::AlienMoss.into();
        world
            .edited_overrides
            .insert(pos, override_chunk(alien_moss));
        world.insert_chunk(pos, solid_chunk(pos, alien_moss));

        let report = world.repair_visual_artifact_overrides();

        assert_eq!(report.scanned_chunks, 1);
        assert_eq!(report.removed_chunks, 1);
        assert_eq!(report.refreshed_loaded_chunks, 1);
        assert_eq!(world.last_repair_report, Some(report));
        assert!(!world.edited_overrides.contains_key(&pos));
        assert!(world.edit_save_dirty);
        assert!(world.edit_dirty_chunks.contains(&pos));
        assert_ne!(world.chunks.get(&pos).unwrap().get(0, 0, 0), alien_moss);
    }

    #[test]
    fn visual_artifact_repair_keeps_normal_build_overrides() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(1, 3, 1);
        world
            .edited_overrides
            .insert(pos, override_chunk(BlockType::Limestone.into()));

        let report = world.repair_visual_artifact_overrides();

        assert_eq!(report.scanned_chunks, 1);
        assert_eq!(report.removed_chunks, 0);
        assert_eq!(report.kept_chunks, 1);
        assert_eq!(world.last_repair_report, Some(report));
        assert!(world.edited_overrides.contains_key(&pos));
        assert!(!world.edit_save_dirty);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn manifestless_v1_edit_store_remains_compatible() {
        let root = EditStoreTestRoot::new("legacy-v1");
        let identity = edit_store_identity(TerrainGrammarVersion::V1, 11);
        let pos = ChunkPos::new(-2, 3, 5);
        let expected = override_chunk(BlockType::Stone.into());
        let manifest = saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "legacy",
            identity,
            AHashMap::from([(pos, expected.clone())]),
        ));

        let chunks = edited_chunk_dir_at(root.path(), "legacy", TerrainGrammarVersion::V1);
        let legacy_record =
            fs::read_to_string(chunks.join(edited_chunk_file_name(pos))).expect("read V1 record");
        assert_eq!(manifest.edited_chunks, 1);
        assert!(!chunks.join("manifest.ron").exists());
        assert!(!edited_v2_root_at(root.path(), "legacy").exists());
        assert!(!edited_v3_root_at(root.path(), "legacy").exists());
        assert!(!legacy_record.contains("schema:"));
        assert!(!legacy_record.contains("generation_identity:"));
        let loaded =
            compatible_overrides(load_edited_overrides_at(root.path(), "legacy", identity));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[&pos].voxels, expected.voxels);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn v2_manifest_and_every_record_bind_exact_generation_identity() {
        let root = EditStoreTestRoot::new("v2-provenance");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 22);
        let pos = ChunkPos::new(1, -4, 9);
        assert!(matches!(
            load_edited_overrides_at(root.path(), "v2", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "v2",
            identity,
            AHashMap::from([(pos, override_chunk(BlockType::Limestone.into()))]),
        ));

        let v2_root = edited_v2_root_at(root.path(), "v2");
        let manifest_text =
            fs::read_to_string(v2_root.join("manifest.ron")).expect("read manifest");
        let record_text =
            fs::read_to_string(v2_root.join("chunks").join(edited_chunk_file_name(pos)))
                .expect("read record");
        let manifest: EditedChunkStoreManifestVersioned =
            ron::from_str(&manifest_text).expect("parse manifest");
        let record: EditedChunkFile = ron::from_str(&record_text).expect("parse record");

        assert_eq!(manifest.schema, EDITED_OVERRIDE_STORE_SCHEMA_V2);
        assert_eq!(manifest.generation_identity, identity);
        assert_eq!(record.schema, Some(EDITED_OVERRIDE_STORE_SCHEMA_V2));
        assert_eq!(record.generation_identity, Some(identity));
        assert!(manifest_text.contains("schema: 2"));
        assert!(manifest_text.contains("terrain_grammar: V2"));
        assert!(record_text.contains("schema: Some(2)"));
        assert!(!manifest_text.contains("terrain_grammar: V3"));
        assert!(!edited_v3_root_at(root.path(), "v2").exists());
        assert_eq!(
            compatible_overrides(load_edited_overrides_at(root.path(), "v2", identity)).len(),
            1
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn v3_fresh_store_uses_an_isolated_schema_three_identity_authority() {
        let root = EditStoreTestRoot::new("v3-provenance");
        let identity = edit_store_identity(TerrainGrammarVersion::V3, 23);
        let pos = ChunkPos::new(-11, 4, 17);
        assert!(matches!(
            load_edited_overrides_at(root.path(), "v3", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "v3",
            identity,
            AHashMap::from([(pos, override_chunk(BlockType::Limestone.into()))]),
        ));

        let v3_root = edited_v3_root_at(root.path(), "v3");
        let manifest_text =
            fs::read_to_string(v3_root.join("manifest.ron")).expect("read V3 manifest");
        let manifest: EditedChunkStoreManifestVersioned =
            ron::from_str(&manifest_text).expect("parse V3 manifest");
        let record: EditedChunkFile = ron::from_str(
            &fs::read_to_string(v3_root.join("chunks").join(edited_chunk_file_name(pos)))
                .expect("read V3 record"),
        )
        .expect("parse V3 record");

        assert_eq!(manifest.schema, EDITED_OVERRIDE_STORE_SCHEMA_V3);
        assert_eq!(manifest.generation_identity, identity);
        assert_eq!(record.schema, Some(EDITED_OVERRIDE_STORE_SCHEMA_V3));
        assert_eq!(record.generation_identity, Some(identity));
        assert!(manifest_text.contains("terrain_grammar: V3"));
        assert!(!edited_v2_root_at(root.path(), "v3").exists());
        assert_eq!(
            compatible_overrides(load_edited_overrides_at(root.path(), "v3", identity)).len(),
            1
        );

        let v2_identity = edit_store_identity(TerrainGrammarVersion::V2, identity.seed);
        assert!(matches!(
            load_edited_overrides_at(root.path(), "v3", v2_identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn v3_manifest_schema_identity_and_case_aliases_fail_closed() {
        let identity = edit_store_identity(TerrainGrammarVersion::V3, 0x3300_0003);

        let unknown = EditStoreTestRoot::new("v3-unknown-schema");
        saved_manifest(save_edited_overrides_snapshot_at(
            unknown.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        let manifest_path = edited_v3_root_at(unknown.path(), "world").join("manifest.ron");
        let text = fs::read_to_string(&manifest_path)
            .expect("read V3 manifest")
            .replacen("schema: 3", "schema: 999", 1);
        fs::write(&manifest_path, text).expect("write unsupported V3 schema");
        assert!(matches!(
            load_edited_overrides_at(unknown.path(), "world", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let mismatch = EditStoreTestRoot::new("v3-identity-mismatch");
        saved_manifest(save_edited_overrides_snapshot_at(
            mismatch.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        assert!(matches!(
            load_edited_overrides_at(
                mismatch.path(),
                "world",
                edit_store_identity(TerrainGrammarVersion::V3, identity.seed + 1)
            ),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let alias = EditStoreTestRoot::new("v3-case-alias");
        let edits_root = edited_override_root_at(alias.path(), "world");
        fs::create_dir_all(edits_root.join("GRAMMAR_V3")).expect("create V3 case alias");
        match load_edited_overrides_at(alias.path(), "world", identity) {
            EditedOverrideStoreLoad::Blocked { reason } => {
                assert!(reason.contains("case-only alias"));
            }
            EditedOverrideStoreLoad::Compatible { .. } => {
                panic!("case-only V3 namespace alias must block")
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn copied_foreign_v2_chunk_blocks_the_entire_store() {
        let root = EditStoreTestRoot::new("foreign-record");
        let identity_a = edit_store_identity(TerrainGrammarVersion::V2, 1001);
        let identity_b = edit_store_identity(TerrainGrammarVersion::V2, 9009);
        let pos = ChunkPos::new(0, 2, 0);
        for (name, identity, voxel) in [
            ("a", identity_a, BlockType::Stone.into()),
            ("b", identity_b, BlockType::Limestone.into()),
        ] {
            saved_manifest(save_edited_overrides_snapshot_at(
                root.path(),
                name,
                identity,
                AHashMap::from([(pos, override_chunk(voxel))]),
            ));
        }

        let a_root = edited_v2_root_at(root.path(), "a");
        let a_record = a_root.join("chunks").join(edited_chunk_file_name(pos));
        let b_record = edited_v2_root_at(root.path(), "b")
            .join("chunks")
            .join(edited_chunk_file_name(pos));
        let foreign_bytes = fs::read(&b_record).expect("read foreign record");
        fs::write(&a_record, &foreign_bytes).expect("copy foreign record");

        // Update the outer checksum so the per-record identity, rather than
        // only the manifest digest, is what rejects the copied chunk.
        let manifest_path = a_root.join("manifest.ron");
        let mut manifest: EditedChunkStoreManifestVersioned =
            ron::from_str(&fs::read_to_string(&manifest_path).expect("read manifest"))
                .expect("parse manifest");
        manifest.records[0].byte_len = foreign_bytes.len() as u64;
        manifest.records[0].content_checksum_fnv1a64 = fnv1a64(&foreign_bytes);
        fs::write(
            &manifest_path,
            ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::default())
                .expect("serialize manifest"),
        )
        .expect("write manifest");

        match load_edited_overrides_at(root.path(), "a", identity_a) {
            EditedOverrideStoreLoad::Blocked { reason } => {
                assert!(reason.contains("different generation identity"));
            }
            EditedOverrideStoreLoad::Compatible { .. } => {
                panic!("foreign edit chunk must block the entire store")
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn malformed_unknown_mismatch_duplicate_and_path_errors_all_block() {
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 44);

        let malformed = EditStoreTestRoot::new("malformed");
        let malformed_root = edited_v2_root_at(malformed.path(), "world");
        fs::create_dir_all(malformed_root.join("chunks")).expect("create malformed store");
        fs::write(malformed_root.join("manifest.ron"), "not valid ron")
            .expect("write malformed manifest");
        assert!(matches!(
            load_edited_overrides_at(malformed.path(), "world", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let unknown = EditStoreTestRoot::new("unknown-schema");
        saved_manifest(save_edited_overrides_snapshot_at(
            unknown.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        let unknown_manifest = edited_v2_root_at(unknown.path(), "world").join("manifest.ron");
        let text = fs::read_to_string(&unknown_manifest)
            .expect("read manifest")
            .replacen("schema: 2", "schema: 999", 1);
        fs::write(&unknown_manifest, text).expect("write unknown schema");
        assert!(matches!(
            load_edited_overrides_at(unknown.path(), "world", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let mismatch = EditStoreTestRoot::new("identity-mismatch");
        saved_manifest(save_edited_overrides_snapshot_at(
            mismatch.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        assert!(matches!(
            load_edited_overrides_at(
                mismatch.path(),
                "world",
                edit_store_identity(TerrainGrammarVersion::V2, 45)
            ),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let duplicate = EditStoreTestRoot::new("duplicate");
        saved_manifest(save_edited_overrides_snapshot_at(
            duplicate.path(),
            "world",
            identity,
            AHashMap::from([
                (
                    ChunkPos::new(0, 0, 0),
                    override_chunk(BlockType::Stone.into()),
                ),
                (
                    ChunkPos::new(1, 0, 0),
                    override_chunk(BlockType::Limestone.into()),
                ),
            ]),
        ));
        let duplicate_manifest = edited_v2_root_at(duplicate.path(), "world").join("manifest.ron");
        let mut manifest: EditedChunkStoreManifestVersioned =
            ron::from_str(&fs::read_to_string(&duplicate_manifest).expect("read manifest"))
                .expect("parse manifest");
        manifest.records[1].pos = manifest.records[0].pos;
        fs::write(
            &duplicate_manifest,
            ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::default())
                .expect("serialize duplicate manifest"),
        )
        .expect("write duplicate manifest");
        assert!(matches!(
            load_edited_overrides_at(duplicate.path(), "world", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));

        let bad_path = EditStoreTestRoot::new("path-mismatch");
        saved_manifest(save_edited_overrides_snapshot_at(
            bad_path.path(),
            "world",
            identity,
            AHashMap::from([(
                ChunkPos::new(0, 0, 0),
                override_chunk(BlockType::Stone.into()),
            )]),
        ));
        fs::write(
            edited_v2_root_at(bad_path.path(), "world")
                .join("chunks")
                .join("unexpected.ron"),
            "()",
        )
        .expect("write unexpected record path");
        assert!(matches!(
            load_edited_overrides_at(bad_path.path(), "world", identity),
            EditedOverrideStoreLoad::Blocked { .. }
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn blocked_save_preserves_every_existing_byte() {
        let root = EditStoreTestRoot::new("blocked-preserves");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 55);
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::from([(
                ChunkPos::new(0, 0, 0),
                override_chunk(BlockType::Stone.into()),
            )]),
        ));
        let manifest = edited_v2_root_at(root.path(), "world").join("manifest.ron");
        fs::write(&manifest, "corrupt authority").expect("corrupt manifest");
        let before = tree_bytes(root.path());

        let outcome = save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::from([(
                ChunkPos::new(9, 9, 9),
                override_chunk(BlockType::Limestone.into()),
            )]),
        );

        assert!(matches!(outcome, EditedOverrideSaveOutcome::Blocked { .. }));
        assert_eq!(tree_bytes(root.path()), before);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn blocked_edit_authority_never_runs_dependent_journal_or_metadata_writes() {
        let root = EditStoreTestRoot::new("blocked-dependent-writes");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 56);
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        let manifest = edited_v2_root_at(root.path(), "world").join("manifest.ron");
        fs::write(&manifest, "corrupt authority").expect("corrupt manifest");
        let before = tree_bytes(root.path());
        let dependent_write_called = std::cell::Cell::new(false);
        let capture = capture_edited_overrides_at(
            root.path(),
            "world",
            identity,
            1,
            EditedOverrideCapturePayload::Snapshot(AHashMap::from([(
                ChunkPos::new(1, 2, 3),
                override_chunk(BlockType::Stone.into()),
            )])),
        );

        let outcome = commit_edited_override_capture_with(capture, |_| {
            dependent_write_called.set(true);
            Ok(())
        });

        assert!(matches!(
            outcome,
            OrderedEditedOverrideSaveOutcome::AuthorityBlocked { .. }
        ));
        assert!(!dependent_write_called.get());
        assert_eq!(tree_bytes(root.path()), before);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn v1_v2_v3_coexist_and_stale_cleanup_never_crosses_namespaces() {
        let root = EditStoreTestRoot::new("coexistence");
        let v1 = edit_store_identity(TerrainGrammarVersion::V1, 66);
        let v2 = edit_store_identity(TerrainGrammarVersion::V2, 66);
        let v3 = edit_store_identity(TerrainGrammarVersion::V3, 66);
        let a = ChunkPos::new(0, 0, 0);
        let b = ChunkPos::new(1, 0, 0);
        let both = AHashMap::from([
            (a, override_chunk(BlockType::Stone.into())),
            (b, override_chunk(BlockType::Limestone.into())),
        ]);
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v1,
            both.clone(),
        ));
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v2,
            both.clone(),
        ));
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v3,
            both,
        ));

        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v1,
            AHashMap::from([(a, override_chunk(BlockType::Stone.into()))]),
        ));
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V1)
                .join(edited_chunk_file_name(a))
                .exists()
        );
        assert!(
            !edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V1)
                .join(edited_chunk_file_name(b))
                .exists()
        );
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V2)
                .join(edited_chunk_file_name(b))
                .exists()
        );
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V3)
                .join(edited_chunk_file_name(b))
                .exists()
        );

        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v2,
            AHashMap::from([(a, override_chunk(BlockType::Limestone.into()))]),
        ));
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V1)
                .join(edited_chunk_file_name(a))
                .exists()
        );
        assert!(
            !edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V2)
                .join(edited_chunk_file_name(b))
                .exists()
        );
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V3)
                .join(edited_chunk_file_name(b))
                .exists()
        );
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            v3,
            AHashMap::from([(b, override_chunk(BlockType::Stone.into()))]),
        ));
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V1)
                .join(edited_chunk_file_name(a))
                .exists()
        );
        assert!(
            edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V2)
                .join(edited_chunk_file_name(a))
                .exists()
        );
        assert!(
            !edited_chunk_dir_at(root.path(), "world", TerrainGrammarVersion::V3)
                .join(edited_chunk_file_name(a))
                .exists()
        );
        assert_eq!(
            compatible_overrides(load_edited_overrides_at(root.path(), "world", v1)).len(),
            1
        );
        assert_eq!(
            compatible_overrides(load_edited_overrides_at(root.path(), "world", v2)).len(),
            1
        );
        assert_eq!(
            compatible_overrides(load_edited_overrides_at(root.path(), "world", v3)).len(),
            1
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn directory_publication_is_complete_and_leaves_no_transaction_paths() {
        let root = EditStoreTestRoot::new("atomic-publication");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 77);
        let pos = ChunkPos::new(-7, 8, -9);
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::from([(pos, override_chunk(BlockType::Stone.into()))]),
        ));
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::from([(pos, override_chunk(BlockType::Limestone.into()))]),
        ));

        let loaded = compatible_overrides(load_edited_overrides_at(root.path(), "world", identity));
        assert_eq!(loaded[&pos].voxels[0], Voxel::from(BlockType::Limestone));
        let edit_root = edited_override_root_at(root.path(), "world");
        let names = fs::read_dir(edit_root)
            .expect("read edit root")
            .map(|entry| {
                entry
                    .expect("read edit-root entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["grammar_v2"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn delayed_older_capture_cannot_replace_newer_snapshot_or_clear_dirty_truth() {
        let root = EditStoreTestRoot::new("ordered-capture");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 0xA0B0_C0D0);
        let a = ChunkPos::new(-3, 2, 5);
        let b = ChunkPos::new(8, -1, -13);
        let mut world = VoxelWorld::new();

        world
            .edited_overrides
            .insert(a, override_chunk(BlockType::Stone.into()));
        world.mark_edit_snapshot_dirty();
        let delayed_a = capture_edited_overrides_at(
            root.path(),
            "ordered",
            identity,
            world.edit_save_revision,
            EditedOverrideCapturePayload::Snapshot(world.edited_overrides.clone()),
        );

        world
            .edited_overrides
            .insert(b, override_chunk(BlockType::Limestone.into()));
        world.mark_edit_snapshot_dirty();
        let capture_a_b = capture_edited_overrides_at(
            root.path(),
            "ordered",
            identity,
            world.edit_save_revision,
            EditedOverrideCapturePayload::Snapshot(world.edited_overrides.clone()),
        );
        let newest_receipt = match commit_edited_override_capture_with(capture_a_b, |_| Ok(())) {
            OrderedEditedOverrideSaveOutcome::Committed(receipt) => receipt,
            other => panic!("newest A+B capture must commit, got {other:?}"),
        };
        assert!(world.edit_save_dirty);
        assert!(world.confirm_edited_override_save(&newest_receipt));
        assert!(!world.edit_save_dirty);

        let old_dependent_called = std::cell::Cell::new(false);
        match commit_edited_override_capture_with(delayed_a, |_| {
            old_dependent_called.set(true);
            Ok(())
        }) {
            OrderedEditedOverrideSaveOutcome::Superseded {
                capture_token,
                latest_capture_token,
            } => assert!(capture_token < latest_capture_token),
            other => panic!("delayed A capture must be superseded, got {other:?}"),
        }
        assert!(!old_dependent_called.get());
        assert!(!world.edit_save_dirty);

        let loaded =
            compatible_overrides(load_edited_overrides_at(root.path(), "ordered", identity));
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[&a].voxels[0], Voxel::from(BlockType::Stone));
        assert_eq!(loaded[&b].voxels[0], Voxel::from(BlockType::Limestone));
        let transaction_names = fs::read_dir(edited_override_root_at(root.path(), "ordered"))
            .expect("read ordered edit root")
            .map(|entry| {
                entry
                    .expect("read ordered edit entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(transaction_names, vec!["grammar_v2"]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn retired_transaction_snapshot_must_be_a_directory() {
        let root = EditStoreTestRoot::new("retired-file");
        let edits_root = edited_override_root_at(root.path(), "world");
        fs::create_dir_all(edits_root.join("grammar_v2")).expect("create final authority");
        fs::write(
            edits_root.join(".grammar_v2.previous-1"),
            b"not a directory",
        )
        .expect("create retired-file impostor");

        let error = reject_transaction_debris(&edits_root, TerrainGrammarVersion::V2)
            .expect_err("a retired snapshot file must fail closed");
        assert!(error.contains("is not a directory"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn one_retired_snapshot_is_removed_before_a_new_publication() {
        let root = EditStoreTestRoot::new("retired-cleanup");
        let identity = edit_store_identity(TerrainGrammarVersion::V2, 405);
        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::new(),
        ));
        let edits_root = edited_override_root_at(root.path(), "world");
        let retired = edits_root.join(".grammar_v2.previous-old");
        fs::create_dir_all(&retired).expect("create bounded retired snapshot");

        saved_manifest(save_edited_overrides_snapshot_at(
            root.path(),
            "world",
            identity,
            AHashMap::new(),
        ));

        assert!(!retired.exists());
        reject_transaction_debris(&edits_root, TerrainGrammarVersion::V2)
            .expect("newly published authority must be immediately loadable");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dangling_edit_store_link_is_not_treated_as_a_missing_directory() {
        let root = EditStoreTestRoot::new("dangling-link");
        let edits_root = edited_override_root_at(root.path(), "world");
        fs::create_dir_all(&edits_root).expect("create edit root");
        let link = edits_root.join("chunks");
        let absent_target = root.path().join("absent-target");

        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            if symlink_dir(&absent_target, &link).is_err() {
                // Windows may withhold symlink creation without Developer
                // Mode. Production still checks both symlink and reparse
                // metadata before considering NotFound.
                return;
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&absent_target, &link).expect("create dangling test symlink");
        }

        let identity = edit_store_identity(TerrainGrammarVersion::V1, 404);
        match load_edited_overrides_at(root.path(), "world", identity) {
            EditedOverrideStoreLoad::Blocked { reason } => {
                assert!(reason.contains("symlink or reparse point"));
            }
            EditedOverrideStoreLoad::Compatible { .. } => {
                panic!("dangling V1 chunks link must not become an empty compatible store")
            }
        }
    }

    #[test]
    fn edit_store_status_exposes_bounded_qa_truth() {
        let identity = WorldGenerationIdentity {
            seed: 88,
            world_profile: crate::settings::WorldProfile::Natural,
            scenery_quality: crate::settings::SceneryQuality::Lush,
            terrain_grammar: TerrainGrammarVersion::V2,
        };
        let compatible = WorldEditStoreStatus::Compatible {
            generation_identity: identity,
            edited_chunks: 3,
        };
        assert_eq!(compatible.label(), "compatible");
        assert_eq!(compatible.edited_chunks(), Some(3));
        assert_eq!(compatible.generation_identity(), Some(identity));
        assert_eq!(compatible.reason_code(), None);
        assert!(compatible.is_compatible_with(identity));

        let blocked = WorldEditStoreStatus::Blocked {
            generation_identity: identity,
            reason_code: "authority_validation_failed",
            detail: "host-specific detail".to_owned(),
        };
        assert_eq!(blocked.label(), "blocked");
        assert_eq!(blocked.edited_chunks(), None);
        assert_eq!(blocked.reason_code(), Some("authority_validation_failed"));
        assert!(!blocked.is_compatible_with(identity));
    }
}
