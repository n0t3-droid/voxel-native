//! Background sky pass with a graphics-tier-scaled procedural star field.
//! Reachable sun, moon and planets are rendered by `celestial.rs`; the
//! legacy showcase bodies in this module are disabled by default.
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
//! Both cameras run HDR + ACES tonemapping. Balanced and High add
//! [`BloomSettings`] to the sky camera; Fast omits that pass. Stars use
//! a single procedural mesh with a tier-scaled count on a sphere and
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
use crate::player::Player;
use crate::settings::{GraphicsMode, WorldSettings};

/// Render layer used exclusively by the sky pass. The world camera stays
/// on the default layer 0 and never sees these meshes; the sky camera
/// only sees these meshes and never sees the world.
pub const SKY_LAYER: usize = 1;

/// Radius of the star shell and optional legacy showcase bodies. Far
/// enough that parallax is invisible during normal play, close enough
/// that floating-point precision is fine.
const SKY_DISTANCE: f32 = 950.0;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_sky)
            // PostUpdate, BEFORE transform propagation. Both camera
            // transforms are read/written in the same frame so the sky
            // never trails mouse-look by one transform propagation.
            .add_systems(
                PostUpdate,
                follow_and_animate_sky.before(bevy::transform::TransformSystem::TransformPropagate),
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
    sun: Option<Handle<StandardMaterial>>,
    moon: Option<Handle<StandardMaterial>>,
    moon_b: Option<Handle<StandardMaterial>>,
    planet: Option<Handle<StandardMaterial>>,
    ring: Option<Handle<StandardMaterial>>,
    planet_b: Option<Handle<StandardMaterial>>,
    stars: Handle<StandardMaterial>,
    nebula: Option<Handle<StandardMaterial>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SkyShowcasePolicy {
    classic_sun_moon: bool,
    nebula: bool,
    second_moon: bool,
    ringed_planet: bool,
    second_planet: bool,
}

fn default_sky_showcase_policy() -> SkyShowcasePolicy {
    SkyShowcasePolicy::default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SkyAssetPolicy {
    star_count: usize,
    bloom: bool,
    classic_sun_moon: bool,
    nebula_resolution: Option<u32>,
    second_moon: bool,
    ringed_planet: bool,
    second_planet: bool,
}

fn sky_asset_policy(graphics: GraphicsMode, showcase: SkyShowcasePolicy) -> SkyAssetPolicy {
    let (nebula_resolution, star_count, bloom) = match graphics {
        GraphicsMode::Fast => (256, 1800, false),
        GraphicsMode::Balanced => (512, 3200, true),
        GraphicsMode::High => (1024, 5200, true),
    };

    SkyAssetPolicy {
        star_count,
        bloom,
        classic_sun_moon: showcase.classic_sun_moon,
        nebula_resolution: showcase.nebula.then_some(nebula_resolution),
        second_moon: showcase.second_moon,
        ringed_planet: showcase.ringed_planet,
        second_planet: showcase.second_planet,
    }
}

fn setup_sky(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    settings: Res<WorldSettings>,
) {
    let sky_layer = RenderLayers::layer(SKY_LAYER);

    let asset_policy = sky_asset_policy(settings.graphics, default_sky_showcase_policy());

    // ----- Sky camera --------------------------------------------------
    // order = -1 → renders BEFORE the world camera in `player.rs` and
    // clears the framebuffer with the global `ClearColor` (which the
    // existing daynight system already animates between sky/sunset/night
    // colours). The world camera then composites on top with
    // `ClearColorConfig::None`.
    let mut sky_camera = commands.spawn((
        Camera3dBundle {
            camera: Camera {
                order: -1,
                hdr: true,
                ..default()
            },
            tonemapping: Tonemapping::AcesFitted,
            transform: Transform::IDENTITY,
            // Fallback only: the player projection is mirrored in
            // PostUpdate before this camera renders.
            projection: Projection::Perspective(PerspectiveProjection {
                fov: 80.0f32.to_radians(),
                near: 1.0,
                far: SKY_DISTANCE * 8.0,
                ..default()
            }),
            ..default()
        },
        sky_layer.clone(),
        SkyCamera,
        Name::new("SkyCamera"),
    ));
    if asset_policy.bloom {
        // Fast omits the component entirely so Bevy can skip the bloom
        // sub-pipeline. Higher tiers use it for the brightest stars.
        sky_camera.insert(BloomSettings {
            composite_mode: BloomCompositeMode::Additive,
            ..BloomSettings::OLD_SCHOOL
        });
    }

    let (sun_mat, moon_mat) = if asset_policy.classic_sun_moon {
        // Legacy sky discs remain available for showcase builds, but the
        // default uses the reachable bodies owned by `celestial.rs`.
        let sun_mesh = meshes.add(
            Sphere::new(28.0)
                .mesh()
                .ico(3)
                .expect("subdivision 3 is within ico limits"),
        );
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
        (Some(sun_mat), Some(moon_mat))
    } else {
        (None, None)
    };

    // ----- Star field --------------------------------------------------
    // Dense, colour-varied star shell. Count scales with graphics tier.
    // Colours follow a simplified stellar classification: mostly cool
    // white, with blue giants, yellow/orange main-sequence, and a few
    // red giants. The bloom pass on the sky cam turns the brightest
    // ones into genuine twinkling haloes.
    let stars_mesh = meshes.add(build_star_mesh(
        asset_policy.star_count,
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

    let nebula_mat = if let Some(nebula_resolution) = asset_policy.nebula_resolution {
        let nebula_image = images.add(build_nebula_image(nebula_resolution, settings.seed as u64));
        let nebula_mesh = meshes.add(
            Sphere::new(SKY_DISTANCE * 2.6)
                .mesh()
                .ico(4)
                .expect("subdivision 4 is within ico limits"),
        );
        let nebula_mat = materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, 1.0),
            base_color_texture: Some(nebula_image.clone()),
            emissive_texture: Some(nebula_image),
            emissive: LinearRgba::rgb(2.0, 1.6, 2.6),
            unlit: true,
            alpha_mode: AlphaMode::Add,
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
        Some(nebula_mat)
    } else {
        None
    };

    // ----- Second (smaller) moon --------------------------------------
    // Slightly offset from the main moon to create the paired-crescent
    // look in the reference art.
    let moon_b_mat = if asset_policy.second_moon {
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
        Some(moon_b_mat)
    } else {
        None
    };

    let (planet_mat, ring_mat) = if asset_policy.ringed_planet {
        let planet_mesh = meshes.add(
            Sphere::new(78.0)
                .mesh()
                .ico(4)
                .expect("subdivision 4 is within ico limits"),
        );
        let planet_mat = materials.add(StandardMaterial {
            base_color: Color::srgb(0.95, 0.55, 1.0),
            emissive: LinearRgba::rgb(6.0, 2.2, 8.5),
            unlit: true,
            ..default()
        });
        let ring_mesh = meshes.add(build_ring_mesh(160.0, 270.0, 160));
        let ring_mat = materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.9, 0.8, 1.0),
            emissive: LinearRgba::rgb(5.5, 4.5, 6.5),
            unlit: true,
            cull_mode: None,
            alpha_mode: AlphaMode::Blend,
            ..default()
        });
        let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
        let planet_pos = planet_dir * SKY_DISTANCE * 0.9;
        commands
            .spawn((
                PbrBundle {
                    mesh: planet_mesh,
                    material: planet_mat.clone(),
                    transform: Transform::from_translation(planet_pos)
                        // Fixed tilt — ring plane tipped toward the viewer.
                        // Never rotates (planets are stationary landmarks).
                        .with_rotation(Quat::from_rotation_x(0.55) * Quat::from_rotation_z(-0.18)),
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
        (Some(planet_mat), Some(ring_mat))
    } else {
        (None, None)
    };

    // ----- Second planet (ice-teal gas giant) -------------------------
    // Parked low on the opposite horizon. Smaller, cooler-coloured,
    // no rings — complements the main giant for a "binary system" feel.
    let planet_b_mat = if asset_policy.second_planet {
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
        Some(planet_b_mat)
    } else {
        None
    };

    commands.insert_resource(SkyMaterials {
        sun: sun_mat,
        moon: moon_mat,
        moon_b: moon_b_mat,
        planet: planet_mat,
        ring: ring_mat,
        planet_b: planet_b_mat,
        stars: stars_mat,
        nebula: nebula_mat,
    });
}

/// Mirror the player camera, rotate the star shell, and update the
/// optional legacy showcase entities and emissives by day factor.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn follow_and_animate_sky(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    main_cam: Query<(&Transform, &Projection), (With<Camera3d>, With<Player>)>,
    mut sky_cam: Query<(&mut Transform, &mut Projection), (With<SkyCamera>, Without<Player>)>,
    mut sun_q: Query<
        &mut Transform,
        (
            With<SunDisc>,
            Without<SkyCamera>,
            Without<Player>,
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
            Without<Player>,
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
            Without<Player>,
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
            Without<Player>,
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
            Without<Player>,
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
            Without<Player>,
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
            Without<Player>,
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
    let Ok((main_tf, main_projection)) = main_cam.get_single() else {
        return;
    };
    let Ok((mut sky_tf, mut sky_projection)) = sky_cam.get_single_mut() else {
        return;
    };

    mirror_sky_camera(main_tf, main_projection, &mut sky_tf, &mut sky_projection);
    let trans = main_tf.translation;

    // Same celestial-angle math as daynight.rs::update_sun. Keep these
    // formulas in sync; they share the same `time_of_day` resource.
    let t = (settings.time_of_day.rem_euclid(24.0) / 24.0) * std::f32::consts::TAU
        - std::f32::consts::FRAC_PI_2;
    let sun_dir = crate::daynight::sun_direction_for_time(settings.time_of_day);

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
        if let Some(handle) = &sky_mats.sun {
            if let Some(mat) = materials.get_mut(handle) {
                let noon = Vec3::new(60.0, 50.0, 30.0);
                let dusk = Vec3::new(80.0, 22.0, 8.0);
                let e = noon.lerp(dusk, sunset);
                mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
            }
        }

        // Moon: cool blue, brightens slightly at night for a clearer disc.
        if let Some(handle) = &sky_mats.moon {
            if let Some(mat) = materials.get_mut(handle) {
                let base = Vec3::new(6.0, 7.0, 11.0);
                let scaled = base * (0.6 + 0.6 * night);
                mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
            }
        }

        // Second moon — cool violet, slightly dimmer.
        if let Some(handle) = &sky_mats.moon_b {
            if let Some(mat) = materials.get_mut(handle) {
                let base = Vec3::new(4.0, 3.0, 8.0);
                let scaled = base * (0.55 + 0.55 * night);
                mat.emissive = LinearRgba::rgb(scaled.x, scaled.y, scaled.z);
            }
        }

        // Ringed planet & rings — brightly emissive at all times so the
        // magenta disc and rainbow rings stay breathtaking at noon too,
        // just like in the reference art. Slight extra glow at
        // night/sunset for the cinematic payoff.
        let planet_scale = 1.8 + 0.8 * night + sunset * 0.5;
        if let Some(handle) = &sky_mats.planet {
            if let Some(mat) = materials.get_mut(handle) {
                let base = Vec3::new(8.0, 3.0, 11.0);
                let s = base * planet_scale;
                mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
            }
        }
        if let Some(handle) = &sky_mats.ring {
            if let Some(mat) = materials.get_mut(handle) {
                let base = Vec3::new(7.0, 6.0, 8.5);
                let s = base * planet_scale;
                mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
            }
        }
        if let Some(handle) = &sky_mats.planet_b {
            if let Some(mat) = materials.get_mut(handle) {
                let base = Vec3::new(3.5, 7.5, 10.0);
                let s = base * planet_scale;
                mat.emissive = LinearRgba::rgb(s.x, s.y, s.z);
            }
        }

        // Nebula — vivid magenta/cyan/orange at all times (additive
        // blend paints clouds on top of the sky gradient). Day values
        // are pushed HARD so the cosmic backdrop reads clearly even
        // against the bright blue noon sky, just like in the reference
        // art where planets and nebulae are visible in broad daylight.
        if let Some(handle) = &sky_mats.nebula {
            if let Some(mat) = materials.get_mut(handle) {
                let base_day = Vec3::new(9.0, 5.0, 13.0); // rich purple/magenta at noon
                let base_night = Vec3::new(8.0, 5.5, 10.0); // full nebula glow at night
                let base_sunset = Vec3::new(12.0, 5.0, 4.5); // warm dusk glow
                let e = (base_day * day + base_night * night + base_sunset * sunset * 0.9)
                    * intel.profile.sky_saturation.max(0.7);
                mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
            }
        }

        // Stars: fade in linearly with night.
        if let Some(mat) = materials.get_mut(&sky_mats.stars) {
            let intensity = 14.0 * night * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(intensity, intensity, intensity * 1.15);
        }
    }
}

fn mirror_sky_camera(
    main_transform: &Transform,
    main_projection: &Projection,
    sky_transform: &mut Transform,
    sky_projection: &mut Projection,
) {
    sky_transform.translation = main_transform.translation;
    sky_transform.rotation = main_transform.rotation;
    sky_transform.scale = Vec3::ONE;

    sync_sky_projection(main_projection, sky_projection);
}

fn sync_sky_projection(main_projection: &Projection, sky_projection: &mut Projection) {
    if let (Projection::Perspective(main), Projection::Perspective(sky)) =
        (main_projection, sky_projection)
    {
        sky.fov = main.fov;
        sky.aspect_ratio = main.aspect_ratio;
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
fn build_ring_mesh(inner: f32, outer: f32, segs: usize) -> Mesh {
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(segs * 2);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(segs * 2);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(segs * 2);
    let mut colors: Vec<[f32; 4]> = Vec::with_capacity(segs * 2);
    let mut indices: Vec<u32> = Vec::with_capacity(segs * 6);

    for i in 0..segs {
        let a = (i as f32 / segs as f32) * std::f32::consts::TAU;
        let (sa, ca) = a.sin_cos();
        positions.push([ca * inner, 0.0, sa * inner]);
        positions.push([ca * outer, 0.0, sa * outer]);
        normals.push([0.0, 1.0, 0.0]);
        normals.push([0.0, 1.0, 0.0]);
        let u = i as f32 / segs as f32;
        uvs.push([u, 0.0]);
        uvs.push([u, 1.0]);
        // Rainbow ring: hue sweeps across the annulus radius so the
        // disc reads as a prismatic Saturn-meets-nebula band. Density
        // bands modulate alpha to give the classic Cassini-gap feel.
        // Colour = HSV-ish rotation through magenta → teal → amber.
        let hue = (u * 3.0).fract();
        let (r, g, b) = if hue < 0.333 {
            let k = hue / 0.333;
            (1.0, 0.45 + 0.5 * k, 0.95 - 0.7 * k)
        } else if hue < 0.666 {
            let k = (hue - 0.333) / 0.333;
            (1.0 - 0.7 * k, 0.95 - 0.2 * k, 0.25 + 0.65 * k)
        } else {
            let k = (hue - 0.666) / 0.334;
            (0.3 + 0.7 * k, 0.75 - 0.25 * k, 0.9 - 0.6 * k)
        };
        // Alternating density bands (bright/dim/dark gap).
        let band = ((i / 4) % 4) as f32;
        let density = match band as i32 {
            0 => 1.0,
            1 => 0.85,
            2 => 0.45,
            _ => 0.75,
        };
        colors.push([r * density, g * density, b * density, 0.95 * density]);
        colors.push([
            r * density * 0.85,
            g * density * 0.85,
            b * density * 0.85,
            0.55 * density,
        ]);
    }
    for i in 0..segs {
        let a = (i * 2) as u32;
        let b = (i * 2 + 1) as u32;
        let c = (((i + 1) % segs) * 2) as u32;
        let d = (((i + 1) % segs) * 2 + 1) as u32;
        indices.extend_from_slice(&[a, b, d, a, d, c]);
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

/// Build a procedural nebula image — multi-octave 3D Perlin on a
/// spherical projection, three colour channels sampled at different
/// frequencies. Produces billowing magenta / cyan / orange clouds
/// reminiscent of Hubble field backdrops. Deterministic by seed.
fn build_nebula_image(size: u32, seed: u64) -> Image {
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
            // real nebulae.
            let amp = (mask + 0.2).max(0.0).powf(1.6);
            let rr = ((r * 0.5 + 0.5) * amp).clamp(0.0, 1.0);
            let gg = ((g * 0.5 + 0.5) * amp * 0.85).clamp(0.0, 1.0);
            let bb = ((b * 0.5 + 0.5) * amp * 1.05).clamp(0.0, 1.0);

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
    fn default_showcase_policy_disables_duplicate_celestials() {
        let policy = default_sky_showcase_policy();

        assert!(!policy.classic_sun_moon);
        assert!(!policy.nebula);
        assert!(!policy.second_moon);
        assert!(!policy.ringed_planet);
        assert!(!policy.second_planet);
    }

    #[test]
    fn asset_policy_skips_default_showcase_allocations() {
        for graphics in [
            GraphicsMode::Fast,
            GraphicsMode::Balanced,
            GraphicsMode::High,
        ] {
            let policy = sky_asset_policy(graphics, default_sky_showcase_policy());

            assert!(!policy.classic_sun_moon);
            assert_eq!(policy.nebula_resolution, None);
            assert!(!policy.second_moon);
            assert!(!policy.ringed_planet);
            assert!(!policy.second_planet);
        }
    }

    #[test]
    fn asset_policy_scales_stars_and_gates_fast_bloom() {
        let fast = sky_asset_policy(GraphicsMode::Fast, default_sky_showcase_policy());
        let balanced = sky_asset_policy(GraphicsMode::Balanced, default_sky_showcase_policy());
        let high = sky_asset_policy(GraphicsMode::High, default_sky_showcase_policy());

        assert_eq!(fast.star_count, 1800);
        assert_eq!(balanced.star_count, 3200);
        assert_eq!(high.star_count, 5200);
        assert!(!fast.bloom);
        assert!(balanced.bloom);
        assert!(high.bloom);
    }

    #[test]
    fn sky_camera_mirrors_player_pose_in_the_same_frame() {
        let main_transform = Transform::from_translation(Vec3::new(12.0, 42.0, -7.0))
            .with_rotation(Quat::from_rotation_y(0.8))
            .with_scale(Vec3::splat(2.0));
        let main_projection = Projection::Perspective(PerspectiveProjection {
            fov: 61.0_f32.to_radians(),
            aspect_ratio: 21.0 / 9.0,
            ..default()
        });
        let mut sky_transform = Transform::IDENTITY;
        let mut sky_projection = Projection::Perspective(PerspectiveProjection {
            fov: 80.0_f32.to_radians(),
            aspect_ratio: 1.0,
            near: 1.0,
            far: SKY_DISTANCE * 8.0,
        });

        mirror_sky_camera(
            &main_transform,
            &main_projection,
            &mut sky_transform,
            &mut sky_projection,
        );

        assert_eq!(sky_transform.translation, main_transform.translation);
        assert_eq!(sky_transform.rotation, main_transform.rotation);
        assert_eq!(sky_transform.scale, Vec3::ONE);
    }

    #[test]
    fn projection_sync_copies_fov_and_aspect_but_preserves_sky_clip_planes() {
        let main_projection = Projection::Perspective(PerspectiveProjection {
            fov: 61.0_f32.to_radians(),
            aspect_ratio: 21.0 / 9.0,
            near: 0.1,
            far: 80_000.0,
        });
        let mut sky_projection = Projection::Perspective(PerspectiveProjection {
            fov: 80.0_f32.to_radians(),
            aspect_ratio: 1.0,
            near: 1.0,
            far: SKY_DISTANCE * 8.0,
        });

        sync_sky_projection(&main_projection, &mut sky_projection);

        let Projection::Perspective(sky) = sky_projection else {
            panic!("sky camera should stay perspective");
        };
        assert!((sky.fov - 61.0_f32.to_radians()).abs() < 1.0e-6);
        assert!((sky.aspect_ratio - 21.0 / 9.0).abs() < 1.0e-6);
        assert_eq!(sky.near, 1.0);
        assert_eq!(sky.far, SKY_DISTANCE * 8.0);
    }

    #[test]
    fn sky_follow_system_initializes_without_conflicting_camera_queries() {
        let mut app = App::new();
        app.insert_resource(WorldSettings::default())
            .insert_resource(WorldIntelRuntime::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .add_systems(Update, follow_and_animate_sky);

        // Bevy validates mutable query disjointness when the system first
        // initializes. This guards the real executable startup path, which a
        // pure projection helper test cannot cover.
        app.update();
    }
}
