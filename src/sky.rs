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
use crate::settings::{GraphicsMode, WorldProfile, WorldSettings};

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

    // Allocate the single Astral nebula shell once, then profile-gate its
    // visibility. World selection happens after Startup, so conditional
    // allocation here would make a newly opened Astral world inherit the
    // previous menu/profile state. Duplicate sun/moon/planet assets remain
    // disabled; the reachable bodies in `celestial.rs` keep ownership.
    let mut allocated_showcase = default_sky_showcase_policy();
    allocated_showcase.nebula = true;
    let asset_policy = sky_asset_policy(settings.graphics, allocated_showcase);

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
        // Additive source alpha starts at zero so the unlit vertex albedo is
        // genuinely invisible by day. Animating emissive alone was not
        // enough: the white base still rendered thousands of square specks
        // against the noon sky.
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.0),
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
        let nebula_image = images.add(build_nebula_image(nebula_resolution, 0xA57A_2026_DA7Au64));
        // Equirectangular cloud maps need duplicated seam vertices. An
        // icosphere shares its seam vertices and interpolates U from nearly
        // one back to zero across a triangle, which produced the giant
        // chevrons visible in Astral QA. A modest UV sphere fixes that at one
        // draw call and about 2k vertices; the sky silhouette never exposes
        // its regular pole topology.
        let nebula_mesh = meshes.add(Sphere::new(SKY_DISTANCE * 2.6).mesh().uv(64, 32));
        let nebula_mat = materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, 0.96),
            base_color_texture: Some(nebula_image.clone()),
            emissive_texture: Some(nebula_image),
            emissive: LinearRgba::rgb(2.0, 1.6, 2.6),
            unlit: true,
            // Blend gives the painted shell real large-scale colour mass.
            // Additive-only rendering disappeared into the bright atmospheric
            // clear colour and left Astral daylight almost flat blue in QA.
            alpha_mode: AlphaMode::Blend,
            cull_mode: Some(bevy::render::render_resource::Face::Front),
            double_sided: true,
            ..default()
        });
        commands.spawn((
            PbrBundle {
                mesh: nebula_mesh,
                material: nebula_mat.clone(),
                // Bevy's UV sphere is Z-up. Rotate its texture latitude onto
                // the world's Y-up sky before the camera-follow system moves
                // it; this also keeps the authored rose horizon horizontal.
                transform: Transform::from_rotation(Quat::from_rotation_x(
                    -std::f32::consts::FRAC_PI_2,
                )),
                visibility: nebula_visibility(settings.effective_world_profile()),
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
        (&mut Transform, &mut Visibility),
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
    if let Ok((mut nebula_tf, mut visibility)) = nebula_q.get_single_mut() {
        // Stationary nebula backdrop — the painterly clouds should
        // feel like a cosmic painting fixed behind us, not a slow
        // carousel. No rotation.
        nebula_tf.translation = trans;
        *visibility = nebula_visibility(settings.effective_world_profile());
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

        // Nebula: additive colour behind the terrain, visible in daylight but
        // deliberately restrained so it supports rather than flattens depth.
        if let Some(handle) = &sky_mats.nebula {
            if let Some(mat) = materials.get_mut(handle) {
                // Daylight stays pastel and subordinate to terrain
                // silhouettes; night opens the colour range without turning
                // the whole frame into emissive magenta fog.
                let base_day = Vec3::new(1.55, 0.92, 2.05);
                let base_night = Vec3::new(2.5, 1.5, 3.6);
                let base_sunset = Vec3::new(2.2, 0.9, 1.15);
                let e = (base_day * day + base_night * night + base_sunset * sunset * 0.9)
                    * intel.profile.sky_saturation.max(0.7);
                mat.emissive = LinearRgba::rgb(e.x, e.y, e.z);
            }
        }

        // Stars: suppress the unlit base as well as emissive light during
        // daylight, then fade both through astronomical twilight.
        if let Some(mat) = materials.get_mut(&sky_mats.stars) {
            let night_visibility = star_visibility_for_sun_elevation(sun_dir.y);
            let visibility = if settings.effective_world_profile() == WorldProfile::AstralFrontier {
                // The reference sky keeps a restrained high-altitude star
                // field in daylight. A small floor preserves that identity
                // without making the scene read as night or adding entities.
                0.07 + night_visibility * 0.93
            } else {
                night_visibility
            };
            mat.base_color = Color::srgba(1.0, 1.0, 1.0, visibility);
            let intensity = 14.0 * visibility * intel.profile.sky_saturation.max(0.7);
            mat.emissive = LinearRgba::rgb(intensity, intensity, intensity * 1.15);
        }
    }
}

fn nebula_visibility(profile: WorldProfile) -> Visibility {
    if profile == WorldProfile::AstralFrontier {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

/// Smooth astronomical-twilight visibility for the additive star shell.
///
/// Sun elevation is represented by the normalized direction Y component.
/// Stars are fully absent once the sun is comfortably above the horizon and
/// fully present in deeper twilight, without a one-frame on/off threshold.
#[inline]
fn star_visibility_for_sun_elevation(elevation_sine: f32) -> f32 {
    let t = ((elevation_sine + 0.16) / 0.28).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
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
    let n_warp = Perlin::new(seed as u32 ^ 0x51A7_4EED);
    // Equirectangular mapping: x → longitude [0, 2π), y → latitude [-π/2, π/2].
    let w = size;
    let h = size / 2;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        // Top-down latitude makes the top texture row world-up after the UV
        // sphere's fixed Z-up -> Y-up rotation.
        let v = std::f64::consts::FRAC_PI_2 - (y as f64 / h as f64) * std::f64::consts::PI;
        let (sv, cv) = v.sin_cos();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            // Unit-sphere sample direction.
            let px = cv * cu;
            let py = sv;
            let pz = cv * su;

            // Startup-only FBM. One generated texture and one unlit draw own
            // the entire runtime sky cost; no procedural noise runs per frame.
            let fbm_at = |n: &Perlin, q: [f64; 3], f: f64, oct: u32| -> f64 {
                let mut sum = 0.0;
                let mut amp = 1.0;
                let mut freq = f;
                let mut norm = 0.0;
                for _ in 0..oct {
                    sum += amp * n.get([q[0] * freq, q[1] * freq, q[2] * freq]);
                    norm += amp;
                    amp *= 0.55;
                    freq *= 2.0;
                }
                sum / norm.max(1e-6)
            };

            let p = [px, py, pz];
            let warp = [
                fbm_at(&n_warp, [px + 7.1, py - 3.7, pz + 1.9], 0.78, 3),
                fbm_at(&n_warp, [px - 5.3, py + 8.9, pz - 2.4], 0.78, 3),
                fbm_at(&n_warp, [px + 2.8, py + 1.6, pz + 9.2], 0.78, 3),
            ];
            let q = [
                p[0] + warp[0] * 0.42,
                p[1] + warp[1] * 0.32,
                p[2] + warp[2] * 0.42,
            ];

            // Broad density establishes a few calm voids. Ridged turbulence
            // adds luminous folds only inside those masses. The result keeps
            // mid-scale structure through daylight ACES tonemapping instead
            // of collapsing into pastel colour mush.
            let density_noise = fbm_at(&n_mask, q, 0.88, 5);
            // The first UV-safe QA pass proved the seam fix but showed only
            // isolated wisps against the cobalt clear colour. Lift broad mass
            // occupancy while retaining alpha-controlled voids between it.
            let density = smooth_unit((density_noise + 0.31) / 0.76);
            let ridge_signal = fbm_at(&n_r, q, 2.25, 5).abs();
            let filament = (1.0 - ridge_signal).clamp(0.0, 1.0).powf(5.0) * density;
            let wisps = smooth_unit((fbm_at(&n_b, q, 1.62, 4) + 0.08) / 0.82) * density;
            let color_flow = fbm_at(&n_g, q, 1.18, 4) * 0.5 + 0.5;

            // Values are authored in display-referred sRGB. Rgba8UnormSrgb
            // performs the defined piecewise decode before emissive
            // multiplication and ACES tonemapping.
            let horizon = (1.0 - (py.abs() * 2.25).min(1.0)).powf(2.0);
            let upper = py.max(0.0).powf(1.4);
            let magenta_mass = density * color_flow;
            let cyan_mass = density * (1.0 - color_flow);
            let r_out = (0.018
                + magenta_mass * 0.78
                + cyan_mass * 0.10
                + filament * 0.54
                + horizon * (0.14 + density * 0.25))
                .min(1.0);
            let g_out = (0.025
                + magenta_mass * 0.12
                + cyan_mass * 0.66
                + filament * 0.36
                + upper * wisps * 0.18
                + horizon * 0.07)
                .min(1.0);
            let b_out = (0.075
                + magenta_mass * 0.68
                + cyan_mass * 0.84
                + filament * 0.58
                + wisps * 0.12
                + upper * 0.06
                + horizon * 0.15)
                .min(1.0);
            let alpha =
                (0.10 + density * 0.60 + filament * 0.24 + horizon * 0.06).clamp(0.08, 0.88);

            data.push((r_out * 255.0) as u8);
            data.push((g_out * 255.0) as u8);
            data.push((b_out * 255.0) as u8);
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

#[inline]
fn smooth_unit(value: f64) -> f64 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
    fn astral_nebula_is_profile_scoped_without_enabling_duplicate_bodies() {
        assert_eq!(
            nebula_visibility(WorldProfile::AstralFrontier),
            Visibility::Visible
        );
        assert_eq!(nebula_visibility(WorldProfile::Natural), Visibility::Hidden);

        let mut policy = default_sky_showcase_policy();
        policy.nebula = true;
        let assets = sky_asset_policy(GraphicsMode::High, policy);
        assert_eq!(assets.nebula_resolution, Some(1024));
        assert!(!assets.classic_sun_moon);
        assert!(!assets.second_moon);
        assert!(!assets.ringed_planet);
        assert!(!assets.second_planet);
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
    fn stars_are_absent_by_day_and_fade_monotonically_through_twilight() {
        assert_eq!(star_visibility_for_sun_elevation(1.0), 0.0);
        assert_eq!(star_visibility_for_sun_elevation(0.12), 0.0);
        assert_eq!(star_visibility_for_sun_elevation(-0.16), 1.0);
        assert_eq!(star_visibility_for_sun_elevation(-1.0), 1.0);

        let daylight = star_visibility_for_sun_elevation(0.08);
        let horizon = star_visibility_for_sun_elevation(0.0);
        let twilight = star_visibility_for_sun_elevation(-0.08);
        assert!(daylight < horizon && horizon < twilight);
        assert!((0.0..=1.0).contains(&horizon));
    }

    #[test]
    fn nebula_texture_is_deterministic_seam_safe_and_structured() {
        let size = 128;
        let first = build_nebula_image(size, 0xA57A_2026_DA7A);
        let second = build_nebula_image(size, 0xA57A_2026_DA7A);
        assert_eq!(first.data, second.data);
        assert_eq!(first.data.len(), (size * (size / 2) * 4) as usize);

        let mut min_rgb = u8::MAX;
        let mut max_rgb = u8::MIN;
        let mut min_alpha = u8::MAX;
        let mut max_alpha = u8::MIN;
        for pixel in first.data.chunks_exact(4) {
            min_rgb = min_rgb.min(pixel[0]).min(pixel[1]).min(pixel[2]);
            max_rgb = max_rgb.max(pixel[0]).max(pixel[1]).max(pixel[2]);
            min_alpha = min_alpha.min(pixel[3]);
            max_alpha = max_alpha.max(pixel[3]);
        }
        assert!(max_rgb.saturating_sub(min_rgb) > 90);
        assert!(min_alpha < 80, "voids need to reveal the clear sky");
        assert!(max_alpha > 120, "cloud masses need visible opacity");

        // Spherical noise returns to the same 3D neighbourhood at U=0/1.
        // Keeping the average edge delta small prevents a visible meridian.
        let row_bytes = size as usize * 4;
        let mut seam_delta = 0u64;
        let mut seam_samples = 0u64;
        for y in 2..(size as usize / 2 - 2) {
            let left = &first.data[y * row_bytes..y * row_bytes + 3];
            let right = &first.data[(y + 1) * row_bytes - 4..(y + 1) * row_bytes - 1];
            for channel in 0..3 {
                seam_delta += u64::from(left[channel].abs_diff(right[channel]));
                seam_samples += 1;
            }
        }
        assert!(seam_delta / seam_samples < 26);
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
