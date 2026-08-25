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
use bevy::time::Real;
use bevy::window::PrimaryWindow;
use serde::Serialize;

use crate::blocks::{BlockType, Voxel, AIR};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::planetary_streaming::{
    FarFieldL0HeightMode, FarFieldSurfaceMaterialMode, PlanetaryStreamingTelemetry,
    FAR_FIELD_LEVELS, FAR_FIELD_MAX_UPDATE_CADENCE_FRAMES, FAR_FIELD_SEMANTIC_COHORT_KIND_COUNT,
};
use crate::player::Player;
use crate::settings::{
    ActiveWorld, SceneryQuality, TerrainGrammarVersion, TimeMode, WorldGenerationIdentity,
    WorldMeta, WorldProfile, WorldSettings,
};
use crate::terrain::{Biome, TerrainGenerator, WATER_LEVEL};
use crate::world::{
    ChunkStreamer, PendingEditedOverrideStore, StreamingGovernor, VoxelWorld,
    MAX_FULL_CHUNK_RESIDENT,
};

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
/// A state/generator handoff is normally a few frames. A bounded wall-clock
/// window guarantees a mismatched generator produces an explicit blocked
/// report instead of leaving an unattended QA process alive indefinitely.
const QA_GENERATOR_HANDOFF_TIMEOUT_SECONDS: f32 = 20.0;
/// Versioned serialized QA contract. Version 2.5 adds an explicit exact and
/// peak proof of the combined resident-plus-in-flight dense chunk budget.
const QA_REPORT_SCHEMA_VERSION: &str = "2.5.0";
/// Deliberately unsupported by the canonical evidence-manifest builder. The
/// alternate L0 height estimator and LOD-provenance palette are diagnostic
/// axes, so their reports must not be mistaken for current release evidence
/// even though they use the same QA harness and executable provenance contract.
const QA_DIAGNOSTIC_L0_HEIGHT_REPORT_SCHEMA_VERSION: &str =
    "2.5.0-diagnostic-l0-cardinal-trimmed-8-v1";
const QA_DIAGNOSTIC_LOD_PROVENANCE_REPORT_SCHEMA_VERSION: &str =
    "2.5.0-diagnostic-lod-provenance-v1";
const QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_REPORT_SCHEMA_VERSION: &str =
    "2.5.0-diagnostic-l0-cardinal-trimmed-8-v1-lod-provenance-v1";
const QA_CANONICAL_EVIDENCE_DISPOSITION: &str = "canonical-candidate";
const QA_DIAGNOSTIC_EVIDENCE_DISPOSITION: &str = "diagnostic-only-non-publishable";
const QA_DIAGNOSTIC_LOD_PROVENANCE_EVIDENCE_DISPOSITION: &str =
    "diagnostic-lod-provenance-only-non-publishable";
const QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_EVIDENCE_DISPOSITION: &str =
    "diagnostic-l0-height-and-lod-provenance-only-non-publishable";
const QA_TERRAIN_GRAMMAR_ENV: &str = "VOXEL_NATIVE_QA_TERRAIN_GRAMMAR";

/// The public QA environment clamps route duration to 600 seconds and capture
/// cadence to at least one second. The round-number ceiling leaves two spare
/// slots beyond the 598 captures possible from the 2.5-second first capture.
const QA_SCREENSHOT_OBSERVATION_CAP: usize = 600;
const QA_SCREENSHOT_PATH_MAX_CHARS: usize = 512;
const QA_SCREENSHOT_LEDGER_BYTE_CAP: usize = 1024 * 1024;
/// Screenshot readback is normally complete within two frames.  This remains
/// a short, independent write bound; it is not a scheduler-settlement budget.
#[cfg(not(target_arch = "wasm32"))]
const QA_SCREENSHOT_WRITE_TIMEOUT_SECONDS: f32 = 3.0;
/// A completion timeout requires both this elapsed tail and the frame bound
/// below.  Slow machines therefore receive enough scheduler opportunities,
/// while fast machines cannot consume the frame allowance in a few seconds.
const QA_COMPLETION_SETTLE_TIMEOUT_SECONDS: f32 = 30.0;
const QA_COMPLETION_SETTLE_MIN_FRAMES: u16 = 96;
/// Real-time reserve for world entry, generator handoff, stream warmup, and
/// camera preflight before the requested route itself must have completed.
/// This is intentionally independent of virtual time and Player availability.
const QA_ROUTE_LIFECYCLE_RESERVE_SECONDS: f32 = 60.0;
/// A single empty queue snapshot is not proof of quiescence: the near-field
/// frontier and debounced L0 coverage observer can enqueue work on a later
/// update. Require both one real second and two maximum-cadence scheduler
/// windows of consecutive, complete settlement before accepting success.
const QA_COMPLETION_STABLE_SECONDS: f32 = 1.0;
const QA_COMPLETION_STABLE_MIN_FRAMES: u16 = 2 * FAR_FIELD_MAX_UPDATE_CADENCE_FRAMES as u16 + 1;
/// Two complete six-ring batches cover a pressure-driven detail transition
/// and one hysteresis reversal.  The extra poll in each batch accounts for
/// installing the final native asynchronous result.
const QA_COMPLETION_TWO_BATCH_MIN_FRAMES: u16 =
    2 * (FAR_FIELD_LEVELS as u16 * FAR_FIELD_MAX_UPDATE_CADENCE_FRAMES as u16 + 1);
const _: () = assert!(QA_COMPLETION_SETTLE_MIN_FRAMES >= QA_COMPLETION_TWO_BATCH_MIN_FRAMES);

/// Canonical authored VolcanicWaste lava fill ceiling in voxel/metre Y. This
/// mirrors terrain generation and Far Hydro v1; it is a world-design contract,
/// not a physical temperature or elevation claim.
const QA_VOLCANIC_LAVA_LEVEL: i32 = 52;
const QA_LAVA_FOCUS_SEARCH_MAX_RADIUS_METRES: i32 = 4_096;
/// QA deliberately keeps a coarser, independently versioned focus lattice.
/// Far-field L0 may become finer without silently quadrupling acceptance-search
/// work or changing the historical focus contract.
const QA_LAVA_FOCUS_STEP_METRES: i32 = 32;
const QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES: usize = {
    let cells = QA_LAVA_FOCUS_SEARCH_MAX_RADIUS_METRES / QA_LAVA_FOCUS_STEP_METRES;
    let side = cells as usize * 2 + 1;
    side * side
};
const _: () = assert!(QA_LAVA_FOCUS_STEP_METRES == 32);
const _: () = assert!(QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES == 66_049);

const QA_CAMERA_ROUTE_VARIANTS: usize = 8;
const QA_CAMERA_ROUTE_VALIDATION_SAMPLES: usize = 16;
const QA_CAMERA_BODY_QUERY_CAP_PER_POSE: usize = 48;
const QA_CAMERA_LOS_RAYS_PER_POSE: usize = 3;
const QA_CAMERA_LOS_QUERY_CAP_PER_RAY: usize = 384;
const QA_CAMERA_ROUTE_POSE_COUNT: usize =
    QA_CAMERA_ROUTE_VARIANTS * QA_CAMERA_ROUTE_VALIDATION_SAMPLES;
const QA_CAMERA_ROUTE_VOXEL_QUERY_CAP: usize = QA_CAMERA_ROUTE_POSE_COUNT
    * (QA_CAMERA_BODY_QUERY_CAP_PER_POSE
        + QA_CAMERA_LOS_RAYS_PER_POSE * QA_CAMERA_LOS_QUERY_CAP_PER_RAY);
const QA_CAMERA_FOLIAGE_HIT_CAP_PER_RAY: usize = 2;
const QA_CAMERA_PREFLIGHT_RESIDENCY_TIMEOUT_SECONDS: f32 = 14.0;
const QA_CAMERA_ROUTE_POLICY_ENV: &str = "VOXEL_NATIVE_QA_CAMERA_ROUTE_POLICY";
const QA_CAMERA_ROUTE_ALGORITHM_VERSION: u64 = 1;
const QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT: f64 = 16_777_216.0;
const _: () = assert!(QA_CAMERA_ROUTE_POSE_COUNT == 128);
const _: () = assert!(QA_CAMERA_ROUTE_VOXEL_QUERY_CAP == 153_600);

#[cfg(debug_assertions)]
const QA_BUILD_PROFILE: &str = "debug";
#[cfg(not(debug_assertions))]
const QA_BUILD_PROFILE: &str = "release";

pub struct QaPlugin;

impl Plugin for QaPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(QaAutopilot::from_env())
            .add_systems(
                Update,
                (
                    qa_enter_game,
                    qa_drive_camera.run_if(in_state(GameState::InGame)),
                    qa_capture_screenshot.run_if(in_state(GameState::InGame)),
                )
                    .chain(),
            )
            // Completion certifies the final state produced by every Update
            // system, including WorldSet::Mesh and the chained planetary
            // scheduler/deferred-command/residency observer.
            .add_systems(PostUpdate, qa_finish.run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Debug)]
struct QaAutopilot {
    enabled: bool,
    started: bool,
    finished: bool,
    elapsed: f32,
    warmup_elapsed: f32,
    generator_handoff_elapsed: f32,
    lifecycle_elapsed: f32,
    write_tail_elapsed: f32,
    route_ready: bool,
    duration: f32,
    screenshot_interval: f32,
    next_screenshot_at: f32,
    screenshot_index: usize,
    finish_wait_frames: u16,
    settled_wait_frames: u16,
    settled_wait_elapsed: f32,
    origin: Vec3,
    origin_set: bool,
    requested_focus_label: String,
    focus: QaFocus,
    resolved_focus: QaFocus,
    focus_anchor: Option<QaFocusAnchor>,
    focus_evidence_ready: bool,
    focus_unavailable_reason: Option<String>,
    focus_search_visited_candidates: Option<usize>,
    focus_classification_queries: Option<usize>,
    focus_search_cap_exhausted: bool,
    focus_search_candidate_cap: usize,
    focus_classification_query_cap: usize,
    generator_signature: Option<QaGeneratorSignature>,
    camera_route_policy: QaCameraRoutePolicy,
    camera_route_preflight_pending: bool,
    camera_route_preflight_elapsed: f32,
    camera_route_plan: Option<QaCameraRoutePlan>,
    camera_route_available: bool,
    camera_route_unavailable_reason: Option<QaCameraRouteUnavailableReason>,
    camera_route_validation: QaCameraRouteValidation,
    streaming_distance_m: f32,
    current_phase: QaRoutePhase,
    route_frame_times: QaFrameTimeAccumulator,
    peak_loaded_chunks: usize,
    peak_dense_chunks: usize,
    dense_chunk_budget_exceeded: bool,
    peak_mesh_entities: usize,
    peak_pending_terrain: usize,
    peak_pending_meshes: usize,
    peak_dirty_chunks: usize,
    max_horizontal_displacement_m: f32,
    stalls: Vec<QaStall>,
    screenshots: Vec<String>,
    screenshot_observations: Vec<QaScreenshotObservation>,
    screenshot_observation_rejections: usize,
    screenshot_observation_cap_exhausted: bool,
    #[cfg(not(target_arch = "wasm32"))]
    report_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QaFocus {
    Scenic,
    Waypoint,
    Streaming,
    River,
    Lava,
    NearFar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum QaCameraRoutePolicy {
    Legacy,
    PreflightV1,
}

impl QaCameraRoutePolicy {
    fn from_env() -> Self {
        match std::env::var(QA_CAMERA_ROUTE_POLICY_ENV)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("legacy") => Self::Legacy,
            Some("preflight-v1" | "preflight_v1" | "preflight") | None => Self::PreflightV1,
            Some(value) => {
                warn!(
                    "QA: unknown {QA_CAMERA_ROUTE_POLICY_ENV}={value:?}; using fail-closed preflight-v1"
                );
                Self::PreflightV1
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::PreflightV1 => "preflight-v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum QaCameraRouteUnavailableReason {
    FocusUnavailable,
    ChunksUnloaded,
    BodyOccluded,
    LineOfSightOccluded,
    WorkCap,
    CoordinateRange,
}

impl QaCameraRouteUnavailableReason {
    const fn label(self) -> &'static str {
        match self {
            Self::FocusUnavailable => "camera-route-focus-unavailable",
            Self::ChunksUnloaded => "camera-route-chunks-unloaded",
            Self::BodyOccluded => "camera-route-body-occluded",
            Self::LineOfSightOccluded => "camera-route-los-occluded",
            Self::WorkCap => "camera-route-work-cap",
            Self::CoordinateRange => "camera-route-coordinate-range",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QaCameraRoutePlan {
    variant_index: u8,
    plan_hash: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct QaCameraRouteValidation {
    variant_count: usize,
    validation_samples: usize,
    selected_clear_samples: usize,
    voxel_queries: usize,
    voxel_query_cap: usize,
    required_chunk_checks: usize,
    loaded_chunk_checks: usize,
    proven_air_chunk_checks: usize,
    unloaded_chunk_checks: usize,
    candidate_body_occlusions: usize,
    candidate_los_occlusions: usize,
    minimum_clearance_voxels: Option<u16>,
    work_cap_exhausted: bool,
}

impl Default for QaCameraRouteValidation {
    fn default() -> Self {
        Self {
            variant_count: 0,
            validation_samples: 0,
            selected_clear_samples: 0,
            voxel_queries: 0,
            voxel_query_cap: 0,
            required_chunk_checks: 0,
            loaded_chunk_checks: 0,
            proven_air_chunk_checks: 0,
            unloaded_chunk_checks: 0,
            candidate_body_occlusions: 0,
            candidate_los_occlusions: 0,
            minimum_clearance_voxels: None,
            work_cap_exhausted: false,
        }
    }
}

impl QaFocus {
    fn parse_env_value(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("waypoint") => Some(Self::Waypoint),
            Some("streaming") => Some(Self::Streaming),
            Some("river") => Some(Self::River),
            Some("lava") => Some(Self::Lava),
            Some("near-far" | "near_far" | "nearfar") => Some(Self::NearFar),
            Some("scenic") | None => Some(Self::Scenic),
            Some(_) => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Scenic => "scenic",
            Self::Waypoint => "waypoint",
            Self::Streaming => "streaming",
            Self::River => "river",
            Self::Lava => "lava",
            Self::NearFar => "near-far",
        }
    }

    const fn requires_hydro_anchor(self) -> bool {
        matches!(self, Self::River | Self::Lava | Self::NearFar)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QaFocusAnchor {
    world_x: i32,
    fluid_y: i32,
    world_z: i32,
}

impl QaFocusAnchor {
    fn render_origin(self) -> Vec3 {
        Vec3::new(
            self.world_x as f32 + 0.5,
            self.fluid_y as f32 + 0.94,
            self.world_z as f32 + 0.5,
        )
    }

    const fn report_value(self) -> [i32; 3] {
        [self.world_x, self.fluid_y, self.world_z]
    }
}

fn qa_checked_lattice_coordinate(cell: i32, step_metres: i32) -> Option<i32> {
    cell.checked_mul(step_metres)
}

fn qa_lava_corner_matches(generator: &TerrainGenerator, world_x: i32, world_z: i32) -> bool {
    generator.surface_height_at(world_x, world_z) < QA_VOLCANIC_LAVA_LEVEL
        && generator.biome_at(world_x, world_z) == Biome::VolcanicWaste
}

/// Find a complete L0 Far Hydro lava quad, not merely one volcanic metre
/// sample. Candidates are enumerated in deterministic Chebyshev rings on the
/// exact 32 m Euclidean lattice. Each arithmetically representable candidate
/// performs exactly four corner classifications, all coordinates use checked
/// arithmetic, and the hard cap is independent of terrain contents.
fn qa_find_lava_focus(
    generator: &TerrainGenerator,
    origin_x: i32,
    origin_z: i32,
    requested_radius_metres: i32,
) -> QaFocusSearchResult {
    let step = QA_LAVA_FOCUS_STEP_METRES;
    let max_radius = requested_radius_metres
        .max(0)
        .min(QA_LAVA_FOCUS_SEARCH_MAX_RADIUS_METRES);
    let max_cells = max_radius.div_euclid(step);
    let origin_cell_x = origin_x.div_euclid(step);
    let origin_cell_z = origin_z.div_euclid(step);
    let mut candidate_count = 0usize;
    let mut classification_queries = 0usize;

    for radius in 0..=max_cells {
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs().max(dz.abs()) != radius {
                    continue;
                }
                if candidate_count >= QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES {
                    return QaFocusSearchResult {
                        anchor: None,
                        visited_candidates: candidate_count,
                        classification_queries,
                        candidate_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES,
                        classification_query_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES * 4,
                    };
                }
                candidate_count += 1;
                let Some(cell_x) = origin_cell_x.checked_add(dx) else {
                    continue;
                };
                let Some(cell_z) = origin_cell_z.checked_add(dz) else {
                    continue;
                };
                let Some(x0) = qa_checked_lattice_coordinate(cell_x, step) else {
                    continue;
                };
                let Some(z0) = qa_checked_lattice_coordinate(cell_z, step) else {
                    continue;
                };
                let Some(x1) = x0.checked_add(step) else {
                    continue;
                };
                let Some(z1) = z0.checked_add(step) else {
                    continue;
                };
                let corners = [(x0, z0), (x1, z0), (x1, z1), (x0, z1)];
                let mut complete_lava_quad = true;
                for (world_x, world_z) in corners {
                    classification_queries = classification_queries.saturating_add(1);
                    complete_lava_quad &= qa_lava_corner_matches(generator, world_x, world_z);
                }
                if complete_lava_quad {
                    let Some(world_x) = x0.checked_add(step / 2) else {
                        continue;
                    };
                    let Some(world_z) = z0.checked_add(step / 2) else {
                        continue;
                    };
                    return QaFocusSearchResult {
                        anchor: Some(QaFocusAnchor {
                            world_x,
                            fluid_y: QA_VOLCANIC_LAVA_LEVEL,
                            world_z,
                        }),
                        visited_candidates: candidate_count,
                        classification_queries,
                        candidate_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES,
                        classification_query_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES * 4,
                    };
                }
            }
        }
    }

    QaFocusSearchResult {
        anchor: None,
        visited_candidates: candidate_count,
        classification_queries,
        candidate_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES,
        classification_query_cap: QA_LAVA_FOCUS_SEARCH_MAX_CANDIDATES * 4,
    }
}

fn qa_find_hydro_focus(
    generator: &TerrainGenerator,
    focus: QaFocus,
    world_profile: WorldProfile,
) -> QaFocusSearchResult {
    match focus {
        QaFocus::River if world_profile == WorldProfile::Natural => {
            let anchor =
                generator
                    .find_hydrographic_focus(0, 0, 4_096)
                    .map(|point| QaFocusAnchor {
                        world_x: point.x,
                        fluid_y: WATER_LEVEL,
                        world_z: point.y,
                    });
            // Terrain's bounded river search currently caps at 263,169
            // candidates; its API does not expose the early-stop count.
            QaFocusSearchResult {
                anchor,
                visited_candidates: 0,
                classification_queries: 0,
                candidate_cap: crate::terrain::HYDROGRAPHIC_SEARCH_MAX_CANDIDATES,
                classification_query_cap: crate::terrain::HYDROGRAPHIC_SEARCH_MAX_CANDIDATES,
            }
        }
        QaFocus::Lava if world_profile == WorldProfile::AstralFrontier => {
            qa_find_lava_focus(generator, 0, 0, QA_LAVA_FOCUS_SEARCH_MAX_RADIUS_METRES)
        }
        QaFocus::NearFar => match world_profile {
            WorldProfile::Natural => qa_find_hydro_focus(generator, QaFocus::River, world_profile),
            WorldProfile::AstralFrontier => {
                qa_find_hydro_focus(generator, QaFocus::Lava, world_profile)
            }
        },
        _ => QaFocusSearchResult {
            anchor: None,
            visited_candidates: 0,
            classification_queries: 0,
            candidate_cap: 0,
            classification_query_cap: 0,
        },
    }
}

fn qa_focus_has_exact_search_work(focus: QaFocus, world_profile: WorldProfile) -> bool {
    matches!(
        (focus, world_profile),
        (QaFocus::Lava, WorldProfile::AstralFrontier)
            | (QaFocus::NearFar, WorldProfile::AstralFrontier)
    )
}

fn qa_focus_search_exhausted(result: QaFocusSearchResult) -> bool {
    result.anchor.is_none()
        && result.candidate_cap > 0
        && result.classification_query_cap > 0
        && (result.visited_candidates == result.candidate_cap
            || result.classification_queries == result.classification_query_cap)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QaFocusSearchResult {
    anchor: Option<QaFocusAnchor>,
    visited_candidates: usize,
    classification_queries: usize,
    candidate_cap: usize,
    classification_query_cap: usize,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
struct QaScreenshotObservation {
    capture_index: usize,
    screenshot_path: String,
    scheduled_capture_seconds: f32,
    player_camera_translation_metres: [f32; 3],
    player_camera_rotation_xyzw: [f32; 4],
}

// This accounts for the observation record, its owned path, and the duplicate
// legacy `screenshots` path retained for report compatibility. Both vectors
// are preallocated to the same fixed cap when QA is enabled.
const _: () = assert!(
    QA_SCREENSHOT_OBSERVATION_CAP
        * (std::mem::size_of::<QaScreenshotObservation>()
            + std::mem::size_of::<String>()
            + 2 * QA_SCREENSHOT_PATH_MAX_CHARS)
        <= QA_SCREENSHOT_LEDGER_BYTE_CAP
);

#[derive(Debug, Serialize)]
struct QaReport {
    qa_report_schema_version: String,
    evidence_disposition: String,
    run_identity: QaRunIdentity,
    world_edit_store_status: String,
    world_edit_store_compatible: bool,
    world_edit_store_seed: Option<u32>,
    world_edit_store_profile: Option<String>,
    world_edit_store_scenery_quality: Option<String>,
    world_edit_store_terrain_grammar: Option<String>,
    world_edit_store_edited_chunks: Option<usize>,
    world_edit_store_block_reason_code: Option<String>,
    viewport: Option<QaViewport>,
    planetary_streaming: Option<QaPlanetaryStreaming>,
    requested_route_focus: String,
    resolved_route_focus: String,
    route_focus_available: bool,
    route_focus_unavailable_reason: Option<String>,
    route_focus_anchor: Option<[i32; 3]>,
    route_focus_search_candidate_cap: usize,
    route_focus_search_visited_candidates: Option<usize>,
    route_focus_classification_query_cap: usize,
    route_focus_classification_queries: Option<usize>,
    route_focus_search_cap_exhausted: bool,
    camera_route_preflight_applicable: bool,
    camera_route_policy: String,
    camera_route_plan_hash: Option<String>,
    camera_route_available: bool,
    camera_route_unavailable_reason: Option<String>,
    camera_route_variant_index: Option<u8>,
    camera_route_variant_count: usize,
    camera_route_validation_samples: usize,
    camera_route_selected_clear_samples: usize,
    camera_route_voxel_queries: usize,
    camera_route_voxel_query_cap: usize,
    camera_route_required_chunk_checks: usize,
    camera_route_loaded_chunk_checks: usize,
    camera_route_proven_air_chunk_checks: usize,
    camera_route_unloaded_chunk_checks: usize,
    camera_route_candidate_body_occlusions: usize,
    camera_route_candidate_los_occlusions: usize,
    camera_route_minimum_clearance_voxels: Option<u16>,
    camera_route_work_cap_exhausted: bool,
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
    dense_chunks: usize,
    dense_chunk_budget: usize,
    dense_chunk_budget_exceeded: bool,
    frontier_complete: bool,
    render_distance: i32,
    peak_loaded_chunks: usize,
    peak_dense_chunks: usize,
    peak_mesh_entities: usize,
    peak_pending_terrain: usize,
    peak_pending_meshes: usize,
    peak_dirty_chunks: usize,
    screenshots: Vec<String>,
    screenshot_observation_cap: usize,
    screenshot_path_max_chars: usize,
    screenshot_observation_count: usize,
    screenshot_observation_valid: bool,
    screenshot_observation_cap_exhausted: bool,
    screenshot_observation_rejections: usize,
    screenshot_observations: Vec<QaScreenshotObservation>,
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
    terrain_grammar: Option<String>,
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
    desired_terrain_grammar: Option<String>,
    active_terrain_grammar: Option<String>,
    desired_l0_height_mode: String,
    active_l0_height_mode: Option<String>,
    resident_l0_height_mode: Option<String>,
    l0_probe_spacing_metres: i64,
    budget_l0_height_queries: usize,
    interaction_radius_metres: i64,
    confirmed_near_extent_metres: i64,
    near_coverage_ready_columns: usize,
    near_coverage_hidden_cells: usize,
    near_coverage_transition_pending: bool,
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
    resident_water_indices: usize,
    resident_lava_indices: usize,
    water_ring_indices: [usize; FAR_FIELD_LEVELS],
    lava_ring_indices: [usize; FAR_FIELD_LEVELS],
    resident_fluid_mesh_bytes: usize,
    resident_semantic_cohort_entities: usize,
    resident_semantic_cohort_vertices: usize,
    resident_semantic_cohort_indices: usize,
    resident_semantic_cohort_mesh_bytes: usize,
    resident_semantic_cohort_count: usize,
    resident_semantic_cohort_kind_counts: [usize; FAR_FIELD_SEMANTIC_COHORT_KIND_COUNT],
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
    scheduler_resident_water_indices: usize,
    scheduler_resident_lava_indices: usize,
    scheduler_water_ring_indices: [usize; FAR_FIELD_LEVELS],
    scheduler_lava_ring_indices: [usize; FAR_FIELD_LEVELS],
    scheduler_resident_fluid_mesh_bytes: usize,
    scheduler_resident_semantic_cohort_entities: usize,
    scheduler_resident_semantic_cohort_vertices: usize,
    scheduler_resident_semantic_cohort_indices: usize,
    scheduler_resident_semantic_cohort_mesh_bytes: usize,
    scheduler_resident_semantic_cohort_count: usize,
    scheduler_resident_semantic_cohort_kind_counts: [usize; FAR_FIELD_SEMANTIC_COHORT_KIND_COUNT],
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
    resident_fluid_kind_integrity_valid: bool,
    resident_fluid_observation_rejections: u64,
    resident_semantic_cohort_observation_valid: bool,
    resident_semantic_cohort_entity_count_overflow: bool,
    resident_semantic_cohort_scheduler_mismatch: bool,
    resident_semantic_cohort_budget_exceeded: bool,
    resident_semantic_cohort_payload_integrity_valid: bool,
    resident_semantic_cohort_observation_rejections: u64,
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
    budget_hydro_atomic_ring_build_bytes: usize,
    budget_atomic_ring_build_bytes: usize,
    budget_semantic_cohort_entities: usize,
    budget_semantic_cohort_vertices: usize,
    budget_semantic_cohort_indices: usize,
    budget_semantic_cohort_mesh_bytes: usize,
    budget_semantic_cohort_hash_scans: usize,
    budget_semantic_cohort_height_queries: usize,
    budget_semantic_cohort_biome_queries: usize,
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
    semantic_cohort_mode: String,
    scheduler_deferred_frames: u64,
    completed_rebuilds: u64,
    stale_builds_discarded: u64,
    budget_rejections: u64,
    last_build_ms: f32,
    max_build_ms: f32,
    last_height_queries: usize,
    last_l0_center_queries: usize,
    last_l0_half_x_queries: usize,
    last_l0_half_z_queries: usize,
    last_l0_trimmed_vertices: usize,
    last_l0_trimmed_up_vertices: usize,
    last_l0_trimmed_down_vertices: usize,
    last_l0_max_abs_adjustment_metres: f32,
    last_l0_cache_update: String,
    last_l0_cache_shift_x_cells: i32,
    last_l0_cache_shift_z_cells: i32,
    last_l0_reused_height_samples: usize,
    last_material_slope_queries: usize,
    last_bridge_v2_cell_reuses: usize,
    last_fluid_classification_queries: usize,
    last_fluid_biome_queries: usize,
    last_fluid_vertices: usize,
    last_fluid_indices: usize,
    last_water_indices: usize,
    last_lava_indices: usize,
    last_semantic_cohort_hash_scans: usize,
    last_semantic_cohort_height_queries: usize,
    last_semantic_cohort_biome_queries: usize,
    last_semantic_cohort_candidates: usize,
    last_semantic_cohort_emitted: usize,
    last_semantic_cohort_vertices: usize,
    last_semantic_cohort_indices: usize,
    last_semantic_cohort_kind_counts: [usize; FAR_FIELD_SEMANTIC_COHORT_KIND_COUNT],
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
        terrain_grammar: active_world.map(|world| format!("{:?}", world.meta.terrain_grammar)),
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

fn qa_report_contract(
    telemetry: Option<&PlanetaryStreamingTelemetry>,
) -> (&'static str, &'static str) {
    let Some(telemetry) = telemetry else {
        return (QA_REPORT_SCHEMA_VERSION, QA_CANONICAL_EVIDENCE_DISPOSITION);
    };
    let diagnostic_l0_height =
        telemetry.desired_l0_height_mode == FarFieldL0HeightMode::CardinalTrimmed8V1;
    let diagnostic_lod_provenance =
        telemetry.surface_material_mode == FarFieldSurfaceMaterialMode::LodProvenanceV1;

    match (diagnostic_l0_height, diagnostic_lod_provenance) {
        (false, false) => (QA_REPORT_SCHEMA_VERSION, QA_CANONICAL_EVIDENCE_DISPOSITION),
        (true, false) => (
            QA_DIAGNOSTIC_L0_HEIGHT_REPORT_SCHEMA_VERSION,
            QA_DIAGNOSTIC_EVIDENCE_DISPOSITION,
        ),
        (false, true) => (
            QA_DIAGNOSTIC_LOD_PROVENANCE_REPORT_SCHEMA_VERSION,
            QA_DIAGNOSTIC_LOD_PROVENANCE_EVIDENCE_DISPOSITION,
        ),
        (true, true) => (
            QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_REPORT_SCHEMA_VERSION,
            QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_EVIDENCE_DISPOSITION,
        ),
    }
}

fn qa_planetary_streaming(
    telemetry: Option<&PlanetaryStreamingTelemetry>,
) -> Option<QaPlanetaryStreaming> {
    let telemetry = telemetry?;
    Some(QaPlanetaryStreaming {
        enabled: telemetry.enabled,
        profile: format!("{:?}", telemetry.profile),
        desired_terrain_grammar: telemetry
            .desired_terrain_grammar
            .map(|grammar| format!("{grammar:?}")),
        active_terrain_grammar: telemetry
            .active_terrain_grammar
            .map(|grammar| format!("{grammar:?}")),
        desired_l0_height_mode: format!("{:?}", telemetry.desired_l0_height_mode),
        active_l0_height_mode: telemetry
            .active_l0_height_mode
            .map(|mode| format!("{mode:?}")),
        resident_l0_height_mode: telemetry
            .resident_l0_height_mode
            .map(|mode| format!("{mode:?}")),
        l0_probe_spacing_metres: telemetry.l0_probe_spacing_metres,
        budget_l0_height_queries: telemetry.budget_l0_height_queries,
        interaction_radius_metres: telemetry.interaction_radius_metres,
        confirmed_near_extent_metres: telemetry.confirmed_near_extent_metres,
        near_coverage_ready_columns: telemetry.near_coverage_ready_columns,
        near_coverage_hidden_cells: telemetry.near_coverage_hidden_cells,
        near_coverage_transition_pending: telemetry.near_coverage_transition_pending,
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
        resident_water_indices: telemetry.resident_water_indices,
        resident_lava_indices: telemetry.resident_lava_indices,
        water_ring_indices: telemetry.water_ring_indices,
        lava_ring_indices: telemetry.lava_ring_indices,
        resident_fluid_mesh_bytes: telemetry.resident_fluid_mesh_bytes,
        resident_semantic_cohort_entities: telemetry.resident_semantic_cohort_entities,
        resident_semantic_cohort_vertices: telemetry.resident_semantic_cohort_vertices,
        resident_semantic_cohort_indices: telemetry.resident_semantic_cohort_indices,
        resident_semantic_cohort_mesh_bytes: telemetry.resident_semantic_cohort_mesh_bytes,
        resident_semantic_cohort_count: telemetry.resident_semantic_cohort_count,
        resident_semantic_cohort_kind_counts: telemetry.resident_semantic_cohort_kind_counts,
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
        scheduler_resident_water_indices: telemetry.scheduler_resident_water_indices,
        scheduler_resident_lava_indices: telemetry.scheduler_resident_lava_indices,
        scheduler_water_ring_indices: telemetry.scheduler_water_ring_indices,
        scheduler_lava_ring_indices: telemetry.scheduler_lava_ring_indices,
        scheduler_resident_fluid_mesh_bytes: telemetry.scheduler_resident_fluid_mesh_bytes,
        scheduler_resident_semantic_cohort_entities: telemetry
            .scheduler_resident_semantic_cohort_entities,
        scheduler_resident_semantic_cohort_vertices: telemetry
            .scheduler_resident_semantic_cohort_vertices,
        scheduler_resident_semantic_cohort_indices: telemetry
            .scheduler_resident_semantic_cohort_indices,
        scheduler_resident_semantic_cohort_mesh_bytes: telemetry
            .scheduler_resident_semantic_cohort_mesh_bytes,
        scheduler_resident_semantic_cohort_count: telemetry
            .scheduler_resident_semantic_cohort_count,
        scheduler_resident_semantic_cohort_kind_counts: telemetry
            .scheduler_resident_semantic_cohort_kind_counts,
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
        resident_fluid_kind_integrity_valid: telemetry.resident_fluid_kind_integrity_valid,
        resident_fluid_observation_rejections: telemetry.resident_fluid_observation_rejections,
        resident_semantic_cohort_observation_valid: telemetry
            .resident_semantic_cohort_observation_valid,
        resident_semantic_cohort_entity_count_overflow: telemetry
            .resident_semantic_cohort_entity_count_overflow,
        resident_semantic_cohort_scheduler_mismatch: telemetry
            .resident_semantic_cohort_scheduler_mismatch,
        resident_semantic_cohort_budget_exceeded: telemetry
            .resident_semantic_cohort_budget_exceeded,
        resident_semantic_cohort_payload_integrity_valid: telemetry
            .resident_semantic_cohort_payload_integrity_valid,
        resident_semantic_cohort_observation_rejections: telemetry
            .resident_semantic_cohort_observation_rejections,
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
        budget_hydro_atomic_ring_build_bytes: telemetry.budget_hydro_atomic_ring_build_bytes,
        budget_atomic_ring_build_bytes: telemetry.budget_atomic_ring_build_bytes,
        budget_semantic_cohort_entities: telemetry.budget_semantic_cohort_entities,
        budget_semantic_cohort_vertices: telemetry.budget_semantic_cohort_vertices,
        budget_semantic_cohort_indices: telemetry.budget_semantic_cohort_indices,
        budget_semantic_cohort_mesh_bytes: telemetry.budget_semantic_cohort_mesh_bytes,
        budget_semantic_cohort_hash_scans: telemetry.budget_semantic_cohort_hash_scans,
        budget_semantic_cohort_height_queries: telemetry.budget_semantic_cohort_height_queries,
        budget_semantic_cohort_biome_queries: telemetry.budget_semantic_cohort_biome_queries,
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
        semantic_cohort_mode: format!("{:?}", telemetry.semantic_cohort_mode),
        scheduler_deferred_frames: telemetry.scheduler_deferred_frames,
        completed_rebuilds: telemetry.completed_rebuilds,
        stale_builds_discarded: telemetry.stale_builds_discarded,
        budget_rejections: telemetry.budget_rejections,
        last_build_ms: telemetry.last_build_ms,
        max_build_ms: telemetry.max_build_ms,
        last_height_queries: telemetry.last_height_queries,
        last_l0_center_queries: telemetry.last_l0_center_queries,
        last_l0_half_x_queries: telemetry.last_l0_half_x_queries,
        last_l0_half_z_queries: telemetry.last_l0_half_z_queries,
        last_l0_trimmed_vertices: telemetry.last_l0_trimmed_vertices,
        last_l0_trimmed_up_vertices: telemetry.last_l0_trimmed_up_vertices,
        last_l0_trimmed_down_vertices: telemetry.last_l0_trimmed_down_vertices,
        last_l0_max_abs_adjustment_metres: telemetry.last_l0_max_abs_adjustment_metres,
        last_l0_cache_update: format!("{:?}", telemetry.last_l0_cache_update),
        last_l0_cache_shift_x_cells: telemetry.last_l0_cache_shift_x_cells,
        last_l0_cache_shift_z_cells: telemetry.last_l0_cache_shift_z_cells,
        last_l0_reused_height_samples: telemetry.last_l0_reused_height_samples,
        last_material_slope_queries: telemetry.last_material_slope_queries,
        last_bridge_v2_cell_reuses: telemetry.last_bridge_v2_cell_reuses,
        last_fluid_classification_queries: telemetry.last_fluid_classification_queries,
        last_fluid_biome_queries: telemetry.last_fluid_biome_queries,
        last_fluid_vertices: telemetry.last_fluid_vertices,
        last_fluid_indices: telemetry.last_fluid_indices,
        last_water_indices: telemetry.last_water_indices,
        last_lava_indices: telemetry.last_lava_indices,
        last_semantic_cohort_hash_scans: telemetry.last_semantic_cohort_hash_scans,
        last_semantic_cohort_height_queries: telemetry.last_semantic_cohort_height_queries,
        last_semantic_cohort_biome_queries: telemetry.last_semantic_cohort_biome_queries,
        last_semantic_cohort_candidates: telemetry.last_semantic_cohort_candidates,
        last_semantic_cohort_emitted: telemetry.last_semantic_cohort_emitted,
        last_semantic_cohort_vertices: telemetry.last_semantic_cohort_vertices,
        last_semantic_cohort_indices: telemetry.last_semantic_cohort_indices,
        last_semantic_cohort_kind_counts: telemetry.last_semantic_cohort_kind_counts,
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
        let requested_focus_value = std::env::var("VOXEL_NATIVE_QA_FOCUS").ok();
        let parsed_focus = QaFocus::parse_env_value(requested_focus_value.as_deref());
        let focus = parsed_focus.unwrap_or(QaFocus::Scenic);
        let requested_focus_label = parsed_focus.map_or_else(
            || {
                requested_focus_value
                    .as_deref()
                    .and_then(|value| qa_bounded_text(&value.to_ascii_lowercase(), 32))
                    .unwrap_or_else(|| "invalid".to_owned())
            },
            |parsed| parsed.label().to_owned(),
        );
        let focus_evidence_ready = parsed_focus.is_some() && !focus.requires_hydro_anchor();
        let focus_unavailable_reason = parsed_focus.is_none().then(|| "invalid-request".to_owned());
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
            generator_handoff_elapsed: 0.0,
            lifecycle_elapsed: 0.0,
            write_tail_elapsed: 0.0,
            route_ready: false,
            duration,
            screenshot_interval,
            next_screenshot_at: 2.5,
            screenshot_index: 0,
            finish_wait_frames: 0,
            settled_wait_frames: 0,
            settled_wait_elapsed: 0.0,
            origin: Vec3::ZERO,
            origin_set: false,
            requested_focus_label,
            focus,
            resolved_focus: focus,
            focus_anchor: None,
            focus_evidence_ready,
            focus_unavailable_reason,
            focus_search_visited_candidates: None,
            focus_classification_queries: None,
            focus_search_cap_exhausted: false,
            focus_search_candidate_cap: 0,
            focus_classification_query_cap: 0,
            generator_signature: None,
            camera_route_policy: QaCameraRoutePolicy::from_env(),
            camera_route_preflight_pending: false,
            camera_route_preflight_elapsed: 0.0,
            camera_route_plan: None,
            camera_route_available: false,
            camera_route_unavailable_reason: None,
            camera_route_validation: QaCameraRouteValidation::default(),
            streaming_distance_m,
            current_phase: QaRoutePhase::Establishing,
            route_frame_times: QaFrameTimeAccumulator::default(),
            peak_loaded_chunks: 0,
            peak_dense_chunks: 0,
            dense_chunk_budget_exceeded: false,
            peak_mesh_entities: 0,
            peak_pending_terrain: 0,
            peak_pending_meshes: 0,
            peak_dirty_chunks: 0,
            max_horizontal_displacement_m: 0.0,
            stalls: Vec::new(),
            screenshots: if enabled {
                Vec::with_capacity(QA_SCREENSHOT_OBSERVATION_CAP)
            } else {
                Vec::new()
            },
            screenshot_observations: if enabled {
                Vec::with_capacity(QA_SCREENSHOT_OBSERVATION_CAP)
            } else {
                Vec::new()
            },
            screenshot_observation_rejections: 0,
            screenshot_observation_cap_exhausted: false,
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
    mut pending_edits: ResMut<PendingEditedOverrideStore>,
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
    let scenery_quality = std::env::var("VOXEL_NATIVE_QA_SCENERY")
        .ok()
        .as_deref()
        .and_then(parse_scenery_quality)
        .unwrap_or(SceneryQuality::Lush);
    let terrain_grammar = match std::env::var(QA_TERRAIN_GRAMMAR_ENV) {
        Ok(value) => parse_terrain_grammar(&value).unwrap_or_else(|| {
            panic!(
                "QA: unsupported {QA_TERRAIN_GRAMMAR_ENV} value {value:?}; expected v1, v2, or v3"
            )
        }),
        Err(std::env::VarError::NotPresent) => TerrainGrammarVersion::CURRENT,
        Err(error) => panic!("QA: could not read {QA_TERRAIN_GRAMMAR_ENV}: {error}"),
    };
    let identity = WorldGenerationIdentity {
        seed,
        world_profile,
        scenery_quality,
        terrain_grammar,
    };
    let world_name =
        std::env::var("VOXEL_NATIVE_QA_WORLD").unwrap_or_else(|_| "qa_autopilot".into());
    let mut meta = WorldMeta::new_with_identity(world_name, identity);
    let generator = TerrainGenerator::from_identity(identity);
    qa.generator_signature = Some(qa_generator_signature(&generator));
    if qa.focus.requires_hydro_anchor() {
        let result = qa_find_hydro_focus(&generator, qa.focus, world_profile);
        qa.focus_anchor = result.anchor;
        qa.focus_evidence_ready = result.anchor.is_some();
        qa.focus_search_candidate_cap = result.candidate_cap;
        qa.focus_classification_query_cap = result.classification_query_cap;
        // River's existing terrain API exposes only its fixed cap, not actual
        // early-stop work. Preserve that as unknown instead of serializing a
        // fabricated zero. Lava has an exact local counter.
        if qa_focus_has_exact_search_work(qa.focus, world_profile) {
            qa.focus_search_visited_candidates = Some(result.visited_candidates);
            qa.focus_classification_queries = Some(result.classification_queries);
            qa.focus_search_cap_exhausted = qa_focus_search_exhausted(result);
        }
        if let Some(anchor) = result.anchor {
            meta.player_pos = [
                anchor.world_x as f32 + 0.5,
                anchor.fluid_y as f32 + 34.0,
                anchor.world_z as f32 + 0.5,
            ];
            meta.player_yaw = 0.0;
            meta.player_pitch = -0.28;
            info!(
                "QA: {} focus at {}, {}, {} after {} candidates / {} classifications",
                qa.focus.label(),
                anchor.world_x,
                anchor.fluid_y,
                anchor.world_z,
                result.visited_candidates,
                result.classification_queries,
            );
        } else {
            if qa.focus_unavailable_reason.is_none() {
                qa.focus_unavailable_reason = Some(
                    if qa.focus == QaFocus::River && world_profile != WorldProfile::Natural
                        || qa.focus == QaFocus::Lava
                            && world_profile != WorldProfile::AstralFrontier
                    {
                        "unsupported-profile".to_owned()
                    } else if qa.focus_search_cap_exhausted {
                        "search-cap-exhausted".to_owned()
                    } else {
                        "anchor-not-found".to_owned()
                    },
                );
            }
            qa.resolved_focus = QaFocus::Scenic;
            warn!(
                "QA: requested {} focus unavailable after {} candidates / {} classifications; evidence will fail closed",
                qa.focus.label(),
                result.visited_candidates,
                result.classification_queries,
            );
        }
    } else if qa.focus == QaFocus::Waypoint && world_profile == WorldProfile::AstralFrontier {
        if let Some(focus) = generator.find_astral_waypoint_near(0, 0, 16) {
            qa.focus_anchor = Some(QaFocusAnchor {
                world_x: focus.x,
                fluid_y: focus.y,
                world_z: focus.z,
            });
            qa.focus_evidence_ready = true;
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
        } else {
            qa.focus_evidence_ready = false;
            qa.focus_unavailable_reason = Some("anchor-not-found".to_owned());
            qa.resolved_focus = QaFocus::Scenic;
        }
    } else if qa.focus == QaFocus::Waypoint {
        qa.focus_evidence_ready = false;
        qa.focus_unavailable_reason = Some("unsupported-profile".to_owned());
        qa.resolved_focus = QaFocus::Scenic;
    }
    meta.time_mode = TimeMode::Fixed;
    meta.time_of_day = env_f32("VOXEL_NATIVE_QA_HOUR")
        .unwrap_or(10.8)
        .clamp(0.0, 24.0);
    let meta = crate::world::prepare_programmatic_world_entry(&meta, false, &mut pending_edits)
        .unwrap_or_else(|reason| panic!("QA: world authority initialization failed: {reason}"));
    settings.seed = seed;
    settings.world_profile = world_profile;
    settings.scenery_quality = scenery_quality;
    settings.terrain_grammar = terrain_grammar;
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

/// Hydro acceptance route. River/lava use the same framing so ON/OFF pairs
/// differ only in world evidence. Near-far deliberately crosses the confirmed
/// near extent while looking back at the anchored fluid quad/channel.
fn qa_hydro_route_sample(
    progress: f32,
    focus: QaFocus,
    confirmed_near_extent_metres: i64,
    visible_radius: f32,
) -> QaRouteSample {
    qa_hydro_route_sample_for_variant(
        progress,
        focus,
        confirmed_near_extent_metres,
        visible_radius,
        Vec2::new(0.82, 0.57).normalize(),
        0,
    )
}

fn qa_hydro_route_sample_for_variant(
    progress: f32,
    focus: QaFocus,
    confirmed_near_extent_metres: i64,
    visible_radius: f32,
    base_axis: Vec2,
    variant_index: u8,
) -> QaRouteSample {
    let seam = (confirmed_near_extent_metres.max(32) as f32).clamp(32.0, 256.0);
    let (axis, right) = qa_camera_route_variant_basis(base_axis, variant_index);
    let frames = if focus == QaFocus::NearFar {
        [
            (
                0.00,
                qa_keyframe(axis * (seam - 24.0), 22.0, Vec2::ZERO, 0.0, 14.0),
            ),
            (
                0.20,
                qa_keyframe(axis * (seam - 8.0), 18.0, Vec2::ZERO, 0.0, 12.0),
            ),
            (0.44, qa_keyframe(axis * seam, 16.0, Vec2::ZERO, 0.0, 11.0)),
            (
                0.72,
                qa_keyframe(axis * (seam + 12.0), 21.0, Vec2::ZERO, 0.0, 13.0),
            ),
            (
                1.00,
                qa_keyframe(
                    axis * (seam + 72.0) + right * 30.0,
                    46.0,
                    Vec2::ZERO,
                    0.0,
                    24.0,
                ),
            ),
        ]
    } else {
        [
            (
                0.00,
                qa_keyframe(axis * 82.0 + right * 24.0, 38.0, Vec2::ZERO, 0.0, 24.0),
            ),
            (
                0.20,
                qa_keyframe(axis * 55.0 + right * 18.0, 31.0, Vec2::ZERO, 0.0, 19.0),
            ),
            (
                0.44,
                qa_keyframe(axis * 30.0 + right * 8.0, 18.0, Vec2::ZERO, 0.0, 12.0),
            ),
            (
                0.72,
                qa_keyframe(-axis * 34.0 + right * 18.0, 25.0, Vec2::ZERO, 0.0, 15.0),
            ),
            (
                1.00,
                qa_keyframe(-axis * 88.0 - right * 24.0, 38.0, Vec2::ZERO, 0.0, 24.0),
            ),
        ]
    };
    qa_constrain_to_visible_radius(qa_sample_keyframes(progress, &frames), visible_radius)
}

fn qa_camera_preflight_staging_sample() -> QaRouteSample {
    QaRouteSample {
        camera_offset: Vec2::ZERO,
        camera_height: 72.0,
        target_offset: Vec2::ZERO,
        target_height: 0.0,
        terrain_clearance: 64.0,
        phase: QaRoutePhase::Establishing,
    }
}

fn qa_camera_route_variant_basis(base_axis: Vec2, variant_index: u8) -> (Vec2, Vec2) {
    let base_axis = if base_axis.is_finite() && base_axis.length_squared() > 0.25 {
        base_axis.normalize()
    } else {
        Vec2::new(0.82, 0.57).normalize()
    };
    let quarter_turns = variant_index % 4;
    let axis = match quarter_turns {
        0 => base_axis,
        1 => Vec2::new(-base_axis.y, base_axis.x),
        2 => -base_axis,
        _ => Vec2::new(base_axis.y, -base_axis.x),
    };
    let handedness = if variant_index < 4 { 1.0 } else { -1.0 };
    (axis, Vec2::new(-axis.y, axis.x) * handedness)
}

fn qa_camera_route_base_axis(
    generator: &TerrainGenerator,
    seed: u32,
    focus: QaFocus,
    anchor: QaFocusAnchor,
) -> Vec2 {
    if focus == QaFocus::River
        || focus == QaFocus::NearFar && generator.world_profile() == WorldProfile::Natural
    {
        let flow = generator
            .environment_sample_at(anchor.world_x, anchor.world_z)
            .flow_direction;
        let axis = Vec2::new(flow[0], flow[1]);
        if axis.is_finite() && axis.length_squared() > 0.25 {
            return axis.normalize();
        }
    }
    qa_camera_route_hashed_axis(seed, anchor)
}

fn qa_camera_route_hashed_axis(seed: u32, anchor: QaFocusAnchor) -> Vec2 {
    let mixed = seed
        ^ (anchor.world_x as u32)
            .wrapping_mul(0x9E37_79B9)
            .rotate_left(11)
        ^ (anchor.world_z as u32)
            .wrapping_mul(0x85EB_CA6B)
            .rotate_right(7)
        ^ (anchor.fluid_y as u32).wrapping_mul(0xC2B2_AE35);
    let angle = (mixed as f64 / u32::MAX as f64 * std::f64::consts::TAU) as f32;
    Vec2::new(angle.cos(), angle.sin()).normalize_or(Vec2::X)
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

const QA_CAMERA_ROUTE_PROGRESS_SAMPLES: [f32; QA_CAMERA_ROUTE_VALIDATION_SAMPLES] = [
    0.0, 0.10, 0.20, 0.30, 0.40, 0.44, 0.51, 0.58, 0.65, 0.72, 0.79, 0.84, 0.89, 0.94, 0.97, 1.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QaCameraProbeResult {
    Clear { clearance_voxels: u16 },
    ChunksUnloaded,
    BodyOccluded,
    LineOfSightOccluded,
    WorkCap,
    CoordinateRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QaCameraVoxelResolution {
    Resident(Voxel),
    ProvenAir,
    Unavailable,
}

trait QaCameraVoxelSource {
    fn resolve_voxel(&self, world_x: i32, world_y: i32, world_z: i32) -> QaCameraVoxelResolution;
}

struct QaWorldVoxelSource<'a> {
    world: &'a VoxelWorld,
    streamer: &'a ChunkStreamer,
}

impl QaCameraVoxelSource for QaWorldVoxelSource<'_> {
    fn resolve_voxel(&self, world_x: i32, world_y: i32, world_z: i32) -> QaCameraVoxelResolution {
        let (chunk_pos, _, _, _) = crate::chunk::world_to_chunk(world_x, world_y, world_z);
        if !self.streamer.requested_chunks.contains(&chunk_pos) {
            return QaCameraVoxelResolution::Unavailable;
        }
        if self.world.is_voxel_chunk_loaded(world_x, world_y, world_z) {
            return QaCameraVoxelResolution::Resident(
                self.world.voxel_at(world_x, world_y, world_z),
            );
        }
        match self.world.voxel_at_if_resolved(world_x, world_y, world_z) {
            Some(voxel) if voxel == AIR => QaCameraVoxelResolution::ProvenAir,
            _ => QaCameraVoxelResolution::Unavailable,
        }
    }
}

impl QaCameraVoxelSource for VoxelWorld {
    fn resolve_voxel(&self, world_x: i32, world_y: i32, world_z: i32) -> QaCameraVoxelResolution {
        if self.is_voxel_chunk_loaded(world_x, world_y, world_z) {
            return QaCameraVoxelResolution::Resident(self.voxel_at(world_x, world_y, world_z));
        }
        match self.voxel_at_if_resolved(world_x, world_y, world_z) {
            Some(voxel) if voxel == AIR => QaCameraVoxelResolution::ProvenAir,
            _ => QaCameraVoxelResolution::Unavailable,
        }
    }
}

#[derive(Debug)]
struct QaCameraQueryBudget {
    voxel_queries: usize,
    voxel_query_cap: usize,
    required_chunk_checks: usize,
    loaded_chunk_checks: usize,
    proven_air_chunk_checks: usize,
    unloaded_chunk_checks: usize,
    body_occlusions: usize,
    los_occlusions: usize,
}

impl QaCameraQueryBudget {
    fn new() -> Self {
        Self {
            voxel_queries: 0,
            voxel_query_cap: QA_CAMERA_ROUTE_VOXEL_QUERY_CAP,
            required_chunk_checks: 0,
            loaded_chunk_checks: 0,
            proven_air_chunk_checks: 0,
            unloaded_chunk_checks: 0,
            body_occlusions: 0,
            los_occlusions: 0,
        }
    }

    fn query(
        &mut self,
        source: &impl QaCameraVoxelSource,
        world_x: i32,
        world_y: i32,
        world_z: i32,
    ) -> Result<Voxel, QaCameraProbeResult> {
        self.voxel_queries = self
            .voxel_queries
            .checked_add(1)
            .filter(|count| *count <= self.voxel_query_cap)
            .ok_or(QaCameraProbeResult::WorkCap)?;
        self.required_chunk_checks = self.required_chunk_checks.saturating_add(1);
        match source.resolve_voxel(world_x, world_y, world_z) {
            QaCameraVoxelResolution::Resident(voxel) => {
                self.loaded_chunk_checks = self.loaded_chunk_checks.saturating_add(1);
                Ok(voxel)
            }
            QaCameraVoxelResolution::ProvenAir => {
                self.proven_air_chunk_checks = self.proven_air_chunk_checks.saturating_add(1);
                Ok(AIR)
            }
            QaCameraVoxelResolution::Unavailable => {
                self.unloaded_chunk_checks = self.unloaded_chunk_checks.saturating_add(1);
                Err(QaCameraProbeResult::ChunksUnloaded)
            }
        }
    }
}

fn qa_camera_pose_voxel(position: Vec3) -> Option<[i32; 3]> {
    if !position.is_finite()
        || f64::from(position.x).abs() > QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT
        || f64::from(position.y).abs() > QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT
        || f64::from(position.z).abs() > QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT
    {
        return None;
    }
    Some([
        crate::chunk::floor_to_i32_safe(position.x),
        crate::chunk::floor_to_i32_safe(position.y),
        crate::chunk::floor_to_i32_safe(position.z),
    ])
}

fn qa_camera_voxel_is_foliage(voxel: Voxel) -> bool {
    matches!(
        BlockType::from_voxel(voxel),
        BlockType::Leaves
            | BlockType::JungleLeaves
            | BlockType::BlossomLeaves
            | BlockType::SakuraPetals
    )
}

fn qa_camera_voxel_is_visual_body_blocker(voxel: Voxel) -> bool {
    // A camera submerged in Water or Lava is not an acceptable inspection
    // pose. Target fluids remain visible because the route target is authored
    // above their surface and the ray endpoint is not treated specially.
    voxel != AIR
}

fn qa_camera_probe_body(
    source: &impl QaCameraVoxelSource,
    position: Vec3,
    budget: &mut QaCameraQueryBudget,
) -> QaCameraProbeResult {
    let Some([base_x, base_y, base_z]) = qa_camera_pose_voxel(position) else {
        return QaCameraProbeResult::CoordinateRange;
    };
    let mut queries = 0usize;
    for dy in -1i32..=2 {
        for dz in -1i32..=1 {
            for dx in -1i32..=1 {
                queries += 1;
                debug_assert!(queries <= QA_CAMERA_BODY_QUERY_CAP_PER_POSE);
                let Some(world_x) = base_x.checked_add(dx) else {
                    return QaCameraProbeResult::CoordinateRange;
                };
                let Some(world_y) = base_y.checked_add(dy) else {
                    return QaCameraProbeResult::CoordinateRange;
                };
                let Some(world_z) = base_z.checked_add(dz) else {
                    return QaCameraProbeResult::CoordinateRange;
                };
                let voxel = match budget.query(source, world_x, world_y, world_z) {
                    Ok(voxel) => voxel,
                    Err(reason) => return reason,
                };
                if qa_camera_voxel_is_visual_body_blocker(voxel) {
                    budget.body_occlusions = budget.body_occlusions.saturating_add(1);
                    return QaCameraProbeResult::BodyOccluded;
                }
            }
        }
    }
    QaCameraProbeResult::Clear {
        // The fixed 3x4x3 stencil proves one complete voxel shell around the
        // camera. Wider clearances are intentionally not inferred without
        // spending additional queries.
        clearance_voxels: 1,
    }
}

fn qa_camera_probe_ray(
    source: &impl QaCameraVoxelSource,
    start: Vec3,
    end: Vec3,
    expected_target_fluid: BlockType,
    budget: &mut QaCameraQueryBudget,
) -> QaCameraProbeResult {
    let Some([start_x, start_y, start_z]) = qa_camera_pose_voxel(start) else {
        return QaCameraProbeResult::CoordinateRange;
    };
    let Some([end_x, end_y, end_z]) = qa_camera_pose_voxel(end) else {
        return QaCameraProbeResult::CoordinateRange;
    };
    let delta = end - start;
    let length = delta.length();
    if !length.is_finite() || length <= f32::EPSILON {
        return QaCameraProbeResult::CoordinateRange;
    }

    let mut cell = [start_x, start_y, start_z];
    let end_cell = [end_x, end_y, end_z];
    let step = [
        if delta.x > 0.0 {
            1
        } else if delta.x < 0.0 {
            -1
        } else {
            0
        },
        if delta.y > 0.0 {
            1
        } else if delta.y < 0.0 {
            -1
        } else {
            0
        },
        if delta.z > 0.0 {
            1
        } else if delta.z < 0.0 {
            -1
        } else {
            0
        },
    ];
    let components = [delta.x, delta.y, delta.z];
    let starts = [start.x, start.y, start.z];
    let mut t_delta = [f32::INFINITY; 3];
    let mut t_max = [f32::INFINITY; 3];
    for axis in 0..3 {
        if step[axis] == 0 {
            continue;
        }
        t_delta[axis] = components[axis].abs().recip();
        let boundary = if step[axis] > 0 {
            cell[axis] as f32 + 1.0
        } else {
            cell[axis] as f32
        };
        t_max[axis] = (boundary - starts[axis]) / components[axis];
    }

    let mut visited = 0usize;
    let mut foliage_hits = 0usize;
    while cell != end_cell {
        let next_t = t_max[0].min(t_max[1]).min(t_max[2]);
        if !next_t.is_finite() || next_t > 1.0 + f32::EPSILON * 8.0 {
            return QaCameraProbeResult::CoordinateRange;
        }
        let equality_epsilon = f32::EPSILON * 16.0 * next_t.abs().max(1.0);
        let mut crossing_axes = [usize::MAX; 3];
        let mut crossing_count = 0usize;
        for axis in 0..3 {
            if (t_max[axis] - next_t).abs() <= equality_epsilon {
                crossing_axes[crossing_count] = axis;
                crossing_count += 1;
            }
        }
        if crossing_count == 0 {
            return QaCameraProbeResult::CoordinateRange;
        }

        // When the segment crosses an edge or corner, every non-empty subset
        // of the simultaneous axis steps touches a voxel. Visiting all of them
        // is the conservative 3D supercover; a one-metre point sampler can
        // silently skip precisely these cells.
        for subset in 1usize..(1usize << crossing_count) {
            let mut candidate = cell;
            for (bit, axis) in crossing_axes[..crossing_count].iter().enumerate() {
                if subset & (1usize << bit) != 0 {
                    let Some(value) = candidate[*axis].checked_add(step[*axis]) else {
                        return QaCameraProbeResult::CoordinateRange;
                    };
                    candidate[*axis] = value;
                }
            }
            visited = visited.saturating_add(1);
            if visited > QA_CAMERA_LOS_QUERY_CAP_PER_RAY {
                return QaCameraProbeResult::WorkCap;
            }
            let voxel = match budget.query(source, candidate[0], candidate[1], candidate[2]) {
                Ok(voxel) => voxel,
                Err(reason) => return reason,
            };
            // The authored target point is the one intentional semantic
            // exemption: a river/lava route may look exactly at its expected
            // fluid surface. Its exact 3D chunk still must be resident. A
            // different fluid or any solid endpoint remains an occluder, as
            // do all non-air intermediate cells.
            if candidate == end_cell {
                if voxel == AIR || voxel == Voxel::from(expected_target_fluid) {
                    continue;
                }
                budget.los_occlusions = budget.los_occlusions.saturating_add(1);
                return QaCameraProbeResult::LineOfSightOccluded;
            }
            if voxel == AIR {
                continue;
            }
            if qa_camera_voxel_is_foliage(voxel) {
                foliage_hits += 1;
                if foliage_hits <= QA_CAMERA_FOLIAGE_HIT_CAP_PER_RAY {
                    continue;
                }
            }
            budget.los_occlusions = budget.los_occlusions.saturating_add(1);
            return QaCameraProbeResult::LineOfSightOccluded;
        }

        for axis in crossing_axes[..crossing_count].iter().copied() {
            let Some(value) = cell[axis].checked_add(step[axis]) else {
                return QaCameraProbeResult::CoordinateRange;
            };
            cell[axis] = value;
            t_max[axis] += t_delta[axis];
        }
    }
    QaCameraProbeResult::Clear {
        clearance_voxels: u16::MAX,
    }
}

fn qa_camera_probe_pose(
    source: &impl QaCameraVoxelSource,
    position: Vec3,
    target: Vec3,
    expected_target_fluid: BlockType,
    budget: &mut QaCameraQueryBudget,
) -> QaCameraProbeResult {
    let body_clearance = match qa_camera_probe_body(source, position, budget) {
        QaCameraProbeResult::Clear { clearance_voxels } => clearance_voxels,
        reason => return reason,
    };
    let forward = (target - position).normalize_or_zero();
    if forward.length_squared() <= f32::EPSILON {
        return QaCameraProbeResult::CoordinateRange;
    }
    let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
    for offset in [Vec3::ZERO, right * 0.9, right * -0.9] {
        match qa_camera_probe_ray(
            source,
            position + offset,
            target + offset * 0.35,
            expected_target_fluid,
            budget,
        ) {
            QaCameraProbeResult::Clear { .. } => {}
            reason => return reason,
        }
    }
    QaCameraProbeResult::Clear {
        clearance_voxels: body_clearance,
    }
}

fn qa_camera_plan_hash(
    seed: u32,
    profile: WorldProfile,
    scenery: SceneryQuality,
    focus: QaFocus,
    anchor: QaFocusAnchor,
    variant_index: u8,
) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for value in [
        QA_CAMERA_ROUTE_ALGORITHM_VERSION,
        u64::from(seed),
        profile as u64,
        scenery as u64,
        focus as u64,
        anchor.world_x as u32 as u64,
        anchor.fluid_y as u32 as u64,
        anchor.world_z as u32 as u64,
        u64::from(variant_index),
    ] {
        hash ^= value;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn qa_camera_route_preflight(
    source: &impl QaCameraVoxelSource,
    world: &VoxelWorld,
    origin: Vec3,
    focus: QaFocus,
    anchor: QaFocusAnchor,
    seed: u32,
    profile: WorldProfile,
    scenery: SceneryQuality,
    confirmed_near_extent_metres: i64,
    visible_radius: f32,
) -> (
    Option<QaCameraRoutePlan>,
    Option<QaCameraRouteUnavailableReason>,
    QaCameraRouteValidation,
) {
    let expected_target_fluid = match (focus, profile) {
        (QaFocus::River, WorldProfile::Natural) | (QaFocus::NearFar, WorldProfile::Natural) => {
            BlockType::Water
        }
        (QaFocus::Lava, WorldProfile::AstralFrontier)
        | (QaFocus::NearFar, WorldProfile::AstralFrontier) => BlockType::Lava,
        _ => {
            return (
                None,
                Some(QaCameraRouteUnavailableReason::FocusUnavailable),
                QaCameraRouteValidation::default(),
            );
        }
    };
    let base_axis = qa_camera_route_base_axis(&world.generator, seed, focus, anchor);
    let mut budget = QaCameraQueryBudget::new();
    let mut best: Option<(u16, u8)> = None;
    let mut observed_unloaded = false;
    let mut observed_body_occlusion = false;
    let mut observed_los_occlusion = false;
    let mut terminal_reason = None;

    for variant in 0..QA_CAMERA_ROUTE_VARIANTS as u8 {
        let mut valid = true;
        let mut minimum_clearance = u16::MAX;
        for progress in QA_CAMERA_ROUTE_PROGRESS_SAMPLES {
            let sample = qa_hydro_route_sample_for_variant(
                progress,
                focus,
                confirmed_near_extent_metres,
                visible_radius,
                base_axis,
                variant,
            );
            let (position, target) = qa_world_pose(world, origin, sample);
            match qa_camera_probe_pose(source, position, target, expected_target_fluid, &mut budget)
            {
                QaCameraProbeResult::Clear { clearance_voxels } => {
                    minimum_clearance = minimum_clearance.min(clearance_voxels);
                }
                result => {
                    valid = false;
                    match result {
                        QaCameraProbeResult::ChunksUnloaded => observed_unloaded = true,
                        QaCameraProbeResult::BodyOccluded => observed_body_occlusion = true,
                        QaCameraProbeResult::LineOfSightOccluded => observed_los_occlusion = true,
                        QaCameraProbeResult::WorkCap => {
                            terminal_reason = Some(QaCameraRouteUnavailableReason::WorkCap)
                        }
                        QaCameraProbeResult::CoordinateRange => {
                            terminal_reason = Some(QaCameraRouteUnavailableReason::CoordinateRange)
                        }
                        QaCameraProbeResult::Clear { .. } => {}
                    }
                    if matches!(
                        result,
                        QaCameraProbeResult::WorkCap | QaCameraProbeResult::CoordinateRange
                    ) {
                        break;
                    }
                }
            }
        }
        if valid {
            let candidate = (minimum_clearance, variant);
            if best.map_or(true, |current| {
                candidate.0 > current.0 || candidate.0 == current.0 && candidate.1 < current.1
            }) {
                best = Some(candidate);
            }
        }
        if budget.voxel_queries >= budget.voxel_query_cap {
            terminal_reason = Some(QaCameraRouteUnavailableReason::WorkCap);
            break;
        }
    }

    let validation = QaCameraRouteValidation {
        variant_count: QA_CAMERA_ROUTE_VARIANTS,
        validation_samples: QA_CAMERA_ROUTE_VALIDATION_SAMPLES,
        selected_clear_samples: if best.is_some() {
            QA_CAMERA_ROUTE_VALIDATION_SAMPLES
        } else {
            0
        },
        voxel_queries: budget.voxel_queries,
        voxel_query_cap: budget.voxel_query_cap,
        required_chunk_checks: budget.required_chunk_checks,
        loaded_chunk_checks: budget.loaded_chunk_checks,
        proven_air_chunk_checks: budget.proven_air_chunk_checks,
        unloaded_chunk_checks: budget.unloaded_chunk_checks,
        candidate_body_occlusions: budget.body_occlusions,
        candidate_los_occlusions: budget.los_occlusions,
        minimum_clearance_voxels: best.map(|candidate| candidate.0),
        work_cap_exhausted: budget.voxel_queries >= budget.voxel_query_cap,
    };
    let plan = best.map(|(_, variant_index)| QaCameraRoutePlan {
        variant_index,
        plan_hash: qa_camera_plan_hash(seed, profile, scenery, focus, anchor, variant_index),
    });
    let reason = plan.is_none().then(|| {
        terminal_reason.unwrap_or(if observed_unloaded {
            QaCameraRouteUnavailableReason::ChunksUnloaded
        } else if observed_body_occlusion {
            QaCameraRouteUnavailableReason::BodyOccluded
        } else if observed_los_occlusion {
            QaCameraRouteUnavailableReason::LineOfSightOccluded
        } else {
            QaCameraRouteUnavailableReason::LineOfSightOccluded
        })
    });
    (plan, reason, validation)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QaGeneratorSignature {
    seed: u32,
    world_profile: WorldProfile,
    scenery_quality: SceneryQuality,
    terrain_grammar: TerrainGrammarVersion,
    samples: [(i32, Biome); 4],
}

fn qa_generator_signature(generator: &TerrainGenerator) -> QaGeneratorSignature {
    const PROBES: [(i32, i32); 4] = [(0, 0), (997, -613), (-1_559, 2_081), (4_093, 8_191)];
    QaGeneratorSignature {
        seed: generator.seed,
        world_profile: generator.world_profile(),
        scenery_quality: generator.scenery_quality(),
        terrain_grammar: generator.terrain_grammar(),
        samples: PROBES.map(|(x, z)| (generator.surface_height_at(x, z), generator.biome_at(x, z))),
    }
}

fn qa_profile_anchor_ready(
    requested_seed: u32,
    requested_profile: WorldProfile,
    requested_scenery: SceneryQuality,
    requested_terrain_grammar: TerrainGrammarVersion,
    expected_signature: Option<QaGeneratorSignature>,
    generated: &TerrainGenerator,
) -> bool {
    generated.seed == requested_seed
        && generated.world_profile() == requested_profile
        && generated.scenery_quality() == requested_scenery
        && generated.terrain_grammar() == requested_terrain_grammar
        && expected_signature.is_some_and(|expected| qa_generator_signature(generated) == expected)
}

fn qa_near_far_route_available(
    telemetry: &PlanetaryStreamingTelemetry,
    expected_profile: WorldProfile,
    visible_radius: f32,
) -> bool {
    if !visible_radius.is_finite()
        || !telemetry.enabled
        || telemetry.profile != expected_profile
        || telemetry.confirmed_near_extent_metres <= 0
        || telemetry.near_coverage_ready_columns == 0
        || telemetry.resident_entities == 0
        || telemetry.ring_vertices[0] == 0
        || telemetry.pending_rebuilds != 0
        || telemetry.dirty_mask != 0
        || telemetry.build_in_flight
        || !telemetry.resident_observation_valid
        || !telemetry.resident_fluid_observation_valid
        || !telemetry.resident_fluid_kind_integrity_valid
    {
        return false;
    }
    let expected_hydro_present = match expected_profile {
        WorldProfile::Natural => telemetry.resident_water_indices > 0,
        WorldProfile::AstralFrontier => telemetry.resident_lava_indices > 0,
    };
    if !expected_hydro_present {
        return false;
    }
    let seam = (telemetry.confirmed_near_extent_metres.max(32) as f32).clamp(32.0, 256.0);
    // The final keyframe must remain outside the seam without visibility
    // clamping, including its authored lateral displacement, while the first
    // two frames remain inside it.
    let final_radius = Vec2::new(seam + 72.0, 30.0).length();
    visible_radius + 0.001 >= final_radius
}

fn qa_drive_camera(
    virtual_time: Res<Time>,
    real_time: Res<Time<Real>>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    planetary_telemetry: Res<PlanetaryStreamingTelemetry>,
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

    // Evidence uses the unclamped monotonic wall-clock delta. Bevy's generic
    // `Time` mirrors Time<Virtual>, whose default 250 ms clamp would otherwise
    // under-report severe route hitches and inflate measured FPS.
    let raw_dt = real_time.delta_seconds();
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
    // Camera/state motion deliberately follows Bevy virtual time. It remains
    // bounded even if the process resumes after a long suspension, while the
    // independent real value above remains the unmodified evidence input.
    let virtual_dt = virtual_time.delta_seconds();
    let dt = if virtual_dt.is_finite() && virtual_dt > 0.0 {
        virtual_dt.min(1.0)
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
    if !qa_profile_anchor_ready(
        settings.seed,
        requested_profile,
        settings.scenery_quality,
        settings.terrain_grammar,
        qa.generator_signature,
        &world.generator,
    ) {
        // State entry and world-generator replacement happen in separate
        // schedules. Do not permanently capture the previous profile's spawn
        // as our cinematic anchor during that one-frame handoff.
        player.velocity = Vec3::ZERO;
        player.flying = true;
        player.placed_on_surface = true;
        qa.generator_handoff_elapsed =
            (qa.generator_handoff_elapsed + dt).min(QA_GENERATOR_HANDOFF_TIMEOUT_SECONDS);
        qa.warmup_elapsed += dt;
        if qa.generator_handoff_elapsed >= QA_GENERATOR_HANDOFF_TIMEOUT_SECONDS {
            qa.focus_evidence_ready = false;
            qa.focus_unavailable_reason = Some("generator-not-ready".to_owned());
            qa.resolved_focus = QaFocus::Scenic;
            qa.route_ready = true;
            qa.elapsed = qa.duration;
            warn!(
                "QA: generator handoff exceeded {:.1}s; writing a blocked report",
                QA_GENERATOR_HANDOFF_TIMEOUT_SECONDS
            );
        }
        return;
    }

    if !qa.origin_set {
        qa.origin = if let Some(anchor) = qa.focus_anchor {
            anchor.render_origin()
        } else if requested_profile == WorldProfile::AstralFrontier && qa.focus != QaFocus::Waypoint
        {
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
    // Route selection follows the explicitly resolved route, not merely the
    // request. This keeps the camera path consistent with report provenance
    // when an invalid/unsupported request falls back to scenic inspection.
    let waypoint_route = astral_route && qa.resolved_focus == QaFocus::Waypoint;
    let streaming_route = qa.resolved_focus == QaFocus::Streaming;
    let hydro_route = qa.resolved_focus.requires_hydro_anchor()
        && qa.focus_anchor.is_some()
        && qa.focus_evidence_ready;
    let route_focus = qa.resolved_focus;
    let confirmed_near_extent_metres = planetary_telemetry.confirmed_near_extent_metres;
    let cinematic_route = astral_route || streaming_route || hydro_route;
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
    let frozen_camera_route_plan = qa.camera_route_plan;
    let frozen_focus_anchor = qa.focus_anchor;

    let route_sample = |progress: f32| {
        if streaming_route {
            qa_streaming_route_sample(progress, streaming_distance_m)
        } else if waypoint_route {
            qa_waypoint_route_sample(progress, qa_waypoint_axis(route_origin), visible_radius)
        } else if hydro_route {
            if let Some(plan) = frozen_camera_route_plan {
                let anchor = frozen_focus_anchor.expect("hydro route has an anchor");
                let base_axis =
                    qa_camera_route_base_axis(&world.generator, settings.seed, route_focus, anchor);
                qa_hydro_route_sample_for_variant(
                    progress,
                    route_focus,
                    confirmed_near_extent_metres,
                    visible_radius,
                    base_axis,
                    plan.variant_index,
                )
            } else {
                qa_hydro_route_sample(
                    progress,
                    route_focus,
                    confirmed_near_extent_metres,
                    visible_radius,
                )
            }
        } else {
            let (landing_offset, landing_height) =
                landing_context.unwrap_or((Vec2::new(-124.0, 24.0), -58.0));
            qa_hero_route_sample(progress, landing_offset, landing_height, visible_radius)
        }
    };

    if !qa.route_ready {
        qa.warmup_elapsed += dt;
        let preflight_staging = qa.camera_route_preflight_pending
            && qa.camera_route_policy == QaCameraRoutePolicy::PreflightV1
            && qa.resolved_focus.requires_hydro_anchor();
        if preflight_staging {
            qa.camera_route_preflight_elapsed += dt;
        }
        // Normal warmup holds the actual establishing shot. Once bounded
        // camera preflight begins, hold the route midpoint instead: every
        // River/Lava variant stays inside the same fixed near-streaming disc,
        // unlike warming one end and probing the antipodal end immediately.
        if cinematic_route {
            let sample = if preflight_staging {
                qa_camera_preflight_staging_sample()
            } else {
                route_sample(0.0)
            };
            let (pos, target) = qa_world_pose(&world, route_origin, sample);
            qa.current_phase = sample.phase;
            qa_apply_pose(&mut transform, &mut player, pos, target);
        } else {
            player.velocity = Vec3::ZERO;
            player.flying = true;
            player.placed_on_surface = true;
        }
        if raw_dt.is_finite() && raw_dt >= 0.10 {
            let warmup_elapsed = qa.warmup_elapsed;
            let (stage, at_seconds, route_seconds) = qa_stall_timing(warmup_elapsed, None);
            qa.stalls.push(QaStall {
                at_seconds,
                stage,
                route_seconds,
                frame_ms: raw_dt * 1000.0,
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
        if qa.focus == QaFocus::NearFar
            && !qa_near_far_route_available(&planetary_telemetry, requested_profile, visible_radius)
        {
            qa.focus_evidence_ready = false;
            qa.focus_unavailable_reason = Some("streaming-not-ready".to_owned());
            qa.resolved_focus = QaFocus::Scenic;
            warn!(
                "QA: near-far focus unavailable: confirmed near extent {} m and visible radius {:.1} m cannot prove both sides of the seam",
                planetary_telemetry.confirmed_near_extent_metres,
                visible_radius,
            );
        }
        // Focus availability can change above after `hydro_route` was derived
        // for this frame. Recompute applicability so a failed NearFar gate
        // cannot inherit an empty camera reason or claim a usable route.
        let preflight_applicable = qa.focus.requires_hydro_anchor();
        let preflight_ready = preflight_applicable
            && qa.resolved_focus.requires_hydro_anchor()
            && qa.focus_anchor.is_some()
            && qa.focus_evidence_ready;
        if preflight_ready {
            if qa.camera_route_policy == QaCameraRoutePolicy::Legacy {
                qa.camera_route_preflight_pending = false;
                qa.camera_route_preflight_elapsed = 0.0;
                qa.camera_route_available = true;
                qa.camera_route_unavailable_reason = None;
                qa.camera_route_validation = QaCameraRouteValidation {
                    variant_count: 1,
                    validation_samples: 0,
                    voxel_query_cap: QA_CAMERA_ROUTE_VOXEL_QUERY_CAP,
                    ..QaCameraRouteValidation::default()
                };
            } else if !qa.camera_route_preflight_pending {
                qa.camera_route_preflight_pending = true;
                qa.camera_route_preflight_elapsed = 0.0;
                let sample = qa_camera_preflight_staging_sample();
                let (pos, target) = qa_world_pose(&world, route_origin, sample);
                qa.current_phase = sample.phase;
                qa_apply_pose(&mut transform, &mut player, pos, target);
                info!("QA: camera preflight warming the bounded route-midpoint residency disc before exact voxel validation");
                return;
            } else if let Some(anchor) = qa.focus_anchor {
                let residency_ready = qa.camera_route_preflight_elapsed >= 3.0
                    && streamer.frontier_complete
                    && streamer.pending_terrain.is_empty();
                if !residency_ready
                    && qa.camera_route_preflight_elapsed
                        < QA_CAMERA_PREFLIGHT_RESIDENCY_TIMEOUT_SECONDS
                {
                    return;
                }
                if !residency_ready {
                    warn!(
                        "QA: camera preflight residency did not settle within {:.1}s; probing fail-closed current request truth",
                        QA_CAMERA_PREFLIGHT_RESIDENCY_TIMEOUT_SECONDS,
                    );
                }
                let voxel_source = QaWorldVoxelSource {
                    world: &world,
                    streamer: &streamer,
                };
                let (plan, reason, validation) = qa_camera_route_preflight(
                    &voxel_source,
                    &world,
                    route_origin,
                    route_focus,
                    anchor,
                    settings.seed,
                    requested_profile,
                    settings.scenery_quality,
                    confirmed_near_extent_metres,
                    visible_radius,
                );
                qa.camera_route_plan = plan;
                qa.camera_route_preflight_pending = false;
                qa.camera_route_available = plan.is_some();
                qa.camera_route_unavailable_reason = reason;
                qa.camera_route_validation = validation;
                if let Some(plan) = plan {
                    info!(
                        "QA: camera preflight selected variant {} plan {:016x} after {}/{} voxel queries",
                        plan.variant_index,
                        plan.plan_hash,
                        validation.voxel_queries,
                        validation.voxel_query_cap,
                    );
                } else {
                    warn!(
                        "QA: camera preflight failed closed: {:?} after {}/{} voxel queries; no acceptance route or captures",
                        reason,
                        validation.voxel_queries,
                        validation.voxel_query_cap,
                    );
                }
            }
        } else {
            // PreflightV1 currently governs exact hydro acceptance routes. The
            // existing scenic/waypoint/streaming paths remain explicit legacy
            // routes until they receive their own semantic visibility rules.
            qa.camera_route_available = false;
            qa.camera_route_unavailable_reason =
                preflight_applicable.then_some(QaCameraRouteUnavailableReason::FocusUnavailable);
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
    }
    let route_t = qa.elapsed.max(0.0);
    if hydro_route && !qa.camera_route_available {
        player.velocity = Vec3::ZERO;
        player.flying = true;
        player.placed_on_surface = true;
        qa.elapsed = qa.duration;
        return;
    }
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

    if raw_dt.is_finite() && raw_dt >= 0.10 {
        let (stage, at_seconds, route_seconds) =
            qa_stall_timing(qa.warmup_elapsed, Some(qa.elapsed));
        let pos = transform.translation;
        qa.stalls.push(QaStall {
            at_seconds,
            stage,
            route_seconds,
            frame_ms: raw_dt * 1000.0,
            pos: [pos.x, pos.y, pos.z],
            pending_terrain: streamer.pending_terrain.len(),
            pending_meshes: streamer.pending_meshes.len(),
            dirty_chunks: streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
        });
    }
}

fn qa_screenshot_observation(
    capture_index: usize,
    screenshot_path: String,
    scheduled_capture_seconds: f32,
    duration_seconds: f32,
    player_transform: &Transform,
) -> Option<QaScreenshotObservation> {
    let observation = QaScreenshotObservation {
        capture_index,
        screenshot_path,
        scheduled_capture_seconds,
        player_camera_translation_metres: player_transform.translation.to_array(),
        player_camera_rotation_xyzw: player_transform.rotation.to_array(),
    };
    qa_screenshot_observation_fields_valid(&observation, duration_seconds).then_some(observation)
}

fn qa_screenshot_observation_fields_valid(
    observation: &QaScreenshotObservation,
    duration_seconds: f32,
) -> bool {
    if observation.capture_index >= QA_SCREENSHOT_OBSERVATION_CAP
        || observation.screenshot_path.is_empty()
        || !observation.screenshot_path.is_ascii()
        || observation.screenshot_path.chars().count() > QA_SCREENSHOT_PATH_MAX_CHARS
        || !duration_seconds.is_finite()
        || observation.scheduled_capture_seconds < 0.0
        || observation.scheduled_capture_seconds > duration_seconds + 0.001
        || !observation.scheduled_capture_seconds.is_finite()
        || observation
            .player_camera_translation_metres
            .iter()
            .any(|value| {
                !value.is_finite() || f64::from(value.abs()) > QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT
            })
        || observation
            .player_camera_rotation_xyzw
            .iter()
            .any(|value| !value.is_finite())
    {
        return false;
    }

    let rotation_norm_squared = observation
        .player_camera_rotation_xyzw
        .iter()
        .map(|value| value * value)
        .sum::<f32>();
    (0.999..=1.001).contains(&rotation_norm_squared)
}

fn qa_screenshot_ledger_valid(
    screenshots: &[String],
    observations: &[QaScreenshotObservation],
    duration_seconds: f32,
    rejections: usize,
    cap_exhausted: bool,
) -> bool {
    if rejections != 0
        || cap_exhausted
        || observations.is_empty()
        || observations.len() > QA_SCREENSHOT_OBSERVATION_CAP
        || screenshots.len() != observations.len()
    {
        return false;
    }

    let mut previous_scheduled_capture = None;
    for (expected_index, (legacy_path, observation)) in
        screenshots.iter().zip(observations).enumerate()
    {
        if observation.capture_index != expected_index
            || legacy_path != &observation.screenshot_path
            || !qa_screenshot_observation_fields_valid(observation, duration_seconds)
            || previous_scheduled_capture
                .is_some_and(|previous| observation.scheduled_capture_seconds <= previous)
            || observations[..expected_index]
                .iter()
                .any(|previous| previous.screenshot_path == observation.screenshot_path)
        {
            return false;
        }
        previous_scheduled_capture = Some(observation.scheduled_capture_seconds);
    }
    true
}

fn qa_capture_screenshot(
    mut qa: ResMut<QaAutopilot>,
    mut screenshots: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
    players: Query<&Transform, With<Player>>,
) {
    if !qa.enabled
        || qa.finished
        || !qa.camera_route_available
        || !qa_screenshot_due(qa.elapsed, qa.next_screenshot_at, qa.duration)
    {
        return;
    }
    let scheduled_capture_seconds = qa.next_screenshot_at;
    qa.next_screenshot_at += qa.screenshot_interval;
    if qa.screenshot_observations.len() >= QA_SCREENSHOT_OBSERVATION_CAP {
        qa.screenshot_observation_cap_exhausted = true;
        qa.screenshot_observation_rejections =
            qa.screenshot_observation_rejections.saturating_add(1);
        warn!("QA: screenshot observation cap of {QA_SCREENSHOT_OBSERVATION_CAP} was exhausted");
        return;
    }
    let Ok(window) = windows.get_single() else {
        qa.screenshot_observation_rejections =
            qa.screenshot_observation_rejections.saturating_add(1);
        warn!("QA: screenshot rejected because the primary window was not unique");
        return;
    };
    let Ok(player_transform) = players.get_single() else {
        qa.screenshot_observation_rejections =
            qa.screenshot_observation_rejections.saturating_add(1);
        warn!("QA: screenshot rejected because the Player camera was not unique");
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

    let display_path = path.to_string_lossy().to_string();
    let Some(observation) = qa_screenshot_observation(
        qa.screenshot_index,
        display_path.clone(),
        scheduled_capture_seconds,
        qa.duration,
        player_transform,
    ) else {
        qa.screenshot_observation_rejections =
            qa.screenshot_observation_rejections.saturating_add(1);
        warn!("QA: screenshot rejected because its bounded pose observation was invalid");
        return;
    };
    match screenshots.save_screenshot_to_disk(window, &path) {
        Ok(_) => {
            info!("QA: screenshot queued for {}", display_path);
            qa.screenshots.push(display_path);
            qa.screenshot_observations.push(observation);
            qa.screenshot_index += 1;
        }
        Err(e) => {
            qa.screenshot_observation_rejections =
                qa.screenshot_observation_rejections.saturating_add(1);
            warn!("QA: screenshot failed: {e}");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QaCompletionDecision {
    Wait,
    Success,
    TimedOut,
}

fn qa_finish(
    real_time: Res<Time<Real>>,
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
    if !qa.enabled || qa.finished {
        return;
    }

    // PostUpdate is the evidence boundary for the complete frame. Observe the
    // combined dense population here so a peak cannot hide behind ordering of
    // individual Update systems that mutate residency or schedule terrain.
    let (dense_chunks, dense_chunk_budget_exceeded) =
        qa_dense_chunk_observation(world.chunks.len(), streamer.pending_terrain.len());
    qa.peak_loaded_chunks = qa.peak_loaded_chunks.max(world.chunks.len());
    qa.peak_pending_terrain = qa.peak_pending_terrain.max(streamer.pending_terrain.len());
    qa.peak_dense_chunks = qa.peak_dense_chunks.max(dense_chunks);
    qa.dense_chunk_budget_exceeded |= dense_chunk_budget_exceeded;

    // These deadlines advance from Bevy's monotonic real clock here,
    // independent of the Player query and virtual route clock in
    // `qa_drive_camera`. A missing Player, paused virtual time, or blocked
    // generator handoff therefore becomes a controlled lifecycle failure.
    let real_dt = real_time.delta_seconds();
    if real_dt.is_finite() && real_dt >= 0.0 {
        let lifecycle_cap = qa.duration + QA_ROUTE_LIFECYCLE_RESERVE_SECONDS;
        qa.lifecycle_elapsed = (qa.lifecycle_elapsed + real_dt).min(lifecycle_cap);
    } else {
        qa.lifecycle_elapsed = f32::NAN;
    }
    let route_timed_out =
        qa_route_lifecycle_timed_out(qa.elapsed, qa.duration, qa.lifecycle_elapsed);
    if qa.elapsed < qa.duration && !route_timed_out {
        return;
    }

    if !route_timed_out {
        qa.write_tail_elapsed = qa_observed_interval_elapsed(
            qa.finish_wait_frames,
            qa.write_tail_elapsed,
            real_dt,
            QA_COMPLETION_SETTLE_TIMEOUT_SECONDS,
        )
        .unwrap_or(f32::NAN);
    }

    // ScreenshotManager performs GPU readback and PNG writes asynchronously.
    // The final capture is commonly queued in this same chained update, so
    // always give the renderer at least two complete frames. Streaming uses a
    // separate dual time/frame drain bound below; a short PNG tail must never
    // be mistaken for proof that the serial far-field scheduler is settled.
    let completion_decision = if route_timed_out {
        QaCompletionDecision::TimedOut
    } else {
        qa.finish_wait_frames = qa.finish_wait_frames.saturating_add(1);
        if qa.finish_wait_frames < 2 {
            return;
        }
        let (_, current_dense_chunk_budget_exceeded) =
            qa_dense_chunk_observation(world.chunks.len(), streamer.pending_terrain.len());
        let completion_streaming_settled = !qa.dense_chunk_budget_exceeded
            && !current_dense_chunk_budget_exceeded
            && qa_completion_streaming_settled(
                streamer.frontier_complete,
                streamer.pending_terrain.len(),
                streamer.pending_meshes.len(),
                streamer.dirty_queue.len() + world.edit_dirty_chunks.len(),
                planetary_telemetry.as_deref(),
            );
        if completion_streaming_settled {
            qa.settled_wait_elapsed = qa_observed_interval_elapsed(
                qa.settled_wait_frames,
                qa.settled_wait_elapsed,
                real_dt,
                QA_COMPLETION_STABLE_SECONDS,
            )
            .unwrap_or(0.0);
            qa.settled_wait_frames = qa.settled_wait_frames.saturating_add(1);
        } else {
            qa.settled_wait_frames = 0;
            qa.settled_wait_elapsed = 0.0;
        }
        qa_completion_decision(
            completion_streaming_settled,
            qa.write_tail_elapsed,
            qa.finish_wait_frames,
            qa.settled_wait_elapsed,
            qa.settled_wait_frames,
        )
    };
    if completion_decision == QaCompletionDecision::Wait {
        return;
    }
    let completion_timed_out = completion_decision == QaCompletionDecision::TimedOut;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let finish_wait_seconds = qa.write_tail_elapsed;
        if !qa
            .screenshots
            .iter()
            .all(|path| qa_screenshot_file_ready(path))
            && !completion_timed_out
            && finish_wait_seconds < QA_SCREENSHOT_WRITE_TIMEOUT_SECONDS
        {
            return;
        }
        let before = qa.screenshot_observations.len();
        let legacy_paths_aligned = qa.screenshots.len() == qa.screenshot_observations.len()
            && qa
                .screenshots
                .iter()
                .zip(&qa.screenshot_observations)
                .all(|(legacy, observation)| legacy == &observation.screenshot_path);
        let mut retained_observations = Vec::with_capacity(QA_SCREENSHOT_OBSERVATION_CAP);
        for observation in std::mem::take(&mut qa.screenshot_observations) {
            if qa_screenshot_file_ready(&observation.screenshot_path) {
                retained_observations.push(observation);
            }
        }
        let dropped = before.saturating_sub(retained_observations.len());
        qa.screenshots = retained_observations
            .iter()
            .map(|observation| observation.screenshot_path.clone())
            .collect();
        qa.screenshot_observations = retained_observations;
        if !legacy_paths_aligned {
            qa.screenshot_observation_rejections =
                qa.screenshot_observation_rejections.saturating_add(1);
        }
        if dropped != 0 {
            qa.screenshot_observation_rejections =
                qa.screenshot_observation_rejections.saturating_add(dropped);
            warn!(
                "QA: {} queued screenshot(s) did not finish within the bounded write tail",
                dropped
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
    let (qa_report_schema_version, evidence_disposition) =
        qa_report_contract(planetary_telemetry.as_deref());
    let screenshot_observation_valid = qa_screenshot_ledger_valid(
        &qa.screenshots,
        &qa.screenshot_observations,
        qa.duration,
        qa.screenshot_observation_rejections,
        qa.screenshot_observation_cap_exhausted,
    );

    let (dense_chunks, current_dense_chunk_budget_exceeded) =
        qa_dense_chunk_observation(world.chunks.len(), streamer.pending_terrain.len());
    let dense_chunk_budget_exceeded =
        qa.dense_chunk_budget_exceeded || current_dense_chunk_budget_exceeded;
    let report = QaReport {
        qa_report_schema_version: qa_report_schema_version.to_owned(),
        evidence_disposition: evidence_disposition.to_owned(),
        run_identity: qa_run_identity(active_world.as_deref()),
        world_edit_store_status: world.edit_store_status.label().to_owned(),
        world_edit_store_compatible: active_world.as_deref().is_some_and(|active_world| {
            world
                .edit_store_status
                .is_compatible_with(active_world.meta.generation_identity())
        }),
        world_edit_store_seed: world
            .edit_store_status
            .generation_identity()
            .map(|identity| identity.seed),
        world_edit_store_profile: world
            .edit_store_status
            .generation_identity()
            .map(|identity| format!("{:?}", identity.world_profile)),
        world_edit_store_scenery_quality: world
            .edit_store_status
            .generation_identity()
            .map(|identity| format!("{:?}", identity.scenery_quality)),
        world_edit_store_terrain_grammar: world
            .edit_store_status
            .generation_identity()
            .map(|identity| format!("{:?}", identity.terrain_grammar)),
        world_edit_store_edited_chunks: world.edit_store_status.edited_chunks(),
        world_edit_store_block_reason_code: world
            .edit_store_status
            .reason_code()
            .map(str::to_owned),
        viewport: qa_viewport(windows.get_single().ok()),
        planetary_streaming: qa_planetary_streaming(planetary_telemetry.as_deref()),
        requested_route_focus: qa.requested_focus_label.clone(),
        resolved_route_focus: qa.resolved_focus.label().to_owned(),
        route_focus_available: qa.focus_evidence_ready,
        route_focus_unavailable_reason: qa.focus_unavailable_reason.clone(),
        route_focus_anchor: qa.focus_anchor.map(QaFocusAnchor::report_value),
        route_focus_search_candidate_cap: qa.focus_search_candidate_cap,
        route_focus_search_visited_candidates: qa.focus_search_visited_candidates,
        route_focus_classification_query_cap: qa.focus_classification_query_cap,
        route_focus_classification_queries: qa.focus_classification_queries,
        route_focus_search_cap_exhausted: qa.focus_search_cap_exhausted,
        camera_route_preflight_applicable: qa.focus.requires_hydro_anchor(),
        camera_route_policy: qa.camera_route_policy.label().to_owned(),
        camera_route_plan_hash: qa
            .camera_route_plan
            .map(|plan| format!("{:016x}", plan.plan_hash)),
        camera_route_available: qa.camera_route_available,
        camera_route_unavailable_reason: qa
            .camera_route_unavailable_reason
            .map(|reason| reason.label().to_owned()),
        camera_route_variant_index: qa.camera_route_plan.map(|plan| plan.variant_index),
        camera_route_variant_count: qa.camera_route_validation.variant_count,
        camera_route_validation_samples: qa.camera_route_validation.validation_samples,
        camera_route_selected_clear_samples: qa.camera_route_validation.selected_clear_samples,
        camera_route_voxel_queries: qa.camera_route_validation.voxel_queries,
        camera_route_voxel_query_cap: qa.camera_route_validation.voxel_query_cap,
        camera_route_required_chunk_checks: qa.camera_route_validation.required_chunk_checks,
        camera_route_loaded_chunk_checks: qa.camera_route_validation.loaded_chunk_checks,
        camera_route_proven_air_chunk_checks: qa.camera_route_validation.proven_air_chunk_checks,
        camera_route_unloaded_chunk_checks: qa.camera_route_validation.unloaded_chunk_checks,
        camera_route_candidate_body_occlusions: qa
            .camera_route_validation
            .candidate_body_occlusions,
        camera_route_candidate_los_occlusions: qa.camera_route_validation.candidate_los_occlusions,
        camera_route_minimum_clearance_voxels: qa.camera_route_validation.minimum_clearance_voxels,
        camera_route_work_cap_exhausted: qa.camera_route_validation.work_cap_exhausted,
        requested_route_distance_m: if qa.focus == QaFocus::Streaming {
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
        dense_chunks,
        dense_chunk_budget: MAX_FULL_CHUNK_RESIDENT,
        dense_chunk_budget_exceeded,
        frontier_complete: streamer.frontier_complete,
        render_distance: governor.effective_render_distance,
        peak_loaded_chunks: qa.peak_loaded_chunks,
        peak_dense_chunks: qa.peak_dense_chunks,
        peak_mesh_entities: qa.peak_mesh_entities,
        peak_pending_terrain: qa.peak_pending_terrain,
        peak_pending_meshes: qa.peak_pending_meshes,
        peak_dirty_chunks: qa.peak_dirty_chunks,
        screenshots: qa.screenshots.clone(),
        screenshot_observation_cap: QA_SCREENSHOT_OBSERVATION_CAP,
        screenshot_path_max_chars: QA_SCREENSHOT_PATH_MAX_CHARS,
        screenshot_observation_count: qa.screenshot_observations.len(),
        screenshot_observation_valid,
        screenshot_observation_cap_exhausted: qa.screenshot_observation_cap_exhausted,
        screenshot_observation_rejections: qa.screenshot_observation_rejections,
        screenshot_observations: qa.screenshot_observations.clone(),
        stalls: qa.stalls.clone(),
    };

    #[cfg(not(target_arch = "wasm32"))]
    let report_saved = {
        let path = qa.report_dir.join("report.ron");
        match ron::ser::to_string_pretty(&report, ron::ser::PrettyConfig::default()) {
            Ok(text) => match qa_write_report_atomic(&path, text.as_bytes()) {
                Ok(_) => {
                    info!("QA: report saved to {}", path.display().to_string());
                    true
                }
                Err(e) => {
                    warn!("QA: report write failed: {e}");
                    false
                }
            },
            Err(e) => {
                warn!("QA: report serialize failed: {e}");
                false
            }
        }
    };

    #[cfg(not(target_arch = "wasm32"))]
    if !report_saved {
        warn!("QA: no atomic report was published; exiting with controlled failure");
        exit.send(AppExit::error());
        return;
    }

    info!(
        "QA: finished {:.1}s, avg {:.1} fps, max {:.1} ms, stalls {}, screenshots {}",
        report.duration_seconds,
        report.average_fps,
        report.max_frame_ms,
        report.stalls.len(),
        report.screenshots.len()
    );
    if completion_timed_out {
        if route_timed_out {
            warn!(
                "QA: route did not complete within {:.1}s of independent real time; exiting with controlled failure",
                qa.lifecycle_elapsed,
            );
        } else {
            warn!(
                "QA: streaming did not settle after {:.1}s and {} completion frames; exiting with controlled failure",
                qa.write_tail_elapsed,
                qa.finish_wait_frames,
            );
        }
        exit.send(AppExit::error());
    } else {
        exit.send(AppExit::Success);
    }
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

pub(crate) fn qa_enabled() -> bool {
    qa_requested_from(
        std::env::var("VOXEL_NATIVE_QA").ok().as_deref(),
        std::env::args_os(),
    )
}

fn qa_requested_from<I, S>(environment_value: Option<&str>, arguments: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    environment_value.is_some_and(env_truthy_value)
        || arguments.into_iter().any(|argument| {
            let argument = argument.as_ref();
            argument == std::ffi::OsStr::new("--qa")
                || argument == std::ffi::OsStr::new("--qa-autopilot")
        })
}

/// Avoid judging half-streamed geometry. Three seconds prevents the empty
/// initial queues from releasing the camera before load scheduling begins;
/// the bounded fallback keeps QA from hanging forever on a slow machine.
fn qa_stream_ready(warmup_seconds: f32, pending_terrain: usize, pending_meshes: usize) -> bool {
    (warmup_seconds >= 3.0 && pending_terrain <= 4 && pending_meshes <= 6) || warmup_seconds >= 14.0
}

fn qa_dense_chunk_observation(resident_chunks: usize, pending_terrain: usize) -> (usize, bool) {
    match resident_chunks.checked_add(pending_terrain) {
        Some(total) => (total, total > MAX_FULL_CHUNK_RESIDENT),
        None => (usize::MAX, true),
    }
}

fn qa_completion_streaming_settled(
    frontier_complete: bool,
    pending_terrain: usize,
    pending_meshes: usize,
    dirty_chunks: usize,
    telemetry: Option<&PlanetaryStreamingTelemetry>,
) -> bool {
    frontier_complete
        && pending_terrain == 0
        && pending_meshes == 0
        && dirty_chunks == 0
        && telemetry.is_some_and(|telemetry| {
            telemetry.enabled
                && telemetry.desired_terrain_grammar.is_some()
                && telemetry.active_terrain_grammar == telemetry.desired_terrain_grammar
                && telemetry.active_l0_height_mode == Some(telemetry.desired_l0_height_mode)
                && telemetry.resident_l0_height_mode == Some(telemetry.desired_l0_height_mode)
                && telemetry.resident_entities == FAR_FIELD_LEVELS
                && telemetry.scheduler_resident_entities == FAR_FIELD_LEVELS
                && telemetry.live_sample_cache_windows == FAR_FIELD_LEVELS
                && telemetry
                    .resident_material_detail
                    .iter()
                    .zip(telemetry.desired_material_detail.iter())
                    .all(|(resident, desired)| *resident == Some(*desired))
                && !telemetry.near_coverage_transition_pending
                && telemetry.resident_observation_valid
                && !telemetry.resident_entity_count_overflow
                && telemetry.resident_duplicate_levels == 0
                && telemetry.resident_out_of_range_levels == 0
                && !telemetry.resident_scheduler_mismatch
                && !telemetry.resident_budget_exceeded
                && telemetry.resident_fluid_observation_valid
                && !telemetry.resident_fluid_entity_count_overflow
                && telemetry.resident_fluid_duplicate_slots == 0
                && telemetry.resident_fluid_out_of_range_levels == 0
                && !telemetry.resident_fluid_scheduler_mismatch
                && !telemetry.resident_fluid_budget_exceeded
                && telemetry.resident_fluid_kind_integrity_valid
                && telemetry.resident_semantic_cohort_observation_valid
                && !telemetry.resident_semantic_cohort_entity_count_overflow
                && !telemetry.resident_semantic_cohort_scheduler_mismatch
                && !telemetry.resident_semantic_cohort_budget_exceeded
                && telemetry.resident_semantic_cohort_payload_integrity_valid
                && telemetry.pending_rebuilds == 0
                && telemetry.dirty_mask == 0
                && !telemetry.build_in_flight
        })
}

fn qa_route_lifecycle_timed_out(
    route_elapsed: f32,
    route_duration: f32,
    lifecycle_elapsed: f32,
) -> bool {
    if route_elapsed.is_finite() && route_duration.is_finite() && route_elapsed >= route_duration {
        return false;
    }
    !route_elapsed.is_finite()
        || !route_duration.is_finite()
        || !lifecycle_elapsed.is_finite()
        || lifecycle_elapsed >= route_duration + QA_ROUTE_LIFECYCLE_RESERVE_SECONDS
}

/// Accumulate only intervals bracketed by two observations. The delta on the
/// first snapshot precedes that snapshot and must never be credited as proven
/// screenshot-tail or quiescence time.
fn qa_observed_interval_elapsed(
    prior_observation_frames: u16,
    elapsed_seconds: f32,
    delta_seconds: f32,
    cap_seconds: f32,
) -> Option<f32> {
    if prior_observation_frames == 0 {
        return Some(0.0);
    }
    if !elapsed_seconds.is_finite()
        || elapsed_seconds < 0.0
        || !delta_seconds.is_finite()
        || delta_seconds < 0.0
        || !cap_seconds.is_finite()
        || cap_seconds < 0.0
    {
        return None;
    }
    Some((elapsed_seconds + delta_seconds).min(cap_seconds))
}

fn qa_completion_decision(
    streaming_settled: bool,
    settle_tail_seconds: f32,
    finish_wait_frames: u16,
    settled_seconds: f32,
    settled_frames: u16,
) -> QaCompletionDecision {
    if streaming_settled
        && settled_seconds.is_finite()
        && settled_seconds >= QA_COMPLETION_STABLE_SECONDS
        && settled_frames >= QA_COMPLETION_STABLE_MIN_FRAMES
    {
        return QaCompletionDecision::Success;
    }
    let elapsed_expired = !settle_tail_seconds.is_finite()
        || settle_tail_seconds >= QA_COMPLETION_SETTLE_TIMEOUT_SECONDS;
    if elapsed_expired && finish_wait_frames >= QA_COMPLETION_SETTLE_MIN_FRAMES {
        QaCompletionDecision::TimedOut
    } else {
        QaCompletionDecision::Wait
    }
}

fn env_truthy_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
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

fn parse_terrain_grammar(value: &str) -> Option<TerrainGrammarVersion> {
    match value.trim().to_ascii_lowercase().as_str() {
        "v1" | "1" | "legacy" => Some(TerrainGrammarVersion::V1),
        "v2" | "2" => Some(TerrainGrammarVersion::V2),
        "v3" | "3" | "current" => Some(TerrainGrammarVersion::V3),
        _ => None,
    }
}

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use bevy::prelude::{Quat, Transform, Vec2, Vec3, Window};
    use std::collections::{HashMap, HashSet};

    use crate::blocks::BlockType;

    #[cfg(not(target_arch = "wasm32"))]
    use super::qa_png_tail_is_complete;
    use super::{
        parse_finite_f32, parse_scenery_quality, parse_terrain_grammar, qa_bounded_text,
        qa_camera_plan_hash, qa_camera_pose_voxel, qa_camera_preflight_staging_sample,
        qa_camera_probe_body, qa_camera_probe_ray, qa_camera_route_preflight,
        qa_camera_route_variant_basis, qa_camera_voxel_is_visual_body_blocker,
        qa_completion_decision, qa_completion_streaming_settled, qa_dense_chunk_observation,
        qa_find_hydro_focus, qa_focus_has_exact_search_work, qa_focus_search_exhausted,
        qa_generator_signature, qa_git_sha, qa_hero_route_sample,
        qa_hydro_route_sample_for_variant, qa_near_far_route_available, qa_nearest_rank,
        qa_observe_route_frame_time, qa_observed_interval_elapsed, qa_optional_bool,
        qa_planetary_streaming, qa_profile_anchor_ready, qa_provenance_token, qa_report_contract,
        qa_requested_from, qa_route_lifecycle_timed_out, qa_route_phase, qa_screenshot_due,
        qa_screenshot_ledger_valid, qa_screenshot_observation, qa_stall_timing, qa_stream_ready,
        qa_streaming_route_sample, qa_viewport, qa_waypoint_axis, qa_waypoint_route_sample,
        QaCameraProbeResult, QaCameraQueryBudget, QaCameraVoxelResolution, QaCameraVoxelSource,
        QaCompletionDecision, QaFocus, QaFocusAnchor, QaFrameTimeAccumulator, QaReport,
        QaRoutePhase, QaRunIdentity, QaScreenshotObservation, QaStallStage, QaWorldVoxelSource,
        MAX_FULL_CHUNK_RESIDENT, QA_BUILD_PROFILE, QA_CAMERA_BODY_QUERY_CAP_PER_POSE,
        QA_CAMERA_LOS_QUERY_CAP_PER_RAY, QA_CAMERA_ROUTE_PROGRESS_SAMPLES,
        QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT, QA_CAMERA_ROUTE_VALIDATION_SAMPLES,
        QA_CAMERA_ROUTE_VARIANTS, QA_CAMERA_ROUTE_VOXEL_QUERY_CAP,
        QA_CANONICAL_EVIDENCE_DISPOSITION, QA_COMPLETION_SETTLE_MIN_FRAMES,
        QA_COMPLETION_SETTLE_TIMEOUT_SECONDS, QA_COMPLETION_STABLE_MIN_FRAMES,
        QA_COMPLETION_STABLE_SECONDS, QA_COMPLETION_TWO_BATCH_MIN_FRAMES,
        QA_DIAGNOSTIC_EVIDENCE_DISPOSITION,
        QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_EVIDENCE_DISPOSITION,
        QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_REPORT_SCHEMA_VERSION,
        QA_DIAGNOSTIC_L0_HEIGHT_REPORT_SCHEMA_VERSION,
        QA_DIAGNOSTIC_LOD_PROVENANCE_EVIDENCE_DISPOSITION,
        QA_DIAGNOSTIC_LOD_PROVENANCE_REPORT_SCHEMA_VERSION, QA_FINGERPRINT_MAX_CHARS,
        QA_FRAME_TIME_ACCUMULATOR_BYTE_CAP, QA_FRAME_TIME_BUCKETS, QA_FRAME_TIME_QUANTILE_WORK_CAP,
        QA_REPORT_SCHEMA_VERSION, QA_ROUTE_LIFECYCLE_RESERVE_SECONDS,
        QA_SCREENSHOT_LEDGER_BYTE_CAP, QA_SCREENSHOT_OBSERVATION_CAP, QA_SCREENSHOT_PATH_MAX_CHARS,
    };
    use crate::planetary_streaming::{
        PlanetaryStreamingTelemetry, FAR_FIELD_LEVELS, FAR_FIELD_MAX_UPDATE_CADENCE_FRAMES,
    };
    use crate::settings::{SceneryQuality, TerrainGrammarVersion, WorldProfile};
    use crate::terrain::TerrainGenerator;

    #[derive(Default)]
    struct TestVoxelSource {
        all_chunks_loaded: bool,
        loaded_chunks: HashSet<(i32, i32, i32)>,
        proven_air_voxels: HashSet<(i32, i32, i32)>,
        voxels: HashMap<(i32, i32, i32), crate::blocks::Voxel>,
    }

    impl QaCameraVoxelSource for TestVoxelSource {
        fn resolve_voxel(
            &self,
            world_x: i32,
            world_y: i32,
            world_z: i32,
        ) -> QaCameraVoxelResolution {
            if self.all_chunks_loaded
                || self.loaded_chunks.contains(&(
                    world_x.div_euclid(crate::chunk::CHUNK_SIZE_I),
                    world_y.div_euclid(crate::chunk::CHUNK_SIZE_I),
                    world_z.div_euclid(crate::chunk::CHUNK_SIZE_I),
                ))
            {
                return QaCameraVoxelResolution::Resident(
                    self.voxels
                        .get(&(world_x, world_y, world_z))
                        .copied()
                        .unwrap_or(crate::blocks::AIR),
                );
            }
            if self
                .proven_air_voxels
                .contains(&(world_x, world_y, world_z))
            {
                return QaCameraVoxelResolution::ProvenAir;
            }
            QaCameraVoxelResolution::Unavailable
        }
    }

    fn loaded_test_source(min: i32, max: i32) -> TestVoxelSource {
        let mut source = TestVoxelSource::default();
        let min_chunk = min.div_euclid(crate::chunk::CHUNK_SIZE_I);
        let max_chunk = max.div_euclid(crate::chunk::CHUNK_SIZE_I);
        for chunk_y in -2..=2 {
            for chunk_x in min_chunk..=max_chunk {
                for chunk_z in min_chunk..=max_chunk {
                    source.loaded_chunks.insert((chunk_x, chunk_y, chunk_z));
                }
            }
        }
        source
    }

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
        let screenshot_path = "qa_runs/run_test/shot_0000_detail.png".to_owned();
        let screenshot_observation = qa_screenshot_observation(
            0,
            screenshot_path.clone(),
            2.5,
            12.0,
            &Transform::from_translation(Vec3::new(1.0, 2.0, 3.0))
                .with_rotation(Quat::from_rotation_y(0.25)),
        )
        .expect("bounded screenshot observation");
        let report = QaReport {
            qa_report_schema_version: QA_REPORT_SCHEMA_VERSION.to_owned(),
            evidence_disposition: QA_CANONICAL_EVIDENCE_DISPOSITION.to_owned(),
            run_identity: QaRunIdentity {
                package_version: "test".to_owned(),
                build_profile: QA_BUILD_PROFILE.to_owned(),
                instance_label: Some("serialization".to_owned()),
                world_name: None,
                world_seed: Some(7),
                world_profile: None,
                scenery_quality: None,
                terrain_grammar: Some("V3".to_owned()),
                git_sha: Some("abcdef1".to_owned()),
                git_dirty: Some(true),
                source_fingerprint: Some("sha256:source".to_owned()),
                executable_hash: Some("sha256:executable".to_owned()),
                toolchain: Some("rustc test".to_owned()),
                hardware: Some("test hardware".to_owned()),
            },
            world_edit_store_status: "compatible".to_owned(),
            world_edit_store_compatible: true,
            world_edit_store_seed: Some(7),
            world_edit_store_profile: Some("Natural".to_owned()),
            world_edit_store_scenery_quality: Some("Balanced".to_owned()),
            world_edit_store_terrain_grammar: Some("V3".to_owned()),
            world_edit_store_edited_chunks: Some(0),
            world_edit_store_block_reason_code: None,
            viewport: None,
            planetary_streaming: None,
            requested_route_focus: "streaming".to_owned(),
            resolved_route_focus: "streaming".to_owned(),
            route_focus_available: true,
            route_focus_unavailable_reason: None,
            route_focus_anchor: None,
            route_focus_search_candidate_cap: 0,
            route_focus_search_visited_candidates: None,
            route_focus_classification_query_cap: 0,
            route_focus_classification_queries: None,
            route_focus_search_cap_exhausted: false,
            camera_route_preflight_applicable: false,
            camera_route_policy: "preflight-v1".to_owned(),
            camera_route_plan_hash: None,
            camera_route_available: false,
            camera_route_unavailable_reason: None,
            camera_route_variant_index: None,
            camera_route_variant_count: 0,
            camera_route_validation_samples: 0,
            camera_route_selected_clear_samples: 0,
            camera_route_voxel_queries: 0,
            camera_route_voxel_query_cap: 0,
            camera_route_required_chunk_checks: 0,
            camera_route_loaded_chunk_checks: 0,
            camera_route_proven_air_chunk_checks: 0,
            camera_route_unloaded_chunk_checks: 0,
            camera_route_candidate_body_occlusions: 0,
            camera_route_candidate_los_occlusions: 0,
            camera_route_minimum_clearance_voxels: None,
            camera_route_work_cap_exhausted: false,
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
            dense_chunks: 0,
            dense_chunk_budget: MAX_FULL_CHUNK_RESIDENT,
            dense_chunk_budget_exceeded: false,
            frontier_complete: true,
            render_distance: 0,
            peak_loaded_chunks: 0,
            peak_dense_chunks: 0,
            peak_mesh_entities: 0,
            peak_pending_terrain: 0,
            peak_pending_meshes: 0,
            peak_dirty_chunks: 0,
            screenshots: vec![screenshot_path],
            screenshot_observation_cap: QA_SCREENSHOT_OBSERVATION_CAP,
            screenshot_path_max_chars: QA_SCREENSHOT_PATH_MAX_CHARS,
            screenshot_observation_count: 1,
            screenshot_observation_valid: true,
            screenshot_observation_cap_exhausted: false,
            screenshot_observation_rejections: 0,
            screenshot_observations: vec![screenshot_observation],
            stalls: Vec::new(),
        };

        let serialized = ron::ser::to_string(&report).expect("serialize QA report");
        assert!(serialized.contains("route_frame_times"));
        assert!(serialized.contains("qa_report_schema_version:\"2.5.0\""));
        assert!(serialized.contains("evidence_disposition:\"canonical-candidate\""));
        assert!(serialized.contains("world_edit_store_status:\"compatible\""));
        assert!(serialized.contains("world_edit_store_compatible:true"));
        assert!(serialized.contains("world_edit_store_edited_chunks:Some(0)"));
        assert!(serialized.contains("terrain_grammar:Some(\"V3\")"));
        assert!(serialized.contains("sample_count:1"));
        assert!(serialized.contains("median_ms:Some(17.0)"));
        assert!(serialized.contains("build_profile:"));
        assert!(serialized.contains("git_dirty:Some(true)"));
        assert!(serialized.contains("requested_duration_seconds:12.0"));
        assert!(serialized.contains("write_tail_seconds:0.25"));
        assert!(serialized.contains("frontier_complete:true"));
        assert!(serialized.contains("screenshot_observation_count:1"));
        assert!(serialized.contains("scheduled_capture_seconds:2.5"));
        assert!(serialized.contains("player_camera_rotation_xyzw"));
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
    fn diagnostic_axes_use_distinct_fail_closed_report_identities() {
        use crate::planetary_streaming::{FarFieldL0HeightMode, FarFieldSurfaceMaterialMode};

        let baseline = PlanetaryStreamingTelemetry::default();
        assert_eq!(
            qa_report_contract(Some(&baseline)),
            (QA_REPORT_SCHEMA_VERSION, QA_CANONICAL_EVIDENCE_DISPOSITION)
        );

        let mut diagnostic_height = PlanetaryStreamingTelemetry::default();
        diagnostic_height.desired_l0_height_mode = FarFieldL0HeightMode::CardinalTrimmed8V1;
        assert_eq!(
            qa_report_contract(Some(&diagnostic_height)),
            (
                QA_DIAGNOSTIC_L0_HEIGHT_REPORT_SCHEMA_VERSION,
                QA_DIAGNOSTIC_EVIDENCE_DISPOSITION
            )
        );

        let mut diagnostic_provenance = PlanetaryStreamingTelemetry::default();
        diagnostic_provenance.surface_material_mode = FarFieldSurfaceMaterialMode::LodProvenanceV1;
        assert_eq!(
            qa_report_contract(Some(&diagnostic_provenance)),
            (
                QA_DIAGNOSTIC_LOD_PROVENANCE_REPORT_SCHEMA_VERSION,
                QA_DIAGNOSTIC_LOD_PROVENANCE_EVIDENCE_DISPOSITION
            )
        );

        let mut composite = diagnostic_provenance;
        composite.desired_l0_height_mode = FarFieldL0HeightMode::CardinalTrimmed8V1;
        assert_eq!(
            qa_report_contract(Some(&composite)),
            (
                QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_REPORT_SCHEMA_VERSION,
                QA_DIAGNOSTIC_L0_HEIGHT_LOD_PROVENANCE_EVIDENCE_DISPOSITION
            )
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
            FarFieldCacheUpdate, FarFieldHydroMode, FarFieldL0HeightMode, FarFieldMaterialDetail,
            FarFieldSemanticCohortMode, FarFieldSurfaceMaterialMode,
        };

        let mut telemetry = PlanetaryStreamingTelemetry::default();
        telemetry.desired_terrain_grammar = Some(TerrainGrammarVersion::V3);
        telemetry.active_terrain_grammar = Some(TerrainGrammarVersion::V3);
        telemetry.desired_l0_height_mode = FarFieldL0HeightMode::CardinalTrimmed8V1;
        telemetry.active_l0_height_mode = Some(FarFieldL0HeightMode::CardinalTrimmed8V1);
        telemetry.resident_l0_height_mode = Some(FarFieldL0HeightMode::CardinalTrimmed8V1);
        telemetry.l0_probe_spacing_metres = 8;
        telemetry.budget_l0_height_queries = 12_805;
        telemetry.last_l0_center_queries = 4_225;
        telemetry.last_l0_half_x_queries = 4_290;
        telemetry.last_l0_half_z_queries = 4_290;
        telemetry.last_l0_trimmed_vertices = 512;
        telemetry.last_l0_trimmed_up_vertices = 300;
        telemetry.last_l0_trimmed_down_vertices = 212;
        telemetry.last_l0_max_abs_adjustment_metres = 12.345;
        telemetry.last_l0_cache_update = FarFieldCacheUpdate::TeleportFallback;
        telemetry.last_l0_cache_shift_x_cells = -65;
        telemetry.last_l0_cache_shift_z_cells = 23;
        telemetry.last_l0_reused_height_samples = 0;
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
        telemetry.water_ring_indices = [180, 300, 420, 600, 720, 900];
        telemetry.lava_ring_indices = [120, 300, 480, 600, 780, 900];
        telemetry.resident_water_indices = telemetry.water_ring_indices.iter().sum();
        telemetry.resident_lava_indices = telemetry.lava_ring_indices.iter().sum();
        telemetry.resident_fluid_entities = crate::planetary_streaming::FAR_FIELD_LEVELS;
        telemetry.resident_fluid_mesh_bytes = 100_800;
        telemetry.scheduler_fluid_ring_vertices = telemetry.fluid_ring_vertices;
        telemetry.scheduler_fluid_ring_indices = telemetry.fluid_ring_indices;
        telemetry.scheduler_water_ring_indices = telemetry.water_ring_indices;
        telemetry.scheduler_lava_ring_indices = telemetry.lava_ring_indices;
        telemetry.scheduler_resident_fluid_entities = telemetry.resident_fluid_entities;
        telemetry.scheduler_resident_fluid_vertices = telemetry.resident_fluid_vertices;
        telemetry.scheduler_resident_fluid_indices = telemetry.resident_fluid_indices;
        telemetry.scheduler_resident_water_indices = telemetry.resident_water_indices;
        telemetry.scheduler_resident_lava_indices = telemetry.resident_lava_indices;
        telemetry.scheduler_resident_fluid_mesh_bytes = telemetry.resident_fluid_mesh_bytes;
        telemetry.last_fluid_vertices = 600;
        telemetry.last_fluid_indices = 1_800;
        telemetry.last_water_indices = 900;
        telemetry.last_lava_indices = 900;
        telemetry.semantic_cohort_mode = FarFieldSemanticCohortMode::SilhouettesV1;
        telemetry.resident_semantic_cohort_entities = 1;
        telemetry.resident_semantic_cohort_count = 2;
        telemetry.resident_semantic_cohort_vertices = 48;
        telemetry.resident_semantic_cohort_indices = 72;
        telemetry.resident_semantic_cohort_mesh_bytes = 2_592;
        telemetry.resident_semantic_cohort_kind_counts = [2, 0, 0, 0, 0, 0];
        telemetry.scheduler_resident_semantic_cohort_entities = 1;
        telemetry.scheduler_resident_semantic_cohort_count = 2;
        telemetry.scheduler_resident_semantic_cohort_vertices = 48;
        telemetry.scheduler_resident_semantic_cohort_indices = 72;
        telemetry.scheduler_resident_semantic_cohort_mesh_bytes = 2_592;
        telemetry.scheduler_resident_semantic_cohort_kind_counts = [2, 0, 0, 0, 0, 0];
        telemetry.last_semantic_cohort_hash_scans = 3_721;
        telemetry.last_semantic_cohort_height_queries = 2;
        telemetry.last_semantic_cohort_biome_queries = 2;
        telemetry.last_semantic_cohort_candidates = 2;
        telemetry.last_semantic_cohort_emitted = 2;
        telemetry.last_semantic_cohort_vertices = 48;
        telemetry.last_semantic_cohort_indices = 72;
        telemetry.last_semantic_cohort_kind_counts = [2, 0, 0, 0, 0, 0];
        telemetry.resident_observation_valid = true;
        telemetry.resident_entity_count_overflow = false;
        telemetry.resident_duplicate_levels = 0;
        telemetry.resident_out_of_range_levels = 0;
        telemetry.resident_scheduler_mismatch = false;
        telemetry.resident_budget_exceeded = false;
        telemetry.resident_observation_rejections = 0;
        let snapshot = qa_planetary_streaming(Some(&telemetry)).expect("telemetry snapshot");
        assert_eq!(snapshot.enabled, telemetry.enabled);
        assert_eq!(snapshot.desired_terrain_grammar.as_deref(), Some("V3"));
        assert_eq!(snapshot.active_terrain_grammar.as_deref(), Some("V3"));
        assert_eq!(snapshot.desired_l0_height_mode, "CardinalTrimmed8V1");
        assert_eq!(
            snapshot.active_l0_height_mode.as_deref(),
            Some("CardinalTrimmed8V1")
        );
        assert_eq!(
            snapshot.resident_l0_height_mode.as_deref(),
            Some("CardinalTrimmed8V1")
        );
        assert_eq!(snapshot.l0_probe_spacing_metres, 8);
        assert_eq!(snapshot.budget_l0_height_queries, 12_805);
        assert_eq!(snapshot.last_l0_center_queries, 4_225);
        assert_eq!(snapshot.last_l0_half_x_queries, 4_290);
        assert_eq!(snapshot.last_l0_half_z_queries, 4_290);
        assert_eq!(snapshot.last_l0_trimmed_vertices, 512);
        assert_eq!(snapshot.last_l0_trimmed_up_vertices, 300);
        assert_eq!(snapshot.last_l0_trimmed_down_vertices, 212);
        assert_eq!(snapshot.last_l0_max_abs_adjustment_metres, 12.345);
        assert_eq!(snapshot.last_l0_cache_update, "TeleportFallback");
        assert_eq!(snapshot.last_l0_cache_shift_x_cells, -65);
        assert_eq!(snapshot.last_l0_cache_shift_z_cells, 23);
        assert_eq!(snapshot.last_l0_reused_height_samples, 0);
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
        assert!(snapshot.resident_fluid_kind_integrity_valid);
        assert_eq!(snapshot.resident_water_indices, 3_120);
        assert_eq!(snapshot.resident_lava_indices, 3_180);
        assert_eq!(
            snapshot.resident_water_indices + snapshot.resident_lava_indices,
            snapshot.resident_fluid_indices
        );
        assert_eq!(
            snapshot.scheduler_water_ring_indices,
            snapshot.water_ring_indices
        );
        assert_eq!(
            snapshot.scheduler_lava_ring_indices,
            snapshot.lava_ring_indices
        );
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
        assert_eq!(snapshot.budget_hydro_atomic_ring_build_bytes, 653_008);
        assert_eq!(snapshot.budget_atomic_ring_build_bytes, 757_984);
        assert_eq!(snapshot.semantic_cohort_mode, "SilhouettesV1");
        assert!(snapshot.resident_semantic_cohort_observation_valid);
        assert!(snapshot.resident_semantic_cohort_payload_integrity_valid);
        assert_eq!(snapshot.resident_semantic_cohort_entities, 1);
        assert_eq!(snapshot.resident_semantic_cohort_count, 2);
        assert_eq!(snapshot.resident_semantic_cohort_vertices, 48);
        assert_eq!(snapshot.resident_semantic_cohort_indices, 72);
        assert_eq!(
            snapshot.resident_semantic_cohort_kind_counts,
            [2, 0, 0, 0, 0, 0]
        );
        assert_eq!(snapshot.last_semantic_cohort_hash_scans, 3_721);
        assert_eq!(snapshot.last_semantic_cohort_emitted, 2);
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
    fn dense_chunk_observation_proves_the_combined_resident_and_inflight_cap() {
        assert_eq!(
            qa_dense_chunk_observation(MAX_FULL_CHUNK_RESIDENT - 1, 1),
            (MAX_FULL_CHUNK_RESIDENT, false)
        );
        assert_eq!(
            qa_dense_chunk_observation(MAX_FULL_CHUNK_RESIDENT, 1),
            (MAX_FULL_CHUNK_RESIDENT + 1, true)
        );
        assert_eq!(
            qa_dense_chunk_observation(usize::MAX, 1),
            (usize::MAX, true),
            "arithmetic overflow must remain a visible budget failure"
        );
    }

    #[test]
    fn completion_waits_for_settled_ecs_and_scheduler_truth() {
        let mut telemetry = PlanetaryStreamingTelemetry::default();
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.enabled = true;
        telemetry.desired_terrain_grammar = Some(TerrainGrammarVersion::V3);
        telemetry.active_terrain_grammar = telemetry.desired_terrain_grammar;
        telemetry.active_l0_height_mode = Some(telemetry.desired_l0_height_mode);
        telemetry.resident_l0_height_mode = Some(telemetry.desired_l0_height_mode);
        telemetry.resident_entities = FAR_FIELD_LEVELS;
        telemetry.scheduler_resident_entities = FAR_FIELD_LEVELS;
        telemetry.live_sample_cache_windows = FAR_FIELD_LEVELS;
        telemetry.resident_material_detail = telemetry.desired_material_detail.map(Some);
        telemetry.resident_observation_valid = true;
        telemetry.resident_fluid_observation_valid = true;
        telemetry.resident_fluid_kind_integrity_valid = true;
        telemetry.resident_semantic_cohort_observation_valid = true;
        telemetry.resident_semantic_cohort_payload_integrity_valid = true;
        assert!(qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        assert!(!qa_completion_streaming_settled(
            false,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        assert!(!qa_completion_streaming_settled(
            true,
            1,
            0,
            0,
            Some(&telemetry)
        ));
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            1,
            Some(&telemetry)
        ));
        assert!(!qa_completion_streaming_settled(true, 0, 0, 0, None));

        telemetry.resident_scheduler_mismatch = true;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.resident_scheduler_mismatch = false;
        telemetry.resident_fluid_duplicate_slots = 1;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.resident_fluid_duplicate_slots = 0;
        telemetry.resident_semantic_cohort_budget_exceeded = true;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.resident_semantic_cohort_budget_exceeded = false;
        telemetry.near_coverage_transition_pending = true;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.near_coverage_transition_pending = false;
        telemetry.resident_entities = FAR_FIELD_LEVELS - 1;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
        telemetry.resident_entities = FAR_FIELD_LEVELS;
        telemetry.build_in_flight = true;
        assert!(!qa_completion_streaming_settled(
            true,
            0,
            0,
            0,
            Some(&telemetry)
        ));
    }

    #[test]
    fn completion_requires_settlement_or_the_full_dual_failure_bound() {
        assert_eq!(
            qa_completion_decision(true, 0.0, 0, 0.0, 0),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                true,
                1.0,
                QA_COMPLETION_STABLE_MIN_FRAMES,
                QA_COMPLETION_STABLE_SECONDS - 0.01,
                QA_COMPLETION_STABLE_MIN_FRAMES,
            ),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                true,
                1.0,
                QA_COMPLETION_STABLE_MIN_FRAMES,
                QA_COMPLETION_STABLE_SECONDS,
                QA_COMPLETION_STABLE_MIN_FRAMES - 1,
            ),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                true,
                1.0,
                QA_COMPLETION_STABLE_MIN_FRAMES,
                QA_COMPLETION_STABLE_SECONDS,
                QA_COMPLETION_STABLE_MIN_FRAMES,
            ),
            QaCompletionDecision::Success
        );
        assert_eq!(
            qa_completion_decision(false, 3.0, 12, QA_COMPLETION_STABLE_SECONDS, 100),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                false,
                QA_COMPLETION_SETTLE_TIMEOUT_SECONDS,
                QA_COMPLETION_SETTLE_MIN_FRAMES - 1,
                0.0,
                0,
            ),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                false,
                QA_COMPLETION_SETTLE_TIMEOUT_SECONDS - 0.1,
                QA_COMPLETION_SETTLE_MIN_FRAMES,
                0.0,
                0,
            ),
            QaCompletionDecision::Wait
        );
        assert_eq!(
            qa_completion_decision(
                false,
                QA_COMPLETION_SETTLE_TIMEOUT_SECONDS,
                QA_COMPLETION_SETTLE_MIN_FRAMES,
                0.0,
                0,
            ),
            QaCompletionDecision::TimedOut
        );
        assert_eq!(
            qa_completion_decision(false, f32::NAN, QA_COMPLETION_SETTLE_MIN_FRAMES, 0.0, 0),
            QaCompletionDecision::TimedOut
        );
    }

    #[test]
    fn route_lifecycle_timeout_is_real_time_bounded_and_completion_safe() {
        let duration = 24.0;
        let deadline = duration + QA_ROUTE_LIFECYCLE_RESERVE_SECONDS;
        assert!(!qa_route_lifecycle_timed_out(
            0.0,
            duration,
            deadline - 0.01
        ));
        assert!(qa_route_lifecycle_timed_out(0.0, duration, deadline));
        assert!(qa_route_lifecycle_timed_out(0.0, duration, f32::NAN));
        assert!(!qa_route_lifecycle_timed_out(duration, duration, f32::NAN));
    }

    #[test]
    fn observed_intervals_never_precredit_the_first_snapshot() {
        assert_eq!(qa_observed_interval_elapsed(0, 0.0, 12.0, 30.0), Some(0.0));
        assert_eq!(qa_observed_interval_elapsed(1, 0.0, 0.6, 1.0), Some(0.6));
        assert_eq!(qa_observed_interval_elapsed(2, 0.6, 0.6, 1.0), Some(1.0));
        assert_eq!(qa_observed_interval_elapsed(1, 0.0, f32::NAN, 1.0), None);
    }

    #[test]
    fn completion_frame_bound_covers_two_serial_max_cadence_ring_batches() {
        assert_eq!(FAR_FIELD_LEVELS, 6);
        assert_eq!(FAR_FIELD_MAX_UPDATE_CADENCE_FRAMES, 4);
        assert_eq!(QA_COMPLETION_STABLE_MIN_FRAMES, 9);
        assert_eq!(QA_COMPLETION_TWO_BATCH_MIN_FRAMES, 50);
        assert!(QA_COMPLETION_SETTLE_MIN_FRAMES >= QA_COMPLETION_TWO_BATCH_MIN_FRAMES);
    }

    #[test]
    fn qa_activation_and_cursor_release_share_environment_and_cli_contract() {
        for value in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(qa_requested_from(Some(value), ["voxel-native"]));
        }
        for argument in ["--qa", "--qa-autopilot"] {
            assert!(
                qa_requested_from(None, ["voxel-native", argument]),
                "{argument} must enable both QA and its display-only cursor release"
            );
        }
        for value in ["", "0", "false", "off", "qa", "enabled"] {
            assert!(!qa_requested_from(Some(value), ["voxel-native"]));
        }
        assert!(!qa_requested_from(None, ["voxel-native"]));
        assert!(!qa_requested_from(None, ["voxel-native", "--qa-extra"]));
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
    fn qa_terrain_grammar_parser_is_explicit_and_fail_closed() {
        assert_eq!(
            parse_terrain_grammar(" v1 "),
            Some(TerrainGrammarVersion::V1)
        );
        assert_eq!(
            parse_terrain_grammar("LEGACY"),
            Some(TerrainGrammarVersion::V1)
        );
        assert_eq!(parse_terrain_grammar("v2"), Some(TerrainGrammarVersion::V2));
        assert_eq!(parse_terrain_grammar("2"), Some(TerrainGrammarVersion::V2));
        assert_eq!(parse_terrain_grammar("v3"), Some(TerrainGrammarVersion::V3));
        assert_eq!(parse_terrain_grammar("3"), Some(TerrainGrammarVersion::V3));
        assert_eq!(
            parse_terrain_grammar("current"),
            Some(TerrainGrammarVersion::V3)
        );
        assert_eq!(parse_terrain_grammar("v4"), None);
        assert_eq!(parse_terrain_grammar(""), None);
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
    fn screenshot_observation_records_the_actual_player_pose_and_legacy_path() {
        let transform = Transform::from_translation(Vec3::new(-15.5, 72.25, 4096.0))
            .with_rotation(Quat::from_rotation_y(0.625));
        let path = "qa_runs/run_7/shot_0000_detail.png".to_owned();
        let observation = qa_screenshot_observation(0, path.clone(), 2.5, 10.0, &transform)
            .expect("valid observation");

        assert_eq!(observation.capture_index, 0);
        assert_eq!(observation.screenshot_path, path);
        assert_eq!(observation.scheduled_capture_seconds, 2.5);
        assert_eq!(
            observation.player_camera_translation_metres,
            transform.translation.to_array()
        );
        assert_eq!(
            observation.player_camera_rotation_xyzw,
            transform.rotation.to_array()
        );
        assert!(qa_screenshot_ledger_valid(
            &[path],
            &[observation],
            10.0,
            0,
            false
        ));
    }

    #[test]
    fn screenshot_observation_contract_rejects_unbounded_or_ambiguous_evidence() {
        let transform = Transform::IDENTITY;
        let valid_path = "qa_runs/run_7/shot_0000_detail.png".to_owned();
        assert!(qa_screenshot_observation(
            QA_SCREENSHOT_OBSERVATION_CAP,
            valid_path.clone(),
            2.5,
            10.0,
            &transform
        )
        .is_none());
        assert!(qa_screenshot_observation(
            0,
            "x".repeat(QA_SCREENSHOT_PATH_MAX_CHARS + 1),
            2.5,
            10.0,
            &transform
        )
        .is_none());
        assert!(
            qa_screenshot_observation(0, "qa_é.png".to_owned(), 2.5, 10.0, &transform).is_none()
        );
        assert!(
            qa_screenshot_observation(0, valid_path.clone(), f32::NAN, 10.0, &transform).is_none()
        );

        let unsafe_translation = Transform::from_translation(Vec3::new(
            QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT as f32 + 2.0,
            0.0,
            0.0,
        ));
        assert!(
            qa_screenshot_observation(0, valid_path.clone(), 2.5, 10.0, &unsafe_translation)
                .is_none()
        );
        let non_unit_rotation = Transform {
            rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 0.5),
            ..Transform::IDENTITY
        };
        assert!(qa_screenshot_observation(0, valid_path, 2.5, 10.0, &non_unit_rotation).is_none());
    }

    #[test]
    fn screenshot_ledger_fails_closed_on_mismatch_duplicate_or_rejection() {
        let first_path = "qa_runs/run_7/shot_0000_detail.png".to_owned();
        let second_path = "qa_runs/run_7/shot_0001_context.png".to_owned();
        let first =
            qa_screenshot_observation(0, first_path.clone(), 2.5, 10.0, &Transform::IDENTITY)
                .expect("first");
        let second =
            qa_screenshot_observation(1, second_path.clone(), 5.0, 10.0, &Transform::IDENTITY)
                .expect("second");
        let legacy = vec![first_path.clone(), second_path.clone()];
        let observations = vec![first.clone(), second.clone()];
        assert!(qa_screenshot_ledger_valid(
            &legacy,
            &observations,
            10.0,
            0,
            false
        ));
        assert!(!qa_screenshot_ledger_valid(
            &legacy,
            &observations,
            10.0,
            1,
            false
        ));
        assert!(!qa_screenshot_ledger_valid(
            &legacy,
            &observations,
            10.0,
            0,
            true
        ));
        assert!(!qa_screenshot_ledger_valid(&[], &[], 10.0, 0, false));

        let mut wrong_index = observations.clone();
        wrong_index[1].capture_index = 0;
        assert!(!qa_screenshot_ledger_valid(
            &legacy,
            &wrong_index,
            10.0,
            0,
            false
        ));
        let mut duplicate_path = observations.clone();
        duplicate_path[1].screenshot_path = first_path;
        assert!(!qa_screenshot_ledger_valid(
            &legacy,
            &duplicate_path,
            10.0,
            0,
            false
        ));
        let mut non_monotonic = observations;
        non_monotonic[1].scheduled_capture_seconds = 2.5;
        assert!(!qa_screenshot_ledger_valid(
            &legacy,
            &non_monotonic,
            10.0,
            0,
            false
        ));
    }

    #[test]
    fn screenshot_schedule_and_memory_have_compile_time_caps() {
        let mut next = 2.5;
        let mut captures = 0usize;
        while qa_screenshot_due(600.0, next, 600.0) {
            captures += 1;
            next += 1.0;
        }
        assert_eq!(captures, 598);
        assert!(captures < QA_SCREENSHOT_OBSERVATION_CAP);
        assert!(
            QA_SCREENSHOT_OBSERVATION_CAP
                * (std::mem::size_of::<QaScreenshotObservation>()
                    + std::mem::size_of::<String>()
                    + 2 * QA_SCREENSHOT_PATH_MAX_CHARS)
                <= QA_SCREENSHOT_LEDGER_BYTE_CAP
        );
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
    fn camera_route_variant_bank_is_eight_unique_deterministic_bases() {
        let base = Vec2::new(0.82, 0.57).normalize();
        let first: Vec<_> = (0..QA_CAMERA_ROUTE_VARIANTS as u8)
            .map(|variant| qa_camera_route_variant_basis(base, variant))
            .collect();
        let replay: Vec<_> = (0..QA_CAMERA_ROUTE_VARIANTS as u8)
            .map(|variant| qa_camera_route_variant_basis(base, variant))
            .collect();
        assert_eq!(first, replay);
        for (index, (axis, right)) in first.iter().enumerate() {
            assert!((axis.length() - 1.0).abs() < 0.0001, "variant {index}");
            assert!((right.length() - 1.0).abs() < 0.0001, "variant {index}");
            assert!(axis.dot(*right).abs() < 0.0001, "variant {index}");
        }
        for left in 0..first.len() {
            for right in left + 1..first.len() {
                assert_ne!(first[left], first[right]);
            }
        }
    }

    #[test]
    fn camera_body_treats_foliage_as_visual_occlusion_not_gameplay_air() {
        let mut source = loaded_test_source(-2, 2);
        let leaves = crate::blocks::Voxel::from(crate::blocks::BlockType::Leaves);
        source.voxels.insert((0, 10, 0), leaves);
        assert!(!crate::blocks::BlockType::Leaves.is_solid());
        assert!(qa_camera_voxel_is_visual_body_blocker(leaves));
        let mut budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_body(&source, Vec3::new(0.5, 10.5, 0.5), &mut budget),
            QaCameraProbeResult::BodyOccluded
        );
        assert!(budget.voxel_queries <= QA_CAMERA_BODY_QUERY_CAP_PER_POSE);
        assert_eq!(budget.body_occlusions, 1);
    }

    #[test]
    fn camera_body_and_los_reject_fluid_instead_of_accepting_submerged_routes() {
        let mut source = loaded_test_source(-4, 32);
        source.voxels.insert(
            (0, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Water),
        );
        let mut body_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_body(&source, Vec3::new(0.5, 10.5, 0.5), &mut body_budget),
            QaCameraProbeResult::BodyOccluded
        );

        source.voxels.remove(&(0, 10, 0));
        source.voxels.insert(
            (4, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Lava),
        );
        let mut los_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(8.5, 10.5, 0.5),
                BlockType::Water,
                &mut los_budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );

        let mut endpoint_source = loaded_test_source(-4, 32);
        endpoint_source.voxels.insert(
            (8, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Water),
        );
        let mut endpoint_budget = QaCameraQueryBudget::new();
        assert!(matches!(
            qa_camera_probe_ray(
                &endpoint_source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(8.5, 10.5, 0.5),
                BlockType::Water,
                &mut endpoint_budget,
            ),
            QaCameraProbeResult::Clear { .. }
        ));

        endpoint_source.voxels.insert(
            (8, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Stone),
        );
        let mut solid_endpoint_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &endpoint_source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(8.5, 10.5, 0.5),
                BlockType::Water,
                &mut solid_endpoint_budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );

        endpoint_source.voxels.insert(
            (8, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Lava),
        );
        let mut wrong_fluid_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &endpoint_source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(8.5, 10.5, 0.5),
                BlockType::Water,
                &mut wrong_fluid_budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );

        endpoint_source.voxels.insert(
            (8, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Leaves),
        );
        let mut foliage_endpoint_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &endpoint_source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(8.5, 10.5, 0.5),
                BlockType::Water,
                &mut foliage_endpoint_budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );
    }

    #[test]
    fn camera_los_supercover_visits_edge_and_corner_touching_voxels() {
        let mut source = TestVoxelSource {
            all_chunks_loaded: true,
            ..Default::default()
        };
        // The diagonal crosses the X/Y/Z boundary simultaneously. A uniform
        // point sampler sees only (1,11,1); conservative supercover must also
        // visit the edge-touching (1,10,0) cell.
        source.voxels.insert(
            (1, 10, 0),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Stone),
        );
        let mut budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(3.5, 13.5, 3.5),
                BlockType::Water,
                &mut budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );
        assert!(budget.voxel_queries <= QA_CAMERA_LOS_QUERY_CAP_PER_RAY);
    }

    #[test]
    fn camera_probe_fails_closed_for_unloaded_voxel_chunks() {
        let source = TestVoxelSource::default();
        let mut budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_body(&source, Vec3::new(-0.5, 10.5, -0.5), &mut budget),
            QaCameraProbeResult::ChunksUnloaded
        );
        assert_eq!(budget.voxel_queries, 1);
        assert_eq!(budget.unloaded_chunk_checks, 1);
        assert_eq!(budget.loaded_chunk_checks, 0);
    }

    #[test]
    fn camera_probe_counts_proven_air_without_claiming_chunk_residency() {
        let mut source = TestVoxelSource::default();
        source.proven_air_voxels.insert((3, 80, -4));
        let mut budget = QaCameraQueryBudget::new();
        assert_eq!(budget.query(&source, 3, 80, -4), Ok(crate::blocks::AIR));
        assert_eq!(budget.voxel_queries, 1);
        assert_eq!(budget.required_chunk_checks, 1);
        assert_eq!(budget.loaded_chunk_checks, 0);
        assert_eq!(budget.proven_air_chunk_checks, 1);
        assert_eq!(budget.unloaded_chunk_checks, 0);
    }

    #[test]
    fn camera_world_source_binds_proven_air_to_the_current_request_set() {
        let mut world = crate::world::VoxelWorld::new();
        let mut streamer = crate::world::ChunkStreamer::default();
        let (pos, _, _, _) = crate::chunk::world_to_chunk(3, 80, -4);
        world.column_top_cy.insert((pos.x, pos.z), pos.y - 1);

        let source = QaWorldVoxelSource {
            world: &world,
            streamer: &streamer,
        };
        assert_eq!(
            source.resolve_voxel(3, 80, -4),
            QaCameraVoxelResolution::Unavailable
        );

        streamer.requested_chunks.insert(pos);
        let source = QaWorldVoxelSource {
            world: &world,
            streamer: &streamer,
        };
        assert_eq!(
            source.resolve_voxel(3, 80, -4),
            QaCameraVoxelResolution::ProvenAir
        );
    }

    #[test]
    fn river_and_lava_preflight_variants_fit_the_anchor_staging_disc() {
        let staging = qa_camera_preflight_staging_sample();
        assert_eq!(staging.camera_offset, Vec2::ZERO);
        let base = Vec2::new(0.82, 0.57).normalize();
        for focus in [QaFocus::River, QaFocus::Lava] {
            for variant in 0..QA_CAMERA_ROUTE_VARIANTS as u8 {
                for progress in QA_CAMERA_ROUTE_PROGRESS_SAMPLES {
                    let sample = qa_hydro_route_sample_for_variant(
                        progress, focus, 64, 2_000.0, base, variant,
                    );
                    assert!(sample.camera_offset.length() <= 92.0);
                    assert!(sample.camera_height <= 38.0);
                }
            }
        }
    }

    #[test]
    fn camera_los_has_a_fixed_cell_cap_and_foliage_threshold() {
        let mut source = loaded_test_source(-4, 400);
        for x in 3..=5 {
            source.voxels.insert(
                (x, 10, 0),
                crate::blocks::Voxel::from(crate::blocks::BlockType::Leaves),
            );
        }
        let mut budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(12.5, 10.5, 0.5),
                BlockType::Water,
                &mut budget,
            ),
            QaCameraProbeResult::LineOfSightOccluded
        );
        assert_eq!(budget.los_occlusions, 1);

        let capped_source = TestVoxelSource {
            all_chunks_loaded: true,
            ..Default::default()
        };
        let mut capped_budget = QaCameraQueryBudget::new();
        assert_eq!(
            qa_camera_probe_ray(
                &capped_source,
                Vec3::new(0.5, 10.5, 0.5),
                Vec3::new(QA_CAMERA_LOS_QUERY_CAP_PER_RAY as f32 + 1.5, 10.5, 0.5,),
                BlockType::Water,
                &mut capped_budget,
            ),
            QaCameraProbeResult::WorkCap
        );
        assert_eq!(capped_budget.voxel_queries, QA_CAMERA_LOS_QUERY_CAP_PER_RAY);
    }

    #[test]
    fn camera_coordinate_conversion_rejects_non_finite_and_unsafe_f32_world_positions() {
        assert_eq!(
            qa_camera_pose_voxel(Vec3::new(-15.25, 63.5, -31.75)),
            Some([-16, 63, -32])
        );
        assert!(qa_camera_pose_voxel(Vec3::new(f32::NAN, 0.0, 0.0)).is_none());
        assert!(qa_camera_pose_voxel(Vec3::new(
            QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT as f32 + 2.0,
            0.0,
            0.0,
        ))
        .is_none());
        assert!(qa_camera_pose_voxel(Vec3::new(
            -(QA_CAMERA_ROUTE_SAFE_INTEGER_LIMIT as f32 + 2.0),
            0.0,
            0.0,
        ))
        .is_none());
    }

    #[test]
    fn available_preflight_proves_selected_route_even_when_a_candidate_is_occluded() {
        let world = crate::world::VoxelWorld::new();
        let anchor = QaFocusAnchor {
            world_x: -32,
            fluid_y: 48,
            world_z: 32,
        };
        let origin = anchor.render_origin();
        let base_axis =
            super::qa_camera_route_base_axis(&world.generator, 12_345, QaFocus::River, anchor);
        let first_sample =
            qa_hydro_route_sample_for_variant(0.0, QaFocus::River, 64, 256.0, base_axis, 0);
        let (first_position, _) = super::qa_world_pose(&world, origin, first_sample);
        let [block_x, block_y, block_z] = qa_camera_pose_voxel(first_position).expect("safe pose");
        let mut source = TestVoxelSource {
            all_chunks_loaded: true,
            ..Default::default()
        };
        source.voxels.insert(
            (block_x, block_y, block_z),
            crate::blocks::Voxel::from(crate::blocks::BlockType::Stone),
        );

        let (plan, reason, validation) = qa_camera_route_preflight(
            &source,
            &world,
            origin,
            QaFocus::River,
            anchor,
            12_345,
            WorldProfile::Natural,
            SceneryQuality::Lush,
            64,
            256.0,
        );

        assert!(plan.is_some());
        assert_eq!(reason, None);
        assert_eq!(validation.validation_samples, 16);
        assert_eq!(validation.selected_clear_samples, 16);
        assert!(validation.candidate_body_occlusions >= 1);
        assert!(validation.candidate_body_occlusions <= 128);
        assert!(validation.candidate_los_occlusions <= 128);
        assert_eq!(validation.required_chunk_checks, validation.voxel_queries);
        assert_eq!(validation.loaded_chunk_checks, validation.voxel_queries);
        assert_eq!(validation.unloaded_chunk_checks, 0);
        assert!(!validation.work_cap_exhausted);
    }

    #[test]
    fn camera_plan_identity_is_stable_and_changes_with_route_truth() {
        let anchor = QaFocusAnchor {
            world_x: -32,
            fluid_y: 48,
            world_z: 32,
        };
        let plan = qa_camera_plan_hash(
            12_345,
            WorldProfile::Natural,
            SceneryQuality::Lush,
            QaFocus::River,
            anchor,
            3,
        );
        assert_eq!(
            plan,
            qa_camera_plan_hash(
                12_345,
                WorldProfile::Natural,
                SceneryQuality::Lush,
                QaFocus::River,
                anchor,
                3,
            )
        );
        assert_ne!(
            plan,
            qa_camera_plan_hash(
                12_345,
                WorldProfile::Natural,
                SceneryQuality::Lush,
                QaFocus::River,
                anchor,
                4,
            )
        );
        assert_ne!(
            plan,
            qa_camera_plan_hash(
                12_345,
                WorldProfile::AstralFrontier,
                SceneryQuality::Lush,
                QaFocus::Lava,
                anchor,
                3,
            )
        );
    }

    #[test]
    fn near_far_variants_keep_all_samples_on_both_sides_of_the_seam() {
        let seam = 96.0;
        let base = Vec2::new(0.42, -0.91).normalize();
        for variant in 0..QA_CAMERA_ROUTE_VARIANTS as u8 {
            let (axis, _) = qa_camera_route_variant_basis(base, variant);
            let projections: Vec<_> = QA_CAMERA_ROUTE_PROGRESS_SAMPLES
                .into_iter()
                .map(|progress| {
                    qa_hydro_route_sample_for_variant(
                        progress,
                        QaFocus::NearFar,
                        seam as i64,
                        512.0,
                        base,
                        variant,
                    )
                    .camera_offset
                    .dot(axis)
                })
                .collect();
            assert!(projections.iter().any(|distance| *distance < seam));
            assert!(projections.iter().any(|distance| *distance > seam));
        }
    }

    #[test]
    fn camera_route_compile_time_work_contract_is_exact() {
        assert_eq!(QA_CAMERA_ROUTE_VALIDATION_SAMPLES, 16);
        assert_eq!(QA_CAMERA_ROUTE_VARIANTS, 8);
        assert_eq!(QA_CAMERA_ROUTE_VOXEL_QUERY_CAP, 153_600);
        assert_eq!(QA_CAMERA_ROUTE_PROGRESS_SAMPLES[0], 0.0);
        assert_eq!(QA_CAMERA_ROUTE_PROGRESS_SAMPLES[15], 1.0);
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
        let natural = TerrainGenerator::new(7).with_world_profile(WorldProfile::Natural);
        let astral = TerrainGenerator::new(7).with_world_profile(WorldProfile::AstralFrontier);
        let natural_signature = Some(qa_generator_signature(&natural));
        let astral_signature = Some(qa_generator_signature(&astral));
        assert!(!qa_profile_anchor_ready(
            7,
            WorldProfile::AstralFrontier,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V3,
            astral_signature,
            &natural,
        ));
        assert!(qa_profile_anchor_ready(
            7,
            WorldProfile::AstralFrontier,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V3,
            astral_signature,
            &astral,
        ));
        assert!(qa_profile_anchor_ready(
            7,
            WorldProfile::Natural,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V3,
            natural_signature,
            &natural,
        ));
        assert!(!qa_profile_anchor_ready(
            8,
            WorldProfile::Natural,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V3,
            natural_signature,
            &natural,
        ));
        assert!(!qa_profile_anchor_ready(
            7,
            WorldProfile::Natural,
            SceneryQuality::Lush,
            TerrainGrammarVersion::V3,
            natural_signature,
            &natural,
        ));

        let lush_astral = TerrainGenerator::new(7)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Lush);
        let lush_astral_signature = Some(qa_generator_signature(&lush_astral));
        assert!(qa_profile_anchor_ready(
            7,
            WorldProfile::AstralFrontier,
            SceneryQuality::Lush,
            TerrainGrammarVersion::V3,
            lush_astral_signature,
            &lush_astral,
        ));
        assert!(!qa_profile_anchor_ready(
            7,
            WorldProfile::AstralFrontier,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V3,
            lush_astral_signature,
            &lush_astral,
        ));

        assert!(!qa_profile_anchor_ready(
            7,
            WorldProfile::Natural,
            SceneryQuality::Balanced,
            TerrainGrammarVersion::V1,
            natural_signature,
            &natural,
        ));
    }

    #[test]
    fn near_far_requires_settled_near_far_and_profile_specific_hydro() {
        let mut telemetry = PlanetaryStreamingTelemetry::default();
        telemetry.enabled = true;
        telemetry.profile = WorldProfile::Natural;
        telemetry.confirmed_near_extent_metres = 96;
        telemetry.near_coverage_ready_columns = 4;
        telemetry.resident_entities = FAR_FIELD_LEVELS;
        telemetry.ring_vertices[0] = 100;
        telemetry.resident_water_indices = 6;
        assert!(qa_near_far_route_available(
            &telemetry,
            WorldProfile::Natural,
            180.0
        ));

        telemetry.dirty_mask = 1;
        assert!(!qa_near_far_route_available(
            &telemetry,
            WorldProfile::Natural,
            180.0
        ));
        telemetry.dirty_mask = 0;
        telemetry.resident_water_indices = 0;
        assert!(!qa_near_far_route_available(
            &telemetry,
            WorldProfile::Natural,
            180.0
        ));
        telemetry.profile = WorldProfile::AstralFrontier;
        telemetry.resident_lava_indices = 6;
        assert!(qa_near_far_route_available(
            &telemetry,
            WorldProfile::AstralFrontier,
            180.0
        ));
        telemetry.build_in_flight = true;
        assert!(!qa_near_far_route_available(
            &telemetry,
            WorldProfile::AstralFrontier,
            180.0
        ));
    }

    #[test]
    fn unsupported_lava_focus_never_fabricates_observed_or_exhausted_work() {
        let natural = TerrainGenerator::new(7).with_world_profile(WorldProfile::Natural);
        let result = qa_find_hydro_focus(&natural, QaFocus::Lava, WorldProfile::Natural);
        assert_eq!(result.anchor, None);
        assert_eq!(result.visited_candidates, 0);
        assert_eq!(result.classification_queries, 0);
        assert_eq!(result.candidate_cap, 0);
        assert_eq!(result.classification_query_cap, 0);
        assert!(!qa_focus_has_exact_search_work(
            QaFocus::Lava,
            WorldProfile::Natural
        ));
        assert!(!qa_focus_search_exhausted(result));
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
