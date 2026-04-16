//! World settings + persistent configuration.
//!
//! All the tunables that `VoxelEngine.tsx` exposed (render distance, FOV,
//! graphics mode, time of day, world seed, …) live here as a single Resource
//! so they can be tweaked at runtime and persisted to disk with Serde/RON.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const SAVE_FILE: &str = "voxel-native-save.ron";

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct WorldSettings {
    /// Deterministic seed for every noise layer in `terrain.rs`.
    pub seed: u32,

    /// Chunks to load on each axis from the player on the X/Z plane.
    pub render_distance: u32,

    /// Number of vertical chunks (each 16 blocks tall). 8 = world height 128.
    pub vertical_chunks: u32,

    /// Hard budget per frame so streaming can't freeze the game.
    pub chunks_per_frame: u32,
    pub meshes_per_frame: u32,

    /// Either "cycle" (auto-advance) or a fixed time (0..24).
    pub time_mode: TimeMode,
    pub time_of_day: f32,
    pub cycle_speed: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum TimeMode {
    Cycle,
    Fixed,
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            seed: 12345,
            render_distance: 6,
            vertical_chunks: 8,
            chunks_per_frame: 6,
            meshes_per_frame: 4,
            time_mode: TimeMode::Cycle,
            time_of_day: 9.0,
            cycle_speed: 0.01,
        }
    }
}

impl WorldSettings {
    pub fn load_or_default() -> Self {
        if let Ok(text) = fs::read_to_string(save_path()) {
            if let Ok(settings) = ron::from_str::<WorldSettings>(&text) {
                info!("Loaded settings from {}", save_path().display());
                return settings;
            }
            warn!("Save file exists but could not be parsed; using defaults.");
        }
        Self::default()
    }

    pub fn save(&self) {
        match ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()) {
            Ok(text) => match fs::write(save_path(), text) {
                Ok(_) => info!("Saved settings to {}", save_path().display()),
                Err(e) => warn!("Failed to write save file: {e}"),
            },
            Err(e) => warn!("Failed to serialise settings: {e}"),
        }
    }
}

fn save_path() -> PathBuf {
    // Next to the executable / cargo project root.
    PathBuf::from(SAVE_FILE)
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WorldSettings::load_or_default())
            .add_systems(Update, save_on_keypress);
    }
}

fn save_on_keypress(keys: Res<ButtonInput<KeyCode>>, settings: Res<WorldSettings>) {
    // F5 = save now. A full game would save on Quit too.
    if keys.just_pressed(KeyCode::F5) {
        settings.save();
    }
}
