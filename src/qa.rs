//! Autonomous QA/autopilot mode for visual and hitch testing.
//!
//! Enable with `--qa` or `VOXEL_NATIVE_QA=1`. The harness enters a fresh
//! world, flies the player camera through a deterministic scenic route,
//! captures periodic screenshots and writes a RON report with frame-time
//! hitches. This lets an agent run the native game and inspect output
//! without asking the user for screenshots every time.

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::PrimaryWindow;
use serde::Serialize;

use crate::menu::{GameState, PendingWorldLoad};
use crate::player::Player;
use crate::settings::{ActiveWorld, TimeMode, WorldMeta, WorldSettings};
use crate::world::{ChunkStreamer, StreamingGovernor, VoxelWorld};

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

pub struct QaPlugin;

impl Plugin for QaPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(QaAutopilot::from_env()).add_systems(
            Update,
            (
                qa_enter_game,
                qa_drive_camera.run_if(in_state(GameState::InGame)),
                qa_capture_screenshot.run_if(in_state(GameState::InGame)),
                qa_finish.run_if(in_state(GameState::InGame)),
            )
                .chain(),
        );
    }
}

#[derive(Resource, Debug)]
struct QaAutopilot {
    enabled: bool,
    started: bool,
    finished: bool,
    elapsed: f32,
    duration: f32,
    screenshot_interval: f32,
    next_screenshot_at: f32,
    screenshot_index: usize,
    origin: Vec3,
    origin_set: bool,
    frames: u64,
    total_dt: f32,
    max_dt: f32,
    stalls: Vec<QaStall>,
    screenshots: Vec<String>,
    #[cfg(not(target_arch = "wasm32"))]
    report_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct QaStall {
    at_seconds: f32,
    frame_ms: f32,
    pos: [f32; 3],
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
}

#[derive(Debug, Serialize)]
struct QaReport {
    duration_seconds: f32,
    frames: u64,
    average_fps: f32,
    max_frame_ms: f32,
    final_smoothed_fps: f32,
    loaded_chunks: usize,
    mesh_entities: usize,
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
    render_distance: i32,
    screenshots: Vec<String>,
    stalls: Vec<QaStall>,
}

impl QaAutopilot {
    fn from_env() -> Self {
        let enabled = qa_enabled();
        let duration = env_f32("VOXEL_NATIVE_QA_SECONDS")
            .unwrap_or(45.0)
            .clamp(8.0, 600.0);
        let screenshot_interval = env_f32("VOXEL_NATIVE_QA_SCREENSHOT_INTERVAL")
            .unwrap_or(7.0)
            .clamp(1.0, 120.0);

        #[cfg(not(target_arch = "wasm32"))]
        let report_dir =
            PathBuf::from("qa_runs").join(format!("run_{}", crate::platform::now_epoch()));

        Self {
            enabled,
            started: false,
            finished: false,
            elapsed: 0.0,
            duration,
            screenshot_interval,
            next_screenshot_at: 2.5,
            screenshot_index: 0,
            origin: Vec3::ZERO,
            origin_set: false,
            frames: 0,
            total_dt: 0.0,
            max_dt: 0.0,
            stalls: Vec::new(),
            screenshots: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            report_dir,
        }
    }
}

fn qa_enter_game(
    mut qa: ResMut<QaAutopilot>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut pending: ResMut<PendingWorldLoad>,
    mut settings: ResMut<WorldSettings>,
    mut commands: Commands,
) {
    if !qa.enabled || qa.started || *state.get() != GameState::MainMenu {
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::fs::create_dir_all(&qa.report_dir) {
        warn!("QA: could not create {}: {e}", qa.report_dir.display());
    }

    let seed = env_u32("VOXEL_NATIVE_QA_SEED").unwrap_or(settings.seed);
    let world_name =
        std::env::var("VOXEL_NATIVE_QA_WORLD").unwrap_or_else(|_| "qa_autopilot".into());
    let mut meta = WorldMeta::new(world_name, seed);
    meta.time_mode = TimeMode::Fixed;
    meta.time_of_day = env_f32("VOXEL_NATIVE_QA_HOUR")
        .unwrap_or(10.8)
        .clamp(0.0, 24.0);
    settings.seed = seed;
    settings.time_mode = meta.time_mode;
    settings.time_of_day = meta.time_of_day;
    commands.insert_resource(ActiveWorld { meta });
    pending.0 = true;
    next.set(GameState::InGame);
    qa.started = true;

    info!(
        "QA: autopilot started (duration {:.1}s, screenshot interval {:.1}s)",
        qa.duration, qa.screenshot_interval
    );
}

fn qa_drive_camera(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    mut qa: ResMut<QaAutopilot>,
    mut query: Query<(&mut Transform, &mut Player)>,
) {
    if !qa.enabled || qa.finished {
        return;
    }
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };

    let dt = time.delta_seconds().min(1.0);
    qa.elapsed += dt;
    qa.frames += 1;
    qa.total_dt += dt;
    qa.max_dt = qa.max_dt.max(dt);

    if !qa.origin_set {
        qa.origin = transform.translation;
        qa.origin_set = true;
    }

    let route_t = qa.elapsed.max(0.0);
    let angle = route_t * 0.115;
    let radius = 95.0 + (route_t * 0.17).sin() * 28.0;
    let x = qa.origin.x + angle.cos() * radius + (angle * 0.47).sin() * 45.0;
    let z = qa.origin.z + angle.sin() * radius + (angle * 0.63).cos() * 35.0;
    let wx = crate::chunk::floor_to_i32_safe(x);
    let wz = crate::chunk::floor_to_i32_safe(z);
    let surface = world.surface_height_at(wx, wz) as f32;
    let height = 36.0 + (route_t * 0.33).sin() * 12.0;
    let pos = Vec3::new(x, surface + height, z);

    let look_x = qa.origin.x + (angle + 0.65).cos() * 45.0;
    let look_z = qa.origin.z + (angle + 0.65).sin() * 45.0;
    let look_y = world.surface_height_at(
        crate::chunk::floor_to_i32_safe(look_x),
        crate::chunk::floor_to_i32_safe(look_z),
    ) as f32
        + 8.0;
    let target = Vec3::new(look_x, look_y, look_z);
    let dir = (target - pos).normalize_or_zero();

    transform.translation = pos;
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    if dir.length_squared() > 0.0 {
        player.yaw = (-dir.x).atan2(-dir.z);
        player.pitch = dir.y.asin().clamp(-1.25, 1.05);
        transform.rotation = Quat::from_axis_angle(Vec3::Y, player.yaw)
            * Quat::from_axis_angle(Vec3::X, player.pitch);
    }

    if dt >= 0.10 {
        let at_seconds = qa.elapsed;
        qa.stalls.push(QaStall {
            at_seconds,
            frame_ms: dt * 1000.0,
            pos: [pos.x, pos.y, pos.z],
            pending_terrain: streamer.pending_terrain.len(),
            pending_meshes: streamer.pending_meshes.len(),
            dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
        });
    }
}

fn qa_capture_screenshot(
    mut qa: ResMut<QaAutopilot>,
    mut screenshots: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    if !qa.enabled || qa.finished || qa.elapsed < qa.next_screenshot_at {
        return;
    }
    qa.next_screenshot_at += qa.screenshot_interval;
    let Ok(window) = windows.get_single() else {
        return;
    };

    #[cfg(not(target_arch = "wasm32"))]
    let path = qa
        .report_dir
        .join(format!("shot_{:04}.png", qa.screenshot_index));
    #[cfg(target_arch = "wasm32")]
    let path = std::path::PathBuf::from(format!("qa_shot_{:04}.png", qa.screenshot_index));

    qa.screenshot_index += 1;
    let display_path = path.to_string_lossy().to_string();
    match screenshots.save_screenshot_to_disk(window, &path) {
        Ok(_) => {
            info!("QA: screenshot saved to {}", display_path);
            qa.screenshots.push(display_path);
        }
        Err(e) => warn!("QA: screenshot failed: {e}"),
    }
}

fn qa_finish(
    diagnostics: Res<DiagnosticsStore>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    mut qa: ResMut<QaAutopilot>,
    mut exit: EventWriter<AppExit>,
) {
    if !qa.enabled || qa.finished || qa.elapsed < qa.duration {
        return;
    }
    qa.finished = true;

    let final_fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let average_fps = if qa.total_dt > 0.0 {
        qa.frames as f32 / qa.total_dt
    } else {
        0.0
    };

    let report = QaReport {
        duration_seconds: qa.elapsed,
        frames: qa.frames,
        average_fps,
        max_frame_ms: qa.max_dt * 1000.0,
        final_smoothed_fps: final_fps,
        loaded_chunks: world.chunks.len(),
        mesh_entities: streamer.entities.len(),
        pending_terrain: streamer.pending_terrain.len(),
        pending_meshes: streamer.pending_meshes.len(),
        dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
        render_distance: governor.effective_render_distance,
        screenshots: qa.screenshots.clone(),
        stalls: qa.stalls.clone(),
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = qa.report_dir.join("report.ron");
        match ron::ser::to_string_pretty(&report, ron::ser::PrettyConfig::default()) {
            Ok(text) => match std::fs::write(&path, text) {
                Ok(_) => info!("QA: report saved to {}", path.display().to_string()),
                Err(e) => warn!("QA: report write failed: {e}"),
            },
            Err(e) => warn!("QA: report serialize failed: {e}"),
        }
    }

    info!(
        "QA: finished {:.1}s, avg {:.1} fps, max {:.1} ms, stalls {}, screenshots {}",
        report.duration_seconds,
        report.average_fps,
        report.max_frame_ms,
        report.stalls.len(),
        report.screenshots.len()
    );
    exit.send(AppExit::Success);
}

fn qa_enabled() -> bool {
    env_flag("VOXEL_NATIVE_QA")
        || std::env::args().any(|arg| matches!(arg.as_str(), "--qa" | "--qa-autopilot"))
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}
