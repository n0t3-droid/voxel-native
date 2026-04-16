//! World settings + persistent configuration.
//!
//! All the tunables that `VoxelEngine.tsx` exposed (render distance, FOV,
//! graphics mode, time of day, world seed, ...) live here as a single Resource
//! so they can be tweaked at runtime and persisted to disk with Serde/RON.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub const SAVE_FILE: &str = "voxel-native-save.ron";
pub const SAVES_DIR: &str = "saves";

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

    /// Graphics tier: controls shadow-map resolution, fog, particles.
    pub graphics: GraphicsMode,

    /// Field of view in degrees.
    pub fov_deg: f32,

    /// Weather (rain/snow/fog/wind). See `weather.rs`.
    pub weather: WeatherSettings,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeMode {
    Cycle,
    Fixed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GraphicsMode {
    Fast,
    Balanced,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WeatherPreset {
    Clear,
    LightRain,
    Storm,
    Snow,
    Fog,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WeatherSettings {
    pub preset: WeatherPreset,
    /// 0 = no rain, 1 = heavy rain.
    pub rain_intensity: f32,
    /// 0 = no snow, 1 = heavy snow.
    pub snow_intensity: f32,
    /// 0 = clear air, 1 = dense fog.
    pub fog_density: f32,
    /// Horizontal wind in world units.
    pub wind_x: f32,
    pub wind_z: f32,
}

impl Default for WeatherSettings {
    fn default() -> Self {
        Self {
            preset: WeatherPreset::Clear,
            rain_intensity: 0.0,
            snow_intensity: 0.0,
            fog_density: 0.0,
            wind_x: 0.0,
            wind_z: 0.0,
        }
    }
}

impl WeatherSettings {
    pub fn apply_preset(&mut self, preset: WeatherPreset) {
        self.preset = preset;
        match preset {
            WeatherPreset::Clear => {
                self.rain_intensity = 0.0;
                self.snow_intensity = 0.0;
                self.fog_density = 0.0;
                self.wind_x = 0.0;
                self.wind_z = 0.0;
            }
            WeatherPreset::LightRain => {
                self.rain_intensity = 0.45;
                self.snow_intensity = 0.0;
                self.fog_density = 0.15;
                self.wind_x = 2.0;
                self.wind_z = 1.0;
            }
            WeatherPreset::Storm => {
                self.rain_intensity = 1.0;
                self.snow_intensity = 0.0;
                self.fog_density = 0.35;
                self.wind_x = 6.0;
                self.wind_z = 4.0;
            }
            WeatherPreset::Snow => {
                self.rain_intensity = 0.0;
                self.snow_intensity = 0.8;
                self.fog_density = 0.25;
                self.wind_x = 1.5;
                self.wind_z = -1.0;
            }
            WeatherPreset::Fog => {
                self.rain_intensity = 0.0;
                self.snow_intensity = 0.0;
                self.fog_density = 0.7;
                self.wind_x = 0.3;
                self.wind_z = 0.0;
            }
            WeatherPreset::Custom => {}
        }
    }
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
            time_of_day: 10.0,
            cycle_speed: 0.01,
            graphics: GraphicsMode::Balanced,
            fov_deg: 75.0,
            weather: WeatherSettings::default(),
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

// ============================== Named worlds ==============================

/// Per-world persistent state: the seed, weather, time, and last player
/// position/orientation so loading drops you exactly where you left off.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMeta {
    pub name: String,
    pub seed: u32,
    pub time_of_day: f32,
    pub time_mode: TimeMode,
    pub cycle_speed: f32,
    pub weather: WeatherSettings,
    pub player_pos: [f32; 3],
    pub player_yaw: f32,
    pub player_pitch: f32,
    pub created_epoch: u64,
    pub last_played_epoch: u64,
}

impl WorldMeta {
    pub fn new(name: String, seed: u32) -> Self {
        let now = now_epoch();
        Self {
            name,
            seed,
            time_of_day: 10.0,
            time_mode: TimeMode::Cycle,
            cycle_speed: 0.01,
            weather: WeatherSettings::default(),
            player_pos: [0.0, 140.0, 0.0],
            player_yaw: 0.0,
            player_pitch: -0.15,
            created_epoch: now,
            last_played_epoch: now,
        }
    }
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn saves_dir() -> PathBuf {
    PathBuf::from(SAVES_DIR)
}

pub fn ensure_saves_dir() {
    let _ = fs::create_dir_all(saves_dir());
}

fn sanitize_world_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn world_file(name: &str) -> PathBuf {
    saves_dir().join(format!("{}.ron", sanitize_world_name(name)))
}

pub fn list_worlds() -> Vec<WorldMeta> {
    ensure_saves_dir();
    let mut out = Vec::new();
    let Ok(read) = fs::read_dir(saves_dir()) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "ron").unwrap_or(true) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(meta) = ron::from_str::<WorldMeta>(&text) {
                out.push(meta);
            }
        }
    }
    out.sort_by(|a, b| b.last_played_epoch.cmp(&a.last_played_epoch));
    out
}

pub fn save_world(meta: &WorldMeta) {
    ensure_saves_dir();
    let path = world_file(&meta.name);
    match ron::ser::to_string_pretty(meta, ron::ser::PrettyConfig::default()) {
        Ok(text) => match fs::write(&path, text) {
            Ok(_) => info!("Saved world '{}' to {}", meta.name, path.display()),
            Err(e) => warn!("Failed to write world file: {e}"),
        },
        Err(e) => warn!("Failed to serialise world: {e}"),
    }
}

pub fn delete_world(name: &str) {
    let path = world_file(name);
    match fs::remove_file(&path) {
        Ok(_) => info!("Deleted world '{}'", name),
        Err(e) => warn!("Failed to delete world '{}': {e}", name),
    }
}

/// The currently active world, inserted once the user picks / creates one.
#[derive(Resource, Debug, Clone)]
pub struct ActiveWorld {
    pub meta: WorldMeta,
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
