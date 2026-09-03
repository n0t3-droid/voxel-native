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

use crate::daynight::{day_factor, sunset_factor, sun_direction, WorldIntelRuntime};
use crate::settings::WorldSettings;

/// Render layer used exclusively by the sky pass. The world camera stays
/// on the default layer 0 and never sees these meshes; the sky camera
/// only sees these meshes and never sees the world.
pub const SKY_LAYER: usize = 1;

/// Distance from the camera at which the sun disc, moon disc and star
/// shell are placed. Far enough that parallax is invisible during
/// normal play, close enough that floating-point precision is fine.
const SKY_DISTANCE: f32 = 950.0;

/// Hero gas giant: parked upper-right of the default spawn look so it
/// reads as the painting's Saturn, not a distant marble.
const PLANET_DIR: Vec3 = Vec3::new(0.58, 0.34, -0.52);
const PLANET_DIST: f32 = SKY_DISTANCE * 0.58;

/// Stars should be a night thing. (1-day)^2 still left them glittering
/// through golden hour; ^4 kills them by 17:00 and keeps 21:30 bright.
fn star_night_factor(day: f32) -> f32 {
    (1.0 - day).powf(4.0)
}

/// Fixed bearing of the great cratered moon: high and to the left.
///
/// The sun sweeps the x/y plane with a constant +z lean, so parking the
/// moon on the -z side is what guarantees the two never come close
/// enough for the sun's bloom to wash the moon out.
const GREAT_MOON_DIR: Vec3 = Vec3::new(-0.52, 0.66, -0.54);

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
                (follow_and_animate_sky, follow_static_sky_bodies)
                    .chain()
                    .before(bevy::transform::TransformSystem::TransformPropagate),
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

/// Big cratered grey moon parked high on a fixed bearing. Unlike the two
/// orbiting moons this one never sets: in the key art it is the largest
/// thing in the sky after the ringed giant, and a landmark that vanishes
/// for half the day is not a landmark.
#[derive(Component)]
struct GreatMoon;

/// A celestial body that keeps a fixed bearing from the player instead
/// of orbiting. Carrying the bearing on the component lets one small
/// system place all of them, rather than growing another arm on the
/// mutually-exclusive `Without<..>` query chain below.
#[derive(Component)]
struct StaticSkyBody {
    dir: Vec3,
    distance: f32,
}

/// Broad band of light hugging the horizon.
///
/// `daynight.rs` drives a single flat `ClearColor` for the whole dome,
/// which is what keeps the fog and the sky matched — but a flat sky is
/// the one thing the key art never has. This dome adds a latitude
/// gradient on top: warm at dusk, violet at night, cool at noon,
/// fading to nothing well before the zenith. It blends additively, so
/// it can only brighten the existing gradient and can never introduce a
/// seam between the sky and the fogged horizon.
#[derive(Component)]
struct HorizonGlow;

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
    great_moon: Handle<StandardMaterial>,
    horizon: Handle<StandardMaterial>,
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
) {
    let sky_layer = RenderLayers::layer(SKY_LAYER);

    // Nebula resolution + star count scale with graphics tier so low-end
    // GPUs still get the look for a fraction of the fill cost.
    use crate::settings::GraphicsMode;
    let (nebula_res, star_count, planet_r, ring_inner, ring_outer, ring_segs) =
        match settings.graphics {
            GraphicsMode::Fast => (256u32, 1800usize, 150.0_f32, 220.0_f32, 380.0_f32, 96usize),
            GraphicsMode::Balanced => (512, 3200, 185.0, 270.0, 470.0, 128),
            GraphicsMode::High => (1024, 5200, 220.0, 320.0, 560.0, 192),
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
        // Bloom — sun disc only. Threshold is high enough that the
        // horizon rim and nebula stay out of the bloom buffer; Additive
        // OLD_SCHOOL at 0.6 was turning the whole lower sky milky.
        BloomSettings {
            composite_mode: BloomCompositeMode::Additive,
            intensity: 0.08,
            low_frequency_boost: 0.35,
            prefilter_settings: bevy::core_pipeline::bloom::BloomPrefilterSettings {
                // Horizon / nebula sit around 0.2–1.2; only the sun disc
                // (emissive 60) and the hottest stars should bloom. The
                // old 0.6 threshold let the additive horizon wall bloom
                // into milky white and crush the rest of the sky.
                threshold: 2.4,
                threshold_softness: 0.35,
            },
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
    let nebula_image = images.add(build_nebula_image(nebula_res, settings.seed as u64));
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
        // Modest start — follow_and_animate_sky owns the live value.
        // Too high here and the first frames bleach the sky to pink fog.
        emissive: LinearRgba::rgb(0.85, 0.55, 1.15),
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

    // ----- Great cratered moon ----------------------------------------
    // Fixed bearing, upper-left. Big enough to dominate that quadrant of
    // the sky without covering the play space at the horizon.
    let great_moon_image = images.add(build_moon_image(nebula_res.min(512), settings.seed as u64));
    let great_moon_mesh = meshes.add(
        Sphere::new(58.0)
            .mesh()
            .ico(4)
            .expect("subdivision 4 is within ico limits"),
    );
    let great_moon_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.86, 0.87, 0.92),
        base_color_texture: Some(great_moon_image.clone()),
        emissive_texture: Some(great_moon_image),
        emissive: LinearRgba::rgb(3.4, 3.5, 4.0),
        unlit: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: great_moon_mesh,
            material: great_moon_mat.clone(),
            transform: Transform::from_translation(GREAT_MOON_DIR * SKY_DISTANCE * 0.92),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        GreatMoon,
        StaticSkyBody {
            dir: GREAT_MOON_DIR.normalize(),
            distance: SKY_DISTANCE * 0.92,
        },
        Name::new("GreatMoon"),
    ));

    // ----- Horizon glow band ------------------------------------------
    // Drawn on a shell outside the nebula so transparency sorting puts
    // it behind the clouds, which is where a scattering band belongs.
    let horizon_image = images.add(build_horizon_gradient_image(128));
    let horizon_mesh = meshes.add(
        Sphere::new(SKY_DISTANCE * 3.4)
            .mesh()
            .ico(3)
            .expect("subdivision 3 is within ico limits"),
    );
    let horizon_mat = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(horizon_image.clone()),
        emissive_texture: Some(horizon_image),
        // Blend (not Add): a coloured rim over ClearColor, not extra
        // HDR energy that bloom turns into a milky white wall.
        emissive: LinearRgba::rgb(0.18, 0.10, 0.08),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: Some(bevy::render::render_resource::Face::Front),
        double_sided: true,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh: horizon_mesh,
            material: horizon_mat.clone(),
            ..default()
        },
        NotShadowCaster,
        sky_layer.clone(),
        HorizonGlow,
        StaticSkyBody {
            dir: Vec3::Y,
            distance: 0.0,
        },
        Name::new("HorizonGlow"),
    ));

    // ----- Ringed gas-giant planet ------------------------------------
    // Parked in a fixed sky direction; doesn't track the sun. Serves as
    // a dramatic backdrop feature like in reference image 2.
    let planet_res = nebula_res.min(512);
    let planet_image = images.add(build_planet_image(planet_res, settings.seed as u64));
    let planet_mesh = meshes.add(
        Sphere::new(planet_r)
            .mesh()
            .ico(4)
            .expect("subdivision 4 is within ico limits"),
    );
    let planet_mat = materials.add(StandardMaterial {
        // Banded gas giant: cream / tan / lavender, shaded rather than
        // a magenta emissive gizmo. The texture already carries a
        // terminator + limb darkening.
        base_color: Color::srgb(0.92, 0.86, 0.78),
        base_color_texture: Some(planet_image.clone()),
        emissive_texture: Some(planet_image),
        emissive: LinearRgba::rgb(1.85, 1.48, 1.12),
        unlit: true,
        ..default()
    });
    // Dusty Saturn-like rings with a Cassini-style gap. Vertex colours
    // carry the band albedo; keep emissive modest so they read as ice
    // and dust, not a rainbow debug disc.
    let ring_mesh = meshes.add(build_ring_mesh(ring_inner, ring_outer, ring_segs));
    let ring_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.92, 0.84, 0.70, 1.0),
        emissive: LinearRgba::rgb(1.85, 1.55, 1.12),
        unlit: true,
        cull_mode: None,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    // Fixed sky direction: upper-right, high enough to dominate the
    // horizon without blocking gameplay sight-lines.
    let planet_dir = PLANET_DIR.normalize();
    let planet_pos = planet_dir * PLANET_DIST;
    commands
        .spawn((
            PbrBundle {
                mesh: planet_mesh,
                material: planet_mat.clone(),
                transform: Transform::from_translation(planet_pos)
                    // Fixed tilt — ring plane tipped toward the viewer.
                    // Never rotates (planets are stationary landmarks).
                    .with_rotation(Quat::from_rotation_x(0.72) * Quat::from_rotation_z(-0.22)),
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
                    transform: Transform::from_rotation(Quat::from_rotation_x(
                        std::f32::consts::FRAC_PI_2,
                    )),
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
        great_moon: great_moon_mat,
        horizon: horizon_mat,
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
    main_cam: Query<&GlobalTransform, (With<Camera3d>, Without<SkyCamera>)>,
    mut sky_cam: Query<&mut Transform, With<SkyCamera>>,
    mut sun_q: Query<
        &mut Transform,
        (
            With<SunDisc>,
            Without<SkyCamera>,
            Without<MoonDisc>,
            Without<MoonDiscB>,
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

    // Same cinematic solar geometry as daynight.rs::update_sun.
    let sun_dir = sun_direction(settings.time_of_day);
    let t = (settings.time_of_day / 24.0) * std::f32::consts::TAU;

    if let Ok(mut sun_tf) = sun_q.get_single_mut() {
        sun_tf.translation = trans + sun_dir * SKY_DISTANCE;
    }
    if let Ok(mut moon_tf) = moon_q.get_single_mut() {
        moon_tf.translation = trans - sun_dir * SKY_DISTANCE;
    }
    if let Ok(mut moon_b_tf) = moon_b_q.get_single_mut() {
        // Second moon: 25° leading the main moon with a slight vertical
        // offset so the pair reads as a binary system.
        let lead = Quat::from_rotation_z(0.42) * Quat::from_rotation_y(0.18);
        let dir_b = (lead * -sun_dir).normalize();
        moon_b_tf.translation = trans + dir_b * SKY_DISTANCE * 0.92 + Vec3::new(0.0, 35.0, 0.0);
    }
    if let Ok(mut stars_tf) = stars_q.get_single_mut() {
        stars_tf.translation = trans;
        // Stars share the celestial rotation so they wheel across the
        // sky together with sun and moon.
        stars_tf.rotation = Quat::from_rotation_z(t);
    }
    if let Ok(mut planet_tf) = planet_q.get_single_mut() {
        // Fixed direction, NEVER rotates — stationary landmark.
        let planet_dir = PLANET_DIR.normalize();
        planet_tf.translation = trans + planet_dir * PLANET_DIST;
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
    let day = day_factor(sun_dir);
    let night = (1.0 - day).powf(2.0);
    let sunset = sunset_factor(sun_dir);

    if let Some(sky_mats) = sky_mats {
        // Sun: warm white at noon → fiery red-orange at sunset.
        if let Some(mat) = materials.get_mut(&sky_mats.sun) {
            let noon = Vec3::new(60.0, 50.0, 30.0);
            let dusk = Vec3::new(80.0, 22.0, 8.0);
            let e = noon.lerp(dusk, sunset);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Moon: cool blue, brightens slightly at night for a clearer disc.
        if let Some(mat) = materials.get_mut(&sky_mats.moon) {
            let base = Vec3::new(6.0, 7.0, 11.0);
            let scaled = base * (0.6 + 0.6 * night);
            mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
        }

        // Second moon — cool violet, slightly dimmer.
        if let Some(mat) = materials.get_mut(&sky_mats.moon_b) {
            let base = Vec3::new(4.0, 3.0, 8.0);
            let scaled = base * (0.55 + 0.55 * night);
            mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
        }

        // Ringed planet & rings — a shaded gas giant, not a neon gizmo.
        // Slight extra glow at night/sunset so bands stay readable
        // against the dark sky without blowing out at noon.
        let planet_scale = 1.05 + 0.22 * night + sunset * 0.12;
        if let Some(mat) = materials.get_mut(&sky_mats.planet) {
            let base = Vec3::new(2.15, 1.72, 1.28);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }
        if let Some(mat) = materials.get_mut(&sky_mats.ring) {
            let base = Vec3::new(2.05, 1.72, 1.22);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }
        if let Some(mat) = materials.get_mut(&sky_mats.planet_b) {
            let base = Vec3::new(0.85, 1.35, 1.70);
            let s = base * planet_scale;
            mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
        }

        // Nebula — cyan vs magenta filaments over a deep-violet zenith.
        // Keep day values structured (not a pale wash). Additive blend
        // plus a high emissive is what bleached noon into milky fog, so
        // the live multipliers stay well below the old 2–7 range.
        if let Some(mat) = materials.get_mut(&sky_mats.nebula) {
            // Day/dusk keep a whisper of structure; the volume only
            // really punches once the sun is down. Hour 17 used to keep
            // night-level filaments plus stars.
            let night_vol = (1.0 - day).powf(2.8);
            let base_day = Vec3::new(0.16, 0.08, 0.32);
            let base_night = Vec3::new(1.65, 0.92, 2.25);
            let base_sunset = Vec3::new(0.85, 0.28, 0.12);
            let e = (base_day * (0.04 + 0.08 * day)
                + base_night * night_vol
                + base_sunset * sunset * 0.22)
                * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Great moon: sunlit grey by day, cool silver at night. Never
        // fades out entirely - it is a permanent sky landmark.
        if let Some(mat) = materials.get_mut(&sky_mats.great_moon) {
            let lit = Vec3::new(4.4, 4.4, 4.6);
            let dark = Vec3::new(2.2, 2.4, 3.4);
            let e = dark.lerp(lit, day);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Horizon band: a thin golden rim at 17:00, a cool whisper at
        // noon. Additive + bloom made a fat dusk emissive bleach the
        // lower third; keep noon dim and let sunset_factor (now peaked
        // at hour 17) drive the warmth.
        if let Some(mat) = materials.get_mut(&sky_mats.horizon) {
            let noon = Vec3::new(0.010, 0.028, 0.052);
            let dusk = Vec3::new(1.35, 0.48, 0.08);
            let deep = Vec3::new(0.10, 0.04, 0.20);
            let e = noon * day * (1.0 - sunset) + deep * night + dusk * sunset;
            let e = e * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
        }

        // Stars: only after true dusk. Unlit + Additive still draws
        // `base_color` even when emissive is 0, which is why hour 11/17
        // stills glittered after the emissive-only fade.
        if let Some(mat) = materials.get_mut(&sky_mats.stars) {
            let star_night = star_night_factor(day);
            let intensity = 16.0 * star_night * intel.profile.sky_saturation.max(0.7);
            mat.base_color = Color::srgba(star_night, star_night, star_night, star_night);
            mat.emissive = LinearRgba::rgb(intensity, intensity, intensity * 1.15);
        }
    }
}

/// Park every fixed-bearing sky body relative to the player camera.
fn follow_static_sky_bodies(
    main_cam: Query<&GlobalTransform, (With<Camera3d>, Without<SkyCamera>)>,
    mut bodies: Query<(&mut Transform, &StaticSkyBody)>,
) {
    let Ok(main_tf) = main_cam.get_single() else {
        return;
    };
    let origin = main_tf.translation();
    for (mut tf, body) in bodies.iter_mut() {
        tf.translation = origin + body.dir * body.distance;
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

/// Build a flat annulus (ring) mesh for the gas-giant. Two-sided via
/// material `cull_mode = None`. `inner`/`outer` are world radii, `segs`
/// is the number of radial slices.
///
/// Colours follow dusty ice-and-dust rings (Saturn A/B/C, cream / tan /
/// grey) with a Cassini-style gap. Radial bands, not a hue sweep — the
/// old HSV rainbow read as a debug gizmo.
fn build_ring_mesh(inner: f32, outer: f32, segs: usize) -> Mesh {
    // Concentric bands so we can punch a real gap rather than dim a
    // rainbow. Saturn's Cassini Division sits at ~1.95–2.03 Rs
    // (NASA Saturn fact sheet); with inner≈1.52 Rs and outer≈2.6 Rs
    // that maps to roughly the middle of this span.
    const BANDS: usize = 16;
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity((BANDS + 1) * segs);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity((BANDS + 1) * segs);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity((BANDS + 1) * segs);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity((BANDS + 1) * segs);
    let mut indices: Vec<u32> = Vec::with_capacity(BANDS * segs * 6);

    // Dusty cream / tan / grey albedos. Not saturated primaries.
    // Approximate Cassini ISS natural-colour rings (ice + dust).
    let band_albedo = |t: f32| -> [f32; 4] {
        // Cassini Division: 0.48..0.58 of the span.
        if t > 0.48 && t < 0.58 {
            return [0.04, 0.035, 0.03, 0.0];
        }
        // C-like inner: dim grey-brown.
        if t < 0.16 {
            let k = t / 0.16;
            return [
                0.42 + 0.10 * k,
                0.36 + 0.10 * k,
                0.30 + 0.08 * k,
                0.28 + 0.22 * k,
            ];
        }
        // B ring: brightest cream/tan.
        if t < 0.48 {
            let k = (t - 0.16) / 0.32;
            let pulse = (k * 11.0).sin().abs() * 0.07;
            return [
                0.92 + pulse,
                0.80 + pulse * 0.7,
                0.60 + pulse * 0.4,
                0.95 - k * 0.05,
            ];
        }
        // A ring: dustier, slightly greyer, with Encke-like dips.
        let k = ((t - 0.58) / 0.42).clamp(0.0, 1.0);
        let dip = if (k - 0.55).abs() < 0.04 { 0.35 } else { 1.0 };
        [
            (0.82 - k * 0.10) * dip,
            (0.72 - k * 0.08) * dip,
            (0.58 - k * 0.06) * dip,
            (0.82 - k * 0.20) * dip,
        ]
    };

    for i in 0..=segs {
        let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
        let (sa, ca) = a.sin_cos();
        for b in 0..=BANDS {
            let t = b as f32 / BANDS as f32;
            let r = inner + (outer - inner) * t;
            positions.push([ca * r, 0.0, sa * r]);
            normals.push([0.0, 1.0, 0.0]);
            uvs.push([i as f32 / segs as f32, t]);
            colors.push(band_albedo(t));
        }
    }
    let stride = (BANDS + 1) as u32;
    for i in 0..segs {
        let a = (i as u32) * stride;
        let c = a + stride;
        for b in 0..BANDS as u32 {
            // Skip the Cassini gap entirely so there's a real hole.
            let t = (b as f32 + 0.5) / BANDS as f32;
            if t > 0.48 && t < 0.58 {
                continue;
            }
            indices.extend_from_slice(&[a + b, a + b + 1, c + b + 1, a + b, c + b + 1, c + b]);
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

/// Build the cratered surface of the great moon: broad maria picked out
/// by low-frequency noise, overlaid with a ring-shaped crater field from
/// sharpened ridge noise. Deterministic by seed.
fn build_moon_image(size: u32, seed: u64) -> Image {
    let maria = Perlin::new(seed as u32 ^ 0x4D_4F_4F_4E);
    let craters = Perlin::new(seed as u32 ^ 0x43_52_41_54);
    let dust = Perlin::new(seed as u32 ^ 0x44_55_53_54);

    let w = size;
    let h = size / 2;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            let px = cv * cu;
            let py = sv;
            let pz = cv * su;

            // Dark basaltic plains.
            let sea = maria.get([px * 1.5, py * 1.5, pz * 1.5]);
            // `1 - |n|` raised to a high power leaves only the thin
            // zero-crossing shells: a field of crater rims.
            let rim = (1.0 - craters.get([px * 7.0, py * 7.0, pz * 7.0]).abs()).powf(14.0);
            let rim2 = (1.0 - craters.get([px * 15.0, py * 15.0, pz * 15.0]).abs()).powf(20.0);
            let grain = dust.get([px * 40.0, py * 40.0, pz * 40.0]) * 0.05;

            let mut b = 0.78 + sea * 0.16 + grain;
            b -= rim * 0.30;
            b -= rim2 * 0.18;
            let b = (b.clamp(0.35, 1.0) * 255.0) as u8;
            // Very slightly warm in the highlands, cool in the maria.
            let tint = ((sea.max(0.0) * 8.0) as u8).min(10);
            data.push(b);
            data.push(b.saturating_sub(tint / 2));
            data.push(b.saturating_sub(tint));
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

/// Equirectangular banded gas-giant body. Cream / tan / lavender belts
/// with a baked terminator and cosine limb darkening so the unlit sky
/// pass still reads as a shaded sphere, not a flat magenta disc.
fn build_planet_image(size: u32, seed: u64) -> Image {
    let belts = Perlin::new(seed as u32 ^ 0x47_41_53_31);
    let swirl = Perlin::new(seed as u32 ^ 0x53_57_52_4C);
    let storm = Perlin::new(seed as u32 ^ 0x53_54_4F_52);

    let w = size;
    let h = size / 2;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    // Subsolar point facing the typical viewer-right sun in the key art.
    let light = Vec3::new(0.72, 0.18, 0.48).normalize();
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        let lat = (v / std::f64::consts::FRAC_PI_2) as f32;
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            let px = cv * cu;
            let py = sv;
            let pz = cv * su;
            let n = Vec3::new(px as f32, py as f32, pz as f32);

            let warp = swirl.get([px * 2.2, py * 6.0, pz * 2.2]) * 0.08;
            let band_n = belts.get([0.0, py * 7.5 + warp, 0.0]);
            let band = (lat * 6.5 + warp as f32).sin() * 0.5 + 0.5 + band_n as f32 * 0.18;
            let band = band.clamp(0.0, 1.0);
            let cream = Vec3::new(0.90, 0.78, 0.58);
            let tan = Vec3::new(0.72, 0.52, 0.34);
            let lavender = Vec3::new(0.62, 0.50, 0.70);
            let col = if band < 0.45 {
                cream.lerp(tan, band / 0.45)
            } else if band < 0.75 {
                tan.lerp(lavender, (band - 0.45) / 0.30)
            } else {
                lavender.lerp(cream, (band - 0.75) / 0.25)
            };
            let oval = storm.get([px * 4.0, py * 4.0 - 1.4, pz * 4.0]);
            let col = if py < -0.15 && oval > 0.55 {
                col.lerp(Vec3::new(0.95, 0.82, 0.70), ((oval - 0.55) / 0.45) as f32)
            } else {
                col
            };

            let ndl = n.dot(light).max(0.0);
            // Stronger limb darkening than pass 1 so the disc reads as a
            // sphere, not a flat sticker, even at the larger hero size.
            let limb = 0.10 + 0.90 * ndl.powf(1.25);
            let night = 0.06 + 0.12 * (1.0 - ndl);
            let shade = if ndl > 0.02 { limb } else { night };
            let col = col * shade;

            data.push((col.x.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((col.y.clamp(0.0, 1.0) * 255.0) as u8);
            data.push((col.z.clamp(0.0, 1.0) * 255.0) as u8);
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

/// Build the horizon-glow gradient.
///
/// Deliberately a function of latitude only. Bevy's icosphere UVs are
/// good enough for billowing nebula clouds but not accurate enough in
/// longitude to aim a directional lobe, and a band only needs the
/// vertical mapping to be monotonic. Peaks on the horizon line, dies out
/// by roughly 40 degrees of elevation, and is black below the ground so
/// it never brightens the underside of the world.
fn build_horizon_gradient_image(height: u32) -> Image {
    const WIDTH: u32 = 4;
    let mut data = Vec::with_capacity((WIDTH * height * 4) as usize);
    for y in 0..height {
        // Latitude in [-PI/2, PI/2]; 0 is the horizon.
        let lat = (y as f32 / height as f32) * std::f32::consts::PI - std::f32::consts::FRAC_PI_2;
        let elevation = lat / std::f32::consts::FRAC_PI_2; // -1 below, +1 zenith
        let intensity = if elevation < 0.0 {
            // Fade out fast below the horizon line.
            (1.0 + elevation * 5.0).max(0.0)
        } else {
            // Smooth falloff to nothing well before the zenith — a thin
            // rim, not a milky wall climbing the sky.
            (1.0 - (elevation / 0.28).min(1.0)).powf(1.85)
        };
        let intensity = intensity.clamp(0.0, 1.0);
        // Greyscale carrier — live emissive supplies the colour so noon
        // stays cool instead of inheriting a baked dusk-orange rim.
        let byte = (intensity * 255.0) as u8;
        let a = (intensity * 0.50 * 255.0) as u8;
        for _ in 0..WIDTH {
            data.push(byte);
            data.push(byte);
            data.push(byte);
            data.push(a);
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: WIDTH,
            height,
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

    /// Read the red channel of the single-column gradient at row `y`.
    fn gradient_row(image: &Image, y: u32) -> u8 {
        let width = image.texture_descriptor.size.width;
        image.data[((y * width) * 4) as usize]
    }

    #[test]
    fn horizon_glow_peaks_on_the_horizon_and_dies_before_the_zenith() {
        const H: u32 = 128;
        let image = build_horizon_gradient_image(H);
        let horizon = H / 2;

        // Brightest at the horizon line.
        assert!(gradient_row(&image, horizon) > 180);
        // Gone below the ground, so the band never lights the underside
        // of the world or fights the terrain fog.
        assert_eq!(gradient_row(&image, 0), 0);
        // Gone at the zenith, so the deep indigo overhead stays deep.
        assert_eq!(gradient_row(&image, H - 1), 0);
        // Monotonic decay upward from the horizon.
        let mut previous = gradient_row(&image, horizon);
        for y in (horizon + 1)..H {
            let current = gradient_row(&image, y);
            assert!(
                current <= previous,
                "horizon band brightens again at row {y}: {previous} -> {current}"
            );
            previous = current;
        }
    }

    #[test]
    fn great_moon_surface_has_dark_maria_and_bright_highlands() {
        let image = build_moon_image(128, 4242);
        let mut min = u8::MAX;
        let mut max = u8::MIN;
        for pixel in image.data.chunks_exact(4) {
            min = min.min(pixel[0]);
            max = max.max(pixel[0]);
        }
        assert!(
            max - min > 60,
            "moon surface only spans {} levels; it would read as a flat disc",
            max - min
        );
    }

    #[test]
    fn great_moon_keeps_clear_of_the_suns_arc() {
        // The cinematic sun still sweeps a +z-leaning arc. If the great
        // moon sat on that arc it would pass behind the sun and get
        // washed out by the bloom for part of every day.
        let moon = GREAT_MOON_DIR.normalize();
        let mut closest = f32::MAX;
        for step in 0..48 {
            let hour = step as f32 * 0.5;
            let sun = sun_direction(hour);
            closest = closest.min(moon.angle_between(sun));
        }
        assert!(
            closest.to_degrees() > 25.0,
            "great moon passes within {:.1} degrees of the sun",
            closest.to_degrees()
        );
    }

    #[test]
    fn ring_mesh_is_dusty_with_a_cassini_gap() {
        let mesh = build_ring_mesh(132.0, 232.0, 48);
        let Some(bevy::render::mesh::VertexAttributeValues::Float32x4(cols)) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("ring mesh is missing vertex colours");
        };
        let mut gap = 0usize;
        let mut dusty = 0usize;
        let mut rainbow = 0usize;
        for c in cols {
            let [r, g, b, a] = *c;
            if a < 0.05 {
                gap += 1;
                continue;
            }
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            if max > 0.85 && min < 0.25 {
                rainbow += 1;
            }
            if r > 0.30 && g > 0.25 && b > 0.18 && (r - b).abs() < 0.45 {
                dusty += 1;
            }
        }
        assert!(gap > 10, "Cassini gap never punched (only {gap} empty verts)");
        assert!(
            dusty > rainbow * 3,
            "rings still look like a rainbow gizmo (dusty={dusty} rainbow={rainbow})"
        );
    }

    #[test]
    fn gas_giant_has_banded_body_and_a_dark_limb() {
        let image = build_planet_image(128, 4242);
        let mut min = u8::MAX;
        let mut max = u8::MIN;
        for pixel in image.data.chunks_exact(4) {
            let y = pixel[0] / 3 + pixel[1] / 2 + pixel[2] / 6;
            min = min.min(y);
            max = max.max(y);
        }
        assert!(
            max - min > 50,
            "planet body only spans {} levels; it would read as a flat disc",
            max - min
        );
    }

    #[test]
    fn stars_are_gone_at_golden_hour_and_present_at_night() {
        let dusk = star_night_factor(day_factor(sun_direction(17.0)));
        let noon = star_night_factor(day_factor(sun_direction(11.0)));
        let night = star_night_factor(day_factor(sun_direction(21.5)));
        assert!(
            dusk < 0.08,
            "hour 17 still has star factor {dusk}; stars would glitter through dusk"
        );
        assert!(noon < 0.01);
        assert!(night > 0.55, "hour 21.5 star factor is only {night}");
    }
}

/// Build a procedural nebula image — swirled multi-octave Perlin on a
/// spherical projection. Magenta and cyan are kept in opposition (not
/// mixed into mud), the zenith stays deep violet, and the horizon
/// picks up a warm burn so dusk doesn't flatten the dome.
fn build_nebula_image(size: u32, seed: u64) -> Image {
    let n_fil = Perlin::new(seed as u32 ^ 0x7777_7777);
    let n_pick = Perlin::new(seed as u32 ^ 0x3333_3333);
    let n_mask = Perlin::new(seed as u32 ^ 0x1234_5678);
    let n_swirl = Perlin::new(seed as u32 ^ 0x51_57_52_4C);
    let w = size;
    let h = size / 2;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        let zenith = sv.max(0.0); // 1 at +Y
        let horizon = 1.0 - sv.abs();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            // Swirl: rotate longitude by a latitude-dependent twist so
            // the clouds read as a volume, not a painted sheet.
            let twist = 0.85 * (v * 2.2).sin() + n_swirl.get([v * 1.3, 0.2]) * 0.55;
            let u_s = u + twist;
            let (su, cu) = u_s.sin_cos();
            let px = cv * cu;
            let py = sv;
            let pz = cv * su;

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
            let filament = (fbm(&n_fil, 1.4, 5) * 0.5 + 0.5).powf(1.35);
            let picker = fbm(&n_pick, 0.7, 3);
            let mask = (fbm(&n_mask, 0.55, 3) + 0.25).max(0.0).powf(1.7);
            let amp = (filament * mask).clamp(0.0, 1.0);

            // Magenta vs cyan: pick a side, don't average them into purple mud.
            let magenta = Vec3::new(1.00, 0.18, 0.82);
            let cyan = Vec3::new(0.12, 0.85, 1.00);
            let mix = ((picker + 0.15) * 1.6).clamp(0.0, 1.0) as f32;
            let mut col = cyan.lerp(magenta, mix) * amp as f32;

            // Deep-violet zenith fill so the dome never goes empty-black
            // or pale-fog, plus a warm horizon burn for golden hour.
            let violet = Vec3::new(0.16, 0.04, 0.38) * (0.18 + 0.55 * zenith as f32);
            let warm = Vec3::new(0.95, 0.38, 0.12) * (horizon as f32).powf(2.2) * 0.32;
            col = col + violet * (0.35 + 0.25 * (1.0 - amp as f32)) + warm;
            col = col.min(Vec3::splat(1.0));
            // Put density in alpha so Additive empty sky does not wash
            // the ClearColor to milky pink. Filaments stay punchy.
            let alpha = (amp * 0.90 + 0.06 * zenith + 0.10 * horizon.powf(1.6)).clamp(0.0, 1.0);

            data.push((col.x * 255.0) as u8);
            data.push((col.y * 255.0) as u8);
            data.push((col.z * 255.0) as u8);
            data.push((alpha * 255.0) as u8);
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
