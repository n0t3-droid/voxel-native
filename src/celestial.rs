//! World-space celestial bodies and boost travel.
//!
//! `sky.rs` renders camera-relative impostors for a clean background. This
//! module adds the missing gameplay layer: large, fixed world-space bodies the
//! player can actually boost toward. The bodies are intentionally one mesh plus
//! one atmosphere shell each, not thousands of cubes, so the feature stays
//! friendly to low-end PCs.

use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::render::view::RenderLayers;
use bevy::transform::TransformSystem;
use noise::{NoiseFn, Perlin};

use crate::menu::GameState;
use crate::player::Player;
use crate::settings::{GraphicsMode, WorldSettings};

const SKY_IMPOSTOR_DISTANCE: f32 = 910.0;
const BOOST_ACCEL: f32 = 950.0;
const BOOST_MAX_SPEED: f32 = 1650.0;
const BOOST_SURFACE_OFFSET: f32 = 220.0;
const BOOST_ARRIVAL_DISTANCE: f32 = 260.0;
const BOOST_FOV_BONUS: f32 = 18.0;

pub struct CelestialPlugin;

impl Plugin for CelestialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CelestialTravel>()
            .add_systems(Startup, setup_celestial_bodies)
            .add_systems(
                PostUpdate,
                (
                    planet_boost_input,
                    update_planet_boost,
                    animate_celestial_bodies,
                    update_sky_impostors,
                    draw_boost_guides,
                )
                    .chain()
                    .run_if(in_state(GameState::InGame))
                    .before(TransformSystem::TransformPropagate),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CelestialKind {
    Moon,
    SakuraPlanet,
    Sun,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CelestialBodySpec {
    pub kind: CelestialKind,
    pub name: &'static str,
    pub center: Vec3,
    pub radius: f32,
    pub atmosphere_radius: f32,
    pub sky_radius: f32,
    pub seed: u32,
    pub spin_speed: f32,
}

#[derive(Component)]
struct CelestialBody {
    index: usize,
    kind: CelestialKind,
    center: Vec3,
    radius: f32,
}

#[derive(Component)]
struct CelestialAtmosphere {
    index: usize,
}

#[derive(Component)]
struct CelestialSkyImpostor {
    index: usize,
    direction: Vec3,
}

#[derive(Resource, Debug, Clone)]
pub(crate) struct CelestialTravel {
    pub target_index: usize,
    pub boosting: bool,
    speed: f32,
}

impl Default for CelestialTravel {
    fn default() -> Self {
        Self {
            target_index: 0,
            boosting: false,
            speed: 0.0,
        }
    }
}

pub(crate) fn default_celestial_bodies() -> [CelestialBodySpec; 3] {
    [
        CelestialBodySpec {
            kind: CelestialKind::Moon,
            name: "Aomi Moon",
            center: Vec3::new(3_200.0, 1_750.0, -5_100.0),
            radius: 520.0,
            atmosphere_radius: 610.0,
            sky_radius: 34.0,
            seed: 0xA0_41_19,
            spin_speed: 0.012,
        },
        CelestialBodySpec {
            kind: CelestialKind::SakuraPlanet,
            name: "Sakura World",
            center: Vec3::new(-5_400.0, 2_850.0, -6_900.0),
            radius: 880.0,
            atmosphere_radius: 1_040.0,
            sky_radius: 58.0,
            seed: 0x5A_CA_02,
            spin_speed: -0.006,
        },
        CelestialBodySpec {
            kind: CelestialKind::Sun,
            name: "Helios Core",
            center: Vec3::new(9_200.0, 4_600.0, 9_800.0),
            radius: 1_150.0,
            atmosphere_radius: 1_560.0,
            sky_radius: 72.0,
            seed: 0xFE_ED_51,
            spin_speed: 0.02,
        },
    ]
}

fn setup_celestial_bodies(
    mut commands: Commands,
    settings: Res<WorldSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let texture_size = match settings.graphics {
        GraphicsMode::Fast => (96, 48),
        GraphicsMode::Balanced => (160, 80),
        GraphicsMode::High => (256, 128),
    };
    let subdivisions = match settings.graphics {
        GraphicsMode::Fast => 3,
        GraphicsMode::Balanced => 4,
        GraphicsMode::High => 5,
    };
    let sky_layer = RenderLayers::layer(crate::sky::SKY_LAYER);

    for (index, spec) in default_celestial_bodies().into_iter().enumerate() {
        let texture = images.add(build_body_texture(
            spec.kind,
            texture_size.0,
            texture_size.1,
            spec.seed,
        ));
        let mesh = meshes.add(
            Sphere::new(spec.radius)
                .mesh()
                .ico(subdivisions)
                .expect("celestial ico subdivision is in Bevy's supported range"),
        );
        let material = materials.add(body_material(spec.kind, texture.clone()));
        commands.spawn((
            PbrBundle {
                mesh,
                material,
                transform: Transform::from_translation(spec.center),
                ..default()
            },
            NotShadowCaster,
            CelestialBody {
                index,
                kind: spec.kind,
                center: spec.center,
                radius: spec.radius,
            },
            Name::new(format!("Celestial.{}", spec.name)),
        ));

        let atmosphere_mesh = meshes.add(
            Sphere::new(spec.atmosphere_radius)
                .mesh()
                .ico(3)
                .expect("atmosphere ico subdivision is in Bevy's supported range"),
        );
        let atmosphere_material = materials.add(atmosphere_material(spec.kind));
        commands.spawn((
            PbrBundle {
                mesh: atmosphere_mesh,
                material: atmosphere_material,
                transform: Transform::from_translation(spec.center),
                ..default()
            },
            NotShadowCaster,
            CelestialAtmosphere { index },
            Name::new(format!("Celestial.{}.Atmosphere", spec.name)),
        ));

        let sky_mesh = meshes.add(
            Sphere::new(spec.sky_radius)
                .mesh()
                .ico(4)
                .expect("sky impostor ico subdivision is in Bevy's supported range"),
        );
        let sky_material = materials.add(sky_impostor_material(spec.kind, texture));
        commands.spawn((
            PbrBundle {
                mesh: sky_mesh,
                material: sky_material,
                transform: Transform::from_translation(
                    spec.center.normalize_or_zero() * SKY_IMPOSTOR_DISTANCE,
                ),
                ..default()
            },
            NotShadowCaster,
            sky_layer.clone(),
            CelestialSkyImpostor {
                index,
                direction: spec.center.normalize_or_zero(),
            },
            Name::new(format!("Celestial.{}.SkyImpostor", spec.name)),
        ));
    }
}

fn body_material(kind: CelestialKind, texture: Handle<Image>) -> StandardMaterial {
    match kind {
        CelestialKind::Sun => StandardMaterial {
            base_color_texture: Some(texture.clone()),
            emissive_texture: Some(texture),
            emissive: LinearRgba::rgb(45.0, 24.0, 8.0),
            unlit: true,
            ..default()
        },
        CelestialKind::Moon => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.95,
            reflectance: 0.18,
            emissive: LinearRgba::rgb(0.18, 0.22, 0.28),
            ..default()
        },
        CelestialKind::SakuraPlanet => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.72,
            reflectance: 0.28,
            emissive: LinearRgba::rgb(0.10, 0.04, 0.13),
            ..default()
        },
    }
}

fn atmosphere_material(kind: CelestialKind) -> StandardMaterial {
    let (color, emissive) = match kind {
        CelestialKind::Sun => (
            Color::srgba(1.0, 0.42, 0.12, 0.18),
            LinearRgba::rgb(18.0, 7.0, 2.0),
        ),
        CelestialKind::Moon => (
            Color::srgba(0.48, 0.72, 1.0, 0.13),
            LinearRgba::rgb(1.4, 2.0, 3.0),
        ),
        CelestialKind::SakuraPlanet => (
            Color::srgba(0.92, 0.38, 0.92, 0.16),
            LinearRgba::rgb(2.3, 0.7, 2.8),
        ),
    };
    StandardMaterial {
        base_color: color,
        emissive,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        ..default()
    }
}

fn sky_impostor_material(kind: CelestialKind, texture: Handle<Image>) -> StandardMaterial {
    let emissive = match kind {
        CelestialKind::Sun => LinearRgba::rgb(55.0, 28.0, 9.0),
        CelestialKind::Moon => LinearRgba::rgb(5.0, 6.0, 8.0),
        CelestialKind::SakuraPlanet => LinearRgba::rgb(7.0, 2.2, 8.5),
    };
    StandardMaterial {
        base_color_texture: Some(texture.clone()),
        emissive_texture: Some(texture),
        emissive,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}

fn planet_boost_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut travel: ResMut<CelestialTravel>,
    player_q: Query<&Transform, With<Player>>,
    bodies: Query<&CelestialBody>,
) {
    if keys.just_pressed(KeyCode::KeyN) {
        let count = bodies.iter().count().max(1);
        travel.target_index = (travel.target_index + 1) % count;
        travel.speed = 0.0;
    }
    if keys.just_pressed(KeyCode::KeyB) {
        if travel.boosting {
            travel.boosting = false;
            travel.speed = 0.0;
            return;
        }
        if let Ok(player_tf) = player_q.get_single() {
            travel.target_index = select_boost_target(
                player_tf.translation,
                player_tf.rotation * -Vec3::Z,
                bodies.iter().map(|b| (b.index, b.center)),
            )
            .unwrap_or(travel.target_index);
        }
        travel.boosting = true;
        travel.speed = 0.0;
    }
    if keys.just_pressed(KeyCode::Escape) {
        travel.boosting = false;
        travel.speed = 0.0;
    }
}

fn update_planet_boost(
    time: Res<Time>,
    mut travel: ResMut<CelestialTravel>,
    bodies: Query<&CelestialBody>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    if !travel.boosting {
        return;
    }
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let Some(body) = bodies.iter().find(|body| body.index == travel.target_index) else {
        travel.boosting = false;
        travel.speed = 0.0;
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 30.0);
    let approach = boost_approach_point(player_tf.translation, body.center, body.radius);
    let to_approach = approach - player_tf.translation;
    let distance = to_approach.length();
    if distance <= BOOST_ARRIVAL_DISTANCE {
        travel.boosting = false;
        travel.speed = 0.0;
        player.velocity = Vec3::ZERO;
        player.fov_bonus = 0.0;
        return;
    }

    let dir = to_approach.normalize_or_zero();
    travel.speed = (travel.speed + BOOST_ACCEL * dt).min(BOOST_MAX_SPEED);
    let step = travel.speed.min(distance / dt.max(1e-4)) * dt;
    player_tf.translation += dir * step;
    // Direct boost motion bypasses normal collision scans; keep velocity zero
    // so the next movement tick does not spend time sweeping huge distances.
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    player.fov_bonus += (BOOST_FOV_BONUS - player.fov_bonus) * (dt * 4.0).min(1.0);
}

fn animate_celestial_bodies(
    time: Res<Time>,
    mut bodies: Query<(&CelestialBody, &mut Transform), Without<CelestialSkyImpostor>>,
    mut atmospheres: Query<(&CelestialAtmosphere, &mut Transform), Without<CelestialBody>>,
) {
    let specs = default_celestial_bodies();
    for (body, mut transform) in &mut bodies {
        let spec = specs[body.index];
        let axis = match body.kind {
            CelestialKind::Sun => Vec3::Y,
            CelestialKind::Moon => Vec3::new(0.2, 1.0, 0.1).normalize(),
            CelestialKind::SakuraPlanet => Vec3::new(0.3, 0.9, 0.25).normalize(),
        };
        transform.rotate(Quat::from_axis_angle(
            axis,
            spec.spin_speed * time.delta_seconds(),
        ));
    }
    for (atmosphere, mut transform) in &mut atmospheres {
        let spec = specs[atmosphere.index];
        transform.rotate_y(-spec.spin_speed * 0.45 * time.delta_seconds());
    }
}

fn update_sky_impostors(
    time: Res<Time>,
    main_cam: Query<&GlobalTransform, (With<Camera3d>, Without<CelestialSkyImpostor>)>,
    player_q: Query<&Transform, With<Player>>,
    mut impostors: Query<(&CelestialSkyImpostor, &mut Transform, &mut Visibility)>,
) {
    let Some(main_tf) = main_cam.iter().next() else {
        return;
    };
    let (_, _, camera_translation) = main_tf.to_scale_rotation_translation();
    let player_pos = player_q
        .get_single()
        .map(|tf| tf.translation)
        .unwrap_or(camera_translation);
    let specs = default_celestial_bodies();
    for (impostor, mut transform, mut visibility) in &mut impostors {
        let spec = specs[impostor.index];
        transform.translation = camera_translation + impostor.direction * SKY_IMPOSTOR_DISTANCE;
        transform.rotate_y(spec.spin_speed * 0.35 * time.delta_seconds());
        let near_real_body = player_pos.distance(spec.center) < spec.radius * 4.2;
        *visibility = if near_real_body {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
}

fn draw_boost_guides(
    mut gizmos: Gizmos,
    travel: Res<CelestialTravel>,
    bodies: Query<&CelestialBody>,
    player_q: Query<&Transform, With<Player>>,
) {
    if !travel.boosting {
        return;
    }
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let Some(body) = bodies.iter().find(|body| body.index == travel.target_index) else {
        return;
    };
    let approach = boost_approach_point(player_tf.translation, body.center, body.radius);
    let color = match body.kind {
        CelestialKind::Sun => Color::srgb(1.0, 0.55, 0.12),
        CelestialKind::Moon => Color::srgb(0.55, 0.88, 1.0),
        CelestialKind::SakuraPlanet => Color::srgb(1.0, 0.42, 0.92),
    };
    gizmos.line(player_tf.translation, approach, color);
    gizmos.sphere(approach, Quat::IDENTITY, 26.0, color);
}

pub(crate) fn select_boost_target(
    player_pos: Vec3,
    forward: Vec3,
    bodies: impl IntoIterator<Item = (usize, Vec3)>,
) -> Option<usize> {
    let forward = forward.normalize_or_zero();
    bodies
        .into_iter()
        .map(|(index, center)| {
            let to_body = (center - player_pos).normalize_or_zero();
            let dot = forward.dot(to_body).max(-1.0);
            let distance_bias = 1.0 / (1.0 + player_pos.distance(center) * 0.00008);
            (index, dot + distance_bias * 0.08)
        })
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index)
}

pub(crate) fn boost_approach_point(player_pos: Vec3, center: Vec3, radius: f32) -> Vec3 {
    let outward = (player_pos - center).normalize_or_zero();
    let outward = if outward.length_squared() < 1e-6 {
        Vec3::Y
    } else {
        outward
    };
    center + outward * (radius + BOOST_SURFACE_OFFSET)
}

fn build_body_texture(kind: CelestialKind, w: u32, h: u32, seed: u32) -> Image {
    let noise_a = Perlin::new(seed);
    let noise_b = Perlin::new(seed.wrapping_add(0x9E37));
    let noise_c = Perlin::new(seed.wrapping_add(0x51ED));
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            let p = [cv * cu, sv, cv * su];
            let fbm = |n: &Perlin, base: f64| -> f64 {
                let mut sum = 0.0;
                let mut amp = 1.0;
                let mut freq = base;
                let mut norm = 0.0;
                for _ in 0..5 {
                    sum += n.get([p[0] * freq, p[1] * freq, p[2] * freq]) * amp;
                    norm += amp;
                    amp *= 0.52;
                    freq *= 2.04;
                }
                (sum / norm.max(1e-6)) * 0.5 + 0.5
            };
            let a = fbm(&noise_a, 1.1);
            let b = fbm(&noise_b, 2.3);
            let c = fbm(&noise_c, 5.1);
            let lat = sv.abs() as f32;
            let (r, g, bl) = match kind {
                CelestialKind::Moon => {
                    let crater = ((1.0 - c as f32).powf(7.0) * 0.55).clamp(0.0, 0.5);
                    let shade = (0.42 + a as f32 * 0.48 - crater).clamp(0.18, 0.92);
                    (shade * 0.86, shade * 0.92, shade)
                }
                CelestialKind::SakuraPlanet => {
                    let ocean = (0.28 + b as f32 * 0.36).clamp(0.0, 1.0);
                    let land = (a as f32 - 0.46).max(0.0) * 2.3;
                    let cloud = (c as f32 - 0.68).max(0.0) * 2.8;
                    let pink = (0.45 + land * 0.55 + cloud * 0.25).clamp(0.0, 1.0);
                    let teal = (0.28 + ocean * 0.55 + cloud * 0.25).clamp(0.0, 1.0);
                    (
                        (0.12 + pink * 0.72 + lat * 0.05).clamp(0.0, 1.0),
                        (0.20 + teal * 0.54 + cloud * 0.24).clamp(0.0, 1.0),
                        (0.35 + teal * 0.46 + pink * 0.18).clamp(0.0, 1.0),
                    )
                }
                CelestialKind::Sun => {
                    let band = ((u * 8.0).sin() as f32 * 0.5 + 0.5) * 0.22;
                    let flare = (a as f32 * 0.65 + b as f32 * 0.35 + band).clamp(0.0, 1.0);
                    (
                        1.0,
                        (0.44 + flare * 0.46).clamp(0.0, 1.0),
                        (0.08 + flare * 0.18).clamp(0.0, 1.0),
                    )
                }
            };
            data.push((r * 255.0) as u8);
            data.push((g * 255.0) as u8);
            data.push((bl * 255.0) as u8);
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
    fn celestial_bodies_are_large_and_reachable() {
        let specs = default_celestial_bodies();
        assert!(specs.iter().all(|spec| spec.radius >= 500.0));
        assert!(specs
            .iter()
            .all(|spec| spec.center.length() < crate::player::WORLD_CAMERA_FAR * 0.5));
    }

    #[test]
    fn boost_target_prefers_body_near_crosshair() {
        let player_pos = Vec3::ZERO;
        let bodies = [
            (0, Vec3::new(0.0, 0.0, -2000.0)),
            (1, Vec3::new(2000.0, 0.0, 0.0)),
        ];

        assert_eq!(
            select_boost_target(player_pos, -Vec3::Z, bodies).unwrap(),
            0
        );
        assert_eq!(select_boost_target(player_pos, Vec3::X, bodies).unwrap(), 1);
    }

    #[test]
    fn boost_approach_stops_above_surface_not_center() {
        let center = Vec3::new(100.0, 50.0, 0.0);
        let player = Vec3::new(100.0, 50.0, -1000.0);
        let approach = boost_approach_point(player, center, 300.0);

        let distance_from_center = approach.distance(center);
        assert!((distance_from_center - (300.0 + BOOST_SURFACE_OFFSET)).abs() < 0.01);
        assert!(approach.z < center.z);
    }
}
