//! Celestial sky pass — visible animated sun & moon disc, ~2400 procedural
//! stars, HDR + ACES tonemapping + bloom.
//!
//! ## Why a separate pass?
//!
//! The world camera in `player.rs` runs with exponential-squared fog
//! (which is what hides the chunk-streaming edge for free, see
//! `daynight.rs`). If we drew the sun/moon/stars in that same pass they
//! would get fogged into invisibility a few hundred blocks out.
//!
//! The classic fix is a two-camera composite:
//!
//! 1. **Sky camera** (`order = -1`, this module) renders the celestial
//!    geometry on its own [`RenderLayers`] layer with NO fog. It clears
//!    the window using the existing `ClearColor` resource — so the
//!    smooth day/night/sunset gradient that `daynight.rs` already
//!    drives still works unchanged.
//! 2. **World camera** (`player.rs`, `order = 0`) is patched to NOT
//!    clear color (`ClearColorConfig::None`), so the sky pass shows
//!    through wherever the world doesn't draw. Its fog is unchanged.
//!
//! Both cameras run HDR + ACES tonemapping; the sky camera additionally
//! has [`BloomSettings`] so the emissive sun disc actually *glares*.
//! Stars use a single procedural mesh (~2400 tiny tris on a sphere) with
//! an unlit emissive material whose intensity is animated by the
//! day-factor each frame — one draw call for the whole night sky.

use bevy::core_pipeline::bloom::{BloomCompositeMode, BloomSettings};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::render::view::RenderLayers;
use noise::{NoiseFn, Perlin};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::daynight::WorldIntelRuntime;
use crate::film::FilmRuntime;
use crate::settings::WorldSettings;

/// Render layer used exclusively by the sky pass. The world camera stays
/// on the default layer 0 and never sees these meshes; the sky camera
/// only sees these meshes and never sees the world.
pub const SKY_LAYER: usize = 1;

/// Distance from the camera at which the sun disc, moon disc and star
/// shell are placed. Far enough that parallax is invisible during
/// normal play, close enough that floating-point precision is fine.
const SKY_DISTANCE: f32 = 950.0;

/// Saturn equatorial radius (km). NASA Saturn Fact Sheet.
pub const SATURN_EQUATORIAL_RADIUS_KM: f64 = 60_268.0;
/// Inner edge of Saturn's C ring (km). NASA Saturn Fact Sheet.
pub const SATURN_C_RING_INNER_KM: f64 = 74_658.0;
/// Outer edge of Saturn's A ring (km). NASA Saturn Fact Sheet.
pub const SATURN_A_RING_OUTER_KM: f64 = 136_775.0;
/// Cassini Division centre (km). NASA Saturn Fact Sheet.
pub const SATURN_CASSINI_DIVISION_KM: f64 = 117_580.0;
/// Mean synodic month (days). Meeus, Astronomical Algorithms.
/// Physical reference for `VISUAL_LUNAR_MONTH_DAYS` compression.
pub const SYNODIC_MONTH_DAYS: f64 = 29.530_588_853;
/// Compressed in-game month so a full phase cycle is visible across a
/// short play session (8 in-game days ≈ one visual month).
pub const VISUAL_LUNAR_MONTH_DAYS: f64 = 8.0;
/// Secondary moon semi-major axis as a fraction of the primary moon.
pub const MOON_B_SEMI_MAJOR: f64 = 0.85;
/// Outer tertiary moon semi-major axis (fraction of primary). Slower by Kepler.
pub const MOON_C_SEMI_MAJOR: f64 = 1.25;
/// Visual ring half-height as a fraction of planet radius.
///
/// Real Saturn rings are only ~10 m thick (NASA Saturn Fact Sheet) against
/// an equatorial radius of 60 268 km — invisible at sky-dome scale. We
/// exaggerate to 8 % of the disc radius so the annulus reads as a true
/// 3D volume when viewed near edge-on, while radial proportions stay
/// NASA-true (C-inner / A-outer / Cassini).
pub const SATURN_RING_VISUAL_HALF_HEIGHT_FRAC: f32 = 0.12;

const _: () = assert!(SATURN_C_RING_INNER_KM > SATURN_EQUATORIAL_RADIUS_KM);
const _: () = assert!(SATURN_A_RING_OUTER_KM > SATURN_CASSINI_DIVISION_KM);
const _: () = assert!(SYNODIC_MONTH_DAYS > VISUAL_LUNAR_MONTH_DAYS);

/// Inner/outer sky radii of a Saturn-proportioned ring around a disc of
/// `planet_radius` world units.
pub fn saturn_ring_radii(planet_radius: f32) -> (f32, f32) {
    let inner = planet_radius * (SATURN_C_RING_INNER_KM / SATURN_EQUATORIAL_RADIUS_KM) as f32;
    let outer = planet_radius * (SATURN_A_RING_OUTER_KM / SATURN_EQUATORIAL_RADIUS_KM) as f32;
    (inner, outer)
}

/// Visual half-height of the 3D ring volume for a planet of the given radius.
pub fn saturn_ring_half_height(planet_radius: f32) -> f32 {
    planet_radius * SATURN_RING_VISUAL_HALF_HEIGHT_FRAC
}

/// Cassini Division as a 0..1 coordinate across the C-inner → A-outer span.
pub fn cassini_division_norm() -> f32 {
    let inner = SATURN_C_RING_INNER_KM / SATURN_EQUATORIAL_RADIUS_KM;
    let outer = SATURN_A_RING_OUTER_KM / SATURN_EQUATORIAL_RADIUS_KM;
    let gap = SATURN_CASSINI_DIVISION_KM / SATURN_EQUATORIAL_RADIUS_KM;
    ((gap - inner) / (outer - inner)) as f32
}

/// Illuminated fraction of a spherical Lambert moon. `phase_angle_rad` is
/// the Sun–Moon–observer angle: 0 = full, π = new.
pub fn moon_illuminated_fraction(phase_angle_rad: f64) -> f64 {
    (1.0 + phase_angle_rad.cos()) * 0.5
}

/// Phase angle in radians for the in-game visual month. Seeded so two
/// worlds with different seeds do not share the same sky calendar.
pub fn lunar_phase_angle(time_of_day_hours: f64, seed: u32) -> f64 {
    let seed_day = (seed as f64 % 1024.0) / 1024.0 * VISUAL_LUNAR_MONTH_DAYS;
    let days = time_of_day_hours / 24.0 + seed_day;
    (days / VISUAL_LUNAR_MONTH_DAYS) * std::f64::consts::TAU
}

/// Kepler's third law for circular orbits around the same primary:
/// `T / T_ref = (a / a_ref)^{3/2}`.
pub fn kepler_period_ratio(semi_major: f64, semi_major_ref: f64) -> f64 {
    (semi_major / semi_major_ref).powf(1.5)
}

/// Mean-motion ratio n / n_ref = (a_ref / a)^{3/2} = 1 / period_ratio.
pub fn kepler_mean_motion_ratio(semi_major: f64, semi_major_ref: f64) -> f64 {
    1.0 / kepler_period_ratio(semi_major, semi_major_ref)
}

/// Unit direction of a moon that has drifted `phase_angle` ahead of the
/// anti-sun point, with a small orbital inclination.
///
/// Phase 0 (full) sits at `-sun_dir`; phase π (new) sits at `+sun_dir`.
/// The rotation axis is perpendicular to the sun so Y-up sun vectors still
/// reach opposition.
pub fn moon_orbit_dir(sun_dir: Vec3, phase_angle: f32, inclination: f32) -> Vec3 {
    let sun_dir = sun_dir.normalize();
    let helper = if sun_dir.z.abs() < 0.9 {
        Vec3::Z
    } else {
        Vec3::X
    };
    let axis = sun_dir.cross(helper).normalize();
    let mut dir = Quat::from_axis_angle(axis, phase_angle) * -sun_dir;
    if inclination.abs() > 1e-5 {
        let incl_axis = dir.cross(axis);
        if incl_axis.length_squared() > 1e-8 {
            dir = Quat::from_axis_angle(incl_axis.normalize(), inclination) * dir;
        }
    }
    dir.normalize()
}

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_sky)
            // PostUpdate, BEFORE transform propagation. We write the
            // sky camera's Transform from the main camera's
            // GlobalTransform; if we ran AFTER propagation, the sky
            // cam's own GlobalTransform would stay one frame stale and
            // the player would see stars swing in the opposite
            // direction of the mouse (because the sky view lagged the
            // world view by exactly one frame).
            .add_systems(
                PostUpdate,
                (
                    follow_and_animate_sky
                        .before(bevy::transform::TransformSystem::TransformPropagate),
                    film_tame_sky_bloom,
                ),
            );
    }
}

#[derive(Component)]
struct SkyCamera;

#[derive(Component)]
struct SunDisc;

#[derive(Component)]
struct MoonDisc;

/// Second, smaller moon that lags behind the main one for a binary-
/// moon system (see reference image 1 with its crescent pair).
#[derive(Component)]
struct MoonDiscB;

/// Outer tertiary moon — slower Kepler companion for the three-moon sky.
#[derive(Component)]
struct MoonDiscC;

/// Distant ringed gas giant parked high in the sky for epic framing.
/// Stays fixed on the celestial dome and rotates slowly for parallax.
#[derive(Component)]
struct RingedPlanet;

/// Second, smaller ice-teal gas giant parked on the opposite horizon for
/// that "alien-system" framing with multiple moons and rings.
#[derive(Component)]
struct PlanetB;

#[derive(Component)]
struct StarField;

/// Huge inside-out sphere painted with procedural multi-colour nebula
/// clouds. Sits behind the stars and blends softly with the day sky.
#[derive(Component)]
struct Nebula;

/// Cached material handles so the day-factor system can re-tint emissives
/// each frame without scanning queries.
#[derive(Resource)]
struct SkyMaterials {
    sun: Handle<StandardMaterial>,
    moon: Handle<StandardMaterial>,
    moon_b: Handle<StandardMaterial>,
    moon_c: Handle<StandardMaterial>,
    planet: Handle<StandardMaterial>,
    ring: Handle<StandardMaterial>,
    planet_b: Handle<StandardMaterial>,
    stars: Handle<StandardMaterial>,
    nebula: Handle<StandardMaterial>,
}

fn setup_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    settings: Res<WorldSettings>,
    film: Res<FilmRuntime>,
) {
    let sky_layer = RenderLayers::layer(SKY_LAYER);

    // Nebula resolution + star count scale with graphics tier so low-end
    // GPUs still get the look for a fraction of the fill cost.
    use crate::settings::GraphicsMode;
    let (nebula_res, star_count) = match settings.graphics {
        GraphicsMode::Fast => (256u32, 1800usize),
        GraphicsMode::Balanced => (512, 3200),
        GraphicsMode::High => (1024, 5200),
    };

    // ----- Sky camera --------------------------------------------------
    // order = -1 → renders BEFORE the world camera in `player.rs` and
    // clears the framebuffer with the global `ClearColor` (which the
    // existing daynight system already animates between sky/sunset/night
    // colours). The world camera then composites on top with
    // `ClearColorConfig::None`.
    commands.spawn((
        Camera3dBundle {
            camera: Camera {
                order: -1,
                hdr: true,
                ..default()
            },
            tonemapping: Tonemapping::AcesFitted,
            transform: Transform::IDENTITY,
            // FOV slightly wider than typical world cam so the sky
            // doesn't shrink when the player FOV is small.
            projection: Projection::Perspective(PerspectiveProjection {
                fov: 80.0f32.to_radians(),
                near: 1.0,
                far: SKY_DISTANCE * 8.0,
                ..default()
            }),
            ..default()
        },
        // Bloom — what makes the sun read as a true light source rather
        // than a flat circle. `OLD_SCHOOL` gives a pronounced halo,
        // perfect for a stylised voxel sky. Threshold is non-zero in
        // that preset, so only the high-intensity emissives (sun + the
        // brightest stars) bloom; the gradient sky stays clean.
        BloomSettings {
            composite_mode: BloomCompositeMode::Additive,
            ..BloomSettings::OLD_SCHOOL
        },
        sky_layer.clone(),
        SkyCamera,
        Name::new("SkyCamera"),
    ));

    // ----- Sun disc ----------------------------------------------------
    // Smooth icosphere — subdivision 3 is plenty smooth at SKY_DISTANCE.
    let sun_mesh = meshes.add(
        Sphere::new(28.0)
            .mesh()
            .ico(3)
            .expect("subdivision 3 is within ico limits"),
    );
    // Emissive intensities are intentionally *huge* (linear-RGB units,
    // > 1.0). Bloom + tonemap turn this into a glaring yellow-white
    // disc with a warm halo. Day/night system retunes this each frame.
    let sun_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.95, 0.85),
        emissive: LinearRgba::rgb(60.0, 50.0, 30.0),
        unlit: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: sun_mesh,
            material: sun_mat.clone(),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        SunDisc,
        Name::new("SunDisc"),
    ));

    // ----- Moon disc ---------------------------------------------------
    let moon_mesh = meshes.add(
        Sphere::new(20.0)
            .mesh()
            .ico(3)
            .expect("subdivision 3 is within ico limits"),
    );
    let moon_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.92, 0.94, 1.0),
        emissive: LinearRgba::rgb(6.0, 7.0, 11.0),
        unlit: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: moon_mesh,
            material: moon_mat.clone(),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        MoonDisc,
        Name::new("MoonDisc"),
    ));

    // ----- Star field --------------------------------------------------
    // Dense, colour-varied star shell. Count scales with graphics tier.
    // Colours follow a simplified stellar classification: mostly cool
    // white, with blue giants, yellow/orange main-sequence, and a few
    // red giants. The bloom pass on the sky cam turns the brightest
    // ones into genuine twinkling haloes.
    let stars_mesh = meshes.add(build_star_mesh(
        star_count,
        0xC0FFEE_u64,
        SKY_DISTANCE * 0.95,
    ));
    let stars_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        // Starts at zero — the day/night system fades them in at night.
        emissive: LinearRgba::rgb(0.0, 0.0, 0.0),
        unlit: true,
        // Additive blend: dark corner vertices contribute nothing,
        // bright centres accumulate on top of the sky gradient. This
        // is what lets the 5-vert fan read as a soft round point
        // instead of a square dark-cornered sprite.
        alpha_mode: AlphaMode::Add,
        // Vertex colours carry the per-star tint (blue/yellow/red).
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: stars_mesh,
            material: stars_mat.clone(),
            transform: Transform::IDENTITY,
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        StarField,
        Name::new("StarField"),
    ));

    // ----- Nebula backdrop --------------------------------------------
    // Huge inside-facing sphere painted with a procedural nebula image.
    // Multi-channel Perlin produces billowing magenta/cyan/orange clouds
    // exactly like the reference art. Cull front-face so we only see it
    // from the inside, and keep it fully unlit/emissive.
    let nebula_image = images.add(build_nebula_image(
        nebula_res,
        settings.seed as u64,
        film.enabled,
    ));
    let nebula_mesh = meshes.add(
        Sphere::new(SKY_DISTANCE * 2.6)
            .mesh()
            .ico(4)
            .expect("subdivision 4 is within ico limits"),
    );
    let nebula_mat = materials.add(StandardMaterial {
        // ADDITIVE blend so the nebula lays its colored clouds on top
        // of the daynight-driven ClearColor instead of replacing it.
        // Without this the inside-facing sphere completely wraps the
        // camera and paints over the sky gradient → black sky at noon.
        base_color: Color::srgba(1.0, 1.0, 1.0, 1.0),
        base_color_texture: Some(nebula_image.clone()),
        emissive_texture: Some(nebula_image),
        // Strong emissive — with AlphaMode::Add the final on-screen
        // colour is sky_clear + nebula_texture*emissive, so we want
        // a punchy value. Day/night loop animates this per-frame.
        emissive: LinearRgba::rgb(2.0, 1.6, 2.6),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        // Render from inside the sphere — show back faces.
        cull_mode: Some(bevy::render::render_resource::Face::Front),
        double_sided: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: nebula_mesh,
            material: nebula_mat.clone(),
            transform: Transform::IDENTITY,
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        Nebula,
        Name::new("Nebula"),
    ));

    // ----- Second (smaller) moon --------------------------------------
    // Slightly offset from the main moon to create the paired-crescent
    // look in the reference art.
    let moon_b_mesh = meshes.add(
        Sphere::new(13.0)
            .mesh()
            .ico(3)
            .expect("subdivision 3 is within ico limits"),
    );
    let moon_b_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.88, 0.80, 0.95),
        emissive: LinearRgba::rgb(4.0, 3.0, 8.0),
        unlit: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: moon_b_mesh,
            material: moon_b_mat.clone(),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        MoonDiscB,
        Name::new("MoonDiscB"),
    ));

    // ----- Third (outer) moon ----------------------------------------
    let moon_c_mesh = meshes.add(
        Sphere::new(9.0)
            .mesh()
            .ico(3)
            .expect("subdivision 3 is within ico limits"),
    );
    let moon_c_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.78, 0.86, 0.95),
        emissive: LinearRgba::rgb(2.5, 3.5, 5.5),
        unlit: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: moon_c_mesh,
            material: moon_c_mat.clone(),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        MoonDiscC,
        Name::new("MoonDiscC"),
    ));

    // ----- Ringed gas-giant planet ------------------------------------
    // Parked in a fixed sky direction; doesn't track the sun. Serves as
    // a dramatic backdrop feature like in reference image 2.
    let planet_radius = 110.0;
    let (ring_inner, ring_outer) = saturn_ring_radii(planet_radius);
    let ring_half_h = saturn_ring_half_height(planet_radius);
    let planet_mesh = meshes.add(
        Sphere::new(planet_radius)
            .mesh()
            .ico(4)
            .expect("subdivision 4 is within ico limits"),
    );
    let planet_mat = materials.add(StandardMaterial {
        // Vibrant magenta–amber gas giant (matches the purple/teal ringed
        // giant in the reference art). Very high emissive so it glows
        // like a light source in its own right, not just reflects the sun.
        base_color: Color::srgb(0.95, 0.55, 1.0),
        emissive: LinearRgba::rgb(6.0, 2.2, 8.5),
        unlit: true,
        ..default()
    });
    // Ring: extruded 3D annulus (top + bottom + walls) with Cassini density.
    let ring_mesh = meshes.add(build_ring_mesh(ring_inner, ring_outer, ring_half_h, 192));
    let ring_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.9, 0.8, 1.0),
        emissive: LinearRgba::rgb(5.5, 4.5, 6.5),
        unlit: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    // Fixed sky direction: upper-right, high enough to dominate the
    // horizon without blocking gameplay sight-lines.
    let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
    let planet_pos = planet_dir * SKY_DISTANCE * 0.9;
    commands
        .spawn((
            PbrBundle {
                mesh: planet_mesh,
                material: planet_mat.clone(),
                transform: Transform::from_translation(planet_pos)
                    // Steeper tilt so film hero frames catch a thick
                    // ring silhouette instead of a face-on disc.
                    .with_rotation(Quat::from_rotation_x(0.95) * Quat::from_rotation_z(-0.22)),
                ..default()
            },
            NotShadowCaster,
            sky_layer.clone(),
            RingedPlanet,
            Name::new("RingedPlanet"),
        ))
        .with_children(|p| {
            p.spawn((
                PbrBundle {
                    mesh: ring_mesh,
                    material: ring_mat.clone(),
                    // Identity — the extruded mesh already spans ±Y, so a
                    // 90° X flip would tip the volume on edge incorrectly.
                    transform: Transform::IDENTITY,
                    ..default()
                },
                NotShadowCaster,
                sky_layer.clone(),
                Name::new("RingedPlanet.Ring"),
            ));
        });

    // ----- Second planet (ice-teal gas giant) -------------------------
    // Parked low on the opposite horizon. Smaller, cooler-coloured,
    // no rings — complements the main giant for a "binary system" feel.
    let planet_b_mesh = meshes.add(
        Sphere::new(72.0)
            .mesh()
            .ico(4)
            .expect("subdivision 4 is within ico limits"),
    );
    let planet_b_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.45, 0.85, 1.0),
        emissive: LinearRgba::rgb(2.5, 5.5, 7.5),
        unlit: true,
        ..default()
    });
    let planet_b_dir = Vec3::new(-0.72, 0.28, 0.55).normalize();
    commands.spawn((
        PbrBundle {
            mesh: planet_b_mesh,
            material: planet_b_mat.clone(),
            transform: Transform::from_translation(planet_b_dir * SKY_DISTANCE * 0.88)
                .with_rotation(Quat::from_rotation_x(0.2)),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        PlanetB,
        Name::new("PlanetB"),
    ));

    commands.insert_resource(SkyMaterials {
        sun: sun_mat,
        moon: moon_mat,
        moon_b: moon_b_mat,
        moon_c: moon_c_mat,
        planet: planet_mat,
        ring: ring_mat,
        planet_b: planet_b_mat,
        stars: stars_mat,
        nebula: nebula_mat,
    });
}

/// Glue the sky camera to the player camera, place sun/moon on the
/// celestial sphere using the same time-of-day formula as `daynight.rs`,
/// rotate the star shell, and re-tint the emissives by day-factor.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn follow_and_animate_sky(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    film: Option<Res<FilmRuntime>>,
    main_cam: Query<&GlobalTransform, (With<Camera3d>, Without<SkyCamera>)>,
    mut sky_cam: Query<&mut Transform, With<SkyCamera>>,
    mut sun_q: Query<
        &mut Transform,
        (
            With<SunDisc>,
            Without<SkyCamera>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut moon_q: Query<
        &mut Transform,
        (
            With<MoonDisc>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut moon_b_q: Query<
        &mut Transform,
        (
            With<MoonDiscB>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut moon_c_q: Query<
        &mut Transform,
        (
            With<MoonDiscC>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut stars_q: Query<
        &mut Transform,
        (
            With<StarField>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<RingedPlanet>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut planet_q: Query<
        &mut Transform,
        (
            With<RingedPlanet>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<PlanetB>,
            Without<Nebula>,
        ),
    >,
    mut planet_b_q: Query<
        &mut Transform,
        (
            With<PlanetB>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<Nebula>,
        ),
    >,
    mut nebula_q: Query<
        &mut Transform,
        (
            With<Nebula>,
            Without<SkyCamera>,
            Without<SunDisc>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
            Without<MoonDiscC>,
            Without<StarField>,
            Without<RingedPlanet>,
            Without<PlanetB>,
        ),
    >,
    sky_mats: Option<Res<SkyMaterials>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(main_tf) = main_cam.get_single() else {
        return;
    };
    let Ok(mut sky_tf) = sky_cam.get_single_mut() else {
        return;
    };

    // Mirror player camera transform exactly so screen-space alignment
    // is identical and the sky moves correctly with mouse-look.
    let (_, rot, trans) = main_tf.to_scale_rotation_translation();
    sky_tf.translation = trans;
    sky_tf.rotation = rot;

    // Same celestial-angle math as daynight.rs::update_sun. Keep these
    // formulas in sync; they share the same `time_of_day` resource.
    let t = (settings.time_of_day / 24.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    let sun_dir = Vec3::new(t.cos(), t.sin(), 0.3).normalize();
    let phase = lunar_phase_angle(settings.time_of_day as f64, settings.seed);
    let illum = moon_illuminated_fraction(phase) as f32;
    let moon_dir = moon_orbit_dir(sun_dir, phase as f32, 0.09);
    let phase_b = phase * kepler_mean_motion_ratio(MOON_B_SEMI_MAJOR, 1.0);
    let moon_b_dir = moon_orbit_dir(sun_dir, phase_b as f32, 0.22);
    let phase_c = phase * kepler_mean_motion_ratio(MOON_C_SEMI_MAJOR, 1.0);
    let moon_c_dir = moon_orbit_dir(sun_dir, phase_c as f32, -0.14);

    if let Ok(mut sun_tf) = sun_q.get_single_mut() {
        sun_tf.translation = trans + sun_dir * SKY_DISTANCE;
    }
    if let Ok(mut moon_tf) = moon_q.get_single_mut() {
        moon_tf.translation = trans + moon_dir * SKY_DISTANCE;
    }
    if let Ok(mut moon_b_tf) = moon_b_q.get_single_mut() {
        moon_b_tf.translation = trans + moon_b_dir * (SKY_DISTANCE * MOON_B_SEMI_MAJOR as f32);
    }
    if let Ok(mut moon_c_tf) = moon_c_q.get_single_mut() {
        moon_c_tf.translation = trans + moon_c_dir * (SKY_DISTANCE * MOON_C_SEMI_MAJOR as f32);
    }
    if let Ok(mut stars_tf) = stars_q.get_single_mut() {
        stars_tf.translation = trans;
        // Stars share the celestial rotation so they wheel across the
        // sky together with sun and moon.
        stars_tf.rotation = Quat::from_rotation_z(t);
    }
    if let Ok(mut planet_tf) = planet_q.get_single_mut() {
        // Fixed direction, NEVER rotates — stationary landmark.
        let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
        planet_tf.translation = trans + planet_dir * SKY_DISTANCE * 0.9;
    }
    if let Ok(mut planet_b_tf) = planet_b_q.get_single_mut() {
        // Fixed direction on the opposite horizon, NEVER rotates.
        let dir = Vec3::new(-0.72, 0.28, 0.55).normalize();
        planet_b_tf.translation = trans + dir * SKY_DISTANCE * 0.88;
    }
    if let Ok(mut nebula_tf) = nebula_q.get_single_mut() {
        // Stationary nebula backdrop — the painterly clouds should
        // feel like a cosmic painting fixed behind us, not a slow
        // carousel. No rotation.
        nebula_tf.translation = trans;
    }

    // ----- Animate emissives by day factor -----------------------------
    let day = sun_dir.y.max(0.0); // 1 at noon, 0 at horizon, 0 at night
    let night = (1.0 - day).powf(2.0); // sharper fade-in for stars
    let sunset = (1.0 - sun_dir.y.abs()).powf(3.0); // peak at horizon

    if let Some(sky_mats) = sky_mats {
        // Sun: warm white at noon → fiery red-orange at sunset.
        if let Some(mat) = materials.get_mut(&sky_mats.sun) {
            let noon = Vec3::new(60.0, 50.0, 30.0);
            let dusk = Vec3::new(80.0, 22.0, 8.0);
            let e = noon.lerp(dusk, sunset);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Moon: cool blue, scaled by the Lambert illuminated fraction so
        // a new moon is a dim disc and a full moon blooms.
        if let Some(mat) = materials.get_mut(&sky_mats.moon) {
            let base = Vec3::new(6.0, 7.0, 11.0);
            let scaled = base * (0.22 + 0.90 * illum) * (0.6 + 0.6 * night);
            mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
        }

        // Second moon — cool violet, Kepler-faster orbit, slightly dimmer.
        if let Some(mat) = materials.get_mut(&sky_mats.moon_b) {
            let illum_b = moon_illuminated_fraction(phase_b) as f32;
            let base = Vec3::new(4.0, 3.0, 8.0);
            let scaled = base * (0.22 + 0.90 * illum_b) * (0.55 + 0.55 * night);
            mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
        }

        // Third moon — ice-teal, Kepler-slower outer companion.
        if let Some(mat) = materials.get_mut(&sky_mats.moon_c) {
            let illum_c = moon_illuminated_fraction(phase_c) as f32;
            let base = Vec3::new(2.5, 3.5, 5.5);
            let scaled = base * (0.20 + 0.85 * illum_c) * (0.50 + 0.55 * night);
            mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
        }

        // Ringed planet & rings — brightly emissive at all times so the
        // magenta disc and rainbow rings stay breathtaking at noon too,
        // just like in the reference art. Slight extra glow at
        // night/sunset for the cinematic payoff.
        let planet_scale = 1.8 + 0.8 * night + sunset * 0.5;
        if let Some(mat) = materials.get_mut(&sky_mats.planet) {
            let base = Vec3::new(8.0, 3.0, 11.0);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }
        if let Some(mat) = materials.get_mut(&sky_mats.ring) {
            let base = Vec3::new(7.0, 6.0, 8.5);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }
        if let Some(mat) = materials.get_mut(&sky_mats.planet_b) {
            let base = Vec3::new(3.5, 7.5, 10.0);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }

        // Nebula — vivid magenta/cyan/orange at all times (additive
        // blend paints clouds on top of the sky gradient). Day values
        // are pushed HARD so the cosmic backdrop reads clearly even
        // against the bright blue noon sky, just like in the reference
        // art where planets and nebulae are visible in broad daylight.
        // Film mode pushes saturation further for painting-hero frames
        // without changing the daytime pad lighting path.
        if let Some(mat) = materials.get_mut(&sky_mats.nebula) {
            let film_on = film.as_ref().map(|f| f.enabled).unwrap_or(false);
            let (base_day, base_night, base_sunset) = if film_on {
                // Punchy but not ACES-white; filaments need headroom.
                (
                    Vec3::new(9.0, 3.5, 14.0),
                    Vec3::new(10.0, 4.5, 15.0),
                    Vec3::new(11.0, 4.0, 4.0),
                )
            } else {
                (
                    Vec3::new(9.0, 5.0, 13.0),
                    Vec3::new(8.0, 5.5, 10.0),
                    Vec3::new(12.0, 5.0, 4.5),
                )
            };
            let e = (base_day * day + base_night * night + base_sunset * sunset * 0.9)
                * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Stars: fade in linearly with night.
        if let Some(mat) = materials.get_mut(&sky_mats.stars) {
            let intensity = 14.0 * night * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(intensity, intensity, intensity * 1.15);
        }
    }
}

fn film_tame_sky_bloom(
    film: Option<Res<FilmRuntime>>,
    mut sky_bloom: Query<&mut BloomSettings, With<SkyCamera>>,
) {
    let Some(film) = film else {
        return;
    };
    if !film.enabled || !film.ready_to_roll {
        return;
    }
    let Ok(mut bloom) = sky_bloom.get_single_mut() else {
        return;
    };
    // Sky-camera OLD_SCHOOL bloom washes nebula filaments to grey haze.
    if matches!(film.shot_index, 7 | 8 | 9 | 10) {
        bloom.intensity = 0.04;
        bloom.prefilter_settings.threshold = 0.85;
    }
}

/// Build a single mesh holding `count` tiny triangles at random points on
/// a sphere of radius `radius`. Each triangle faces the origin so the
/// flat side is always visible from inside the shell.
///
/// Deterministic: same `seed` always produces the same star pattern, so
/// the night sky doesn't flicker between runs (or between save reloads).
///
/// Per-star colours follow a simplified stellar classification so the
/// sky reads as richer than a monochrome sprinkle: cool white majority,
/// with blue giants, yellow main-sequence and a few red giants. Tinted
/// through Mesh::ATTRIBUTE_COLOR which StandardMaterial multiplies by
/// `emissive` during the night fade-in.
fn build_star_mesh(count: usize, seed: u64, radius: f32) -> Mesh {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    // 5 verts per star (1 centre + 4 corners), 4 triangles (12 indices).
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(count * 5);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(count * 5);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(count * 5);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(count * 5);
    let mut indices: Vec<u32> = Vec::with_capacity(count * 12);

    // Stellar classes: (weight, rgb tint).
    const CLASSES: [(f32, [f32; 3]); 5] = [
        (0.55, [1.00, 1.00, 1.00]), // cool white (majority)
        (0.18, [0.70, 0.80, 1.00]), // blue giants
        (0.15, [1.00, 0.95, 0.75]), // yellow main sequence
        (0.08, [1.00, 0.70, 0.45]), // orange
        (0.04, [1.00, 0.45, 0.35]), // red giants
    ];

    for i in 0..count {
        // Uniform points on a sphere (Marsaglia).
        let z: f32 = rng.gen_range(-1.0_f32..1.0_f32);
        let phi: f32 = rng.gen_range(0.0_f32..std::f32::consts::TAU);
        let s = (1.0 - z * z).sqrt();
        let dir = Vec3::new(s * phi.cos(), z, s * phi.sin());
        let center = dir * radius;

        // Long-tail size distribution. Corners sit in near-darkness
        // so the visible blob is smaller than `star_size` — that's
        // what makes stars read as round points rather than diamonds.
        let r: f32 = rng.gen();
        let star_size = 0.6 + r.powf(7.0) * 4.5;

        // Per-star brightness multiplier — a few punchy, most dim.
        let b: f32 = rng.gen();
        let bright = 0.30 + b.powf(3.0) * 1.6;

        // Pick stellar class.
        let pick: f32 = rng.gen();
        let mut accum = 0.0;
        let mut tint = [1.0, 1.0, 1.0];
        for (w, t) in &CLASSES {
            accum += w;
            if pick <= accum {
                tint = *t;
                break;
            }
        }

        // Camera-facing basis (the sky camera sits at the shell
        // centre, so the normal toward the camera is `-dir`).
        let n = -dir;
        let up = if n.y.abs() < 0.9 { Vec3::Y } else { Vec3::X };
        let tx = n.cross(up).normalize();
        let ty = tx.cross(n).normalize();

        // Triangle fan: centre vertex (bright) + 4 corner vertices
        // (nearly black). Gouraud interpolation between them produces
        // a soft circular falloff across the quad — each star reads
        // as a tiny round glow, then bloom turns the brightest ones
        // into genuine haloes.
        let centre_col = [tint[0] * bright, tint[1] * bright, tint[2] * bright, 1.0];
        let corner_col = [0.0, 0.0, 0.0, 1.0];

        let p_c = center;
        let p0 = center + (tx + ty) * star_size; // +x +y
        let p1 = center + (-tx + ty) * star_size; // -x +y
        let p2 = center + (-tx - ty) * star_size; // -x -y
        let p3 = center + (tx - ty) * star_size; // +x -y

        let base = (i * 5) as u32;
        positions.push(p_c.to_array());
        positions.push(p0.to_array());
        positions.push(p1.to_array());
        positions.push(p2.to_array());
        positions.push(p3.to_array());
        for _ in 0..5 {
            normals.push(n.to_array());
            uvs.push([0.5, 0.5]);
        }
        colors.push(centre_col);
        colors.push(corner_col);
        colors.push(corner_col);
        colors.push(corner_col);
        colors.push(corner_col);
        // Four CCW triangles fanning out from the centre vertex.
        for (a, b) in [(1, 2), (2, 3), (3, 4), (4, 1)] {
            indices.push(base);
            indices.push(base + a);
            indices.push(base + b);
        }
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Optical depth of the Saturn-proportioned ring at radial coordinate
/// `t` in [0, 1] from C-inner to A-outer. Cassini Division is a Gaussian
/// density drop around [`cassini_division_norm`].
pub fn saturn_ring_density(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let cassini = cassini_division_norm();
    let gap = 1.0 - (-((t - cassini) * 28.0).powi(2)).exp() * 0.82;
    (0.55 + 0.45 * (1.0 - (t - 0.35).abs())).clamp(0.22, 1.0) * gap
}

/// Build an extruded Saturn-proportioned ring volume.
///
/// Top + bottom discs plus inner/outer cylindrical walls give a readable
/// 3D silhouette (not a flat billboard). `half_height` is the ±Y extent;
/// radial samples carry Cassini Division density via vertex colour.
fn build_ring_mesh(inner: f32, outer: f32, half_height: f32, segs: usize) -> Mesh {
    const RINGS: usize = 8;
    let verts_per_seg = (RINGS + 1) * 2; // top + bottom per radial sample
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(segs * verts_per_seg + segs * 4);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(segs * verts_per_seg + segs * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(segs * verts_per_seg + segs * 4);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(segs * verts_per_seg + segs * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(segs * RINGS * 12 + segs * 24);

    let warm = Vec3::new(0.95, 0.78, 0.58);
    let ice = Vec3::new(0.72, 0.80, 0.95);
    let h = half_height.max(0.05);

    for i in 0..segs {
        let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
        let (sa, ca) = a.sin_cos();
        for r in 0..=RINGS {
            let t = r as f32 / RINGS as f32;
            let rad = inner + (outer - inner) * t;
            let density = saturn_ring_density(t);
            let c = warm.lerp(ice, t) * density;
            let col = [c.x, c.y, c.z, 0.88 * density];
            // Top
            positions.push([ca * rad, h, sa * rad]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([i as f32 / segs as f32, t]);
            colors.push(col);
            // Bottom
            positions.push([ca * rad, -h, sa * rad]);
            normals.push([0.0, -1.0, 0.0]);
            uvs.push([i as f32 / segs as f32, t]);
            colors.push(col);
        }
    }

    let disc_vert_count = (segs * verts_per_seg) as u32;
    for i in 0..segs {
        let i0 = (i * verts_per_seg) as u32;
        let i1 = (((i + 1) % segs) * verts_per_seg) as u32;
        for r in 0..RINGS {
            let base = (r * 2) as u32;
            // Top face (even indices)
            let a = i0 + base;
            let b = a + 2;
            let c = i1 + base;
            let d = c + 2;
            indices.extend_from_slice(&[a, b, d, a, d, c]);
            // Bottom face (odd indices) — winding flipped for −Y normals
            let a = i0 + base + 1;
            let b = a + 2;
            let c = i1 + base + 1;
            let d = c + 2;
            indices.extend_from_slice(&[a, d, b, a, c, d]);
        }
    }

    // Outer + inner walls
    let wall_start = disc_vert_count;
    for i in 0..segs {
        let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
        let (sa, ca) = a.sin_cos();
        // Outer wall verts
        positions.push([ca * outer, h, sa * outer]);
        normals.push([ca, 0.0, sa]);
        uvs.push([i as f32 / segs as f32, 1.0]);
        colors.push([0.85, 0.78, 0.70, 0.75]);
        positions.push([ca * outer, -h, sa * outer]);
        normals.push([ca, 0.0, sa]);
        uvs.push([i as f32 / segs as f32, 0.0]);
        colors.push([0.85, 0.78, 0.70, 0.75]);
        // Inner wall verts
        positions.push([ca * inner, h, sa * inner]);
        normals.push([-ca, 0.0, -sa]);
        uvs.push([i as f32 / segs as f32, 1.0]);
        colors.push([0.70, 0.72, 0.85, 0.55]);
        positions.push([ca * inner, -h, sa * inner]);
        normals.push([-ca, 0.0, -sa]);
        uvs.push([i as f32 / segs as f32, 0.0]);
        colors.push([0.70, 0.72, 0.85, 0.55]);
    }
    for i in 0..segs {
        let i0 = wall_start + (i * 4) as u32;
        let i1 = wall_start + (((i + 1) % segs) * 4) as u32;
        // Outer wall
        indices.extend_from_slice(&[i0, i0 + 1, i1 + 1, i0, i1 + 1, i1]);
        // Inner wall
        indices.extend_from_slice(&[i0 + 2, i1 + 2, i1 + 3, i0 + 2, i1 + 3, i0 + 3]);
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Axis-aligned Y extent of a ring mesh built by [`build_ring_mesh`].
#[cfg(test)]
fn ring_mesh_y_extent(mesh: &Mesh) -> f32 {
    let Some(attr) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else {
        return 0.0;
    };
    let Some(positions) = attr.as_float3() else {
        return 0.0;
    };
    let mut min_y = f32::MAX;
    let mut max_y = f32::MIN;
    for p in positions {
        min_y = min_y.min(p[1]);
        max_y = max_y.max(p[1]);
    }
    max_y - min_y
}

/// Build a procedural nebula image — multi-octave 3D Perlin on a
/// spherical projection, three colour channels sampled at different
/// frequencies. Produces billowing magenta / cyan / orange clouds
/// reminiscent of Hubble field backdrops. Deterministic by seed.
fn build_nebula_image(size: u32, seed: u64, dense: bool) -> Image {
    let n_r = Perlin::new(seed as u32 ^ 0x7777_7777);
    let n_g = Perlin::new(seed as u32 ^ 0x3333_3333);
    let n_b = Perlin::new(seed as u32 ^ 0xBBBB_BBBB);
    let n_mask = Perlin::new(seed as u32 ^ 0x1234_5678);
    // Equirectangular mapping: x → longitude [0, 2π), y → latitude [-π/2, π/2].
    let w = size;
    let h = size / 2;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            // Unit-sphere sample direction.
            let px = cv * cu;
            let py = sv;
            let pz = cv * su;

            // FBM helper inlined for speed.
            let fbm = |n: &Perlin, f: f64, oct: u32| -> f64 {
                let mut sum = 0.0;
                let mut amp = 1.0;
                let mut freq = f;
                let mut norm = 0.0;
                for _ in 0..oct {
                    sum += amp * n.get([px * freq, py * freq, pz * freq]);
                    norm += amp;
                    amp *= 0.55;
                    freq *= 2.0;
                }
                sum / norm.max(1e-6)
            };
            let r = fbm(&n_r, 1.3, 5);
            let g = fbm(&n_g, 1.7, 5);
            let b = fbm(&n_b, 1.1, 5);
            let mask = fbm(&n_mask, 0.6, 3);
            // Soft mask so large regions of the sphere are near-black,
            // and only a few filaments glow strongly — exactly like
            // real nebulae. Film mode widens/saturates the cloud mass.
            let amp = if dense {
                // Filamentary: keep large black voids so Additive chroma survives ACES.
                (mask + 0.28).max(0.0).powf(2.05)
            } else {
                (mask + 0.2).max(0.0).powf(1.6)
            };
            let sat = if dense { 2.15 } else { 1.0 };
            let rr = ((r * 0.5 + 0.5) * amp * sat).clamp(0.0, 1.0);
            let gg = ((g * 0.5 + 0.5) * amp * 0.55 * sat).clamp(0.0, 1.0);
            let bb = ((b * 0.5 + 0.5) * amp * 1.35 * sat).clamp(0.0, 1.0);

            // Colour palette skewed toward magenta / cyan / warm orange
            // highlights. Mix the raw channels with fixed biases so the
            // image looks painterly, not random.
            let mag = (rr * 1.15 + bb * 0.35).min(1.0);
            let cyn = (gg * 0.9 + bb * 1.1).min(1.0);
            let wrm = (rr * 1.05 + gg * 0.7).min(1.0);
            let r_out = (mag * 0.9 + wrm * 0.6).min(1.0);
            let g_out = (cyn * 0.6 + wrm * 0.55).min(1.0);
            let b_out = (mag * 0.5 + cyn * 1.0).min(1.0);

            data.push((r_out * 255.0) as u8);
            data.push((g_out * 255.0) as u8);
            data.push((b_out * 255.0) as u8);
            data.push(255);
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::ClampToEdge,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturn_ring_proportions_match_nasa_fact_sheet() {
        let (inner, outer) = saturn_ring_radii(100.0);
        let inner_ratio = inner / 100.0;
        let outer_ratio = outer / 100.0;
        assert!((inner_ratio - 1.2388).abs() < 0.002);
        assert!((outer_ratio - 2.2694).abs() < 0.002);
        assert!(inner < outer);
        let cassini = cassini_division_norm();
        assert!(cassini > 0.6 && cassini < 0.75);
        assert!(
            saturn_ring_density(cassini) < saturn_ring_density(0.35) * 0.5,
            "Cassini Division should be a real density drop"
        );
        let half_h = saturn_ring_half_height(100.0);
        assert!((half_h - 12.0).abs() < 1e-4);
        let mesh = build_ring_mesh(inner, outer, half_h, 48);
        let y_extent = ring_mesh_y_extent(&mesh);
        assert!(
            y_extent > half_h * 1.9,
            "ring mesh must be a 3D volume, got Y extent {y_extent}"
        );
    }

    #[test]
    fn kepler_three_moon_hierarchy_and_opposition() {
        let inner = kepler_mean_motion_ratio(MOON_B_SEMI_MAJOR, 1.0);
        let outer = kepler_mean_motion_ratio(MOON_C_SEMI_MAJOR, 1.0);
        assert!(inner > 1.0, "inner moon should orbit faster than primary");
        assert!(outer < 1.0, "outer moon should orbit slower than primary");
        assert!(inner > outer);
        let period_b = kepler_period_ratio(MOON_B_SEMI_MAJOR, 1.0);
        let period_c = kepler_period_ratio(MOON_C_SEMI_MAJOR, 1.0);
        assert!((period_b - MOON_B_SEMI_MAJOR.powf(1.5)).abs() < 1e-9);
        assert!((period_c - MOON_C_SEMI_MAJOR.powf(1.5)).abs() < 1e-9);
        assert!((inner * period_b - 1.0).abs() < 1e-12);
        let sun = Vec3::new(0.0, 1.0, 0.3).normalize();
        let full = moon_orbit_dir(sun, 0.0, 0.0);
        let new = moon_orbit_dir(sun, std::f32::consts::PI, 0.0);
        assert!(full.dot(-sun) > 0.95);
        assert!(new.dot(sun) > 0.95);
    }

    #[test]
    fn lambert_moon_phase_hits_full_quarter_and_new() {
        assert!((moon_illuminated_fraction(0.0) - 1.0).abs() < 1e-9);
        assert!((moon_illuminated_fraction(std::f64::consts::PI) - 0.0).abs() < 1e-9);
        assert!((moon_illuminated_fraction(std::f64::consts::FRAC_PI_2) - 0.5).abs() < 1e-9);
        let a = lunar_phase_angle(12.0, 12345);
        let b = lunar_phase_angle(12.0, 12345);
        let c = lunar_phase_angle(12.0, 99);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let wrap = lunar_phase_angle(24.0 * VISUAL_LUNAR_MONTH_DAYS, 0);
        assert!(wrap.abs() < 1e-9 || (wrap - std::f64::consts::TAU).abs() < 1e-9);
        let artifact_dir = std::path::Path::new("/opt/cursor/artifacts");
        if artifact_dir.is_dir() {
            let (inner, outer) = saturn_ring_radii(100.0);
            let _ = std::fs::write(
                artifact_dir.join("aether_celestial_sky.txt"),
                format!(
                    "saturn_req_km={}\nsaturn_c_inner_km={}\nsaturn_a_outer_km={}\ncassini_km={}\nring_inner_per_100={:.4}\nring_outer_per_100={:.4}\ncassini_norm={:.4}\nsynodic_month_days={}\nvisual_month_days={}\nmoon_b_semi_major={}\nkepler_period_ratio={:.6}\nkepler_mean_motion={:.6}\nlambert_full={:.3}\nlambert_quarter={:.3}\nlambert_new={:.3}\nphase_seed_12345_noon={:.6}\n",
                    SATURN_EQUATORIAL_RADIUS_KM,
                    SATURN_C_RING_INNER_KM,
                    SATURN_A_RING_OUTER_KM,
                    SATURN_CASSINI_DIVISION_KM,
                    inner / 100.0,
                    outer / 100.0,
                    cassini_division_norm(),
                    SYNODIC_MONTH_DAYS,
                    VISUAL_LUNAR_MONTH_DAYS,
                    MOON_B_SEMI_MAJOR,
                    kepler_period_ratio(MOON_B_SEMI_MAJOR, 1.0),
                    kepler_mean_motion_ratio(MOON_B_SEMI_MAJOR, 1.0),
                    moon_illuminated_fraction(0.0),
                    moon_illuminated_fraction(std::f64::consts::FRAC_PI_2),
                    moon_illuminated_fraction(std::f64::consts::PI),
                    lunar_phase_angle(12.0, 12345)
                ),
            );
        }
    }
}
