//! World plugin — owns the chunk map, streams chunks around the player,
//! and schedules terrain + meshing work with a per-frame budget so the
//! frame rate never tanks when the player sprints through fresh terrain.
//!
//! Port target: `lib/voxel/ChunkManager.ts` + `lib/voxel/worker.ts`.

use ahash::AHashMap;
use bevy::prelude::*;

use crate::blocks::{voxel_is_solid, Voxel, AIR};
use crate::chunk::{world_to_chunk, Chunk, ChunkPos, CHUNK_SIZE_I};
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
    settings: Res<WorldSettings>,
    pending: Res<crate::menu::PendingWorldLoad>,
    mut commands: Commands,
) {
    if !pending.0 {
        return;
    }
    world.generator = TerrainGenerator::new(settings.seed);
    world.chunks.clear();
    for (_, entity) in streamer.entities.drain() {
        commands.entity(entity).despawn_recursive();
    }
}

#[derive(Resource)]
pub struct VoxelWorld {
    pub chunks: AHashMap<ChunkPos, Chunk>,
    pub generator: TerrainGenerator,
}

impl VoxelWorld {
    pub fn new() -> Self {
        Self {
            chunks: AHashMap::new(),
            generator: TerrainGenerator::new(12345),
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
}

/// Tracks which chunk entities are currently spawned so we can despawn them
/// when they stream out of range.
#[derive(Resource, Default)]
pub struct ChunkStreamer {
    pub entities: AHashMap<ChunkPos, Entity>,
    pub material: Option<Handle<StandardMaterial>>,
    pub water_material: Option<Handle<StandardMaterial>>,
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

/// Load chunks inside `render_distance` of the player (measured in chunks
/// on the X/Z plane) and unload any that drift outside retention range.
/// Also applies a per-frame budget so terrain gen can't stall the frame.
fn stream_chunks(
    anchors: Query<&Transform, With<ChunkAnchor>>,
    settings: Res<WorldSettings>,
    mut world: ResMut<VoxelWorld>,
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

    let rd = settings.render_distance as i32;
    let rd2 = rd * rd;
    let retain = (settings.render_distance + 1) as i32;
    let retain2 = retain * retain;
    let vertical = settings.vertical_chunks as i32;

    // 1. Unload chunks outside the retention radius.
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

    // 2. Load missing chunks near the player, nearest-first, within budget.
    let mut budget = settings.chunks_per_frame;
    let mut wanted: Vec<(i32, ChunkPos)> = Vec::new();
    for dx in -rd..=rd {
        for dz in -rd..=rd {
            let d2 = dx * dx + dz * dz;
            if d2 > rd2 {
                continue;
            }
            for cy in 0..vertical {
                let cp = ChunkPos::new(pcx + dx, cy, pcz + dz);
                if !world.chunks.contains_key(&cp) {
                    wanted.push((d2, cp));
                }
            }
        }
    }
    wanted.sort_unstable_by_key(|(d, _)| *d);

    // Clone the generator handle so we can iterate without borrowing world twice.
    let gen_seed = world.generator.seed;
    let gen = TerrainGenerator::new(gen_seed);

    for (_d, pos) in wanted {
        if budget == 0 {
            break;
        }
        let mut chunk = Chunk::new(pos);
        gen.generate(&mut chunk);
        world.chunks.insert(pos, chunk);
        budget -= 1;
    }
}

/// Re-mesh every chunk marked dirty (only if all 4 cardinal neighbours on
/// X/Z are loaded, to avoid seam artefacts). Uses a per-frame budget so a
/// flood of dirty chunks can't freeze the game.
fn mesh_dirty_chunks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    settings: Res<WorldSettings>,
) {
    let Some(material) = streamer.material.clone() else {
        return;
    };

    // Collect candidate dirty chunks first (so we don't mutate while iterating).
    let mut candidates: Vec<ChunkPos> = world
        .chunks
        .iter()
        .filter_map(|(p, c)| if c.dirty { Some(*p) } else { None })
        .collect();
    candidates.sort_unstable_by_key(|p| p.x * p.x + p.z * p.z);

    let mut budget = settings.meshes_per_frame;

    for pos in candidates {
        if budget == 0 {
            break;
        }

        // Require 4 horizontal neighbours loaded so chunk seams don't flicker.
        let neighbours = [
            ChunkPos::new(pos.x + 1, pos.y, pos.z),
            ChunkPos::new(pos.x - 1, pos.y, pos.z),
            ChunkPos::new(pos.x, pos.y, pos.z + 1),
            ChunkPos::new(pos.x, pos.y, pos.z - 1),
        ];
        if !neighbours.iter().all(|n| world.chunks.contains_key(n)) {
            continue;
        }

        let mesh = {
            let world_ref = &*world;
            build_mesh(pos, |wx, wy, wz| world_ref.voxel_at(wx, wy, wz))
        };

        // Mark the source chunk clean.
        if let Some(c) = world.chunks.get_mut(&pos) {
            c.dirty = false;
        }

        let handle = meshes.add(mesh);
        let (ox, oy, oz) = pos.origin();
        let transform = Transform::from_xyz(ox as f32, oy as f32, oz as f32);

        // Replace the old entity (if any) with a fresh one.
        if let Some(prev) = streamer.entities.remove(&pos) {
            commands.entity(prev).despawn_recursive();
        }
        let entity = commands
            .spawn(PbrBundle {
                mesh: handle,
                material: material.clone(),
                transform,
                ..default()
            })
            .id();
        streamer.entities.insert(pos, entity);

        budget -= 1;
    }

    // Clean up mesh entities whose chunk has streamed out.
    let mut orphaned = Vec::new();
    for (pos, entity) in streamer.entities.iter() {
        if !world.chunks.contains_key(pos) {
            orphaned.push((*pos, *entity));
        }
    }
    for (pos, entity) in orphaned {
        commands.entity(entity).despawn_recursive();
        streamer.entities.remove(&pos);
    }
}
