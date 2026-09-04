//! World settings + persistent configuration.
//!
//! All the tunables that `VoxelEngine.tsx` exposed (render distance, FOV,
//! graphics mode, time of day, world seed, ...) live here as a single Resource
//! so they can be tweaked at runtime and persisted to disk with Serde/RON.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use crate::bots::BotWorldSave;
use crate::neurocore::RuntimeProfile;

#[cfg(not(target_arch = "wasm32"))]
pub const SAVE_FILE: &str = "voxel-native-save.ron";
#[cfg(not(target_arch = "wasm32"))]
pub const SAVES_DIR: &str = "saves";
#[cfg(target_arch = "wasm32")]
const WEB_SETTINGS_KEY: &str = "voxel_native.settings";
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_MANIFEST_KEY: &str = "voxel_native.worlds";
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_PREFIX: &str = "voxel_native.world.";

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

    /// Maximum finished meshes uploaded to the GPU per frame. The mesh
    /// upload (mesh.add() + commands.spawn()) happens on the main thread,
    /// so capping this avoids frame spikes when many tasks finish at once.
    #[serde(default = "default_mesh_applies_per_frame")]
    pub mesh_applies_per_frame: u32,

    /// Maximum number of background terrain + meshing tasks in flight at
    /// any given moment. Higher = uses more cores = loads further faster.
    #[serde(default = "default_in_flight_terrain")]
    pub max_in_flight_terrain: u32,
    #[serde(default = "default_in_flight_meshes")]
    pub max_in_flight_meshes: u32,

    /// When `true`, piloting a shuttle spawns orbiting drones over time
    /// (infinite skirmish, paced for fun). When `false`, no drones — free
    /// flight and placement without combat. Persisted; default off.
    #[serde(default)]
    pub ship_skirmish_ai: bool,

    /// Adaptive runtime governor. These are user preferences; NeuroCore
    /// publishes the effective frame-by-frame budget separately.
    #[serde(default = "default_neurocore_enabled")]
    pub neurocore_enabled: bool,
    #[serde(default)]
    pub runtime_profile: RuntimeProfile,
    #[serde(default = "default_target_fps")]
    pub target_fps: f32,

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

    /// Global art direction. Defaults to natural terrain; old neon-showcase
    /// saves still parse, but startup normalization moves the live engine back
    /// to the grounded world profile.
    #[serde(default = "default_visual_preset")]
    pub visual_preset: VisualPreset,

    /// Admin / cheat toggles. Persisted next to the rest of the
    /// settings so a player who unlocked admin mode keeps it across
    /// sessions. Default: admin off, infinite ammo on (preserves
    /// the engine's original "infinite energy cells" behaviour).
    #[serde(default)]
    pub cheats: CheatSettings,

    /// Editor / HUD theme preferences (phosphor colour, scanlines,
    /// click beeps). See [`crate::theme`].
    #[serde(default)]
    pub theme: crate::theme::ThemeSettings,

    /// Gameplay HUD layout. Guided is the default because it keeps a
    /// clear objective and core vitals without exposing raw telemetry.
    #[serde(default)]
    pub hud_profile: HudProfile,

    /// Hidden by default; unlocks exact engine tunables in the Toolbench.
    #[serde(default)]
    pub show_advanced_settings: bool,

    /// Shared motion preference used by animated UI and in-world overlays.
    #[serde(default)]
    pub reduce_motion: bool,

    /// HUD panel opacity, tuned to remain readable over ice, sky and night.
    #[serde(default = "default_hud_panel_opacity")]
    pub hud_panel_opacity: f32,

    /// Teleport bookmarks set from the WELT tab. Each entry stores a
    /// world position + camera yaw/pitch + a short label so the
    /// player can jump between favourite spots instantly. Persisted
    /// so bookmarks survive restarts.
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,

    /// Screen and inventory controls for the two instruction-first
    /// companions.
    #[serde(default)]
    pub companion_ui: CompanionUiSettings,
}

const NORMAL_TIME_OF_DAY: f32 = 12.25;
/// Dusk postcard hour for a freshly created world. Existing saves keep
/// whatever they stored.
const NEW_WORLD_TIME_OF_DAY: f32 = 17.0;

/// Named teleport target stored in [`WorldSettings::bookmarks`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CompanionDockPosition {
    Left,
    Right,
    Bottom,
}

impl Default for CompanionDockPosition {
    fn default() -> Self {
        Self::Left
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompanionUiSettings {
    #[serde(default = "default_show_companion_dock")]
    pub show_companion_dock: bool,
    #[serde(default)]
    pub dock_position: CompanionDockPosition,
    #[serde(default = "default_companion_editor_assist_enabled")]
    pub editor_assist_enabled: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HudProfile {
    Guided,
    Focused,
    Creator,
}

impl Default for HudProfile {
    fn default() -> Self {
        Self::Guided
    }
}

impl HudProfile {
    pub const ALL: [Self; 3] = [Self::Guided, Self::Focused, Self::Creator];

    pub fn label(self) -> &'static str {
        match self {
            Self::Guided => "Guided",
            Self::Focused => "Focused",
            Self::Creator => "Creator",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Guided => "Objective, compass, vitals and map stay visible.",
            Self::Focused => "World-first HUD; details appear only when useful.",
            Self::Creator => "Build state, undo, brush and companion status are prioritized.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldModeCard {
    ExploreFar,
    SmoothBuild,
    FastLaptop,
    Cinematic,
}

impl WorldModeCard {
    pub const ALL: [Self; 4] = [
        Self::ExploreFar,
        Self::SmoothBuild,
        Self::FastLaptop,
        Self::Cinematic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::ExploreFar => "Explore Far",
            Self::SmoothBuild => "Smooth Build",
            Self::FastLaptop => "Fast Laptop",
            Self::Cinematic => "Cinematic",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::ExploreFar => "More distance with balanced streaming.",
            Self::SmoothBuild => "Stable editing with fewer frame spikes.",
            Self::FastLaptop => "Lower distance and smoother frame pacing.",
            Self::Cinematic => "Stronger visuals for screenshots.",
        }
    }
}

impl Default for CompanionUiSettings {
    fn default() -> Self {
        Self {
            show_companion_dock: default_show_companion_dock(),
            dock_position: CompanionDockPosition::default(),
            editor_assist_enabled: default_companion_editor_assist_enabled(),
        }
    }
}

fn default_show_companion_dock() -> bool {
    false
}

fn default_companion_editor_assist_enabled() -> bool {
    false
}

/// Admin-gated gameplay toggles. `admin_mode` is the master gate: when
/// `false`, the in-game UI hides the cheat panel entirely and the
/// keybind to flip individual cheats is ignored. The field values
/// themselves still apply, so a player who enabled infinite ammo and
/// then turned admin mode off still benefits from infinite ammo until
/// they re-enter admin mode and switch it back.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CheatSettings {
    pub admin_mode: bool,
    pub infinite_ammo: bool,
}

impl Default for CheatSettings {
    fn default() -> Self {
        Self {
            admin_mode: false,
            infinite_ammo: true,
        }
    }
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VisualPreset {
    NaturalWorld,
    NeonShuttle,
}

fn default_visual_preset() -> VisualPreset {
    VisualPreset::NaturalWorld
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
            ship_skirmish_ai: false,
            render_distance: 50,
            // 10 × 16 = 160 blocks of streamed height. The frontier hangs
            // sky islands and docking platforms between y=86 and y=152;
            // at the old 8 chunks (128 blocks) the streamer simply never
            // loaded the slab they live in.
            vertical_chunks: 10,
            chunks_per_frame: 10,
            meshes_per_frame: 10,
            mesh_applies_per_frame: default_mesh_applies_per_frame(),
            max_in_flight_terrain: default_in_flight_terrain(),
            max_in_flight_meshes: default_in_flight_meshes(),
            neurocore_enabled: default_neurocore_enabled(),
            runtime_profile: RuntimeProfile::Auto,
            target_fps: default_target_fps(),
            time_mode: TimeMode::Fixed,
            time_of_day: NORMAL_TIME_OF_DAY,
            cycle_speed: 0.01,
            graphics: GraphicsMode::Balanced,
            fov_deg: 78.0,
            weather: WeatherSettings::default(),
            visual_preset: default_visual_preset(),
            cheats: CheatSettings::default(),
            theme: crate::theme::ThemeSettings::default(),
            hud_profile: HudProfile::default(),
            show_advanced_settings: false,
            reduce_motion: false,
            hud_panel_opacity: default_hud_panel_opacity(),
            bookmarks: Vec::new(),
            companion_ui: CompanionUiSettings::default(),
        }
    }
}

fn default_in_flight_terrain() -> u32 {
    // Keep the worker pool busy without flooding CPU caches. Very high
    // in-flight counts filled RD=50 fast, but could cause visible 1-2s
    // stalls on shared-thermal laptops when generation and meshing peaked
    // together.
    (num_threads() as u32).max(4) * 6
}

fn default_in_flight_meshes() -> u32 {
    // Meshing is pure CPU too, but it competes with terrain generation
    // and render prep. A moderate queue feels smoother than a giant wave
    // of completed meshes reaching the main thread at once.
    (num_threads() as u32).max(4) * 5
}

/// Cap mesh uploads per frame to avoid GPU-upload spikes. Each upload
/// allocates a GPU buffer + spawns an entity on the main thread. At
/// RD=64 the disc contains ~13k columns × vertical_chunks chunks. The
/// default favours smooth frame pacing over instant fill-in; NeuroCore can
/// still scale down further under pressure.
fn default_mesh_applies_per_frame() -> u32 {
    8
}

fn default_neurocore_enabled() -> bool {
    true
}

fn default_target_fps() -> f32 {
    60.0
}

fn default_hud_panel_opacity() -> f32 {
    0.72
}

pub(crate) const SAFE_MIN_RENDER_DISTANCE: u32 = 8;
pub(crate) const SAFE_MAX_RENDER_DISTANCE: u32 = 64;
pub(crate) const SAFE_MIN_VERTICAL_CHUNKS: u32 = 4;
pub(crate) const SAFE_MAX_VERTICAL_CHUNKS: u32 = 12;
pub(crate) const SAFE_MIN_CHUNKS_PER_FRAME: u32 = 2;
pub(crate) const SAFE_MAX_CHUNKS_PER_FRAME: u32 = 18;
pub(crate) const SAFE_MIN_MESHES_PER_FRAME: u32 = 2;
pub(crate) const SAFE_MAX_MESHES_PER_FRAME: u32 = 16;
pub(crate) const SAFE_MIN_MESH_APPLIES_PER_FRAME: u32 = 2;
pub(crate) const SAFE_MAX_MESH_APPLIES_PER_FRAME: u32 = 10;
pub(crate) const SAFE_MIN_IN_FLIGHT_TERRAIN: u32 = 24;
pub(crate) const SAFE_MAX_IN_FLIGHT_TERRAIN: u32 = 224;
pub(crate) const SAFE_MIN_IN_FLIGHT_MESHES: u32 = 20;
pub(crate) const SAFE_MAX_IN_FLIGHT_MESHES: u32 = 168;

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

fn num_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

impl WorldSettings {
    pub fn apply_world_mode_card(&mut self, mode: WorldModeCard) {
        match mode {
            WorldModeCard::ExploreFar => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::Auto;
                self.render_distance = 64;
                self.vertical_chunks = 10;
                self.chunks_per_frame = 16;
                self.meshes_per_frame = 14;
                self.mesh_applies_per_frame = 10;
                self.max_in_flight_terrain = 224;
                self.max_in_flight_meshes = 168;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::Balanced;
            }
            WorldModeCard::SmoothBuild => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::Balanced;
                self.render_distance = 40;
                self.vertical_chunks = 10;
                self.chunks_per_frame = 10;
                self.meshes_per_frame = 10;
                self.mesh_applies_per_frame = 8;
                self.max_in_flight_terrain = 128;
                self.max_in_flight_meshes = 96;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::Balanced;
            }
            WorldModeCard::FastLaptop => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::LowSpec;
                self.render_distance = 24;
                // Still the low-spec preset, but one slab taller so the
                // lowest sky islands are inside the streamed volume.
                self.vertical_chunks = 7;
                self.chunks_per_frame = 18;
                self.meshes_per_frame = 16;
                self.mesh_applies_per_frame = 8;
                self.max_in_flight_terrain = 96;
                self.max_in_flight_meshes = 80;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::Fast;
            }
            WorldModeCard::Cinematic => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::Cinematic;
                self.render_distance = 56;
                self.vertical_chunks = 12;
                self.chunks_per_frame = 8;
                self.meshes_per_frame = 8;
                self.mesh_applies_per_frame = 6;
                self.max_in_flight_terrain = 144;
                self.max_in_flight_meshes = 112;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::High;
            }
        }
    }

    /// Keep persisted settings inside a startup-safe envelope. Save files
    /// can outlive engine internals, and a huge old render/streaming budget
    /// should degrade into a cinematic preset instead of hanging the app.
    pub fn normalize_runtime_safety(&mut self) {
        self.render_distance = self
            .render_distance
            .clamp(SAFE_MIN_RENDER_DISTANCE, SAFE_MAX_RENDER_DISTANCE);
        self.vertical_chunks = self
            .vertical_chunks
            .clamp(SAFE_MIN_VERTICAL_CHUNKS, SAFE_MAX_VERTICAL_CHUNKS);
        self.chunks_per_frame = self
            .chunks_per_frame
            .clamp(SAFE_MIN_CHUNKS_PER_FRAME, SAFE_MAX_CHUNKS_PER_FRAME);
        self.meshes_per_frame = self
            .meshes_per_frame
            .clamp(SAFE_MIN_MESHES_PER_FRAME, SAFE_MAX_MESHES_PER_FRAME);
        self.mesh_applies_per_frame = self.mesh_applies_per_frame.clamp(
            SAFE_MIN_MESH_APPLIES_PER_FRAME,
            SAFE_MAX_MESH_APPLIES_PER_FRAME,
        );
        self.max_in_flight_terrain = self
            .max_in_flight_terrain
            .clamp(SAFE_MIN_IN_FLIGHT_TERRAIN, SAFE_MAX_IN_FLIGHT_TERRAIN);
        self.max_in_flight_meshes = self
            .max_in_flight_meshes
            .clamp(SAFE_MIN_IN_FLIGHT_MESHES, SAFE_MAX_IN_FLIGHT_MESHES);

        self.target_fps = finite_or(self.target_fps, default_target_fps()).clamp(30.0, 144.0);
        self.fov_deg = finite_or(self.fov_deg, 78.0).clamp(55.0, 100.0);
        self.time_of_day = finite_or(self.time_of_day, NORMAL_TIME_OF_DAY).rem_euclid(24.0);
        if self.time_mode == TimeMode::Fixed && (self.time_of_day - 21.35).abs() < 0.05 {
            self.time_of_day = NORMAL_TIME_OF_DAY;
        }
        self.cycle_speed = finite_or(self.cycle_speed, 0.01).clamp(0.0, 2.0);
        self.hud_panel_opacity =
            finite_or(self.hud_panel_opacity, default_hud_panel_opacity()).clamp(0.35, 0.92);
        self.theme.style = crate::theme::ThemeStyle::LiquidGlass;
        self.theme.scanlines = false;
        self.visual_preset = VisualPreset::NaturalWorld;
        self.companion_ui.show_companion_dock = false;
        self.companion_ui.editor_assist_enabled = false;

        self.weather.rain_intensity = finite_or(self.weather.rain_intensity, 0.0).clamp(0.0, 1.0);
        self.weather.snow_intensity = finite_or(self.weather.snow_intensity, 0.0).clamp(0.0, 1.0);
        self.weather.fog_density = finite_or(self.weather.fog_density, 0.0).clamp(0.0, 1.0);
        self.weather.wind_x = finite_or(self.weather.wind_x, 0.0).clamp(-12.0, 12.0);
        self.weather.wind_z = finite_or(self.weather.wind_z, 0.0).clamp(-12.0, 12.0);
    }

    pub fn load_or_default() -> Self {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(text) = crate::platform::browser_storage_get(WEB_SETTINGS_KEY) {
                if let Ok(mut settings) = ron::from_str::<WorldSettings>(&text) {
                    settings.normalize_runtime_safety();
                    info!("Loaded browser settings from localStorage");
                    return settings;
                }
                warn!("Browser settings exist but could not be parsed; using defaults.");
            }
            return Self::default();
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Ok(text) = fs::read_to_string(save_path()) {
                if let Ok(mut settings) = ron::from_str::<WorldSettings>(&text) {
                    settings.normalize_runtime_safety();
                    info!("Loaded settings from {}", save_path().display());
                    return settings;
                }
                warn!("Save file exists but could not be parsed; using defaults.");
            }
            // No save file yet — first run. Return defaults; the adapter-
            // detection system in world.rs will downgrade to Fast + RD=14
            // once the Bevy render adapter is available.
            Self::default()
        }
    }

    pub fn save(&self) {
        let mut safe = self.clone();
        safe.normalize_runtime_safety();
        match ron::ser::to_string_pretty(&safe, ron::ser::PrettyConfig::default()) {
            Ok(text) => {
                #[cfg(target_arch = "wasm32")]
                {
                    match crate::platform::browser_storage_set(WEB_SETTINGS_KEY, &text) {
                        Ok(_) => info!("Saved browser settings to localStorage"),
                        Err(e) => warn!("{e}"),
                    }
                }

                #[cfg(not(target_arch = "wasm32"))]
                match atomic_write(&save_path(), &text) {
                    Ok(_) => info!("Saved settings to {}", save_path().display()),
                    Err(e) => warn!("Failed to write save file: {e}"),
                }
            }
            Err(e) => warn!("Failed to serialise settings: {e}"),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn save_path() -> PathBuf {
    // Next to the executable / cargo project root.
    PathBuf::from(SAVE_FILE)
}

// ============================== Named worlds ==============================

/// Mined neon resources persisted per world (HUD counters).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct PlayerMiningSave {
    pub luminite: u64,
    pub magnetite: u64,
    pub iridium: u64,
}

/// Exosuit vitals persisted per world.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SuitVitalsSave {
    pub health: f32,
    pub shield: f32,
    pub oxygen: f32,
    pub laser_drill_charge: f32,
}

impl Default for SuitVitalsSave {
    fn default() -> Self {
        Self {
            health: 100.0,
            shield: 60.0,
            oxygen: 97.0,
            laser_drill_charge: 100.0,
        }
    }
}

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
    #[serde(default)]
    pub ships: Vec<crate::ships::SavedShipInstance>,
    #[serde(default)]
    pub ship_inventory: crate::ships::ShipInventory,
    #[serde(default)]
    pub player_mining: PlayerMiningSave,
    #[serde(default)]
    pub player_suit: SuitVitalsSave,
    #[serde(default)]
    pub bot_world: BotWorldSave,
    #[serde(default)]
    pub world_edit_manifest: WorldEditManifest,
    pub created_epoch: u64,
    pub last_played_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorldEditManifest {
    pub edited_chunks: usize,
    pub last_saved_epoch: u64,
}

impl WorldMeta {
    pub fn new(name: String, seed: u32) -> Self {
        let now = now_epoch();
        let (spawn, yaw, pitch) = crate::terrain::TerrainGenerator::new(seed).scenic_frontier_spawn();
        Self {
            name,
            seed,
            time_of_day: NEW_WORLD_TIME_OF_DAY,
            time_mode: TimeMode::Fixed,
            cycle_speed: 0.01,
            weather: WeatherSettings::default(),
            player_pos: spawn,
            player_yaw: yaw,
            player_pitch: pitch,
            ships: Vec::new(),
            ship_inventory: crate::ships::ShipInventory::default(),
            player_mining: PlayerMiningSave::default(),
            player_suit: SuitVitalsSave::default(),
            bot_world: BotWorldSave::default(),
            world_edit_manifest: WorldEditManifest::default(),
            created_epoch: now,
            last_played_epoch: now,
        }
    }
}

fn now_epoch() -> u64 {
    crate::platform::now_epoch()
}

#[cfg(not(target_arch = "wasm32"))]
fn saves_dir() -> PathBuf {
    PathBuf::from(SAVES_DIR)
}

#[cfg(not(target_arch = "wasm32"))]
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

pub fn world_storage_stem(name: &str) -> String {
    sanitize_world_name(name)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn world_file(name: &str) -> PathBuf {
    saves_dir().join(format!("{}.ron", sanitize_world_name(name)))
}

pub fn list_worlds() -> Vec<WorldMeta> {
    #[cfg(target_arch = "wasm32")]
    {
        return list_browser_worlds();
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
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
}

pub fn save_world(meta: &WorldMeta) {
    match ron::ser::to_string_pretty(meta, ron::ser::PrettyConfig::default()) {
        Ok(text) => {
            #[cfg(target_arch = "wasm32")]
            {
                if let Err(e) =
                    crate::platform::browser_storage_set(&browser_world_key(&meta.name), &text)
                {
                    warn!("{e}");
                    return;
                }
                save_browser_world_manifest_entry(&meta.name);
                info!("Saved browser world '{}' to localStorage", meta.name);
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                ensure_saves_dir();
                let path = world_file(&meta.name);
                match atomic_write(&path, &text) {
                    Ok(_) => info!("Saved world '{}' to {}", meta.name, path.display()),
                    Err(e) => warn!("Failed to write world file: {e}"),
                }
            }
        }
        Err(e) => warn!("Failed to serialise world: {e}"),
    }
}

pub fn save_player_pose_checkpoint(
    active_meta: &WorldMeta,
    settings: &WorldSettings,
    player_pos: [f32; 3],
    player_yaw: f32,
    player_pitch: f32,
    player_mining: PlayerMiningSave,
    player_suit: SuitVitalsSave,
) {
    let mut meta = list_worlds()
        .into_iter()
        .find(|meta| meta.name == active_meta.name)
        .unwrap_or_else(|| active_meta.clone());
    meta.seed = settings.seed;
    meta.time_of_day = settings.time_of_day;
    meta.time_mode = settings.time_mode;
    meta.cycle_speed = settings.cycle_speed;
    meta.weather = settings.weather;
    meta.player_pos = player_pos;
    meta.player_yaw = player_yaw;
    meta.player_pitch = player_pitch;
    meta.player_mining = player_mining;
    meta.player_suit = player_suit;
    meta.last_played_epoch = now_epoch();
    save_world(&meta);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn atomic_write_text(final_path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = final_path.with_extension("ron.tmp");
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp_path, final_path) {
        let _ = fs::write(final_path, text);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

/// Write `text` to `final_path` atomically: serialise to a sibling
/// `.tmp` file, fsync the content, then rename over the target. If the
/// process is killed mid-write we keep the previous good save instead
/// of leaving a half-written file. Falls back to a direct write if the
/// rename cannot be performed (e.g. cross-device on some OSes).
#[cfg(not(target_arch = "wasm32"))]
fn atomic_write(final_path: &std::path::Path, text: &str) -> std::io::Result<()> {
    atomic_write_text(final_path, text)
}

#[cfg(target_arch = "wasm32")]
fn browser_world_key(name: &str) -> String {
    format!("{WEB_WORLD_PREFIX}{}", sanitize_world_name(name))
}

#[cfg(target_arch = "wasm32")]
fn browser_world_manifest() -> Vec<String> {
    crate::platform::browser_storage_get(WEB_WORLD_MANIFEST_KEY)
        .and_then(|text| ron::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

#[cfg(target_arch = "wasm32")]
fn write_browser_world_manifest(names: &[String]) {
    if let Ok(text) = ron::ser::to_string(names) {
        if let Err(e) = crate::platform::browser_storage_set(WEB_WORLD_MANIFEST_KEY, &text) {
            warn!("{e}");
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn save_browser_world_manifest_entry(name: &str) {
    let clean = sanitize_world_name(name);
    let mut names = browser_world_manifest();
    if !names.iter().any(|existing| existing == &clean) {
        names.push(clean);
        names.sort();
    }
    write_browser_world_manifest(&names);
}

#[cfg(target_arch = "wasm32")]
fn remove_browser_world_manifest_entry(name: &str) {
    let clean = sanitize_world_name(name);
    let mut names = browser_world_manifest();
    names.retain(|existing| existing != &clean);
    write_browser_world_manifest(&names);
}

#[cfg(target_arch = "wasm32")]
fn list_browser_worlds() -> Vec<WorldMeta> {
    let mut out = Vec::new();
    for name in browser_world_manifest() {
        let Some(text) = crate::platform::browser_storage_get(&browser_world_key(&name)) else {
            continue;
        };
        if let Ok(meta) = ron::from_str::<WorldMeta>(&text) {
            out.push(meta);
        }
    }
    out.sort_by(|a, b| b.last_played_epoch.cmp(&a.last_played_epoch));
    out
}

pub fn delete_world(name: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        let key = browser_world_key(name);
        if let Err(e) = crate::platform::browser_storage_remove(&key) {
            warn!("{e}");
        }
        remove_browser_world_manifest_entry(name);
        info!("Deleted browser world '{}'", name);
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = world_file(name);
        match fs::remove_file(&path) {
            Ok(_) => info!("Deleted world '{}'", name),
            Err(e) => warn!("Failed to delete world '{}': {e}", name),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_world_meta_without_ship_fields_loads() {
        let text = r#"(
            name: "legacy",
            seed: 42,
            time_of_day: 10.0,
            time_mode: Cycle,
            cycle_speed: 0.01,
            weather: (
                preset: Clear,
                rain_intensity: 0.0,
                snow_intensity: 0.0,
                fog_density: 0.0,
                wind_x: 0.0,
                wind_z: 0.0,
            ),
            player_pos: (0.0, 140.0, 0.0),
            player_yaw: 0.0,
            player_pitch: -0.15,
            created_epoch: 1,
            last_played_epoch: 1,
        )"#;
        let meta: WorldMeta = ron::from_str(text).unwrap();
        assert!(meta.ships.is_empty());
        assert_eq!(
            meta.ship_inventory.unlocked.len(),
            crate::ships::ShipKind::ALL.len()
        );
        assert!(meta.bot_world.agents.is_empty());
        assert_eq!(meta.world_edit_manifest.edited_chunks, 0);
    }

    #[test]
    fn old_settings_load_with_companion_ui_defaults() {
        let text = r#"(
            seed: 42,
            render_distance: 32,
            vertical_chunks: 8,
            chunks_per_frame: 16,
            meshes_per_frame: 16,
            time_mode: Fixed,
            time_of_day: 21.0,
            cycle_speed: 0.01,
            graphics: Balanced,
            fov_deg: 78.0,
            weather: (
                preset: Clear,
                rain_intensity: 0.0,
                snow_intensity: 0.0,
                fog_density: 0.0,
                wind_x: 0.0,
                wind_z: 0.0,
            ),
        )"#;
        let settings: WorldSettings = ron::from_str(text).unwrap();
        assert!(!settings.companion_ui.show_companion_dock);
        assert!(!settings.companion_ui.editor_assist_enabled);
        assert_eq!(
            settings.companion_ui.dock_position,
            CompanionDockPosition::Left
        );
        assert_eq!(settings.hud_profile, HudProfile::Guided);
        assert!(!settings.show_advanced_settings);
        assert!(!settings.reduce_motion);
        assert!((settings.hud_panel_opacity - 0.72).abs() < f32::EPSILON);
        assert_eq!(settings.theme.style, crate::theme::ThemeStyle::LiquidGlass);
        assert_eq!(settings.theme.density, crate::theme::UiDensity::Comfortable);
        assert_eq!(settings.visual_preset, VisualPreset::NaturalWorld);
    }

    #[test]
    fn runtime_safety_clamps_overdriven_saved_settings() {
        let mut settings = WorldSettings::default();
        settings.render_distance = 96;
        settings.vertical_chunks = 16;
        settings.chunks_per_frame = 64;
        settings.meshes_per_frame = 64;
        settings.mesh_applies_per_frame = 64;
        settings.max_in_flight_terrain = 512;
        settings.max_in_flight_meshes = 384;
        settings.target_fps = 500.0;
        settings.fov_deg = 150.0;
        settings.time_of_day = 49.5;
        settings.cycle_speed = 9.0;
        settings.hud_panel_opacity = 1.0;
        settings.weather.rain_intensity = 2.0;
        settings.weather.snow_intensity = 2.0;
        settings.weather.fog_density = 2.0;
        settings.weather.wind_x = 80.0;
        settings.weather.wind_z = -80.0;
        settings.theme.style = crate::theme::ThemeStyle::ClassicCrt;
        settings.theme.scanlines = true;
        settings.visual_preset = VisualPreset::NeonShuttle;
        settings.companion_ui.show_companion_dock = true;
        settings.companion_ui.editor_assist_enabled = true;

        settings.normalize_runtime_safety();

        assert_eq!(settings.render_distance, SAFE_MAX_RENDER_DISTANCE);
        assert_eq!(settings.vertical_chunks, SAFE_MAX_VERTICAL_CHUNKS);
        assert_eq!(settings.chunks_per_frame, SAFE_MAX_CHUNKS_PER_FRAME);
        assert_eq!(settings.meshes_per_frame, SAFE_MAX_MESHES_PER_FRAME);
        assert_eq!(
            settings.mesh_applies_per_frame,
            SAFE_MAX_MESH_APPLIES_PER_FRAME
        );
        assert_eq!(settings.max_in_flight_terrain, SAFE_MAX_IN_FLIGHT_TERRAIN);
        assert_eq!(settings.max_in_flight_meshes, SAFE_MAX_IN_FLIGHT_MESHES);
        assert_eq!(settings.target_fps, 144.0);
        assert_eq!(settings.fov_deg, 100.0);
        assert!((0.0..24.0).contains(&settings.time_of_day));
        assert_eq!(settings.cycle_speed, 2.0);
        assert_eq!(settings.hud_panel_opacity, 0.92);
        assert_eq!(settings.weather.rain_intensity, 1.0);
        assert_eq!(settings.weather.snow_intensity, 1.0);
        assert_eq!(settings.weather.fog_density, 1.0);
        assert_eq!(settings.weather.wind_x, 12.0);
        assert_eq!(settings.weather.wind_z, -12.0);
        assert_eq!(settings.theme.style, crate::theme::ThemeStyle::LiquidGlass);
        assert!(!settings.theme.scanlines);
        assert_eq!(settings.visual_preset, VisualPreset::NaturalWorld);
        assert!(!settings.companion_ui.show_companion_dock);
        assert!(!settings.companion_ui.editor_assist_enabled);
    }

    #[test]
    fn world_mode_cards_apply_friendly_presets() {
        let mut settings = WorldSettings::default();
        settings.apply_world_mode_card(WorldModeCard::FastLaptop);
        assert_eq!(settings.graphics, GraphicsMode::Fast);
        assert_eq!(settings.runtime_profile, RuntimeProfile::LowSpec);
        assert!(settings.render_distance <= 24);
        assert_eq!(settings.vertical_chunks, 7);

        settings.apply_world_mode_card(WorldModeCard::Cinematic);
        assert_eq!(settings.graphics, GraphicsMode::High);
        assert_eq!(settings.runtime_profile, RuntimeProfile::Cinematic);
        assert!(settings.render_distance >= 56);
        assert!(settings.mesh_applies_per_frame <= 8);
    }
}
