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
use crate::mode::{ActiveMode, ModeContext};
use crate::planetary_streaming::{PlanetaryStreamingTelemetry, FAR_FIELD_LEVELS};
use crate::player::Player;
use crate::settings::{
    ActiveWorld, SceneryQuality, TimeMode, WorldMeta, WorldProfile, WorldSettings,
};
use crate::world::{ChunkStreamer, StreamingGovernor, VoxelWorld};

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

const QA_FRAME_TIME_BUCKET_WIDTH_MS: u16 = 1;
const QA_FRAME_TIME_EXACT_BUCKETS: usize = 1_000;
const QA_FRAME_TIME_OVERFLOW_BUCKET: usize = QA_FRAME_TIME_EXACT_BUCKETS;
const QA_FRAME_TIME_BUCKETS: usize = QA_FRAME_TIME_EXACT_BUCKETS + 1;
const QA_FRAME_TIME_EXACT_MAX_MS: f64 =
    QA_FRAME_TIME_EXACT_BUCKETS as f64 * QA_FRAME_TIME_BUCKET_WIDTH_MS as f64;
/// Samples beyond this limit normally mean the process was suspended or the
/// clock source was corrupted. They invalidate the measurement instead of
/// silently dominating its mean.
const QA_FRAME_TIME_ACCEPTED_MAX_MS: f64 = 60_000.0;
const QA_FRAME_TIME_ACCUMULATOR_BYTE_CAP: usize = 16 * 1_024;
const QA_FRAME_TIME_QUANTILE_WORK_CAP: usize = 1_024;

const QA_GIT_SHA_MAX_CHARS: usize = 64;
const QA_FINGERPRINT_MAX_CHARS: usize = 128;
const QA_TOOLCHAIN_MAX_CHARS: usize = 160;
const QA_HARDWARE_MAX_CHARS: usize = 320;
/// Versioned serialized QA contract. Version 2 adds explicit render-only Far
/// Hydro mode, post-deferred ECS truth, scheduler truth, and hard budgets.
const QA_REPORT_SCHEMA_VERSION: &str = "2.0.0";

#[cfg(debug_assertions)]
const QA_BUILD_PROFILE: &str = "debug";
#[cfg(not(debug_assertions))]
const QA_BUILD_PROFILE: &str = "release";

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
    warmup_elapsed: f32,
    write_tail_elapsed: f32,
    route_ready: bool,
    duration: f32,
    screenshot_interval: f32,
    next_screenshot_at: f32,
    screenshot_index: usize,
    finish_wait_frames: u16,
    origin: Vec3,
    origin_set: bool,
    focus_waypoint: bool,
    focus_streaming: bool,
    streaming_distance_m: f32,
    current_phase: QaRoutePhase,
    route_frame_times: QaFrameTimeAccumulator,
    peak_loaded_chunks: usize,
    peak_mesh_entities: usize,
    peak_pending_terrain: usize,
    peak_pending_meshes: usize,
    peak_dirty_chunks: usize,
    max_horizontal_displacement_m: f32,
    stalls: Vec<QaStall>,
    screenshots: Vec<String>,
    #[cfg(not(target_arch = "wasm32"))]
    report_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum QaStallStage {
    Warmup,
    Route,
}

#[derive(Debug, Clone, Serialize)]
struct QaStall {
    /// Monotonic time since autonomous camera control began. Warmup and route
    /// samples share this clock so report consumers never need to interpret a
    /// negative timestamp as a hidden state flag.
    at_seconds: f32,
    stage: QaStallStage,
    /// Route-local time is absent during streaming warmup.
    route_seconds: Option<f32>,
    frame_ms: f32,
    pos: [f32; 3],
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum QaRoutePhase {
    Establishing,
    Approach,
    Detail,
    Context,
}

impl QaRoutePhase {
    const fn label(self) -> &'static str {
        match self {
            Self::Establishing => "establishing",
            Self::Approach => "approach",
            Self::Detail => "detail",
            Self::Context => "context",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct QaRouteSample {
    camera_offset: Vec2,
    camera_height: f32,
    target_offset: Vec2,
    target_height: f32,
    terrain_clearance: f32,
    phase: QaRoutePhase,
}

impl QaRouteSample {
    fn interpolate(a: Self, b: Self, t: f32, phase: QaRoutePhase) -> Self {
        let t = smoothstep01(t);
        Self {
            camera_offset: a.camera_offset.lerp(b.camera_offset, t),
            camera_height: a.camera_height.lerp(b.camera_height, t),
            target_offset: a.target_offset.lerp(b.target_offset, t),
            target_height: a.target_height.lerp(b.target_height, t),
            terrain_clearance: a.terrain_clearance.lerp(b.terrain_clearance, t),
            phase,
        }
    }
}

#[derive(Debug, Serialize)]
struct QaReport {
    qa_report_schema_version: String,
    run_identity: QaRunIdentity,
    viewport: Option<QaViewport>,
    planetary_streaming: Option<QaPlanetaryStreaming>,
    route_focus: String,
    requested_route_distance_m: f32,
    max_horizontal_displacement_m: f32,
    requested_duration_seconds: f32,
    duration_seconds: f32,
    warmup_seconds: f32,
    write_tail_seconds: f32,
    frames: u64,
    average_fps: f32,
    max_frame_ms: f32,
    route_frame_times: QaRouteFrameTimeSummary,
    final_smoothed_fps: f32,
    loaded_chunks: usize,
    mesh_entities: usize,
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
    render_distance: i32,
    peak_loaded_chunks: usize,
    peak_mesh_entities: usize,
    peak_pending_terrain: usize,
    peak_pending_meshes: usize,
    peak_dirty_chunks: usize,
    screenshots: Vec<String>,
    stalls: Vec<QaStall>,
}

#[derive(Debug, Clone, Serialize)]
struct QaRunIdentity {
    package_version: String,
    build_profile: String,
    instance_label: Option<String>,
    world_name: Option<String>,
    world_seed: Option<u32>,
    world_profile: Option<String>,
    scenery_quality: Option<String>,
    git_sha: Option<String>,
    git_dirty: Option<bool>,
    source_fingerprint: Option<String>,
    executable_hash: Option<String>,
    toolchain: Option<String>,
    hardware: Option<String>,
}

/// Fixed-memory frame-time evidence for the active route only. The first
/// 1,000 buckets represent `(n-1, n]` milliseconds; the final bucket records
/// accepted samples above 1,000 ms. Quantiles that land in the overflow bucket
/// fail closed to `None` instead of pretending to have a 1 ms error bound.
#[derive(Debug, Clone)]
struct QaFrameTimeAccumulator {
    buckets: [u64; QA_FRAME_TIME_BUCKETS],
    sample_count: u64,
    excluded_warmup_sample_count: u64,
    excluded_write_tail_sample_count: u64,
    rejected_non_finite_sample_count: u64,
    rejected_non_positive_sample_count: u64,
    rejected_huge_sample_count: u64,
    rejected_arithmetic_overflow_sample_count: u64,
    total_microseconds: u128,
    max_microseconds: u64,
}

impl Default for QaFrameTimeAccumulator {
    fn default() -> Self {
        Self {
            buckets: [0; QA_FRAME_TIME_BUCKETS],
            sample_count: 0,
            excluded_warmup_sample_count: 0,
            excluded_write_tail_sample_count: 0,
            rejected_non_finite_sample_count: 0,
            rejected_non_positive_sample_count: 0,
            rejected_huge_sample_count: 0,
            rejected_arithmetic_overflow_sample_count: 0,
            total_microseconds: 0,
            max_microseconds: 0,
        }
    }
}

const _: () =
    assert!(std::mem::size_of::<QaFrameTimeAccumulator>() <= QA_FRAME_TIME_ACCUMULATOR_BYTE_CAP);
const _: () = assert!(QA_FRAME_TIME_BUCKETS <= QA_FRAME_TIME_QUANTILE_WORK_CAP);

#[derive(Debug, Clone, Serialize)]
struct QaRouteFrameTimeSummary {
    scope: String,
    sample_count: u64,
    excluded_warmup_sample_count: u64,
    excluded_write_tail_sample_count: u64,
    rejected_sample_count: u64,
    rejected_non_finite_sample_count: u64,
    rejected_non_positive_sample_count: u64,
    rejected_huge_sample_count: u64,
    rejected_arithmetic_overflow_sample_count: u64,
    histogram_overflow_sample_count: u64,
    histogram_bucket_count: usize,
    histogram_bucket_width_ms: u16,
    histogram_exact_max_ms: u16,
    accepted_sample_max_ms: u32,
    quantile_method: String,
    quantile_values_are_bucket_upper_bounds: bool,
    quantile_max_error_ms: f32,
    mean_sample_rounding_max_error_ms: f32,
    quantiles_complete: bool,
    measurement_valid: bool,
    mean_ms: Option<f32>,
    median_ms: Option<f32>,
    p95_ms: Option<f32>,
    p99_ms: Option<f32>,
    max_ms: Option<f32>,
    accumulator_bytes: usize,
    quantile_scan_work_cap: usize,
}

impl QaFrameTimeAccumulator {
    fn exclude_warmup_frame(&mut self) {
        self.excluded_warmup_sample_count = self.excluded_warmup_sample_count.saturating_add(1);
    }

    fn exclude_write_tail_frame(&mut self) {
        self.excluded_write_tail_sample_count =
            self.excluded_write_tail_sample_count.saturating_add(1);
    }

    /// Records one active-route frame. Returns `false` when the sample is not
    /// evidence-safe; rejection remains visible in the serialized summary.
    fn record_route_frame(&mut self, delta_seconds: f32) -> bool {
        if !delta_seconds.is_finite() {
            self.rejected_non_finite_sample_count =
                self.rejected_non_finite_sample_count.saturating_add(1);
            return false;
        }

        let frame_ms = f64::from(delta_seconds) * 1_000.0;
        if frame_ms <= 0.0 {
            self.rejected_non_positive_sample_count =
                self.rejected_non_positive_sample_count.saturating_add(1);
            return false;
        }
        if frame_ms > QA_FRAME_TIME_ACCEPTED_MAX_MS {
            self.rejected_huge_sample_count = self.rejected_huge_sample_count.saturating_add(1);
            return false;
        }

        let rounded_microseconds = (frame_ms * 1_000.0).round().max(1.0) as u64;
        let bucket_index = if frame_ms > QA_FRAME_TIME_EXACT_MAX_MS {
            QA_FRAME_TIME_OVERFLOW_BUCKET
        } else {
            // Exact buckets encode (n-1, n] ms and therefore report a
            // conservative upper bound with less than 1 ms absolute error.
            (frame_ms.ceil() as usize)
                .saturating_sub(1)
                .min(QA_FRAME_TIME_EXACT_BUCKETS - 1)
        };

        let Some(next_sample_count) = self.sample_count.checked_add(1) else {
            self.reject_arithmetic_overflow();
            return false;
        };
        let Some(next_bucket_count) = self.buckets[bucket_index].checked_add(1) else {
            self.reject_arithmetic_overflow();
            return false;
        };
        let Some(next_total_microseconds) = self
            .total_microseconds
            .checked_add(u128::from(rounded_microseconds))
        else {
            self.reject_arithmetic_overflow();
            return false;
        };

        self.sample_count = next_sample_count;
        self.buckets[bucket_index] = next_bucket_count;
        self.total_microseconds = next_total_microseconds;
        self.max_microseconds = self.max_microseconds.max(rounded_microseconds);
        true
    }

    fn reject_arithmetic_overflow(&mut self) {
        self.rejected_arithmetic_overflow_sample_count = self
            .rejected_arithmetic_overflow_sample_count
            .saturating_add(1);
    }

    fn quantile_ms(&self, percentile: u8) -> Option<f32> {
        let rank = qa_nearest_rank(self.sample_count, percentile)?;
        let mut cumulative = 0_u64;
        for (index, count) in self.buckets.iter().enumerate() {
            cumulative = cumulative.saturating_add(*count);
            if cumulative >= rank {
                if index == QA_FRAME_TIME_OVERFLOW_BUCKET {
                    return None;
                }
                let upper_bound_ms = (index + 1) * usize::from(QA_FRAME_TIME_BUCKET_WIDTH_MS);
                return Some(upper_bound_ms as f32);
            }
        }
        None
    }

    fn summary(&self) -> QaRouteFrameTimeSummary {
        let rejected_sample_count = self
            .rejected_non_finite_sample_count
            .saturating_add(self.rejected_non_positive_sample_count)
            .saturating_add(self.rejected_huge_sample_count)
            .saturating_add(self.rejected_arithmetic_overflow_sample_count);
        let mean_ms = (self.sample_count > 0)
            .then(|| (self.total_microseconds as f64 / self.sample_count as f64 / 1_000.0) as f32);
        let max_ms = (self.sample_count > 0).then(|| self.max_microseconds as f32 / 1_000.0);
        let median_ms = self.quantile_ms(50);
        let p95_ms = self.quantile_ms(95);
        let p99_ms = self.quantile_ms(99);
        let quantiles_complete =
            self.sample_count > 0 && median_ms.is_some() && p95_ms.is_some() && p99_ms.is_some();

        QaRouteFrameTimeSummary {
            scope: "active_route_only_warmup_and_write_tail_excluded".to_owned(),
            sample_count: self.sample_count,
            excluded_warmup_sample_count: self.excluded_warmup_sample_count,
            excluded_write_tail_sample_count: self.excluded_write_tail_sample_count,
            rejected_sample_count,
            rejected_non_finite_sample_count: self.rejected_non_finite_sample_count,
            rejected_non_positive_sample_count: self.rejected_non_positive_sample_count,
            rejected_huge_sample_count: self.rejected_huge_sample_count,
            rejected_arithmetic_overflow_sample_count: self
                .rejected_arithmetic_overflow_sample_count,
            histogram_overflow_sample_count: self.buckets[QA_FRAME_TIME_OVERFLOW_BUCKET],
            histogram_bucket_count: QA_FRAME_TIME_BUCKETS,
            histogram_bucket_width_ms: QA_FRAME_TIME_BUCKET_WIDTH_MS,
            histogram_exact_max_ms: QA_FRAME_TIME_EXACT_MAX_MS as u16,
            accepted_sample_max_ms: QA_FRAME_TIME_ACCEPTED_MAX_MS as u32,
            quantile_method: "nearest_rank_conservative_bucket_upper_bound".to_owned(),
            quantile_values_are_bucket_upper_bounds: true,
            quantile_max_error_ms: QA_FRAME_TIME_BUCKET_WIDTH_MS as f32,
            mean_sample_rounding_max_error_ms: 0.0005,
            quantiles_complete,
            measurement_valid: rejected_sample_count == 0 && quantiles_complete,
            mean_ms,
            median_ms,
            p95_ms,
            p99_ms,
            max_ms,
            accumulator_bytes: std::mem::size_of::<Self>(),
            quantile_scan_work_cap: QA_FRAME_TIME_QUANTILE_WORK_CAP,
        }
    }
}

/// Deterministic nearest-rank quantile: `ceil(percentile * n / 100)`.
fn qa_nearest_rank(sample_count: u64, percentile: u8) -> Option<u64> {
    if sample_count == 0 || !(1..=100).contains(&percentile) {
        return None;
    }
    let numerator = u128::from(sample_count) * u128::from(percentile);
    let rank = numerator.div_ceil(100);
    Some(rank.min(u128::from(u64::MAX)) as u64)
}

fn qa_observe_route_frame_time(
    accumulator: &mut QaFrameTimeAccumulator,
    route_ready: bool,
    route_elapsed: f32,
    route_duration: f32,
    delta_seconds: f32,
) {
    if !route_ready {
        accumulator.exclude_warmup_frame();
    } else if route_elapsed.is_finite()
        && route_duration.is_finite()
        && route_elapsed >= 0.0
        && route_duration > 0.0
        && route_elapsed < route_duration
    {
        accumulator.record_route_frame(delta_seconds);
    } else {
        accumulator.exclude_write_tail_frame();
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
struct QaViewport {
    logical_width: f32,
    logical_height: f32,
    physical_width: u32,
    physical_height: u32,
    scale_factor: f32,
    dpi_percent: f32,
}

/// Exact far-field state captured with the visual evidence. Keeping both the
/// live values and their hard budgets in one report prevents a screenshot
/// from looking successful while silently relying on an oversized cache or a
/// fallback that shortened the horizon.
#[derive(Debug, Clone, Serialize)]
struct QaPlanetaryStreaming {
    enabled: bool,
    profile: String,
    interaction_radius_metres: i64,
    confirmed_near_extent_metres: i64,
    near_coverage_ready_columns: usize,
    near_coverage_hidden_cells: usize,
    far_radius_metres: i64,
    resident_entities: usize,
    resident_vertices: usize,
    resident_indices: usize,
    ring_vertices: [usize; FAR_FIELD_LEVELS],
    ring_indices: [usize; FAR_FIELD_LEVELS],
    resident_mesh_bytes: usize,
    resident_fluid_entities: usize,
    resident_fluid_vertices: usize,
    resident_fluid_indices: usize,
    fluid_ring_vertices: [usize; FAR_FIELD_LEVELS],
    fluid_ring_indices: [usize; FAR_FIELD_LEVELS],
    resident_fluid_mesh_bytes: usize,
    scheduler_resident_entities: usize,
    scheduler_resident_vertices: usize,
    scheduler_resident_indices: usize,
    scheduler_ring_vertices: [usize; FAR_FIELD_LEVELS],
    scheduler_ring_indices: [usize; FAR_FIELD_LEVELS],
    scheduler_resident_mesh_bytes: usize,
    scheduler_resident_fluid_entities: usize,
    scheduler_resident_fluid_vertices: usize,
    scheduler_resident_fluid_indices: usize,
    scheduler_fluid_ring_vertices: [usize; FAR_FIELD_LEVELS],
    scheduler_fluid_ring_indices: [usize; FAR_FIELD_LEVELS],
    scheduler_resident_fluid_mesh_bytes: usize,
    resident_observation_valid: bool,
    resident_entity_count_overflow: bool,
    resident_duplicate_levels: usize,
    resident_out_of_range_levels: usize,
    resident_scheduler_mismatch: bool,
    resident_budget_exceeded: bool,
    resident_observation_rejections: u64,
    resident_fluid_observation_valid: bool,
    resident_fluid_entity_count_overflow: bool,
    resident_fluid_duplicate_slots: usize,
    resident_fluid_out_of_range_levels: usize,
    resident_fluid_scheduler_mismatch: bool,
    resident_fluid_budget_exceeded: bool,
    resident_fluid_observation_rejections: u64,
    live_sample_cache_windows: usize,
    live_sample_cache_bytes: usize,
    peak_live_sample_cache_windows: usize,
    peak_live_sample_cache_bytes: usize,
    budget_entities: usize,
    budget_vertices: usize,
    budget_indices: usize,
    budget_mesh_bytes: usize,
    budget_build_jobs: usize,
    budget_ring_build_bytes: usize,
    budget_sample_cache_bytes: usize,
    budget_coverage_work_bytes: usize,
    budget_fluid_entities: usize,
    budget_fluid_vertices: usize,
    budget_fluid_indices: usize,
    budget_fluid_mesh_bytes: usize,
    budget_fluid_ring_build_bytes: usize,
    budget_atomic_ring_build_bytes: usize,
    pending_rebuilds: usize,
    dirty_mask: u8,
    build_in_flight: bool,
    update_cadence_frames: u8,
    /// Backward-compatible summary of the global pressure policy. The two
    /// per-level arrays below are the authoritative transition state.
    material_detail: String,
    desired_material_detail: [String; FAR_FIELD_LEVELS],
    resident_material_detail: [Option<String>; FAR_FIELD_LEVELS],
    resident_detailed_levels: usize,
    resident_reduced_levels: usize,
    surface_material_mode: String,
    hydro_mode: String,
    scheduler_deferred_frames: u64,
    completed_rebuilds: u64,
    stale_builds_discarded: u64,
    budget_rejections: u64,
    last_build_ms: f32,
    max_build_ms: f32,
    last_height_queries: usize,
    last_material_slope_queries: usize,
    last_bridge_v2_cell_reuses: usize,
    last_fluid_classification_queries: usize,
    last_fluid_biome_queries: usize,
    last_fluid_vertices: usize,
    last_fluid_indices: usize,
    last_biome_queries: usize,
    last_reused_height_samples: usize,
    last_reused_biome_samples: usize,
    last_cache_shift_x_cells: i32,
    last_cache_shift_z_cells: i32,
    last_cache_update: String,
    incremental_strip_rebuilds: u64,
    full_cache_rebuilds: u64,
    teleport_fallbacks: u64,
    last_clamped_queries: usize,
    camera_world_x: i64,
    camera_world_z: i64,
}

fn qa_run_identity(active_world: Option<&ActiveWorld>) -> QaRunIdentity {
    let instance_label = std::env::var("VOXEL_NATIVE_INSTANCE_LABEL")
        .ok()
        .and_then(|value| qa_bounded_text(&value, 96));
    QaRunIdentity {
        package_version: env!("CARGO_PKG_VERSION").to_owned(),
        build_profile: QA_BUILD_PROFILE.to_owned(),
        instance_label,
        world_name: active_world.map(|world| world.meta.name.clone()),
        world_seed: active_world.map(|world| world.meta.seed),
        world_profile: active_world.map(|world| format!("{:?}", world.meta.world_profile)),
        scenery_quality: active_world.map(|world| format!("{:?}", world.meta.scenery_quality)),
        git_sha: std::env::var("VOXEL_NATIVE_QA_GIT_SHA")
            .ok()
            .and_then(|value| qa_git_sha(&value)),
        git_dirty: std::env::var("VOXEL_NATIVE_QA_GIT_DIRTY")
            .ok()
            .and_then(|value| qa_optional_bool(&value)),
        source_fingerprint: std::env::var("VOXEL_NATIVE_QA_SOURCE_FINGERPRINT")
            .ok()
            .and_then(|value| qa_provenance_token(&value, QA_FINGERPRINT_MAX_CHARS)),
        executable_hash: std::env::var("VOXEL_NATIVE_QA_EXECUTABLE_HASH")
            .ok()
            .and_then(|value| qa_provenance_token(&value, QA_FINGERPRINT_MAX_CHARS)),
        toolchain: std::env::var("VOXEL_NATIVE_QA_TOOLCHAIN")
            .ok()
            .and_then(|value| qa_bounded_text(&value, QA_TOOLCHAIN_MAX_CHARS)),
        hardware: std::env::var("VOXEL_NATIVE_QA_HARDWARE")
            .ok()
            .and_then(|value| qa_bounded_text(&value, QA_HARDWARE_MAX_CHARS)),
    }
}

fn qa_bounded_text(value: &str, max_chars: usize) -> Option<String> {
    let normalized: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect();
    let trimmed = normalized.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn qa_git_sha(value: &str) -> Option<String> {
    let value = value.trim();
    let length = value.chars().count();
    ((7..=QA_GIT_SHA_MAX_CHARS).contains(&length)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| value.to_ascii_lowercase())
}

fn qa_provenance_token(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    let length = value.chars().count();
    (!value.is_empty()
        && length <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')))
    .then(|| value.to_owned())
}

fn qa_optional_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn qa_viewport(window: Option<&Window>) -> Option<QaViewport> {
    let window = window?;
    let scale_factor = window.resolution.scale_factor();
    Some(QaViewport {
        logical_width: window.resolution.width(),
        logical_height: window.resolution.height(),
        physical_width: window.resolution.physical_width(),
        physical_height: window.resolution.physical_height(),
        scale_factor,
        dpi_percent: scale_factor * 100.0,
    })
}

fn qa_planetary_streaming(
    telemetry: Option<&PlanetaryStreamingTelemetry>,
) -> Option<QaPlanetaryStreaming> {
    let telemetry = telemetry?;
    Some(QaPlanetaryStreaming {
        enabled: telemetry.enabled,
        profile: format!("{:?}", telemetry.profile),
        interaction_radius_metres: telemetry.interaction_radius_metres,
        confirmed_near_extent_metres: telemetry.confirmed_near_extent_metres,
        near_coverage_ready_columns: telemetry.near_coverage_ready_columns,
        near_coverage_hidden_cells: telemetry.near_coverage_hidden_cells,
        far_radius_metres: telemetry.far_radius_metres,
        resident_entities: telemetry.resident_entities,
        resident_vertices: telemetry.resident_vertices,
        resident_indices: telemetry.resident_indices,
        ring_vertices: telemetry.ring_vertices,
        ring_indices: telemetry.ring_indices,
        resident_mesh_bytes: telemetry.resident_mesh_bytes,
        resident_fluid_entities: telemetry.resident_fluid_entities,
        resident_fluid_vertices: telemetry.resident_fluid_vertices,
        resident_fluid_indices: telemetry.resident_fluid_indices,
        fluid_ring_vertices: telemetry.fluid_ring_vertices,
        fluid_ring_indices: telemetry.fluid_ring_indices,
        resident_fluid_mesh_bytes: telemetry.resident_fluid_mesh_bytes,
        scheduler_resident_entities: telemetry.scheduler_resident_entities,
        scheduler_resident_vertices: telemetry.scheduler_resident_vertices,
        scheduler_resident_indices: telemetry.scheduler_resident_indices,
        scheduler_ring_vertices: telemetry.scheduler_ring_vertices,
        scheduler_ring_indices: telemetry.scheduler_ring_indices,
        scheduler_resident_mesh_bytes: telemetry.scheduler_resident_mesh_bytes,
        scheduler_resident_fluid_entities: telemetry.scheduler_resident_fluid_entities,
        scheduler_resident_fluid_vertices: telemetry.scheduler_resident_fluid_vertices,
        scheduler_resident_fluid_indices: telemetry.scheduler_resident_fluid_indices,
        scheduler_fluid_ring_vertices: telemetry.scheduler_fluid_ring_vertices,
        scheduler_fluid_ring_indices: telemetry.scheduler_fluid_ring_indices,
        scheduler_resident_fluid_mesh_bytes: telemetry.scheduler_resident_fluid_mesh_bytes,
        resident_observation_valid: telemetry.resident_observation_valid,
        resident_entity_count_overflow: telemetry.resident_entity_count_overflow,
        resident_duplicate_levels: telemetry.resident_duplicate_levels,
        resident_out_of_range_levels: telemetry.resident_out_of_range_levels,
        resident_scheduler_mismatch: telemetry.resident_scheduler_mismatch,
        resident_budget_exceeded: telemetry.resident_budget_exceeded,
        resident_observation_rejections: telemetry.resident_observation_rejections,
        resident_fluid_observation_valid: telemetry.resident_fluid_observation_valid,
        resident_fluid_entity_count_overflow: telemetry.resident_fluid_entity_count_overflow,
        resident_fluid_duplicate_slots: telemetry.resident_fluid_duplicate_slots,
        resident_fluid_out_of_range_levels: telemetry.resident_fluid_out_of_range_levels,
        resident_fluid_scheduler_mismatch: telemetry.resident_fluid_scheduler_mismatch,
        resident_fluid_budget_exceeded: telemetry.resident_fluid_budget_exceeded,
        resident_fluid_observation_rejections: telemetry.resident_fluid_observation_rejections,
        live_sample_cache_windows: telemetry.live_sample_cache_windows,
        live_sample_cache_bytes: telemetry.live_sample_cache_bytes,
        peak_live_sample_cache_windows: telemetry.peak_live_sample_cache_windows,
        peak_live_sample_cache_bytes: telemetry.peak_live_sample_cache_bytes,
        budget_entities: telemetry.budget_entities,
        budget_vertices: telemetry.budget_vertices,
        budget_indices: telemetry.budget_indices,
        budget_mesh_bytes: telemetry.budget_mesh_bytes,
        budget_build_jobs: telemetry.budget_build_jobs,
        budget_ring_build_bytes: telemetry.budget_ring_build_bytes,
        budget_sample_cache_bytes: telemetry.budget_sample_cache_bytes,
        budget_coverage_work_bytes: telemetry.budget_coverage_work_bytes,
        budget_fluid_entities: telemetry.budget_fluid_entities,
        budget_fluid_vertices: telemetry.budget_fluid_vertices,
        budget_fluid_indices: telemetry.budget_fluid_indices,
        budget_fluid_mesh_bytes: telemetry.budget_fluid_mesh_bytes,
        budget_fluid_ring_build_bytes: telemetry.budget_fluid_ring_build_bytes,
        budget_atomic_ring_build_bytes: telemetry.budget_atomic_ring_build_bytes,
        pending_rebuilds: telemetry.pending_rebuilds,
        dirty_mask: telemetry.dirty_mask,
        build_in_flight: telemetry.build_in_flight,
        update_cadence_frames: telemetry.update_cadence_frames,
        material_detail: format!("{:?}", telemetry.material_detail),
        desired_material_detail: telemetry
            .desired_material_detail
            .map(|detail| format!("{detail:?}")),
        resident_material_detail: telemetry
            .resident_material_detail
            .map(|detail| detail.map(|detail| format!("{detail:?}"))),
        resident_detailed_levels: telemetry.resident_detailed_levels,
        resident_reduced_levels: telemetry.resident_reduced_levels,
        surface_material_mode: format!("{:?}", telemetry.surface_material_mode),
        hydro_mode: format!("{:?}", telemetry.hydro_mode),
        scheduler_deferred_frames: telemetry.scheduler_deferred_frames,
        completed_rebuilds: telemetry.completed_rebuilds,
        stale_builds_discarded: telemetry.stale_builds_discarded,
        budget_rejections: telemetry.budget_rejections,
        last_build_ms: telemetry.last_build_ms,
        max_build_ms: telemetry.max_build_ms,
        last_height_queries: telemetry.last_height_queries,
        last_material_slope_queries: telemetry.last_material_slope_queries,
        last_bridge_v2_cell_reuses: telemetry.last_bridge_v2_cell_reuses,
        last_fluid_classification_queries: telemetry.last_fluid_classification_queries,
        last_fluid_biome_queries: telemetry.last_fluid_biome_queries,
        last_fluid_vertices: telemetry.last_fluid_vertices,
        last_fluid_indices: telemetry.last_fluid_indices,
        last_biome_queries: telemetry.last_biome_queries,
        last_reused_height_samples: telemetry.last_reused_height_samples,
        last_reused_biome_samples: telemetry.last_reused_biome_samples,
        last_cache_shift_x_cells: telemetry.last_cache_shift_x_cells,
        last_cache_shift_z_cells: telemetry.last_cache_shift_z_cells,
        last_cache_update: format!("{:?}", telemetry.last_cache_update),
        incremental_strip_rebuilds: telemetry.incremental_strip_rebuilds,
        full_cache_rebuilds: telemetry.full_cache_rebuilds,
        teleport_fallbacks: telemetry.teleport_fallbacks,
        last_clamped_queries: telemetry.last_clamped_queries,
        camera_world_x: telemetry.camera_world_x,
        camera_world_z: telemetry.camera_world_z,
    })
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
        let focus_waypoint = std::env::var("VOXEL_NATIVE_QA_FOCUS")
            .map(|value| value.trim().eq_ignore_ascii_case("waypoint"))
            .unwrap_or(false);
        let focus_streaming = std::env::var("VOXEL_NATIVE_QA_FOCUS")
            .map(|value| value.trim().eq_ignore_ascii_case("streaming"))
            .unwrap_or(false);
        let streaming_distance_m = env_f32("VOXEL_NATIVE_QA_DISTANCE_KM")
            .unwrap_or(8.0)
            .clamp(1.0, 100.0)
            * 1_000.0;

        #[cfg(not(target_arch = "wasm32"))]
        let report_dir =
            PathBuf::from("qa_runs").join(format!("run_{}", crate::platform::now_epoch()));

        Self {
            enabled,
            started: false,
            finished: false,
            elapsed: 0.0,
            warmup_elapsed: 0.0,
            write_tail_elapsed: 0.0,
            route_ready: false,
            duration,
            screenshot_interval,
            next_screenshot_at: 2.5,
            screenshot_index: 0,
            finish_wait_frames: 0,
            origin: Vec3::ZERO,
            origin_set: false,
            focus_waypoint,
            focus_streaming,
            streaming_distance_m,
            current_phase: QaRoutePhase::Establishing,
            route_frame_times: QaFrameTimeAccumulator::default(),
            peak_loaded_chunks: 0,
            peak_mesh_entities: 0,
            peak_pending_terrain: 0,
            peak_pending_meshes: 0,
            peak_dirty_chunks: 0,
            max_horizontal_displacement_m: 0.0,
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
    let world_profile = std::env::var("VOXEL_NATIVE_QA_PROFILE")
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "astral" | "astral_frontier" | "frontier" => Some(WorldProfile::AstralFrontier),
            "natural" => Some(WorldProfile::Natural),
            _ => None,
        })
        .unwrap_or(settings.world_profile);
    let world_name =
        std::env::var("VOXEL_NATIVE_QA_WORLD").unwrap_or_else(|_| "qa_autopilot".into());
    let mut meta = WorldMeta::new_with_profile(world_name, seed, world_profile);
    let scenery_quality = std::env::var("VOXEL_NATIVE_QA_SCENERY")
        .ok()
        .as_deref()
        .and_then(parse_scenery_quality)
        .unwrap_or(meta.scenery_quality);
    meta.scenery_quality = scenery_quality;
    let qa_focus = std::env::var("VOXEL_NATIVE_QA_FOCUS")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if qa_focus == "river" {
        let generator =
            crate::terrain::TerrainGenerator::new(seed).with_world_profile(world_profile);
        if let Some(focus) = generator.find_hydrographic_focus(0, 0, 4096) {
            meta.player_pos = [
                focus.x as f32 + 0.5,
                crate::terrain::WATER_LEVEL as f32 + 34.0,
                focus.y as f32 + 0.5,
            ];
            meta.player_yaw = 0.0;
            meta.player_pitch = -0.28;
            info!(
                "QA: river focus at {}, {}, surface {}",
                focus.x,
                focus.y,
                generator.surface_height_at(focus.x, focus.y)
            );
        }
    } else if qa.focus_waypoint && world_profile == WorldProfile::AstralFrontier {
        let generator =
            crate::terrain::TerrainGenerator::new(seed).with_world_profile(world_profile);
        if let Some(focus) = generator.find_astral_waypoint_near(0, 0, 16) {
            meta.player_pos = [
                focus.x as f32 + 0.5,
                focus.y as f32 + 28.0,
                focus.z as f32 + 0.5,
            ];
            meta.player_yaw = 0.0;
            meta.player_pitch = -0.28;
            info!(
                "QA: global Astral waypoint focus at {}, {}, {}",
                focus.x, focus.y, focus.z
            );
        }
    }
    meta.time_mode = TimeMode::Fixed;
    meta.time_of_day = env_f32("VOXEL_NATIVE_QA_HOUR")
        .unwrap_or(10.8)
        .clamp(0.0, 24.0);
    settings.seed = seed;
    settings.world_profile = world_profile;
    settings.scenery_quality = scenery_quality;
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

fn smoothstep01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn qa_route_phase(progress: f32) -> QaRoutePhase {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.20 {
        QaRoutePhase::Establishing
    } else if progress < 0.44 {
        QaRoutePhase::Approach
    } else if progress < 0.72 {
        QaRoutePhase::Detail
    } else {
        QaRoutePhase::Context
    }
}

fn qa_keyframe(
    camera_offset: Vec2,
    camera_height: f32,
    target_offset: Vec2,
    target_height: f32,
    terrain_clearance: f32,
) -> QaRouteSample {
    QaRouteSample {
        camera_offset,
        camera_height,
        target_offset,
        target_height,
        terrain_clearance,
        phase: QaRoutePhase::Establishing,
    }
}

fn qa_sample_keyframes(progress: f32, frames: &[(f32, QaRouteSample)]) -> QaRouteSample {
    debug_assert!(frames.len() >= 2);
    let progress = progress.clamp(0.0, 1.0);
    let phase = qa_route_phase(progress);
    for window in frames.windows(2) {
        let (start_t, start) = window[0];
        let (end_t, end) = window[1];
        if progress <= end_t {
            let local_t = if end_t > start_t {
                (progress - start_t) / (end_t - start_t)
            } else {
                1.0
            };
            return QaRouteSample::interpolate(start, end, local_t, phase);
        }
    }
    let mut final_frame = frames[frames.len() - 1].1;
    final_frame.phase = phase;
    final_frame
}

/// Keep the point being inspected inside the currently affordable horizontal
/// streaming horizon. The route itself remains spatially varied; only the
/// camera-to-subject baseline contracts when the governor lowers distance.
fn qa_constrain_to_visible_radius(mut sample: QaRouteSample, visible_radius: f32) -> QaRouteSample {
    let baseline = sample.camera_offset - sample.target_offset;
    let limit = visible_radius.max(24.0);
    let distance = baseline.length();
    if distance.is_finite() && distance > limit {
        sample.camera_offset = sample.target_offset + baseline * (limit / distance);
    }
    sample
}

fn qa_hero_route_sample(
    progress: f32,
    landing_offset: Vec2,
    landing_height: f32,
    visible_radius: f32,
) -> QaRouteSample {
    let landing_offset = if landing_offset.is_finite() && landing_offset.length() >= 48.0 {
        landing_offset
    } else {
        Vec2::new(-124.0, 24.0)
    };
    let landing_height = if landing_height.is_finite() {
        landing_height.clamp(-128.0, 96.0)
    } else {
        -58.0
    };
    let toward_hub = (-landing_offset).normalize_or(Vec2::X);
    let right = Vec2::new(-toward_hub.y, toward_hub.x);

    // The route deliberately tells four different spatial stories rather than
    // orbiting one summit: whole precinct, landing/transit approach, citadel
    // craft pass, and a reverse wide shot that restores world context.
    let frames = [
        (
            0.00,
            qa_keyframe(
                landing_offset - toward_hub * 58.0 + right * 32.0,
                landing_height + 80.0,
                landing_offset * 0.48,
                landing_height * 0.45 + 10.0,
                34.0,
            ),
        ),
        (
            0.20,
            qa_keyframe(
                landing_offset - toward_hub * 28.0 + right * 20.0,
                landing_height + 48.0,
                landing_offset * 0.78,
                landing_height + 8.0,
                24.0,
            ),
        ),
        (
            0.44,
            qa_keyframe(
                landing_offset + toward_hub * 6.0 + right * 28.0,
                landing_height + 32.0,
                landing_offset + toward_hub * 34.0,
                landing_height + 8.0,
                20.0,
            ),
        ),
        (
            0.58,
            qa_keyframe(
                landing_offset * 0.30 + right * 62.0,
                34.0,
                right * 4.0,
                20.0,
                22.0,
            ),
        ),
        (
            0.72,
            qa_keyframe(
                toward_hub * 62.0 - right * 40.0,
                44.0,
                -right * 3.0,
                22.0,
                24.0,
            ),
        ),
        (
            1.00,
            qa_keyframe(
                toward_hub * 160.0 + right * 56.0,
                86.0,
                landing_offset * 0.23,
                landing_height * 0.18 + 8.0,
                42.0,
            ),
        ),
    ];
    qa_constrain_to_visible_radius(qa_sample_keyframes(progress, &frames), visible_radius)
}

fn qa_waypoint_route_sample(progress: f32, forward: Vec2, visible_radius: f32) -> QaRouteSample {
    let forward = if forward.is_finite() && forward.length_squared() > 0.25 {
        forward.normalize()
    } else {
        Vec2::new(0.86, 0.51).normalize()
    };
    let right = Vec2::new(-forward.y, forward.x);
    let frames = [
        (
            0.00,
            qa_keyframe(forward * 112.0 + right * 34.0, 62.0, Vec2::ZERO, 8.0, 34.0),
        ),
        (
            0.20,
            qa_keyframe(forward * 58.0 + right * 16.0, 36.0, Vec2::ZERO, 7.0, 24.0),
        ),
        (
            0.44,
            qa_keyframe(forward * 28.0 + right * 7.0, 21.0, Vec2::ZERO, 8.0, 15.0),
        ),
        (
            0.58,
            qa_keyframe(right * 28.0 - forward * 8.0, 17.0, Vec2::ZERO, 10.0, 14.0),
        ),
        (
            0.72,
            qa_keyframe(-forward * 38.0 + right * 18.0, 28.0, Vec2::ZERO, 9.0, 18.0),
        ),
        (
            1.00,
            qa_keyframe(-forward * 122.0 - right * 36.0, 74.0, Vec2::ZERO, 5.0, 38.0),
        ),
    ];
    qa_constrain_to_visible_radius(qa_sample_keyframes(progress, &frames), visible_radius)
}

/// A deterministic high-speed route used to prove that visual range and
/// travelled distance do not expand full-voxel residency.  The camera follows
/// a shallow S-curve so the test exercises forward prediction, lateral ring
/// shifts and stale-request cancellation instead of benchmarking one axis.
fn qa_streaming_route_sample(progress: f32, requested_distance_m: f32) -> QaRouteSample {
    let distance = if requested_distance_m.is_finite() {
        requested_distance_m.clamp(1_000.0, 100_000.0)
    } else {
        8_000.0
    };
    let look_ahead = (distance * 0.03).clamp(96.0, 240.0);
    let camera =
        |x_fraction: f32, z_fraction: f32| Vec2::new(distance * x_fraction, distance * z_fraction);
    let frame = |x_fraction: f32, z_fraction: f32, tangent: Vec2, height: f32| {
        let camera_offset = camera(x_fraction, z_fraction);
        let forward = tangent.normalize_or(Vec2::X);
        qa_keyframe(
            camera_offset,
            height,
            camera_offset + forward * look_ahead,
            12.0,
            46.0,
        )
    };
    let frames = [
        (0.00, frame(0.00, 0.000, Vec2::new(1.0, 0.20), 88.0)),
        (0.20, frame(0.19, 0.038, Vec2::new(1.0, -0.32), 78.0)),
        (0.44, frame(0.43, -0.052, Vec2::new(1.0, 0.38), 70.0)),
        (0.72, frame(0.71, 0.048, Vec2::new(1.0, -0.28), 82.0)),
        (1.00, frame(1.00, 0.000, Vec2::new(1.0, 0.05), 96.0)),
    ];
    qa_sample_keyframes(progress, &frames)
}

fn qa_waypoint_axis(origin: Vec3) -> Vec2 {
    let x = crate::chunk::floor_to_i32_safe(origin.x) as u32;
    let z = crate::chunk::floor_to_i32_safe(origin.z) as u32;
    let mixed =
        x.wrapping_mul(0x9E37_79B9).rotate_left(11) ^ z.wrapping_mul(0x85EB_CA6B).rotate_right(7);
    let angle = (mixed as f64 / u32::MAX as f64 * std::f64::consts::TAU) as f32;
    Vec2::new(angle.cos(), angle.sin())
}

fn qa_visible_radius(governor: &StreamingGovernor, target_distance: u32) -> f32 {
    let chunks = governor.active_render_distance(target_distance).max(2);
    (chunks as f32 * crate::chunk::CHUNK_SIZE_I as f32 - 32.0).max(24.0)
}

fn qa_surface_envelope(world: &VoxelWorld, x: f32, z: f32) -> f32 {
    const OFFSETS: [Vec2; 5] = [
        Vec2::ZERO,
        Vec2::new(6.0, 0.0),
        Vec2::new(-6.0, 0.0),
        Vec2::new(0.0, 6.0),
        Vec2::new(0.0, -6.0),
    ];
    OFFSETS.into_iter().fold(f32::NEG_INFINITY, |top, offset| {
        let wx = crate::chunk::floor_to_i32_safe(x + offset.x);
        let wz = crate::chunk::floor_to_i32_safe(z + offset.y);
        top.max(world.surface_height_at(wx, wz) as f32)
    })
}

fn qa_world_pose(world: &VoxelWorld, origin: Vec3, sample: QaRouteSample) -> (Vec3, Vec3) {
    let x = origin.x + sample.camera_offset.x;
    let z = origin.z + sample.camera_offset.y;
    let surface = qa_surface_envelope(world, x, z);
    let camera_y = (origin.y + sample.camera_height).max(surface + sample.terrain_clearance);

    let target_x = origin.x + sample.target_offset.x;
    let target_z = origin.z + sample.target_offset.y;
    let target_surface = world.surface_height_at(
        crate::chunk::floor_to_i32_safe(target_x),
        crate::chunk::floor_to_i32_safe(target_z),
    ) as f32;
    let target_y = (origin.y + sample.target_height).max(target_surface + 3.5);
    (
        Vec3::new(x, camera_y, z),
        Vec3::new(target_x, target_y, target_z),
    )
}

fn qa_apply_pose(transform: &mut Transform, player: &mut Player, pos: Vec3, target: Vec3) {
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
}

fn qa_stall_timing(
    warmup_seconds: f32,
    route_seconds: Option<f32>,
) -> (QaStallStage, f32, Option<f32>) {
    let warmup_seconds = warmup_seconds.max(0.0);
    match route_seconds {
        Some(route_seconds) => {
            let route_seconds = route_seconds.max(0.0);
            (
                QaStallStage::Route,
                warmup_seconds + route_seconds,
                Some(route_seconds),
            )
        }
        None => (QaStallStage::Warmup, warmup_seconds, None),
    }
}

fn qa_profile_anchor_ready(requested: WorldProfile, generated: WorldProfile) -> bool {
    requested != WorldProfile::AstralFrontier || generated == WorldProfile::AstralFrontier
}

fn qa_drive_camera(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    settings: Res<WorldSettings>,
    mut qa: ResMut<QaAutopilot>,
    mut mode: ResMut<ModeContext>,
    mut query: Query<(&mut Transform, &mut Player)>,
    mut weapon_viewmodels: Query<&mut Visibility, With<crate::weapons::Weapon>>,
) {
    if !qa.enabled || qa.finished {
        return;
    }
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    mode.set(
        ActiveMode::Combat,
        "Autonomous scenic flight: world presentation and streaming inspection.",
    );
    for mut visibility in &mut weapon_viewmodels {
        *visibility = Visibility::Hidden;
    }

    let raw_dt = time.delta_seconds();
    let route_ready = qa.route_ready;
    let route_elapsed = qa.elapsed;
    let route_duration = qa.duration;
    qa_observe_route_frame_time(
        &mut qa.route_frame_times,
        route_ready,
        route_elapsed,
        route_duration,
        raw_dt,
    );
    // Motion stays bounded even if the platform clock is corrupted or the
    // process resumes after a long suspension. The raw value above remains the
    // evidence input and is rejected visibly rather than silently clamped.
    let dt = if raw_dt.is_finite() && raw_dt > 0.0 {
        raw_dt.min(1.0)
    } else {
        0.0
    };
    let dirty_chunks = streamer.dirty_queue.len() + world.edit_dirty_chunks.len();
    qa.peak_loaded_chunks = qa.peak_loaded_chunks.max(world.chunks.len());
    qa.peak_mesh_entities = qa.peak_mesh_entities.max(streamer.entities.len());
    qa.peak_pending_terrain = qa.peak_pending_terrain.max(streamer.pending_terrain.len());
    qa.peak_pending_meshes = qa.peak_pending_meshes.max(streamer.pending_meshes.len());
    qa.peak_dirty_chunks = qa.peak_dirty_chunks.max(dirty_chunks);

    let requested_profile = settings.effective_world_profile();
    if !qa_profile_anchor_ready(requested_profile, world.generator.world_profile()) {
        // State entry and world-generator replacement happen in separate
        // schedules. Do not permanently capture the previous profile's spawn
        // as our cinematic anchor during that one-frame handoff.
        player.velocity = Vec3::ZERO;
        player.flying = true;
        player.placed_on_surface = true;
        return;
    }

    if !qa.origin_set {
        qa.origin = if requested_profile == WorldProfile::AstralFrontier && !qa.focus_waypoint {
            world
                .generator
                .astral_frontier_hub()
                .map_or(transform.translation, |hub| {
                    Vec3::new(
                        hub.x as f32 + 0.5,
                        world.surface_height_at(hub.x, hub.y) as f32,
                        hub.y as f32 + 0.5,
                    )
                })
        } else if requested_profile == WorldProfile::AstralFrontier {
            world
                .generator
                .find_astral_waypoint_near(0, 0, 16)
                .map_or(transform.translation, |focus| {
                    Vec3::new(focus.x as f32 + 0.5, focus.y as f32, focus.z as f32 + 0.5)
                })
        } else {
            transform.translation
        };
        qa.origin_set = true;
    }

    let astral_route = requested_profile == WorldProfile::AstralFrontier;
    let waypoint_route = astral_route && qa.focus_waypoint;
    let streaming_route = qa.focus_streaming;
    let cinematic_route = astral_route || streaming_route;
    let streaming_distance_m = qa.streaming_distance_m;
    let route_origin = qa.origin;
    let visible_radius = qa_visible_radius(&governor, settings.render_distance);
    let landing_context = world.generator.astral_frontier_landing().map(|landing| {
        let landing_x = landing.x as f32 + 0.5;
        let landing_z = landing.y as f32 + 0.5;
        let landing_y = world.surface_height_at(landing.x, landing.y) as f32;
        (
            Vec2::new(landing_x - route_origin.x, landing_z - route_origin.z),
            landing_y - route_origin.y,
        )
    });

    let route_sample = |progress: f32| {
        if streaming_route {
            qa_streaming_route_sample(progress, streaming_distance_m)
        } else if waypoint_route {
            qa_waypoint_route_sample(progress, qa_waypoint_axis(route_origin), visible_radius)
        } else {
            let (landing_offset, landing_height) =
                landing_context.unwrap_or((Vec2::new(-124.0, 24.0), -58.0));
            qa_hero_route_sample(progress, landing_offset, landing_height, visible_radius)
        }
    };

    if !qa.route_ready {
        qa.warmup_elapsed += dt;
        // Hold the actual establishing-shot position while streaming warms up.
        // This primes geometry around frame zero instead of releasing the route
        // with a long teleport from the normal gameplay spawn.
        if cinematic_route {
            let sample = route_sample(0.0);
            let (pos, target) = qa_world_pose(&world, route_origin, sample);
            qa.current_phase = sample.phase;
            qa_apply_pose(&mut transform, &mut player, pos, target);
        } else {
            player.velocity = Vec3::ZERO;
            player.flying = true;
            player.placed_on_surface = true;
        }
        if dt >= 0.10 {
            let warmup_elapsed = qa.warmup_elapsed;
            let (stage, at_seconds, route_seconds) = qa_stall_timing(warmup_elapsed, None);
            qa.stalls.push(QaStall {
                at_seconds,
                stage,
                route_seconds,
                frame_ms: dt * 1000.0,
                pos: [
                    transform.translation.x,
                    transform.translation.y,
                    transform.translation.z,
                ],
                pending_terrain: streamer.pending_terrain.len(),
                pending_meshes: streamer.pending_meshes.len(),
                dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
            });
        }
        if !qa_stream_ready(
            qa.warmup_elapsed,
            streamer.pending_terrain.len(),
            streamer.pending_meshes.len(),
        ) {
            return;
        }
        qa.route_ready = true;
        info!(
            "QA: scenic route released after {:.1}s warmup (terrain {}, meshes {})",
            qa.warmup_elapsed,
            streamer.pending_terrain.len(),
            streamer.pending_meshes.len()
        );
        // This frame was observed and accumulated as warm-up at the top of
        // the system. Starting the route clock with the same `dt` would count
        // one physical frame in two phases while the histogram contains it in
        // only one. Hold the establishing pose and begin route time on the
        // next frame, where observation and clock advancement both agree that
        // the route was active at frame start.
        return;
    }

    if qa.elapsed < qa.duration {
        qa.elapsed = (qa.elapsed + dt).min(qa.duration);
    } else {
        qa.write_tail_elapsed = (qa.write_tail_elapsed + dt).min(3.0);
    }
    let route_t = qa.elapsed.max(0.0);
    if cinematic_route {
        let progress = (route_t / qa.duration.max(f32::EPSILON)).clamp(0.0, 1.0);
        let sample = route_sample(progress);
        let (pos, target) = qa_world_pose(&world, route_origin, sample);
        qa.current_phase = sample.phase;
        qa_apply_pose(&mut transform, &mut player, pos, target);
    } else {
        let angle = route_t * 0.115;
        let radius = 95.0 + (route_t * 0.17).sin() * 28.0;
        let x = qa.origin.x + angle.cos() * radius + (angle * 0.47).sin() * 45.0;
        let z = qa.origin.z + angle.sin() * radius + (angle * 0.63).cos() * 35.0;
        let surface = qa_surface_envelope(&world, x, z);
        let pos = Vec3::new(x, surface + 36.0 + (route_t * 0.33).sin() * 12.0, z);
        let look_x = qa.origin.x + (angle + 0.65).cos() * 45.0;
        let look_z = qa.origin.z + (angle + 0.65).sin() * 45.0;
        let look_y = world.surface_height_at(
            crate::chunk::floor_to_i32_safe(look_x),
            crate::chunk::floor_to_i32_safe(look_z),
        ) as f32
            + 8.0;
        qa.current_phase = qa_route_phase(route_t / qa.duration.max(f32::EPSILON));
        qa_apply_pose(
            &mut transform,
            &mut player,
            pos,
            Vec3::new(look_x, look_y, look_z),
        );
    }

    let horizontal_displacement = Vec2::new(
        transform.translation.x - route_origin.x,
        transform.translation.z - route_origin.z,
    )
    .length();
    if horizontal_displacement.is_finite() {
        qa.max_horizontal_displacement_m = qa
            .max_horizontal_displacement_m
            .max(horizontal_displacement);
    }

    if dt >= 0.10 {
        let (stage, at_seconds, route_seconds) =
            qa_stall_timing(qa.warmup_elapsed, Some(qa.elapsed));
        let pos = transform.translation;
        qa.stalls.push(QaStall {
            at_seconds,
            stage,
            route_seconds,
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
    if !qa.enabled
        || qa.finished
        || !qa_screenshot_due(qa.elapsed, qa.next_screenshot_at, qa.duration)
    {
        return;
    }
    qa.next_screenshot_at += qa.screenshot_interval;
    let Ok(window) = windows.get_single() else {
        return;
    };

    #[cfg(not(target_arch = "wasm32"))]
    let path = qa.report_dir.join(format!(
        "shot_{:04}_{}.png",
        qa.screenshot_index,
        qa.current_phase.label()
    ));
    #[cfg(target_arch = "wasm32")]
    let path = std::path::PathBuf::from(format!(
        "qa_shot_{:04}_{}.png",
        qa.screenshot_index,
        qa.current_phase.label()
    ));

    qa.screenshot_index += 1;
    let display_path = path.to_string_lossy().to_string();
    match screenshots.save_screenshot_to_disk(window, &path) {
        Ok(_) => {
            info!("QA: screenshot queued for {}", display_path);
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
    active_world: Option<Res<ActiveWorld>>,
    planetary_telemetry: Option<Res<PlanetaryStreamingTelemetry>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut qa: ResMut<QaAutopilot>,
    mut exit: EventWriter<AppExit>,
) {
    if !qa.enabled || qa.finished || qa.elapsed < qa.duration {
        return;
    }

    // ScreenshotManager performs the GPU readback and PNG write
    // asynchronously. The final capture is commonly queued in this same
    // chained update, so exiting immediately can leave the report pointing at
    // a file that was never written. Give the renderer at least two complete
    // frames, then wait for non-empty files with a bounded three-second tail.
    qa.finish_wait_frames = qa.finish_wait_frames.saturating_add(1);
    if qa.finish_wait_frames < 2 {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let finish_wait_seconds = qa.write_tail_elapsed;
        if !qa
            .screenshots
            .iter()
            .all(|path| qa_screenshot_file_ready(path))
            && finish_wait_seconds < 3.0
        {
            return;
        }
        let before = qa.screenshots.len();
        qa.screenshots.retain(|path| qa_screenshot_file_ready(path));
        if qa.screenshots.len() != before {
            warn!(
                "QA: {} queued screenshot(s) did not finish within the bounded write tail",
                before - qa.screenshots.len()
            );
        }
    }
    qa.finished = true;

    let final_fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    let route_frame_times = qa.route_frame_times.summary();
    let average_fps = route_frame_times
        .mean_ms
        .filter(|mean_ms| mean_ms.is_finite() && *mean_ms > 0.0)
        .map_or(0.0, |mean_ms| 1_000.0 / mean_ms);
    let frames = route_frame_times.sample_count;
    let max_frame_ms = route_frame_times.max_ms.unwrap_or(0.0);

    let report = QaReport {
        qa_report_schema_version: QA_REPORT_SCHEMA_VERSION.to_owned(),
        run_identity: qa_run_identity(active_world.as_deref()),
        viewport: qa_viewport(windows.get_single().ok()),
        planetary_streaming: qa_planetary_streaming(planetary_telemetry.as_deref()),
        route_focus: if qa.focus_streaming {
            "streaming".into()
        } else if qa.focus_waypoint {
            "waypoint".into()
        } else {
            "scenic".into()
        },
        requested_route_distance_m: if qa.focus_streaming {
            qa.streaming_distance_m
        } else {
            0.0
        },
        max_horizontal_displacement_m: qa.max_horizontal_displacement_m,
        requested_duration_seconds: qa.duration,
        duration_seconds: qa.elapsed,
        warmup_seconds: qa.warmup_elapsed,
        write_tail_seconds: qa.write_tail_elapsed,
        frames,
        average_fps,
        max_frame_ms,
        route_frame_times,
        final_smoothed_fps: final_fps,
        loaded_chunks: world.chunks.len(),
        mesh_entities: streamer.entities.len(),
        pending_terrain: streamer.pending_terrain.len(),
        pending_meshes: streamer.pending_meshes.len(),
        dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
        render_distance: governor.effective_render_distance,
        peak_loaded_chunks: qa.peak_loaded_chunks,
        peak_mesh_entities: qa.peak_mesh_entities,
        peak_pending_terrain: qa.peak_pending_terrain,
        peak_pending_meshes: qa.peak_pending_meshes,
        peak_dirty_chunks: qa.peak_dirty_chunks,
        screenshots: qa.screenshots.clone(),
        stalls: qa.stalls.clone(),
    };

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = qa.report_dir.join("report.ron");
        match ron::ser::to_string_pretty(&report, ron::ser::PrettyConfig::default()) {
            Ok(text) => match qa_write_report_atomic(&path, text.as_bytes()) {
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

/// Publish one complete report from a sibling temporary file. QA run
/// directories are unique by contract; refusing an existing final path keeps
/// two accidental writers from silently replacing one another. The temporary
/// file stays beside the report so the final rename cannot cross filesystems.
#[cfg(not(target_arch = "wasm32"))]
fn qa_write_report_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};

    if path.exists() {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "QA report already exists; refusing to overwrite evidence",
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid QA report filename"))?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.partial", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // A same-directory hard link atomically creates the final name and fails
    // if it already exists on every supported desktop platform. Unlike rename
    // it cannot replace a racing writer on Unix. Removing our own partial name
    // leaves the already-flushed inode reachable through `report.ron`.
    std::fs::hard_link(&temporary, path)?;
    match std::fs::remove_file(&temporary) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        // The final report is already complete and durable. A leftover partial
        // is visible evidence of cleanup failure, not grounds to misreport the
        // successfully published final as absent.
        Err(_) => Ok(()),
    }
}

fn qa_screenshot_due(elapsed: f32, next: f32, duration: f32) -> bool {
    elapsed.is_finite()
        && next.is_finite()
        && duration.is_finite()
        && elapsed >= next
        && next <= duration + 0.001
}

#[cfg(not(target_arch = "wasm32"))]
fn qa_screenshot_file_ready(path: &str) -> bool {
    use std::io::{Read, Seek};

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() < 12 {
        return false;
    }
    if file.seek(std::io::SeekFrom::End(-12)).is_err() {
        return false;
    }
    let mut png_tail = [0_u8; 12];
    file.read_exact(&mut png_tail).is_ok() && qa_png_tail_is_complete(&png_tail)
}

#[cfg(not(target_arch = "wasm32"))]
fn qa_png_tail_is_complete(png_tail: &[u8; 12]) -> bool {
    png_tail[..4] == [0, 0, 0, 0] && &png_tail[4..8] == b"IEND"
}

fn qa_enabled() -> bool {
    env_flag("VOXEL_NATIVE_QA")
        || std::env::args().any(|arg| matches!(arg.as_str(), "--qa" | "--qa-autopilot"))
}

/// Avoid judging half-streamed geometry. Three seconds prevents the empty
/// initial queues from releasing the camera before load scheduling begins;
/// the bounded fallback keeps QA from hanging forever on a slow machine.
fn qa_stream_ready(warmup_seconds: f32, pending_terrain: usize, pending_meshes: usize) -> bool {
    (warmup_seconds >= 3.0 && pending_terrain <= 4 && pending_meshes <= 6) || warmup_seconds >= 14.0
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
    parse_finite_f32(&std::env::var(name).ok()?)
}

fn parse_finite_f32(value: &str) -> Option<f32> {
    let parsed = value.trim().parse::<f32>().ok()?;
    parsed.is_finite().then_some(parsed)
}

fn parse_scenery_quality(value: &str) -> Option<SceneryQuality> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(SceneryQuality::Off),
        "lean" => Some(SceneryQuality::Lean),
        "balanced" => Some(SceneryQuality::Balanced),
        "lush" => Some(SceneryQuality::Lush),
        _ => None,
    }
}

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use bevy::prelude::{Vec2, Vec3, Window};

    #[cfg(not(target_arch = "wasm32"))]
    use super::qa_png_tail_is_complete;
    use super::{
        parse_finite_f32, parse_scenery_quality, qa_bounded_text, qa_git_sha, qa_hero_route_sample,
        qa_nearest_rank, qa_observe_route_frame_time, qa_optional_bool, qa_planetary_streaming,
        qa_profile_anchor_ready, qa_provenance_token, qa_route_phase, qa_screenshot_due,
        qa_stall_timing, qa_stream_ready, qa_streaming_route_sample, qa_viewport, qa_waypoint_axis,
        qa_waypoint_route_sample, QaFrameTimeAccumulator, QaReport, QaRoutePhase, QaRunIdentity,
        QaStallStage, QA_BUILD_PROFILE, QA_FINGERPRINT_MAX_CHARS,
        QA_FRAME_TIME_ACCUMULATOR_BYTE_CAP, QA_FRAME_TIME_BUCKETS, QA_FRAME_TIME_QUANTILE_WORK_CAP,
        QA_REPORT_SCHEMA_VERSION,
    };
    use crate::planetary_streaming::PlanetaryStreamingTelemetry;
    use crate::settings::{SceneryQuality, WorldProfile};

    #[test]
    fn report_text_is_control_free_trimmed_and_bounded_by_characters() {
        assert_eq!(
            qa_bounded_text("  Agent\nLive\t ", 32).as_deref(),
            Some("AgentLive")
        );
        assert_eq!(qa_bounded_text("  ", 32), None);
        assert_eq!(qa_bounded_text("anything", 0), None);
        assert_eq!(
            qa_bounded_text("Astra\u{0308}l", 4).as_deref(),
            Some("Astr")
        );
    }

    #[test]
    fn route_frame_histogram_has_deterministic_nearest_rank_quantiles() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        for bucket in 0..100 {
            let frame_ms = bucket as f32 + 0.5;
            assert!(accumulator.record_route_frame(frame_ms / 1_000.0));
        }

        let summary = accumulator.summary();
        assert_eq!(summary.sample_count, 100);
        assert!((summary.mean_ms.expect("mean") - 50.0).abs() < 0.001);
        assert_eq!(summary.median_ms, Some(50.0));
        assert_eq!(summary.p95_ms, Some(95.0));
        assert_eq!(summary.p99_ms, Some(99.0));
        assert_eq!(summary.max_ms, Some(99.5));
        assert!(summary.quantiles_complete);
        assert!(summary.measurement_valid);

        assert_eq!(qa_nearest_rank(1, 50), Some(1));
        assert_eq!(qa_nearest_rank(2, 50), Some(1));
        assert_eq!(qa_nearest_rank(3, 50), Some(2));
        assert_eq!(qa_nearest_rank(100, 95), Some(95));
        assert_eq!(qa_nearest_rank(100, 99), Some(99));
        assert_eq!(qa_nearest_rank(0, 95), None);
        assert_eq!(qa_nearest_rank(10, 0), None);
    }

    #[test]
    fn route_frame_quantile_is_a_conservative_sub_millisecond_bucket_bound() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        let frame_ms = 16.25;
        assert!(accumulator.record_route_frame(frame_ms / 1_000.0));
        let summary = accumulator.summary();
        let estimate = summary.median_ms.expect("median");
        assert!(estimate >= frame_ms);
        assert!(estimate - frame_ms < summary.quantile_max_error_ms);
        assert_eq!(estimate, 17.0);
    }

    #[test]
    fn route_frame_scope_separates_warmup_active_route_and_write_tail() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        qa_observe_route_frame_time(&mut accumulator, false, 0.0, 10.0, 0.250);
        qa_observe_route_frame_time(&mut accumulator, false, 0.0, 10.0, 0.125);
        qa_observe_route_frame_time(&mut accumulator, true, 0.0, 10.0, 0.01625);
        qa_observe_route_frame_time(&mut accumulator, true, 9.99, 10.0, 0.02025);
        qa_observe_route_frame_time(&mut accumulator, true, 10.0, 10.0, 0.500);
        qa_observe_route_frame_time(&mut accumulator, true, 10.5, 10.0, 0.750);

        let summary = accumulator.summary();
        assert_eq!(summary.excluded_warmup_sample_count, 2);
        assert_eq!(summary.sample_count, 2);
        assert_eq!(summary.excluded_write_tail_sample_count, 2);
        assert_eq!(summary.median_ms, Some(17.0));
        assert_eq!(summary.p95_ms, Some(21.0));
        assert_eq!(summary.max_ms, Some(20.25));
        assert!(summary.mean_ms.expect("mean") < 21.0);
    }

    #[test]
    fn route_frame_accumulator_rejects_bad_values_and_fails_closed_on_overflow_quantiles() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        assert!(accumulator.record_route_frame(1.5));
        assert!(!accumulator.record_route_frame(f32::NAN));
        assert!(!accumulator.record_route_frame(f32::INFINITY));
        assert!(!accumulator.record_route_frame(-0.001));
        assert!(!accumulator.record_route_frame(0.0));
        assert!(!accumulator.record_route_frame(60.001));
        assert!(!accumulator.record_route_frame(f32::MAX));

        let summary = accumulator.summary();
        assert_eq!(summary.sample_count, 1);
        assert_eq!(summary.histogram_overflow_sample_count, 1);
        assert_eq!(summary.rejected_non_finite_sample_count, 2);
        assert_eq!(summary.rejected_non_positive_sample_count, 2);
        assert_eq!(summary.rejected_huge_sample_count, 2);
        assert_eq!(summary.rejected_sample_count, 6);
        assert_eq!(summary.mean_ms, Some(1_500.0));
        assert_eq!(summary.max_ms, Some(1_500.0));
        assert_eq!(summary.median_ms, None);
        assert_eq!(summary.p95_ms, None);
        assert_eq!(summary.p99_ms, None);
        assert!(!summary.quantiles_complete);
        assert!(!summary.measurement_valid);
    }

    #[test]
    fn route_frame_accumulator_has_compile_time_memory_and_work_caps() {
        let accumulator = QaFrameTimeAccumulator::default();
        assert_eq!(accumulator.buckets.len(), QA_FRAME_TIME_BUCKETS);
        assert!(
            std::mem::size_of::<QaFrameTimeAccumulator>() <= QA_FRAME_TIME_ACCUMULATOR_BYTE_CAP
        );
        assert!(QA_FRAME_TIME_BUCKETS <= QA_FRAME_TIME_QUANTILE_WORK_CAP);
        assert!(!std::mem::needs_drop::<QaFrameTimeAccumulator>());
    }

    #[test]
    fn route_frame_accumulator_reports_counter_overflow_without_partial_commit() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        accumulator.sample_count = u64::MAX;
        assert!(!accumulator.record_route_frame(0.016));
        assert_eq!(accumulator.sample_count, u64::MAX);
        assert_eq!(accumulator.buckets.iter().sum::<u64>(), 0);
        assert_eq!(accumulator.rejected_arithmetic_overflow_sample_count, 1);
    }

    #[test]
    fn qa_provenance_values_are_bounded_sanitized_and_fail_closed() {
        assert_eq!(qa_git_sha(" ABCDEF1 ").as_deref(), Some("abcdef1"));
        assert_eq!(qa_git_sha("abcdef"), None);
        assert_eq!(qa_git_sha("abcdefg!"), None);
        assert_eq!(qa_git_sha(&"a".repeat(65)), None);

        assert_eq!(
            qa_provenance_token("sha256:AB_cd-09.", QA_FINGERPRINT_MAX_CHARS).as_deref(),
            Some("sha256:AB_cd-09.")
        );
        assert_eq!(
            qa_provenance_token("sha256:abc/def", QA_FINGERPRINT_MAX_CHARS),
            None
        );
        assert_eq!(
            qa_provenance_token(
                &"a".repeat(QA_FINGERPRINT_MAX_CHARS + 1),
                QA_FINGERPRINT_MAX_CHARS
            ),
            None
        );

        assert_eq!(qa_optional_bool(" TRUE "), Some(true));
        assert_eq!(qa_optional_bool("0"), Some(false));
        assert_eq!(qa_optional_bool("dirty"), None);
        assert_eq!(
            qa_bounded_text(" GPU\nRTX\t5090 ", 32).as_deref(),
            Some("GPURTX5090")
        );
    }

    #[test]
    fn report_serialization_includes_route_statistics_and_build_provenance() {
        let mut accumulator = QaFrameTimeAccumulator::default();
        assert!(accumulator.record_route_frame(0.01625));
        let route_frame_times = accumulator.summary();
        let report = QaReport {
            qa_report_schema_version: QA_REPORT_SCHEMA_VERSION.to_owned(),
            run_identity: QaRunIdentity {
                package_version: "test".to_owned(),
                build_profile: QA_BUILD_PROFILE.to_owned(),
                instance_label: Some("serialization".to_owned()),
                world_name: None,
                world_seed: Some(7),
                world_profile: None,
                scenery_quality: None,
                git_sha: Some("abcdef1".to_owned()),
                git_dirty: Some(true),
                source_fingerprint: Some("sha256:source".to_owned()),
                executable_hash: Some("sha256:executable".to_owned()),
                toolchain: Some("rustc test".to_owned()),
                hardware: Some("test hardware".to_owned()),
            },
            viewport: None,
            planetary_streaming: None,
            route_focus: "streaming".to_owned(),
            requested_route_distance_m: 8_000.0,
            max_horizontal_displacement_m: 8_000.0,
            requested_duration_seconds: 12.0,
            duration_seconds: 12.0,
            warmup_seconds: 3.0,
            write_tail_seconds: 0.25,
            frames: route_frame_times.sample_count,
            average_fps: 1_000.0 / route_frame_times.mean_ms.expect("mean"),
            max_frame_ms: route_frame_times.max_ms.expect("max"),
            route_frame_times,
            final_smoothed_fps: 60.0,
            loaded_chunks: 0,
            mesh_entities: 0,
            pending_terrain: 0,
            pending_meshes: 0,
            dirty_chunks: 0,
            render_distance: 0,
            peak_loaded_chunks: 0,
            peak_mesh_entities: 0,
            peak_pending_terrain: 0,
            peak_pending_meshes: 0,
            peak_dirty_chunks: 0,
            screenshots: Vec::new(),
            stalls: Vec::new(),
        };

        let serialized = ron::ser::to_string(&report).expect("serialize QA report");
        assert!(serialized.contains("route_frame_times"));
        assert!(serialized.contains("qa_report_schema_version:\"2.0.0\""));
        assert!(serialized.contains("sample_count:1"));
        assert!(serialized.contains("median_ms:Some(17.0)"));
        assert!(serialized.contains("build_profile:"));
        assert!(serialized.contains("git_dirty:Some(true)"));
        assert!(serialized.contains("requested_duration_seconds:12.0"));
        assert!(serialized.contains("write_tail_seconds:0.25"));
        assert_eq!(
            QA_BUILD_PROFILE,
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
    }

    #[test]
    fn report_viewport_records_logical_physical_and_dpi_contract() {
        assert!(qa_viewport(None).is_none());
        let window = Window::default();
        let viewport = qa_viewport(Some(&window)).expect("default primary window");
        assert!(viewport.logical_width > 0.0);
        assert!(viewport.logical_height > 0.0);
        assert!(viewport.physical_width > 0);
        assert!(viewport.physical_height > 0);
        assert!(viewport.scale_factor.is_finite() && viewport.scale_factor > 0.0);
        assert_eq!(viewport.dpi_percent, viewport.scale_factor * 100.0);
    }

    #[test]
    fn report_captures_far_field_live_state_and_hard_budgets_together() {
        use crate::planetary_streaming::{
            FarFieldHydroMode, FarFieldMaterialDetail, FarFieldSurfaceMaterialMode,
        };

        let mut telemetry = PlanetaryStreamingTelemetry::default();
        telemetry.material_detail = FarFieldMaterialDetail::Reduced;
        telemetry.desired_material_detail = [
            FarFieldMaterialDetail::Detailed,
            FarFieldMaterialDetail::Detailed,
            FarFieldMaterialDetail::Reduced,
            FarFieldMaterialDetail::Reduced,
            FarFieldMaterialDetail::Reduced,
            FarFieldMaterialDetail::Reduced,
        ];
        telemetry.resident_material_detail = [
            Some(FarFieldMaterialDetail::Detailed),
            Some(FarFieldMaterialDetail::Detailed),
            Some(FarFieldMaterialDetail::Reduced),
            None,
            None,
            None,
        ];
        telemetry.resident_detailed_levels = 2;
        telemetry.resident_reduced_levels = 1;
        telemetry.surface_material_mode = FarFieldSurfaceMaterialMode::LegacyPalette;
        telemetry.hydro_mode = FarFieldHydroMode::DescriptiveV1;
        telemetry.last_material_slope_queries = 4_225;
        telemetry.last_bridge_v2_cell_reuses = 3_584;
        telemetry.last_fluid_classification_queries = 3_721;
        telemetry.last_fluid_biome_queries = 835;
        telemetry.live_sample_cache_windows = 5;
        telemetry.live_sample_cache_bytes = 420_000;
        telemetry.peak_live_sample_cache_windows = 6;
        telemetry.peak_live_sample_cache_bytes = 510_000;
        telemetry.ring_vertices = [1_000, 2_000, 3_000, 4_000, 5_000, 6_000];
        telemetry.ring_indices = [2_000, 4_000, 6_000, 8_000, 10_000, 12_000];
        telemetry.resident_vertices = telemetry.ring_vertices.iter().sum();
        telemetry.resident_indices = telemetry.ring_indices.iter().sum();
        telemetry.resident_entities = crate::planetary_streaming::FAR_FIELD_LEVELS;
        telemetry.resident_mesh_bytes = 504_000;
        telemetry.scheduler_ring_vertices = telemetry.ring_vertices;
        telemetry.scheduler_ring_indices = telemetry.ring_indices;
        telemetry.scheduler_resident_entities = telemetry.resident_entities;
        telemetry.scheduler_resident_vertices = telemetry.resident_vertices;
        telemetry.scheduler_resident_indices = telemetry.resident_indices;
        telemetry.scheduler_resident_mesh_bytes = telemetry.resident_mesh_bytes;
        telemetry.fluid_ring_vertices = [100, 200, 300, 400, 500, 600];
        telemetry.fluid_ring_indices = [300, 600, 900, 1_200, 1_500, 1_800];
        telemetry.resident_fluid_vertices = telemetry.fluid_ring_vertices.iter().sum();
        telemetry.resident_fluid_indices = telemetry.fluid_ring_indices.iter().sum();
        telemetry.resident_fluid_entities = crate::planetary_streaming::FAR_FIELD_LEVELS;
        telemetry.resident_fluid_mesh_bytes = 100_800;
        telemetry.scheduler_fluid_ring_vertices = telemetry.fluid_ring_vertices;
        telemetry.scheduler_fluid_ring_indices = telemetry.fluid_ring_indices;
        telemetry.scheduler_resident_fluid_entities = telemetry.resident_fluid_entities;
        telemetry.scheduler_resident_fluid_vertices = telemetry.resident_fluid_vertices;
        telemetry.scheduler_resident_fluid_indices = telemetry.resident_fluid_indices;
        telemetry.scheduler_resident_fluid_mesh_bytes = telemetry.resident_fluid_mesh_bytes;
        telemetry.last_fluid_vertices = 600;
        telemetry.last_fluid_indices = 1_800;
        telemetry.resident_observation_valid = true;
        telemetry.resident_entity_count_overflow = false;
        telemetry.resident_duplicate_levels = 0;
        telemetry.resident_out_of_range_levels = 0;
        telemetry.resident_scheduler_mismatch = false;
        telemetry.resident_budget_exceeded = false;
        telemetry.resident_observation_rejections = 0;
        let snapshot = qa_planetary_streaming(Some(&telemetry)).expect("telemetry snapshot");
        assert_eq!(snapshot.enabled, telemetry.enabled);
        assert_eq!(snapshot.far_radius_metres, telemetry.far_radius_metres);
        assert_eq!(
            snapshot.confirmed_near_extent_metres,
            telemetry.confirmed_near_extent_metres
        );
        assert_eq!(
            snapshot.near_coverage_ready_columns,
            telemetry.near_coverage_ready_columns
        );
        assert_eq!(
            snapshot.near_coverage_hidden_cells,
            telemetry.near_coverage_hidden_cells
        );
        assert_eq!(snapshot.budget_entities, telemetry.budget_entities);
        assert_eq!(snapshot.budget_vertices, telemetry.budget_vertices);
        assert_eq!(snapshot.budget_indices, telemetry.budget_indices);
        assert_eq!(snapshot.ring_vertices, telemetry.ring_vertices);
        assert_eq!(snapshot.ring_indices, telemetry.ring_indices);
        assert_eq!(snapshot.scheduler_ring_vertices, telemetry.ring_vertices);
        assert_eq!(snapshot.scheduler_ring_indices, telemetry.ring_indices);
        assert_eq!(
            snapshot.scheduler_resident_entities,
            snapshot.resident_entities
        );
        assert_eq!(
            snapshot.scheduler_resident_vertices,
            snapshot.resident_vertices
        );
        assert_eq!(
            snapshot.scheduler_resident_indices,
            snapshot.resident_indices
        );
        assert_eq!(
            snapshot.scheduler_resident_mesh_bytes,
            snapshot.resident_mesh_bytes
        );
        assert!(snapshot.resident_observation_valid);
        assert!(!snapshot.resident_entity_count_overflow);
        assert_eq!(snapshot.resident_duplicate_levels, 0);
        assert_eq!(snapshot.resident_out_of_range_levels, 0);
        assert!(!snapshot.resident_scheduler_mismatch);
        assert!(!snapshot.resident_budget_exceeded);
        assert_eq!(snapshot.resident_observation_rejections, 0);
        assert_eq!(snapshot.hydro_mode, "DescriptiveV1");
        assert_eq!(snapshot.fluid_ring_vertices, telemetry.fluid_ring_vertices);
        assert_eq!(snapshot.fluid_ring_indices, telemetry.fluid_ring_indices);
        assert_eq!(
            snapshot.scheduler_fluid_ring_vertices,
            snapshot.fluid_ring_vertices
        );
        assert_eq!(
            snapshot.scheduler_fluid_ring_indices,
            snapshot.fluid_ring_indices
        );
        assert_eq!(
            snapshot.scheduler_resident_fluid_entities,
            snapshot.resident_fluid_entities
        );
        assert_eq!(
            snapshot.scheduler_resident_fluid_vertices,
            snapshot.resident_fluid_vertices
        );
        assert_eq!(
            snapshot.scheduler_resident_fluid_indices,
            snapshot.resident_fluid_indices
        );
        assert_eq!(
            snapshot.scheduler_resident_fluid_mesh_bytes,
            snapshot.resident_fluid_mesh_bytes
        );
        assert!(snapshot.resident_fluid_observation_valid);
        assert!(!snapshot.resident_fluid_entity_count_overflow);
        assert_eq!(snapshot.resident_fluid_duplicate_slots, 0);
        assert_eq!(snapshot.resident_fluid_out_of_range_levels, 0);
        assert!(!snapshot.resident_fluid_scheduler_mismatch);
        assert!(!snapshot.resident_fluid_budget_exceeded);
        assert_eq!(snapshot.resident_fluid_observation_rejections, 0);
        assert_eq!(
            snapshot.fluid_ring_vertices.iter().sum::<usize>(),
            snapshot.resident_fluid_vertices
        );
        assert_eq!(
            snapshot.fluid_ring_indices.iter().sum::<usize>(),
            snapshot.resident_fluid_indices
        );
        assert_eq!(
            snapshot.budget_fluid_entities,
            telemetry.budget_fluid_entities
        );
        assert_eq!(
            snapshot.budget_fluid_vertices,
            telemetry.budget_fluid_vertices
        );
        assert_eq!(
            snapshot.budget_fluid_indices,
            telemetry.budget_fluid_indices
        );
        assert_eq!(
            snapshot.budget_fluid_mesh_bytes,
            telemetry.budget_fluid_mesh_bytes
        );
        assert_eq!(
            snapshot.budget_atomic_ring_build_bytes,
            telemetry.budget_atomic_ring_build_bytes
        );
        assert_eq!(
            snapshot.ring_vertices.iter().sum::<usize>(),
            snapshot.resident_vertices
        );
        assert_eq!(
            snapshot.ring_indices.iter().sum::<usize>(),
            snapshot.resident_indices
        );
        assert_eq!(snapshot.budget_mesh_bytes, telemetry.budget_mesh_bytes);
        assert_eq!(snapshot.live_sample_cache_windows, 5);
        assert_eq!(snapshot.live_sample_cache_bytes, 420_000);
        assert_eq!(snapshot.peak_live_sample_cache_windows, 6);
        assert_eq!(snapshot.peak_live_sample_cache_bytes, 510_000);
        assert_eq!(
            snapshot.budget_sample_cache_bytes,
            telemetry.budget_sample_cache_bytes
        );
        assert_eq!(
            snapshot.budget_coverage_work_bytes,
            telemetry.budget_coverage_work_bytes
        );
        assert_eq!(snapshot.pending_rebuilds, 0);
        assert!(!snapshot.build_in_flight);
        assert_eq!(snapshot.material_detail, "Reduced");
        assert_eq!(
            snapshot.desired_material_detail,
            ["Detailed", "Detailed", "Reduced", "Reduced", "Reduced", "Reduced"]
        );
        assert_eq!(
            snapshot.resident_material_detail,
            [
                Some("Detailed".to_owned()),
                Some("Detailed".to_owned()),
                Some("Reduced".to_owned()),
                None,
                None,
                None,
            ]
        );
        assert_eq!(snapshot.resident_detailed_levels, 2);
        assert_eq!(snapshot.resident_reduced_levels, 1);
        assert_eq!(snapshot.surface_material_mode, "LegacyPalette");
        assert_eq!(snapshot.last_material_slope_queries, 4_225);
        assert_eq!(snapshot.last_bridge_v2_cell_reuses, 3_584);
        assert_eq!(snapshot.last_fluid_classification_queries, 3_721);
        assert_eq!(snapshot.last_fluid_biome_queries, 835);
        assert_eq!(snapshot.last_fluid_vertices, 600);
        assert_eq!(snapshot.last_fluid_indices, 1_800);
    }

    #[test]
    fn visual_route_waits_for_real_streaming_and_has_a_bounded_fallback() {
        assert!(!qa_stream_ready(0.0, 0, 0));
        assert!(!qa_stream_ready(2.99, 0, 0));
        assert!(!qa_stream_ready(5.0, 5, 0));
        assert!(!qa_stream_ready(5.0, 0, 7));
        assert!(qa_stream_ready(3.0, 4, 6));
        assert!(qa_stream_ready(14.0, usize::MAX, usize::MAX));
    }

    #[test]
    fn qa_numeric_environment_values_reject_non_finite_inputs() {
        assert_eq!(parse_finite_f32(" 8.25 "), Some(8.25));
        assert_eq!(parse_finite_f32("-12"), Some(-12.0));
        assert_eq!(parse_finite_f32("NaN"), None);
        assert_eq!(parse_finite_f32("inf"), None);
        assert_eq!(parse_finite_f32("-inf"), None);
        assert_eq!(parse_finite_f32("many"), None);
    }

    #[test]
    fn qa_scenery_parser_is_explicit_case_insensitive_and_fail_closed() {
        assert_eq!(parse_scenery_quality(" off "), Some(SceneryQuality::Off));
        assert_eq!(parse_scenery_quality("LEAN"), Some(SceneryQuality::Lean));
        assert_eq!(
            parse_scenery_quality("Balanced"),
            Some(SceneryQuality::Balanced)
        );
        assert_eq!(parse_scenery_quality("lush"), Some(SceneryQuality::Lush));
        assert_eq!(parse_scenery_quality("ultra"), None);
        assert_eq!(parse_scenery_quality(""), None);
    }

    #[test]
    fn screenshot_schedule_keeps_the_final_boundary_but_never_runs_past_it() {
        assert!(!qa_screenshot_due(2.49, 2.5, 10.0));
        assert!(qa_screenshot_due(2.5, 2.5, 10.0));
        assert!(qa_screenshot_due(10.013, 10.0, 10.0));
        assert!(!qa_screenshot_due(12.5, 12.5, 10.0));
        assert!(!qa_screenshot_due(f32::NAN, 2.5, 10.0));
        assert!(!qa_screenshot_due(2.5, f32::INFINITY, 10.0));
        assert!(!qa_screenshot_due(2.5, 2.5, f32::NEG_INFINITY));
    }

    #[test]
    fn astral_hero_route_inspects_precinct_landing_detail_and_world_context() {
        let landing = Vec2::new(-124.0, 24.0);
        let landing_height = -63.0;
        let mut observed = [false; 4];
        for step in 0..=400 {
            let progress = step as f32 / 400.0;
            let sample = qa_hero_route_sample(progress, landing, landing_height, 200.0);
            assert!(sample.camera_offset.is_finite());
            assert!(sample.target_offset.is_finite());
            assert!(sample.camera_height.is_finite());
            assert!(sample.target_height.is_finite());
            assert!(sample.terrain_clearance >= 18.0);
            assert!(sample.camera_offset.distance(sample.target_offset) <= 200.001);
            observed[sample.phase as usize] = true;
        }
        assert!(observed.into_iter().all(|phase| phase));

        let approach = qa_hero_route_sample(0.44, landing, landing_height, 200.0);
        assert!(approach.camera_offset.distance(landing) < 30.0);
        assert!(approach.camera_height <= landing_height + 33.0);

        let detail = qa_hero_route_sample(0.58, landing, landing_height, 200.0);
        assert!(detail.camera_offset.length() < 80.0);
        assert!(detail.target_offset.length() < 6.0);

        let context = qa_hero_route_sample(1.0, landing, landing_height, 200.0);
        assert!(context.camera_offset.length() > 160.0);
        assert!(context.camera_height >= 80.0);
    }

    #[test]
    fn waypoint_route_moves_close_to_the_structure_then_restores_global_context() {
        let axis = qa_waypoint_axis(Vec3::new(912.5, 88.0, -1376.5));
        assert!((axis.length() - 1.0).abs() < 0.0001);
        assert_eq!(axis, qa_waypoint_axis(Vec3::new(912.5, 12.0, -1376.5)));

        let establish = qa_waypoint_route_sample(0.0, axis, 180.0);
        let approach = qa_waypoint_route_sample(0.44, axis, 180.0);
        let detail = qa_waypoint_route_sample(0.58, axis, 180.0);
        let context = qa_waypoint_route_sample(1.0, axis, 180.0);
        assert!(establish.camera_offset.length() > 110.0);
        assert!(approach.camera_offset.length() < 30.0);
        assert!(detail.camera_offset.length() < 30.0);
        assert!(detail.camera_height >= 17.0);
        assert!(context.camera_offset.length() > 120.0);
        assert!(context.camera_height >= 70.0);

        let constrained = qa_waypoint_route_sample(0.0, axis, 64.0);
        assert!(
            constrained
                .camera_offset
                .distance(constrained.target_offset)
                <= 64.001
        );
    }

    #[test]
    fn streaming_route_crosses_kilometres_without_unbounded_lookahead() {
        let requested = 8_000.0;
        let mut previous_x = f32::NEG_INFINITY;
        let mut phases = [false; 4];
        for step in 0..=1_000 {
            let progress = step as f32 / 1_000.0;
            let sample = qa_streaming_route_sample(progress, requested);
            assert!(sample.camera_offset.is_finite());
            assert!(sample.target_offset.is_finite());
            assert!(sample.camera_offset.x >= previous_x - 0.001);
            assert!(sample.camera_offset.distance(sample.target_offset) <= 240.001);
            assert!(sample.terrain_clearance >= 46.0);
            phases[sample.phase as usize] = true;
            previous_x = sample.camera_offset.x;
        }
        let final_sample = qa_streaming_route_sample(1.0, requested);
        assert!((final_sample.camera_offset.x - requested).abs() < 0.001);
        assert!(phases.into_iter().all(|observed| observed));

        let fallback = qa_streaming_route_sample(1.0, f32::NAN);
        assert_eq!(fallback.camera_offset.x, 8_000.0);
        let clamped = qa_streaming_route_sample(1.0, f32::MAX);
        assert_eq!(clamped.camera_offset.x, 100_000.0);
    }

    #[test]
    fn route_phase_boundaries_are_explicit_and_cover_the_full_story() {
        assert_eq!(qa_route_phase(0.0), QaRoutePhase::Establishing);
        assert_eq!(qa_route_phase(0.199), QaRoutePhase::Establishing);
        assert_eq!(qa_route_phase(0.20), QaRoutePhase::Approach);
        assert_eq!(qa_route_phase(0.439), QaRoutePhase::Approach);
        assert_eq!(qa_route_phase(0.44), QaRoutePhase::Detail);
        assert_eq!(qa_route_phase(0.719), QaRoutePhase::Detail);
        assert_eq!(qa_route_phase(0.72), QaRoutePhase::Context);
        assert_eq!(qa_route_phase(1.0), QaRoutePhase::Context);
    }

    #[test]
    fn stall_clock_is_non_negative_monotonic_and_stage_explicit() {
        let warmup = qa_stall_timing(2.75, None);
        assert_eq!(warmup, (QaStallStage::Warmup, 2.75, None));

        let route = qa_stall_timing(3.0, Some(1.25));
        assert_eq!(route, (QaStallStage::Route, 4.25, Some(1.25)));
        assert!(route.1 > warmup.1);

        let sanitized = qa_stall_timing(-4.0, Some(-2.0));
        assert_eq!(sanitized, (QaStallStage::Route, 0.0, Some(0.0)));
    }

    #[test]
    fn astral_anchor_waits_for_the_generator_profile_handoff() {
        assert!(!qa_profile_anchor_ready(
            WorldProfile::AstralFrontier,
            WorldProfile::Natural
        ));
        assert!(qa_profile_anchor_ready(
            WorldProfile::AstralFrontier,
            WorldProfile::AstralFrontier
        ));
        assert!(qa_profile_anchor_ready(
            WorldProfile::Natural,
            WorldProfile::Natural
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn screenshot_readiness_requires_the_terminal_png_iend_chunk() {
        let mut complete = [0_u8; 12];
        complete[4..8].copy_from_slice(b"IEND");
        assert!(qa_png_tail_is_complete(&complete));

        let mut partial = complete;
        partial[4..8].copy_from_slice(b"IDAT");
        assert!(!qa_png_tail_is_complete(&partial));

        let mut malformed = complete;
        malformed[3] = 1;
        assert!(!qa_png_tail_is_complete(&malformed));
    }
}
