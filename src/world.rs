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
use crate::mesher::build_mesh_buckets_ex;
use crate::neurocore::{QualityState, RuntimeBudget, RuntimeIntent, RuntimeProfile};
use crate::settings::WorldSettings;
use crate::terrain::TerrainGenerator;

pub struct WorldPlugin;

#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone)]
pub enum WorldSet {
    NeuroCore,
    Stream,
    Mesh,
}

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
    mut images: ResMut<Assets<Image>>,
    mut world: ResMut<VoxelWorld>,
) {
    if !library.reload_requested {
        return;
    }
    library.rebuild(
        &mut materials,
        &mut images,
        crate::textures::BUILTIN_SWATCH_SIZE,
    );
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
    world.generator = TerrainGenerator::new(settings.seed);
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
    streamer.needs_orphan_scan = true;
    streamer.stream_elapsed = 0.0;
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

    // Crystal, moss, glow-sand and lava are ordinary frontier materials
    // now, and the palette is a headline building feature, so a chunk
    // full of them is a player's neon build - not an artifact. The only
    // surviving signature is the old bug that flooded a chunk with solid
    // ice or snow in a biome that has no business being frozen.
    let mut non_air = 0usize;
    let mut cold = 0usize;
    for &voxel in &data.voxels {
        if voxel == AIR {
            continue;
        }
        non_air += 1;
        if matches!(
            BlockType::from_voxel(voxel),
            BlockType::Snow | BlockType::Ice
        ) {
            cold += 1;
        }
    }

    if non_air < 64 {
        return false;
    }

    let cold_ratio = cold as f32 / non_air as f32;
    cold_ratio >= 0.72
        && !matches!(
            biome,
            crate::terrain::Biome::SnowyMountains
                | crate::terrain::Biome::Tundra
                | crate::terrain::Biome::Ocean
                | crate::terrain::Biome::GlacierShards
        )
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
    pub water_material: Option<Handle<StandardMaterial>>,
    /// In-flight terrain-generation tasks (one per chunk position).
    pub pending_terrain: AHashMap<ChunkPos, Task<(ChunkPos, SharedVoxels)>>,
    /// In-flight meshing tasks (one per chunk position). `None` mesh =
    /// chunk is empty / uniform-solid and needs no geometry.
    pub pending_meshes: AHashMap<ChunkPos, Task<(ChunkPos, Vec<(MaterialId, Mesh)>, bool)>>,
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
    /// Seconds since this world started streaming. First few seconds
    /// fill a small disc as fast as possible so the player sees ground
    /// instead of an empty sky.
    pub stream_elapsed: f32,
    /// Last anchor chunk position we scanned from. When this changes, a
    /// new frontier sweep is required.
    pub last_anchor_cxz: Option<(i32, i32)>,
}

#[derive(Clone)]
pub struct ChunkMeshEntity {
    pub entity: Entity,
    pub handle: Handle<Mesh>,
    pub material: MaterialId,
    pub far_lod: bool,
}

fn init_world(
    mut streamer: ResMut<ChunkStreamer>,
    mut world: ResMut<VoxelWorld>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut material_library: ResMut<crate::textures::MaterialLibrary>,
    settings: Res<WorldSettings>,
) {
    world.generator = TerrainGenerator::new(settings.seed);
    let swatch_size = match settings.graphics {
        crate::settings::GraphicsMode::Fast => 64,
        // High used to bake 256² swatches on the first Startup frame,
        // which delayed the window on iGPUs. 128 already survives mips;
        // Cinematic stills stay readable and Fast actually appears.
        crate::settings::GraphicsMode::Balanced | crate::settings::GraphicsMode::High => 128,
    };
    material_library.rebuild(&mut materials, &mut images, swatch_size);

    // Bake the procedural surface-grain texture once. 128×128 is the
    // sweet spot: still crisp at arm's length under `Repeat` sampling,
    // but generates in <50 ms on an iGPU (vs ~250 ms at 256×256 with
    // the 6-octave + warp + Worley + strata + sparkle pipeline). Users
    // who drop a real 512²/1024² PNG in ./textures/universal_grain.png
    // get photorealistic detail for free via the override path.
    let grain_size = swatch_size;
    let grain = images.add(crate::textures::universal_grain_or_override(grain_size));

    streamer.material = Some(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(grain.clone()),
        perceptual_roughness: 1.0,
        reflectance: 0.05,
        ..default()
    }));

    streamer.water_material = Some(materials.add(StandardMaterial {
        base_color: Color::srgba(0.2, 0.55, 0.85, 0.6),
        perceptual_roughness: 0.1,
        reflectance: 0.3,
        alpha_mode: AlphaMode::AlphaToCoverage,
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

#[inline]
fn biome_stream_bonus(generator: &TerrainGenerator, cx: i32, cz: i32) -> i32 {
    let wx = cx * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    let wz = cz * CHUNK_SIZE_I + CHUNK_SIZE_I / 2;
    crate::daynight::BiomeArtProfile::for_biome(generator.biome_at(wx, wz)).streaming_bonus
}

/// Fast keeps a 6-chunk near field at full materials. Beyond that, opaque
/// terrain collapses to one draw call. Balanced only collapses the outer
/// third. Cinematic/High keep the rich far geometry.
fn chunk_wants_far_lod(graphics: crate::settings::GraphicsMode, render_distance: i32, dx: i32, dz: i32) -> bool {
    let d2 = dx * dx + dz * dz;
    match graphics {
        crate::settings::GraphicsMode::Fast => d2 > 6 * 6,
        crate::settings::GraphicsMode::Balanced => {
            let r = (render_distance * 2 / 3).max(10);
            d2 > r * r
        }
        crate::settings::GraphicsMode::High => false,
    }
}

/// Fast far columns keep the surface slab, one below for cliff faces,
/// and the +2 safety slabs for trees / low islands. Deep underground
/// is skipped until the player flies closer.
fn skip_fast_deep_far(fast: bool, far_lod: bool, cy: i32, col_top: i32) -> bool {
    fast && far_lod && cy + 3 < col_top
}

fn rebuild_load_offsets(streamer: &mut ChunkStreamer, rd: i32) {
    streamer.load_offsets.clear();
    let rd2 = rd * rd;
    for dx in -rd..=rd {
        for dz in -rd..=rd {
            let d2 = dx * dx + dz * dz;
            if d2 <= rd2 {
                streamer.load_offsets.push((d2, dx, dz));
            }
        }
    }
    streamer
        .load_offsets
        .sort_unstable_by_key(|(d2, dx, dz)| (*d2, dx.abs() + dz.abs()));
    streamer.load_offsets_rd = rd;
    streamer.load_cursor = 0;
    streamer.frontier_complete = false;
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

/// Neighbours past the load disc are never generated, so waiting on them
/// leaves a permanent dirty rim (~300 chunks at Fast RD 12) that the
/// mesher rescans every frame. Treat those slots as air; the snapshot
/// already samples missing voxels as AIR. Inside the disc we still wait.
fn chunk_slot_mesh_neighbour_ready(
    world: &mut VoxelWorld,
    pos: ChunkPos,
    vertical_chunks: i32,
    pcx: i32,
    pcz: i32,
    load_r2: i32,
) -> bool {
    if chunk_slot_loaded_or_known_air(world, pos, vertical_chunks) {
        return true;
    }
    let dx = pos.x - pcx;
    let dz = pos.z - pcz;
    dx * dx + dz * dz > load_r2
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
    time: Res<Time>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    mut governor: ResMut<StreamingGovernor>,
) {
    let Ok(transform) = anchors.get_single() else {
        return;
    };
    let (px, _py, pz) = (
        crate::chunk::to_i32_safe(transform.translation.x),
        crate::chunk::to_i32_safe(transform.translation.y),
        crate::chunk::to_i32_safe(transform.translation.z),
    );
    let pcx = px.div_euclid(CHUNK_SIZE_I);
    let pcz = pz.div_euclid(CHUNK_SIZE_I);

    streamer.stream_elapsed += time.delta_seconds();
    // Staged startup: fill a spawn ring first, then open the horizon.
    // Holding RD=8 for a full 4s made Fast/iGPU worlds feel like they
    // "take super long till it shows up" — the disc was ready but the
    // streamer refused to apply it.
    let fast = settings.graphics == crate::settings::GraphicsMode::Fast;
    let elapsed = streamer.stream_elapsed;
    let mut rd = sync_streaming_governor(&mut governor, &budget, &streamer);
    let (rd_cap, warmup_boost) = if elapsed < 0.90 {
        (if fast { 5 } else { 6 }, true)
    } else if elapsed < 2.20 {
        (if fast { 8 } else { 10 }, true)
    } else if elapsed < 3.40 {
        (rd.min(if fast { 12 } else { 16 }), elapsed < 3.0)
    } else if elapsed < 5.0 {
        (rd.min(if fast { 12 } else { 18 }), false)
    } else {
        (rd, false)
    };
    rd = rd.min(rd_cap).max(2);
    let retain = rd + 2;
    let retain2 = retain * retain;
    let vertical = settings.vertical_chunks as i32;

    if streamer.load_offsets_rd != rd {
        rebuild_load_offsets(&mut streamer, rd);
    }
    let vertical_changed = streamer.last_vertical_chunks != vertical;
    if vertical_changed {
        streamer.last_vertical_chunks = vertical;
        streamer.frontier_complete = false;
        streamer.load_cursor = 0;
    }

    // Did the player cross a chunk boundary? That's the only event (aside
    // from unload / pending-task completion) that can make new chunks
    // become needed, so we only reset the frontier flag on a real move.
    let cur_anchor = (pcx, pcz);
    let moved = streamer.last_anchor_cxz != Some(cur_anchor);
    if moved {
        streamer.frontier_complete = false;
        streamer.last_anchor_cxz = Some(cur_anchor);
        streamer.load_cursor = 0;
    }

    // 1. Unload chunks outside the retention radius (also drop stale
    //    pending tasks for those positions so we don't keep working on
    //    chunks the player already left behind). Only run on real
    //    movement — a stationary player cannot invalidate the retention
    //    set, and at RD=50 this scan touches ~47 k chunk keys.
    if moved || vertical_changed {
        let mut to_drop = Vec::new();
        for pos in world.chunks.keys() {
            let dx = pos.x - pcx;
            let dz = pos.z - pcz;
            if dx * dx + dz * dz > retain2 || pos.y < 0 || pos.y >= vertical {
                to_drop.push(*pos);
            }
        }
        if !to_drop.is_empty() {
            // Space opened up at the frontier — must rescan.
            streamer.frontier_complete = false;
            // Mesh entities might now be orphaned — let the mesh
            // system run its cleanup pass this frame.
            streamer.needs_orphan_scan = true;
        }
        for p in &to_drop {
            world.remove_chunk(p);
        }
        // Evict the column-top cache for columns that fell outside
        // the retain radius. Without this the cache grew unbounded
        // as the player explored — ≈24 bytes per (cx,cz) entry
        // times millions of visited columns over a long session.
        world
            .column_top_cy
            .retain(|(cx, cz), _| (cx - pcx).pow(2) + (cz - pcz).pow(2) <= retain2);
        streamer.pending_terrain.retain(|p, _| {
            (p.x - pcx).pow(2) + (p.z - pcz).pow(2) <= retain2 && p.y >= 0 && p.y < vertical
        });
        streamer.pending_meshes.retain(|p, _| {
            (p.x - pcx).pow(2) + (p.z - pcz).pow(2) <= retain2 && p.y >= 0 && p.y < vertical
        });
        // Drop any dirty entries that are now out of range too.
        streamer.dirty_queue.retain(|p| {
            (p.x - pcx).pow(2) + (p.z - pcz).pow(2) <= retain2 && p.y >= 0 && p.y < vertical
        });
    }

    // 2. Poll finished terrain tasks and fold them back into the world.
    // Cap installs too: terrain generation finishes on worker threads in
    // waves, and installing every completed chunk in one frame causes the
    // one-second hitch the player sees while flying at max distance.
    let terrain_apply_cap = if warmup_boost || budget.startup_fill {
        (budget.chunks_per_frame.max(1) as usize)
            .saturating_mul(if warmup_boost { 3 } else { 2 })
            .max(if fast { 20 } else { 16 })
            .min(if fast { 40 } else { 32 })
    } else {
        (budget.chunks_per_frame.max(1) as usize).min(12)
    };
    let mut applied_terrain = 0usize;
    let mut done: Vec<ChunkPos> = Vec::new();
    let mut newly_loaded: Vec<ChunkPos> = Vec::new();
    for (pos, task) in streamer.pending_terrain.iter_mut() {
        if applied_terrain >= terrain_apply_cap {
            break;
        }
        if let Some((cp, voxels)) = future::block_on(future::poll_once(task)) {
            let mut chunk = Chunk::new(cp);
            chunk.install_voxels(voxels);
            if let Some(edited) = world.edited_overrides.get(&cp).cloned() {
                if let Some((voxels, materials)) = edited.into_shared() {
                    chunk.install_voxels_and_materials(voxels, materials);
                }
            }
            world.insert_chunk(cp, chunk);
            done.push(*pos);
            newly_loaded.push(cp);
            applied_terrain += 1;
        }
    }
    for p in done {
        streamer.pending_terrain.remove(&p);
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
            // Fast far meshes skip seam remesh later; don't keep those
            // neighbours in the dirty queue or the governor never settles.
            if fast {
                let already_far = streamer
                    .entities
                    .get(&n)
                    .and_then(|group| group.first())
                    .is_some_and(|entry| entry.far_lod);
                if already_far
                    && chunk_wants_far_lod(settings.graphics, rd, n.x - pcx, n.z - pcz)
                {
                    continue;
                }
            }
            if let Some(c) = world.chunks.get_mut(&n) {
                c.dirty = true;
                streamer.dirty_queue.insert(n);
            }
        }
    }

    // 3. Queue new terrain jobs for nearby chunks, camera-priority first,
    //    up to `max_in_flight_terrain` tasks total across threads.
    //
    //    Fast-path: if the frontier is fully loaded AND every pending
    //    task slot is busy (or no slots matter because there's nothing
    //    to schedule), skip the entire sweep. This turns RD=50 steady-
    //    state cost from ~160 k HashMap lookups per frame down to zero.
    let max_in_flight = budget.max_in_flight_terrain as usize;
    if streamer.frontier_complete || streamer.pending_terrain.len() >= max_in_flight {
        // Nothing to do — frontier already saturated or no task slots.
    } else if streamer.pending_terrain.len() < max_in_flight {
        #[cfg(not(target_arch = "wasm32"))]
        let pool = AsyncComputeTaskPool::get();
        let gen_seed = world.generator.seed;
        let spawn_budget = if warmup_boost {
            (budget.chunks_per_frame.max(1) as usize).max(if fast { 16 } else { 12 })
        } else {
            budget.chunks_per_frame.max(1) as usize
        };
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
            let col_top = world.column_top_cy_cached(cx, cz);
            let mut column_complete = true;

            for cy in 0..vertical {
                let cp = ChunkPos::new(cx, cy, cz);
                if world.chunks.contains_key(&cp) || streamer.pending_terrain.contains_key(&cp) {
                    continue;
                }
                if cy > col_top {
                    continue;
                }
                if skip_fast_deep_far(
                    fast,
                    chunk_wants_far_lod(settings.graphics, rd, dx, dz),
                    cy,
                    col_top,
                ) {
                    continue;
                }
                if streamer.pending_terrain.len() >= max_in_flight || spawned >= spawn_budget {
                    column_complete = false;
                    break;
                }
                let gen = TerrainGenerator::new(gen_seed);
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
                    streamer.pending_terrain.insert(cp, task);
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
    if !world.edit_dirty_chunks.is_empty() {
        streamer.dirty_queue.extend(world.edit_dirty_chunks.drain());
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

    // 1. Poll finished meshing tasks. Cap how many we actually *apply*
    //    (spawn entities for) per frame so a flood of finished tasks
    //    can't spike the frame budget with mesh.add() + commands.spawn().
    let spawn_cap = if budget.startup_fill || streamer.stream_elapsed < 2.4 {
        (budget.mesh_applies_per_frame as usize)
            .saturating_mul(3)
            .max(24)
            .min(56)
    } else if streamer.stream_elapsed < 4.0 {
        (budget.mesh_applies_per_frame as usize)
            .saturating_mul(2)
            .max(16)
            .min(48)
    } else {
        budget.mesh_applies_per_frame as usize
    };
    let mut applied = 0usize;
    let mut done_keys: Vec<ChunkPos> = Vec::new();
    let mut finished: Vec<(ChunkPos, Vec<(MaterialId, Mesh)>, bool)> = Vec::new();

    for (pos, task) in streamer.pending_meshes.iter_mut() {
        if applied >= spawn_cap {
            break;
        }
        if let Some((cp, mesh, far_lod)) = future::block_on(future::poll_once(task)) {
            finished.push((cp, mesh, far_lod));
            done_keys.push(*pos);
            applied += 1;
        }
    }
    for p in done_keys {
        streamer.pending_meshes.remove(&p);
    }
    for (pos, buckets, far_lod) in finished {
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
        let aabb = bevy::render::primitives::Aabb::from_min_max(
            Vec3::ZERO,
            Vec3::splat(CHUNK_SIZE_I as f32),
        );
        let shadow_radius = budget.shadow_radius.max(2);
        let shadow_r2 = shadow_radius * shadow_radius;
        let dx = pos.x - pcx;
        let dz = pos.z - pcz;
        let far = dx * dx + dz * dz > shadow_r2;
        let mut next_entries = Vec::with_capacity(buckets.len());

        for (material_id, mesh) in buckets {
            let Some(material_handle) = material_library
                .handle_for(material_id)
                .or_else(|| streamer.material.clone())
            else {
                continue;
            };

            if let Some(idx) = previous
                .iter()
                .position(|entry| entry.material == material_id)
            {
                let mut entry = previous.swap_remove(idx);
                if let Some(slot) = meshes.get_mut(&entry.handle) {
                    *slot = mesh;
                } else {
                    let new_handle = meshes.add(mesh);
                    if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                        entity_commands.insert(new_handle.clone());
                    }
                    entry.handle = new_handle;
                }
                entry.far_lod = far_lod;
                next_entries.push(entry);
                continue;
            }

            let handle = meshes.add(mesh);
            let entity = if far {
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
                far_lod,
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
    let max_in_flight = budget.max_in_flight_meshes as usize;
    if streamer.pending_meshes.len() >= max_in_flight {
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
    candidates.reserve(scan_budget.min(dirty_total));
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
    let mut slots = max_in_flight - streamer.pending_meshes.len();
    let mut scheduled_this_frame = 0usize;

    for (_s, pos) in candidates.drain(..) {
        let dx = pos.x - pcx;
        let dz = pos.z - pcz;
        let far_collapse = chunk_wants_far_lod(settings.graphics, budget.render_distance, dx, dz);
        let already_far_lod = streamer
            .entities
            .get(&pos)
            .and_then(|group| group.first())
            .is_some_and(|entry| entry.far_lod);
        // Fast/Balanced far meshes are one (plus emissive) draw call.
        // Neighbour-seam remeshes at that distance are fog-hidden and
        // were the post-fill hitch source. Remesh only when LOD level
        // changes (player flew closer) or the chunk has no mesh yet.
        if far_collapse && already_far_lod {
            if let Some(c) = world.chunks.get_mut(&pos) {
                c.dirty = false;
            }
            continue;
        }
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
        let load_r2 = budget.render_distance.max(2) * budget.render_distance.max(2);
        let neighbours_needed = [
            ChunkPos::new(pos.x + 1, pos.y, pos.z),
            ChunkPos::new(pos.x - 1, pos.y, pos.z),
            ChunkPos::new(pos.x, pos.y, pos.z + 1),
            ChunkPos::new(pos.x, pos.y, pos.z - 1),
            ChunkPos::new(pos.x, pos.y + 1, pos.z),
            ChunkPos::new(pos.x, pos.y - 1, pos.z),
        ];
        let all_neighbours_ready = neighbours_needed.into_iter().all(|n| {
            chunk_slot_mesh_neighbour_ready(
                &mut world,
                n,
                vertical_chunks,
                pcx,
                pcz,
                load_r2,
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

        // LOD: skip per-corner AO past `lod_radius`. Fast also collapses
        // opaque far chunks to one draw call (vertex-tinted + emissives).
        let lod_radius = (budget.render_distance / 2).max(4);
        let use_ao = settings.graphics != crate::settings::GraphicsMode::Fast
            && dx * dx + dz * dz <= lod_radius * lod_radius;

        #[cfg(target_arch = "wasm32")]
        {
            let buckets = build_mesh_buckets_ex(
                pos,
                |wx, wy, wz| snap.sample_with_material(wx, wy, wz),
                use_ao,
                far_collapse,
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
                far_collapse,
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let task = pool.spawn(async move {
                let buckets = build_mesh_buckets_ex(
                    pos,
                    |wx, wy, wz| snap.sample_with_material(wx, wy, wz),
                    use_ao,
                    far_collapse,
                );
                (pos, buckets, far_collapse)
            });
            streamer.pending_meshes.insert(pos, task);
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
        32
    } else if pressure >= 0.55 {
        48
    } else {
        96
    };
    let budget = schedule_budget
        .max(1)
        .saturating_mul(multiplier)
        .max(max_in_flight.saturating_mul(2))
        .max(64);
    budget.min(dirty_total)
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
    far_lod: bool,
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
    let aabb =
        bevy::render::primitives::Aabb::from_min_max(Vec3::ZERO, Vec3::splat(CHUNK_SIZE_I as f32));
    let shadow_radius = budget.shadow_radius.max(2);
    let shadow_r2 = shadow_radius * shadow_radius;
    let dx = pos.x - pcx;
    let dz = pos.z - pcz;
    let far = dx * dx + dz * dz > shadow_r2;
    let mut next_entries = Vec::with_capacity(buckets.len());

    for (material_id, mesh) in buckets {
        let Some(material_handle) = material_library
            .handle_for(material_id)
            .or_else(|| streamer.material.clone())
        else {
            continue;
        };

        if let Some(idx) = previous
            .iter()
            .position(|entry| entry.material == material_id)
        {
            let mut entry = previous.swap_remove(idx);
            if let Some(slot) = meshes.get_mut(&entry.handle) {
                *slot = mesh;
            } else {
                let new_handle = meshes.add(mesh);
                if let Some(mut entity_commands) = commands.get_entity(entry.entity) {
                    entity_commands.insert(new_handle.clone());
                }
                entry.handle = new_handle;
            }
            entry.far_lod = far_lod;
            next_entries.push(entry);
            continue;
        }

        let handle = meshes.add(mesh);
        let entity = if far {
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
            far_lod,
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
        assert_eq!(budget, 160);
    }

    #[test]
    fn dirty_mesh_candidate_scan_budget_expands_when_stable() {
        let pressured = dirty_mesh_candidate_scan_budget(10_000, 4, 80, 1.0);
        let stable = dirty_mesh_candidate_scan_budget(10_000, 4, 80, 0.0);

        assert!(stable > pressured);
        assert_eq!(stable, 384);
    }

    #[test]
    fn visual_artifact_repair_removes_frozen_override_and_regenerates_loaded_chunk() {
        let mut world = VoxelWorld::new();
        let pos = ChunkPos::new(0, 3, 0);
        let ice: Voxel = BlockType::Ice.into();
        world.edited_overrides.insert(pos, override_chunk(ice));
        world.insert_chunk(pos, solid_chunk(pos, ice));

        let report = world.repair_visual_artifact_overrides();

        assert_eq!(report.scanned_chunks, 1);
        assert_eq!(report.removed_chunks, 1);
        assert_eq!(report.refreshed_loaded_chunks, 1);
        assert_eq!(world.last_repair_report, Some(report));
        assert!(!world.edited_overrides.contains_key(&pos));
        assert!(world.edit_save_dirty);
        assert!(world.edit_dirty_chunks.contains(&pos));
        assert_ne!(world.chunks.get(&pos).unwrap().get(0, 0, 0), ice);
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

    #[test]
    fn visual_artifact_repair_keeps_neon_builds_made_from_the_frontier_palette() {
        // Crystal and glow-sand are ordinary frontier materials and a
        // headline building feature. Wiping a player's neon tower as an
        // "artifact" would be the single most destructive thing the
        // repair pass could do.
        for block in [
            BlockType::Crystal,
            BlockType::LuminiteCrystal,
            BlockType::AlienMoss,
            BlockType::GlowSand,
            BlockType::CrystalMagenta,
            BlockType::PlatingWhite,
        ] {
            let mut world = VoxelWorld::new();
            let pos = ChunkPos::new(2, 3, 2);
            world
                .edited_overrides
                .insert(pos, override_chunk(block.into()));

            let report = world.repair_visual_artifact_overrides();

            assert_eq!(
                report.removed_chunks, 0,
                "repair pass deleted a build made of {block:?}"
            );
            assert!(world.edited_overrides.contains_key(&pos));
        }
    }

    #[test]
    fn fast_collapses_far_chunks_and_cinematic_does_not() {
        assert!(chunk_wants_far_lod(
            crate::settings::GraphicsMode::Fast,
            16,
            8,
            0
        ));
        assert!(!chunk_wants_far_lod(
            crate::settings::GraphicsMode::Fast,
            16,
            4,
            0
        ));
        assert!(!chunk_wants_far_lod(
            crate::settings::GraphicsMode::High,
            40,
            20,
            0
        ));
        assert!(skip_fast_deep_far(true, true, 0, 6));
        assert!(!skip_fast_deep_far(true, true, 4, 6));
        assert!(!skip_fast_deep_far(true, false, 0, 6));
        assert!(!skip_fast_deep_far(false, true, 0, 6));
    }

    #[test]
    fn mesh_neighbours_outside_the_load_disc_count_as_ready() {
        let mut world = VoxelWorld::new();
        let load_r2 = 12 * 12;
        world.column_top_cy.insert((5, 0), 2);
        world.column_top_cy.insert((13, 0), 2);
        assert!(chunk_slot_mesh_neighbour_ready(
            &mut world,
            ChunkPos::new(13, 0, 0),
            7,
            0,
            0,
            load_r2
        ));
        assert!(!chunk_slot_mesh_neighbour_ready(
            &mut world,
            ChunkPos::new(5, 0, 0),
            7,
            0,
            0,
            load_r2
        ));
        world.insert_chunk(ChunkPos::new(5, 0, 0), Chunk::new(ChunkPos::new(5, 0, 0)));
        assert!(chunk_slot_mesh_neighbour_ready(
            &mut world,
            ChunkPos::new(5, 0, 0),
            7,
            0,
            0,
            load_r2
        ));
    }
}
