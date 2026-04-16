//! World plugin — owns the chunk map, streams chunks around the player,
//! and schedules terrain + meshing work on background threads via
//! `AsyncComputeTaskPool` so a render distance of 20+ chunks stays snappy
//! even on modest hardware.
//!
//! Port target: `lib/voxel/ChunkManager.ts` + `lib/voxel/worker.ts`.

use ahash::AHashMap;
use bevy::prelude::*;
use bevy::tasks::{AsyncComputeTaskPool, Task};
use futures_lite::future;

use crate::blocks::{voxel_is_solid, Voxel, AIR};
use crate::chunk::{world_to_chunk, Chunk, ChunkPos, SharedVoxels, CHUNK_SIZE_I, CHUNK_VOLUME};
use crate::mesher::build_mesh;
use crate::settings::WorldSettings;
use crate::terrain::TerrainGenerator;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(VoxelWorld::new())
            .insert_resource(ChunkStreamer::default())
            .add_systems(Startup, init_world)
            .add_systems(
                OnEnter(crate::menu::GameState::InGame),
                reinit_world_for_active,
            )
            .add_systems(
                Update,
                (stream_chunks, mesh_dirty_chunks)
                    .chain()
                    .run_if(in_state(crate::menu::GameState::InGame)),
            );
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
    pending: Res<crate::menu::PendingWorldLoad>,
    mut commands: Commands,
) {
    if !pending.0 {
        return;
    }
    world.generator = TerrainGenerator::new(settings.seed);
    world.chunks.clear();
    world.column_top_cy.clear();
    streamer.pending_terrain.clear();
    streamer.pending_meshes.clear();
    streamer.dirty_queue.clear();
    for (_, (entity, handle)) in streamer.entities.drain() {
        commands.entity(entity).despawn_recursive();
        let _ = meshes.remove(&handle);
    }
}

#[derive(Resource)]
pub struct VoxelWorld {
    pub chunks: AHashMap<ChunkPos, Chunk>,
    pub generator: TerrainGenerator,
    /// Per (cx, cz) column: the maximum surface height within that column,
    /// quantised to a chunk-y index. Chunks strictly above this index are
    /// guaranteed air and don't need terrain-gen / meshing work. Populated
    /// lazily as columns are first visited so RD=50 stays cheap.
    pub column_top_cy: AHashMap<(i32, i32), i32>,
}

impl VoxelWorld {
    pub fn new() -> Self {
        Self {
            chunks: AHashMap::new(),
            generator: TerrainGenerator::new(12345),
            column_top_cy: AHashMap::new(),
        }
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
        self.chunks.keys().any(|p| p.x == cx && p.z == cz)
    }

    /// Terrain surface height (block y of the topmost solid block) at a
    /// world (x, z) column.
    pub fn surface_height_at(&self, wx: i32, wz: i32) -> i32 {
        self.generator.surface_height_at(wx, wz)
    }

    /// Highest chunk-y index that could contain non-air terrain for the
    /// given chunk column, cached. Samples the surface height at 4 corners
    /// + centre of the column so we catch the max peak without evaluating
    /// the expensive noise at every block. Adds +1 chunk of headroom for
    /// trees and other decorations.
    pub fn column_top_cy_cached(&mut self, cx: i32, cz: i32) -> i32 {
        if let Some(v) = self.column_top_cy.get(&(cx, cz)) {
            return *v;
        }
        let s = CHUNK_SIZE_I;
        let wx0 = cx * s;
        let wz0 = cz * s;
        let points = [
            (wx0, wz0),
            (wx0 + s - 1, wz0),
            (wx0, wz0 + s - 1),
            (wx0 + s - 1, wz0 + s - 1),
            (wx0 + s / 2, wz0 + s / 2),
        ];
        let mut max_h = i32::MIN;
        for (x, z) in points {
            let h = self.generator.surface_height_at(x, z);
            if h > max_h {
                max_h = h;
            }
        }
        // +1 chunk of headroom for trees / overhangs / water surface.
        let top_cy = (max_h / s) + 1;
        self.column_top_cy.insert((cx, cz), top_cy);
        top_cy
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
    pub entities: AHashMap<ChunkPos, (Entity, Handle<Mesh>)>,
    pub material: Option<Handle<StandardMaterial>>,
    pub water_material: Option<Handle<StandardMaterial>>,
    /// In-flight terrain-generation tasks (one per chunk position).
    pub pending_terrain: AHashMap<ChunkPos, Task<(ChunkPos, SharedVoxels)>>,
    /// In-flight meshing tasks (one per chunk position). `None` mesh =
    /// chunk is empty / uniform-solid and needs no geometry.
    pub pending_meshes: AHashMap<ChunkPos, Task<(ChunkPos, Option<Mesh>)>>,
    /// Dirty-chunk queue so the mesh scheduler doesn't walk the entire
    /// chunk hashmap every frame. Populated by whoever flips `c.dirty`.
    pub dirty_queue: Vec<ChunkPos>,
}

fn init_world(
    mut streamer: ResMut<ChunkStreamer>,
    mut world: ResMut<VoxelWorld>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    settings: Res<WorldSettings>,
) {
    world.generator = TerrainGenerator::new(settings.seed);

    streamer.material = Some(materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        reflectance: 0.05,
        ..default()
    }));

    streamer.water_material = Some(materials.add(StandardMaterial {
        base_color: Color::srgba(0.2, 0.55, 0.85, 0.6),
        perceptual_roughness: 0.1,
        reflectance: 0.3,
        alpha_mode: AlphaMode::Blend,
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

/// Load chunks inside `render_distance` of the player (measured in chunks
/// on the X/Z plane) and unload any that drift outside retention range.
/// Terrain generation runs on the async compute task pool.
fn stream_chunks(
    anchors: Query<&Transform, With<ChunkAnchor>>,
    settings: Res<WorldSettings>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
) {
    let Ok(transform) = anchors.get_single() else {
        return;
    };
    let (px, _py, pz) = (
        transform.translation.x as i32,
        transform.translation.y as i32,
        transform.translation.z as i32,
    );
    let pcx = px.div_euclid(CHUNK_SIZE_I);
    let pcz = pz.div_euclid(CHUNK_SIZE_I);

    let fwd3 = transform.forward();
    let forward = Vec2::new(fwd3.x, fwd3.z).normalize_or_zero();

    let rd = settings.render_distance as i32;
    let rd2 = rd * rd;
    let retain = (settings.render_distance + 2) as i32;
    let retain2 = retain * retain;
    let vertical = settings.vertical_chunks as i32;

    // 1. Unload chunks outside the retention radius (also drop stale
    //    pending tasks for those positions so we don't keep working on
    //    chunks the player already left behind).
    let mut to_drop = Vec::new();
    for pos in world.chunks.keys() {
        let dx = pos.x - pcx;
        let dz = pos.z - pcz;
        if dx * dx + dz * dz > retain2 {
            to_drop.push(*pos);
        }
    }
    for p in &to_drop {
        world.chunks.remove(p);
    }
    streamer
        .pending_terrain
        .retain(|p, _| (p.x - pcx).pow(2) + (p.z - pcz).pow(2) <= retain2);
    streamer
        .pending_meshes
        .retain(|p, _| (p.x - pcx).pow(2) + (p.z - pcz).pow(2) <= retain2);

    // 2. Poll finished terrain tasks and fold them back into the world.
    let mut done: Vec<ChunkPos> = Vec::new();
    let mut newly_loaded: Vec<ChunkPos> = Vec::new();
    for (pos, task) in streamer.pending_terrain.iter_mut() {
        if let Some((cp, voxels)) = future::block_on(future::poll_once(task)) {
            let mut chunk = Chunk::new(cp);
            chunk.install_voxels(voxels);
            world.chunks.insert(cp, chunk);
            done.push(*pos);
            newly_loaded.push(cp);
        }
    }
    for p in done {
        streamer.pending_terrain.remove(&p);
    }
    // Enqueue every newly-loaded chunk + its 6 neighbours so seams get
    // remeshed cleanly without a whole-world scan.
    for cp in newly_loaded {
        streamer.dirty_queue.push(cp);
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
                streamer.dirty_queue.push(n);
            }
        }
    }

    // 3. Queue new terrain jobs for nearby chunks, camera-priority first,
    //    up to `max_in_flight_terrain` tasks total across threads.
    let max_in_flight = settings.max_in_flight_terrain as usize;
    if streamer.pending_terrain.len() < max_in_flight {
        let mut wanted: Vec<(i32, ChunkPos)> = Vec::new();
        for dx in -rd..=rd {
            for dz in -rd..=rd {
                let d2 = dx * dx + dz * dz;
                if d2 > rd2 {
                    continue;
                }
                let score = priority_score(dx, dz, forward);
                let cx = pcx + dx;
                let cz = pcz + dz;
                // Column-level cull: any chunk above the column's max
                // surface height is guaranteed air. We can pretend it's
                // loaded and empty without ever generating it. This is
                // the single biggest win for RD >= 30 — typical terrain
                // covers at most 2-3 of the 8 vertical chunks.
                let col_top = world.column_top_cy_cached(cx, cz);
                for cy in 0..vertical {
                    let cp = ChunkPos::new(cx, cy, cz);
                    if world.chunks.contains_key(&cp)
                        || streamer.pending_terrain.contains_key(&cp)
                    {
                        continue;
                    }
                    if cy > col_top {
                        // Synthesise an empty chunk on the main thread —
                        // trivial, no task needed.
                        let mut air_chunk = Chunk::new(cp);
                        air_chunk.install_voxels(std::sync::Arc::new([AIR; CHUNK_VOLUME]));
                        world.chunks.insert(cp, air_chunk);
                        streamer.dirty_queue.push(cp);
                        continue;
                    }
                    let cy_bias = (cy - vertical / 2).abs() * 4;
                    wanted.push((score + cy_bias, cp));
                }
            }
        }
        wanted.sort_unstable_by_key(|(s, _)| *s);

        let pool = AsyncComputeTaskPool::get();
        let gen_seed = world.generator.seed;
        let slots = max_in_flight - streamer.pending_terrain.len();
        for (_d, pos) in wanted.into_iter().take(slots) {
            let gen = TerrainGenerator::new(gen_seed);
            let task = pool.spawn(async move {
                let mut chunk = Chunk::new(pos);
                gen.generate(&mut chunk);
                (pos, chunk.voxels_shared())
            });
            streamer.pending_terrain.insert(pos, task);
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
    anchors: Query<&Transform, With<ChunkAnchor>>,
) {
    let Some(material) = streamer.material.clone() else {
        return;
    };

    // 1. Poll finished meshing tasks. Cap how many we actually *apply*
    //    (spawn entities for) per frame so a flood of finished tasks
    //    can't spike the frame budget with mesh.add() + commands.spawn().
    let spawn_cap = settings.mesh_applies_per_frame as usize;
    let mut applied = 0usize;
    let mut done_keys: Vec<ChunkPos> = Vec::new();
    let mut finished: Vec<(ChunkPos, Option<Mesh>)> = Vec::new();

    for (pos, task) in streamer.pending_meshes.iter_mut() {
        if applied >= spawn_cap {
            break;
        }
        if let Some((cp, mesh)) = future::block_on(future::poll_once(task)) {
            finished.push((cp, mesh));
            done_keys.push(*pos);
            applied += 1;
        }
    }
    for p in done_keys {
        streamer.pending_meshes.remove(&p);
    }
    for (pos, mesh_opt) in finished {
        // Despawn previous mesh entity AND free its GPU buffer explicitly.
        if let Some((prev, old_handle)) = streamer.entities.remove(&pos) {
            commands.entity(prev).despawn_recursive();
            let _ = meshes.remove(&old_handle);
        }
        if let Some(mesh) = mesh_opt {
            let handle = meshes.add(mesh);
            let (ox, oy, oz) = pos.origin();
            let transform = Transform::from_xyz(ox as f32, oy as f32, oz as f32);
            // Explicit AABB covering the full chunk volume. Bevy's auto-
            // AABB is derived from vertex positions — a chunk with a
            // tight greedy mesh near one corner would be frustum-culled
            // too eagerly and show up as a visible hole at grazing
            // angles. This forces Bevy to treat every chunk as a full
            // 16×16×16 box for culling purposes.
            let aabb = bevy::render::primitives::Aabb::from_min_max(
                Vec3::ZERO,
                Vec3::splat(CHUNK_SIZE_I as f32),
            );
            let entity = commands
                .spawn((
                    PbrBundle {
                        mesh: handle.clone(),
                        material: material.clone(),
                        transform,
                        ..default()
                    },
                    aabb,
                ))
                .id();
            streamer.entities.insert(pos, (entity, handle));
        }
    }

    // 2. Queue new mesh jobs. Camera-priority first.
    let max_in_flight = settings.max_in_flight_meshes as usize;
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
    let (pcx, pcz) = anchors
        .get_single()
        .map(|t| {
            (
                (t.translation.x as i32).div_euclid(CHUNK_SIZE_I),
                (t.translation.z as i32).div_euclid(CHUNK_SIZE_I),
            )
        })
        .unwrap_or((0, 0));

    // Drain the dirty queue into a candidate list. Retain anything we
    // can't schedule this frame (no slots / missing neighbours) so it's
    // picked up next frame without rescanning the whole world.
    let mut candidates: Vec<(i32, ChunkPos)> = Vec::with_capacity(streamer.dirty_queue.len());
    let queue = std::mem::take(&mut streamer.dirty_queue);
    for p in queue {
        let Some(c) = world.chunks.get(&p) else { continue };
        if !c.dirty || streamer.pending_meshes.contains_key(&p) {
            continue;
        }
        candidates.push((priority_score(p.x - pcx, p.z - pcz, forward), p));
    }
    candidates.sort_unstable_by_key(|(s, _)| *s);

    let pool = AsyncComputeTaskPool::get();
    let mut slots = max_in_flight - streamer.pending_meshes.len();

    for (_s, pos) in candidates {
        if slots == 0 {
            // Put back into the dirty queue for next frame.
            streamer.dirty_queue.push(pos);
            continue;
        }
        // Horizontal seam avoidance: require 4 XZ neighbours loaded.
        let neighbours_xz = [
            ChunkPos::new(pos.x + 1, pos.y, pos.z),
            ChunkPos::new(pos.x - 1, pos.y, pos.z),
            ChunkPos::new(pos.x, pos.y, pos.z + 1),
            ChunkPos::new(pos.x, pos.y, pos.z - 1),
        ];
        if !neighbours_xz.iter().all(|n| world.chunks.contains_key(n)) {
            // Neighbours haven't streamed in yet; try again next frame.
            streamer.dirty_queue.push(pos);
            continue;
        }

        // All six neighbours (XZ + Y above/below) for the uniform fast
        // path. Critical: we ONLY skip chunks that are completely empty
        // (all AIR) AND whose six neighbours are all empty. The stricter
        // `is_uniform_solid` skip was removed because it was producing
        // chunk-sized holes where a solid chunk touched mixed terrain
        // whose uniform-flag briefly disagreed across frames.
        let neighbours_all = [
            ChunkPos::new(pos.x + 1, pos.y, pos.z),
            ChunkPos::new(pos.x - 1, pos.y, pos.z),
            ChunkPos::new(pos.x, pos.y, pos.z + 1),
            ChunkPos::new(pos.x, pos.y, pos.z - 1),
            ChunkPos::new(pos.x, pos.y + 1, pos.z),
            ChunkPos::new(pos.x, pos.y - 1, pos.z),
        ];
        let fast_skip = {
            let center = world.chunks.get(&pos).unwrap();
            center.is_empty
                && neighbours_all
                    .iter()
                    .all(|n| world.chunks.get(n).map(|c| c.is_empty).unwrap_or(false))
        };
        if fast_skip {
            if let Some((prev, old_handle)) = streamer.entities.remove(&pos) {
                commands.entity(prev).despawn_recursive();
                let _ = meshes.remove(&old_handle);
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

        let task = pool.spawn(async move {
            let mesh = build_mesh(pos, |wx, wy, wz| snap.sample(wx, wy, wz));
            // Meshes with no vertices are pure air/occluded -> skip spawn.
            let opt = if mesh_is_empty(&mesh) { None } else { Some(mesh) };
            (pos, opt)
        });
        streamer.pending_meshes.insert(pos, task);
        slots -= 1;
    }

    // 3. Clean up orphaned mesh entities whose chunk has streamed out.
    //    Free the GPU buffer too so long sessions don't accumulate.
    let mut orphaned = Vec::new();
    for (pos, (entity, handle)) in streamer.entities.iter() {
        if !world.chunks.contains_key(pos) {
            orphaned.push((*pos, *entity, handle.clone()));
        }
    }
    for (pos, entity, handle) in orphaned {
        commands.entity(entity).despawn_recursive();
        let _ = meshes.remove(&handle);
        streamer.entities.remove(&pos);
    }
}

fn mesh_is_empty(mesh: &Mesh) -> bool {
    match mesh.indices() {
        Some(bevy::render::mesh::Indices::U32(i)) => i.is_empty(),
        Some(bevy::render::mesh::Indices::U16(i)) => i.is_empty(),
        None => true,
    }
}

/// Immutable snapshot of a chunk + its 6 cardinal neighbours, used by
/// the off-thread mesher. All storage is `Arc`-shared so the snapshot is
/// an O(1) refcount bump instead of 7 × 4 KB memcpy on the main thread.
struct ChunkSnapshot {
    pos: ChunkPos,
    center: SharedVoxels,
    neighbours: [Option<SharedVoxels>; 6],
}

impl ChunkSnapshot {
    fn build(world: &VoxelWorld, pos: ChunkPos) -> Self {
        let center = world
            .chunks
            .get(&pos)
            .map(|c| c.voxels_shared())
            .unwrap_or_else(|| std::sync::Arc::new([AIR; CHUNK_VOLUME]));
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
        Self {
            pos,
            center,
            neighbours,
        }
    }

    #[inline]
    fn sample(&self, wx: i32, wy: i32, wz: i32) -> Voxel {
        let (cp, lx, ly, lz) = world_to_chunk(wx, wy, wz);
        let dx = cp.x - self.pos.x;
        let dy = cp.y - self.pos.y;
        let dz = cp.z - self.pos.z;
        let idx = Chunk::index(lx, ly, lz);
        if (dx, dy, dz) == (0, 0, 0) {
            return self.center[idx];
        }
        let ni = match (dx, dy, dz) {
            (1, 0, 0) => 0,
            (-1, 0, 0) => 1,
            (0, 1, 0) => 2,
            (0, -1, 0) => 3,
            (0, 0, 1) => 4,
            (0, 0, -1) => 5,
            _ => return AIR,
        };
        self.neighbours[ni]
            .as_ref()
            .map(|v| v[idx])
            .unwrap_or(AIR)
    }
}
