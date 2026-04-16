//! World plugin: owns chunks, runs terrain gen + mesh upload.
//!
//! Port target: `lib/voxel/ChunkManager.ts` + `lib/voxel/worker.ts`.
//! For the scaffold we generate a small fixed grid around the origin on
//! startup. Streaming (load/unload around the player, LOD selection,
//! worker-thread meshing) will land in follow-up commits.

use bevy::prelude::*;

use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE};
use crate::mesher::build_mesh;
use crate::terrain::TerrainGenerator;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WorldConfig {
            seed: 12345,
            initial_radius: 3,
            vertical_chunks: 6, // 0..=5 covers y in 0..96
        })
        .add_systems(Startup, spawn_initial_chunks);
    }
}

#[derive(Resource)]
pub struct WorldConfig {
    pub seed: u32,
    /// Radius in chunk columns around origin to spawn at startup.
    pub initial_radius: i32,
    /// Number of vertical chunks (each 16 blocks tall).
    pub vertical_chunks: i32,
}

fn spawn_initial_chunks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    cfg: Res<WorldConfig>,
) {
    let gen = TerrainGenerator::new(cfg.seed);

    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        reflectance: 0.05,
        ..default()
    });

    for cx in -cfg.initial_radius..=cfg.initial_radius {
        for cz in -cfg.initial_radius..=cfg.initial_radius {
            for cy in 0..cfg.vertical_chunks {
                let pos = ChunkPos { x: cx, y: cy, z: cz };
                let mut chunk = Chunk::new(pos);
                gen.generate(&mut chunk);

                let mesh = build_mesh(&chunk);
                let mesh_handle = meshes.add(mesh);

                commands.spawn(PbrBundle {
                    mesh: mesh_handle,
                    material: material.clone(),
                    transform: Transform::from_xyz(
                        (cx * CHUNK_SIZE as i32) as f32,
                        (cy * CHUNK_SIZE as i32) as f32,
                        (cz * CHUNK_SIZE as i32) as f32,
                    ),
                    ..default()
                });
            }
        }
    }

    info!(
        "World: spawned {}x{} columns x {} vertical chunks (seed={})",
        cfg.initial_radius * 2 + 1,
        cfg.initial_radius * 2 + 1,
        cfg.vertical_chunks,
        cfg.seed
    );
}
