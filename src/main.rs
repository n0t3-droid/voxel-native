//! Voxel-Native — successor to R93G.
//!
//! Native voxel engine built with Rust + Bevy (wgpu: Vulkan/DX12/Metal).
//! Module layout mirrors the concepts from R93G so the port stays readable:
//!   - `blocks`    : block types + palette (from lib/voxel/blocks.ts)
//!   - `chunk`     : chunk data (16x16x16) + storage (from lib/voxel/world.ts)
//!   - `terrain`   : noise-based world generation (from lib/voxel/terrain.ts)
//!   - `mesher`    : greedy-ish meshing into Bevy meshes (from mesher.ts)
//!   - `world`     : chunk streaming + LOD policy (from ChunkManager.ts)
//!   - `player`    : first-person camera + controls (from Player.tsx)

mod blocks;
mod chunk;
mod mesher;
mod player;
mod terrain;
mod world;

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Voxel-Native (R93G successor)".into(),
                resolution: (1280.0, 720.0).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.53, 0.80, 0.98)))
        .add_plugins((world::WorldPlugin, player::PlayerPlugin))
        .add_systems(Startup, setup_scene)
        .run();
}

/// Basic scene setup: sunlight + ambient. Sky/day-night will move into a
/// dedicated module later (equivalent of R93G's `DayNightCycle`).
fn setup_scene(mut commands: Commands) {
    commands.insert_resource(AmbientLight {
        color: Color::WHITE,
        brightness: 300.0,
    });

    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            illuminance: 10_000.0,
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_xyz(50.0, 100.0, 50.0).looking_at(Vec3::ZERO, Vec3::Y),
        ..default()
    });
}
