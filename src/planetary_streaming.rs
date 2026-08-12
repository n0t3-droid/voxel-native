//! Constant-budget, kilometres-visible terrain horizon.
//!
//! Full voxel chunks remain the interaction/simulation representation. This
//! module adds a render-only height-field clipmap outside that near bubble:
//! one finest parent grid plus five square annuli, one mesh entity per level,
//! no colliders, and no world-cell simulation. The parent stays complete where
//! fast travel outruns detailed chunk meshing; a fixed irregular stencil cuts
//! out only cells with proven current near-mesh coverage. Camera travel changes
//! sample coordinates but cannot grow the entity, vertex, index, task, stencil,
//! or request-queue budgets.
//!
//! The unusual part is deliberate: every ring uses integer world anchors and
//! local `f32` vertices. A future floating-origin system can change
//! [`PlanetaryRenderOrigin`] without rebuilding height data or putting huge
//! global coordinates in GPU vertex buffers.

use bevy::ecs::schedule::apply_deferred;
use bevy::pbr::{NotShadowCaster, NotShadowReceiver};
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::Face;
#[cfg(not(target_arch = "wasm32"))]
use bevy::tasks::{AsyncComputeTaskPool, Task};
#[cfg(not(target_arch = "wasm32"))]
use futures_lite::future;
use std::mem::size_of;
use std::time::Instant;

use crate::blocks::BlockType;
use crate::chunk::{ChunkPos, CHUNK_SIZE_I};
use crate::settings::{
    SceneryQuality, WorldProfile, WorldSettings, SAFE_MAX_VERTICAL_CHUNKS, SAFE_MIN_VERTICAL_CHUNKS,
};
use crate::terrain::{coarse_surface_family, Biome, TerrainGenerator, WATER_LEVEL};
use crate::world::{ChunkAnchor, ChunkStreamer, StreamingGovernor, VoxelWorld, WorldSet};

pub const FAR_FIELD_LEVELS: usize = 6;
pub const FAR_FIELD_GRID_CELLS: i32 = 60;
pub const FAR_FIELD_GRID_VERTICES: i32 = FAR_FIELD_GRID_CELLS + 1;
pub const FAR_FIELD_BASE_STEP_METRES: i64 = 32;
pub const FAR_FIELD_MAX_ENTITIES: usize = FAR_FIELD_LEVELS;
/// The observer inspects the six admissible entities plus one sentinel. The
/// sentinel proves an over-populated ECS without allowing a duplicate-spawn
/// bug to turn telemetry into unbounded per-frame work.
pub const FAR_FIELD_OBSERVATION_SCAN_LIMIT: usize = FAR_FIELD_MAX_ENTITIES + 1;
pub const FAR_FIELD_MAX_VERTICES: usize = 35_000;
pub const FAR_FIELD_MAX_INDICES: usize = 150_000;
pub const FAR_FIELD_MAX_BUILDS_IN_FLIGHT: usize = 1;
pub const FAR_FIELD_MAX_RING_VERTICES: usize = 6_000;
pub const FAR_FIELD_MAX_RING_INDICES: usize = 25_000;
/// Exact generated attribute/index payload (allocator and render-asset
/// bookkeeping are deliberately excluded and documented separately).
pub const FAR_FIELD_MAX_MESH_BYTES: usize =
    FAR_FIELD_MAX_VERTICES * (3 + 3 + 4 + 2) * size_of::<f32>()
        + FAR_FIELD_MAX_INDICES * size_of::<u32>();
pub const FAR_FIELD_MAX_RING_BUILD_BYTES: usize =
    FAR_FIELD_MAX_RING_VERTICES * (3 + 3 + 4 + 2) * size_of::<f32>()
        + FAR_FIELD_MAX_RING_INDICES * size_of::<u32>();
/// Hydrographic Continuity v1 adds at most one combined water/lava mesh per
/// LOD. The mesh reuses the 61x61 terrain lattice and emits top faces only;
/// water and lava remain vertex-colour categories inside one draw entity.
pub const FAR_FIELD_MAX_FLUID_ENTITIES: usize = FAR_FIELD_LEVELS;
pub const FAR_FIELD_MAX_RENDER_ENTITIES: usize =
    FAR_FIELD_MAX_ENTITIES + FAR_FIELD_MAX_FLUID_ENTITIES;
pub const FAR_FIELD_FLUID_OBSERVATION_SCAN_LIMIT: usize = FAR_FIELD_MAX_FLUID_ENTITIES + 1;
pub const FAR_FIELD_MAX_FLUID_VERTICES_PER_RING: usize =
    FAR_FIELD_GRID_VERTICES as usize * FAR_FIELD_GRID_VERTICES as usize;
pub const FAR_FIELD_MAX_FLUID_INDICES_PER_RING: usize =
    FAR_FIELD_GRID_CELLS as usize * FAR_FIELD_GRID_CELLS as usize * 6;
pub const FAR_FIELD_MAX_FLUID_VERTICES: usize =
    FAR_FIELD_MAX_FLUID_VERTICES_PER_RING * FAR_FIELD_LEVELS;
pub const FAR_FIELD_MAX_FLUID_INDICES: usize =
    FAR_FIELD_MAX_FLUID_INDICES_PER_RING * FAR_FIELD_LEVELS;
pub const FAR_FIELD_MAX_FLUID_MESH_BYTES: usize =
    FAR_FIELD_MAX_FLUID_VERTICES * (3 + 3 + 4 + 2) * size_of::<f32>()
        + FAR_FIELD_MAX_FLUID_INDICES * size_of::<u32>();
pub const FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES: usize =
    FAR_FIELD_MAX_FLUID_VERTICES_PER_RING * (3 + 3 + 4 + 2) * size_of::<f32>()
        + FAR_FIELD_MAX_FLUID_INDICES_PER_RING * size_of::<u32>();
pub const FAR_FIELD_MAX_ATOMIC_RING_BUILD_BYTES: usize =
    FAR_FIELD_MAX_RING_BUILD_BYTES + FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES;
/// One exact cached classification per lattice vertex. Potentially wet
/// vertices issue at most one additional exact biome query; dry high ground
/// needs no biome query.
pub const FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING: usize =
    FAR_FIELD_MAX_FLUID_VERTICES_PER_RING;
pub const FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING: usize = FAR_FIELD_MAX_FLUID_VERTICES_PER_RING;
const _: () = assert!(FAR_FIELD_MAX_RENDER_ENTITIES == 12);
const _: () = assert!(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING == 3_721);
const _: () = assert!(FAR_FIELD_MAX_FLUID_VERTICES == 22_326);
const _: () = assert!(FAR_FIELD_MAX_FLUID_INDICES == 129_600);
const _: () = assert!(FAR_FIELD_MAX_FLUID_MESH_BYTES == 1_590_048);
pub const FAR_FIELD_SAMPLE_HALO_CELLS: i32 = 2;
pub const FAR_FIELD_SAMPLE_CACHE_SIDE: usize =
    FAR_FIELD_GRID_VERTICES as usize + FAR_FIELD_SAMPLE_HALO_CELLS as usize * 2;
pub const FAR_FIELD_SAMPLE_CACHE_CELLS: usize =
    FAR_FIELD_SAMPLE_CACHE_SIDE * FAR_FIELD_SAMPLE_CACHE_SIDE;
/// Both material bridges evaluate one categorical surface family at each
/// visible top vertex on an absolute integer-world lattice. Bridge-v1 also
/// issues four one-metre slope-height queries per vertex; bridge-v2 assigns
/// vertices to fixed 128 m material cells and selects each cell biome's
/// canonical base family, issuing no slope queries. Skirts copy their parent
/// vertex colour. These are hard per-job CPU query ceilings, not adaptive
/// targets.
pub const FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING: usize =
    FAR_FIELD_GRID_VERTICES as usize * FAR_FIELD_GRID_VERTICES as usize;
pub const FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING: usize =
    FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING * 4;
/// Bridge-v2 assigns every vertex to this absolute, Euclidean world-space
/// material cell. A fixed cell is independent of clipmap level and anchor;
/// adjacent vertices may therefore reuse one category without a map or heap.
pub const FAR_FIELD_BRIDGE_V2_MATERIAL_CELL_METRES: i64 = 128;
pub const FAR_FIELD_MAX_BRIDGE_V2_CELL_REUSES_PER_RING: usize =
    FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING - 1;
/// Exactly six windows exist across resident storage and the sole worker. The
/// rebuilding level moves into the worker instead of being cloned, and even an
/// incompatible retarget refills those arrays in place rather than allocating
/// a defensive seventh window.
pub const FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS: usize = FAR_FIELD_LEVELS;
pub const FAR_FIELD_MAX_SAMPLE_CACHE_BYTES: usize = 512 * 1024;
pub const FAR_FIELD_OUTER_RADIUS_METRES: i64 =
    (FAR_FIELD_GRID_CELLS as i64 / 2) * (FAR_FIELD_BASE_STEP_METRES << (FAR_FIELD_LEVELS - 1));
/// The finest clipmap is a complete fallback parent. Full-voxel terrain is
/// rendered over it; an inner hole would expose the sky whenever fast travel
/// temporarily outruns detailed chunk meshing.
pub const FAR_FIELD_FINEST_INNER_EXTENT_METRES: i64 = 0;
const FAR_FIELD_COVERAGE_CELLS: usize =
    FAR_FIELD_GRID_CELLS as usize * FAR_FIELD_GRID_CELLS as usize;
const FAR_FIELD_COVERAGE_WORDS: usize = FAR_FIELD_COVERAGE_CELLS.div_ceil(64);
const NEAR_COVERAGE_SIDE: usize = crate::world::MAX_INTERACTION_RADIUS_CHUNKS as usize * 2 + 1;
const NEAR_COVERAGE_COLUMNS: usize = NEAR_COVERAGE_SIDE * NEAR_COVERAGE_SIDE;
pub const FAR_FIELD_MAX_COVERAGE_WORK_BYTES: usize =
    NEAR_COVERAGE_COLUMNS * size_of::<bool>() + FAR_FIELD_COVERAGE_WORDS * size_of::<u64>();
const _: () = assert!(FAR_FIELD_MAX_COVERAGE_WORK_BYTES <= 2 * 1024);
/// Newly proven coverage may wait briefly behind the always-present parent so
/// dozens of independently finishing chunk meshes collapse into one rebuild.
/// Coverage loss bypasses this window immediately and can therefore never
/// expose a sky hole.
const FAR_FIELD_COVERAGE_STABILITY_SECONDS: f32 = 0.5;
/// Entering and leaving reduced material detail use distinct dimensionless
/// pressure thresholds. The three-point band around the old 0.55 boundary
/// prevents deterministic frame-to-frame rebuild thrash without changing the
/// geometry cadence or outer extent.
const FAR_FIELD_REDUCED_DETAIL_ENTER_PRESSURE: f32 = 0.58;
const FAR_FIELD_REDUCED_DETAIL_EXIT_PRESSURE: f32 = 0.52;
/// Near terrain classifies the exposed surface from one-block cardinal rise.
/// Reusing that absolute one-metre quantum makes a shared world vertex choose
/// the same family at every far-field LOD and after anchor retargets.
const FAR_FIELD_MATERIAL_SLOPE_QUANTUM_METRES: i64 = 1;

const FULL_DIRTY_MASK: u8 = (1_u8 << FAR_FIELD_LEVELS) - 1;
const TOP_SURFACE_OFFSET: f32 = 0.94;
const LEVEL_DEPTH_BIAS: f32 = 0.12;
/// Near voxels place the visible top of the block at `y + 1`. The far field
/// already uses a 0.94 m top offset to avoid coincident surfaces; fluids share
/// that convention. A 0.02 m per-LOD bias prevents coplanar overlap in the
/// clipmap morph band while remaining far below a one-metre voxel step.
const FAR_FIELD_FLUID_TOP_OFFSET_METRES: f32 = TOP_SURFACE_OFFSET;
const FAR_FIELD_FLUID_LEVEL_DEPTH_BIAS_METRES: f32 = 0.02;
/// Canonical near-generation lava fill ceiling in VolcanicWaste columns.
/// This is an authored world rule (metres/voxel Y), not a physical claim.
const FAR_FIELD_VOLCANIC_LAVA_LEVEL: i32 = 52;

pub struct PlanetaryStreamingPlugin;

impl Plugin for PlanetaryStreamingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlanetaryStreamingConfig>()
            .init_resource::<PlanetaryRenderOrigin>()
            .init_resource::<PlanetaryStreamingTelemetry>()
            .init_resource::<PlanetaryStreamingRuntime>()
            .add_systems(
                Update,
                (
                    update_planetary_streaming,
                    apply_deferred,
                    observe_planetary_residency,
                )
                    .chain()
                    .after(WorldSet::Stream)
                    .run_if(in_state(crate::menu::GameState::InGame)),
            )
            .add_systems(
                OnExit(crate::menu::GameState::InGame),
                (
                    teardown_planetary_streaming,
                    apply_deferred,
                    observe_planetary_residency,
                )
                    .chain(),
            );
    }
}

/// Reversible rollout gate. Natural worlds remain byte-for-byte visually
/// unchanged by default until the new horizon has completed visual QA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FarFieldProfileGate {
    Disabled,
    AstralOnly,
    All,
}

/// CPU-side palette sampling tier. Geometry/height fidelity and the outer
/// extent never change under pressure; only the optional biome colour query
/// is skipped for newly rebuilt rings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FarFieldMaterialDetail {
    Detailed,
    Reduced,
}

/// Non-persisted A/B gate for far-surface material convergence.
///
/// This participates in [`FarFieldWorldKey`], so flipping the environment
/// rollback cannot reuse meshes or sample windows authored under another
/// material interpretation. It never enters a save file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FarFieldSurfaceMaterialMode {
    LegacyPalette,
    /// Exact near-terrain one-metre slope classification. Retained as the
    /// expensive diagnostic/reference path.
    BridgeV1,
    /// Fast canonical base-family classification. This is the shipping
    /// default: absolute-coordinate stable, categorical, and slope-query free.
    BridgeV2,
}

/// Non-persisted, reversible rollout gate for render-only far hydrography.
/// It participates in the world/cache identity, so an off/on transition can
/// never reinterpret an old cache or publish a stale fluid result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FarFieldHydroMode {
    Disabled,
    DescriptiveV1,
}

impl FarFieldHydroMode {
    fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("0" | "off" | "false" | "disabled" | "none") => Self::Disabled,
            Some("1" | "on" | "true" | "v1" | "descriptive-v1" | "descriptive_v1") | None => {
                Self::DescriptiveV1
            }
            Some(_) => Self::DescriptiveV1,
        }
    }
}

impl FarFieldSurfaceMaterialMode {
    fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("0" | "off" | "legacy" | "legacy-palette" | "legacy_palette") => {
                Self::LegacyPalette
            }
            Some("bridge-v1" | "bridge_v1" | "v1" | "exact" | "exact-slope") => Self::BridgeV1,
            Some("1" | "on" | "bridge" | "bridge-v2" | "bridge_v2" | "v2" | "fast") | None => {
                Self::BridgeV2
            }
            Some(_) => Self::BridgeV2,
        }
    }
}

/// Why the fixed sample window needed procedural terrain queries.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FarFieldCacheUpdate {
    #[default]
    Cold,
    IncrementalStrip,
    TeleportFallback,
    IncompatibleFallback,
}

impl FarFieldProfileGate {
    fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("0" | "off" | "false" | "disabled" | "none") => Self::Disabled,
            Some("1" | "on" | "true" | "all" | "both" | "natural") => Self::All,
            Some("astral" | "astral-only" | "astral_only") | None => Self::AstralOnly,
            Some(_) => Self::AstralOnly,
        }
    }

    pub const fn allows(self, profile: WorldProfile) -> bool {
        match self {
            Self::Disabled => false,
            Self::AstralOnly => matches!(profile, WorldProfile::AstralFrontier),
            Self::All => true,
        }
    }
}

#[derive(Resource, Debug, Clone, Copy)]
pub struct PlanetaryStreamingConfig {
    pub profile_gate: FarFieldProfileGate,
    pub surface_material_mode: FarFieldSurfaceMaterialMode,
    pub hydro_mode: FarFieldHydroMode,
}

impl Default for PlanetaryStreamingConfig {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let profile_gate = FarFieldProfileGate::from_env_value(
            std::env::var("VOXEL_NATIVE_PLANETARY_STREAMING")
                .ok()
                .as_deref(),
        );
        #[cfg(target_arch = "wasm32")]
        let profile_gate = FarFieldProfileGate::AstralOnly;

        #[cfg(not(target_arch = "wasm32"))]
        let surface_material_mode = FarFieldSurfaceMaterialMode::from_env_value(
            std::env::var("VOXEL_NATIVE_FAR_SURFACE_MATERIAL")
                .ok()
                .as_deref(),
        );
        #[cfg(target_arch = "wasm32")]
        let surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV2;

        #[cfg(not(target_arch = "wasm32"))]
        let hydro_mode = FarFieldHydroMode::from_env_value(
            std::env::var("VOXEL_NATIVE_FAR_HYDROGRAPHY")
                .ok()
                .as_deref(),
        );
        #[cfg(target_arch = "wasm32")]
        let hydro_mode = FarFieldHydroMode::DescriptiveV1;

        Self {
            profile_gate,
            surface_material_mode,
            hydro_mode,
        }
    }
}

/// Absolute X/Z coordinate represented by render-space `(0, 0)`.
///
/// It is zero in the current engine. Keeping this contract inside Phase 1
/// makes the far field compatible with a later floating-origin shift without
/// changing mesh topology or reinterpreting saved world coordinates.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlanetaryRenderOrigin {
    pub world_x: i64,
    pub world_z: i64,
}

/// Public bounded-work evidence for Agent Control, QA, and future dashboards.
#[derive(Resource, Debug, Clone)]
pub struct PlanetaryStreamingTelemetry {
    pub enabled: bool,
    pub profile: WorldProfile,
    pub interaction_radius_metres: i64,
    pub confirmed_near_extent_metres: i64,
    pub near_coverage_ready_columns: usize,
    pub near_coverage_hidden_cells: usize,
    pub far_radius_metres: i64,
    /// Post-deferred ECS truth. In a structurally valid observation this is
    /// the exact number of [`FarFieldRing`] entities. When
    /// [`Self::resident_entity_count_overflow`] is true, the fixed value
    /// [`FAR_FIELD_OBSERVATION_SCAN_LIMIT`] is a fail-closed lower bound.
    pub resident_entities: usize,
    pub resident_vertices: usize,
    pub resident_indices: usize,
    pub resident_mesh_bytes: usize,
    /// Post-deferred render-only hydrographic residency. These counters are
    /// separate from terrain so the established six-ring truth contract does
    /// not silently change meaning during rollout.
    pub resident_fluid_entities: usize,
    pub resident_fluid_vertices: usize,
    pub resident_fluid_indices: usize,
    pub resident_fluid_mesh_bytes: usize,
    /// Scheduler/runtime bookkeeping is intentionally separate from observed
    /// ECS residency. A disagreement is evidence, never silently relabelled as
    /// a completed install.
    pub scheduler_resident_entities: usize,
    pub scheduler_resident_vertices: usize,
    pub scheduler_resident_indices: usize,
    pub scheduler_resident_mesh_bytes: usize,
    pub scheduler_resident_fluid_entities: usize,
    pub scheduler_resident_fluid_vertices: usize,
    pub scheduler_resident_fluid_indices: usize,
    pub scheduler_resident_fluid_mesh_bytes: usize,
    /// Frame-boundary population across resident slots and the single
    /// in-flight ownership slot. In-place incompatible refill guarantees that
    /// the worker has no unreported temporary window.
    pub live_sample_cache_windows: usize,
    pub live_sample_cache_bytes: usize,
    pub peak_live_sample_cache_windows: usize,
    pub peak_live_sample_cache_bytes: usize,
    pub budget_entities: usize,
    pub budget_vertices: usize,
    pub budget_indices: usize,
    pub budget_mesh_bytes: usize,
    pub budget_build_jobs: usize,
    pub budget_ring_build_bytes: usize,
    pub budget_sample_cache_bytes: usize,
    pub budget_coverage_work_bytes: usize,
    pub budget_fluid_entities: usize,
    pub budget_fluid_vertices: usize,
    pub budget_fluid_indices: usize,
    pub budget_fluid_mesh_bytes: usize,
    pub budget_fluid_ring_build_bytes: usize,
    pub budget_atomic_ring_build_bytes: usize,
    /// Post-deferred ECS population grouped by LOD. Duplicate LODs are summed
    /// deterministically and also invalidate the observation.
    pub ring_vertices: [usize; FAR_FIELD_LEVELS],
    pub ring_indices: [usize; FAR_FIELD_LEVELS],
    pub scheduler_ring_vertices: [usize; FAR_FIELD_LEVELS],
    pub scheduler_ring_indices: [usize; FAR_FIELD_LEVELS],
    pub fluid_ring_vertices: [usize; FAR_FIELD_LEVELS],
    pub fluid_ring_indices: [usize; FAR_FIELD_LEVELS],
    pub scheduler_fluid_ring_vertices: [usize; FAR_FIELD_LEVELS],
    pub scheduler_fluid_ring_indices: [usize; FAR_FIELD_LEVELS],
    /// True only when the bounded ECS observation is complete, structurally
    /// unique, in range, within budgets, and equal to scheduler bookkeeping.
    pub resident_observation_valid: bool,
    /// At least seven matching entities exist. Work stops at the seventh and
    /// aggregate payload fields fail closed to `usize::MAX`.
    pub resident_entity_count_overflow: bool,
    pub resident_duplicate_levels: usize,
    pub resident_out_of_range_levels: usize,
    pub resident_scheduler_mismatch: bool,
    pub resident_budget_exceeded: bool,
    /// Counts transitions into an invalid observation, not every frame spent
    /// invalid, so a persistent fault cannot manufacture an unbounded rate.
    pub resident_observation_rejections: u64,
    pub resident_fluid_observation_valid: bool,
    pub resident_fluid_entity_count_overflow: bool,
    pub resident_fluid_duplicate_slots: usize,
    pub resident_fluid_out_of_range_levels: usize,
    pub resident_fluid_scheduler_mismatch: bool,
    pub resident_fluid_budget_exceeded: bool,
    pub resident_fluid_observation_rejections: u64,
    pub pending_rebuilds: usize,
    pub dirty_mask: u8,
    pub build_in_flight: bool,
    pub update_cadence_frames: u8,
    /// Desired detail for future/coalesced ring rebuilds. Kept under the
    /// original field name for the QA report schema.
    pub material_detail: FarFieldMaterialDetail,
    pub desired_material_detail: [FarFieldMaterialDetail; FAR_FIELD_LEVELS],
    /// Actual detail currently installed at each LOD. `None` means that level
    /// has no resident mesh, so transition telemetry cannot imply completion.
    pub resident_material_detail: [Option<FarFieldMaterialDetail>; FAR_FIELD_LEVELS],
    pub resident_detailed_levels: usize,
    pub resident_reduced_levels: usize,
    pub surface_material_mode: FarFieldSurfaceMaterialMode,
    pub hydro_mode: FarFieldHydroMode,
    pub scheduler_deferred_frames: u64,
    pub completed_rebuilds: u64,
    pub stale_builds_discarded: u64,
    pub budget_rejections: u64,
    pub last_build_ms: f32,
    pub max_build_ms: f32,
    pub last_height_queries: usize,
    pub last_material_slope_queries: usize,
    pub last_biome_queries: usize,
    pub last_bridge_v2_cell_reuses: usize,
    pub last_fluid_classification_queries: usize,
    pub last_fluid_biome_queries: usize,
    pub last_fluid_vertices: usize,
    pub last_fluid_indices: usize,
    pub last_reused_height_samples: usize,
    pub last_reused_biome_samples: usize,
    pub last_cache_shift_x_cells: i32,
    pub last_cache_shift_z_cells: i32,
    pub last_cache_update: FarFieldCacheUpdate,
    pub incremental_strip_rebuilds: u64,
    pub full_cache_rebuilds: u64,
    pub teleport_fallbacks: u64,
    pub last_clamped_queries: usize,
    pub camera_world_x: i64,
    pub camera_world_z: i64,
}

impl Default for PlanetaryStreamingTelemetry {
    fn default() -> Self {
        Self {
            enabled: false,
            profile: WorldProfile::Natural,
            interaction_radius_metres: 0,
            confirmed_near_extent_metres: 0,
            near_coverage_ready_columns: 0,
            near_coverage_hidden_cells: 0,
            far_radius_metres: FAR_FIELD_OUTER_RADIUS_METRES,
            resident_entities: 0,
            resident_vertices: 0,
            resident_indices: 0,
            resident_mesh_bytes: 0,
            resident_fluid_entities: 0,
            resident_fluid_vertices: 0,
            resident_fluid_indices: 0,
            resident_fluid_mesh_bytes: 0,
            scheduler_resident_entities: 0,
            scheduler_resident_vertices: 0,
            scheduler_resident_indices: 0,
            scheduler_resident_mesh_bytes: 0,
            scheduler_resident_fluid_entities: 0,
            scheduler_resident_fluid_vertices: 0,
            scheduler_resident_fluid_indices: 0,
            scheduler_resident_fluid_mesh_bytes: 0,
            live_sample_cache_windows: 0,
            live_sample_cache_bytes: 0,
            peak_live_sample_cache_windows: 0,
            peak_live_sample_cache_bytes: 0,
            budget_entities: FAR_FIELD_MAX_ENTITIES,
            budget_vertices: FAR_FIELD_MAX_VERTICES,
            budget_indices: FAR_FIELD_MAX_INDICES,
            budget_mesh_bytes: FAR_FIELD_MAX_MESH_BYTES,
            budget_build_jobs: FAR_FIELD_MAX_BUILDS_IN_FLIGHT,
            budget_ring_build_bytes: FAR_FIELD_MAX_RING_BUILD_BYTES,
            budget_sample_cache_bytes: FAR_FIELD_MAX_SAMPLE_CACHE_BYTES,
            budget_coverage_work_bytes: FAR_FIELD_MAX_COVERAGE_WORK_BYTES,
            budget_fluid_entities: FAR_FIELD_MAX_FLUID_ENTITIES,
            budget_fluid_vertices: FAR_FIELD_MAX_FLUID_VERTICES,
            budget_fluid_indices: FAR_FIELD_MAX_FLUID_INDICES,
            budget_fluid_mesh_bytes: FAR_FIELD_MAX_FLUID_MESH_BYTES,
            budget_fluid_ring_build_bytes: FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES,
            budget_atomic_ring_build_bytes: FAR_FIELD_MAX_ATOMIC_RING_BUILD_BYTES,
            ring_vertices: [0; FAR_FIELD_LEVELS],
            ring_indices: [0; FAR_FIELD_LEVELS],
            scheduler_ring_vertices: [0; FAR_FIELD_LEVELS],
            scheduler_ring_indices: [0; FAR_FIELD_LEVELS],
            fluid_ring_vertices: [0; FAR_FIELD_LEVELS],
            fluid_ring_indices: [0; FAR_FIELD_LEVELS],
            scheduler_fluid_ring_vertices: [0; FAR_FIELD_LEVELS],
            scheduler_fluid_ring_indices: [0; FAR_FIELD_LEVELS],
            resident_observation_valid: true,
            resident_entity_count_overflow: false,
            resident_duplicate_levels: 0,
            resident_out_of_range_levels: 0,
            resident_scheduler_mismatch: false,
            resident_budget_exceeded: false,
            resident_observation_rejections: 0,
            resident_fluid_observation_valid: true,
            resident_fluid_entity_count_overflow: false,
            resident_fluid_duplicate_slots: 0,
            resident_fluid_out_of_range_levels: 0,
            resident_fluid_scheduler_mismatch: false,
            resident_fluid_budget_exceeded: false,
            resident_fluid_observation_rejections: 0,
            pending_rebuilds: 0,
            dirty_mask: 0,
            build_in_flight: false,
            update_cadence_frames: 1,
            material_detail: FarFieldMaterialDetail::Detailed,
            desired_material_detail: [FarFieldMaterialDetail::Detailed; FAR_FIELD_LEVELS],
            resident_material_detail: [None; FAR_FIELD_LEVELS],
            resident_detailed_levels: 0,
            resident_reduced_levels: 0,
            surface_material_mode: FarFieldSurfaceMaterialMode::BridgeV2,
            hydro_mode: FarFieldHydroMode::DescriptiveV1,
            scheduler_deferred_frames: 0,
            completed_rebuilds: 0,
            stale_builds_discarded: 0,
            budget_rejections: 0,
            last_build_ms: 0.0,
            max_build_ms: 0.0,
            last_height_queries: 0,
            last_material_slope_queries: 0,
            last_biome_queries: 0,
            last_bridge_v2_cell_reuses: 0,
            last_fluid_classification_queries: 0,
            last_fluid_biome_queries: 0,
            last_fluid_vertices: 0,
            last_fluid_indices: 0,
            last_reused_height_samples: 0,
            last_reused_biome_samples: 0,
            last_cache_shift_x_cells: 0,
            last_cache_shift_z_cells: 0,
            last_cache_update: FarFieldCacheUpdate::Cold,
            incremental_strip_rebuilds: 0,
            full_cache_rebuilds: 0,
            teleport_fallbacks: 0,
            last_clamped_queries: 0,
            camera_world_x: 0,
            camera_world_z: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FarFieldWorldKey {
    seed: u32,
    profile: WorldProfile,
    scenery: SceneryQuality,
    surface_material_mode: FarFieldSurfaceMaterialMode,
    hydro_mode: FarFieldHydroMode,
}

/// Minimal 64-bit world-space pair. Bevy 0.14's public math prelude exposes
/// `IVec2` (i32) but not an i64 equivalent, and far-field coordinates must not
/// lose integer precision before they become render-local `f32` values.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct WorldXZ {
    x: i64,
    z: i64,
}

impl WorldXZ {
    const ZERO: Self = Self { x: 0, z: 0 };

    const fn new(x: i64, z: i64) -> Self {
        Self { x, z }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingSpec {
    level: usize,
    step: i64,
    inner_extent: i64,
    outer_extent: i64,
    anchor: WorldXZ,
}

/// Fixed finest-grid stencil. A set bit means current-epoch near meshes cover
/// the complete 32 m parent cell, so emitting the coarse fallback there would
/// risk z-fighting or cutting through voxel relief.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NearCoverageMask {
    hidden: [u64; FAR_FIELD_COVERAGE_WORDS],
}

impl Default for NearCoverageMask {
    fn default() -> Self {
        Self {
            hidden: [0; FAR_FIELD_COVERAGE_WORDS],
        }
    }
}

impl NearCoverageMask {
    fn hide(&mut self, cx: i32, cz: i32) {
        let index = cell_index(cx, cz);
        self.hidden[index / 64] |= 1_u64 << (index % 64);
    }

    fn hides(self, cx: i32, cz: i32) -> bool {
        let index = cell_index(cx, cz);
        self.hidden[index / 64] & (1_u64 << (index % 64)) != 0
    }

    fn hidden_cells(self) -> usize {
        self.hidden
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    fn has_hidden_not_in(self, other: Self) -> bool {
        self.hidden
            .iter()
            .zip(other.hidden)
            .any(|(current, candidate)| current & !candidate != 0)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct NearCoverageSnapshot {
    mask: NearCoverageMask,
    confirmed_square_extent_metres: i64,
    ready_columns: usize,
}

impl RingSpec {
    fn for_level(level: usize, first_inner_extent: i64, camera_world: WorldXZ) -> Self {
        let step = FAR_FIELD_BASE_STEP_METRES << level;
        let outer_extent = (FAR_FIELD_GRID_CELLS as i64 / 2) * step;
        let inner_extent = if level == 0 {
            first_inner_extent
        } else {
            let previous_step = step / 2;
            let previous_outer = (FAR_FIELD_GRID_CELLS as i64 / 2) * previous_step;
            // Two coarse cells cover both the one-cell anchor disagreement and
            // the height-morph band between adjacent levels.
            (previous_outer - step * 2).max(step * 2)
        };
        Self {
            level,
            step,
            inner_extent,
            outer_extent,
            anchor: WorldXZ::new(
                snap_world_coordinate(camera_world.x, step),
                snap_world_coordinate(camera_world.z, step),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingBuildRequest {
    world: FarFieldWorldKey,
    spec: RingSpec,
    material_detail: FarFieldMaterialDetail,
    near_coverage: NearCoverageMask,
}

struct ScheduledRingBuild {
    request: RingBuildRequest,
    sample_cache: Option<RingSampleCache>,
}

#[derive(Component, Debug)]
struct FarFieldRing {
    level: usize,
    anchor: WorldXZ,
    material_detail: FarFieldMaterialDetail,
    vertices: usize,
    indices: usize,
}

/// One optional combined water/lava top-surface entity paired with a terrain
/// ring. Its request identity lives in `RingBuildRequest`; this component is
/// observational render state only.
#[derive(Component, Debug)]
struct FarFieldFluidRing {
    level: usize,
    anchor: WorldXZ,
    vertices: usize,
    indices: usize,
}

#[derive(Resource)]
struct PlanetaryStreamingRuntime {
    world_key: Option<FarFieldWorldKey>,
    target_specs: [RingSpec; FAR_FIELD_LEVELS],
    resident_specs: [Option<RingSpec>; FAR_FIELD_LEVELS],
    target_material_detail: [FarFieldMaterialDetail; FAR_FIELD_LEVELS],
    resident_material_detail: [Option<FarFieldMaterialDetail>; FAR_FIELD_LEVELS],
    target_near_coverage: NearCoverageMask,
    resident_near_coverage: NearCoverageMask,
    pending_near_coverage: NearCoverageMask,
    pending_near_coverage_stable_seconds: f32,
    resident_vertices: [usize; FAR_FIELD_LEVELS],
    resident_indices: [usize; FAR_FIELD_LEVELS],
    resident_fluid_vertices: [usize; FAR_FIELD_LEVELS],
    resident_fluid_indices: [usize; FAR_FIELD_LEVELS],
    sample_caches: [Option<RingSampleCache>; FAR_FIELD_LEVELS],
    dirty_mask: u8,
    next_level_cursor: usize,
    scheduler_frame: u64,
    scheduler_deferred_frames: u64,
    material: Option<Handle<StandardMaterial>>,
    fluid_material: Option<Handle<StandardMaterial>>,
    #[cfg(not(target_arch = "wasm32"))]
    in_flight: Option<Task<RingBuildResult>>,
    in_flight_cache_windows: usize,
}

impl Default for PlanetaryStreamingRuntime {
    fn default() -> Self {
        let empty_spec = RingSpec::for_level(0, 0, WorldXZ::ZERO);
        Self {
            world_key: None,
            target_specs: [empty_spec; FAR_FIELD_LEVELS],
            resident_specs: [None; FAR_FIELD_LEVELS],
            target_material_detail: [FarFieldMaterialDetail::Detailed; FAR_FIELD_LEVELS],
            resident_material_detail: [None; FAR_FIELD_LEVELS],
            target_near_coverage: NearCoverageMask::default(),
            resident_near_coverage: NearCoverageMask::default(),
            pending_near_coverage: NearCoverageMask::default(),
            pending_near_coverage_stable_seconds: 0.0,
            resident_vertices: [0; FAR_FIELD_LEVELS],
            resident_indices: [0; FAR_FIELD_LEVELS],
            resident_fluid_vertices: [0; FAR_FIELD_LEVELS],
            resident_fluid_indices: [0; FAR_FIELD_LEVELS],
            sample_caches: std::array::from_fn(|_| None),
            dirty_mask: 0,
            next_level_cursor: 0,
            scheduler_frame: 0,
            scheduler_deferred_frames: 0,
            material: None,
            fluid_material: None,
            #[cfg(not(target_arch = "wasm32"))]
            in_flight: None,
            in_flight_cache_windows: 0,
        }
    }
}

impl PlanetaryStreamingRuntime {
    fn clear_residency(&mut self) {
        self.resident_specs = [None; FAR_FIELD_LEVELS];
        self.target_material_detail = [FarFieldMaterialDetail::Detailed; FAR_FIELD_LEVELS];
        self.resident_material_detail = [None; FAR_FIELD_LEVELS];
        self.target_near_coverage = NearCoverageMask::default();
        self.resident_near_coverage = NearCoverageMask::default();
        self.pending_near_coverage = NearCoverageMask::default();
        self.pending_near_coverage_stable_seconds = 0.0;
        self.resident_vertices = [0; FAR_FIELD_LEVELS];
        self.resident_indices = [0; FAR_FIELD_LEVELS];
        self.resident_fluid_vertices = [0; FAR_FIELD_LEVELS];
        self.resident_fluid_indices = [0; FAR_FIELD_LEVELS];
        self.sample_caches = std::array::from_fn(|_| None);
        self.dirty_mask = 0;
        self.next_level_cursor = 0;
        self.scheduler_frame = 0;
        self.scheduler_deferred_frames = 0;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.in_flight = None;
        }
        self.in_flight_cache_windows = 0;
    }

    fn mark_dirty(&mut self, level: usize) {
        debug_assert!(level < FAR_FIELD_LEVELS);
        self.dirty_mask |= 1_u8 << level;
    }

    fn set_target_material_detail(&mut self, desired: FarFieldMaterialDetail) {
        for level in 0..FAR_FIELD_LEVELS {
            if self.target_material_detail[level] != desired {
                self.target_material_detail[level] = desired;
                // One bit per LOD coalesces any number of pressure
                // oscillations; there can never be more than six queued
                // material rebuilds.
                self.mark_dirty(level);
            }
        }
    }

    fn next_dirty_level(&mut self) -> Option<usize> {
        for offset in 0..FAR_FIELD_LEVELS {
            let level = (self.next_level_cursor + offset) % FAR_FIELD_LEVELS;
            if self.dirty_mask & (1_u8 << level) != 0 {
                self.dirty_mask &= !(1_u8 << level);
                self.next_level_cursor = (level + 1) % FAR_FIELD_LEVELS;
                return Some(level);
            }
        }
        None
    }

    /// Debounce only safe parent removal. Reintroducing any parent cell is an
    /// immediate safety transition, as is changing the finest-grid anchor.
    fn observe_near_coverage(
        &mut self,
        candidate: NearCoverageMask,
        force: bool,
        delta_seconds: f32,
    ) {
        let delta_seconds = if delta_seconds.is_finite() {
            delta_seconds.clamp(0.0, 0.25)
        } else {
            0.0
        };
        if self.pending_near_coverage == candidate {
            self.pending_near_coverage_stable_seconds = (self.pending_near_coverage_stable_seconds
                + delta_seconds)
                .min(FAR_FIELD_COVERAGE_STABILITY_SECONDS);
        } else {
            self.pending_near_coverage = candidate;
            self.pending_near_coverage_stable_seconds = delta_seconds;
        }

        let lost_coverage = self.target_near_coverage.has_hidden_not_in(candidate);
        let stable_expansion = self.target_near_coverage != candidate
            && self.pending_near_coverage_stable_seconds >= FAR_FIELD_COVERAGE_STABILITY_SECONDS;
        if force || lost_coverage || stable_expansion {
            self.target_near_coverage = candidate;
        }
    }
}

struct RingBuildResult {
    request: RingBuildRequest,
    mesh: FarFieldMeshData,
    fluid_mesh: FarFieldMeshData,
    sample_cache: RingSampleCache,
    build_ms: f32,
    sampling: SamplingStats,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
struct SamplingStats {
    height_queries: usize,
    material_slope_queries: usize,
    biome_queries: usize,
    bridge_v2_cell_reuses: usize,
    fluid_classification_queries: usize,
    fluid_biome_queries: usize,
    reused_height_samples: usize,
    reused_biome_samples: usize,
    clamped_queries: usize,
    cache_shift_x_cells: i32,
    cache_shift_z_cells: i32,
    cache_update: FarFieldCacheUpdate,
}

/// Fixed-size CPU source window. `origin_*` rotates logical coordinates over
/// the physical arrays, so a one-cell camera move overwrites one entering
/// strip instead of rebuilding 4,225 terrain samples. The two-cell halo is
/// sufficient for height morphing and the legacy 4x biome palette. Bridge-v1
/// adds one cached categorical family and validity bit per cell; its canonical
/// one-metre slope probes are counted separately and never retained as an
/// unbounded coordinate map.
struct RingSampleCache {
    world: FarFieldWorldKey,
    level: usize,
    step: i64,
    anchor: WorldXZ,
    origin_x: i32,
    origin_z: i32,
    heights: Box<[f32; FAR_FIELD_SAMPLE_CACHE_CELLS]>,
    biomes: Box<[Biome; FAR_FIELD_SAMPLE_CACHE_CELLS]>,
    biome_valid: Box<[bool; FAR_FIELD_SAMPLE_CACHE_CELLS]>,
    surface_families: Box<[BlockType; FAR_FIELD_SAMPLE_CACHE_CELLS]>,
    surface_family_valid: Box<[bool; FAR_FIELD_SAMPLE_CACHE_CELLS]>,
}

const RING_SAMPLE_CACHE_ACCOUNTED_BYTES: usize = size_of::<RingSampleCache>()
    + FAR_FIELD_SAMPLE_CACHE_CELLS
        * (size_of::<f32>()
            + size_of::<Biome>()
            + size_of::<bool>()
            + size_of::<BlockType>()
            + size_of::<bool>());
const _: () = assert!(
    RING_SAMPLE_CACHE_ACCOUNTED_BYTES * FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS
        <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES
);

impl RingSampleCache {
    const LOGICAL_HALF: i32 = FAR_FIELD_GRID_CELLS / 2 + FAR_FIELD_SAMPLE_HALO_CELLS;

    fn cold<S: FarFieldSampler>(
        sampler: &S,
        world: FarFieldWorldKey,
        spec: RingSpec,
        sampling: &mut SamplingStats,
        update: FarFieldCacheUpdate,
    ) -> Self {
        let mut cache = Self {
            world,
            level: spec.level,
            step: spec.step,
            anchor: spec.anchor,
            origin_x: 0,
            origin_z: 0,
            heights: Box::new([0.0; FAR_FIELD_SAMPLE_CACHE_CELLS]),
            biomes: Box::new([Biome::Plains; FAR_FIELD_SAMPLE_CACHE_CELLS]),
            biome_valid: Box::new([false; FAR_FIELD_SAMPLE_CACHE_CELLS]),
            surface_families: Box::new([BlockType::Stone; FAR_FIELD_SAMPLE_CACHE_CELLS]),
            surface_family_valid: Box::new([false; FAR_FIELD_SAMPLE_CACHE_CELLS]),
        };
        sampling.cache_update = update;
        cache.refill_all(sampler, sampling);
        cache
    }

    fn retarget<S: FarFieldSampler>(
        mut self,
        sampler: &S,
        world: FarFieldWorldKey,
        spec: RingSpec,
        sampling: &mut SamplingStats,
    ) -> Self {
        if self.world != world || self.level != spec.level || self.step != spec.step {
            // Keep ownership of the existing fixed arrays while changing their
            // interpretation. Constructing `Self::cold` here used to allocate
            // a replacement before `self` dropped, producing a hidden seventh
            // cache window inside the worker. In-place refill makes the public
            // resident + in-flight accounting an exact population ceiling.
            self.world = world;
            self.level = spec.level;
            self.step = spec.step;
            self.anchor = spec.anchor;
            self.origin_x = 0;
            self.origin_z = 0;
            sampling.cache_update = FarFieldCacheUpdate::IncompatibleFallback;
            self.refill_all(sampler, sampling);
            return self;
        }

        let Some((shift_x, shift_z)) = self.shift_cells(spec.anchor) else {
            self.anchor = spec.anchor;
            self.origin_x = 0;
            self.origin_z = 0;
            sampling.cache_update = FarFieldCacheUpdate::TeleportFallback;
            self.refill_all(sampler, sampling);
            return self;
        };
        sampling.cache_shift_x_cells = shift_x;
        sampling.cache_shift_z_cells = shift_z;
        if shift_x.unsigned_abs() as usize >= FAR_FIELD_SAMPLE_CACHE_SIDE
            || shift_z.unsigned_abs() as usize >= FAR_FIELD_SAMPLE_CACHE_SIDE
        {
            self.anchor = spec.anchor;
            self.origin_x = 0;
            self.origin_z = 0;
            sampling.cache_update = FarFieldCacheUpdate::TeleportFallback;
            self.refill_all(sampler, sampling);
            return self;
        }

        // Logical new(g) is old(g + shift). Rotating the origin makes every
        // overlapping sample immediately addressable without moving it.
        self.origin_x = (self.origin_x + shift_x).rem_euclid(FAR_FIELD_SAMPLE_CACHE_SIDE as i32);
        self.origin_z = (self.origin_z + shift_z).rem_euclid(FAR_FIELD_SAMPLE_CACHE_SIDE as i32);
        self.anchor = spec.anchor;
        sampling.cache_update = FarFieldCacheUpdate::IncrementalStrip;

        for gz in -Self::LOGICAL_HALF..=Self::LOGICAL_HALF {
            for gx in -Self::LOGICAL_HALF..=Self::LOGICAL_HALF {
                let old_gx = i64::from(gx) + i64::from(shift_x);
                let old_gz = i64::from(gz) + i64::from(shift_z);
                let index = self.index(gx, gz);
                if old_gx >= -i64::from(Self::LOGICAL_HALF)
                    && old_gx <= i64::from(Self::LOGICAL_HALF)
                    && old_gz >= -i64::from(Self::LOGICAL_HALF)
                    && old_gz <= i64::from(Self::LOGICAL_HALF)
                {
                    sampling.reused_height_samples =
                        sampling.reused_height_samples.saturating_add(1);
                    if self.biome_valid[index] {
                        sampling.reused_biome_samples =
                            sampling.reused_biome_samples.saturating_add(1);
                    }
                    continue;
                }

                let (world_x, world_z) = self.world_coordinate(gx, gz);
                self.heights[index] = sampled_height(sampler, world_x, world_z, sampling);
                self.biome_valid[index] = false;
                self.surface_family_valid[index] = false;
            }
        }
        self
    }

    fn refill_all<S: FarFieldSampler>(&mut self, sampler: &S, sampling: &mut SamplingStats) {
        self.biome_valid.fill(false);
        self.surface_family_valid.fill(false);
        for gz in -Self::LOGICAL_HALF..=Self::LOGICAL_HALF {
            for gx in -Self::LOGICAL_HALF..=Self::LOGICAL_HALF {
                let index = self.index(gx, gz);
                let (world_x, world_z) = self.world_coordinate(gx, gz);
                self.heights[index] = sampled_height(sampler, world_x, world_z, sampling);
            }
        }
    }

    fn shift_cells(&self, target: WorldXZ) -> Option<(i32, i32)> {
        let step = i128::from(self.step);
        let dx = i128::from(target.x) - i128::from(self.anchor.x);
        let dz = i128::from(target.z) - i128::from(self.anchor.z);
        if dx.rem_euclid(step) != 0 || dz.rem_euclid(step) != 0 {
            return None;
        }
        let shift_x = i32::try_from(dx.div_euclid(step)).ok()?;
        let shift_z = i32::try_from(dz.div_euclid(step)).ok()?;
        Some((shift_x, shift_z))
    }

    fn index(&self, gx: i32, gz: i32) -> usize {
        debug_assert!((-Self::LOGICAL_HALF..=Self::LOGICAL_HALF).contains(&gx));
        debug_assert!((-Self::LOGICAL_HALF..=Self::LOGICAL_HALF).contains(&gz));
        let x = (gx + Self::LOGICAL_HALF + self.origin_x)
            .rem_euclid(FAR_FIELD_SAMPLE_CACHE_SIDE as i32) as usize;
        let z = (gz + Self::LOGICAL_HALF + self.origin_z)
            .rem_euclid(FAR_FIELD_SAMPLE_CACHE_SIDE as i32) as usize;
        z * FAR_FIELD_SAMPLE_CACHE_SIDE + x
    }

    fn height(&self, gx: i32, gz: i32) -> f32 {
        self.heights[self.index(gx, gz)]
    }

    fn biome_at_or_sample<S: FarFieldSampler>(
        &mut self,
        sampler: &S,
        gx: i32,
        gz: i32,
        sampling: &mut SamplingStats,
    ) -> Biome {
        let index = self.index(gx, gz);
        if self.biome_valid[index] {
            return self.biomes[index];
        }
        let (world_x, world_z) = self.world_coordinate(gx, gz);
        let biome = sampled_biome(sampler, world_x, world_z, sampling);
        self.biomes[index] = biome;
        self.biome_valid[index] = true;
        biome
    }

    /// Bridge-v2's fixed absolute-cell classifier. The current toroidal slot is
    /// authoritative when valid (including after retarget). Otherwise the
    /// row-major build can copy from the immediately preceding X or Z vertex
    /// when both map to the same 128 m cell. This is a fixed O(1) memo: no hash
    /// map, no extra allocation, and at most two neighbour checks per vertex.
    fn bridge_v2_biome_at_or_sample<S: FarFieldSampler>(
        &mut self,
        sampler: &S,
        gx: i32,
        gz: i32,
        sampling: &mut SamplingStats,
    ) -> Biome {
        let index = self.index(gx, gz);
        if self.biome_valid[index] {
            return self.biomes[index];
        }

        let (world_x, world_z) = self.world_coordinate(gx, gz);
        let sample_x = bridge_v2_material_sample_coordinate(world_x);
        let sample_z = bridge_v2_material_sample_coordinate(world_z);
        for (candidate_x, candidate_z) in [
            gx.checked_sub(1).map(|x| (x, gz)),
            gz.checked_sub(1).map(|z| (gx, z)),
        ]
        .into_iter()
        .flatten()
        {
            if candidate_x < -Self::LOGICAL_HALF || candidate_z < -Self::LOGICAL_HALF {
                continue;
            }
            let candidate_index = self.index(candidate_x, candidate_z);
            if !self.biome_valid[candidate_index] {
                continue;
            }
            let (candidate_world_x, candidate_world_z) =
                self.world_coordinate(candidate_x, candidate_z);
            if bridge_v2_material_sample_coordinate(candidate_world_x) == sample_x
                && bridge_v2_material_sample_coordinate(candidate_world_z) == sample_z
            {
                let biome = self.biomes[candidate_index];
                self.biomes[index] = biome;
                self.biome_valid[index] = true;
                sampling.bridge_v2_cell_reuses = sampling.bridge_v2_cell_reuses.saturating_add(1);
                return biome;
            }
        }

        let biome = sampled_biome(sampler, sample_x, sample_z, sampling);
        self.biomes[index] = biome;
        self.biome_valid[index] = true;
        biome
    }

    /// Exact categorical material at this absolute world vertex. Unlike the
    /// legacy ring-local four-sample palette, this key is independent of LOD,
    /// anchor phase, and build order. Four one-metre probes reproduce the
    /// shared near-terrain slope rule; their hard per-ring cap is declared by
    /// `FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING`.
    fn surface_family_at_or_sample<S: FarFieldSampler>(
        &mut self,
        sampler: &S,
        gx: i32,
        gz: i32,
        sampling: &mut SamplingStats,
    ) -> BlockType {
        let index = self.index(gx, gz);
        if self.surface_family_valid[index] {
            return self.surface_families[index];
        }

        let (world_x, world_z) = self.world_coordinate(gx, gz);
        let center = self.heights[index];
        let quantum = FAR_FIELD_MATERIAL_SLOPE_QUANTUM_METRES;
        let max_rise = [
            sampled_material_height(sampler, world_x.saturating_sub(quantum), world_z, sampling),
            sampled_material_height(sampler, world_x.saturating_add(quantum), world_z, sampling),
            sampled_material_height(sampler, world_x, world_z.saturating_sub(quantum), sampling),
            sampled_material_height(sampler, world_x, world_z.saturating_add(quantum), sampling),
        ]
        .into_iter()
        .filter(|height| center.is_finite() && height.is_finite())
        .map(|height| (center - height).abs())
        .fold(0.0_f32, f32::max);
        let slope = (max_rise / quantum as f32).max(0.0);
        let biome = self.biome_at_or_sample(sampler, gx, gz, sampling);
        let family = coarse_surface_family(biome, slope);
        self.surface_families[index] = family;
        self.surface_family_valid[index] = true;
        family
    }

    fn world_coordinate(&self, gx: i32, gz: i32) -> (i64, i64) {
        (
            self.anchor
                .x
                .saturating_add(i64::from(gx).saturating_mul(self.step)),
            self.anchor
                .z
                .saturating_add(i64::from(gz).saturating_mul(self.step)),
        )
    }
}

struct FarFieldMeshData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl FarFieldMeshData {
    fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    fn index_count(&self) -> usize {
        self.indices.len()
    }

    fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn empty() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            colors: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
        }
    }

    fn into_mesh(self) -> Mesh {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_indices(Indices::U32(self.indices));
        mesh
    }
}

trait FarFieldSampler {
    fn height_at(&self, x: i32, z: i32) -> f32;
    fn biome_at(&self, x: i32, z: i32) -> Biome;
}

impl FarFieldSampler for TerrainGenerator {
    fn height_at(&self, x: i32, z: i32) -> f32 {
        self.surface_height_at(x, z) as f32
    }

    fn biome_at(&self, x: i32, z: i32) -> Biome {
        TerrainGenerator::biome_at(self, x, z)
    }
}

#[allow(clippy::too_many_arguments)]
fn update_planetary_streaming(
    mut commands: Commands,
    time: Res<Time>,
    config: Res<PlanetaryStreamingConfig>,
    render_origin: Res<PlanetaryRenderOrigin>,
    settings: Res<WorldSettings>,
    governor: Res<StreamingGovernor>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    anchors: Query<&Transform, (With<ChunkAnchor>, Without<FarFieldRing>)>,
    mut runtime: ResMut<PlanetaryStreamingRuntime>,
    mut telemetry: ResMut<PlanetaryStreamingTelemetry>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rings: Query<
        (Entity, &mut FarFieldRing, &mut Handle<Mesh>, &mut Transform),
        Without<ChunkAnchor>,
    >,
    mut fluid_rings: Query<
        (
            Entity,
            &mut FarFieldFluidRing,
            &mut Handle<Mesh>,
            &mut Transform,
        ),
        (Without<ChunkAnchor>, Without<FarFieldRing>),
    >,
) {
    let profile = settings.effective_world_profile();
    telemetry.profile = profile;
    telemetry.surface_material_mode = config.surface_material_mode;
    telemetry.hydro_mode = config.hydro_mode;
    telemetry.enabled = config.profile_gate.allows(profile);
    telemetry.far_radius_metres = FAR_FIELD_OUTER_RADIUS_METRES;

    if !telemetry.enabled {
        if runtime.world_key.is_some() || rings.iter().next().is_some() {
            clear_render_rings(&mut commands, &mut meshes, &mut rings, &mut fluid_rings);
            runtime.clear_residency();
            runtime.world_key = None;
        }
        refresh_telemetry(&runtime, &mut telemetry);
        return;
    }

    let Ok(anchor_transform) = anchors.get_single() else {
        refresh_telemetry(&runtime, &mut telemetry);
        return;
    };
    let local_x = finite_floor_i64(anchor_transform.translation.x);
    let local_z = finite_floor_i64(anchor_transform.translation.z);
    let camera_world = WorldXZ::new(
        render_origin.world_x.saturating_add(local_x),
        render_origin.world_z.saturating_add(local_z),
    );
    telemetry.camera_world_x = camera_world.x;
    telemetry.camera_world_z = camera_world.z;

    // The interaction streamer has a public hard radius ceiling. Using that
    // ceiling rather than the requested visual RD prevents an accidental gap
    // when a user asks for a huge horizon that the full-chunk tier must reject.
    let active_interaction_chunks = governor
        .interaction_radius_chunks
        .max(2)
        .min(crate::world::MAX_INTERACTION_RADIUS_CHUNKS);
    let interaction_radius =
        i64::from(active_interaction_chunks) * i64::from(crate::chunk::CHUNK_SIZE_I);
    telemetry.interaction_radius_metres = interaction_radius;
    let world_key = FarFieldWorldKey {
        seed: settings.seed,
        profile,
        scenery: settings.scenery_quality,
        surface_material_mode: config.surface_material_mode,
        hydro_mode: config.hydro_mode,
    };
    let world_changed = runtime.world_key != Some(world_key);
    if world_changed {
        // Never show a previous seed/profile as if it belonged to the new
        // world. Runtime mesh assets are released; no project/user file is
        // touched by this operation.
        let queued_despawns =
            clear_render_rings(&mut commands, &mut meshes, &mut rings, &mut fluid_rings);
        runtime.clear_residency();
        runtime.world_key = Some(world_key);
        runtime.dirty_mask = FULL_DIRTY_MASK;
        if queued_despawns != 0 {
            // `Commands` are deferred until this system returns. In the WASM
            // synchronous path, installing immediately would otherwise find
            // and mutate an old ring that is already queued for despawn, then
            // record it as resident after Bevy removes it. A one-frame barrier
            // is fail-closed and also protects any synchronous executor used
            // by tests or future native fallback paths.
            refresh_telemetry(&runtime, &mut telemetry);
            return;
        }
    }

    let finest_spec = RingSpec::for_level(0, FAR_FIELD_FINEST_INNER_EXTENT_METRES, camera_world);
    let finest_target_changed = runtime.target_specs[0] != finest_spec;
    let camera_block_x = crate::chunk::floor_to_i32_safe(anchor_transform.translation.x);
    let camera_block_z = crate::chunk::floor_to_i32_safe(anchor_transform.translation.z);
    let vertical_chunks = settings
        .vertical_chunks
        .clamp(SAFE_MIN_VERTICAL_CHUNKS, SAFE_MAX_VERTICAL_CHUNKS) as i32;
    let coverage = if world_changed {
        NearCoverageSnapshot::default()
    } else {
        build_near_coverage_snapshot(
            finest_spec,
            camera_block_x,
            camera_block_z,
            active_interaction_chunks,
            |cx, cz| near_column_is_visually_ready(&world, &streamer, cx, cz, vertical_chunks),
        )
    };
    telemetry.confirmed_near_extent_metres = coverage.confirmed_square_extent_metres;
    telemetry.near_coverage_ready_columns = coverage.ready_columns;
    runtime.observe_near_coverage(
        coverage.mask,
        world_changed || finest_target_changed,
        time.delta_seconds(),
    );
    telemetry.near_coverage_hidden_cells = runtime.target_near_coverage.hidden_cells();

    let (update_cadence, desired_material_detail) = pressure_policy(&governor, &runtime);
    runtime.set_target_material_detail(desired_material_detail);

    for level in 0..FAR_FIELD_LEVELS {
        let target = RingSpec::for_level(level, FAR_FIELD_FINEST_INNER_EXTENT_METRES, camera_world);
        runtime.target_specs[level] = target;
        if runtime.resident_specs[level] != Some(target)
            || runtime.resident_material_detail[level]
                != Some(runtime.target_material_detail[level])
            || (level == 0 && runtime.resident_near_coverage != runtime.target_near_coverage)
        {
            runtime.mark_dirty(level);
        }
    }

    // A render-origin shift moves existing local entities but intentionally
    // does not regenerate their world-space height samples.
    for (_, ring, _, mut transform) in &mut rings {
        transform.translation.x = relative_f32(ring.anchor.x, render_origin.world_x);
        transform.translation.z = relative_f32(ring.anchor.z, render_origin.world_z);
    }
    for (_, ring, _, mut transform) in &mut fluid_rings {
        transform.translation.x = relative_f32(ring.anchor.x, render_origin.world_x);
        transform.translation.z = relative_f32(ring.anchor.z, render_origin.world_z);
    }

    runtime.scheduler_frame = runtime.scheduler_frame.wrapping_add(1);
    telemetry.update_cadence_frames = update_cadence;
    let schedule_now = runtime.scheduler_frame % u64::from(update_cadence) == 0;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let completed = runtime
            .in_flight
            .as_mut()
            .and_then(|task| future::block_on(future::poll_once(task)));
        if let Some(result) = completed {
            runtime.in_flight = None;
            runtime.in_flight_cache_windows = 0;
            install_ring_result(
                result,
                &mut commands,
                &render_origin,
                &mut runtime,
                &mut telemetry,
                &mut meshes,
                &mut materials,
                &mut rings,
                &mut fluid_rings,
            );
        }

        if runtime.in_flight.is_none() && schedule_now {
            if let Some(level) = runtime.next_dirty_level() {
                let request = RingBuildRequest {
                    world: world_key,
                    spec: runtime.target_specs[level],
                    material_detail: runtime.target_material_detail[level],
                    near_coverage: if level == 0 {
                        runtime.target_near_coverage
                    } else {
                        NearCoverageMask::default()
                    },
                };
                let scheduled = ScheduledRingBuild {
                    request,
                    // The old render mesh stays resident. Only its CPU sample
                    // source moves to the worker, avoiding a seventh steady
                    // cache window and an O(4k) clone per rebuild.
                    sample_cache: runtime.sample_caches[level].take(),
                };
                runtime.in_flight_cache_windows = 1;
                runtime.in_flight = Some(
                    AsyncComputeTaskPool::get().spawn(async move { build_ring_request(scheduled) }),
                );
            }
        } else if runtime.in_flight.is_none() && runtime.dirty_mask != 0 {
            runtime.scheduler_deferred_frames = runtime.scheduler_deferred_frames.saturating_add(1);
        }
    }

    #[cfg(target_arch = "wasm32")]
    if schedule_now {
        if let Some(level) = runtime.next_dirty_level() {
            // Web builds do not assume worker threads. One ring per frame retains
            // the same bounded install cadence and avoids an unbounded task queue.
            let request = RingBuildRequest {
                world: world_key,
                spec: runtime.target_specs[level],
                material_detail: runtime.target_material_detail[level],
                near_coverage: if level == 0 {
                    runtime.target_near_coverage
                } else {
                    NearCoverageMask::default()
                },
            };
            let scheduled = ScheduledRingBuild {
                request,
                sample_cache: runtime.sample_caches[level].take(),
            };
            let result = build_ring_request(scheduled);
            install_ring_result(
                result,
                &mut commands,
                &render_origin,
                &mut runtime,
                &mut telemetry,
                &mut meshes,
                &mut materials,
                &mut rings,
                &mut fluid_rings,
            );
        }
    } else if runtime.dirty_mask != 0 {
        runtime.scheduler_deferred_frames = runtime.scheduler_deferred_frames.saturating_add(1);
    }

    refresh_telemetry(&runtime, &mut telemetry);
}

#[allow(clippy::too_many_arguments)]
fn install_ring_result(
    result: RingBuildResult,
    commands: &mut Commands,
    render_origin: &PlanetaryRenderOrigin,
    runtime: &mut PlanetaryStreamingRuntime,
    telemetry: &mut PlanetaryStreamingTelemetry,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    rings: &mut Query<
        (Entity, &mut FarFieldRing, &mut Handle<Mesh>, &mut Transform),
        Without<ChunkAnchor>,
    >,
    fluid_rings: &mut Query<
        (
            Entity,
            &mut FarFieldFluidRing,
            &mut Handle<Mesh>,
            &mut Transform,
        ),
        (Without<ChunkAnchor>, Without<FarFieldRing>),
    >,
) {
    let level = result.request.spec.level;
    if !ring_request_is_current(runtime, result.request) {
        telemetry.stale_builds_discarded = telemetry.stale_builds_discarded.saturating_add(1);
        // A same-world stale cache is safe and useful as the deterministic
        // source for the next coalesced target; only the visible mesh result
        // is fail-closed. A previous seed/profile cache is never retained.
        if level < FAR_FIELD_LEVELS && runtime.world_key == Some(result.request.world) {
            runtime.sample_caches[level] = Some(result.sample_cache);
            runtime.mark_dirty(level);
        }
        return;
    }

    let vertices = result.mesh.vertex_count();
    let indices = result.mesh.index_count();
    let fluid_vertices = result.fluid_mesh.vertex_count();
    let fluid_indices = result.fluid_mesh.index_count();
    let ring_build_bytes = mesh_payload_bytes(vertices, indices);
    let fluid_ring_build_bytes = mesh_payload_bytes(fluid_vertices, fluid_indices);
    let atomic_ring_build_bytes = ring_build_bytes.saturating_add(fluid_ring_build_bytes);
    let projected_vertices = runtime
        .resident_vertices
        .iter()
        .sum::<usize>()
        .saturating_sub(runtime.resident_vertices[level])
        .saturating_add(vertices);
    let projected_indices = runtime
        .resident_indices
        .iter()
        .sum::<usize>()
        .saturating_sub(runtime.resident_indices[level])
        .saturating_add(indices);
    let projected_mesh_bytes = mesh_payload_bytes(projected_vertices, projected_indices);
    let projected_fluid_vertices = runtime
        .resident_fluid_vertices
        .iter()
        .sum::<usize>()
        .saturating_sub(runtime.resident_fluid_vertices[level])
        .saturating_add(fluid_vertices);
    let projected_fluid_indices = runtime
        .resident_fluid_indices
        .iter()
        .sum::<usize>()
        .saturating_sub(runtime.resident_fluid_indices[level])
        .saturating_add(fluid_indices);
    let projected_fluid_mesh_bytes =
        mesh_payload_bytes(projected_fluid_vertices, projected_fluid_indices);
    if vertices > FAR_FIELD_MAX_RING_VERTICES
        || indices > FAR_FIELD_MAX_RING_INDICES
        || ring_build_bytes > FAR_FIELD_MAX_RING_BUILD_BYTES
        || projected_vertices > FAR_FIELD_MAX_VERTICES
        || projected_indices > FAR_FIELD_MAX_INDICES
        || projected_mesh_bytes > FAR_FIELD_MAX_MESH_BYTES
        || fluid_vertices > FAR_FIELD_MAX_FLUID_VERTICES_PER_RING
        || fluid_indices > FAR_FIELD_MAX_FLUID_INDICES_PER_RING
        || fluid_ring_build_bytes > FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES
        || atomic_ring_build_bytes > FAR_FIELD_MAX_ATOMIC_RING_BUILD_BYTES
        || projected_fluid_vertices > FAR_FIELD_MAX_FLUID_VERTICES
        || projected_fluid_indices > FAR_FIELD_MAX_FLUID_INDICES
        || projected_fluid_mesh_bytes > FAR_FIELD_MAX_FLUID_MESH_BYTES
    {
        error!(
            "planetary streaming rejected atomic level {}: terrain {} vertices / {} indices / {} bytes, fluid {} / {} / {} bytes, atomic {} bytes; projected terrain {} / {} / {}, fluid {} / {} / {} exceeds declared budgets",
            level,
            vertices,
            indices,
            ring_build_bytes,
            fluid_vertices,
            fluid_indices,
            fluid_ring_build_bytes,
            atomic_ring_build_bytes,
            projected_vertices,
            projected_indices,
            projected_mesh_bytes,
            projected_fluid_vertices,
            projected_fluid_indices,
            projected_fluid_mesh_bytes
        );
        telemetry.budget_rejections = telemetry.budget_rejections.saturating_add(1);
        runtime.sample_caches[level] = Some(result.sample_cache);
        runtime.mark_dirty(level);
        return;
    }

    let translation = Vec3::new(
        relative_f32(result.request.spec.anchor.x, render_origin.world_x),
        0.0,
        relative_f32(result.request.spec.anchor.z, render_origin.world_z),
    );

    // Both CPU payloads have passed every per-ring and aggregate ceiling.
    // Asset insertion and ECS mutation begin only now, so a rejected/stale
    // result can never publish half of the terrain/fluid pair.
    let mesh_handle = meshes.add(result.mesh.into_mesh());
    let fluid_mesh_handle = if result.fluid_mesh.is_empty() {
        None
    } else {
        Some(meshes.add(result.fluid_mesh.into_mesh()))
    };

    let mut updated_existing = false;
    for (_, mut ring, mut old_handle, mut transform) in rings.iter_mut() {
        if ring.level != level {
            continue;
        }
        let replaced = std::mem::replace(&mut *old_handle, mesh_handle.clone());
        let _ = meshes.remove(replaced.id());
        transform.translation = translation;
        ring.anchor = result.request.spec.anchor;
        ring.material_detail = result.request.material_detail;
        ring.vertices = vertices;
        ring.indices = indices;
        updated_existing = true;
        break;
    }

    if !updated_existing {
        let material = runtime
            .material
            .get_or_insert_with(|| materials.add(far_field_material()))
            .clone();
        commands.spawn((
            PbrBundle {
                mesh: mesh_handle,
                material,
                transform: Transform::from_translation(translation),
                ..default()
            },
            NotShadowCaster,
            NotShadowReceiver,
            FarFieldRing {
                level,
                anchor: result.request.spec.anchor,
                material_detail: result.request.material_detail,
                vertices,
                indices,
            },
            Name::new(format!("Planetary far field L{level}")),
        ));
    }

    if let Some(new_handle) = fluid_mesh_handle {
        let mut updated_existing_fluid = false;
        for (_, mut ring, mut old_handle, mut transform) in fluid_rings.iter_mut() {
            if ring.level != level {
                continue;
            }
            let replaced = std::mem::replace(&mut *old_handle, new_handle.clone());
            let _ = meshes.remove(replaced.id());
            transform.translation = translation;
            ring.anchor = result.request.spec.anchor;
            ring.vertices = fluid_vertices;
            ring.indices = fluid_indices;
            updated_existing_fluid = true;
            break;
        }
        if !updated_existing_fluid {
            let material = runtime
                .fluid_material
                .get_or_insert_with(|| materials.add(far_field_fluid_material()))
                .clone();
            commands.spawn((
                PbrBundle {
                    mesh: new_handle,
                    material,
                    transform: Transform::from_translation(translation),
                    ..default()
                },
                NotShadowCaster,
                NotShadowReceiver,
                FarFieldFluidRing {
                    level,
                    anchor: result.request.spec.anchor,
                    vertices: fluid_vertices,
                    indices: fluid_indices,
                },
                Name::new(format!("Planetary far hydrography L{level}")),
            ));
        }
    } else {
        for (entity, ring, old_handle, _) in fluid_rings.iter_mut() {
            if ring.level != level {
                continue;
            }
            commands.entity(entity).despawn_recursive();
            let _ = meshes.remove(old_handle.id());
            break;
        }
    }

    runtime.resident_specs[level] = Some(result.request.spec);
    runtime.resident_material_detail[level] = Some(result.request.material_detail);
    if level == 0 {
        runtime.resident_near_coverage = result.request.near_coverage;
    }
    runtime.resident_vertices[level] = vertices;
    runtime.resident_indices[level] = indices;
    runtime.resident_fluid_vertices[level] = fluid_vertices;
    runtime.resident_fluid_indices[level] = fluid_indices;
    runtime.sample_caches[level] = Some(result.sample_cache);
    if runtime.target_specs[level] == result.request.spec
        && runtime.target_material_detail[level] == result.request.material_detail
        && (level != 0 || runtime.target_near_coverage == result.request.near_coverage)
    {
        runtime.dirty_mask &= !(1_u8 << level);
    }
    telemetry.completed_rebuilds = telemetry.completed_rebuilds.saturating_add(1);
    telemetry.last_build_ms = result.build_ms;
    telemetry.max_build_ms = telemetry.max_build_ms.max(result.build_ms);
    telemetry.last_height_queries = result.sampling.height_queries;
    telemetry.last_material_slope_queries = result.sampling.material_slope_queries;
    telemetry.last_biome_queries = result.sampling.biome_queries;
    telemetry.last_bridge_v2_cell_reuses = result.sampling.bridge_v2_cell_reuses;
    telemetry.last_fluid_classification_queries = result.sampling.fluid_classification_queries;
    telemetry.last_fluid_biome_queries = result.sampling.fluid_biome_queries;
    telemetry.last_fluid_vertices = fluid_vertices;
    telemetry.last_fluid_indices = fluid_indices;
    telemetry.last_reused_height_samples = result.sampling.reused_height_samples;
    telemetry.last_reused_biome_samples = result.sampling.reused_biome_samples;
    telemetry.last_cache_shift_x_cells = result.sampling.cache_shift_x_cells;
    telemetry.last_cache_shift_z_cells = result.sampling.cache_shift_z_cells;
    telemetry.last_cache_update = result.sampling.cache_update;
    match result.sampling.cache_update {
        FarFieldCacheUpdate::IncrementalStrip => {
            telemetry.incremental_strip_rebuilds =
                telemetry.incremental_strip_rebuilds.saturating_add(1);
        }
        FarFieldCacheUpdate::TeleportFallback => {
            telemetry.full_cache_rebuilds = telemetry.full_cache_rebuilds.saturating_add(1);
            telemetry.teleport_fallbacks = telemetry.teleport_fallbacks.saturating_add(1);
        }
        FarFieldCacheUpdate::Cold | FarFieldCacheUpdate::IncompatibleFallback => {
            telemetry.full_cache_rebuilds = telemetry.full_cache_rebuilds.saturating_add(1);
        }
    }
    telemetry.last_clamped_queries = result.sampling.clamped_queries;
}

fn ring_request_is_current(runtime: &PlanetaryStreamingRuntime, request: RingBuildRequest) -> bool {
    request.spec.level < FAR_FIELD_LEVELS
        && runtime.world_key == Some(request.world)
        && runtime.target_specs[request.spec.level] == request.spec
        && runtime.target_material_detail[request.spec.level] == request.material_detail
        && (request.spec.level != 0 || runtime.target_near_coverage == request.near_coverage)
}

fn clear_render_rings(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    rings: &mut Query<
        (Entity, &mut FarFieldRing, &mut Handle<Mesh>, &mut Transform),
        Without<ChunkAnchor>,
    >,
    fluid_rings: &mut Query<
        (
            Entity,
            &mut FarFieldFluidRing,
            &mut Handle<Mesh>,
            &mut Transform,
        ),
        (Without<ChunkAnchor>, Without<FarFieldRing>),
    >,
) -> usize {
    let mut queued_despawns = 0usize;
    for (entity, _, mesh, _) in rings.iter_mut() {
        commands.entity(entity).despawn_recursive();
        let _ = meshes.remove(mesh.id());
        queued_despawns = queued_despawns.saturating_add(1);
    }
    for (entity, _, mesh, _) in fluid_rings.iter_mut() {
        commands.entity(entity).despawn_recursive();
        let _ = meshes.remove(mesh.id());
        queued_despawns = queued_despawns.saturating_add(1);
    }
    queued_despawns
}

fn teardown_planetary_streaming(
    mut commands: Commands,
    mut runtime: ResMut<PlanetaryStreamingRuntime>,
    mut telemetry: ResMut<PlanetaryStreamingTelemetry>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rings: Query<
        (Entity, &mut FarFieldRing, &mut Handle<Mesh>, &mut Transform),
        Without<ChunkAnchor>,
    >,
    mut fluid_rings: Query<
        (
            Entity,
            &mut FarFieldFluidRing,
            &mut Handle<Mesh>,
            &mut Transform,
        ),
        (Without<ChunkAnchor>, Without<FarFieldRing>),
    >,
) {
    clear_render_rings(&mut commands, &mut meshes, &mut rings, &mut fluid_rings);
    if let Some(material) = runtime.material.take() {
        let _ = materials.remove(material.id());
    }
    if let Some(material) = runtime.fluid_material.take() {
        let _ = materials.remove(material.id());
    }
    runtime.clear_residency();
    runtime.world_key = None;
    *telemetry = PlanetaryStreamingTelemetry::default();
}

/// Observe the render entities only after the explicit deferred-command
/// barrier in [`PlanetaryStreamingPlugin`]. The scan is permanently bounded:
/// six admissible rings plus one sentinel that proves over-population. Valid
/// states therefore have exact entity and per-level payload counts; invalid
/// over-populated states publish fail-closed sentinels instead of understating
/// unseen work.
fn observe_planetary_residency(
    rings: Query<&FarFieldRing>,
    fluid_rings: Query<&FarFieldFluidRing>,
    mut telemetry: ResMut<PlanetaryStreamingTelemetry>,
) {
    let was_valid = telemetry.resident_observation_valid;
    let mut entities = 0usize;
    let mut vertices = 0usize;
    let mut indices = 0usize;
    let mut ring_vertices = [0usize; FAR_FIELD_LEVELS];
    let mut ring_indices = [0usize; FAR_FIELD_LEVELS];
    let mut level_seen = [false; FAR_FIELD_LEVELS];
    let mut duplicate_levels = 0usize;
    let mut out_of_range_levels = 0usize;

    for ring in rings.iter().take(FAR_FIELD_OBSERVATION_SCAN_LIMIT) {
        entities = entities.saturating_add(1);
        vertices = vertices.saturating_add(ring.vertices);
        indices = indices.saturating_add(ring.indices);
        if ring.level >= FAR_FIELD_LEVELS {
            out_of_range_levels = out_of_range_levels.saturating_add(1);
            continue;
        }
        if level_seen[ring.level] {
            duplicate_levels = duplicate_levels.saturating_add(1);
        }
        level_seen[ring.level] = true;
        ring_vertices[ring.level] = ring_vertices[ring.level].saturating_add(ring.vertices);
        ring_indices[ring.level] = ring_indices[ring.level].saturating_add(ring.indices);
    }

    // Reaching the seventh observation is sufficient to reject the state. Do
    // not inspect an eighth: the telemetry workload is independent of the
    // magnitude of a duplicate-spawn failure.
    let entity_count_overflow = entities == FAR_FIELD_OBSERVATION_SCAN_LIMIT;
    if entity_count_overflow {
        vertices = usize::MAX;
        indices = usize::MAX;
        ring_vertices = [usize::MAX; FAR_FIELD_LEVELS];
        ring_indices = [usize::MAX; FAR_FIELD_LEVELS];
    }
    let mesh_bytes = mesh_payload_bytes(vertices, indices);
    let budget_exceeded = entity_count_overflow
        || entities > telemetry.budget_entities
        || vertices > telemetry.budget_vertices
        || indices > telemetry.budget_indices
        || mesh_bytes > telemetry.budget_mesh_bytes;
    let scheduler_mismatch = entity_count_overflow
        || entities != telemetry.scheduler_resident_entities
        || vertices != telemetry.scheduler_resident_vertices
        || indices != telemetry.scheduler_resident_indices
        || mesh_bytes != telemetry.scheduler_resident_mesh_bytes
        || ring_vertices != telemetry.scheduler_ring_vertices
        || ring_indices != telemetry.scheduler_ring_indices;
    let valid = !entity_count_overflow
        && duplicate_levels == 0
        && out_of_range_levels == 0
        && !budget_exceeded
        && !scheduler_mismatch;

    telemetry.resident_entities = entities;
    telemetry.resident_vertices = vertices;
    telemetry.resident_indices = indices;
    telemetry.resident_mesh_bytes = mesh_bytes;
    telemetry.ring_vertices = ring_vertices;
    telemetry.ring_indices = ring_indices;
    telemetry.resident_entity_count_overflow = entity_count_overflow;
    telemetry.resident_duplicate_levels = duplicate_levels;
    telemetry.resident_out_of_range_levels = out_of_range_levels;
    telemetry.resident_scheduler_mismatch = scheduler_mismatch;
    telemetry.resident_budget_exceeded = budget_exceeded;
    telemetry.resident_observation_valid = valid;
    if !valid && was_valid {
        telemetry.resident_observation_rejections =
            telemetry.resident_observation_rejections.saturating_add(1);
    }

    let fluid_was_valid = telemetry.resident_fluid_observation_valid;
    let mut fluid_entities = 0usize;
    let mut fluid_vertices = 0usize;
    let mut fluid_indices = 0usize;
    let mut fluid_ring_vertices = [0usize; FAR_FIELD_LEVELS];
    let mut fluid_ring_indices = [0usize; FAR_FIELD_LEVELS];
    let mut fluid_level_seen = [false; FAR_FIELD_LEVELS];
    let mut fluid_duplicate_slots = 0usize;
    let mut fluid_out_of_range_levels = 0usize;
    for ring in fluid_rings
        .iter()
        .take(FAR_FIELD_FLUID_OBSERVATION_SCAN_LIMIT)
    {
        fluid_entities = fluid_entities.saturating_add(1);
        fluid_vertices = fluid_vertices.saturating_add(ring.vertices);
        fluid_indices = fluid_indices.saturating_add(ring.indices);
        if ring.level >= FAR_FIELD_LEVELS {
            fluid_out_of_range_levels = fluid_out_of_range_levels.saturating_add(1);
            continue;
        }
        if fluid_level_seen[ring.level] {
            fluid_duplicate_slots = fluid_duplicate_slots.saturating_add(1);
        }
        fluid_level_seen[ring.level] = true;
        fluid_ring_vertices[ring.level] =
            fluid_ring_vertices[ring.level].saturating_add(ring.vertices);
        fluid_ring_indices[ring.level] =
            fluid_ring_indices[ring.level].saturating_add(ring.indices);
    }
    let fluid_entity_count_overflow = fluid_entities == FAR_FIELD_FLUID_OBSERVATION_SCAN_LIMIT;
    if fluid_entity_count_overflow {
        fluid_vertices = usize::MAX;
        fluid_indices = usize::MAX;
        fluid_ring_vertices = [usize::MAX; FAR_FIELD_LEVELS];
        fluid_ring_indices = [usize::MAX; FAR_FIELD_LEVELS];
    }
    let fluid_mesh_bytes = mesh_payload_bytes(fluid_vertices, fluid_indices);
    let fluid_budget_exceeded = fluid_entity_count_overflow
        || fluid_entities > telemetry.budget_fluid_entities
        || fluid_vertices > telemetry.budget_fluid_vertices
        || fluid_indices > telemetry.budget_fluid_indices
        || fluid_mesh_bytes > telemetry.budget_fluid_mesh_bytes;
    let fluid_scheduler_mismatch = fluid_entity_count_overflow
        || fluid_entities != telemetry.scheduler_resident_fluid_entities
        || fluid_vertices != telemetry.scheduler_resident_fluid_vertices
        || fluid_indices != telemetry.scheduler_resident_fluid_indices
        || fluid_mesh_bytes != telemetry.scheduler_resident_fluid_mesh_bytes
        || fluid_ring_vertices != telemetry.scheduler_fluid_ring_vertices
        || fluid_ring_indices != telemetry.scheduler_fluid_ring_indices;
    let fluid_valid = !fluid_entity_count_overflow
        && fluid_duplicate_slots == 0
        && fluid_out_of_range_levels == 0
        && !fluid_budget_exceeded
        && !fluid_scheduler_mismatch;
    telemetry.resident_fluid_entities = fluid_entities;
    telemetry.resident_fluid_vertices = fluid_vertices;
    telemetry.resident_fluid_indices = fluid_indices;
    telemetry.resident_fluid_mesh_bytes = fluid_mesh_bytes;
    telemetry.fluid_ring_vertices = fluid_ring_vertices;
    telemetry.fluid_ring_indices = fluid_ring_indices;
    telemetry.resident_fluid_entity_count_overflow = fluid_entity_count_overflow;
    telemetry.resident_fluid_duplicate_slots = fluid_duplicate_slots;
    telemetry.resident_fluid_out_of_range_levels = fluid_out_of_range_levels;
    telemetry.resident_fluid_scheduler_mismatch = fluid_scheduler_mismatch;
    telemetry.resident_fluid_budget_exceeded = fluid_budget_exceeded;
    telemetry.resident_fluid_observation_valid = fluid_valid;
    if !fluid_valid && fluid_was_valid {
        telemetry.resident_fluid_observation_rejections = telemetry
            .resident_fluid_observation_rejections
            .saturating_add(1);
    }
}

fn refresh_telemetry(
    runtime: &PlanetaryStreamingRuntime,
    telemetry: &mut PlanetaryStreamingTelemetry,
) {
    telemetry.scheduler_ring_vertices = runtime.resident_vertices;
    telemetry.scheduler_ring_indices = runtime.resident_indices;
    telemetry.scheduler_resident_entities = runtime
        .resident_specs
        .iter()
        .filter(|spec| spec.is_some())
        .count();
    telemetry.scheduler_resident_vertices = runtime.resident_vertices.iter().sum();
    telemetry.scheduler_resident_indices = runtime.resident_indices.iter().sum();
    telemetry.scheduler_resident_mesh_bytes = mesh_payload_bytes(
        telemetry.scheduler_resident_vertices,
        telemetry.scheduler_resident_indices,
    );
    telemetry.scheduler_fluid_ring_vertices = runtime.resident_fluid_vertices;
    telemetry.scheduler_fluid_ring_indices = runtime.resident_fluid_indices;
    telemetry.scheduler_resident_fluid_entities = runtime
        .resident_fluid_indices
        .iter()
        .filter(|indices| **indices != 0)
        .count();
    telemetry.scheduler_resident_fluid_vertices = runtime.resident_fluid_vertices.iter().sum();
    telemetry.scheduler_resident_fluid_indices = runtime.resident_fluid_indices.iter().sum();
    telemetry.scheduler_resident_fluid_mesh_bytes = mesh_payload_bytes(
        telemetry.scheduler_resident_fluid_vertices,
        telemetry.scheduler_resident_fluid_indices,
    );
    let live_cache_windows = runtime
        .sample_caches
        .iter()
        .filter(|cache| cache.is_some())
        .count()
        .saturating_add(runtime.in_flight_cache_windows);
    telemetry.live_sample_cache_windows = live_cache_windows;
    telemetry.live_sample_cache_bytes =
        live_cache_windows.saturating_mul(RING_SAMPLE_CACHE_ACCOUNTED_BYTES);
    telemetry.peak_live_sample_cache_windows = telemetry
        .peak_live_sample_cache_windows
        .max(telemetry.live_sample_cache_windows);
    telemetry.peak_live_sample_cache_bytes = telemetry
        .peak_live_sample_cache_bytes
        .max(telemetry.live_sample_cache_bytes);
    debug_assert!(live_cache_windows <= FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS);
    debug_assert!(telemetry.live_sample_cache_bytes <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES);
    debug_assert!(telemetry.peak_live_sample_cache_windows <= FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS);
    debug_assert!(telemetry.peak_live_sample_cache_bytes <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES);
    telemetry.pending_rebuilds = runtime.dirty_mask.count_ones() as usize;
    telemetry.dirty_mask = runtime.dirty_mask;
    telemetry.material_detail = runtime.target_material_detail[0];
    telemetry.desired_material_detail = runtime.target_material_detail;
    telemetry.resident_material_detail = runtime.resident_material_detail;
    telemetry.resident_detailed_levels = runtime
        .resident_material_detail
        .iter()
        .filter(|detail| **detail == Some(FarFieldMaterialDetail::Detailed))
        .count();
    telemetry.resident_reduced_levels = runtime
        .resident_material_detail
        .iter()
        .filter(|detail| **detail == Some(FarFieldMaterialDetail::Reduced))
        .count();
    telemetry.scheduler_deferred_frames = runtime.scheduler_deferred_frames;
    #[cfg(not(target_arch = "wasm32"))]
    {
        telemetry.build_in_flight = runtime.in_flight.is_some();
    }
    #[cfg(target_arch = "wasm32")]
    {
        telemetry.build_in_flight = false;
    }
}

const fn mesh_payload_bytes(vertices: usize, indices: usize) -> usize {
    vertices
        .saturating_mul((3 + 3 + 4 + 2) * size_of::<f32>())
        .saturating_add(indices.saturating_mul(size_of::<u32>()))
}

fn pressure_policy(
    governor: &StreamingGovernor,
    runtime: &PlanetaryStreamingRuntime,
) -> (u8, FarFieldMaterialDetail) {
    let frame_pressure = if governor.frame_pressure.is_finite() {
        governor.frame_pressure.max(0.0)
    } else {
        1.0
    };
    let queue_pressure = if governor.queue_pressure.is_finite() {
        governor.queue_pressure.max(0.0)
    } else {
        1.0
    };
    let pressure = frame_pressure.max(queue_pressure);
    let resident = runtime
        .resident_specs
        .iter()
        .filter(|spec| spec.is_some())
        .count();

    // Initial horizon fill is never delayed. Once present, old rings remain
    // visible while refresh work is deferred under pressure; extent is never
    // a quality knob and therefore requires no user optimisation.
    let cadence = if resident < FAR_FIELD_LEVELS {
        1
    } else if pressure >= 0.82 {
        4
    } else if pressure >= 0.55 {
        2
    } else {
        1
    };
    let detail = match runtime.target_material_detail[0] {
        FarFieldMaterialDetail::Detailed if pressure >= FAR_FIELD_REDUCED_DETAIL_ENTER_PRESSURE => {
            FarFieldMaterialDetail::Reduced
        }
        FarFieldMaterialDetail::Reduced if pressure <= FAR_FIELD_REDUCED_DETAIL_EXIT_PRESSURE => {
            FarFieldMaterialDetail::Detailed
        }
        current => current,
    };
    (cadence, detail)
}

fn far_field_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.94,
        metallic: 0.0,
        reflectance: 0.12,
        // Skirts have deliberately explicit outward normals, but disabling
        // culling also prevents a temporary camera-below-horizon hole during
        // high-altitude flight. The hard six-entity budget bounds the cost.
        cull_mode: None::<Face>,
        double_sided: true,
        ..default()
    }
}

fn far_field_fluid_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.72,
        metallic: 0.0,
        reflectance: 0.24,
        // Hydro v1 deliberately uses opaque vertex colours. With global MSAA
        // disabled this is depth-stable, order-independent, and avoids making
        // a descriptive horizon overlay look like simulation-grade refraction.
        alpha_mode: AlphaMode::Opaque,
        cull_mode: None::<Face>,
        double_sided: true,
        ..default()
    }
}

fn build_ring_request(scheduled: ScheduledRingBuild) -> RingBuildResult {
    let started = Instant::now();
    let request = scheduled.request;
    let generator = TerrainGenerator::new(request.world.seed)
        .with_world_profile(request.world.profile)
        .with_scenery_quality(request.world.scenery);
    let (mesh, fluid_mesh, sampling, sample_cache) =
        build_ring_mesh_incremental_with_coverage_and_hydro(
            &generator,
            request.world,
            request.spec,
            request.world.profile,
            request.material_detail,
            scheduled.sample_cache,
            request.near_coverage,
        );
    RingBuildResult {
        request,
        mesh,
        fluid_mesh,
        sample_cache,
        build_ms: started.elapsed().as_secs_f32() * 1_000.0,
        sampling,
    }
}

#[cfg(test)]
fn build_ring_mesh<S: FarFieldSampler>(
    sampler: &S,
    spec: RingSpec,
    profile: WorldProfile,
    material_detail: FarFieldMaterialDetail,
) -> (FarFieldMeshData, SamplingStats) {
    let world = FarFieldWorldKey {
        seed: 0,
        profile,
        scenery: SceneryQuality::Balanced,
        surface_material_mode: FarFieldSurfaceMaterialMode::BridgeV2,
        hydro_mode: FarFieldHydroMode::Disabled,
    };
    let (mesh, sampling, _) =
        build_ring_mesh_incremental(sampler, world, spec, profile, material_detail, None);
    (mesh, sampling)
}

#[cfg(test)]
fn build_ring_mesh_incremental<S: FarFieldSampler>(
    sampler: &S,
    world: FarFieldWorldKey,
    spec: RingSpec,
    profile: WorldProfile,
    material_detail: FarFieldMaterialDetail,
    previous_cache: Option<RingSampleCache>,
) -> (FarFieldMeshData, SamplingStats, RingSampleCache) {
    build_ring_mesh_incremental_with_coverage(
        sampler,
        world,
        spec,
        profile,
        material_detail,
        previous_cache,
        NearCoverageMask::default(),
    )
}

fn build_ring_mesh_incremental_with_coverage<S: FarFieldSampler>(
    sampler: &S,
    world: FarFieldWorldKey,
    spec: RingSpec,
    profile: WorldProfile,
    material_detail: FarFieldMaterialDetail,
    previous_cache: Option<RingSampleCache>,
    near_coverage: NearCoverageMask,
) -> (FarFieldMeshData, SamplingStats, RingSampleCache) {
    let (mesh, _, sampling, cache) = build_ring_mesh_incremental_with_coverage_and_hydro(
        sampler,
        world,
        spec,
        profile,
        material_detail,
        previous_cache,
        near_coverage,
    );
    (mesh, sampling, cache)
}

fn build_ring_mesh_incremental_with_coverage_and_hydro<S: FarFieldSampler>(
    sampler: &S,
    world: FarFieldWorldKey,
    spec: RingSpec,
    profile: WorldProfile,
    material_detail: FarFieldMaterialDetail,
    previous_cache: Option<RingSampleCache>,
    near_coverage: NearCoverageMask,
) -> (
    FarFieldMeshData,
    FarFieldMeshData,
    SamplingStats,
    RingSampleCache,
) {
    let side = FAR_FIELD_GRID_VERTICES as usize;
    let half = FAR_FIELD_GRID_CELLS / 2;
    let top_vertices = side * side;
    let mut sampling = SamplingStats::default();
    let mut sample_cache = if let Some(cache) = previous_cache {
        cache.retarget(sampler, world, spec, &mut sampling)
    } else {
        RingSampleCache::cold(
            sampler,
            world,
            spec,
            &mut sampling,
            FarFieldCacheUpdate::Cold,
        )
    };
    let mut positions = Vec::with_capacity(top_vertices + 2_048);
    let mut normals = vec![[0.0, 0.0, 0.0]; top_vertices];
    let mut colors = Vec::with_capacity(top_vertices + 2_048);
    let mut uvs = Vec::with_capacity(top_vertices + 2_048);
    let mut indices = Vec::with_capacity(24_000);
    // Bridge-v2 derives this fixed 256-byte table from the authoritative block
    // palette once per rebuild. Performing the exact sRGB transfer once per
    // biome, rather than once per vertex, removes thousands of `powf` calls
    // without duplicating authored colours or retaining process-global state.
    let bridge_v2_palette = matches!(
        (material_detail, world.surface_material_mode),
        (
            FarFieldMaterialDetail::Detailed,
            FarFieldSurfaceMaterialMode::BridgeV2
        )
    )
    .then(BridgeV2CanonicalPalette::new);
    // Biome validity lives in the same toroidal window. Existing palette
    // samples therefore survive anchor shifts while new strips remain lazy.

    for gz in -half..=half {
        for gx in -half..=half {
            let offset_x = i64::from(gx) * spec.step;
            let offset_z = i64::from(gz) * spec.step;
            let height = morphed_cached_height(&sample_cache, gx, gz, spec) + TOP_SURFACE_OFFSET
                - spec.level as f32 * LEVEL_DEPTH_BIAS;
            positions.push([offset_x as f32, height, offset_z as f32]);
            colors.push(match material_detail {
                FarFieldMaterialDetail::Detailed => match world.surface_material_mode {
                    FarFieldSurfaceMaterialMode::LegacyPalette => cached_detailed_color(
                        sampler,
                        gx,
                        gz,
                        spec,
                        profile,
                        height,
                        &mut sample_cache,
                        &mut sampling,
                    ),
                    FarFieldSurfaceMaterialMode::BridgeV1 => cached_bridge_surface_color(
                        sampler,
                        gx,
                        gz,
                        &mut sample_cache,
                        &mut sampling,
                    ),
                    FarFieldSurfaceMaterialMode::BridgeV2 => cached_bridge_v2_surface_color(
                        sampler,
                        gx,
                        gz,
                        &mut sample_cache,
                        &mut sampling,
                        bridge_v2_palette
                            .as_ref()
                            .expect("bridge-v2 detailed mesh owns its fixed palette"),
                    ),
                },
                FarFieldMaterialDetail::Reduced => match world.surface_material_mode {
                    FarFieldSurfaceMaterialMode::LegacyPalette => {
                        reduced_far_field_color(profile, height, spec.level)
                    }
                    FarFieldSurfaceMaterialMode::BridgeV1
                    | FarFieldSurfaceMaterialMode::BridgeV2 => {
                        bridge_reduced_surface_color(profile)
                    }
                },
            });
            uvs.push([
                (gx + half) as f32 / FAR_FIELD_GRID_CELLS as f32,
                (gz + half) as f32 / FAR_FIELD_GRID_CELLS as f32,
            ]);
        }
    }

    debug_assert!(sampling.material_slope_queries <= FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING);
    debug_assert!(sampling.biome_queries <= FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING);
    debug_assert!(sampling.bridge_v2_cell_reuses <= FAR_FIELD_MAX_BRIDGE_V2_CELL_REUSES_PER_RING);

    let mut included = vec![false; (FAR_FIELD_GRID_CELLS * FAR_FIELD_GRID_CELLS) as usize];
    for cz in -half..half {
        for cx in -half..half {
            let cell_index = cell_index(cx, cz);
            if cell_is_in_ring(cx, cz, spec.step, spec.inner_extent) && !near_coverage.hides(cx, cz)
            {
                included[cell_index] = true;
                let a = top_index(cx, cz);
                let b = top_index(cx + 1, cz);
                let c = top_index(cx + 1, cz + 1);
                let d = top_index(cx, cz + 1);
                // Counter-clockwise from above in Bevy's +Y-up coordinates.
                indices.extend_from_slice(&[a, d, c, a, c, b]);
            }
        }
    }

    let top_index_count = indices.len();
    accumulate_top_normals(&positions, &indices[..top_index_count], &mut normals);
    for normal in &mut normals {
        let n = Vec3::from_array(*normal).normalize_or_zero();
        *normal = if n.length_squared() > 0.0 {
            n.to_array()
        } else {
            Vec3::Y.to_array()
        };
    }

    let skirt_depth = (spec.step as f32 * 0.55).clamp(16.0, 128.0);
    for cz in -half..half {
        for cx in -half..half {
            if !included[cell_index(cx, cz)] {
                continue;
            }
            let a = top_index(cx, cz);
            let b = top_index(cx + 1, cz);
            let c = top_index(cx + 1, cz + 1);
            let d = top_index(cx, cz + 1);

            // Only close the finite outer horizon. The missing neighbour on
            // an annulus' inner boundary is the intentional near-field hole;
            // putting a downward skirt there creates a kilometre-wide wall
            // directly in front of a camera flying inside that hole.
            if cell_outside_grid(cx - 1, cz) {
                add_skirt_edge(
                    a,
                    d,
                    Vec3::NEG_X,
                    skirt_depth,
                    &mut positions,
                    &mut normals,
                    &mut colors,
                    &mut uvs,
                    &mut indices,
                );
            }
            if cell_outside_grid(cx + 1, cz) {
                add_skirt_edge(
                    c,
                    b,
                    Vec3::X,
                    skirt_depth,
                    &mut positions,
                    &mut normals,
                    &mut colors,
                    &mut uvs,
                    &mut indices,
                );
            }
            if cell_outside_grid(cx, cz - 1) {
                add_skirt_edge(
                    b,
                    a,
                    Vec3::NEG_Z,
                    skirt_depth,
                    &mut positions,
                    &mut normals,
                    &mut colors,
                    &mut uvs,
                    &mut indices,
                );
            }
            if cell_outside_grid(cx, cz + 1) {
                add_skirt_edge(
                    d,
                    c,
                    Vec3::Z,
                    skirt_depth,
                    &mut positions,
                    &mut normals,
                    &mut colors,
                    &mut uvs,
                    &mut indices,
                );
            }
        }
    }

    debug_assert_eq!(positions.len(), normals.len());
    debug_assert_eq!(positions.len(), colors.len());
    debug_assert_eq!(positions.len(), uvs.len());
    let terrain_mesh = FarFieldMeshData {
        positions,
        normals,
        colors,
        uvs,
        indices,
    };
    let fluid_mesh = if world.hydro_mode == FarFieldHydroMode::DescriptiveV1 {
        build_far_field_fluid_mesh(
            sampler,
            spec,
            near_coverage,
            &mut sample_cache,
            &mut sampling,
        )
    } else {
        FarFieldMeshData::empty()
    };
    debug_assert!(
        sampling.fluid_classification_queries
            <= FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING
    );
    debug_assert!(sampling.fluid_biome_queries <= FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING);
    (terrain_mesh, fluid_mesh, sampling, sample_cache)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FarFieldFluidKind {
    Water,
    Lava,
}

/// Shared near-column fill rule, evaluated at the existing far-field lattice.
/// Classification uses the exact cached procedural surface height, never the
/// morphed display height. This avoids changing a liquid category as LOD
/// morphing changes while keeping all signed world-coordinate handling inside
/// the established saturating/i32-clamped sampler path.
fn far_field_fluid_kind<S: FarFieldSampler>(
    sampler: &S,
    gx: i32,
    gz: i32,
    cache: &mut RingSampleCache,
    sampling: &mut SamplingStats,
) -> Option<FarFieldFluidKind> {
    sampling.fluid_classification_queries = sampling.fluid_classification_queries.saturating_add(1);
    let height = cache.height(gx, gz);
    if !height.is_finite() {
        return None;
    }
    let surface = height.floor() as i32;
    if surface >= FAR_FIELD_VOLCANIC_LAVA_LEVEL {
        return None;
    }

    let (world_x, world_z) = cache.world_coordinate(gx, gz);
    let biome = sampled_fluid_biome(sampler, world_x, world_z, sampling);
    if biome == Biome::VolcanicWaste {
        return Some(FarFieldFluidKind::Lava);
    }
    (surface < WATER_LEVEL).then_some(FarFieldFluidKind::Water)
}

fn far_field_fluid_vertex_color(kind: FarFieldFluidKind) -> [f32; 4] {
    let block = match kind {
        FarFieldFluidKind::Water => BlockType::Water,
        FarFieldFluidKind::Lava => BlockType::Lava,
    };
    let mut color = block_linear_albedo(block);
    // Hydro v1 is intentionally opaque under global `Msaa::Off`: preserve the
    // authored RGB through the exact sRGB transfer, but do not feed authored
    // translucency into an order-dependent blend path.
    color[3] = 1.0;
    color
}

fn build_far_field_fluid_mesh<S: FarFieldSampler>(
    sampler: &S,
    spec: RingSpec,
    near_coverage: NearCoverageMask,
    cache: &mut RingSampleCache,
    sampling: &mut SamplingStats,
) -> FarFieldMeshData {
    let half = FAR_FIELD_GRID_CELLS / 2;
    let side = FAR_FIELD_GRID_VERTICES as usize;
    let mut kinds = vec![None; side * side];
    for gz in -half..=half {
        for gx in -half..=half {
            kinds[top_index(gx, gz) as usize] =
                far_field_fluid_kind(sampler, gx, gz, cache, sampling);
        }
    }

    let mut positions = Vec::with_capacity(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
    let mut normals = Vec::with_capacity(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
    let mut colors = Vec::with_capacity(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
    let mut uvs = Vec::with_capacity(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
    let mut indices = Vec::with_capacity(FAR_FIELD_MAX_FLUID_INDICES_PER_RING);
    for gz in -half..=half {
        for gx in -half..=half {
            let kind = kinds[top_index(gx, gz) as usize].unwrap_or(FarFieldFluidKind::Water);
            let level = match kind {
                FarFieldFluidKind::Water => WATER_LEVEL,
                FarFieldFluidKind::Lava => FAR_FIELD_VOLCANIC_LAVA_LEVEL,
            } as f32
                + FAR_FIELD_FLUID_TOP_OFFSET_METRES
                - spec.level as f32 * FAR_FIELD_FLUID_LEVEL_DEPTH_BIAS_METRES;
            positions.push([
                (i64::from(gx) * spec.step) as f32,
                level,
                (i64::from(gz) * spec.step) as f32,
            ]);
            normals.push(Vec3::Y.to_array());
            colors.push(
                kinds[top_index(gx, gz) as usize]
                    .map(far_field_fluid_vertex_color)
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]),
            );
            uvs.push([
                (gx + half) as f32 / FAR_FIELD_GRID_CELLS as f32,
                (gz + half) as f32 / FAR_FIELD_GRID_CELLS as f32,
            ]);
        }
    }

    // A cell is fluid only when all four corner classifications agree. This
    // conservative rule prevents a single coarse wet sample from spreading a
    // lake or lava pool across an entire outer-ring cell. It also makes the
    // topology independent of iteration order and needs no connectivity pass.
    for cz in -half..half {
        for cx in -half..half {
            if !cell_is_in_ring(cx, cz, spec.step, spec.inner_extent) || near_coverage.hides(cx, cz)
            {
                continue;
            }
            let a = kinds[top_index(cx, cz) as usize];
            let b = kinds[top_index(cx + 1, cz) as usize];
            let c = kinds[top_index(cx + 1, cz + 1) as usize];
            let d = kinds[top_index(cx, cz + 1) as usize];
            let Some(_kind) = a.filter(|kind| Some(*kind) == b && b == c && c == d) else {
                continue;
            };
            let a = top_index(cx, cz);
            let b = top_index(cx + 1, cz);
            let c = top_index(cx + 1, cz + 1);
            let d = top_index(cx, cz + 1);
            indices.extend_from_slice(&[a, d, c, a, c, b]);
        }
    }
    if indices.is_empty() {
        return FarFieldMeshData::empty();
    }
    FarFieldMeshData {
        positions,
        normals,
        colors,
        uvs,
        indices,
    }
}

fn top_index(gx: i32, gz: i32) -> u32 {
    let half = FAR_FIELD_GRID_CELLS / 2;
    ((gz + half) * FAR_FIELD_GRID_VERTICES + (gx + half)) as u32
}

fn cell_index(cx: i32, cz: i32) -> usize {
    let half = FAR_FIELD_GRID_CELLS / 2;
    ((cz + half) * FAR_FIELD_GRID_CELLS + (cx + half)) as usize
}

fn cell_is_in_ring(cx: i32, cz: i32, step: i64, inner_extent: i64) -> bool {
    let center_x = i64::from(cx) * step + step / 2;
    let center_z = i64::from(cz) * step + step / 2;
    center_x.abs().max(center_z.abs()) >= inner_extent
}

fn cell_outside_grid(cx: i32, cz: i32) -> bool {
    let half = FAR_FIELD_GRID_CELLS / 2;
    cx < -half || cx >= half || cz < -half || cz >= half
}

#[allow(clippy::too_many_arguments)]
fn add_skirt_edge(
    top_a: u32,
    top_b: u32,
    outward: Vec3,
    depth: f32,
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colors: &mut Vec<[f32; 4]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
) {
    let pa = positions[top_a as usize];
    let pb = positions[top_b as usize];
    let mut ca = colors[top_a as usize];
    let mut cb = colors[top_b as usize];
    for channel in 0..3 {
        ca[channel] *= 0.58;
        cb[channel] *= 0.58;
    }
    let base = positions.len() as u32;
    positions.extend_from_slice(&[
        pa,
        pb,
        [pa[0], pa[1] - depth, pa[2]],
        [pb[0], pb[1] - depth, pb[2]],
    ]);
    normals.extend_from_slice(&[outward.to_array(); 4]);
    colors.extend_from_slice(&[ca, cb, ca, cb]);
    uvs.extend_from_slice(&[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);
    indices.extend_from_slice(&[base, base + 2, base + 3, base, base + 3, base + 1]);
}

fn accumulate_top_normals(positions: &[[f32; 3]], indices: &[u32], normals: &mut [[f32; 3]]) {
    for triangle in indices.chunks_exact(3) {
        let ia = triangle[0] as usize;
        let ib = triangle[1] as usize;
        let ic = triangle[2] as usize;
        let a = Vec3::from_array(positions[ia]);
        let b = Vec3::from_array(positions[ib]);
        let c = Vec3::from_array(positions[ic]);
        let face = (b - a).cross(c - a);
        for index in [ia, ib, ic] {
            let accumulated = Vec3::from_array(normals[index]) + face;
            normals[index] = accumulated.to_array();
        }
    }
}

fn morphed_cached_height(cache: &RingSampleCache, grid_x: i32, grid_z: i32, spec: RingSpec) -> f32 {
    let exact = cache.height(grid_x, grid_z);
    if spec.level + 1 >= FAR_FIELD_LEVELS {
        return exact;
    }
    let offset_x = i64::from(grid_x) * spec.step;
    let offset_z = i64::from(grid_z) * spec.step;
    let edge_distance = offset_x.abs().max(offset_z.abs()) as f32;
    let morph_width = (spec.step * 3) as f32;
    let morph_start = spec.outer_extent as f32 - morph_width;
    let t = smoothstep01((edge_distance - morph_start) / morph_width.max(1.0));
    if t <= 0.0 {
        return exact;
    }

    let anchor_grid_x = i128::from(spec.anchor.x).div_euclid(i128::from(spec.step));
    let anchor_grid_z = i128::from(spec.anchor.z).div_euclid(i128::from(spec.step));
    let global_x = anchor_grid_x + i128::from(grid_x);
    let global_z = anchor_grid_z + i128::from(grid_z);
    let coarse_x0 = global_x.div_euclid(2) * 2;
    let coarse_z0 = global_z.div_euclid(2) * 2;
    let (Ok(x0), Ok(z0)) = (
        i32::try_from(coarse_x0 - anchor_grid_x),
        i32::try_from(coarse_z0 - anchor_grid_z),
    ) else {
        return exact;
    };
    let x1 = x0 + 2;
    let z1 = z0 + 2;
    if [x0, x1, z0, z1].iter().any(|value| {
        !(-RingSampleCache::LOGICAL_HALF..=RingSampleCache::LOGICAL_HALF).contains(value)
    }) {
        return exact;
    }
    let tx = global_x.rem_euclid(2) as f32 * 0.5;
    let tz = global_z.rem_euclid(2) as f32 * 0.5;
    let h00 = cache.height(x0, z0);
    let h10 = cache.height(x1, z0);
    let h01 = cache.height(x0, z1);
    let h11 = cache.height(x1, z1);
    let a = h00 + (h10 - h00) * tx;
    let b = h01 + (h11 - h01) * tx;
    exact + (a + (b - a) * tz - exact) * t
}

fn sampled_height<S: FarFieldSampler>(
    sampler: &S,
    world_x: i64,
    world_z: i64,
    sampling: &mut SamplingStats,
) -> f32 {
    let (x, x_clamped) = terrain_coordinate(world_x);
    let (z, z_clamped) = terrain_coordinate(world_z);
    sampling.height_queries = sampling.height_queries.saturating_add(1);
    sampling.clamped_queries = sampling
        .clamped_queries
        .saturating_add(usize::from(x_clamped || z_clamped));
    sampler.height_at(x, z)
}

fn sampled_material_height<S: FarFieldSampler>(
    sampler: &S,
    world_x: i64,
    world_z: i64,
    sampling: &mut SamplingStats,
) -> f32 {
    let (x, x_clamped) = terrain_coordinate(world_x);
    let (z, z_clamped) = terrain_coordinate(world_z);
    sampling.material_slope_queries = sampling.material_slope_queries.saturating_add(1);
    sampling.clamped_queries = sampling
        .clamped_queries
        .saturating_add(usize::from(x_clamped || z_clamped));
    sampler.height_at(x, z)
}

fn sampled_biome<S: FarFieldSampler>(
    sampler: &S,
    world_x: i64,
    world_z: i64,
    sampling: &mut SamplingStats,
) -> Biome {
    let (x, x_clamped) = terrain_coordinate(world_x);
    let (z, z_clamped) = terrain_coordinate(world_z);
    sampling.biome_queries = sampling.biome_queries.saturating_add(1);
    sampling.clamped_queries = sampling
        .clamped_queries
        .saturating_add(usize::from(x_clamped || z_clamped));
    sampler.biome_at(x, z)
}

fn sampled_fluid_biome<S: FarFieldSampler>(
    sampler: &S,
    world_x: i64,
    world_z: i64,
    sampling: &mut SamplingStats,
) -> Biome {
    let (x, x_clamped) = terrain_coordinate(world_x);
    let (z, z_clamped) = terrain_coordinate(world_z);
    sampling.fluid_biome_queries = sampling.fluid_biome_queries.saturating_add(1);
    sampling.clamped_queries = sampling
        .clamped_queries
        .saturating_add(usize::from(x_clamped || z_clamped));
    sampler.biome_at(x, z)
}

#[allow(clippy::too_many_arguments)]
fn cached_detailed_color<S: FarFieldSampler>(
    sampler: &S,
    grid_x: i32,
    grid_z: i32,
    spec: RingSpec,
    profile: WorldProfile,
    height: f32,
    cache: &mut RingSampleCache,
    sampling: &mut SamplingStats,
) -> [f32; 4] {
    let stride_cells = if spec.level == 0 { 2_i32 } else { 4_i32 };
    let x0 = grid_x.div_euclid(stride_cells) * stride_cells;
    let z0 = grid_z.div_euclid(stride_cells) * stride_cells;
    let x1 = x0 + stride_cells;
    let z1 = z0 + stride_cells;
    let tx = grid_x.rem_euclid(stride_cells) as f32 / stride_cells as f32;
    let tz = grid_z.rem_euclid(stride_cells) as f32 / stride_cells as f32;

    let mut palette = |gx: i32, gz: i32| {
        let biome = cache.biome_at_or_sample(sampler, gx, gz, sampling);
        far_field_color(profile, biome, height, spec.level)
    };

    let c00 = palette(x0, z0);
    let c10 = palette(x1, z0);
    let c01 = palette(x0, z1);
    let c11 = palette(x1, z1);
    let a = lerp_rgba(c00, c10, tx);
    let b = lerp_rgba(c01, c11, tx);
    lerp_rgba(a, b, tz)
}

/// Bridge-v1 samples the exact absolute integer-world vertex and the same
/// one-metre cardinal slope convention as near terrain. This deliberately
/// replaces ring-local four-sample voting: no four samples whose phase/stride
/// changes with anchor or LOD can guarantee a general categorical equality.
///
/// The smallest bounded representation is two fixed 4,225-cell arrays in the
/// existing toroidal cache (one `BlockType`, one validity bit). A cold detailed
/// ring is capped at 3,721 biome plus 14,884 material-slope queries; retargets
/// reuse overlapping families and query only the entering visible strip.
/// Geometry, entities, task count, and cache-window count remain unchanged.
/// The known failure mode is cold-build CPU cost, contained by the one-job
/// scheduler, pressure-driven Reduced fallback, and separately visible slope
/// query telemetry. The ignored A/B distribution benchmark records that cost
/// against the legacy baseline instead of assuming the fixed cap is fast.
/// `VOXEL_NATIVE_FAR_SURFACE_MATERIAL=bridge-v1` retains this reference path;
/// `legacy` remains the old interpolated-palette rollback.
fn cached_bridge_surface_color<S: FarFieldSampler>(
    sampler: &S,
    grid_x: i32,
    grid_z: i32,
    cache: &mut RingSampleCache,
    sampling: &mut SamplingStats,
) -> [f32; 4] {
    let family = cache.surface_family_at_or_sample(sampler, grid_x, grid_z, sampling);
    block_linear_albedo(family)
}

const BRIDGE_V2_BIOME_COUNT: usize = 16;

/// Fixed per-ring canonical albedo table used by bridge-v2.
///
/// Three bounded approaches were evaluated for the cold-build hot path:
///
/// 1. Bridge-v1's exact one-metre slope probes preserve near-family equality,
///    but require 14,884 additional procedural height queries per cold ring.
/// 2. Deriving slope from the existing clipmap heights is query-free, but its
///    classification changes with LOD step and therefore violates categorical
///    equality at shared vertices.
/// 3. Sampling one fixed absolute 128 m material cell and selecting its
///    canonical base family is LOD/anchor/retarget stable, removes every slope
///    query, and permits O(1) neighbour reuse without an allocation. A fixed
///    table also amortizes the relatively expensive sRGB transfer while still
///    deriving every value from `BlockType::color()` on each rebuild.
///
/// The third approach is bridge-v2. Its explicit loss is slope-specific Dirt,
/// Stone, and Limestone accents; geometry normals still provide lighting
/// relief, but never influence the categorical family. Bridge-v1 is kept as an
/// exact diagnostic and the legacy palette as a visual rollback. A cell's
/// representative can be 64 m away on either axis; visual QA must reject the
/// mode if that broad categorical transition reads as blocky at flight range.
#[derive(Clone, Copy)]
struct BridgeV2CanonicalPalette {
    by_biome: [[f32; 4]; BRIDGE_V2_BIOME_COUNT],
}

const _: () =
    assert!(size_of::<BridgeV2CanonicalPalette>() == BRIDGE_V2_BIOME_COUNT * 4 * size_of::<f32>());

impl BridgeV2CanonicalPalette {
    fn new() -> Self {
        const BIOMES: [Biome; BRIDGE_V2_BIOME_COUNT] = [
            Biome::Ocean,
            Biome::Beach,
            Biome::Plains,
            Biome::Forest,
            Biome::Jungle,
            Biome::Desert,
            Biome::Savanna,
            Biome::Tundra,
            Biome::SnowyMountains,
            Biome::Mountains,
            Biome::Mesa,
            Biome::Karst,
            Biome::CrystalSpires,
            Biome::VolcanicWaste,
            Biome::GlacierShards,
            Biome::AlienReef,
        ];
        Self {
            by_biome: BIOMES.map(|biome| block_linear_albedo(coarse_surface_family(biome, 0.0))),
        }
    }

    #[inline]
    fn color(&self, biome: Biome) -> [f32; 4] {
        // Exhaustive matching makes a future biome addition fail compilation
        // instead of silently indexing a stale hand-authored palette.
        let index = match biome {
            Biome::Ocean => 0,
            Biome::Beach => 1,
            Biome::Plains => 2,
            Biome::Forest => 3,
            Biome::Jungle => 4,
            Biome::Desert => 5,
            Biome::Savanna => 6,
            Biome::Tundra => 7,
            Biome::SnowyMountains => 8,
            Biome::Mountains => 9,
            Biome::Mesa => 10,
            Biome::Karst => 11,
            Biome::CrystalSpires => 12,
            Biome::VolcanicWaste => 13,
            Biome::GlacierShards => 14,
            Biome::AlienReef => 15,
        };
        self.by_biome[index]
    }
}

/// Fast categorical bridge: every top vertex resolves one absolute 128 m
/// material cell, no material-height query, no interpolation, and no
/// LOD-dependent geometry-normal feedback. Adjacent vertices in one cell share
/// its single biome query; the toroidal biome cache preserves the result across
/// retargets, so entering strips query only previously unseen cells.
fn cached_bridge_v2_surface_color<S: FarFieldSampler>(
    sampler: &S,
    grid_x: i32,
    grid_z: i32,
    cache: &mut RingSampleCache,
    sampling: &mut SamplingStats,
    palette: &BridgeV2CanonicalPalette,
) -> [f32; 4] {
    let biome = cache.bridge_v2_biome_at_or_sample(sampler, grid_x, grid_z, sampling);
    palette.color(biome)
}

/// Canonical block colors are authored in sRGB. Bevy's `to_linear` applies
/// the IEC 61966-2-1 transfer exactly once before the values enter PBR vertex
/// albedo; no duplicate far-field palette or level tint participates.
fn block_linear_albedo(block: BlockType) -> [f32; 4] {
    block.color().to_linear().to_f32_array()
}

/// Pressure fallback keeps the complete geometry/horizon and adds no material
/// query. Existing fixed-budget height sampling still builds the silhouette;
/// biome sampling remains zero. One canonical block family per world profile
/// replaces a manufactured averaged emergency palette.
fn bridge_reduced_surface_color(profile: WorldProfile) -> [f32; 4] {
    let family = match profile {
        WorldProfile::Natural => BlockType::Grass,
        WorldProfile::AstralFrontier => BlockType::GlowSand,
    };
    block_linear_albedo(family)
}

fn lerp_rgba(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

fn bridge_v2_material_sample_coordinate(value: i64) -> i64 {
    let quantum = FAR_FIELD_BRIDGE_V2_MATERIAL_CELL_METRES;
    debug_assert!(quantum > 0);
    value
        .div_euclid(quantum)
        .saturating_mul(quantum)
        .saturating_add(quantum / 2)
}

fn terrain_coordinate(value: i64) -> (i32, bool) {
    // Leave room for the coarser bilinear neighbour. This turns generator
    // limits into explicit saturation rather than signed overflow near i32
    // world edges; telemetry exposes any saturation to QA.
    const MARGIN: i64 = 4_096;
    let bounded = value.clamp(i64::from(i32::MIN) + MARGIN, i64::from(i32::MAX) - MARGIN);
    (bounded as i32, bounded != value)
}

fn far_field_color(profile: WorldProfile, biome: Biome, height: f32, level: usize) -> [f32; 4] {
    let base = match (profile, biome) {
        (_, Biome::Ocean) => [0.07, 0.30, 0.48],
        (_, Biome::Beach) => [0.60, 0.53, 0.32],
        (WorldProfile::Natural, Biome::Plains) => [0.30, 0.54, 0.24],
        (WorldProfile::Natural, Biome::Forest) => [0.13, 0.38, 0.18],
        (WorldProfile::Natural, Biome::Jungle) => [0.10, 0.43, 0.22],
        (WorldProfile::Natural, Biome::Desert) => [0.67, 0.50, 0.26],
        (WorldProfile::Natural, Biome::Savanna) => [0.54, 0.52, 0.22],
        (WorldProfile::Natural, Biome::Tundra) => [0.39, 0.45, 0.39],
        (WorldProfile::Natural, Biome::SnowyMountains) => [0.74, 0.80, 0.82],
        (WorldProfile::Natural, Biome::Mountains) => [0.34, 0.39, 0.40],
        (WorldProfile::Natural, Biome::Mesa) => [0.58, 0.28, 0.17],
        (WorldProfile::Natural, Biome::Karst) => [0.22, 0.46, 0.25],
        (WorldProfile::Natural, Biome::CrystalSpires) => [0.20, 0.55, 0.64],
        (WorldProfile::Natural, Biome::VolcanicWaste) => [0.28, 0.20, 0.18],
        (WorldProfile::Natural, Biome::GlacierShards) => [0.58, 0.72, 0.78],
        (WorldProfile::Natural, Biome::AlienReef) => [0.38, 0.25, 0.50],
        (WorldProfile::AstralFrontier, Biome::CrystalSpires) => [0.18, 0.70, 0.82],
        (WorldProfile::AstralFrontier, Biome::VolcanicWaste) => [0.36, 0.16, 0.12],
        (WorldProfile::AstralFrontier, Biome::GlacierShards) => [0.55, 0.78, 0.88],
        (WorldProfile::AstralFrontier, Biome::AlienReef) => [0.42, 0.24, 0.58],
        (WorldProfile::AstralFrontier, Biome::Mesa) => [0.65, 0.29, 0.16],
        (WorldProfile::AstralFrontier, Biome::Karst) => [0.20, 0.56, 0.30],
        (WorldProfile::AstralFrontier, Biome::Plains) => [0.35, 0.56, 0.25],
        (WorldProfile::AstralFrontier, Biome::Forest) => [0.13, 0.43, 0.24],
        (WorldProfile::AstralFrontier, Biome::Jungle) => [0.12, 0.50, 0.29],
        (WorldProfile::AstralFrontier, Biome::Desert) => [0.73, 0.46, 0.24],
        (WorldProfile::AstralFrontier, Biome::Savanna) => [0.51, 0.55, 0.24],
        (WorldProfile::AstralFrontier, Biome::Tundra) => [0.37, 0.48, 0.50],
        (WorldProfile::AstralFrontier, Biome::SnowyMountains) => [0.72, 0.82, 0.88],
        (WorldProfile::AstralFrontier, Biome::Mountains) => [0.31, 0.37, 0.45],
    };
    let relief = ((height - 48.0) / 180.0).clamp(-0.12, 0.18);
    far_field_linear_albedo(base, relief, 0.7, level)
}

fn reduced_far_field_color(profile: WorldProfile, height: f32, level: usize) -> [f32; 4] {
    let base = match profile {
        WorldProfile::Natural => [0.27, 0.46, 0.29],
        WorldProfile::AstralFrontier => [0.31, 0.42, 0.50],
    };
    let relief = ((height - 48.0) / 190.0).clamp(-0.10, 0.16);
    far_field_linear_albedo(base, relief, 0.65, level)
}

/// A near column is safe to cut out of the fallback parent only after every
/// potentially visible vertical chunk belongs to the current request and has
/// current voxel data. An already spawned mesh remains valid visual coverage
/// while its replacement is pending or dirty; keeping the coarse parent below
/// it would create the very overlap this handshake prevents. A chunk without
/// a spawned mesh must instead be fully settled, which covers legitimate
/// empty/uniform chunks without punching a hole during generation.
fn near_column_is_visually_ready(
    world: &VoxelWorld,
    streamer: &ChunkStreamer,
    cx: i32,
    cz: i32,
    vertical_chunks: i32,
) -> bool {
    if vertical_chunks <= 0 {
        return false;
    }
    let Some(&top_cy) = world.column_top_cy.get(&(cx, cz)) else {
        return false;
    };
    let top_cy = top_cy.clamp(-1, vertical_chunks - 1);

    // Membership of the base slot proves this column belongs to the current
    // request plan. Otherwise old resident metadata must never punch a hole.
    let base = ChunkPos::new(cx, 0, cz);
    if !streamer.requested_chunks.contains(&base) {
        return false;
    }

    (0..=top_cy).all(|cy| {
        let pos = ChunkPos::new(cx, cy, cz);
        if !streamer.requested_chunks.contains(&pos) || !world.chunks.contains_key(&pos) {
            return false;
        }
        if streamer
            .entities
            .get(&pos)
            .is_some_and(|entities| !entities.is_empty())
        {
            return true;
        }
        !streamer.pending_terrain.contains_key(&pos)
            && !streamer.pending_meshes.contains_key(&pos)
            && !streamer.dirty_queue.contains(&pos)
            && !world.edit_dirty_chunks.contains(&pos)
    })
}

/// Build the exact irregular finest-parent cutout from a fixed camera-centred
/// readiness window. Every 32 m parent cell spans four 16 m chunk columns and
/// is hidden only if all four are proven ready. Missing, out-of-window, or
/// unrepresentable coordinates fail closed and leave the fallback visible.
/// The closure is evaluated at most once for each of the fixed 33x33 columns.
fn build_near_coverage_snapshot<F>(
    finest_spec: RingSpec,
    camera_block_x: i32,
    camera_block_z: i32,
    max_radius_chunks: i32,
    mut column_ready: F,
) -> NearCoverageSnapshot
where
    F: FnMut(i32, i32) -> bool,
{
    debug_assert_eq!(finest_spec.level, 0);
    debug_assert_eq!(finest_spec.step, FAR_FIELD_BASE_STEP_METRES);

    let hard_radius = crate::world::MAX_INTERACTION_RADIUS_CHUNKS;
    let active_radius = max_radius_chunks.max(0).min(hard_radius);
    let camera_cx = camera_block_x.div_euclid(CHUNK_SIZE_I);
    let camera_cz = camera_block_z.div_euclid(CHUNK_SIZE_I);
    let mut ready = [false; NEAR_COVERAGE_COLUMNS];
    let mut ready_columns = 0usize;

    for dz in -hard_radius..=hard_radius {
        for dx in -hard_radius..=hard_radius {
            if dx.abs() > active_radius || dz.abs() > active_radius {
                continue;
            }
            let cx = camera_cx.saturating_add(dx);
            let cz = camera_cz.saturating_add(dz);
            let index = near_coverage_column_index(dx, dz);
            ready[index] = column_ready(cx, cz);
            ready_columns = ready_columns.saturating_add(usize::from(ready[index]));
        }
    }

    let lookup = |cx: i32, cz: i32| {
        let dx = i64::from(cx) - i64::from(camera_cx);
        let dz = i64::from(cz) - i64::from(camera_cz);
        if dx < -i64::from(hard_radius)
            || dx > i64::from(hard_radius)
            || dz < -i64::from(hard_radius)
            || dz > i64::from(hard_radius)
        {
            return false;
        }
        ready[near_coverage_column_index(dx as i32, dz as i32)]
    };

    let confirmed_square_extent_metres =
        confirmed_near_visual_extent(camera_block_x, camera_block_z, active_radius, lookup);
    let mut mask = NearCoverageMask::default();
    let half = FAR_FIELD_GRID_CELLS / 2;
    let chunk_size = i64::from(CHUNK_SIZE_I);
    for cz in -half..half {
        for cx in -half..half {
            let min_x = finest_spec
                .anchor
                .x
                .saturating_add(i64::from(cx).saturating_mul(finest_spec.step));
            let min_z = finest_spec
                .anchor
                .z
                .saturating_add(i64::from(cz).saturating_mul(finest_spec.step));
            let max_x = min_x.saturating_add(finest_spec.step.saturating_sub(1));
            let max_z = min_z.saturating_add(finest_spec.step.saturating_sub(1));
            let Some((min_cx, max_cx, min_cz, max_cz)) =
                i32::try_from(min_x.div_euclid(chunk_size))
                    .ok()
                    .zip(i32::try_from(max_x.div_euclid(chunk_size)).ok())
                    .zip(i32::try_from(min_z.div_euclid(chunk_size)).ok())
                    .zip(i32::try_from(max_z.div_euclid(chunk_size)).ok())
                    .map(|(((min_cx, max_cx), min_cz), max_cz)| (min_cx, max_cx, min_cz, max_cz))
            else {
                continue;
            };

            let covered = (min_cz..=max_cz)
                .all(|near_cz| (min_cx..=max_cx).all(|near_cx| lookup(near_cx, near_cz)));
            if covered {
                mask.hide(cx, cz);
            }
        }
    }

    NearCoverageSnapshot {
        mask,
        confirmed_square_extent_metres,
        ready_columns,
    }
}

fn near_coverage_column_index(dx: i32, dz: i32) -> usize {
    debug_assert!(dx.abs() <= crate::world::MAX_INTERACTION_RADIUS_CHUNKS);
    debug_assert!(dz.abs() <= crate::world::MAX_INTERACTION_RADIUS_CHUNKS);
    let radius = crate::world::MAX_INTERACTION_RADIUS_CHUNKS;
    ((dz + radius) as usize) * NEAR_COVERAGE_SIDE + (dx + radius) as usize
}

/// Largest camera-centred square with proven near-field visual coverage,
/// quantised down to the finest clipmap lattice. Only the newly added square
/// perimeter is examined at each radius, so the bounded scan visits each
/// admitted column at most once. A missing column stops growth immediately.
fn confirmed_near_visual_extent<F>(
    camera_block_x: i32,
    camera_block_z: i32,
    max_radius_chunks: i32,
    mut column_ready: F,
) -> i64
where
    F: FnMut(i32, i32) -> bool,
{
    let max_radius = max_radius_chunks
        .max(0)
        .min(crate::world::MAX_INTERACTION_RADIUS_CHUNKS);
    let pcx = camera_block_x.div_euclid(CHUNK_SIZE_I);
    let pcz = camera_block_z.div_euclid(CHUNK_SIZE_I);
    let mut confirmed_radius = -1_i32;

    for radius in 0..=max_radius {
        let boundary_ready = if radius == 0 {
            column_ready(pcx, pcz)
        } else {
            let north_south = (-radius..=radius).all(|dx| {
                column_ready(pcx.saturating_add(dx), pcz.saturating_sub(radius))
                    && column_ready(pcx.saturating_add(dx), pcz.saturating_add(radius))
            });
            let east_west = (-(radius - 1)..=(radius - 1)).all(|dz| {
                column_ready(pcx.saturating_sub(radius), pcz.saturating_add(dz))
                    && column_ready(pcx.saturating_add(radius), pcz.saturating_add(dz))
            });
            north_south && east_west
        };
        if !boundary_ready {
            break;
        }
        confirmed_radius = radius;
    }

    if confirmed_radius < 0 {
        return FAR_FIELD_FINEST_INNER_EXTENT_METRES;
    }
    let radius = i64::from(confirmed_radius);
    let chunk = i64::from(CHUNK_SIZE_I);
    let left = (i64::from(pcx) - radius) * chunk;
    let right = (i64::from(pcx) + radius + 1) * chunk;
    let near_z = (i64::from(pcz) - radius) * chunk;
    let far_z = (i64::from(pcz) + radius + 1) * chunk;
    let camera_x = i64::from(camera_block_x);
    let camera_z = i64::from(camera_block_z);
    let conservative_extent = (camera_x - left)
        .min(right - camera_x)
        .min(camera_z - near_z)
        .min(far_z - camera_z)
        .max(0);
    let quantized =
        conservative_extent.div_euclid(FAR_FIELD_BASE_STEP_METRES) * FAR_FIELD_BASE_STEP_METRES;
    let finest_outer = (FAR_FIELD_GRID_CELLS as i64 / 2) * FAR_FIELD_BASE_STEP_METRES;
    quantized.clamp(
        FAR_FIELD_FINEST_INNER_EXTENT_METRES,
        finest_outer - FAR_FIELD_BASE_STEP_METRES * 2,
    )
}

/// Convert the authored display palette into the linear albedo required by
/// Bevy's PBR path. Vertex attributes are linear-light values; feeding the
/// original sRGB numbers directly made a 0.5 swatch carry 0.5 energy instead
/// of about 0.214 and clipped most daylight terrain toward white.
fn far_field_linear_albedo(
    base_srgb: [f32; 3],
    relief_srgb: f32,
    blue_relief_scale: f32,
    level: usize,
) -> [f32; 4] {
    let atmospheric = (1.0 - level as f32 * 0.035).clamp(0.0, 1.0);
    let corrected = [
        srgb_channel_to_linear((base_srgb[0] + relief_srgb).clamp(0.0, 1.0)),
        srgb_channel_to_linear((base_srgb[1] + relief_srgb).clamp(0.0, 1.0)),
        srgb_channel_to_linear((base_srgb[2] + relief_srgb * blue_relief_scale).clamp(0.0, 1.0)),
    ];
    [
        corrected[0] * atmospheric,
        corrected[1] * atmospheric,
        corrected[2] * atmospheric,
        1.0,
    ]
}

fn srgb_channel_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn snap_world_coordinate(value: i64, quantum: i64) -> i64 {
    debug_assert!(quantum > 0);
    value.div_euclid(quantum) * quantum
}

fn finite_floor_i64(value: f32) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    // `f32` cannot encode either i64 endpoint exactly. Saturating to a broad
    // safe interval prevents a cast from depending on platform edge behavior.
    value.floor().clamp(-9.0e15, 9.0e15) as i64
}

fn relative_f32(world: i64, origin: i64) -> f32 {
    world.saturating_sub(origin) as f32
}

fn smoothstep01(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::IntoSystem;

    #[derive(Resource, Default)]
    struct DeferredWorldSwapHarness {
        phase: u8,
        queued_despawns: usize,
        installed_replacement: bool,
        reported_resident: bool,
    }

    #[derive(Resource, Default)]
    struct ResidentObservationTransitionHarness {
        phase: u8,
        entity: Option<Entity>,
    }

    fn test_far_field_ring(level: usize, vertices: usize, indices: usize) -> FarFieldRing {
        FarFieldRing {
            level,
            anchor: WorldXZ::new(level as i64 * 64, -(level as i64) * 64),
            material_detail: FarFieldMaterialDetail::Detailed,
            vertices,
            indices,
        }
    }

    fn resident_observation_transition_harness(
        mut commands: Commands,
        mut state: ResMut<ResidentObservationTransitionHarness>,
        mut runtime: ResMut<PlanetaryStreamingRuntime>,
    ) {
        let level = 2;
        if state.phase == 0 {
            let spec = RingSpec::for_level(level, 0, WorldXZ::ZERO);
            state.entity = Some(commands.spawn(test_far_field_ring(level, 321, 654)).id());
            runtime.resident_specs[level] = Some(spec);
            runtime.resident_vertices[level] = 321;
            runtime.resident_indices[level] = 654;
            state.phase = 1;
        } else if state.phase == 1 {
            commands
                .entity(state.entity.take().expect("spawned ring entity"))
                .despawn();
            runtime.resident_specs[level] = None;
            runtime.resident_vertices[level] = 0;
            runtime.resident_indices[level] = 0;
            state.phase = 2;
        }
    }

    fn refresh_scheduler_telemetry_for_test(
        runtime: Res<PlanetaryStreamingRuntime>,
        mut telemetry: ResMut<PlanetaryStreamingTelemetry>,
    ) {
        refresh_telemetry(&runtime, &mut telemetry);
    }

    fn synchronous_world_swap_harness(
        mut commands: Commands,
        mut meshes: ResMut<Assets<Mesh>>,
        mut state: ResMut<DeferredWorldSwapHarness>,
        mut rings: Query<
            (Entity, &mut FarFieldRing, &mut Handle<Mesh>, &mut Transform),
            Without<ChunkAnchor>,
        >,
        mut fluid_rings: Query<
            (
                Entity,
                &mut FarFieldFluidRing,
                &mut Handle<Mesh>,
                &mut Transform,
            ),
            (Without<ChunkAnchor>, Without<FarFieldRing>),
        >,
    ) {
        if state.phase == 0 {
            state.queued_despawns =
                clear_render_rings(&mut commands, &mut meshes, &mut rings, &mut fluid_rings);
            state.phase = 1;
            if state.queued_despawns != 0 {
                return;
            }
        }
        if state.phase == 1 {
            commands.spawn((
                FarFieldRing {
                    level: 0,
                    anchor: WorldXZ::new(64, -64),
                    material_detail: FarFieldMaterialDetail::Detailed,
                    vertices: 1,
                    indices: 0,
                },
                Handle::<Mesh>::default(),
                Transform::default(),
            ));
            state.installed_replacement = true;
            state.reported_resident = true;
            state.phase = 2;
        }
    }

    #[test]
    fn update_system_transform_queries_are_schedule_disjoint() {
        // Bevy validates query aliasing when a system is initialized. This
        // regression test catches a runtime-only B0001 panic before a real
        // engine window is launched; resources are not needed for parameter
        // initialization.
        let mut world = World::new();
        let mut system = IntoSystem::into_system(update_planetary_streaming);
        system.initialize(&mut world);
    }

    #[test]
    fn synchronous_world_swap_waits_for_deferred_despawn_flush() {
        let mut world = World::new();
        world.insert_resource(Assets::<Mesh>::default());
        world.insert_resource(DeferredWorldSwapHarness::default());
        let old = world
            .spawn((
                FarFieldRing {
                    level: 0,
                    anchor: WorldXZ::new(-64, 64),
                    material_detail: FarFieldMaterialDetail::Reduced,
                    vertices: 1,
                    indices: 0,
                },
                Handle::<Mesh>::default(),
                Transform::default(),
            ))
            .id();
        let mut schedule = Schedule::default();
        schedule.add_systems(synchronous_world_swap_harness);

        schedule.run(&mut world);
        let first = world.resource::<DeferredWorldSwapHarness>();
        let (queued_despawns, installed_replacement, reported_resident) = (
            first.queued_despawns,
            first.installed_replacement,
            first.reported_resident,
        );
        assert_eq!(queued_despawns, 1);
        assert!(!installed_replacement);
        assert!(!reported_resident);
        assert!(world.get_entity(old).is_none());
        assert_eq!(
            world
                .query_filtered::<Entity, With<FarFieldRing>>()
                .iter(&world)
                .count(),
            0
        );

        schedule.run(&mut world);
        let installed_replacement = world
            .resource::<DeferredWorldSwapHarness>()
            .installed_replacement;
        assert!(installed_replacement);
        assert_eq!(
            world
                .query_filtered::<Entity, With<FarFieldRing>>()
                .iter(&world)
                .count(),
            1
        );
    }

    #[test]
    fn ecs_residency_observer_sees_spawn_and_despawn_after_deferred_barrier() {
        let mut world = World::new();
        world.insert_resource(PlanetaryStreamingRuntime::default());
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        world.insert_resource(ResidentObservationTransitionHarness::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                resident_observation_transition_harness,
                refresh_scheduler_telemetry_for_test,
                apply_deferred,
                observe_planetary_residency,
            )
                .chain(),
        );

        schedule.run(&mut world);
        let spawned = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(spawned.resident_entities, 1);
        assert_eq!(spawned.ring_vertices[2], 321);
        assert_eq!(spawned.ring_indices[2], 654);
        assert_eq!(spawned.resident_vertices, 321);
        assert_eq!(spawned.resident_indices, 654);
        assert!(spawned.resident_observation_valid);
        assert!(!spawned.resident_scheduler_mismatch);

        schedule.run(&mut world);
        let despawned = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(despawned.resident_entities, 0);
        assert_eq!(despawned.ring_vertices, [0; FAR_FIELD_LEVELS]);
        assert_eq!(despawned.ring_indices, [0; FAR_FIELD_LEVELS]);
        assert_eq!(despawned.scheduler_resident_entities, 0);
        assert!(despawned.resident_observation_valid);
        assert_eq!(despawned.resident_observation_rejections, 0);
    }

    #[test]
    fn ecs_residency_observer_rejects_duplicate_level_without_order_dependence() {
        let mut world = World::new();
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        world.spawn(test_far_field_ring(1, 40, 90));
        world.spawn(test_far_field_ring(1, 60, 110));
        let mut schedule = Schedule::default();
        schedule.add_systems(observe_planetary_residency);

        schedule.run(&mut world);
        let first = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(first.resident_entities, 2);
        assert_eq!(first.ring_vertices[1], 100);
        assert_eq!(first.ring_indices[1], 200);
        assert_eq!(first.resident_duplicate_levels, 1);
        assert!(first.resident_scheduler_mismatch);
        assert!(!first.resident_observation_valid);
        assert_eq!(first.resident_observation_rejections, 1);

        // A persistent invalid state is one rejection episode, not one event
        // per frame.
        schedule.run(&mut world);
        assert_eq!(
            world
                .resource::<PlanetaryStreamingTelemetry>()
                .resident_observation_rejections,
            1
        );
    }

    #[test]
    fn ecs_residency_observer_rejects_out_of_range_and_overflow_fail_closed() {
        let mut world = World::new();
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        world.spawn(test_far_field_ring(FAR_FIELD_LEVELS, 1, 2));
        for level in 0..FAR_FIELD_LEVELS {
            world.spawn(test_far_field_ring(level, 10 + level, 20 + level));
        }
        // An eighth entity proves the observer stops at its seventh sentinel,
        // regardless of how large a duplicate-spawn bug becomes.
        world.spawn(test_far_field_ring(0, 999, 999));
        let mut schedule = Schedule::default();
        schedule.add_systems(observe_planetary_residency);
        schedule.run(&mut world);

        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(
            telemetry.resident_entities,
            FAR_FIELD_OBSERVATION_SCAN_LIMIT
        );
        assert!(telemetry.resident_entity_count_overflow);
        assert!(telemetry.resident_budget_exceeded);
        assert_eq!(telemetry.resident_vertices, usize::MAX);
        assert_eq!(telemetry.resident_indices, usize::MAX);
        assert_eq!(telemetry.ring_vertices, [usize::MAX; FAR_FIELD_LEVELS]);
        assert_eq!(telemetry.ring_indices, [usize::MAX; FAR_FIELD_LEVELS]);
        assert!(!telemetry.resident_observation_valid);
    }

    #[test]
    fn ecs_residency_observer_rejects_an_out_of_range_level_exactly() {
        let mut world = World::new();
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        world.spawn(test_far_field_ring(FAR_FIELD_LEVELS + 41, 7, 13));
        let mut schedule = Schedule::default();
        schedule.add_systems(observe_planetary_residency);
        schedule.run(&mut world);

        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(telemetry.resident_entities, 1);
        assert_eq!(telemetry.resident_vertices, 7);
        assert_eq!(telemetry.resident_indices, 13);
        assert_eq!(telemetry.ring_vertices, [0; FAR_FIELD_LEVELS]);
        assert_eq!(telemetry.ring_indices, [0; FAR_FIELD_LEVELS]);
        assert_eq!(telemetry.resident_out_of_range_levels, 1);
        assert!(!telemetry.resident_entity_count_overflow);
        assert!(!telemetry.resident_observation_valid);
    }

    #[test]
    fn ecs_residency_observer_rejects_payload_over_budget_even_when_scheduler_matches() {
        let mut world = World::new();
        let mut runtime = PlanetaryStreamingRuntime::default();
        let level = 0;
        let vertices = FAR_FIELD_MAX_VERTICES + 1;
        runtime.resident_specs[level] = Some(RingSpec::for_level(level, 0, WorldXZ::ZERO));
        runtime.resident_vertices[level] = vertices;
        world.spawn(test_far_field_ring(level, vertices, 0));
        world.insert_resource(runtime);
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                refresh_scheduler_telemetry_for_test,
                observe_planetary_residency,
            )
                .chain(),
        );
        schedule.run(&mut world);

        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert!(!telemetry.resident_scheduler_mismatch);
        assert!(telemetry.resident_budget_exceeded);
        assert!(!telemetry.resident_observation_valid);
        assert_eq!(telemetry.resident_observation_rejections, 1);
    }

    #[test]
    fn ecs_residency_observer_is_exact_and_stable_for_six_unique_levels() {
        let mut world = World::new();
        let mut runtime = PlanetaryStreamingRuntime::default();
        let expected_vertices: [usize; FAR_FIELD_LEVELS] =
            std::array::from_fn(|level| 100 + level * 7);
        let expected_indices: [usize; FAR_FIELD_LEVELS] =
            std::array::from_fn(|level| 300 + level * 11);
        for level in 0..FAR_FIELD_LEVELS {
            runtime.resident_specs[level] = Some(RingSpec::for_level(level, 0, WorldXZ::ZERO));
            runtime.resident_vertices[level] = expected_vertices[level];
            runtime.resident_indices[level] = expected_indices[level];
            world.spawn(test_far_field_ring(
                level,
                expected_vertices[level],
                expected_indices[level],
            ));
        }
        world.insert_resource(runtime);
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                refresh_scheduler_telemetry_for_test,
                observe_planetary_residency,
            )
                .chain(),
        );

        schedule.run(&mut world);
        schedule.run(&mut world);
        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(telemetry.resident_entities, FAR_FIELD_LEVELS);
        assert_eq!(telemetry.ring_vertices, expected_vertices);
        assert_eq!(telemetry.ring_indices, expected_indices);
        assert_eq!(
            telemetry.resident_vertices,
            expected_vertices.iter().sum::<usize>()
        );
        assert_eq!(
            telemetry.resident_indices,
            expected_indices.iter().sum::<usize>()
        );
        assert_eq!(telemetry.resident_duplicate_levels, 0);
        assert_eq!(telemetry.resident_out_of_range_levels, 0);
        assert!(!telemetry.resident_entity_count_overflow);
        assert!(!telemetry.resident_scheduler_mismatch);
        assert!(!telemetry.resident_budget_exceeded);
        assert!(telemetry.resident_observation_valid);
        assert_eq!(telemetry.resident_observation_rejections, 0);
    }

    #[test]
    fn ecs_fluid_observer_is_exact_and_rejects_duplicate_or_over_budget_state() {
        let mut world = World::new();
        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.resident_fluid_vertices[1] = 100;
        runtime.resident_fluid_indices[1] = 300;
        world.spawn(FarFieldFluidRing {
            level: 1,
            anchor: WorldXZ::new(-64, 64),
            vertices: 100,
            indices: 300,
        });
        world.insert_resource(runtime);
        world.insert_resource(PlanetaryStreamingTelemetry::default());
        let mut schedule = Schedule::default();
        schedule.add_systems(
            (
                refresh_scheduler_telemetry_for_test,
                observe_planetary_residency,
            )
                .chain(),
        );
        schedule.run(&mut world);
        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(telemetry.resident_fluid_entities, 1);
        assert_eq!(telemetry.fluid_ring_vertices[1], 100);
        assert_eq!(telemetry.fluid_ring_indices[1], 300);
        assert!(telemetry.resident_fluid_observation_valid);
        assert!(!telemetry.resident_fluid_scheduler_mismatch);

        world.spawn(FarFieldFluidRing {
            level: 1,
            anchor: WorldXZ::ZERO,
            vertices: FAR_FIELD_MAX_FLUID_VERTICES + 1,
            indices: 1,
        });
        schedule.run(&mut world);
        let telemetry = world.resource::<PlanetaryStreamingTelemetry>();
        assert_eq!(telemetry.resident_fluid_duplicate_slots, 1);
        assert!(telemetry.resident_fluid_budget_exceeded);
        assert!(telemetry.resident_fluid_scheduler_mismatch);
        assert!(!telemetry.resident_fluid_observation_valid);
        assert_eq!(telemetry.resident_fluid_observation_rejections, 1);
    }

    struct TestSampler;

    impl FarFieldSampler for TestSampler {
        fn height_at(&self, x: i32, z: i32) -> f32 {
            let x = x as f32;
            let z = z as f32;
            64.0 + x * 0.001 + z * 0.002 + (x * 0.0003).sin() * 7.0
        }

        fn biome_at(&self, _x: i32, _z: i32) -> Biome {
            Biome::Plains
        }
    }

    struct PatternSampler;

    impl FarFieldSampler for PatternSampler {
        fn height_at(&self, x: i32, z: i32) -> f32 {
            72.0 + x as f32 * 0.12 - z as f32 * 0.08
        }

        fn biome_at(&self, x: i32, z: i32) -> Biome {
            match (x.div_euclid(128) + z.div_euclid(128)).rem_euclid(4) {
                0 => Biome::Plains,
                1 => Biome::Desert,
                2 => Biome::Karst,
                _ => Biome::VolcanicWaste,
            }
        }
    }

    /// Adversarial absolute stripes plus a non-power-of-two checker. The
    /// periods intentionally alias the old 2x/4x ring-local palette phase.
    struct StripeCheckerSampler;

    impl FarFieldSampler for StripeCheckerSampler {
        fn height_at(&self, x: i32, z: i32) -> f32 {
            let terrace = (x.div_euclid(17) + z.div_euclid(23)).rem_euclid(5) as f32;
            80.0 + terrace * 1.75 + x as f32 * 0.002 - z as f32 * 0.003
        }

        fn biome_at(&self, x: i32, z: i32) -> Biome {
            match (x.div_euclid(37) + z.div_euclid(53)).rem_euclid(4) {
                0 => Biome::Plains,
                1 => Biome::Desert,
                2 => Biome::Forest,
                _ => Biome::VolcanicWaste,
            }
        }
    }

    #[derive(Clone, Copy)]
    struct FluidSampler {
        height: f32,
        biome: Biome,
    }

    impl FarFieldSampler for FluidSampler {
        fn height_at(&self, _x: i32, _z: i32) -> f32 {
            self.height
        }

        fn biome_at(&self, _x: i32, _z: i32) -> Biome {
            self.biome
        }
    }

    fn build_test_fluid_mesh(
        sampler: FluidSampler,
        profile: WorldProfile,
        anchor: WorldXZ,
        level: usize,
    ) -> (FarFieldMeshData, SamplingStats) {
        let mut world = test_world(profile);
        world.hydro_mode = FarFieldHydroMode::DescriptiveV1;
        let spec = RingSpec::for_level(level, 0, anchor);
        let (_, fluid, stats, _) = build_ring_mesh_incremental_with_coverage_and_hydro(
            &sampler,
            world,
            spec,
            profile,
            FarFieldMaterialDetail::Detailed,
            None,
            NearCoverageMask::default(),
        );
        (fluid, stats)
    }

    fn expected_absolute_bridge_color<S: FarFieldSampler>(
        sampler: &S,
        world_x: i64,
        world_z: i64,
    ) -> [f32; 4] {
        let (x, _) = terrain_coordinate(world_x);
        let (z, _) = terrain_coordinate(world_z);
        let center = sampler.height_at(x, z);
        let max_rise = [
            sampler.height_at(x.saturating_sub(1), z),
            sampler.height_at(x.saturating_add(1), z),
            sampler.height_at(x, z.saturating_sub(1)),
            sampler.height_at(x, z.saturating_add(1)),
        ]
        .into_iter()
        .filter(|height| center.is_finite() && height.is_finite())
        .map(|height| (center - height).abs())
        .fold(0.0_f32, f32::max);
        block_linear_albedo(coarse_surface_family(sampler.biome_at(x, z), max_rise))
    }

    fn expected_absolute_bridge_v2_color<S: FarFieldSampler>(
        sampler: &S,
        world_x: i64,
        world_z: i64,
    ) -> [f32; 4] {
        let (x, _) = terrain_coordinate(bridge_v2_material_sample_coordinate(world_x));
        let (z, _) = terrain_coordinate(bridge_v2_material_sample_coordinate(world_z));
        block_linear_albedo(coarse_surface_family(sampler.biome_at(x, z), 0.0))
    }

    fn test_world(profile: WorldProfile) -> FarFieldWorldKey {
        FarFieldWorldKey {
            seed: 91_337,
            profile,
            scenery: SceneryQuality::Balanced,
            surface_material_mode: FarFieldSurfaceMaterialMode::BridgeV2,
            hydro_mode: FarFieldHydroMode::Disabled,
        }
    }

    #[test]
    fn documented_default_frontier_column_count_matches_integer_disc() {
        let radius = 50_i32;
        let columns = (-radius..=radius)
            .flat_map(|x| (-radius..=radius).map(move |z| (x, z)))
            .filter(|(x, z)| x * x + z * z <= radius * radius)
            .count();
        assert_eq!(columns, 7_845);
    }

    #[test]
    fn profile_gate_keeps_natural_reversible_by_default() {
        let default = FarFieldProfileGate::from_env_value(None);
        assert!(default.allows(WorldProfile::AstralFrontier));
        assert!(!default.allows(WorldProfile::Natural));
        assert_eq!(
            FarFieldProfileGate::from_env_value(Some("off")),
            FarFieldProfileGate::Disabled
        );
        assert!(FarFieldProfileGate::from_env_value(Some("all")).allows(WorldProfile::Natural));
    }

    #[test]
    fn surface_material_gate_defaults_to_v2_and_parses_both_rollbacks() {
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(None),
            FarFieldSurfaceMaterialMode::BridgeV2
        );
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(Some("legacy")),
            FarFieldSurfaceMaterialMode::LegacyPalette
        );
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(Some("bridge-v1")),
            FarFieldSurfaceMaterialMode::BridgeV1
        );
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(Some("bridge-v2")),
            FarFieldSurfaceMaterialMode::BridgeV2
        );
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(Some("bridge")),
            FarFieldSurfaceMaterialMode::BridgeV2
        );
        assert_eq!(
            FarFieldSurfaceMaterialMode::from_env_value(Some("unknown")),
            FarFieldSurfaceMaterialMode::BridgeV2
        );
    }

    #[test]
    fn hydro_gate_defaults_to_v1_and_retains_explicit_rollback() {
        assert_eq!(
            FarFieldHydroMode::from_env_value(None),
            FarFieldHydroMode::DescriptiveV1
        );
        assert_eq!(
            FarFieldHydroMode::from_env_value(Some("off")),
            FarFieldHydroMode::Disabled
        );
        assert_eq!(
            FarFieldHydroMode::from_env_value(Some("v1")),
            FarFieldHydroMode::DescriptiveV1
        );
        assert_eq!(
            FarFieldHydroMode::from_env_value(Some("unknown")),
            FarFieldHydroMode::DescriptiveV1
        );
    }

    #[test]
    fn natural_water_and_astral_lava_follow_near_column_fill_rules() {
        let (water, water_stats) = build_test_fluid_mesh(
            FluidSampler {
                height: (WATER_LEVEL - 3) as f32,
                biome: Biome::Ocean,
            },
            WorldProfile::Natural,
            WorldXZ::new(-12_345, 67_890),
            0,
        );
        assert_eq!(water.vertex_count(), FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
        assert!(water.index_count() <= FAR_FIELD_MAX_FLUID_INDICES_PER_RING);
        assert!(water.index_count() > 0);
        assert_eq!(
            water.colors[0],
            far_field_fluid_vertex_color(FarFieldFluidKind::Water)
        );
        assert_eq!(
            water_stats.fluid_classification_queries,
            FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING
        );
        assert_eq!(
            water_stats.fluid_biome_queries,
            FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING
        );

        let (lava, _) = build_test_fluid_mesh(
            FluidSampler {
                height: (FAR_FIELD_VOLCANIC_LAVA_LEVEL - 3) as f32,
                biome: Biome::VolcanicWaste,
            },
            WorldProfile::AstralFrontier,
            WorldXZ::new(i64::from(i32::MIN) - 8_192, i64::from(i32::MAX) + 8_192),
            FAR_FIELD_LEVELS - 1,
        );
        assert_eq!(lava.vertex_count(), FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
        assert!(lava.index_count() <= FAR_FIELD_MAX_FLUID_INDICES_PER_RING);
        assert!(lava.index_count() > 0);
        assert_eq!(
            lava.colors[0],
            far_field_fluid_vertex_color(FarFieldFluidKind::Lava)
        );
        assert_eq!(lava.colors[0][3], 1.0);
    }

    #[test]
    fn dry_high_ground_and_nonfinite_heights_emit_no_fluid_payload() {
        for height in [(WATER_LEVEL + 20) as f32, f32::NAN, f32::INFINITY] {
            let (fluid, stats) = build_test_fluid_mesh(
                FluidSampler {
                    height,
                    biome: Biome::Plains,
                },
                WorldProfile::Natural,
                WorldXZ::new(-1, -1),
                2,
            );
            assert!(fluid.is_empty());
            assert_eq!(fluid.vertex_count(), 0);
            assert_eq!(fluid.index_count(), 0);
            assert_eq!(
                stats.fluid_classification_queries,
                FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING
            );
            if height.is_finite() {
                assert_eq!(stats.fluid_biome_queries, 0);
            }
        }
    }

    #[test]
    fn fluid_topology_and_queries_are_hard_capped_under_pressure() {
        let (fluid, stats) = build_test_fluid_mesh(
            FluidSampler {
                height: (WATER_LEVEL - 8) as f32,
                biome: Biome::Ocean,
            },
            WorldProfile::Natural,
            WorldXZ::new(-999_999, 999_999),
            FAR_FIELD_LEVELS - 1,
        );
        assert!(fluid.vertex_count() <= FAR_FIELD_MAX_FLUID_VERTICES_PER_RING);
        assert!(fluid.index_count() <= FAR_FIELD_MAX_FLUID_INDICES_PER_RING);
        assert!(
            mesh_payload_bytes(fluid.vertex_count(), fluid.index_count())
                <= FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES
        );
        assert!(
            stats.fluid_classification_queries
                <= FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING
        );
        assert!(stats.fluid_biome_queries <= FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING);
        assert_eq!(FAR_FIELD_MAX_RENDER_ENTITIES, 12);
        assert_eq!(FAR_FIELD_MAX_FLUID_MESH_BYTES, 1_590_048);

        let mut runtime = PlanetaryStreamingRuntime::default();
        for _ in 0..100_000 {
            runtime.set_target_material_detail(FarFieldMaterialDetail::Reduced);
            runtime.mark_dirty(0);
        }
        assert!(runtime.dirty_mask.count_ones() as usize <= FAR_FIELD_LEVELS);
        assert_eq!(FAR_FIELD_MAX_BUILDS_IN_FLIGHT, 1);
        assert_eq!(FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS, FAR_FIELD_LEVELS);
    }

    #[test]
    fn hydro_mode_is_cache_identity_and_stale_result_identity() {
        let sampler = FluidSampler {
            height: (WATER_LEVEL - 2) as f32,
            biome: Biome::Ocean,
        };
        let mut disabled = test_world(WorldProfile::Natural);
        disabled.hydro_mode = FarFieldHydroMode::Disabled;
        let spec = RingSpec::for_level(0, 0, WorldXZ::new(-64, 64));
        let (_, _, cache) = build_ring_mesh_incremental(
            &sampler,
            disabled,
            spec,
            disabled.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let height_allocation = cache.heights.as_ptr();
        let enabled = FarFieldWorldKey {
            hydro_mode: FarFieldHydroMode::DescriptiveV1,
            ..disabled
        };
        let (_, _, enabled_cache) = build_ring_mesh_incremental(
            &sampler,
            enabled,
            spec,
            enabled.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        assert_eq!(enabled_cache.heights.as_ptr(), height_allocation);

        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.world_key = Some(enabled);
        runtime.target_specs[0] = spec;
        let stale = RingBuildRequest {
            world: disabled,
            spec,
            material_detail: FarFieldMaterialDetail::Detailed,
            near_coverage: NearCoverageMask::default(),
        };
        assert!(!ring_request_is_current(&runtime, stale));
        assert!(ring_request_is_current(
            &runtime,
            RingBuildRequest {
                world: enabled,
                ..stale
            }
        ));
    }

    #[test]
    fn finest_grid_is_a_complete_bounded_parent_under_near_voxels() {
        let spec = RingSpec::for_level(0, FAR_FIELD_FINEST_INNER_EXTENT_METRES, WorldXZ::ZERO);
        assert_eq!(spec.inner_extent, 0);
        let half = FAR_FIELD_GRID_CELLS / 2;
        assert!((-half..half).all(|cz| {
            (-half..half).all(|cx| cell_is_in_ring(cx, cz, spec.step, spec.inner_extent))
        }));

        let (mesh, _) = build_ring_mesh(
            &TestSampler,
            spec,
            WorldProfile::Natural,
            FarFieldMaterialDetail::Detailed,
        );
        let top_indices = FAR_FIELD_GRID_CELLS as usize * FAR_FIELD_GRID_CELLS as usize * 6;
        assert!(mesh.index_count() >= top_indices);
        assert_eq!(mesh.positions.len(), mesh.normals.len());
        assert_eq!(mesh.positions.len(), mesh.colors.len());
    }

    #[test]
    fn coverage_handshake_quantizes_down_and_visits_each_column_once() {
        let mut visits = 0usize;
        let extent = confirmed_near_visual_extent(8, 8, 2, |cx, cz| {
            visits += 1;
            cx.abs() <= 2 && cz.abs() <= 2
        });
        assert_eq!(visits, 25);
        assert_eq!(extent, FAR_FIELD_BASE_STEP_METRES);

        let wider = confirmed_near_visual_extent(8, 8, 4, |cx, cz| cx.abs() <= 4 && cz.abs() <= 4);
        assert_eq!(wider, FAR_FIELD_BASE_STEP_METRES * 2);

        let missing_first_ring =
            confirmed_near_visual_extent(8, 8, 4, |cx, cz| !(cx == 1 && cz == 0));
        assert_eq!(missing_first_ring, FAR_FIELD_FINEST_INNER_EXTENT_METRES);
        assert_eq!(
            confirmed_near_visual_extent(8, 8, 16, |_cx, _cz| false),
            FAR_FIELD_FINEST_INNER_EXTENT_METRES
        );
    }

    #[test]
    fn irregular_coverage_stencil_is_fixed_bounded_and_fail_closed() {
        let spec = RingSpec::for_level(0, FAR_FIELD_FINEST_INNER_EXTENT_METRES, WorldXZ::ZERO);
        let coverage =
            build_near_coverage_snapshot(spec, 8, 8, 4, |cx, cz| cx.abs() <= 4 && cz.abs() <= 4);
        assert_eq!(coverage.ready_columns, 81);
        assert_eq!(coverage.mask.hidden_cells(), 16);
        for cz in -2..=1 {
            for cx in -2..=1 {
                assert!(
                    coverage.mask.hides(cx, cz),
                    "missing covered cell {cx},{cz}"
                );
            }
        }
        assert!(!coverage.mask.hides(-3, 0));
        assert!(!coverage.mask.hides(2, 0));

        let missing = build_near_coverage_snapshot(spec, 8, 8, 16, |_cx, _cz| false);
        assert_eq!(missing.ready_columns, 0);
        assert_eq!(missing.mask.hidden_cells(), 0);
        assert_eq!(size_of::<NearCoverageMask>(), FAR_FIELD_COVERAGE_WORDS * 8);
        assert_eq!(size_of::<NearCoverageMask>(), 456);
        assert!(FAR_FIELD_MAX_COVERAGE_WORK_BYTES <= 2 * 1024);
    }

    #[test]
    fn coverage_expansion_batches_but_coverage_loss_is_immediate() {
        let mut runtime = PlanetaryStreamingRuntime::default();
        let mut expanded = NearCoverageMask::default();
        expanded.hide(0, 0);
        for _ in 0..4 {
            runtime.observe_near_coverage(expanded, false, 0.1);
            assert_eq!(runtime.target_near_coverage, NearCoverageMask::default());
        }
        runtime.observe_near_coverage(expanded, false, 0.1);
        assert_eq!(runtime.target_near_coverage, expanded);

        runtime.observe_near_coverage(NearCoverageMask::default(), false, 0.0);
        assert_eq!(runtime.target_near_coverage, NearCoverageMask::default());

        runtime.observe_near_coverage(expanded, true, f32::NAN);
        assert_eq!(runtime.target_near_coverage, expanded);
    }

    #[test]
    fn coverage_stencil_removes_only_requested_parent_cell_topology() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let spec = RingSpec::for_level(0, FAR_FIELD_FINEST_INNER_EXTENT_METRES, WorldXZ::ZERO);
        let (complete, _, _) = build_ring_mesh_incremental_with_coverage(
            &sampler,
            world,
            spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
            NearCoverageMask::default(),
        );
        let mut mask = NearCoverageMask::default();
        mask.hide(0, 0);
        let (cutout, _, _) = build_ring_mesh_incremental_with_coverage(
            &sampler,
            world,
            spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
            mask,
        );
        let (replayed, _, _) = build_ring_mesh_incremental_with_coverage(
            &sampler,
            world,
            spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
            mask,
        );
        assert_eq!(cutout.index_count() + 6, complete.index_count());
        assert_eq!(cutout.positions, complete.positions);
        assert_eq!(cutout.colors, complete.colors);
        assert_eq!(cutout.positions, replayed.positions);
        assert_eq!(cutout.normals, replayed.normals);
        assert_eq!(cutout.colors, replayed.colors);
        assert_eq!(cutout.uvs, replayed.uvs);
        assert_eq!(cutout.indices, replayed.indices);
    }

    #[test]
    fn near_visual_readiness_fails_closed_for_missing_or_dirty_surface_chunks() {
        let mut world = VoxelWorld::new();
        let mut streamer = ChunkStreamer::default();
        let pos = ChunkPos::new(3, 0, -2);
        world.column_top_cy.insert((pos.x, pos.z), 0);
        streamer.requested_chunks.insert(pos);

        assert!(!near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));
        world.chunks.insert(pos, crate::chunk::Chunk::new(pos));
        assert!(near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));

        streamer.dirty_queue.insert(pos);
        assert!(!near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));
        streamer.entities.insert(
            pos,
            vec![crate::world::ChunkMeshEntity {
                entity: Entity::PLACEHOLDER,
                handle: Handle::default(),
                material: 0,
            }],
        );
        assert!(near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));
        streamer.entities.clear();
        streamer.dirty_queue.clear();
        world.edit_dirty_chunks.insert(pos);
        assert!(!near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));

        world.edit_dirty_chunks.clear();
        world.column_top_cy.insert((pos.x, pos.z), 1);
        assert!(!near_column_is_visually_ready(
            &world, &streamer, pos.x, pos.z, 8
        ));
    }

    #[test]
    fn authored_far_palette_is_converted_to_bounded_linear_albedo() {
        let midpoint = srgb_channel_to_linear(0.5);
        assert!((midpoint - 0.214_041_14).abs() < 0.000_001);
        assert_eq!(srgb_channel_to_linear(0.0), 0.0);
        assert_eq!(srgb_channel_to_linear(1.0), 1.0);

        for biome in [
            Biome::Ocean,
            Biome::Plains,
            Biome::Forest,
            Biome::Desert,
            Biome::SnowyMountains,
            Biome::VolcanicWaste,
            Biome::AlienReef,
        ] {
            let near = far_field_color(WorldProfile::Natural, biome, 48.0, 0);
            let distant = far_field_color(WorldProfile::Natural, biome, 48.0, 5);
            for channel in 0..3 {
                assert!(near[channel].is_finite());
                assert!((0.0..=1.0).contains(&near[channel]));
                assert!(distant[channel] <= near[channel]);
            }
            assert_eq!(near[3], 1.0);
        }

        let plains = far_field_color(WorldProfile::Natural, Biome::Plains, 48.0, 0);
        assert!(plains[0] < 0.08, "red albedo remained in sRGB: {plains:?}");
        assert!(
            plains[1] < 0.26,
            "green albedo remained in sRGB: {plains:?}"
        );
    }

    #[test]
    fn bridge_v1_absolute_checker_samples_are_canonical_not_pastel() {
        let sampler = StripeCheckerSampler;
        let mut world = test_world(WorldProfile::Natural);
        world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV1;
        let spec = RingSpec::for_level(0, 0, WorldXZ::new(-2_080, -3_104));
        let (mesh, stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );

        for gz in -30..=30 {
            for gx in -30..=30 {
                let world_x = spec.anchor.x + i64::from(gx) * spec.step;
                let world_z = spec.anchor.z + i64::from(gz) * spec.step;
                let expected = expected_absolute_bridge_color(&sampler, world_x, world_z);
                assert_eq!(mesh.colors[top_index(gx, gz) as usize], expected);
            }
        }
        assert_eq!(
            stats.biome_queries,
            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING
        );
        assert_eq!(
            stats.material_slope_queries,
            FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING
        );

        let grass = block_linear_albedo(BlockType::Grass);
        let sand = block_linear_albedo(BlockType::Sand);
        let pastel = lerp_rgba(grass, sand, 0.5);
        assert!(mesh.colors.iter().all(|color| *color != pastel));
    }

    #[test]
    fn bridge_colors_are_finite_bounded_canonical_block_albedo() {
        let biomes = [
            Biome::Ocean,
            Biome::Beach,
            Biome::Plains,
            Biome::Forest,
            Biome::Jungle,
            Biome::Desert,
            Biome::Savanna,
            Biome::Tundra,
            Biome::SnowyMountains,
            Biome::Mountains,
            Biome::Mesa,
            Biome::Karst,
            Biome::CrystalSpires,
            Biome::VolcanicWaste,
            Biome::GlacierShards,
            Biome::AlienReef,
        ];
        for biome in biomes {
            for slope in [0.0, 1.0, 2.0, 4.0, f32::NAN, f32::INFINITY] {
                let color = block_linear_albedo(coarse_surface_family(biome, slope));
                for channel in color {
                    assert!(channel.is_finite(), "non-finite {biome:?} color {color:?}");
                    assert!(
                        (0.0..=1.0).contains(&channel),
                        "out-of-range {biome:?} color {color:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn all_material_modes_preserve_geometry_with_bounded_queries() {
        let sampler = PatternSampler;
        let spec = RingSpec::for_level(
            1,
            FAR_FIELD_FINEST_INNER_EXTENT_METRES,
            WorldXZ::new(-4_352, 7_744),
        );
        let mut legacy_world = test_world(WorldProfile::Natural);
        legacy_world.surface_material_mode = FarFieldSurfaceMaterialMode::LegacyPalette;
        let mut bridge_v1_world = legacy_world;
        bridge_v1_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV1;
        let mut bridge_v2_world = legacy_world;
        bridge_v2_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV2;

        let (legacy, legacy_stats, _) = build_ring_mesh_incremental(
            &sampler,
            legacy_world,
            spec,
            legacy_world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let (bridge_v1, bridge_v1_stats, _) = build_ring_mesh_incremental(
            &sampler,
            bridge_v1_world,
            spec,
            bridge_v1_world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let (bridge_v2, bridge_v2_stats, _) = build_ring_mesh_incremental(
            &sampler,
            bridge_v2_world,
            spec,
            bridge_v2_world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );

        for bridge in [&bridge_v1, &bridge_v2] {
            assert_eq!(legacy.positions, bridge.positions);
            assert_eq!(legacy.normals, bridge.normals);
            assert_eq!(legacy.uvs, bridge.uvs);
            assert_eq!(legacy.indices, bridge.indices);
            assert_eq!(legacy.vertex_count(), bridge.vertex_count());
            assert_eq!(legacy.index_count(), bridge.index_count());
            assert_ne!(legacy.colors, bridge.colors);
        }
        assert_eq!(legacy_stats.height_queries, bridge_v1_stats.height_queries);
        assert_eq!(legacy_stats.height_queries, bridge_v2_stats.height_queries);
        assert_eq!(legacy_stats.material_slope_queries, 0);
        assert_eq!(
            bridge_v1_stats.material_slope_queries,
            FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING
        );
        assert_eq!(bridge_v2_stats.material_slope_queries, 0);
        assert!(bridge_v1_stats.biome_queries <= FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING);
        assert_eq!(bridge_v2_stats.biome_queries, 31 * 31);
        assert_eq!(
            bridge_v2_stats.bridge_v2_cell_reuses,
            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING - 31 * 31
        );
    }

    #[test]
    fn material_mode_is_part_of_cache_identity_and_forces_safe_rebuild() {
        let sampler = TestSampler;
        let spec = RingSpec::for_level(0, 0, WorldXZ::ZERO);
        let mut legacy_world = test_world(WorldProfile::Natural);
        legacy_world.surface_material_mode = FarFieldSurfaceMaterialMode::LegacyPalette;
        let (_, _, cache) = build_ring_mesh_incremental(
            &sampler,
            legacy_world,
            spec,
            legacy_world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let height_allocation = cache.heights.as_ptr();
        let biome_allocation = cache.biomes.as_ptr();
        let family_allocation = cache.surface_families.as_ptr();
        let mut bridge_v1_world = legacy_world;
        bridge_v1_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV1;
        let (_, stats, cache) = build_ring_mesh_incremental(
            &sampler,
            bridge_v1_world,
            spec,
            bridge_v1_world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        assert_eq!(
            stats.cache_update,
            FarFieldCacheUpdate::IncompatibleFallback
        );
        assert_eq!(stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
        assert_eq!(cache.heights.as_ptr(), height_allocation);
        assert_eq!(cache.biomes.as_ptr(), biome_allocation);
        assert_eq!(cache.surface_families.as_ptr(), family_allocation);

        let mut bridge_v2_world = bridge_v1_world;
        bridge_v2_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV2;
        let (_, stats, cache) = build_ring_mesh_incremental(
            &sampler,
            bridge_v2_world,
            spec,
            bridge_v2_world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        assert_eq!(
            stats.cache_update,
            FarFieldCacheUpdate::IncompatibleFallback
        );
        assert_eq!(stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
        assert_eq!(cache.heights.as_ptr(), height_allocation);
        assert_eq!(cache.biomes.as_ptr(), biome_allocation);
        assert_eq!(cache.surface_families.as_ptr(), family_allocation);
    }

    #[test]
    fn bridge_v1_checker_is_identical_across_l0_l1_negative_anchor_retarget() {
        let sampler = StripeCheckerSampler;
        let mut world = test_world(WorldProfile::Natural);
        world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV1;
        let anchor = WorldXZ::new(-4_096, -2_048);
        let fine_spec = RingSpec::for_level(0, 0, anchor);
        let coarse_spec = RingSpec::for_level(1, 0, anchor);
        let (fine, fine_stats, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            fine_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let (coarse, coarse_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            coarse_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        for coarse_gz in -14..=14 {
            for coarse_gx in -14..=14 {
                let world_x = anchor.x + i64::from(coarse_gx) * coarse_spec.step;
                let world_z = anchor.z + i64::from(coarse_gz) * coarse_spec.step;
                let fine_gx = i32::try_from((world_x - anchor.x).div_euclid(fine_spec.step))
                    .expect("shared fine x is bounded");
                let fine_gz = i32::try_from((world_z - anchor.z).div_euclid(fine_spec.step))
                    .expect("shared fine z is bounded");
                let expected = expected_absolute_bridge_color(&sampler, world_x, world_z);
                assert_eq!(fine.colors[top_index(fine_gx, fine_gz) as usize], expected);
                assert_eq!(
                    coarse.colors[top_index(coarse_gx, coarse_gz) as usize],
                    expected
                );
            }
        }
        assert_eq!(
            fine_stats.material_slope_queries,
            FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING
        );
        assert_eq!(
            coarse_stats.material_slope_queries,
            FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING
        );

        let moved_spec = RingSpec::for_level(
            0,
            0,
            WorldXZ::new(
                fine_spec.anchor.x + fine_spec.step,
                fine_spec.anchor.z - fine_spec.step,
            ),
        );
        let (moved, moved_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            moved_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        for old_gz in -20..=19 {
            for old_gx in -19..=20 {
                let world_x = fine_spec.anchor.x + i64::from(old_gx) * fine_spec.step;
                let world_z = fine_spec.anchor.z + i64::from(old_gz) * fine_spec.step;
                let moved_gx = old_gx - 1;
                let moved_gz = old_gz + 1;
                assert_eq!(
                    fine.colors[top_index(old_gx, old_gz) as usize],
                    moved.colors[top_index(moved_gx, moved_gz) as usize],
                    "absolute bridge mismatch at ({world_x}, {world_z})"
                );
            }
        }
        assert_eq!(
            moved_stats.cache_update,
            FarFieldCacheUpdate::IncrementalStrip
        );
        assert_eq!(
            moved_stats.biome_queries,
            2 * FAR_FIELD_GRID_VERTICES as usize - 1
        );
        assert_eq!(
            moved_stats.material_slope_queries,
            (2 * FAR_FIELD_GRID_VERTICES as usize - 1) * 4
        );
    }

    #[test]
    fn bridge_v2_checker_is_canonical_lod_and_retarget_stable_without_slope_queries() {
        let sampler = StripeCheckerSampler;
        let world = test_world(WorldProfile::Natural);
        let anchor = WorldXZ::new(-4_096, -2_048);
        let fine_spec = RingSpec::for_level(0, 0, anchor);
        let coarse_spec = RingSpec::for_level(1, 0, anchor);
        let (fine, fine_stats, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            fine_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let (coarse, coarse_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            coarse_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );

        for coarse_gz in -14..=14 {
            for coarse_gx in -14..=14 {
                let world_x = anchor.x + i64::from(coarse_gx) * coarse_spec.step;
                let world_z = anchor.z + i64::from(coarse_gz) * coarse_spec.step;
                let fine_gx = i32::try_from((world_x - anchor.x).div_euclid(fine_spec.step))
                    .expect("shared fine x is bounded");
                let fine_gz = i32::try_from((world_z - anchor.z).div_euclid(fine_spec.step))
                    .expect("shared fine z is bounded");
                let expected = expected_absolute_bridge_v2_color(&sampler, world_x, world_z);
                assert_eq!(fine.colors[top_index(fine_gx, fine_gz) as usize], expected);
                assert_eq!(
                    coarse.colors[top_index(coarse_gx, coarse_gz) as usize],
                    expected
                );
            }
        }
        assert_eq!(fine_stats.biome_queries, 16 * 16);
        assert_eq!(
            fine_stats.bridge_v2_cell_reuses,
            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING - 16 * 16
        );
        assert_eq!(fine_stats.material_slope_queries, 0);
        assert_eq!(coarse_stats.biome_queries, 31 * 31);
        assert_eq!(
            coarse_stats.bridge_v2_cell_reuses,
            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING - 31 * 31
        );
        assert_eq!(coarse_stats.material_slope_queries, 0);

        let moved_spec = RingSpec::for_level(
            0,
            0,
            WorldXZ::new(
                fine_spec.anchor.x + fine_spec.step,
                fine_spec.anchor.z - fine_spec.step,
            ),
        );
        let (moved, moved_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            moved_spec,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        for old_gz in -20..=19 {
            for old_gx in -19..=20 {
                let moved_gx = old_gx - 1;
                let moved_gz = old_gz + 1;
                assert_eq!(
                    fine.colors[top_index(old_gx, old_gz) as usize],
                    moved.colors[top_index(moved_gx, moved_gz) as usize]
                );
            }
        }
        assert_eq!(
            moved_stats.cache_update,
            FarFieldCacheUpdate::IncrementalStrip
        );
        assert_eq!(moved_stats.biome_queries, 16);
        assert_eq!(
            moved_stats.bridge_v2_cell_reuses,
            (2 * FAR_FIELD_GRID_VERTICES as usize - 1) - 16
        );
        assert_eq!(moved_stats.material_slope_queries, 0);

        let grass = block_linear_albedo(BlockType::Grass);
        let sand = block_linear_albedo(BlockType::Sand);
        assert!(fine
            .colors
            .iter()
            .all(|color| *color != lerp_rgba(grass, sand, 0.5)));
    }

    #[test]
    fn bridge_reduced_material_is_biome_query_free_canonical_fallback() {
        let sampler = PatternSampler;
        let spec = RingSpec::for_level(2, 0, WorldXZ::new(-8_192, 4_096));
        let world = test_world(WorldProfile::AstralFrontier);
        let (mesh, stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            spec,
            world.profile,
            FarFieldMaterialDetail::Reduced,
            None,
        );
        assert_eq!(stats.biome_queries, 0);
        assert_eq!(stats.material_slope_queries, 0);
        let expected = block_linear_albedo(BlockType::GlowSand);
        let top_vertices = FAR_FIELD_GRID_VERTICES as usize * FAR_FIELD_GRID_VERTICES as usize;
        assert!(mesh.colors[..top_vertices]
            .iter()
            .all(|color| *color == expected));
    }

    #[test]
    fn negative_world_snapping_uses_euclidean_cells() {
        assert_eq!(snap_world_coordinate(-1, 32), -32);
        assert_eq!(snap_world_coordinate(-32, 32), -32);
        assert_eq!(snap_world_coordinate(-33, 32), -64);
        assert_eq!(snap_world_coordinate(0, 32), 0);
        assert_eq!(snap_world_coordinate(31, 32), 0);
        assert_eq!(snap_world_coordinate(32, 32), 32);

        assert_eq!(bridge_v2_material_sample_coordinate(-129), -192);
        assert_eq!(bridge_v2_material_sample_coordinate(-128), -64);
        assert_eq!(bridge_v2_material_sample_coordinate(-1), -64);
        assert_eq!(bridge_v2_material_sample_coordinate(0), 64);
        assert_eq!(bridge_v2_material_sample_coordinate(127), 64);
        assert_eq!(bridge_v2_material_sample_coordinate(128), 192);
        assert_eq!(
            bridge_v2_material_sample_coordinate(i64::MIN),
            i64::MIN.saturating_add(FAR_FIELD_BRIDGE_V2_MATERIAL_CELL_METRES / 2)
        );
        assert_eq!(
            bridge_v2_material_sample_coordinate(i64::MAX),
            i64::MAX - 63
        );
    }

    #[test]
    fn explicit_mesh_job_and_cache_byte_caps_cover_declared_payloads() {
        assert_eq!(FAR_FIELD_MAX_BUILDS_IN_FLIGHT, 1);
        assert_eq!(FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS, FAR_FIELD_LEVELS);
        assert_eq!(FAR_FIELD_MAX_MESH_BYTES, 2_280_000);
        assert_eq!(FAR_FIELD_MAX_RING_BUILD_BYTES, 388_000);
        assert!(
            mesh_payload_bytes(FAR_FIELD_MAX_VERTICES, FAR_FIELD_MAX_INDICES)
                <= FAR_FIELD_MAX_MESH_BYTES
        );
        assert!(
            RING_SAMPLE_CACHE_ACCOUNTED_BYTES * FAR_FIELD_MAX_SAMPLE_CACHE_WINDOWS
                <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES
        );
        assert!(FAR_FIELD_MAX_COVERAGE_WORK_BYTES <= 2 * 1024);
        assert_eq!(FAR_FIELD_SAMPLE_CACHE_SIDE, 65);
        assert_eq!(FAR_FIELD_SAMPLE_CACHE_CELLS, 4_225);
        assert_eq!(FAR_FIELD_MATERIAL_SLOPE_QUANTUM_METRES, 1);
        assert_eq!(FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING, 3_721);
        assert_eq!(FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING, 14_884);
        assert_eq!(FAR_FIELD_BRIDGE_V2_MATERIAL_CELL_METRES, 128);
        assert_eq!(FAR_FIELD_MAX_BRIDGE_V2_CELL_REUSES_PER_RING, 3_720);
        assert_eq!(size_of::<BridgeV2CanonicalPalette>(), 256);
        assert_eq!(FAR_FIELD_MAX_RENDER_ENTITIES, 12);
        assert_eq!(FAR_FIELD_MAX_FLUID_VERTICES_PER_RING, 3_721);
        assert_eq!(FAR_FIELD_MAX_FLUID_INDICES_PER_RING, 21_600);
        assert_eq!(FAR_FIELD_MAX_FLUID_VERTICES, 22_326);
        assert_eq!(FAR_FIELD_MAX_FLUID_INDICES, 129_600);
        assert_eq!(FAR_FIELD_MAX_FLUID_MESH_BYTES, 1_590_048);
        assert_eq!(FAR_FIELD_MAX_FLUID_RING_BUILD_BYTES, 265_008);
        assert_eq!(FAR_FIELD_MAX_ATOMIC_RING_BUILD_BYTES, 653_008);
        assert_eq!(FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING, 3_721);
        assert_eq!(FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING, 3_721);
    }

    #[test]
    fn one_cell_move_samples_exactly_one_entering_toroidal_strip() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let initial = RingSpec::for_level(0, 192, WorldXZ::ZERO);
        let (_, cold, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            initial,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        assert_eq!(cold.cache_update, FarFieldCacheUpdate::Cold);
        assert_eq!(cold.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);

        let moved = RingSpec::for_level(0, 192, WorldXZ::new(initial.step, 0));
        let (_, shifted, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            moved,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        assert_eq!(shifted.cache_update, FarFieldCacheUpdate::IncrementalStrip);
        assert_eq!(
            (shifted.cache_shift_x_cells, shifted.cache_shift_z_cells),
            (1, 0)
        );
        assert_eq!(shifted.height_queries, FAR_FIELD_SAMPLE_CACHE_SIDE);
        assert_eq!(
            shifted.reused_height_samples,
            FAR_FIELD_SAMPLE_CACHE_CELLS - FAR_FIELD_SAMPLE_CACHE_SIDE
        );
    }

    #[test]
    fn diagonal_move_samples_union_of_two_entering_strips() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let initial = RingSpec::for_level(0, 192, WorldXZ::ZERO);
        let (_, _, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            initial,
            world.profile,
            FarFieldMaterialDetail::Reduced,
            None,
        );
        let moved = RingSpec::for_level(0, 192, WorldXZ::new(initial.step, -initial.step));
        let (_, shifted, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            moved,
            world.profile,
            FarFieldMaterialDetail::Reduced,
            Some(cache),
        );
        assert_eq!(shifted.height_queries, FAR_FIELD_SAMPLE_CACHE_SIDE * 2 - 1);
        assert_eq!(
            shifted.reused_height_samples + shifted.height_queries,
            FAR_FIELD_SAMPLE_CACHE_CELLS
        );
    }

    #[test]
    fn incremental_target_is_byte_identical_to_cold_target() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let initial = RingSpec::for_level(1, 192, WorldXZ::new(-96, 160));
        let (_, _, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            initial,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let target = RingSpec::for_level(
            1,
            192,
            WorldXZ::new(
                initial.anchor.x + initial.step * 4,
                initial.anchor.z - initial.step * 4,
            ),
        );
        let (incremental, incremental_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            target,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        let (cold, cold_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            target,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        assert_eq!(incremental.positions, cold.positions);
        assert_eq!(incremental.normals, cold.normals);
        assert_eq!(incremental.colors, cold.colors);
        assert_eq!(incremental.uvs, cold.uvs);
        assert_eq!(incremental.indices, cold.indices);
        assert!(incremental_stats.height_queries < cold_stats.height_queries);
        assert!(incremental_stats.biome_queries < cold_stats.biome_queries);
    }

    #[test]
    fn stale_or_invalid_build_tokens_fail_closed() {
        let world = test_world(WorldProfile::AstralFrontier);
        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.world_key = Some(world);
        let current = RingSpec::for_level(0, 192, WorldXZ::new(2_048, -4_096));
        runtime.target_specs[0] = current;
        let valid = RingBuildRequest {
            world,
            spec: current,
            material_detail: FarFieldMaterialDetail::Detailed,
            near_coverage: NearCoverageMask::default(),
        };
        assert!(ring_request_is_current(&runtime, valid));

        let mut changed_coverage = NearCoverageMask::default();
        changed_coverage.hide(0, 0);
        let stale_coverage = RingBuildRequest {
            near_coverage: changed_coverage,
            ..valid
        };
        assert!(!ring_request_is_current(&runtime, stale_coverage));

        let stale_spec = RingBuildRequest {
            spec: RingSpec::for_level(0, 192, WorldXZ::new(2_080, -4_096)),
            ..valid
        };
        assert!(!ring_request_is_current(&runtime, stale_spec));
        let stale_world = RingBuildRequest {
            world: FarFieldWorldKey { seed: 4, ..world },
            ..valid
        };
        assert!(!ring_request_is_current(&runtime, stale_world));
        let stale_material = RingBuildRequest {
            material_detail: FarFieldMaterialDetail::Reduced,
            ..valid
        };
        assert!(!ring_request_is_current(&runtime, stale_material));
        let invalid_level = RingBuildRequest {
            spec: RingSpec {
                level: FAR_FIELD_LEVELS,
                ..current
            },
            ..valid
        };
        assert!(!ring_request_is_current(&runtime, invalid_level));
    }

    #[test]
    fn ring_extents_reach_thirty_kilometres_with_six_entities() {
        assert_eq!(FAR_FIELD_MAX_ENTITIES, 6);
        assert_eq!(FAR_FIELD_OUTER_RADIUS_METRES, 30_720);
        let camera = WorldXZ::new(17_345, -98_765);
        let extents: Vec<_> = (0..FAR_FIELD_LEVELS)
            .map(|level| RingSpec::for_level(level, 192, camera).outer_extent)
            .collect();
        assert_eq!(extents, [960, 1_920, 3_840, 7_680, 15_360, 30_720]);
    }

    #[test]
    fn coalesced_dirty_mask_cannot_grow_with_flight_speed() {
        let mut runtime = PlanetaryStreamingRuntime::default();
        for _ in 0..100_000 {
            for level in 0..FAR_FIELD_LEVELS {
                runtime.mark_dirty(level);
            }
        }
        assert_eq!(runtime.dirty_mask, FULL_DIRTY_MASK);
        let mut drained = Vec::new();
        while let Some(level) = runtime.next_dirty_level() {
            drained.push(level);
        }
        assert_eq!(drained.len(), FAR_FIELD_LEVELS);
        assert_eq!(runtime.dirty_mask, 0);
    }

    #[test]
    fn detail_transition_rebuilds_each_lod_once_and_stale_flight_fails_closed() {
        let world = test_world(WorldProfile::AstralFrontier);
        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.world_key = Some(world);
        for level in 0..FAR_FIELD_LEVELS {
            let spec = RingSpec::for_level(level, 0, WorldXZ::new(-2_048, 4_096));
            runtime.target_specs[level] = spec;
            runtime.resident_specs[level] = Some(spec);
            runtime.resident_material_detail[level] = Some(FarFieldMaterialDetail::Detailed);
        }
        let stale_in_flight = RingBuildRequest {
            world,
            spec: runtime.target_specs[3],
            material_detail: FarFieldMaterialDetail::Detailed,
            near_coverage: NearCoverageMask::default(),
        };

        runtime.set_target_material_detail(FarFieldMaterialDetail::Reduced);
        assert_eq!(runtime.dirty_mask, FULL_DIRTY_MASK);
        assert!(!ring_request_is_current(&runtime, stale_in_flight));
        let mut builds = 0usize;
        while let Some(level) = runtime.next_dirty_level() {
            runtime.resident_material_detail[level] = Some(runtime.target_material_detail[level]);
            builds += 1;
        }
        assert_eq!(builds, FAR_FIELD_LEVELS);
        assert_eq!(runtime.dirty_mask, 0);

        // Repeated identical pressure decisions do not enqueue another batch.
        for _ in 0..10_000 {
            runtime.set_target_material_detail(FarFieldMaterialDetail::Reduced);
        }
        assert_eq!(runtime.dirty_mask, 0);

        runtime.set_target_material_detail(FarFieldMaterialDetail::Detailed);
        assert_eq!(runtime.dirty_mask.count_ones() as usize, FAR_FIELD_LEVELS);
        let mut reverse_builds = 0usize;
        while let Some(level) = runtime.next_dirty_level() {
            runtime.resident_material_detail[level] = Some(runtime.target_material_detail[level]);
            reverse_builds += 1;
        }
        assert_eq!(reverse_builds, FAR_FIELD_LEVELS);
    }

    #[test]
    fn cache_population_telemetry_tracks_current_and_peak_without_hidden_window() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::Natural);
        let mut runtime = PlanetaryStreamingRuntime::default();
        for level in 0..(FAR_FIELD_LEVELS - 1) {
            let spec = RingSpec::for_level(level, 0, WorldXZ::ZERO);
            let mut stats = SamplingStats::default();
            runtime.sample_caches[level] = Some(RingSampleCache::cold(
                &sampler,
                world,
                spec,
                &mut stats,
                FarFieldCacheUpdate::Cold,
            ));
        }
        // The sixth ownership slot is inside (or reserved for) the sole worker;
        // incompatible retargeting reuses that allocation in place.
        runtime.in_flight_cache_windows = 1;
        let mut telemetry = PlanetaryStreamingTelemetry::default();
        refresh_telemetry(&runtime, &mut telemetry);
        assert_eq!(telemetry.live_sample_cache_windows, FAR_FIELD_LEVELS);
        assert_eq!(
            telemetry.live_sample_cache_bytes,
            FAR_FIELD_LEVELS * RING_SAMPLE_CACHE_ACCOUNTED_BYTES
        );
        assert_eq!(telemetry.peak_live_sample_cache_windows, FAR_FIELD_LEVELS);
        assert_eq!(
            telemetry.peak_live_sample_cache_bytes,
            FAR_FIELD_LEVELS * RING_SAMPLE_CACHE_ACCOUNTED_BYTES
        );

        runtime.in_flight_cache_windows = 0;
        runtime.sample_caches[0] = None;
        refresh_telemetry(&runtime, &mut telemetry);
        assert_eq!(telemetry.live_sample_cache_windows, FAR_FIELD_LEVELS - 2);
        assert_eq!(
            telemetry.live_sample_cache_bytes,
            (FAR_FIELD_LEVELS - 2) * RING_SAMPLE_CACHE_ACCOUNTED_BYTES
        );
        assert_eq!(telemetry.peak_live_sample_cache_windows, FAR_FIELD_LEVELS);
        assert_eq!(
            telemetry.peak_live_sample_cache_bytes,
            FAR_FIELD_LEVELS * RING_SAMPLE_CACHE_ACCOUNTED_BYTES
        );
        assert!(telemetry.peak_live_sample_cache_bytes <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES);
    }

    #[test]
    fn telemetry_reports_desired_and_mixed_resident_detail_honestly() {
        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.target_material_detail = [FarFieldMaterialDetail::Reduced; FAR_FIELD_LEVELS];
        runtime.resident_material_detail = [
            Some(FarFieldMaterialDetail::Reduced),
            Some(FarFieldMaterialDetail::Reduced),
            Some(FarFieldMaterialDetail::Detailed),
            None,
            None,
            None,
        ];
        let mut telemetry = PlanetaryStreamingTelemetry::default();
        refresh_telemetry(&runtime, &mut telemetry);
        assert_eq!(telemetry.material_detail, FarFieldMaterialDetail::Reduced);
        assert_eq!(
            telemetry.desired_material_detail,
            [FarFieldMaterialDetail::Reduced; FAR_FIELD_LEVELS]
        );
        assert_eq!(
            telemetry.resident_material_detail,
            runtime.resident_material_detail
        );
        assert_eq!(telemetry.resident_reduced_levels, 2);
        assert_eq!(telemetry.resident_detailed_levels, 1);
        assert_eq!(
            telemetry.resident_reduced_levels + telemetry.resident_detailed_levels,
            3
        );
    }

    #[test]
    fn pressure_defers_refresh_work_before_horizon_extent() {
        let mut runtime = PlanetaryStreamingRuntime::default();
        let spec = RingSpec::for_level(0, 192, WorldXZ::ZERO);
        runtime.resident_specs = [Some(spec); FAR_FIELD_LEVELS];
        let mut governor = StreamingGovernor::default();

        governor.frame_pressure = 0.20;
        governor.queue_pressure = 0.10;
        assert_eq!(
            pressure_policy(&governor, &runtime),
            (1, FarFieldMaterialDetail::Detailed)
        );

        governor.frame_pressure = 0.60;
        assert_eq!(
            pressure_policy(&governor, &runtime),
            (2, FarFieldMaterialDetail::Reduced)
        );

        governor.queue_pressure = 0.90;
        assert_eq!(
            pressure_policy(&governor, &runtime),
            (4, FarFieldMaterialDetail::Reduced)
        );

        runtime.resident_specs[5] = None;
        assert_eq!(pressure_policy(&governor, &runtime).0, 1);
        assert_eq!(FAR_FIELD_OUTER_RADIUS_METRES, 30_720);
    }

    #[test]
    fn material_pressure_hysteresis_is_deterministic_and_nan_fails_reduced() {
        let mut runtime = PlanetaryStreamingRuntime::default();
        runtime.resident_specs = [Some(RingSpec::for_level(0, 0, WorldXZ::ZERO)); FAR_FIELD_LEVELS];
        let mut governor = StreamingGovernor::default();

        governor.frame_pressure = 0.56;
        assert_eq!(
            pressure_policy(&governor, &runtime).1,
            FarFieldMaterialDetail::Detailed
        );
        governor.frame_pressure = FAR_FIELD_REDUCED_DETAIL_ENTER_PRESSURE;
        let reduced = pressure_policy(&governor, &runtime).1;
        assert_eq!(reduced, FarFieldMaterialDetail::Reduced);
        runtime.set_target_material_detail(reduced);

        governor.frame_pressure = 0.54;
        assert_eq!(
            pressure_policy(&governor, &runtime).1,
            FarFieldMaterialDetail::Reduced
        );
        governor.frame_pressure = FAR_FIELD_REDUCED_DETAIL_EXIT_PRESSURE;
        assert_eq!(
            pressure_policy(&governor, &runtime).1,
            FarFieldMaterialDetail::Detailed
        );

        runtime.target_material_detail = [FarFieldMaterialDetail::Detailed; FAR_FIELD_LEVELS];
        governor.frame_pressure = f32::NAN;
        governor.queue_pressure = 0.0;
        assert_eq!(
            pressure_policy(&governor, &runtime).1,
            FarFieldMaterialDetail::Reduced
        );
    }

    #[test]
    fn reduced_material_detail_skips_biome_queries_not_height_silhouette() {
        let sampler = TestSampler;
        let spec = RingSpec::for_level(0, 192, WorldXZ::ZERO);
        let (detailed, detailed_stats) = build_ring_mesh(
            &sampler,
            spec,
            WorldProfile::AstralFrontier,
            FarFieldMaterialDetail::Detailed,
        );
        let (reduced, reduced_stats) = build_ring_mesh(
            &sampler,
            spec,
            WorldProfile::AstralFrontier,
            FarFieldMaterialDetail::Reduced,
        );
        assert_eq!(detailed.positions, reduced.positions);
        assert_eq!(detailed.indices, reduced.indices);
        assert!(detailed_stats.biome_queries > 0);
        assert_eq!(reduced_stats.biome_queries, 0);
        assert_eq!(detailed_stats.height_queries, reduced_stats.height_queries);
        assert_eq!(detailed_stats.material_slope_queries, 0);
        assert_eq!(reduced_stats.material_slope_queries, 0);
    }

    #[test]
    fn coarse_morph_is_exact_on_shared_lattice_points() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let spec = RingSpec::for_level(2, 192, WorldXZ::ZERO);
        let mut stats = SamplingStats::default();
        let cache =
            RingSampleCache::cold(&sampler, world, spec, &mut stats, FarFieldCacheUpdate::Cold);
        let grid_edge = FAR_FIELD_GRID_CELLS / 2;
        let exact = cache.height(grid_edge, grid_edge);
        let morphed = morphed_cached_height(&cache, grid_edge, grid_edge, spec);
        assert!((exact - morphed).abs() < f32::EPSILON);
    }

    #[test]
    fn skirts_close_only_the_outer_horizon_never_the_camera_hole() {
        let sampler = TestSampler;
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let top_vertices = FAR_FIELD_GRID_VERTICES as usize * FAR_FIELD_GRID_VERTICES as usize;

        for level in 0..FAR_FIELD_LEVELS {
            let spec = RingSpec::for_level(level, first_inner, WorldXZ::ZERO);
            let (mesh, _) = build_ring_mesh(
                &sampler,
                spec,
                WorldProfile::Natural,
                FarFieldMaterialDetail::Detailed,
            );
            let skirt_vertices = &mesh.positions[top_vertices..];
            assert!(!skirt_vertices.is_empty());
            assert_eq!(skirt_vertices.len() % 4, 0);

            for edge in skirt_vertices.chunks_exact(4) {
                for endpoint in &edge[..2] {
                    let edge_extent = endpoint[0].abs().max(endpoint[2].abs());
                    assert_eq!(edge_extent, spec.outer_extent as f32);
                    assert!(
                        edge_extent >= spec.inner_extent as f32,
                        "level {level} emitted an inner-hole skirt at {endpoint:?}"
                    );
                }
                assert_eq!(edge[0][0], edge[2][0]);
                assert_eq!(edge[0][2], edge[2][2]);
                assert!(edge[2][1] < edge[0][1]);
                assert_eq!(edge[1][0], edge[3][0]);
                assert_eq!(edge[1][2], edge[3][2]);
                assert!(edge[3][1] < edge[1][1]);
            }
        }
    }

    #[test]
    fn all_hole_sizes_stay_inside_hard_geometry_budgets() {
        let sampler = TestSampler;
        let mut worst_vertices = 0usize;
        let mut worst_indices = 0usize;
        let outer = (FAR_FIELD_GRID_CELLS as i64 / 2) * FAR_FIELD_BASE_STEP_METRES;
        for first_inner in (0..=outer - FAR_FIELD_BASE_STEP_METRES * 2)
            .step_by(FAR_FIELD_BASE_STEP_METRES as usize)
        {
            let mut vertices = 0usize;
            let mut indices = 0usize;
            for level in 0..FAR_FIELD_LEVELS {
                let spec = RingSpec::for_level(level, first_inner, WorldXZ::ZERO);
                let (mesh, _) = build_ring_mesh(
                    &sampler,
                    spec,
                    WorldProfile::AstralFrontier,
                    FarFieldMaterialDetail::Detailed,
                );
                assert!(mesh.vertex_count() <= FAR_FIELD_MAX_RING_VERTICES);
                assert!(mesh.index_count() <= FAR_FIELD_MAX_RING_INDICES);
                assert!(
                    mesh_payload_bytes(mesh.vertex_count(), mesh.index_count())
                        <= FAR_FIELD_MAX_RING_BUILD_BYTES
                );
                vertices += mesh.vertex_count();
                indices += mesh.index_count();
            }
            worst_vertices = worst_vertices.max(vertices);
            worst_indices = worst_indices.max(indices);
        }
        assert!(
            worst_vertices <= FAR_FIELD_MAX_VERTICES,
            "{worst_vertices} > {FAR_FIELD_MAX_VERTICES}"
        );
        assert!(
            worst_indices <= FAR_FIELD_MAX_INDICES,
            "{worst_indices} > {FAR_FIELD_MAX_INDICES}"
        );
    }

    #[test]
    fn thousand_kilometre_travel_keeps_mesh_counts_constant() {
        let sampler = TestSampler;
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let distances_metres = [0_i64, 1_000, 10_000, 100_000, 1_000_000];
        let mut baseline = None;
        for distance in distances_metres {
            let camera = WorldXZ::new(distance, -distance / 3);
            let mut counts = Vec::new();
            for level in 0..FAR_FIELD_LEVELS {
                let spec = RingSpec::for_level(level, first_inner, camera);
                let (mesh, _) = build_ring_mesh(
                    &sampler,
                    spec,
                    WorldProfile::AstralFrontier,
                    FarFieldMaterialDetail::Detailed,
                );
                counts.push((mesh.vertex_count(), mesh.index_count()));
            }
            if let Some(expected) = &baseline {
                assert_eq!(&counts, expected, "budget changed at {distance} metres");
            } else {
                baseline = Some(counts);
            }
        }
    }

    #[test]
    fn six_ring_one_kilometre_jump_reuses_all_overlapping_height_samples() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let initial_camera = WorldXZ::ZERO;
        let moved_camera = WorldXZ::new(1_024, 0);
        let mut caches: [Option<RingSampleCache>; FAR_FIELD_LEVELS] = std::array::from_fn(|_| None);

        let mut cold_queries = 0usize;
        for (level, cache_slot) in caches.iter_mut().enumerate() {
            let spec = RingSpec::for_level(level, first_inner, initial_camera);
            let (_, stats, cache) = build_ring_mesh_incremental(
                &sampler,
                world,
                spec,
                world.profile,
                FarFieldMaterialDetail::Reduced,
                None,
            );
            cold_queries += stats.height_queries;
            *cache_slot = Some(cache);
        }
        assert_eq!(
            cold_queries,
            FAR_FIELD_LEVELS * FAR_FIELD_SAMPLE_CACHE_CELLS
        );

        let mut incremental_queries = 0usize;
        let mut expected_shift = 32usize;
        for (level, cache_slot) in caches.iter_mut().enumerate() {
            let spec = RingSpec::for_level(level, first_inner, moved_camera);
            let (_, stats, cache) = build_ring_mesh_incremental(
                &sampler,
                world,
                spec,
                world.profile,
                FarFieldMaterialDetail::Reduced,
                cache_slot.take(),
            );
            assert_eq!(stats.cache_update, FarFieldCacheUpdate::IncrementalStrip);
            assert_eq!(
                stats.height_queries,
                FAR_FIELD_SAMPLE_CACHE_SIDE * expected_shift
            );
            incremental_queries += stats.height_queries;
            *cache_slot = Some(cache);
            expected_shift /= 2;
        }
        assert_eq!(incremental_queries, 4_095);
        assert!(incremental_queries * 6 < cold_queries);
    }

    #[test]
    fn extreme_signed_anchors_and_teleports_remain_finite_and_bounded() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let low = RingSpec::for_level(0, 192, WorldXZ::new(i64::MIN, i64::MAX));
        let (low_mesh, low_stats, cache) = build_ring_mesh_incremental(
            &sampler,
            world,
            low,
            world.profile,
            FarFieldMaterialDetail::Reduced,
            None,
        );
        assert_eq!(low_stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
        assert!(low_mesh
            .positions
            .iter()
            .flatten()
            .all(|value| value.is_finite()));

        let high = RingSpec::for_level(0, 192, WorldXZ::new(i64::MAX, i64::MIN));
        let (high_mesh, high_stats, _) = build_ring_mesh_incremental(
            &sampler,
            world,
            high,
            world.profile,
            FarFieldMaterialDetail::Reduced,
            Some(cache),
        );
        assert_eq!(
            high_stats.cache_update,
            FarFieldCacheUpdate::TeleportFallback
        );
        assert_eq!(high_stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
        assert!(high_mesh
            .positions
            .iter()
            .flatten()
            .all(|value| value.is_finite()));
        assert!(high_mesh.vertex_count() <= FAR_FIELD_MAX_VERTICES);
        assert!(high_mesh.index_count() <= FAR_FIELD_MAX_INDICES);
    }

    #[test]
    fn twenty_thousand_kilometre_route_has_no_cache_or_work_growth() {
        let sampler = TestSampler;
        let world = test_world(WorldProfile::AstralFrontier);
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let initial = RingSpec::for_level(0, first_inner, WorldXZ::ZERO);
        let mut initial_stats = SamplingStats::default();
        let mut cache = RingSampleCache::cold(
            &sampler,
            world,
            initial,
            &mut initial_stats,
            FarFieldCacheUpdate::Cold,
        );
        assert_eq!(initial_stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);

        // 200 fixed-memory legs of 100 km cover 20,000 km. Every leg exceeds
        // window overlap and must take the same bounded full-window fallback.
        for leg in 1_i64..=200 {
            let distance = leg * 100_000;
            let spec = RingSpec::for_level(
                0,
                first_inner,
                WorldXZ::new(distance, -distance.div_euclid(3)),
            );
            let mut stats = SamplingStats::default();
            cache = cache.retarget(&sampler, world, spec, &mut stats);
            assert_eq!(stats.cache_update, FarFieldCacheUpdate::TeleportFallback);
            assert_eq!(stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
            assert_eq!(cache.heights.len(), FAR_FIELD_SAMPLE_CACHE_CELLS);
            assert_eq!(cache.biomes.len(), FAR_FIELD_SAMPLE_CACHE_CELLS);
            assert_eq!(cache.biome_valid.len(), FAR_FIELD_SAMPLE_CACHE_CELLS);
            assert_eq!(cache.surface_families.len(), FAR_FIELD_SAMPLE_CACHE_CELLS);
            assert_eq!(
                cache.surface_family_valid.len(),
                FAR_FIELD_SAMPLE_CACHE_CELLS
            );
            assert!(RING_SAMPLE_CACHE_ACCOUNTED_BYTES <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES);
        }
    }

    #[test]
    fn real_terrain_ring_replays_byte_stably() {
        let generator = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Balanced);
        let spec = RingSpec::for_level(0, 192, WorldXZ::new(12_345, -54_321));
        let (first, first_stats) = build_ring_mesh(
            &generator,
            spec,
            WorldProfile::AstralFrontier,
            FarFieldMaterialDetail::Detailed,
        );
        let (second, second_stats) = build_ring_mesh(
            &generator,
            spec,
            WorldProfile::AstralFrontier,
            FarFieldMaterialDetail::Detailed,
        );
        assert_eq!(first.positions, second.positions);
        assert_eq!(first.colors, second.colors);
        assert_eq!(first.indices, second.indices);
        assert_eq!(first_stats, second_stats);
    }

    #[test]
    fn real_terrain_incremental_ring_matches_cold_target() {
        let generator = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Balanced);
        let world = test_world(WorldProfile::AstralFrontier);
        let first = RingSpec::for_level(0, 192, WorldXZ::new(12_345, -54_321));
        let (_, _, cache) = build_ring_mesh_incremental(
            &generator,
            world,
            first,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        let target = RingSpec::for_level(
            0,
            192,
            WorldXZ::new(first.anchor.x + first.step, first.anchor.z - first.step),
        );
        let (incremental, incremental_stats, _) = build_ring_mesh_incremental(
            &generator,
            world,
            target,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            Some(cache),
        );
        let (cold, cold_stats, _) = build_ring_mesh_incremental(
            &generator,
            world,
            target,
            world.profile,
            FarFieldMaterialDetail::Detailed,
            None,
        );
        assert_eq!(incremental.positions, cold.positions);
        assert_eq!(incremental.normals, cold.normals);
        assert_eq!(incremental.colors, cold.colors);
        assert_eq!(incremental.indices, cold.indices);
        assert_eq!(incremental_stats.height_queries, 129);
        assert_eq!(cold_stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
    }

    #[test]
    #[ignore = "manual hydro distribution benchmark; run with --ignored --nocapture"]
    fn benchmark_hydrography_distribution() {
        let sampler = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::Natural)
            .with_scenery_quality(SceneryQuality::Balanced);
        let spec = RingSpec::for_level(1, 0, WorldXZ::new(-4_352, 7_744));
        let mut disabled = test_world(WorldProfile::Natural);
        disabled.hydro_mode = FarFieldHydroMode::Disabled;
        let enabled = FarFieldWorldKey {
            hydro_mode: FarFieldHydroMode::DescriptiveV1,
            ..disabled
        };
        let mut disabled_ms = Vec::with_capacity(25);
        let mut enabled_ms = Vec::with_capacity(25);
        let mut enabled_queries = Vec::with_capacity(25);
        for iteration in 0..25 {
            let modes = if iteration % 2 == 0 {
                [disabled, enabled]
            } else {
                [enabled, disabled]
            };
            for world in modes {
                let started = Instant::now();
                let (_, fluid, stats, _) = build_ring_mesh_incremental_with_coverage_and_hydro(
                    &sampler,
                    world,
                    spec,
                    world.profile,
                    FarFieldMaterialDetail::Reduced,
                    None,
                    NearCoverageMask::default(),
                );
                let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
                if world.hydro_mode == FarFieldHydroMode::Disabled {
                    assert!(fluid.is_empty());
                    disabled_ms.push(elapsed);
                } else {
                    assert!(
                        stats.fluid_classification_queries
                            <= FAR_FIELD_MAX_FLUID_CLASSIFICATION_QUERIES_PER_RING
                    );
                    assert!(
                        stats.fluid_biome_queries <= FAR_FIELD_MAX_FLUID_BIOME_QUERIES_PER_RING
                    );
                    enabled_queries.push((
                        stats.fluid_classification_queries,
                        stats.fluid_biome_queries,
                    ));
                    enabled_ms.push(elapsed);
                }
            }
        }
        disabled_ms.sort_by(f64::total_cmp);
        enabled_ms.sort_by(f64::total_cmp);
        let p50 = |samples: &[f64]| samples[samples.len() / 2];
        let p95 = |samples: &[f64]| samples[samples.len() * 95 / 100];
        println!(
            "hydro distribution (25 cold L1 builds): off p50 {:.3} ms p95 {:.3} ms; v1 p50 {:.3} ms p95 {:.3} ms; query pairs {:?}",
            p50(&disabled_ms),
            p95(&disabled_ms),
            p50(&enabled_ms),
            p95(&enabled_ms),
            enabled_queries
        );
    }

    #[test]
    #[ignore = "manual A/B distribution benchmark; run with --ignored --nocapture"]
    fn benchmark_surface_material_bridge_distribution() {
        let sampler = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::Natural)
            .with_scenery_quality(SceneryQuality::Balanced);
        let spec = RingSpec::for_level(1, 0, WorldXZ::new(-4_352, 7_744));
        let mut legacy_world = test_world(WorldProfile::Natural);
        legacy_world.surface_material_mode = FarFieldSurfaceMaterialMode::LegacyPalette;
        let mut bridge_v1_world = legacy_world;
        bridge_v1_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV1;
        let mut bridge_v2_world = legacy_world;
        bridge_v2_world.surface_material_mode = FarFieldSurfaceMaterialMode::BridgeV2;
        let mut legacy_ms = Vec::with_capacity(25);
        let mut bridge_v1_ms = Vec::with_capacity(25);
        let mut bridge_v2_ms = Vec::with_capacity(25);

        for iteration in 0..25 {
            // Rotate order to avoid consistently gifting one mode the warmer
            // instruction/data-cache position within an iteration.
            let modes = match iteration % 3 {
                0 => [legacy_world, bridge_v1_world, bridge_v2_world],
                1 => [bridge_v1_world, bridge_v2_world, legacy_world],
                _ => [bridge_v2_world, legacy_world, bridge_v1_world],
            };
            for world in modes {
                let started = Instant::now();
                let (_, stats, _) = build_ring_mesh_incremental(
                    &sampler,
                    world,
                    spec,
                    world.profile,
                    FarFieldMaterialDetail::Detailed,
                    None,
                );
                let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
                assert_eq!(stats.height_queries, FAR_FIELD_SAMPLE_CACHE_CELLS);
                match world.surface_material_mode {
                    FarFieldSurfaceMaterialMode::LegacyPalette => {
                        assert_eq!(stats.biome_queries, 289);
                        assert_eq!(stats.material_slope_queries, 0);
                        legacy_ms.push(elapsed_ms);
                    }
                    FarFieldSurfaceMaterialMode::BridgeV1 => {
                        assert_eq!(
                            stats.biome_queries,
                            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING
                        );
                        assert_eq!(
                            stats.material_slope_queries,
                            FAR_FIELD_MAX_BRIDGE_SLOPE_QUERIES_PER_RING
                        );
                        bridge_v1_ms.push(elapsed_ms);
                    }
                    FarFieldSurfaceMaterialMode::BridgeV2 => {
                        assert_eq!(stats.biome_queries, 31 * 31);
                        assert_eq!(
                            stats.bridge_v2_cell_reuses,
                            FAR_FIELD_MAX_BRIDGE_FAMILY_QUERIES_PER_RING - 31 * 31
                        );
                        assert_eq!(stats.material_slope_queries, 0);
                        bridge_v2_ms.push(elapsed_ms);
                    }
                }
            }
        }
        legacy_ms.sort_by(f64::total_cmp);
        bridge_v1_ms.sort_by(f64::total_cmp);
        bridge_v2_ms.sort_by(f64::total_cmp);
        let p50 = |samples: &[f64]| samples[samples.len() / 2];
        let p95 = |samples: &[f64]| samples[(samples.len() * 95).div_ceil(100) - 1];
        println!(
            "surface material distribution (25 cold L1 builds each): legacy p50 {:.3} ms p95 {:.3} ms; bridge-v1 p50 {:.3} ms p95 {:.3} ms; bridge-v2 p50 {:.3} ms p95 {:.3} ms",
            p50(&legacy_ms),
            p95(&legacy_ms),
            p50(&bridge_v1_ms),
            p95(&bridge_v1_ms),
            p50(&bridge_v2_ms),
            p95(&bridge_v2_ms),
        );
    }

    #[test]
    #[ignore = "manual microbenchmark; run with --ignored --nocapture"]
    fn benchmark_full_six_ring_rebuild_with_real_terrain_sampler() {
        let generator = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Balanced);
        let camera = WorldXZ::new(12_345, -54_321);
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let started = Instant::now();
        let mut vertices = 0usize;
        let mut indices = 0usize;
        let mut height_queries = 0usize;
        let mut material_slope_queries = 0usize;
        let mut biome_queries = 0usize;
        for level in 0..FAR_FIELD_LEVELS {
            let spec = RingSpec::for_level(level, first_inner, camera);
            let (mesh, stats) = build_ring_mesh(
                &generator,
                spec,
                WorldProfile::AstralFrontier,
                FarFieldMaterialDetail::Detailed,
            );
            vertices += mesh.vertex_count();
            indices += mesh.index_count();
            height_queries += stats.height_queries;
            material_slope_queries += stats.material_slope_queries;
            biome_queries += stats.biome_queries;
        }
        let elapsed = started.elapsed();
        println!(
            "cold six-ring real sampler: {:.3} ms, {vertices} vertices, {indices} indices, {height_queries} geometry-height + {material_slope_queries} material-slope + {biome_queries} biome queries",
            elapsed.as_secs_f64() * 1_000.0
        );
        assert!(vertices <= FAR_FIELD_MAX_VERTICES);
        assert!(indices <= FAR_FIELD_MAX_INDICES);
    }

    #[test]
    #[ignore = "manual distribution benchmark; run with --ignored --nocapture"]
    fn benchmark_incremental_route_distribution_with_real_terrain_sampler() {
        let generator = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Balanced);
        let world = test_world(WorldProfile::AstralFrontier);
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let mut caches: [Option<RingSampleCache>; FAR_FIELD_LEVELS] = std::array::from_fn(|_| None);
        let mut resident_specs =
            [RingSpec::for_level(0, first_inner, WorldXZ::ZERO); FAR_FIELD_LEVELS];

        for level in 0..FAR_FIELD_LEVELS {
            let spec = RingSpec::for_level(level, first_inner, WorldXZ::ZERO);
            let (_, _, cache) = build_ring_mesh_incremental(
                &generator,
                world,
                spec,
                world.profile,
                FarFieldMaterialDetail::Detailed,
                None,
            );
            caches[level] = Some(cache);
            resident_specs[level] = spec;
        }

        let mut samples_ms = Vec::with_capacity(64);
        let mut total_height_queries = 0usize;
        let mut total_biome_queries = 0usize;
        for base_step in 1_i64..=32 {
            let camera = WorldXZ::new(
                base_step * FAR_FIELD_BASE_STEP_METRES,
                -base_step * FAR_FIELD_BASE_STEP_METRES,
            );
            for level in 0..FAR_FIELD_LEVELS {
                let target = RingSpec::for_level(level, first_inner, camera);
                if target == resident_specs[level] {
                    continue;
                }
                let started = Instant::now();
                let (_, stats, cache) = build_ring_mesh_incremental(
                    &generator,
                    world,
                    target,
                    world.profile,
                    FarFieldMaterialDetail::Detailed,
                    caches[level].take(),
                );
                samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
                total_height_queries += stats.height_queries;
                total_biome_queries += stats.biome_queries;
                assert_eq!(stats.cache_update, FarFieldCacheUpdate::IncrementalStrip);
                caches[level] = Some(cache);
                resident_specs[level] = target;
            }
        }

        samples_ms.sort_by(f64::total_cmp);
        let percentile = |numerator: usize, denominator: usize| {
            let index = ((samples_ms.len() - 1) * numerator).div_ceil(denominator);
            samples_ms[index]
        };
        println!(
            "incremental 32-step diagonal route: {} jobs, min {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms, {} height + {} biome queries",
            samples_ms.len(),
            samples_ms[0],
            percentile(1, 2),
            percentile(95, 100),
            samples_ms[samples_ms.len() - 1],
            total_height_queries,
            total_biome_queries,
        );
        assert!(!samples_ms.is_empty());
        assert!(samples_ms.len() <= 32 * FAR_FIELD_LEVELS);
        assert!(total_height_queries <= samples_ms.len() * (FAR_FIELD_SAMPLE_CACHE_SIDE * 2 - 1));
        assert!(
            caches.iter().filter(|cache| cache.is_some()).count()
                * RING_SAMPLE_CACHE_ACCOUNTED_BYTES
                <= FAR_FIELD_MAX_SAMPLE_CACHE_BYTES
        );
    }

    #[test]
    #[ignore = "manual teleport distribution benchmark; run with --ignored --nocapture"]
    fn benchmark_teleport_fallback_distribution_with_real_terrain_sampler() {
        let generator = TerrainGenerator::new(91_337)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(SceneryQuality::Balanced);
        let world = test_world(WorldProfile::AstralFrontier);
        let first_inner = FAR_FIELD_FINEST_INNER_EXTENT_METRES;
        let mut caches: [Option<RingSampleCache>; FAR_FIELD_LEVELS] = std::array::from_fn(|_| None);
        for (level, cache_slot) in caches.iter_mut().enumerate() {
            let spec = RingSpec::for_level(level, first_inner, WorldXZ::ZERO);
            let (_, _, cache) = build_ring_mesh_incremental(
                &generator,
                world,
                spec,
                world.profile,
                FarFieldMaterialDetail::Detailed,
                None,
            );
            *cache_slot = Some(cache);
        }

        let mut samples_ms = Vec::with_capacity(9);
        for jump in 1_i64..=9 {
            let distance = jump * 20_000_000;
            let camera = if jump % 2 == 0 {
                WorldXZ::new(-distance, distance / 3)
            } else {
                WorldXZ::new(distance, -distance / 3)
            };
            let started = Instant::now();
            let mut height_queries = 0usize;
            for (level, cache_slot) in caches.iter_mut().enumerate() {
                let target = RingSpec::for_level(level, first_inner, camera);
                let (_, stats, cache) = build_ring_mesh_incremental(
                    &generator,
                    world,
                    target,
                    world.profile,
                    FarFieldMaterialDetail::Detailed,
                    cache_slot.take(),
                );
                assert_eq!(stats.cache_update, FarFieldCacheUpdate::TeleportFallback);
                height_queries += stats.height_queries;
                *cache_slot = Some(cache);
            }
            assert_eq!(
                height_queries,
                FAR_FIELD_LEVELS * FAR_FIELD_SAMPLE_CACHE_CELLS
            );
            samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        }

        samples_ms.sort_by(f64::total_cmp);
        println!(
            "six-ring teleport fallback: 9 batches, min {:.3} ms, p50 {:.3} ms, p95/max {:.3} ms, {} height queries/batch",
            samples_ms[0],
            samples_ms[4],
            samples_ms[8],
            FAR_FIELD_LEVELS * FAR_FIELD_SAMPLE_CACHE_CELLS,
        );
    }

    #[test]
    fn query_saturation_is_explicit_not_overflowing() {
        let (high, high_clamped) = terrain_coordinate(i64::MAX);
        let (low, low_clamped) = terrain_coordinate(i64::MIN);
        assert!(high_clamped && low_clamped);
        assert!(high < i32::MAX);
        assert!(low > i32::MIN);
    }
}
