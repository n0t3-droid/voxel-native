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
    /// Initial landmark or orbital-radius vector, in terrain blocks.
    pub center: Vec3,
    /// Solid-body radius, in terrain blocks.
    pub radius: f32,
    /// Outer atmosphere-shell radius, in terrain blocks.
    pub atmosphere_radius: f32,
    pub seed: u32,
    /// Authored cinematic spin rate, in radians per real second.
    pub spin_speed: f32,
}

#[derive(Component)]
struct CelestialBody {
    index: usize,
    kind: CelestialKind,
    center: Vec3,
    radius: f32,
    material: Handle<StandardMaterial>,
}

#[derive(Component)]
struct CelestialAtmosphere {
    index: usize,
    kind: CelestialKind,
    material: Handle<StandardMaterial>,
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
            // Only the length is authoritative. Its world-space direction is
            // supplied by the same orbit used by the moon key light.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CelestialAssetPolicy {
    texture_size: (u32, u32),
    body_subdivisions: usize,
    atmosphere_subdivisions: usize,
    cloud_subdivisions: usize,
    cloud_layer: bool,
}

fn celestial_asset_policy(graphics: GraphicsMode) -> CelestialAssetPolicy {
    match graphics {
        GraphicsMode::Fast => CelestialAssetPolicy {
            texture_size: (160, 80),
            body_subdivisions: 3,
            atmosphere_subdivisions: 3,
            cloud_subdivisions: 3,
            // Fast keeps the atmosphere silhouette but folds cloud detail
            // into the surface texture, saving one transparent draw per frame.
            cloud_layer: false,
        },
        GraphicsMode::Balanced => CelestialAssetPolicy {
            texture_size: (320, 160),
            body_subdivisions: 4,
            atmosphere_subdivisions: 3,
            cloud_subdivisions: 4,
            cloud_layer: true,
        },
        GraphicsMode::High => CelestialAssetPolicy {
            texture_size: (640, 320),
            body_subdivisions: 5,
            atmosphere_subdivisions: 4,
            cloud_subdivisions: 5,
            cloud_layer: true,
        },
    }
}

fn setup_celestial_bodies(
    mut commands: Commands,
    settings: Res<WorldSettings>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let asset_policy = celestial_asset_policy(settings.graphics);
    let texture_size = asset_policy.texture_size;
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
                .ico(asset_policy.body_subdivisions)
                .expect("celestial ico subdivision is in Bevy's supported range"),
        );
        let material = materials.add(body_material(spec.kind, texture.clone()));
        commands.spawn((
            PbrBundle {
                mesh,
                material: material.clone(),
                transform: Transform::from_translation(center)
                    .with_rotation(body_visual_rotation(spec, 0.0)),
                ..default()
            },
            NotShadowCaster,
            CelestialBody {
                index,
                kind: spec.kind,
                center,
                radius: spec.radius,
                material,
            },
            Name::new(format!("Celestial.{}", spec.name)),
        ));

        let atmosphere_mesh = meshes.add(
            Sphere::new(spec.atmosphere_radius)
                .mesh()
                .ico(asset_policy.atmosphere_subdivisions)
                .expect("atmosphere ico subdivision is in Bevy's supported range"),
        );
        let atmosphere_texture = images.add(build_atmosphere_texture(
            spec.kind,
            (texture_size.0 / 2).max(64),
            (texture_size.1 / 2).max(32),
            spec.seed.wrapping_add(0xA7A0_51E5),
        ));
        let atmosphere_material = materials.add(atmosphere_material(
            spec.kind,
            settings.graphics,
            atmosphere_texture,
        ));
        commands.spawn((
            PbrBundle {
                mesh: atmosphere_mesh,
                material: atmosphere_material.clone(),
                transform: Transform::from_translation(center),
                ..default()
            },
            NotShadowCaster,
            CelestialAtmosphere {
                index,
                kind: spec.kind,
                material: atmosphere_material,
            },
            Name::new(format!("Celestial.{}.Atmosphere", spec.name)),
        ));

        // The inhabited world gets a separate slowly rotating cloud veil.
        // It is a single mesh and texture, so the cinematic layer adds one
        // draw call rather than thousands of decorative entities.
        if spec.kind == CelestialKind::SakuraPlanet && asset_policy.cloud_layer {
            let cloud_texture = images.add(build_cloud_texture(
                texture_size.0,
                texture_size.1,
                spec.seed.wrapping_add(0xC10D_5EED),
            ));
            let cloud_mesh = meshes.add(
                Sphere::new(spec.radius * 1.018)
                    .mesh()
                    .ico(asset_policy.cloud_subdivisions)
                    .expect("cloud ico subdivision is in Bevy's supported range"),
            );
            commands.spawn((
                PbrBundle {
                    mesh: cloud_mesh,
                    material: materials.add(cloud_layer_material(cloud_texture, settings.graphics)),
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
            emissive: body_emissive(kind, 1.0),
            unlit: true,
            fog_enabled: false,
            ..default()
        },
        CelestialKind::Moon => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.95,
            reflectance: 0.18,
            emissive: body_emissive(kind, 1.0),
            fog_enabled: false,
            ..default()
        },
        CelestialKind::SakuraPlanet => StandardMaterial {
            base_color_texture: Some(texture),
            perceptual_roughness: 0.72,
            reflectance: 0.28,
            emissive: body_emissive(kind, 1.0),
            fog_enabled: false,
            ..default()
        },
    }
}

fn body_emissive(kind: CelestialKind, scale: f32) -> LinearRgba {
    let scale = scale.max(0.0);
    match kind {
        CelestialKind::Sun => LinearRgba::rgb(34.0 * scale, 19.0 * scale, 5.5 * scale),
        CelestialKind::Moon => LinearRgba::rgb(0.035 * scale, 0.045 * scale, 0.060 * scale),
        CelestialKind::SakuraPlanet => LinearRgba::rgb(0.045 * scale, 0.018 * scale, 0.060 * scale),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AtmosphereTuning {
    opacity_scale: f32,
    emissive_scale: f32,
    depth_bias: f32,
}

fn atmosphere_tuning(quality: GraphicsMode) -> AtmosphereTuning {
    match quality {
        GraphicsMode::Fast => AtmosphereTuning {
            opacity_scale: 0.68,
            emissive_scale: 0.62,
            depth_bias: -0.20,
        },
        GraphicsMode::Balanced => AtmosphereTuning {
            opacity_scale: 0.86,
            emissive_scale: 0.82,
            depth_bias: -0.38,
        },
        GraphicsMode::High => AtmosphereTuning {
            opacity_scale: 1.0,
            emissive_scale: 1.0,
            depth_bias: -0.58,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct AtmosphereOptics {
    color: [f32; 3],
    alpha: f32,
    emissive: [f32; 3],
}

fn atmosphere_optics(kind: CelestialKind) -> AtmosphereOptics {
    let (color, alpha, emissive) = match kind {
        CelestialKind::Sun => ([1.0, 0.42, 0.12], 0.18, [18.0, 7.0, 2.0]),
        CelestialKind::Moon => ([0.48, 0.72, 1.0], 0.13, [1.4, 2.0, 3.0]),
        CelestialKind::SakuraPlanet => ([0.92, 0.38, 0.92], 0.16, [2.3, 0.7, 2.8]),
    };
    AtmosphereOptics {
        color,
        alpha,
        emissive,
    }
}

fn atmosphere_material(
    kind: CelestialKind,
    quality: GraphicsMode,
    texture: Handle<Image>,
) -> StandardMaterial {
    let tuning = atmosphere_tuning(quality);
    let optics = atmosphere_optics(kind);
    StandardMaterial {
        base_color: Color::srgba(
            optics.color[0],
            optics.color[1],
            optics.color[2],
            optics.alpha * tuning.opacity_scale,
        ),
        base_color_texture: Some(texture.clone()),
        emissive_texture: Some(texture),
        emissive: LinearRgba::rgb(
            optics.emissive[0] * tuning.emissive_scale,
            optics.emissive[1] * tuning.emissive_scale,
            optics.emissive[2] * tuning.emissive_scale,
        ),
        unlit: true,
        // Premultiplied alpha keeps thin limb pixels luminous without the
        // dark fringe that makes a transparent sphere read as a flat disc.
        alpha_mode: AlphaMode::Premultiplied,
        // Back-face culling prevents the front and rear shell from adding
        // into a flat ring when viewed from the surface.
        cull_mode: Some(bevy::render::render_resource::Face::Back),
        double_sided: false,
        fog_enabled: false,
        depth_bias: tuning.depth_bias,
        ..default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CelestialPresentation {
    angular_radius_rad: f32,
    atmosphere_shell_scale: f32,
    atmosphere_opacity: f32,
    atmosphere_emissive: f32,
    surface_night_fill: f32,
}

fn celestial_presentation(
    observer_distance: f32,
    body_radius: f32,
    atmosphere_radius: f32,
    quality: GraphicsMode,
) -> CelestialPresentation {
    let safe_distance = observer_distance.max(body_radius);
    // Angular radius of a sphere as seen by an external observer. Keeping the
    // transition in angular rather than world distance makes the same body
    // read consistently on the ground, during boost, and on final approach.
    let angular_radius_rad = (body_radius / safe_distance).clamp(0.0, 1.0).asin();
    let angular_radius_deg = angular_radius_rad.to_degrees();
    let apparent_size = smoothstep(0.35, 8.0, angular_radius_deg);
    let quality_thickness = match quality {
        GraphicsMode::Fast => 0.58,
        GraphicsMode::Balanced => 0.82,
        GraphicsMode::High => 1.0,
    };
    let surface_fill_floor = match quality {
        // The lowest tier trades a precise PBR terminator for enough baked
        // fill to remain legible without extra lights or shader passes.
        GraphicsMode::Fast => 0.42,
        GraphicsMode::Balanced => 0.24,
        GraphicsMode::High => 0.12,
    };
    let full_thickness = (atmosphere_radius - body_radius).max(0.001);
    let visible_thickness =
        full_thickness * (0.12 + 0.88 * apparent_size * quality_thickness).clamp(0.08, 1.0);
    let atmosphere_shell_scale =
        (body_radius + visible_thickness) / atmosphere_radius.max(body_radius);

    // A transparent sphere becomes a full-screen color wash once the camera
    // enters its shell. Fade that shell close to the surface; local weather
    // and fog systems own the actual in-atmosphere view.
    let altitude_ratio = ((observer_distance - body_radius) / full_thickness).max(0.0);
    let outside_shell_fade = smoothstep(0.15, 1.6, altitude_ratio);
    CelestialPresentation {
        angular_radius_rad,
        atmosphere_shell_scale,
        atmosphere_opacity: (0.42 + 0.58 * apparent_size) * outside_shell_fade,
        atmosphere_emissive: (0.50 + 0.50 * apparent_size) * outside_shell_fade,
        // Far silhouettes retain a little fill. As geometry fills the screen,
        // fill recedes so PBR lighting can reveal a proper terminator.
        surface_night_fill: (1.0 - apparent_size * 0.88).clamp(surface_fill_floor, 1.0),
    }
}

fn update_atmosphere_material(
    material: &mut StandardMaterial,
    kind: CelestialKind,
    quality: GraphicsMode,
    presentation: CelestialPresentation,
) {
    let tuning = atmosphere_tuning(quality);
    let optics = atmosphere_optics(kind);
    material.base_color = Color::srgba(
        optics.color[0],
        optics.color[1],
        optics.color[2],
        optics.alpha * tuning.opacity_scale * presentation.atmosphere_opacity,
    );
    material.emissive = LinearRgba::rgb(
        optics.emissive[0] * tuning.emissive_scale * presentation.atmosphere_emissive,
        optics.emissive[1] * tuning.emissive_scale * presentation.atmosphere_emissive,
        optics.emissive[2] * tuning.emissive_scale * presentation.atmosphere_emissive,
    );
}

fn seed_rotation_phase(seed: u32) -> f32 {
    (seed as f64 / (u32::MAX as f64 + 1.0) * std::f64::consts::TAU) as f32
}

fn body_spin_rate(spec: CelestialBodySpec) -> f32 {
    let credibility_scale = match spec.kind {
        CelestialKind::Sun => 0.18,
        CelestialKind::Moon => 0.12,
        CelestialKind::SakuraPlanet => 0.15,
    };
    spec.spin_speed * credibility_scale
}

fn body_axial_tilt(kind: CelestialKind) -> Quat {
    match kind {
        CelestialKind::Sun => Quat::from_rotation_z(0.08),
        CelestialKind::Moon => Quat::from_rotation_z(-0.12),
        CelestialKind::SakuraPlanet => Quat::from_rotation_z(0.31),
    }
}

fn wrapped_spin_phase(
    spec: CelestialBodySpec,
    elapsed_seconds: f64,
    rate_scale: f64,
    phase_offset_rad: f64,
) -> f32 {
    let phase = seed_rotation_phase(spec.seed) as f64
        + body_spin_rate(spec) as f64 * rate_scale * elapsed_seconds
        + phase_offset_rad;
    phase.rem_euclid(std::f64::consts::TAU) as f32
}

fn body_visual_rotation(spec: CelestialBodySpec, elapsed_seconds: f64) -> Quat {
    body_axial_tilt(spec.kind)
        * Quat::from_rotation_y(wrapped_spin_phase(spec, elapsed_seconds, 1.0, 0.0))
}

fn cloud_visual_rotation(spec: CelestialBodySpec, elapsed_seconds: f64) -> Quat {
    // Clouds move in the same direction as the surface with a small
    // differential drift. Reversing them made the layer look mechanical.
    body_axial_tilt(spec.kind)
        * Quat::from_rotation_y(wrapped_spin_phase(spec, elapsed_seconds, 1.035, 0.08))
}

fn atmosphere_light_rotation(light_direction: Vec3, seed: u32) -> Quat {
    let light_direction = if light_direction.length_squared() > 1e-8 {
        light_direction.normalize()
    } else {
        Vec3::X
    };
    // The texture's local +X hemisphere is the illuminated side. Rotation
    // around that axis changes the seam without disturbing terminator aim.
    Quat::from_rotation_arc(Vec3::X, light_direction)
        * Quat::from_axis_angle(Vec3::X, seed_rotation_phase(seed))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CloudMaterialTuning {
    alpha: f32,
    emissive: f32,
    unlit: bool,
}

fn cloud_material_tuning(quality: GraphicsMode) -> CloudMaterialTuning {
    match quality {
        GraphicsMode::Fast => CloudMaterialTuning {
            alpha: 0.58,
            emissive: 0.055,
            unlit: true,
        },
        GraphicsMode::Balanced => CloudMaterialTuning {
            alpha: 0.66,
            emissive: 0.022,
            unlit: false,
        },
        GraphicsMode::High => CloudMaterialTuning {
            alpha: 0.72,
            emissive: 0.012,
            unlit: false,
        },
    }
}

fn cloud_layer_material(texture: Handle<Image>, quality: GraphicsMode) -> StandardMaterial {
    let tuning = cloud_material_tuning(quality);
    StandardMaterial {
        base_color: Color::srgba(1.0, 0.92, 0.98, tuning.alpha),
        base_color_texture: Some(texture.clone()),
        emissive_texture: Some(texture),
        emissive: LinearRgba::rgb(
            tuning.emissive,
            tuning.emissive * 0.55,
            tuning.emissive * 1.08,
        ),
        unlit: tuning.unlit,
        perceptual_roughness: 0.92,
        reflectance: 0.16,
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
    match spec.kind {
        CelestialKind::Sun => {
            crate::daynight::sun_direction_for_time(time_of_day) * spec.center.length()
        }
        CelestialKind::Moon => {
            crate::daynight::moon_direction_for_time(time_of_day) * spec.center.length()
        }
        CelestialKind::SakuraPlanet => spec.center,
    }
}

fn animate_celestial_bodies(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    player_q: Query<&GlobalTransform, With<Player>>,
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
    let observer = player_q
        .get_single()
        .map(GlobalTransform::translation)
        .unwrap_or(Vec3::ZERO);
    // Keep phase accumulation in f64 and wrap before constructing a Quat;
    // long-running sessions otherwise lose visible f32 rotation precision.
    let elapsed_seconds = time.elapsed_seconds_f64();
    let sun_center = celestial_center(specs[2], settings.time_of_day);
    for (mut body, mut transform) in &mut bodies {
        let spec = specs[body.index];
        let center = celestial_center(spec, settings.time_of_day);
        body.center = center;
        transform.translation = center;
        transform.rotation = body_visual_rotation(spec, elapsed_seconds);

        if let Some(material) = materials.get_mut(&body.material) {
            let presentation = celestial_presentation(
                observer.distance(center),
                spec.radius,
                spec.atmosphere_radius,
                settings.graphics,
            );
            let fill = if body.kind == CelestialKind::Sun {
                1.0
            } else {
                presentation.surface_night_fill
            };
            material.emissive = body_emissive(body.kind, fill);
        }
    }
    for (atmosphere, mut transform) in &mut atmospheres {
        let spec = specs[atmosphere.index];
        let center = celestial_center(spec, settings.time_of_day);
        let presentation = celestial_presentation(
            observer.distance(center),
            spec.radius,
            spec.atmosphere_radius,
            settings.graphics,
        );
        transform.translation = center;
        transform.scale = Vec3::splat(presentation.atmosphere_shell_scale);
        transform.rotation = if atmosphere.kind == CelestialKind::Sun {
            body_visual_rotation(spec, elapsed_seconds * 0.32)
        } else {
            atmosphere_light_rotation(sun_center - center, spec.seed)
        };
        if let Some(material) = materials.get_mut(&atmosphere.material) {
            update_atmosphere_material(material, atmosphere.kind, settings.graphics, presentation);
        }
    }
    for (cloud, mut transform) in &mut clouds {
        let spec = specs[cloud.index];
        transform.translation = celestial_center(spec, settings.time_of_day);
        transform.rotation = cloud_visual_rotation(spec, elapsed_seconds);
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
    let craters = if kind == CelestialKind::Moon {
        build_crater_stamps(seed)
    } else {
        Vec::new()
    };
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let v = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sv, cv) = v.sin_cos();
        for x in 0..w {
            let u = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (su, cu) = u.sin_cos();
            let p = [cv * cu, sv, cv * su];
            let lat = sv.abs() as f32;
            let [r, g, bl] = match kind {
                CelestialKind::Moon => moon_surface_color(p, &noise_a, &noise_b, &craters),
                CelestialKind::SakuraPlanet => {
                    sakura_surface_color(p, lat, &noise_a, &noise_b, &noise_c)
                }
                CelestialKind::Sun => sun_surface_color(p, u, &noise_a, &noise_b),
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

const SAKURA_SEA_LEVEL: f32 = 0.5;
const MOON_CRATER_COUNT: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SurfaceMasks {
    land: f32,
    ocean: f32,
    shore: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CraterStamp {
    center: [f64; 3],
    radius_chord: f64,
    strength: f32,
}

fn sample_fbm(noise: &Perlin, p: [f64; 3], base_frequency: f64, octaves: usize) -> f64 {
    let mut sum = 0.0;
    let mut amplitude = 1.0;
    let mut frequency = base_frequency;
    let mut normalization: f64 = 0.0;
    for _ in 0..octaves {
        sum += noise.get([p[0] * frequency, p[1] * frequency, p[2] * frequency]) * amplitude;
        normalization += amplitude;
        amplitude *= 0.52;
        frequency *= 2.04;
    }
    (sum / normalization.max(1e-6)) * 0.5 + 0.5
}

fn warp_sphere_point(p: [f64; 3], noise: &Perlin) -> [f64; 3] {
    let frequency = 1.35;
    let amount = 0.34;
    let warped = [
        p[0] + noise.get([
            p[0] * frequency + 17.1,
            p[1] * frequency - 4.7,
            p[2] * frequency + 9.2,
        ]) * amount,
        p[1] + noise.get([
            p[0] * frequency - 8.3,
            p[1] * frequency + 13.6,
            p[2] * frequency + 2.4,
        ]) * amount,
        p[2] + noise.get([
            p[0] * frequency + 5.9,
            p[1] * frequency + 1.7,
            p[2] * frequency - 15.2,
        ]) * amount,
    ];
    let inverse_length = (warped[0] * warped[0] + warped[1] * warped[1] + warped[2] * warped[2])
        .sqrt()
        .recip();
    [
        warped[0] * inverse_length,
        warped[1] * inverse_length,
        warped[2] * inverse_length,
    ]
}

fn land_ocean_masks(elevation: f32) -> SurfaceMasks {
    let (land, ocean) = if elevation >= SAKURA_SEA_LEVEL {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    let shore = (1.0 - (elevation - SAKURA_SEA_LEVEL).abs() / 0.055).clamp(0.0, 1.0);
    SurfaceMasks { land, ocean, shore }
}

fn mix_rgb(a: [f32; 3], b: [f32; 3], amount: f32) -> [f32; 3] {
    let amount = amount.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * amount,
        a[1] + (b[1] - a[1]) * amount,
        a[2] + (b[2] - a[2]) * amount,
    ]
}

fn moon_surface_color(
    p: [f64; 3],
    broad_noise: &Perlin,
    detail_noise: &Perlin,
    craters: &[CraterStamp],
) -> [f32; 3] {
    let broad = sample_fbm(broad_noise, p, 0.9, 5) as f32;
    let detail = sample_fbm(detail_noise, p, 4.8, 4) as f32;
    let crater_relief = craters
        .iter()
        .map(|crater| crater_relief_at(p, *crater))
        .sum::<f32>()
        .clamp(-0.34, 0.28);
    let shade =
        (0.5 + (broad - 0.5) * 0.46 + (detail - 0.5) * 0.13 + crater_relief).clamp(0.16, 0.9);
    let warmth = (broad - 0.5) * 0.025;
    [
        (shade * 0.87 + warmth).clamp(0.0, 1.0),
        (shade * 0.92 + warmth * 0.4).clamp(0.0, 1.0),
        shade,
    ]
}

fn sakura_surface_color(
    p: [f64; 3],
    latitude: f32,
    continental_noise: &Perlin,
    detail_noise: &Perlin,
    warp_noise: &Perlin,
) -> [f32; 3] {
    let warped = warp_sphere_point(p, warp_noise);
    let continental = sample_fbm(continental_noise, warped, 0.86, 5) as f32;
    let detail = sample_fbm(detail_noise, warped, 3.4, 4) as f32;
    let elevation = (continental * 0.84 + detail * 0.16).clamp(0.0, 1.0);
    let masks = land_ocean_masks(elevation);
    let biome = sample_fbm(detail_noise, warped, 1.65, 3) as f32;

    let ocean_depth = ((SAKURA_SEA_LEVEL - elevation) / 0.18).clamp(0.0, 1.0);
    let ocean = mix_rgb([0.08, 0.42, 0.49], [0.025, 0.10, 0.27], ocean_depth);
    let land_height = ((elevation - SAKURA_SEA_LEVEL) / 0.24).clamp(0.0, 1.0);
    let lowland = mix_rgb([0.34, 0.14, 0.30], [0.67, 0.23, 0.49], biome);
    let mut land = mix_rgb(lowland, [0.76, 0.62, 0.76], land_height.powf(1.35));
    let polar_frost = smoothstep(0.78, 0.97, latitude)
        * smoothstep(0.38, 0.68, sample_fbm(detail_noise, p, 5.2, 3) as f32);
    land = mix_rgb(land, [0.88, 0.83, 0.91], polar_frost * 0.82);

    let surface = [
        land[0] * masks.land + ocean[0] * masks.ocean,
        land[1] * masks.land + ocean[1] * masks.ocean,
        land[2] * masks.land + ocean[2] * masks.ocean,
    ];
    let shore_color = [
        0.83 * masks.land + 0.10 * masks.ocean,
        0.62 * masks.land + 0.49 * masks.ocean,
        0.70 * masks.land + 0.53 * masks.ocean,
    ];
    let shore_strength = (0.58 * masks.land + 0.52 * masks.ocean) * masks.shore;
    mix_rgb(surface, shore_color, shore_strength)
}

fn sun_surface_color(
    p: [f64; 3],
    longitude: f64,
    broad_noise: &Perlin,
    detail_noise: &Perlin,
) -> [f32; 3] {
    let broad = sample_fbm(broad_noise, p, 1.1, 5) as f32;
    let detail = sample_fbm(detail_noise, p, 2.3, 5) as f32;
    let band = ((longitude * 8.0).sin() as f32 * 0.5 + 0.5) * 0.22;
    let flare = (broad * 0.65 + detail * 0.35 + band).clamp(0.0, 1.0);
    [
        1.0,
        (0.44 + flare * 0.46).clamp(0.0, 1.0),
        (0.08 + flare * 0.18).clamp(0.0, 1.0),
    ]
}

fn next_crater_random(state: &mut u32) -> u32 {
    let mut value = *state;
    if value == 0 {
        value = 0xA341_316C;
    }
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    *state = value;
    value
}

fn crater_random_unit(state: &mut u32) -> f64 {
    next_crater_random(state) as f64 / (u32::MAX as f64 + 1.0)
}

fn build_crater_stamps(seed: u32) -> Vec<CraterStamp> {
    let mut state = seed ^ 0xC4A7_EA51;
    (0..MOON_CRATER_COUNT)
        .map(|index| {
            let vertical = crater_random_unit(&mut state) * 2.0 - 1.0;
            let longitude = crater_random_unit(&mut state) * std::f64::consts::TAU;
            let horizontal = (1.0 - vertical * vertical).sqrt();
            let size_roll = crater_random_unit(&mut state);
            let angular_radius = if index < 5 {
                0.10 + size_roll * 0.075
            } else {
                0.026 + size_roll * size_roll * 0.072
            };
            CraterStamp {
                center: [
                    horizontal * longitude.cos(),
                    vertical,
                    horizontal * longitude.sin(),
                ],
                radius_chord: 2.0 * (angular_radius * 0.5).sin(),
                strength: (0.72 + crater_random_unit(&mut state) * 0.48) as f32,
            }
        })
        .collect()
}

fn crater_profile(normalized_distance: f32) -> f32 {
    if normalized_distance >= 1.4 {
        return 0.0;
    }
    let bowl = if normalized_distance < 0.82 {
        let inside = 1.0 - (normalized_distance / 0.82).powi(2);
        -0.20 * inside.powf(0.68)
    } else {
        0.0
    };
    let rim_offset = (normalized_distance - 0.91) / 0.09;
    let rim = 0.18 * (-rim_offset * rim_offset).exp();
    let ejecta_offset = (normalized_distance - 1.12) / 0.19;
    let ejecta = 0.025 * (-ejecta_offset * ejecta_offset).exp();
    bowl + rim + ejecta
}

fn crater_relief_at(p: [f64; 3], crater: CraterStamp) -> f32 {
    let dot = (p[0] * crater.center[0] + p[1] * crater.center[1] + p[2] * crater.center[2])
        .clamp(-1.0, 1.0);
    let chord_distance = (2.0 * (1.0 - dot)).sqrt();
    crater_profile((chord_distance / crater.radius_chord) as f32) * crater.strength
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn build_atmosphere_texture(kind: CelestialKind, w: u32, h: u32, seed: u32) -> Image {
    let broad_noise = Perlin::new(seed);
    let detail_noise = Perlin::new(seed.wrapping_add(0x9E37_79B9));
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let latitude = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sin_lat, cos_lat) = latitude.sin_cos();
        for x in 0..w {
            let longitude = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (sin_lon, cos_lon) = longitude.sin_cos();
            let p = [cos_lat * cos_lon, sin_lat, cos_lat * sin_lon];
            let broad = sample_fbm(&broad_noise, p, 1.45, 4) as f32;
            let detail = sample_fbm(&detail_noise, p, 5.4, 3) as f32;
            let modulation = (0.82 + broad * 0.22 + detail * 0.10).clamp(0.72, 1.12);
            let (rgb, alpha) = if kind == CelestialKind::Sun {
                let band = ((latitude * 7.0 + longitude * 1.7).sin() * 0.5 + 0.5) as f32;
                let alpha = (0.58 + broad * 0.24 + detail * 0.10 + band * 0.08).clamp(0.38, 1.0);
                ([1.0, 0.88 + broad * 0.10, 0.68 + detail * 0.16], alpha)
            } else {
                // Local +X is the day hemisphere. The runtime rotates this
                // field toward the sun, giving the shell a coherent
                // terminator instead of uniform alpha over the whole disc.
                let day = smoothstep(-0.26, 0.58, p[0] as f32);
                let terminator = (-(p[0] as f32 / 0.18).powi(2)).exp();
                let alpha =
                    ((0.10 + day * 0.66 + terminator * 0.24) * modulation).clamp(0.035, 1.0);
                let warmth = terminator * 0.08;
                ([1.0, 0.94 + warmth, 0.98 + warmth * 0.25], alpha)
            };
            data.push((rgb[0].clamp(0.0, 1.0) * 255.0) as u8);
            data.push((rgb[1].clamp(0.0, 1.0) * 255.0) as u8);
            data.push((rgb[2].clamp(0.0, 1.0) * 255.0) as u8);
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

fn build_cloud_texture(w: u32, h: u32, seed: u32) -> Image {
    let primary = Perlin::new(seed);
    let detail = Perlin::new(seed.wrapping_add(0x9E37_79B9));
    let warp = Perlin::new(seed.wrapping_add(0x51ED_270B));
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let latitude = (y as f64 / h as f64) * std::f64::consts::PI - std::f64::consts::FRAC_PI_2;
        let (sin_lat, cos_lat) = latitude.sin_cos();
        for x in 0..w {
            let longitude = (x as f64 / w as f64) * std::f64::consts::TAU;
            let (sin_lon, cos_lon) = longitude.sin_cos();
            let p = [cos_lat * cos_lon, sin_lat, cos_lat * sin_lon];
            let wind = warp.get([p[0] * 1.4, p[1] * 1.4, p[2] * 1.4]) * 0.34
                + (latitude * 3.0).sin() * 0.08;
            let advected_longitude = longitude + wind;
            let (advected_sin, advected_cos) = advected_longitude.sin_cos();
            let q = [cos_lat * advected_cos, sin_lat, cos_lat * advected_sin];
            let broad = primary.get([q[0] * 1.8, q[1] * 4.6, q[2] * 1.8]) * 0.5 + 0.5;
            let billow = 1.0 - detail.get([q[0] * 4.7, q[1] * 8.2, q[2] * 4.7]).abs();
            let wisps =
                detail.get([q[0] * 9.1 + 11.3, q[1] * 5.4 - 7.8, q[2] * 9.1 + 3.6]) * 0.5 + 0.5;
            let coverage = broad * 0.68 + billow * 0.22 + wisps * 0.10;
            let polar_fade = 1.0 - smoothstep(0.84, 0.99, sin_lat.abs() as f32) as f64 * 0.55;
            let alpha = smoothstep(0.46, 0.72, coverage as f32).powf(1.18) * polar_fade as f32;
            let warmth = (broad * 0.12) as f32;
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
    fn land_and_ocean_masks_are_exclusive_at_every_elevation() {
        for elevation in [0.0, 0.31, 0.499, 0.5, 0.67, 1.0] {
            let masks = land_ocean_masks(elevation);
            assert_eq!(masks.land * masks.ocean, 0.0);
            assert_eq!(masks.land + masks.ocean, 1.0);
            assert!((0.0..=1.0).contains(&masks.shore));
        }
    }

    #[test]
    fn crater_stamp_is_circular_in_sphere_space_with_bowl_and_rim() {
        let angular_radius = 0.12_f64;
        let crater = CraterStamp {
            center: [1.0, 0.0, 0.0],
            radius_chord: 2.0 * (angular_radius * 0.5).sin(),
            strength: 1.0,
        };
        let sample_angle = angular_radius * 0.91;
        let around_y = [sample_angle.cos(), sample_angle.sin(), 0.0];
        let around_z = [sample_angle.cos(), 0.0, sample_angle.sin()];

        assert!(
            (crater_relief_at(around_y, crater) - crater_relief_at(around_z, crater)).abs() < 1e-6
        );
        assert!(crater_relief_at(crater.center, crater) < -0.15);
        assert!(crater_relief_at(around_y, crater) > 0.12);
        assert_eq!(crater_profile(1.5), 0.0);
    }

    #[test]
    fn procedural_surface_and_cloud_fields_are_deterministic_and_separate() {
        let seed = 0xA0_41_19;
        assert_eq!(build_crater_stamps(seed), build_crater_stamps(seed));
        assert_ne!(build_crater_stamps(seed), build_crater_stamps(seed + 1));

        let base_a = build_body_texture(CelestialKind::SakuraPlanet, 48, 24, seed);
        let base_b = build_body_texture(CelestialKind::SakuraPlanet, 48, 24, seed);
        let clouds_a = build_cloud_texture(48, 24, seed.wrapping_add(0xC10D_5EED));
        let clouds_b = build_cloud_texture(48, 24, seed.wrapping_add(0xC10D_5EED));
        assert_eq!(base_a.data, base_b.data);
        assert_eq!(clouds_a.data, clouds_b.data);
        assert!(base_a.data.chunks_exact(4).all(|pixel| pixel[3] == 255));

        let mut cloud_alpha = clouds_a.data.chunks_exact(4).map(|pixel| pixel[3]);
        let first = cloud_alpha.next().expect("cloud texture has pixels");
        let (min_alpha, max_alpha) = cloud_alpha
            .fold((first, first), |(min_alpha, max_alpha), alpha| {
                (min_alpha.min(alpha), max_alpha.max(alpha))
            });
        assert!(min_alpha < max_alpha);
        assert!(max_alpha > 0);
    }

    #[test]
    fn atmosphere_strength_tracks_graphics_quality() {
        let fast = atmosphere_tuning(GraphicsMode::Fast);
        let balanced = atmosphere_tuning(GraphicsMode::Balanced);
        let high = atmosphere_tuning(GraphicsMode::High);

        assert!(fast.opacity_scale < balanced.opacity_scale);
        assert!(balanced.opacity_scale < high.opacity_scale);
        assert!(fast.emissive_scale < balanced.emissive_scale);
        assert!(balanced.emissive_scale < high.emissive_scale);
        assert!(fast.depth_bias > balanced.depth_bias);
        assert!(balanced.depth_bias > high.depth_bias);
    }

    #[test]
    fn atmosphere_presentation_expands_with_apparent_size_and_fades_inside_shell() {
        let radius = 1_000.0;
        let atmosphere_radius = 1_080.0;
        let far = celestial_presentation(30_000.0, radius, atmosphere_radius, GraphicsMode::High);
        let near = celestial_presentation(3_500.0, radius, atmosphere_radius, GraphicsMode::High);
        let inside_shell =
            celestial_presentation(1_015.0, radius, atmosphere_radius, GraphicsMode::High);

        assert!(far.angular_radius_rad < near.angular_radius_rad);
        assert!(far.atmosphere_shell_scale < near.atmosphere_shell_scale);
        assert!(far.atmosphere_shell_scale > radius / atmosphere_radius);
        assert!(near.atmosphere_shell_scale <= 1.0);
        assert!(inside_shell.atmosphere_opacity < near.atmosphere_opacity * 0.2);
        assert!(inside_shell.atmosphere_emissive < near.atmosphere_emissive * 0.2);
        assert!(near.surface_night_fill < far.surface_night_fill);
    }

    #[test]
    fn low_spec_keeps_a_thinner_cheaper_limb_than_high_quality() {
        let fast = celestial_presentation(4_000.0, 1_000.0, 1_100.0, GraphicsMode::Fast);
        let high = celestial_presentation(4_000.0, 1_000.0, 1_100.0, GraphicsMode::High);
        let fast_assets = celestial_asset_policy(GraphicsMode::Fast);
        let balanced_assets = celestial_asset_policy(GraphicsMode::Balanced);
        let high_assets = celestial_asset_policy(GraphicsMode::High);

        assert!(fast.atmosphere_shell_scale < high.atmosphere_shell_scale);
        assert!(fast.surface_night_fill > high.surface_night_fill);
        assert!(fast_assets.body_subdivisions < balanced_assets.body_subdivisions);
        assert!(balanced_assets.body_subdivisions < high_assets.body_subdivisions);
        assert!(fast_assets.texture_size.0 < balanced_assets.texture_size.0);
        assert!(balanced_assets.texture_size.0 < high_assets.texture_size.0);
        assert!(!fast_assets.cloud_layer);
        assert!(balanced_assets.cloud_layer && high_assets.cloud_layer);
        assert!(
            atmosphere_tuning(GraphicsMode::Fast).opacity_scale
                < atmosphere_tuning(GraphicsMode::High).opacity_scale
        );
        assert!(
            atmosphere_tuning(GraphicsMode::Fast).emissive_scale
                < atmosphere_tuning(GraphicsMode::High).emissive_scale
        );

        let fast_clouds = cloud_material_tuning(GraphicsMode::Fast);
        let high_clouds = cloud_material_tuning(GraphicsMode::High);
        assert!(fast_clouds.unlit);
        assert!(!high_clouds.unlit);
        assert!(fast_clouds.emissive > high_clouds.emissive);
        assert!(fast_clouds.alpha < high_clouds.alpha);
    }

    #[test]
    fn atmosphere_texture_has_a_deterministic_sun_facing_hemisphere() {
        let width = 64;
        let height = 32;
        let seed = 0x51A7_2026;
        let first = build_atmosphere_texture(CelestialKind::SakuraPlanet, width, height, seed);
        let second = build_atmosphere_texture(CelestialKind::SakuraPlanet, width, height, seed);
        assert_eq!(first.data, second.data);

        let alpha_at = |x: u32, y: u32| first.data[((y * width + x) * 4 + 3) as usize];
        let day_alpha = alpha_at(0, height / 2);
        let night_alpha = alpha_at(width / 2, height / 2);
        assert!(day_alpha > night_alpha.saturating_add(80));
    }

    #[test]
    fn atmosphere_rotation_aims_local_day_side_at_light() {
        let light = Vec3::new(-0.3, 0.7, 0.5).normalize();
        let rotation = atmosphere_light_rotation(light, 0xA0_41_19);
        assert!((rotation * Vec3::X).dot(light) > 0.9999);
    }

    #[test]
    fn cloud_drift_is_subtle_and_follows_surface_direction() {
        let spec = default_celestial_bodies()[1];
        let body_rate = body_spin_rate(spec);
        let cloud_rate = body_rate * 1.035;
        assert_eq!(body_rate.signum(), cloud_rate.signum());
        assert!(((cloud_rate / body_rate).abs() - 1.035).abs() < 1e-6);

        let body_now = body_visual_rotation(spec, 120.0);
        let body_again = body_visual_rotation(spec, 120.0);
        assert!(body_now.dot(body_again).abs() > 0.99999);

        let period_seconds = std::f64::consts::TAU / body_rate.abs() as f64;
        let much_later = body_visual_rotation(spec, 120.0 + period_seconds * 1_000_000.0);
        assert!(body_now.dot(much_later).abs() > 0.9999);
        assert!(much_later.is_finite());
    }

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
    fn moon_center_uses_the_shared_world_orbit_not_an_observer_position() {
        let moon = default_celestial_bodies()[0];
        let time = 2.75;
        let center = celestial_center(moon, time);
        let expected = crate::daynight::moon_direction_for_time(time);

        assert!(center.normalize().dot(expected) > 0.99999);
        assert!((center.length() - moon.center.length()).abs() < 0.01);
        assert_ne!(center, moon.center);
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
