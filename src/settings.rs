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
use std::path::{Path, PathBuf};

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
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_V2_MANIFEST_KEY: &str = "voxel_native.worlds.v2";
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_V2_PREFIX: &str = "voxel_native.world.v2.";
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_V3_MANIFEST_KEY: &str = "voxel_native.worlds.v3";
#[cfg(target_arch = "wasm32")]
const WEB_WORLD_V3_PREFIX: &str = "voxel_native.world.v3.";

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct WorldSettings {
    /// Deterministic seed for every noise layer in `terrain.rs`.
    pub seed: u32,

    /// Persistent semantic identity of the generated world. This is kept
    /// separate from `VisualPreset`: a UI/ship presentation must never
    /// silently rewrite terrain, while an Astral Frontier world must reopen
    /// with the same provinces on every machine.
    #[serde(default)]
    pub world_profile: WorldProfile,

    /// Immutable byte-generation grammar of the active named world.
    ///
    /// The explicit legacy default is intentionally different from
    /// [`TerrainGrammarVersion::default`]: an old settings file that predates
    /// this field can only describe V1 bytes, while a genuinely new settings
    /// resource starts on [`TerrainGrammarVersion::CURRENT`].
    #[serde(default = "legacy_terrain_grammar")]
    pub terrain_grammar: TerrainGrammarVersion,

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

    /// Generated scenery density and tree scale. This is intentionally a
    /// simple tier so the UI can expose rich world controls without making
    /// low-end machines pay for decorative voxels they do not need.
    #[serde(default)]
    pub scenery_quality: SceneryQuality,

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SceneryQuality {
    Off,
    Lean,
    Balanced,
    Lush,
}

/// High-level terrain contract stored in every named world.
///
/// `Natural` selects the Earth-like profile. The separate persisted
/// [`TerrainGrammarVersion`] determines whether its established V1 bytes or
/// the current V3 bank grammar is authoritative. `AstralFrontier` activates
/// the authored canyon, plateau, alien-reef, crystal-spire and volcanic
/// provinces as one deliberately composed world.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum WorldProfile {
    #[default]
    Natural,
    AstralFrontier,
}

/// Persisted contract for all procedural bytes that make up an unedited
/// world.
///
/// Variant names are serialized directly by Serde. Consequently an unknown
/// future variant fails deserialization instead of being guessed or silently
/// normalized to a different terrain grammar.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TerrainGrammarVersion {
    V1,
    V2,
    V3,
}

impl TerrainGrammarVersion {
    pub const CURRENT: Self = Self::V3;
}

impl Default for TerrainGrammarVersion {
    fn default() -> Self {
        Self::CURRENT
    }
}

const fn legacy_terrain_grammar() -> TerrainGrammarVersion {
    TerrainGrammarVersion::V1
}

/// Exact, immutable identity of generated terrain and its decoration budget.
/// Any cache, edit store, far-field worker, or QA result that can outlive a
/// generator instance should bind itself to this whole value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct WorldGenerationIdentity {
    pub seed: u32,
    pub world_profile: WorldProfile,
    pub scenery_quality: SceneryQuality,
    pub terrain_grammar: TerrainGrammarVersion,
}

impl WorldProfile {
    pub const ALL: [Self; 2] = [Self::AstralFrontier, Self::Natural];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Natural => "NATURAL WORLD",
            Self::AstralFrontier => "ASTRAL FRONTIER",
        }
    }

    pub const fn detail(self) -> &'static str {
        match self {
            Self::Natural => "Grounded rivers, forests, karst and mountains.",
            Self::AstralFrontier => {
                "Layered canyons, green plateaus, crystal routes and volcanic rifts."
            }
        }
    }
}

impl Default for SceneryQuality {
    fn default() -> Self {
        Self::Balanced
    }
}

impl SceneryQuality {
    pub const ALL: [Self; 4] = [Self::Off, Self::Lean, Self::Balanced, Self::Lush];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Lean => "Lean",
            Self::Balanced => "Balanced",
            Self::Lush => "Lush",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Off => "No generated trees/flora for weakest PCs.",
            Self::Lean => "Sparse trees, clear visibility, fastest terrain.",
            Self::Balanced => "Readable forests without crowding the world.",
            Self::Lush => "Bigger bonsai and blossom silhouettes for scenic worlds.",
        }
    }

    pub fn density_scale(self) -> f64 {
        match self {
            Self::Off => 0.0,
            Self::Lean => 0.45,
            Self::Balanced => 1.0,
            Self::Lush => 1.65,
        }
    }

    pub fn height_bonus(self) -> i32 {
        match self {
            Self::Off | Self::Lean => 0,
            Self::Balanced => 1,
            Self::Lush => 3,
        }
    }
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
            world_profile: WorldProfile::Natural,
            terrain_grammar: TerrainGrammarVersion::CURRENT,
            ship_skirmish_ai: false,
            render_distance: 50,
            vertical_chunks: 8,
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
            scenery_quality: SceneryQuality::Balanced,
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
    /// Exact immutable identity currently selected by the settings resource.
    /// Persisted named worlds remain authoritative when one is active.
    pub const fn generation_identity(&self) -> WorldGenerationIdentity {
        WorldGenerationIdentity {
            seed: self.seed,
            world_profile: self.world_profile,
            scenery_quality: self.scenery_quality,
            terrain_grammar: self.terrain_grammar,
        }
    }

    /// Legacy Neon Shuttle remains a presentation preset, but it now gets the
    /// terrain it always expected instead of searching a Natural world for
    /// provinces that can never exist there.
    pub const fn effective_world_profile(&self) -> WorldProfile {
        if matches!(self.visual_preset, VisualPreset::NeonShuttle) {
            WorldProfile::AstralFrontier
        } else {
            self.world_profile
        }
    }

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
                self.scenery_quality = SceneryQuality::Lean;
            }
            WorldModeCard::SmoothBuild => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::Balanced;
                self.render_distance = 40;
                self.vertical_chunks = 8;
                self.chunks_per_frame = 10;
                self.meshes_per_frame = 10;
                self.mesh_applies_per_frame = 8;
                self.max_in_flight_terrain = 128;
                self.max_in_flight_meshes = 96;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::Balanced;
                self.scenery_quality = SceneryQuality::Balanced;
            }
            WorldModeCard::FastLaptop => {
                self.neurocore_enabled = true;
                self.runtime_profile = RuntimeProfile::LowSpec;
                self.render_distance = 22;
                self.vertical_chunks = 6;
                self.chunks_per_frame = 5;
                self.meshes_per_frame = 5;
                self.mesh_applies_per_frame = 3;
                self.max_in_flight_terrain = 56;
                self.max_in_flight_meshes = 40;
                self.target_fps = 60.0;
                self.graphics = GraphicsMode::Fast;
                self.scenery_quality = SceneryQuality::Lean;
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
                self.scenery_quality = SceneryQuality::Lush;
            }
        }
    }

    pub fn apply_zen_garden_look(&mut self) {
        self.graphics = GraphicsMode::High;
        self.scenery_quality = SceneryQuality::Lush;
        self.time_mode = TimeMode::Fixed;
        self.time_of_day = 14.15;
        self.weather.apply_preset(WeatherPreset::Clear);
        self.weather.fog_density = 0.04;
        self.weather.wind_x = 1.4;
        self.weather.wind_z = 0.8;
        self.theme.style = crate::theme::ThemeStyle::LiquidGlass;
        self.theme.color = crate::theme::ThemeColor::Sakura;
        self.theme.scanlines = false;
        self.hud_panel_opacity = 0.74;
        self.visual_preset = VisualPreset::NaturalWorld;
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
        self.weather.rain_intensity = finite_or(self.weather.rain_intensity, 0.0).clamp(0.0, 1.0);
        self.weather.snow_intensity = finite_or(self.weather.snow_intensity, 0.0).clamp(0.0, 1.0);
        self.weather.fog_density = finite_or(self.weather.fog_density, 0.0).clamp(0.0, 1.0);
        self.weather.wind_x = finite_or(self.weather.wind_x, 0.0).clamp(-12.0, 12.0);
        self.weather.wind_z = finite_or(self.weather.wind_z, 0.0).clamp(-12.0, 12.0);
    }

    pub fn load_or_default() -> Self {
        let qa_enabled = crate::qa::qa_enabled();
        let isolated_observer_enabled = crate::agent_control::isolated_observer_enabled();
        if !settings_persistence_allowed(qa_enabled, isolated_observer_enabled) {
            info!(
                "Isolated automation mode: using defaults without reading persistent settings \
                 (qa={qa_enabled}, agent_observer={isolated_observer_enabled})"
            );
            return Self::default();
        }

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
        let qa_enabled = crate::qa::qa_enabled();
        let isolated_observer_enabled = crate::agent_control::isolated_observer_enabled();
        if !settings_persistence_allowed(qa_enabled, isolated_observer_enabled) {
            info!(
                "Isolated automation mode: skipped persistent settings write \
                 (qa={qa_enabled}, agent_observer={isolated_observer_enabled})"
            );
            return;
        }

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

#[inline]
fn settings_persistence_allowed(qa_enabled: bool, isolated_observer_enabled: bool) -> bool {
    !qa_enabled && !isolated_observer_enabled
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
    /// Added after the original save format. Old worlds deserialize as
    /// `Natural`, preserving their terrain exactly.
    #[serde(default)]
    pub world_profile: WorldProfile,
    /// Exact byte-generation grammar. Absence means V1 because every world
    /// written before this field existed was generated with V1 terrain.
    #[serde(default = "legacy_terrain_grammar")]
    pub terrain_grammar: TerrainGrammarVersion,
    pub time_of_day: f32,
    pub time_mode: TimeMode,
    pub cycle_speed: f32,
    pub weather: WeatherSettings,
    #[serde(default)]
    pub scenery_quality: SceneryQuality,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WorldEditManifest {
    pub edited_chunks: usize,
    pub last_saved_epoch: u64,
}

impl WorldMeta {
    pub fn new(name: String, seed: u32) -> Self {
        Self::new_with_profile(name, seed, WorldProfile::Natural)
    }

    pub fn new_with_profile(name: String, seed: u32, world_profile: WorldProfile) -> Self {
        Self::new_with_identity(
            name,
            WorldGenerationIdentity {
                seed,
                world_profile,
                scenery_quality: SceneryQuality::Lush,
                terrain_grammar: TerrainGrammarVersion::CURRENT,
            },
        )
    }

    pub fn new_with_identity(name: String, identity: WorldGenerationIdentity) -> Self {
        let now = now_epoch();
        let generator = crate::terrain::TerrainGenerator::from_identity(identity);
        let spawn = match identity.world_profile {
            WorldProfile::Natural => generator
                .find_natural_spawn(0, 0, 4096)
                .map(|p| [p.x as f32 + 0.5, p.y as f32, p.z as f32 + 0.5]),
            WorldProfile::AstralFrontier => generator
                .find_neon_showcase_spawn(0, 0, 4096)
                .map(|p| [p.x as f32 + 0.5, p.y as f32, p.z as f32 + 0.5])
                .or_else(|| {
                    generator
                        .find_natural_spawn(0, 0, 4096)
                        .map(|p| [p.x as f32 + 0.5, p.y as f32, p.z as f32 + 0.5])
                }),
        }
        .unwrap_or([0.0, 140.0, 0.0]);
        let (time_of_day, player_yaw, player_pitch) = match identity.world_profile {
            WorldProfile::Natural => (14.15, 0.0, -0.15),
            WorldProfile::AstralFrontier => {
                let yaw = generator.astral_frontier_hub().map_or(-0.72, |hub| {
                    let dx = hub.x as f32 + 0.5 - spawn[0];
                    let dz = hub.y as f32 + 0.5 - spawn[2];
                    (-dx).atan2(-dz)
                });
                // A high, warm afternoon keeps construction readable while
                // giving the pastel nebula and terrain enough directional form.
                (15.65, yaw, -0.12)
            }
        };
        let mut weather = WeatherSettings::default();
        weather.apply_preset(WeatherPreset::Clear);
        weather.fog_density = 0.06;
        Self {
            name,
            seed: identity.seed,
            world_profile: identity.world_profile,
            terrain_grammar: identity.terrain_grammar,
            time_of_day,
            time_mode: TimeMode::Fixed,
            cycle_speed: 0.01,
            weather,
            scenery_quality: identity.scenery_quality,
            player_pos: spawn,
            player_yaw,
            player_pitch,
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

    pub const fn generation_identity(&self) -> WorldGenerationIdentity {
        WorldGenerationIdentity {
            seed: self.seed,
            world_profile: self.world_profile,
            scenery_quality: self.scenery_quality,
            terrain_grammar: self.terrain_grammar,
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

const WORLD_ARTIFACT_SUFFIXES: [&str; 3] = ["_edits", "_bots", "_city"];
const MAX_BROWSER_WORLD_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_BROWSER_WORLD_MANIFEST_ENTRIES: usize = 4_096;
const MAX_BROWSER_WORLD_NAME_BYTES: usize = 256;
const MAX_BROWSER_STORAGE_KEYS_SCANNED: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorldStorageNamespace {
    LegacyV1,
    GrammarV2,
    GrammarV3,
}

impl WorldStorageNamespace {
    #[cfg(any(target_arch = "wasm32", test))]
    const ALL: [Self; 3] = [Self::LegacyV1, Self::GrammarV2, Self::GrammarV3];

    const fn for_grammar(grammar: TerrainGrammarVersion) -> Self {
        match grammar {
            TerrainGrammarVersion::V1 => Self::LegacyV1,
            TerrainGrammarVersion::V2 => Self::GrammarV2,
            TerrainGrammarVersion::V3 => Self::GrammarV3,
        }
    }

    const fn grammar(self) -> TerrainGrammarVersion {
        match self {
            Self::LegacyV1 => TerrainGrammarVersion::V1,
            Self::GrammarV2 => TerrainGrammarVersion::V2,
            Self::GrammarV3 => TerrainGrammarVersion::V3,
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::LegacyV1 => "ron",
            Self::GrammarV2 => "world2",
            Self::GrammarV3 => "world3",
        }
    }

    const fn claim_bit(self) -> u8 {
        match self {
            Self::LegacyV1 => 0b001,
            Self::GrammarV2 => 0b010,
            Self::GrammarV3 => 0b100,
        }
    }

    fn from_extension(extension: Option<&str>) -> Option<Self> {
        let extension = extension?;
        if extension.eq_ignore_ascii_case("ron") {
            Some(Self::LegacyV1)
        } else if extension.eq_ignore_ascii_case("world2") {
            Some(Self::GrammarV2)
        } else if extension.eq_ignore_ascii_case("world3") {
            Some(Self::GrammarV3)
        } else {
            None
        }
    }
}

fn storage_claim_is_uniquely_decodable(
    physical_or_manifest_claims: usize,
    namespace_mask: u8,
    decoded_candidates: usize,
) -> bool {
    physical_or_manifest_claims == 1 && namespace_mask.count_ones() == 1 && decoded_candidates == 1
}

fn decode_world_meta_in_namespace(
    text: &str,
    namespace: WorldStorageNamespace,
) -> Result<WorldMeta, String> {
    let meta = ron::from_str::<WorldMeta>(text).map_err(|error| error.to_string())?;
    if meta.terrain_grammar != namespace.grammar() {
        return Err(format!(
            "world grammar {:?} contradicts the .{} storage namespace",
            meta.terrain_grammar,
            namespace.extension()
        ));
    }
    Ok(meta)
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
    let clean = sanitize_world_name(name);
    if clean.is_empty() {
        "world".to_string()
    } else {
        clean
    }
}

/// Filesystem ownership key for a sanitized world stem. World names are
/// restricted to ASCII after sanitization, so ASCII case-folding exactly
/// prevents `Foo`/`foo` aliases on the case-insensitive Windows target while
/// remaining deterministic on every test host.
pub fn world_storage_claim_key(name: &str) -> String {
    world_storage_stem(name).to_ascii_lowercase()
}

fn stored_stem_claim_key(stem: &str) -> String {
    stem.to_ascii_lowercase()
}

pub fn world_artifact_stem_from_entry_name(entry_name: &str) -> Option<String> {
    for suffix in WORLD_ARTIFACT_SUFFIXES {
        let Some(split) = entry_name.len().checked_sub(suffix.len()) else {
            continue;
        };
        if entry_name.is_char_boundary(split) && entry_name[split..].eq_ignore_ascii_case(suffix) {
            let stem = &entry_name[..split];
            if !stem.is_empty() {
                return Some(stem.to_string());
            }
        }
    }
    None
}

pub fn reserved_world_storage_stems() -> std::collections::HashSet<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let claims = WorldStorageNamespace::ALL
            .into_iter()
            .try_fold(Vec::new(), |mut all, namespace| {
                all.extend(browser_world_manifest(namespace)?);
                Ok::<_, String>(all)
            })
            .and_then(|manifest| {
                browser_physical_world_claims().map(|physical| (manifest, physical))
            });
        return match claims {
            Ok((manifest, physical)) => manifest
                .into_iter()
                .map(|stem| stored_stem_claim_key(&stem))
                .chain(
                    physical
                        .into_iter()
                        .map(|(_, stem)| stored_stem_claim_key(&stem)),
                )
                .collect(),
            Err(error) => {
                // This list is only a naming hint. The authoritative save
                // preflight below repeats the fallible read and blocks any
                // create/save instead of treating a malformed manifest as
                // empty.
                warn!("Browser world storage claims are unavailable: {error}");
                std::collections::HashSet::new()
            }
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        ensure_saves_dir();
        let mut out = std::collections::HashSet::new();
        if let Err(error) = ensure_safe_native_directory(&saves_dir()) {
            warn!("World storage root is unsafe: {error}");
            return out;
        }
        let Ok(read) = fs::read_dir(saves_dir()) else {
            return out;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if WorldStorageNamespace::from_extension(path.extension().and_then(|e| e.to_str()))
                .is_some()
            {
                if let Some(stem) = path.file_stem().and_then(|name| name.to_str()) {
                    out.insert(stored_stem_claim_key(stem));
                }
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if let Some(stem) = world_artifact_stem_from_entry_name(name) {
                out.insert(stored_stem_claim_key(&stem));
            }
        }
        out
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn world_file(name: &str) -> PathBuf {
    saves_dir().join(format!("{}.ron", world_storage_stem(name)))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn world_file_for_grammar(name: &str, grammar: TerrainGrammarVersion) -> PathBuf {
    let namespace = WorldStorageNamespace::for_grammar(grammar);
    saves_dir().join(format!(
        "{}.{}",
        world_storage_stem(name),
        namespace.extension()
    ))
}

pub fn list_worlds() -> Vec<WorldMeta> {
    #[cfg(target_arch = "wasm32")]
    {
        return list_browser_worlds();
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        ensure_saves_dir();
        if let Err(error) = ensure_safe_native_directory(&saves_dir()) {
            warn!("Refused to list worlds from an unsafe storage root: {error}");
            return Vec::new();
        }
        let mut candidates = std::collections::HashMap::<String, Vec<WorldMeta>>::new();
        let mut namespace_claims = std::collections::HashMap::<String, u8>::new();
        let mut physical_claim_counts = std::collections::HashMap::<String, usize>::new();
        let Ok(read) = fs::read_dir(saves_dir()) else {
            return Vec::new();
        };
        for entry in read.flatten() {
            let path = entry.path();
            let Some(namespace) = WorldStorageNamespace::from_extension(
                path.extension().and_then(|extension| extension.to_str()),
            ) else {
                continue;
            };
            let extension = path.extension().and_then(|extension| extension.to_str());
            let Some(path_stem) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let claim_bit = namespace.claim_bit();
            let claim_key = stored_stem_claim_key(&path_stem);
            *namespace_claims.entry(claim_key.clone()).or_default() |= claim_bit;
            let claim_count = physical_claim_counts.entry(claim_key.clone()).or_default();
            *claim_count = claim_count.saturating_add(1);
            if extension != Some(namespace.extension()) {
                warn!(
                    "Rejected world file '{}': metadata extension is not canonical lowercase",
                    path.display()
                );
                continue;
            }
            if let Err(error) = ensure_safe_native_regular_file_or_missing(&path) {
                warn!(
                    "Rejected world file '{}' because it is not a safe regular file: {error}",
                    path.display()
                );
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                match decode_world_meta_in_namespace(&text, namespace) {
                    Ok(meta) => {
                        if world_storage_stem(&meta.name) != path_stem {
                            warn!(
                                "Rejected world file '{}': embedded name '{}' does not own this storage stem",
                                path.display(),
                                meta.name
                            );
                            continue;
                        }
                        candidates.entry(claim_key).or_default().push(meta);
                    }
                    Err(error) => warn!(
                        "Rejected world file '{}' because its persisted identity is invalid: {error}",
                        path.display()
                    ),
                }
            }
        }
        let mut out = Vec::new();
        for (stem, mut worlds) in candidates {
            let claim_count = physical_claim_counts
                .get(&stem)
                .copied()
                .unwrap_or_default();
            let namespace_mask = namespace_claims.get(&stem).copied().unwrap_or_default();
            if storage_claim_is_uniquely_decodable(claim_count, namespace_mask, worlds.len()) {
                out.push(worlds.pop().expect("single candidate"));
            } else if claim_count != 1 {
                warn!(
                    "Rejected world storage stem '{stem}': multiple physical files claim its case-insensitive identity"
                );
            } else if namespace_mask.count_ones() > 1 {
                warn!(
                    "Rejected world storage stem '{stem}': multiple terrain-grammar namespaces physically claim it"
                );
            } else {
                warn!(
                    "Rejected world storage stem '{stem}': multiple terrain-grammar namespaces claim it"
                );
            }
        }
        out.sort_by(|a, b| b.last_played_epoch.cmp(&a.last_played_epoch));
        out
    }
}

pub fn validate_world_storage_for_save(meta: &WorldMeta) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
        let clean = world_storage_stem(&meta.name);
        if clean.is_empty() || clean.len() > MAX_BROWSER_WORLD_NAME_BYTES {
            return Err("browser world name is not canonical and bounded".to_owned());
        }
        let claim_key = stored_stem_claim_key(&clean);
        let mut matching_manifest_claims = 0usize;
        for manifest_namespace in WorldStorageNamespace::ALL {
            let names = browser_world_manifest(manifest_namespace)?;
            for claimed_name in names {
                if stored_stem_claim_key(&claimed_name) != claim_key {
                    continue;
                }
                matching_manifest_claims = matching_manifest_claims.saturating_add(1);
                if manifest_namespace != namespace || claimed_name != clean {
                    return Err(format!(
                        "browser world stem '{clean}' has a case or terrain-grammar namespace collision"
                    ));
                }
            }
        }
        let physical_claims = browser_physical_world_claims()?;
        let matching_physical = physical_claims
            .iter()
            .filter(|(_, stem)| stored_stem_claim_key(stem) == claim_key)
            .collect::<Vec<_>>();
        if matching_physical.len() > 1 {
            return Err(format!(
                "multiple browser payload keys claim world stem '{clean}'"
            ));
        }
        if let Some((physical_namespace, physical_name)) = matching_physical.first().copied() {
            if *physical_namespace != namespace || physical_name != &clean {
                return Err(format!(
                    "browser world stem '{clean}' has a physical case or terrain-grammar namespace collision"
                ));
            }
            if matching_manifest_claims != 1 {
                return Err(format!(
                    "browser world payload '{clean}' has no exact manifest authority"
                ));
            }
        } else if matching_manifest_claims > 0 {
            // A claim-first write may survive a quota failure. Keeping that
            // exact dangling claim visible and fillable is the safe recovery
            // state.
        }
        if let Some(existing) =
            crate::platform::browser_storage_get_checked(&browser_world_key(&meta.name, namespace))?
        {
            let existing = decode_world_meta_in_namespace(&existing, namespace)
                .map_err(|error| format!("existing browser world metadata is invalid: {error}"))?;
            if existing.name != meta.name
                || existing.generation_identity() != meta.generation_identity()
            {
                return Err(
                    "existing browser world metadata does not match the exact active identity"
                        .to_owned(),
                );
            }
        }
        return Ok(());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        ensure_saves_dir();
        ensure_safe_native_directory(&saves_dir())
            .map_err(|error| format!("world storage root is unsafe: {error}"))?;
        let expected_namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
        let expected_claim_key = world_storage_claim_key(&meta.name);
        let expected_stem = world_storage_stem(&meta.name);
        let mut matching_files = 0usize;
        let mut matching_artifact_roots = 0usize;
        let read = fs::read_dir(saves_dir())
            .map_err(|error| format!("could not inspect world-storage ownership: {error}"))?;
        for entry in read {
            let entry = entry
                .map_err(|error| format!("could not inspect world-storage ownership: {error}"))?;
            let path = entry.path();
            let namespace = WorldStorageNamespace::from_extension(
                path.extension().and_then(|extension| extension.to_str()),
            );
            if namespace.is_none() {
                let Some(entry_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Some(artifact_stem) = world_artifact_stem_from_entry_name(entry_name) else {
                    continue;
                };
                if stored_stem_claim_key(&artifact_stem) != expected_claim_key {
                    continue;
                }
                if artifact_stem != expected_stem {
                    return Err(format!(
                        "world stem '{expected_stem}' has a case-only sidecar collision with '{}'",
                        path.display()
                    ));
                }
                matching_artifact_roots = matching_artifact_roots.saturating_add(1);
                continue;
            }
            let namespace = namespace.expect("checked above");
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if stored_stem_claim_key(stem) != expected_claim_key {
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str())
                != Some(namespace.extension())
            {
                return Err(format!(
                    "existing world '{}' uses a non-canonical metadata extension",
                    path.display()
                ));
            }
            matching_files = matching_files.saturating_add(1);
            if matching_files > 1 || namespace != expected_namespace {
                return Err(format!(
                    "world stem '{}' has a case or terrain-grammar namespace collision",
                    world_storage_stem(&meta.name)
                ));
            }
            ensure_safe_native_regular_file_or_missing(&path).map_err(|error| {
                format!(
                    "existing world '{}' is not a safe regular file: {error}",
                    path.display()
                )
            })?;
            let text = fs::read_to_string(&path).map_err(|error| {
                format!(
                    "could not validate existing world '{}': {error}",
                    path.display()
                )
            })?;
            let existing = decode_world_meta_in_namespace(&text, namespace).map_err(|error| {
                format!("existing world '{}' is invalid: {error}", path.display())
            })?;
            if existing.name != meta.name
                || world_storage_stem(&existing.name) != stem
                || existing.generation_identity() != meta.generation_identity()
            {
                return Err(format!(
                    "existing world '{}' does not match the exact active name and generation identity",
                    path.display()
                ));
            }
        }
        if matching_artifact_roots > 0 && matching_files == 0 {
            return Err(format!(
                "world stem '{expected_stem}' is already reserved by orphaned sidecar authority"
            ));
        }
        Ok(())
    }
}

pub fn save_world(meta: &WorldMeta) -> Result<(), String> {
    validate_world_storage_for_save(meta)?;
    let text = ron::ser::to_string_pretty(meta, ron::ser::PrettyConfig::default())
        .map_err(|error| format!("failed to serialise world metadata: {error}"))?;

    #[cfg(target_arch = "wasm32")]
    {
        let namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
        // Publish the bounded claim before the payload. If the payload write
        // later fails (for example at the browser quota boundary), the
        // manifest retains a visible, reserved dangling claim instead of
        // leaving an undiscoverable key that a case-only alias could reuse.
        save_browser_world_manifest_entry(&meta.name, namespace)?;
        crate::platform::browser_storage_set(&browser_world_key(&meta.name, namespace), &text)
            .map_err(|error| error.to_string())?;
        info!("Saved browser world '{}' to localStorage", meta.name);
        return Ok(());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = world_file_for_grammar(&meta.name, meta.terrain_grammar);
        atomic_write(&path, &text)
            .map_err(|error| format!("failed to write '{}': {error}", path.display()))?;
        info!("Saved world '{}' to {}", meta.name, path.display());
        Ok(())
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
) -> Result<(), String> {
    let mut meta = list_worlds()
        .into_iter()
        .find(|meta| {
            meta.name == active_meta.name
                && meta.generation_identity() == active_meta.generation_identity()
        })
        .unwrap_or_else(|| active_meta.clone());
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
    save_world(&meta)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn atomic_write_text(final_path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent)?;
        ensure_safe_native_directory(parent)?;
    }
    ensure_safe_native_regular_file_or_missing(final_path)?;
    let tmp_path = final_path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world-save"),
        std::process::id(),
        crate::platform::now_nanos_seed()
    ));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        if let Err(rename_error) = fs::rename(&tmp_path, final_path) {
            ensure_safe_native_regular_file_or_missing(final_path)?;
            #[cfg(windows)]
            replace_existing_file_windows(final_path, &tmp_path).map_err(|replace_error| {
                std::io::Error::new(
                    replace_error.kind(),
                    format!(
                        "atomic rename failed ({rename_error}); ReplaceFileW failed ({replace_error})"
                    ),
                )
            })?;
            #[cfg(not(windows))]
            return Err(rename_error);
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_native_directory(path: &std::path::Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || native_metadata_is_reparse_point(&metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{}' is not a safe regular directory", path.display()),
        ));
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_native_regular_file_or_missing(path: &std::path::Path) -> std::io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || native_metadata_is_reparse_point(&metadata)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("'{}' is not a safe regular file", path.display()),
        ));
    }
    Ok(())
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn native_metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(not(target_arch = "wasm32"), not(windows)))]
fn native_metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn replace_existing_file_windows(
    final_path: &std::path::Path,
    replacement_path: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let final_wide = final_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are owned, NUL-terminated UTF-16 paths and remain
    // alive for the duration of the synchronous Win32 call. Optional backup
    // and exclusion pointers are deliberately null.
    let replaced = unsafe {
        ReplaceFileW(
            final_wide.as_ptr(),
            replacement_wide.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Write `text` to `final_path` atomically: serialise to a sibling
/// unique `.tmp` file, fsync the content, then atomically rename/replace the
/// target. If the process is killed mid-write, the previous good save remains
/// intact; Windows uses `ReplaceFileW` because `std::fs::rename` cannot replace
/// an existing destination there.
#[cfg(not(target_arch = "wasm32"))]
fn atomic_write(final_path: &std::path::Path, text: &str) -> std::io::Result<()> {
    atomic_write_text(final_path, text)
}

#[cfg(target_arch = "wasm32")]
fn browser_world_key(name: &str, namespace: WorldStorageNamespace) -> String {
    let prefix = match namespace {
        WorldStorageNamespace::LegacyV1 => WEB_WORLD_PREFIX,
        WorldStorageNamespace::GrammarV2 => WEB_WORLD_V2_PREFIX,
        WorldStorageNamespace::GrammarV3 => WEB_WORLD_V3_PREFIX,
    };
    format!("{prefix}{}", world_storage_stem(name))
}

#[cfg(target_arch = "wasm32")]
fn browser_physical_world_claims() -> Result<Vec<(WorldStorageNamespace, String)>, String> {
    let keys = crate::platform::browser_storage_keys_checked(MAX_BROWSER_STORAGE_KEYS_SCANNED)?;
    let mut claims = Vec::new();
    for key in keys {
        // Versioned prefixes must be checked before the legacy prefix so a
        // future namespace can never be misclassified as part of a V1 name.
        let (namespace, name) = if let Some(name) = key.strip_prefix(WEB_WORLD_V3_PREFIX) {
            (WorldStorageNamespace::GrammarV3, name)
        } else if let Some(name) = key.strip_prefix(WEB_WORLD_V2_PREFIX) {
            (WorldStorageNamespace::GrammarV2, name)
        } else if let Some(name) = key.strip_prefix(WEB_WORLD_PREFIX) {
            (WorldStorageNamespace::LegacyV1, name)
        } else {
            continue;
        };
        if name.is_empty()
            || name.len() > MAX_BROWSER_WORLD_NAME_BYTES
            || world_storage_stem(name) != name
        {
            return Err(format!(
                "browser payload key '{key}' has a non-canonical world name"
            ));
        }
        claims.push((namespace, name.to_owned()));
        if claims.len() > MAX_BROWSER_WORLD_MANIFEST_ENTRIES {
            return Err("browser world payload claims exceed their hard limit".to_owned());
        }
    }
    Ok(claims)
}

#[cfg(target_arch = "wasm32")]
fn browser_world_manifest(namespace: WorldStorageNamespace) -> Result<Vec<String>, String> {
    let key = match namespace {
        WorldStorageNamespace::LegacyV1 => WEB_WORLD_MANIFEST_KEY,
        WorldStorageNamespace::GrammarV2 => WEB_WORLD_V2_MANIFEST_KEY,
        WorldStorageNamespace::GrammarV3 => WEB_WORLD_V3_MANIFEST_KEY,
    };
    let text = crate::platform::browser_storage_get_checked(key)?;
    decode_browser_world_manifest(text.as_deref())
        .map_err(|error| format!("browser world manifest '{key}' is invalid: {error}"))
}

fn decode_browser_world_manifest(text: Option<&str>) -> Result<Vec<String>, String> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    if text.len() > MAX_BROWSER_WORLD_MANIFEST_BYTES {
        return Err("manifest exceeds its byte limit".to_owned());
    }
    let names = ron::from_str::<Vec<String>>(text).map_err(|error| error.to_string())?;
    if names.len() > MAX_BROWSER_WORLD_MANIFEST_ENTRIES {
        return Err("manifest exceeds its entry limit".to_owned());
    }
    let mut previous = None::<&str>;
    let mut claim_keys = std::collections::HashSet::with_capacity(names.len());
    for name in &names {
        if name.is_empty()
            || name.len() > MAX_BROWSER_WORLD_NAME_BYTES
            || sanitize_world_name(name) != *name
            || world_storage_stem(name) != *name
        {
            return Err("manifest contains a non-canonical world name".to_owned());
        }
        if previous.is_some_and(|prior| prior >= name.as_str()) {
            return Err("manifest names are not in strict canonical order".to_owned());
        }
        if !claim_keys.insert(world_storage_claim_key(name)) {
            return Err("manifest contains duplicate case-insensitive ownership".to_owned());
        }
        previous = Some(name);
    }
    Ok(names)
}

#[cfg(target_arch = "wasm32")]
fn write_browser_world_manifest(
    names: &[String],
    namespace: WorldStorageNamespace,
) -> Result<(), String> {
    let key = match namespace {
        WorldStorageNamespace::LegacyV1 => WEB_WORLD_MANIFEST_KEY,
        WorldStorageNamespace::GrammarV2 => WEB_WORLD_V2_MANIFEST_KEY,
        WorldStorageNamespace::GrammarV3 => WEB_WORLD_V3_MANIFEST_KEY,
    };
    let text = ron::ser::to_string(names)
        .map_err(|error| format!("failed to serialise browser world manifest: {error}"))?;
    let validated = decode_browser_world_manifest(Some(&text))?;
    if validated != names {
        return Err("browser world manifest is not canonical".to_owned());
    }
    crate::platform::browser_storage_set(key, &text).map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
fn save_browser_world_manifest_entry(
    name: &str,
    namespace: WorldStorageNamespace,
) -> Result<(), String> {
    let clean = world_storage_stem(name);
    if clean.is_empty() || clean.len() > MAX_BROWSER_WORLD_NAME_BYTES {
        return Err("browser world name is not canonical and bounded".to_owned());
    }
    let mut names = browser_world_manifest(namespace)?;
    if !names.iter().any(|existing| existing == &clean) {
        names.push(clean);
        names.sort();
    }
    write_browser_world_manifest(&names, namespace)
}

#[cfg(target_arch = "wasm32")]
fn list_browser_worlds() -> Vec<WorldMeta> {
    let mut candidates = std::collections::HashMap::<String, Vec<WorldMeta>>::new();
    let mut manifests = Vec::with_capacity(WorldStorageNamespace::ALL.len());
    for namespace in WorldStorageNamespace::ALL {
        match browser_world_manifest(namespace) {
            Ok(names) => manifests.push((namespace, names)),
            Err(error) => {
                warn!(
                    "Refused to list browser worlds because the .{} manifest is invalid: {error}",
                    namespace.extension()
                );
                return Vec::new();
            }
        }
    }
    let physical_claims = match browser_physical_world_claims() {
        Ok(claims) => claims,
        Err(error) => {
            warn!("Refused to list browser worlds after payload enumeration failed: {error}");
            return Vec::new();
        }
    };
    let mut physical_by_claim =
        std::collections::HashMap::<String, Vec<(WorldStorageNamespace, String)>>::new();
    for (namespace, name) in physical_claims {
        physical_by_claim
            .entry(world_storage_claim_key(&name))
            .or_default()
            .push((namespace, name));
    }
    let mut namespace_claims = std::collections::HashMap::<String, u8>::new();
    let mut manifest_claim_counts = std::collections::HashMap::<String, usize>::new();
    for (namespace, names) in manifests {
        let claim_bit = namespace.claim_bit();
        for name in names {
            let claim_key = stored_stem_claim_key(&name);
            *namespace_claims.entry(claim_key.clone()).or_default() |= claim_bit;
            let claim_count = manifest_claim_counts.entry(claim_key.clone()).or_default();
            *claim_count = claim_count.saturating_add(1);
            let matching_physical = physical_by_claim
                .get(&claim_key)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if matching_physical.len() != 1
                || matching_physical[0].0 != namespace
                || matching_physical[0].1 != name
            {
                warn!(
                    "Rejected browser world '{name}': manifest and physical payload ownership are not one-to-one"
                );
                continue;
            }
            let text = match crate::platform::browser_storage_get_checked(&browser_world_key(
                &name, namespace,
            )) {
                Ok(Some(text)) => text,
                Ok(None) => continue,
                Err(error) => {
                    warn!("Refused to list browser worlds after an authority read failed: {error}");
                    return Vec::new();
                }
            };
            match decode_world_meta_in_namespace(&text, namespace) {
                Ok(meta) if world_storage_stem(&meta.name) == name => {
                    candidates.entry(claim_key).or_default().push(meta);
                }
                Ok(meta) => warn!(
                    "Rejected browser world manifest entry '{}': embedded name '{}' does not own this storage stem",
                    name, meta.name
                ),
                Err(error) => warn!(
                    "Rejected browser world '{}' because its persisted identity is invalid: {error}",
                    name
                ),
            }
        }
    }
    let mut out = Vec::new();
    for (stem, mut worlds) in candidates {
        let claim_count = manifest_claim_counts
            .get(&stem)
            .copied()
            .unwrap_or_default();
        let namespace_mask = namespace_claims.get(&stem).copied().unwrap_or_default();
        if storage_claim_is_uniquely_decodable(claim_count, namespace_mask, worlds.len()) {
            out.push(worlds.pop().expect("single candidate"));
        } else if claim_count != 1 {
            warn!(
                "Rejected browser world storage stem '{stem}': multiple manifest entries claim its case-insensitive identity"
            );
        } else if namespace_mask.count_ones() > 1 {
            warn!(
                "Rejected browser world storage stem '{stem}': multiple terrain-grammar manifests claim it"
            );
        } else {
            warn!(
                "Rejected browser world storage stem '{stem}': multiple terrain-grammar namespaces claim it"
            );
        }
    }
    out.sort_by(|a, b| b.last_played_epoch.cmp(&a.last_played_epoch));
    out
}

/// Delete one selected world only after its persisted namespace, name, and
/// generation identity have been revalidated. The operation fails closed on
/// ownership ambiguity, unsafe native paths, or any non-NotFound I/O failure.
pub fn delete_world(meta: &WorldMeta) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
        let stem = world_storage_stem(&meta.name);
        let mut manifests = Vec::with_capacity(WorldStorageNamespace::ALL.len());
        for manifest_namespace in WorldStorageNamespace::ALL {
            manifests.push((
                manifest_namespace,
                browser_world_manifest(manifest_namespace)?,
            ));
        }
        let physical_claims = browser_physical_world_claims()?;
        validate_exact_world_delete_claims(meta, &manifests, &physical_claims)?;

        let key = browser_world_key(&meta.name, namespace);
        let payload = crate::platform::browser_storage_get_checked(&key)?
            .ok_or_else(|| format!("browser world payload '{key}' is missing"))?;
        let persisted = decode_world_meta_in_namespace(&payload, namespace)
            .map_err(|error| format!("browser world payload '{key}' is invalid: {error}"))?;
        validate_world_delete_identity(meta, &persisted, &stem)?;

        let original_manifest = manifests
            .iter()
            .find_map(|(candidate, names)| (*candidate == namespace).then(|| names.clone()))
            .expect("every browser namespace was preflighted");
        let mut remaining_manifest = original_manifest.clone();
        remaining_manifest.retain(|claimed| claimed != &stem);

        // Revoke the exact manifest claim before removing its payload. A
        // malformed or unwritable manifest therefore cannot destroy data. If
        // payload removal fails, best-effort rollback restores discoverability
        // and both failures are reported when the rollback also fails.
        write_browser_world_manifest(&remaining_manifest, namespace)?;
        if let Err(remove_error) = crate::platform::browser_storage_remove(&key) {
            let rollback = write_browser_world_manifest(&original_manifest, namespace);
            return match rollback {
                Ok(()) => Err(format!(
                    "browser world payload removal failed after its manifest was updated; the manifest was restored: {remove_error}"
                )),
                Err(rollback_error) => Err(format!(
                    "browser world payload removal failed ({remove_error}); manifest rollback also failed ({rollback_error})"
                )),
            };
        }
        info!(
            "Deleted browser world '{}' from the exact .{} namespace",
            meta.name,
            namespace.extension()
        );
        return Ok(());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        delete_world_from_native_root(meta, &saves_dir())
    }
}

fn validate_world_delete_identity(
    requested: &WorldMeta,
    persisted: &WorldMeta,
    expected_stem: &str,
) -> Result<(), String> {
    if persisted.name != requested.name
        || world_storage_stem(&persisted.name) != expected_stem
        || persisted.generation_identity() != requested.generation_identity()
    {
        return Err(format!(
            "persisted world '{}' does not match the exact selected name and generation identity",
            persisted.name
        ));
    }
    Ok(())
}

#[cfg(any(target_arch = "wasm32", test))]
fn validate_exact_world_delete_claims(
    meta: &WorldMeta,
    manifests: &[(WorldStorageNamespace, Vec<String>)],
    physical_claims: &[(WorldStorageNamespace, String)],
) -> Result<(), String> {
    let namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
    let stem = world_storage_stem(&meta.name);
    if stem.is_empty() || stem.len() > MAX_BROWSER_WORLD_NAME_BYTES {
        return Err("world name is not canonical and bounded".to_owned());
    }
    if manifests.len() != WorldStorageNamespace::ALL.len()
        || WorldStorageNamespace::ALL
            .into_iter()
            .any(|expected| !manifests.iter().any(|(actual, _)| *actual == expected))
    {
        return Err("not every terrain-grammar manifest was preflighted".to_owned());
    }

    let claim_key = stored_stem_claim_key(&stem);
    let matching_manifest = manifests
        .iter()
        .flat_map(|(manifest_namespace, names)| {
            names
                .iter()
                .map(move |name| (*manifest_namespace, name.as_str()))
        })
        .filter(|(_, name)| stored_stem_claim_key(name) == claim_key)
        .collect::<Vec<_>>();
    if matching_manifest.len() != 1 || matching_manifest[0] != (namespace, stem.as_str()) {
        return Err(format!(
            "world stem '{stem}' does not have one exact terrain-grammar manifest claim"
        ));
    }

    let matching_physical = physical_claims
        .iter()
        .filter(|(_, name)| stored_stem_claim_key(name) == claim_key)
        .collect::<Vec<_>>();
    if matching_physical.len() != 1
        || matching_physical[0].0 != namespace
        || matching_physical[0].1.as_str() != stem
    {
        return Err(format!(
            "world stem '{stem}' does not have one exact terrain-grammar payload claim"
        ));
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn world_artifact_roots_at(saves_root: &Path, name: &str) -> [PathBuf; 3] {
    let stem = world_storage_stem(name);
    [
        saves_root.join(format!("{stem}_edits")),
        saves_root.join(format!("{stem}_bots")),
        saves_root.join(format!("{stem}_city")),
    ]
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct NativeWorldDeletePlan {
    saves_root: PathBuf,
    metadata_path: PathBuf,
    artifact_roots: [PathBuf; 3],
}

#[cfg(not(target_arch = "wasm32"))]
const MAX_WORLD_DELETE_PREFLIGHT_ENTRIES: usize = 65_536;
#[cfg(not(target_arch = "wasm32"))]
const MAX_WORLD_METADATA_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
fn delete_world_from_native_root(meta: &WorldMeta, saves_root: &Path) -> Result<(), String> {
    let plan = preflight_native_world_delete(meta, saves_root)?;
    execute_native_world_delete_plan(
        meta,
        &plan,
        |path| fs::remove_dir_all(path),
        |path| fs::remove_file(path),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn preflight_native_world_delete(
    meta: &WorldMeta,
    saves_root: &Path,
) -> Result<NativeWorldDeletePlan, String> {
    let saves_root = lexical_absolute_native_path(saves_root)?;
    ensure_safe_native_directory_chain(&saves_root)
        .map_err(|error| format!("world storage root is unsafe: {error}"))?;

    let namespace = WorldStorageNamespace::for_grammar(meta.terrain_grammar);
    let stem = world_storage_stem(&meta.name);
    let claim_key = stored_stem_claim_key(&stem);
    let metadata_path = saves_root.join(format!("{stem}.{}", namespace.extension()));
    let artifact_roots = world_artifact_roots_at(&saves_root, &meta.name);
    let expected_artifact_names = WORLD_ARTIFACT_SUFFIXES.map(|suffix| format!("{stem}{suffix}"));
    let mut metadata_claims = 0usize;
    let mut artifact_claims = [false; 3];
    let mut root_entries = 0usize;

    let read = fs::read_dir(&saves_root)
        .map_err(|error| format!("could not inspect world-delete ownership: {error}"))?;
    for entry in read {
        let entry =
            entry.map_err(|error| format!("could not inspect world-delete ownership: {error}"))?;
        root_entries = root_entries
            .checked_add(1)
            .ok_or_else(|| "world-delete root preflight count overflowed".to_owned())?;
        if root_entries > MAX_WORLD_DELETE_PREFLIGHT_ENTRIES {
            return Err(format!(
                "world storage root exceeds its {} entry deletion-preflight limit",
                MAX_WORLD_DELETE_PREFLIGHT_ENTRIES
            ));
        }
        let path = entry.path();
        if let Some(candidate_namespace) = WorldStorageNamespace::from_extension(
            path.extension().and_then(|extension| extension.to_str()),
        ) {
            let Some(candidate_stem) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if stored_stem_claim_key(candidate_stem) != claim_key {
                continue;
            }
            metadata_claims = metadata_claims.saturating_add(1);
            if path != metadata_path
                || candidate_namespace != namespace
                || path.extension().and_then(|extension| extension.to_str())
                    != Some(namespace.extension())
            {
                return Err(format!(
                    "world stem '{stem}' has a case or terrain-grammar metadata collision at '{}'",
                    path.display()
                ));
            }
            continue;
        }

        let Some(entry_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(candidate_stem) = world_artifact_stem_from_entry_name(entry_name) else {
            continue;
        };
        if stored_stem_claim_key(&candidate_stem) != claim_key {
            continue;
        }
        let Some(index) = expected_artifact_names
            .iter()
            .position(|expected| expected == entry_name)
        else {
            return Err(format!(
                "world stem '{stem}' has a case-only sidecar collision at '{}'",
                path.display()
            ));
        };
        if artifact_claims[index] || path != artifact_roots[index] {
            return Err(format!(
                "world stem '{stem}' has duplicate sidecar ownership at '{}'",
                path.display()
            ));
        }
        artifact_claims[index] = true;
    }

    if metadata_claims != 1 {
        return Err(format!(
            "world stem '{stem}' does not have exactly one metadata authority"
        ));
    }
    validate_native_world_metadata(meta, &metadata_path, &saves_root)?;
    for (index, (exists, root)) in artifact_claims.into_iter().zip(&artifact_roots).enumerate() {
        if exists {
            validate_safe_native_delete_tree(root, &saves_root)?;
            if index == 0 {
                validate_exact_edit_sidecar_namespace(root, meta.terrain_grammar)?;
            }
        }
    }

    Ok(NativeWorldDeletePlan {
        saves_root,
        metadata_path,
        artifact_roots,
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn execute_native_world_delete_plan(
    meta: &WorldMeta,
    plan: &NativeWorldDeletePlan,
    mut remove_dir: impl FnMut(&Path) -> std::io::Result<()>,
    mut remove_file: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut removed_artifacts = 0usize;
    for (index, root) in plan.artifact_roots.iter().enumerate() {
        if let Err(error) = ensure_safe_native_directory_chain(&plan.saves_root) {
            errors.push(format!(
                "world storage root became unsafe before deleting '{}': {error}",
                root.display()
            ));
            continue;
        }
        match fs::symlink_metadata(root) {
            Ok(_) => {
                if let Err(error) = validate_safe_native_delete_tree(root, &plan.saves_root) {
                    errors.push(error);
                    continue;
                }
                if index == 0 {
                    if let Err(error) =
                        validate_exact_edit_sidecar_namespace(root, meta.terrain_grammar)
                    {
                        errors.push(error);
                        continue;
                    }
                }
                match remove_dir(root) {
                    Ok(()) => removed_artifacts = removed_artifacts.saturating_add(1),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => errors.push(format!(
                        "failed to delete validated world sidecar '{}': {error}",
                        root.display()
                    )),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => errors.push(format!(
                "failed to revalidate world sidecar '{}': {error}",
                root.display()
            )),
        }
    }

    // Metadata is the discoverable authority and is deliberately removed last.
    // If any sidecar failed, retaining it keeps the world visible and prevents
    // the menu from treating a partial deletion as success.
    if !errors.is_empty() {
        return Err(format_delete_errors(errors));
    }

    ensure_safe_native_directory_chain(&plan.saves_root)
        .map_err(|error| format!("world storage root became unsafe: {error}"))?;
    validate_native_world_metadata(meta, &plan.metadata_path, &plan.saves_root)?;
    match remove_file(&plan.metadata_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!(
            "failed to delete validated world metadata '{}': {error}",
            plan.metadata_path.display()
        )),
    }
    if !errors.is_empty() {
        return Err(format_delete_errors(errors));
    }

    info!(
        "Deleted exact .{} world authority '{}' and {removed_artifacts} sidecar root(s)",
        WorldStorageNamespace::for_grammar(meta.terrain_grammar).extension(),
        meta.name
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn lexical_absolute_native_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    if absolute.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        return Err(format!(
            "world storage path '{}' is not lexically canonical",
            absolute.display()
        ));
    }
    Ok(absolute)
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_safe_native_directory_chain(path: &Path) -> std::io::Result<()> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || native_metadata_is_reparse_point(&metadata)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "'{}' or one of its ancestors is not a safe regular directory",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_native_world_metadata(
    requested: &WorldMeta,
    path: &Path,
    saves_root: &Path,
) -> Result<(), String> {
    ensure_safe_native_directory_chain(saves_root)
        .map_err(|error| format!("world storage root is unsafe: {error}"))?;
    ensure_safe_native_regular_file_or_missing(path)
        .map_err(|error| format!("world metadata '{}' is unsafe: {error}", path.display()))?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "could not inspect world metadata '{}': {error}",
            path.display()
        )
    })?;
    if metadata.len() > MAX_WORLD_METADATA_BYTES {
        return Err(format!(
            "world metadata '{}' exceeds its {} byte limit",
            path.display(),
            MAX_WORLD_METADATA_BYTES
        ));
    }
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "could not read world metadata '{}': {error}",
            path.display()
        )
    })?;
    ensure_safe_native_directory_chain(saves_root)
        .map_err(|error| format!("world storage root became unsafe: {error}"))?;
    ensure_safe_native_regular_file_or_missing(path).map_err(|error| {
        format!(
            "world metadata '{}' became unsafe while it was read: {error}",
            path.display()
        )
    })?;
    let namespace = WorldStorageNamespace::for_grammar(requested.terrain_grammar);
    let persisted = decode_world_meta_in_namespace(&text, namespace)
        .map_err(|error| format!("world metadata '{}' is invalid: {error}", path.display()))?;
    validate_world_delete_identity(requested, &persisted, &world_storage_stem(&requested.name))
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_safe_native_delete_tree(root: &Path, saves_root: &Path) -> Result<(), String> {
    ensure_safe_native_directory_chain(saves_root)
        .map_err(|error| format!("world storage root is unsafe: {error}"))?;
    let root_metadata = fs::symlink_metadata(root).map_err(|error| {
        format!(
            "could not inspect world sidecar '{}': {error}",
            root.display()
        )
    })?;
    if !root_metadata.file_type().is_dir()
        || root_metadata.file_type().is_symlink()
        || native_metadata_is_reparse_point(&root_metadata)
    {
        return Err(format!(
            "world sidecar '{}' is not a safe regular directory",
            root.display()
        ));
    }

    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let read = fs::read_dir(&directory).map_err(|error| {
            format!(
                "could not inspect world sidecar directory '{}': {error}",
                directory.display()
            )
        })?;
        for entry in read {
            let entry = entry.map_err(|error| {
                format!(
                    "could not inspect world sidecar directory '{}': {error}",
                    directory.display()
                )
            })?;
            visited = visited
                .checked_add(1)
                .ok_or_else(|| "world sidecar preflight count overflowed".to_owned())?;
            if visited > MAX_WORLD_DELETE_PREFLIGHT_ENTRIES {
                return Err(format!(
                    "world sidecar '{}' exceeds its {} entry preflight limit",
                    root.display(),
                    MAX_WORLD_DELETE_PREFLIGHT_ENTRIES
                ));
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "could not inspect world sidecar '{}': {error}",
                    path.display()
                )
            })?;
            if metadata.file_type().is_symlink() || native_metadata_is_reparse_point(&metadata) {
                return Err(format!(
                    "world sidecar '{}' contains a symlink or reparse point",
                    path.display()
                ));
            }
            if metadata.file_type().is_dir() {
                pending.push(path);
            } else if !metadata.file_type().is_file() {
                return Err(format!(
                    "world sidecar '{}' contains a non-regular entry",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn validate_exact_edit_sidecar_namespace(
    edits_root: &Path,
    selected_grammar: TerrainGrammarVersion,
) -> Result<(), String> {
    let read = fs::read_dir(edits_root).map_err(|error| {
        format!(
            "could not inspect edit sidecar namespace '{}': {error}",
            edits_root.display()
        )
    })?;
    for entry in read {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect edit sidecar namespace '{}': {error}",
                edits_root.display()
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let folded = name.to_ascii_lowercase();
        for (grammar, namespace) in [
            (TerrainGrammarVersion::V1, "chunks"),
            (TerrainGrammarVersion::V2, "grammar_v2"),
            (TerrainGrammarVersion::V3, "grammar_v3"),
        ] {
            let claims_namespace = folded == namespace
                || folded.starts_with(&format!(".{namespace}.stage-"))
                || folded.starts_with(&format!(".{namespace}.previous-"));
            if claims_namespace && grammar != selected_grammar {
                return Err(format!(
                    "edit sidecar '{}' contains the foreign {grammar:?} terrain-grammar namespace '{name}'",
                    edits_root.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn format_delete_errors(errors: Vec<String>) -> String {
    format!(
        "world deletion was incomplete ({} error(s)): {}",
        errors.len(),
        errors.join(" | ")
    )
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

    #[cfg(not(target_arch = "wasm32"))]
    fn isolated_world_delete_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock must be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "voxel-native-world-delete-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create isolated world-delete test root");
        root
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn remove_exact_test_tree(path: &Path) {
        let temp = std::env::temp_dir();
        assert_eq!(
            path.parent(),
            Some(temp.as_path()),
            "test cleanup must stay directly inside the process temp directory"
        );
        assert!(
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("voxel-native-world-delete-")),
            "test cleanup target must retain its unique world-delete prefix"
        );

        fn remove_entry(path: &Path) {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(error) => panic!("inspect exact test cleanup target: {error}"),
            };
            if metadata.file_type().is_symlink() {
                #[cfg(windows)]
                std::fs::remove_dir(path).expect("remove exact directory symlink");
                #[cfg(not(windows))]
                std::fs::remove_file(path).expect("remove exact directory symlink");
            } else if metadata.file_type().is_dir() {
                let mut children = std::fs::read_dir(path)
                    .expect("read exact test cleanup directory")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("enumerate exact test cleanup directory");
                children.sort_by_key(|entry| entry.file_name());
                for child in children {
                    remove_entry(&child.path());
                }
                std::fs::remove_dir(path).expect("remove exact empty test directory");
            } else {
                std::fs::remove_file(path).expect("remove exact test file");
            }
        }

        remove_entry(path);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_world_delete_metadata(root: &Path, meta: &WorldMeta) -> PathBuf {
        let path = root.join(format!(
            "{}.{}",
            world_storage_stem(&meta.name),
            WorldStorageNamespace::for_grammar(meta.terrain_grammar).extension()
        ));
        let text = ron::ser::to_string_pretty(meta, ron::ser::PrettyConfig::default())
            .expect("serialize isolated world-delete metadata");
        std::fs::write(&path, text).expect("write isolated world-delete metadata");
        path
    }

    #[cfg(all(not(target_arch = "wasm32"), windows))]
    fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(all(not(target_arch = "wasm32"), unix))]
    fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[test]
    fn isolated_automation_modes_cannot_read_or_write_persistent_settings() {
        let cases = [
            (false, false, true),
            (true, false, false),
            (false, true, false),
            (true, true, false),
        ];

        for (qa_enabled, isolated_observer_enabled, expected) in cases {
            assert_eq!(
                settings_persistence_allowed(qa_enabled, isolated_observer_enabled),
                expected,
                "qa={qa_enabled}, isolated_observer={isolated_observer_enabled}"
            );
        }
    }

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
        assert_eq!(meta.scenery_quality, SceneryQuality::Balanced);
        assert_eq!(meta.terrain_grammar, TerrainGrammarVersion::V1);
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
        assert_eq!(settings.scenery_quality, SceneryQuality::Balanced);
        assert_eq!(settings.terrain_grammar, TerrainGrammarVersion::V1);
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
        assert_eq!(settings.theme.style, crate::theme::ThemeStyle::ClassicCrt);
        assert!(settings.theme.scanlines);
        assert_eq!(settings.visual_preset, VisualPreset::NeonShuttle);
        assert!(settings.companion_ui.show_companion_dock);
        assert!(settings.companion_ui.editor_assist_enabled);
    }

    #[test]
    fn world_mode_cards_apply_friendly_presets() {
        let mut settings = WorldSettings::default();
        settings.apply_world_mode_card(WorldModeCard::FastLaptop);
        assert_eq!(settings.graphics, GraphicsMode::Fast);
        assert_eq!(settings.runtime_profile, RuntimeProfile::LowSpec);
        assert_eq!(settings.scenery_quality, SceneryQuality::Lean);
        assert!(settings.render_distance <= 24);
        assert_eq!(settings.vertical_chunks, 6);

        settings.apply_world_mode_card(WorldModeCard::Cinematic);
        assert_eq!(settings.graphics, GraphicsMode::High);
        assert_eq!(settings.runtime_profile, RuntimeProfile::Cinematic);
        assert_eq!(settings.scenery_quality, SceneryQuality::Lush);
        assert!(settings.render_distance >= 56);
        assert!(settings.mesh_applies_per_frame <= 8);
    }

    #[test]
    fn fast_laptop_preset_prefers_frame_pacing_over_chunk_flooding() {
        let mut settings = WorldSettings::default();
        settings.apply_world_mode_card(WorldModeCard::FastLaptop);

        assert!(settings.render_distance <= 24);
        assert!(settings.vertical_chunks <= 6);
        assert!(settings.chunks_per_frame <= 6);
        assert!(settings.meshes_per_frame <= 6);
        assert!(settings.mesh_applies_per_frame <= 4);
        assert!(settings.max_in_flight_terrain <= 64);
        assert!(settings.max_in_flight_meshes <= 48);
    }

    #[test]
    fn artifact_root_names_reserve_world_storage_stems() {
        assert_eq!(
            world_artifact_stem_from_entry_name("world_04_edits"),
            Some("world_04".to_string())
        );
        assert_eq!(
            world_artifact_stem_from_entry_name("world_04_bots"),
            Some("world_04".to_string())
        );
        assert_eq!(
            world_artifact_stem_from_entry_name("world_04_city"),
            Some("world_04".to_string())
        );
        assert_eq!(world_artifact_stem_from_entry_name("world_04"), None);
        assert_eq!(
            world_artifact_stem_from_entry_name("Foo_EDITS"),
            Some("Foo".to_string())
        );
        assert_eq!(
            world_artifact_stem_from_entry_name("BAR_BOTS"),
            Some("BAR".to_string())
        );
        assert_eq!(
            world_artifact_stem_from_entry_name("Baz_CITY"),
            Some("Baz".to_string())
        );
    }

    #[test]
    fn world_storage_claim_keys_are_case_insensitive_after_sanitization() {
        assert_eq!(world_storage_claim_key("Foo"), "foo");
        assert_eq!(world_storage_claim_key("foo"), "foo");
        assert_eq!(world_storage_claim_key("DREAM?CITY"), "dream_city");
        assert_eq!(
            world_storage_claim_key("dream_city"),
            world_storage_claim_key("DREAM?CITY")
        );
    }

    #[test]
    fn one_valid_candidate_cannot_hide_a_second_case_alias_claim() {
        assert!(storage_claim_is_uniquely_decodable(1, 0b10, 1));
        assert!(storage_claim_is_uniquely_decodable(1, 0b100, 1));
        assert!(!storage_claim_is_uniquely_decodable(2, 0b10, 1));
        assert!(!storage_claim_is_uniquely_decodable(2, 0b10, 2));
        assert!(!storage_claim_is_uniquely_decodable(2, 0b11, 1));
        assert!(!storage_claim_is_uniquely_decodable(2, 0b110, 1));
        assert!(!storage_claim_is_uniquely_decodable(3, 0b111, 1));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn atomic_write_text_replaces_an_existing_regular_file_without_debris() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock must be after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "voxel-native-atomic-write-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create isolated atomic-write test directory");
        let target = root.join("world.world2");

        atomic_write_text(&target, "first authority")
            .expect("initial authority publication must succeed");
        atomic_write_text(&target, "replacement authority")
            .expect("existing authority replacement must succeed");

        assert_eq!(
            std::fs::read_to_string(&target).expect("read replaced authority"),
            "replacement authority"
        );
        let entries = std::fs::read_dir(&root)
            .expect("inspect isolated atomic-write test directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read isolated atomic-write test entries");
        assert_eq!(
            entries.len(),
            1,
            "temporary publication debris must be retired"
        );
        assert_eq!(entries[0].path(), target);

        std::fs::remove_file(&target).expect("remove exact atomic-write test file");
        std::fs::remove_dir(&root).expect("remove empty atomic-write test directory");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn exact_world_delete_removes_only_selected_metadata_and_exact_sidecars() {
        let root = isolated_world_delete_root("exact-success");
        let mut selected = WorldMeta::new("garden".to_owned(), 0xA11C_E001);
        selected.terrain_grammar = TerrainGrammarVersion::V2;
        let metadata = write_world_delete_metadata(&root, &selected);
        let sidecars = world_artifact_roots_at(&root, &selected.name);
        for (index, sidecar) in sidecars.iter().enumerate() {
            std::fs::create_dir(sidecar).expect("create exact sidecar root");
            std::fs::write(sidecar.join(format!("record-{index}.ron")), "authority")
                .expect("write exact sidecar fixture");
        }
        let unrelated = root.join("other.world3");
        std::fs::write(&unrelated, "unrelated authority").expect("write unrelated fixture");

        delete_world_from_native_root(&selected, &root)
            .expect("exact selected world deletion should succeed");

        assert!(!metadata.exists());
        assert!(sidecars.iter().all(|sidecar| !sidecar.exists()));
        assert_eq!(
            std::fs::read_to_string(&unrelated).expect("unrelated file must remain"),
            "unrelated authority"
        );
        remove_exact_test_tree(&root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn world_delete_rejects_other_grammar_claims_without_mutation() {
        let root = isolated_world_delete_root("grammar-collision");
        let mut selected = WorldMeta::new("shared".to_owned(), 0xA11C_E002);
        selected.terrain_grammar = TerrainGrammarVersion::V2;
        let exact = write_world_delete_metadata(&root, &selected);
        let mut legacy = selected.clone();
        legacy.terrain_grammar = TerrainGrammarVersion::V1;
        let legacy_path = write_world_delete_metadata(&root, &legacy);
        let sidecar = world_artifact_roots_at(&root, &selected.name)[0].clone();
        std::fs::create_dir(&sidecar).expect("create collision sidecar fixture");

        let error = delete_world_from_native_root(&selected, &root)
            .expect_err("a second grammar namespace must block deletion");

        assert!(error.contains("terrain-grammar metadata collision"));
        assert!(exact.exists());
        assert!(legacy_path.exists());
        assert!(sidecar.exists());
        remove_exact_test_tree(&root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn world_delete_rejects_foreign_edit_namespace_without_mutation() {
        let root = isolated_world_delete_root("foreign-edit-namespace");
        let mut selected = WorldMeta::new("edits".to_owned(), 0xA11C_E007);
        selected.terrain_grammar = TerrainGrammarVersion::V2;
        let metadata = write_world_delete_metadata(&root, &selected);
        let edits_root = world_artifact_roots_at(&root, &selected.name)[0].clone();
        let exact_namespace = edits_root.join("grammar_v2");
        let foreign_namespace = edits_root.join("grammar_v3");
        std::fs::create_dir(&edits_root).expect("create edit sidecar root");
        std::fs::create_dir(&exact_namespace).expect("create exact edit namespace");
        std::fs::create_dir(&foreign_namespace).expect("create foreign edit namespace");

        let error = delete_world_from_native_root(&selected, &root)
            .expect_err("foreign edit namespace must block exact deletion");

        assert!(error.contains("foreign V3 terrain-grammar namespace"));
        assert!(metadata.exists());
        assert!(exact_namespace.exists());
        assert!(foreign_namespace.exists());
        remove_exact_test_tree(&root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn world_delete_rejects_stale_generation_identity_without_mutation() {
        let root = isolated_world_delete_root("identity-mismatch");
        let mut selected = WorldMeta::new("identity".to_owned(), 0xA11C_E003);
        selected.terrain_grammar = TerrainGrammarVersion::V3;
        let mut persisted = selected.clone();
        persisted.seed = persisted.seed.wrapping_add(1);
        let metadata = write_world_delete_metadata(&root, &persisted);
        let sidecar = world_artifact_roots_at(&root, &selected.name)[1].clone();
        std::fs::create_dir(&sidecar).expect("create identity sidecar fixture");

        let error = delete_world_from_native_root(&selected, &root)
            .expect_err("stale selected identity must block deletion");

        assert!(error.contains("exact selected name and generation identity"));
        assert!(metadata.exists());
        assert!(sidecar.exists());
        remove_exact_test_tree(&root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn sidecar_failures_are_aggregated_and_metadata_authority_is_retained() {
        let root = isolated_world_delete_root("aggregate-errors");
        let mut selected = WorldMeta::new("aggregate".to_owned(), 0xA11C_E004);
        selected.terrain_grammar = TerrainGrammarVersion::V3;
        let metadata = write_world_delete_metadata(&root, &selected);
        let sidecars = world_artifact_roots_at(&root, &selected.name);
        std::fs::create_dir(&sidecars[0]).expect("create first failing sidecar");
        std::fs::create_dir(&sidecars[1]).expect("create second failing sidecar");
        let plan = preflight_native_world_delete(&selected, &root)
            .expect("isolated delete plan should preflight");
        let metadata_remove_called = std::cell::Cell::new(false);

        let error = execute_native_world_delete_plan(
            &selected,
            &plan,
            |path| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("simulated refusal for {}", path.display()),
                ))
            },
            |_| {
                metadata_remove_called.set(true);
                Ok(())
            },
        )
        .expect_err("sidecar failures must fail the whole deletion");

        assert!(error.contains("2 error(s)"));
        assert!(error.contains("aggregate_edits"));
        assert!(error.contains("aggregate_bots"));
        assert!(!metadata_remove_called.get());
        assert!(metadata.exists());
        remove_exact_test_tree(&root);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn world_delete_rejects_a_linked_storage_root_and_preserves_authority() {
        let container = isolated_world_delete_root("linked-root");
        let real_root = container.join("real-saves");
        let linked_root = container.join("linked-saves");
        std::fs::create_dir(&real_root).expect("create real storage root");
        let selected = WorldMeta::new("linked".to_owned(), 0xA11C_E005);
        let metadata = write_world_delete_metadata(&real_root, &selected);
        if let Err(error) = create_directory_link(&real_root, &linked_root) {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                remove_exact_test_tree(&container);
                return;
            }
            panic!("create isolated directory link: {error}");
        }

        let error = delete_world_from_native_root(&selected, &linked_root)
            .expect_err("linked storage root must fail closed");

        assert!(error.contains("world storage root is unsafe"));
        assert!(metadata.exists());
        remove_exact_test_tree(&container);
    }

    #[test]
    fn browser_delete_claim_preflight_requires_one_exact_namespace_and_payload() {
        let mut selected = WorldMeta::new("browser".to_owned(), 0xA11C_E006);
        selected.terrain_grammar = TerrainGrammarVersion::V2;
        let exact_manifests = vec![
            (WorldStorageNamespace::LegacyV1, Vec::new()),
            (WorldStorageNamespace::GrammarV2, vec!["browser".to_owned()]),
            (WorldStorageNamespace::GrammarV3, Vec::new()),
        ];
        let exact_payload = vec![(WorldStorageNamespace::GrammarV2, "browser".to_owned())];
        assert!(
            validate_exact_world_delete_claims(&selected, &exact_manifests, &exact_payload).is_ok()
        );

        let mut colliding_manifests = exact_manifests.clone();
        colliding_manifests[0].1.push("BROWSER".to_owned());
        assert!(validate_exact_world_delete_claims(
            &selected,
            &colliding_manifests,
            &exact_payload
        )
        .is_err());

        let colliding_payloads = vec![
            (WorldStorageNamespace::GrammarV2, "browser".to_owned()),
            (WorldStorageNamespace::GrammarV3, "BROWSER".to_owned()),
        ];
        assert!(validate_exact_world_delete_claims(
            &selected,
            &exact_manifests,
            &colliding_payloads
        )
        .is_err());
    }

    #[test]
    fn malformed_browser_manifest_is_not_normalized_to_an_empty_claim_set() {
        assert_eq!(
            decode_browser_world_manifest(None).expect("missing manifest is an empty new store"),
            Vec::<String>::new()
        );
        assert_eq!(
            decode_browser_world_manifest(Some("[\"Foo\"]")).expect("valid manifest should decode"),
            vec!["Foo".to_string()]
        );
        assert!(decode_browser_world_manifest(Some("[\"unterminated")).is_err());
        assert!(decode_browser_world_manifest(Some("[\"Foo\",\"foo\"]")).is_err());
        assert!(decode_browser_world_manifest(Some("[\"Foo?\"]")).is_err());
        assert!(decode_browser_world_manifest(Some("[\"\"]")).is_err());

        let too_many = (0..=MAX_BROWSER_WORLD_MANIFEST_ENTRIES)
            .map(|index| format!("world_{index:04}"))
            .collect::<Vec<_>>();
        let too_many = ron::ser::to_string(&too_many).expect("serialize oversized manifest");
        assert!(decode_browser_world_manifest(Some(&too_many)).is_err());
    }

    #[test]
    fn generation_identity_rejects_unknown_nested_authority_fields() {
        let future = r#"(
            seed: 7,
            world_profile: Natural,
            scenery_quality: Lush,
            terrain_grammar: V2,
            future_generator_field: 1,
        )"#;
        assert!(ron::from_str::<WorldGenerationIdentity>(future).is_err());
    }

    #[test]
    fn zen_garden_look_applies_lush_readable_world() {
        let mut settings = WorldSettings::default();

        settings.apply_zen_garden_look();

        assert_eq!(settings.theme.style, crate::theme::ThemeStyle::LiquidGlass);
        assert_eq!(settings.theme.color, crate::theme::ThemeColor::Sakura);
        assert_eq!(settings.scenery_quality, SceneryQuality::Lush);
        assert_eq!(settings.weather.preset, WeatherPreset::Clear);
        assert!(settings.weather.fog_density <= 0.24);
        assert_eq!(settings.time_mode, TimeMode::Fixed);
        assert!(
            (12.5..=15.25).contains(&settings.time_of_day),
            "zen look should start in bright editable sakura daylight"
        );
    }

    #[test]
    fn zen_garden_look_uses_light_haze_not_dark_fog() {
        let mut settings = WorldSettings::default();

        settings.apply_zen_garden_look();

        assert!(
            settings.weather.fog_density <= 0.08,
            "zen garden startup should use light atmospheric haze, not a dark fog wall"
        );
    }

    #[test]
    fn zen_garden_look_starts_in_bright_editable_daylight() {
        let mut settings = WorldSettings::default();

        settings.apply_zen_garden_look();

        assert!(
            (12.5..=15.25).contains(&settings.time_of_day),
            "editor worlds should start in readable daylight instead of low-angle dark shadow bands"
        );
    }

    #[test]
    fn new_world_meta_starts_as_lush_zen_garden() {
        let meta = WorldMeta::new("garden".to_string(), 930514);

        assert_eq!(meta.world_profile, WorldProfile::Natural);
        assert_eq!(meta.terrain_grammar, TerrainGrammarVersion::CURRENT);
        assert_eq!(meta.scenery_quality, SceneryQuality::Lush);
        assert_eq!(meta.weather.preset, WeatherPreset::Clear);
        assert!(meta.weather.fog_density <= 0.08);
        assert!(
            (12.5..=15.25).contains(&meta.time_of_day),
            "new worlds should open in bright Zen editing light, not dark low-angle shadows"
        );
    }

    #[test]
    fn astral_world_meta_spawns_in_its_persisted_showcase_profile() {
        let meta = WorldMeta::new_with_profile(
            "frontier".to_string(),
            12345,
            WorldProfile::AstralFrontier,
        );
        let generator =
            crate::terrain::TerrainGenerator::new(meta.seed).with_world_profile(meta.world_profile);
        let x = meta.player_pos[0].floor() as i32;
        let z = meta.player_pos[2].floor() as i32;

        assert_eq!(meta.world_profile, WorldProfile::AstralFrontier);
        assert!(generator.biome_at(x, z).is_neon_showcase());
        assert!(meta.player_pos[1] > crate::terrain::WATER_LEVEL as f32 + 20.0);
    }

    #[test]
    fn legacy_world_without_profile_deserializes_as_natural() {
        let meta = WorldMeta::new("legacy".to_string(), 44);
        let encoded = ron::ser::to_string_pretty(&meta, ron::ser::PrettyConfig::default())
            .expect("world meta should serialize");
        let legacy = encoded
            .lines()
            .filter(|line| !line.contains("world_profile"))
            .collect::<Vec<_>>()
            .join("\n");
        let decoded: WorldMeta = ron::from_str(&legacy).expect("legacy save should migrate");

        assert_eq!(decoded.world_profile, WorldProfile::Natural);
    }

    #[test]
    fn new_defaults_use_v3_but_missing_persisted_grammar_is_v1() {
        assert_eq!(
            WorldSettings::default().terrain_grammar,
            TerrainGrammarVersion::V3
        );
        assert_eq!(
            WorldMeta::new("new".to_string(), 42).terrain_grammar,
            TerrainGrammarVersion::V3
        );

        let current = WorldMeta::new("legacy".to_string(), 42);
        let encoded = ron::ser::to_string_pretty(&current, ron::ser::PrettyConfig::default())
            .expect("world meta should serialize");
        let legacy = encoded
            .lines()
            .filter(|line| !line.contains("terrain_grammar"))
            .collect::<Vec<_>>()
            .join("\n");
        let decoded: WorldMeta = ron::from_str(&legacy).expect("legacy world should parse as V1");
        assert_eq!(decoded.terrain_grammar, TerrainGrammarVersion::V1);
    }

    #[test]
    fn unknown_terrain_grammar_is_rejected_instead_of_guessed() {
        let meta = WorldMeta::new("future".to_string(), 42);
        let encoded = ron::ser::to_string_pretty(&meta, ron::ser::PrettyConfig::default())
            .expect("world meta should serialize");
        let future = encoded.replace("terrain_grammar: V3", "terrain_grammar: V999");
        assert_ne!(future, encoded, "test fixture must replace the enum value");
        assert!(ron::from_str::<WorldMeta>(&future).is_err());
    }

    #[test]
    fn world_storage_namespace_and_persisted_grammar_must_agree() {
        assert_eq!(
            WorldStorageNamespace::from_extension(Some("RON")),
            Some(WorldStorageNamespace::LegacyV1)
        );
        assert_eq!(
            WorldStorageNamespace::from_extension(Some("WORLD2")),
            Some(WorldStorageNamespace::GrammarV2)
        );
        assert_eq!(
            WorldStorageNamespace::from_extension(Some("WORLD3")),
            Some(WorldStorageNamespace::GrammarV3)
        );

        let mut v1 = WorldMeta::new("legacy".to_string(), 7);
        v1.terrain_grammar = TerrainGrammarVersion::V1;
        let v1_text = ron::ser::to_string(&v1).expect("V1 world should serialize");
        assert!(decode_world_meta_in_namespace(&v1_text, WorldStorageNamespace::LegacyV1).is_ok());
        assert!(
            decode_world_meta_in_namespace(&v1_text, WorldStorageNamespace::GrammarV2).is_err()
        );

        let mut v2 = WorldMeta::new("v2".to_string(), 7);
        v2.terrain_grammar = TerrainGrammarVersion::V2;
        let v2_text = ron::ser::to_string(&v2).expect("V2 world should serialize");
        assert!(decode_world_meta_in_namespace(&v2_text, WorldStorageNamespace::GrammarV2).is_ok());
        assert!(decode_world_meta_in_namespace(&v2_text, WorldStorageNamespace::LegacyV1).is_err());
        assert!(
            decode_world_meta_in_namespace(&v2_text, WorldStorageNamespace::GrammarV3).is_err()
        );

        let v3 = WorldMeta::new("current".to_string(), 7);
        let v3_text = ron::ser::to_string(&v3).expect("V3 world should serialize");
        assert!(decode_world_meta_in_namespace(&v3_text, WorldStorageNamespace::GrammarV3).is_ok());
        assert!(decode_world_meta_in_namespace(&v3_text, WorldStorageNamespace::LegacyV1).is_err());
        assert!(
            decode_world_meta_in_namespace(&v3_text, WorldStorageNamespace::GrammarV2).is_err()
        );

        assert_eq!(
            ron::ser::to_string(&TerrainGrammarVersion::V1).expect("serialize V1"),
            "V1"
        );
        assert_eq!(
            ron::ser::to_string(&TerrainGrammarVersion::V2).expect("serialize V2"),
            "V2"
        );

        #[cfg(not(target_arch = "wasm32"))]
        {
            assert_eq!(
                world_file_for_grammar("same", TerrainGrammarVersion::V1)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("ron")
            );
            assert_eq!(
                world_file_for_grammar("same", TerrainGrammarVersion::V2)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("world2")
            );
            assert_eq!(
                world_file_for_grammar("same", TerrainGrammarVersion::V3)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("world3")
            );
            assert_ne!(
                world_file_for_grammar("same", TerrainGrammarVersion::V1),
                world_file_for_grammar("same", TerrainGrammarVersion::V2)
            );
            assert_ne!(
                world_file_for_grammar("same", TerrainGrammarVersion::V2),
                world_file_for_grammar("same", TerrainGrammarVersion::V3)
            );
        }
    }

    #[test]
    fn generation_identity_includes_every_byte_authority_field() {
        let identity = WorldGenerationIdentity {
            seed: u32::MAX,
            world_profile: WorldProfile::AstralFrontier,
            scenery_quality: SceneryQuality::Off,
            terrain_grammar: TerrainGrammarVersion::V1,
        };
        let meta = WorldMeta::new_with_identity("identity".to_string(), identity);
        assert_eq!(meta.generation_identity(), identity);

        let settings = WorldSettings {
            seed: identity.seed,
            world_profile: identity.world_profile,
            scenery_quality: identity.scenery_quality,
            terrain_grammar: identity.terrain_grammar,
            ..Default::default()
        };
        assert_eq!(settings.generation_identity(), identity);
    }

    #[test]
    fn legacy_neon_presentation_gets_the_astral_terrain_it_searches_for() {
        let mut settings = WorldSettings::default();
        settings.world_profile = WorldProfile::Natural;
        settings.visual_preset = VisualPreset::NeonShuttle;

        assert_eq!(
            settings.effective_world_profile(),
            WorldProfile::AstralFrontier
        );
    }

    #[test]
    fn runtime_safety_preserves_user_selected_theme_color() {
        let mut settings = WorldSettings::default();
        settings.theme.color = crate::theme::ThemeColor::Green;

        settings.normalize_runtime_safety();

        assert_eq!(settings.theme.color, crate::theme::ThemeColor::Green);
    }
}
