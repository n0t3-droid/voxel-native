//! Voxel-Native - native voxel engine, Rust + Bevy + wgpu.
//! Successor to R93G (https://github.com/n0t3-droid/N5).

mod blocks;
mod chunk;
mod daynight;
mod editor;
mod hud;
mod menu;
mod mesher;
mod player;
mod settings;
mod terrain;
mod weather;
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
            weather::WeatherPlugin,
            hud::HudPlugin,
            editor::EditorPlugin,
            menu::MenuPlugin,
        ))
        .add_systems(Startup, print_controls)
        .run();
}

fn print_controls() {
    info!("-------- Voxel-Native Controls (Minecraft-style) --------");
    info!("  WASD         : move");
    info!("  Space        : jump  (double-tap = toggle fly)");
    info!("  Ctrl         : sprint");
    info!("  Shift        : fly down / sneak");
    info!("  1-9          : hotbar");
    info!("  E            : open inventory");
    info!("  ESC          : pause menu / close overlay");
    info!("  F3           : toggle debug overlay");
    info!("  F2           : screenshot");
    info!("  F5           : save world + settings");
    info!("  LMB          : capture mouse");
    info!("---------------------------------------------------------");
}