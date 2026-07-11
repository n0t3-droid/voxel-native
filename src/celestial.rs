//! World-space celestial bodies and boost travel.
//!
//! These are real world-space bodies, not camera-relative billboard discs: the
//! same object seen from the ground is the one a ship reaches. Each body stays
//! one mesh plus small atmosphere/cloud shells instead of thousands of entities,
//! preserving the huge silhouette without sacrificing low-end hardware.

use bevy::pbr::NotShadowCaster;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::transform::TransformSystem;
use noise::{NoiseFn, Perlin};

use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::player::Player;
use crate::settings::{GraphicsMode, WorldSettings};
use crate::ships::{PilotState, ShipInstance};

/// Gameplay-space distances are deliberately compressed while body radii and
/// angular size remain coherent. One terrain block is still one metre near the
/// player; interplanetary travel uses a cinematic navigation scale so a trip is
/// measured in seconds rather than real-world days.
///
/// Ground-truth anchors (NASA): the Moon's mean orbital distance is 384,400 km
/// and equatorial radius is 1,737.5 km; the Sun's radius is roughly 700,000 km
/// and Earth distance roughly 150 million km. Sources:
/// <https://science.nasa.gov/moon/by-the-numbers/> and
/// <https://science.nasa.gov/sun/facts/>. The runtime values below are an
/// explicit cinematic compression, never presented as SI astronomy.
const BOOST_ACCEL_BLOCKS_PER_S2: f32 = 760.0;
const BOOST_MAX_SPEED_BLOCKS_PER_S: f32 = 1_850.0;
const BOOST_SURFACE_OFFSET_BLOCKS: f32 = 22.0;
const BOOST_ARRIVAL_DISTANCE_BLOCKS: f32 = 10.0;
const BOOST_FOV_BONUS_DEG: f32 = 8.0;
const TRANSIT_STREAMING_SPEED_BLOCKS_PER_S: f32 = 280.0;
const TRANSIT_GUIDE_LENGTH_BLOCKS: f32 = 140.0;

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
                    maintain_celestial_surface_clearance,
                    animate_celestial_bodies,
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
struct CelestialCloudLayer {
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CelestialTravelPhase {
    #[default]
    Idle,
    Accelerating,
    Cruise,
    Approach,
    Arrived,
}

#[derive(Resource, Debug, Clone)]
pub(crate) struct CelestialTravel {
    pub target_index: usize,
    pub boosting: bool,
    speed: f32,
    pub phase: CelestialTravelPhase,
    pub distance_remaining: f32,
}

impl Default for CelestialTravel {
    fn default() -> Self {
        Self {
            target_index: 0,
            boosting: false,
            speed: 0.0,
            phase: CelestialTravelPhase::Idle,
            distance_remaining: 0.0,
        }
    }
}

impl CelestialTravel {
    pub(crate) fn suspends_ground_streaming(&self) -> bool {
        self.boosting && self.speed >= TRANSIT_STREAMING_SPEED_BLOCKS_PER_S
    }

    fn cancel(&mut self) {
        self.boosting = false;
        self.speed = 0.0;
        self.distance_remaining = 0.0;
        self.phase = CelestialTravelPhase::Idle;
    }
}

pub(crate) fn default_celestial_bodies() -> [CelestialBodySpec; 3] {
    [
        CelestialBodySpec {
            kind: CelestialKind::Moon,
            name: "Aomi Moon",
            center: Vec3::new(6_900.0, 4_300.0, -10_600.0),
            radius: 900.0,
            atmosphere_radius: 958.0,
            seed: 0xA0_41_19,
            spin_speed: 0.012,
        },
        CelestialBodySpec {
            kind: CelestialKind::SakuraPlanet,
            name: "Sakura World",
            center: Vec3::new(-14_800.0, 8_600.0, -18_400.0),
            radius: 2_850.0,
            atmosphere_radius: 3_080.0,
            seed: 0x5A_CA_02,
            spin_speed: -0.006,
        },
        CelestialBodySpec {
            kind: CelestialKind::Sun,
            name: "Helios Core",
            // Only the length is authoritative. Its direction is shared with
            // the daylight system so the visible star and lighting agree.
            center: Vec3::new(0.0, 0.0, 36_000.0),
            radius: 4_100.0,
            atmosphere_radius: 4_680.0,
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
        GraphicsMode::Fast => (192, 96),
        GraphicsMode::Balanced => (384, 192),
        GraphicsMode::High => (640, 320),
    };
    let subdivisions = match settings.graphics {
        GraphicsMode::Fast => 3,
        GraphicsMode::Balanced => 4,
        GraphicsMode::High => 5,
    };
    for (index, spec) in default_celestial_bodies().into_iter().enumerate() {
        let center = celestial_center(spec, settings.time_of_day);
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
                transform: Transform::from_translation(center),
                ..default()
            },
            NotShadowCaster,
            CelestialBody {
                index,
                kind: spec.kind,
                center,
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
                transform: Transform::from_translation(center),
                ..default()
            },
            NotShadowCaster,
            CelestialAtmosphere { index },
            Name::new(format!("Celestial.{}.Atmosphere", spec.name)),
        ));

        // The inhabited world gets a separate slowly rotating cloud veil.
        // It is a single mesh and texture, so the cinematic layer adds one
        // draw call rather than thousands of decorative entities.
        if spec.kind == CelestialKind::SakuraPlanet {
            let cloud_texture = images.add(build_cloud_texture(
                texture_size.0,
                texture_size.1,
                spec.seed.wrapping_add(0xC10D_5EED),
            ));
            let cloud_mesh = meshes.add(
                Sphere::new(spec.radius * 1.018)
                    .mesh()
                    .ico(subdivisions)
                    .expect("cloud ico subdivision is in Bevy's supported range"),
            );
            commands.spawn((
                PbrBundle {
                    mesh: cloud_mesh,
                    material: materials.add(cloud_layer_material(cloud_texture)),
                    transform: Transform::from_translation(center),
                    ..default()
                },
                NotShadowCaster,
                CelestialCloudLayer { index },
                Name::new(format!("Celestial.{}.Clouds", spec.name)),
            ));
        }
    }
}

fn body_material(kind: CelestialKind, texture: Handle<Image>) -> StandardMaterial {
    match kind {
        CelestialKind::Sun => StandardMaterial {
            base_color_texture: Some(texture.clone()),
            emissive_texture: Some(texture),
            emissive: LinearRgba::rgb(34.0, 19.0, 5.5),
            unlit: true,
            fog_enabled: false,
            ..default()
        },
        CelestialKind::Moon => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.95,
            reflectance: 0.18,
            emissive: LinearRgba::rgb(0.035, 0.045, 0.060),
            fog_enabled: false,
            ..default()
        },
        CelestialKind::SakuraPlanet => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.72,
            reflectance: 0.28,
            emissive: LinearRgba::rgb(0.045, 0.018, 0.060),
            fog_enabled: false,
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
        alpha_mode: AlphaMode::Blend,
        // Back-face culling prevents the front and rear shell from adding
        // into a flat ring when viewed from the surface.
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        double_sided: false,
        fog_enabled: false,
        depth_bias: -2.0,
        ..default()
    }
}

fn cloud_layer_material(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::srgba(1.0, 0.92, 0.98, 0.72),
        base_color_texture: Some(texture.clone()),
        emissive_texture: Some(texture),
        emissive: LinearRgba::rgb(0.14, 0.07, 0.16),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        fog_enabled: false,
        depth_bias: -1.0,
        ..default()
    }
}

fn planet_boost_input(
    keys: Res<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    pilot: Res<PilotState>,
    mut travel: ResMut<CelestialTravel>,
    player_q: Query<&Transform, With<Player>>,
    ship_q: Query<&Transform, (With<ShipInstance>, Without<Player>)>,
    bodies: Query<&CelestialBody>,
) {
    if keys.just_pressed(KeyCode::Escape) && travel.boosting {
        travel.cancel();
        return;
    }
    if !celestial_input_allowed(mode.mode) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyN) {
        let count = bodies.iter().count().max(1);
        travel.target_index = (travel.target_index + 1) % count;
        travel.speed = 0.0;
        travel.phase = CelestialTravelPhase::Idle;
    }
    if keys.just_pressed(KeyCode::KeyB) {
        if travel.boosting {
            travel.cancel();
            return;
        }
        let carrier = pilot
            .active_ship
            .and_then(|entity| ship_q.get(entity).ok())
            .or_else(|| player_q.get_single().ok());
        if let Some(carrier_tf) = carrier {
            travel.target_index = select_boost_target(
                carrier_tf.translation,
                carrier_tf.rotation * -Vec3::Z,
                bodies.iter().map(|b| (b.index, b.center)),
            )
            .unwrap_or(travel.target_index);
        }
        travel.boosting = true;
        travel.speed = 0.0;
        travel.phase = CelestialTravelPhase::Accelerating;
    }
}

fn celestial_input_allowed(mode: ActiveMode) -> bool {
    matches!(mode, ActiveMode::Combat | ActiveMode::ShipFlight { .. })
}

fn update_planet_boost(
    time: Res<Time>,
    mut travel: ResMut<CelestialTravel>,
    mut pilot: ResMut<PilotState>,
    bodies: Query<&CelestialBody>,
    mut ship_q: Query<&mut Transform, (With<ShipInstance>, Without<Player>)>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    if !travel.boosting {
        return;
    }
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let Some(body) = bodies.iter().find(|body| body.index == travel.target_index) else {
        travel.cancel();
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 30.0);
    let active_ship = pilot.active_ship;
    let carrier_position = active_ship
        .and_then(|entity| ship_q.get_mut(entity).ok().map(|tf| tf.translation))
        .unwrap_or(player_tf.translation);
    let approach = boost_approach_point(carrier_position, body.center, body.radius);
    let to_approach = approach - carrier_position;
    let distance = to_approach.length();
    travel.distance_remaining = distance;
    if distance <= BOOST_ARRIVAL_DISTANCE_BLOCKS {
        travel.boosting = false;
        travel.speed = 0.0;
        travel.distance_remaining = 0.0;
        travel.phase = CelestialTravelPhase::Arrived;
        player.velocity = Vec3::ZERO;
        player.fov_bonus = 0.0;
        pilot.speed = 0.0;
        pilot.status = format!(
            "{} orbital hold.",
            default_celestial_bodies()[body.index].name
        );
        return;
    }

    let dir = to_approach.normalize_or_zero();
    // Kinematic braking envelope: v <= sqrt(2*a*d). It prevents the carrier
    // from snapping or overshooting at the surface while retaining a fast
    // cruise through empty space. Units reduce to blocks/second.
    let braking_speed = (2.0 * BOOST_ACCEL_BLOCKS_PER_S2 * distance).sqrt();
    let target_speed = braking_speed.min(BOOST_MAX_SPEED_BLOCKS_PER_S);
    let speed_step = BOOST_ACCEL_BLOCKS_PER_S2 * dt;
    travel.speed = move_towards(travel.speed, target_speed, speed_step);
    travel.phase = if distance < body.radius * 0.32 {
        CelestialTravelPhase::Approach
    } else if travel.speed >= BOOST_MAX_SPEED_BLOCKS_PER_S * 0.98 {
        CelestialTravelPhase::Cruise
    } else {
        CelestialTravelPhase::Accelerating
    };
    let step = travel.speed.min(distance / dt.max(1e-4)) * dt;
    let delta = dir * step;
    if let Some(entity) = active_ship {
        if let Ok(mut ship_tf) = ship_q.get_mut(entity) {
            ship_tf.translation += delta;
            // The player camera is the cockpit child in world terms, but is a
            // separate ECS transform. Move it by the identical delta now so
            // the ship-flight system cannot produce a one-frame camera tear.
            player_tf.translation += delta;
        } else {
            pilot.active_ship = None;
            player_tf.translation += delta;
        }
    } else {
        player_tf.translation += delta;
    }
    // Direct boost motion bypasses normal collision scans; keep velocity zero
    // so the next movement tick does not spend time sweeping huge distances.
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    player.fov_bonus += (BOOST_FOV_BONUS_DEG - player.fov_bonus) * (dt * 4.0).min(1.0);
    pilot.speed = travel.speed;
    pilot.status = format!(
        "{}  {:.0} m  {:?}",
        default_celestial_bodies()[body.index].name,
        distance,
        travel.phase
    );
}

fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    if (target - current).abs() <= max_delta {
        target
    } else {
        current + (target - current).signum() * max_delta
    }
}

fn maintain_celestial_surface_clearance(
    pilot: Res<PilotState>,
    bodies: Query<&CelestialBody>,
    mut ship_q: Query<&mut Transform, (With<ShipInstance>, Without<Player>)>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    if let Some(entity) = pilot.active_ship {
        if let Ok(mut ship_tf) = ship_q.get_mut(entity) {
            let mut total = Vec3::ZERO;
            for body in &bodies {
                let clearance = if body.kind == CelestialKind::Sun {
                    180.0
                } else {
                    14.0
                };
                total += surface_clearance_delta(
                    ship_tf.translation + total,
                    body.center,
                    body.radius + clearance,
                );
            }
            if total.length_squared() > 0.0 {
                ship_tf.translation += total;
                player_tf.translation += total;
                player.velocity = Vec3::ZERO;
            }
            return;
        }
    }

    let mut total = Vec3::ZERO;
    for body in &bodies {
        let clearance = if body.kind == CelestialKind::Sun {
            120.0
        } else {
            2.2
        };
        total += surface_clearance_delta(
            player_tf.translation + total,
            body.center,
            body.radius + clearance,
        );
    }
    if total.length_squared() > 0.0 {
        player_tf.translation += total;
        player.velocity = Vec3::ZERO;
        player.flying = true;
    }
}

fn surface_clearance_delta(position: Vec3, center: Vec3, minimum_radius: f32) -> Vec3 {
    let from_center = position - center;
    let distance = from_center.length();
    if distance >= minimum_radius {
        return Vec3::ZERO;
    }
    let normal = if distance > 1e-5 {
        from_center / distance
    } else {
        Vec3::Y
    };
    normal * (minimum_radius - distance)
}

fn celestial_center(spec: CelestialBodySpec, time_of_day: f32) -> Vec3 {
    if spec.kind == CelestialKind::Sun {
        crate::daynight::sun_direction_for_time(time_of_day) * spec.center.length()
    } else {
        spec.center
    }
}

fn animate_celestial_bodies(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut bodies: Query<
        (&mut CelestialBody, &mut Transform),
        (Without<CelestialAtmosphere>, Without<CelestialCloudLayer>),
    >,
    mut atmospheres: Query<
        (&CelestialAtmosphere, &mut Transform),
        (Without<CelestialBody>, Without<CelestialCloudLayer>),
    >,
    mut clouds: Query<
        (&CelestialCloudLayer, &mut Transform),
        (Without<CelestialBody>, Without<CelestialAtmosphere>),
    >,
) {
    let specs = default_celestial_bodies();
    for (mut body, mut transform) in &mut bodies {
        let spec = specs[body.index];
        let center = celestial_center(spec, settings.time_of_day);
        body.center = center;
        transform.translation = center;
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
        transform.translation = celestial_center(spec, settings.time_of_day);
        transform.rotate_y(-spec.spin_speed * 0.45 * time.delta_seconds());
    }
    for (cloud, mut transform) in &mut clouds {
        let spec = specs[cloud.index];
        transform.translation = celestial_center(spec, settings.time_of_day);
        transform.rotate_y(-spec.spin_speed * 0.72 * time.delta_seconds());
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
    let to_target = approach - player_tf.translation;
    let distance = to_target.length();
    let direction = to_target.normalize_or_zero();
    let guide_start = approach - direction * distance.min(TRANSIT_GUIDE_LENGTH_BLOCKS);
    gizmos.line(guide_start, approach, color);
    gizmos.sphere(approach, Quat::IDENTITY, 8.0, color);
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
    center + outward * (radius + BOOST_SURFACE_OFFSET_BLOCKS)
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

fn build_cloud_texture(w: u32, h: u32, seed: u32) -> Image {
    let primary = Perlin::new(seed);
    let detail = Perlin::new(seed.wrapping_add(0x9E37_79B9));
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let latitude = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sin_lat, cos_lat) = latitude.sin_cos();
        for x in 0..w {
            let longitude = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (sin_lon, cos_lon) = longitude.sin_cos();
            let p = [cos_lat * cos_lon, sin_lat, cos_lat * sin_lon];
            let broad = primary.get([p[0] * 2.1, p[1] * 2.1, p[2] * 2.1]);
            let wisps = detail.get([p[0] * 7.3, p[1] * 4.6, p[2] * 7.3]);
            let coverage = (broad * 0.72 + wisps * 0.28) * 0.5 + 0.5;
            let alpha = ((coverage - 0.54) / 0.26).clamp(0.0, 1.0).powf(1.35);
            let warmth = ((broad * 0.5 + 0.5) * 0.12) as f32;
            data.push(((0.92 + warmth).min(1.0) * 255.0) as u8);
            data.push(((0.88 + warmth * 0.45).min(1.0) * 255.0) as u8);
            data.push(((0.96 + warmth * 0.25).min(1.0) * 255.0) as u8);
            data.push((alpha * 205.0) as u8);
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
        assert!(specs.iter().all(|spec| spec.radius >= 850.0));
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
        assert!((distance_from_center - (300.0 + BOOST_SURFACE_OFFSET_BLOCKS)).abs() < 0.01);
        assert!(approach.z < center.z);
    }

    #[test]
    fn sun_center_uses_the_same_direction_as_daylight() {
        let sun = default_celestial_bodies()[2];
        let time = 14.15;
        let center = celestial_center(sun, time);
        let expected = crate::daynight::sun_direction_for_time(time);

        assert!(center.normalize().dot(expected) > 0.99999);
        assert!((center.length() - sun.center.length()).abs() < 0.01);
    }

    #[test]
    fn surface_clearance_projects_out_without_teleporting_safe_points() {
        let center = Vec3::new(100.0, 50.0, -20.0);
        let radius = 40.0;
        let inside = center + Vec3::X * 10.0;
        let delta = surface_clearance_delta(inside, center, radius);
        assert!(((inside + delta).distance(center) - radius).abs() < 0.001);
        assert_eq!(
            surface_clearance_delta(center + Vec3::X * 60.0, center, radius),
            Vec3::ZERO
        );
    }

    #[test]
    fn high_speed_transit_suspends_ground_streaming_only_while_active() {
        let mut travel = CelestialTravel::default();
        travel.boosting = true;
        travel.speed = TRANSIT_STREAMING_SPEED_BLOCKS_PER_S - 1.0;
        assert!(!travel.suspends_ground_streaming());
        travel.speed = TRANSIT_STREAMING_SPEED_BLOCKS_PER_S;
        assert!(travel.suspends_ground_streaming());
        travel.cancel();
        assert!(!travel.suspends_ground_streaming());
    }

    #[test]
    fn celestial_hotkeys_never_steal_builder_input() {
        assert!(celestial_input_allowed(ActiveMode::Combat));
        assert!(!celestial_input_allowed(ActiveMode::BuildLive {
            tool: crate::toolbelt::ToolbeltTool::DrawRect,
        }));
        assert!(!celestial_input_allowed(ActiveMode::BuildPicker {
            tool: crate::toolbelt::ToolbeltTool::Sculpt,
        }));
    }

    #[test]
    fn braking_envelope_converges_without_overshoot() {
        assert_eq!(move_towards(0.0, 100.0, 20.0), 20.0);
        assert_eq!(move_towards(95.0, 100.0, 20.0), 100.0);
        assert_eq!(move_towards(120.0, 100.0, 5.0), 115.0);
    }
}
