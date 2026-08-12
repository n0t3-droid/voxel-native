//! Pure, fixed-budget macro-world grammar.
//!
//! This module is compiled by the application, but no live ECS system consumes
//! it yet. It produces descriptive macro fields and planning hints, never
//! authoritative collision, edit, or fluid state. The generator is a
//! zero-sized, stateless value: world travel cannot grow an internal cache
//! because there is none.

use std::mem::size_of;

pub const MACRO_TILE_SIDE: usize = 32;
pub const MACRO_TILE_CELLS: usize = MACRO_TILE_SIDE * MACRO_TILE_SIDE;
pub const MACRO_VERTEX_SIDE: usize = MACRO_TILE_SIDE + 1;
pub const MACRO_VERTEX_CELLS: usize = MACRO_VERTEX_SIDE * MACRO_VERTEX_SIDE;

/// Art-direction scale. This is not a measured geophysical resolution.
pub const MACRO_CELL_SIZE_M: f64 = 256.0;
pub const MACRO_TILE_SPAN_M: f64 = MACRO_CELL_SIZE_M * MACRO_TILE_SIDE as f64;

pub const NEAR_LOD_STRIDE: usize = 1;
pub const MID_LOD_STRIDE: usize = 2;
pub const FAR_LOD_STRIDE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LodReductionRule {
    /// Rainfall and routed-water mass retain their total under downsampling.
    Sum,
    /// Moisture, soil, vegetation, route, and settlement fields retain their mean.
    AreaWeightedMean,
    /// Elevation retains minimum, mean, and maximum instead of one lossy sample.
    EnvelopeAndMean,
    /// Species guilds retain a histogram instead of an unstable majority label.
    Histogram,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodFeedContract {
    pub near_stride: usize,
    pub mid_stride: usize,
    pub far_stride: usize,
    pub core_is_half_open: bool,
    pub edges_use_shared_vertices: bool,
    pub extensive_field_rule: LodReductionRule,
    pub intensive_field_rule: LodReductionRule,
    pub elevation_rule: LodReductionRule,
    pub category_rule: LodReductionRule,
    pub descriptive_only: bool,
}

/// Every visual band must reduce this same macro tile; a band may not reseed an
/// independent terrain function. The live renderer does not consume this yet.
pub const LOD_FEED_CONTRACT: LodFeedContract = LodFeedContract {
    near_stride: NEAR_LOD_STRIDE,
    mid_stride: MID_LOD_STRIDE,
    far_stride: FAR_LOD_STRIDE,
    core_is_half_open: true,
    edges_use_shared_vertices: true,
    extensive_field_rule: LodReductionRule::Sum,
    intensive_field_rule: LodReductionRule::AreaWeightedMean,
    elevation_rule: LodReductionRule::EnvelopeAndMean,
    category_rule: LodReductionRule::Histogram,
    descriptive_only: true,
};

/// Local contributing-area radius and downhill trace horizon.
pub const FLOW_SOURCE_RADIUS: usize = 4;
pub const FLOW_MAX_STEPS: usize = 8;
pub const FLOW_SOURCE_DIAMETER: usize = FLOW_SOURCE_RADIUS * 2 + 1;
pub const FLOW_SOURCE_CELLS: usize = FLOW_SOURCE_DIAMETER * FLOW_SOURCE_DIAMETER;

/// The extra cell guarantees that every bounded trace remains inside scratch.
pub const MACRO_HALO: usize = FLOW_SOURCE_RADIUS + FLOW_MAX_STEPS + 1;
pub const WORK_SIDE: usize = MACRO_TILE_SIDE + MACRO_HALO * 2;
pub const WORK_CELLS: usize = WORK_SIDE * WORK_SIDE;

pub const MAX_RAINFALL_MASS_PER_CELL: f32 = 2.0;
pub const MAX_LOCAL_FLOW_ACCUMULATION: f32 = FLOW_SOURCE_CELLS as f32 * MAX_RAINFALL_MASS_PER_CELL;

/// A work unit is one macro sample, D8 neighbour inspection, or flow-trace visit.
pub const MAX_GENERATION_WORK_UNITS: usize = WORK_CELLS * 2 // geology + rainfall samples
    + WORK_CELLS * 8 // D8 receiver comparisons
    + MACRO_VERTEX_CELLS
    + MACRO_TILE_CELLS * FLOW_SOURCE_CELLS * (FLOW_MAX_STEPS + 1)
    + MACRO_TILE_CELLS * 4 // route water + soil + vegetation + planning
    + MACRO_TILE_SIDE * 4
    + 4; // incoming edge and corner sources

pub const MAX_OUTPUT_BYTES: usize = 72 * 1024;
pub const MAX_SCRATCH_BYTES: usize = 64 * 1024;
pub const MORPHOGENESIS_GRAMMAR_VERSION: u32 = 1;

pub const DESCRIPTIVE_ONLY_REASON: &str =
    "macro grammar only; collision, edits, and fluids remain authoritative elsewhere";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacroTileCoord {
    pub x: i64,
    pub z: i64,
}

impl MacroTileCoord {
    pub const fn new(x: i64, z: i64) -> Self {
        Self { x, z }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MorphogenesisProfile {
    TemperateBasins = 0,
    AridPlateaus = 1,
    AlpineRifts = 2,
    VolcanicArchipelago = 3,
    AstralCrystalline = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MorphogenesisDomain {
    Natural,
    Astral,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpeciesGuild {
    Bare = 0,
    PioneerGrass = 1,
    Shrubland = 2,
    ClosedCanopy = 3,
    Riparian = 4,
    Alpine = 5,
    XericScrub = 6,
    CrystalPioneer = 7,
    LuminousGrove = 8,
}

#[derive(Clone, Copy)]
struct ProfileParameters {
    domain: MorphogenesisDomain,
    base_elevation_m: f64,
    relief_m: f64,
    uplift_m: f64,
    strata_amplitude_m: f64,
    rainfall_mass: f64,
    weathering: f64,
    warmth: f64,
    aridity: f64,
}

impl MorphogenesisProfile {
    pub const fn domain(self) -> MorphogenesisDomain {
        match self {
            Self::AstralCrystalline => MorphogenesisDomain::Astral,
            _ => MorphogenesisDomain::Natural,
        }
    }

    const fn parameters(self) -> ProfileParameters {
        // These are art-direction parameters, not claims about a real biome.
        match self {
            Self::TemperateBasins => ProfileParameters {
                domain: MorphogenesisDomain::Natural,
                base_elevation_m: 260.0,
                relief_m: 1_350.0,
                uplift_m: 720.0,
                strata_amplitude_m: 130.0,
                rainfall_mass: 1.10,
                weathering: 0.78,
                warmth: 0.72,
                aridity: 0.12,
            },
            Self::AridPlateaus => ProfileParameters {
                domain: MorphogenesisDomain::Natural,
                base_elevation_m: 520.0,
                relief_m: 1_750.0,
                uplift_m: 980.0,
                strata_amplitude_m: 230.0,
                rainfall_mass: 0.42,
                weathering: 0.36,
                warmth: 0.90,
                aridity: 0.78,
            },
            Self::AlpineRifts => ProfileParameters {
                domain: MorphogenesisDomain::Natural,
                base_elevation_m: 1_050.0,
                relief_m: 2_600.0,
                uplift_m: 1_650.0,
                strata_amplitude_m: 180.0,
                rainfall_mass: 0.88,
                weathering: 0.52,
                warmth: 0.34,
                aridity: 0.18,
            },
            Self::VolcanicArchipelago => ProfileParameters {
                domain: MorphogenesisDomain::Natural,
                base_elevation_m: 80.0,
                relief_m: 2_050.0,
                uplift_m: 1_900.0,
                strata_amplitude_m: 95.0,
                rainfall_mass: 1.34,
                weathering: 0.68,
                warmth: 0.84,
                aridity: 0.08,
            },
            Self::AstralCrystalline => ProfileParameters {
                domain: MorphogenesisDomain::Astral,
                base_elevation_m: 720.0,
                relief_m: 3_100.0,
                uplift_m: 2_350.0,
                strata_amplitude_m: 360.0,
                rainfall_mass: 1.02,
                weathering: 0.62,
                warmth: 0.74,
                aridity: 0.16,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SharedVertexFields {
    pub elevation_m: [f32; MACRO_VERTEX_CELLS],
    pub uplift: [f32; MACRO_VERTEX_CELLS],
    pub strata_phase: [f32; MACRO_VERTEX_CELLS],
}

impl SharedVertexFields {
    fn zeroed() -> Self {
        Self {
            elevation_m: [0.0; MACRO_VERTEX_CELLS],
            uplift: [0.0; MACRO_VERTEX_CELLS],
            strata_phase: [0.0; MACRO_VERTEX_CELLS],
        }
    }

    pub fn index(x: usize, z: usize) -> usize {
        assert!(x < MACRO_VERTEX_SIDE && z < MACRO_VERTEX_SIDE);
        z * MACRO_VERTEX_SIDE + x
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualMacroFields {
    pub elevation_m: [f32; MACRO_TILE_CELLS],
    pub uplift: [f32; MACRO_TILE_CELLS],
    pub strata_phase: [f32; MACRO_TILE_CELLS],
    pub slope_grade: [f32; MACRO_TILE_CELLS],
    pub downhill_drop_m: [f32; MACRO_TILE_CELLS],
    pub local_flow_accumulation: [f32; MACRO_TILE_CELLS],
    pub routed_surface_water: [f32; MACRO_TILE_CELLS],
    pub soil_depth_m: [f32; MACRO_TILE_CELLS],
    pub moisture: [f32; MACRO_TILE_CELLS],
    pub vegetation_potential: [f32; MACRO_TILE_CELLS],
    pub flow_dx: [i8; MACRO_TILE_CELLS],
    pub flow_dz: [i8; MACRO_TILE_CELLS],
    pub species_guild: [SpeciesGuild; MACRO_TILE_CELLS],
}

impl VisualMacroFields {
    fn zeroed() -> Self {
        Self {
            elevation_m: [0.0; MACRO_TILE_CELLS],
            uplift: [0.0; MACRO_TILE_CELLS],
            strata_phase: [0.0; MACRO_TILE_CELLS],
            slope_grade: [0.0; MACRO_TILE_CELLS],
            downhill_drop_m: [0.0; MACRO_TILE_CELLS],
            local_flow_accumulation: [0.0; MACRO_TILE_CELLS],
            routed_surface_water: [0.0; MACRO_TILE_CELLS],
            soil_depth_m: [0.0; MACRO_TILE_CELLS],
            moisture: [0.0; MACRO_TILE_CELLS],
            vegetation_potential: [0.0; MACRO_TILE_CELLS],
            flow_dx: [0; MACRO_TILE_CELLS],
            flow_dz: [0; MACRO_TILE_CELLS],
            species_guild: [SpeciesGuild::Bare; MACRO_TILE_CELLS],
        }
    }

    pub fn index(x: usize, z: usize) -> usize {
        assert!(x < MACRO_TILE_SIDE && z < MACRO_TILE_SIDE);
        z * MACRO_TILE_SIDE + x
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanningMacroFields {
    /// Dimensionless local preference, not a solved global path.
    pub route_suitability: [f32; MACRO_TILE_CELLS],
    /// Dimensionless local preference, not permission to place gameplay objects.
    pub settlement_suitability: [f32; MACRO_TILE_CELLS],
}

impl PlanningMacroFields {
    fn zeroed() -> Self {
        Self {
            route_suitability: [0.0; MACRO_TILE_CELLS],
            settlement_suitability: [0.0; MACRO_TILE_CELLS],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EdgeFlux {
    /// Increasing index is west-to-east.
    pub north: [f32; MACRO_TILE_SIDE],
    /// Increasing index is north-to-south.
    pub east: [f32; MACRO_TILE_SIDE],
    /// Increasing index is west-to-east.
    pub south: [f32; MACRO_TILE_SIDE],
    /// Increasing index is north-to-south.
    pub west: [f32; MACRO_TILE_SIDE],
    /// NW, NE, SE, SW.
    pub corners: [f32; 4],
}

impl EdgeFlux {
    fn zeroed() -> Self {
        Self {
            north: [0.0; MACRO_TILE_SIDE],
            east: [0.0; MACRO_TILE_SIDE],
            south: [0.0; MACRO_TILE_SIDE],
            west: [0.0; MACRO_TILE_SIDE],
            corners: [0.0; 4],
        }
    }

    fn total_f64(&self) -> f64 {
        self.north
            .iter()
            .chain(self.east.iter())
            .chain(self.south.iter())
            .chain(self.west.iter())
            .chain(self.corners.iter())
            .map(|&value| value as f64)
            .sum()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryFluxContract {
    pub incoming: EdgeFlux,
    pub outgoing: EdgeFlux,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HydrologyReport {
    pub initial_water_mass: f64,
    pub boundary_inflow_mass: f64,
    pub retained_water_mass: f64,
    pub boundary_outflow_mass: f64,
    pub post_route_core_mass: f64,
    pub max_local_accumulation: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationReport {
    pub work_units: usize,
    pub work_unit_cap: usize,
    pub output_bytes: usize,
    pub output_byte_cap: usize,
    pub scratch_bytes: usize,
    pub scratch_byte_cap: usize,
    pub causal_stages_completed: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContinuumTile {
    pub grammar_version: u32,
    pub seed: u64,
    pub coord: MacroTileCoord,
    pub profile: MorphogenesisProfile,
    pub vertices: SharedVertexFields,
    pub visual: VisualMacroFields,
    pub planning: PlanningMacroFields,
    pub boundary_flux: BoundaryFluxContract,
    pub hydrology: HydrologyReport,
    pub generation: GenerationReport,
}

impl ContinuumTile {
    pub const fn authority_reason(&self) -> &'static str {
        DESCRIPTIVE_ONLY_REASON
    }

    pub const fn accounted_output_bytes() -> usize {
        OUTPUT_ACCOUNTED_BYTES
    }

    pub fn fingerprint(&self) -> u64 {
        let mut hash = mix64(self.seed ^ self.profile as u64);
        hash = fingerprint_word(hash, self.grammar_version as u64);
        hash = fingerprint_word(hash, self.coord.x as u64);
        hash = fingerprint_word(hash, self.coord.z as u64);

        for field in [
            &self.vertices.elevation_m[..],
            &self.vertices.uplift[..],
            &self.vertices.strata_phase[..],
            &self.visual.elevation_m[..],
            &self.visual.uplift[..],
            &self.visual.strata_phase[..],
            &self.visual.slope_grade[..],
            &self.visual.downhill_drop_m[..],
            &self.visual.local_flow_accumulation[..],
            &self.visual.routed_surface_water[..],
            &self.visual.soil_depth_m[..],
            &self.visual.moisture[..],
            &self.visual.vegetation_potential[..],
            &self.planning.route_suitability[..],
            &self.planning.settlement_suitability[..],
            &self.boundary_flux.incoming.north[..],
            &self.boundary_flux.incoming.east[..],
            &self.boundary_flux.incoming.south[..],
            &self.boundary_flux.incoming.west[..],
            &self.boundary_flux.incoming.corners[..],
            &self.boundary_flux.outgoing.north[..],
            &self.boundary_flux.outgoing.east[..],
            &self.boundary_flux.outgoing.south[..],
            &self.boundary_flux.outgoing.west[..],
            &self.boundary_flux.outgoing.corners[..],
        ] {
            for &value in field {
                hash = fingerprint_word(hash, value.to_bits() as u64);
            }
        }
        for &value in &self.visual.flow_dx {
            hash = fingerprint_word(hash, value as u8 as u64);
        }
        for &value in &self.visual.flow_dz {
            hash = fingerprint_word(hash, value as u8 as u64);
        }
        for &value in &self.visual.species_guild {
            hash = fingerprint_word(hash, value as u64);
        }
        for value in [
            self.hydrology.initial_water_mass,
            self.hydrology.boundary_inflow_mass,
            self.hydrology.retained_water_mass,
            self.hydrology.boundary_outflow_mass,
            self.hydrology.post_route_core_mass,
        ] {
            hash = fingerprint_word(hash, value.to_bits());
        }
        hash = fingerprint_word(hash, self.hydrology.max_local_accumulation.to_bits() as u64);
        hash
    }

    pub fn all_scalars_are_finite(&self) -> bool {
        [
            &self.vertices.elevation_m[..],
            &self.vertices.uplift[..],
            &self.vertices.strata_phase[..],
            &self.visual.elevation_m[..],
            &self.visual.uplift[..],
            &self.visual.strata_phase[..],
            &self.visual.slope_grade[..],
            &self.visual.downhill_drop_m[..],
            &self.visual.local_flow_accumulation[..],
            &self.visual.routed_surface_water[..],
            &self.visual.soil_depth_m[..],
            &self.visual.moisture[..],
            &self.visual.vegetation_potential[..],
            &self.planning.route_suitability[..],
            &self.planning.settlement_suitability[..],
            &self.boundary_flux.incoming.north[..],
            &self.boundary_flux.incoming.east[..],
            &self.boundary_flux.incoming.south[..],
            &self.boundary_flux.incoming.west[..],
            &self.boundary_flux.incoming.corners[..],
            &self.boundary_flux.outgoing.north[..],
            &self.boundary_flux.outgoing.east[..],
            &self.boundary_flux.outgoing.south[..],
            &self.boundary_flux.outgoing.west[..],
            &self.boundary_flux.outgoing.corners[..],
        ]
        .into_iter()
        .flatten()
        .all(|value| value.is_finite())
            && self.hydrology.initial_water_mass.is_finite()
            && self.hydrology.boundary_inflow_mass.is_finite()
            && self.hydrology.retained_water_mass.is_finite()
            && self.hydrology.boundary_outflow_mass.is_finite()
            && self.hydrology.post_route_core_mass.is_finite()
            && self.hydrology.max_local_accumulation.is_finite()
    }
}

struct Scratch {
    elevation_m: [f32; WORK_CELLS],
    uplift: [f32; WORK_CELLS],
    rainfall_mass: [f32; WORK_CELLS],
    flow_dx: [i8; WORK_CELLS],
    flow_dz: [i8; WORK_CELLS],
    downhill_drop_m: [f32; WORK_CELLS],
}

impl Scratch {
    fn zeroed() -> Self {
        Self {
            elevation_m: [0.0; WORK_CELLS],
            uplift: [0.0; WORK_CELLS],
            rainfall_mass: [0.0; WORK_CELLS],
            flow_dx: [0; WORK_CELLS],
            flow_dz: [0; WORK_CELLS],
            downhill_drop_m: [0.0; WORK_CELLS],
        }
    }
}

pub const OUTPUT_ACCOUNTED_BYTES: usize = size_of::<ContinuumTile>();
pub const SCRATCH_ACCOUNTED_BYTES: usize = size_of::<Scratch>();

const _: () = assert!(OUTPUT_ACCOUNTED_BYTES <= MAX_OUTPUT_BYTES);
const _: () = assert!(SCRATCH_ACCOUNTED_BYTES <= MAX_SCRATCH_BYTES);
const _: () = assert!(MACRO_HALO >= FLOW_SOURCE_RADIUS + FLOW_MAX_STEPS + 1);
const _: () = assert!(MACRO_TILE_SIDE % MID_LOD_STRIDE == 0);
const _: () = assert!(MACRO_TILE_SIDE % FAR_LOD_STRIDE == 0);

#[derive(Default)]
struct WorkCounter {
    used: usize,
}

impl WorkCounter {
    fn spend(&mut self, units: usize) {
        self.used = self
            .used
            .checked_add(units)
            .expect("fixed morphogenesis work counter overflowed");
        assert!(
            self.used <= MAX_GENERATION_WORK_UNITS,
            "fixed morphogenesis work budget exceeded"
        );
    }
}

/// Stateless generator. Its size is zero bytes and it owns no cache.
#[derive(Clone, Copy, Debug, Default)]
pub struct ContinuumGenerator;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationError {
    UnsupportedGrammarVersion { requested: u32, supported: u32 },
}

impl ContinuumGenerator {
    pub const fn state_bytes(&self) -> usize {
        0
    }

    pub fn generate_versioned(
        &self,
        seed: u64,
        coord: MacroTileCoord,
        profile: MorphogenesisProfile,
        grammar_version: u32,
    ) -> Result<ContinuumTile, GenerationError> {
        if grammar_version != MORPHOGENESIS_GRAMMAR_VERSION {
            return Err(GenerationError::UnsupportedGrammarVersion {
                requested: grammar_version,
                supported: MORPHOGENESIS_GRAMMAR_VERSION,
            });
        }
        Ok(self.generate(seed, coord, profile))
    }

    pub fn generate(
        &self,
        seed: u64,
        coord: MacroTileCoord,
        profile: MorphogenesisProfile,
    ) -> ContinuumTile {
        let params = profile.parameters();
        let mut work = WorkCounter::default();
        let mut scratch = Scratch::zeroed();

        // Stage 1: geology, strata, and uplift over a fixed translation-invariant halo.
        for wz in 0..WORK_SIDE {
            for wx in 0..WORK_SIDE {
                let index = work_index(wx, wz);
                let gx = global_cell_coord(coord.x, wx as i32 - MACRO_HALO as i32);
                let gz = global_cell_coord(coord.z, wz as i32 - MACRO_HALO as i32);
                let sample = sample_geology(seed, gx, gz, params);
                scratch.elevation_m[index] = sample.elevation_m;
                scratch.uplift[index] = sample.uplift;
            }
        }
        work.spend(WORK_CELLS);

        // Stage 2a: bounded rainfall forcing. It is a visual grammar input, not SI runoff.
        for wz in 0..WORK_SIDE {
            for wx in 0..WORK_SIDE {
                let index = work_index(wx, wz);
                let gx = global_cell_coord(coord.x, wx as i32 - MACRO_HALO as i32);
                let gz = global_cell_coord(coord.z, wz as i32 - MACRO_HALO as i32);
                scratch.rainfall_mass[index] =
                    sample_rainfall(seed, gx, gz, scratch.uplift[index], params);
            }
        }
        work.spend(WORK_CELLS);

        // Stage 2b: deterministic D8 single-flow receiver. Ties follow D8 order.
        for wz in 0..WORK_SIDE {
            for wx in 0..WORK_SIDE {
                let index = work_index(wx, wz);
                let current = scratch.elevation_m[index];
                let mut best_grade = 0.0_f32;
                let mut best_drop = 0.0_f32;
                let mut best = (0_i8, 0_i8);
                for &(dx, dz) in &D8_OFFSETS {
                    let nx = wx as isize + dx as isize;
                    let nz = wz as isize + dz as isize;
                    if nx < 0 || nz < 0 || nx >= WORK_SIDE as isize || nz >= WORK_SIDE as isize {
                        continue;
                    }
                    let neighbor = scratch.elevation_m[work_index(nx as usize, nz as usize)];
                    let drop_m = current - neighbor;
                    if drop_m <= 0.0 {
                        continue;
                    }
                    let distance_m = if dx != 0 && dz != 0 {
                        (MACRO_CELL_SIZE_M * std::f64::consts::SQRT_2) as f32
                    } else {
                        MACRO_CELL_SIZE_M as f32
                    };
                    let grade = drop_m / distance_m;
                    if grade > best_grade {
                        best_grade = grade;
                        best_drop = drop_m;
                        best = (dx, dz);
                    }
                }
                scratch.flow_dx[index] = best.0;
                scratch.flow_dz[index] = best.1;
                scratch.downhill_drop_m[index] = best_drop;
            }
        }
        work.spend(WORK_CELLS * D8_OFFSETS.len());

        let mut vertices = SharedVertexFields::zeroed();
        for vz in 0..MACRO_VERTEX_SIDE {
            for vx in 0..MACRO_VERTEX_SIDE {
                let index = SharedVertexFields::index(vx, vz);
                let gx = global_cell_coord(coord.x, vx as i32);
                let gz = global_cell_coord(coord.z, vz as i32);
                let sample = sample_geology(seed, gx, gz, params);
                vertices.elevation_m[index] = sample.elevation_m;
                vertices.uplift[index] = sample.uplift;
                vertices.strata_phase[index] = sample.strata_phase;
            }
        }
        work.spend(MACRO_VERTEX_CELLS);

        let mut visual = VisualMacroFields::zeroed();
        for z in 0..MACRO_TILE_SIDE {
            for x in 0..MACRO_TILE_SIDE {
                let output = VisualMacroFields::index(x, z);
                let source = work_index(x + MACRO_HALO, z + MACRO_HALO);
                let vertex = SharedVertexFields::index(x, z);
                visual.elevation_m[output] = vertices.elevation_m[vertex];
                visual.uplift[output] = vertices.uplift[vertex];
                visual.strata_phase[output] = vertices.strata_phase[vertex];
                visual.flow_dx[output] = scratch.flow_dx[source];
                visual.flow_dz[output] = scratch.flow_dz[source];
                visual.downhill_drop_m[output] = scratch.downhill_drop_m[source];
                visual.slope_grade[output] =
                    if scratch.flow_dx[source] != 0 && scratch.flow_dz[source] != 0 {
                        scratch.downhill_drop_m[source]
                            / (MACRO_CELL_SIZE_M * std::f64::consts::SQRT_2) as f32
                    } else {
                        scratch.downhill_drop_m[source] / MACRO_CELL_SIZE_M as f32
                    };
            }
        }

        // Stage 2c: translation-invariant, bounded local contributing mass.
        let mut observed_max_accumulation = 0.0_f32;
        for target_z in 0..MACRO_TILE_SIDE {
            for target_x in 0..MACRO_TILE_SIDE {
                let target_work = work_index(target_x + MACRO_HALO, target_z + MACRO_HALO);
                let mut accumulation = 0.0_f32;
                for source_z in 0..FLOW_SOURCE_DIAMETER {
                    for source_x in 0..FLOW_SOURCE_DIAMETER {
                        let offset_x = source_x as isize - FLOW_SOURCE_RADIUS as isize;
                        let offset_z = source_z as isize - FLOW_SOURCE_RADIUS as isize;
                        let sx = (target_x + MACRO_HALO) as isize + offset_x;
                        let sz = (target_z + MACRO_HALO) as isize + offset_z;
                        let source_work = work_index(sx as usize, sz as usize);
                        let mut cursor = source_work;
                        for _ in 0..=FLOW_MAX_STEPS {
                            work.spend(1);
                            if cursor == target_work {
                                accumulation += scratch.rainfall_mass[source_work];
                                break;
                            }
                            let dx = scratch.flow_dx[cursor];
                            let dz = scratch.flow_dz[cursor];
                            if dx == 0 && dz == 0 {
                                break;
                            }
                            let cx = cursor % WORK_SIDE;
                            let cz = cursor / WORK_SIDE;
                            let nx = cx as isize + dx as isize;
                            let nz = cz as isize + dz as isize;
                            debug_assert!(nx >= 0 && nz >= 0);
                            debug_assert!(nx < WORK_SIDE as isize && nz < WORK_SIDE as isize);
                            cursor = work_index(nx as usize, nz as usize);
                        }
                    }
                }
                debug_assert!(accumulation <= MAX_LOCAL_FLOW_ACCUMULATION + f32::EPSILON * 64.0);
                let output = VisualMacroFields::index(target_x, target_z);
                visual.local_flow_accumulation[output] = accumulation;
                observed_max_accumulation = observed_max_accumulation.max(accumulation);
            }
        }

        // Stage 2d: one-step mass routing and explicit cross-tile flux ports.
        let mut incoming = EdgeFlux::zeroed();
        let mut outgoing = EdgeFlux::zeroed();
        let mut initial_water_mass = 0.0_f64;
        let mut retained_water_mass = 0.0_f64;
        let mut boundary_outflow_mass = 0.0_f64;

        for z in 0..MACRO_TILE_SIDE {
            for x in 0..MACRO_TILE_SIDE {
                let source = work_index(x + MACRO_HALO, z + MACRO_HALO);
                let mass = scratch.rainfall_mass[source];
                initial_water_mass += mass as f64;
                let dx = scratch.flow_dx[source] as isize;
                let dz = scratch.flow_dz[source] as isize;
                let destination_x = x as isize + dx;
                let destination_z = z as isize + dz;
                if inside_core(destination_x, destination_z) {
                    let destination =
                        VisualMacroFields::index(destination_x as usize, destination_z as usize);
                    visual.routed_surface_water[destination] += mass;
                    retained_water_mass += mass as f64;
                } else {
                    add_outgoing_flux(&mut outgoing, destination_x, destination_z, mass);
                    boundary_outflow_mass += mass as f64;
                }
            }
        }
        work.spend(MACRO_TILE_CELLS);

        accumulate_incoming_ring(&scratch, &mut incoming, &mut visual, &mut work);

        let boundary_inflow_mass = incoming.total_f64();
        let post_route_core_mass = retained_water_mass + boundary_inflow_mass;
        let conservation_error = (initial_water_mass + boundary_inflow_mass
            - post_route_core_mass
            - boundary_outflow_mass)
            .abs();
        debug_assert!(conservation_error <= 1.0e-9);
        debug_assert!((outgoing.total_f64() - boundary_outflow_mass).abs() < 1.0e-3);

        // Stage 3: descriptive soil and moisture grammar.
        for z in 0..MACRO_TILE_SIDE {
            for x in 0..MACRO_TILE_SIDE {
                let output = VisualMacroFields::index(x, z);
                let source = work_index(x + MACRO_HALO, z + MACRO_HALO);
                let slope = visual.slope_grade[output] as f64;
                let wetness =
                    normalized_log_accumulation(visual.local_flow_accumulation[output] as f64);
                let gx = global_cell_coord(coord.x, x as i32);
                let gz = global_cell_coord(coord.z, z as i32);
                let regolith_noise = value_noise(seed, gx, gz, 37, 0x534f_494c);
                let slope_retention = (1.0 - slope * 4.0).clamp(0.0, 1.0);
                let soil_depth_m =
                    4.0 * params.weathering * slope_retention * (0.55 + 0.45 * regolith_noise);
                let rainfall =
                    scratch.rainfall_mass[source] as f64 / MAX_RAINFALL_MASS_PER_CELL as f64;
                let moisture = (rainfall * 0.46 + wetness * 0.44 + (soil_depth_m / 4.0) * 0.18
                    - slope.min(1.0) * 0.38
                    - params.aridity * 0.30)
                    .clamp(0.0, 1.0);
                visual.soil_depth_m[output] = soil_depth_m as f32;
                visual.moisture[output] = moisture as f32;
            }
        }
        work.spend(MACRO_TILE_CELLS);

        // Stage 4: MOD17-inspired limiting scalars, not an NPP calculation.
        for index in 0..MACRO_TILE_CELLS {
            let relief_fraction = ((visual.elevation_m[index] as f64 - params.base_elevation_m)
                / params.relief_m.max(1.0))
            .max(0.0);
            let temperature_scalar = (params.warmth - relief_fraction * 0.38).clamp(0.0, 1.0);
            let water_scalar = smooth_unit(visual.moisture[index] as f64);
            let soil_scalar = (visual.soil_depth_m[index] as f64 / 2.0).clamp(0.0, 1.0);
            let dry_air_scalar =
                (1.0 - params.aridity * (1.0 - visual.moisture[index] as f64)).clamp(0.0, 1.0);
            let slope_scalar = (1.0 - visual.slope_grade[index] as f64 * 3.5).clamp(0.0, 1.0);
            let potential =
                (temperature_scalar * water_scalar * soil_scalar * dry_air_scalar * slope_scalar)
                    .clamp(0.0, 1.0);
            visual.vegetation_potential[index] = potential as f32;
            let wetness = normalized_log_accumulation(visual.local_flow_accumulation[index] as f64);
            visual.species_guild[index] = classify_guild(
                potential,
                visual.moisture[index] as f64,
                wetness,
                temperature_scalar,
                params.aridity,
                params.domain,
            );
        }
        work.spend(MACRO_TILE_CELLS);

        // Stage 5: local route and settlement preference fields.
        let mut planning = PlanningMacroFields::zeroed();
        for index in 0..MACRO_TILE_CELLS {
            let slope = visual.slope_grade[index] as f64;
            let moisture = visual.moisture[index] as f64;
            let vegetation = visual.vegetation_potential[index] as f64;
            let soil = (visual.soil_depth_m[index] as f64 / 4.0).clamp(0.0, 1.0);
            let water_access =
                normalized_log_accumulation(visual.local_flow_accumulation[index] as f64);
            let flood_penalty = ((moisture - 0.72) / 0.28).clamp(0.0, 1.0) * water_access;
            let gentle = (1.0 - slope * 5.0).clamp(0.0, 1.0);
            planning.route_suitability[index] = (gentle
                * (1.0 - flood_penalty * 0.72)
                * (1.0 - vegetation * 0.22)
                * (0.78 + soil * 0.22))
                .clamp(0.0, 1.0) as f32;

            let moisture_comfort = (1.0 - (moisture - 0.55).abs() / 0.55).clamp(0.0, 1.0);
            let uplift_hazard = (visual.uplift[index] as f64 * 0.35).clamp(0.0, 0.35);
            planning.settlement_suitability[index] = (gentle
                * moisture_comfort
                * (0.45 + soil * 0.55)
                * (0.72 + water_access * 0.28)
                * (1.0 - flood_penalty)
                * (1.0 - uplift_hazard))
                .clamp(0.0, 1.0) as f32;
        }
        work.spend(MACRO_TILE_CELLS);

        debug_assert!(work.used <= MAX_GENERATION_WORK_UNITS);

        ContinuumTile {
            grammar_version: MORPHOGENESIS_GRAMMAR_VERSION,
            seed,
            coord,
            profile,
            vertices,
            visual,
            planning,
            boundary_flux: BoundaryFluxContract { incoming, outgoing },
            hydrology: HydrologyReport {
                initial_water_mass,
                boundary_inflow_mass,
                retained_water_mass,
                boundary_outflow_mass,
                post_route_core_mass,
                max_local_accumulation: observed_max_accumulation,
            },
            generation: GenerationReport {
                work_units: work.used,
                work_unit_cap: MAX_GENERATION_WORK_UNITS,
                output_bytes: OUTPUT_ACCOUNTED_BYTES,
                output_byte_cap: MAX_OUTPUT_BYTES,
                scratch_bytes: SCRATCH_ACCOUNTED_BYTES,
                scratch_byte_cap: MAX_SCRATCH_BYTES,
                causal_stages_completed: 5,
            },
        }
    }
}

const D8_OFFSETS: [(i8, i8); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

#[derive(Clone, Copy)]
struct GeologySample {
    elevation_m: f32,
    uplift: f32,
    strata_phase: f32,
}

fn sample_geology(seed: u64, gx: i128, gz: i128, params: ProfileParameters) -> GeologySample {
    let continental = value_noise(seed, gx, gz, 512, 0x434f_4e54);
    let regional = value_noise(seed, gx, gz, 128, 0x5245_474e);
    let local = value_noise(seed, gx, gz, 31, 0x4c4f_434c);
    let ridge_signal = value_noise(seed, gx, gz, 73, 0x5550_4c46);
    let ridge = 1.0 - (ridge_signal * 2.0 - 1.0).abs();
    let uplift = smooth_unit(((ridge - 0.42) / 0.58).clamp(0.0, 1.0));

    let phase_numerator = (gx * 3 + gz * 5).rem_euclid(257) as f64;
    let strata_noise = value_noise(seed, gx, gz, 47, 0x5354_5241);
    let strata_phase = ((phase_numerator / 257.0) + strata_noise * 0.35).fract();
    let strata_step = ((strata_phase * 7.0).floor() / 6.0 - 0.5).clamp(-0.5, 0.5);

    let relief = (continental - 0.5) * 0.58 + (regional - 0.5) * 0.32 + (local - 0.5) * 0.10;
    let elevation_m = params.base_elevation_m
        + relief * params.relief_m
        + uplift * params.uplift_m
        + strata_step * params.strata_amplitude_m;

    GeologySample {
        elevation_m: elevation_m as f32,
        uplift: uplift as f32,
        strata_phase: strata_phase as f32,
    }
}

fn sample_rainfall(seed: u64, gx: i128, gz: i128, uplift: f32, params: ProfileParameters) -> f32 {
    let weather = value_noise(seed, gx, gz, 97, 0x5241_494e);
    let local = value_noise(seed, gx, gz, 29, 0x434c_4f55);
    let orographic_hint = 0.82 + uplift as f64 * 0.25;
    (params.rainfall_mass * (0.58 + weather * 0.30 + local * 0.12) * orographic_hint)
        .clamp(0.0, MAX_RAINFALL_MASS_PER_CELL as f64) as f32
}

fn classify_guild(
    potential: f64,
    moisture: f64,
    wetness: f64,
    temperature: f64,
    aridity: f64,
    domain: MorphogenesisDomain,
) -> SpeciesGuild {
    if potential < 0.08 {
        SpeciesGuild::Bare
    } else if domain == MorphogenesisDomain::Astral {
        if moisture > 0.48 || wetness > 0.30 {
            SpeciesGuild::LuminousGrove
        } else {
            SpeciesGuild::CrystalPioneer
        }
    } else if moisture > 0.72 && wetness > 0.26 {
        SpeciesGuild::Riparian
    } else if temperature < 0.24 {
        SpeciesGuild::Alpine
    } else if aridity > 0.58 && moisture < 0.42 {
        SpeciesGuild::XericScrub
    } else if potential > 0.68 && moisture > 0.48 {
        SpeciesGuild::ClosedCanopy
    } else if potential > 0.34 {
        SpeciesGuild::Shrubland
    } else {
        SpeciesGuild::PioneerGrass
    }
}

fn accumulate_incoming_ring(
    scratch: &Scratch,
    incoming: &mut EdgeFlux,
    visual: &mut VisualMacroFields,
    work: &mut WorkCounter,
) {
    for along in 0..MACRO_TILE_SIDE {
        add_incoming_source(scratch, incoming, visual, -1, along as isize);
        add_incoming_source(
            scratch,
            incoming,
            visual,
            MACRO_TILE_SIDE as isize,
            along as isize,
        );
        add_incoming_source(scratch, incoming, visual, along as isize, -1);
        add_incoming_source(
            scratch,
            incoming,
            visual,
            along as isize,
            MACRO_TILE_SIDE as isize,
        );
    }
    for &(x, z) in &[
        (-1, -1),
        (MACRO_TILE_SIDE as isize, -1),
        (MACRO_TILE_SIDE as isize, MACRO_TILE_SIDE as isize),
        (-1, MACRO_TILE_SIDE as isize),
    ] {
        add_incoming_source(scratch, incoming, visual, x, z);
    }
    work.spend(MACRO_TILE_SIDE * 4 + 4);
}

fn add_incoming_source(
    scratch: &Scratch,
    incoming: &mut EdgeFlux,
    visual: &mut VisualMacroFields,
    x: isize,
    z: isize,
) {
    let wx = (x + MACRO_HALO as isize) as usize;
    let wz = (z + MACRO_HALO as isize) as usize;
    let source = work_index(wx, wz);
    let destination_x = x + scratch.flow_dx[source] as isize;
    let destination_z = z + scratch.flow_dz[source] as isize;
    if !inside_core(destination_x, destination_z) {
        return;
    }
    let mass = scratch.rainfall_mass[source];
    let destination = VisualMacroFields::index(destination_x as usize, destination_z as usize);
    visual.routed_surface_water[destination] += mass;
    if x < 0 && z < 0 {
        incoming.corners[0] += mass;
    } else if x >= MACRO_TILE_SIDE as isize && z < 0 {
        incoming.corners[1] += mass;
    } else if x >= MACRO_TILE_SIDE as isize && z >= MACRO_TILE_SIDE as isize {
        incoming.corners[2] += mass;
    } else if x < 0 && z >= MACRO_TILE_SIDE as isize {
        incoming.corners[3] += mass;
    } else if z < 0 {
        incoming.north[destination_x as usize] += mass;
    } else if x >= MACRO_TILE_SIDE as isize {
        incoming.east[destination_z as usize] += mass;
    } else if z >= MACRO_TILE_SIDE as isize {
        incoming.south[destination_x as usize] += mass;
    } else if x < 0 {
        incoming.west[destination_z as usize] += mass;
    }
}

fn add_outgoing_flux(
    outgoing: &mut EdgeFlux,
    destination_x: isize,
    destination_z: isize,
    mass: f32,
) {
    if destination_x < 0 && destination_z < 0 {
        outgoing.corners[0] += mass;
    } else if destination_x >= MACRO_TILE_SIDE as isize && destination_z < 0 {
        outgoing.corners[1] += mass;
    } else if destination_x >= MACRO_TILE_SIDE as isize && destination_z >= MACRO_TILE_SIDE as isize
    {
        outgoing.corners[2] += mass;
    } else if destination_x < 0 && destination_z >= MACRO_TILE_SIDE as isize {
        outgoing.corners[3] += mass;
    } else if destination_z < 0 {
        outgoing.north[destination_x as usize] += mass;
    } else if destination_x >= MACRO_TILE_SIDE as isize {
        outgoing.east[destination_z as usize] += mass;
    } else if destination_z >= MACRO_TILE_SIDE as isize {
        outgoing.south[destination_x as usize] += mass;
    } else if destination_x < 0 {
        outgoing.west[destination_z as usize] += mass;
    }
}

fn inside_core(x: isize, z: isize) -> bool {
    x >= 0 && z >= 0 && x < MACRO_TILE_SIDE as isize && z < MACRO_TILE_SIDE as isize
}

fn global_cell_coord(tile: i64, local: i32) -> i128 {
    tile as i128 * MACRO_TILE_SIDE as i128 + local as i128
}

fn work_index(x: usize, z: usize) -> usize {
    debug_assert!(x < WORK_SIDE && z < WORK_SIDE);
    z * WORK_SIDE + x
}

fn normalized_log_accumulation(accumulation: f64) -> f64 {
    accumulation.max(0.0).ln_1p() / (MAX_LOCAL_FLOW_ACCUMULATION as f64).ln_1p()
}

fn value_noise(seed: u64, x: i128, z: i128, period: i128, channel: u64) -> f64 {
    debug_assert!(period > 0);
    let lattice_x = x.div_euclid(period);
    let lattice_z = z.div_euclid(period);
    let tx = x.rem_euclid(period) as f64 / period as f64;
    let tz = z.rem_euclid(period) as f64 / period as f64;
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sz = tz * tz * (3.0 - 2.0 * tz);
    let n00 = hash_unit(seed, lattice_x, lattice_z, channel);
    let n10 = hash_unit(seed, lattice_x + 1, lattice_z, channel);
    let n01 = hash_unit(seed, lattice_x, lattice_z + 1, channel);
    let n11 = hash_unit(seed, lattice_x + 1, lattice_z + 1, channel);
    lerp(lerp(n00, n10, sx), lerp(n01, n11, sx), sz)
}

fn hash_unit(seed: u64, x: i128, z: i128, channel: u64) -> f64 {
    let ux = x as u128;
    let uz = z as u128;
    let mut hash = mix64(seed ^ channel.rotate_left(17));
    hash = fingerprint_word(hash, ux as u64);
    hash = fingerprint_word(hash, (ux >> 64) as u64);
    hash = fingerprint_word(hash, uz as u64);
    hash = fingerprint_word(hash, (uz >> 64) as u64);
    let mantissa24 = (hash >> 40) as u32;
    mantissa24 as f64 / 16_777_215.0
}

fn fingerprint_word(hash: u64, word: u64) -> u64 {
    mix64(hash ^ word.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn smooth_unit(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
