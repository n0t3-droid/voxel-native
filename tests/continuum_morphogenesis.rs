#[path = "../src/continuum_morphogenesis.rs"]
mod continuum_morphogenesis;

use std::collections::BTreeSet;
use std::mem::{needs_drop, size_of};
use std::time::{Duration, Instant};

use continuum_morphogenesis::{
    ContinuumGenerator, ContinuumTile, EdgeFlux, GenerationError, MacroTileCoord,
    MorphogenesisDomain, MorphogenesisProfile, SharedVertexFields, SpeciesGuild,
    DESCRIPTIVE_ONLY_REASON, FAR_LOD_STRIDE, LOD_FEED_CONTRACT, MACRO_TILE_CELLS, MACRO_TILE_SIDE,
    MACRO_TILE_SPAN_M, MACRO_VERTEX_SIDE, MAX_GENERATION_WORK_UNITS, MAX_LOCAL_FLOW_ACCUMULATION,
    MAX_OUTPUT_BYTES, MAX_SCRATCH_BYTES, MID_LOD_STRIDE, MORPHOGENESIS_GRAMMAR_VERSION,
    OUTPUT_ACCOUNTED_BYTES, SCRATCH_ACCOUNTED_BYTES,
};

const SEED: u64 = 0x8f62_a1c4_77d9_3b05;

fn generate(coord: MacroTileCoord, profile: MorphogenesisProfile) -> ContinuumTile {
    ContinuumGenerator.generate(SEED, coord, profile)
}

fn edge_total(edge: &EdgeFlux) -> f64 {
    edge.north
        .iter()
        .chain(edge.east.iter())
        .chain(edge.south.iter())
        .chain(edge.west.iter())
        .chain(edge.corners.iter())
        .map(|&mass| mass as f64)
        .sum()
}

fn mass(values: &[f32]) -> f64 {
    values.iter().map(|&value| value as f64).sum()
}

fn assert_unit_interval(values: &[f32]) {
    for &value in values {
        assert!(value.is_finite());
        assert!((0.0..=1.0).contains(&value), "value {value} escaped [0, 1]");
    }
}

#[test]
fn deterministic_replay_is_independent_of_request_order() {
    let generator = ContinuumGenerator;
    let a_coord = MacroTileCoord::new(-17, 42);
    let b_coord = MacroTileCoord::new(91, -3);

    let a_first = generator.generate(SEED, a_coord, MorphogenesisProfile::TemperateBasins);
    let b_second = generator.generate(SEED, b_coord, MorphogenesisProfile::VolcanicArchipelago);
    let b_first = generator.generate(SEED, b_coord, MorphogenesisProfile::VolcanicArchipelago);
    let a_second = generator.generate(SEED, a_coord, MorphogenesisProfile::TemperateBasins);

    assert_eq!(a_first, a_second);
    assert_eq!(b_first, b_second);
    assert_eq!(a_first.fingerprint(), a_second.fingerprint());
    assert_eq!(b_first.fingerprint(), b_second.fingerprint());
}

#[test]
fn grammar_version_is_reproducible_and_unknown_versions_fail_closed() {
    let generator = ContinuumGenerator;
    let coord = MacroTileCoord::new(2, -5);
    let versioned = generator
        .generate_versioned(
            SEED,
            coord,
            MorphogenesisProfile::TemperateBasins,
            MORPHOGENESIS_GRAMMAR_VERSION,
        )
        .expect("current grammar version must be supported");
    let direct = generator.generate(SEED, coord, MorphogenesisProfile::TemperateBasins);
    assert_eq!(versioned, direct);
    assert_eq!(versioned.grammar_version, MORPHOGENESIS_GRAMMAR_VERSION);

    let unsupported = MORPHOGENESIS_GRAMMAR_VERSION.saturating_add(1);
    assert_eq!(
        generator.generate_versioned(
            SEED,
            coord,
            MorphogenesisProfile::TemperateBasins,
            unsupported,
        ),
        Err(GenerationError::UnsupportedGrammarVersion {
            requested: unsupported,
            supported: MORPHOGENESIS_GRAMMAR_VERSION,
        })
    );
}

#[test]
fn signed_extreme_coordinates_are_finite_and_do_not_panic() {
    let cases = [
        MacroTileCoord::new(i64::MIN, i64::MIN),
        MacroTileCoord::new(i64::MAX, i64::MAX),
        MacroTileCoord::new(i64::MIN, i64::MAX),
        MacroTileCoord::new(i64::MAX, i64::MIN),
        MacroTileCoord::new(-1, 0),
        MacroTileCoord::new(0, -1),
    ];

    for (index, coord) in cases.into_iter().enumerate() {
        let profile = match index % 5 {
            0 => MorphogenesisProfile::TemperateBasins,
            1 => MorphogenesisProfile::AridPlateaus,
            2 => MorphogenesisProfile::AlpineRifts,
            3 => MorphogenesisProfile::VolcanicArchipelago,
            _ => MorphogenesisProfile::AstralCrystalline,
        };
        let tile = generate(coord, profile);
        assert!(
            tile.all_scalars_are_finite(),
            "non-finite tile at {coord:?}"
        );
    }
}

#[test]
fn neighboring_tiles_share_exact_vertices_and_flux_ports() {
    let center_coord = MacroTileCoord::new(-8, 13);
    let center = generate(center_coord, MorphogenesisProfile::TemperateBasins);
    let east = generate(
        MacroTileCoord::new(center_coord.x + 1, center_coord.z),
        MorphogenesisProfile::TemperateBasins,
    );
    let south = generate(
        MacroTileCoord::new(center_coord.x, center_coord.z + 1),
        MorphogenesisProfile::TemperateBasins,
    );
    let southeast = generate(
        MacroTileCoord::new(center_coord.x + 1, center_coord.z + 1),
        MorphogenesisProfile::TemperateBasins,
    );
    let east_generated_first = generate(
        MacroTileCoord::new(center_coord.x + 1, center_coord.z),
        MorphogenesisProfile::TemperateBasins,
    );
    let center_generated_second = generate(center_coord, MorphogenesisProfile::TemperateBasins);
    assert_eq!(east, east_generated_first);
    assert_eq!(center, center_generated_second);

    for z in 0..MACRO_VERTEX_SIDE {
        let center_edge = SharedVertexFields::index(MACRO_TILE_SIDE, z);
        let east_edge = SharedVertexFields::index(0, z);
        assert_eq!(
            center.vertices.elevation_m[center_edge],
            east.vertices.elevation_m[east_edge]
        );
        assert_eq!(
            center.vertices.uplift[center_edge],
            east.vertices.uplift[east_edge]
        );
        assert_eq!(
            center.vertices.strata_phase[center_edge],
            east.vertices.strata_phase[east_edge]
        );
    }
    for x in 0..MACRO_VERTEX_SIDE {
        let center_edge = SharedVertexFields::index(x, MACRO_TILE_SIDE);
        let south_edge = SharedVertexFields::index(x, 0);
        assert_eq!(
            center.vertices.elevation_m[center_edge],
            south.vertices.elevation_m[south_edge]
        );
        assert_eq!(
            center.vertices.uplift[center_edge],
            south.vertices.uplift[south_edge]
        );
        assert_eq!(
            center.vertices.strata_phase[center_edge],
            south.vertices.strata_phase[south_edge]
        );
    }

    assert_eq!(
        center.boundary_flux.outgoing.east,
        east.boundary_flux.incoming.west
    );
    assert_eq!(
        center.boundary_flux.outgoing.south,
        south.boundary_flux.incoming.north
    );
    assert_eq!(
        center.boundary_flux.outgoing.corners[2],
        southeast.boundary_flux.incoming.corners[0]
    );
}

#[test]
fn two_by_two_region_cancels_internal_flux_and_conserves_mass() {
    let profile = MorphogenesisProfile::AstralCrystalline;
    let northwest = generate(MacroTileCoord::new(-1, -1), profile);
    let northeast = generate(MacroTileCoord::new(0, -1), profile);
    let southwest = generate(MacroTileCoord::new(-1, 0), profile);
    let southeast = generate(MacroTileCoord::new(0, 0), profile);

    assert_eq!(
        northwest.boundary_flux.outgoing.east,
        northeast.boundary_flux.incoming.west
    );
    assert_eq!(
        northeast.boundary_flux.outgoing.west,
        northwest.boundary_flux.incoming.east
    );
    assert_eq!(
        southwest.boundary_flux.outgoing.east,
        southeast.boundary_flux.incoming.west
    );
    assert_eq!(
        southeast.boundary_flux.outgoing.west,
        southwest.boundary_flux.incoming.east
    );
    assert_eq!(
        northwest.boundary_flux.outgoing.south,
        southwest.boundary_flux.incoming.north
    );
    assert_eq!(
        southwest.boundary_flux.outgoing.north,
        northwest.boundary_flux.incoming.south
    );
    assert_eq!(
        northeast.boundary_flux.outgoing.south,
        southeast.boundary_flux.incoming.north
    );
    assert_eq!(
        southeast.boundary_flux.outgoing.north,
        northeast.boundary_flux.incoming.south
    );
    assert_eq!(
        northwest.boundary_flux.outgoing.corners[2],
        southeast.boundary_flux.incoming.corners[0]
    );
    assert_eq!(
        southeast.boundary_flux.outgoing.corners[0],
        northwest.boundary_flux.incoming.corners[2]
    );
    assert_eq!(
        northeast.boundary_flux.outgoing.corners[3],
        southwest.boundary_flux.incoming.corners[1]
    );
    assert_eq!(
        southwest.boundary_flux.outgoing.corners[1],
        northeast.boundary_flux.incoming.corners[3]
    );

    let tiles = [&northwest, &northeast, &southwest, &southeast];
    let total_initial: f64 = tiles
        .iter()
        .map(|tile| tile.hydrology.initial_water_mass)
        .sum();
    let total_inflow: f64 = tiles
        .iter()
        .map(|tile| tile.hydrology.boundary_inflow_mass)
        .sum();
    let total_outflow: f64 = tiles
        .iter()
        .map(|tile| tile.hydrology.boundary_outflow_mass)
        .sum();
    let total_post_route: f64 = tiles
        .iter()
        .map(|tile| tile.hydrology.post_route_core_mass)
        .sum();

    let internal_outflow = mass(&northwest.boundary_flux.outgoing.east)
        + mass(&northeast.boundary_flux.outgoing.west)
        + mass(&southwest.boundary_flux.outgoing.east)
        + mass(&southeast.boundary_flux.outgoing.west)
        + mass(&northwest.boundary_flux.outgoing.south)
        + mass(&southwest.boundary_flux.outgoing.north)
        + mass(&northeast.boundary_flux.outgoing.south)
        + mass(&southeast.boundary_flux.outgoing.north)
        + northwest.boundary_flux.outgoing.corners[2] as f64
        + southeast.boundary_flux.outgoing.corners[0] as f64
        + northeast.boundary_flux.outgoing.corners[3] as f64
        + southwest.boundary_flux.outgoing.corners[1] as f64;
    let internal_inflow = mass(&northeast.boundary_flux.incoming.west)
        + mass(&northwest.boundary_flux.incoming.east)
        + mass(&southeast.boundary_flux.incoming.west)
        + mass(&southwest.boundary_flux.incoming.east)
        + mass(&southwest.boundary_flux.incoming.north)
        + mass(&northwest.boundary_flux.incoming.south)
        + mass(&southeast.boundary_flux.incoming.north)
        + mass(&northeast.boundary_flux.incoming.south)
        + southeast.boundary_flux.incoming.corners[0] as f64
        + northwest.boundary_flux.incoming.corners[2] as f64
        + southwest.boundary_flux.incoming.corners[1] as f64
        + northeast.boundary_flux.incoming.corners[3] as f64;
    assert_eq!(internal_outflow, internal_inflow);

    let external_inflow = total_inflow - internal_inflow;
    let external_outflow = total_outflow - internal_outflow;
    let regional_error =
        (total_initial + external_inflow - total_post_route - external_outflow).abs();
    assert!(
        regional_error <= 1.0e-8,
        "regional mass error {regional_error}"
    );
}

#[test]
fn bounded_water_routes_downhill_and_conserves_one_step_mass() {
    let tile = generate(
        MacroTileCoord::new(4, -9),
        MorphogenesisProfile::VolcanicArchipelago,
    );

    for index in 0..MACRO_TILE_CELLS {
        let dx = tile.visual.flow_dx[index];
        let dz = tile.visual.flow_dz[index];
        if dx != 0 || dz != 0 {
            assert!(tile.visual.downhill_drop_m[index] > 0.0);
            assert!(tile.visual.slope_grade[index] > 0.0);
        } else {
            assert_eq!(tile.visual.downhill_drop_m[index], 0.0);
        }
        assert!(
            tile.visual.local_flow_accumulation[index]
                <= MAX_LOCAL_FLOW_ACCUMULATION + f32::EPSILON * 64.0
        );
    }

    let report = tile.hydrology;
    let conservation_error = (report.initial_water_mass + report.boundary_inflow_mass
        - report.post_route_core_mass
        - report.boundary_outflow_mass)
        .abs();
    assert!(
        conservation_error <= 1.0e-9,
        "mass error {conservation_error}"
    );
    assert!(
        (edge_total(&tile.boundary_flux.outgoing) - report.boundary_outflow_mass).abs() <= 1.0e-3
    );
    assert!(
        (edge_total(&tile.boundary_flux.incoming) - report.boundary_inflow_mass).abs() <= 1.0e-3
    );
    let routed_sum: f64 = tile
        .visual
        .routed_surface_water
        .iter()
        .map(|&mass| mass as f64)
        .sum();
    assert!((routed_sum - report.post_route_core_mass).abs() <= 1.0e-3);
    assert!(report.max_local_accumulation <= MAX_LOCAL_FLOW_ACCUMULATION);
}

#[test]
fn all_public_scalar_fields_are_finite_and_normalized_fields_are_bounded() {
    for profile in [
        MorphogenesisProfile::TemperateBasins,
        MorphogenesisProfile::AridPlateaus,
        MorphogenesisProfile::AlpineRifts,
        MorphogenesisProfile::VolcanicArchipelago,
        MorphogenesisProfile::AstralCrystalline,
    ] {
        let tile = generate(MacroTileCoord::new(-203, 507), profile);
        assert!(tile.all_scalars_are_finite());
        assert_unit_interval(&tile.vertices.uplift);
        assert_unit_interval(&tile.vertices.strata_phase);
        assert_unit_interval(&tile.visual.uplift);
        assert_unit_interval(&tile.visual.strata_phase);
        assert_unit_interval(&tile.visual.moisture);
        assert_unit_interval(&tile.visual.vegetation_potential);
        assert_unit_interval(&tile.planning.route_suitability);
        assert_unit_interval(&tile.planning.settlement_suitability);
        assert!(tile.visual.soil_depth_m.iter().all(|&value| value >= 0.0));
        assert!(tile.visual.slope_grade.iter().all(|&value| value >= 0.0));
    }
}

#[test]
fn output_scratch_and_work_have_compile_time_caps() {
    let generator = ContinuumGenerator;
    assert_eq!(size_of::<ContinuumGenerator>(), 0);
    assert_eq!(generator.state_bytes(), 0);
    assert!(!needs_drop::<ContinuumGenerator>());
    assert!(!needs_drop::<ContinuumTile>());
    assert_eq!(size_of::<ContinuumTile>(), OUTPUT_ACCOUNTED_BYTES);
    assert_eq!(
        ContinuumTile::accounted_output_bytes(),
        OUTPUT_ACCOUNTED_BYTES
    );
    assert!(OUTPUT_ACCOUNTED_BYTES <= MAX_OUTPUT_BYTES);
    assert!(SCRATCH_ACCOUNTED_BYTES <= MAX_SCRATCH_BYTES);

    let tile = generator.generate(
        SEED,
        MacroTileCoord::new(0, 0),
        MorphogenesisProfile::TemperateBasins,
    );
    assert_eq!(tile.generation.output_bytes, OUTPUT_ACCOUNTED_BYTES);
    assert_eq!(tile.generation.scratch_bytes, SCRATCH_ACCOUNTED_BYTES);
    assert_eq!(tile.generation.work_unit_cap, MAX_GENERATION_WORK_UNITS);
    assert!(tile.generation.work_units <= MAX_GENERATION_WORK_UNITS);
    assert_eq!(tile.generation.causal_stages_completed, 5);
    assert_eq!(tile.authority_reason(), DESCRIPTIVE_ONLY_REASON);
    assert_eq!(MACRO_TILE_SIDE % MID_LOD_STRIDE, 0);
    assert_eq!(MACRO_TILE_SIDE % FAR_LOD_STRIDE, 0);
    assert!(LOD_FEED_CONTRACT.core_is_half_open);
    assert!(LOD_FEED_CONTRACT.edges_use_shared_vertices);
    assert!(LOD_FEED_CONTRACT.descriptive_only);
}

#[test]
fn profiles_produce_distinct_macro_grammars() {
    let coord = MacroTileCoord::new(73, -121);
    let tiles = [
        generate(coord, MorphogenesisProfile::TemperateBasins),
        generate(coord, MorphogenesisProfile::AridPlateaus),
        generate(coord, MorphogenesisProfile::AlpineRifts),
        generate(coord, MorphogenesisProfile::VolcanicArchipelago),
        generate(coord, MorphogenesisProfile::AstralCrystalline),
    ];
    let fingerprints: BTreeSet<_> = tiles.iter().map(ContinuumTile::fingerprint).collect();
    assert_eq!(fingerprints.len(), tiles.len());

    let mean_moisture: Vec<f64> = tiles
        .iter()
        .map(|tile| {
            tile.visual
                .moisture
                .iter()
                .map(|&value| value as f64)
                .sum::<f64>()
                / MACRO_TILE_CELLS as f64
        })
        .collect();
    let moisture_span = mean_moisture
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max)
        - mean_moisture.iter().copied().fold(f64::INFINITY, f64::min);
    assert!(
        moisture_span > 0.10,
        "profile moisture span was {moisture_span}"
    );

    let natural = &tiles[0];
    let astral = &tiles[4];
    assert_eq!(natural.profile.domain(), MorphogenesisDomain::Natural);
    assert_eq!(astral.profile.domain(), MorphogenesisDomain::Astral);
    assert!(astral.visual.species_guild.iter().any(|guild| matches!(
        guild,
        SpeciesGuild::CrystalPioneer | SpeciesGuild::LuminousGrove
    )));
    assert!(natural.visual.species_guild.iter().all(|guild| !matches!(
        guild,
        SpeciesGuild::CrystalPioneer | SpeciesGuild::LuminousGrove
    )));
}

#[test]
fn twenty_thousand_kilometre_route_does_not_grow_generator_state() {
    const SEGMENTS: i64 = 64;
    const TILE_STEP: i64 = 39;
    let generator = ContinuumGenerator;
    let route_distance_m = SEGMENTS as f64 * TILE_STEP as f64 * MACRO_TILE_SPAN_M;
    assert!(route_distance_m >= 20_000_000.0);

    let mut previous_fingerprint = None;
    let mut distinct_transitions = 0_usize;
    let mut peak_output_bytes = 0;
    let mut peak_scratch_bytes = 0;
    let mut peak_work_units = 0;
    for segment in 0..=SEGMENTS {
        let coord = MacroTileCoord::new(
            segment.saturating_mul(TILE_STEP),
            segment.saturating_mul(-17),
        );
        let profile = match segment % 5 {
            0 => MorphogenesisProfile::TemperateBasins,
            1 => MorphogenesisProfile::AridPlateaus,
            2 => MorphogenesisProfile::AlpineRifts,
            3 => MorphogenesisProfile::VolcanicArchipelago,
            _ => MorphogenesisProfile::AstralCrystalline,
        };
        let tile = generator.generate(SEED, coord, profile);
        let fingerprint = tile.fingerprint();
        if previous_fingerprint != Some(fingerprint) {
            distinct_transitions += 1;
        }
        previous_fingerprint = Some(fingerprint);
        peak_output_bytes = peak_output_bytes.max(tile.generation.output_bytes);
        peak_scratch_bytes = peak_scratch_bytes.max(tile.generation.scratch_bytes);
        peak_work_units = peak_work_units.max(tile.generation.work_units);
        assert_eq!(generator.state_bytes(), 0);
    }

    assert_eq!(distinct_transitions, (SEGMENTS + 1) as usize);
    assert_eq!(peak_output_bytes, OUTPUT_ACCOUNTED_BYTES);
    assert_eq!(peak_scratch_bytes, SCRATCH_ACCOUNTED_BYTES);
    assert!(peak_work_units <= MAX_GENERATION_WORK_UNITS);
    eprintln!(
        "route_km={:.3} samples={} generator_state_bytes={} output_bytes={} scratch_bytes={} peak_work_units={}",
        route_distance_m / 1_000.0,
        SEGMENTS + 1,
        generator.state_bytes(),
        peak_output_bytes,
        peak_scratch_bytes,
        peak_work_units
    );
}

#[test]
fn benchmark_fixed_macro_tile_generation() {
    const SAMPLES: i64 = 32;
    let generator = ContinuumGenerator;
    let _warmup = generator.generate(
        SEED,
        MacroTileCoord::new(-1, 1),
        MorphogenesisProfile::TemperateBasins,
    );
    let mut durations = Vec::with_capacity(SAMPLES as usize);
    let mut fingerprint = 0_u64;
    for sample in 0..SAMPLES {
        let start = Instant::now();
        let tile = generator.generate(
            SEED ^ sample as u64,
            MacroTileCoord::new(sample * 11 - 19, sample * -7 + 5),
            MorphogenesisProfile::TemperateBasins,
        );
        durations.push(start.elapsed());
        assert_eq!(tile.generation.output_bytes, OUTPUT_ACCOUNTED_BYTES);
        assert_eq!(tile.generation.scratch_bytes, SCRATCH_ACCOUNTED_BYTES);
        assert!(tile.generation.work_units <= MAX_GENERATION_WORK_UNITS);
        fingerprint ^= tile.fingerprint().rotate_left(sample as u32);
    }
    assert_ne!(fingerprint, 0);
    durations.sort_unstable();
    let total: Duration = durations.iter().copied().sum();
    let percentile = |numerator: usize, denominator: usize| {
        let index = ((durations.len() - 1) * numerator).div_ceil(denominator);
        durations[index]
    };
    eprintln!(
        "benchmark_mode=cargo_test samples={SAMPLES} mean_ms={:.3} min_ms={:.3} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
        total.as_secs_f64() * 1_000.0 / SAMPLES as f64,
        durations[0].as_secs_f64() * 1_000.0,
        percentile(50, 100).as_secs_f64() * 1_000.0,
        percentile(95, 100).as_secs_f64() * 1_000.0,
        durations[durations.len() - 1].as_secs_f64() * 1_000.0,
    );
}
