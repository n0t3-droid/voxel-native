//! Voxel-Native - native voxel engine, Rust + Bevy + wgpu.
//! Successor to R93G (https://github.com/n0t3-droid/N5).

mod blocks;
mod chunk;
mod daynight;
mod mesher;
mod player;
mod settings;
mod terrain;
mod world;

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Voxel-Native (R93G successor)".into(),
                resolution: (1280.0, 720.0).into(),
                present_mode: bevy::window::PresentMode::AutoNoVsync,
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(Color::srgb(0.53, 0.80, 0.98)))
        .add_plugins((
            settings::SettingsPlugin,
            world::WorldPlugin,
            player::PlayerPlugin,
            daynight::DayNightPlugin,
        ))
        .add_systems(Startup, print_controls)
        .run();
}

fn print_controls() {
    info!("-------- Voxel-Native controls --------");
    info!("  Click window          : capture mouse");
    info!("  Esc                   : release mouse");
    info!("  WASD                  : move");
    info!("  Space                 : jump / fly up");
    info!("  Shift                 : fly down");
    info!("  Ctrl                  : sprint / fly boost");
    info!("  F                     : toggle fly mode");
    info!("  F5                    : save settings");
    info!("---------------------------------------");
}