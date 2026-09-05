//! NeuroCore runtime budget controller.
//!
//! WorldSettings stores the user's desired ceilings. NeuroCore turns
//! current telemetry plus mode intent into the effective per-frame budget
//! used by streaming, meshing, weather, HUD, and editor readouts.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::settings::WorldSettings;
use crate::world::{ChunkStreamer, VoxelWorld, WorldSet};

pub struct NeuroCorePlugin;

impl Plugin for NeuroCorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NeuroCore>()
            .init_resource::<RuntimeBudget>()
            .add_systems(Update, update_neurocore.in_set(WorldSet::NeuroCore));
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RuntimeProfile {
    #[default]
    Auto,
    LowSpec,
    Balanced,
    Cinematic,
    Benchmark,
}

impl RuntimeProfile {
    pub const ALL: [RuntimeProfile; 5] = [
        RuntimeProfile::Auto,
        RuntimeProfile::LowSpec,
        RuntimeProfile::Balanced,
        RuntimeProfile::Cinematic,
        RuntimeProfile::Benchmark,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RuntimeProfile::Auto => "AUTO",
            RuntimeProfile::LowSpec => "LOW",
            RuntimeProfile::Balanced => "BAL",
            RuntimeProfile::Cinematic => "CINE",
            RuntimeProfile::Benchmark => "BENCH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeIntent {
    #[default]
    Explore,
    Build,
    Combat,
    Editor,
    Menu,
}

impl RuntimeIntent {
    pub fn from_mode(mode: ActiveMode) -> Self {
        match mode {
            ActiveMode::BuildPicker { .. } | ActiveMode::BuildLive { .. } => Self::Build,
            ActiveMode::ShipPlacement { .. } => Self::Build,
            ActiveMode::ShipFlight { .. } => Self::Combat,
            ActiveMode::Editor { .. } => Self::Editor,
            ActiveMode::Inventory | ActiveMode::Paused | ActiveMode::CommandPalette => Self::Menu,
            ActiveMode::Combat => Self::Combat,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RuntimeIntent::Explore => "Explore",
            RuntimeIntent::Build => "Build",
            RuntimeIntent::Combat => "Combat",
            RuntimeIntent::Editor => "Editor",
            RuntimeIntent::Menu => "Menu",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QualityState {
    Critical,
    Throttled,
    #[default]
    Nominal,
    Expanding,
    Benchmark,
}

impl QualityState {
    pub fn label(self) -> &'static str {
        match self {
            QualityState::Critical => "CRITICAL",
            QualityState::Throttled => "THROTTLED",
            QualityState::Nominal => "NOMINAL",
            QualityState::Expanding => "EXPANDING",
            QualityState::Benchmark => "BENCHMARK",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeTelemetry {
    pub fps: f32,
    pub frame_ms: f32,
    pub target_fps: f32,
    pub loaded_chunks: usize,
    pub mesh_entities: usize,
    pub pending_terrain: usize,
    pub pending_meshes: usize,
    pub dirty_chunks: usize,
    pub intent: RuntimeIntent,
    pub queue_pressure: f32,
    pub frame_pressure: f32,
    pub stream_elapsed: f32,
}

impl Default for RuntimeTelemetry {
    fn default() -> Self {
        Self {
            fps: 0.0,
            frame_ms: 0.0,
            target_fps: 60.0,
            loaded_chunks: 0,
            mesh_entities: 0,
            pending_terrain: 0,
            pending_meshes: 0,
            dirty_chunks: 0,
            intent: RuntimeIntent::Explore,
            queue_pressure: 0.0,
            frame_pressure: 0.0,
            stream_elapsed: 0.0,
        }
    }
}

impl RuntimeTelemetry {
    fn queue_total(&self) -> usize {
        self.pending_terrain + self.pending_meshes + self.dirty_chunks
    }

    fn resident_total(&self) -> usize {
        self.loaded_chunks + self.mesh_entities
    }
}

#[derive(Resource, Debug, Clone)]
pub struct RuntimeBudget {
    pub enabled: bool,
    pub profile: RuntimeProfile,
    pub intent: RuntimeIntent,
    pub quality: QualityState,
    pub target_render_distance: i32,
    pub render_distance: i32,
    pub chunks_per_frame: u32,
    pub meshes_per_frame: u32,
    pub mesh_applies_per_frame: u32,
    pub max_in_flight_terrain: u32,
    pub max_in_flight_meshes: u32,
    pub shadow_radius: i32,
    pub weather_fx_scale: f32,
    pub weapon_fx_scale: f32,
    pub update_cadence: f32,
    pub fps: f32,
    pub frame_ms: f32,
    pub queue_pressure: f32,
    pub frame_pressure: f32,
    pub status: String,
    /// First ~2s of a session: fill the spawn disc at the card's full
    /// job budget instead of the LowSpec / smoothness throttle.
    pub startup_fill: bool,
}

const SMOOTH_MAX_CHUNKS_PER_FRAME: u32 = 10;
const SMOOTH_MAX_MESHES_PER_FRAME: u32 = 10;
const SMOOTH_MAX_MESH_APPLIES_PER_FRAME: u32 = 8;
const STARTUP_MAX_CHUNKS_PER_FRAME: u32 = 18;
const STARTUP_MAX_MESHES_PER_FRAME: u32 = 16;
const STARTUP_MAX_MESH_APPLIES_PER_FRAME: u32 = 16;
const SMOOTH_MAX_IN_FLIGHT_TERRAIN: u32 = 96;
const SMOOTH_MAX_IN_FLIGHT_MESHES: u32 = 80;
/// First seconds of a session fill the spawn disc at the card budget
/// instead of the LowSpec / smoothness throttle.
const STARTUP_FILL_SECONDS: f32 = 2.2;

impl Default for RuntimeBudget {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: RuntimeProfile::Auto,
            intent: RuntimeIntent::Explore,
            quality: QualityState::Nominal,
            target_render_distance: 16,
            render_distance: 16,
            chunks_per_frame: 8,
            meshes_per_frame: 8,
            mesh_applies_per_frame: 8,
            max_in_flight_terrain: 64,
            max_in_flight_meshes: 48,
            shadow_radius: 8,
            weather_fx_scale: 1.0,
            weapon_fx_scale: 1.0,
            update_cadence: 0.5,
            fps: 0.0,
            frame_ms: 0.0,
            queue_pressure: 0.0,
            frame_pressure: 0.0,
            status: "warming up".into(),
            startup_fill: false,
        }
    }
}

impl RuntimeBudget {
    pub fn from_settings(settings: &WorldSettings) -> Self {
        let target = (settings.render_distance as i32).max(2);
        let mut out = Self {
            enabled: settings.neurocore_enabled,
            profile: settings.runtime_profile,
            target_render_distance: target,
            render_distance: target,
            chunks_per_frame: settings.chunks_per_frame.max(1),
            meshes_per_frame: settings.meshes_per_frame.max(1),
            mesh_applies_per_frame: settings.mesh_applies_per_frame.max(1),
            max_in_flight_terrain: settings.max_in_flight_terrain.max(1),
            max_in_flight_meshes: settings.max_in_flight_meshes.max(1),
            shadow_radius: target.min(10).max(4),
            status: "direct".into(),
            ..Self::default()
        };
        out.clamp_to_settings(settings);
        out
    }

    fn clamp_to_settings(&mut self, settings: &WorldSettings) {
        let target = (settings.render_distance as i32).max(2);
        self.target_render_distance = target;
        self.render_distance = self.render_distance.clamp(2, target);
        let max_chunks = if self.startup_fill {
            STARTUP_MAX_CHUNKS_PER_FRAME
        } else {
            SMOOTH_MAX_CHUNKS_PER_FRAME
        };
        let max_meshes = if self.startup_fill {
            STARTUP_MAX_MESHES_PER_FRAME
        } else {
            SMOOTH_MAX_MESHES_PER_FRAME
        };
        let max_applies = if self.startup_fill {
            STARTUP_MAX_MESH_APPLIES_PER_FRAME
        } else {
            SMOOTH_MAX_MESH_APPLIES_PER_FRAME
        };
        self.chunks_per_frame = self.chunks_per_frame.clamp(
            1,
            settings.chunks_per_frame.max(1).min(max_chunks),
        );
        self.meshes_per_frame = self.meshes_per_frame.clamp(
            1,
            settings.meshes_per_frame.max(1).min(max_meshes),
        );
        self.mesh_applies_per_frame = self.mesh_applies_per_frame.clamp(
            1,
            settings.mesh_applies_per_frame.max(1).min(max_applies),
        );
        self.max_in_flight_terrain = self.max_in_flight_terrain.clamp(
            1,
            settings
                .max_in_flight_terrain
                .max(1)
                .min(SMOOTH_MAX_IN_FLIGHT_TERRAIN),
        );
        self.max_in_flight_meshes = self.max_in_flight_meshes.clamp(
            1,
            settings
                .max_in_flight_meshes
                .max(1)
                .min(SMOOTH_MAX_IN_FLIGHT_MESHES),
        );
        self.shadow_radius = self.shadow_radius.clamp(2, self.render_distance.max(2));
        self.weather_fx_scale = self.weather_fx_scale.clamp(0.0, 1.0);
        self.weapon_fx_scale = self.weapon_fx_scale.clamp(0.0, 1.0);
    }
}

#[derive(Resource, Debug, Clone)]
pub struct NeuroCore {
    pub enabled: bool,
    pub profile: RuntimeProfile,
    pub intent: RuntimeIntent,
    pub quality: QualityState,
    pub telemetry: RuntimeTelemetry,
    pub status: String,
    effective_render_distance: i32,
    sample_timer: f32,
    stable_samples: u8,
    hold_samples: u8,
}

impl Default for NeuroCore {
    fn default() -> Self {
        Self {
            enabled: true,
            profile: RuntimeProfile::Auto,
            intent: RuntimeIntent::Explore,
            quality: QualityState::Nominal,
            telemetry: RuntimeTelemetry::default(),
            status: "warming up".into(),
            effective_render_distance: 0,
            sample_timer: 0.0,
            stable_samples: 0,
            hold_samples: 0,
        }
    }
}

impl NeuroCore {
    pub fn update_budget(
        &mut self,
        settings: &WorldSettings,
        telemetry: RuntimeTelemetry,
        dt: f32,
    ) -> RuntimeBudget {
        self.enabled = settings.neurocore_enabled;
        self.profile = settings.runtime_profile;
        self.intent = telemetry.intent;
        self.telemetry = telemetry;

        let mut budget = if !self.enabled {
            let mut direct = RuntimeBudget::from_settings(settings);
            direct.enabled = false;
            direct.intent = self.intent;
            direct.quality = QualityState::Nominal;
            direct.status = "NeuroCore disabled".into();
            self.quality = direct.quality;
            self.status = direct.status.clone();
            return direct;
        } else if self.profile == RuntimeProfile::Benchmark {
            let mut raw = RuntimeBudget::from_settings(settings);
            raw.profile = RuntimeProfile::Benchmark;
            raw.intent = self.intent;
            raw.quality = QualityState::Benchmark;
            raw.status = "benchmark raw budget".into();
            raw.fps = self.telemetry.fps;
            raw.frame_ms = self.telemetry.frame_ms;
            raw.queue_pressure = self.telemetry.queue_pressure;
            raw.frame_pressure = self.telemetry.frame_pressure;
            self.quality = raw.quality;
            self.status = raw.status.clone();
            return raw;
        } else {
            self.profile_budget(settings, dt)
        };

        budget.profile = self.profile;
        budget.intent = self.intent;
        budget.enabled = self.enabled;
        budget.fps = self.telemetry.fps;
        budget.frame_ms = self.telemetry.frame_ms;
        budget.queue_pressure = self.telemetry.queue_pressure;
        budget.frame_pressure = self.telemetry.frame_pressure;
        budget.clamp_to_settings(settings);
        self.quality = budget.quality;
        self.status = budget.status.clone();
        budget
    }

    fn profile_budget(&mut self, settings: &WorldSettings, dt: f32) -> RuntimeBudget {
        let target = (settings.render_distance as i32).max(2);
        let startup_fill = self.telemetry.stream_elapsed > 0.05
            && self.telemetry.stream_elapsed < STARTUP_FILL_SECONDS;
        let (
            rd,
            mut job_scale,
            mut upload_scale,
            mut weather_scale,
            mut weapon_scale,
            quality,
            status,
        ) = match self.profile {
            RuntimeProfile::LowSpec => {
                let fast = settings.graphics == crate::settings::GraphicsMode::Fast;
                // Fast keeps a 16-chunk settled horizon so Vega isn't
                // drawing the Cinematic 24-chunk disc. Spawn fill still
                // ramps 5→8→16 via the streamer; this is only the cap.
                let rd_cap = if fast { 12 } else { 24 };
                (
                target.min(rd_cap).max(6),
                if startup_fill { 1.0 } else { 0.52 },
                if startup_fill { 1.0 } else { 0.55 },
                0.25,
                0.65,
                if startup_fill {
                    QualityState::Expanding
                } else {
                    QualityState::Throttled
                },
                String::from(if startup_fill {
                    "low-spec spawn fill"
                } else if fast {
                    "low-spec settled horizon"
                } else {
                    "low-spec fixed budget"
                }),
            )
            },
            RuntimeProfile::Balanced => (
                target.min(32).max(6),
                0.75,
                0.8,
                0.75,
                1.0,
                QualityState::Nominal,
                String::from("balanced fixed budget"),
            ),
            RuntimeProfile::Cinematic => (
                target,
                0.9,
                0.9,
                1.0,
                1.0,
                QualityState::Nominal,
                String::from("cinematic quality budget"),
            ),
            RuntimeProfile::Auto => {
                let rd = self.auto_render_distance(settings, dt);
                let rd_ratio = rd as f32 / target.max(1) as f32;
                let job_scale = rd_ratio.sqrt().clamp(0.25, 1.0);
                let upload_scale = (0.45 + rd_ratio * 0.55).clamp(0.35, 1.0);
                let pressure = self
                    .telemetry
                    .frame_pressure
                    .max(self.telemetry.queue_pressure);
                if pressure >= 0.85 {
                    (
                        rd,
                        job_scale,
                        upload_scale,
                        0.20,
                        0.50,
                        QualityState::Critical,
                        String::from("auto hard throttle"),
                    )
                } else if pressure >= 0.55 || rd < target {
                    let status = if rd < target {
                        String::from("auto holding reduced horizon")
                    } else {
                        String::from("auto throttling queues")
                    };
                    (
                        rd,
                        job_scale,
                        upload_scale,
                        0.45,
                        0.75,
                        QualityState::Throttled,
                        status,
                    )
                } else if self.stable_samples > 0 && rd < target {
                    (
                        rd,
                        job_scale,
                        upload_scale,
                        1.0,
                        1.0,
                        QualityState::Expanding,
                        String::from("auto expanding slowly"),
                    )
                } else {
                    (
                        rd,
                        job_scale,
                        upload_scale,
                        1.0,
                        1.0,
                        QualityState::Nominal,
                        String::from("auto full budget"),
                    )
                }
            }
            RuntimeProfile::Benchmark => unreachable!("handled before profile_budget"),
        };
        let status = format!(
            "{status} q{} res{}",
            self.telemetry.queue_total(),
            self.telemetry.resident_total()
        );

        let (
            intent_job_scale,
            intent_mesh_scale,
            intent_upload_scale,
            intent_weather,
            intent_weapon,
        ) = self.intent_modifiers(quality);
        let mesh_scale = (job_scale * intent_mesh_scale).clamp(0.15, 1.0);
        job_scale = (job_scale * intent_job_scale).clamp(0.15, 1.0);
        upload_scale = (upload_scale * intent_upload_scale).clamp(0.15, 1.0);
        weather_scale = (weather_scale * intent_weather).clamp(0.0, 1.0);
        weapon_scale = (weapon_scale * intent_weapon).clamp(0.0, 1.0);

        let mut budget = RuntimeBudget {
            enabled: true,
            profile: self.profile,
            intent: self.intent,
            quality,
            target_render_distance: target,
            render_distance: rd,
            chunks_per_frame: scaled_budget(settings.chunks_per_frame, job_scale, 1),
            meshes_per_frame: scaled_budget(settings.meshes_per_frame, mesh_scale, 1),
            mesh_applies_per_frame: scaled_budget(settings.mesh_applies_per_frame, upload_scale, 1),
            max_in_flight_terrain: scaled_budget(settings.max_in_flight_terrain, job_scale, 8),
            max_in_flight_meshes: scaled_budget(settings.max_in_flight_meshes, mesh_scale, 8),
            shadow_radius: shadow_radius_for(self.profile, rd),
            weather_fx_scale: weather_scale,
            weapon_fx_scale: weapon_scale,
            update_cadence: 0.5,
            fps: self.telemetry.fps,
            frame_ms: self.telemetry.frame_ms,
            queue_pressure: self.telemetry.queue_pressure,
            frame_pressure: self.telemetry.frame_pressure,
            status,
            startup_fill,
        };
        budget.clamp_to_settings(settings);
        budget
    }

    fn auto_render_distance(&mut self, settings: &WorldSettings, dt: f32) -> i32 {
        let target = (settings.render_distance as i32).max(2);
        let explore_floor = if self.telemetry.stream_elapsed > 0.05
            && self.telemetry.stream_elapsed < 1.2
        {
            4
        } else if self.telemetry.stream_elapsed > 0.05
            && self.telemetry.stream_elapsed < 2.5
        {
            8
        } else {
            (target / 3).clamp(8, 18).min(target)
        };
        let floor = match self.intent {
            RuntimeIntent::Combat => target.min(12).max(6),
            RuntimeIntent::Build => (target / 4).clamp(8, 16).min(target),
            RuntimeIntent::Editor => (target / 5).clamp(6, 12).min(target),
            RuntimeIntent::Menu => target.min(6).max(3),
            RuntimeIntent::Explore => explore_floor,
        };

        if self.effective_render_distance <= 0 {
            // Start with a spawn ring so the first frames fill ground
            // under the player instead of a 32-chunk empty sky.
            self.effective_render_distance = target.min(5).max(4);
        }
        if self.effective_render_distance > target {
            self.effective_render_distance = target;
        }
        if matches!(self.intent, RuntimeIntent::Build | RuntimeIntent::Editor) {
            self.effective_render_distance = self
                .effective_render_distance
                .min(target.max(floor).min((target * 3 / 4).max(floor)));
        }
        if matches!(self.intent, RuntimeIntent::Menu) {
            self.effective_render_distance = self
                .effective_render_distance
                .min(target.max(floor).min((target / 2).max(floor)));
        }

        self.sample_timer += dt.max(0.0);
        if self.sample_timer < 0.5 {
            return self.effective_render_distance.clamp(floor, target);
        }
        self.sample_timer = 0.0;

        let fps = self.telemetry.fps;
        let target_fps = self.telemetry.target_fps.max(15.0);
        let pressure = self
            .telemetry
            .frame_pressure
            .max(self.telemetry.queue_pressure);
        let hard = (fps > 0.0 && fps < target_fps * 0.70) || pressure >= 0.90;
        let soft = (fps > 0.0 && fps < target_fps * 0.86) || pressure >= 0.65;
        let stable = (fps <= 0.0 || fps >= target_fps * 0.94) && pressure < 0.35;
        let held = self.effective_render_distance;

        if hard {
            self.stable_samples = 0;
            let step = (target / 8).max(4);
            self.effective_render_distance = (self.effective_render_distance - step).max(floor);
        } else if soft {
            self.stable_samples = 0;
            self.effective_render_distance = (self.effective_render_distance - 2).max(floor);
        } else if stable && self.effective_render_distance < target {
            self.stable_samples = self.stable_samples.saturating_add(1);
            let fast_recovery = pressure < 0.18 && (fps <= 0.0 || fps >= target_fps * 0.98);
            let required_samples = if fast_recovery { 1 } else { 2 };
            if self.stable_samples >= required_samples {
                let gap = target - self.effective_render_distance;
                let step = if fast_recovery {
                    if gap > 24 {
                        4
                    } else if gap > 12 {
                        3
                    } else if gap > 6 {
                        2
                    } else {
                        1
                    }
                } else {
                    1
                };
                self.effective_render_distance =
                    (self.effective_render_distance + step).min(target);
                self.stable_samples = 0;
            }
        } else {
            self.stable_samples = 0;
        }

        // After the spawn ramp, a loaded disc that keeps hunting RD
        // re-streams rings and hitchs. Hold unless FPS is critically low
        // or clearly has headroom to grow.
        if self.telemetry.stream_elapsed > 6.0 && self.hold_samples >= 4 {
            let critical = fps > 0.0 && fps < target_fps * 0.55 && hard;
            let grow = stable
                && pressure < 0.18
                && (fps <= 0.0 || fps >= target_fps * 1.05)
                && self.effective_render_distance > held;
            if !critical && !grow {
                self.effective_render_distance = held;
            }
        }
        if self.effective_render_distance == held {
            self.hold_samples = self.hold_samples.saturating_add(1);
        } else {
            self.hold_samples = 0;
        }

        self.effective_render_distance.clamp(floor, target)
    }

    fn intent_modifiers(&self, quality: QualityState) -> (f32, f32, f32, f32, f32) {
        match self.intent {
            RuntimeIntent::Build => (0.55, 1.0, 1.0, 0.35, 0.15),
            RuntimeIntent::Editor => (0.40, 0.75, 0.70, 0.20, 0.10),
            RuntimeIntent::Menu => (0.25, 0.50, 0.45, 0.10, 0.05),
            RuntimeIntent::Combat => {
                if quality == QualityState::Critical {
                    (0.65, 0.90, 0.85, 0.45, 0.60)
                } else {
                    (0.80, 1.0, 1.0, 0.70, 1.0)
                }
            }
            RuntimeIntent::Explore => (1.0, 1.0, 1.0, 1.0, 1.0),
        }
    }
}

fn update_neurocore(
    time: Res<Time>,
    diagnostics: Option<Res<DiagnosticsStore>>,
    settings: Res<WorldSettings>,
    mode: Option<Res<ModeContext>>,
    game_state: Option<Res<State<GameState>>>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    mut core: ResMut<NeuroCore>,
    mut budget: ResMut<RuntimeBudget>,
) {
    let fps = diagnostics
        .as_deref()
        .and_then(|d| d.get(&FrameTimeDiagnosticsPlugin::FPS))
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let frame_ms = diagnostics
        .as_deref()
        .and_then(|d| d.get(&FrameTimeDiagnosticsPlugin::FRAME_TIME))
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let intent = runtime_intent(mode.as_deref(), game_state.as_deref());
    let target_fps = settings.target_fps.clamp(15.0, 240.0);
    let pending_terrain = streamer.pending_terrain.len();
    let pending_meshes = streamer.pending_meshes.len();
    let dirty_chunks = streamer.dirty_queue.len() + world.edit_dirty_chunks.len();
    let queue_pressure = queue_pressure(&settings, pending_terrain, pending_meshes, dirty_chunks);
    let frame_pressure = if fps <= 0.0 {
        0.0
    } else {
        ((target_fps - fps) / target_fps).clamp(0.0, 1.0)
    };

    let telemetry = RuntimeTelemetry {
        fps,
        frame_ms,
        target_fps,
        loaded_chunks: world.chunks.len(),
        mesh_entities: streamer.entities.len(),
        pending_terrain,
        pending_meshes,
        dirty_chunks,
        intent,
        queue_pressure,
        frame_pressure,
        stream_elapsed: streamer.stream_elapsed,
    };
    *budget = core.update_budget(&settings, telemetry, time.delta_seconds());
}

fn runtime_intent(
    mode: Option<&ModeContext>,
    game_state: Option<&State<GameState>>,
) -> RuntimeIntent {
    if let Some(mode) = mode {
        return RuntimeIntent::from_mode(mode.mode);
    }
    match game_state.map(|s| s.get()) {
        Some(GameState::MainMenu | GameState::Paused) => RuntimeIntent::Menu,
        Some(GameState::InGame) => RuntimeIntent::Explore,
        None => RuntimeIntent::Explore,
    }
}

fn queue_pressure(
    settings: &WorldSettings,
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
) -> f32 {
    let terrain_cap = settings
        .max_in_flight_terrain
        .max(1)
        .min(SMOOTH_MAX_IN_FLIGHT_TERRAIN);
    let mesh_cap = settings
        .max_in_flight_meshes
        .max(1)
        .min(SMOOTH_MAX_IN_FLIGHT_MESHES);
    let terrain = pending_terrain as f32 / terrain_cap as f32;
    let meshes = pending_meshes as f32 / mesh_cap as f32;
    let dirty = dirty_chunks as f32 / 2_000.0;
    terrain.max(meshes).max(dirty).clamp(0.0, 1.25)
}

fn scaled_budget(max: u32, scale: f32, min: u32) -> u32 {
    let max = max.max(1);
    let min = min.max(1).min(max);
    ((max as f32 * scale).round() as u32).clamp(min, max)
}

fn shadow_radius_for(profile: RuntimeProfile, rd: i32) -> i32 {
    let cap = match profile {
        RuntimeProfile::LowSpec => 6,
        RuntimeProfile::Balanced | RuntimeProfile::Auto => 10,
        RuntimeProfile::Cinematic | RuntimeProfile::Benchmark => 14,
    };
    rd.min(cap).max(3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbelt::ToolbeltTool;

    fn telemetry(fps: f32, queue_pressure: f32, intent: RuntimeIntent) -> RuntimeTelemetry {
        RuntimeTelemetry {
            fps,
            target_fps: 60.0,
            intent,
            queue_pressure,
            frame_pressure: if fps <= 0.0 {
                0.0
            } else {
                ((60.0 - fps) / 60.0).clamp(0.0, 1.0)
            },
            ..RuntimeTelemetry::default()
        }
    }

    #[test]
    fn runtime_profile_defaults_deserialize_from_old_settings() {
        let text = r#"(
            seed: 12345,
            render_distance: 50,
            vertical_chunks: 8,
            chunks_per_frame: 16,
            meshes_per_frame: 16,
            time_mode: Cycle,
            time_of_day: 10.0,
            cycle_speed: 0.01,
            graphics: Balanced,
            fov_deg: 75.0,
            weather: (
                preset: Clear,
                rain_intensity: 0.0,
                snow_intensity: 0.0,
                fog_density: 0.0,
                wind_x: 0.0,
                wind_z: 0.0,
            ),
        )"#;
        let settings: WorldSettings = ron::from_str(text).expect("old settings should load");
        assert!(settings.neurocore_enabled);
        assert_eq!(settings.runtime_profile, RuntimeProfile::Auto);
        assert_eq!(settings.target_fps, 60.0);
    }

    #[test]
    fn intent_maps_from_mode_context() {
        assert_eq!(
            RuntimeIntent::from_mode(ActiveMode::BuildLive {
                tool: ToolbeltTool::DrawRect
            }),
            RuntimeIntent::Build
        );
        assert_eq!(
            RuntimeIntent::from_mode(ActiveMode::Editor {
                tab: crate::editor::EditorTab::System
            }),
            RuntimeIntent::Editor
        );
        assert_eq!(
            RuntimeIntent::from_mode(ActiveMode::Combat),
            RuntimeIntent::Combat
        );
        assert_eq!(
            RuntimeIntent::from_mode(ActiveMode::Paused),
            RuntimeIntent::Menu
        );
    }

    #[test]
    fn auto_budget_throttles_under_low_fps_and_queue_pressure() {
        let mut settings = WorldSettings::default();
        settings.render_distance = 60;
        settings.runtime_profile = RuntimeProfile::Auto;
        let mut core = NeuroCore::default();
        let first =
            core.update_budget(&settings, telemetry(20.0, 1.0, RuntimeIntent::Explore), 0.6);
        assert!(first.render_distance < settings.render_distance as i32);
        assert!(first.chunks_per_frame < settings.chunks_per_frame);
        assert!(matches!(
            first.quality,
            QualityState::Critical | QualityState::Throttled
        ));
    }

    #[test]
    fn auto_budget_expands_slowly_after_stable_fps() {
        let mut settings = WorldSettings::default();
        settings.render_distance = 40;
        settings.runtime_profile = RuntimeProfile::Auto;
        let mut core = NeuroCore::default();
        let throttled =
            core.update_budget(&settings, telemetry(22.0, 1.0, RuntimeIntent::Explore), 0.6);
        let held = core.update_budget(&settings, telemetry(60.0, 0.0, RuntimeIntent::Explore), 0.6);
        let expanded =
            core.update_budget(&settings, telemetry(60.0, 0.0, RuntimeIntent::Explore), 0.6);
        assert!(held.render_distance >= throttled.render_distance);
        assert!(expanded.render_distance > held.render_distance);
        assert!(expanded.render_distance <= settings.render_distance as i32);
    }

    #[test]
    fn user_maximums_are_never_exceeded() {
        let mut settings = WorldSettings::default();
        settings.render_distance = 18;
        settings.chunks_per_frame = 3;
        settings.meshes_per_frame = 4;
        settings.mesh_applies_per_frame = 5;
        settings.max_in_flight_terrain = 20;
        settings.max_in_flight_meshes = 12;
        settings.runtime_profile = RuntimeProfile::Cinematic;
        let mut core = NeuroCore::default();
        let budget = core.update_budget(
            &settings,
            telemetry(120.0, 0.0, RuntimeIntent::Explore),
            0.6,
        );
        assert!(budget.render_distance <= 18);
        assert!(budget.chunks_per_frame <= 3);
        assert!(budget.meshes_per_frame <= 4);
        assert!(budget.mesh_applies_per_frame <= 5);
        assert!(budget.max_in_flight_terrain <= 20);
        assert!(budget.max_in_flight_meshes <= 12);
    }

    #[test]
    fn benchmark_uses_raw_budget() {
        let mut settings = WorldSettings::default();
        settings.runtime_profile = RuntimeProfile::Benchmark;
        let mut core = NeuroCore::default();
        let budget =
            core.update_budget(&settings, telemetry(12.0, 1.0, RuntimeIntent::Combat), 0.6);
        assert_eq!(budget.render_distance, settings.render_distance as i32);
        assert_eq!(budget.chunks_per_frame, settings.chunks_per_frame);
        assert_eq!(budget.meshes_per_frame, settings.meshes_per_frame);
        assert_eq!(
            budget.mesh_applies_per_frame,
            settings.mesh_applies_per_frame
        );
        assert_eq!(budget.quality, QualityState::Benchmark);
    }

    #[test]
    fn lowspec_startup_fill_uses_the_card_job_budget() {
        let mut settings = WorldSettings::default();
        settings.apply_world_mode_card(crate::settings::WorldModeCard::FastLaptop);
        let mut core = NeuroCore::default();
        let mut tel = telemetry(30.0, 0.2, RuntimeIntent::Explore);
        tel.stream_elapsed = 0.4;
        let fill = core.update_budget(&settings, tel, 0.16);
        assert!(fill.startup_fill);
        assert!(
            fill.chunks_per_frame >= 12,
            "spawn fill starved: {} chunks/frame",
            fill.chunks_per_frame
        );
        assert!(fill.render_distance >= 6);
    }

    #[test]
    fn lowspec_after_warmup_throttles_jobs_again() {
        let mut settings = WorldSettings::default();
        settings.apply_world_mode_card(crate::settings::WorldModeCard::FastLaptop);
        let mut core = NeuroCore::default();
        let mut tel = telemetry(30.0, 0.2, RuntimeIntent::Explore);
        tel.stream_elapsed = 8.0;
        let steady = core.update_budget(&settings, tel, 0.16);
        assert!(!steady.startup_fill);
        assert!(
            steady.chunks_per_frame < settings.chunks_per_frame,
            "steady LowSpec should stay below the Fast card ceiling"
        );
        assert!(steady.render_distance <= 12);
        assert_eq!(steady.render_distance, 12);
    }

    #[test]
    fn auto_settled_horizon_ignores_soft_throttle() {
        let mut settings = WorldSettings::default();
        settings.render_distance = 40;
        settings.runtime_profile = RuntimeProfile::Auto;
        let mut core = NeuroCore::default();
        let mut tel = telemetry(60.0, 0.1, RuntimeIntent::Explore);
        tel.stream_elapsed = 8.0;
        let mut held = 0i32;
        // Climb to the horizon, then sit there long enough for the
        // hold counter to arm.
        for _ in 0..40 {
            held = core.update_budget(&settings, tel.clone(), 0.6).render_distance;
        }
        for _ in 0..8 {
            held = core.update_budget(&settings, tel.clone(), 0.6).render_distance;
        }
        tel.fps = 50.0;
        tel.frame_pressure = (60.0 - 50.0) / 60.0;
        tel.queue_pressure = 0.20;
        let after = core.update_budget(&settings, tel, 0.6);
        assert_eq!(
            after.render_distance, held,
            "soft throttle after settle must not re-stream"
        );
    }
}
