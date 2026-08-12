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
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use crate::blocks::{
    effective_material_for_voxel, normalize_material_for_voxel, voxel_is_solid, BlockType,
    MaterialId, Voxel, AIR, DEFAULT_MATERIAL,
};
use crate::chunk::{
    world_to_chunk, Chunk, ChunkPos, SharedMaterials, SharedVoxels, CHUNK_SIZE_I, CHUNK_VOLUME,
};
use crate::horizon::SharedHorizonCache;
use crate::mesher::build_mesh_buckets_budgeted_with_horizon;
use crate::neurocore::{QualityState, RuntimeBudget, RuntimeIntent, RuntimeProfile};
use crate::settings::WorldSettings;
use crate::terrain::TerrainGenerator;
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
    settings: Res<WorldSettings>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    pending: Res<crate::menu::PendingWorldLoad>,
    mut commands: Commands,
) {
    if !pending.0 {
        return;
    }
    world.generator = TerrainGenerator::new(settings.seed)
        .with_world_profile(settings.effective_world_profile())
        .with_scenery_quality(settings.scenery_quality);
    world.clear_chunks();
    world.edited_overrides.clear();
    world.column_top_cy.clear();
    world.edit_dirty_chunks.clear();
    world.edit_save_dirty = false;
    world.last_repair_report = None;
    if let Some(active) = active.as_deref() {
        let (overrides, manifest) = load_edited_overrides_for_world(&active.meta.name);
        if !overrides.is_empty() {
            info!(
                "world edits: loaded {} edited chunks for '{}'",
                manifest.edited_chunks, active.meta.name
            );
        }
        world.edited_overrides = overrides;
    }
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
    /// True once direct edits changed `edited_overrides` since the last
    /// save request. Autosave uses this to avoid serialising every edit
    /// chunk every 30 seconds when nothing changed.
    pub edit_save_dirty: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg(not(target_arch = "wasm32"))]
struct EditedChunkFile {
    pos: ChunkPos,
    data: EditedChunkOverride,
}

pub fn save_edited_overrides_for_world(
    world_name: &str,
    world: &VoxelWorld,
) -> crate::settings::WorldEditManifest {
    save_edited_overrides_snapshot(world_name, world.edited_overrides.clone())
}

pub fn save_edited_overrides_snapshot(
    world_name: &str,
    overrides: AHashMap<ChunkPos, EditedChunkOverride>,
) -> crate::settings::WorldEditManifest {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = world_name;
        return crate::settings::WorldEditManifest {
            edited_chunks: overrides.len(),
            last_saved_epoch: now_epoch(),
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = edited_chunk_dir(world_name);
        if let Err(e) = fs::create_dir_all(&dir) {
            warn!("world edits: could not create {}: {e}", dir.display());
            return crate::settings::WorldEditManifest {
                edited_chunks: overrides.len(),
                last_saved_epoch: now_epoch(),
            };
        }

        let mut expected = AHashSet::new();
        for (pos, data) in overrides {
            let file = edited_chunk_file(&dir, pos);
            expected.insert(file.clone());
            let record = EditedChunkFile { pos, data };
            match ron::ser::to_string_pretty(&record, ron::ser::PrettyConfig::default()) {
                Ok(text) => {
                    if let Err(e) = crate::settings::atomic_write_text(&file, &text) {
                        warn!("world edits: failed writing {}: {e}", file.display());
                    }
                }
                Err(e) => warn!("world edits: failed serialising {:?}: {e}", pos),
            }
        }

        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("ron")
                    && !expected.contains(&path)
                {
                    let _ = fs::remove_file(path);
                }
            }
        }

        crate::settings::WorldEditManifest {
            edited_chunks: expected.len(),
            last_saved_epoch: now_epoch(),
        }
    }
}

pub fn load_edited_overrides_for_world(
    world_name: &str,
) -> (
    AHashMap<ChunkPos, EditedChunkOverride>,
    crate::settings::WorldEditManifest,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = world_name;
        return (
            AHashMap::new(),
            crate::settings::WorldEditManifest::default(),
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = edited_chunk_dir(world_name);
        let mut out = AHashMap::new();
        let Ok(read) = fs::read_dir(&dir) else {
            return (out, crate::settings::WorldEditManifest::default());
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            match ron::from_str::<EditedChunkFile>(&text) {
                Ok(record) => {
                    if record.data.voxels.len() == CHUNK_VOLUME {
                        out.insert(record.pos, record.data);
                    }
                }
                Err(e) => warn!("world edits: failed parsing {}: {e}", path.display()),
            }
        }
        let manifest = crate::settings::WorldEditManifest {
            edited_chunks: out.len(),
            last_saved_epoch: now_epoch(),
        };
        (out, manifest)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_chunk_dir(world_name: &str) -> PathBuf {
    PathBuf::from(crate::settings::SAVES_DIR)
        .join(format!(
            "{}_edits",
            crate::settings::world_storage_stem(world_name)
        ))
        .join("chunks")
}

#[cfg(not(target_arch = "wasm32"))]
fn edited_chunk_file(dir: &std::path::Path, pos: ChunkPos) -> PathBuf {
    dir.join(format!("{}_{}_{}.ron", pos.x, pos.y, pos.z))
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
            edit_save_dirty: false,
            last_repair_report: None,
        }
    }

    pub fn clear_chunks(&mut self) {
        self.chunks.clear();
        self.loaded_column_counts.clear();
        self.horizon_cache.clear();
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
            self.edit_save_dirty = true;
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
        let chunk = self
            .chunks
            .entry(cp)
            .or_insert_with(|| crate::chunk::Chunk::new(cp));
        let prev = chunk.get(lx, ly, lz);
        if prev == v {
            return None;
        }
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
        let chunk = self
            .chunks
            .entry(cp)
            .or_insert_with(|| crate::chunk::Chunk::new(cp));
        let prev = (chunk.get(lx, ly, lz), chunk.get_material(lx, ly, lz));
        let next = (v, material);
        if prev == next {
            return None;
        }
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
        let chunk = self.chunks.get_mut(&cp)?;
        let voxel = chunk.get(lx, ly, lz);
        if voxel == AIR {
            return None;
        }
        let material = normalize_material_for_voxel(voxel, material);
        let prev = chunk.get_material(lx, ly, lz);
        if prev == material {
            return None;
        }
        chunk.set_material(lx, ly, lz, material);
        batch.mark(cp, lx, ly, lz);
        Some((prev, material))
    }

    /// Finalise a direct-edit batch and publish all touched chunks to the
    /// mesher queue. Safe to call with an empty batch.
    pub fn finish_edit_batch(&mut self, batch: WorldEditBatch) {
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
        for cp in &batch.modified_chunks {
            if let Some(c) = self.chunks.get_mut(cp) {
                c.finalize_uniform_flags();
                c.dirty = true;
                self.edited_overrides
                    .insert(*cp, EditedChunkOverride::from_chunk(c));
                self.edit_save_dirty = true;
            }
        }

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
}

impl WorldEditBatch {
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
    pub pending_meshes: AHashMap<ChunkPos, (u64, Task<(ChunkPos, Vec<(MaterialId, Mesh)>)>)>,
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
    pub material: MaterialId,
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
    world.generator = TerrainGenerator::new(settings.seed)
        .with_world_profile(settings.effective_world_profile())
        .with_scenery_quality(settings.scenery_quality);
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
            // Terrain generation and meshing are deterministic for a chunk.
            // Retagging still-requested jobs avoids throwing away useful work;
            // jobs outside the new epoch's exact plan are dropped/cancelled.
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
fn terrain_task_limit(runtime_limit: usize, resident: usize, requested: usize) -> usize {
    runtime_limit
        .min(MAX_IN_FLIGHT_TERRAIN_TASKS)
        .min(requested.saturating_sub(resident))
}

#[inline]
fn mesh_task_limit(runtime_limit: usize) -> usize {
    runtime_limit.min(MAX_IN_FLIGHT_MESH_TASKS)
}

/// Automatic pressure ladder for the dense interaction radius. This is not a
/// user tuning requirement: the horizon representation keeps its extent,
/// while expensive editable/collidable chunks contract first and recover
/// deterministically when NeuroCore reports headroom.
fn adaptive_interaction_radius(
    visual_render_distance: i32,
    quality: QualityState,
    pressure: f32,
) -> i32 {
    let pressure = if pressure.is_finite() {
        pressure.clamp(0.0, 1.25)
    } else {
        1.25
    };
    let automatic_cap = match quality {
        QualityState::Critical => 8,
        QualityState::Throttled => 11,
        QualityState::Nominal if pressure >= 0.9 => 9,
        QualityState::Nominal if pressure >= 0.65 => 12,
        QualityState::Nominal => MAX_INTERACTION_RADIUS_CHUNKS,
        QualityState::Expanding | QualityState::Benchmark => MAX_INTERACTION_RADIUS_CHUNKS,
    };
    visual_render_distance
        .max(GUARANTEED_INTERACTION_CORE_CHUNKS)
        .min(automatic_cap)
        .min(MAX_INTERACTION_RADIUS_CHUNKS)
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
        retarget_epoch_jobs(
            &mut streamer.pending_meshes,
            requested,
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
    let to_drop: Vec<ChunkPos> = world
        .chunks
        .keys()
        .filter(|pos| !streamer.requested_chunks.contains(pos))
        .copied()
        .collect();
    for pos in &to_drop {
        world.remove_chunk(pos);
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
    let interaction_radius = adaptive_interaction_radius(
        settings.render_distance as i32,
        budget.quality,
        budget.queue_pressure.max(budget.frame_pressure),
    );
    let nearby_edit_outside_plan = world.edit_dirty_chunks.iter().any(|pos| {
        let dx = i64::from(pos.x) - i64::from(pcx);
        let dz = i64::from(pos.z) - i64::from(pcz);
        dx * dx + dz * dz <= i64::from(interaction_radius) * i64::from(interaction_radius)
            && pos.y >= 0
            && pos.y < vertical
            && !streamer.requested_chunks.contains(pos)
    });
    let plan_changed = streamer.requested_chunks.is_empty()
        || streamer.load_offsets_rd != interaction_radius
        || moved
        || vertical_changed
        || nearby_edit_outside_plan;
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
        let gen_seed = world.generator.seed;
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
                let gen = TerrainGenerator::new(gen_seed)
                    .with_world_profile(settings.effective_world_profile())
                    .with_scenery_quality(settings.scenery_quality);
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

    debug_assert!(streamer.requested_chunks.len() <= MAX_FULL_CHUNK_RESIDENT);
    debug_assert!(world.chunks.len() <= MAX_FULL_CHUNK_RESIDENT);
    debug_assert!(
        world
            .chunks
            .len()
            .saturating_add(streamer.pending_terrain.len())
            <= MAX_FULL_CHUNK_RESIDENT
    );
    publish_streaming_telemetry(&mut streamer, world.chunks.len(), &mut governor);
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
    let mut finished: Vec<(u64, ChunkPos, Vec<(MaterialId, Mesh)>)> = Vec::new();

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

        for (material_id, mesh) in buckets {
            let vegetation_material = material_library.vegetation_handle_for(material_id);
            let Some(material_handle) = material_library
                .handle_for(material_id)
                .or_else(|| streamer.material.clone())
            else {
                continue;
            };
            let culling_margin = if vegetation_material.is_some() {
                crate::vegetation::MAX_VEGETATION_DISPLACEMENT_VOXELS
            } else {
                0.0
            };
            let aabb = bevy::render::primitives::Aabb::from_min_max(
                Vec3::splat(-culling_margin),
                Vec3::splat(CHUNK_SIZE_I as f32 + culling_margin),
            );

            if let Some(idx) = previous
                .iter()
                .position(|entry| entry.material == material_id)
            {
                let mut entry = previous.swap_remove(idx);
                if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                    if let Some(vegetation_material) = vegetation_material.clone() {
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
            let entity = if let Some(vegetation_material) = vegetation_material {
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
                material: material_id,
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
        let all_neighbours_ready = neighbours_needed
            .into_iter()
            .all(|n| chunk_slot_loaded_or_known_air(&mut world, n, vertical_chunks));
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
    budget: &RuntimeBudget,
    pcx: i32,
    pcz: i32,
    pos: ChunkPos,
    buckets: Vec<(MaterialId, Mesh)>,
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

    for (material_id, mesh) in buckets {
        let vegetation_material = material_library.vegetation_handle_for(material_id);
        let Some(material_handle) = material_library
            .handle_for(material_id)
            .or_else(|| streamer.material.clone())
        else {
            continue;
        };
        let culling_margin = if vegetation_material.is_some() {
            crate::vegetation::MAX_VEGETATION_DISPLACEMENT_VOXELS
        } else {
            0.0
        };
        let aabb = bevy::render::primitives::Aabb::from_min_max(
            Vec3::splat(-culling_margin),
            Vec3::splat(CHUNK_SIZE_I as f32 + culling_margin),
        );

        if let Some(idx) = previous
            .iter()
            .position(|entry| entry.material == material_id)
        {
            let mut entry = previous.swap_remove(idx);
            if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                if let Some(vegetation_material) = vegetation_material.clone() {
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
        let entity = if let Some(vegetation_material) = vegetation_material {
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
            material: material_id,
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
    fn automatic_pressure_ladder_preserves_horizon_setting_but_contracts_dense_near_field() {
        assert_eq!(
            adaptive_interaction_radius(64, QualityState::Nominal, 0.1),
            MAX_INTERACTION_RADIUS_CHUNKS
        );
        assert_eq!(
            adaptive_interaction_radius(64, QualityState::Throttled, 0.2),
            11
        );
        assert_eq!(
            adaptive_interaction_radius(64, QualityState::Critical, 0.0),
            8
        );
        assert_eq!(
            adaptive_interaction_radius(64, QualityState::Nominal, f32::NAN),
            9
        );
        assert_eq!(
            adaptive_interaction_radius(2, QualityState::Nominal, 0.0),
            GUARANTEED_INTERACTION_CORE_CHUNKS
        );
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
                    material: DEFAULT_MATERIAL,
                },
                ChunkMeshEntity {
                    entity: Entity::from_raw(2),
                    handle: Handle::default(),
                    material: DEFAULT_MATERIAL,
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
}
