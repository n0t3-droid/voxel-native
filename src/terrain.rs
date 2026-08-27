//! Terrain generation.
//!
//! Voxel Native's deterministic terrain stack is:
//!
//!   1. Continentalness + erosion FBM (low freq) â†’ large-scale landmass shape.
//!   2. Domain-warped FBM (mid freq) â†’ organic-looking hills.
//!   3. Ridged FBM â†’ mountain ridges in high-continentalness areas.
//!   4. 3D narrow-band cave noise â†’ hollows under the surface.
//!   5. Temperature + Moisture classifier â†’ biome â†’ surface block palette.
//!
//! Each noise layer is seeded deterministically off the world seed. Two
//! generators with the same complete [`WorldGenerationIdentity`] produce
//! byte-identical chunks; the persisted grammar prevents a newer formula from
//! silently being mistaken for the same world.

use crate::blocks::{BlockType, Voxel, AIR};
use crate::chunk::{Chunk, ChunkPos, CHUNK_SIZE, CHUNK_SIZE_I};
use crate::settings::{TerrainGrammarVersion, WorldGenerationIdentity, WorldProfile};
use bevy::math::IVec2;
use noise::{NoiseFn, Perlin};

pub const WATER_LEVEL: i32 = 48;
pub const BEDROCK_LEVEL: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Biome {
    Ocean,
    Beach,
    Plains,
    Forest,
    Jungle,
    Desert,
    Savanna,
    Tundra,
    SnowyMountains,
    Mountains,
    /// Iconic American canyon: red sandstone mesas, bone-dry plateaus.
    Mesa,
    /// Chinese karst: tall green-mossy limestone pillars amid jungle.
    Karst,
    // ---- Alien planetary biomes (sniper-shooter playgrounds) ----
    /// Pandora-style towering cyan crystal spires over a glowing pale
    /// sand floor. Vertical sniper nests + long horizontal sightlines
    /// underneath the spire canopy.
    CrystalSpires,
    /// Mars/Io basalt plains laced with bright lava rivers.
    /// Wide-open kill corridors broken by impassable lava channels.
    VolcanicWaste,
    /// Hoth-style razor ice ridges and crevasses. Long-bowl shots
    /// between ridges; ridge-lines double as ambush cover.
    GlacierShards,
    /// Bioluminescent purple moss with bone-white pillar arches.
    /// Mid-range cover-and-move terrain.
    AlienReef,
}

/// Stable render-only grammar for kilometre-scale semantic silhouettes.
///
/// These categories describe authored visual language, not simulation or
/// voxel authority. The far-field renderer may use them to add landmarks;
/// terrain generation, saves, collisions, vegetation, and resource logic do
/// not consume them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FarSemanticCohortKind {
    NaturalGrove,
    NaturalKarst,
    NaturalMesa,
    AstralCrystal,
    AstralBasalt,
    AstralReef,
}

impl FarSemanticCohortKind {
    pub(crate) const COUNT: usize = 6;

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::NaturalGrove => 0,
            Self::NaturalKarst => 1,
            Self::NaturalMesa => 2,
            Self::AstralCrystal => 3,
            Self::AstralBasalt => 4,
            Self::AstralReef => 5,
        }
    }
}

/// Query-free first phase of Far Semantic Cohorts v1.
///
/// `stable_id` is a pure function of grammar version, world seed, profile,
/// and Euclidean 1,024 m cell coordinates. `admitted` is the absolute 8x8
/// supertile decision: exactly one local cell is selected per supertile, so a
/// moving viewport cannot change absolute admission. The renderer's separate
/// near-authority handoff may still intentionally suppress that cell while it
/// lies close enough to the exact voxel tier.
/// `shape_variant` supplies deterministic authored variation without a
/// random-number generator or retained state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FarSemanticCohortSignature {
    pub stable_id: u64,
    pub admitted: bool,
    pub shape_variant: u8,
}

const FAR_SEMANTIC_COHORT_GRAMMAR_V1: u64 = 0x5345_4D41_4E54_4943;
pub(crate) const FAR_SEMANTIC_COHORT_SUPERTILE_CELLS: i64 = 8;

#[inline]
const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// Deterministic semantic signature for one Euclidean kilometre cell.
pub(crate) fn far_semantic_cohort_signature(
    seed: u32,
    profile: WorldProfile,
    cell_x: i64,
    cell_z: i64,
) -> FarSemanticCohortSignature {
    let profile_tag = match profile {
        WorldProfile::Natural => 0x4E41_5455_5241_4C01,
        WorldProfile::AstralFrontier => 0x4153_5452_414C_0001,
    };
    let x = splitmix64(cell_x as u64 ^ 0xA076_1D64_78BD_642F);
    let z = splitmix64(cell_z as u64 ^ 0xE703_7ED1_A0B4_28DB);
    let stable_id = splitmix64(
        FAR_SEMANTIC_COHORT_GRAMMAR_V1
            ^ u64::from(seed).rotate_left(17)
            ^ profile_tag
            ^ x
            ^ z.rotate_left(29),
    );
    let super_x = cell_x.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
    let super_z = cell_z.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
    let super_id = splitmix64(
        FAR_SEMANTIC_COHORT_GRAMMAR_V1
            ^ u64::from(seed).rotate_right(11)
            ^ profile_tag.rotate_left(7)
            ^ splitmix64(super_x as u64)
            ^ splitmix64(super_z as u64).rotate_left(31),
    );
    let selected_x = (super_id & 7) as i64;
    let selected_z = ((super_id >> 3) & 7) as i64;
    FarSemanticCohortSignature {
        stable_id,
        admitted: cell_x.rem_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS) == selected_x
            && cell_z.rem_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS) == selected_z,
        shape_variant: (splitmix64(stable_id ^ 0x5899_65CC_7537_4CC3) & 0x0f) as u8,
    }
}

/// Second, terrain-aware phase of the semantic grammar. It runs only for the
/// fixed candidate set chosen by [`far_semantic_cohort_signature`].
pub(crate) fn far_semantic_cohort_kind(
    profile: WorldProfile,
    biome: Biome,
) -> Option<FarSemanticCohortKind> {
    match profile {
        WorldProfile::Natural => match biome {
            Biome::Ocean | Biome::Beach => None,
            Biome::Mesa | Biome::Desert => Some(FarSemanticCohortKind::NaturalMesa),
            Biome::Karst
            | Biome::Mountains
            | Biome::SnowyMountains
            | Biome::Tundra
            | Biome::GlacierShards => Some(FarSemanticCohortKind::NaturalKarst),
            Biome::Plains
            | Biome::Forest
            | Biome::Jungle
            | Biome::Savanna
            | Biome::CrystalSpires
            | Biome::VolcanicWaste
            | Biome::AlienReef => Some(FarSemanticCohortKind::NaturalGrove),
        },
        WorldProfile::AstralFrontier => match biome {
            Biome::Ocean | Biome::Beach => None,
            Biome::CrystalSpires | Biome::GlacierShards | Biome::SnowyMountains => {
                Some(FarSemanticCohortKind::AstralCrystal)
            }
            Biome::VolcanicWaste | Biome::Mesa | Biome::Desert | Biome::Mountains => {
                Some(FarSemanticCohortKind::AstralBasalt)
            }
            Biome::AlienReef
            | Biome::Karst
            | Biome::Plains
            | Biome::Forest
            | Biome::Jungle
            | Biome::Savanna
            | Biome::Tundra => Some(FarSemanticCohortKind::AstralReef),
        },
    }
}

impl Biome {
    #[inline]
    pub fn is_neon_showcase(self) -> bool {
        matches!(self, Biome::AlienReef | Biome::CrystalSpires)
    }

    #[inline]
    pub fn is_showcase_terrain(self) -> bool {
        matches!(
            self,
            Biome::AlienReef | Biome::CrystalSpires | Biome::GlacierShards | Biome::VolcanicWaste
        )
    }
}

/// Coarse exposed-material family shared by voxel terrain and render-only LODs.
///
/// `slope_rise_per_run` is dimensionless (vertical blocks/metres divided by
/// horizontal blocks/metres). The near terrain passes its one-cell cardinal
/// rise directly; coarse consumers must divide their cached height delta by
/// their sample spacing before calling this helper. Keeping this function
/// pure prevents a render LOD from silently inventing a second biome palette.
pub(crate) fn coarse_surface_family(biome: Biome, slope_rise_per_run: f32) -> BlockType {
    let slope = if slope_rise_per_run.is_finite() {
        slope_rise_per_run.max(0.0)
    } else {
        0.0
    };
    let base = TerrainGenerator::blocks_for(biome).0;

    if biome == Biome::Karst && slope >= 2.0 {
        return BlockType::Limestone;
    }

    let exposes_natural_substrate = matches!(
        biome,
        Biome::Plains
            | Biome::Forest
            | Biome::Jungle
            | Biome::Savanna
            | Biome::Tundra
            | Biome::Mountains
    );
    if exposes_natural_substrate && slope >= 4.0 {
        BlockType::Stone
    } else if exposes_natural_substrate && slope >= 2.0 {
        BlockType::Dirt
    } else {
        base
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeonSpawnPoint {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub biome: Biome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NaturalSpawnPoint {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub biome: Biome,
}

/// Dimensionless environmental conditions at one world-space column.
///
/// These values deliberately stay in `[0, 1]` instead of pretending that a
/// procedural noise sample is a calibrated degree, rainfall, or soil assay.
/// Terrain, vegetation, telemetry, and future simulation passes can therefore
/// share one stable ecological contract without coupling flight physics to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvironmentSample {
    pub temperature_norm: f32,
    pub atmospheric_moisture: f32,
    pub soil_moisture: f32,
    pub river_strength: f32,
    pub mineral_resonance: f32,
    pub flowering_resonance: f32,
    /// Unit-length tangent of the nearest major hydrographic course in X/Z.
    pub flow_direction: [f32; 2],
}

#[derive(Debug, Clone, Copy)]
struct HydrographicField {
    corridor: f64,
    channel: f64,
}

// Natural river cross-section v2 works in voxel-height units. `corridor` and
// `channel` are dimensionless [0, 1] weights; every other term is a vertical
// block count. This is an authored static-water profile, not a
// shallow-water-equation or erosion simulation.
const NATURAL_RIVER_BANK_RELIEF_BLOCKS: f64 = 6.0;

/// Byte-established generic river carve retained for Astral. The caller feeds
/// finite, generator-bounded heights and dimensionless hydro weights.
fn hydrographic_cross_section_v1(mut pre_carve_height: f64, hydro: HydrographicField) -> f64 {
    if pre_carve_height > WATER_LEVEL as f64 - 5.0 && hydro.corridor > 0.0 {
        let bank_target = WATER_LEVEL as f64 + 4.5;
        if pre_carve_height > bank_target {
            let bank_blend = (hydro.corridor * 0.46).clamp(0.0, 0.46);
            pre_carve_height = pre_carve_height * (1.0 - bank_blend) + bank_target * bank_blend;
        }
        let channel_blend = smoothstep(0.18, 0.78, hydro.channel).powf(1.15);
        if channel_blend > 0.0 {
            let bed_target = WATER_LEVEL as f64 - 2.0;
            pre_carve_height =
                pre_carve_height * (1.0 - channel_blend) + bed_target * channel_blend;
        }
    }
    pre_carve_height
}

/// Pure, total Natural-profile bank envelope. Non-finite weights fail closed
/// to no influence; a non-finite height returns the finite water-level bed for
/// the caller's final bounded integer conversion.
///
/// A bounded candidate sweep at seed 12,345 rejected the rational shoulder
/// (`T=52.5`, `K=3.5`) because it cannot alter the focus's 49 -> 52 edge.
/// Direct-envelope-only increased the active steep-edge population; stronger
/// corridor easing alone retained the old route maximum. Their combination was
/// the only tested class to improve both focus and active-route gates. The
/// exact production assertions live beside the real cross-section tests. This
/// formula adds no queries or retained state.
fn natural_hydrographic_cross_section_v2(pre_carve_height: f64, hydro: HydrographicField) -> f64 {
    let bed_height = WATER_LEVEL as f64 - 2.0;
    if !pre_carve_height.is_finite() {
        return bed_height;
    }
    let unit_weight = |value: f64| {
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let corridor = unit_weight(hydro.corridor);
    if pre_carve_height <= WATER_LEVEL as f64 - 5.0 || corridor <= 0.0 {
        return pre_carve_height;
    }

    let channel = unit_weight(hydro.channel);
    let channel_blend = smoothstep(0.18, 0.78, channel).powf(1.15);
    let target_height = bed_height + NATURAL_RIVER_BANK_RELIEF_BLOCKS * (1.0 - channel_blend);
    let envelope_height = pre_carve_height.min(target_height);
    let corridor_easing = corridor.sqrt();
    (1.0 - corridor_easing) * pre_carve_height + corridor_easing * envelope_height
}

// Natural river cross-section v3 remains an authored static-water profile.
// Heights are vertical voxel-block counts; hydrographic weights are
// dimensionless. The three relief bands encode bed -> sediment shelf ->
// living cap without claiming a calibrated metre scale or erosion model.
const NATURAL_RIVER_V3_SEDIMENT_SHELF_RELIEF_BLOCKS: f64 = 3.0;
const NATURAL_RIVER_V3_LIVING_CAP_RELIEF_BLOCKS: f64 = 2.0;
const NATURAL_RIVER_V3_LIVING_TO_SHELF_START_WEIGHT: f64 = 0.26;
const NATURAL_RIVER_V3_LIVING_TO_SHELF_END_WEIGHT: f64 = 0.50;
const NATURAL_RIVER_V3_SHELF_TO_BED_START_WEIGHT: f64 = 0.66;
const NATURAL_RIVER_V3_SHELF_TO_BED_END_WEIGHT: f64 = 0.90;

/// Natural-only v3 bank grammar: a submerged bed, an explicit sediment shelf,
/// then a low living cap. Both transitions are eased before voxel rounding,
/// so the shelf is a real horizontal band rather than an incidental sample of
/// one continuous six-block ramp.
///
/// The function is pure, total, O(1), and consumes exactly the pre-existing
/// [`HydrographicField`]. V1 and V2 remain separate byte-established paths.
fn natural_hydrographic_cross_section_v3(pre_carve_height: f64, hydro: HydrographicField) -> f64 {
    let bed_height = WATER_LEVEL as f64 - 2.0;
    if !pre_carve_height.is_finite() {
        return bed_height;
    }
    let unit_weight = |value: f64| {
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let corridor = unit_weight(hydro.corridor);
    if pre_carve_height <= WATER_LEVEL as f64 - 5.0 || corridor <= 0.0 {
        return pre_carve_height;
    }

    let channel = unit_weight(hydro.channel);
    let living_to_shelf = smoothstep(
        NATURAL_RIVER_V3_LIVING_TO_SHELF_START_WEIGHT,
        NATURAL_RIVER_V3_LIVING_TO_SHELF_END_WEIGHT,
        channel,
    );
    let shelf_to_bed = smoothstep(
        NATURAL_RIVER_V3_SHELF_TO_BED_START_WEIGHT,
        NATURAL_RIVER_V3_SHELF_TO_BED_END_WEIGHT,
        channel,
    );
    let target_height = bed_height
        + NATURAL_RIVER_V3_SEDIMENT_SHELF_RELIEF_BLOCKS * (1.0 - shelf_to_bed)
        + NATURAL_RIVER_V3_LIVING_CAP_RELIEF_BLOCKS * (1.0 - living_to_shelf);
    let envelope_height = pre_carve_height.min(target_height);

    // The fourth root reaches the authored envelope earlier across the broad
    // corridor. It cannot overshoot because both interpolation endpoints are
    // finite and the factor remains in [0, 1].
    let corridor_easing = corridor.sqrt().sqrt();
    (1.0 - corridor_easing) * pre_carve_height + corridor_easing * envelope_height
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HydrographicCrossSection {
    width: i32,
    mean_bank_height: i32,
    max_bank_height: i32,
    bank_height_span: i32,
    living_banks: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HydrographicFocusContext {
    open_water_probes: u8,
    min_surface_height: i32,
    max_surface_height: i32,
}

impl HydrographicFocusContext {
    fn relief_span(self) -> i32 {
        self.max_surface_height
            .saturating_sub(self.min_surface_height)
    }
}

/// Macro-region province. Returned by `region()` for any world (x,z).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    Plains,
    Canyon,
    Plateau,
    Highland,
    Wetland,
    Karst,
    // ---- Alien planetary regions ----
    CrystalSpires,
    VolcanicWaste,
    GlacierShards,
    AlienReef,
}

const ASTRAL_ISLAND_CELL_SIZE: i32 = 192;
const ASTRAL_ISLAND_MAX_RADIUS: i32 = 30;
const ASTRAL_ISLAND_CLEARANCE_MIN: i32 = 30;
const ASTRAL_ISLAND_CLEARANCE_VARIATION: i32 = 18;
const ASTRAL_ISLAND_CLEARANCE_MAX: i32 =
    ASTRAL_ISLAND_CLEARANCE_MIN + ASTRAL_ISLAND_CLEARANCE_VARIATION - 1;
const ASTRAL_PRECINCT_RADIUS: f64 = 440.0;
const ASTRAL_PRECINCT_STRUCTURE_RADIUS: i32 = 240;
const ASTRAL_AUTHORED_ISLAND_RADIUS: i32 = 336;
const ASTRAL_WAYPOINT_CELL_SIZE: i32 = 448;
const ASTRAL_WAYPOINT_MAX_RADIUS: i32 = 28;
const ASTRAL_WAYPOINT_HUB_EXCLUSION: i32 = 420;
const ASTRAL_WORLD_NODE_RADIUS: f64 = 112.0;
const ASTRAL_ROUTE_SEARCH_CELLS: i32 = 2;

/// Hard public-search radii. These preserve every current call site while
/// preventing an untrusted tool/save/API radius from turning a deterministic
/// locator into unbounded quadratic work.
pub const NEON_SPAWN_SEARCH_MAX_RADIUS: i32 = 16_000;
pub const NATURAL_SPAWN_SEARCH_MAX_RADIUS: i32 = 4_096;
pub const HYDROGRAPHIC_SEARCH_MAX_RADIUS: i32 = 4_096;

const NEON_SPAWN_SEARCH_STEP: i32 = 64;
const NATURAL_SPAWN_SEARCH_STEP: i32 = 32;
const HYDROGRAPHIC_SEARCH_STEP: i32 = 16;
const HYDROGRAPHIC_FOCUS_MAX_BANK_HEIGHT: i32 = WATER_LEVEL + 28;
const HYDROGRAPHIC_FOCUS_MAX_BANK_SPAN: i32 = 20;
const HYDROGRAPHIC_FOCUS_MAX_CONTEXT_HEIGHT: i32 = WATER_LEVEL + 48;
const HYDROGRAPHIC_FOCUS_MAX_CONTEXT_RELIEF: i32 = 52;

const fn square_search_candidate_cap(max_radius: i32, step: i32) -> usize {
    let rings = (max_radius / step) as usize;
    // Centre plus the eight unique perimeter rays accumulated over all rings:
    // 1 + 8 * sum(1..=rings) = 1 + 4*rings*(rings+1).
    1 + 4 * rings * (rings + 1)
}

pub const NEON_SPAWN_SEARCH_MAX_CANDIDATES: usize =
    square_search_candidate_cap(NEON_SPAWN_SEARCH_MAX_RADIUS, NEON_SPAWN_SEARCH_STEP);
pub const NATURAL_SPAWN_SEARCH_MAX_CANDIDATES: usize =
    square_search_candidate_cap(NATURAL_SPAWN_SEARCH_MAX_RADIUS, NATURAL_SPAWN_SEARCH_STEP);
pub const HYDROGRAPHIC_SEARCH_MAX_CANDIDATES: usize =
    square_search_candidate_cap(HYDROGRAPHIC_SEARCH_MAX_RADIUS, HYDROGRAPHIC_SEARCH_STEP);

const _: () = assert!(NEON_SPAWN_SEARCH_MAX_CANDIDATES == 251_001);
const _: () = assert!(NATURAL_SPAWN_SEARCH_MAX_CANDIDATES == 66_049);
const _: () = assert!(HYDROGRAPHIC_SEARCH_MAX_CANDIDATES == 263_169);

#[inline]
fn bounded_search_radius(requested: i32, step: i32, hard_max: i32) -> i32 {
    requested.max(step).min(hard_max)
}

/// Visit one square perimeter without allocating a temporary ring. All loop
/// arithmetic stays in i64; points outside the public i32 column domain count
/// against the work budget but are skipped rather than wrapped.
fn visit_bounded_square_perimeter(
    origin_x: i32,
    origin_z: i32,
    radius: i32,
    step: i32,
    visited: &mut usize,
    visit_cap: usize,
    mut visit: impl FnMut(i32, i32),
) -> bool {
    debug_assert!(radius >= 0);
    debug_assert!(step > 0);

    let mut visit_point = |x: i64, z: i64| {
        let Some(next) = visited.checked_add(1) else {
            return false;
        };
        if next > visit_cap {
            return false;
        }
        *visited = next;
        if let (Ok(x), Ok(z)) = (i32::try_from(x), i32::try_from(z)) {
            visit(x, z);
        }
        true
    };

    if radius == 0 {
        return visit_point(i64::from(origin_x), i64::from(origin_z));
    }

    let radius = i64::from(radius);
    let step = i64::from(step);
    let min_x = i64::from(origin_x) - radius;
    let max_x = i64::from(origin_x) + radius;
    let min_z = i64::from(origin_z) - radius;
    let max_z = i64::from(origin_z) + radius;

    let mut x = min_x;
    while x <= max_x {
        if !visit_point(x, min_z) || !visit_point(x, max_z) {
            return false;
        }
        x += step;
    }

    let mut z = min_z + step;
    while z <= max_z - step {
        if !visit_point(min_x, z) || !visit_point(max_x, z) {
            return false;
        }
        z += step;
    }
    true
}

/// Preserve a coherent geological shell above procedural underground space.
/// Steep terrain needs a deeper shell because a constant vertical buffer can
/// still be exposed sideways by the neighbouring lower column.
#[inline]
fn subterranean_surface_skin(slope: i32) -> i32 {
    12 + slope.clamp(0, 8) * 2
}

/// Seed-stable authored composition around the first Astral Frontier entry.
///
/// Procedural biomes still cover the infinite world, but a completely random
/// first view cannot promise the hierarchy required by the game's flight and
/// building loop. This inexpensive layout supplies one coherent hero sector
/// per seed while quarter-turn rotation and translation prevent every world
/// from reading as the same compass-aligned set piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AstralFrontierLayout {
    hub: IVec2,
    quarter_turns: u8,
}

impl AstralFrontierLayout {
    fn for_seed(seed: u32) -> Self {
        let mixed_x = seed.wrapping_mul(0x9E37_79B9).rotate_left(7);
        let mixed_z = seed.wrapping_mul(0x85EB_CA6B).rotate_right(9);
        let offset_x = ((mixed_x % 5) as i32 - 2) * 32;
        let offset_z = ((mixed_z % 5) as i32 - 2) * 32;
        Self {
            hub: IVec2::new(offset_x, offset_z),
            quarter_turns: ((seed ^ seed.rotate_left(13)) & 3) as u8,
        }
    }

    fn rotate_quarters_i64(x: i64, z: i64, turns: u8) -> (i64, i64) {
        match turns & 3 {
            0 => (x, z),
            1 => (-z, x),
            2 => (-x, -z),
            _ => (z, -x),
        }
    }

    fn saturating_ivec2(x: i64, z: i64) -> IVec2 {
        IVec2::new(
            x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            z.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        )
    }

    fn rotate_quarters(point: IVec2, turns: u8) -> IVec2 {
        let (x, z) = Self::rotate_quarters_i64(i64::from(point.x), i64::from(point.y), turns);
        Self::saturating_ivec2(x, z)
    }

    fn rotate_quarters_f64(x: f64, z: f64, turns: u8) -> (f64, f64) {
        match turns & 3 {
            0 => (x, z),
            1 => (-z, x),
            2 => (-x, -z),
            _ => (z, -x),
        }
    }

    fn world_from_local(self, point: IVec2) -> IVec2 {
        let (x, z) =
            Self::rotate_quarters_i64(i64::from(point.x), i64::from(point.y), self.quarter_turns);
        Self::saturating_ivec2(x + i64::from(self.hub.x), z + i64::from(self.hub.y))
    }

    fn local_from_world(self, point: IVec2) -> IVec2 {
        let delta_x = i64::from(point.x) - i64::from(self.hub.x);
        let delta_z = i64::from(point.y) - i64::from(self.hub.y);
        let (x, z) = Self::rotate_quarters_i64(delta_x, delta_z, (4 - self.quarter_turns) & 3);
        Self::saturating_ivec2(x, z)
    }

    fn landing(self) -> IVec2 {
        self.world_from_local(IVec2::new(-124, 24))
    }

    fn observatory(self) -> IVec2 {
        self.world_from_local(IVec2::new(142, 112))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FloatingIslandSpec {
    center: bevy::math::IVec3,
    radius_x: i32,
    radius_z: i32,
    thickness: i32,
    cap: BlockType,
    sub: BlockType,
    core: BlockType,
    tip: BlockType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AstralWaypointKind {
    RelaySpire,
    SkyDock,
    CrystalGarden,
    TransitGate,
}

/// Height-field anchor shared by the regional landform and destination
/// grammars. It deliberately contains no sampled surface height: deriving an
/// anchor never calls back into `surface_height`, so the whole-world pass is
/// recursion-free and generation order cannot affect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AstralMacroNode {
    center: IVec2,
    kind: AstralWaypointKind,
    quarter_turns: u8,
}

impl AstralMacroNode {
    fn terrain_target(self) -> f64 {
        match self.kind {
            AstralWaypointKind::RelaySpire => 98.0,
            AstralWaypointKind::SkyDock => 86.0,
            AstralWaypointKind::CrystalGarden => 72.0,
            AstralWaypointKind::TransitGate => 80.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AstralRouteSpec {
    start: AstralMacroNode,
    end: AstralMacroNode,
    accent: BlockType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AstralWaypointSpec {
    center: bevy::math::IVec3,
    kind: AstralWaypointKind,
    quarter_turns: u8,
    radius: i32,
    height: i32,
    platform: BlockType,
    accent: BlockType,
}

impl AstralWaypointSpec {
    fn top(self) -> i32 {
        self.center.y.saturating_add(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeSilhouette {
    Conifer,
    Layered,
    Windswept,
    Crowned,
    /// Moist river-gallery tree with broad lateral limbs and hanging foliage.
    /// This is selected from the hydrographic habitat, never from a global
    /// random species roll, so it traces water courses as a readable biome.
    Riparian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TreeProfile {
    trunk_height: i32,
    canopy_radius: i32,
    branch_reach: i32,
    tiers: usize,
    max_extent: i32,
    crown_lift: i32,
    silhouette: TreeSilhouette,
}

impl TreeProfile {
    fn total_height(self) -> i32 {
        self.trunk_height + self.crown_lift + 1
    }
}

/// Ecological understorey is restricted to climates where a low woody/leafy
/// layer is visually plausible. Density is dimensionless and later modulated
/// by moisture, a cohort field, scenery quality, and local slope.
fn understory_profile(biome: Biome, style_roll: f64) -> Option<(BlockType, f64)> {
    match biome {
        Biome::Plains => Some((
            if style_roll < 0.16 {
                BlockType::BlossomLeaves
            } else {
                BlockType::Leaves
            },
            0.30,
        )),
        Biome::Forest => Some((
            if style_roll < 0.12 {
                BlockType::BlossomLeaves
            } else {
                BlockType::Leaves
            },
            0.78,
        )),
        Biome::Jungle => Some((BlockType::JungleLeaves, 0.90)),
        Biome::Savanna => Some((BlockType::Leaves, 0.24)),
        Biome::Karst => Some((BlockType::JungleLeaves, 0.68)),
        _ => None,
    }
}

#[inline]
fn understory_colony_gate(moisture: f64, cohort: f64) -> f64 {
    let colony_strength =
        (0.35 + moisture.clamp(0.0, 1.0) * 0.40 + cohort.clamp(0.0, 1.0) * 0.25).clamp(0.0, 1.0);
    smoothstep(0.48, 0.82, colony_strength).powi(2)
}

fn cardinal_surface_slope(generator: &TerrainGenerator, wx: i32, wz: i32, surface: i32) -> i32 {
    [
        generator
            .surface_height(f64::from(wx.saturating_sub(1)), f64::from(wz))
            .0,
        generator
            .surface_height(f64::from(wx.saturating_add(1)), f64::from(wz))
            .0,
        generator
            .surface_height(f64::from(wx), f64::from(wz.saturating_sub(1)))
            .0,
        generator
            .surface_height(f64::from(wx), f64::from(wz.saturating_add(1)))
            .0,
    ]
    .into_iter()
    .map(|neighbour| {
        (i64::from(surface) - i64::from(neighbour))
            .abs()
            .min(i64::from(i32::MAX)) as i32
    })
    .max()
    .unwrap_or(0)
}

/// Convert the hydrographic tangent into the first cardinal crown direction.
/// The remaining branch tiers rotate around this index, so the tree still has
/// volume while its strongest silhouette follows the river corridor.
#[inline]
fn cardinal_direction_index(flow_direction: [f32; 2]) -> usize {
    let [x, z] = flow_direction;
    if !x.is_finite() || !z.is_finite() {
        return 0;
    }
    if x.abs() >= z.abs() {
        if x >= 0.0 {
            0
        } else {
            2
        }
    } else if z >= 0.0 {
        1
    } else {
        3
    }
}

/// Flowering trees form localized groves instead of turning the whole Lush
/// world pink. The input is a dimensionless, low-frequency habitat signal in
/// [0, 1]; the returned probability is per eligible tree.
fn flowering_canopy_chance(
    quality: crate::settings::SceneryQuality,
    biome: Biome,
    grove_signal: f64,
) -> f64 {
    use crate::settings::SceneryQuality;

    if !matches!(biome, Biome::Plains | Biome::Forest | Biome::Karst) {
        return 0.0;
    }
    let grove = smoothstep(0.56, 0.82, grove_signal.clamp(0.0, 1.0));
    match quality {
        SceneryQuality::Off | SceneryQuality::Lean => 0.0,
        SceneryQuality::Balanced => 0.025 + 0.26 * grove,
        SceneryQuality::Lush => 0.065 + 0.58 * grove,
    }
}

/// Per-candidate density for grouped Astral Frontier props. Values are kept
/// low because each accepted candidate paints a multi-voxel silhouette; the
/// visual unit is a cluster or landmark, never a field of glitter cells.
fn astral_prop_density(biome: Biome) -> f64 {
    match biome {
        Biome::CrystalSpires => 0.075,
        Biome::AlienReef => 0.065,
        Biome::VolcanicWaste => 0.040,
        Biome::GlacierShards => 0.030,
        Biome::Mesa | Biome::Karst => 0.025,
        _ => 0.0,
    }
}

pub struct TerrainGenerator {
    pub seed: u32,
    world_profile: WorldProfile,
    scenery_quality: crate::settings::SceneryQuality,
    terrain_grammar: TerrainGrammarVersion,
    continent: Perlin,
    erosion: Perlin,
    hills_a: Perlin,
    hills_b: Perlin,
    warp_x: Perlin,
    warp_z: Perlin,
    ridges: Perlin,
    caves_a: Perlin,
    caves_b: Perlin,
    /// Long, sinuous "worm" tunnel layer that carves big horizontal
    /// tubes straight through mountains. Separate from the narrow
    /// cave band so tunnels feel distinct from cramped caves.
    tunnel_a: Perlin,
    tunnel_b: Perlin,
    /// Low-frequency noise used to decide tunnel elevation + flow
    /// direction so tunnels stay roughly horizontal over long spans.
    tunnel_path: Perlin,
    /// Huge spherical cavern rooms scattered underground â€” the
    /// dramatic "oh wow" chambers you stumble into from a tunnel.
    cavern: Perlin,
    temperature: Perlin,
    moisture: Perlin,
    /// World-space hydrographic course fields. Their zero contours create
    /// kilometre-scale winding rivers; a separate warp keeps them organic
    /// while remaining byte-stable across chunk borders and generation order.
    river_course: Perlin,
    river_warp: Perlin,
    river_tributary: Perlin,
    /// Macro-region noise â€” extremely low frequency (~0.0002), defines
    /// vast geographic provinces hundreds of chunks across: highlands,
    /// canyon mesas, vast plateaus, lush wetlands. The single biggest
    /// "real-world variety" cue: walking 2 km in one direction takes
    /// you from European meadows to Grand-Canyon-style plateaus.
    region: Perlin,
    /// Secondary region channel, orthogonal to `region`, used to break
    /// up region boundaries so they don't all line up along one axis.
    region_b: Perlin,
}

const SURFACE_GRID_SIDE: usize = CHUNK_SIZE + 2;
/// Full chunk generation uses i32 voxel identities in many existing authoring
/// and decoration paths. Reserve a fixed halo at the numeric edge and fail
/// closed outside it instead of allowing debug overflow or release wrapping
/// to make the cached surface and written voxels describe different worlds.
/// The supported span still exceeds four million kilometres per axis.
const TERRAIN_GENERATION_COORDINATE_MARGIN: i64 = 4_096;

fn checked_terrain_chunk_origins(pos: ChunkPos) -> Option<(i32, i32, i32)> {
    let checked_axis = |axis: i32| {
        let origin = i64::from(axis) * i64::from(CHUNK_SIZE_I);
        let minimum = i64::from(i32::MIN) + TERRAIN_GENERATION_COORDINATE_MARGIN;
        let maximum = i64::from(i32::MAX)
            - TERRAIN_GENERATION_COORDINATE_MARGIN
            - i64::from(CHUNK_SIZE_I - 1);
        (minimum..=maximum)
            .contains(&origin)
            .then_some(origin as i32)
    };
    Some((
        checked_axis(pos.x)?,
        checked_axis(pos.y)?,
        checked_axis(pos.z)?,
    ))
}

/// Exact one-cell-border cache for the canonical terrain surface.
///
/// The fill pass needs each column plus its four cardinal neighbours. Keeping
/// that 18x18 stencil per chunk removes 956 repeated noise evaluations while
/// preserving byte-identical terrain for every seed.
struct TerrainSurfaceGrid {
    samples: Vec<(i32, f64)>,
}

impl TerrainSurfaceGrid {
    fn build(generator: &TerrainGenerator, cx: i32, cz: i32) -> Self {
        let origin_x = i64::from(cx) * i64::from(CHUNK_SIZE_I);
        let origin_z = i64::from(cz) * i64::from(CHUNK_SIZE_I);
        let mut samples = Vec::with_capacity(SURFACE_GRID_SIDE * SURFACE_GRID_SIDE);
        for grid_z in 0..SURFACE_GRID_SIDE {
            for grid_x in 0..SURFACE_GRID_SIDE {
                let wx = origin_x + grid_x as i64 - 1;
                let wz = origin_z + grid_z as i64 - 1;
                samples.push(generator.surface_height(wx as f64, wz as f64));
            }
        }
        debug_assert_eq!(
            samples.len(),
            crate::voxel_budget::CACHED_SURFACE_SAMPLES_PER_CHUNK
        );
        Self { samples }
    }

    #[inline]
    fn sample(&self, lx: usize, lz: usize) -> (i32, f64) {
        self.sample_offset(lx, lz, 0, 0)
    }

    #[inline]
    fn sample_offset(&self, lx: usize, lz: usize, offset_x: i32, offset_z: i32) -> (i32, f64) {
        let grid_x = lx as i32 + offset_x + 1;
        let grid_z = lz as i32 + offset_z + 1;
        debug_assert!((0..SURFACE_GRID_SIDE as i32).contains(&grid_x));
        debug_assert!((0..SURFACE_GRID_SIDE as i32).contains(&grid_z));
        self.samples[grid_z as usize * SURFACE_GRID_SIDE + grid_x as usize]
    }
}

impl TerrainGenerator {
    pub fn new(seed: u32) -> Self {
        // Derive per-layer seeds from the world seed so everything stays
        // deterministic but each layer has its own noise field.
        Self {
            seed,
            world_profile: WorldProfile::Natural,
            scenery_quality: crate::settings::SceneryQuality::Balanced,
            terrain_grammar: TerrainGrammarVersion::CURRENT,
            continent: Perlin::new(seed.wrapping_add(1)),
            erosion: Perlin::new(seed.wrapping_add(2)),
            hills_a: Perlin::new(seed.wrapping_add(3)),
            hills_b: Perlin::new(seed.wrapping_add(4)),
            warp_x: Perlin::new(seed.wrapping_add(5)),
            warp_z: Perlin::new(seed.wrapping_add(6)),
            ridges: Perlin::new(seed.wrapping_add(7)),
            caves_a: Perlin::new(seed.wrapping_add(8)),
            caves_b: Perlin::new(seed.wrapping_add(9)),
            tunnel_a: Perlin::new(seed.wrapping_add(21)),
            tunnel_b: Perlin::new(seed.wrapping_add(22)),
            tunnel_path: Perlin::new(seed.wrapping_add(23)),
            cavern: Perlin::new(seed.wrapping_add(24)),
            temperature: Perlin::new(seed.wrapping_add(10)),
            moisture: Perlin::new(seed.wrapping_add(11)),
            river_course: Perlin::new(seed.wrapping_add(14)),
            river_warp: Perlin::new(seed.wrapping_add(15)),
            river_tributary: Perlin::new(seed.wrapping_add(16)),
            region: Perlin::new(seed.wrapping_add(12)),
            region_b: Perlin::new(seed.wrapping_add(13)),
        }
    }

    /// Reconstruct a generator from the complete persisted identity. This is
    /// the preferred boundary for worlds, workers, caches, edit stores, and
    /// QA because it cannot accidentally reset one identity component.
    pub fn from_identity(identity: WorldGenerationIdentity) -> Self {
        Self::new(identity.seed)
            .with_world_profile(identity.world_profile)
            .with_scenery_quality(identity.scenery_quality)
            .with_terrain_grammar(identity.terrain_grammar)
    }

    pub fn with_scenery_quality(mut self, quality: crate::settings::SceneryQuality) -> Self {
        self.scenery_quality = quality;
        self
    }

    pub fn with_world_profile(mut self, world_profile: WorldProfile) -> Self {
        self.world_profile = world_profile;
        self
    }

    pub fn with_terrain_grammar(mut self, terrain_grammar: TerrainGrammarVersion) -> Self {
        self.terrain_grammar = terrain_grammar;
        self
    }

    pub const fn world_profile(&self) -> WorldProfile {
        self.world_profile
    }

    pub const fn grammar_version(&self) -> TerrainGrammarVersion {
        self.terrain_grammar
    }

    pub const fn terrain_grammar(&self) -> TerrainGrammarVersion {
        self.terrain_grammar
    }

    /// Exact immutable generator identity used by autonomous QA readiness.
    /// Surface probes remain a useful implementation checksum, but callers
    /// must not mistake four coincident samples for proof of seed or scenery.
    pub const fn scenery_quality(&self) -> crate::settings::SceneryQuality {
        self.scenery_quality
    }

    pub const fn generation_identity(&self) -> WorldGenerationIdentity {
        WorldGenerationIdentity {
            seed: self.seed,
            world_profile: self.world_profile,
            scenery_quality: self.scenery_quality,
            terrain_grammar: self.terrain_grammar,
        }
    }

    fn astral_layout(&self) -> Option<AstralFrontierLayout> {
        (self.world_profile == WorldProfile::AstralFrontier)
            .then(|| AstralFrontierLayout::for_seed(self.seed))
    }

    /// Navigation focus shared by spawn selection, autonomous visual QA and
    /// future map/mission systems. Returning the actual authored coordinate
    /// prevents those consumers from independently guessing where the hero
    /// composition ought to be.
    pub fn astral_frontier_hub(&self) -> Option<IVec2> {
        self.astral_layout().map(|layout| layout.hub)
    }

    pub fn astral_frontier_landing(&self) -> Option<IVec2> {
        self.astral_layout().map(AstralFrontierLayout::landing)
    }

    fn astral_local(&self, wx: i32, wz: i32) -> Option<IVec2> {
        self.astral_layout()
            .map(|layout| layout.local_from_world(IVec2::new(wx, wz)))
    }

    /// One sparse node per active macro-cell. The same node drives landform,
    /// route and waypoint placement, which makes infrastructure look grown
    /// from the world instead of pasted onto an unrelated flat field.
    fn astral_macro_node_for_cell(
        &self,
        owner_cell_x: i32,
        owner_cell_z: i32,
    ) -> Option<AstralMacroNode> {
        if self.world_profile != WorldProfile::AstralFrontier
            || column_rand(self.seed ^ 0xA57A_71A1, owner_cell_x, owner_cell_z) > 0.58
        {
            return None;
        }

        const MARGIN: i32 = 64;
        let cell_min_x = owner_cell_x.saturating_mul(ASTRAL_WAYPOINT_CELL_SIZE);
        let cell_min_z = owner_cell_z.saturating_mul(ASTRAL_WAYPOINT_CELL_SIZE);
        let jitter_span = ASTRAL_WAYPOINT_CELL_SIZE - MARGIN * 2;
        let center_x = cell_min_x.saturating_add(MARGIN).saturating_add(
            (column_rand(self.seed ^ 0xA57A_71A2, owner_cell_x, owner_cell_z)
                * f64::from(jitter_span)) as i32,
        );
        let center_z = cell_min_z.saturating_add(MARGIN).saturating_add(
            (column_rand(self.seed ^ 0xA57A_71A3, owner_cell_x, owner_cell_z)
                * f64::from(jitter_span)) as i32,
        );

        // The first-flight precinct owns its composition. Rejecting the
        // anchor itself is cheaper and more reliable than trying to mask each
        // downstream terrain, route and decoration pass independently.
        if let Some(layout) = self.astral_layout() {
            let dx = i128::from(center_x) - i128::from(layout.hub.x);
            let dz = i128::from(center_z) - i128::from(layout.hub.y);
            let exclusion = i128::from(ASTRAL_WAYPOINT_HUB_EXCLUSION);
            if dx * dx + dz * dz <= exclusion * exclusion {
                return None;
            }
        }

        let kind_roll = column_rand(self.seed ^ 0xA57A_71A4, owner_cell_x, owner_cell_z);
        let kind = match (kind_roll * 4.0) as u8 {
            0 => AstralWaypointKind::RelaySpire,
            1 => AstralWaypointKind::SkyDock,
            2 => AstralWaypointKind::CrystalGarden,
            _ => AstralWaypointKind::TransitGate,
        };
        let quarter_turns =
            (column_rand(self.seed ^ 0xA57A_71A5, owner_cell_x, owner_cell_z) * 4.0) as u8 & 3;
        Some(AstralMacroNode {
            center: IVec2::new(center_x, center_z),
            kind,
            quarter_turns,
        })
    }

    fn next_astral_macro_node(
        &self,
        owner_cell_x: i32,
        owner_cell_z: i32,
        step_x: i32,
        step_z: i32,
    ) -> Option<AstralMacroNode> {
        (1..=ASTRAL_ROUTE_SEARCH_CELLS).find_map(|step| {
            self.astral_macro_node_for_cell(
                owner_cell_x.saturating_add(step_x.saturating_mul(step)),
                owner_cell_z.saturating_add(step_z.saturating_mul(step)),
            )
        })
    }

    fn astral_route_accent(start: AstralMacroNode, end: AstralMacroNode) -> BlockType {
        if matches!(
            (start.kind, end.kind),
            (
                AstralWaypointKind::CrystalGarden,
                AstralWaypointKind::CrystalGarden
            )
        ) {
            BlockType::NeonMagenta
        } else if matches!(
            start.kind,
            AstralWaypointKind::SkyDock | AstralWaypointKind::TransitGate
        ) || matches!(
            end.kind,
            AstralWaypointKind::SkyDock | AstralWaypointKind::TransitGate
        ) {
            BlockType::NeonCyan
        } else {
            BlockType::NeonAmber
        }
    }

    /// Replay only the bounded macro-cell halo capable of intersecting the
    /// supplied world point. Each node owns its east/south edges, so route
    /// identity and traversal are deterministic without an allocation or a
    /// global graph build.
    fn for_each_astral_route_near_point(
        &self,
        wx: f64,
        wz: f64,
        mut visit: impl FnMut(AstralRouteSpec),
    ) {
        if self.world_profile != WorldProfile::AstralFrontier {
            return;
        }
        let owner_x = (wx.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_z = (wz.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        for cell_z in owner_z.saturating_sub(2)..=owner_z.saturating_add(1) {
            for cell_x in owner_x.saturating_sub(2)..=owner_x.saturating_add(1) {
                let Some(start) = self.astral_macro_node_for_cell(cell_x, cell_z) else {
                    continue;
                };
                for (step_x, step_z) in [(1, 0), (0, 1)] {
                    let Some(end) = self.next_astral_macro_node(cell_x, cell_z, step_x, step_z)
                    else {
                        continue;
                    };
                    visit(AstralRouteSpec {
                        start,
                        end,
                        accent: Self::astral_route_accent(start, end),
                    });
                }
            }
        }
    }

    /// Apply an infinite but locally replayable Astral world grammar. Four
    /// node archetypes create summit, mesa, amphitheatre and saddle terrain;
    /// graded causeways join nearby nodes. The pass is analytic, allocation
    /// free and profile-gated, so Natural terrain remains byte-identical.
    fn apply_astral_world_height(&self, wx: f64, wz: f64, base_height: f64) -> f64 {
        if self.world_profile != WorldProfile::AstralFrontier {
            return base_height;
        }

        let mut height = base_height;
        let mut strongest_route = 0.0_f64;
        let mut route_target = base_height;
        self.for_each_astral_route_near_point(wx, wz, |route| {
            let (distance, t) = Self::point_segment_distance(
                wx,
                wz,
                f64::from(route.start.center.x),
                f64::from(route.start.center.y),
                f64::from(route.end.center.x),
                f64::from(route.end.center.y),
            );
            if distance > 38.0 {
                return;
            }
            let shoulder = 1.0 - smoothstep(13.0, 38.0, distance);
            let spine = 1.0 - smoothstep(3.0, 9.0, distance);
            let strength = shoulder * 0.28 + spine * 0.56;
            if strength > strongest_route {
                strongest_route = strength;
                route_target = route.start.terrain_target()
                    + (route.end.terrain_target() - route.start.terrain_target()) * t;
            }
        });
        if strongest_route > 0.0 {
            height += (route_target - height) * strongest_route;
        }

        let owner_x = (wx.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_z = (wz.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        for cell_z in owner_z.saturating_sub(1)..=owner_z.saturating_add(1) {
            for cell_x in owner_x.saturating_sub(1)..=owner_x.saturating_add(1) {
                let Some(node) = self.astral_macro_node_for_cell(cell_x, cell_z) else {
                    continue;
                };
                let (x, z) = AstralFrontierLayout::rotate_quarters_f64(
                    wx.round() - f64::from(node.center.x),
                    wz.round() - f64::from(node.center.y),
                    (4 - node.quarter_turns) & 3,
                );
                let distance = match node.kind {
                    AstralWaypointKind::RelaySpire => {
                        ((x / 0.92).powi(2) + (z / 1.08).powi(2)).sqrt()
                    }
                    AstralWaypointKind::SkyDock => ((x / 1.36).powi(2) + (z / 0.82).powi(2)).sqrt(),
                    AstralWaypointKind::CrystalGarden => {
                        ((x / 1.12).powi(2) + (z / 0.94).powi(2)).sqrt()
                    }
                    AstralWaypointKind::TransitGate => {
                        ((x / 1.48).powi(2) + (z / 0.76).powi(2)).sqrt()
                    }
                };
                if distance > ASTRAL_WORLD_NODE_RADIUS {
                    continue;
                }

                let outer = 1.0 - smoothstep(62.0, ASTRAL_WORLD_NODE_RADIUS, distance);
                let inner = 1.0 - smoothstep(25.0, 38.0, distance);
                let target = node.terrain_target();
                match node.kind {
                    AstralWaypointKind::RelaySpire => {
                        let summit = target - (distance / 18.0).floor().min(3.0) * 5.0;
                        height += (height.max(summit) - height) * outer * 0.84;
                    }
                    AstralWaypointKind::SkyDock => {
                        height += (target - height) * outer * 0.78;
                    }
                    AstralWaypointKind::CrystalGarden => {
                        let ring = 1.0 - smoothstep(9.0, 24.0, (distance - 49.0).abs());
                        height += ((target + 13.0) - height) * ring * outer * 0.72;
                        height += (target - height) * outer * 0.38;
                    }
                    AstralWaypointKind::TransitGate => {
                        let saddle = target + (z.abs() / 32.0).min(1.0) * 7.0;
                        height += (saddle - height) * outer * 0.72;
                    }
                }

                // Partial quantisation creates usable terraces without the
                // perfectly concentric contour rings rejected in visual QA.
                let terraced = (height / 5.0).round() * 5.0;
                height += (terraced - height) * outer * (1.0 - inner) * 0.48;
                // A destination's inner 25-block footprint is an exact
                // gameplay contract: every archetype gets a level, landable
                // core while its surroundings retain regional relief.
                height += (target - height) * inner;
            }
        }
        height
    }

    /// Conservative integer AABB/disc overlap used to reject distant chunks
    /// before any surface noise or authored-structure work is evaluated.
    /// i64 arithmetic keeps the public infinite-coordinate contract fail-safe.
    fn chunk_intersects_disc(cx: i32, cz: i32, center: IVec2, radius: i32) -> bool {
        let min_x = i64::from(cx) * i64::from(CHUNK_SIZE_I);
        let min_z = i64::from(cz) * i64::from(CHUNK_SIZE_I);
        let max_x = min_x + i64::from(CHUNK_SIZE_I - 1);
        let max_z = min_z + i64::from(CHUNK_SIZE_I - 1);
        let center_x = i64::from(center.x);
        let center_z = i64::from(center.y);
        let closest_x = center_x.clamp(min_x, max_x);
        let closest_z = center_z.clamp(min_z, max_z);
        let dx = i128::from(closest_x) - i128::from(center_x);
        let dz = i128::from(closest_z) - i128::from(center_z);
        let radius = i128::from(radius.max(0));
        dx * dx + dz * dz <= radius * radius
    }

    fn astral_canyon_distance(local_x: f64, local_z: f64) -> f64 {
        let canyon_x = -58.0 + ((local_z + 34.0) * 0.026).sin() * 13.0;
        (local_x - canyon_x).abs()
    }

    /// Compose one readable first-flight sector on top of the infinite noise
    /// field. The operation is analytic and world-space only, so neighbouring
    /// chunks, async workers and save reloads produce the same cliffs.
    fn apply_astral_frontier_height(&self, wx: f64, wz: f64, base_height: f64) -> f64 {
        let Some(layout) = self.astral_layout() else {
            return base_height;
        };
        let (x, z) = AstralFrontierLayout::rotate_quarters_f64(
            wx.round() - f64::from(layout.hub.x),
            wz.round() - f64::from(layout.hub.y),
            (4 - layout.quarter_turns) & 3,
        );
        let radius = (x * x + z * z).sqrt();
        if radius > ASTRAL_PRECINCT_RADIUS {
            return base_height;
        }

        let envelope = 1.0 - smoothstep(345.0, ASTRAL_PRECINCT_RADIUS, radius);
        let broad_detail = self
            .hills_a
            .get([(wx + 217.0) * 0.0031, (wz - 149.0) * 0.0031]);
        let living_floor = 66.0 + broad_detail * 5.5;
        let mut height = base_height + (living_floor - base_height).max(0.0) * envelope * 0.94;

        // A green garden shelf and a warmer observatory shelf give the hub
        // three distinct approach silhouettes rather than one circular hill.
        let garden_distance = (((x - 102.0) / 1.18).powi(2) + (z + 88.0).powi(2)).sqrt();
        let garden = 1.0 - smoothstep(43.0, 67.0, garden_distance);
        height += (86.0 + broad_detail * 1.5 - height) * garden;

        let observatory_distance =
            (((x - 142.0) / 1.08).powi(2) + ((z - 112.0) / 0.92).powi(2)).sqrt();
        let observatory = 1.0 - smoothstep(34.0, 58.0, observatory_distance);
        height += (94.0 + broad_detail * 2.0 - height) * observatory;

        // Central stepped mountain: broad enough to read at flight distance,
        // terraced enough to support paths/buildings, and capped below the
        // engine's vertical budget so the citadel can still rise above it.
        let mountain_warp_x = self
            .warp_x
            .get([(wx + 311.0) * 0.0062, (wz - 197.0) * 0.0062])
            * 14.0;
        let mountain_warp_z = self
            .warp_z
            .get([(wx - 163.0) * 0.0058, (wz + 283.0) * 0.0058])
            * 12.0;
        let angle = z.atan2(x);
        let seed_phase = f64::from(self.seed % 997) * 0.013;
        let silhouette_lobes =
            (angle * 3.0 + seed_phase).sin() * 6.0 + (angle * 5.0 - seed_phase * 0.7).sin() * 3.0;
        let mountain_distance = ((((x + mountain_warp_x) / 1.12).powi(2)
            + ((z + mountain_warp_z) / 0.94).powi(2))
        .sqrt()
            + silhouette_lobes)
            .max(0.0);
        let mountain = 1.0 - smoothstep(45.0, 148.0, mountain_distance);
        if mountain > 0.0 {
            let strata_detail = self
                .hills_b
                .get([(wx + 71.0) * 0.011, (wz - 119.0) * 0.011]);
            let raw_target = 84.0 + mountain.powf(1.22) * 58.0 + strata_detail * mountain * 3.5;
            let terraced_target = (raw_target / 6.0).round() * 6.0;
            let mountain_envelope = 1.0 - smoothstep(122.0, 158.0, mountain_distance);
            // Partial quantisation keeps traversable shelves but avoids perfect
            // concentric contour rings that read as a generated wedding cake.
            let sculpted_target = raw_target * 0.38 + terraced_target * 0.62;
            let target = height.max(sculpted_target);
            height += (target - height) * mountain_envelope;
        }

        // The canyon is intentional negative space, so it is resolved after
        // all positive relief. Its VolcanicWaste biome lets the existing fluid
        // fill produce a bounded lava course instead of the former white sea.
        let canyon_distance = Self::astral_canyon_distance(x, z);
        let canyon = (1.0 - smoothstep(10.0, 27.0, canyon_distance))
            * (1.0 - smoothstep(300.0, 420.0, radius));
        height += (WATER_LEVEL as f64 - 5.0 - height) * canyon * 0.96;

        // Landability is a gameplay contract, not merely a silhouette. Resolve
        // the western shuttle mesa last so neither the mountain nor canyon can
        // reintroduce slopes underneath the authored pad.
        let landing_distance = (((x + 124.0) / 1.10).powi(2) + (z - 24.0).powi(2)).sqrt();
        let landing = 1.0 - smoothstep(31.0, 47.0, landing_distance);
        height += (79.0 - height) * landing;

        height
    }

    fn astral_frontier_biome_override(&self, wx: f64, wz: f64) -> Option<Biome> {
        let layout = self.astral_layout()?;
        let (x, z) = AstralFrontierLayout::rotate_quarters_f64(
            wx.round() - f64::from(layout.hub.x),
            wz.round() - f64::from(layout.hub.y),
            (4 - layout.quarter_turns) & 3,
        );
        let radius = (x * x + z * z).sqrt();
        if radius > ASTRAL_PRECINCT_RADIUS {
            return None;
        }
        if Self::astral_canyon_distance(x, z) < 29.0 && radius < 420.0 {
            return Some(Biome::VolcanicWaste);
        }
        if ((x + 124.0).powi(2) + (z - 24.0).powi(2)).sqrt() < 58.0 {
            return Some(Biome::AlienReef);
        }
        if ((x - 102.0).powi(2) + (z + 88.0).powi(2)).sqrt() < 82.0 {
            return Some(Biome::Karst);
        }
        if ((x - 142.0).powi(2) + (z - 112.0).powi(2)).sqrt() < 72.0 {
            return Some(Biome::Mesa);
        }
        let central_distance = ((x / 1.12).powi(2) + (z / 0.94).powi(2)).sqrt();
        if central_distance < 72.0 {
            // The citadel requires a readable mid-value geological mass.
            // CrystalSpires remain global Astral provinces and sparse accents,
            // but using translucent dark crystal for an entire mountain turned
            // the first-flight silhouette into one black wall in real QA.
            return Some(Biome::Mountains);
        }
        if central_distance < 164.0 {
            // Moss on flat terraces plus pale limestone on exposed faces gives
            // the mountain a legible material hierarchy at flight distance.
            return Some(Biome::Karst);
        }

        // Large calm colour fields preserve visual rest between the hero
        // accents. They intentionally use existing ecological palettes so
        // all vegetation, materials and tools keep their established rules.
        if z < -34.0 {
            Some(Biome::Plains)
        } else if x < -42.0 {
            Some(Biome::AlienReef)
        } else if x > 112.0 {
            Some(Biome::Mesa)
        } else {
            Some(Biome::Karst)
        }
    }

    /// Give every global node a broad material province matching its
    /// landform purpose. The radius is smaller than the height envelope, so
    /// the existing ecotone pass gets a real transition shoulder instead of
    /// one hard material circle under the architecture.
    fn astral_world_biome_override(&self, wx: f64, wz: f64) -> Option<Biome> {
        if self.world_profile != WorldProfile::AstralFrontier {
            return None;
        }
        let owner_x = (wx.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_z = (wz.floor() as i32).div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let mut nearest: Option<(f64, AstralWaypointKind)> = None;
        for cell_z in owner_z.saturating_sub(1)..=owner_z.saturating_add(1) {
            for cell_x in owner_x.saturating_sub(1)..=owner_x.saturating_add(1) {
                let Some(node) = self.astral_macro_node_for_cell(cell_x, cell_z) else {
                    continue;
                };
                let (x, z) = AstralFrontierLayout::rotate_quarters_f64(
                    wx.round() - f64::from(node.center.x),
                    wz.round() - f64::from(node.center.y),
                    (4 - node.quarter_turns) & 3,
                );
                let distance = match node.kind {
                    AstralWaypointKind::RelaySpire => {
                        ((x / 0.92).powi(2) + (z / 1.08).powi(2)).sqrt()
                    }
                    AstralWaypointKind::SkyDock => ((x / 1.36).powi(2) + (z / 0.82).powi(2)).sqrt(),
                    AstralWaypointKind::CrystalGarden => {
                        ((x / 1.12).powi(2) + (z / 0.94).powi(2)).sqrt()
                    }
                    AstralWaypointKind::TransitGate => {
                        ((x / 1.48).powi(2) + (z / 0.76).powi(2)).sqrt()
                    }
                };
                if distance <= 78.0
                    && nearest.is_none_or(|(best_distance, _)| distance < best_distance)
                {
                    nearest = Some((distance, node.kind));
                }
            }
        }
        nearest.map(|(_, kind)| match kind {
            AstralWaypointKind::RelaySpire | AstralWaypointKind::TransitGate => Biome::Karst,
            AstralWaypointKind::SkyDock => Biome::Mesa,
            AstralWaypointKind::CrystalGarden => Biome::AlienReef,
        })
    }

    /// Signed major-river course. Rivers follow the zero contour rather than
    /// a thresholded height blob, which produces long connected bends instead
    /// of isolated circular ponds. Every input is absolute world space, so the
    /// result cannot know or reveal a chunk seam.
    #[inline]
    fn major_river_axis(&self, wx: f64, wz: f64) -> f64 {
        let warp_x = self
            .river_warp
            .get([wx * 0.00037 + 17.0, wz * 0.00037 - 29.0])
            * 180.0;
        let warp_z = self
            .river_tributary
            .get([wx * 0.00041 - 43.0, wz * 0.00041 + 11.0])
            * 150.0;
        self.river_course.get([
            (wx + warp_x) * 0.00115 + 31.0,
            (wz + warp_z) * 0.00115 - 23.0,
        ])
    }

    /// Seam-stable river and floodplain strength for a pre-carve surface.
    /// The second contour only activates in broad wet catchments, so it reads
    /// as tributaries feeding a dominant course rather than a uniform maze.
    fn hydrographic_field_for_surface(
        &self,
        wx: f64,
        wz: f64,
        pre_carve_surface: f64,
    ) -> HydrographicField {
        let major_distance = self.major_river_axis(wx, wz).abs();
        let major_corridor = 1.0 - smoothstep(0.025, 0.095, major_distance);
        let major_channel = 1.0 - smoothstep(0.006, 0.030, major_distance);

        let catchment = (self
            .river_warp
            .get([wx * 0.00063 - 71.0, wz * 0.00063 + 47.0])
            * 0.5
            + 0.5)
            .clamp(0.0, 1.0);
        let tributary_axis = self
            .river_tributary
            .get([wx * 0.00205 + 53.0, wz * 0.00205 - 67.0]);
        let tributary_gate = smoothstep(0.61, 0.82, catchment);
        let tributary_corridor =
            (1.0 - smoothstep(0.014, 0.055, tributary_axis.abs())) * tributary_gate * 0.82;
        let tributary_channel =
            (1.0 - smoothstep(0.006, 0.022, tributary_axis.abs())) * tributary_gate * 0.72;

        // Static water currently occupies the global sea level. Restrict the
        // first visible river network to lowlands so every carved channel has
        // a coherent water surface instead of climbing mountains. This is a
        // visual hydrographic foundation, not a claim of a full SWE solver.
        let lowland = 1.0
            - smoothstep(
                WATER_LEVEL as f64 + 10.0,
                WATER_LEVEL as f64 + 39.0,
                pre_carve_surface,
            );
        HydrographicField {
            corridor: major_corridor.max(tributary_corridor) * lowland,
            channel: major_channel.max(tributary_channel) * lowland,
        }
    }

    fn environment_sample_for_surface(&self, wx: i32, wz: i32, surface: i32) -> EnvironmentSample {
        let x = wx as f64;
        let z = wz as f64;
        let atmospheric_moisture =
            (self.moisture.get([x * 0.0015, z * 0.0015]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let baseline_temperature =
            (self.temperature.get([x * 0.0015, z * 0.0015]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let altitude_cooling = ((surface - (WATER_LEVEL + 8)).max(0) as f64 / 180.0) * 0.30;
        let temperature_norm = (baseline_temperature - altitude_cooling).clamp(0.0, 1.0);
        let hydro = self.hydrographic_field_for_surface(x, z, surface as f64);
        let soil_moisture = (atmospheric_moisture * 0.68 + hydro.corridor * 0.56).clamp(0.0, 1.0);

        let mineral_broad =
            (self.ridges.get([x * 0.0023 - 13.0, z * 0.0023 + 37.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let mineral_detail =
            (self.erosion.get([x * 0.0061 + 19.0, z * 0.0061 - 41.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let mineral_resonance = (mineral_broad * 0.72 + mineral_detail * 0.28).clamp(0.0, 1.0);

        // Broad ecological suitability creates flowering corridors and groves
        // instead of independent pink dice rolls. It is intentionally a
        // dimensionless art/ecology signal, not a biological growth model.
        let temperature_fit = (1.0 - ((temperature_norm - 0.60).abs() / 0.46)).clamp(0.0, 1.0);
        let moisture_fit = (1.0 - ((soil_moisture - 0.67).abs() / 0.48)).clamp(0.0, 1.0);
        let grove =
            (self.region_b.get([x * 0.0018 + 43.0, z * 0.0018 - 29.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let flowering_resonance =
            (temperature_fit * moisture_fit * (0.58 + grove * 0.42)).clamp(0.0, 1.0);

        EnvironmentSample {
            temperature_norm: temperature_norm as f32,
            atmospheric_moisture: atmospheric_moisture as f32,
            soil_moisture: soil_moisture as f32,
            river_strength: hydro.channel.clamp(0.0, 1.0) as f32,
            mineral_resonance: mineral_resonance as f32,
            flowering_resonance: flowering_resonance as f32,
            flow_direction: [0.0, 0.0],
        }
    }

    /// Public, cheap environmental telemetry for agents, inspectors, and
    /// future simulation systems. It does not mutate terrain or flight state.
    pub fn environment_sample_at(&self, wx: i32, wz: i32) -> EnvironmentSample {
        let surface = self.surface_height_at(wx, wz);
        let mut sample = self.environment_sample_for_surface(wx, wz, surface);
        let step = 2.0;
        let dx = self.major_river_axis(wx as f64 + step, wz as f64)
            - self.major_river_axis(wx as f64 - step, wz as f64);
        let dz = self.major_river_axis(wx as f64, wz as f64 + step)
            - self.major_river_axis(wx as f64, wz as f64 - step);
        let tangent_x = -dz;
        let tangent_z = dx;
        let length = (tangent_x * tangent_x + tangent_z * tangent_z).sqrt();
        if length > 1.0e-9 {
            sample.flow_direction = [(tangent_x / length) as f32, (tangent_z / length) as f32];
        }
        sample
    }

    pub fn tree_density_for_biome(&self, biome: Biome) -> f64 {
        let base = match biome {
            Biome::Forest => 0.085,
            Biome::Jungle => 0.135,
            Biome::Plains => 0.018,
            Biome::Savanna => 0.018,
            Biome::Tundra => 0.008,
            // Karst needs actual groves in sheltered basins, not isolated
            // specimen trees on an otherwise geological test map. The
            // low-frequency habitat multiplier below still creates large
            // light wells, so this extra density concentrates into cohorts.
            Biome::Karst => 0.078,
            _ => 0.0,
        };
        base * self.scenery_quality.density_scale()
    }

    /// Low-frequency stand ecology. A forest now contains dense cohorts,
    /// feathered edges, and real openings instead of converging toward one
    /// equally likely tree in every chunk. This only modulates the existing
    /// bounded candidate budget; it cannot create an unbounded decoration
    /// pass.
    fn tree_habitat_multiplier(&self, biome: Biome, wx: i32, wz: i32, surface: i32) -> f64 {
        let x = wx as f64;
        let z = wz as f64;
        let broad = (self.fbm2(
            &self.region_b,
            x * 0.0021 + 31.0,
            z * 0.0021 - 19.0,
            3,
            2.0,
            0.52,
        ) * 0.5
            + 0.5)
            .clamp(0.0, 1.0);
        let edge =
            (self.hills_b.get([x * 0.0073 - 11.0, z * 0.0073 + 23.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let moisture = self
            .environment_sample_for_surface(wx, wz, surface)
            .soil_moisture as f64;
        let habitat = broad * 0.50 + edge * 0.20 + moisture * 0.30;

        // Square the cohort response for temperate/karst habitats. The old
        // nearly-linear floor made even a poor forest site receive one or two
        // trees per chunk, which was statistically varied but visually read as
        // a plantation. A near-zero floor plus a steeper shoulder creates
        // genuine light wells between dense stands without increasing the
        // bounded per-chunk candidate budget.
        match biome {
            Biome::Forest => 0.015 + 1.435 * smoothstep(0.42, 0.76, habitat).powi(2),
            Biome::Jungle => 0.34 + 1.08 * smoothstep(0.28, 0.72, habitat),
            Biome::Karst => 0.025 + 1.675 * smoothstep(0.39, 0.73, habitat).powf(1.7),
            Biome::Plains => 0.008 + 1.24 * smoothstep(0.69, 0.90, habitat).powi(2),
            Biome::Savanna => 0.035 + 1.16 * smoothstep(0.60, 0.88, habitat).powi(2),
            Biome::Tundra => 0.08 + 0.70 * smoothstep(0.50, 0.84, habitat),
            _ => 0.0,
        }
        .clamp(0.0, 1.45)
    }

    fn tree_leaf_for_site(&self, biome: Biome, wx: i32, wz: i32, surface: i32) -> BlockType {
        if biome == Biome::Jungle {
            return BlockType::JungleLeaves;
        }
        if !matches!(biome, Biome::Plains | Biome::Forest | Biome::Karst) {
            return BlockType::Leaves;
        }

        let x = wx as f64;
        let z = wz as f64;
        let broad =
            (self.region_b.get([x * 0.0018 + 43.0, z * 0.0018 - 29.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let detail =
            (self.hills_a.get([x * 0.0047 - 17.0, z * 0.0047 + 37.0]) * 0.5 + 0.5).clamp(0.0, 1.0);
        let flowering = self
            .environment_sample_for_surface(wx, wz, surface)
            .flowering_resonance as f64;
        let grove_signal = broad * 0.44 + detail * 0.18 + flowering * 0.38;
        let chance = flowering_canopy_chance(self.scenery_quality, biome, grove_signal);
        let individual = column_rand(self.seed ^ 0xF10A_EA57, wx, wz);
        if individual < chance {
            BlockType::BlossomLeaves
        } else {
            BlockType::Leaves
        }
    }

    pub fn tree_height_for_biome(&self, biome: Biome, r: f64) -> (i32, BlockType) {
        use crate::settings::SceneryQuality;

        let variance = (r * 997.0) as i32;
        let (lean, balanced, lush, variance_span) = match biome {
            Biome::Jungle => (8, 10, 14, 4),
            Biome::Forest => (6, 9, 13, 4),
            Biome::Karst => (7, 9, 13, 4),
            Biome::Plains => (5, 7, 12, 3),
            Biome::Savanna => (5, 6, 8, 2),
            Biome::Tundra => (4, 5, 6, 2),
            _ => (5, 6, 8, 2),
        };
        let base = match self.scenery_quality {
            SceneryQuality::Off | SceneryQuality::Lean => lean,
            SceneryQuality::Balanced => balanced,
            SceneryQuality::Lush => lush,
        };
        // A minority of young trees break the uniform mature-height ceiling.
        // The multiplied fractional roll is deterministic but decorrelated
        // from the coarse height variance and silhouette bands.
        let age_roll = (r * 104_729.0).fract().abs();
        let juvenile_reduction = match self.scenery_quality {
            SceneryQuality::Balanced if age_roll < 0.10 => 2,
            SceneryQuality::Lush if age_roll < 0.16 => 4,
            _ => 0,
        };
        let leaves = if biome == Biome::Jungle {
            BlockType::JungleLeaves
        } else {
            BlockType::Leaves
        };

        (
            (base + variance.rem_euclid(variance_span) - juvenile_reduction).max(4),
            leaves,
        )
    }

    fn tree_profile(&self, biome: Biome, style_roll: f64) -> Option<(TreeProfile, BlockType)> {
        use crate::settings::SceneryQuality;

        if self.scenery_quality == SceneryQuality::Off || self.tree_density_for_biome(biome) == 0.0
        {
            return None;
        }
        // Do not let species and age share the same visible random band. With
        // one roll, every conifer also landed in the same height family. This
        // deterministic nonlinear remap keeps replay exact while decorrelating
        // silhouette, age and height.
        let height_roll = (style_roll * 83.0 + style_roll * style_roll * 37.0 + 0.173).fract();
        let (trunk_height, leaves) = self.tree_height_for_biome(biome, height_roll);
        let silhouette = match biome {
            // Cold climates and part of the temperate forest now receive a
            // genuinely tapered conifer architecture. This is a fourth tree
            // topology, not a recolour of the existing pad-based crowns.
            Biome::Tundra => TreeSilhouette::Conifer,
            Biome::Forest => {
                if style_roll < 0.18 {
                    TreeSilhouette::Conifer
                } else if style_roll < 0.40 {
                    TreeSilhouette::Layered
                } else if style_roll < 0.66 {
                    TreeSilhouette::Windswept
                } else {
                    TreeSilhouette::Crowned
                }
            }
            Biome::Savanna => {
                if style_roll < 0.78 {
                    TreeSilhouette::Crowned
                } else {
                    TreeSilhouette::Windswept
                }
            }
            Biome::Plains => {
                if style_roll < 0.58 {
                    TreeSilhouette::Crowned
                } else if style_roll < 0.78 {
                    TreeSilhouette::Windswept
                } else {
                    TreeSilhouette::Layered
                }
            }
            Biome::Jungle => {
                if style_roll < 0.24 {
                    TreeSilhouette::Layered
                } else if style_roll < 0.46 {
                    TreeSilhouette::Windswept
                } else {
                    TreeSilhouette::Crowned
                }
            }
            _ => {
                if style_roll < 0.30 {
                    TreeSilhouette::Layered
                } else if style_roll < 0.54 {
                    TreeSilhouette::Windswept
                } else {
                    TreeSilhouette::Crowned
                }
            }
        };
        let profile = match self.scenery_quality {
            SceneryQuality::Off => return None,
            SceneryQuality::Lean => TreeProfile {
                trunk_height,
                canopy_radius: 2,
                branch_reach: 1,
                tiers: 2,
                max_extent: 3,
                crown_lift: 2,
                // Lean deliberately preserves the previous tree shape and
                // cost. Shape variety is a Balanced/Lush visual feature.
                silhouette: TreeSilhouette::Layered,
            },
            SceneryQuality::Balanced => match silhouette {
                TreeSilhouette::Conifer => TreeProfile {
                    trunk_height,
                    canopy_radius: 3,
                    branch_reach: 3,
                    tiers: 5,
                    max_extent: 5,
                    crown_lift: 3,
                    silhouette,
                },
                TreeSilhouette::Layered => TreeProfile {
                    trunk_height,
                    canopy_radius: 2,
                    branch_reach: 2,
                    tiers: 4,
                    max_extent: 5,
                    crown_lift: 3,
                    silhouette,
                },
                TreeSilhouette::Windswept => TreeProfile {
                    trunk_height,
                    canopy_radius: 2,
                    branch_reach: 3,
                    tiers: 4,
                    max_extent: 5,
                    crown_lift: 3,
                    silhouette,
                },
                TreeSilhouette::Crowned => TreeProfile {
                    trunk_height,
                    canopy_radius: 3,
                    branch_reach: 3,
                    // Three branch lobes plus one dominant crown make a
                    // readable broadleaf volume instead of four flat plates.
                    tiers: 3,
                    max_extent: 5,
                    crown_lift: 4,
                    silhouette,
                },
                // Site adaptation happens after this baseline profile is
                // built; the ordinary biome roll never emits Riparian.
                TreeSilhouette::Riparian => unreachable!("riparian is site-derived"),
            },
            SceneryQuality::Lush => match silhouette {
                TreeSilhouette::Conifer => TreeProfile {
                    trunk_height,
                    canopy_radius: 4,
                    branch_reach: 4,
                    tiers: 7,
                    max_extent: 7,
                    crown_lift: 4,
                    silhouette,
                },
                TreeSilhouette::Layered => TreeProfile {
                    trunk_height,
                    canopy_radius: 3,
                    branch_reach: 3,
                    tiers: 6,
                    max_extent: 7,
                    crown_lift: 4,
                    silhouette,
                },
                TreeSilhouette::Windswept => TreeProfile {
                    trunk_height,
                    canopy_radius: 3,
                    branch_reach: 4,
                    tiers: 5,
                    max_extent: 7,
                    crown_lift: 4,
                    silhouette,
                },
                TreeSilhouette::Crowned => TreeProfile {
                    trunk_height,
                    canopy_radius: 4,
                    branch_reach: 3,
                    tiers: 4,
                    max_extent: 7,
                    crown_lift: 5,
                    silhouette,
                },
                TreeSilhouette::Riparian => unreachable!("riparian is site-derived"),
            },
        };
        // The darker, denser existing jungle-leaf material gives conifers a
        // distinct blue-green needle mass and already owns the slower wind
        // response appropriate for a heavier crown.
        let leaves = if silhouette == TreeSilhouette::Conifer {
            BlockType::JungleLeaves
        } else {
            leaves
        };
        Some((profile, leaves))
    }

    /// Promote an ordinary biome tree into a river-gallery species only when
    /// the actual hydrographic, moisture, elevation, and quality signals all
    /// agree. This makes a river legible from flight through vegetation form,
    /// while dry hills keep their native conifers and broadleaf silhouettes.
    fn adapt_tree_profile_to_site(
        &self,
        baseline: TreeProfile,
        biome: Biome,
        wx: i32,
        wz: i32,
        surface: i32,
        style_roll: f64,
    ) -> TreeProfile {
        use crate::settings::SceneryQuality;

        if !matches!(
            self.scenery_quality,
            SceneryQuality::Balanced | SceneryQuality::Lush
        ) || !matches!(
            biome,
            Biome::Plains | Biome::Forest | Biome::Jungle | Biome::Tundra | Biome::Karst
        ) || surface <= WATER_LEVEL + 2
            || surface > WATER_LEVEL + 34
            || style_roll < 0.22
        {
            return baseline;
        }

        let hydro = self.hydrographic_field_for_surface(wx as f64, wz as f64, surface as f64);
        let environment = self.environment_sample_for_surface(wx, wz, surface);
        if hydro.corridor <= 0.16 || environment.soil_moisture < 0.68 {
            return baseline;
        }

        match self.scenery_quality {
            SceneryQuality::Balanced => TreeProfile {
                trunk_height: baseline.trunk_height.max(7),
                canopy_radius: 3,
                branch_reach: 4,
                tiers: 4,
                max_extent: 6,
                crown_lift: 3,
                silhouette: TreeSilhouette::Riparian,
            },
            SceneryQuality::Lush => TreeProfile {
                trunk_height: baseline.trunk_height.max(9),
                canopy_radius: 4,
                branch_reach: 5,
                tiers: 5,
                max_extent: 7,
                crown_lift: 4,
                silhouette: TreeSilhouette::Riparian,
            },
            SceneryQuality::Off | SceneryQuality::Lean => baseline,
        }
    }

    /// Fractional Brownian Motion (stacked octaves of Perlin noise, in [-1,1]).
    fn fbm2(&self, n: &Perlin, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            sum += amp * n.get([x * freq, z * freq]);
            norm += amp;
            amp *= gain;
            freq *= lacunarity;
        }
        sum / norm.max(1e-6)
    }

    fn fbm3(&self, n: &Perlin, x: f64, y: f64, z: f64, octaves: u32) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            sum += amp * n.get([x * freq, y * freq, z * freq]);
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6)
    }

    /// Ridged FBM: `1 - |noise|` per octave, stacked. Gives mountain ridges.
    fn ridged_fbm(&self, n: &Perlin, x: f64, z: f64, octaves: u32) -> f64 {
        let mut sum = 0.0;
        let mut amp = 1.0;
        let mut freq = 1.0;
        let mut norm = 0.0;
        for _ in 0..octaves {
            let v = 1.0 - n.get([x * freq, z * freq]).abs();
            sum += amp * v * v;
            norm += amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        sum / norm.max(1e-6)
    }

    /// Macro-region classification. Each world point is dominated by
    /// ONE province (canyon / plateau / highland / wetland / karst /
    /// normal). Province boundaries are smoothed but the dominant
    /// region's modifier is heavily weighted so cliffs from canyon
    /// regions don't leak into adjacent plains. Returns:
    /// (kind, strength in 0..1).
    fn region(&self, wx: f64, wz: f64) -> (Region, f64) {
        // Three orthogonal very-low-frequency channels in [-1, 1].
        let a = self.region.get([wx * 0.00018, wz * 0.00018]);
        let b = self.region_b.get([wx * 0.00021 + 17.3, wz * 0.00021 - 9.7]);
        // Third axis: separates karst out from the other 4 quadrants.
        let c = self.region.get([wx * 0.00013 - 41.7, wz * 0.00013 + 23.1]);

        // Region centers in (a, b, c) space. Natural retains the exact
        // established center set, so adding Astral Frontier cannot move a
        // single column in an old world. Astral composes broad calm landforms
        // with sparse hero provinces: it is a navigable world, not uniform
        // emissive noise.
        const NATURAL_CENTERS: &[(f64, f64, f64, Region)] = &[
            (-0.55, -0.55, -0.4, Region::Canyon),
            (0.55, -0.55, -0.4, Region::Plateau),
            (-0.55, 0.55, -0.4, Region::Highland),
            (0.55, 0.55, -0.4, Region::Wetland),
            (0.0, 0.0, 0.4, Region::Karst),
        ];
        const ASTRAL_CENTERS: &[(f64, f64, f64, Region)] = &[
            (-0.64, -0.56, -0.42, Region::Canyon),
            (0.62, -0.56, -0.42, Region::Plateau),
            (-0.62, 0.56, -0.38, Region::Highland),
            (0.62, 0.56, -0.38, Region::AlienReef),
            (-0.52, -0.18, 0.50, Region::VolcanicWaste),
            (0.52, -0.18, 0.50, Region::CrystalSpires),
            (0.00, 0.58, 0.50, Region::Karst),
        ];
        let centers = match self.world_profile {
            WorldProfile::Natural => NATURAL_CENTERS,
            WorldProfile::AstralFrontier => ASTRAL_CENTERS,
        };

        // Find dominant region by closest center; strength is how much
        // it dominates the runner-up (so deep interiors get full effect
        // and only the thin boundary band feathers).
        let mut best = (Region::Plains, f64::INFINITY);
        let mut second = f64::INFINITY;
        for (cx, cy, cz, r) in centers {
            let dx = a - cx;
            let dy = b - cy;
            let dz = c - cz;
            let d = dx * dx + dy * dy + dz * dz;
            if d < best.1 {
                second = best.1;
                best = (*r, d);
            } else if d < second {
                second = d;
            }
        }
        // Strength: 0 right at the boundary, ~1 deep inside the region.
        // Squared-distance ratio gives a soft falloff.
        let margin = (second - best.1).max(0.0);
        let strength = (margin * 4.0).min(1.0);
        // Below a threshold, treat as "normal" mixed terrain so we don't
        // see weak canyon striations everywhere.
        if strength < 0.15 {
            (Region::Plains, 0.0)
        } else {
            (best.0, strength)
        }
    }

    /// Broad unfiltered height field. Keep high-amplitude octaves wide enough
    /// that a feature cannot collapse to a single surface column.
    fn raw_surface_height(&self, wx: f64, wz: f64) -> (i32, f64) {
        // 1. Continentalness â€” very low frequency, defines ocean vs land.
        //    Halved frequency so continents stretch ~2Ã— wider â€” bigger
        //    plains, longer coastlines, gentler oceanâ†’land transitions.
        let cont = self.fbm2(&self.continent, wx * 0.00024, wz * 0.00024, 4, 2.0, 0.5);

        // 2. Erosion â€” smooths out where it's high, carves where it's low.
        let erod = self.fbm2(&self.erosion, wx * 0.00055, wz * 0.00055, 3, 2.0, 0.5);

        // 3. Domain-warped hills â€” the "lumpy" medium-scale terrain.
        //    Lower frequency + bigger warp = wider, more flowing hills
        //    instead of bumpy fields.
        let warp_scale = 96.0;
        let dx = self.warp_x.get([wx * 0.0008, wz * 0.0008]) * warp_scale;
        let dz = self.warp_z.get([wx * 0.0008, wz * 0.0008]) * warp_scale;
        let hills = self.fbm2(
            &self.hills_a,
            (wx + dx) * 0.0019,
            (wz + dz) * 0.0019,
            4,
            2.0,
            0.5,
        );

        // 4. Ridged mountains â€” only "felt" where continentalness is high.
        //    Lower frequency = wider mountain ranges with broad foothills
        //    rather than tightly-packed spires.
        let ridges = self.ridged_fbm(&self.ridges, wx * 0.00095, wz * 0.00095, 4);
        let continental_mask = smoothstep(0.08, 0.52, cont);
        let ridge_mask = smoothstep(0.40, 0.76, ridges);
        let mountain_mask = continental_mask * ridge_mask;

        // Erosion controls rolling relief while smooth masks reserve the
        // stronger uplift for broad continental ridges. This keeps plains
        // legible and mountain approaches gradual over many chunks.
        let peak_boost = smoothstep(0.48, 0.78, cont) * 18.0;
        let rolling_relief = 7.0 + (1.0 - erod.abs()) * 7.0;
        let base = 50.0 + cont * 34.0 + erod * 4.0;
        let mut h = base + hills * rolling_relief + mountain_mask * 52.0 + peak_boost;

        // ----------- Macro-region modifier -----------
        // Apply ONE geographic-province transform with high strength
        // inside the region's interior, smoothly fading to nothing at
        // the boundary. Winner-take-all so canyon banding NEVER leaks
        // into adjacent plains and vice versa.
        let (region, rs) = self.region(wx, wz);
        match region {
            Region::Canyon => {
                // Grand-Canyon style mesas: snap heights to ~16-block
                // plateaus separated by sharp drops. Only above water.
                if h > WATER_LEVEL as f64 + 4.0 {
                    let step = 24.0; // taller mesa steps
                    let banded = (h / step).round() * step;
                    let pull = rs * 0.92;
                    h = h * (1.0 - pull) + banded * pull;
                    // Boost overall canyon altitude so mesas tower
                    // dramatically above the canyon floor.
                    h += rs * 22.0;
                    // Carved canyon floors: where erosion is high, drop
                    // a deep slot. Creates river-cut canyons through
                    // the mesa fields.
                    let carve = (erod.abs() - 0.45).max(0.0) * 38.0;
                    h -= rs * carve;
                }
            }
            Region::Plateau => {
                // Vast tableland at h~88. Tibetan / Iberian high steppe.
                let plateau_h = 88.0;
                let pull = rs * 0.70;
                h = h * (1.0 - pull) + plateau_h * pull;
                h += rs * hills * 8.0;
            }
            Region::Highland => {
                // Alpine: strong enough for skyline silhouettes, capped
                // so normal worlds do not turn into vertical walls that
                // hitch low-end machines when approached.
                h += rs * ridge_mask * 34.0;
            }
            Region::Wetland => {
                // Floodplain: pull to just above water level.
                let target = WATER_LEVEL as f64 + 1.5;
                let pull = rs * 0.6;
                h = h * (1.0 - pull) + target * pull;
                h += rs * hills * 4.5;
            }
            Region::Karst => {
                // Wide limestone towers rise from a calm jungle floor. Two
                // low-frequency masks retain a karst skyline without the
                // single-column peaks created by cubed high-frequency noise.
                let base_pull = rs * 0.4;
                let karst_floor = WATER_LEVEL as f64 + 6.0;
                h = h * (1.0 - base_pull) + karst_floor * base_pull;
                let broad = self.ridged_fbm(&self.ridges, wx * 0.0022, wz * 0.0022, 3);
                let shoulder =
                    self.ridged_fbm(&self.hills_b, wx * 0.0037 + 31.0, wz * 0.0037 - 17.0, 2);
                let tower = smoothstep(0.48, 0.76, broad * 0.72 + shoulder * 0.28);
                h += rs * tower * tower * 44.0;
            }
            Region::CrystalSpires => {
                // Towering hex-prism-feel pillars on a flat glow-sand
                // floor. The high threshold keeps the biome readable as
                // hero spires with flight corridors instead of a dense
                // translucent wall that overloads low-end GPUs up close.
                let base_pull = rs * 0.62;
                let floor = WATER_LEVEL as f64 + 10.0;
                h = h * (1.0 - base_pull) + floor * base_pull;
                let r1 = self.ridged_fbm(&self.ridges, wx * 0.0075, wz * 0.0075, 3);
                let r2 = self.ridged_fbm(&self.hills_b, wx * 0.0085 + 91.3, wz * 0.0085 - 47.5, 3);
                let spike = r1.min(r2);
                let spike = (spike - 0.49).max(0.0);
                let spike = spike * spike * spike * 2500.0;
                h += rs * spike;
            }
            Region::VolcanicWaste => {
                // Huge basalt plains and massive lava rivers for RPGs and vehicle passes.
                let plateau_h = 72.0;
                let pull = rs * 0.55;
                h = h * (1.0 - pull) + plateau_h * pull;
                h += rs * hills * 4.0;
                let river = self.ridged_fbm(&self.hills_a, wx * 0.003, wz * 0.003, 3); // wider rivers
                if river > 0.70 {
                    // easier threshold for huge canyons
                    let depth = ((river - 0.70) * 50.0).min(18.0);
                    h -= rs * depth;
                }
            }
            Region::GlacierShards => {
                // Razor ridge crevasses for huge bowls and high elevation sniper lookouts
                let ridge_sharp = ridges * ridges;
                h += rs * ridge_sharp * 180.0; // taller ridges
                let base_pull = rs * 0.25;
                let floor = WATER_LEVEL as f64 + 8.0;
                h = h * (1.0 - base_pull) + floor * base_pull;
            }
            Region::AlienReef => {
                // Huge moss hills and huge bone arches
                let base_pull = rs * 0.5;
                let reef_floor = WATER_LEVEL as f64 + 12.0;
                h = h * (1.0 - base_pull) + reef_floor * base_pull;
                h += rs * hills * 15.0; // taller hills
                let pillar_n =
                    self.ridged_fbm(&self.ridges, wx * 0.015 - 13.7, wz * 0.015 + 8.4, 3);
                let pillar = (pillar_n - 0.65).max(0.0);
                let pillar = pillar * pillar * 2400.0; // massive pillars
                h += rs * pillar;
            }
            Region::Plains => {
                // Mixed/normal terrain â€” no province modifier.
            }
        }

        // Coastal smoothing: heights close to the water line create
        // pointy "teeth" shorelines because rounding flips neighbouring
        // columns between y=48 and y=49. Pull heights in the narrow
        // band [WATER_LEVEL-1.5, WATER_LEVEL+2.5] toward a two-level
        // shore curve (sub-water ocean floor vs firm beach at
        // WATER_LEVEL+1) so the transition is stable rather than
        // stochastic.
        // Hydrographic carve: a broad floodplain first settles low terrain
        // toward the water table, then a narrower core cuts a real submerged
        // bed. Existing water filling supplies the visible river surface, so
        // this adds no per-frame simulation or extra render entity.
        let hydro = self.hydrographic_field_for_surface(wx, wz, h);
        h = match (self.world_profile, self.terrain_grammar) {
            (WorldProfile::Natural, TerrainGrammarVersion::V1) => {
                hydrographic_cross_section_v1(h, hydro)
            }
            (WorldProfile::Natural, TerrainGrammarVersion::V2) => {
                natural_hydrographic_cross_section_v2(h, hydro)
            }
            (WorldProfile::Natural, TerrainGrammarVersion::V3) => {
                natural_hydrographic_cross_section_v3(h, hydro)
            }
            // Astral's subsequent world/first-flight authority intentionally
            // retains the byte-established v1 input rather than inheriting a
            // Natural-only bank experiment.
            (WorldProfile::AstralFrontier, _) => hydrographic_cross_section_v1(h, hydro),
        };

        // Astral's infinite regional grammar is applied after generic river
        // carving, then the first-flight sector receives final authority over
        // its lava canyon, landable shelves and mountain hierarchy. Natural
        // worlds never enter this branch and retain byte-identical fields.
        if self.world_profile == WorldProfile::AstralFrontier {
            h = self.apply_astral_world_height(wx, wz, h);
            h = self.apply_astral_frontier_height(wx, wz, h);
        }

        let wl = WATER_LEVEL as f64;
        let delta = h - wl;
        if delta > -1.5 && delta < 2.5 {
            // Smooth-step from "just barely submerged" to "firm beach".
            // Ensures shore columns snap to WATER_LEVEL-1 or WATER_LEVEL+1.
            if delta < 0.5 {
                h = wl - 1.0; // submerged shore â†’ ocean floor
            } else {
                h = wl + 1.0; // exposed shore â†’ beach
            }
        }

        (h.clamp(8.0, 208.0).round() as i32, cont)
    }

    /// Canonical surface used by filling, decoration, spawn lookup, and
    /// public queries. Shoreline cleanup lives here so later passes cannot
    /// decorate a raw column that the terrain pass already submerged.
    fn surface_height(&self, wx: f64, wz: f64) -> (i32, f64) {
        let (mut surface, cont) = self.raw_surface_height(wx, wz);
        if (WATER_LEVEL..=WATER_LEVEL + 2).contains(&surface) {
            let mut land_neighbours = 0;
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    let neighbour = self.raw_surface_height(wx + dx as f64, wz + dz as f64).0;
                    land_neighbours += (neighbour >= WATER_LEVEL) as i32;
                }
            }
            if land_neighbours < 2 {
                surface = WATER_LEVEL - 1;
            }
        }
        (surface, cont)
    }

    /// 3D narrow-band cave noise. Returns `true` if this world cell is
    /// hollow (carved out by a cave).
    fn is_cave(&self, wx: f64, wy: f64, wz: f64) -> bool {
        // Perlin noise is identically zero at the origin for every seed,
        // so without a seed-dependent offset the cave condition
        // (|a| < band && |b| < band) is ALWAYS true at (0, 0, 0) â€”
        // which is right under spawn. That's why every freshly-generated
        // world showed the same cluster of surface holes at spawn no
        // matter what seed was used. Offsetting the sample coords by a
        // seed-derived vector moves that degenerate point somewhere else.
        let s = self.seed as f64;
        let ox = (s * 0.12345).sin() * 10_000.0 + 100_000.0;
        let oy = (s * 0.54321).cos() * 10_000.0 + 100_000.0;
        let oz = (s * 0.98765).sin() * 10_000.0 + 100_000.0;
        // Two FBM fields; caves live where BOTH are close to zero (narrow
        // band), which produces tunnel-like geometry rather than big blobs.
        let a = self.fbm3(
            &self.caves_a,
            (wx + ox) * 0.02, // lower frequency for massive caves
            (wy + oy) * 0.04,
            (wz + oz) * 0.02,
            3,
        );
        let b = self.fbm3(
            &self.caves_b,
            (wx + ox) * 0.02 + 13.7,
            (wy + oy) * 0.04 + 7.1,
            (wz + oz) * 0.02 - 5.3,
            3,
        );
        let band = 0.045; // much wider cave systems
        a.abs() < band && b.abs() < band
    }

    /// Big horizontal "worm" tunnel â€” cuts straight through mountain
    /// sides and hillsides so the player can walk/drive/fly through
    /// them. Distinct from `is_cave` (which is narrow winding caves).
    ///
    /// Strategy: two orthogonal low-frequency 3D fields; the
    /// intersection of their near-zero bands traces a sinuous line in
    /// 3D space â€” a tunnel. The Y coordinate is compressed so the
    /// tunnel stays roughly horizontal (flatter in Y than in XZ).
    fn is_tunnel(&self, wx: f64, wy: f64, wz: f64) -> bool {
        let s = self.seed as f64;
        let ox = (s * 0.37121).sin() * 10_000.0 + 200_000.0;
        let oy = (s * 0.81723).cos() * 10_000.0 + 200_000.0;
        let oz = (s * 0.41287).sin() * 10_000.0 + 200_000.0;

        // Path noise gently bends the tunnel vertically so long
        // tunnels rise and fall like a real subway line.
        let bend = self
            .tunnel_path
            .get([(wx + ox) * 0.0015, (wz + oz) * 0.0015])
            * 12.0;
        let wy_adj = wy - bend;

        // Strong Y compression (Ã—0.18) â†’ tunnels are much longer in
        // XZ than in Y â†’ you get long horizontal tubes, not vertical
        // shafts.
        let a = self.fbm3(
            &self.tunnel_a,
            (wx + ox) * 0.010,
            (wy_adj + oy) * 0.018,
            (wz + oz) * 0.010,
            3,
        );
        let b = self.fbm3(
            &self.tunnel_b,
            (wx + ox) * 0.010 + 31.3,
            (wy_adj + oy) * 0.018 + 17.1,
            (wz + oz) * 0.010 - 12.7,
            3,
        );
        // Wider band than normal caves â†’ 3-6 block tall corridors.
        let band = 0.055;
        a.abs() < band && b.abs() < band
    }

    /// Massive spherical cavern rooms, sparse and deep. The player
    /// pops out of a tunnel into one of these once in a while.
    fn is_cavern(&self, wx: f64, wy: f64, wz: f64) -> bool {
        let s = self.seed as f64;
        let ox = (s * 0.61987).sin() * 10_000.0 + 300_000.0;
        let oy = (s * 0.23918).cos() * 10_000.0 + 300_000.0;
        let oz = (s * 0.72831).sin() * 10_000.0 + 300_000.0;
        let v = self.fbm3(
            &self.cavern,
            (wx + ox) * 0.006,
            (wy + oy) * 0.010,
            (wz + oz) * 0.006,
            2,
        );
        // Very sparse â€” only the strongest noise peaks.
        v > 0.58
    }

    /// Pick a biome for this column based on temperature + moisture +
    /// continentalness (so beaches appear at coastlines, mountains at high
    /// continentalness, etc.).
    fn biome(&self, wx: f64, wz: f64, height: i32, cont: f64) -> Biome {
        if let Some(biome) = self.astral_frontier_biome_override(wx, wz) {
            return biome;
        }
        if let Some(biome) = self.astral_world_biome_override(wx, wz) {
            return biome;
        }
        if height <= WATER_LEVEL - 2 {
            return Biome::Ocean;
        }
        let hydro = self.hydrographic_field_for_surface(wx, wz, height as f64);
        let v3_living_river_cap = self.world_profile == WorldProfile::Natural
            && self.terrain_grammar == TerrainGrammarVersion::V3
            && height > WATER_LEVEL + 2
            && hydro.corridor > 0.16;
        // Region overrides (above water): alien & special regions
        // dominate even at weak strength so the player sees them
        // often. Classic canyons / karst need a bit more authority. V3's
        // explicit river cap is the one Natural-only exception: a regional
        // rock palette must not turn the new living shoulder back into a
        // limestone palisade.
        let (region, rs) = self.region(wx, wz);
        if !v3_living_river_cap && rs > 0.08 && height > WATER_LEVEL + 2 {
            match region {
                Region::Canyon => {
                    if rs > 0.25 {
                        return Biome::Mesa;
                    }
                }
                Region::Karst => {
                    if rs > 0.25 {
                        return Biome::Karst;
                    }
                }
                Region::CrystalSpires => return Biome::CrystalSpires,
                Region::VolcanicWaste => return Biome::VolcanicWaste,
                Region::GlacierShards => return Biome::GlacierShards,
                Region::AlienReef => return Biome::AlienReef,
                _ => {}
            }
        }

        // Higher shoulders of a hydrographic corridor are living floodplain,
        // not an ocean beach. Keep the lowest two exposed steps sandy, then
        // let moisture pull the bank into a climate-appropriate green biome.
        // This yields a thin readable shoreline backed by gallery vegetation
        // instead of a hundred-block desert ribbon around every river.
        if height > WATER_LEVEL + 2 && hydro.corridor > 0.16 {
            let temperature = self.temperature.get([wx * 0.0015, wz * 0.0015]);
            let atmospheric_moisture = self.moisture.get([wx * 0.0015, wz * 0.0015]);
            if temperature < -0.34 {
                return Biome::Tundra;
            }
            if temperature > 0.16 && atmospheric_moisture > 0.18 {
                return Biome::Jungle;
            }
            return Biome::Forest;
        }
        // Wider beach band (up to +3 above water) gives shores actual
        // depth instead of a 1-block sand stripe. The exact extent is
        // perturbed by a low-frequency noise so beaches feather into
        // grass with organic in-and-out fingers â€” the single biggest
        // visual upgrade for coastlines, no extra geometry needed.
        let beach_wobble = self.moisture.get([wx * 0.008, wz * 0.008]) * 3.5; // wider, softer beaches
        let beach_top = WATER_LEVEL + 4 + beach_wobble as i32; // even higher beach transitions
        if height <= beach_top {
            return Biome::Beach;
        }

        let temp = self.temperature.get([wx * 0.0015, wz * 0.0015]); // vast temperature bands
        let moist = self.moisture.get([wx * 0.0015, wz * 0.0015]);

        // Altitude-driven snow line, perturbed by temperature so the line
        // isn't a ruler-straight horizontal cut. Cold latitudes have snow
        // starting ~15 blocks lower, warm latitudes push it ~15 higher.
        // A second low-freq noise wobbles the line column-by-column Â±6
        // blocks so the grass-rock-snow transition fingers organically
        // up and down the mountainside instead of running as a clean
        // horizontal stripe.
        let line_wobble = self.moisture.get([wx * 0.008, wz * 0.008]) * 6.0
            + self.erosion.get([wx * 0.02, wz * 0.02]) * 3.0;
        let snow_line = 138 + (temp * -15.0) as i32 + line_wobble as i32;
        let rock_line = snow_line - 20 + (line_wobble * 0.6) as i32;
        if height > snow_line {
            return Biome::SnowyMountains;
        }
        if height > rock_line {
            return Biome::Mountains;
        }

        if cont > 0.55 && temp > 0.2 {
            return if temp > 0.4 {
                Biome::Desert
            } else {
                Biome::Savanna
            };
        }

        if temp < -0.3 {
            return Biome::Tundra;
        }

        if moist > 0.3 && temp > 0.1 {
            return Biome::Jungle;
        }

        if moist > 0.0 {
            return Biome::Forest;
        }

        Biome::Plains
    }

    /// Pick the surface / sub-surface / stone block for a biome.
    fn blocks_for(biome: Biome) -> (BlockType, BlockType, BlockType) {
        // (surface, sub-surface 3-blocks deep, everything below)
        match biome {
            Biome::Ocean | Biome::Beach => (BlockType::Sand, BlockType::Sand, BlockType::Stone),
            Biome::Plains => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Forest => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Jungle => (BlockType::Grass, BlockType::Dirt, BlockType::Stone),
            Biome::Desert => (BlockType::Sand, BlockType::Sand, BlockType::Stone),
            Biome::Savanna => (BlockType::SavannaGrass, BlockType::Dirt, BlockType::Stone),
            Biome::Tundra => (BlockType::TundraGrass, BlockType::Dirt, BlockType::Stone),
            Biome::Mountains => (BlockType::Stone, BlockType::Stone, BlockType::Stone),
            Biome::SnowyMountains => (BlockType::Snow, BlockType::Stone, BlockType::Stone),
            // Mesa: rust-red dust on top, sandstone underneath. The
            // generate() loop overrides `core` per-Y so cliff faces
            // stripe between RedStone / MesaClay / RedSand bands.
            Biome::Mesa => (BlockType::RedSand, BlockType::RedStone, BlockType::RedStone),
            // Karst pillars: dark mossy limestone with a brighter core.
            Biome::Karst => (
                BlockType::MossStone,
                BlockType::Limestone,
                BlockType::Limestone,
            ),
            // Alien crystal spires: glow-sand floor, crystal cores.
            // The generate() loop overrides `top` for tall columns so
            // the spire shafts read as solid crystal rather than sand.
            Biome::CrystalSpires => (BlockType::GlowSand, BlockType::Crystal, BlockType::Crystal),
            // Volcanic basalt waste â€” lava channel handling is special-
            // cased in generate() (see VolcanicWaste branch below).
            Biome::VolcanicWaste => (BlockType::Basalt, BlockType::Basalt, BlockType::Basalt),
            // Glacier ridges: snow cap, ice body, stone deep base.
            Biome::GlacierShards => (BlockType::Snow, BlockType::Ice, BlockType::Stone),
            // Alien reef: magenta moss surface, bone rock for pillars.
            Biome::AlienReef => (
                BlockType::AlienMoss,
                BlockType::BoneRock,
                BlockType::BoneRock,
            ),
        }
    }

    /// Resolve the exposed surface and shallow layer for a local slope.
    /// Karst needs a dedicated rule: moss caps flat shelves, while every
    /// inclined face exposes one continuous limestone mass. Leaving a dark
    /// moss voxel over three pale sub-surface voxels on each stair produced
    /// high-frequency zebra bands across distant cliffs.
    fn slope_surface_layers(
        biome: Biome,
        slope: i32,
        top: BlockType,
        sub: BlockType,
    ) -> (BlockType, BlockType) {
        match coarse_surface_family(biome, slope as f32) {
            BlockType::Limestone if biome == Biome::Karst && slope >= 2 => {
                (BlockType::Limestone, BlockType::Limestone)
            }
            BlockType::Stone if slope >= 4 => (BlockType::Stone, BlockType::Stone),
            BlockType::Dirt if slope >= 2 => (BlockType::Dirt, BlockType::Gravel),
            _ => (top, sub),
        }
    }

    /// Whether a surface palette may take part in natural biome feathering.
    #[inline]
    fn supports_natural_ecotone(biome: Biome) -> bool {
        !matches!(biome, Biome::Ocean) && !biome.is_showcase_terrain()
    }

    /// Select the neighbouring surface material in broad, deterministic
    /// clusters rather than per-voxel speckle. World-space cells keep the
    /// result seamless across chunk borders, while one low-frequency sample
    /// softens the otherwise square cell silhouette.
    fn clustered_ecotone_choice(
        &self,
        current: BlockType,
        neighbour: BlockType,
        wx: i32,
        wz: i32,
    ) -> BlockType {
        use crate::settings::SceneryQuality;

        let (cell_size, cell_coverage, noise_floor) = match self.scenery_quality {
            SceneryQuality::Off | SceneryQuality::Lean => return current,
            SceneryQuality::Balanced => (8, 0.52, 0.06),
            SceneryQuality::Lush => (10, 0.72, -0.14),
        };
        if current == neighbour {
            return current;
        }

        let cell_x = wx.div_euclid(cell_size);
        let cell_z = wz.div_euclid(cell_size);
        let cell_roll = column_rand(self.seed ^ 0xEC07_0AE1, cell_x, cell_z);
        if cell_roll > cell_coverage {
            return current;
        }

        let organic = self.hills_a.get([
            wx as f64 * 0.034 + cell_roll * 7.0,
            wz as f64 * 0.034 - cell_roll * 5.0,
        ]);
        if organic + (cell_roll - 0.5) * 0.32 >= noise_floor {
            neighbour
        } else {
            current
        }
    }

    /// Feather only the visible surface skin near biome borders. This never
    /// changes the canonical height, cave field, sub-surface material, or
    /// decoration count. Off/Lean return before any additional noise work.
    fn ecotone_surface_block(
        &self,
        biome: Biome,
        current: BlockType,
        surface: i32,
        cont: f64,
        wx: i32,
        wz: i32,
    ) -> BlockType {
        use crate::settings::SceneryQuality;

        let sample_distance = match self.scenery_quality {
            SceneryQuality::Off | SceneryQuality::Lean => return current,
            SceneryQuality::Balanced => 9,
            SceneryQuality::Lush => 13,
        };
        if !Self::supports_natural_ecotone(biome) || current != Self::blocks_for(biome).0 {
            return current;
        }

        let cell_size = if self.scenery_quality == SceneryQuality::Lush {
            10
        } else {
            8
        };
        let cell_x = wx.div_euclid(cell_size);
        let cell_z = wz.div_euclid(cell_size);
        let direction_roll = column_rand(self.seed ^ 0xEC07_0D12, cell_x, cell_z);
        let directions = [
            (1, 0),
            (1, 1),
            (0, 1),
            (-1, 1),
            (-1, 0),
            (-1, -1),
            (0, -1),
            (1, -1),
        ];
        let direction = directions
            [((direction_roll * directions.len() as f64) as usize).min(directions.len() - 1)];
        let neighbour_biome = self.biome(
            (wx + direction.0 * sample_distance) as f64,
            (wz + direction.1 * sample_distance) as f64,
            surface,
            cont,
        );
        if neighbour_biome == biome || !Self::supports_natural_ecotone(neighbour_biome) {
            return current;
        }

        self.clustered_ecotone_choice(current, Self::blocks_for(neighbour_biome).0, wx, wz)
    }

    /// World-Y banding keeps mesa sediment continuous across columns.
    fn mesa_band(wy: i32) -> BlockType {
        // 6-block bands cycling through 4 colors. The repetition pattern
        // (red, red, clay, red, clay, red, ...) avoids feeling stripey
        // while still reading as sedimentary geology.
        let band = ((wy.rem_euclid(24)) / 4) as u8;
        match band {
            0 => BlockType::RedStone,
            1 => BlockType::MesaClay,
            2 => BlockType::RedStone,
            3 => BlockType::RedSand,
            4 => BlockType::MesaClay,
            _ => BlockType::RedStone,
        }
    }

    fn surface_detail_block(
        &self,
        biome: Biome,
        current: BlockType,
        slope: i32,
        wx: i32,
        wz: i32,
    ) -> BlockType {
        match biome {
            Biome::Plains
            | Biome::Forest
            | Biome::Jungle
            | Biome::Beach
            | Biome::Desert
            | Biome::Savanna
            | Biome::Ocean => return current,
            _ => {}
        }

        let grain = self
            .hills_b
            .get([wx as f64 * 0.018 + 19.0, wz as f64 * 0.018 - 31.0]);

        match biome {
            Biome::Tundra => {
                if grain > 0.42 {
                    BlockType::Snow
                } else if grain < -0.56 && slope >= 1 {
                    BlockType::Gravel
                } else {
                    current
                }
            }
            Biome::Mountains | Biome::SnowyMountains => {
                if slope >= 2 && grain < -0.28 {
                    BlockType::Gravel
                } else if matches!(biome, Biome::SnowyMountains) && grain > -0.18 {
                    BlockType::Snow
                } else {
                    current
                }
            }
            Biome::Mesa => {
                if grain > 0.56 {
                    BlockType::MesaClay
                } else if grain < -0.62 {
                    BlockType::RedStone
                } else {
                    current
                }
            }
            Biome::Karst => {
                // Karst is not a white quarry from edge to edge. Broad,
                // sheltered flats support meadow; damp ledges retain muted
                // mossy rock; only exposed flats and every inclined face stay
                // limestone. Both fields are low-frequency so the three
                // materials form landscape masses rather than contour noise.
                if slope >= 2 || current != BlockType::MossStone {
                    return BlockType::Limestone;
                }
                let shelter = self
                    .region_b
                    .get([wx as f64 * 0.0041 - 41.0, wz as f64 * 0.0041 + 27.0]);
                let moisture = self
                    .hills_a
                    .get([wx as f64 * 0.0027 + 13.0, wz as f64 * 0.0027 - 37.0]);
                let habitat = shelter * 0.62 + moisture * 0.38;
                let living_threshold = if slope == 1 { 0.20 } else { -0.04 };
                let moss_threshold = if slope == 1 { -0.06 } else { -0.25 };
                if habitat > living_threshold {
                    BlockType::Grass
                } else if habitat > moss_threshold {
                    BlockType::MossStone
                } else {
                    BlockType::Limestone
                }
            }
            Biome::CrystalSpires => {
                if grain > 0.45 {
                    BlockType::LuminiteCrystal
                } else if grain > 0.18 {
                    BlockType::Crystal
                } else {
                    current
                }
            }
            Biome::VolcanicWaste => match self.terrain_grammar {
                // Byte-established V1 behavior. Do not "clean up" this branch:
                // legacy edited chunks were composed against these exact dry
                // grain-selected Lava top voxels.
                TerrainGrammarVersion::V1 if grain > 0.67 && slope == 0 => BlockType::Lava,
                // V2 gives Lava one explicit volume authority: the bounded
                // VolcanicWaste channel fill in `generate`. This avoids
                // isolated emissive puddles on unrelated dry ground.
                TerrainGrammarVersion::V1
                | TerrainGrammarVersion::V2
                | TerrainGrammarVersion::V3 => current,
            },
            Biome::GlacierShards => {
                if grain > 0.20 {
                    BlockType::Ice
                } else {
                    current
                }
            }
            Biome::AlienReef => {
                if grain > 0.52 {
                    BlockType::IridiumVein
                } else if grain < -0.48 {
                    BlockType::BoneRock
                } else {
                    current
                }
            }
            Biome::Plains
            | Biome::Forest
            | Biome::Jungle
            | Biome::Beach
            | Biome::Desert
            | Biome::Savanna
            | Biome::Ocean => current,
        }
    }

    /// Fill a chunk with terrain. Deterministic for a given (seed, pos).
    pub fn generate(&self, chunk: &mut Chunk) {
        let cx = chunk.pos.x;
        let cz = chunk.pos.z;
        let Some((origin_x, origin_y, origin_z)) = checked_terrain_chunk_origins(chunk.pos) else {
            // Invalid numeric-domain chunks are explicit empty space. Reset a
            // reused caller buffer as well as a newly allocated chunk so the
            // rejection cannot preserve stale authoritative-looking voxels.
            chunk.install_voxels(std::sync::Arc::new([AIR; crate::chunk::CHUNK_VOLUME]));
            return;
        };
        let surface_grid = TerrainSurfaceGrid::build(self, cx, cz);

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = origin_x + lx as i32;
                let wz = origin_z + lz as i32;
                let (surface, cont) = surface_grid.sample(lx, lz);

                // --------- Slope analysis ---------
                // Compute the local gradient from 4 cardinal height
                // samples. The slope (max Î”height across neighbours) is
                // what makes the difference between "grass on a shallow
                // hill" and "cliff face of exposed rock". Steep columns
                // drop grass â†’ stone for the surface block, producing
                // believable cliffs, scree lines on mountainsides, and
                // dirt streaks where slopes transition.
                let (hn, _) = surface_grid.sample_offset(lx, lz, 0, -1);
                let (hs, _) = surface_grid.sample_offset(lx, lz, 0, 1);
                let (he, _) = surface_grid.sample_offset(lx, lz, 1, 0);
                let (hw, _) = surface_grid.sample_offset(lx, lz, -1, 0);
                let slope = (surface - hn)
                    .abs()
                    .max((surface - hs).abs())
                    .max((surface - he).abs())
                    .max((surface - hw).abs());

                let biome = self.biome(wx as f64, wz as f64, surface, cont);
                let (mut top, sub, core) = Self::blocks_for(biome);

                // Crystal Spires: tall columns ARE the spires, so their
                // top block must be Crystal (not the GlowSand floor).
                // Threshold = floor + 6 blocks: anything above that is
                // a pillar shaft.
                if biome == Biome::CrystalSpires && surface > WATER_LEVEL + 16 {
                    top = BlockType::Crystal;
                }

                // Volcanic Waste: per-column lava-fill level. Lowered
                // to 52 (just above water level) so lava fills only
                // deep channels, not the whole basin \u2014 keeps the
                // biome walkable and doesn't blind the player with a
                // sea of emissive blocks.
                let volcanic_lava_level: i32 = 52;
                let in_volcanic = biome == Biome::VolcanicWaste;

                // Slope overrides keep exposed masses coherent at flight
                // distance while preserving biome-specific flat caps. A
                // living river shoulder keeps one continuous soil mass;
                // alternating dirt/gravel on every voxel step produced a
                // barcode-like trench in flight views.
                let riparian_bank = surface > WATER_LEVEL + 2
                    && self
                        .hydrographic_field_for_surface(wx as f64, wz as f64, surface as f64)
                        .corridor
                        > 0.16
                    && matches!(biome, Biome::Forest | Biome::Jungle | Biome::Tundra);
                let (top, sub) = if riparian_bank {
                    if slope >= 4 {
                        (BlockType::Dirt, BlockType::Dirt)
                    } else {
                        (top, BlockType::Dirt)
                    }
                } else {
                    Self::slope_surface_layers(biome, slope, top, sub)
                };
                let top = self.surface_detail_block(biome, top, slope, wx, wz);
                let top = self.ecotone_surface_block(biome, top, surface, cont, wx, wz);

                for ly in 0..CHUNK_SIZE {
                    let wy = origin_y + ly as i32;

                    // Bedrock at the bottom of the world.
                    if wy <= BEDROCK_LEVEL {
                        chunk.set(lx, ly, lz, BlockType::Bedrock.into());
                        continue;
                    }

                    // Above the surface: air or water (or lava in
                    // VolcanicWaste regions, where the carved channels
                    // pool molten basalt instead of seawater).
                    if wy > surface {
                        if in_volcanic && wy <= volcanic_lava_level {
                            chunk.set(lx, ly, lz, BlockType::Lava.into());
                        } else if wy <= WATER_LEVEL {
                            chunk.set(lx, ly, lz, BlockType::Water.into());
                        }
                        // else: leave as AIR (default-initialised).
                        continue;
                    }

                    // Carve caves â€” never inside the top layer (preserves
                    // the surface skin) and never near the water line so
                    // oceans don't drain through holes. The dynamic buffer
                    // grows on steep terrain so lateral faces stay coherent.
                    // steep cliff edges (Î”surface â‰¤ 14 blocks between
                    // Also keep a wide buffer around WATER_LEVEL
                    // (Â±6) so sub-surface aquifers never punch through
                    // beaches or shallow seabeds.
                    // This keeps caves underground even when a neighbouring
                    // column drops sharply along a cliff face.
                    let surface_skin = subterranean_surface_skin(slope);
                    let cave_allowed = wy < surface - surface_skin.max(18)
                        && wy > BEDROCK_LEVEL + 2
                        && (wy < WATER_LEVEL - 6 || wy > WATER_LEVEL + 6);
                    if cave_allowed && self.is_cave(wx as f64, wy as f64, wz as f64) {
                        continue;
                    }

                    // Big horizontal tunnels â€” carve through mountains
                    // and ridges, but preserve the same geological shell.
                    // Curated portals can expose selected mouths without
                    // turning every steep skyline into a procedural sponge.
                    let tunnel_allowed = wy < surface - surface_skin
                        && wy > BEDROCK_LEVEL + 2
                        && (wy < WATER_LEVEL - 4 || wy > WATER_LEVEL + 4);
                    if tunnel_allowed && self.is_tunnel(wx as f64, wy as f64, wz as f64) {
                        continue;
                    }

                    // Rare giant caverns â€” only deep underground so
                    // they never collapse the surface.
                    if wy < surface - 30
                        && wy > BEDROCK_LEVEL + 2
                        && self.is_cavern(wx as f64, wy as f64, wz as f64)
                    {
                        continue;
                    }

                    let depth = surface - wy;
                    let block = if depth == 0 {
                        top
                    } else if depth <= 3 {
                        sub
                    } else if matches!(biome, Biome::Mesa) {
                        // Mesa cliff faces show horizontal red/buff
                        // sedimentary banding â€” pure function of Y so
                        // adjacent columns line up into continuous
                        // stripes the player can read as geology.
                        Self::mesa_band(wy)
                    } else {
                        core
                    };
                    chunk.set(lx, ly, lz, block.into());
                }
            }
        }

        chunk.dirty = true;
        // Decorate AFTER the main fill so trees see the final surface.
        self.decorate(chunk);
        chunk.finalize_uniform_flags();
    }

    /// One sparse, seed-stable destination candidate per Astral macro-cell.
    /// The site only materialises on land with a usable local slope; rejecting
    /// an unsuitable cell is preferable to flattening terrain behind the
    /// player's back or forcing every province to contain a building.
    fn astral_waypoint_for_cell(
        &self,
        owner_cell_x: i32,
        owner_cell_z: i32,
    ) -> Option<AstralWaypointSpec> {
        let node = self.astral_macro_node_for_cell(owner_cell_x, owner_cell_z)?;
        let center_x = node.center.x;
        let center_z = node.center.y;
        let surface = self.surface_height_at(center_x, center_z);
        if surface <= WATER_LEVEL + 5 || surface >= 190 {
            return None;
        }
        let slope = cardinal_surface_slope(self, center_x, center_z, surface);
        if slope > 3 {
            return None;
        }
        let biome = self.biome_at(center_x, center_z);
        let (radius, height) = match node.kind {
            AstralWaypointKind::RelaySpire => (22, 30),
            AstralWaypointKind::SkyDock => (28, 18),
            AstralWaypointKind::CrystalGarden => (24, 20),
            AstralWaypointKind::TransitGate => (26, 22),
        };
        let platform = match biome {
            Biome::Karst | Biome::Plains | Biome::Forest | Biome::Jungle => BlockType::ZenStone,
            Biome::Mesa | Biome::VolcanicWaste => BlockType::ShipHullDark,
            _ => BlockType::ShipHullAlloy,
        };
        let accent = match (node.kind, biome) {
            (AstralWaypointKind::CrystalGarden, Biome::CrystalSpires | Biome::AlienReef) => {
                BlockType::LuminiteCrystal
            }
            (AstralWaypointKind::CrystalGarden, _) => BlockType::Crystal,
            (_, Biome::Mesa | Biome::VolcanicWaste) => BlockType::NeonAmber,
            (_, Biome::AlienReef) => BlockType::NeonMagenta,
            _ => BlockType::NeonCyan,
        };
        Some(AstralWaypointSpec {
            center: bevy::math::IVec3::new(center_x, surface + 1, center_z),
            kind: node.kind,
            quarter_turns: node.quarter_turns,
            radius,
            height,
            platform,
            accent,
        })
    }

    fn for_each_astral_waypoint_near_chunk(
        &self,
        cx: i32,
        cz: i32,
        mut visit: impl FnMut(AstralWaypointSpec),
    ) {
        if self.world_profile != WorldProfile::AstralFrontier {
            return;
        }
        let min_x = cx.saturating_mul(CHUNK_SIZE_I);
        let min_z = cz.saturating_mul(CHUNK_SIZE_I);
        let max_x = min_x.saturating_add(CHUNK_SIZE_I - 1);
        let max_z = min_z.saturating_add(CHUNK_SIZE_I - 1);
        let owner_min_x = min_x
            .saturating_sub(ASTRAL_WAYPOINT_MAX_RADIUS)
            .div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_max_x = max_x
            .saturating_add(ASTRAL_WAYPOINT_MAX_RADIUS)
            .div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_min_z = min_z
            .saturating_sub(ASTRAL_WAYPOINT_MAX_RADIUS)
            .div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_max_z = max_z
            .saturating_add(ASTRAL_WAYPOINT_MAX_RADIUS)
            .div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);

        for owner_z in owner_min_z..=owner_max_z {
            for owner_x in owner_min_x..=owner_max_x {
                let Some(spec) = self.astral_waypoint_for_cell(owner_x, owner_z) else {
                    continue;
                };
                if spec.center.x.saturating_add(spec.radius) < min_x
                    || spec.center.x.saturating_sub(spec.radius) > max_x
                    || spec.center.z.saturating_add(spec.radius) < min_z
                    || spec.center.z.saturating_sub(spec.radius) > max_z
                {
                    continue;
                }
                visit(spec);
            }
        }
    }

    /// Locate the nearest generated waypoint within a bounded macro-cell ring.
    /// Map, mission and autonomous-QA systems receive only the stable world
    /// coordinate; the private shape grammar can evolve without save coupling.
    pub fn find_astral_waypoint_near(
        &self,
        wx: i32,
        wz: i32,
        max_cell_radius: i32,
    ) -> Option<bevy::math::IVec3> {
        if self.world_profile != WorldProfile::AstralFrontier {
            return None;
        }
        let owner_x = wx.div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let owner_z = wz.div_euclid(ASTRAL_WAYPOINT_CELL_SIZE);
        let mut best: Option<(i128, bevy::math::IVec3)> = None;
        for radius in 0..=max_cell_radius.clamp(0, 64) {
            for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if radius > 0 && dx.abs() < radius && dz.abs() < radius {
                        continue;
                    }
                    let Some(spec) = self.astral_waypoint_for_cell(
                        owner_x.saturating_add(dx),
                        owner_z.saturating_add(dz),
                    ) else {
                        continue;
                    };
                    let delta_x = i128::from(spec.center.x) - i128::from(wx);
                    let delta_z = i128::from(spec.center.z) - i128::from(wz);
                    let distance_sq = delta_x * delta_x + delta_z * delta_z;
                    if best.is_none_or(|(best_distance, _)| distance_sq < best_distance) {
                        best = Some((distance_sq, spec.center));
                    }
                }
            }
            // Once a complete nearer ring produced a site, any later ring is
            // at least one full macro-cell farther; keep search latency fixed.
            if best.is_some() && radius >= 2 {
                break;
            }
        }
        best.map(|(_, center)| center)
    }

    fn for_each_astral_route_near_chunk(
        &self,
        cx: i32,
        cz: i32,
        mut visit: impl FnMut(AstralRouteSpec),
    ) {
        let min_x = cx.saturating_mul(CHUNK_SIZE_I);
        let min_z = cz.saturating_mul(CHUNK_SIZE_I);
        let max_x = min_x.saturating_add(CHUNK_SIZE_I - 1);
        let max_z = min_z.saturating_add(CHUNK_SIZE_I - 1);
        let center_x = f64::from(min_x.saturating_add(CHUNK_SIZE_I / 2));
        let center_z = f64::from(min_z.saturating_add(CHUNK_SIZE_I / 2));
        self.for_each_astral_route_near_point(center_x, center_z, |route| {
            const PAINT_MARGIN: i32 = 4;
            let route_min_x = route
                .start
                .center
                .x
                .min(route.end.center.x)
                .saturating_sub(PAINT_MARGIN);
            let route_max_x = route
                .start
                .center
                .x
                .max(route.end.center.x)
                .saturating_add(PAINT_MARGIN);
            let route_min_z = route
                .start
                .center
                .y
                .min(route.end.center.y)
                .saturating_sub(PAINT_MARGIN);
            let route_max_z = route
                .start
                .center
                .y
                .max(route.end.center.y)
                .saturating_add(PAINT_MARGIN);
            if route_max_x < min_x
                || route_min_x > max_x
                || route_max_z < min_z
                || route_min_z > max_z
            {
                return;
            }
            visit(route);
        });
    }

    fn paint_astral_route_into_chunk(&self, chunk: &mut Chunk, route: AstralRouteSpec) -> usize {
        let origin_x = chunk.pos.x * CHUNK_SIZE_I;
        let origin_y = chunk.pos.y * CHUNK_SIZE_I;
        let origin_z = chunk.pos.z * CHUNK_SIZE_I;
        let segment_length = ((f64::from(route.end.center.x - route.start.center.x)).powi(2)
            + (f64::from(route.end.center.y - route.start.center.y)).powi(2))
        .sqrt();
        let mut painted = 0usize;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = origin_x + lx as i32;
                let wz = origin_z + lz as i32;
                let (distance, t) = Self::point_segment_distance(
                    f64::from(wx),
                    f64::from(wz),
                    f64::from(route.start.center.x),
                    f64::from(route.start.center.y),
                    f64::from(route.end.center.x),
                    f64::from(route.end.center.y),
                );
                if distance > 3.4 {
                    continue;
                }
                let deck_y = self.surface_height_at(wx, wz) + 1;
                let phase = (t * segment_length).round() as i32;
                let block = if phase.rem_euclid(14) <= 1 {
                    route.accent
                } else if distance > 2.15 {
                    BlockType::ShipHullAlloy
                } else {
                    BlockType::ShipHullDark
                };
                set_authored(chunk, lx, deck_y, lz, block, origin_y);
                for dy in 1..=3 {
                    set_authored(chunk, lx, deck_y + dy, lz, BlockType::Air, origin_y);
                }
                painted += 1;
            }
        }
        painted
    }

    fn decorate_astral_routes(&self, chunk: &mut Chunk) {
        let pos = chunk.pos;
        self.for_each_astral_route_near_chunk(pos.x, pos.z, |route| {
            self.paint_astral_route_into_chunk(chunk, route);
        });
    }

    fn paint_astral_waypoint_into_chunk(chunk: &mut Chunk, spec: AstralWaypointSpec) -> usize {
        let origin_x = chunk.pos.x * CHUNK_SIZE_I;
        let origin_y = chunk.pos.y * CHUNK_SIZE_I;
        let origin_z = chunk.pos.z * CHUNK_SIZE_I;
        let mut painted = 0usize;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = origin_x + lx as i32;
                let wz = origin_z + lz as i32;
                let delta = IVec2::new(wx - spec.center.x, wz - spec.center.z);
                let local =
                    AstralFrontierLayout::rotate_quarters(delta, (4 - spec.quarter_turns) & 3);
                let x = local.x;
                let z = local.y;
                let radial_sq = x * x + z * z;
                let inside = match spec.kind {
                    AstralWaypointKind::RelaySpire => {
                        radial_sq <= 12 * 12
                            || (x.abs() <= 3 && z.abs() <= spec.radius)
                            || (z.abs() <= 3 && x.abs() <= spec.radius)
                    }
                    AstralWaypointKind::SkyDock => {
                        let ellipse = x * x * 10 * 10 + z * z * spec.radius * spec.radius;
                        ellipse <= spec.radius * spec.radius * 10 * 10
                            || (x.abs() <= 16 && z.abs() <= 12)
                    }
                    AstralWaypointKind::CrystalGarden => {
                        let radius = (f64::from(radial_sq)).sqrt();
                        radial_sq <= 17 * 17
                            || (radius >= 20.0 && radius <= f64::from(spec.radius))
                            || ((x.abs() <= 3 || z.abs() <= 3)
                                && radial_sq <= spec.radius * spec.radius)
                    }
                    AstralWaypointKind::TransitGate => {
                        (x.abs() <= spec.radius && z.abs() <= 6)
                            || ((x - 11).pow(2) + z * z <= 10 * 10)
                            || ((x + 11).pow(2) + z * z <= 10 * 10)
                    }
                };
                if !inside {
                    continue;
                }

                let rim = match spec.kind {
                    AstralWaypointKind::RelaySpire => {
                        radial_sq >= 10 * 10 || x.abs().max(z.abs()) >= spec.radius - 2
                    }
                    AstralWaypointKind::SkyDock => z.abs() >= 8 || x.abs() >= spec.radius - 2,
                    AstralWaypointKind::CrystalGarden => {
                        let radius = (f64::from(radial_sq)).sqrt();
                        radius >= f64::from(spec.radius - 2) || (17.0..=20.0).contains(&radius)
                    }
                    AstralWaypointKind::TransitGate => z.abs() >= 5 || x.abs() >= spec.radius - 2,
                };
                let axis = x.abs() <= 1 || z.abs() <= 1;
                let deck = if rim {
                    spec.accent
                } else if axis {
                    BlockType::ShipHullAlloy
                } else {
                    spec.platform
                };
                set_authored(chunk, lx, spec.center.y, lz, deck, origin_y);
                painted += 1;

                // Clear only the headroom promised by every site. Saved player
                // edits are applied after deterministic generation, so this
                // removes procedural trees/props without erasing user work.
                for dy in 1..=3 {
                    set_authored(chunk, lx, spec.center.y + dy, lz, BlockType::Air, origin_y);
                }

                match spec.kind {
                    AstralWaypointKind::RelaySpire => {
                        let cheb = x.abs().max(z.abs());
                        if cheb <= 3 {
                            for dy in 1..=spec.height {
                                let taper = (dy > spec.height - 5 && cheb > 1)
                                    || (dy > spec.height - 2 && cheb > 0);
                                if taper {
                                    continue;
                                }
                                let block = if dy == spec.height || dy.rem_euclid(6) == 0 {
                                    spec.accent
                                } else if cheb == 3 {
                                    BlockType::ShipHullDark
                                } else {
                                    BlockType::ShipHullAlloy
                                };
                                set_authored(chunk, lx, spec.center.y + dy, lz, block, origin_y);
                                painted += 1;
                            }
                        }
                        for (beacon_x, beacon_z) in [(14, 0), (-14, 0), (0, 14), (0, -14)] {
                            let beacon_cheb = (x - beacon_x).abs().max((z - beacon_z).abs());
                            if beacon_cheb <= 1 {
                                for dy in 1..=10 {
                                    set_authored(
                                        chunk,
                                        lx,
                                        spec.center.y + dy,
                                        lz,
                                        if dy == 10 || dy == 6 {
                                            spec.accent
                                        } else {
                                            BlockType::ShipHullDark
                                        },
                                        origin_y,
                                    );
                                    painted += 1;
                                }
                            }
                        }
                    }
                    AstralWaypointKind::SkyDock => {
                        let pylon = (x.abs() - 20).abs() <= 1 && (z.abs() - 5).abs() <= 1;
                        if pylon {
                            for dy in 1..=12 {
                                let block = if dy == 12 || dy == 6 {
                                    spec.accent
                                } else {
                                    BlockType::ShipHullAlloy
                                };
                                set_authored(chunk, lx, spec.center.y + dy, lz, block, origin_y);
                                painted += 1;
                            }
                        }
                        if z.abs() >= 4
                            && z.abs() <= 6
                            && x.abs() <= 21
                            && (x.abs() >= 19 || z.abs() == 5)
                        {
                            set_authored(
                                chunk,
                                lx,
                                spec.center.y + 12,
                                lz,
                                if x.rem_euclid(6) == 0 {
                                    spec.accent
                                } else {
                                    BlockType::ShipHullAlloy
                                },
                                origin_y,
                            );
                            painted += 1;
                        }
                        // An asymmetrical control pavilion gives the dock a
                        // recognisable arrival side instead of radial symmetry.
                        if (-13..=-4).contains(&x) && z.abs() <= 5 {
                            let shell = x == -13 || x == -4 || z.abs() == 5;
                            for dy in 1..=8 {
                                if shell || dy == 8 {
                                    let window = shell && (3..=5).contains(&dy) && z.abs() == 5;
                                    set_authored(
                                        chunk,
                                        lx,
                                        spec.center.y + dy,
                                        lz,
                                        if window {
                                            BlockType::NeonGlass
                                        } else if dy == 8 && (x + z).rem_euclid(4) == 0 {
                                            spec.accent
                                        } else {
                                            BlockType::ShipHullDark
                                        },
                                        origin_y,
                                    );
                                    painted += 1;
                                }
                            }
                        }
                        if x == -8 && z == 0 {
                            for dy in 9..=spec.height {
                                set_authored(
                                    chunk,
                                    lx,
                                    spec.center.y + dy,
                                    lz,
                                    if dy == spec.height || dy.rem_euclid(4) == 0 {
                                        spec.accent
                                    } else {
                                        BlockType::ShipHullAlloy
                                    },
                                    origin_y,
                                );
                                painted += 1;
                            }
                        }
                    }
                    AstralWaypointKind::CrystalGarden => {
                        let cluster = [
                            (0, 0, spec.height),
                            (9, 2, 11),
                            (-9, -2, 9),
                            (2, 10, 13),
                            (-2, -10, 8),
                            (15, -12, 7),
                            (-14, 13, 6),
                        ]
                        .into_iter()
                        .find(|(cx, cz, _)| (x - cx).abs() <= 1 && (z - cz).abs() <= 1);
                        if let Some((cx, cz, height)) = cluster {
                            let centre_column = x == cx && z == cz;
                            let column_height = if centre_column { height } else { height / 2 };
                            for dy in 1..=column_height {
                                set_authored(
                                    chunk,
                                    lx,
                                    spec.center.y + dy,
                                    lz,
                                    spec.accent,
                                    origin_y,
                                );
                                painted += 1;
                            }
                        }
                        let radius = (f64::from(radial_sq)).sqrt();
                        if (20.0..=22.0).contains(&radius) {
                            set_authored(
                                chunk,
                                lx,
                                spec.center.y + 1,
                                lz,
                                if (x + z).rem_euclid(7) == 0 {
                                    spec.accent
                                } else {
                                    BlockType::ZenStone
                                },
                                origin_y,
                            );
                            painted += 1;
                        }
                    }
                    AstralWaypointKind::TransitGate => {
                        let gate_x = (x.abs() - 11).abs() <= 1;
                        let pylon = gate_x && (z.abs() - 6).abs() <= 1;
                        if pylon {
                            for dy in 1..=spec.height {
                                let block = if dy.rem_euclid(4) == 0 || dy == spec.height {
                                    spec.accent
                                } else {
                                    BlockType::ShipHullDark
                                };
                                set_authored(chunk, lx, spec.center.y + dy, lz, block, origin_y);
                                painted += 1;
                            }
                        }
                        if gate_x && z.abs() <= 7 {
                            set_authored(
                                chunk,
                                lx,
                                spec.center.y + spec.height,
                                lz,
                                spec.accent,
                                origin_y,
                            );
                            painted += 1;
                        }
                        if z.abs() <= 1 && x.abs() <= spec.radius {
                            set_authored(
                                chunk,
                                lx,
                                spec.center.y + 1,
                                lz,
                                if x.rem_euclid(6) == 0 {
                                    spec.accent
                                } else {
                                    BlockType::NeonGlass
                                },
                                origin_y,
                            );
                            painted += 1;
                        }
                    }
                }
            }
        }
        painted
    }

    fn decorate_astral_waypoints(&self, chunk: &mut Chunk) {
        let pos = chunk.pos;
        self.for_each_astral_waypoint_near_chunk(pos.x, pos.z, |spec| {
            Self::paint_astral_waypoint_into_chunk(chunk, spec);
        });
    }

    fn floating_island_for_cell(
        &self,
        owner_cell_x: i32,
        owner_cell_z: i32,
    ) -> Option<FloatingIslandSpec> {
        if self.world_profile != WorldProfile::AstralFrontier {
            return None;
        }
        let gate = column_rand(self.seed ^ 0xA57A_151A, owner_cell_x, owner_cell_z);
        if gate > 0.24 {
            return None;
        }

        const MARGIN: i32 = 28;
        let span = ASTRAL_ISLAND_CELL_SIZE - MARGIN * 2;
        let offset_x = MARGIN
            + (column_rand(self.seed ^ 0xA57A_151B, owner_cell_x, owner_cell_z) * span as f64)
                as i32;
        let offset_z = MARGIN
            + (column_rand(self.seed ^ 0xA57A_151C, owner_cell_x, owner_cell_z) * span as f64)
                as i32;
        let center_x = owner_cell_x
            .saturating_mul(ASTRAL_ISLAND_CELL_SIZE)
            .saturating_add(offset_x);
        let center_z = owner_cell_z
            .saturating_mul(ASTRAL_ISLAND_CELL_SIZE)
            .saturating_add(offset_z);
        let surface = self.surface_height_at(center_x, center_z);
        if !(WATER_LEVEL + 6..=110).contains(&surface) {
            return None;
        }
        let (region, strength) = self.region(center_x as f64, center_z as f64);
        if strength < 0.22 {
            return None;
        }
        let (cap, sub, core, tip) = match region {
            Region::Plateau | Region::Highland => (
                BlockType::Grass,
                BlockType::Dirt,
                BlockType::Stone,
                BlockType::Crystal,
            ),
            Region::Canyon => (
                BlockType::RedSand,
                BlockType::RedStone,
                BlockType::RedStone,
                BlockType::MagnetiteOre,
            ),
            Region::Karst => (
                BlockType::MossStone,
                BlockType::Limestone,
                BlockType::Limestone,
                BlockType::Crystal,
            ),
            Region::AlienReef => (
                BlockType::AlienMoss,
                BlockType::BoneRock,
                BlockType::BoneRock,
                BlockType::LuminiteCrystal,
            ),
            Region::CrystalSpires => (
                BlockType::GlowSand,
                BlockType::Limestone,
                BlockType::Crystal,
                BlockType::LuminiteCrystal,
            ),
            _ => return None,
        };
        let radius_x =
            14 + (column_rand(self.seed ^ 0xA57A_151D, owner_cell_x, owner_cell_z) * 12.0) as i32;
        let radius_z =
            13 + (column_rand(self.seed ^ 0xA57A_151E, owner_cell_x, owner_cell_z) * 11.0) as i32;
        let thickness =
            10 + (column_rand(self.seed ^ 0xA57A_151F, owner_cell_x, owner_cell_z) * 7.0) as i32;
        let clearance = ASTRAL_ISLAND_CLEARANCE_MIN
            + (column_rand(self.seed ^ 0xA57A_1520, owner_cell_x, owner_cell_z)
                * f64::from(ASTRAL_ISLAND_CLEARANCE_VARIATION)) as i32;
        debug_assert!(
            (ASTRAL_ISLAND_CLEARANCE_MIN..=ASTRAL_ISLAND_CLEARANCE_MAX).contains(&clearance)
        );

        Some(FloatingIslandSpec {
            center: bevy::math::IVec3::new(center_x, surface + clearance, center_z),
            radius_x: radius_x.min(ASTRAL_ISLAND_MAX_RADIUS),
            radius_z: radius_z.min(ASTRAL_ISLAND_MAX_RADIUS),
            thickness,
            cap,
            sub,
            core,
            tip,
        })
    }

    fn authored_astral_islands(&self) -> Option<[FloatingIslandSpec; 3]> {
        let layout = self.astral_layout()?;
        let green = layout.world_from_local(IVec2::new(184, -34));
        let crystal = layout.world_from_local(IVec2::new(-46, 184));
        let mesa = layout.world_from_local(IVec2::new(238, 132));
        Some([
            FloatingIslandSpec {
                center: bevy::math::IVec3::new(
                    green.x,
                    self.surface_height_at(green.x, green.y) + 52,
                    green.y,
                ),
                radius_x: 19,
                radius_z: 16,
                thickness: 10,
                cap: BlockType::Grass,
                sub: BlockType::Dirt,
                core: BlockType::Stone,
                tip: BlockType::Crystal,
            },
            FloatingIslandSpec {
                center: bevy::math::IVec3::new(
                    crystal.x,
                    self.surface_height_at(crystal.x, crystal.y) + 61,
                    crystal.y,
                ),
                radius_x: 16,
                radius_z: 15,
                thickness: 11,
                cap: BlockType::AlienMoss,
                sub: BlockType::BoneRock,
                core: BlockType::BoneRock,
                tip: BlockType::LuminiteCrystal,
            },
            FloatingIslandSpec {
                center: bevy::math::IVec3::new(
                    mesa.x,
                    self.surface_height_at(mesa.x, mesa.y) + 56,
                    mesa.y,
                ),
                radius_x: 14,
                radius_z: 12,
                thickness: 8,
                cap: BlockType::RedSand,
                sub: BlockType::RedStone,
                core: BlockType::RedStone,
                tip: BlockType::MagnetiteOre,
            },
        ])
    }

    fn for_each_floating_island_near_chunk(
        &self,
        cx: i32,
        cz: i32,
        mut visit: impl FnMut(FloatingIslandSpec),
    ) {
        if self.world_profile != WorldProfile::AstralFrontier {
            return;
        }
        let min_x = cx.saturating_mul(CHUNK_SIZE_I);
        let min_z = cz.saturating_mul(CHUNK_SIZE_I);
        let max_x = min_x.saturating_add(CHUNK_SIZE_I - 1);
        let max_z = min_z.saturating_add(CHUNK_SIZE_I - 1);

        if let Some(layout) = self.astral_layout() {
            if Self::chunk_intersects_disc(cx, cz, layout.hub, ASTRAL_AUTHORED_ISLAND_RADIUS) {
                if let Some(authored) = self.authored_astral_islands() {
                    for spec in authored {
                        if spec.center.x + spec.radius_x < min_x
                            || spec.center.x - spec.radius_x > max_x
                            || spec.center.z + spec.radius_z < min_z
                            || spec.center.z - spec.radius_z > max_z
                        {
                            continue;
                        }
                        visit(spec);
                    }
                }
            }
        }

        let owner_min_x = min_x
            .saturating_sub(ASTRAL_ISLAND_MAX_RADIUS)
            .div_euclid(ASTRAL_ISLAND_CELL_SIZE);
        let owner_max_x = max_x
            .saturating_add(ASTRAL_ISLAND_MAX_RADIUS)
            .div_euclid(ASTRAL_ISLAND_CELL_SIZE);
        let owner_min_z = min_z
            .saturating_sub(ASTRAL_ISLAND_MAX_RADIUS)
            .div_euclid(ASTRAL_ISLAND_CELL_SIZE);
        let owner_max_z = max_z
            .saturating_add(ASTRAL_ISLAND_MAX_RADIUS)
            .div_euclid(ASTRAL_ISLAND_CELL_SIZE);

        for owner_z in owner_min_z..=owner_max_z {
            for owner_x in owner_min_x..=owner_max_x {
                let Some(spec) = self.floating_island_for_cell(owner_x, owner_z) else {
                    continue;
                };
                if spec.center.x + spec.radius_x < min_x
                    || spec.center.x - spec.radius_x > max_x
                    || spec.center.z + spec.radius_z < min_z
                    || spec.center.z - spec.radius_z > max_z
                {
                    continue;
                }
                visit(spec);
            }
        }
    }

    /// Highest authored non-height-field feature intersecting one horizontal
    /// chunk. The streamer uses this to avoid pruning a floating island as an
    /// "empty" air chunk before generation can paint it.
    pub fn decorative_top_hint_for_chunk(&self, cx: i32, cz: i32) -> Option<i32> {
        let mut top = None;
        self.for_each_floating_island_near_chunk(cx, cz, |spec| {
            top = Some(top.map_or(spec.center.y, |current: i32| current.max(spec.center.y)));
        });
        self.for_each_astral_waypoint_near_chunk(cx, cz, |spec| {
            top = Some(top.map_or(spec.top(), |current: i32| current.max(spec.top())));
        });
        if let Some(precinct_top) = self.astral_precinct_top_hint_for_chunk(cx, cz) {
            top = Some(top.map_or(precinct_top, |current: i32| current.max(precinct_top)));
        }
        top
    }

    fn decorate_floating_islands(&self, chunk: &mut Chunk) {
        let pos = chunk.pos;
        self.for_each_floating_island_near_chunk(pos.x, pos.z, |spec| {
            Self::paint_floating_island_into_chunk(chunk, spec);
        });
    }

    fn point_segment_distance(
        point_x: f64,
        point_z: f64,
        start_x: f64,
        start_z: f64,
        end_x: f64,
        end_z: f64,
    ) -> (f64, f64) {
        let vx = end_x - start_x;
        let vz = end_z - start_z;
        let length_sq = vx * vx + vz * vz;
        if length_sq <= f64::EPSILON {
            return (
                ((point_x - start_x).powi(2) + (point_z - start_z).powi(2)).sqrt(),
                0.0,
            );
        }
        let t = (((point_x - start_x) * vx + (point_z - start_z) * vz) / length_sq).clamp(0.0, 1.0);
        let nearest_x = start_x + vx * t;
        let nearest_z = start_z + vz * t;
        (
            ((point_x - nearest_x).powi(2) + (point_z - nearest_z).powi(2)).sqrt(),
            t,
        )
    }

    fn astral_precinct_top_hint_for_chunk(&self, cx: i32, cz: i32) -> Option<i32> {
        let layout = self.astral_layout()?;
        if !Self::chunk_intersects_disc(cx, cz, layout.hub, ASTRAL_PRECINCT_STRUCTURE_RADIUS) {
            return None;
        }
        let hub_surface = self.surface_height_at(layout.hub.x, layout.hub.y);
        let landing = layout.landing();
        let landing_surface = self.surface_height_at(landing.x, landing.y);
        let gateway = layout.world_from_local(IVec2::new(-25, 4));
        let gateway_surface = self.surface_height_at(gateway.x, gateway.y);
        let observatory = layout.observatory();
        let observatory_surface = self.surface_height_at(observatory.x, observatory.y);
        let mut top = None;

        let origin_x = cx.saturating_mul(CHUNK_SIZE_I);
        let origin_z = cz.saturating_mul(CHUNK_SIZE_I);
        for lz in 0..CHUNK_SIZE_I {
            for lx in 0..CHUNK_SIZE_I {
                let local = layout.local_from_world(IVec2::new(origin_x + lx, origin_z + lz));
                let x = local.x as f64;
                let z = local.y as f64;
                if local.x.abs().max(local.y.abs()) <= 25 {
                    top = Some(top.map_or(hub_surface + 60, |v: i32| v.max(hub_surface + 60)));
                }
                if ((x + 124.0).powi(2) + (z - 24.0).powi(2)).sqrt() <= 31.0 {
                    top =
                        Some(top.map_or(landing_surface + 9, |v: i32| v.max(landing_surface + 9)));
                }
                let (bridge_distance, t) =
                    Self::point_segment_distance(x, z, -124.0, 24.0, -25.0, 4.0);
                if bridge_distance <= 4.0 {
                    let bridge_y = (landing_surface as f64
                        + 3.0
                        + (gateway_surface - landing_surface) as f64 * t)
                        .round() as i32;
                    top = Some(top.map_or(bridge_y + 3, |v: i32| v.max(bridge_y + 3)));
                }
                if ((x - 142.0).powi(2) + (z - 112.0).powi(2)).sqrt() <= 22.0 {
                    top = Some(top.map_or(observatory_surface + 24, |v: i32| {
                        v.max(observatory_surface + 24)
                    }));
                }
            }
        }
        top
    }

    fn decorate_astral_precinct(&self, chunk: &mut Chunk) {
        let Some(layout) = self.astral_layout() else {
            return;
        };
        if !Self::chunk_intersects_disc(
            chunk.pos.x,
            chunk.pos.z,
            layout.hub,
            ASTRAL_PRECINCT_STRUCTURE_RADIUS,
        ) {
            return;
        }
        let origin_x = chunk.pos.x * CHUNK_SIZE_I;
        let origin_y = chunk.pos.y * CHUNK_SIZE_I;
        let origin_z = chunk.pos.z * CHUNK_SIZE_I;
        let hub_surface = self.surface_height_at(layout.hub.x, layout.hub.y);
        let hub_base = hub_surface + 1;
        let landing = layout.landing();
        let landing_surface = self.surface_height_at(landing.x, landing.y);
        let landing_y = landing_surface + 1;
        let gateway = layout.world_from_local(IVec2::new(-25, 4));
        let gateway_surface = self.surface_height_at(gateway.x, gateway.y);
        let observatory = layout.observatory();
        let observatory_surface = self.surface_height_at(observatory.x, observatory.y);

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = origin_x + lx as i32;
                let wz = origin_z + lz as i32;
                let local = layout.local_from_world(IVec2::new(wx, wz));
                let x = local.x as f64;
                let z = local.y as f64;
                let surface = self.surface_height_at(wx, wz);

                // Western landing pad: an actual landable disc with readable
                // cyan outline, dark centre, spokes and four approach beacons.
                let pad_dx = local.x + 124;
                let pad_dz = local.y - 24;
                let pad_radius = ((pad_dx * pad_dx + pad_dz * pad_dz) as f64).sqrt();
                if pad_radius <= 30.0 {
                    let block = if pad_radius >= 26.0 {
                        if (pad_dx + pad_dz).rem_euclid(4) == 0 {
                            BlockType::NeonAmber
                        } else {
                            BlockType::NeonCyan
                        }
                    } else if pad_dx.abs() <= 1 || pad_dz.abs() <= 1 {
                        BlockType::ShipHullAlloy
                    } else if pad_radius <= 8.0 {
                        BlockType::NeonGlass
                    } else {
                        BlockType::ShipHullDark
                    };
                    set_authored(chunk, lx, landing_y, lz, block, origin_y);

                    let beacon = ((pad_dx.abs() - 23).abs() <= 1 && pad_dz.abs() <= 1)
                        || ((pad_dz.abs() - 23).abs() <= 1 && pad_dx.abs() <= 1);
                    if beacon {
                        for dy in 1..=7 {
                            let mast = if dy == 7 || dy == 4 {
                                BlockType::NeonCyan
                            } else {
                                BlockType::ShipHullAlloy
                            };
                            set_authored(chunk, lx, landing_y + dy, lz, mast, origin_y);
                        }
                    }
                }

                // Elevated transit spine from pad to citadel. A repeating
                // support cadence gives speed/scale cues while the continuous
                // deck remains walkable and fly-under-clear across the canyon.
                let (bridge_distance, bridge_t) =
                    Self::point_segment_distance(x, z, -124.0, 24.0, -25.0, 4.0);
                if bridge_distance <= 3.5 {
                    let bridge_y = (landing_surface as f64
                        + 3.0
                        + (gateway_surface - landing_surface) as f64 * bridge_t)
                        .round() as i32;
                    let edge = bridge_distance >= 2.35;
                    let deck = if edge {
                        BlockType::NeonCyan
                    } else if ((bridge_t * 96.0).round() as i32).rem_euclid(8) == 0 {
                        BlockType::ShipHullAlloy
                    } else {
                        BlockType::ShipHullDark
                    };
                    set_authored(chunk, lx, bridge_y, lz, deck, origin_y);
                    if edge {
                        set_authored(
                            chunk,
                            lx,
                            bridge_y + 1,
                            lz,
                            BlockType::ShipHullAlloy,
                            origin_y,
                        );
                    }
                    let progress = (bridge_t * 101.0).round() as i32;
                    if progress.rem_euclid(22) <= 1 && edge {
                        for wy in (surface + 1)..bridge_y {
                            set_authored(chunk, lx, wy, lz, BlockType::ShipHullDark, origin_y);
                        }
                    }
                }

                // Mountain-integrated citadel. Constant-height platform and
                // supports meet the procedural summit; the shell remains
                // hollow between structural floors so it is an editable place,
                // not merely a distant solid prop.
                let hx = local.x;
                let hz = local.y;
                let cheb = hx.abs().max(hz.abs());
                if cheb <= 24 {
                    let radial = (((hx * hx + hz * hz) as f64).sqrt()).round() as i32;
                    if radial <= 24 {
                        let platform = if radial >= 21 {
                            BlockType::NeonCyan
                        } else if hx.rem_euclid(7) == 0 || hz.rem_euclid(7) == 0 {
                            BlockType::ShipHullAlloy
                        } else {
                            BlockType::ZenStone
                        };
                        set_authored(chunk, lx, hub_base, lz, platform, origin_y);
                    }
                    let corner_support = (hx.abs() - 18).abs() <= 1 && (hz.abs() - 18).abs() <= 1;
                    if corner_support {
                        for wy in (surface + 1)..hub_base {
                            set_authored(chunk, lx, wy, lz, BlockType::ShipHullAlloy, origin_y);
                        }
                    }

                    let tower_cheb = hx.abs().max(hz.abs());
                    if tower_cheb <= 10 {
                        for dy in 1_i32..=34 {
                            let shell = tower_cheb >= 8;
                            let floor = dy.rem_euclid(7) == 0;
                            if !shell && !floor {
                                continue;
                            }
                            let window = shell
                                && matches!(dy.rem_euclid(6), 2 | 3)
                                && (hx.abs() <= 2 || hz.abs() <= 2);
                            let block = if window {
                                if dy.rem_euclid(12) < 6 {
                                    BlockType::NeonCyan
                                } else {
                                    BlockType::NeonMagenta
                                }
                            } else if floor {
                                BlockType::ShipHullAlloy
                            } else {
                                BlockType::Limestone
                            };
                            set_authored(chunk, lx, hub_base + dy, lz, block, origin_y);
                        }
                    }

                    // Four lower wings create an occupiable civic base rather
                    // than balancing a narrow tower on an empty summit.
                    let north_south_wing = hx.abs() <= 5 && hz.abs() <= 18;
                    let east_west_wing = hz.abs() <= 5 && hx.abs() <= 18;
                    let wing = north_south_wing || east_west_wing;
                    if wing && tower_cheb > 10 {
                        for dy in 1_i32..=15 {
                            let boundary = (north_south_wing && (hx.abs() >= 4 || hz.abs() >= 17))
                                || (east_west_wing && (hz.abs() >= 4 || hx.abs() >= 17));
                            let floor = matches!(dy, 1 | 8 | 15);
                            if !boundary && !floor {
                                continue;
                            }
                            let window = boundary
                                && matches!(dy, 4 | 5 | 11 | 12)
                                && (hx.abs() <= 1 || hz.abs() <= 1);
                            let block = if window {
                                if (hx + hz).rem_euclid(2) == 0 {
                                    BlockType::NeonCyan
                                } else {
                                    BlockType::NeonGlass
                                }
                            } else if floor {
                                BlockType::ShipHullAlloy
                            } else {
                                BlockType::Limestone
                            };
                            set_authored(chunk, lx, hub_base + dy, lz, block, origin_y);
                        }
                    }

                    if cheb <= 7 {
                        for dy in 35..=58 {
                            let crown_radius = (7 - (dy - 35) / 4).max(1);
                            if cheb > crown_radius {
                                continue;
                            }
                            let edge = cheb == crown_radius;
                            let block = if dy == 58 {
                                BlockType::LuminiteCrystal
                            } else if edge && dy.rem_euclid(5) == 0 {
                                BlockType::NeonCyan
                            } else {
                                BlockType::ShipHullAlloy
                            };
                            set_authored(chunk, lx, hub_base + dy, lz, block, origin_y);
                        }
                    }

                    let side_spire =
                        [(-18, 0), (18, 0), (0, -18), (0, 18)]
                            .into_iter()
                            .find_map(|(sx, sz)| {
                                let d = (hx - sx).abs().max((hz - sz).abs());
                                (d <= 2).then_some((d, sx, sz))
                            });
                    if let Some((spire_cheb, sx, sz)) = side_spire {
                        let spire_height = 27 + ((sx.abs() + sz.abs()) / 6);
                        for dy in 1..=spire_height {
                            if spire_cheb == 2 && dy > spire_height - 5 {
                                continue;
                            }
                            let block = if dy == spire_height {
                                BlockType::NeonMagenta
                            } else if dy.rem_euclid(6) == 0 {
                                BlockType::NeonCyan
                            } else {
                                BlockType::Limestone
                            };
                            set_authored(chunk, lx, hub_base + dy, lz, block, origin_y);
                        }
                    }
                }

                // Eastern observatory makes the far shelf a destination and
                // counterbalances the citadel without competing with it.
                let obs_dx = wx - observatory.x;
                let obs_dz = wz - observatory.y;
                let obs_radius = ((obs_dx * obs_dx + obs_dz * obs_dz) as f64).sqrt();
                let obs_y = observatory_surface + 1;
                if obs_radius <= 20.0 {
                    let floor = if obs_radius >= 17.0 {
                        BlockType::NeonAmber
                    } else {
                        BlockType::ShipHullDark
                    };
                    set_authored(chunk, lx, obs_y, lz, floor, origin_y);
                    if obs_dx.abs().max(obs_dz.abs()) <= 4 {
                        for dy in 1..=22 {
                            let taper = (4 - dy / 6).max(1);
                            if obs_dx.abs().max(obs_dz.abs()) > taper {
                                continue;
                            }
                            let block = if dy == 22 {
                                BlockType::LuminiteCrystal
                            } else if dy.rem_euclid(5) == 0 {
                                BlockType::NeonCyan
                            } else {
                                BlockType::ShipHullAlloy
                            };
                            set_authored(chunk, lx, obs_y + dy, lz, block, origin_y);
                        }
                    }
                }
            }
        }
    }

    fn paint_floating_island_into_chunk(chunk: &mut Chunk, spec: FloatingIslandSpec) -> usize {
        let min_x = chunk.pos.x * CHUNK_SIZE_I;
        let min_y = chunk.pos.y * CHUNK_SIZE_I;
        let min_z = chunk.pos.z * CHUNK_SIZE_I;
        let max_x = min_x + CHUNK_SIZE_I - 1;
        let max_y = min_y + CHUNK_SIZE_I - 1;
        let max_z = min_z + CHUNK_SIZE_I - 1;
        let start_x = (spec.center.x - spec.radius_x).max(min_x);
        let end_x = (spec.center.x + spec.radius_x).min(max_x);
        let start_z = (spec.center.z - spec.radius_z).max(min_z);
        let end_z = (spec.center.z + spec.radius_z).min(max_z);
        let rx2 = i64::from(spec.radius_x) * i64::from(spec.radius_x);
        let rz2 = i64::from(spec.radius_z) * i64::from(spec.radius_z);
        let limit = rx2 * rz2;
        let mut painted = 0usize;

        for wz in start_z..=end_z {
            for wx in start_x..=end_x {
                let dx = i64::from(wx - spec.center.x);
                let dz = i64::from(wz - spec.center.z);
                let score = dx * dx * rz2 + dz * dz * rx2;
                if score > limit {
                    continue;
                }
                let top_drop = ((score * 2) / limit.max(1)) as i32;
                let top_y = spec.center.y - top_drop;
                let body_depth =
                    1 + ((i64::from(spec.thickness) * (limit - score)) / limit.max(1)) as i32;
                let body_bottom = top_y - body_depth + 1;
                let tip_length =
                    if (wx - spec.center.x).abs() <= 1 && (wz - spec.center.z).abs() <= 1 {
                        4
                    } else {
                        0
                    };
                let bottom_y = body_bottom - tip_length;

                for wy in bottom_y.max(min_y)..=top_y.min(max_y) {
                    let lx = (wx - min_x) as usize;
                    let ly = (wy - min_y) as usize;
                    let lz = (wz - min_z) as usize;
                    if chunk.get(lx, ly, lz) != AIR {
                        continue;
                    }
                    let block = if wy < body_bottom {
                        spec.tip
                    } else if wy == top_y {
                        spec.cap
                    } else if wy >= top_y - 2 {
                        spec.sub
                    } else {
                        spec.core
                    };
                    chunk.set(lx, ly, lz, block.into());
                    painted += 1;
                }
            }
        }
        painted
    }

    /// Paint deterministic tree slices. Roots are owned by stable horizontal
    /// cells and replayed into every intersecting chunk; vertical slices also
    /// repeat, so neither crowns nor trunks are clipped at a 16-block seam.
    fn decorate(&self, chunk: &mut Chunk) {
        use crate::settings::SceneryQuality;

        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // Traversable macro geometry is a world-profile feature, not a
        // quality-tier decoration. It must therefore remain identical when a
        // user lowers foliage density for performance.
        if self.world_profile == WorldProfile::AstralFrontier {
            self.decorate_floating_islands(chunk);
        }

        let (candidate_budget, chance_scale) = match self.scenery_quality {
            SceneryQuality::Off => {
                if self.world_profile == WorldProfile::AstralFrontier {
                    self.decorate_astral_routes(chunk);
                    self.decorate_astral_waypoints(chunk);
                    self.decorate_astral_precinct(chunk);
                }
                return;
            }
            SceneryQuality::Lean => (2usize, 10.0),
            // More bounded attempts with a lower individual probability let a
            // strong habitat grow a cohort while weak habitat becomes a real
            // clearing. Expected average density stays in the same order, but
            // its spatial distribution no longer converges to one tree/chunk.
            SceneryQuality::Balanced => (3usize, 5.0),
            SceneryQuality::Lush => (4usize, 3.0),
        };

        // Tree roots belong to jittered 16x16 ownership cells, but crowns may
        // cross a chunk seam. Every target chunk replays the bounded roots
        // from its 3x3 owner halo and clips writes to itself. In the previous
        // chunk-local layout a Lush tree's seven-voxel safety margin left only
        // x/z=7 or 8 as legal roots, visibly aligning forests to the chunk
        // grid. World-space ownership removes that grid without adding shared
        // mutable state or generation-order dependence.
        let target_min_x = cx * CHUNK_SIZE_I;
        let target_min_z = cz * CHUNK_SIZE_I;
        let target_max_x = target_min_x + CHUNK_SIZE_I - 1;
        let target_max_z = target_min_z + CHUNK_SIZE_I - 1;

        for owner_cz in (cz - 1)..=(cz + 1) {
            for owner_cx in (cx - 1)..=(cx + 1) {
                for candidate in 0..candidate_budget {
                    let root = tree_root_in_owner_cell(self.seed, owner_cx, owner_cz, candidate);
                    let owner_lx = root.x;
                    let owner_lz = root.y;
                    let wx = owner_cx * CHUNK_SIZE_I + owner_lx;
                    let wz = owner_cz * CHUNK_SIZE_I + owner_lz;
                    let (surface, cont) = self.surface_height(wx as f64, wz as f64);
                    let biome = self.biome(wx as f64, wz as f64, surface, cont);

                    // Trees can't grow on cliffs / steep slopes. Same slope
                    // test as the main generator â€” if any cardinal neighbour
                    // is >= 3 blocks lower/higher, skip.
                    let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
                    let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
                    let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
                    let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
                    let slope = (surface - hn)
                        .abs()
                        .max((surface - hs).abs())
                        .max((surface - he).abs())
                        .max((surface - hw).abs());
                    if slope >= 3 {
                        continue;
                    }

                    // Density scales with SceneryQuality so low-end PCs can
                    // keep foliage sparse while cinematic worlds get larger
                    // bonsai/blossom silhouettes.
                    let density = self.tree_density_for_biome(biome);
                    if density == 0.0 {
                        continue;
                    }

                    let gate_roll = column_rand(
                        self.seed ^ 0x71EE_3003_u32.wrapping_add(candidate as u32 * 997),
                        wx,
                        wz,
                    );
                    let habitat_multiplier = self.tree_habitat_multiplier(biome, wx, wz, surface);
                    if gate_roll > (density * chance_scale * habitat_multiplier).min(0.98) {
                        continue;
                    }

                    let style_roll = column_rand(self.seed ^ 0x71EE_4004, wx, wz);
                    let Some((baseline_profile, baseline_leaf_kind)) =
                        self.tree_profile(biome, style_roll)
                    else {
                        continue;
                    };
                    let profile = self.adapt_tree_profile_to_site(
                        baseline_profile,
                        biome,
                        wx,
                        wz,
                        surface,
                        style_roll,
                    );
                    let extent = profile.max_extent;
                    if wx + extent < target_min_x
                        || wx - extent > target_max_x
                        || wz + extent < target_min_z
                        || wz - extent > target_max_z
                    {
                        continue;
                    }
                    let leaf_kind = if profile.silhouette == TreeSilhouette::Conifer {
                        baseline_leaf_kind
                    } else {
                        self.tree_leaf_for_site(biome, wx, wz, surface)
                    };
                    let base_y = surface + 1;
                    let tree_top = base_y + profile.total_height() - 1;
                    if tree_top < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    self.try_place_bonsai_tree(
                        chunk,
                        wx - target_min_x,
                        wz - target_min_z,
                        base_y,
                        origin_y,
                        profile,
                        leaf_kind,
                    );
                }
            }
        }

        // The second vegetation layer is intentionally patch-based rather
        // than random one-block scatter. Forest floors read as habitats with
        // openings and edges, while every placed cell stays non-solid and
        // follows its own local ground height.
        self.decorate_understory(chunk);

        if self.world_profile == WorldProfile::AstralFrontier {
            // These authored systems predate world profiles but were never
            // reachable from normal generation. Astral Frontier activates
            // their grouped landmarks deliberately while leaving Natural
            // terrain untouched. Large clusters and rare silhouettes carry
            // the composition; the old glitter/speck pass remains disabled so
            // emissive detail does not become uniform visual noise.
            self.decorate_props(chunk);
            self.decorate_structures(chunk);
            self.decorate_astral_routes(chunk);
            self.decorate_astral_waypoints(chunk);
            // The hero precinct is applied last and owns its footprint. This
            // prevents a random tree/prop from punching holes through landing
            // pads, transit rails or the citadel shell.
            self.decorate_astral_precinct(chunk);
        }

        // Artificial ruins and unsupported one-block debris remain excluded
        // from natural generation: they obscure terrain silhouettes without
        // adding ecological structure or useful gameplay.
    }

    fn decorate_understory(&self, chunk: &mut Chunk) {
        use crate::settings::SceneryQuality;

        let candidate_budget = match self.scenery_quality {
            SceneryQuality::Off | SceneryQuality::Lean => return,
            SceneryQuality::Balanced => 1usize,
            SceneryQuality::Lush => 3usize,
        };
        let quality_scale = if self.scenery_quality == SceneryQuality::Lush {
            1.0
        } else {
            0.58
        };
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;
        const PATCH_MARGIN: usize = 3;
        let interior = CHUNK_SIZE - PATCH_MARGIN * 2;

        for candidate in 0..candidate_budget {
            let salt = candidate as u32 * 1_009;
            let x_roll = column_rand(self.seed ^ 0x5A17_1101_u32.wrapping_add(salt), cx, cz);
            let z_roll = column_rand(self.seed ^ 0x5A17_2202_u32.wrapping_add(salt), cx, cz);
            let lx = PATCH_MARGIN + ((x_roll * interior as f64) as usize).min(interior - 1);
            let lz = PATCH_MARGIN + ((z_roll * interior as f64) as usize).min(interior - 1);
            let wx = cx * CHUNK_SIZE_I + lx as i32;
            let wz = cz * CHUNK_SIZE_I + lz as i32;
            let (surface, cont) = self.surface_height(wx as f64, wz as f64);
            let biome = self.biome(wx as f64, wz as f64, surface, cont);
            let style_roll = column_rand(self.seed ^ 0x5A17_3303_u32.wrapping_add(salt), wx, wz);
            let environment = self.environment_sample_for_surface(wx, wz, surface);
            let hydro = self.hydrographic_field_for_surface(wx as f64, wz as f64, surface as f64);
            let moisture = environment.soil_moisture as f64;
            let riparian_habitat = surface > WATER_LEVEL + 1
                && hydro.corridor > 0.14
                && environment.soil_moisture > 0.66;
            let Some((leaf_kind, mut habitat_density)) = understory_profile(biome, style_roll)
                .or_else(|| {
                    (riparian_habitat && biome == Biome::Tundra)
                        .then_some((BlockType::Leaves, 0.74))
                })
            else {
                continue;
            };
            if riparian_habitat {
                habitat_density = habitat_density.max(0.86);
            }

            // Broad moisture and an 8x8-column cohort gate make vegetation
            // arrive in readable colonies, then leave genuine clearings.
            let cohort = column_rand(self.seed ^ 0x5A17_4404, wx.div_euclid(8), wz.div_euclid(8));
            let local_gate = column_rand(self.seed ^ 0x5A17_5505_u32.wrapping_add(salt), wx, wz);
            // A linear floor put one compact cross-shaped shrub into most
            // eligible chunks. The eased, squared response concentrates the
            // same bounded candidate pass into moist thickets and returns
            // open ground between them.
            let mut colony_gate = understory_colony_gate(moisture, cohort);
            if riparian_habitat {
                // The corridor is continuous in world space, so this floor
                // creates a gallery edge rather than one shrub per chunk.
                colony_gate = colony_gate.max(smoothstep(0.14, 0.34, hydro.corridor) * 0.78);
            }
            if local_gate > habitat_density * quality_scale * colony_gate {
                continue;
            }

            let slope = cardinal_surface_slope(self, wx, wz, surface);
            if slope > 1 {
                continue;
            }
            let radius = if riparian_habitat {
                2
            } else {
                match self.scenery_quality {
                    SceneryQuality::Lush if colony_gate > 0.35 || style_roll > 0.72 => 2,
                    SceneryQuality::Balanced if colony_gate > 0.58 && style_roll > 0.35 => 2,
                    _ => 1,
                }
            };
            self.try_place_understory_patch(
                chunk,
                IVec2::new(lx as i32, lz as i32),
                origin_y,
                radius,
                leaf_kind,
                salt,
            );
        }
    }

    fn try_place_understory_patch(
        &self,
        chunk: &mut Chunk,
        center: IVec2,
        origin_y: i32,
        radius: i32,
        leaf_kind: BlockType,
        salt: u32,
    ) -> usize {
        let mut placed = 0usize;
        let mut centre_base_placed = false;
        let centre_wx = chunk.pos.x * CHUNK_SIZE_I + center.x;
        let centre_wz = chunk.pos.z * CHUNK_SIZE_I + center.y;
        let compact_directions = [(1, 0), (0, 1), (-1, 0), (0, -1)];
        let compact_lobe = compact_directions[((column_rand(
            self.seed ^ 0x5A17_6C0B_u32.wrapping_add(salt),
            centre_wx,
            centre_wz,
        ) * compact_directions.len() as f64)
            as usize)
            .min(compact_directions.len() - 1)];

        for dz in -radius..=radius {
            for dx in -radius..=radius {
                let distance_sq = dx * dx + dz * dz;
                if distance_sq > radius * radius {
                    continue;
                }
                let wx = centre_wx + dx;
                let wz = centre_wz + dz;
                let silhouette =
                    column_rand(self.seed ^ 0x5A17_6606_u32.wrapping_add(salt), wx, wz);
                let compact_spine = radius <= 1 && (dx, dz) == compact_lobe;
                let connected_core = radius > 1 && distance_sq <= 1;
                let edge_cut = if radius <= 1 { 1.0 } else { 0.46 };
                if (dx != 0 || dz != 0)
                    && !compact_spine
                    && !connected_core
                    && silhouette < edge_cut
                {
                    continue;
                }
                let (surface, _) = self.surface_height(wx as f64, wz as f64);
                let local_x = center.x + dx;
                let local_z = center.y + dz;
                if !(0..CHUNK_SIZE_I).contains(&local_x) || !(0..CHUNK_SIZE_I).contains(&local_z) {
                    continue;
                }
                let ground_y = surface - origin_y;
                let foliage_y = ground_y + 1;
                if !(0..CHUNK_SIZE_I).contains(&ground_y) || !(0..CHUNK_SIZE_I).contains(&foliage_y)
                {
                    continue;
                }
                let lx = local_x as usize;
                let lz = local_z as usize;
                if !BlockType::from_voxel(chunk.get(lx, ground_y as usize, lz)).is_solid()
                    || chunk.get(lx, foliage_y as usize, lz) != AIR
                {
                    continue;
                }
                chunk.set(lx, foliage_y as usize, lz, leaf_kind.into());
                placed += 1;
                if dx == 0 && dz == 0 {
                    centre_base_placed = true;
                }
            }
        }

        // Build a second crown on every successful colony. Compact shrubs use
        // one central shoot above an asymmetric two-cell footprint; repeating
        // the side lobe on the upper layer made them read as little green
        // walls. Broad colonies may grow sparse upper lobes; every raised cell
        // remains supported by foliage below.
        if centre_base_placed {
            for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                if radius <= 1 && (dx != 0 || dz != 0) {
                    continue;
                }
                let local_x = center.x + dx;
                let local_z = center.y + dz;
                if !(0..CHUNK_SIZE_I).contains(&local_x) || !(0..CHUNK_SIZE_I).contains(&local_z) {
                    continue;
                }
                let wx = centre_wx + dx;
                let wz = centre_wz + dz;
                let lobe_roll = column_rand(self.seed ^ 0x5A17_7707_u32.wrapping_add(salt), wx, wz);
                if (dx != 0 || dz != 0) && lobe_roll < 0.56 {
                    continue;
                }
                let (surface, _) = self.surface_height(wx as f64, wz as f64);
                let raised_y = surface + 2 - origin_y;
                let below_y = raised_y - 1;
                if !(0..CHUNK_SIZE_I).contains(&raised_y) || !(0..CHUNK_SIZE_I).contains(&below_y) {
                    continue;
                }
                let lx = local_x as usize;
                let lz = local_z as usize;
                if chunk.get(lx, below_y as usize, lz) == Voxel::from(leaf_kind)
                    && chunk.get(lx, raised_y as usize, lz) == AIR
                {
                    chunk.set(lx, raised_y as usize, lz, leaf_kind.into());
                    placed += 1;
                }
            }
        }

        placed
    }

    fn try_place_bonsai_tree(
        &self,
        chunk: &mut Chunk,
        lx: i32,
        lz: i32,
        base_y: i32,
        origin_y: i32,
        profile: TreeProfile,
        leaf_kind: BlockType,
    ) -> bool {
        let wx = chunk.pos.x * CHUNK_SIZE_I + lx;
        let wz = chunk.pos.z * CHUNK_SIZE_I + lz;
        let lean_roll = column_rand(self.seed ^ 0xB05A_2002, wx, wz);
        let local_ground_y = base_y - 1 - origin_y;
        let root_inside_target = (0..CHUNK_SIZE_I).contains(&lx)
            && (0..CHUNK_SIZE_I).contains(&lz)
            && (0..CHUNK_SIZE_I).contains(&local_ground_y);
        if root_inside_target
            && !BlockType::from_voxel(chunk.get(lx as usize, local_ground_y as usize, lz as usize))
                .is_solid()
        {
            return false;
        }

        let (lean_x, lean_z) =
            if profile.max_extent >= 5 && profile.silhouette != TreeSilhouette::Conifer {
                match (lean_roll * 4.0) as i32 {
                    0 => (-1, 0),
                    1 => (1, 0),
                    2 => (0, -1),
                    _ => (0, 1),
                }
            } else {
                (0, 0)
            };

        let trunk_offset = |dy: i32| {
            if dy >= profile.trunk_height / 2 {
                (lean_x, lean_z)
            } else {
                (0, 0)
            }
        };

        for dy in 0..profile.trunk_height {
            let (ox, oz) = trunk_offset(dy);
            if dy == profile.trunk_height / 2 && (lean_x != 0 || lean_z != 0) {
                set_tree_wood(chunk, lx, base_y + dy, lz, origin_y);
            }
            set_tree_wood(chunk, lx + ox, base_y + dy, lz + oz, origin_y);
        }

        if profile.silhouette == TreeSilhouette::Conifer {
            self.place_conifer_crown(chunk, lx, lz, base_y, origin_y, profile, leaf_kind);
            return true;
        }

        let rotation = if profile.silhouette == TreeSilhouette::Riparian {
            cardinal_direction_index(self.environment_sample_at(wx, wz).flow_direction)
        } else {
            (column_rand(self.seed ^ 0xB05A_3003, wx, wz) * 4.0) as usize
        };
        let directions = [(1, 0), (0, 1), (-1, 0), (0, -1)];
        let lower_dy = match profile.silhouette {
            TreeSilhouette::Crowned => (profile.trunk_height * 2 / 3).max(3),
            TreeSilhouette::Conifer
            | TreeSilhouette::Layered
            | TreeSilhouette::Windswept
            | TreeSilhouette::Riparian => (profile.trunk_height / 2).max(3),
        };
        let branch_span = (profile.trunk_height - lower_dy - 2).max(1);
        let mut crowns = [(0i32, 0i32, 0i32, 0i32); 6];

        for tier in 0..profile.tiers {
            let tier_divisor = (profile.tiers - 1).max(1) as i32;
            let branch_dy = lower_dy + tier as i32 * branch_span / tier_divisor;
            let (trunk_ox, trunk_oz) = trunk_offset(branch_dy);
            let direction_index = match profile.silhouette {
                TreeSilhouette::Conifer | TreeSilhouette::Layered => tier + rotation,
                TreeSilhouette::Windswept => {
                    let side_step = match tier % 4 {
                        1 => 1,
                        3 => directions.len() - 1,
                        _ => 0,
                    };
                    rotation + side_step
                }
                TreeSilhouette::Crowned => tier * 3 + rotation,
                TreeSilhouette::Riparian => tier + rotation,
            } % directions.len();
            let (dir_x, dir_z) = directions[direction_index];
            let reach = match profile.silhouette {
                TreeSilhouette::Conifer | TreeSilhouette::Layered => {
                    (profile.branch_reach - tier as i32 / 3).max(1)
                }
                TreeSilhouette::Windswept => (profile.branch_reach - tier as i32 / 4).max(1),
                TreeSilhouette::Crowned => (profile.branch_reach - tier as i32 / 3).max(1),
                TreeSilhouette::Riparian => (profile.branch_reach - tier as i32 / 4).max(2),
            };
            let branch_y = base_y + branch_dy;

            for step in 0..=reach {
                set_tree_wood(
                    chunk,
                    lx + trunk_ox + dir_x * step,
                    branch_y,
                    lz + trunk_oz + dir_z * step,
                    origin_y,
                );
            }
            let upturn = match profile.silhouette {
                TreeSilhouette::Crowned | TreeSilhouette::Riparian => 1,
                TreeSilhouette::Conifer | TreeSilhouette::Layered | TreeSilhouette::Windswept => {
                    (tier % 2 == 0) as i32
                }
            };
            let crown_x = lx + trunk_ox + dir_x * reach;
            let crown_z = lz + trunk_oz + dir_z * reach;
            if upturn == 1 {
                set_tree_wood(chunk, crown_x, branch_y + 1, crown_z, origin_y);
            }
            crowns[tier] = (crown_x, branch_y + upturn, crown_z, profile.canopy_radius);
        }

        let (top_ox, top_oz) = trunk_offset(profile.trunk_height - 1);
        let top_x = lx + top_ox;
        let top_z = lz + top_oz;
        let trunk_top_y = base_y + profile.trunk_height - 1;
        for lift in 1..=profile.crown_lift {
            set_tree_wood(chunk, top_x, trunk_top_y + lift, top_z, origin_y);
        }

        let pad_layers = if profile.canopy_radius >= 2 { 2 } else { 1 };
        for (tier, &(crown_x, crown_y, crown_z, radius)) in
            crowns.iter().take(profile.tiers).enumerate()
        {
            // Flowering trees retain a green structural under-crown. This
            // reads as blossoms carried by a living tree instead of a solid
            // pink sculpture, and guarantees mixed canopy depth at a glance.
            let pad_leaf_kind = if leaf_kind == BlockType::BlossomLeaves
                && (tier == 0
                    || column_rand(
                        self.seed ^ 0xB05A_5105_u32.wrapping_add(tier as u32),
                        wx,
                        wz,
                    ) < 0.18)
            {
                BlockType::Leaves
            } else {
                leaf_kind
            };
            if matches!(
                profile.silhouette,
                TreeSilhouette::Crowned | TreeSilhouette::Riparian
            ) {
                let cloud_radius = (radius - 1).max(1);
                place_leaf_cloud(
                    chunk,
                    crown_x,
                    crown_y,
                    crown_z,
                    cloud_radius,
                    if cloud_radius >= 3 { 2 } else { 1 },
                    pad_leaf_kind,
                    origin_y,
                    self.seed ^ 0xB05A_6106_u32.wrapping_add(tier as u32),
                );
                if profile.silhouette == TreeSilhouette::Riparian {
                    self.place_riparian_tendrils(
                        chunk, crown_x, crown_y, crown_z, base_y, origin_y, leaf_kind, tier,
                        rotation,
                    );
                }
            } else {
                place_leaf_pad(
                    chunk,
                    crown_x,
                    crown_y,
                    crown_z,
                    radius,
                    pad_layers,
                    pad_leaf_kind,
                    origin_y,
                    self.seed ^ 0xB05A_6106_u32.wrapping_add(tier as u32),
                );
            }
        }
        // The branch-end pads already define the broad crown. Keeping the
        // terminal pad one step tighter creates a readable domed outline and
        // avoids spending dozens of hidden/overlapping voxels at the centre.
        if matches!(
            profile.silhouette,
            TreeSilhouette::Crowned | TreeSilhouette::Riparian
        ) {
            place_leaf_cloud(
                chunk,
                top_x,
                trunk_top_y + profile.crown_lift,
                top_z,
                profile.canopy_radius,
                if profile.canopy_radius >= 4 { 3 } else { 2 },
                leaf_kind,
                origin_y,
                self.seed ^ 0xB05A_7107,
            );
            if profile.silhouette == TreeSilhouette::Riparian {
                self.place_riparian_tendrils(
                    chunk,
                    top_x,
                    trunk_top_y + profile.crown_lift,
                    top_z,
                    base_y,
                    origin_y,
                    leaf_kind,
                    profile.tiers,
                    rotation,
                );
            }
        } else {
            let top_radius = (profile.canopy_radius - 1).max(1);
            place_leaf_pad(
                chunk,
                top_x,
                trunk_top_y + profile.crown_lift,
                top_z,
                top_radius,
                2,
                leaf_kind,
                origin_y,
                self.seed ^ 0xB05A_7107,
            );
        }

        true
    }

    #[allow(clippy::too_many_arguments)]
    fn place_riparian_tendrils(
        &self,
        chunk: &mut Chunk,
        crown_x: i32,
        crown_y: i32,
        crown_z: i32,
        base_y: i32,
        origin_y: i32,
        leaf_kind: BlockType,
        tier: usize,
        rotation: usize,
    ) {
        let directions = [(1, 0), (0, 1), (-1, 0), (0, -1)];
        let branch_direction = directions[(tier + rotation) % directions.len()];
        let side = (-branch_direction.1, branch_direction.0);

        // Three fringes per lobe are enough to create a readable willow-like
        // outline. Every side fringe is first attached horizontally to the
        // woody crown centre, then grows downward, preserving one editable
        // six-neighbour object without floating decorative cells.
        for (fringe, (ox, oz)) in [(0, 0), side, (-side.0, -side.1)].into_iter().enumerate() {
            if ox != 0 || oz != 0 {
                set_tree_leaf(
                    chunk,
                    crown_x + ox,
                    crown_y,
                    crown_z + oz,
                    leaf_kind,
                    origin_y,
                );
            }
            let world_x = chunk.pos.x * CHUNK_SIZE_I + crown_x + ox;
            let world_z = chunk.pos.z * CHUNK_SIZE_I + crown_z + oz;
            let drop_roll = column_rand(
                self.seed
                    ^ 0xB05A_8208_u32
                        .wrapping_add(tier as u32 * 257)
                        .wrapping_add(fringe as u32 * 4_099),
                world_x,
                world_z,
            );
            let drop = 2 + (drop_roll * 3.0) as i32;
            for dy in 1..=drop {
                let y = crown_y - dy;
                if y <= base_y + 1 {
                    break;
                }
                set_tree_leaf(chunk, crown_x + ox, y, crown_z + oz, leaf_kind, origin_y);
            }
        }
    }

    /// A connected, tapered conifer crown derived from tree hierarchy rather
    /// than overlapping spherical pads. Each tier owns four woody branch
    /// spines; needle voxels sit on or directly beside those spines, so the
    /// whole tree remains one six-neighbour semantic object for Select/Edit.
    #[allow(clippy::too_many_arguments)]
    fn place_conifer_crown(
        &self,
        chunk: &mut Chunk,
        trunk_x: i32,
        trunk_z: i32,
        base_y: i32,
        origin_y: i32,
        profile: TreeProfile,
        leaf_kind: BlockType,
    ) {
        debug_assert_eq!(profile.silhouette, TreeSilhouette::Conifer);
        let trunk_top_y = base_y + profile.trunk_height - 1;
        for lift in 1..=profile.crown_lift {
            set_tree_wood(chunk, trunk_x, trunk_top_y + lift, trunk_z, origin_y);
        }

        let crown_start = (profile.trunk_height / 3).max(2);
        let crown_span = (profile.trunk_height - 1 + profile.crown_lift - crown_start).max(1);
        let tier_divisor = (profile.tiers - 1).max(1) as i32;
        let cardinal = [(1, 0), (0, 1), (-1, 0), (0, -1)];
        let world_trunk_x = chunk.pos.x * CHUNK_SIZE_I + trunk_x;
        let world_trunk_z = chunk.pos.z * CHUNK_SIZE_I + trunk_z;
        // Give every tree a stable crown aspect. One quadrant receives a
        // little more growth and the opposite side is occasionally shorter,
        // approximating long-term exposure to prevailing wind and light.
        // This is geometry-only ecology: the render wind can animate needles
        // without ever entering flight or collision physics.
        let aspect_roll = column_rand(self.seed ^ 0xC01F_A5EC, world_trunk_x, world_trunk_z);
        let dominant_direction =
            ((aspect_roll * cardinal.len() as f64).floor() as usize).min(cardinal.len() - 1);

        for tier in 0..profile.tiers {
            let tier_i = tier as i32;
            let nominal_tier_y = base_y + crown_start + tier_i * crown_span / tier_divisor;
            // Interior tiers rise and fall by one voxel in deterministic,
            // low-frequency bands. Keeping the first and tip tiers fixed
            // preserves a grounded base and a clean tapered apex.
            let tier_jitter = if tier == 0 || tier + 1 == profile.tiers {
                0
            } else {
                let roll = column_rand(
                    self.seed ^ 0xC01F_71E2_u32.wrapping_add(tier as u32 * 7_919),
                    world_trunk_x,
                    world_trunk_z,
                );
                if roll < 0.24 {
                    -1
                } else if roll > 0.76 {
                    1
                } else {
                    0
                }
            };
            let tier_y = nominal_tier_y + tier_jitter;
            let remaining = tier_divisor - tier_i.min(tier_divisor);
            let radius = 1 + remaining * (profile.canopy_radius - 1).max(0) / tier_divisor.max(1);

            // A central tuft joins the tier visually to the continuous trunk.
            set_tree_leaf(chunk, trunk_x, tier_y + 1, trunk_z, leaf_kind, origin_y);

            for (direction_index, (dx, dz)) in cardinal.iter().copied().enumerate() {
                let (side_x, side_z) = (-dz, dx);
                let opposite_direction = (dominant_direction + 2) % cardinal.len();
                let aspect_delta = if direction_index == dominant_direction && tier % 2 == 0 {
                    1
                } else if direction_index == opposite_direction
                    && tier > 0
                    && tier + 1 < profile.tiers
                    && tier % 2 == 1
                {
                    -1
                } else {
                    0
                };
                let direction_radius =
                    (radius + aspect_delta).clamp(1, profile.canopy_radius.saturating_add(1));
                let woody_reach = (direction_radius - 1).max(1).min(profile.branch_reach);

                for step in 1..=woody_reach {
                    let branch_x = trunk_x + dx * step;
                    let branch_z = trunk_z + dz * step;
                    set_tree_wood(chunk, branch_x, tier_y, branch_z, origin_y);
                    set_tree_leaf(chunk, branch_x, tier_y + 1, branch_z, leaf_kind, origin_y);

                    // Needle fans grow from the actual woody spine instead of
                    // being pasted as a sphere. Their staggered side choice
                    // breaks the plus-sign silhouette while every voxel stays
                    // six-neighbour connected through the branch-top needle.
                    for side in [-1, 1] {
                        let world_x = world_trunk_x + dx * step + side_x * side;
                        let world_z = world_trunk_z + dz * step + side_z * side;
                        let fan_roll = column_rand(
                            self.seed
                                ^ 0xC01F_FA65_u32
                                    .wrapping_add(tier as u32 * 4_099)
                                    .wrapping_add(direction_index as u32 * 257)
                                    .wrapping_add(step as u32 * 31)
                                    .wrapping_add((side > 0) as u32 * 17),
                            world_x,
                            world_z,
                        );
                        if fan_roll > 0.48 + tier as f64 * 0.025 {
                            set_tree_leaf(
                                chunk,
                                branch_x + side_x * side,
                                tier_y + 1,
                                branch_z + side_z * side,
                                leaf_kind,
                                origin_y,
                            );
                        }
                    }
                }

                let tip_x = trunk_x + dx * direction_radius;
                let tip_z = trunk_z + dz * direction_radius;
                for vertical in [-1, 0, 1] {
                    set_tree_leaf(chunk, tip_x, tier_y + vertical, tip_z, leaf_kind, origin_y);
                }

                // Side feathers are deterministic and asymmetric enough to
                // avoid a mechanical plus sign, while remaining face-adjacent
                // to a branch or tip voxel.
                for side in [-1, 1] {
                    let feather_x = tip_x + side_x * side;
                    let feather_z = tip_z + side_z * side;
                    let world_x = chunk.pos.x * CHUNK_SIZE_I + feather_x;
                    let world_z = chunk.pos.z * CHUNK_SIZE_I + feather_z;
                    let keep = column_rand(
                        self.seed
                            ^ 0xC01F_EA57_u32
                                .wrapping_add(tier as u32 * 4_099)
                                .wrapping_add(direction_index as u32 * 257)
                                .wrapping_add((side > 0) as u32 * 17),
                        world_x,
                        world_z,
                    );
                    if keep > 0.22 + tier as f64 * 0.035 {
                        set_tree_leaf(chunk, feather_x, tier_y, feather_z, leaf_kind, origin_y);
                    }
                }

                // Lower boughs carry a little extra hanging needle mass. It
                // shares the tip column, so it cannot create floating voxels.
                if tier_i * 2 < tier_divisor {
                    set_tree_leaf(chunk, tip_x, tier_y - 2, tip_z, leaf_kind, origin_y);
                }
            }
        }

        let crown_tip_y = trunk_top_y + profile.crown_lift;
        for (dx, dz) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            set_tree_leaf(
                chunk,
                trunk_x + dx,
                crown_tip_y + 1,
                trunk_z + dz,
                leaf_kind,
                origin_y,
            );
        }
    }

    /// Low-density single-block tufts for atmosphere. Deliberately
    /// sparse so the ground stays smooth and walkable â€” the player
    /// must never have to jump over decoration. No 2-tall stacks.
    #[allow(dead_code)]
    fn decorate_flora(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let (surface, cont) = self.surface_height(wx as f64, wz as f64);
                let biome = self.biome(wx as f64, wz as f64, surface, cont);
                let surface_ly = surface - origin_y;
                let above_ly = surface_ly + 1;
                if surface_ly < 0 || surface_ly >= CHUNK_SIZE_I {
                    continue;
                }
                if above_ly < 0 || above_ly >= CHUNK_SIZE_I {
                    continue;
                }
                let r = column_rand(self.seed ^ 0xF107A, wx, wz);
                let ground = chunk.get(lx, surface_ly as usize, lz);
                let above_slot = chunk.get(lx, above_ly as usize, lz);
                if above_slot != AIR {
                    continue;
                }
                // Skip entirely if any of the 4 cardinal neighbours is
                // lower than the current surface â€” avoids placing
                // flora on cliff edges where it would look floating.
                // This also keeps the ground visually calmer.

                let is_grass_ground = ground == <BlockType as Into<Voxel>>::into(BlockType::Grass)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::SavannaGrass)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::TundraGrass);
                let is_sand_ground = ground == <BlockType as Into<Voxel>>::into(BlockType::Sand)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::GlowSand)
                    || ground == <BlockType as Into<Voxel>>::into(BlockType::RedSand);
                let lush = self.scenery_quality == crate::settings::SceneryQuality::Lush;

                // One single-block tuft per biome, very sparse.
                // Densities chosen so the ground reads as "populated"
                // but never as "obstacle course".
                match biome {
                    Biome::Plains => {
                        if lush && is_grass_ground && r < 0.003 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::SakuraPetals.into());
                        }
                    }
                    Biome::Forest => {
                        if lush && is_grass_ground && r < 0.005 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::SakuraPetals.into());
                        }
                    }
                    Biome::Jungle => {
                        if lush && is_grass_ground && r < 0.006 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Bamboo.into());
                        }
                    }
                    Biome::Savanna => {
                        if lush && is_grass_ground && r < 0.002 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::SavannaGrass.into());
                        }
                    }
                    Biome::Desert => {
                        if is_sand_ground && r < 0.002 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Leaves.into());
                        }
                    }
                    Biome::Tundra => {
                        if lush && is_grass_ground && r < 0.003 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::TundraGrass.into());
                        }
                    }
                    Biome::SnowyMountains | Biome::Mountains => {
                        if r < 0.002 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Gravel.into());
                        }
                    }
                    Biome::Mesa => {
                        if is_sand_ground && r < 0.003 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::RedStone.into());
                        }
                    }
                    Biome::Karst => {
                        if lush && is_grass_ground && r < 0.006 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::SakuraPetals.into());
                        }
                    }
                    Biome::Beach => {
                        if lush && is_sand_ground && r < 0.001 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Gravel.into());
                        }
                    }
                    Biome::Ocean => {
                        // Kelp stays below water â€” single-block coral
                        // patches only, no tall stalks that block
                        // visibility.
                        if is_sand_ground && surface < WATER_LEVEL - 2 && r < 0.06 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::MossStone.into());
                        }
                    }
                    Biome::CrystalSpires => {
                        if r < 0.025 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Crystal.into());
                        } else if r < 0.055 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::AlienMoss.into());
                        }
                    }
                    Biome::VolcanicWaste => {
                        if r < 0.012 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Basalt.into());
                        }
                    }
                    Biome::GlacierShards => {
                        if r < 0.010 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::Ice.into());
                        }
                    }
                    Biome::AlienReef => {
                        if r < 0.030 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::AlienMoss.into());
                        } else if r < 0.040 {
                            chunk.set(lx, above_ly as usize, lz, BlockType::BoneRock.into());
                        }
                    }
                }
            }
        }

        // Dense sci-fi micro-props pass â€” see `decorate_props`. Runs
        // after flora so we overwrite bland grass tufts with neon
        // pylons, crates and holo-antennas where they land.
        self.decorate_props(chunk);
        self.decorate_micro_specks(chunk);
    }

    /// Populate the surface with small detailed sci-fi structures
    /// (2-6 block voxel props): neon pylons, cargo crates, holo
    /// antennas, warning barriers, landing-pad tiles and energy
    /// conduits. Every structure is defined in chunk-local space and
    /// clipped to chunk bounds so there's no cross-chunk coordination
    /// needed. Density is deliberately high in alien biomes and
    /// moderate in plains/savanna so the world reads as "inhabited
    /// frontier outpost" rather than "empty Minecraft field".
    fn decorate_props(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // Helper: safe set that ignores out-of-chunk + non-AIR slots.
        let _ = origin_y; // used by helpers below via closure

        // We roll up to ~12 prop candidates per chunk. Each candidate
        // picks a deterministic position + kind from a hash stream.
        const CANDIDATES: usize = 24;
        for i in 0..CANDIDATES {
            let r_pos = column_rand(self.seed ^ (0xF00D_FACE + i as u32 * 7919), cx, cz);
            let r_kind = column_rand(self.seed ^ (0xBEEF_BABE + i as u32 * 104_729), cx, cz);
            let r_gate = column_rand(self.seed ^ (0x1234_5678 + i as u32 * 31), cx, cz);

            let lx = ((r_pos * 65537.0) as usize) % CHUNK_SIZE;
            let lz = ((r_pos * 997.0) as usize) % CHUNK_SIZE;
            let wx = cx * CHUNK_SIZE_I + lx as i32;
            let wz = cz * CHUNK_SIZE_I + lz as i32;
            let (surface, cont) = self.surface_height(wx as f64, wz as f64);
            let biome = self.biome(wx as f64, wz as f64, surface, cont);

            // Density gate per biome. Alien biomes get lots of props;
            // forests/jungles get very few (preserve wilderness).
            let density = astral_prop_density(biome);
            if r_gate > density {
                continue;
            }

            // Slope test â€” props only on reasonably flat ground.
            let (hn, _) = self.surface_height(wx as f64, (wz - 1) as f64);
            let (hs, _) = self.surface_height(wx as f64, (wz + 1) as f64);
            let (he, _) = self.surface_height((wx + 1) as f64, wz as f64);
            let (hw, _) = self.surface_height((wx - 1) as f64, wz as f64);
            let slope = (surface - hn)
                .abs()
                .max((surface - hs).abs())
                .max((surface - he).abs())
                .max((surface - hw).abs());
            if slope >= 2 {
                continue;
            }

            let base_y = surface + 1;
            if base_y < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                continue;
            }

            // Pick a prop shape from the kind roll â€” each prop is a
            // tightly-packed blueprint of (dx, dy, dz, block) offsets
            // from the base column. All small enough to fit in-chunk
            // with the margin we enforce below.
            let kind = {
                let base = (r_kind * 100.0) as u32 % 10;
                if biome != Biome::CrystalSpires {
                    base
                } else {
                    // Weight toward mushroom caps + crystal gardens so the biome
                    // reads closer to reference art (organic neon fungi silhouettes).
                    let u = (column_rand(self.seed ^ (0xC0FFEE_u32 + i as u32 * 97), cx, cz)
                        * 100.0) as u32
                        % 100;
                    if u < 24 {
                        6
                    } else if u < 46 {
                        7
                    } else if u < 60 {
                        4
                    } else if u < 72 {
                        5
                    } else {
                        base
                    }
                }
            };
            match (biome, kind) {
                // --- CRYSTAL SPIRES ------------------------------------
                (Biome::CrystalSpires, 0) | (Biome::AlienReef, 0) => {
                    // Neon pylon: 1x4 crystal column on a bone-rock base,
                    // crowned with a glowing dot. Like reference image.
                    set_safe(chunk, lx, base_y, lz, BlockType::BoneRock, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Crystal, origin_y);
                    set_safe(chunk, lx, base_y + 3, lz, BlockType::IridiumVein, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 4,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 1 | 2) => {
                    // Crystal cluster: 5-block asymmetric sparkle.
                    set_safe(chunk, lx, base_y, lz, BlockType::LuminiteCrystal, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Crystal, origin_y);
                    set_safe(chunk, lx + 1, base_y, lz, BlockType::MagnetiteOre, origin_y);
                    set_safe(chunk, lx, base_y, lz + 1, BlockType::Crystal, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 2,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 4 | 5) => {
                    // Resource garden: saturated crystal cluster with
                    // cyan/magenta tips, dense enough to read at flight speed.
                    for dx in -1..=1 {
                        for dz in -1..=1 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let h = 2 + ((dx * 31 + dz * 17).abs() % 4);
                            for dy in 0..h {
                                let block = match (dx + dz + dy).rem_euclid(5) {
                                    0 => BlockType::LuminiteCrystal,
                                    1 => BlockType::MagnetiteOre,
                                    2 => BlockType::IridiumVein,
                                    3 => BlockType::Crystal,
                                    _ => BlockType::Crystal,
                                };
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    base_y + dy,
                                    nz as usize,
                                    block,
                                    origin_y,
                                );
                            }
                            set_safe(
                                chunk,
                                nx as usize,
                                base_y + h,
                                nz as usize,
                                BlockType::LuminiteCrystal,
                                origin_y,
                            );
                        }
                    }
                }
                (Biome::CrystalSpires, 6 | 7) => {
                    // Giant mushroom landmark — same silhouette class as AlienReef,
                    // but cap reads cyan/crystal (cockpit key art).
                    for dy in 0..5 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    let cap_y = base_y + 5;
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let dist = dx.abs().max(dz.abs());
                            if dist <= 2 {
                                let block = if dist == 2 {
                                    BlockType::Crystal
                                } else {
                                    BlockType::LuminiteCrystal
                                };
                                set_safe(chunk, nx as usize, cap_y, nz as usize, block, origin_y);
                            }
                            if dist <= 1 {
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    cap_y - 1,
                                    nz as usize,
                                    if (dx + dz).rem_euclid(2) == 0 {
                                        BlockType::LuminiteCrystal
                                    } else {
                                        BlockType::Crystal
                                    },
                                    origin_y,
                                );
                            }
                        }
                    }
                    set_safe(
                        chunk,
                        lx,
                        cap_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::CrystalSpires, 3) | (Biome::AlienReef, 3) => {
                    // Holo-antenna: 4-block thin mast with cyan tip.
                    for dy in 0..3 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    set_safe(
                        chunk,
                        lx,
                        base_y + 3,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y + 2, lz, BlockType::Crystal, origin_y);
                    }
                    if lx >= 1 {
                        set_safe(chunk, lx - 1, base_y + 2, lz, BlockType::Crystal, origin_y);
                    }
                }

                // --- ALIEN REEF ----------------------------------------
                (Biome::AlienReef, 1 | 2) => {
                    // Purple bioluminescent coral fan.
                    set_safe(chunk, lx, base_y, lz, BlockType::AlienMoss, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::AlienMoss, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx + 1,
                            base_y + 1,
                            lz,
                            BlockType::AlienMoss,
                            origin_y,
                        );
                    }
                    if lz + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx,
                            base_y + 1,
                            lz + 1,
                            BlockType::AlienMoss,
                            origin_y,
                        );
                    }
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Crystal, origin_y);
                }
                (Biome::AlienReef, 4 | 5) => {
                    // Large neon mushroom: dark organic stem, broad cap,
                    // bright underside dots. This is the main reference
                    // silhouette from the cockpit image.
                    for dy in 0..5 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::BoneRock, origin_y);
                    }
                    let cap_y = base_y + 5;
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let dist = dx.abs().max(dz.abs());
                            if dist <= 2 {
                                let block = if dist == 2 {
                                    BlockType::Crystal
                                } else {
                                    BlockType::AlienMoss
                                };
                                set_safe(chunk, nx as usize, cap_y, nz as usize, block, origin_y);
                            }
                            if dist <= 1 {
                                set_safe(
                                    chunk,
                                    nx as usize,
                                    cap_y - 1,
                                    nz as usize,
                                    if (dx + dz).rem_euclid(2) == 0 {
                                        BlockType::MagnetiteOre
                                    } else {
                                        BlockType::Crystal
                                    },
                                    origin_y,
                                );
                            }
                        }
                    }
                    set_safe(
                        chunk,
                        lx,
                        cap_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::AlienReef, 6 | 7) => {
                    // Short bone-and-neon arch. This creates flight
                    // corridors and strong silhouettes without needing
                    // cross-chunk structures.
                    for dx in -2..=2 {
                        let nx = lx as i32 + dx;
                        if nx < 0 {
                            continue;
                        }
                        let edge = dx.abs() == 2;
                        let h = if edge { 5 } else { 3 };
                        for dy in 0..h {
                            let block = if edge {
                                BlockType::BoneRock
                            } else {
                                BlockType::Crystal
                            };
                            set_safe(chunk, nx as usize, base_y + dy, lz, block, origin_y);
                        }
                        let cap = if edge {
                            BlockType::MagnetiteOre
                        } else {
                            BlockType::LuminiteCrystal
                        };
                        set_safe(chunk, nx as usize, base_y + h, lz, cap, origin_y);
                    }
                }
                (Biome::AlienReef, 8 | 9) | (Biome::CrystalSpires, 8 | 9) => {
                    // Mini landing pad / tech plate: dark center,
                    // cyan-magenta rim, amber corner lights.
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            let nx = lx as i32 + dx;
                            let nz = lz as i32 + dz;
                            if nx < 0 || nz < 0 {
                                continue;
                            }
                            let edge = dx.abs() == 2 || dz.abs() == 2;
                            let corner = dx.abs() == 2 && dz.abs() == 2;
                            let block = if corner {
                                BlockType::MagnetiteOre
                            } else if edge {
                                if (dx + dz).rem_euclid(2) == 0 {
                                    BlockType::LuminiteCrystal
                                } else {
                                    BlockType::Crystal
                                }
                            } else {
                                BlockType::Basalt
                            };
                            set_safe(chunk, nx as usize, base_y, nz as usize, block, origin_y);
                        }
                    }
                }

                // --- VOLCANIC WASTE -----------------------------------
                (Biome::VolcanicWaste, _) => {
                    // Obsidian drill rig: 2-wide basalt pedestal with
                    // a lava core glowing inside.
                    set_safe(chunk, lx, base_y, lz, BlockType::Basalt, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Basalt, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Lava, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y, lz, BlockType::Basalt, origin_y);
                        set_safe(chunk, lx + 1, base_y + 1, lz, BlockType::Basalt, origin_y);
                    }
                }

                // --- GLACIER SHARDS -----------------------------------
                (Biome::GlacierShards, _) => {
                    // Ice sensor spike: 4-tall ice with a glow crown.
                    for dy in 0..3 {
                        set_safe(chunk, lx, base_y + dy, lz, BlockType::Ice, origin_y);
                    }
                    set_safe(
                        chunk,
                        lx,
                        base_y + 3,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }

                // --- PLAINS / SAVANNA / DESERT -------------------------
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 0 | 1) => {
                    // Cargo crate: 2x2x2 stone box (stackable shipping
                    // container). Classic sci-fi shooter prop.
                    for dx in 0..2 {
                        for dz in 0..2 {
                            for dy in 0..2 {
                                let nx = lx + dx;
                                let nz = lz + dz;
                                if nx >= CHUNK_SIZE || nz >= CHUNK_SIZE {
                                    continue;
                                }
                                let block = if dy == 1 && (dx + dz) % 2 == 0 {
                                    BlockType::LuminiteCrystal // glowing label strip
                                } else {
                                    BlockType::Stone
                                };
                                set_safe(chunk, nx, base_y + dy, nz, block, origin_y);
                            }
                        }
                    }
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 2 | 3) => {
                    // Holo-console: 1x2 stone block with a glowing
                    // crystal top â€” like a sci-fi signpost / terminal.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 4) => {
                    // Landing-pad strip: 3x1 glow-sand tile with
                    // stone markers at each end.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(
                            chunk,
                            lx + 1,
                            base_y,
                            lz,
                            BlockType::LuminiteCrystal,
                            origin_y,
                        );
                    }
                    if lx + 2 < CHUNK_SIZE {
                        set_safe(chunk, lx + 2, base_y, lz, BlockType::Stone, origin_y);
                    }
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, 5) => {
                    // Warning pylon: 4-tall stone with alternating
                    // crystal stripes â€” reads as a striped hazard post.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::MagnetiteOre, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 3, lz, BlockType::MagnetiteOre, origin_y);
                }
                (Biome::Plains | Biome::Savanna | Biome::Tundra | Biome::Desert, _) => {
                    // Fuel barrel: single-block glow-sand on stone
                    // pedestal â€” the catch-all cheap prop.
                    set_safe(chunk, lx, base_y, lz, BlockType::Stone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::Lava, origin_y);
                }

                // --- MESA ---------------------------------------------
                (Biome::Mesa, _) => {
                    // Rust-red ruin post with a glow crown.
                    set_safe(chunk, lx, base_y, lz, BlockType::RedStone, origin_y);
                    set_safe(chunk, lx, base_y + 1, lz, BlockType::RedStone, origin_y);
                    set_safe(chunk, lx, base_y + 2, lz, BlockType::MagnetiteOre, origin_y);
                }

                // --- FOREST / JUNGLE (rare) ---------------------------
                (Biome::Forest | Biome::Jungle, _) => {
                    // Abandoned alien survey beacon so normal terrain
                    // still carries the neon sci-fi language.
                    set_safe(chunk, lx, base_y, lz, BlockType::Basalt, origin_y);
                    set_safe(
                        chunk,
                        lx,
                        base_y + 1,
                        lz,
                        BlockType::LuminiteCrystal,
                        origin_y,
                    );
                    if lx + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx + 1, base_y, lz, BlockType::Crystal, origin_y);
                    }
                    if lz + 1 < CHUNK_SIZE {
                        set_safe(chunk, lx, base_y, lz + 1, BlockType::MagnetiteOre, origin_y);
                    }
                }

                _ => {}
            }
        }
    }

    /// Single-block crystal / neon specks on the surface — micro-detail
    /// that reads as glitter without extra mesh types.
    fn decorate_micro_specks(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;
        const N: usize = 36;
        for i in 0..N {
            let r_pos = column_rand(self.seed ^ (0x51EE_1110_u32 + i as u32 * 401), cx, cz);
            let r_mat = column_rand(self.seed ^ (0x51EE_2220_u32 + i as u32 * 403), cx, cz);
            let r_gate = column_rand(self.seed ^ (0x51EE_3330_u32 + i as u32 * 407), cx, cz);

            let lx = ((r_pos * 131_071.0) as usize) % CHUNK_SIZE;
            let lz = ((r_pos * 524_287.0) as usize) % CHUNK_SIZE;
            let wx = cx * CHUNK_SIZE_I + lx as i32;
            let wz = cz * CHUNK_SIZE_I + lz as i32;
            let (surface, _cont) = self.surface_height(wx as f64, wz as f64);
            let biome = self.biome(wx as f64, wz as f64, surface, _cont);

            let keep = match biome {
                Biome::CrystalSpires | Biome::AlienReef => r_gate < 0.18,
                Biome::GlacierShards => r_gate < 0.045,
                Biome::VolcanicWaste => r_gate < 0.030,
                Biome::Forest | Biome::Jungle | Biome::Karst => false,
                Biome::Mesa => false,
                Biome::Desert | Biome::Savanna | Biome::Beach | Biome::Ocean => false,
                Biome::Mountains | Biome::SnowyMountains | Biome::Tundra => false,
                _ => false,
            };
            if !keep {
                continue;
            }

            let base_y = surface + 1;
            if base_y < origin_y || base_y >= origin_y + CHUNK_SIZE_I {
                continue;
            }

            let roll = ((r_mat * 100.0) as u32) % 6;
            let bt = match biome {
                Biome::CrystalSpires => match roll {
                    0 | 1 => BlockType::Crystal,
                    2 => BlockType::LuminiteCrystal,
                    3 => BlockType::GlowSand,
                    4 => BlockType::Limestone,
                    _ => BlockType::MossStone,
                },
                Biome::AlienReef => match roll {
                    0 | 1 => BlockType::AlienMoss,
                    2 => BlockType::BoneRock,
                    3 => BlockType::Crystal,
                    4 => BlockType::MossStone,
                    _ => BlockType::Limestone,
                },
                Biome::GlacierShards => match roll {
                    0 | 1 => BlockType::Ice,
                    2 => BlockType::Crystal,
                    3 => BlockType::Snow,
                    _ => BlockType::Gravel,
                },
                Biome::VolcanicWaste => match roll {
                    0 | 1 => BlockType::Basalt,
                    2 => BlockType::Lava,
                    3 => BlockType::RedStone,
                    _ => BlockType::Gravel,
                },
                Biome::Mesa => match roll {
                    0 | 1 => BlockType::MesaClay,
                    2 => BlockType::RedSand,
                    3 => BlockType::RedStone,
                    _ => BlockType::Gravel,
                },
                Biome::Desert | Biome::Savanna => match roll {
                    0 | 1 => BlockType::Sand,
                    2 => BlockType::RedSand,
                    3 => BlockType::SavannaGrass,
                    _ => BlockType::Gravel,
                },
                Biome::Forest => match roll {
                    0 | 1 => BlockType::MossStone,
                    2 => BlockType::Leaves,
                    3 => BlockType::Wood,
                    _ => BlockType::Gravel,
                },
                Biome::Jungle => match roll {
                    0 | 1 => BlockType::JungleLeaves,
                    2 => BlockType::MossStone,
                    3 => BlockType::Wood,
                    _ => BlockType::Gravel,
                },
                Biome::Karst => match roll {
                    0 | 1 => BlockType::Limestone,
                    2 => BlockType::MossStone,
                    _ => BlockType::Gravel,
                },
                Biome::SnowyMountains | Biome::Tundra => match roll {
                    0 | 1 => BlockType::Snow,
                    2 => BlockType::Ice,
                    _ => BlockType::Gravel,
                },
                Biome::Beach | Biome::Ocean => match roll {
                    0 | 1 => BlockType::Sand,
                    _ => BlockType::Gravel,
                },
                _ => match roll {
                    0 | 1 => BlockType::MossStone,
                    2 => BlockType::Grass,
                    _ => BlockType::Gravel,
                },
            };
            set_safe(chunk, lx, base_y, lz, bt, origin_y);
        }
    }

    /// Scatter natural arches, ruin pillars and boulder piles in
    /// mountain/mesa/karst biomes. Purely chunk-local: anything that
    /// would poke past the chunk boundary is skipped.
    fn decorate_structures(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        let origin_y = cy * CHUNK_SIZE_I;

        // One roll per chunk decides which (if any) landmark spawns
        // here. Keeps density low (â‰ˆ one every few chunks).
        let roll = column_rand(self.seed ^ 0xA11CE, cx, cz);
        // Random but stable anchor inside the chunk â€” not always the
        // centre, so neighbouring chunks don't line up in a grid.
        let anchor_x = 4 + ((column_rand(self.seed ^ 0xB077, cx, cz) * 8.0) as i32);
        let anchor_z = 4 + ((column_rand(self.seed ^ 0xC099, cx, cz) * 8.0) as i32);
        let wx_anchor = cx * CHUNK_SIZE_I + anchor_x;
        let wz_anchor = cz * CHUNK_SIZE_I + anchor_z;
        let (surface, cont) = self.surface_height(wx_anchor as f64, wz_anchor as f64);
        let biome = self.biome(wx_anchor as f64, wz_anchor as f64, surface, cont);

        // Macro landmarks: very rare hero silhouettes that act as
        // long-range navigation anchors and create strong reveal moments.
        if roll < 0.010 && matches!(biome, Biome::CrystalSpires | Biome::AlienReef) {
            self.try_place_spire_cathedral(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        if roll >= 0.010 && roll < 0.016 && matches!(biome, Biome::Mesa | Biome::VolcanicWaste) {
            self.try_place_crater_basin(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }

        // Arch: 1 chance in ~40 chunks, only in rocky biomes.
        if roll < 0.025
            && matches!(
                biome,
                Biome::Mountains | Biome::SnowyMountains | Biome::Mesa | Biome::Karst
            )
        {
            self.try_place_arch(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        // Ruin pillar cluster: 1 in ~60, plains/mountains/mesa.
        if roll >= 0.025
            && roll < 0.042
            && matches!(
                biome,
                Biome::Plains | Biome::Savanna | Biome::Mountains | Biome::Mesa
            )
        {
            self.try_place_ruin_pillars(chunk, anchor_x, anchor_z, surface, origin_y, biome);
            return;
        }
        // Boulder pile: 1 in ~40 in rocky / alien biomes.
        if roll >= 0.042
            && roll < 0.067
            && matches!(
                biome,
                Biome::Mountains
                    | Biome::SnowyMountains
                    | Biome::Mesa
                    | Biome::Tundra
                    | Biome::CrystalSpires
                    | Biome::GlacierShards
            )
        {
            self.try_place_boulder_pile(chunk, anchor_x, anchor_z, surface, origin_y, biome);
        }
    }

    /// Natural stone arch â€” two pillars with a span of blocks joining
    /// them at the top. Walkable underneath. Scales with biome.
    fn try_place_arch(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let height = 7 + ((column_rand(self.seed ^ 0xAAAA, ax, az) * 4.0) as i32);
        let span = 5 + ((column_rand(self.seed ^ 0xBBBB, ax, az) * 3.0) as i32);
        let top_y = surface + height;
        if top_y + 1 >= origin_y + CHUNK_SIZE_I {
            return;
        }
        if surface < origin_y - 1 {
            return;
        }
        let block = match biome {
            Biome::Mesa => BlockType::RedStone,
            Biome::Karst => BlockType::Limestone,
            Biome::SnowyMountains => BlockType::Stone,
            _ => BlockType::Stone,
        };
        let left_x = ax - span / 2;
        let right_x = ax + span / 2;
        if left_x < 0 || right_x >= CHUNK_SIZE_I {
            return;
        }
        let lz = az as usize;
        // Two pillars.
        for x in [left_x, right_x] {
            for y in (surface + 1)..=top_y {
                if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }
                let ly = (y - origin_y) as usize;
                chunk.set(x as usize, ly, lz, block.into());
            }
        }
        // Arching span at top_y with a gentle curve (one row slightly
        // lower at the ends â†’ cleaner arch silhouette).
        for x in left_x..=right_x {
            let curve_off = if x == left_x || x == right_x { 0 } else { 0 };
            let y = top_y - curve_off;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            chunk.set(x as usize, ly, lz, block.into());
        }
        // Crown stone on very top centre for visual "keystone".
        let keystone_y = top_y + 1;
        if keystone_y >= origin_y && keystone_y < origin_y + CHUNK_SIZE_I {
            let ly = (keystone_y - origin_y) as usize;
            chunk.set(ax as usize, ly, lz, block.into());
        }
    }

    /// Cluster of 4â€“7 broken pillars on the surface â€” looks like an
    /// ancient ruin. Heights vary so the silhouette feels natural.
    fn try_place_ruin_pillars(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        _surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let block = match biome {
            Biome::Mesa => BlockType::MesaClay,
            Biome::Savanna => BlockType::Limestone,
            _ => BlockType::Stone,
        };
        let cap = match biome {
            Biome::Mesa => BlockType::RedStone,
            _ => BlockType::MossStone,
        };
        let positions = [(-3, -2), (-2, 2), (0, 0), (2, -1), (3, 2), (-1, -3), (1, 3)];
        for (i, (dx, dz)) in positions.iter().enumerate() {
            let x = ax + dx;
            let z = az + dz;
            if x < 0 || x >= CHUNK_SIZE_I || z < 0 || z >= CHUNK_SIZE_I {
                continue;
            }
            // Varying pillar heights (3, 5, 2, 6, 4, 3, 5).
            let h = 2 + ((column_rand(self.seed ^ (i as u32 * 17), ax + dx, az + dz) * 5.0) as i32);
            let wx = chunk.pos.x * CHUNK_SIZE_I + x;
            let wz = chunk.pos.z * CHUNK_SIZE_I + z;
            let (col_surface, _) = self.surface_height(wx as f64, wz as f64);
            for dy in 1..=h {
                let y = col_surface + dy;
                if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                    continue;
                }
                let ly = (y - origin_y) as usize;
                let b = if dy == h { cap } else { block };
                chunk.set(x as usize, ly, z as usize, b.into());
            }
        }
    }

    /// Loose pile of boulders (5Ã—5 low dome of stone blocks).
    fn try_place_boulder_pile(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let block = match biome {
            Biome::Mesa => BlockType::RedStone,
            Biome::GlacierShards => BlockType::Ice,
            Biome::SnowyMountains => BlockType::Stone,
            Biome::CrystalSpires => BlockType::Crystal,
            Biome::Tundra => BlockType::Gravel,
            _ => BlockType::Stone,
        };
        // 3 layer dome, shrinking radius.
        let layers = [(2i32, 0i32), (1, 1), (0, 2)];
        for (radius, dy) in layers.iter() {
            let y = surface + dy;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            for dz in -*radius..=*radius {
                for dx in -*radius..=*radius {
                    if dx.abs() == *radius && dz.abs() == *radius && *radius == 2 {
                        continue;
                    }
                    let nx = ax + dx;
                    let nz = az + dz;
                    if nx < 0 || nx >= CHUNK_SIZE_I || nz < 0 || nz >= CHUNK_SIZE_I {
                        continue;
                    }
                    chunk.set(nx as usize, ly, nz as usize, block.into());
                }
            }
        }
    }

    fn try_place_spire_cathedral(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let h = 16 + ((column_rand(self.seed ^ 0x51A1_9001, ax, az) * 9.0) as i32);
        let block = if biome == Biome::AlienReef {
            BlockType::LuminiteCrystal
        } else {
            BlockType::Crystal
        };
        let buttress = if biome == Biome::AlienReef {
            BlockType::ShipHullAlloy
        } else {
            BlockType::Limestone
        };
        for dy in 0..=h {
            let y = surface + dy;
            if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                continue;
            }
            let ly = (y - origin_y) as usize;
            let taper = (h - dy) / 5;
            for dz in -1 - taper..=1 + taper {
                for dx in -1 - taper..=1 + taper {
                    let nx = ax + dx;
                    let nz = az + dz;
                    if nx < 1 || nx >= CHUNK_SIZE_I - 1 || nz < 1 || nz >= CHUNK_SIZE_I - 1 {
                        continue;
                    }
                    let edge = dx.abs() == 1 + taper || dz.abs() == 1 + taper;
                    chunk.set(
                        nx as usize,
                        ly,
                        nz as usize,
                        if edge { buttress } else { block }.into(),
                    );
                }
            }
        }
    }

    fn try_place_crater_basin(
        &self,
        chunk: &mut Chunk,
        ax: i32,
        az: i32,
        surface: i32,
        origin_y: i32,
        biome: Biome,
    ) {
        let rim_block = if biome == Biome::VolcanicWaste {
            BlockType::Basalt
        } else {
            BlockType::RedStone
        };
        let core_block = if biome == Biome::VolcanicWaste {
            BlockType::Lava
        } else {
            BlockType::MagnetiteOre
        };
        for dz in -4..=4 {
            for dx in -4..=4 {
                let nx = ax + dx;
                let nz = az + dz;
                if nx < 0 || nx >= CHUNK_SIZE_I || nz < 0 || nz >= CHUNK_SIZE_I {
                    continue;
                }
                let d2 = dx * dx + dz * dz;
                let depth = if d2 <= 2 {
                    3
                } else if d2 <= 8 {
                    2
                } else if d2 <= 16 {
                    1
                } else {
                    0
                };
                for k in 0..=depth {
                    let y = surface - k;
                    if y < origin_y || y >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    let ly = (y - origin_y) as usize;
                    let b = if d2 <= 2 && k == depth {
                        core_block
                    } else {
                        rim_block
                    };
                    chunk.set(nx as usize, ly, nz as usize, b.into());
                }
            }
        }
    }

    /// Hill-sculpting palace pass. Rather than placing buildings on
    /// flattened land, we TURN THE HILL ITSELF into a futuristic
    /// palace: the natural peak becomes the building silhouette,
    /// the insides get hollowed out with multiple floors, the shell
    /// stays solid for manual cutouts, and cardinal entrances let the
    /// player walk inside.
    #[allow(dead_code)]
    pub fn try_sculpt_palace(&self, chunk: &mut Chunk) {
        let ChunkPos {
            x: cx,
            y: cy,
            z: cz,
        } = chunk.pos;
        // Palace grid: each 4Ã—4 chunks (64Ã—64 blocks) is one district.
        const DISTRICT: i32 = 4;
        let dx = cx.div_euclid(DISTRICT);
        let dz = cz.div_euclid(DISTRICT);
        let roll = column_rand(self.seed ^ 0xC17A_F00D, dx, dz);
        // ~55% of districts become a sculpted palace.
        if roll > 0.55 {
            return;
        }

        // Centre of the district drives biome + base altitude.
        let centre_wx = dx * DISTRICT * CHUNK_SIZE_I + (DISTRICT * CHUNK_SIZE_I) / 2;
        let centre_wz = dz * DISTRICT * CHUNK_SIZE_I + (DISTRICT * CHUNK_SIZE_I) / 2;
        let (centre_h, centre_cont) = self.surface_height(centre_wx as f64, centre_wz as f64);
        let centre_biome = self.biome(centre_wx as f64, centre_wz as f64, centre_h, centre_cont);
        // Skip water + dangerous terrain.
        if matches!(centre_biome, Biome::Ocean | Biome::GlacierShards) {
            return;
        }
        if centre_h <= WATER_LEVEL + 6 {
            return; // need a real hill to sculpt
        }

        let origin_y = cy * CHUNK_SIZE_I;
        // Ground floor slightly below the peak so the palace merges
        // with the hill instead of floating on top.
        let base_y = centre_h - 2;

        // Palette per biome. `wall` is the bulk of the shell,
        // `accent` is the roof cap, `glow` lights the interior floor.
        let (wall, accent, glow, floor) = match centre_biome {
            Biome::VolcanicWaste => (
                BlockType::Basalt,
                BlockType::Stone,
                BlockType::Lava,
                BlockType::GlowSand,
            ),
            Biome::CrystalSpires => (
                BlockType::BoneRock,
                BlockType::BoneRock,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            Biome::AlienReef => (
                BlockType::BoneRock,
                BlockType::BoneRock,
                BlockType::AlienMoss,
                BlockType::GlowSand,
            ),
            Biome::Desert | Biome::Savanna | Biome::Mesa => (
                BlockType::BoneRock,
                BlockType::Stone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            Biome::SnowyMountains | Biome::Tundra => (
                BlockType::Ice,
                BlockType::Limestone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
            _ => (
                BlockType::Limestone,
                BlockType::Stone,
                BlockType::Crystal,
                BlockType::GlowSand,
            ),
        };

        let district_side = DISTRICT * CHUNK_SIZE_I;
        let district_ox = dx * district_side;
        let district_oz = dz * district_side;
        let half_side = district_side / 2;
        // Footprint radius â€” leave a ring of natural landscape around
        // the palace for approach/landscaping.
        let max_r = half_side - 6;

        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let wx = cx * CHUNK_SIZE_I + lx as i32;
                let wz = cz * CHUNK_SIZE_I + lz as i32;
                let rx = wx - (district_ox + half_side);
                let rz = wz - (district_oz + half_side);
                let cheb = rx.abs().max(rz.abs());
                if cheb > max_r {
                    continue; // outside palace footprint â€” keep natural terrain
                }

                let (h_here, _) = self.surface_height(wx as f64, wz as f64);
                let palace_top = h_here;
                if palace_top - base_y < 8 {
                    // column too short â€” leave natural terrain untouched
                    continue;
                }

                // Shell detection: edge of footprint OR neighbour with
                // a significantly lower natural surface. The shell
                // follows the hill's silhouette.
                let edge_of_footprint = cheb >= max_r - 1;
                let n = [
                    self.surface_height((wx + 1) as f64, wz as f64).0,
                    self.surface_height((wx - 1) as f64, wz as f64).0,
                    self.surface_height(wx as f64, (wz + 1) as f64).0,
                    self.surface_height(wx as f64, (wz - 1) as f64).0,
                ];
                let height_drop = n.iter().any(|&nh| nh + 3 < palace_top);
                let is_shell = edge_of_footprint || height_drop;

                // Rebuild this column from base_y up to palace_top.
                for wy in base_y..=palace_top {
                    if wy < origin_y || wy >= origin_y + CHUNK_SIZE_I {
                        continue;
                    }
                    let ly = (wy - origin_y) as usize;
                    let dy_bottom = wy - base_y;
                    let dy_top = palace_top - wy;

                    // Ground floor is always a warm emissive slab so
                    // the interior is lit from below.
                    if dy_bottom == 0 {
                        chunk.set(lx, ly, lz, floor.into());
                        continue;
                    }

                    if is_shell {
                        // Entrance arches on cardinal axes at the
                        // palace edge, height 1..=3.
                        let on_axis = rx == 0 || rz == 0;
                        let at_edge = cheb == max_r;
                        if on_axis && at_edge && dy_bottom >= 1 && dy_bottom <= 3 {
                            chunk.set(lx, ly, lz, AIR);
                            continue;
                        }
                        if dy_top == 0 {
                            chunk.set(lx, ly, lz, accent.into());
                        } else {
                            chunk.set(lx, ly, lz, wall.into());
                        }
                    } else {
                        // Interior column: hollow with a structural
                        // floor every 6 y (walkable multi-storey).
                        let floor_band = dy_bottom % 6 == 0;
                        if dy_top == 0 {
                            chunk.set(lx, ly, lz, wall.into()); // roof cap
                        } else if floor_band && dy_top >= 2 {
                            chunk.set(lx, ly, lz, wall.into());
                        } else {
                            chunk.set(lx, ly, lz, AIR);
                        }
                    }
                }

                // Pillar tips: at the very top of the hill (within
                // 2 of centre in cheb), extend a thin spire 4 y above
                // palace_top for a gothic-futuristic roofline.
                if cheb <= 1 {
                    for extra in 1..=4 {
                        let wy = palace_top + extra;
                        if wy < origin_y || wy >= origin_y + CHUNK_SIZE_I {
                            continue;
                        }
                        let ly = (wy - origin_y) as usize;
                        let block = if extra == 4 { glow } else { accent };
                        chunk.set(lx, ly, lz, block.into());
                    }
                }
            }
        }
    }

    /// Player and bot commands now own city silhouettes. The automatic
    /// terrain-palace pass made normal worlds look randomly hollowed and
    /// artificial, so default terrain generation leaves cities alone.
    #[inline]
    pub fn try_place_city(&self, _chunk: &mut Chunk) {}

    pub fn biome_at(&self, wx: i32, wz: i32) -> Biome {
        let (h, cont) = self.surface_height(wx as f64, wz as f64);
        self.biome(wx as f64, wz as f64, h, cont)
    }

    pub fn find_neon_showcase_spawn(
        &self,
        origin_x: i32,
        origin_z: i32,
        max_radius: i32,
    ) -> Option<NeonSpawnPoint> {
        if let Some(landing) = self.astral_frontier_landing() {
            let distance = (i64::from(landing.x) - i64::from(origin_x))
                .abs()
                .max((i64::from(landing.y) - i64::from(origin_z)).abs());
            let landing_radius = max_radius.max(0).min(NEON_SPAWN_SEARCH_MAX_RADIUS);
            if distance <= i64::from(landing_radius) {
                let surface = self.surface_height_at(landing.x, landing.y);
                let biome = self.biome_at(landing.x, landing.y);
                debug_assert!(biome.is_neon_showcase());
                return Some(NeonSpawnPoint {
                    x: landing.x,
                    y: surface.saturating_add(24),
                    z: landing.y,
                    biome,
                });
            }
        }

        let mut best: Option<(i64, NeonSpawnPoint)> = None;
        let step = NEON_SPAWN_SEARCH_STEP;
        let max_radius = bounded_search_radius(max_radius, step, NEON_SPAWN_SEARCH_MAX_RADIUS);
        let mut visited = 0usize;

        for radius in (0..=max_radius).step_by(step as usize) {
            let complete = visit_bounded_square_perimeter(
                origin_x,
                origin_z,
                radius,
                step,
                &mut visited,
                NEON_SPAWN_SEARCH_MAX_CANDIDATES,
                |x, z| {
                    let surface = self.surface_height_at(x, z);
                    let biome = self.biome_at(x, z);
                    if !biome.is_neon_showcase() || surface <= WATER_LEVEL + 6 {
                        return;
                    }

                    let hn = self.surface_height_at(x, z.saturating_sub(2));
                    let hs = self.surface_height_at(x, z.saturating_add(2));
                    let he = self.surface_height_at(x.saturating_add(2), z);
                    let hw = self.surface_height_at(x.saturating_sub(2), z);
                    let slope = (i64::from(surface) - i64::from(hn))
                        .abs()
                        .max((i64::from(surface) - i64::from(hs)).abs())
                        .max((i64::from(surface) - i64::from(he)).abs())
                        .max((i64::from(surface) - i64::from(hw)).abs());
                    if slope > 5 {
                        return;
                    }

                    let distance = (i64::from(x) - i64::from(origin_x))
                        .abs()
                        .max((i64::from(z) - i64::from(origin_z)).abs());
                    let floor_score = (i64::from(surface) - i64::from(WATER_LEVEL + 22)).abs();
                    let biome_bonus = if biome == Biome::AlienReef {
                        -280_i64
                    } else {
                        -180_i64
                    };
                    let score = distance + slope * 320 + floor_score * 6 + biome_bonus;
                    let candidate = NeonSpawnPoint {
                        x,
                        y: surface.saturating_add(26),
                        z,
                        biome,
                    };
                    if best.map_or(true, |(best_score, _)| score < best_score) {
                        best = Some((score, candidate));
                    }
                },
            );
            debug_assert!(complete, "neon spawn search exceeded its hard work cap");
            if !complete {
                break;
            }

            if radius >= 512 && best.is_some() {
                break;
            }
        }

        best.map(|(_, point)| point)
    }

    pub fn find_natural_spawn(
        &self,
        origin_x: i32,
        origin_z: i32,
        max_radius: i32,
    ) -> Option<NaturalSpawnPoint> {
        let mut best: Option<(i64, NaturalSpawnPoint)> = None;
        let step = NATURAL_SPAWN_SEARCH_STEP;
        let max_radius = bounded_search_radius(max_radius, step, NATURAL_SPAWN_SEARCH_MAX_RADIUS);
        let mut visited = 0usize;

        for radius in (0..=max_radius).step_by(step as usize) {
            let complete = visit_bounded_square_perimeter(
                origin_x,
                origin_z,
                radius,
                step,
                &mut visited,
                NATURAL_SPAWN_SEARCH_MAX_CANDIDATES,
                |x, z| {
                    let surface = self.surface_height_at(x, z);
                    let biome = self.biome_at(x, z);
                    if biome.is_showcase_terrain() || surface <= WATER_LEVEL + 4 {
                        return;
                    }

                    let hn = self.surface_height_at(x, z.saturating_sub(2));
                    let hs = self.surface_height_at(x, z.saturating_add(2));
                    let he = self.surface_height_at(x.saturating_add(2), z);
                    let hw = self.surface_height_at(x.saturating_sub(2), z);
                    let slope = (i64::from(surface) - i64::from(hn))
                        .abs()
                        .max((i64::from(surface) - i64::from(hs)).abs())
                        .max((i64::from(surface) - i64::from(he)).abs())
                        .max((i64::from(surface) - i64::from(hw)).abs());
                    if slope > 6 {
                        return;
                    }

                    let distance = (i64::from(x) - i64::from(origin_x))
                        .abs()
                        .max((i64::from(z) - i64::from(origin_z)).abs());
                    let comfortable_height =
                        (i64::from(surface) - i64::from(WATER_LEVEL + 18)).abs();
                    let score = distance + slope * 96 + comfortable_height * 2;
                    let candidate = NaturalSpawnPoint {
                        x,
                        y: surface.saturating_add(10),
                        z,
                        biome,
                    };
                    if best.map_or(true, |(best_score, _)| score < best_score) {
                        best = Some((score, candidate));
                    }
                },
            );
            debug_assert!(complete, "natural spawn search exceeded its hard work cap");
            if !complete {
                break;
            }

            if radius >= 256 && best.is_some() {
                break;
            }
        }

        best.map(|(_, point)| point)
    }

    /// Public surface height lookup â€” block y of the topmost solid block
    /// at a world (x, z) column. Used to spawn the player above terrain.
    /// Locate a strong, water-filled lowland course for visual QA, cinematic
    /// bookmarks, or an agent inspection pass. The search is bounded and
    /// deterministic; ordinary world spawning remains unchanged.
    fn hydrographic_focus_cross_section(&self, x: i32, z: i32) -> Option<HydrographicCrossSection> {
        let environment = self.environment_sample_at(x, z);
        let [flow_x, flow_z] = environment.flow_direction;
        let normal = if flow_x.abs() >= flow_z.abs() {
            IVec2::Y
        } else {
            IVec2::X
        };

        let mut banks = [(0i32, 0i32, false); 2];
        for (index, side) in [-1, 1].into_iter().enumerate() {
            for distance in (2..=48).step_by(2) {
                let offset = side * distance;
                let sample = IVec2::new(
                    x.saturating_add(normal.x.saturating_mul(offset)),
                    z.saturating_add(normal.y.saturating_mul(offset)),
                );
                let surface = self.surface_height_at(sample.x, sample.y);
                if surface <= WATER_LEVEL + 1 {
                    continue;
                }
                let biome = self.biome_at(sample.x, sample.y);
                let living = matches!(
                    biome,
                    Biome::Plains
                        | Biome::Forest
                        | Biome::Jungle
                        | Biome::Savanna
                        | Biome::Tundra
                        | Biome::Karst
                );
                banks[index] = (distance, surface, living);
                break;
            }
        }
        if banks.iter().any(|bank| bank.0 == 0) {
            return None;
        }

        Some(HydrographicCrossSection {
            width: banks[0].0 + banks[1].0,
            mean_bank_height: ((i64::from(banks[0].1) + i64::from(banks[1].1)) / 2)
                .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                as i32,
            max_bank_height: banks[0].1.max(banks[1].1),
            bank_height_span: banks[0].1.abs_diff(banks[1].1).min(i32::MAX as u32) as i32,
            living_banks: banks.iter().filter(|bank| bank.2).count() as u8,
        })
    }

    fn hydrographic_focus_context(&self, x: i32, z: i32, radius: i32) -> HydrographicFocusContext {
        let mut open_water_probes = 0u8;
        let mut min_surface_height = i32::MAX;
        let mut max_surface_height = i32::MIN;

        for (dx, dz) in [
            (-2, -2),
            (-1, -2),
            (0, -2),
            (1, -2),
            (2, -2),
            (-2, -1),
            (2, -1),
            (-2, 0),
            (2, 0),
            (-2, 1),
            (2, 1),
            (-2, 2),
            (-1, 2),
            (0, 2),
            (1, 2),
            (2, 2),
        ] {
            let surface = self.surface_height_at(
                x.saturating_add(dx * radius / 2),
                z.saturating_add(dz * radius / 2),
            );
            open_water_probes = open_water_probes.saturating_add((surface <= WATER_LEVEL) as u8);
            min_surface_height = min_surface_height.min(surface);
            max_surface_height = max_surface_height.max(surface);
        }

        HydrographicFocusContext {
            open_water_probes,
            min_surface_height,
            max_surface_height,
        }
    }

    pub fn find_hydrographic_focus(
        &self,
        origin_x: i32,
        origin_z: i32,
        max_radius: i32,
    ) -> Option<IVec2> {
        let mut best: Option<(i64, IVec2)> = None;
        let step = HYDROGRAPHIC_SEARCH_STEP;
        let max_radius = bounded_search_radius(max_radius, step, HYDROGRAPHIC_SEARCH_MAX_RADIUS);
        let mut visited = 0usize;

        for radius in (0..=max_radius).step_by(step as usize) {
            let complete = visit_bounded_square_perimeter(
                origin_x,
                origin_z,
                radius,
                step,
                &mut visited,
                HYDROGRAPHIC_SEARCH_MAX_CANDIDATES,
                |x, z| {
                    let surface = self.surface_height_at(x, z);
                    if surface > WATER_LEVEL - 2 {
                        return;
                    }
                    let environment = self.environment_sample_for_surface(x, z, surface);
                    if environment.river_strength < 0.82 {
                        return;
                    }
                    let distance = (i64::from(x) - i64::from(origin_x))
                        .abs()
                        .max((i64::from(z) - i64::from(origin_z)).abs());
                    let strength_penalty =
                        i64::from(((1.0 - environment.river_strength) * 180.0) as i32);
                    let Some(cross_section) = self.hydrographic_focus_cross_section(x, z) else {
                        return;
                    };
                    if cross_section.living_banks < 2
                        || cross_section.max_bank_height > HYDROGRAPHIC_FOCUS_MAX_BANK_HEIGHT
                        || cross_section.bank_height_span > HYDROGRAPHIC_FOCUS_MAX_BANK_SPAN
                    {
                        return;
                    }
                    let context = self.hydrographic_focus_context(x, z, 64);
                    if context.open_water_probes > 2 {
                        // A winding inland channel normally exposes water in the
                        // two tangent directions, occasionally touching adjacent
                        // perimeter probes. A third ray exposes a coast,
                        // lake centre, or broad confluence rather than a corridor.
                        return;
                    }
                    if context.max_surface_height > HYDROGRAPHIC_FOCUS_MAX_CONTEXT_HEIGHT
                        || context.relief_span() > HYDROGRAPHIC_FOCUS_MAX_CONTEXT_RELIEF
                    {
                        // An anchor can be a hydrologically valid channel and
                        // still be a disastrous inspection route when a karst
                        // tower or canyon wall fills the camera corridor. Keep
                        // route discovery fail-closed instead of falling back
                        // to such a point merely because it is wet.
                        return;
                    }
                    let candidate = IVec2::new(x, z);
                    let width_penalty = i64::from((cross_section.width - 18).abs()) * 12;
                    let bank_relief =
                        i64::from(cross_section.mean_bank_height) - i64::from(WATER_LEVEL);
                    let relief_penalty = (bank_relief - 10).abs() * 3;
                    let context_relief_for_score = match self.terrain_grammar {
                        // V3's explicit bed/shelf rounding can move one context
                        // probe by one block without changing the scene's
                        // relief class. Four-block score bands preserve the
                        // established deterministic route anchor; the exact
                        // fail-closed height and relief caps above remain
                        // unquantized.
                        TerrainGrammarVersion::V3 => context.relief_span().div_euclid(4) * 4,
                        TerrainGrammarVersion::V1 | TerrainGrammarVersion::V2 => {
                            context.relief_span()
                        }
                    };
                    let context_relief_penalty = i64::from(context_relief_for_score) * 2;
                    let exposure_penalty = i64::from(context.open_water_probes) * 48;
                    let score = distance * 4
                        + strength_penalty
                        + width_penalty
                        + relief_penalty
                        + context_relief_penalty
                        + exposure_penalty;
                    if best.map_or(true, |(best_score, _)| score < best_score) {
                        best = Some((score, candidate));
                    }
                },
            );
            debug_assert!(
                complete,
                "hydrographic focus search exceeded its hard work cap"
            );
            if !complete {
                break;
            }

            if radius >= 512 && best.is_some() {
                break;
            }
        }

        best.map(|(_, point)| point)
    }

    pub fn surface_height_at(&self, wx: i32, wz: i32) -> i32 {
        self.surface_height(wx as f64, wz as f64).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::IVec3;
    use std::collections::HashSet;

    const NATURAL_BANK_PROFILE_RADIUS: i32 = 48;
    const NATURAL_STABLE_BANK_RUN: usize = 3;

    fn cardinal_surface_profile(
        generator: &TerrainGenerator,
        center: IVec2,
        axis: IVec2,
        radius: i32,
    ) -> Vec<i32> {
        (-radius..=radius)
            .map(|offset| {
                generator.surface_height_at(
                    center.x.saturating_add(axis.x.saturating_mul(offset)),
                    center.y.saturating_add(axis.y.saturating_mul(offset)),
                )
            })
            .collect()
    }

    fn center_connected_wet_span(heights: &[i32]) -> Option<(usize, usize)> {
        let center = heights.len() / 2;
        if heights.get(center).copied()? >= WATER_LEVEL {
            return None;
        }

        let mut start = center;
        while start > 0 && heights[start - 1] < WATER_LEVEL {
            start -= 1;
        }
        let mut end = center;
        while end + 1 < heights.len() && heights[end + 1] < WATER_LEVEL {
            end += 1;
        }
        Some((start, end))
    }

    fn bank_transition_max_rise(
        heights: &[i32],
        wet_span: (usize, usize),
        direction: isize,
    ) -> Option<u32> {
        debug_assert!(direction == -1 || direction == 1);
        let wet_edge = if direction < 0 {
            wet_span.0
        } else {
            wet_span.1
        };
        let mut index = wet_edge as isize + direction;
        let mut stable_run = 0usize;

        while (0..heights.len() as isize).contains(&index) {
            let height = heights[index as usize];
            if height > WATER_LEVEL + 1 {
                stable_run += 1;
            } else {
                stable_run = 0;
            }
            if stable_run == NATURAL_STABLE_BANK_RUN {
                let outer = index as usize;
                let start = wet_edge.min(outer);
                let end = wet_edge.max(outer);
                return heights[start..=end]
                    .windows(2)
                    .map(|pair| pair[0].abs_diff(pair[1]))
                    .max();
            }
            index += direction;
        }
        None
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct NaturalBankShelfMetrics {
        shelf_width: usize,
        max_adjacent_rise: u32,
        first_outer_height: i32,
    }

    fn natural_bank_shelf_metrics(
        heights: &[i32],
        wet_span: (usize, usize),
        direction: isize,
    ) -> Option<NaturalBankShelfMetrics> {
        debug_assert!(direction == -1 || direction == 1);
        let wet_edge = if direction < 0 {
            wet_span.0
        } else {
            wet_span.1
        };
        let mut index = wet_edge as isize + direction;
        let mut shelf_width = 0usize;
        while (0..heights.len() as isize).contains(&index)
            && heights[index as usize] == WATER_LEVEL + 1
        {
            shelf_width += 1;
            index += direction;
        }
        let first_outer_height = *heights.get(index as usize)?;
        Some(NaturalBankShelfMetrics {
            shelf_width,
            max_adjacent_rise: bank_transition_max_rise(heights, wet_span, direction)?,
            first_outer_height,
        })
    }

    fn recentered_default_river_slice(
        generator: &TerrainGenerator,
        focus: IVec2,
        tangent_offset: i32,
    ) -> Option<(Vec<i32>, (usize, usize))> {
        let z = focus.y.saturating_add(tangent_offset);
        let center_x = (-24i32..=24)
            .filter_map(|dx| {
                let x = focus.x.saturating_add(dx);
                let height = generator.surface_height_at(x, z);
                (height < WATER_LEVEL).then_some((height, dx.abs(), dx, x))
            })
            .min_by_key(|&(height, distance, dx, _)| (height, distance, dx))
            .map(|(_, _, _, x)| x)?;
        let heights = cardinal_surface_profile(
            generator,
            IVec2::new(center_x, z),
            IVec2::X,
            NATURAL_BANK_PROFILE_RADIUS,
        );
        let wet_span = center_connected_wet_span(&heights)?;
        Some((heights, wet_span))
    }

    fn voxel_fnv1a64(voxels: &[Voxel]) -> u64 {
        voxels
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |mut state, &voxel| {
                for byte in voxel.to_le_bytes() {
                    state ^= u64::from(byte);
                    state = state.wrapping_mul(0x0000_0100_0000_01b3);
                }
                state
            })
    }

    #[test]
    fn semantic_selector_admits_exactly_one_per_euclidean_supertile() {
        let edge_supertiles = [
            i64::MIN.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS),
            -2,
            -1,
            0,
            1,
            i64::MAX.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS),
        ];
        for seed in [0, 1, 2, u32::MAX] {
            for profile in [WorldProfile::Natural, WorldProfile::AstralFrontier] {
                for super_x in edge_supertiles {
                    for super_z in edge_supertiles {
                        let base_x = super_x
                            .checked_mul(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS)
                            .expect("edge supertile x base remains representable");
                        let base_z = super_z
                            .checked_mul(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS)
                            .expect("edge supertile z base remains representable");
                        let mut admitted = Vec::new();
                        for local_z in 0..FAR_SEMANTIC_COHORT_SUPERTILE_CELLS {
                            for local_x in 0..FAR_SEMANTIC_COHORT_SUPERTILE_CELLS {
                                let cell_x = base_x
                                    .checked_add(local_x)
                                    .expect("complete edge supertile x cell");
                                let cell_z = base_z
                                    .checked_add(local_z)
                                    .expect("complete edge supertile z cell");
                                let first =
                                    far_semantic_cohort_signature(seed, profile, cell_x, cell_z);
                                assert_eq!(
                                    first,
                                    far_semantic_cohort_signature(seed, profile, cell_x, cell_z),
                                    "semantic signatures must replay exactly"
                                );
                                if first.admitted {
                                    admitted.push((cell_x, cell_z));
                                }
                            }
                        }
                        assert_eq!(
                            admitted.len(),
                            1,
                            "one absolute cell must be selected in supertile ({super_x}, {super_z})"
                        );
                    }
                }
            }
        }

        for coordinate in [-9, -8, -1, 0, 7, 8, i64::MIN, i64::MAX] {
            let supertile = coordinate.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
            let remainder = coordinate.rem_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
            assert!((0..FAR_SEMANTIC_COHORT_SUPERTILE_CELLS).contains(&remainder));
            assert_eq!(
                i128::from(supertile) * i128::from(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS)
                    + i128::from(remainder),
                i128::from(coordinate)
            );
        }
    }

    #[test]
    fn semantic_61x61_window_never_exceeds_81_or_duplicates_a_supertile() {
        let starts = [
            -1_037_i64,
            -64,
            -63,
            -9,
            -8,
            -1,
            0,
            1,
            7,
            8,
            63,
            64,
            1_037,
            i64::MIN,
            i64::MAX - 60,
        ];
        for seed in [0, 2, u32::MAX] {
            for profile in [WorldProfile::Natural, WorldProfile::AstralFrontier] {
                for start_x in starts {
                    for start_z in starts {
                        let mut admitted_supertiles = HashSet::new();
                        let mut x_supertiles = HashSet::new();
                        let mut z_supertiles = HashSet::new();
                        for dz in 0..61_i64 {
                            for dx in 0..61_i64 {
                                let x = start_x.checked_add(dx).expect("bounded x window");
                                let z = start_z.checked_add(dz).expect("bounded z window");
                                let sx = x.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
                                let sz = z.div_euclid(FAR_SEMANTIC_COHORT_SUPERTILE_CELLS);
                                x_supertiles.insert(sx);
                                z_supertiles.insert(sz);
                                if far_semantic_cohort_signature(seed, profile, x, z).admitted {
                                    assert!(
                                        admitted_supertiles.insert((sx, sz)),
                                        "a window admitted two cells from one supertile"
                                    );
                                }
                            }
                        }
                        assert!(x_supertiles.len() <= 9);
                        assert!(z_supertiles.len() <= 9);
                        assert!(admitted_supertiles.len() <= 81);
                    }
                }
            }
        }
    }

    #[test]
    fn coarse_surface_family_agrees_with_near_base_and_slope_rules() {
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
            let (base_top, base_sub, _) = TerrainGenerator::blocks_for(biome);
            for slope in [0, 1, 2, 3, 4, 8] {
                let near =
                    TerrainGenerator::slope_surface_layers(biome, slope, base_top, base_sub).0;
                assert_eq!(
                    coarse_surface_family(biome, slope as f32),
                    near,
                    "coarse family drifted from near terrain for {biome:?} at slope {slope}"
                );
            }
        }

        assert_eq!(
            coarse_surface_family(Biome::Plains, f32::NAN),
            BlockType::Grass
        );
        assert_eq!(
            coarse_surface_family(Biome::Karst, f32::INFINITY),
            BlockType::MossStone
        );
    }

    #[test]
    fn public_square_searches_have_exact_radius_and_candidate_caps() {
        let cases = [
            (
                NEON_SPAWN_SEARCH_STEP,
                NEON_SPAWN_SEARCH_MAX_RADIUS,
                NEON_SPAWN_SEARCH_MAX_CANDIDATES,
            ),
            (
                NATURAL_SPAWN_SEARCH_STEP,
                NATURAL_SPAWN_SEARCH_MAX_RADIUS,
                NATURAL_SPAWN_SEARCH_MAX_CANDIDATES,
            ),
            (
                HYDROGRAPHIC_SEARCH_STEP,
                HYDROGRAPHIC_SEARCH_MAX_RADIUS,
                HYDROGRAPHIC_SEARCH_MAX_CANDIDATES,
            ),
        ];

        for (step, hard_radius, hard_candidates) in cases {
            assert_eq!(
                bounded_search_radius(i32::MAX, step, hard_radius),
                hard_radius
            );
            assert_eq!(bounded_search_radius(i32::MIN, step, hard_radius), step);

            let mut visited = 0usize;
            for radius in (0..=hard_radius).step_by(step as usize) {
                assert!(visit_bounded_square_perimeter(
                    -17,
                    23,
                    radius,
                    step,
                    &mut visited,
                    hard_candidates,
                    |_, _| {},
                ));
            }
            assert_eq!(visited, hard_candidates);
            assert!(!visit_bounded_square_perimeter(
                -17,
                23,
                hard_radius.saturating_add(step),
                step,
                &mut visited,
                hard_candidates,
                |_, _| {},
            ));
            assert_eq!(visited, hard_candidates);

            for (origin_x, origin_z) in [
                (i32::MIN, i32::MIN),
                (i32::MIN, i32::MAX),
                (i32::MAX, i32::MIN),
                (i32::MAX, i32::MAX),
            ] {
                let mut edge_visited = 0usize;
                let mut yielded = 0usize;
                for radius in (0..=hard_radius).step_by(step as usize) {
                    assert!(visit_bounded_square_perimeter(
                        origin_x,
                        origin_z,
                        radius,
                        step,
                        &mut edge_visited,
                        hard_candidates,
                        |_, _| {
                            yielded += 1;
                        },
                    ));
                }
                assert_eq!(edge_visited, hard_candidates);
                assert!(yielded > 0);
                assert!(yielded <= edge_visited);
            }
        }
    }

    #[test]
    fn public_spawn_and_hydro_searches_do_not_overflow_at_i32_edges() {
        let generator = TerrainGenerator::new(0x51A7_EE11);
        for (origin_x, origin_z) in [
            (i32::MIN, i32::MIN),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MAX, i32::MAX),
        ] {
            let _ = generator.find_neon_showcase_spawn(origin_x, origin_z, 0);
            let _ = generator.find_natural_spawn(origin_x, origin_z, 0);
            let _ = generator.find_hydrographic_focus(origin_x, origin_z, 0);
        }
    }

    #[test]
    fn astral_layout_rotation_and_surface_cache_fail_safe_at_coordinate_edges() {
        let layout = AstralFrontierLayout {
            hub: IVec2::new(64, -64),
            quarter_turns: 0,
        };
        let edge_points = [
            IVec2::new(i32::MIN, i32::MIN),
            IVec2::new(i32::MIN, i32::MAX),
            IVec2::new(i32::MAX, i32::MIN),
            IVec2::new(i32::MAX, i32::MAX),
        ];
        for turns in 0..4 {
            for point in edge_points {
                let rotated = AstralFrontierLayout::rotate_quarters(point, turns);
                let replay = AstralFrontierLayout::rotate_quarters(point, turns);
                assert_eq!(rotated, replay);
                let _ = layout.local_from_world(point);
                let _ = layout.world_from_local(point);
            }
        }

        let generator =
            TerrainGenerator::new(0xA57A_11ED).with_world_profile(WorldProfile::AstralFrontier);
        for (cx, cz) in [
            (i32::MIN, i32::MIN),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MAX, i32::MAX),
        ] {
            let grid = TerrainSurfaceGrid::build(&generator, cx, cz);
            assert_eq!(
                grid.samples.len(),
                crate::voxel_budget::CACHED_SURFACE_SAMPLES_PER_CHUNK
            );
            assert!(grid
                .samples
                .iter()
                .all(|(height, continentalness)| (8..=208).contains(height)
                    && continentalness.is_finite()));
        }
    }

    #[test]
    fn full_generation_has_an_explicit_edge_domain_and_rejects_outside_chunks_as_air() {
        use crate::blocks::{CUSTOM_MATERIAL_BASE, DEFAULT_MATERIAL};
        use crate::settings::SceneryQuality;

        fn assert_uniform_flags_match_payload(chunk: &Chunk, case: &str) {
            let voxels = chunk.voxels_shared();
            let first = voxels[0];
            let uniform = voxels.iter().all(|voxel| *voxel == first);
            assert_eq!(
                chunk.is_empty,
                uniform && first == AIR,
                "is_empty drifted from the payload for {case}"
            );
            assert_eq!(
                chunk.is_uniform_solid,
                uniform && first != AIR,
                "is_uniform_solid drifted from the payload for {case}"
            );
            assert_eq!(
                chunk.uniform_voxel,
                if uniform { first } else { AIR },
                "uniform_voxel drifted from the payload for {case}"
            );
        }

        let minimum_origin = i64::from(i32::MIN) + TERRAIN_GENERATION_COORDINATE_MARGIN;
        let maximum_origin = i64::from(i32::MAX)
            - TERRAIN_GENERATION_COORDINATE_MARGIN
            - i64::from(CHUNK_SIZE_I - 1);
        let minimum_axis = ((minimum_origin + i64::from(CHUNK_SIZE_I - 1))
            .div_euclid(i64::from(CHUNK_SIZE_I))) as i32;
        let maximum_axis = maximum_origin.div_euclid(i64::from(CHUNK_SIZE_I)) as i32;

        let minimum_valid_origin = i64::from(minimum_axis) * i64::from(CHUNK_SIZE_I);
        let maximum_valid_origin = i64::from(maximum_axis) * i64::from(CHUNK_SIZE_I);
        assert!(minimum_valid_origin >= minimum_origin);
        assert!(minimum_valid_origin - i64::from(CHUNK_SIZE_I) < minimum_origin);
        assert!(maximum_valid_origin <= maximum_origin);
        assert!(maximum_valid_origin + i64::from(CHUNK_SIZE_I) > maximum_origin);

        let valid_positions = [
            // The two valid faces of every independent axis.
            ChunkPos::new(minimum_axis, 0, 0),
            ChunkPos::new(maximum_axis, 0, 0),
            ChunkPos::new(0, minimum_axis, 0),
            ChunkPos::new(0, maximum_axis, 0),
            ChunkPos::new(0, 0, minimum_axis),
            ChunkPos::new(0, 0, maximum_axis),
            // Combined horizontal extremes exercise the four X/Z corners.
            ChunkPos::new(minimum_axis, 0, minimum_axis),
            ChunkPos::new(minimum_axis, 0, maximum_axis),
            ChunkPos::new(maximum_axis, 0, minimum_axis),
            ChunkPos::new(maximum_axis, 0, maximum_axis),
        ];
        let invalid_positions = [
            ChunkPos::new(minimum_axis - 1, 0, 0),
            ChunkPos::new(maximum_axis + 1, 0, 0),
            ChunkPos::new(0, minimum_axis - 1, 0),
            ChunkPos::new(0, maximum_axis + 1, 0),
            ChunkPos::new(0, 0, minimum_axis - 1),
            ChunkPos::new(0, 0, maximum_axis + 1),
        ];

        for position in valid_positions {
            assert!(
                checked_terrain_chunk_origins(position).is_some(),
                "expected {position:?} to be the last supported chunk"
            );
        }
        for position in invalid_positions {
            assert!(
                checked_terrain_chunk_origins(position).is_none(),
                "expected {position:?} to be the first rejected chunk"
            );
        }

        let generators = [
            ("natural-balanced", TerrainGenerator::new(0xE6E0_0001)),
            (
                "astral-off",
                TerrainGenerator::new(0xE6E0_0002)
                    .with_world_profile(WorldProfile::AstralFrontier)
                    .with_scenery_quality(SceneryQuality::Off),
            ),
            (
                "astral-lush",
                TerrainGenerator::new(0xE6E0_0003)
                    .with_world_profile(WorldProfile::AstralFrontier)
                    .with_scenery_quality(SceneryQuality::Lush),
            ),
        ];

        for (generator_name, generator) in &generators {
            for position in valid_positions {
                let mut boundary = Chunk::new(position);
                generator.generate(&mut boundary);
                let case = format!("{generator_name} valid {position:?}");
                assert_eq!(
                    boundary.voxels_shared().len(),
                    crate::chunk::CHUNK_VOLUME,
                    "voxel payload length changed for {case}"
                );
                assert_eq!(
                    boundary.materials_shared().len(),
                    crate::chunk::CHUNK_VOLUME,
                    "material payload length changed for {case}"
                );
                assert_uniform_flags_match_payload(&boundary, &case);
            }

            let stale_voxel: Voxel = BlockType::Stone.into();
            let stale_voxels: crate::chunk::SharedVoxels =
                std::sync::Arc::new([stale_voxel; crate::chunk::CHUNK_VOLUME]);
            let stale_materials: crate::chunk::SharedMaterials =
                std::sync::Arc::new([CUSTOM_MATERIAL_BASE; crate::chunk::CHUNK_VOLUME]);
            let mut rejected = Chunk::new(invalid_positions[0]);

            for position in invalid_positions {
                // Reuse both the Chunk object and identical stale payload
                // buffers. Rejection must replace every authoritative byte
                // and recompute all cached flags on every axis and side.
                rejected.pos = position;
                rejected
                    .install_voxels_and_materials(stale_voxels.clone(), stale_materials.clone());
                rejected.dirty = false;
                assert!(!rejected.is_empty);
                assert!(rejected.is_uniform_solid);
                assert_eq!(rejected.uniform_voxel, stale_voxel);

                generator.generate(&mut rejected);

                let case = format!("{generator_name} rejected {position:?}");
                assert!(rejected.dirty, "rejection did not dirty {case}");
                assert_uniform_flags_match_payload(&rejected, &case);
                assert!(
                    rejected.voxels_shared().iter().all(|voxel| *voxel == AIR),
                    "stale voxel survived {case}"
                );
                assert!(
                    rejected
                        .materials_shared()
                        .iter()
                        .all(|material| *material == DEFAULT_MATERIAL),
                    "stale custom material survived {case}"
                );
            }
        }
    }

    #[test]
    fn surface_grid_matches_canonical_height_and_slope_exactly() {
        let seeds = [0_u32, 12_345, u32::MAX - 17];
        let chunks = [(-11, 7), (-1, -1), (0, 0), (13, -9)];

        for seed in seeds {
            let generator = TerrainGenerator::new(seed);
            for (cx, cz) in chunks {
                let grid = TerrainSurfaceGrid::build(&generator, cx, cz);
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let wx = cx * CHUNK_SIZE_I + lx as i32;
                        let wz = cz * CHUNK_SIZE_I + lz as i32;
                        let direct = generator.surface_height(wx as f64, wz as f64);
                        assert_eq!(grid.sample(lx, lz), direct);

                        let cached_neighbours = [
                            grid.sample_offset(lx, lz, 0, -1),
                            grid.sample_offset(lx, lz, 0, 1),
                            grid.sample_offset(lx, lz, 1, 0),
                            grid.sample_offset(lx, lz, -1, 0),
                        ];
                        let direct_neighbours = [
                            generator.surface_height(wx as f64, (wz - 1) as f64),
                            generator.surface_height(wx as f64, (wz + 1) as f64),
                            generator.surface_height((wx + 1) as f64, wz as f64),
                            generator.surface_height((wx - 1) as f64, wz as f64),
                        ];
                        assert_eq!(cached_neighbours, direct_neighbours);

                        let cached_slope = cached_neighbours
                            .iter()
                            .map(|(height, _)| (direct.0 - *height).abs())
                            .max()
                            .unwrap_or(0);
                        let direct_slope = direct_neighbours
                            .iter()
                            .map(|(height, _)| (direct.0 - *height).abs())
                            .max()
                            .unwrap_or(0);
                        assert_eq!(cached_slope, direct_slope);
                    }
                }
            }
        }
    }

    fn paint_tree_for_test(
        generator: &TerrainGenerator,
        biome: Biome,
        style_roll: f64,
        base_y: i32,
    ) -> (TreeProfile, Vec<(i32, i32, i32, Voxel)>) {
        let (profile, leaf_kind) = generator
            .tree_profile(biome, style_roll)
            .expect("quality and biome should produce a tree profile");
        let blocks = paint_specific_tree_for_test(generator, profile, leaf_kind, base_y);
        (profile, blocks)
    }

    fn paint_specific_tree_for_test(
        generator: &TerrainGenerator,
        profile: TreeProfile,
        leaf_kind: BlockType,
        base_y: i32,
    ) -> Vec<(i32, i32, i32, Voxel)> {
        let first_cy = (base_y - 1).div_euclid(CHUNK_SIZE_I);
        let last_cy = (base_y + profile.total_height() - 1).div_euclid(CHUNK_SIZE_I);
        let mut blocks = Vec::new();

        for cy in first_cy..=last_cy {
            let origin_y = cy * CHUNK_SIZE_I;
            let mut chunk = Chunk::new(ChunkPos::new(0, cy, 0));
            if (base_y - 1).div_euclid(CHUNK_SIZE_I) == cy {
                chunk.set(
                    8,
                    (base_y - 1 - origin_y) as usize,
                    8,
                    BlockType::Grass.into(),
                );
            }
            assert!(generator
                .try_place_bonsai_tree(&mut chunk, 8, 8, base_y, origin_y, profile, leaf_kind,));

            for ly in 0..CHUNK_SIZE {
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let voxel = chunk.get(lx, ly, lz);
                        if matches!(
                            BlockType::from_voxel(voxel),
                            BlockType::Wood
                                | BlockType::Leaves
                                | BlockType::JungleLeaves
                                | BlockType::BlossomLeaves
                        ) {
                            blocks.push((lx as i32, origin_y + ly as i32, lz as i32, voxel));
                        }
                    }
                }
            }
        }

        blocks
    }

    #[test]
    fn understory_profiles_form_biome_specific_layers_not_global_scatter() {
        assert_eq!(
            understory_profile(Biome::Jungle, 0.5),
            Some((BlockType::JungleLeaves, 0.90))
        );
        assert_eq!(
            understory_profile(Biome::Savanna, 0.5),
            Some((BlockType::Leaves, 0.24))
        );
        assert_eq!(
            understory_profile(Biome::Forest, 0.05).map(|profile| profile.0),
            Some(BlockType::BlossomLeaves)
        );
        assert_eq!(
            understory_profile(Biome::Forest, 0.5).map(|profile| profile.0),
            Some(BlockType::Leaves)
        );
        for biome in [
            Biome::Ocean,
            Biome::Beach,
            Biome::Desert,
            Biome::Tundra,
            Biome::SnowyMountains,
            Biome::Mountains,
            Biome::Mesa,
            Biome::CrystalSpires,
            Biome::VolcanicWaste,
            Biome::GlacierShards,
            Biome::AlienReef,
        ] {
            assert_eq!(understory_profile(biome, 0.5), None, "{biome:?}");
        }
    }

    #[test]
    fn understory_colony_gate_has_open_ground_and_dense_thicket_extremes() {
        assert_eq!(understory_colony_gate(0.0, 0.0), 0.0);
        assert_eq!(understory_colony_gate(1.0, 1.0), 1.0);
        let middle = understory_colony_gate(0.55, 0.55);
        assert!(
            middle > 0.05 && middle < 0.70,
            "middle habitat should feather a colony edge, got {middle:.3}"
        );
    }

    #[test]
    fn stand_ecology_is_deterministic_bounded_and_creates_real_density_range() {
        let generator = TerrainGenerator::new(93_7421)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;

        for z in (-2_048..=2_048).step_by(137) {
            for x in (-2_048..=2_048).step_by(149) {
                let surface = generator.surface_height_at(x, z);
                let value = generator.tree_habitat_multiplier(Biome::Forest, x, z, surface);
                assert!((0.0..=1.45).contains(&value));
                assert_eq!(
                    value,
                    generator.tree_habitat_multiplier(Biome::Forest, x, z, surface)
                );
                minimum = minimum.min(value);
                maximum = maximum.max(value);
            }
        }

        assert!(
            maximum - minimum > 0.55,
            "forest stands need dense cohorts and genuine openings, range was {minimum:.3}..{maximum:.3}"
        );
        assert!(
            minimum < 0.05,
            "weak forest habitat must become a real light well, minimum was {minimum:.3}"
        );
        assert!(
            maximum > 1.20,
            "strong forest habitat must still support a dense cohort, maximum was {maximum:.3}"
        );
    }

    #[test]
    fn blossom_probability_forms_groves_instead_of_global_pink_forests() {
        use crate::settings::SceneryQuality;

        assert_eq!(
            flowering_canopy_chance(SceneryQuality::Lush, Biome::Desert, 1.0),
            0.0
        );
        assert_eq!(
            flowering_canopy_chance(SceneryQuality::Lean, Biome::Forest, 1.0),
            0.0
        );
        let lush_outside = flowering_canopy_chance(SceneryQuality::Lush, Biome::Forest, 0.0);
        let lush_grove = flowering_canopy_chance(SceneryQuality::Lush, Biome::Forest, 1.0);
        let balanced_grove = flowering_canopy_chance(SceneryQuality::Balanced, Biome::Forest, 1.0);
        assert!(lush_outside < 0.10);
        assert!(lush_grove > 0.60 && lush_grove < 0.70);
        assert!(balanced_grove < lush_grove);
    }

    #[test]
    fn understory_patch_is_grounded_connected_non_solid_and_chunk_local() {
        let generator = TerrainGenerator::new(93_7421)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let center = IVec2::new(8, 8);
        let centre_surface = generator.surface_height(center.x as f64, center.y as f64).0;
        let cy = centre_surface.div_euclid(CHUNK_SIZE_I);
        let origin_y = cy * CHUNK_SIZE_I;
        let mut chunk = Chunk::new(ChunkPos::new(0, cy, 0));

        for dz in -2..=2 {
            for dx in -2..=2 {
                let wx = center.x + dx;
                let wz = center.y + dz;
                let (surface, _) = generator.surface_height(wx as f64, wz as f64);
                let local_y = surface - origin_y;
                if (0..CHUNK_SIZE_I).contains(&local_y) {
                    chunk.set(
                        wx as usize,
                        local_y as usize,
                        wz as usize,
                        BlockType::Grass.into(),
                    );
                }
            }
        }

        let placed = generator.try_place_understory_patch(
            &mut chunk,
            center,
            origin_y,
            2,
            BlockType::Leaves,
            0,
        );
        assert!(placed >= 5, "expected an irregular colony, got {placed}");
        assert!(!BlockType::Leaves.is_solid());

        let mut foliage = HashSet::new();
        for ly in 0..CHUNK_SIZE {
            for lz in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    if chunk.get(lx, ly, lz) == Voxel::from(BlockType::Leaves) {
                        foliage.insert(IVec3::new(lx as i32, ly as i32, lz as i32));
                    }
                }
            }
        }
        assert_eq!(foliage.len(), placed);
        assert!(foliage
            .iter()
            .all(|cell| { cell.x >= 6 && cell.x <= 10 && cell.z >= 6 && cell.z <= 10 }));
        assert!(foliage.iter().all(|cell| {
            let below = *cell - IVec3::Y;
            chunk.get(below.x as usize, below.y as usize, below.z as usize) != AIR
        }));

        let first = *foliage.iter().next().expect("foliage");
        let mut visited = HashSet::from([first]);
        let mut stack = vec![first];
        while let Some(cell) = stack.pop() {
            for step in [
                IVec3::X,
                -IVec3::X,
                IVec3::Y,
                -IVec3::Y,
                IVec3::Z,
                -IVec3::Z,
            ] {
                let neighbour = cell + step;
                if foliage.contains(&neighbour) && visited.insert(neighbour) {
                    stack.push(neighbour);
                }
            }
        }
        assert_eq!(visited, foliage, "bush foliage must be one editable object");
    }

    #[test]
    fn compact_understory_has_a_supported_vertical_shoot_not_a_flat_plus_sign() {
        let generator = TerrainGenerator::new(93_7421)
            .with_scenery_quality(crate::settings::SceneryQuality::Balanced);
        let center = IVec2::new(8, 8);
        let (surface, _) = generator.surface_height(center.x as f64, center.y as f64);
        let cy = surface.div_euclid(CHUNK_SIZE_I);
        let origin_y = cy * CHUNK_SIZE_I;
        let mut chunk = Chunk::new(ChunkPos::new(0, cy, 0));

        for dz in -1..=1 {
            for dx in -1..=1 {
                let wx = center.x + dx;
                let wz = center.y + dz;
                let (ground, _) = generator.surface_height(wx as f64, wz as f64);
                let local_y = ground - origin_y;
                if (0..CHUNK_SIZE_I).contains(&local_y) {
                    chunk.set(
                        wx as usize,
                        local_y as usize,
                        wz as usize,
                        BlockType::Grass.into(),
                    );
                }
            }
        }

        let placed = generator.try_place_understory_patch(
            &mut chunk,
            center,
            origin_y,
            1,
            BlockType::Leaves,
            17,
        );
        let centre_ground_y = surface - origin_y;
        assert!(placed >= 2);
        assert_eq!(
            chunk.get(8, (centre_ground_y + 1) as usize, 8),
            Voxel::from(BlockType::Leaves)
        );
        assert_eq!(
            chunk.get(8, (centre_ground_y + 2) as usize, 8),
            Voxel::from(BlockType::Leaves),
            "compact shrubs need a vertical silhouette"
        );
        let raised_count = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .filter(|&(dx, dz)| {
                let wx = center.x + dx;
                let wz = center.y + dz;
                let (local_surface, _) = generator.surface_height(wx as f64, wz as f64);
                let local_raised_y = local_surface - origin_y + 2;
                (0..CHUNK_SIZE_I).contains(&local_raised_y)
                    && chunk.get(wx as usize, local_raised_y as usize, wz as usize)
                        == Voxel::from(BlockType::Leaves)
            })
            .count();
        assert_eq!(
            raised_count, 1,
            "compact shrubs need one central shoot above an asymmetric footprint"
        );
    }

    #[test]
    fn environmental_sample_is_deterministic_bounded_and_has_unit_flow() {
        let generator = TerrainGenerator::new(0x48D2_09A1);
        for (x, z) in [
            (0, 0),
            (15, 16),
            (-17, -33),
            (1_024, -2_048),
            (-12_345, 23_456),
        ] {
            let first = generator.environment_sample_at(x, z);
            let second = generator.environment_sample_at(x, z);
            assert_eq!(first, second, "environment changed at {x},{z}");
            for value in [
                first.temperature_norm,
                first.atmospheric_moisture,
                first.soil_moisture,
                first.river_strength,
                first.mineral_resonance,
                first.flowering_resonance,
            ] {
                assert!(value.is_finite() && (0.0..=1.0).contains(&value));
            }
            let flow_length = (first.flow_direction[0] * first.flow_direction[0]
                + first.flow_direction[1] * first.flow_direction[1])
                .sqrt();
            assert!(
                (flow_length - 1.0).abs() < 1.0e-3 || flow_length == 0.0,
                "flow tangent must be normalized, got {flow_length}"
            );
        }
    }

    #[test]
    fn hydrographic_course_is_continuous_at_chunk_boundaries() {
        let generator = TerrainGenerator::new(0x48D2_09A1);
        for seam_x in [-512, -16, 0, 16, 512] {
            for z in (-640..=640).step_by(97) {
                let left = generator.major_river_axis(seam_x as f64 - 0.001, z as f64);
                let right = generator.major_river_axis(seam_x as f64 + 0.001, z as f64);
                assert!(
                    (left - right).abs() < 0.001,
                    "river field jumped at chunk seam x={seam_x}, z={z}: {left} -> {right}"
                );
            }
        }
    }

    #[test]
    fn strong_lowland_course_carves_a_real_water_filled_river_bed() {
        let generator = TerrainGenerator::new(0x48D2_09A1)
            .with_scenery_quality(crate::settings::SceneryQuality::Off);
        let bounded_focus = generator
            .find_hydrographic_focus(0, 0, 4_096)
            .expect("bounded river focus should find a cinematic course");
        let focus_environment = generator.environment_sample_at(bounded_focus.x, bounded_focus.y);
        assert!(focus_environment.river_strength >= 0.82);
        assert!(generator.surface_height_at(bounded_focus.x, bounded_focus.y) <= WATER_LEVEL - 2);
        let mut riparian_bank = None;
        'bank: for radius in 3..=64 {
            for (dx, dz) in [(radius, 0), (-radius, 0), (0, radius), (0, -radius)] {
                let x = bounded_focus.x + dx;
                let z = bounded_focus.y + dz;
                let surface = generator.surface_height_at(x, z);
                let hydro =
                    generator.hydrographic_field_for_surface(x as f64, z as f64, surface as f64);
                if surface > WATER_LEVEL + 2 && hydro.corridor > 0.16 {
                    riparian_bank = Some((x, z, generator.biome_at(x, z)));
                    break 'bank;
                }
            }
        }
        let (bank_x, bank_z, bank_biome) =
            riparian_bank.expect("strong river should expose a living high bank");
        assert!(
            matches!(bank_biome, Biome::Forest | Biome::Jungle | Biome::Tundra),
            "river bank at {bank_x},{bank_z} should be green, got {bank_biome:?}"
        );
        let bank_surface = generator.surface_height_at(bank_x, bank_z);
        let bank_chunk_pos = ChunkPos::new(
            bank_x.div_euclid(CHUNK_SIZE_I),
            bank_surface.div_euclid(CHUNK_SIZE_I),
            bank_z.div_euclid(CHUNK_SIZE_I),
        );
        let mut bank_chunk = Chunk::new(bank_chunk_pos);
        generator.generate(&mut bank_chunk);
        let bank_lx = bank_x.rem_euclid(CHUNK_SIZE_I) as usize;
        let bank_lz = bank_z.rem_euclid(CHUNK_SIZE_I) as usize;
        let bank_ly = bank_surface.rem_euclid(CHUNK_SIZE_I) as usize;
        assert_ne!(
            BlockType::from_voxel(bank_chunk.get(bank_lx, bank_ly, bank_lz)),
            BlockType::Gravel,
            "living river shoulder should not expose a gravel barcode"
        );
        if bank_ly > 0 {
            assert_eq!(
                BlockType::from_voxel(bank_chunk.get(bank_lx, bank_ly - 1, bank_lz)),
                BlockType::Dirt,
                "riparian shallow layer should remain continuous soil"
            );
        }
        let mut hero = None;
        'search: for z in (-1_024..=1_024).step_by(8) {
            for x in (-1_024..=1_024).step_by(8) {
                let environment = generator.environment_sample_at(x, z);
                let surface = generator.surface_height_at(x, z);
                if environment.river_strength > 0.92 && surface <= WATER_LEVEL - 2 {
                    hero = Some((x, z, surface, environment));
                    break 'search;
                }
            }
        }
        let (x, z, surface, environment) = hero.expect("expected a strong lowland river course");
        assert!(surface <= WATER_LEVEL - 2);
        assert!(
            environment.soil_moisture + f32::EPSILON >= environment.atmospheric_moisture * 0.68,
            "river corridor should not dry the soil"
        );

        let pos = ChunkPos::new(
            x.div_euclid(CHUNK_SIZE_I),
            WATER_LEVEL.div_euclid(CHUNK_SIZE_I),
            z.div_euclid(CHUNK_SIZE_I),
        );
        let mut chunk = Chunk::new(pos);
        generator.generate(&mut chunk);
        let lx = x.rem_euclid(CHUNK_SIZE_I) as usize;
        let ly = WATER_LEVEL.rem_euclid(CHUNK_SIZE_I) as usize;
        let lz = z.rem_euclid(CHUNK_SIZE_I) as usize;
        assert_eq!(
            chunk.get(lx, ly, lz),
            Voxel::from(BlockType::Water),
            "carved course at {x},{z} must contain visible water"
        );
    }

    #[test]
    fn hydrographic_focus_prefers_two_living_inland_banks_over_open_water() {
        let generator = TerrainGenerator::new(0x48D2_09A1)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let focus = generator
            .find_hydrographic_focus(0, 0, 4_096)
            .expect("seed should expose a bounded cinematic river");
        let cross_section = generator
            .hydrographic_focus_cross_section(focus.x, focus.y)
            .expect("chosen river focus should have both banks inside the inspection span");

        assert_eq!(cross_section.living_banks, 2);
        assert!((4..=96).contains(&cross_section.width));
        assert!(cross_section.mean_bank_height > WATER_LEVEL + 1);
        assert!(cross_section.max_bank_height <= HYDROGRAPHIC_FOCUS_MAX_BANK_HEIGHT);
        assert!(cross_section.bank_height_span <= HYDROGRAPHIC_FOCUS_MAX_BANK_SPAN);
        let context = generator.hydrographic_focus_context(focus.x, focus.y, 64);
        assert!(context.open_water_probes <= 2);
        assert!(context.max_surface_height <= HYDROGRAPHIC_FOCUS_MAX_CONTEXT_HEIGHT);
        assert!(context.relief_span() <= HYDROGRAPHIC_FOCUS_MAX_CONTEXT_RELIEF);
        assert!(
            generator
                .environment_sample_at(focus.x, focus.y)
                .river_strength
                >= 0.82
        );
    }

    #[test]
    fn natural_bank_envelope_is_total_bounded_and_monotone() {
        let bed_height = WATER_LEVEL as f64 - 2.0;
        let heights = [
            f64::NAN,
            f64::NEG_INFINITY,
            f64::INFINITY,
            -f64::MAX,
            42.0,
            43.0,
            43.25,
            46.0,
            52.0,
            80.0,
            208.0,
            f64::MAX,
        ];
        let weights = [
            f64::NAN,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            0.18,
            0.5,
            0.78,
            1.0,
            2.0,
            f64::INFINITY,
        ];

        for height in heights {
            for corridor in weights {
                for channel in weights {
                    let hydro = HydrographicField { corridor, channel };
                    let result = natural_hydrographic_cross_section_v2(height, hydro);
                    let replay = natural_hydrographic_cross_section_v2(height, hydro);
                    assert!(
                        result.is_finite(),
                        "non-finite result for height={height}, corridor={corridor}, channel={channel}"
                    );
                    assert_eq!(result.to_bits(), replay.to_bits());
                    if !height.is_finite() {
                        assert_eq!(result.to_bits(), bed_height.to_bits());
                    } else {
                        assert!(result <= height);
                        if !corridor.is_finite() || corridor <= 0.0 {
                            assert_eq!(result.to_bits(), height.to_bits());
                        }
                    }
                }
            }
        }

        assert_eq!(
            natural_hydrographic_cross_section_v2(
                80.0,
                HydrographicField {
                    corridor: 1.0,
                    channel: 1.0,
                },
            )
            .to_bits(),
            46.0f64.to_bits()
        );
        assert_eq!(
            natural_hydrographic_cross_section_v2(
                80.0,
                HydrographicField {
                    corridor: 1.0,
                    channel: 0.0,
                },
            )
            .to_bits(),
            52.0f64.to_bits()
        );

        for height in [44.0, 46.0, 52.0, 80.0, 208.0] {
            for channel in [0.0, 0.18, 0.5, 0.78, 1.0] {
                let mut previous = height;
                for corridor in [0.0, 0.04, 0.16, 0.36, 0.64, 1.0] {
                    let current = natural_hydrographic_cross_section_v2(
                        height,
                        HydrographicField { corridor, channel },
                    );
                    assert!(
                        current <= previous + 1.0e-12,
                        "corridor monotonicity failed: height={height}, channel={channel}, corridor={corridor}, {previous} -> {current}"
                    );
                    previous = current;
                }
            }
            for corridor in [0.04, 0.16, 0.36, 0.64, 1.0] {
                let mut previous = height;
                for channel in [0.0, 0.18, 0.5, 0.78, 1.0] {
                    let current = natural_hydrographic_cross_section_v2(
                        height,
                        HydrographicField { corridor, channel },
                    );
                    assert!(
                        current <= previous + 1.0e-12,
                        "channel monotonicity failed: height={height}, corridor={corridor}, channel={channel}, {previous} -> {current}"
                    );
                    previous = current;
                }
            }
        }
    }

    #[test]
    fn natural_v3_bank_envelope_is_total_bounded_monotone_and_three_zone() {
        let bed_height = WATER_LEVEL as f64 - 2.0;
        let heights = [
            f64::NAN,
            f64::NEG_INFINITY,
            f64::INFINITY,
            -f64::MAX,
            42.0,
            43.0,
            43.25,
            46.0,
            51.0,
            80.0,
            208.0,
            f64::MAX,
        ];
        let weights = [
            f64::NAN,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            0.18,
            0.5,
            0.78,
            1.0,
            2.0,
            f64::INFINITY,
        ];

        for height in heights {
            for corridor in weights {
                for channel in weights {
                    let hydro = HydrographicField { corridor, channel };
                    let result = natural_hydrographic_cross_section_v3(height, hydro);
                    let replay = natural_hydrographic_cross_section_v3(height, hydro);
                    assert!(
                        result.is_finite(),
                        "non-finite V3 result for height={height}, corridor={corridor}, channel={channel}"
                    );
                    assert_eq!(result.to_bits(), replay.to_bits());
                    if !height.is_finite() {
                        assert_eq!(result.to_bits(), bed_height.to_bits());
                    } else {
                        assert!(result <= height);
                        if !corridor.is_finite() || corridor <= 0.0 {
                            assert_eq!(result.to_bits(), height.to_bits());
                        }
                    }
                }
            }
        }

        for (channel, expected_height) in [(0.0, 51.0), (0.50, 49.0), (0.66, 49.0), (1.0, 46.0)] {
            let expected_height: f64 = expected_height;
            assert_eq!(
                natural_hydrographic_cross_section_v3(
                    80.0,
                    HydrographicField {
                        corridor: 1.0,
                        channel,
                    },
                )
                .to_bits(),
                expected_height.to_bits(),
                "V3 three-zone anchor drifted for channel={channel}"
            );
        }

        for height in [44.0, 46.0, 51.0, 80.0, 208.0] {
            for channel in [0.0, 0.26, 0.38, 0.50, 0.66, 0.78, 0.90, 1.0] {
                let mut previous = height;
                for corridor in [0.0, 0.04, 0.16, 0.36, 0.64, 1.0] {
                    let current = natural_hydrographic_cross_section_v3(
                        height,
                        HydrographicField { corridor, channel },
                    );
                    assert!(
                        current <= previous + 1.0e-12,
                        "V3 corridor monotonicity failed: height={height}, channel={channel}, corridor={corridor}, {previous} -> {current}"
                    );
                    previous = current;
                }
            }
            for corridor in [0.04, 0.16, 0.36, 0.64, 1.0] {
                let mut previous = height;
                for channel in [0.0, 0.26, 0.38, 0.50, 0.66, 0.78, 0.90, 1.0] {
                    let current = natural_hydrographic_cross_section_v3(
                        height,
                        HydrographicField { corridor, channel },
                    );
                    assert!(
                        current <= previous + 1.0e-12,
                        "V3 channel monotonicity failed: height={height}, corridor={corridor}, channel={channel}, {previous} -> {current}"
                    );
                    previous = current;
                }
            }
        }
    }

    #[test]
    fn astral_v1_carve_is_bit_exact_against_the_legacy_formula() {
        let legacy = |mut height: f64, hydro: HydrographicField| {
            if height > WATER_LEVEL as f64 - 5.0 && hydro.corridor > 0.0 {
                let bank_target = WATER_LEVEL as f64 + 4.5;
                if height > bank_target {
                    let bank_blend = (hydro.corridor * 0.46).clamp(0.0, 0.46);
                    height = height * (1.0 - bank_blend) + bank_target * bank_blend;
                }
                let channel_blend = smoothstep(0.18, 0.78, hydro.channel).powf(1.15);
                if channel_blend > 0.0 {
                    let bed_target = WATER_LEVEL as f64 - 2.0;
                    height = height * (1.0 - channel_blend) + bed_target * channel_blend;
                }
            }
            height
        };

        for height in [8.0, 42.0, 43.0, 43.25, 46.0, 52.5, 53.0, 80.0, 208.0] {
            for corridor in [0.0, 0.04, 0.5, 1.0, 2.0] {
                for channel in [0.0, 0.18, 0.5, 0.78, 1.0, 2.0] {
                    let hydro = HydrographicField { corridor, channel };
                    assert_eq!(
                        hydrographic_cross_section_v1(height, hydro).to_bits(),
                        legacy(height, hydro).to_bits(),
                        "Astral v1 drifted at height={height}, corridor={corridor}, channel={channel}"
                    );
                }
            }
        }
    }

    #[test]
    fn natural_v2_carve_is_bit_exact_against_the_established_formula() {
        let established_v2 = |pre_carve_height: f64, hydro: HydrographicField| {
            let bed_height = WATER_LEVEL as f64 - 2.0;
            if !pre_carve_height.is_finite() {
                return bed_height;
            }
            let unit_weight = |value: f64| {
                if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            };
            let corridor = unit_weight(hydro.corridor);
            if pre_carve_height <= WATER_LEVEL as f64 - 5.0 || corridor <= 0.0 {
                return pre_carve_height;
            }
            let channel = unit_weight(hydro.channel);
            let channel_blend = smoothstep(0.18, 0.78, channel).powf(1.15);
            let target_height =
                bed_height + NATURAL_RIVER_BANK_RELIEF_BLOCKS * (1.0 - channel_blend);
            let envelope_height = pre_carve_height.min(target_height);
            let corridor_easing = corridor.sqrt();
            (1.0 - corridor_easing) * pre_carve_height + corridor_easing * envelope_height
        };

        for height in [
            f64::NAN,
            f64::NEG_INFINITY,
            -f64::MAX,
            8.0,
            42.0,
            43.0,
            43.25,
            46.0,
            52.5,
            53.0,
            80.0,
            208.0,
            f64::MAX,
            f64::INFINITY,
        ] {
            for corridor in [
                f64::NAN,
                f64::NEG_INFINITY,
                -1.0,
                0.0,
                0.04,
                0.5,
                1.0,
                2.0,
                f64::INFINITY,
            ] {
                for channel in [
                    f64::NAN,
                    f64::NEG_INFINITY,
                    -1.0,
                    0.0,
                    0.18,
                    0.5,
                    0.78,
                    1.0,
                    2.0,
                    f64::INFINITY,
                ] {
                    let hydro = HydrographicField { corridor, channel };
                    assert_eq!(
                        natural_hydrographic_cross_section_v2(height, hydro).to_bits(),
                        established_v2(height, hydro).to_bits(),
                        "Natural V2 drifted at height={height}, corridor={corridor}, channel={channel}"
                    );
                }
            }
        }
    }

    #[test]
    fn natural_v1_v2_chunk_bytes_replay_the_established_checksums() {
        let position = ChunkPos::new(-4, 3, 4);
        for (grammar, expected_checksum) in [
            (TerrainGrammarVersion::V1, 0xbca7_6b20_990e_392e),
            (TerrainGrammarVersion::V2, 0x0649_18f3_e974_c9ab),
        ] {
            let generator = TerrainGenerator::new(12_345)
                .with_scenery_quality(crate::settings::SceneryQuality::Off)
                .with_terrain_grammar(grammar);
            let mut chunk = Chunk::new(position);
            generator.generate(&mut chunk);
            assert_eq!(
                voxel_fnv1a64(&chunk.voxels_vec()),
                expected_checksum,
                "{grammar:?} chunk bytes drifted at {position:?}"
            );
        }
    }

    #[test]
    fn terrain_grammar_selects_distinct_natural_chunk_bytes() {
        let v1 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Off)
            .with_terrain_grammar(TerrainGrammarVersion::V1);
        let v2 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Off)
            .with_terrain_grammar(TerrainGrammarVersion::V2);

        let differing_column = (-192..=192).step_by(4).find_map(|z| {
            (-192..=192).step_by(4).find_map(|x| {
                let v1_height = v1.surface_height_at(x, z);
                let v2_height = v2.surface_height_at(x, z);
                (v1_height != v2_height).then_some((x, z, v1_height, v2_height))
            })
        });
        let (x, z, v1_height, v2_height) =
            differing_column.expect("Natural V1 and V2 must expose distinct bank bytes");
        let pos = ChunkPos::new(
            x.div_euclid(CHUNK_SIZE_I),
            v1_height.max(v2_height).div_euclid(CHUNK_SIZE_I),
            z.div_euclid(CHUNK_SIZE_I),
        );
        let mut v1_chunk = Chunk::new(pos);
        let mut v2_chunk = Chunk::new(pos);
        v1.generate(&mut v1_chunk);
        v2.generate(&mut v2_chunk);

        assert_ne!(v1_chunk.voxels_vec(), v2_chunk.voxels_vec());
        assert_eq!(v1.surface_height_at(x, z), v1_height);
        assert_eq!(v2.surface_height_at(x, z), v2_height);
    }

    #[test]
    fn terrain_grammar_v3_selects_distinct_natural_chunk_bytes() {
        let v2 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Off)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let v3 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Off)
            .with_terrain_grammar(TerrainGrammarVersion::V3);

        let differing_column = (-192..=192).step_by(2).find_map(|z| {
            (-192..=192).step_by(2).find_map(|x| {
                let v2_height = v2.surface_height_at(x, z);
                let v3_height = v3.surface_height_at(x, z);
                (v2_height != v3_height).then_some((x, z, v2_height, v3_height))
            })
        });
        let (x, z, v2_height, v3_height) =
            differing_column.expect("Natural V2 and V3 must expose distinct bank bytes");
        let position = ChunkPos::new(
            x.div_euclid(CHUNK_SIZE_I),
            v2_height.max(v3_height).div_euclid(CHUNK_SIZE_I),
            z.div_euclid(CHUNK_SIZE_I),
        );
        let mut v2_chunk = Chunk::new(position);
        let mut v3_chunk = Chunk::new(position);
        v2.generate(&mut v2_chunk);
        v3.generate(&mut v3_chunk);

        assert_ne!(v2_chunk.voxels_vec(), v3_chunk.voxels_vec());
        assert_eq!(v2.surface_height_at(x, z), v2_height);
        assert_eq!(v3.surface_height_at(x, z), v3_height);
    }

    #[test]
    fn terrain_generator_clone_preserves_the_exact_generation_identity() {
        let identity = WorldGenerationIdentity {
            seed: u32::MAX,
            world_profile: WorldProfile::AstralFrontier,
            scenery_quality: crate::settings::SceneryQuality::Lush,
            terrain_grammar: TerrainGrammarVersion::V1,
        };
        let generator = TerrainGenerator::from_identity(identity);
        let cloned = generator.clone();

        assert_eq!(generator.generation_identity(), identity);
        assert_eq!(cloned.generation_identity(), identity);
        assert_eq!(cloned.grammar_version(), TerrainGrammarVersion::V1);
        assert_eq!(cloned.terrain_grammar(), TerrainGrammarVersion::V1);
    }

    #[test]
    fn default_natural_focus_has_a_bed_two_living_banks_and_gradual_x_z_profiles() {
        let generator = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let focus = IVec2::new(-64, 64);
        assert_eq!(
            generator.find_hydrographic_focus(0, 0, HYDROGRAPHIC_SEARCH_MAX_RADIUS),
            Some(focus)
        );
        assert_eq!(
            generator.surface_height_at(focus.x, focus.y),
            WATER_LEVEL - 2
        );
        assert!(
            generator
                .environment_sample_at(focus.x, focus.y)
                .river_strength
                >= 0.82
        );

        let cross = generator
            .hydrographic_focus_cross_section(focus.x, focus.y)
            .expect("fixed focus should retain two stable banks");
        assert_eq!(cross.width, 28);
        assert_eq!(cross.mean_bank_height, 51);
        assert_eq!(cross.max_bank_height, 51);
        assert_eq!(cross.bank_height_span, 0);
        assert_eq!(cross.living_banks, 2);

        for (axis, expected_offsets) in [(IVec2::X, (-11, 12)), (IVec2::Y, (-11, 13))] {
            let heights =
                cardinal_surface_profile(&generator, focus, axis, NATURAL_BANK_PROFILE_RADIUS);
            let mut reverse = (-NATURAL_BANK_PROFILE_RADIUS..=NATURAL_BANK_PROFILE_RADIUS)
                .rev()
                .map(|offset| {
                    generator.surface_height_at(
                        focus.x.saturating_add(axis.x.saturating_mul(offset)),
                        focus.y.saturating_add(axis.y.saturating_mul(offset)),
                    )
                })
                .collect::<Vec<_>>();
            reverse.reverse();
            assert_eq!(
                heights, reverse,
                "cardinal profile changed with query order"
            );

            let wet_span = center_connected_wet_span(&heights)
                .expect("fixed focus should contain center-connected water");
            let offsets = (
                wet_span.0 as i32 - NATURAL_BANK_PROFILE_RADIUS,
                wet_span.1 as i32 - NATURAL_BANK_PROFILE_RADIUS,
            );
            assert_eq!(offsets, expected_offsets);
            assert_eq!(
                heights
                    .windows(2)
                    .map(|pair| pair[0].abs_diff(pair[1]))
                    .max(),
                Some(2)
            );
            for direction in [-1, 1] {
                assert!(
                    bank_transition_max_rise(&heights, wet_span, direction)
                        .is_some_and(|rise| rise <= 2),
                    "axis={axis:?}, direction={direction} retained a steep bank"
                );
            }
        }
    }

    #[test]
    fn default_natural_focus_has_no_long_run_of_steep_recentered_bank_slices() {
        let generator = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let focus = IVec2::new(-64, 64);
        let mut current_steep_run = [0usize; 2];
        let mut longest_steep_run = [0usize; 2];
        let mut valid_slices = 0usize;

        // The focus flow is Z-dominant, so X is its established cardinalized
        // normal. Recenter every bounded Z slice before measuring both banks;
        // one transverse edge can no longer masquerade as a short wall.
        for tangent_offset in -16..=16 {
            let z = focus.y + tangent_offset;
            let center_x = (-24i32..=24)
                .filter_map(|dx| {
                    let x = focus.x + dx;
                    let height = generator.surface_height_at(x, z);
                    (height < WATER_LEVEL).then_some((height, dx.abs(), dx, x))
                })
                .min_by_key(|&(height, distance, dx, _)| (height, distance, dx))
                .map(|(_, _, _, x)| x)
                .expect("every bounded tangent slice should intersect the fixed river");
            let heights = cardinal_surface_profile(
                &generator,
                IVec2::new(center_x, z),
                IVec2::X,
                NATURAL_BANK_PROFILE_RADIUS,
            );
            let wet_span = center_connected_wet_span(&heights)
                .expect("recentered slice should remain inside the river bed");

            for (side, direction) in [-1, 1].into_iter().enumerate() {
                let rise = bank_transition_max_rise(&heights, wet_span, direction)
                    .expect("bounded slice should reach three stable land samples per bank");
                if rise >= 3 {
                    current_steep_run[side] += 1;
                    longest_steep_run[side] = longest_steep_run[side].max(current_steep_run[side]);
                } else {
                    current_steep_run[side] = 0;
                }
            }
            valid_slices += 1;
        }

        assert_eq!(valid_slices, 33);
        assert!(
            longest_steep_run.into_iter().all(|run| run < 4),
            "river retained a longitudinal palisade: {longest_steep_run:?}"
        );
    }

    #[test]
    fn v3_default_anchor_has_multi_voxel_shelves_and_no_immediate_wall() {
        let focus = IVec2::new(-64, 64);
        let v2 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let v3 = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush)
            .with_terrain_grammar(TerrainGrammarVersion::V3);

        assert_eq!(
            v3.find_hydrographic_focus(0, 0, HYDROGRAPHIC_SEARCH_MAX_RADIUS),
            Some(focus),
            "V3 must retain the established bounded visual-comparison anchor"
        );
        assert_eq!(v3.surface_height_at(focus.x, focus.y), WATER_LEVEL - 2);
        assert_eq!(
            v3.hydrographic_focus_cross_section(focus.x, focus.y),
            Some(HydrographicCrossSection {
                width: 32,
                mean_bank_height: WATER_LEVEL + 3,
                max_bank_height: WATER_LEVEL + 3,
                bank_height_span: 0,
                living_banks: 2,
            })
        );

        let mut v2_shelf_range = (usize::MAX, 0usize);
        let mut v3_shelf_range = (usize::MAX, 0usize);
        let mut checked_sides = 0usize;
        for tangent_offset in -16..=16 {
            let (v2_heights, v2_wet_span) =
                recentered_default_river_slice(&v2, focus, tangent_offset)
                    .expect("every V2 tangent slice must intersect the fixed river");
            let (v3_heights, v3_wet_span) =
                recentered_default_river_slice(&v3, focus, tangent_offset)
                    .expect("every V3 tangent slice must intersect the fixed river");

            for direction in [-1isize, 1] {
                let v2_metrics = natural_bank_shelf_metrics(&v2_heights, v2_wet_span, direction)
                    .expect("V2 slice must reach stable outer relief");
                v2_shelf_range.0 = v2_shelf_range.0.min(v2_metrics.shelf_width);
                v2_shelf_range.1 = v2_shelf_range.1.max(v2_metrics.shelf_width);

                let v3_metrics = natural_bank_shelf_metrics(&v3_heights, v3_wet_span, direction)
                    .expect("V3 slice must reach stable outer relief");
                assert!(
                    (4..=5).contains(&v3_metrics.shelf_width),
                    "dz={tangent_offset}, direction={direction} shelf width escaped the authored 4..=5 block band: {v3_metrics:?}"
                );
                assert!(
                    v3_metrics.max_adjacent_rise < 3,
                    "dz={tangent_offset}, direction={direction} retained an immediate >=3-block wall: {v3_metrics:?}"
                );
                assert_eq!(
                    v3_metrics.first_outer_height,
                    WATER_LEVEL + 3,
                    "dz={tangent_offset}, direction={direction} skipped the low living cap"
                );
                v3_shelf_range.0 = v3_shelf_range.0.min(v3_metrics.shelf_width);
                v3_shelf_range.1 = v3_shelf_range.1.max(v3_metrics.shelf_width);
                checked_sides += 1;
            }
        }

        assert_eq!(checked_sides, 66);
        assert_eq!(v2_shelf_range, (1, 2), "the measured V2 baseline drifted");
        assert_eq!(v3_shelf_range, (4, 5));
    }

    #[test]
    fn v3_default_anchor_orders_water_sand_shelf_and_living_cap_materials() {
        let generator = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Off)
            .with_terrain_grammar(TerrainGrammarVersion::V3);
        let focus = IVec2::new(-64, 64);

        let generated_block = |x: i32, y: i32, z: i32| {
            let position = ChunkPos::new(
                x.div_euclid(CHUNK_SIZE_I),
                y.div_euclid(CHUNK_SIZE_I),
                z.div_euclid(CHUNK_SIZE_I),
            );
            let mut chunk = Chunk::new(position);
            generator.generate(&mut chunk);
            BlockType::from_voxel(chunk.get(
                x.rem_euclid(CHUNK_SIZE_I) as usize,
                y.rem_euclid(CHUNK_SIZE_I) as usize,
                z.rem_euclid(CHUNK_SIZE_I) as usize,
            ))
        };

        assert_eq!(
            generated_block(focus.x, WATER_LEVEL, focus.y),
            BlockType::Water
        );
        for direction in [-1, 1] {
            let mut shelves = Vec::new();
            let mut living_cap = None;
            for distance in 1..=48 {
                let x = focus.x.saturating_add(direction * distance);
                let surface = generator.surface_height_at(x, focus.y);
                if surface < WATER_LEVEL {
                    continue;
                }
                if surface == WATER_LEVEL + 1 {
                    shelves.push((x, surface));
                    continue;
                }
                if surface >= WATER_LEVEL + 3 {
                    living_cap = Some((x, surface));
                    break;
                }
            }

            assert!(
                (4..=5).contains(&shelves.len()),
                "direction={direction} did not expose a multi-voxel sediment shelf: {shelves:?}"
            );
            for &(x, surface) in &shelves {
                assert_eq!(generator.biome_at(x, focus.y), Biome::Beach);
                assert_eq!(
                    generated_block(x, surface, focus.y),
                    BlockType::Sand,
                    "shelf column {x},{surface},{} lost its sediment surface",
                    focus.y
                );
            }

            let (cap_x, cap_surface) = living_cap.expect("shelf must lead to living outer relief");
            assert_eq!(cap_surface, WATER_LEVEL + 3);
            let cap_biome = generator.biome_at(cap_x, focus.y);
            assert!(matches!(
                cap_biome,
                Biome::Plains
                    | Biome::Forest
                    | Biome::Jungle
                    | Biome::Savanna
                    | Biome::Tundra
                    | Biome::Karst
            ));
            let cap_block = generated_block(cap_x, cap_surface, focus.y);
            assert!(matches!(
                cap_block,
                BlockType::Grass
                    | BlockType::TundraGrass
                    | BlockType::SavannaGrass
                    | BlockType::MossStone
            ), "direction={direction} cap at {cap_x},{cap_surface},{} was {cap_biome:?}/{cap_block:?}", focus.y);
        }
    }

    #[test]
    fn v3_bank_contract_replays_across_seeds_signed_seams_and_extremes() {
        for seed in [0x48D2_09A1, 12_345] {
            let generator = TerrainGenerator::new(seed)
                .with_scenery_quality(crate::settings::SceneryQuality::Lush)
                .with_terrain_grammar(TerrainGrammarVersion::V3);
            let focus = generator
                .find_hydrographic_focus(0, 0, HYDROGRAPHIC_SEARCH_MAX_RADIUS)
                .expect("V3 seed should retain a bounded river focus");
            assert!(generator.surface_height_at(focus.x, focus.y) <= WATER_LEVEL - 2);
            let [flow_x, flow_z] = generator
                .environment_sample_at(focus.x, focus.y)
                .flow_direction;
            let normal = if flow_x.abs() >= flow_z.abs() {
                IVec2::Y
            } else {
                IVec2::X
            };
            let heights = cardinal_surface_profile(&generator, focus, normal, 48);
            let wet_span = center_connected_wet_span(&heights)
                .expect("V3 multi-seed focus must contain center-connected water");
            for direction in [-1isize, 1] {
                let metrics = natural_bank_shelf_metrics(&heights, wet_span, direction)
                    .expect("V3 multi-seed bank must reach stable outer relief");
                assert!(
                    (2..=12).contains(&metrics.shelf_width),
                    "seed={seed}, direction={direction} shelf escaped its global bound: {metrics:?}"
                );
                assert!(
                    metrics.max_adjacent_rise < 3,
                    "seed={seed}, direction={direction} retained a >=3-block wall: {metrics:?}"
                );
            }
        }

        let generator =
            TerrainGenerator::new(12_345).with_terrain_grammar(TerrainGrammarVersion::V3);
        let mut points = Vec::new();
        for seam in [-64, -32, -16, 0, 16, 32, 64] {
            for delta in -1..=1 {
                points.push((seam + delta, 64));
                points.push((-64, seam + delta));
            }
        }
        points.extend([
            (i32::MIN, i32::MIN),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MAX, i32::MAX),
            (i32::MIN + 1, 0),
            (i32::MAX - 1, 0),
        ]);

        let forward = points
            .iter()
            .map(|&(x, z)| ((x, z), generator.surface_height_at(x, z)))
            .collect::<Vec<_>>();
        let mut reverse = points
            .iter()
            .rev()
            .map(|&(x, z)| ((x, z), generator.surface_height_at(x, z)))
            .collect::<Vec<_>>();
        reverse.reverse();
        assert_eq!(forward, reverse);

        points
            .sort_by_key(|&(x, z)| (x.div_euclid(CHUNK_SIZE_I), z.div_euclid(CHUNK_SIZE_I), x, z));
        for (x, z) in points {
            let expected = forward
                .iter()
                .find_map(|&((sample_x, sample_z), height)| {
                    (sample_x == x && sample_z == z).then_some(height)
                })
                .expect("ordered V3 sample should exist in baseline");
            let actual = generator.surface_height_at(x, z);
            assert_eq!(actual, expected, "V3 surface changed at {x},{z}");
            assert!((8..=208).contains(&actual));
        }
    }

    #[test]
    fn natural_bank_contract_replays_across_seeds_signed_seams_and_extreme_coordinates() {
        for (seed, expected_focus, expected_width) in [
            (0x48D2_09A1, IVec2::new(-16, -32), 22),
            (12_345, IVec2::new(-64, 64), 28),
        ] {
            let generator = TerrainGenerator::new(seed)
                .with_scenery_quality(crate::settings::SceneryQuality::Lush)
                .with_terrain_grammar(TerrainGrammarVersion::V2);
            let focus = generator
                .find_hydrographic_focus(0, 0, HYDROGRAPHIC_SEARCH_MAX_RADIUS)
                .expect("seed should retain a bounded river focus");
            assert_eq!(focus, expected_focus);
            assert!(generator.surface_height_at(focus.x, focus.y) <= WATER_LEVEL - 2);
            let cross = generator
                .hydrographic_focus_cross_section(focus.x, focus.y)
                .expect("multi-seed focus should retain both banks");
            assert_eq!(cross.width, expected_width);
            assert_eq!(cross.living_banks, 2);

            let [flow_x, flow_z] = generator
                .environment_sample_at(focus.x, focus.y)
                .flow_direction;
            let normal = if flow_x.abs() >= flow_z.abs() {
                IVec2::Y
            } else {
                IVec2::X
            };
            let heights = cardinal_surface_profile(&generator, focus, normal, 32);
            assert!(
                heights
                    .windows(2)
                    .map(|pair| pair[0].abs_diff(pair[1]))
                    .max()
                    .is_some_and(|rise| rise <= 2),
                "seed={seed} retained a steep focus bank"
            );
        }

        let generator =
            TerrainGenerator::new(12_345).with_terrain_grammar(TerrainGrammarVersion::V2);
        let mut points = Vec::new();
        for seam in [-64, -32, -16, 0, 16, 32, 64] {
            for delta in -1..=1 {
                points.push((seam + delta, 64));
                points.push((-64, seam + delta));
            }
        }
        points.extend([
            (i32::MIN, i32::MIN),
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MAX, i32::MAX),
            (i32::MIN + 1, 0),
            (i32::MAX - 1, 0),
        ]);

        let forward = points
            .iter()
            .map(|&(x, z)| ((x, z), generator.surface_height_at(x, z)))
            .collect::<Vec<_>>();
        let mut reverse = points
            .iter()
            .rev()
            .map(|&(x, z)| ((x, z), generator.surface_height_at(x, z)))
            .collect::<Vec<_>>();
        reverse.reverse();
        assert_eq!(forward, reverse);

        let mut chunk_grouped = points;
        chunk_grouped
            .sort_by_key(|&(x, z)| (x.div_euclid(CHUNK_SIZE_I), z.div_euclid(CHUNK_SIZE_I), x, z));
        for (x, z) in chunk_grouped {
            let expected = forward
                .iter()
                .find_map(|&((sample_x, sample_z), height)| {
                    (sample_x == x && sample_z == z).then_some(height)
                })
                .expect("ordered sample should exist in the baseline");
            let actual = generator.surface_height_at(x, z);
            assert_eq!(actual, expected, "surface changed at {x},{z}");
            assert!((8..=208).contains(&actual));
        }
    }

    #[test]
    fn default_lush_river_focus_rejects_the_frame_filling_karst_wall() {
        let generator = TerrainGenerator::new(12_345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let rejected_anchor = IVec2::new(-32, 32);
        let rejected_cross = generator
            .hydrographic_focus_cross_section(rejected_anchor.x, rejected_anchor.y)
            .expect("the former QA anchor should still describe a real channel");
        let rejected_context =
            generator.hydrographic_focus_context(rejected_anchor.x, rejected_anchor.y, 64);
        assert!(
            rejected_cross.max_bank_height > HYDROGRAPHIC_FOCUS_MAX_BANK_HEIGHT
                || rejected_cross.bank_height_span > HYDROGRAPHIC_FOCUS_MAX_BANK_SPAN
                || rejected_context.max_surface_height > HYDROGRAPHIC_FOCUS_MAX_CONTEXT_HEIGHT
                || rejected_context.relief_span() > HYDROGRAPHIC_FOCUS_MAX_CONTEXT_RELIEF,
            "the known wall anchor must fail the bounded visual-relief contract: cross={rejected_cross:?}, context={rejected_context:?}"
        );

        let focus = generator
            .find_hydrographic_focus(0, 0, HYDROGRAPHIC_SEARCH_MAX_RADIUS)
            .expect("default lush world should retain a bounded low-relief river focus");
        assert_ne!(focus, rejected_anchor);
        let cross = generator
            .hydrographic_focus_cross_section(focus.x, focus.y)
            .expect("selected focus must retain two banks");
        let context = generator.hydrographic_focus_context(focus.x, focus.y, 64);
        assert_eq!(cross.living_banks, 2);
        assert!(cross.max_bank_height <= HYDROGRAPHIC_FOCUS_MAX_BANK_HEIGHT);
        assert!(cross.bank_height_span <= HYDROGRAPHIC_FOCUS_MAX_BANK_SPAN);
        assert!(context.open_water_probes <= 2);
        assert!(context.max_surface_height <= HYDROGRAPHIC_FOCUS_MAX_CONTEXT_HEIGHT);
        assert!(context.relief_span() <= HYDROGRAPHIC_FOCUS_MAX_CONTEXT_RELIEF);
    }

    #[test]
    fn default_world_regions_stay_natural_not_alien_showcases() {
        let generator = TerrainGenerator::new(12345);
        assert_eq!(generator.world_profile(), WorldProfile::Natural);
        let mut samples = 0usize;

        for z in (-12_000..=12_000).step_by(512) {
            for x in (-12_000..=12_000).step_by(512) {
                let (region, strength) = generator.region(x as f64, z as f64);
                assert!(
                    !matches!(
                        region,
                        Region::CrystalSpires
                            | Region::VolcanicWaste
                            | Region::GlacierShards
                            | Region::AlienReef
                    ),
                    "default terrain should not pick showcase region {region:?} at strength {strength}"
                );
                assert!(
                    !generator.biome_at(x, z).is_showcase_terrain(),
                    "default terrain should not pick showcase biome at {x},{z}"
                );
                samples += 1;
            }
        }

        assert!(samples > 100);
    }

    #[test]
    fn astral_frontier_activates_a_composed_mix_of_calm_and_hero_provinces() {
        let generator =
            TerrainGenerator::new(12345).with_world_profile(WorldProfile::AstralFrontier);
        let replay = TerrainGenerator::new(12345).with_world_profile(WorldProfile::AstralFrontier);
        let mut regions = HashSet::new();
        let mut showcase_samples = 0usize;
        let mut calm_samples = 0usize;

        for z in (-16_384..=16_384).step_by(256) {
            for x in (-16_384..=16_384).step_by(256) {
                let first = generator.region(x as f64, z as f64);
                assert_eq!(first, replay.region(x as f64, z as f64));
                regions.insert(first.0);
                let biome = generator.biome_at(x, z);
                if biome.is_showcase_terrain() {
                    showcase_samples += 1;
                } else {
                    calm_samples += 1;
                }
            }
        }

        for required in [
            Region::Canyon,
            Region::Plateau,
            Region::Highland,
            Region::Karst,
            Region::CrystalSpires,
            Region::VolcanicWaste,
            Region::AlienReef,
        ] {
            assert!(
                regions.contains(&required),
                "Astral Frontier never exposed {required:?}; sampled {regions:?}"
            );
        }
        assert!(showcase_samples > 0, "hero provinces must be reachable");
        assert!(
            calm_samples > 0,
            "the world needs visual rest and safe routes"
        );
    }

    #[test]
    fn astral_frontier_has_a_bounded_flyable_showcase_entry() {
        let generator =
            TerrainGenerator::new(12345).with_world_profile(WorldProfile::AstralFrontier);
        let spawn = generator
            .find_neon_showcase_spawn(0, 0, 4096)
            .expect("Astral Frontier must expose a nearby hero province");

        assert!(spawn.biome.is_neon_showcase());
        assert!(spawn.y > WATER_LEVEL + 20);
        assert!(spawn.x.abs().max(spawn.z.abs()) <= 4096);
    }

    #[test]
    fn astral_frontier_layout_is_seed_stable_reversible_and_profile_scoped() {
        for seed in [0_u32, 1, 12_345, u32::MAX] {
            let first =
                TerrainGenerator::new(seed).with_world_profile(WorldProfile::AstralFrontier);
            let replay =
                TerrainGenerator::new(seed).with_world_profile(WorldProfile::AstralFrontier);
            let layout = first
                .astral_layout()
                .expect("Astral worlds own a hero layout");

            assert_eq!(first.astral_layout(), replay.astral_layout());
            assert_eq!(first.astral_frontier_hub(), Some(layout.hub));
            assert_eq!(first.astral_frontier_landing(), Some(layout.landing()));
            assert_ne!(layout.hub, layout.landing());
            for local in [
                IVec2::ZERO,
                IVec2::new(-124, 24),
                IVec2::new(142, 112),
                IVec2::new(-257, 311),
            ] {
                assert_eq!(
                    layout.local_from_world(layout.world_from_local(local)),
                    local
                );
            }
        }

        let natural = TerrainGenerator::new(12_345);
        assert_eq!(natural.astral_frontier_hub(), None);
        assert_eq!(natural.astral_frontier_landing(), None);
        assert_eq!(natural.astral_layout(), None);
    }

    #[test]
    fn astral_frontier_precinct_guarantees_relief_canyon_and_flat_landing() {
        let generator =
            TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let layout = generator.astral_layout().expect("Astral layout");
        let hub_height = generator.surface_height_at(layout.hub.x, layout.hub.y);
        let landing = layout.landing();
        let landing_height = generator.surface_height_at(landing.x, landing.y);

        assert_eq!(
            landing_height, 79,
            "the authored pad contract must be level"
        );
        for local_offset in [
            IVec2::ZERO,
            IVec2::new(16, 0),
            IVec2::new(-16, 0),
            IVec2::new(0, 16),
            IVec2::new(0, -16),
            IVec2::new(12, 12),
        ] {
            let sample =
                landing + AstralFrontierLayout::rotate_quarters(local_offset, layout.quarter_turns);
            assert_eq!(
                generator.surface_height_at(sample.x, sample.y),
                landing_height,
                "the inner landing disc must not inherit mountain or canyon slope"
            );
        }
        assert!(
            hub_height - landing_height >= 48,
            "the citadel needs a legible vertical hierarchy: hub={hub_height}, landing={landing_height}"
        );
        assert_eq!(generator.biome_at(landing.x, landing.y), Biome::AlienReef);

        let canyon_z = 82;
        let canyon_x = (-58.0 + ((canyon_z as f64 + 34.0) * 0.026).sin() * 13.0).round() as i32;
        let canyon = layout.world_from_local(IVec2::new(canyon_x, canyon_z));
        let canyon_height = generator.surface_height_at(canyon.x, canyon.y);
        assert!(
            canyon_height <= WATER_LEVEL,
            "the negative-space canyon was filled back in: {canyon_height}"
        );
        assert_eq!(generator.biome_at(canyon.x, canyon.y), Biome::VolcanicWaste);
    }

    #[test]
    fn astral_frontier_streaming_retains_authored_vertical_destinations_only_nearby() {
        let generator =
            TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let layout = generator.astral_layout().expect("Astral layout");
        let hub_cx = layout.hub.x.div_euclid(CHUNK_SIZE_I);
        let hub_cz = layout.hub.y.div_euclid(CHUNK_SIZE_I);
        let hub_surface = generator.surface_height_at(layout.hub.x, layout.hub.y);
        assert!(
            generator
                .astral_precinct_top_hint_for_chunk(hub_cx, hub_cz)
                .is_some_and(|top| top >= hub_surface + 50),
            "streaming must retain the citadel crown"
        );

        let far_cx = hub_cx.saturating_add(10_000);
        let far_cz = hub_cz.saturating_sub(10_000);
        assert_eq!(
            generator.astral_precinct_top_hint_for_chunk(far_cx, far_cz),
            None,
            "distant chunks must reject precinct work before surface sampling"
        );

        let landing = layout.landing();
        let landing_y = generator.surface_height_at(landing.x, landing.y) + 1;
        let mut pad_chunk = Chunk::new(ChunkPos::new(
            landing.x.div_euclid(CHUNK_SIZE_I),
            landing_y.div_euclid(CHUNK_SIZE_I),
            landing.y.div_euclid(CHUNK_SIZE_I),
        ));
        generator.generate(&mut pad_chunk);
        let authored_materials = [
            Voxel::from(BlockType::ShipHullDark),
            Voxel::from(BlockType::ShipHullAlloy),
            Voxel::from(BlockType::NeonCyan),
            Voxel::from(BlockType::NeonAmber),
            Voxel::from(BlockType::NeonGlass),
        ];
        let authored_count = pad_chunk
            .voxels_vec()
            .iter()
            .filter(|voxel| authored_materials.contains(voxel))
            .count();
        assert!(
            authored_count >= 32,
            "the entry chunk should contain a readable authored landing pad, got {authored_count} cells"
        );
    }

    #[test]
    fn slope_aware_underground_skin_is_monotonic_and_bounded() {
        assert_eq!(subterranean_surface_skin(-10), 12);
        assert_eq!(subterranean_surface_skin(0), 12);
        assert_eq!(subterranean_surface_skin(1), 14);
        assert_eq!(subterranean_surface_skin(3), 18);
        assert_eq!(subterranean_surface_skin(8), 28);
        assert_eq!(subterranean_surface_skin(80), 28);
    }

    #[test]
    fn astral_waypoints_form_a_global_varied_deterministic_network() {
        let generator =
            TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let replay = TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let natural = TerrainGenerator::new(12_345);
        let layout = generator.astral_layout().expect("Astral layout");
        let mut sites = Vec::new();
        let mut kinds = HashSet::new();
        let mut occupied_macro_quadrants = HashSet::new();

        for owner_z in -12..=12 {
            for owner_x in -12..=12 {
                let first = generator.astral_waypoint_for_cell(owner_x, owner_z);
                assert_eq!(first, replay.astral_waypoint_for_cell(owner_x, owner_z));
                assert_eq!(natural.astral_waypoint_for_cell(owner_x, owner_z), None);
                let Some(spec) = first else {
                    continue;
                };
                let dx = i64::from(spec.center.x) - i64::from(layout.hub.x);
                let dz = i64::from(spec.center.z) - i64::from(layout.hub.y);
                assert!(
                    dx * dx + dz * dz > i64::from(ASTRAL_WAYPOINT_HUB_EXCLUSION).pow(2),
                    "global grammar must not clutter the authored entry precinct"
                );
                assert!(spec.radius <= ASTRAL_WAYPOINT_MAX_RADIUS);
                assert!(spec.center.y > WATER_LEVEL + 5);
                kinds.insert(spec.kind);
                occupied_macro_quadrants.insert((owner_x.signum(), owner_z.signum()));
                sites.push(spec);
            }
        }

        assert!(
            sites.len() >= 40,
            "the world grammar should yield many destinations, found {}",
            sites.len()
        );
        assert_eq!(kinds.len(), 4, "all waypoint grammars must be reachable");
        assert!(
            occupied_macro_quadrants.len() >= 4,
            "destinations must cover the explored world rather than one location"
        );
    }

    #[test]
    fn astral_macro_nodes_sculpt_four_landable_regional_grammars() {
        let generator =
            TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let natural = TerrainGenerator::new(12_345);
        let mut kinds = HashSet::new();
        let mut centre_heights = HashSet::new();
        let mut relief_samples = 0usize;

        'cells: for owner_z in -18..=18 {
            for owner_x in -18..=18 {
                let Some(node) = generator.astral_macro_node_for_cell(owner_x, owner_z) else {
                    continue;
                };
                if !kinds.insert(node.kind) {
                    continue;
                }
                let target = node.terrain_target().round() as i32;
                let centre = generator.surface_height_at(node.center.x, node.center.y);
                assert_eq!(
                    centre, target,
                    "global destination core must honour its landable target"
                );
                assert!(
                    cardinal_surface_slope(&generator, node.center.x, node.center.y, centre) <= 1,
                    "global destination core is not level: {node:?}"
                );
                let expected_biome = match node.kind {
                    AstralWaypointKind::RelaySpire | AstralWaypointKind::TransitGate => {
                        Biome::Karst
                    }
                    AstralWaypointKind::SkyDock => Biome::Mesa,
                    AstralWaypointKind::CrystalGarden => Biome::AlienReef,
                };
                assert_eq!(
                    generator.biome_at(node.center.x, node.center.y),
                    expected_biome
                );

                let outer_local = match node.kind {
                    AstralWaypointKind::SkyDock | AstralWaypointKind::TransitGate => {
                        IVec2::new(0, 63)
                    }
                    _ => IVec2::new(63, 0),
                };
                let outer = node.center
                    + AstralFrontierLayout::rotate_quarters(outer_local, node.quarter_turns);
                let outer_height = generator.surface_height_at(outer.x, outer.y);
                relief_samples += usize::from(outer_height != centre);
                centre_heights.insert(centre);
                if kinds.len() == 4 {
                    break 'cells;
                }
            }
        }

        assert_eq!(kinds.len(), 4, "every regional grammar must be reachable");
        assert!(
            centre_heights.len() >= 3,
            "regional targets collapsed to one elevation"
        );
        assert!(
            relief_samples >= 3,
            "node envelopes should create relief, not four larger flat discs"
        );
        assert_eq!(natural.astral_macro_node_for_cell(7, -9), None);
        assert_eq!(
            natural.apply_astral_world_height(123.0, -456.0, 73.25),
            73.25
        );
        assert_eq!(natural.astral_world_biome_override(123.0, -456.0), None);
    }

    #[test]
    fn astral_route_graph_grades_and_paints_connected_remote_provinces() {
        let generator =
            TerrainGenerator::new(91_337).with_world_profile(WorldProfile::AstralFrontier);
        let replay = TerrainGenerator::new(91_337).with_world_profile(WorldProfile::AstralFrontier);
        let mut selected = None;
        'search: for owner_z in -18..=18 {
            for owner_x in -18..=18 {
                let Some(start) = generator.astral_macro_node_for_cell(owner_x, owner_z) else {
                    continue;
                };
                for (step_x, step_z) in [(1, 0), (0, 1)] {
                    if let Some(end) =
                        generator.next_astral_macro_node(owner_x, owner_z, step_x, step_z)
                    {
                        selected = Some(AstralRouteSpec {
                            start,
                            end,
                            accent: TerrainGenerator::astral_route_accent(start, end),
                        });
                        break 'search;
                    }
                }
            }
        }
        let route = selected.expect("sample world should contain a global connection");
        let midpoint = IVec2::new(
            (route.start.center.x + route.end.center.x) / 2,
            (route.start.center.y + route.end.center.y) / 2,
        );
        let surface = generator.surface_height_at(midpoint.x, midpoint.y);
        assert_eq!(surface, replay.surface_height_at(midpoint.x, midpoint.y));
        assert!(
            cardinal_surface_slope(&generator, midpoint.x, midpoint.y, surface) <= 4,
            "graded world connection became an impassable seam"
        );

        let mut chunk = Chunk::new(ChunkPos::new(
            midpoint.x.div_euclid(CHUNK_SIZE_I),
            (surface + 1).div_euclid(CHUNK_SIZE_I),
            midpoint.y.div_euclid(CHUNK_SIZE_I),
        ));
        let painted = generator.paint_astral_route_into_chunk(&mut chunk, route);
        assert!(
            painted >= 20,
            "remote route did not cross its midpoint chunk"
        );
        let route_materials = [
            Voxel::from(BlockType::ShipHullDark),
            Voxel::from(BlockType::ShipHullAlloy),
            Voxel::from(route.accent),
        ];
        assert!(
            chunk
                .voxels_vec()
                .iter()
                .filter(|voxel| route_materials.contains(voxel))
                .count()
                >= 10
        );
    }

    #[test]
    fn astral_waypoint_archetypes_are_not_disguised_circular_discs() {
        let omitted_local = [
            (AstralWaypointKind::RelaySpire, IVec2::new(10, 10), 22, 30),
            (AstralWaypointKind::SkyDock, IVec2::new(0, 16), 28, 18),
            (
                AstralWaypointKind::CrystalGarden,
                IVec2::new(13, 13),
                24,
                20,
            ),
            (AstralWaypointKind::TransitGate, IVec2::new(0, 14), 26, 22),
        ];
        for (kind, omitted, radius, height) in omitted_local {
            let spec = AstralWaypointSpec {
                center: IVec3::new(128, 96, 128),
                kind,
                quarter_turns: 0,
                radius,
                height,
                platform: BlockType::ZenStone,
                accent: BlockType::NeonCyan,
            };
            assert!(omitted.length_squared() < radius * radius);
            let omitted_world = spec.center + IVec3::new(omitted.x, 0, omitted.y);
            let mut omitted_chunk = Chunk::new(ChunkPos::new(
                omitted_world.x.div_euclid(CHUNK_SIZE_I),
                omitted_world.y.div_euclid(CHUNK_SIZE_I),
                omitted_world.z.div_euclid(CHUNK_SIZE_I),
            ));
            TerrainGenerator::paint_astral_waypoint_into_chunk(&mut omitted_chunk, spec);
            assert_eq!(
                omitted_chunk.get(
                    omitted_world.x.rem_euclid(CHUNK_SIZE_I) as usize,
                    omitted_world.y.rem_euclid(CHUNK_SIZE_I) as usize,
                    omitted_world.z.rem_euclid(CHUNK_SIZE_I) as usize,
                ),
                AIR,
                "{kind:?} still filled its whole circular bounding disc"
            );

            let centre_chunk_pos = ChunkPos::new(
                spec.center.x.div_euclid(CHUNK_SIZE_I),
                spec.center.y.div_euclid(CHUNK_SIZE_I),
                spec.center.z.div_euclid(CHUNK_SIZE_I),
            );
            let mut centre_chunk = Chunk::new(centre_chunk_pos);
            TerrainGenerator::paint_astral_waypoint_into_chunk(&mut centre_chunk, spec);
            assert_ne!(
                centre_chunk.get(
                    spec.center.x.rem_euclid(CHUNK_SIZE_I) as usize,
                    spec.center.y.rem_euclid(CHUNK_SIZE_I) as usize,
                    spec.center.z.rem_euclid(CHUNK_SIZE_I) as usize,
                ),
                AIR,
                "{kind:?} lost its connected root"
            );
        }
    }

    #[test]
    fn astral_waypoint_geometry_is_connected_streamable_and_budgeted() {
        let generator =
            TerrainGenerator::new(91_337).with_world_profile(WorldProfile::AstralFrontier);
        let spec = (-24..=24)
            .flat_map(|z| (-24..=24).map(move |x| (x, z)))
            .find_map(|(x, z)| generator.astral_waypoint_for_cell(x, z))
            .expect("sample world should contain at least one global waypoint");
        let min_cx = (spec.center.x - spec.radius).div_euclid(CHUNK_SIZE_I);
        let max_cx = (spec.center.x + spec.radius).div_euclid(CHUNK_SIZE_I);
        let min_cz = (spec.center.z - spec.radius).div_euclid(CHUNK_SIZE_I);
        let max_cz = (spec.center.z + spec.radius).div_euclid(CHUNK_SIZE_I);
        let min_cy = spec.center.y.div_euclid(CHUNK_SIZE_I);
        let max_cy = spec.top().div_euclid(CHUNK_SIZE_I);
        let mut positions = HashSet::new();
        let mut accent_cells = 0usize;

        for cy in min_cy..=max_cy {
            for cz in min_cz..=max_cz {
                for cx in min_cx..=max_cx {
                    let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                    TerrainGenerator::paint_astral_waypoint_into_chunk(&mut chunk, spec);
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let voxel = chunk.get(lx, ly, lz);
                                if voxel == AIR {
                                    continue;
                                }
                                if voxel == Voxel::from(spec.accent) {
                                    accent_cells += 1;
                                }
                                positions.insert((
                                    cx * CHUNK_SIZE_I + lx as i32,
                                    cy * CHUNK_SIZE_I + ly as i32,
                                    cz * CHUNK_SIZE_I + lz as i32,
                                ));
                            }
                        }
                    }
                }
            }
        }

        assert!((200..=4_000).contains(&positions.len()));
        assert!(
            accent_cells >= 8,
            "destination needs readable wayfinding light"
        );
        let root = (spec.center.x, spec.center.y, spec.center.z);
        assert!(positions.contains(&root));
        let mut visited = HashSet::from([root]);
        let mut pending = vec![root];
        while let Some((x, y, z)) = pending.pop() {
            for neighbour in [
                (x + 1, y, z),
                (x - 1, y, z),
                (x, y + 1, z),
                (x, y - 1, z),
                (x, y, z + 1),
                (x, y, z - 1),
            ] {
                if positions.contains(&neighbour) && visited.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        assert_eq!(
            visited.len(),
            positions.len(),
            "waypoint split into islands"
        );
        assert!(
            generator
                .decorative_top_hint_for_chunk(
                    spec.center.x.div_euclid(CHUNK_SIZE_I),
                    spec.center.z.div_euclid(CHUNK_SIZE_I),
                )
                .is_some_and(|top| top >= spec.top()),
            "vertical streaming must retain the complete destination"
        );
    }

    #[test]
    fn authored_astral_islands_are_destinations_not_global_generation_tax() {
        let generator =
            TerrainGenerator::new(12_345).with_world_profile(WorldProfile::AstralFrontier);
        let layout = generator.astral_layout().expect("Astral layout");
        let authored = generator
            .authored_astral_islands()
            .expect("Astral layout owns three composed destinations");
        let centers: HashSet<_> = authored.iter().map(|spec| spec.center).collect();
        assert_eq!(centers.len(), authored.len());

        for spec in authored {
            let cx = spec.center.x.div_euclid(CHUNK_SIZE_I);
            let cz = spec.center.z.div_euclid(CHUNK_SIZE_I);
            assert!(TerrainGenerator::chunk_intersects_disc(
                cx,
                cz,
                layout.hub,
                ASTRAL_AUTHORED_ISLAND_RADIUS
            ));
            assert!(
                generator
                    .decorative_top_hint_for_chunk(cx, cz)
                    .is_some_and(|top| top >= spec.center.y),
                "the vertical streamer must retain authored island {spec:?}"
            );
        }

        let natural = TerrainGenerator::new(12_345);
        assert_eq!(natural.authored_astral_islands(), None);
    }

    #[test]
    fn astral_showcase_landmarks_are_deterministic_grouped_and_bounded() {
        fn authored_cells(generator: &TerrainGenerator, cx: i32, cz: i32) -> Vec<(i32, Voxel)> {
            let mut cells = Vec::new();
            for cy in 0..14 {
                let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                generator.generate(&mut chunk);
                let origin_y = cy * CHUNK_SIZE_I;
                for lz in 0..CHUNK_SIZE {
                    for lx in 0..CHUNK_SIZE {
                        let wx = cx * CHUNK_SIZE_I + lx as i32;
                        let wz = cz * CHUNK_SIZE_I + lz as i32;
                        let surface = generator.surface_height_at(wx, wz);
                        for ly in 0..CHUNK_SIZE {
                            let wy = origin_y + ly as i32;
                            let voxel = chunk.get(lx, ly, lz);
                            if wy > surface.max(WATER_LEVEL) && voxel != AIR {
                                let packed = (ly as i32) << 16 | (lz as i32) << 8 | lx as i32;
                                cells.push((cy << 24 | packed, voxel));
                            }
                        }
                    }
                }
            }
            cells
        }

        let generator = TerrainGenerator::new(12345)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let spawn = generator
            .find_neon_showcase_spawn(0, 0, 4096)
            .expect("Astral Frontier must expose a showcase entry");
        let center_cx = spawn.x.div_euclid(CHUNK_SIZE_I);
        let center_cz = spawn.z.div_euclid(CHUNK_SIZE_I);
        let mut chosen_chunk = None;
        for radius in 0_i32..=32 {
            'ring: for dz in -radius..=radius {
                for dx in -radius..=radius {
                    if radius > 0 && dx.abs() < radius && dz.abs() < radius {
                        continue;
                    }
                    let cx = center_cx + dx;
                    let cz = center_cz + dz;
                    for i in 0..24usize {
                        let r_pos =
                            column_rand(generator.seed ^ (0xF00D_FACE + i as u32 * 7919), cx, cz);
                        let r_gate =
                            column_rand(generator.seed ^ (0x1234_5678 + i as u32 * 31), cx, cz);
                        let lx = ((r_pos * 65537.0) as usize) % CHUNK_SIZE;
                        let lz = ((r_pos * 997.0) as usize) % CHUNK_SIZE;
                        let wx = cx * CHUNK_SIZE_I + lx as i32;
                        let wz = cz * CHUNK_SIZE_I + lz as i32;
                        let surface = generator.surface_height_at(wx, wz);
                        let biome = generator.biome_at(wx, wz);
                        if r_gate > astral_prop_density(biome) {
                            continue;
                        }
                        let slope = cardinal_surface_slope(&generator, wx, wz, surface);
                        if slope < 2 {
                            chosen_chunk = Some((cx, cz));
                            break 'ring;
                        }
                    }
                }
            }
            if chosen_chunk.is_some() {
                break;
            }
        }
        let (cx, cz) = chosen_chunk.expect("showcase entry needs grouped authored landmarks");
        let first = authored_cells(&generator, cx, cz);
        let replay = TerrainGenerator::new(12345)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let second = authored_cells(&replay, cx, cz);

        assert_eq!(first, second);
        assert!(first.len() >= 4, "single glitter cells are not landmarks");
        assert!(
            first.len() <= 2_048,
            "one horizontal chunk exceeded the authored silhouette budget"
        );
    }

    #[test]
    fn astral_floating_island_sites_are_sparse_replayable_and_profile_scoped() {
        let generator =
            TerrainGenerator::new(12345).with_world_profile(WorldProfile::AstralFrontier);
        let replay = TerrainGenerator::new(12345).with_world_profile(WorldProfile::AstralFrontier);
        let natural = TerrainGenerator::new(12345);
        let mut found = None;

        'search: for owner_z in -32..=32 {
            for owner_x in -32..=32 {
                if let Some(spec) = generator.floating_island_for_cell(owner_x, owner_z) {
                    found = Some((owner_x, owner_z, spec));
                    break 'search;
                }
            }
        }
        let (owner_x, owner_z, spec) = found.expect("Astral profile needs bounded island sites");
        let ground = generator.surface_height_at(spec.center.x, spec.center.z);

        assert_eq!(
            replay.floating_island_for_cell(owner_x, owner_z),
            Some(spec)
        );
        assert_eq!(natural.floating_island_for_cell(owner_x, owner_z), None);
        assert!((ASTRAL_ISLAND_CLEARANCE_MIN..=ASTRAL_ISLAND_CLEARANCE_MAX)
            .contains(&(spec.center.y - ground)));
        assert!((8..=ASTRAL_ISLAND_MAX_RADIUS).contains(&spec.radius_x));
        assert!((8..=ASTRAL_ISLAND_MAX_RADIUS).contains(&spec.radius_z));
        assert!(
            generator
                .decorative_top_hint_for_chunk(
                    spec.center.x.div_euclid(CHUNK_SIZE_I),
                    spec.center.z.div_euclid(CHUNK_SIZE_I),
                )
                .is_some_and(|top| top >= spec.center.y),
            "streaming must retain the island's vertical chunk"
        );
    }

    #[test]
    fn floating_island_is_one_landable_connected_object_inside_geometry_budget() {
        let spec = FloatingIslandSpec {
            center: bevy::math::IVec3::new(0, 90, 0),
            radius_x: 10,
            radius_z: 8,
            thickness: 9,
            cap: BlockType::Grass,
            sub: BlockType::Dirt,
            core: BlockType::Stone,
            tip: BlockType::Crystal,
        };
        let mut painted = Vec::new();
        for cy in 4..=5 {
            for cz in -1..=0 {
                for cx in -1..=0 {
                    let pos = ChunkPos::new(cx, cy, cz);
                    let mut chunk = Chunk::new(pos);
                    TerrainGenerator::paint_floating_island_into_chunk(&mut chunk, spec);
                    for ly in 0..CHUNK_SIZE {
                        for lz in 0..CHUNK_SIZE {
                            for lx in 0..CHUNK_SIZE {
                                let voxel = chunk.get(lx, ly, lz);
                                if voxel == AIR {
                                    continue;
                                }
                                painted.push((
                                    (
                                        cx * CHUNK_SIZE_I + lx as i32,
                                        cy * CHUNK_SIZE_I + ly as i32,
                                        cz * CHUNK_SIZE_I + lz as i32,
                                    ),
                                    voxel,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let positions: HashSet<_> = painted.iter().map(|(position, _)| *position).collect();
        let root = (spec.center.x, spec.center.y, spec.center.z);
        let mut visited = HashSet::from([root]);
        let mut pending = vec![root];
        while let Some((x, y, z)) = pending.pop() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let neighbour = (x + dx, y + dy, z + dz);
                if positions.contains(&neighbour) && visited.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }

        assert_eq!(
            painted
                .iter()
                .find(|(position, _)| *position == root)
                .map(|(_, voxel)| BlockType::from_voxel(*voxel)),
            Some(BlockType::Grass)
        );
        assert_eq!(
            visited, positions,
            "island and crystal keel must be one object"
        );
        assert!((100..=5_000).contains(&positions.len()));
    }

    #[test]
    fn natural_spawn_finds_walkable_non_showcase_ground() {
        let generator = TerrainGenerator::new(12345);
        let spawn = generator
            .find_natural_spawn(0, 0, 4096)
            .expect("normal worlds need a nearby safe terrain entry");

        assert!(!spawn.biome.is_showcase_terrain());
        assert!(spawn.y > WATER_LEVEL + 4);
    }

    #[test]
    fn default_generated_chunks_do_not_scatter_showcase_blocks() {
        let generator = TerrainGenerator::new(12345);
        let showcase_blocks: [Voxel; 9] = [
            BlockType::Crystal.into(),
            BlockType::LuminiteCrystal.into(),
            BlockType::MagnetiteOre.into(),
            BlockType::IridiumVein.into(),
            BlockType::AlienMoss.into(),
            BlockType::BoneRock.into(),
            BlockType::GlowSand.into(),
            BlockType::Basalt.into(),
            BlockType::Lava.into(),
        ];
        let sample_columns = [(-8, -8), (-3, 5), (0, 0), (6, -4), (11, 9)];

        for (cx, cz) in sample_columns {
            for cy in 0..10 {
                let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                generator.generate(&mut chunk);
                for ly in 0..CHUNK_SIZE {
                    for lz in 0..CHUNK_SIZE {
                        for lx in 0..CHUNK_SIZE {
                            let voxel = chunk.get(lx, ly, lz);
                            assert!(
                                !showcase_blocks.contains(&voxel),
                                "default chunk {cx},{cy},{cz} unexpectedly contains showcase block {voxel}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn default_surface_heights_stay_in_playable_streaming_range() {
        let generator = TerrainGenerator::new(12345);
        let mut highest = i32::MIN;

        for z in (-12_000..=12_000).step_by(384) {
            for x in (-12_000..=12_000).step_by(384) {
                let surface = generator.surface_height_at(x, z);
                highest = highest.max(surface);
            }
        }

        assert!(
            highest <= 220,
            "default terrain should stay playable for normal streaming budgets; highest sample was {highest}"
        );
    }

    #[test]
    fn scenery_quality_scales_tree_density_without_changing_seed() {
        let lean = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lean);
        let lush = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);

        assert!(
            lush.tree_density_for_biome(Biome::Forest) > lean.tree_density_for_biome(Biome::Forest)
        );
        assert!(
            lush.tree_height_for_biome(Biome::Forest, 0.5).0
                > lean.tree_height_for_biome(Biome::Forest, 0.5).0
        );
        assert_eq!(
            lush.tree_height_for_biome(Biome::Forest, 0.4).1,
            BlockType::Leaves
        );
    }

    #[test]
    fn off_and_lean_ecotones_are_strict_no_ops() {
        use crate::settings::SceneryQuality;

        for quality in [SceneryQuality::Off, SceneryQuality::Lean] {
            let generator = TerrainGenerator::new(0xEC07_0AE1).with_scenery_quality(quality);
            for z in (-96..=96).step_by(3) {
                for x in (-96..=96).step_by(3) {
                    assert_eq!(
                        generator.clustered_ecotone_choice(
                            BlockType::Grass,
                            BlockType::SavannaGrass,
                            x,
                            z,
                        ),
                        BlockType::Grass,
                    );
                    assert_eq!(
                        generator.ecotone_surface_block(
                            Biome::Plains,
                            BlockType::Grass,
                            WATER_LEVEL + 20,
                            0.0,
                            x,
                            z,
                        ),
                        BlockType::Grass,
                    );
                }
            }
        }
    }

    #[test]
    fn balanced_and_lush_ecotones_are_deterministic_bounded_clusters() {
        use crate::settings::SceneryQuality;

        for quality in [SceneryQuality::Balanced, SceneryQuality::Lush] {
            let generator = TerrainGenerator::new(0xEC07_0AE1).with_scenery_quality(quality);
            let replay = TerrainGenerator::new(0xEC07_0AE1).with_scenery_quality(quality);
            let mut changed = HashSet::new();

            for z in -64..64 {
                for x in -64..64 {
                    let first = generator.clustered_ecotone_choice(
                        BlockType::Grass,
                        BlockType::SavannaGrass,
                        x,
                        z,
                    );
                    let second = replay.clustered_ecotone_choice(
                        BlockType::Grass,
                        BlockType::SavannaGrass,
                        x,
                        z,
                    );
                    assert_eq!(first, second, "same seed must replay at {x},{z}");
                    if first == BlockType::SavannaGrass {
                        changed.insert((x, z));
                    }
                }
            }

            let total = 128usize * 128;
            assert!(
                changed.len() > total / 40,
                "{quality:?} ecotone should visibly feather a boundary"
            );
            assert!(
                changed.len() < total * 3 / 4,
                "{quality:?} ecotone must preserve the dominant biome"
            );

            let connected = changed
                .iter()
                .filter(|&&(x, z)| {
                    [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .any(|&(dx, dz)| changed.contains(&(x + dx, z + dz)))
                })
                .count();
            assert!(
                connected * 10 >= changed.len() * 9,
                "{quality:?} ecotone should form clusters, not isolated speckle"
            );
        }
    }

    #[test]
    fn karst_shelves_and_gentle_slopes_can_live_but_cliffs_are_continuous_limestone() {
        let (top, sub, _) = TerrainGenerator::blocks_for(Biome::Karst);
        assert_eq!((top, sub), (BlockType::MossStone, BlockType::Limestone));
        assert_eq!(
            TerrainGenerator::slope_surface_layers(Biome::Karst, 0, top, sub),
            (BlockType::MossStone, BlockType::Limestone)
        );
        assert_eq!(
            TerrainGenerator::slope_surface_layers(Biome::Karst, 1, top, sub),
            (BlockType::MossStone, BlockType::Limestone),
            "a one-block grade may carry a broad moss or meadow skin"
        );
        for slope in 2..=8 {
            assert_eq!(
                TerrainGenerator::slope_surface_layers(Biome::Karst, slope, top, sub),
                (BlockType::Limestone, BlockType::Limestone),
                "karst cliff {slope} must not alternate moss and limestone per stair"
            );
        }

        let (forest_top, forest_sub, _) = TerrainGenerator::blocks_for(Biome::Forest);
        assert_eq!(
            TerrainGenerator::slope_surface_layers(Biome::Forest, 1, forest_top, forest_sub,),
            (BlockType::Grass, BlockType::Dirt),
            "the karst fix must not flatten ordinary biome transitions"
        );
    }

    #[test]
    fn karst_floor_forms_broad_meadow_moss_and_stone_masses_without_contour_stripes() {
        let generator = TerrainGenerator::new(12345);
        let mut meadow = 0usize;
        let mut moss = 0usize;
        let mut limestone = 0usize;

        for z in (-192..=192).step_by(3) {
            for x in (-192..=192).step_by(3) {
                let material =
                    generator.surface_detail_block(Biome::Karst, BlockType::MossStone, 0, x, z);
                assert_eq!(
                    material,
                    generator.surface_detail_block(Biome::Karst, BlockType::MossStone, 0, x, z,),
                    "shelf ecology must replay exactly"
                );
                match material {
                    BlockType::Grass => meadow += 1,
                    BlockType::MossStone => moss += 1,
                    BlockType::Limestone => limestone += 1,
                    other => panic!("unexpected karst shelf material {other:?}"),
                }
                assert_eq!(
                    generator.surface_detail_block(Biome::Karst, BlockType::Limestone, 2, x, z,),
                    BlockType::Limestone,
                    "steep karst must never regain a striped moss cap"
                );
            }
        }

        let total = meadow + moss + limestone;
        assert!(meadow > total / 6, "sheltered karst needs living meadow");
        assert!(moss > total / 12, "moss shelves should remain visible");
        assert!(
            limestone > total / 12,
            "exposed karst must remain geological"
        );
        assert!(
            meadow.max(moss).max(limestone) < total * 4 / 5,
            "one surface material must not flatten the entire karst floor"
        );
    }

    #[test]
    fn balanced_and_lush_bonsai_offer_four_bounded_silhouettes() {
        use crate::settings::SceneryQuality;

        for (quality, budget) in [
            (SceneryQuality::Balanced, 220usize),
            (SceneryQuality::Lush, 480usize),
        ] {
            let generator = TerrainGenerator::new(12345).with_scenery_quality(quality);
            let mut silhouettes = Vec::new();
            for style_roll in [0.12, 0.35, 0.58, 0.88] {
                let (profile, blocks) =
                    paint_tree_for_test(&generator, Biome::Forest, style_roll, 14);
                assert!(
                    blocks.len() <= budget,
                    "{quality:?} {:?} tree used {} blocks, budget is {budget}",
                    profile.silhouette,
                    blocks.len(),
                );
                silhouettes.push(profile.silhouette);
            }

            assert_eq!(
                silhouettes,
                vec![
                    TreeSilhouette::Conifer,
                    TreeSilhouette::Layered,
                    TreeSilhouette::Windswept,
                    TreeSilhouette::Crowned,
                ],
            );
        }
    }

    #[test]
    fn riparian_tree_is_derived_from_real_habitat_not_a_global_species_roll() {
        use crate::settings::SceneryQuality;

        let generator =
            TerrainGenerator::new(0x48D2_09A1).with_scenery_quality(SceneryQuality::Lush);
        let focus = generator
            .find_hydrographic_focus(0, 0, 4_096)
            .expect("seed should expose a bounded river course");
        let mut promoted = None;
        'search: for dz in (-96..=96).step_by(2) {
            for dx in (-96..=96).step_by(2) {
                let wx = focus.x + dx;
                let wz = focus.y + dz;
                let surface = generator.surface_height_at(wx, wz);
                let biome = generator.biome_at(wx, wz);
                let Some((baseline, _)) = generator.tree_profile(biome, 0.88) else {
                    continue;
                };
                let profile =
                    generator.adapt_tree_profile_to_site(baseline, biome, wx, wz, surface, 0.88);
                if profile.silhouette == TreeSilhouette::Riparian {
                    promoted = Some((wx, wz, surface, biome, baseline, profile));
                    break 'search;
                }
            }
        }

        let (wx, wz, surface, biome, baseline, profile) =
            promoted.expect("moist river shoulder should promote a gallery tree");
        assert!(profile.branch_reach > baseline.branch_reach);
        assert!(profile.canopy_radius >= baseline.canopy_radius);
        assert!(
            generator
                .hydrographic_field_for_surface(wx as f64, wz as f64, surface as f64)
                .corridor
                > 0.16
        );
        assert!(generator.environment_sample_at(wx, wz).soil_moisture >= 0.68);

        assert_eq!(
            generator
                .adapt_tree_profile_to_site(baseline, biome, wx, wz, surface, 0.10)
                .silhouette,
            baseline.silhouette,
            "the habitat must retain species variety instead of cloning every bank tree"
        );
        let lean = TerrainGenerator::new(0x48D2_09A1).with_scenery_quality(SceneryQuality::Lean);
        assert_eq!(
            lean.adapt_tree_profile_to_site(baseline, biome, wx, wz, surface, 0.88),
            baseline,
            "precision vegetation must remain a quality-scaled feature"
        );
    }

    #[test]
    fn riparian_crown_has_connected_hanging_fringe_inside_geometry_budget() {
        let generator = TerrainGenerator::new(0x48D2_09A1)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let base_y = 14;
        let profile = TreeProfile {
            trunk_height: 9,
            canopy_radius: 4,
            branch_reach: 5,
            tiers: 5,
            max_extent: 7,
            crown_lift: 4,
            silhouette: TreeSilhouette::Riparian,
        };
        let blocks = paint_specific_tree_for_test(&generator, profile, BlockType::Leaves, base_y);
        assert!(
            blocks.len() <= 600,
            "gallery tree exceeded bounded voxel budget"
        );

        let positions: HashSet<_> = blocks.iter().map(|&(x, y, z, _)| (x, y, z)).collect();
        let foliage: HashSet<_> = blocks
            .iter()
            .filter(|&&(_, _, _, voxel)| BlockType::from_voxel(voxel) != BlockType::Wood)
            .map(|&(x, y, z, _)| (x, y, z))
            .collect();
        assert!(foliage.iter().any(|&(x, y, z)| {
            foliage.contains(&(x, y + 1, z)) && foliage.contains(&(x, y + 2, z))
        }));
        assert!(
            foliage
                .iter()
                .map(|&(_, y, _)| y)
                .min()
                .is_some_and(|lowest| lowest < base_y + profile.trunk_height / 2),
            "hanging foliage must descend below the first lateral crown tier"
        );

        let root = (8, base_y, 8);
        let mut visited = HashSet::from([root]);
        let mut pending = vec![root];
        while let Some((x, y, z)) = pending.pop() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let neighbour = (x + dx, y + dy, z + dz);
                if positions.contains(&neighbour) && visited.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        assert_eq!(
            visited, positions,
            "gallery tree must remain one editable object"
        );
    }

    #[test]
    fn river_flow_selects_a_stable_cardinal_crown_axis() {
        assert_eq!(cardinal_direction_index([1.0, 0.2]), 0);
        assert_eq!(cardinal_direction_index([0.1, 1.0]), 1);
        assert_eq!(cardinal_direction_index([-1.0, 0.2]), 2);
        assert_eq!(cardinal_direction_index([0.1, -1.0]), 3);
        assert_eq!(cardinal_direction_index([f32::NAN, 1.0]), 0);
    }

    #[test]
    fn lush_tree_roots_jitter_across_the_owner_cell_instead_of_chunk_centre_grid() {
        let mut xs = HashSet::new();
        let mut zs = HashSet::new();
        for owner_z in -24..=24 {
            for owner_x in -24..=24 {
                for candidate in 0..4 {
                    let root = tree_root_in_owner_cell(12345, owner_x, owner_z, candidate);
                    assert!((1..CHUNK_SIZE_I - 1).contains(&root.x));
                    assert!((1..CHUNK_SIZE_I - 1).contains(&root.y));
                    xs.insert(root.x);
                    zs.insert(root.y);
                }
            }
        }

        assert_eq!(xs.len(), (CHUNK_SIZE_I - 2) as usize);
        assert_eq!(zs.len(), (CHUNK_SIZE_I - 2) as usize);
        assert!(xs.contains(&1) && xs.contains(&(CHUNK_SIZE_I - 2)));
        assert!(zs.contains(&1) && zs.contains(&(CHUNK_SIZE_I - 2)));
    }

    #[test]
    fn large_tree_crown_replays_across_horizontal_chunk_seam() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let (profile, leaf_kind) = generator.tree_profile(Biome::Forest, 0.88).unwrap();
        // Keep the broad crown inside this vertical slice so the assertion
        // isolates the horizontal seam contract.
        let base_y = 2;
        let origin_y = 0;

        let mut west = Chunk::new(ChunkPos::new(0, 0, 0));
        west.set(15, (base_y - 1) as usize, 8, BlockType::Grass.into());
        let mut east = Chunk::new(ChunkPos::new(1, 0, 0));

        assert!(generator
            .try_place_bonsai_tree(&mut west, 15, 8, base_y, origin_y, profile, leaf_kind,));
        assert!(generator
            .try_place_bonsai_tree(&mut east, -1, 8, base_y, origin_y, profile, leaf_kind,));

        let natural = |voxel| {
            matches!(
                BlockType::from_voxel(voxel),
                BlockType::Wood
                    | BlockType::Leaves
                    | BlockType::JungleLeaves
                    | BlockType::BlossomLeaves
            )
        };
        let west_seam: HashSet<_> = (0..CHUNK_SIZE)
            .flat_map(|y| (0..CHUNK_SIZE).map(move |z| (y, z)))
            .filter(|&(y, z)| natural(west.get(CHUNK_SIZE - 1, y, z)))
            .collect();
        let east_seam: HashSet<_> = (0..CHUNK_SIZE)
            .flat_map(|y| (0..CHUNK_SIZE).map(move |z| (y, z)))
            .filter(|&(y, z)| natural(east.get(0, y, z)))
            .collect();

        assert!(!east_seam.is_empty(), "the crown must enter its neighbour");
        assert!(
            west_seam.iter().any(|cell| east_seam.contains(cell)),
            "at least one face-adjacent leaf/branch pair must bridge the seam"
        );
    }

    #[test]
    fn crowned_broadleaf_uses_a_connected_vertical_cloud_not_flat_pads() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Balanced);
        let base_y = 14;
        let (profile, blocks) = paint_tree_for_test(&generator, Biome::Forest, 0.88, base_y);
        assert_eq!(profile.silhouette, TreeSilhouette::Crowned);

        let crown_center_y = base_y + profile.trunk_height - 1 + profile.crown_lift;
        let foliage: Vec<_> = blocks
            .iter()
            .filter(|&&(_, _, _, voxel)| BlockType::from_voxel(voxel) != BlockType::Wood)
            .collect();
        assert!(foliage.iter().any(|&&(_, y, _, _)| y < crown_center_y));
        assert!(foliage.iter().any(|&&(_, y, _, _)| y > crown_center_y));

        let positions: HashSet<_> = blocks.iter().map(|&(x, y, z, _)| (x, y, z)).collect();
        let root = (8, base_y, 8);
        let mut visited = HashSet::from([root]);
        let mut pending = vec![root];
        while let Some((x, y, z)) = pending.pop() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let neighbour = (x + dx, y + dy, z + dz);
                if positions.contains(&neighbour) && visited.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        assert_eq!(visited, positions, "broadleaf cloud must remain one object");
    }

    #[test]
    fn lush_conifer_is_tapered_connected_and_uses_heavier_needles() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let base_y = 14;
        let (profile, blocks) = paint_tree_for_test(&generator, Biome::Forest, 0.12, base_y);

        assert_eq!(profile.silhouette, TreeSilhouette::Conifer);
        assert!(blocks.iter().all(|&(_, _, _, voxel)| matches!(
            BlockType::from_voxel(voxel),
            BlockType::Wood | BlockType::JungleLeaves
        )));

        let positions: HashSet<_> = blocks.iter().map(|&(x, y, z, _)| (x, y, z)).collect();
        let root = (8, base_y, 8);
        let mut visited = HashSet::from([root]);
        let mut pending = vec![root];
        while let Some((x, y, z)) = pending.pop() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let neighbour = (x + dx, y + dy, z + dz);
                if positions.contains(&neighbour) && visited.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        assert_eq!(
            visited, positions,
            "every needle cluster must join the trunk"
        );

        let crown_split_y = base_y + profile.trunk_height * 2 / 3;
        let horizontal_radius = |lower_half: bool| {
            blocks
                .iter()
                .filter(|&&(_, y, _, voxel)| {
                    BlockType::from_voxel(voxel) == BlockType::JungleLeaves
                        && ((y <= crown_split_y) == lower_half)
                })
                .map(|&(x, _, z, _)| (x - 8).abs().max((z - 8).abs()))
                .max()
                .unwrap_or(0)
        };
        let lower_radius = horizontal_radius(true);
        let upper_radius = horizontal_radius(false);
        assert!(
            lower_radius > upper_radius,
            "conifer must taper toward its tip: lower={lower_radius}, upper={upper_radius}"
        );
        assert!(
            blocks.len() <= 480,
            "lush conifer exceeded its geometry budget"
        );
    }

    #[test]
    fn natural_grass_surface_detail_does_not_create_dark_single_patch_noise() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let mut checked = 0;

        for z in (-512..=512).step_by(13) {
            for x in (-512..=512).step_by(11) {
                for biome in [Biome::Plains, Biome::Forest, Biome::Jungle] {
                    checked += 1;
                    let detail = generator.surface_detail_block(biome, BlockType::Grass, 0, x, z);
                    assert!(
                        !matches!(detail, BlockType::Dirt | BlockType::MossStone),
                        "lush natural grass at {x},{z} should not turn into dark isolated patch noise"
                    );
                }
            }
        }

        assert!(checked > 10_000);
    }

    #[test]
    fn volcanic_surface_detail_keeps_dry_ground_non_fluid_while_channel_fill_remains_lava() {
        let generator = TerrainGenerator::new(12_345)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);

        for z in (-512..=512).step_by(13) {
            for x in (-512..=512).step_by(11) {
                assert_eq!(
                    generator.surface_detail_block(
                        Biome::VolcanicWaste,
                        BlockType::Basalt,
                        0,
                        x,
                        z,
                    ),
                    BlockType::Basalt,
                    "surface grain must not create a disconnected Lava top at {x},{z}"
                );
            }
        }

        let layout = generator.astral_layout().expect("Astral layout");
        let canyon_local_z = 82;
        let canyon_local_x =
            (-58.0 + (((canyon_local_z as f64) + 34.0) * 0.026).sin() * 13.0).round() as i32;
        let canyon = layout.world_from_local(IVec2::new(canyon_local_x, canyon_local_z));
        assert_eq!(generator.biome_at(canyon.x, canyon.y), Biome::VolcanicWaste);
        assert!(generator.surface_height_at(canyon.x, canyon.y) < 52);

        let mut chunk = Chunk::new(ChunkPos::new(
            canyon.x.div_euclid(CHUNK_SIZE_I),
            52_i32.div_euclid(CHUNK_SIZE_I),
            canyon.y.div_euclid(CHUNK_SIZE_I),
        ));
        generator.generate(&mut chunk);
        assert_eq!(
            BlockType::from_voxel(chunk.get(
                canyon.x.rem_euclid(CHUNK_SIZE_I) as usize,
                52_i32.rem_euclid(CHUNK_SIZE_I) as usize,
                canyon.y.rem_euclid(CHUNK_SIZE_I) as usize,
            )),
            BlockType::Lava,
            "the explicit bounded channel-fill authority must still produce Lava"
        );
    }

    #[test]
    fn volcanic_surface_detail_is_legacy_lava_only_in_v1() {
        let v1 = TerrainGenerator::new(12_345)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_terrain_grammar(TerrainGrammarVersion::V1);
        let v2 = TerrainGenerator::new(12_345)
            .with_world_profile(WorldProfile::AstralFrontier)
            .with_terrain_grammar(TerrainGrammarVersion::V2);
        let coordinate = (-512..=512).step_by(7).find_map(|z| {
            (-512..=512).step_by(7).find_map(|x| {
                (v1.surface_detail_block(Biome::VolcanicWaste, BlockType::Basalt, 0, x, z)
                    == BlockType::Lava)
                    .then_some((x, z))
            })
        });
        let (x, z) = coordinate.expect("bounded sample should include V1 lava grain");

        assert_eq!(
            v1.surface_detail_block(Biome::VolcanicWaste, BlockType::Basalt, 0, x, z),
            BlockType::Lava
        );
        assert_eq!(
            v2.surface_detail_block(Biome::VolcanicWaste, BlockType::Basalt, 0, x, z),
            BlockType::Basalt
        );
        assert_eq!(
            v1.surface_detail_block(Biome::VolcanicWaste, BlockType::Basalt, 1, x, z),
            BlockType::Basalt,
            "the exact V1 rule only promoted flat top voxels"
        );
    }

    #[test]
    fn off_quality_has_no_blocks_above_the_canonical_surface() {
        let generator =
            TerrainGenerator::new(12345).with_scenery_quality(crate::settings::SceneryQuality::Off);
        let sample_columns = [(-8, -8), (-3, 5), (0, 0), (6, -4), (11, 9)];

        for (cx, cz) in sample_columns {
            let mut surfaces = [[0i32; CHUNK_SIZE]; CHUNK_SIZE];
            for (lz, row) in surfaces.iter_mut().enumerate() {
                for (lx, surface) in row.iter_mut().enumerate() {
                    let wx = cx * CHUNK_SIZE_I + lx as i32;
                    let wz = cz * CHUNK_SIZE_I + lz as i32;
                    *surface = generator.surface_height_at(wx, wz);
                }
            }

            for cy in 0..14 {
                let mut chunk = Chunk::new(ChunkPos::new(cx, cy, cz));
                generator.generate(&mut chunk);
                let origin_y = cy * CHUNK_SIZE_I;
                for (lz, row) in surfaces.iter().enumerate() {
                    for (lx, &surface) in row.iter().enumerate() {
                        for ly in 0..CHUNK_SIZE {
                            let wy = origin_y + ly as i32;
                            if wy <= surface {
                                continue;
                            }
                            let expected = if wy <= WATER_LEVEL {
                                BlockType::Water.into()
                            } else {
                                AIR
                            };
                            assert_eq!(
                                chunk.get(lx, ly, lz),
                                expected,
                                "off quality placed a block above canonical surface at {wx},{wy},{wz}",
                                wx = cx * CHUNK_SIZE_I + lx as i32,
                                wz = cz * CHUNK_SIZE_I + lz as i32,
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn macro_height_field_has_no_single_column_spikes() {
        for seed in [7, 12345, 0xA11C_E551] {
            let generator = TerrainGenerator::new(seed);
            for z in (-512..=512).step_by(17) {
                for x in (-512..=512).step_by(19) {
                    let height = generator.surface_height_at(x, z);
                    if height <= WATER_LEVEL + 2 {
                        continue;
                    }
                    let mut neighbour_max = i32::MIN;
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            if dx == 0 && dz == 0 {
                                continue;
                            }
                            neighbour_max =
                                neighbour_max.max(generator.surface_height_at(x + dx, z + dz));
                        }
                    }
                    assert!(
                        height - neighbour_max <= 3,
                        "isolated terrain spike at {x},{z}: {height} vs neighbour {neighbour_max}"
                    );
                }
            }
        }
    }

    #[test]
    fn tree_profiles_scale_under_fixed_geometry_budgets() {
        use crate::settings::SceneryQuality;

        assert!(TerrainGenerator::new(12345)
            .with_scenery_quality(SceneryQuality::Off)
            .tree_profile(Biome::Forest, 0.4)
            .is_none());

        let mut counts = Vec::new();
        let mut profiles = Vec::new();
        for (quality, budget) in [
            (SceneryQuality::Lean, 100usize),
            (SceneryQuality::Balanced, 220usize),
            (SceneryQuality::Lush, 480usize),
        ] {
            let generator = TerrainGenerator::new(12345).with_scenery_quality(quality);
            let (profile, blocks) = paint_tree_for_test(&generator, Biome::Forest, 0.4, 14);
            assert!(
                blocks.len() <= budget,
                "{quality:?} tree used {} blocks, budget is {budget}",
                blocks.len()
            );
            counts.push(blocks.len());
            profiles.push(profile);
        }

        assert!(counts[0] < counts[1] && counts[1] < counts[2]);
        assert!(profiles[0].tiers < profiles[1].tiers && profiles[1].tiers < profiles[2].tiers);
        assert!(
            profiles[0].total_height() < profiles[1].total_height()
                && profiles[1].total_height() < profiles[2].total_height()
        );
    }

    #[test]
    fn tree_trunk_rejects_missing_local_ground_support() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Balanced);
        let (profile, leaf_kind) = generator.tree_profile(Biome::Forest, 0.4).unwrap();
        let mut empty_chunk = Chunk::new(ChunkPos::new(0, 0, 0));

        assert!(!generator.try_place_bonsai_tree(
            &mut empty_chunk,
            8,
            8,
            14,
            0,
            profile,
            leaf_kind,
        ));
        assert_eq!(empty_chunk.get(8, 14, 8), AIR);
    }

    #[test]
    fn lush_bonsai_is_large_grounded_and_fully_connected() {
        let generator = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);
        let (profile, blocks) = paint_tree_for_test(&generator, Biome::Forest, 0.4, 14);
        let positions: HashSet<_> = blocks.iter().map(|&(x, y, z, _)| (x, y, z)).collect();
        let root = (8, 14, 8);
        assert!(
            positions.contains(&root),
            "trunk must start directly above ground"
        );

        let mut seen = HashSet::new();
        let mut pending = vec![root];
        seen.insert(root);
        while let Some((x, y, z)) = pending.pop() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let neighbour = (x + dx, y + dy, z + dz);
                if positions.contains(&neighbour) && seen.insert(neighbour) {
                    pending.push(neighbour);
                }
            }
        }
        assert_eq!(
            seen.len(),
            positions.len(),
            "every branch and leaf pad must connect to the grounded trunk"
        );

        let min_x = positions.iter().map(|p| p.0).min().unwrap();
        let max_x = positions.iter().map(|p| p.0).max().unwrap();
        let min_y = positions.iter().map(|p| p.1).min().unwrap();
        let max_y = positions.iter().map(|p| p.1).max().unwrap();
        let min_z = positions.iter().map(|p| p.2).min().unwrap();
        let max_z = positions.iter().map(|p| p.2).max().unwrap();
        assert!(max_x - min_x + 1 >= 11);
        assert!(max_z - min_z + 1 >= 11);
        assert!(max_y - min_y + 1 >= 18);
        assert!(min_y.div_euclid(CHUNK_SIZE_I) != max_y.div_euclid(CHUNK_SIZE_I));
        assert_eq!(max_y - min_y + 1, profile.total_height());
    }

    #[test]
    fn lush_plains_and_forests_keep_large_scale_without_global_blossom_bias() {
        let lush = TerrainGenerator::new(12345)
            .with_scenery_quality(crate::settings::SceneryQuality::Lush);

        let (plains_h, plains_leaves) = lush.tree_height_for_biome(Biome::Plains, 0.40);
        let (forest_h, forest_leaves) = lush.tree_height_for_biome(Biome::Forest, 0.40);

        assert!(plains_h >= 12, "lush plains bonsai should not be tiny");
        assert!(
            forest_h >= 13,
            "lush forest bonsai should read as a real canopy"
        );
        assert_eq!(plains_leaves, BlockType::Leaves);
        assert_eq!(forest_leaves, BlockType::Leaves);
    }
}

// Derive Copy/Clone only for lookup (biome blocks helper is `&self`-free).
impl Clone for TerrainGenerator {
    fn clone(&self) -> Self {
        Self::from_identity(self.generation_identity())
    }
}

#[inline]
fn smoothstep(edge0: f64, edge1: f64, value: f64) -> f64 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Cheap deterministic hash â†’ float in [0,1) keyed by (seed, x, z).
/// Used by the decoration pass so tree placement is stable per-seed.
#[inline]
fn column_rand(seed: u32, x: i32, z: i32) -> f64 {
    let mut h = seed as u64;
    h ^= (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(27).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= (z as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h = h.rotate_left(31).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h >> 11) as f64) * (1.0 / (1u64 << 53) as f64)
}

/// Stable root jitter inside a logical 16x16 ownership cell. A one-voxel
/// inset avoids putting the trunk itself exactly on a seam; the crown is free
/// to cross because every touched chunk replays this same root.
#[inline]
fn tree_root_in_owner_cell(seed: u32, owner_x: i32, owner_z: i32, candidate: usize) -> IVec2 {
    const ROOT_INSET: i32 = 1;
    const ROOT_SPAN: i32 = CHUNK_SIZE_I - ROOT_INSET * 2;
    let x_roll = column_rand(
        seed ^ 0x71EE_1001_u32.wrapping_add(candidate as u32 * 977),
        owner_x,
        owner_z,
    );
    let z_roll = column_rand(
        seed ^ 0x71EE_2002_u32.wrapping_add(candidate as u32 * 991),
        owner_x,
        owner_z,
    );
    IVec2::new(
        ROOT_INSET + ((x_roll * ROOT_SPAN as f64) as i32).min(ROOT_SPAN - 1),
        ROOT_INSET + ((z_roll * ROOT_SPAN as f64) as i32).min(ROOT_SPAN - 1),
    )
}

fn set_tree_wood(chunk: &mut Chunk, lx: i32, wy: i32, lz: i32, origin_y: i32) {
    if lx < 0 || lz < 0 || lx >= CHUNK_SIZE_I || lz >= CHUNK_SIZE_I {
        return;
    }
    let ly = wy - origin_y;
    if ly < 0 || ly >= CHUNK_SIZE_I {
        return;
    }
    let current = chunk.get(lx as usize, ly as usize, lz as usize);
    let replaceable = [
        AIR,
        BlockType::Leaves.into(),
        BlockType::JungleLeaves.into(),
        BlockType::BlossomLeaves.into(),
    ];
    if replaceable.contains(&current) {
        chunk.set(
            lx as usize,
            ly as usize,
            lz as usize,
            BlockType::Wood.into(),
        );
    }
}

/// Chunk-clipped leaf write used by cross-chunk natural trees. Keeping the
/// coordinates signed is essential: a crown replayed from the west/north
/// ownership cell legitimately enters this chunk with a negative local root.
#[inline]
fn set_tree_leaf(chunk: &mut Chunk, lx: i32, wy: i32, lz: i32, block: BlockType, origin_y: i32) {
    if !(0..CHUNK_SIZE_I).contains(&lx) || !(0..CHUNK_SIZE_I).contains(&lz) {
        return;
    }
    let ly = wy - origin_y;
    if !(0..CHUNK_SIZE_I).contains(&ly) {
        return;
    }
    if chunk.get(lx as usize, ly as usize, lz as usize) == AIR {
        chunk.set(lx as usize, ly as usize, lz as usize, block.into());
    }
}

/// Connected irregular ellipsoid for broadleaf crowns.
///
/// A compact filled core guarantees six-neighbour connectivity to the branch
/// at its centre. A one-voxel shell is stochastically opened, and a shell
/// voxel is admitted only when it touches the core. Keeping the core below
/// half the ellipsoid radius avoids the solid green cuboids produced by the
/// old 72%-filled body while retaining one editable natural object.
#[allow(clippy::too_many_arguments)]
fn place_leaf_cloud(
    chunk: &mut Chunk,
    centre_x: i32,
    centre_y: i32,
    centre_z: i32,
    horizontal_radius: i32,
    vertical_radius: i32,
    leaf_kind: BlockType,
    origin_y: i32,
    silhouette_seed: u32,
) {
    let horizontal_radius = horizontal_radius.max(1);
    let vertical_radius = vertical_radius.max(1);
    let hr_sq = (horizontal_radius * horizontal_radius) as f64;
    let vr_sq = (vertical_radius * vertical_radius) as f64;

    for dy in -vertical_radius..=vertical_radius {
        for dz in -horizontal_radius..=horizontal_radius {
            for dx in -horizontal_radius..=horizontal_radius {
                let normalized = (dx * dx + dz * dz) as f64 / hr_sq + (dy * dy) as f64 / vr_sq;
                const CORE_LIMIT: f64 = 0.48;
                const OUTER_LIMIT: f64 = 1.10;
                if normalized > OUTER_LIMIT {
                    continue;
                }

                let x_share = dx.abs() as f64 / horizontal_radius as f64;
                let y_share = dy.abs() as f64 / vertical_radius as f64;
                let z_share = dz.abs() as f64 / horizontal_radius as f64;
                let (inward_dx, inward_dy, inward_dz) = if y_share >= x_share && y_share >= z_share
                {
                    (dx, dy - dy.signum(), dz)
                } else if x_share >= z_share {
                    (dx - dx.signum(), dy, dz)
                } else {
                    (dx, dy, dz - dz.signum())
                };
                let inward_normalized = (inward_dx * inward_dx + inward_dz * inward_dz) as f64
                    / hr_sq
                    + (inward_dy * inward_dy) as f64 / vr_sq;
                let shell = normalized > CORE_LIMIT;
                if shell && inward_normalized > CORE_LIMIT {
                    continue;
                }

                let nx = centre_x + dx;
                let nz = centre_z + dz;
                if shell {
                    let world_x = chunk.pos.x * CHUNK_SIZE_I + nx;
                    let world_z = chunk.pos.z * CHUNK_SIZE_I + nz;
                    let silhouette = column_rand(
                        silhouette_seed
                            ^ 0xC10D_0000_u32.wrapping_add((dy + vertical_radius) as u32 * 4_099),
                        world_x,
                        world_z,
                    );
                    let shell_depth =
                        ((normalized - CORE_LIMIT) / (OUTER_LIMIT - CORE_LIMIT)).clamp(0.0, 1.0);
                    let axial_tip = dx == 0 && dz == 0;
                    let cut = 0.26 + shell_depth * 0.22;
                    if !axial_tip && silhouette < cut {
                        continue;
                    }
                }
                set_tree_leaf(chunk, nx, centre_y + dy, nz, leaf_kind, origin_y);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn place_leaf_pad(
    chunk: &mut Chunk,
    centre_x: i32,
    centre_y: i32,
    centre_z: i32,
    radius: i32,
    layers: i32,
    leaf_kind: BlockType,
    origin_y: i32,
    silhouette_seed: u32,
) {
    for layer in 0..layers {
        let layer_radius = (radius - layer).max(1);
        for dz in -layer_radius..=layer_radius {
            for dx in -layer_radius..=layer_radius {
                let distance_sq = dx * dx + dz * dz;
                if distance_sq > layer_radius * layer_radius + 1 {
                    continue;
                }
                let nx = centre_x + dx;
                let nz = centre_z + dz;
                let inner_radius = (layer_radius - 1).max(0);
                let on_edge = layer_radius >= 2 && distance_sq > inner_radius * inner_radius + 1;
                let world_x = chunk.pos.x * CHUNK_SIZE_I + nx;
                let world_z = chunk.pos.z * CHUNK_SIZE_I + nz;
                let silhouette = column_rand(
                    silhouette_seed ^ 0x1EA5_0000_u32.wrapping_add(layer as u32 * 4099),
                    world_x,
                    world_z,
                );
                // Only perforate the outer ring. The complete inner disk is
                // a connectivity spine, so every retained lobe stays joined
                // to its branch and remains one editable natural object.
                if on_edge && silhouette < 0.30 {
                    continue;
                }
                set_tree_leaf(chunk, nx, centre_y + layer, nz, leaf_kind, origin_y);
                if layer == 0 && on_edge && silhouette > 0.84 {
                    set_tree_leaf(chunk, nx, centre_y - 1, nz, leaf_kind, origin_y);
                }
            }
        }
    }
}

/// Safe block-set for the sci-fi prop pass. Writes `block` into the
/// chunk at local (lx, wy-origin_y, lz) iff the target slot is
/// currently AIR and within chunk bounds. Used by `decorate_props` so
/// a prop never overwrites existing terrain or pushes out-of-bounds.
#[inline]
fn set_safe(chunk: &mut Chunk, lx: usize, wy: i32, lz: usize, block: BlockType, origin_y: i32) {
    if lx >= CHUNK_SIZE || lz >= CHUNK_SIZE {
        return;
    }
    let ly = wy - origin_y;
    if ly < 0 || ly >= CHUNK_SIZE_I {
        return;
    }
    let ly_u = ly as usize;
    if chunk.get(lx, ly_u, lz) == AIR {
        chunk.set(lx, ly_u, lz, block.into());
    }
}

/// Authoritative counterpart for the seed-stable Astral hero precinct. These
/// cells intentionally replace procedural vegetation/props inside a tightly
/// bounded footprint, while never writing outside the current chunk.
#[inline]
fn set_authored(chunk: &mut Chunk, lx: usize, wy: i32, lz: usize, block: BlockType, origin_y: i32) {
    if lx >= CHUNK_SIZE || lz >= CHUNK_SIZE {
        return;
    }
    let ly = wy - origin_y;
    if !(0..CHUNK_SIZE_I).contains(&ly) {
        return;
    }
    chunk.set(lx, ly as usize, lz, block.into());
}
