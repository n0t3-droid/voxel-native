//! Shuttle battles: local voxel ships, cockpit flight, ship weapons and drones.
//!
//! Ships are moving entity hierarchies with smooth meshes for the visible
//! shuttle hulls. They do not mutate terrain chunks while flying, which keeps
//! chunk streaming and the mesher stable.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::blocks::BlockType;
use crate::director::{SimulationDirector, UnifiedTelemetry};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::neurocore::RuntimeProfile;
use crate::player::Player;
use crate::settings::{ActiveWorld, GraphicsMode, WorldSettings};
use crate::world::VoxelWorld;

pub struct ShipPlugin;

impl Plugin for ShipPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ShipInventory::default())
            .insert_resource(ShipPlacementState::default())
            .insert_resource(PilotState::default())
            .insert_resource(ShipBoardingState::default())
            .insert_resource(CockpitTransition::default())
            .insert_resource(ShipFxCache::default())
            .add_systems(OnEnter(GameState::MainMenu), cleanup_ship_runtime)
            .add_systems(OnEnter(GameState::InGame), spawn_saved_ships_once)
            .add_systems(
                Update,
                (
                    ship_placement_input,
                    ship_interaction_input,
                    draw_ship_boarding_hud,
                    update_cockpit_transition,
                    ship_flight_input,
                    update_hero_flyby,
                    update_sky_traffic,
                    update_ship_energy_trails,
                    update_ship_projectiles,
                    spawn_enemy_drones,
                    update_enemy_drones,
                    update_ship_explosions,
                    draw_ship_cockpit_hud,
                )
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ShipKind {
    #[default]
    ScoutShuttle,
    StrikeFighter,
    HeavyDropship,
}

impl ShipKind {
    pub const ALL: [ShipKind; 3] = [
        ShipKind::ScoutShuttle,
        ShipKind::StrikeFighter,
        ShipKind::HeavyDropship,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ShipKind::ScoutShuttle => "Scout Shuttle",
            ShipKind::StrikeFighter => "Strike Fighter",
            ShipKind::HeavyDropship => "Heavy Dropship",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            ShipKind::ScoutShuttle => "SCOUT",
            ShipKind::StrikeFighter => "STRIKE",
            ShipKind::HeavyDropship => "HEAVY",
        }
    }
}

#[derive(Resource, Debug, Clone, Serialize, Deserialize)]
pub struct ShipInventory {
    pub unlocked: Vec<ShipKind>,
    pub selected: ShipKind,
}

impl Default for ShipInventory {
    fn default() -> Self {
        Self {
            unlocked: ShipKind::ALL.to_vec(),
            selected: ShipKind::ScoutShuttle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedShipInstance {
    pub kind: ShipKind,
    pub pos: [f32; 3],
    pub yaw: f32,
    #[serde(default = "default_ship_shield")]
    pub shield: f32,
}

fn default_ship_shield() -> f32 {
    100.0
}

impl SavedShipInstance {
    pub fn from_world(kind: ShipKind, transform: &Transform, shield: f32) -> Self {
        Self {
            kind,
            pos: [
                transform.translation.x,
                transform.translation.y,
                transform.translation.z,
            ],
            yaw: transform.rotation.to_euler(EulerRot::YXZ).0,
            shield,
        }
    }
}

#[derive(Resource, Debug, Clone)]
pub struct ShipPlacementState {
    pub active: bool,
    pub kind: ShipKind,
    pub yaw: f32,
    pub preview: Option<Entity>,
    pub last_pos: Vec3,
    pub status: String,
}

impl Default for ShipPlacementState {
    fn default() -> Self {
        Self {
            active: false,
            kind: ShipKind::ScoutShuttle,
            yaw: 0.0,
            preview: None,
            last_pos: Vec3::ZERO,
            status: "Hangar ready.".into(),
        }
    }
}

impl ShipPlacementState {
    pub fn start(&mut self, kind: ShipKind) {
        self.active = true;
        self.kind = kind;
        self.yaw = 0.0;
        self.status = format!("Placing {}.", kind.label());
    }
}

#[derive(Resource, Debug, Clone)]
pub struct PilotState {
    pub active_ship: Option<Entity>,
    pub weapon: ShipWeaponKind,
    pub speed: f32,
    /// Current hull blueprint max speed — for thrust / boost HUD normalization.
    pub cruise_max_speed: f32,
    pub shield: f32,
    pub shield_flash: f32,
    pub primary_cooldown: f32,
    pub secondary_cooldown: f32,
    pub status: String,
    /// Seconds after cockpit link before enemy drones can spawn / hunt.
    pub entry_peace_timer: f32,
}

impl Default for PilotState {
    fn default() -> Self {
        Self {
            active_ship: None,
            weapon: ShipWeaponKind::IonRocket,
            speed: 0.0,
            cruise_max_speed: 180.0,
            shield: 100.0,
            shield_flash: 0.0,
            primary_cooldown: 0.0,
            secondary_cooldown: 0.0,
            status: "No shuttle linked.".into(),
            entry_peace_timer: 0.0,
        }
    }
}

#[derive(Resource, Debug, Default, Clone)]
struct ShipBoardingState {
    target: Option<Entity>,
    kind: Option<ShipKind>,
    distance: f32,
    lock: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipWeaponKind {
    IonRocket,
    PlasmaFlak,
    RailLance,
}

impl ShipWeaponKind {
    const ALL: [ShipWeaponKind; 3] = [
        ShipWeaponKind::IonRocket,
        ShipWeaponKind::PlasmaFlak,
        ShipWeaponKind::RailLance,
    ];

    fn label(self) -> &'static str {
        match self {
            ShipWeaponKind::IonRocket => "ION ROCKET",
            ShipWeaponKind::PlasmaFlak => "PLASMA FLAK",
            ShipWeaponKind::RailLance => "RAIL LANCE",
        }
    }

    fn color(self) -> Color {
        match self {
            ShipWeaponKind::IonRocket => Color::srgb(0.10, 0.95, 1.00),
            ShipWeaponKind::PlasmaFlak => Color::srgb(1.00, 0.20, 0.86),
            ShipWeaponKind::RailLance => Color::srgb(1.00, 0.65, 0.14),
        }
    }

    fn next(self, delta: i32) -> Self {
        let idx = Self::ALL.iter().position(|w| *w == self).unwrap_or(0) as i32;
        let next = (idx + delta).rem_euclid(Self::ALL.len() as i32) as usize;
        Self::ALL[next]
    }

    fn profile(self) -> WeaponProfile {
        match self {
            ShipWeaponKind::IonRocket => WeaponProfile {
                speed: 150.0,
                damage: 42.0,
                radius: 3.3,
                cooldown: 0.55,
                size: Vec3::new(0.18, 0.18, 1.8),
            },
            ShipWeaponKind::PlasmaFlak => WeaponProfile {
                speed: 110.0,
                damage: 28.0,
                radius: 5.2,
                cooldown: 0.34,
                size: Vec3::new(0.22, 0.22, 1.2),
            },
            ShipWeaponKind::RailLance => WeaponProfile {
                speed: 260.0,
                damage: 70.0,
                radius: 1.8,
                cooldown: 0.82,
                size: Vec3::new(0.12, 0.12, 2.8),
            },
        }
    }
}

struct WeaponProfile {
    speed: f32,
    damage: f32,
    radius: f32,
    cooldown: f32,
    size: Vec3,
}

#[derive(Component, Debug, Clone)]
pub struct ShipInstance {
    pub kind: ShipKind,
    pub shield: f32,
}

#[derive(Component, Debug, Clone)]
struct ShipMotion {
    yaw: f32,
    pitch: f32,
    roll: f32,
    speed: f32,
    /// Smoothed angular rates (rad/s) used to ease mouse + keyboard input so
    /// the shuttle never snaps from frame to frame.
    yaw_rate: f32,
    pitch_rate: f32,
    /// Smoothed lateral velocity component (m/s) for inertial banking drift.
    lateral_speed: f32,
}

/// Cinematic shuttle pass that loops across the spawn postcard. Not
/// player-piloted unless they board it; engine plumes stay lit because
/// `speed` is held in cruise.
#[derive(Component, Debug, Clone)]
struct HeroFlyby {
    t: f32,
    origin: Vec3,
}

/// Distant looping aerial traffic. Cheap cuboid drones with one trail;
/// count is capped by graphics tier and the path wraps, so the set never
/// grows.
#[derive(Component, Debug, Clone)]
struct SkyTraffic {
    t: f32,
    speed: f32,
    origin: Vec3,
    span: Vec3,
    scale: f32,
}

/// World-unit scale for the New-World hero pass. Sized so a ~8-unit
/// shuttle reads above the scenic look target without filling the frame.
const HERO_FLYBY_SCALE: f32 = 2.85;

const SHIP_MOUSE_YAW_SENS: f32 = 0.00016;
const SHIP_MOUSE_PITCH_SENS: f32 = 0.00048;
const SHIP_KEY_YAW_RATE: f32 = 0.42;
const SHIP_KEY_PITCH_RATE: f32 = 0.56;
const SHIP_YAW_RATE_LIMIT: f32 = 0.95;
const SHIP_PITCH_RATE_LIMIT: f32 = 0.80;
const SHIP_TARGET_ROLL: f32 = 0.32;
const SHIP_BANK_YAW_RATE: f32 = 0.14;
const SHIP_RUDDER_YAW_RATE: f32 = 0.12;

fn ship_target_angular_rates(
    mouse_dx: f32,
    mouse_dy: f32,
    turn_input: f32,
    pitch_input: f32,
    dt: f32,
) -> (f32, f32) {
    let inv_dt = if dt > 1e-4 { 1.0 / dt } else { 60.0 };
    let mouse_rate_scale = inv_dt.min(90.0);
    let yaw = (-mouse_dx * SHIP_MOUSE_YAW_SENS) * mouse_rate_scale + turn_input * SHIP_KEY_YAW_RATE;
    let pitch =
        (-mouse_dy * SHIP_MOUSE_PITCH_SENS) * mouse_rate_scale + pitch_input * SHIP_KEY_PITCH_RATE;
    (
        yaw.clamp(-SHIP_YAW_RATE_LIMIT, SHIP_YAW_RATE_LIMIT),
        pitch.clamp(-SHIP_PITCH_RATE_LIMIT, SHIP_PITCH_RATE_LIMIT),
    )
}

#[derive(Component)]
struct ShipPreview;

#[derive(Component)]
struct EnemyDrone {
    hp: f32,
    fire_cooldown: f32,
    orbit: f32,
    /// Smoothed translational velocity (m/s) — used to add inertia so drones
    /// glide on a curve toward intercept points instead of teleporting.
    velocity: Vec3,
}

#[derive(Component)]
struct ShipProjectile {
    owner: ProjectileOwner,
    velocity: Vec3,
    damage: f32,
    radius: f32,
    life: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectileOwner {
    Player,
    Drone,
}

#[derive(Component)]
struct ShipExplosion {
    life: f32,
    max_life: f32,
}

#[derive(Component)]
struct ShipEnergyTrail {
    base_translation: Vec3,
    base_scale: Vec3,
    phase: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipTrailTone {
    Cyan,
    Amber,
}

#[derive(Debug, Clone, Copy)]
struct ShipTrailSpec {
    base_translation: Vec3,
    base_scale: Vec3,
    phase: f32,
    tone: ShipTrailTone,
}

#[derive(Debug, Clone, Copy)]
struct ShipWaveResponse {
    vertical_velocity: f32,
    pitch: f32,
    roll: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CockpitPanelTone {
    Cyan,
    Magenta,
    Amber,
    Shell,
    Seat,
    Frame,
    Glass,
}

#[derive(Debug, Clone, Copy)]
struct CockpitPanelSpec {
    offset: Vec3,
    scale: Vec3,
    rotation: Quat,
    tone: CockpitPanelTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealShipMeshKind {
    SmoothEllipsoid,
    RoundNozzle,
    AeroPlate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RealShipTone {
    CeramicWhite,
    ShuttleWhite,
    ShuttleOrange,
    CarbonBlack,
    SmokedGlass,
    CyanEmission,
    AmberHeat,
    LuminiteGlass,
    MagentaSignal,
    SeatLeather,
    ConsoleBlack,
}

#[derive(Debug, Clone, Copy)]
struct RealShipPartSpec {
    mesh: RealShipMeshKind,
    tone: RealShipTone,
    offset: Vec3,
    scale: Vec3,
    rotation: Quat,
}

#[derive(Resource, Debug, Clone)]
struct CockpitTransition {
    active: bool,
    ship: Option<Entity>,
    timer: f32,
    duration: f32,
    from: Transform,
    to: Transform,
}

impl Default for CockpitTransition {
    fn default() -> Self {
        Self {
            active: false,
            ship: None,
            timer: 0.0,
            duration: 0.46,
            from: Transform::default(),
            to: Transform::default(),
        }
    }
}

impl CockpitTransition {
    fn start(&mut self, ship: Entity, from: Transform, to: Transform) {
        self.active = true;
        self.ship = Some(ship);
        self.timer = 0.0;
        self.from = from;
        self.to = to;
    }

    fn clear(&mut self) {
        self.active = false;
        self.ship = None;
        self.timer = 0.0;
    }
}

#[derive(Default, Resource)]
struct ShipFxCache {
    cube: Option<Handle<Mesh>>,
    real_sphere: Option<Handle<Mesh>>,
    real_cylinder: Option<Handle<Mesh>>,
    real_plate: Option<Handle<Mesh>>,
    projectile: Option<Handle<Mesh>>,
    explosion: Option<Handle<Mesh>>,
    real_mats: std::collections::HashMap<(RealShipTone, bool), Handle<StandardMaterial>>,
    projectile_mats: std::collections::HashMap<u8, Handle<StandardMaterial>>,
    cockpit_mats: std::collections::HashMap<u8, Handle<StandardMaterial>>,
    drone_mat: Option<Handle<StandardMaterial>>,
}

#[derive(Clone)]
struct ShipBlueprint {
    #[allow(dead_code)]
    voxels: Vec<ShipVoxel>,
    cockpit_offset: Vec3,
    exit_offset: Vec3,
    hardpoints: [Vec3; 2],
    hull_radius: f32,
    max_speed: f32,
    accel: f32,
    shield: f32,
}

#[derive(Clone, Copy)]
struct ShipVoxel {
    pos: IVec3,
    block: BlockType,
}

fn blueprint(kind: ShipKind) -> ShipBlueprint {
    match kind {
        ShipKind::ScoutShuttle => scout_blueprint(),
        ShipKind::StrikeFighter => strike_blueprint(),
        ShipKind::HeavyDropship => dropship_blueprint(),
    }
}

fn scout_blueprint() -> ShipBlueprint {
    let mut voxels = Vec::new();

    // White/orange orbiter that matches the visible cuboid hull:
    // pointed nose (−Z), swept wings, cyan glass strip, twin engines.
    push_box(
        &mut voxels,
        IVec3::new(-2, -1, -6),
        IVec3::new(2, -1, 6),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-2, 0, -6),
        IVec3::new(2, 0, 6),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 0, -8),
        IVec3::new(1, 2, 8),
        BlockType::PlatingWhite,
    );
    push_box(
        &mut voxels,
        IVec3::new(-2, 1, -4),
        IVec3::new(2, 2, 6),
        BlockType::PlatingWhite,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 0, -12),
        IVec3::new(1, 1, -9),
        BlockType::PlatingWhite,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 1, -13),
        IVec3::new(0, 1, -13),
        BlockType::PlatingWhite,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 3, -9),
        IVec3::new(1, 3, -5),
        BlockType::CockpitGlass,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 3, -2),
        IVec3::new(0, 3, 8),
        BlockType::NeonAmber,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, -1, -4),
        IVec3::new(1, -1, 6),
        BlockType::NeonAmber,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 3, 6),
        IVec3::new(0, 6, 8),
        BlockType::PlatingWhite,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 6, 6),
        IVec3::new(0, 6, 8),
        BlockType::NeonAmber,
    );
    for &sx in &[-1, 1] {
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, 1, -1),
            IVec3::new(sx * 8, 1, 3),
            BlockType::PlatingWhite,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 6, 1, -2),
            IVec3::new(sx * 9, 1, 0),
            BlockType::NeonAmber,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 1, 1, 10),
            IVec3::new(sx * 2, 2, 12),
            BlockType::EngineCore,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 1, 1, 9),
            IVec3::new(sx * 2, 2, 9),
            BlockType::ShipHullDark,
        );
    }
    ShipBlueprint {
        voxels,
        cockpit_offset: Vec3::new(0.0, 2.6, -3.8),
        exit_offset: Vec3::new(2.8, 0.5, 1.5),
        hardpoints: [Vec3::new(-2.8, 0.6, -3.3), Vec3::new(2.8, 0.6, -3.3)],
        hull_radius: 6.0,
        max_speed: 82.0,
        accel: 44.0,
        shield: 90.0,
    }
}

fn strike_blueprint() -> ShipBlueprint {
    let mut voxels = Vec::new();

    // -- Original space-opera interceptor silhouette --

    // Central spherical-ish pod
    push_box(
        &mut voxels,
        IVec3::new(-2, -2, -2),
        IVec3::new(2, 2, 3),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, -3, -1),
        IVec3::new(1, 3, 2),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, -1, -3),
        IVec3::new(1, 1, -3),
        BlockType::CockpitGlass,
    ); // Window
    push_box(
        &mut voxels,
        IVec3::new(-1, -1, 4),
        IVec3::new(1, 1, 4),
        BlockType::EngineCore,
    ); // Rear engine

    // Wing Pylons connecting pod to solar arrays
    push_box(
        &mut voxels,
        IVec3::new(-6, 0, 0),
        IVec3::new(6, 0, 1),
        BlockType::ShipHullAlloy,
    );

    // Solar array wings (Dagger shaped)
    for &sx in &[-7, 7] {
        let inside = if sx < 0 { sx + 1 } else { sx - 1 };

        // Wing central hub
        push_box(
            &mut voxels,
            IVec3::new(inside, -1, -1),
            IVec3::new(sx, 1, 2),
            BlockType::ShipHullAlloy,
        );

        // Top chevron
        push_box(
            &mut voxels,
            IVec3::new(sx, 1, -1),
            IVec3::new(sx, 5, 5),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx, 1, -4),
            IVec3::new(sx, 2, -1),
            BlockType::ShipHullDark,
        ); // Forward point

        // Bottom chevron
        push_box(
            &mut voxels,
            IVec3::new(sx, -4, -1),
            IVec3::new(sx, -1, 5),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx, -2, -4),
            IVec3::new(sx, -1, -1),
            BlockType::ShipHullDark,
        ); // Forward point

        // Laser cannons on wing tips
        push_box(
            &mut voxels,
            IVec3::new(sx, 5, -5),
            IVec3::new(sx, 5, -3),
            BlockType::NeonCyan,
        ); // Green lasers firing
        push_box(
            &mut voxels,
            IVec3::new(sx, -4, -5),
            IVec3::new(sx, -4, -3),
            BlockType::NeonCyan,
        );
    }

    add_strike_realism(&mut voxels);
    add_future_wave_shuttle_skin(&mut voxels, ShipKind::StrikeFighter);
    ShipBlueprint {
        voxels,
        cockpit_offset: Vec3::new(0.0, 0.0, -3.5),
        exit_offset: Vec3::new(0.0, -3.5, 0.0),
        hardpoints: [Vec3::new(-7.0, 5.0, -5.0), Vec3::new(7.0, -4.0, -5.0)],
        hull_radius: 8.0,
        max_speed: 105.0,
        accel: 58.0,
        shield: 115.0,
    }
}

fn dropship_blueprint() -> ShipBlueprint {
    let mut voxels = Vec::new();

    // -- Original space-opera gunship silhouette --

    // Troop Bay / Main Fuselage
    push_box(
        &mut voxels,
        IVec3::new(-3, -1, -2),
        IVec3::new(3, 3, 5),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-3, -1, 4),
        IVec3::new(3, 3, 6),
        BlockType::ShipHullDark,
    ); // rear cargo door

    // Side sliding doors (dark hull indents)
    push_box(
        &mut voxels,
        IVec3::new(-4, 0, 0),
        IVec3::new(-3, 2, 3),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(3, 0, 0),
        IVec3::new(4, 2, 3),
        BlockType::ShipHullDark,
    );

    // Front Nose Section
    push_box(
        &mut voxels,
        IVec3::new(-1, 0, -8),
        IVec3::new(1, 2, -3),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-2, 0, -6),
        IVec3::new(2, 1, -3),
        BlockType::ShipHullAlloy,
    );

    // Tandem Double Cockpits (Stepped up)
    // Pilot cockpit
    push_box(
        &mut voxels,
        IVec3::new(-1, 3, -6),
        IVec3::new(1, 4, -4),
        BlockType::CockpitGlass,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 2, -6),
        IVec3::new(1, 2, -4),
        BlockType::ShipHullDark,
    );
    // Gunner cockpit
    push_box(
        &mut voxels,
        IVec3::new(-1, 5, -3),
        IVec3::new(1, 5, -1),
        BlockType::CockpitGlass,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 4, -3),
        IVec3::new(1, 4, -1),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 3, -3),
        IVec3::new(1, 3, -1),
        BlockType::ShipHullDark,
    );

    // Top Spine structure
    push_box(
        &mut voxels,
        IVec3::new(-1, 4, 0),
        IVec3::new(1, 4, 7),
        BlockType::ShipHullAlloy,
    );

    // High Wings extending directly out
    push_box(
        &mut voxels,
        IVec3::new(-8, 3, 1),
        IVec3::new(8, 3, 4),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-8, 3, 1),
        IVec3::new(-7, 3, 4),
        BlockType::ShipHullDark,
    ); // Wing tips
    push_box(
        &mut voxels,
        IVec3::new(7, 3, 1),
        IVec3::new(8, 3, 4),
        BlockType::ShipHullDark,
    );

    // Wing missile launchers / pods
    push_box(
        &mut voxels,
        IVec3::new(-8, 2, 2),
        IVec3::new(-7, 2, 3),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(7, 2, 2),
        IVec3::new(8, 2, 3),
        BlockType::ShipHullDark,
    );

    // Top Engines mounted high above wings at back
    for &sx in &[-2, 2] {
        push_box(
            &mut voxels,
            IVec3::new(sx - 1, 5, 2),
            IVec3::new(sx + 1, 6, 7),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx - 1, 5, 8),
            IVec3::new(sx + 1, 6, 8),
            BlockType::EngineCore,
        );
    }

    // Front chin ball turrets
    push_box(
        &mut voxels,
        IVec3::new(-2, -1, -8),
        IVec3::new(-2, -1, -7),
        BlockType::NeonCyan,
    ); // Laser port
    push_box(
        &mut voxels,
        IVec3::new(2, -1, -8),
        IVec3::new(2, -1, -7),
        BlockType::NeonCyan,
    );

    add_dropship_realism(&mut voxels);
    add_future_wave_shuttle_skin(&mut voxels, ShipKind::HeavyDropship);
    ShipBlueprint {
        voxels,
        cockpit_offset: Vec3::new(0.0, 4.0, -5.5),
        exit_offset: Vec3::new(4.5, 0.5, 0.0),
        hardpoints: [Vec3::new(-4.6, 1.5, -3.5), Vec3::new(4.6, 1.5, -3.5)],
        hull_radius: 9.5,
        max_speed: 64.0,
        accel: 34.0,
        shield: 170.0,
    }
}

fn add_strike_realism(voxels: &mut Vec<ShipVoxel>) {
    // Faceted armored cockpit, visible instrument well and wing heat-striping.
    push_box(
        voxels,
        IVec3::new(-2, -2, -4),
        IVec3::new(2, 2, -4),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-1, -1, -4),
        IVec3::new(1, 1, -4),
        BlockType::CockpitGlass,
    );
    push_box(
        voxels,
        IVec3::new(0, -1, -4),
        IVec3::new(0, 1, -4),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-1, -2, -3),
        IVec3::new(1, -2, -2),
        BlockType::NeonAmber,
    );
    push_box(
        voxels,
        IVec3::new(-3, -1, -2),
        IVec3::new(3, 1, -2),
        BlockType::ShipHullAlloy,
    );
    push_box(
        voxels,
        IVec3::new(-2, -2, 5),
        IVec3::new(2, 2, 5),
        BlockType::EngineCore,
    );
    for sx in [-1, 1] {
        let x = sx * 7;
        push_box(
            voxels,
            IVec3::new(x, 4, -4),
            IVec3::new(x, 5, 5),
            BlockType::NeonCyan,
        );
        push_box(
            voxels,
            IVec3::new(x, -4, -4),
            IVec3::new(x, -3, 5),
            BlockType::NeonMagenta,
        );
        push_box(
            voxels,
            IVec3::new(sx * 5, 1, -3),
            IVec3::new(sx * 6, 1, -1),
            BlockType::ShipHullAlloy,
        );
        push_box(
            voxels,
            IVec3::new(sx * 8, 0, 3),
            IVec3::new(sx * 8, 0, 5),
            BlockType::NeonAmber,
        );
    }
}

fn add_dropship_realism(voxels: &mut Vec<ShipVoxel>) {
    // Heavy armored glazing, troop-bay doors, landing gear and engine detail.
    for sx in [-1, 1] {
        push_box(
            voxels,
            IVec3::new(sx * 2, 3, -6),
            IVec3::new(sx * 2, 5, -1),
            BlockType::ShipHullDark,
        );
        push_box(
            voxels,
            IVec3::new(sx * 4, 1, -1),
            IVec3::new(sx * 4, 2, 3),
            BlockType::NeonCyan,
        );
        push_box(
            voxels,
            IVec3::new(sx * 5, -2, -1),
            IVec3::new(sx * 5, -2, 4),
            BlockType::ShipHullDark,
        );
        push_box(
            voxels,
            IVec3::new(sx * 6, 2, 5),
            IVec3::new(sx * 6, 3, 8),
            BlockType::ShipHullDark,
        );
        push_box(
            voxels,
            IVec3::new(sx * 6, 2, 9),
            IVec3::new(sx * 6, 3, 9),
            BlockType::EngineCore,
        );
    }
    push_box(
        voxels,
        IVec3::new(-2, 5, -6),
        IVec3::new(2, 5, -5),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-1, 3, -7),
        IVec3::new(1, 3, -7),
        BlockType::CockpitGlass,
    );
    push_box(
        voxels,
        IVec3::new(-1, 4, -4),
        IVec3::new(1, 4, -4),
        BlockType::NeonAmber,
    );
    push_box(
        voxels,
        IVec3::new(-2, -2, -7),
        IVec3::new(2, -2, -6),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-3, 0, 7),
        IVec3::new(3, 3, 7),
        BlockType::EngineCore,
    );
}

fn add_future_wave_shuttle_skin(voxels: &mut Vec<ShipVoxel>, kind: ShipKind) {
    // Reference-video traits: bright shuttle skin, smoked opaque cockpit nose,
    // dual cyan exhaust sources and warm heat panels around the rear body.
    push_box(
        voxels,
        IVec3::new(-2, 0, -8),
        IVec3::new(2, 1, 6),
        BlockType::ShipHullAlloy,
    );
    push_box(
        voxels,
        IVec3::new(-3, 1, -4),
        IVec3::new(3, 2, 4),
        BlockType::ShipHullAlloy,
    );
    push_box(
        voxels,
        IVec3::new(-1, 2, -10),
        IVec3::new(1, 2, -8),
        BlockType::ShipHullAlloy,
    );
    push_box(
        voxels,
        IVec3::new(-3, 1, -8),
        IVec3::new(3, 2, -5),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-2, 2, -9),
        IVec3::new(2, 3, -5),
        BlockType::CockpitGlass,
    );
    push_box(
        voxels,
        IVec3::new(-1, 1, -10),
        IVec3::new(1, 1, -9),
        BlockType::CockpitGlass,
    );
    push_box(
        voxels,
        IVec3::new(-3, 0, 6),
        IVec3::new(3, 2, 7),
        BlockType::ShipHullDark,
    );
    for sx in [-1, 1] {
        push_box(
            voxels,
            IVec3::new(sx * 2, 0, 8),
            IVec3::new(sx * 3, 1, 9),
            BlockType::EngineCore,
        );
        push_box(
            voxels,
            IVec3::new(sx * 3, 0, 5),
            IVec3::new(sx * 4, 1, 7),
            BlockType::NeonAmber,
        );
        push_box(
            voxels,
            IVec3::new(sx * 2, 2, 4),
            IVec3::new(sx * 3, 4, 7),
            BlockType::ShipHullAlloy,
        );
        push_box(
            voxels,
            IVec3::new(sx * 3, 0, -1),
            IVec3::new(sx * 8, 0, 3),
            BlockType::ShipHullAlloy,
        );
        push_box(
            voxels,
            IVec3::new(sx * 7, 0, -2),
            IVec3::new(sx * 9, 0, 1),
            BlockType::ShipHullDark,
        );
        push_box(
            voxels,
            IVec3::new(sx * 5, 1, -1),
            IVec3::new(sx * 8, 1, -1),
            BlockType::NeonCyan,
        );
        push_box(
            voxels,
            IVec3::new(sx * 2, 3, -8),
            IVec3::new(sx * 2, 3, -6),
            BlockType::LuminiteCrystal,
        );
        push_box(
            voxels,
            IVec3::new(sx * 4, 2, -2),
            IVec3::new(sx * 4, 2, 0),
            BlockType::LuminiteCrystal,
        );
        push_box(
            voxels,
            IVec3::new(sx * 3, 3, 5),
            IVec3::new(sx * 3, 3, 7),
            BlockType::NeonMagenta,
        );
        push_box(
            voxels,
            IVec3::new(sx * 6, 1, 2),
            IVec3::new(sx * 7, 1, 2),
            BlockType::NeonMagenta,
        );
    }

    match kind {
        ShipKind::ScoutShuttle => {
            // Overwrite the dark X-wing skin with a chunky white/orange
            // orbiter so a 3× flyby still reads as a shuttle, not a
            // grey speck or a hull wall.
            push_box(
                voxels,
                IVec3::new(-3, 0, -8),
                IVec3::new(3, 3, 5),
                BlockType::PlatingWhite,
            );
            push_box(
                voxels,
                IVec3::new(-5, 1, -2),
                IVec3::new(5, 2, 3),
                BlockType::PlatingWhite,
            );
            push_box(
                voxels,
                IVec3::new(-2, -1, -6),
                IVec3::new(2, -1, 4),
                BlockType::NeonAmber,
            );
            push_box(
                voxels,
                IVec3::new(-3, 1, 6),
                IVec3::new(3, 3, 9),
                BlockType::NeonAmber,
            );
            push_box(
                voxels,
                IVec3::new(-1, 2, -9),
                IVec3::new(1, 3, -6),
                BlockType::CockpitGlass,
            );
        }
        ShipKind::StrikeFighter => {
            push_box(
                voxels,
                IVec3::new(-1, -2, -5),
                IVec3::new(1, -1, 3),
                BlockType::ShipHullDark,
            );
            for sx in [-1, 1] {
                push_box(
                    voxels,
                    IVec3::new(sx * 6, -3, -2),
                    IVec3::new(sx * 8, 3, 5),
                    BlockType::ShipHullDark,
                );
                push_box(
                    voxels,
                    IVec3::new(sx * 6, -2, -1),
                    IVec3::new(sx * 6, 2, 4),
                    BlockType::NeonCyan,
                );
            }
        }
        ShipKind::HeavyDropship => {
            push_box(
                voxels,
                IVec3::new(-4, -1, -3),
                IVec3::new(4, 3, 7),
                BlockType::ShipHullAlloy,
            );
            push_box(
                voxels,
                IVec3::new(-4, 0, -1),
                IVec3::new(4, 2, 5),
                BlockType::ShipHullDark,
            );
            push_box(
                voxels,
                IVec3::new(-3, 1, -8),
                IVec3::new(3, 3, -5),
                BlockType::CockpitGlass,
            );
            for sx in [-1, 1] {
                push_box(
                    voxels,
                    IVec3::new(sx * 5, 2, 0),
                    IVec3::new(sx * 9, 3, 5),
                    BlockType::ShipHullAlloy,
                );
                push_box(
                    voxels,
                    IVec3::new(sx * 3, 1, 8),
                    IVec3::new(sx * 4, 2, 10),
                    BlockType::EngineCore,
                );
            }
        }
    }

    for sx in [-1, 1] {
        push_box(
            voxels,
            IVec3::new(sx * 3, 2, -3),
            IVec3::new(sx * 3, 2, -3),
            BlockType::LuminiteCrystal,
        );
        push_box(
            voxels,
            IVec3::new(sx * 5, 1, 1),
            IVec3::new(sx * 5, 1, 1),
            BlockType::LuminiteCrystal,
        );
        push_box(
            voxels,
            IVec3::new(sx * 7, 0, 3),
            IVec3::new(sx * 7, 0, 3),
            BlockType::LuminiteCrystal,
        );
        push_box(
            voxels,
            IVec3::new(sx * 2, 2, 6),
            IVec3::new(sx * 2, 2, 6),
            BlockType::NeonMagenta,
        );
        push_box(
            voxels,
            IVec3::new(sx * 4, 0, 4),
            IVec3::new(sx * 4, 0, 4),
            BlockType::NeonMagenta,
        );
    }
}

fn push_box(out: &mut Vec<ShipVoxel>, min: IVec3, max: IVec3, block: BlockType) {
    let lo = IVec3::new(min.x.min(max.x), min.y.min(max.y), min.z.min(max.z));
    let hi = IVec3::new(min.x.max(max.x), min.y.max(max.y), min.z.max(max.z));
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let pos = IVec3::new(x, y, z);
                if let Some(existing) = out.iter_mut().find(|v| v.pos == pos) {
                    existing.block = block;
                } else {
                    out.push(ShipVoxel { pos, block });
                }
            }
        }
    }
}

fn scout_shuttle_exterior_specs() -> Vec<RealShipPartSpec> {
    let mut parts = Vec::with_capacity(18);
    let identity = Quat::IDENTITY;
    // Blocky orbiter: pointed nose, rectangular fuselage, swept wings,
    // vertical tail, cyan glass strip, orange livery, twin rear glow.
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleWhite,
        Vec3::new(0.0, 0.28, 0.15),
        Vec3::new(1.35, 0.78, 7.2),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleWhite,
        Vec3::new(0.0, 0.22, -4.05),
        Vec3::new(0.92, 0.52, 1.85),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleWhite,
        Vec3::new(0.0, 0.16, -5.35),
        Vec3::new(0.42, 0.28, 0.95),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CyanEmission,
        Vec3::new(0.0, 0.72, -3.15),
        Vec3::new(1.05, 0.18, 1.85),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleOrange,
        Vec3::new(0.0, 0.72, 0.55),
        Vec3::new(0.28, 0.14, 5.6),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleOrange,
        Vec3::new(0.0, -0.18, 0.20),
        Vec3::new(1.12, 0.12, 6.4),
        identity,
    );
    for sx in [-1.0, 1.0] {
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::ShuttleWhite,
            Vec3::new(sx * 2.85, 0.08, 0.55),
            Vec3::new(4.35, 0.14, 2.15),
            Quat::from_rotation_y(-sx * 0.16),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::ShuttleOrange,
            Vec3::new(sx * 3.15, 0.12, -0.55),
            Vec3::new(3.85, 0.10, 0.42),
            Quat::from_rotation_y(-sx * 0.16),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CarbonBlack,
            Vec3::new(sx * 0.52, 0.18, 3.95),
            Vec3::new(0.38, 0.72, 0.38),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CyanEmission,
            Vec3::new(sx * 0.52, 0.18, 4.38),
            Vec3::new(0.26, 0.22, 0.26),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::AmberHeat,
            Vec3::new(sx * 0.52, 0.18, 4.22),
            Vec3::new(0.16, 0.14, 0.16),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
    }
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleWhite,
        Vec3::new(0.0, 1.45, 2.55),
        Vec3::new(0.14, 1.85, 1.35),
        identity,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ShuttleOrange,
        Vec3::new(0.0, 2.28, 2.52),
        Vec3::new(0.16, 0.32, 1.12),
        identity,
    );
    parts
}

fn realistic_ship_exterior_specs(kind: ShipKind) -> Vec<RealShipPartSpec> {
    if kind == ShipKind::ScoutShuttle {
        return scout_shuttle_exterior_specs();
    }
    let mut parts = Vec::with_capacity(24);
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::CeramicWhite,
        Vec3::new(0.0, 0.55, -0.8),
        Vec3::new(2.55, 0.82, 7.8),
        Quat::IDENTITY,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::CeramicWhite,
        Vec3::new(0.0, 0.86, -6.25),
        Vec3::new(1.45, 0.55, 2.65),
        Quat::IDENTITY,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::CarbonBlack,
        Vec3::new(0.0, 0.08, -0.9),
        Vec3::new(1.92, 0.22, 6.6),
        Quat::IDENTITY,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::SmokedGlass,
        Vec3::new(0.0, 1.42, -4.95),
        Vec3::new(1.45, 0.38, 1.82),
        Quat::from_rotation_x(-0.05),
    );
    for sx in [-1.0, 1.0] {
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::CeramicWhite,
            Vec3::new(sx * 4.55, 0.38, 0.05),
            Vec3::new(5.35, 0.12, 2.55),
            Quat::from_rotation_y(-sx * 0.14) * Quat::from_rotation_z(sx * 0.04),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::CarbonBlack,
            Vec3::new(sx * 7.12, 0.52, -0.24),
            Vec3::new(1.18, 0.14, 2.25),
            Quat::from_rotation_y(-sx * 0.18),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::CeramicWhite,
            Vec3::new(sx * 1.65, 2.20, 4.75),
            Vec3::new(0.18, 1.64, 2.42),
            Quat::from_rotation_z(sx * 0.25),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CarbonBlack,
            Vec3::new(sx * 1.55, 0.48, 6.95),
            Vec3::new(0.55, 1.22, 0.55),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CyanEmission,
            Vec3::new(sx * 1.55, 0.48, 7.30),
            Vec3::new(0.36, 0.38, 0.36),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::LuminiteGlass,
            Vec3::new(sx * 3.20, 0.98, -1.72),
            Vec3::new(1.85, 0.035, 0.13),
            Quat::from_rotation_y(-sx * 0.14),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::MagentaSignal,
            Vec3::new(sx * 2.22, 1.02, 4.48),
            Vec3::new(0.86, 0.035, 0.13),
            Quat::from_rotation_y(sx * 0.08),
        );
    }
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::AmberHeat,
        Vec3::new(0.0, 0.34, 6.15),
        Vec3::new(2.35, 0.48, 1.05),
        Quat::IDENTITY,
    );

    match kind {
        ShipKind::ScoutShuttle => {}
        ShipKind::StrikeFighter => {
            for sx in [-1.0, 1.0] {
                push_real_part(
                    &mut parts,
                    RealShipMeshKind::AeroPlate,
                    RealShipTone::CarbonBlack,
                    Vec3::new(sx * 7.85, 0.08, 1.30),
                    Vec3::new(0.28, 4.90, 4.70),
                    Quat::from_rotation_z(sx * 0.06),
                );
                push_real_part(
                    &mut parts,
                    RealShipMeshKind::AeroPlate,
                    RealShipTone::CyanEmission,
                    Vec3::new(sx * 7.70, 0.10, 0.82),
                    Vec3::new(0.035, 3.20, 2.90),
                    Quat::IDENTITY,
                );
            }
        }
        ShipKind::HeavyDropship => {
            push_real_part(
                &mut parts,
                RealShipMeshKind::SmoothEllipsoid,
                RealShipTone::CeramicWhite,
                Vec3::new(0.0, 0.55, 1.45),
                Vec3::new(3.75, 1.18, 5.9),
                Quat::IDENTITY,
            );
            for sx in [-1.0, 1.0] {
                push_real_part(
                    &mut parts,
                    RealShipMeshKind::RoundNozzle,
                    RealShipTone::CyanEmission,
                    Vec3::new(sx * 3.2, 0.74, 7.82),
                    Vec3::new(0.52, 0.54, 0.52),
                    Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                );
            }
        }
    }
    parts
}

fn realistic_cockpit_part_specs(kind: ShipKind, bp: &ShipBlueprint) -> Vec<RealShipPartSpec> {
    let mut parts = Vec::with_capacity(14);
    let scale = (bp.hull_radius / 8.0).clamp(0.86, 1.30);
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::SmokedGlass,
        Vec3::new(0.0, 0.78, -1.18),
        Vec3::new(1.78 * scale, 0.34, 0.78),
        Quat::from_rotation_x(0.06),
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::SmoothEllipsoid,
        RealShipTone::SeatLeather,
        Vec3::new(0.0, -1.25, 0.68),
        Vec3::new(0.62, 0.28, 0.72),
        Quat::IDENTITY,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::SeatLeather,
        Vec3::new(0.0, -0.70, 1.05),
        Vec3::new(0.92, 0.92, 0.14),
        Quat::from_rotation_x(0.22),
    );
    for sx in [-1.0, 1.0] {
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::ConsoleBlack,
            Vec3::new(sx * 1.28, -0.82, -0.58),
            Vec3::new(0.94, 0.08, 0.82),
            Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(-sx * 0.12),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CarbonBlack,
            Vec3::new(sx * 0.46, -0.58, -0.46),
            Vec3::new(0.055, 0.62, 0.055),
            Quat::from_rotation_x(0.85) * Quat::from_rotation_z(sx * 0.36),
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CyanEmission,
            Vec3::new(sx * 1.25, -0.68, -0.86),
            Vec3::new(0.075, 0.28, 0.075),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
    }
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ConsoleBlack,
        Vec3::new(0.0, -0.88, -1.08),
        Vec3::new(2.85, 0.10, 1.16),
        Quat::from_rotation_x(-0.42),
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CyanEmission,
        Vec3::new(0.0, -0.76, -1.23),
        Vec3::new(2.18, 0.035, 0.52),
        Quat::from_rotation_x(-0.42),
    );
    if matches!(kind, ShipKind::HeavyDropship) {
        for sx in [-1.0, 1.0] {
            push_real_part(
                &mut parts,
                RealShipMeshKind::SmoothEllipsoid,
                RealShipTone::SeatLeather,
                Vec3::new(sx * 0.56, -1.18, 1.28),
                Vec3::new(0.46, 0.24, 0.54),
                Quat::IDENTITY,
            );
        }
    }
    parts
}

fn push_real_part(
    parts: &mut Vec<RealShipPartSpec>,
    mesh: RealShipMeshKind,
    tone: RealShipTone,
    offset: Vec3,
    scale: Vec3,
    rotation: Quat,
) {
    parts.push(RealShipPartSpec {
        mesh,
        tone,
        offset,
        scale,
        rotation,
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_ship_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    _images: &mut Assets<Image>,
    fx: &mut ShipFxCache,
    kind: ShipKind,
    pos: Vec3,
    yaw: f32,
    preview: bool,
    shield_override: Option<f32>,
) -> Entity {
    let bp = blueprint(kind);
    let root = commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(pos)
                    .with_rotation(Quat::from_rotation_y(yaw)),
                ..default()
            },
            Name::new(if preview {
                "RealShipPlacementPreview"
            } else {
                "RealisticShuttle"
            }),
        ))
        .id();

    if preview {
        commands.entity(root).insert(ShipPreview);
    } else {
        commands.entity(root).insert((
            ShipInstance {
                kind,
                shield: shield_override.unwrap_or(bp.shield),
            },
            ShipMotion {
                yaw,
                pitch: 0.0,
                roll: 0.0,
                speed: 0.0,
                yaw_rate: 0.0,
                pitch_rate: 0.0,
                lateral_speed: 0.0,
            },
        ));
    }

    let cube = fx
        .cube
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    commands.entity(root).with_children(|p| {
        spawn_realistic_ship_exterior(p, meshes, materials, fx, kind, preview);
        if !preview {
            spawn_cockpit_holograms(p, meshes, materials, fx, &cube, kind, &bp);
            spawn_ship_energy_trails(p, materials, fx, &cube, kind);
            p.spawn(PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(0.64, 0.92, 1.0),
                    intensity: 130_000.0,
                    range: 18.0,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(
                    bp.cockpit_offset + Vec3::new(0.0, 1.0, 0.6),
                ),
                ..default()
            });
            p.spawn(PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(0.25, 0.90, 1.0),
                    intensity: 450_000.0,
                    range: 22.0,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(Vec3::new(0.0, 1.4, 6.8)),
                ..default()
            });
        }
    });
    root
}

fn spawn_realistic_ship_exterior(
    parent: &mut ChildBuilder,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    kind: ShipKind,
    preview: bool,
) {
    for part in realistic_ship_exterior_specs(kind) {
        spawn_real_ship_part(
            parent,
            meshes,
            materials,
            fx,
            part,
            preview,
            "RealShipExterior",
        );
    }
}

fn spawn_realistic_cockpit_parts(
    parent: &mut ChildBuilder,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    kind: ShipKind,
    bp: &ShipBlueprint,
) {
    for part in realistic_cockpit_part_specs(kind, bp) {
        let mut part = part;
        part.offset += bp.cockpit_offset;
        spawn_real_ship_part(
            parent,
            meshes,
            materials,
            fx,
            part,
            false,
            "RealCockpitPart",
        );
    }
}

fn spawn_real_ship_part(
    parent: &mut ChildBuilder,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    part: RealShipPartSpec,
    preview: bool,
    name: &'static str,
) {
    let mesh = real_ship_mesh(fx, meshes, part.mesh);
    let material = real_ship_material(fx, materials, part.tone, preview);
    parent.spawn((
        PbrBundle {
            mesh,
            material,
            transform: Transform::from_translation(part.offset)
                .with_rotation(part.rotation)
                .with_scale(part.scale),
            ..default()
        },
        Name::new(name),
    ));
}

fn real_ship_mesh(
    fx: &mut ShipFxCache,
    meshes: &mut Assets<Mesh>,
    kind: RealShipMeshKind,
) -> Handle<Mesh> {
    match kind {
        RealShipMeshKind::SmoothEllipsoid => fx
            .real_sphere
            .get_or_insert_with(|| meshes.add(Sphere::new(1.0)))
            .clone(),
        RealShipMeshKind::RoundNozzle => fx
            .real_cylinder
            .get_or_insert_with(|| meshes.add(Cylinder::new(1.0, 1.0)))
            .clone(),
        RealShipMeshKind::AeroPlate => fx
            .real_plate
            .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
            .clone(),
    }
}

fn real_ship_material(
    fx: &mut ShipFxCache,
    materials: &mut Assets<StandardMaterial>,
    tone: RealShipTone,
    preview: bool,
) -> Handle<StandardMaterial> {
    let key = (tone, preview);
    if let Some(mat) = fx.real_mats.get(&key) {
        return mat.clone();
    }
    let preview_alpha = if preview { 0.38 } else { 1.0 };
    let (base, emissive, alpha_mode, metallic, roughness, reflectance) = match tone {
        RealShipTone::CeramicWhite => (
            // Warm off-white ceramic, not chrome. Midday stills blew this
            // into a white blob because metallic 0.72 + sRGB 0.96 sat
            // above the Cinematic bloom threshold. Linear peak stays
            // under ~0.38 so ACES + ~16k lux leave the hull readable.
            Color::srgba(0.42, 0.37, 0.30, preview_alpha),
            LinearRgba::rgb(0.006, 0.005, 0.004),
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.06,
            0.64,
            0.16,
        ),
        RealShipTone::ShuttleWhite => (
            // Readable orbiter white. Brighter than CeramicWhite so the
            // cuboid hull reads as a craft, still under OLD_SCHOOL bloom.
            Color::srgba(0.78, 0.72, 0.64, preview_alpha),
            LinearRgba::rgb(0.04, 0.035, 0.028),
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.04,
            0.58,
            0.18,
        ),
        RealShipTone::ShuttleOrange => (
            // Opaque RCC/leading-edge paint, not additive heat bloom.
            Color::srgba(0.74, 0.32, 0.07, preview_alpha),
            LinearRgba::rgb(0.22, 0.06, 0.01),
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.08,
            0.52,
            0.20,
        ),
        RealShipTone::CarbonBlack => (
            Color::srgba(0.006, 0.010, 0.014, preview_alpha),
            LinearRgba::rgb(0.005, 0.025, 0.030),
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.86,
            0.22,
            0.58,
        ),
        RealShipTone::SmokedGlass => (
            Color::srgba(0.0, 0.025, 0.035, if preview { 0.42 } else { 0.88 }),
            LinearRgba::rgb(0.02, 0.22, 0.28),
            AlphaMode::Blend,
            0.10,
            0.03,
            0.95,
        ),
        RealShipTone::CyanEmission => (
            Color::srgba(0.02, 0.86, 1.0, if preview { 0.52 } else { 0.74 }),
            LinearRgba::rgb(0.22, 7.8, 9.4),
            AlphaMode::Add,
            0.0,
            0.08,
            0.72,
        ),
        RealShipTone::AmberHeat => (
            Color::srgba(1.0, 0.36, 0.06, if preview { 0.34 } else { 0.52 }),
            LinearRgba::rgb(8.5, 2.2, 0.10),
            AlphaMode::Add,
            0.0,
            0.15,
            0.58,
        ),
        RealShipTone::LuminiteGlass => (
            Color::srgba(0.56, 1.0, 1.0, if preview { 0.44 } else { 0.66 }),
            LinearRgba::rgb(1.6, 6.8, 7.2),
            AlphaMode::Add,
            0.05,
            0.04,
            0.9,
        ),
        RealShipTone::MagentaSignal => (
            Color::srgba(1.0, 0.12, 0.82, if preview { 0.46 } else { 0.72 }),
            LinearRgba::rgb(6.4, 0.18, 4.8),
            AlphaMode::Add,
            0.0,
            0.09,
            0.72,
        ),
        RealShipTone::SeatLeather => (
            Color::srgba(0.018, 0.016, 0.014, preview_alpha),
            LinearRgba::BLACK,
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.15,
            0.42,
            0.35,
        ),
        RealShipTone::ConsoleBlack => (
            Color::srgba(0.006, 0.012, 0.018, preview_alpha),
            LinearRgba::rgb(0.0, 0.08, 0.12),
            if preview {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            0.55,
            0.20,
            0.62,
        ),
    };
    let mat = materials.add(StandardMaterial {
        base_color: base,
        emissive,
        alpha_mode,
        metallic,
        perceptual_roughness: roughness,
        reflectance,
        ..default()
    });
    fx.real_mats.insert(key, mat.clone());
    mat
}

fn ship_trail_specs(kind: ShipKind) -> Vec<ShipTrailSpec> {
    let mut specs = vec![
        ShipTrailSpec {
            base_translation: Vec3::new(-2.65, 0.55, 11.3),
            base_scale: Vec3::new(0.34, 0.22, 6.8),
            phase: 0.0,
            tone: ShipTrailTone::Cyan,
        },
        ShipTrailSpec {
            base_translation: Vec3::new(2.65, 0.55, 11.3),
            base_scale: Vec3::new(0.34, 0.22, 6.8),
            phase: 1.7,
            tone: ShipTrailTone::Cyan,
        },
        ShipTrailSpec {
            base_translation: Vec3::new(0.0, 0.70, 8.7),
            base_scale: Vec3::new(3.9, 1.15, 1.9),
            phase: 0.8,
            tone: ShipTrailTone::Amber,
        },
    ];
    match kind {
        ShipKind::ScoutShuttle => {
            specs[0].base_translation = Vec3::new(-0.52, 0.18, 6.4);
            specs[1].base_translation = Vec3::new(0.52, 0.18, 6.4);
            specs[0].base_scale = Vec3::new(0.22, 0.16, 8.8);
            specs[1].base_scale = Vec3::new(0.22, 0.16, 8.8);
            specs[2].base_translation = Vec3::new(0.0, 0.22, 5.2);
            specs[2].base_scale = Vec3::new(1.15, 0.28, 1.6);
            specs.push(ShipTrailSpec {
                base_translation: Vec3::new(0.0, 0.18, 9.6),
                base_scale: Vec3::new(0.55, 0.22, 10.5),
                phase: 2.8,
                tone: ShipTrailTone::Cyan,
            });
        }
        ShipKind::StrikeFighter => {
            specs[0].base_translation = Vec3::new(-7.4, 4.5, 8.2);
            specs[1].base_translation = Vec3::new(7.4, -3.4, 8.2);
            specs[0].base_scale = Vec3::new(0.22, 0.42, 7.4);
            specs[1].base_scale = Vec3::new(0.22, 0.42, 7.4);
            specs.push(ShipTrailSpec {
                base_translation: Vec3::new(0.0, 0.0, 8.8),
                base_scale: Vec3::new(0.62, 0.34, 5.4),
                phase: 3.1,
                tone: ShipTrailTone::Cyan,
            });
        }
        ShipKind::HeavyDropship => {
            specs[0].base_translation = Vec3::new(-3.6, 2.0, 12.5);
            specs[1].base_translation = Vec3::new(3.6, 2.0, 12.5);
            specs[0].base_scale = Vec3::new(0.46, 0.34, 5.6);
            specs[1].base_scale = Vec3::new(0.46, 0.34, 5.6);
            specs[2].base_scale = Vec3::new(5.3, 1.55, 2.3);
        }
    }
    specs
}

fn spawn_ship_energy_trails(
    parent: &mut ChildBuilder,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    cube: &Handle<Mesh>,
    kind: ShipKind,
) {
    for spec in ship_trail_specs(kind) {
        let material = ship_trail_material(fx, materials, spec.tone);
        parent.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material,
                transform: Transform::from_translation(spec.base_translation)
                    .with_scale(spec.base_scale),
                ..default()
            },
            ShipEnergyTrail {
                base_translation: spec.base_translation,
                base_scale: spec.base_scale,
                phase: spec.phase,
            },
            Name::new(match spec.tone {
                ShipTrailTone::Cyan => "CyanEnergyWake",
                ShipTrailTone::Amber => "AmberHeatBloom",
            }),
        ));
    }
}

fn ship_trail_material(
    fx: &mut ShipFxCache,
    materials: &mut Assets<StandardMaterial>,
    tone: ShipTrailTone,
) -> Handle<StandardMaterial> {
    match tone {
        ShipTrailTone::Cyan => cockpit_material(
            fx,
            materials,
            21,
            Color::srgba(0.02, 0.88, 1.0, 0.38),
            LinearRgba::rgb(0.2, 7.5, 9.5),
            AlphaMode::Add,
            true,
        ),
        ShipTrailTone::Amber => cockpit_material(
            fx,
            materials,
            22,
            Color::srgba(1.0, 0.36, 0.06, 0.34),
            LinearRgba::rgb(8.5, 2.4, 0.12),
            AlphaMode::Add,
            true,
        ),
    }
}

fn new_world_look_basis() -> (Vec3, Vec3, Vec3) {
    // Matches `TerrainGenerator::scenic_frontier_spawn` (eye 64,-79 → look 110,-80).
    let yaw = 46.0_f32.atan2(1.0);
    let pitch = 6.0_f32.atan2(46.0);
    let rot = Quat::from_axis_angle(Vec3::Y, yaw) * Quat::from_axis_angle(Vec3::X, pitch);
    let forward = rot * -Vec3::Z;
    let right = rot * Vec3::X;
    let forward_h = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
    let right_h = Vec3::new(right.x, 0.0, right.z).normalize_or_zero();
    (forward, forward_h, right_h)
}

fn hero_flyby_pose(origin: Vec3, u: f32) -> (Vec3, f32, f32) {
    let u = u.clamp(0.0, 1.0);
    let (_forward, forward_h, right_h) = new_world_look_basis();
    // Bank across the open sky in front of the authored postcard.
    let ahead = 44.0 + u * 16.0;
    let lateral = -16.0 + u * 40.0;
    let height = 16.0 + (u * std::f32::consts::PI).sin() * 2.2;
    let pos = origin + forward_h * ahead + right_h * lateral + Vec3::Y * height;
    let travel = forward_h * 16.0 + right_h * 40.0;
    let yaw = (-travel.x).atan2(-travel.z);
    let roll = -0.40 + (u * std::f32::consts::TAU).sin() * 0.28;
    (pos, yaw, roll)
}

fn update_hero_flyby(
    time: Res<Time>,
    pilot: Res<PilotState>,
    mut q: Query<(Entity, &mut Transform, &mut HeroFlyby, &mut ShipMotion), Without<Player>>,
) {
    let dt = time.delta_seconds();
    for (entity, mut tf, mut fly, mut motion) in q.iter_mut() {
        if pilot.active_ship == Some(entity) {
            continue;
        }
        fly.t = (fly.t + dt * 0.018).rem_euclid(1.0);
        let (pos, yaw, roll) = hero_flyby_pose(fly.origin, fly.t);
        tf.translation = pos;
        tf.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_z(roll);
        tf.scale = Vec3::splat(HERO_FLYBY_SCALE);
        motion.yaw = yaw;
        motion.pitch = -0.10;
        motion.roll = roll;
        motion.speed = 110.0;
    }
}

fn sky_traffic_count(graphics: GraphicsMode, cinematic: bool) -> usize {
    match graphics {
        GraphicsMode::Fast => 2,
        GraphicsMode::Balanced => 4,
        GraphicsMode::High if cinematic => 6,
        GraphicsMode::High => 5,
    }
}

fn sky_traffic_lanes() -> [(Vec3, Vec3, f32, f32, f32, u8); 6] {
    let (_forward, forward_h, right_h) = new_world_look_basis();
    let lane = |ahead: f32, height: f32, lateral: f32, d_ahead: f32, d_lat: f32, d_up: f32, scale: f32, speed: f32, t0: f32, variant: u8| {
        let origin = forward_h * ahead + Vec3::Y * height + right_h * lateral;
        let span = forward_h * d_ahead + right_h * d_lat + Vec3::Y * d_up;
        (origin, span, scale, speed, t0, variant)
    };
    [
        lane(70.0, 22.0, -28.0, 18.0, 90.0, 4.0, 1.55, 0.028, 0.12, 0),
        lane(95.0, 28.0, 32.0, 14.0, -110.0, -3.0, 1.85, 0.022, 0.40, 1),
        lane(58.0, 18.0, 48.0, 36.0, -30.0, 5.0, 1.25, 0.032, 0.68, 0),
        lane(130.0, 32.0, -12.0, -10.0, 100.0, 3.0, 2.1, 0.016, 0.22, 1),
        lane(88.0, 24.0, 16.0, 30.0, 55.0, 4.0, 1.4, 0.024, 0.55, 0),
        lane(150.0, 20.0, -40.0, 8.0, 120.0, 6.0, 1.35, 0.020, 0.80, 1),
    ]
}

fn sky_traffic_pose(origin: Vec3, span: Vec3, t: f32) -> (Vec3, f32) {
    let u = t.rem_euclid(1.0);
    let pos = origin + span * u;
    (pos, (-span.x).atan2(-span.z))
}

fn ambient_traffic_specs(variant: u8, detailed: bool) -> Vec<RealShipPartSpec> {
    let mut parts = Vec::with_capacity(6);
    let white = RealShipTone::ShuttleWhite;
    let body = if variant == 0 {
        (Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.55, 0.28, 2.4))
    } else {
        (Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.15, 0.22, 1.8))
    };
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        white,
        body.0,
        body.1,
        Quat::IDENTITY,
    );
    for sx in [-1.0, 1.0] {
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            white,
            Vec3::new(sx * 0.95, 0.0, 0.25),
            Vec3::new(1.35, 0.08, 0.55),
            Quat::IDENTITY,
        );
    }
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CyanEmission,
        Vec3::new(0.0, 0.0, 3.4),
        Vec3::new(0.16, 0.10, 5.8),
        Quat::IDENTITY,
    );
    if detailed {
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::ShuttleOrange,
            Vec3::new(0.0, 0.16, -0.4),
            Vec3::new(0.12, 0.08, 1.6),
            Quat::IDENTITY,
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::RoundNozzle,
            RealShipTone::CyanEmission,
            Vec3::new(0.0, 0.0, 1.35),
            Vec3::new(0.16, 0.18, 0.16),
            Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        );
    }
    parts
}

fn spawn_sky_traffic(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    origin: Vec3,
    graphics: GraphicsMode,
    cinematic: bool,
) {
    let count = sky_traffic_count(graphics, cinematic);
    let detailed = graphics != GraphicsMode::Fast;
    for (offset, span, scale, speed, t0, variant) in sky_traffic_lanes().into_iter().take(count) {
        let lane_origin = origin + offset;
        let (pos, yaw) = sky_traffic_pose(lane_origin, span, t0);
        let root = commands
            .spawn((
                SpatialBundle {
                    transform: Transform::from_translation(pos)
                        .with_rotation(Quat::from_rotation_y(yaw))
                        .with_scale(Vec3::splat(scale)),
                    ..default()
                },
                SkyTraffic {
                    t: t0,
                    speed,
                    origin: lane_origin,
                    span,
                    scale,
                },
                Name::new("SkyTraffic"),
            ))
            .id();
        commands.entity(root).with_children(|parent| {
            for part in ambient_traffic_specs(variant, detailed) {
                spawn_real_ship_part(
                    parent,
                    meshes,
                    materials,
                    fx,
                    part,
                    false,
                    "SkyTrafficPart",
                );
            }
        });
    }
}

fn update_sky_traffic(time: Res<Time>, mut q: Query<(&mut Transform, &mut SkyTraffic)>) {
    let dt = time.delta_seconds();
    for (mut tf, mut traffic) in q.iter_mut() {
        traffic.t = (traffic.t + dt * traffic.speed).rem_euclid(1.0);
        let (pos, yaw) = sky_traffic_pose(traffic.origin, traffic.span, traffic.t);
        tf.translation = pos;
        tf.rotation = Quat::from_rotation_y(yaw);
        tf.scale = Vec3::splat(traffic.scale);
    }
}

fn update_ship_energy_trails(
    time: Res<Time>,
    pilot: Res<PilotState>,
    ship_q: Query<(&ShipInstance, &ShipMotion)>,
    mut trails: Query<(&ShipEnergyTrail, &mut Transform)>,
) {
    let intensity = pilot
        .active_ship
        .and_then(|ship| ship_q.get(ship).ok())
        .map(|(ship, motion)| {
            let bp = blueprint(ship.kind);
            (motion.speed / bp.max_speed.max(1.0)).clamp(0.0, 1.0)
        })
        .unwrap_or(0.18);
    let seconds = time.elapsed_seconds_wrapped();
    for (trail, mut tf) in trails.iter_mut() {
        let wave = (seconds * 7.4 + trail.phase).sin();
        let pulse = 0.72 + intensity * 0.55 + wave.abs() * 0.22;
        tf.translation = trail.base_translation + Vec3::new(0.0, wave * 0.08, wave * 0.28);
        tf.scale = Vec3::new(
            trail.base_scale.x * (0.86 + pulse * 0.14),
            trail.base_scale.y * (0.82 + pulse * 0.18),
            trail.base_scale.z * (0.78 + pulse * 0.28),
        );
    }
}

fn spawn_cockpit_holograms(
    parent: &mut ChildBuilder,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    cube: &Handle<Mesh>,
    kind: ShipKind,
    bp: &ShipBlueprint,
) {
    spawn_realistic_cockpit_parts(parent, meshes, materials, fx, kind, bp);
    for panel in cockpit_panel_specs(kind, bp) {
        let mat = cockpit_panel_material(fx, materials, panel.tone);
        spawn_panel(
            parent,
            cube,
            mat,
            bp.cockpit_offset + panel.offset,
            panel.scale,
            panel.rotation,
        );
    }
}

fn cockpit_panel_specs(kind: ShipKind, bp: &ShipBlueprint) -> Vec<CockpitPanelSpec> {
    let mut panels = Vec::with_capacity(40);
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, -1.35, 0.72),
        Vec3::new(1.10, 0.26, 1.18),
        Quat::IDENTITY,
        CockpitPanelTone::Seat,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, -0.70, 1.08),
        Vec3::new(1.12, 1.05, 0.18),
        Quat::from_rotation_x(0.28),
        CockpitPanelTone::Seat,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, 0.64, -0.96),
        Vec3::new(3.20, 0.16, 0.22),
        Quat::IDENTITY,
        CockpitPanelTone::Frame,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, 0.92, -1.15),
        Vec3::new(2.25, 0.045, 0.58),
        Quat::from_rotation_x(0.10),
        CockpitPanelTone::Glass,
    );
    for side in [-1.0, 1.0] {
        push_cockpit_panel(
            &mut panels,
            Vec3::new(side * 1.62, -0.08, -0.74),
            Vec3::new(0.15, 1.48, 0.20),
            Quat::from_rotation_z(side * 0.10),
            CockpitPanelTone::Frame,
        );
        push_cockpit_panel(
            &mut panels,
            Vec3::new(side * 0.48, -0.50, -0.92),
            Vec3::new(0.12, 0.12, 0.58),
            Quat::from_rotation_x(-0.24) * Quat::from_rotation_z(side * 0.30),
            CockpitPanelTone::Frame,
        );
        push_cockpit_panel(
            &mut panels,
            Vec3::new(side * 1.18, 0.28, -1.02),
            Vec3::new(0.08, 1.08, 0.18),
            Quat::from_rotation_z(side * 0.28),
            CockpitPanelTone::Glass,
        );
        push_cockpit_panel(
            &mut panels,
            Vec3::new(side * 3.0, -0.72, 0.18),
            Vec3::new(0.30, 0.08, 1.65),
            Quat::from_rotation_z(side * 0.14),
            CockpitPanelTone::Amber,
        );
        push_cockpit_panel(
            &mut panels,
            Vec3::new(side * 2.44, -0.47, -0.92),
            Vec3::new(0.42, 0.045, 0.74),
            Quat::from_rotation_x(-0.30) * Quat::from_rotation_z(side * 0.18),
            if side < 0.0 {
                CockpitPanelTone::Cyan
            } else {
                CockpitPanelTone::Magenta
            },
        );
    }
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, -0.82, -1.10),
        Vec3::new(3.1, 0.08, 1.35),
        Quat::from_rotation_x(-0.46),
        CockpitPanelTone::Shell,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(0.0, -0.75, -1.18),
        Vec3::new(2.55, 0.035, 0.78),
        Quat::from_rotation_x(-0.46),
        CockpitPanelTone::Cyan,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(-2.15, -0.88, -0.55),
        Vec3::new(1.10, 0.08, 1.05),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(0.10),
        CockpitPanelTone::Shell,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(-2.15, -0.80, -0.62),
        Vec3::new(0.74, 0.04, 0.68),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(0.10),
        CockpitPanelTone::Cyan,
    );
    push_cockpit_panel(
        &mut panels,
        Vec3::new(2.15, -0.84, -0.60),
        Vec3::new(0.78, 0.04, 0.78),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(-0.10),
        CockpitPanelTone::Magenta,
    );

    for i in 0..8 {
        let x = -1.55 + i as f32 * 0.44;
        let tone = match i % 4 {
            0 => CockpitPanelTone::Amber,
            1 => CockpitPanelTone::Cyan,
            2 => CockpitPanelTone::Magenta,
            _ => CockpitPanelTone::Cyan,
        };
        push_cockpit_panel(
            &mut panels,
            Vec3::new(x, -0.62, -0.56),
            Vec3::new(0.20, 0.065, 0.16),
            Quat::from_rotation_x(-0.36),
            tone,
        );
    }

    let scale = (bp.hull_radius / 8.0).clamp(0.72, 1.22);
    match kind {
        ShipKind::ScoutShuttle => {
            for side in [-1.0, 1.0] {
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(side * 0.72, -0.22, -1.54),
                    Vec3::new(0.40, 0.035, 0.72),
                    Quat::from_rotation_x(-0.58) * Quat::from_rotation_z(side * 0.12),
                    CockpitPanelTone::Cyan,
                );
            }
            push_cockpit_panel(
                &mut panels,
                Vec3::new(0.0, -0.22, -1.80),
                Vec3::new(1.15, 0.035, 0.30),
                Quat::from_rotation_x(-0.58),
                CockpitPanelTone::Amber,
            );
        }
        ShipKind::StrikeFighter => {
            for side in [-1.0, 1.0] {
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(side * 0.94, -0.14, -1.48),
                    Vec3::new(0.38, 0.035, 0.90),
                    Quat::from_rotation_x(-0.62) * Quat::from_rotation_z(side * 0.32),
                    if side < 0.0 {
                        CockpitPanelTone::Cyan
                    } else {
                        CockpitPanelTone::Magenta
                    },
                );
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(side * 1.38, 0.16, -0.86),
                    Vec3::new(0.10, 1.18, 0.16),
                    Quat::from_rotation_z(side * 0.45),
                    CockpitPanelTone::Frame,
                );
            }
            push_cockpit_panel(
                &mut panels,
                Vec3::new(0.0, -0.18, -1.82),
                Vec3::new(1.55, 0.035, 0.24),
                Quat::from_rotation_x(-0.64),
                CockpitPanelTone::Amber,
            );
        }
        ShipKind::HeavyDropship => {
            for seat_x in [-0.58, 0.58] {
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(seat_x, -1.30, 1.42),
                    Vec3::new(0.82, 0.22, 0.92),
                    Quat::IDENTITY,
                    CockpitPanelTone::Seat,
                );
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(seat_x, -0.62, 1.72),
                    Vec3::new(0.80, 0.96, 0.16),
                    Quat::from_rotation_x(0.24),
                    CockpitPanelTone::Seat,
                );
            }
            for side in [-1.0, 1.0] {
                push_cockpit_panel(
                    &mut panels,
                    Vec3::new(side * 1.30, -0.18, -1.52),
                    Vec3::new(0.48 * scale, 0.04, 0.90),
                    Quat::from_rotation_x(-0.52) * Quat::from_rotation_z(side * 0.16),
                    CockpitPanelTone::Cyan,
                );
            }
            push_cockpit_panel(
                &mut panels,
                Vec3::new(0.0, 0.36, -1.40),
                Vec3::new(2.45, 0.045, 0.32),
                Quat::from_rotation_x(-0.10),
                CockpitPanelTone::Amber,
            );
        }
    }
    panels
}

fn push_cockpit_panel(
    panels: &mut Vec<CockpitPanelSpec>,
    offset: Vec3,
    scale: Vec3,
    rotation: Quat,
    tone: CockpitPanelTone,
) {
    panels.push(CockpitPanelSpec {
        offset,
        scale,
        rotation,
        tone,
    });
}

fn cockpit_panel_material(
    fx: &mut ShipFxCache,
    materials: &mut Assets<StandardMaterial>,
    tone: CockpitPanelTone,
) -> Handle<StandardMaterial> {
    match tone {
        CockpitPanelTone::Cyan => cockpit_material(
            fx,
            materials,
            1,
            Color::srgba(0.04, 0.95, 1.0, 0.52),
            LinearRgba::rgb(0.25, 4.5, 5.5),
            AlphaMode::Add,
            true,
        ),
        CockpitPanelTone::Magenta => cockpit_material(
            fx,
            materials,
            2,
            Color::srgba(1.0, 0.10, 0.78, 0.46),
            LinearRgba::rgb(4.5, 0.25, 3.2),
            AlphaMode::Add,
            true,
        ),
        CockpitPanelTone::Amber => cockpit_material(
            fx,
            materials,
            3,
            Color::srgba(1.0, 0.48, 0.08, 0.72),
            LinearRgba::rgb(4.8, 1.7, 0.18),
            AlphaMode::Add,
            true,
        ),
        CockpitPanelTone::Shell => cockpit_material(
            fx,
            materials,
            4,
            Color::srgb(0.010, 0.020, 0.030),
            LinearRgba::rgb(0.0, 0.18, 0.25),
            AlphaMode::Opaque,
            false,
        ),
        CockpitPanelTone::Seat => cockpit_material(
            fx,
            materials,
            5,
            Color::srgb(0.018, 0.019, 0.024),
            LinearRgba::BLACK,
            AlphaMode::Opaque,
            false,
        ),
        CockpitPanelTone::Frame => cockpit_material(
            fx,
            materials,
            6,
            Color::srgb(0.23, 0.28, 0.32),
            LinearRgba::rgb(0.02, 0.05, 0.06),
            AlphaMode::Opaque,
            false,
        ),
        CockpitPanelTone::Glass => cockpit_material(
            fx,
            materials,
            7,
            Color::srgba(0.05, 0.32, 0.40, 0.72),
            LinearRgba::rgb(0.08, 0.48, 0.62),
            AlphaMode::Blend,
            false,
        ),
    }
}

fn spawn_panel(
    parent: &mut ChildBuilder,
    cube: &Handle<Mesh>,
    material: Handle<StandardMaterial>,
    translation: Vec3,
    scale: Vec3,
    rotation: Quat,
) {
    parent.spawn(PbrBundle {
        mesh: cube.clone(),
        material,
        transform: Transform::from_translation(translation)
            .with_rotation(rotation)
            .with_scale(scale),
        ..default()
    });
}

fn cockpit_material(
    fx: &mut ShipFxCache,
    materials: &mut Assets<StandardMaterial>,
    key: u8,
    base_color: Color,
    emissive: LinearRgba,
    alpha_mode: AlphaMode,
    unlit: bool,
) -> Handle<StandardMaterial> {
    if let Some(mat) = fx.cockpit_mats.get(&key) {
        return mat.clone();
    }
    let mat = materials.add(StandardMaterial {
        base_color,
        emissive,
        alpha_mode,
        unlit,
        metallic: 0.0,
        perceptual_roughness: 0.12,
        ..default()
    });
    fx.cockpit_mats.insert(key, mat.clone());
    mat
}

fn spawn_saved_ships_once(
    pending: Res<PendingWorldLoad>,
    active: Option<Res<ActiveWorld>>,
    settings: Res<WorldSettings>,
    mut inventory: ResMut<ShipInventory>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut fx: ResMut<ShipFxCache>,
    existing: Query<Entity, Or<(With<ShipInstance>, With<SkyTraffic>)>>,
) {
    if !pending.0 {
        return;
    }
    for e in existing.iter() {
        if let Some(entity_commands) = commands.get_entity(e) {
            entity_commands.despawn_recursive();
        }
    }
    let Some(active) = active else {
        return;
    };
    *inventory = active.meta.ship_inventory.clone();
    let generator = crate::terrain::TerrainGenerator::new(active.meta.seed);
    let (player_anchor, player_yaw) = resolved_world_entry_anchor(&active, &settings, &generator);

    for saved in &active.meta.ships {
        spawn_ship_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut fx,
            saved.kind,
            Vec3::new(saved.pos[0], saved.pos[1], saved.pos[2]),
            saved.yaw,
            false,
            Some(saved.shield),
        );
    }

    let has_nearby_ship = active.meta.ships.iter().any(|saved| {
        Vec2::new(saved.pos[0], saved.pos[2]).distance(Vec2::new(player_anchor.x, player_anchor.z))
            < 260.0
    });
    if active.meta.ships.is_empty() || !has_nearby_ship {
        let px = player_anchor.x + 6.0;
        let pz = player_anchor.z + 46.0;
        let py = player_anchor.y - 12.0;
        spawn_ship_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut fx,
            ShipKind::ScoutShuttle,
            Vec3::new(px, py, pz),
            player_yaw + 0.35,
            false,
            None,
        );
        let t0 = 0.28;
        let (fly_pos, fly_yaw, _) = hero_flyby_pose(player_anchor, t0);
        let fly = spawn_ship_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut fx,
            ShipKind::ScoutShuttle,
            fly_pos,
            fly_yaw,
            false,
            None,
        );
        commands.entity(fly).insert(HeroFlyby {
            t: t0,
            origin: player_anchor,
        });
    }
    spawn_sky_traffic(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut fx,
        player_anchor,
        settings.graphics,
        settings.runtime_profile == RuntimeProfile::Cinematic,
    );
}

fn resolved_world_entry_anchor(
    active: &ActiveWorld,
    settings: &WorldSettings,
    generator: &crate::terrain::TerrainGenerator,
) -> (Vec3, f32) {
    let pos = active.meta.player_pos;
    let mut anchor = Vec3::new(pos[0], pos[1], pos[2]);
    let mut yaw = active.meta.player_yaw;
    if settings.visual_preset == crate::settings::VisualPreset::NaturalWorld {
        let bx = crate::chunk::floor_to_i32_safe(anchor.x);
        let bz = crate::chunk::floor_to_i32_safe(anchor.z);
        let surface = generator.surface_height_at(bx, bz);
        // Ships park where the player left them; only a genuinely
        // stranded anchor (adrift far above any terrain) gets moved.
        if anchor.y > surface as f32 + 160.0 || anchor.y < 1.0 {
            if let Some(spawn) = generator.find_natural_spawn(bx, bz, 4096) {
                anchor = Vec3::new(spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5);
                yaw = 0.0;
            }
        }
    } else if settings.visual_preset == crate::settings::VisualPreset::NeonShuttle {
        let bx = crate::chunk::floor_to_i32_safe(anchor.x);
        let bz = crate::chunk::floor_to_i32_safe(anchor.z);
        if !generator.biome_at(bx, bz).is_neon_showcase() {
            if let Some(spawn) = generator.find_neon_showcase_spawn(bx, bz, 14_000) {
                anchor = Vec3::new(spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5);
                yaw = -0.72;
            }
        }
    }
    (anchor, yaw)
}

fn cleanup_ship_runtime(
    mut commands: Commands,
    mut pilot: ResMut<PilotState>,
    mut placement: ResMut<ShipPlacementState>,
    mut boarding: ResMut<ShipBoardingState>,
    mut transition: ResMut<CockpitTransition>,
    entities: Query<
        Entity,
        Or<(
            With<ShipInstance>,
            With<ShipPreview>,
            With<ShipProjectile>,
            With<EnemyDrone>,
            With<ShipExplosion>,
            With<SkyTraffic>,
        )>,
    >,
) {
    for entity in entities.iter() {
        despawn(&mut commands, entity);
    }
    *pilot = PilotState::default();
    *placement = ShipPlacementState::default();
    *boarding = ShipBoardingState::default();
    transition.clear();
}

#[allow(clippy::too_many_arguments)]
fn ship_placement_input(
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut wheel: EventReader<MouseWheel>,
    world: Res<VoxelWorld>,
    camera_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    mut placement: ResMut<ShipPlacementState>,
    mut mode: ResMut<ModeContext>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut fx: ResMut<ShipFxCache>,
    mut preview_q: Query<&mut Transform, With<ShipPreview>>,
) {
    if !placement.active && !matches!(mode.mode, ActiveMode::ShipPlacement { .. }) {
        return;
    }
    placement.active = true;

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if wheel_delta.abs() > 0.1 {
        placement.yaw += wheel_delta.signum() * 15.0_f32.to_radians();
    }

    let Ok(cam) = camera_q.get_single() else {
        return;
    };
    let origin = cam.translation();
    let forward = cam.forward();
    let dir = Vec3::new(forward.x, forward.y, forward.z).normalize_or_zero();
    let pos = placement_target(&world, origin, dir).unwrap_or(origin + dir * 18.0);
    placement.last_pos = pos;

    let preview = match placement.preview {
        Some(e) if preview_q.get_mut(e).is_ok() => e,
        _ => {
            let e = spawn_ship_entity(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut fx,
                placement.kind,
                pos,
                placement.yaw,
                true,
                None,
            );
            placement.preview = Some(e);
            e
        }
    };
    if let Ok(mut tf) = preview_q.get_mut(preview) {
        let hover = (time.elapsed_seconds_wrapped() * 4.0).sin() * 0.12;
        tf.translation = pos + Vec3::Y * hover;
        tf.rotation = Quat::from_rotation_y(placement.yaw);
    }

    if mouse.just_pressed(MouseButton::Right) || keys.just_pressed(KeyCode::Escape) {
        if let Some(e) = placement.preview.take() {
            if let Some(entity_commands) = commands.get_entity(e) {
                entity_commands.despawn_recursive();
            }
        }
        placement.active = false;
        mode.set(ActiveMode::Combat, "Ship placement cancelled.");
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        if let Some(e) = placement.preview.take() {
            if let Some(entity_commands) = commands.get_entity(e) {
                entity_commands.despawn_recursive();
            }
        }
        let ship = spawn_ship_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut fx,
            placement.kind,
            pos,
            placement.yaw,
            false,
            None,
        );
        placement.active = false;
        mode.set(
            ActiveMode::Combat,
            format!(
                "{} placed. Aim at cockpit and click to enter.",
                placement.kind.label()
            ),
        );
        commands
            .entity(ship)
            .insert(Name::new(placement.kind.label()));
    }
}

fn placement_target(world: &VoxelWorld, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let (_, prev) = crate::sculpt::raycast::dda_voxel(world, origin, dir, 180.0)?;
    Some(prev.as_vec3() + Vec3::new(0.5, 1.1, 0.5))
}

fn ship_interaction_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut pilot: ResMut<PilotState>,
    mut boarding: ResMut<ShipBoardingState>,
    mut transition: ResMut<CockpitTransition>,
    mut mode: ResMut<ModeContext>,
    mut player_q: Query<
        (&mut Transform, &GlobalTransform, &mut Player),
        (With<Camera3d>, Without<ShipInstance>),
    >,
    mut ship_q: Query<
        (Entity, &Transform, &ShipInstance, &mut ShipMotion),
        (With<ShipInstance>, Without<Player>, Without<Camera3d>),
    >,
) {
    let Ok((mut player_tf, player_global, mut player)) = player_q.get_single_mut() else {
        return;
    };

    if pilot.active_ship.is_some() && !keys.just_pressed(KeyCode::KeyX) {
        return;
    }

    if let Some(active) = pilot.active_ship.take() {
        transition.clear();
        if let Ok((_, ship_tf, instance, _)) = ship_q.get_mut(active) {
            let bp = blueprint(instance.kind);
            player_tf.translation = ship_tf.translation + ship_tf.rotation * bp.exit_offset;
            player.flying = true;
            player.velocity = Vec3::ZERO;
        }
        pilot.status = "Exited shuttle.".into();
        mode.set(ActiveMode::Combat, "Exited shuttle cockpit.");
        return;
    }

    boarding.target = None;
    boarding.kind = None;
    boarding.distance = 0.0;
    boarding.lock = 0.0;

    if !matches!(mode.mode, ActiveMode::Combat) {
        return;
    }

    let origin = player_global.translation();
    let forward = player_global.forward();
    let dir = Vec3::new(forward.x, forward.y, forward.z).normalize_or_zero();
    let dir_len2 = dir.length_squared();
    let mut nearest: Option<(Entity, f32, f32, ShipKind, Transform, f32, f32)> = None;
    for (entity, ship_tf, instance, motion) in ship_q.iter_mut() {
        let bp = blueprint(instance.kind);
        let aim_point = ship_tf.translation + ship_tf.rotation * bp.cockpit_offset;
        let to_cockpit = aim_point - origin;
        let dist_cockpit = to_cockpit.length();
        let along = if dir_len2 > 1e-6 {
            to_cockpit.dot(dir)
        } else {
            0.0
        };
        let lateral = (to_cockpit - dir * along).length();
        let lock_radius = (bp.hull_radius * 0.58).clamp(3.2, 8.0);
        let d2 = player_tf.translation.distance_squared(ship_tf.translation);
        let max_root_dist = (bp.hull_radius * 7.2).clamp(48.0, 72.0);
        if d2 > max_root_dist * max_root_dist {
            continue;
        }
        let aim_ok = dir_len2 > 1e-6 && along >= 0.18 && along <= 44.0 && lateral <= lock_radius;
        let view_dot = if dist_cockpit > 1e-3 && dir_len2 > 1e-6 {
            to_cockpit.normalize().dot(dir)
        } else {
            0.0
        };
        // After X-exit you stand on the hull: classic cone test often has along < 1.
        let hug_ok = dist_cockpit <= 8.5 + bp.hull_radius * 0.12 && view_dot >= 0.38;
        if !(aim_ok || hug_ok) {
            continue;
        }
        let score_lateral = lateral;
        if nearest
            .as_ref()
            .map(|(_, _, best, _, _, _, _)| score_lateral < *best)
            .unwrap_or(true)
        {
            nearest = Some((
                entity,
                d2,
                score_lateral,
                instance.kind,
                *ship_tf,
                motion.yaw,
                lock_radius,
            ));
        }
    }

    let Some((entity, d2, lateral, kind, ship_tf, yaw, lock_radius)) = nearest else {
        return;
    };
    boarding.target = Some(entity);
    boarding.kind = Some(kind);
    boarding.distance = d2.sqrt();
    boarding.lock = (1.0 - (lateral / lock_radius.max(0.01))).clamp(0.0, 1.0);
    let board = mouse.just_pressed(MouseButton::Left) || keys.just_pressed(KeyCode::KeyH);
    if !board {
        return;
    }

    let bp = blueprint(kind);
    let cockpit_tf =
        Transform::from_translation(ship_tf.translation + ship_tf.rotation * bp.cockpit_offset)
            .with_rotation(ship_tf.rotation);
    pilot.active_ship = Some(entity);
    pilot.speed = 0.0;
    pilot.cruise_max_speed = bp.max_speed;
    pilot.shield = bp.shield;
    pilot.entry_peace_timer = 26.0;
    pilot.status = format!("Linking {} cockpit.", kind.label());
    transition.start(entity, *player_tf, cockpit_tf);
    player.yaw = yaw;
    player.pitch = 0.0;
    player.flying = true;
    player.velocity = Vec3::ZERO;
    mode.set(
        ActiveMode::ShipFlight { entity },
        format!("{} cockpit linked.", kind.label()),
    );
}

fn update_cockpit_transition(
    time: Res<Time>,
    mut transition: ResMut<CockpitTransition>,
    mut player_q: Query<(&mut Transform, &mut Player), (With<Camera3d>, Without<ShipInstance>)>,
    ship_q: Query<
        (&Transform, &ShipInstance),
        (With<ShipInstance>, Without<Player>, Without<Camera3d>),
    >,
) {
    if !transition.active {
        return;
    }
    let Some(ship_entity) = transition.ship else {
        transition.clear();
        return;
    };
    let Ok((ship_tf, instance)) = ship_q.get(ship_entity) else {
        transition.clear();
        return;
    };
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };

    let bp = blueprint(instance.kind);
    transition.to =
        Transform::from_translation(ship_tf.translation + ship_tf.rotation * bp.cockpit_offset)
            .with_rotation(ship_tf.rotation);
    transition.timer += time.delta_seconds();
    let raw = (transition.timer / transition.duration.max(0.01)).clamp(0.0, 1.0);
    let t = raw * raw * (3.0 - 2.0 * raw);
    player_tf.translation = transition
        .from
        .translation
        .lerp(transition.to.translation, t);
    player_tf.rotation = transition.from.rotation.slerp(transition.to.rotation, t);
    player.flying = true;
    player.velocity = Vec3::ZERO;

    if raw >= 1.0 {
        player_tf.translation = transition.to.translation;
        player_tf.rotation = transition.to.rotation;
        transition.clear();
    }
}

fn draw_ship_boarding_hud(
    mut contexts: EguiContexts,
    boarding: Res<ShipBoardingState>,
    pilot: Res<PilotState>,
    mode: Res<ModeContext>,
    photo: Option<Res<crate::hud::PhotoMode>>,
) {
    if photo.map(|p| p.hidden).unwrap_or(false) {
        return;
    }
    if pilot.active_ship.is_some() || !matches!(mode.mode, ActiveMode::Combat) {
        return;
    }
    let Some(kind) = boarding.kind else {
        return;
    };
    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ship_boarding_hud"),
    ));
    let center = screen.center();
    let cyan = egui::Color32::from_rgb(0, 235, 255);
    let amber = egui::Color32::from_rgb(255, 160, 35);
    let magenta = egui::Color32::from_rgb(255, 40, 220);
    let lock = boarding.lock.clamp(0.2, 1.0);
    let radius = 28.0 + lock * 10.0;

    painter.circle_stroke(center, radius, egui::Stroke::new(1.5 + lock, cyan));
    painter.circle_stroke(
        center,
        radius + 8.0,
        egui::Stroke::new(0.8, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 90)),
    );
    for i in 0..4 {
        let angle = i as f32 * std::f32::consts::FRAC_PI_2 + lock * 0.45;
        let a = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [center + a * (radius + 5.0), center + a * (radius + 21.0)],
            egui::Stroke::new(2.0, if i % 2 == 0 { cyan } else { magenta }),
        );
    }

    let mouse =
        egui::Rect::from_center_size(center + egui::vec2(0.0, 58.0), egui::vec2(32.0, 42.0));
    painter.rect_stroke(
        mouse,
        egui::Rounding::same(13.0),
        egui::Stroke::new(1.4, cyan),
    );
    painter.line_segment(
        [
            mouse.center_top() + egui::vec2(0.0, 6.0),
            mouse.center() - egui::vec2(0.0, 1.0),
        ],
        egui::Stroke::new(1.0, cyan),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            mouse.left_top() + egui::vec2(3.0, 3.0),
            mouse.center_top() + egui::vec2(-1.0, 19.0),
        ),
        egui::Rounding::same(7.0),
        egui::Color32::from_rgba_unmultiplied(255, 160, 35, 130),
    );
    painter.text(
        center + egui::vec2(0.0, 92.0),
        egui::Align2::CENTER_CENTER,
        kind.short(),
        egui::FontId::monospace(12.0),
        amber,
    );
    painter.text(
        center + egui::vec2(0.0, 112.0),
        egui::Align2::CENTER_CENTER,
        "LMB / H  Cockpit",
        egui::FontId::monospace(11.0),
        egui::Color32::from_rgba_unmultiplied(180, 255, 240, 220),
    );
}

fn ship_wave_response(
    kind: ShipKind,
    speed: f32,
    max_speed: f32,
    seconds: f32,
) -> ShipWaveResponse {
    let intensity = (speed / max_speed.max(1.0)).clamp(0.0, 1.0);
    if intensity <= 0.001 {
        return ShipWaveResponse {
            vertical_velocity: 0.0,
            pitch: 0.0,
            roll: 0.0,
        };
    }
    let (freq, lift_amp, pitch_amp, roll_amp) = match kind {
        ShipKind::ScoutShuttle => (3.9, 0.78, 0.034, 0.052),
        ShipKind::StrikeFighter => (4.8, 0.54, 0.028, 0.064),
        ShipKind::HeavyDropship => (2.7, 0.48, 0.022, 0.036),
    };
    let phase = seconds * freq + kind as u8 as f32 * 1.31;
    let softened = intensity * intensity * (3.0 - 2.0 * intensity);
    ShipWaveResponse {
        vertical_velocity: phase.cos() * lift_amp * freq * softened,
        pitch: (phase * 0.71).sin() * pitch_amp * softened,
        roll: (phase * 1.17).sin() * roll_amp * softened,
    }
}

#[allow(clippy::too_many_arguments)]
fn ship_flight_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut mouse_motion: EventReader<MouseMotion>,
    mut wheel: EventReader<MouseWheel>,
    world: Res<VoxelWorld>,
    mut pilot: ResMut<PilotState>,
    transition: Res<CockpitTransition>,
    mut mode: ResMut<ModeContext>,
    mut player_q: Query<(&mut Transform, &mut Player), (With<Camera3d>, Without<ShipInstance>)>,
    mut ship_q: Query<
        (&mut Transform, &mut ShipInstance, &mut ShipMotion),
        (With<ShipInstance>, Without<Player>, Without<Camera3d>),
    >,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<ShipFxCache>,
    mut telemetry: ResMut<UnifiedTelemetry>,
) {
    let Some(active) = pilot.active_ship else {
        return;
    };
    if !matches!(mode.mode, ActiveMode::ShipFlight { entity } if entity == active) {
        return;
    }
    if transition.active {
        return;
    }
    let Ok((mut ship_tf, mut ship, mut motion)) = ship_q.get_mut(active) else {
        pilot.active_ship = None;
        return;
    };
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 20.0);
    pilot.shield_flash = (pilot.shield_flash - dt * 1.8).max(0.0);

    if pilot.shield <= 0.0 {
        let bp = blueprint(ship.kind);
        spawn_ship_explosion(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            ship_tf.translation,
            bp.hull_radius * 1.35,
        );
        player_tf.translation =
            ship_tf.translation + ship_tf.rotation * bp.exit_offset + Vec3::Y * 1.5;
        player_tf.rotation = Quat::from_rotation_y(motion.yaw);
        player.yaw = motion.yaw;
        player.pitch = 0.0;
        player.velocity = Vec3::ZERO;
        player.flying = true;
        despawn(&mut commands, active);
        pilot.active_ship = None;
        pilot.speed = 0.0;
        pilot.status = "Shuttle disabled. Emergency eject complete.".into();
        mode.set(ActiveMode::Combat, "Shuttle disabled. Emergency eject.");
        return;
    }

    // --- Smooth pitch/yaw from mouse look + keyboard flight trim -----------
    // Convert raw mouse pixels into a target angular rate (rad/s), then
    // blend in keyboard rudder/elevator input so cockpit flight stays usable
    // without constantly reaching for the mouse.
    let mut mouse_dx = 0.0_f32;
    let mut mouse_dy = 0.0_f32;
    for ev in mouse_motion.read() {
        mouse_dx += ev.delta.x;
        mouse_dy += ev.delta.y;
    }
    // Sensitivity stays deliberately low: mouse aims the nose, while A/D
    // commits a smooth aircraft-style turn instead of snapping hard.
    let key_turn_left = keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft);
    let key_turn_right = keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight);
    let turn_input = match (key_turn_left, key_turn_right) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    };
    let key_pitch_up = keys.pressed(KeyCode::Space) || keys.pressed(KeyCode::ArrowUp);
    let key_pitch_down = keys.pressed(KeyCode::ShiftLeft)
        || keys.pressed(KeyCode::ShiftRight)
        || keys.pressed(KeyCode::ArrowDown);
    let pitch_input = match (key_pitch_up, key_pitch_down) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    };
    let (target_yaw_rate, target_pitch_rate) =
        ship_target_angular_rates(mouse_dx, mouse_dy, turn_input, pitch_input, dt);
    let look_ease = 1.0 - (-dt * 6.5).exp();
    motion.yaw_rate += (target_yaw_rate - motion.yaw_rate) * look_ease;
    motion.pitch_rate += (target_pitch_rate - motion.pitch_rate) * look_ease;
    // Clamp angular velocities for a heavier, more cinematic feel.
    motion.yaw_rate = motion
        .yaw_rate
        .clamp(-SHIP_YAW_RATE_LIMIT, SHIP_YAW_RATE_LIMIT);
    motion.pitch_rate = motion
        .pitch_rate
        .clamp(-SHIP_PITCH_RATE_LIMIT, SHIP_PITCH_RATE_LIMIT);

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if wheel_delta.abs() > 0.1 {
        pilot.weapon = pilot.weapon.next(if wheel_delta > 0.0 { -1 } else { 1 });
    }

    let bp = blueprint(ship.kind);

    // --- Roll-coordinated turn: A/D visibly banks the shuttle, but the
    // keyboard rudder above is the primary authority. A is left, D is right.
    let roll_input = turn_input;
    let target_roll = roll_input * SHIP_TARGET_ROLL;
    let roll_ease = 1.0 - (-dt * 2.4).exp();
    motion.roll += (target_roll - motion.roll) * roll_ease;
    // Bank-driven turn now follows the same sign as keyboard yaw, so holding
    // A no longer slips/turns right while D no longer slips/turns left.
    let bank_yaw_rate = motion.roll * SHIP_BANK_YAW_RATE;
    let rudder = turn_input * SHIP_RUDDER_YAW_RATE;
    motion.yaw += (motion.yaw_rate + bank_yaw_rate + rudder) * dt;
    motion.pitch = (motion.pitch + motion.pitch_rate * dt).clamp(-0.85, 0.72);

    // --- Throttle with cruise inertia. ------------------------------------
    // W accelerates toward cruise, S brakes, and releasing both keeps most of
    // the current speed so long flights do not require holding W forever.
    let accelerating = keys.pressed(KeyCode::KeyW);
    let braking = keys.pressed(KeyCode::KeyS);
    let target_speed = if accelerating {
        bp.max_speed
    } else if braking {
        0.0
    } else {
        motion.speed
    };
    let speed_response = if accelerating {
        (bp.accel / bp.max_speed.max(1.0)) * 1.9
    } else if braking {
        2.8
    } else {
        0.15
    };
    let speed_ease = 1.0 - (-dt * speed_response).exp();
    motion.speed += (target_speed - motion.speed) * speed_ease;
    if !accelerating && !braking {
        motion.speed *= (1.0 - dt * 0.018).max(0.0);
    }
    motion.speed = motion.speed.clamp(0.0, bp.max_speed);

    let wave = ship_wave_response(
        ship.kind,
        motion.speed,
        bp.max_speed,
        time.elapsed_seconds_wrapped(),
    );
    ship_tf.rotation = Quat::from_rotation_y(motion.yaw)
        * Quat::from_rotation_x(motion.pitch)
        * Quat::from_rotation_z(motion.roll)
        * Quat::from_rotation_x(wave.pitch)
        * Quat::from_rotation_z(wave.roll);
    let forward = *ship_tf.forward();
    let right = *ship_tf.right();
    // Keep only a whisper of bank drift; A/D should feel like turning the ship,
    // not sliding sideways across the terrain.
    let target_lateral = -motion.roll * motion.speed * 0.025;
    motion.lateral_speed += (target_lateral - motion.lateral_speed) * (1.0 - (-dt * 4.5).exp());
    let lift_speed = pitch_input * bp.max_speed * 0.34 + wave.vertical_velocity;
    ship_tf.translation +=
        (forward * motion.speed + right * motion.lateral_speed + Vec3::Y * lift_speed) * dt;

    let probe = ship_tf.translation + forward * 7.0;
    if world.is_solid(
        probe.x.floor() as i32,
        probe.y.floor() as i32,
        probe.z.floor() as i32,
    ) {
        ship_tf.translation.y += 14.0 * dt;
        motion.speed *= 0.82;
    }
    ship_tf.translation.y = ship_tf.translation.y.max(8.0);

    pilot.speed = motion.speed;
    pilot.cruise_max_speed = bp.max_speed;
    pilot.shield = pilot.shield.max(0.0);
    ship.shield = pilot.shield;
    if pilot.shield < bp.shield * 0.28 {
        pilot.status = "Critical shields.".into();
    }
    pilot.primary_cooldown = (pilot.primary_cooldown - dt).max(0.0);
    pilot.secondary_cooldown = (pilot.secondary_cooldown - dt).max(0.0);

    if mouse.pressed(MouseButton::Left) && pilot.primary_cooldown <= 0.0 {
        fire_ship_pulse(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            &ship_tf,
            &bp,
        );
        pilot.primary_cooldown = 0.12;
        telemetry.ship_shots = telemetry.ship_shots.saturating_add(1);
    }
    if mouse.pressed(MouseButton::Right) && pilot.secondary_cooldown <= 0.0 {
        fire_ship_secondary(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            &ship_tf,
            &bp,
            pilot.weapon,
        );
        pilot.secondary_cooldown = pilot.weapon.profile().cooldown;
        telemetry.ship_shots = telemetry.ship_shots.saturating_add(1);
    }

    let cockpit = ship_tf.translation + ship_tf.rotation * bp.cockpit_offset;
    player_tf.translation = cockpit;
    player_tf.rotation = ship_tf.rotation;
    player.yaw = motion.yaw;
    player.pitch = motion.pitch;
    player.velocity = Vec3::ZERO;
    player.flying = true;
}

fn fire_ship_pulse(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    ship_tf: &Transform,
    bp: &ShipBlueprint,
) {
    let dir = *ship_tf.forward();
    for hardpoint in bp.hardpoints {
        let origin = ship_tf.translation + ship_tf.rotation * hardpoint;
        spawn_ship_projectile(
            commands,
            meshes,
            materials,
            fx,
            origin,
            dir,
            ProjectileOwner::Player,
            Color::srgb(0.08, 0.96, 1.0),
            WeaponProfile {
                speed: 230.0,
                damage: 16.0,
                radius: 1.6,
                cooldown: 0.12,
                size: Vec3::new(0.08, 0.08, 1.6),
            },
        );
    }
}

fn fire_ship_secondary(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    ship_tf: &Transform,
    bp: &ShipBlueprint,
    weapon: ShipWeaponKind,
) {
    let dir = *ship_tf.forward();
    let origin =
        ship_tf.translation + ship_tf.rotation * ((bp.hardpoints[0] + bp.hardpoints[1]) * 0.5);
    spawn_ship_projectile(
        commands,
        meshes,
        materials,
        fx,
        origin,
        dir,
        ProjectileOwner::Player,
        weapon.color(),
        weapon.profile(),
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_ship_projectile(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    origin: Vec3,
    dir: Vec3,
    owner: ProjectileOwner,
    color: Color,
    profile: WeaponProfile,
) {
    let mesh = fx
        .projectile
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let mat_key = match owner {
        ProjectileOwner::Player => 1,
        ProjectileOwner::Drone => 2,
    };
    let mat = if let Some(mat) = fx.projectile_mats.get(&mat_key) {
        mat.clone()
    } else {
        let lin = color.to_linear();
        let mat = materials.add(StandardMaterial {
            base_color: color,
            emissive: LinearRgba::rgb(lin.red * 4.0, lin.green * 4.0, lin.blue * 4.0),
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        fx.projectile_mats.insert(mat_key, mat.clone());
        mat
    };
    let ndir = dir.normalize_or_zero();
    let rot = Quat::from_rotation_arc(Vec3::Z, ndir);
    commands.spawn((
        PbrBundle {
            mesh,
            material: mat,
            transform: Transform::from_translation(origin)
                .with_rotation(rot)
                .with_scale(profile.size),
            ..default()
        },
        ShipProjectile {
            owner,
            velocity: ndir * profile.speed,
            damage: profile.damage,
            radius: profile.radius,
            life: 4.0,
        },
        Name::new("ShipProjectile"),
    ));
}

#[allow(clippy::too_many_arguments)]
fn update_ship_projectiles(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    mut pilot: ResMut<PilotState>,
    mut commands: Commands,
    mut projectiles: Query<
        (Entity, &mut Transform, &mut ShipProjectile),
        (
            With<ShipProjectile>,
            Without<EnemyDrone>,
            Without<ShipInstance>,
        ),
    >,
    mut drones: Query<
        (Entity, &Transform, &mut EnemyDrone),
        (
            With<EnemyDrone>,
            Without<ShipProjectile>,
            Without<ShipInstance>,
        ),
    >,
    ships: Query<
        &Transform,
        (
            With<ShipInstance>,
            Without<ShipProjectile>,
            Without<EnemyDrone>,
        ),
    >,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<ShipFxCache>,
    mut telemetry: ResMut<UnifiedTelemetry>,
) {
    if !settings.ship_skirmish_ai {
        let mut kill = Vec::new();
        for (e, _, p) in projectiles.iter() {
            if p.owner == ProjectileOwner::Drone {
                kill.push(e);
            }
        }
        for e in kill {
            despawn(&mut commands, e);
        }
    }

    let dt = time.delta_seconds().min(1.0 / 20.0);
    for (entity, mut tf, mut p) in projectiles.iter_mut() {
        p.life -= dt;
        tf.translation += p.velocity * dt;
        tf.rotation = Quat::from_rotation_arc(Vec3::Z, p.velocity.normalize_or_zero());
        if p.life <= 0.0 {
            despawn(&mut commands, entity);
            continue;
        }
        let cell = tf.translation.floor().as_ivec3();
        if world.is_solid(cell.x, cell.y, cell.z) {
            spawn_ship_explosion(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut fx,
                tf.translation,
                p.radius,
            );
            despawn(&mut commands, entity);
            continue;
        }
        match p.owner {
            ProjectileOwner::Player => {
                let mut hit = false;
                for (drone_e, drone_tf, mut drone) in drones.iter_mut() {
                    if drone_tf.translation.distance(tf.translation) <= p.radius + 1.2 {
                        drone.hp -= p.damage;
                        spawn_ship_explosion(
                            &mut commands,
                            &mut meshes,
                            &mut materials,
                            &mut fx,
                            drone_tf.translation,
                            p.radius,
                        );
                        if drone.hp <= 0.0 {
                            despawn(&mut commands, drone_e);
                            telemetry.ship_kills = telemetry.ship_kills.saturating_add(1);
                        }
                        hit = true;
                        break;
                    }
                }
                if hit {
                    despawn(&mut commands, entity);
                }
            }
            ProjectileOwner::Drone => {
                if let Some(active) = pilot.active_ship {
                    if let Ok(ship_tf) = ships.get(active) {
                        if ship_tf.translation.distance(tf.translation) <= p.radius + 2.0 {
                            pilot.shield = (pilot.shield - p.damage).max(0.0);
                            pilot.shield_flash = 1.0;
                            pilot.status = if pilot.shield <= 0.0 {
                                "Shields collapsed.".into()
                            } else if pilot.shield < 35.0 {
                                "Critical shield impact.".into()
                            } else {
                                "Shield impact.".into()
                            };
                            spawn_ship_explosion(
                                &mut commands,
                                &mut meshes,
                                &mut materials,
                                &mut fx,
                                ship_tf.translation,
                                2.5,
                            );
                            despawn(&mut commands, entity);
                        }
                    }
                }
            }
        }
    }
}

fn spawn_enemy_drones(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut pilot: ResMut<PilotState>,
    world: Res<VoxelWorld>,
    ship_q: Query<&Transform, With<ShipInstance>>,
    drone_q: Query<Entity, With<EnemyDrone>>,
    mut timer: Local<f32>,
    mut skirmish: Local<(bool, f32)>,
    director: Option<Res<SimulationDirector>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<ShipFxCache>,
) {
    if !settings.ship_skirmish_ai {
        skirmish.0 = false;
        *timer = 0.0;
        pilot.entry_peace_timer = 0.0;
        return;
    }

    let Some(active) = pilot.active_ship else {
        *timer = 0.0;
        return;
    };
    let Ok(ship_tf) = ship_q.get(active) else {
        return;
    };

    let rising = !skirmish.0;
    skirmish.0 = true;
    if rising {
        pilot.entry_peace_timer = pilot.entry_peace_timer.max(16.0);
        skirmish.1 = 0.0;
    }
    skirmish.1 += time.delta_seconds();
    let sk_t = skirmish.1;

    if pilot.entry_peace_timer > 0.0 {
        pilot.entry_peace_timer -= time.delta_seconds();
        *timer = (*timer).max(6.0);
        return;
    }
    *timer -= time.delta_seconds();
    let pressure = director
        .as_deref()
        .map(SimulationDirector::enemy_pressure)
        .unwrap_or(0.85);
    let n = drone_q.iter().count();
    // Soft cap rises slowly with skirmish time — endless waves, not a 7-drone brick wall.
    let wave_cap = (3.0 + (sk_t / 38.0).floor() * 2.0 + pressure * 2.5).clamp(3.0, 16.0) as usize;
    let max_drones = wave_cap;
    if n >= max_drones {
        *timer = (*timer).max(2.5);
        return;
    }
    if *timer > 0.0 {
        return;
    }
    let shield_ease = (pilot.shield / 95.0).clamp(0.25, 1.0);
    *timer = (9.5 + n as f32 * 2.4 + (1.0 - shield_ease) * 7.0 - pressure * 1.4).clamp(5.0, 24.0);
    let mut rng =
        ChaCha8Rng::seed_from_u64((time.elapsed_seconds_wrapped() * 1000.0) as u64 ^ 0x5157_5A11);
    let angle = rng.gen_range(0.0..std::f32::consts::TAU);
    let mut pos = ship_tf.translation + Vec3::new(angle.cos() * 95.0, 34.0, angle.sin() * 95.0);
    for _ in 0..8 {
        let dist = rng.gen_range(70.0..125.0);
        let candidate = ship_tf.translation
            + Vec3::new(
                angle.cos() * dist,
                rng.gen_range(20.0..52.0),
                angle.sin() * dist,
            );
        let cell = candidate.floor().as_ivec3();
        if !world.is_solid(cell.x, cell.y, cell.z) && !world.is_solid(cell.x, cell.y + 1, cell.z) {
            pos = candidate;
            break;
        }
    }
    spawn_drone(&mut commands, &mut meshes, &mut materials, &mut fx, pos);
}

fn spawn_drone(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    pos: Vec3,
) {
    let cube = fx
        .cube
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.96, 0.96, 0.96)))
        .clone();
    let mat = fx
        .drone_mat
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.10, 0.02, 0.14),
                emissive: LinearRgba::rgb(2.5, 0.2, 3.2),
                metallic: 0.6,
                perceptual_roughness: 0.2,
                ..default()
            })
        })
        .clone();
    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(pos),
                ..default()
            },
            EnemyDrone {
                hp: 100.0,
                fire_cooldown: 3.8,
                orbit: pos.x.sin(),
                velocity: Vec3::ZERO,
            },
            Name::new("EnemyDrone"),
        ))
        .with_children(|p| {
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: mat.clone(),
                transform: Transform::from_scale(Vec3::new(2.0, 0.8, 2.0)),
                ..default()
            });
            p.spawn(PbrBundle {
                mesh: cube,
                material: mat,
                transform: Transform::from_translation(Vec3::new(0.0, 0.6, 0.0))
                    .with_scale(Vec3::new(0.8, 0.8, 0.8)),
                ..default()
            });
            p.spawn(PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(1.0, 0.1, 0.9),
                    intensity: 180_000.0,
                    range: 16.0,
                    shadows_enabled: false,
                    ..default()
                },
                ..default()
            });
        });
}

#[allow(clippy::too_many_arguments)]
fn update_enemy_drones(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    pilot: Res<PilotState>,
    world: Res<VoxelWorld>,
    ship_q: Query<&Transform, (With<ShipInstance>, Without<EnemyDrone>)>,
    mut drones: Query<(Entity, &mut Transform, &mut EnemyDrone), Without<ShipInstance>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fx: ResMut<ShipFxCache>,
) {
    if !settings.ship_skirmish_ai {
        for (e, _, _) in drones.iter() {
            despawn(&mut commands, e);
        }
        return;
    }

    let Some(active) = pilot.active_ship else {
        for (e, _, _) in drones.iter() {
            despawn(&mut commands, e);
        }
        return;
    };
    let Ok(ship_tf) = ship_q.get(active) else {
        return;
    };
    let dt = time.delta_seconds().min(1.0 / 20.0);
    let drone_count = drones.iter().count().max(1);
    for (_entity, mut tf, mut drone) in drones.iter_mut() {
        drone.orbit += dt * 0.55;
        drone.fire_cooldown -= dt;
        let to_target = ship_tf.translation - tf.translation;
        let dist = to_target.length().max(0.1);
        let target_dir = to_target.normalize_or_zero();
        // Lazy orbit pattern — slow, smooth circle around the prey.
        let orbit = Vec3::new(
            drone.orbit.cos(),
            (drone.orbit * 0.6).sin() * 0.35,
            drone.orbit.sin(),
        );
        let mut desired = target_dir * 30.0 + orbit * 11.0;
        if dist < 36.0 {
            desired -= target_dir * 38.0;
        }
        desired += drone_terrain_avoidance(&world, tf.translation, desired);
        // Inertial steering: ease velocity toward `desired` rather than
        // snapping. This gives the drone a believable arc instead of the
        // jittery, ricocheting old motion.
        let steer_ease = 1.0 - (-dt * 2.2).exp();
        let current_v = drone.velocity;
        drone.velocity = current_v + (desired - current_v) * steer_ease;
        // Cap to a sensible top speed so flock stays cinematic.
        let max_v = 48.0;
        if drone.velocity.length() > max_v {
            drone.velocity = drone.velocity.normalize_or_zero() * max_v;
        }
        tf.translation += drone.velocity * dt;
        tf.translation.y = tf.translation.y.max(10.0);

        let cell = tf.translation.floor().as_ivec3();
        if world.is_solid(cell.x, cell.y, cell.z) {
            tf.translation.y += 16.0 * dt;
        }
        // Smooth aim: slerp toward the target rotation rather than snapping.
        let look_target = ship_tf.translation;
        let to_look = look_target - tf.translation;
        if to_look.length_squared() > 0.01 {
            let mut wanted = Transform::from_translation(tf.translation);
            wanted.look_at(look_target, Vec3::Y);
            let look_ease = (1.0 - (-dt * 4.5).exp()).clamp(0.0, 1.0);
            tf.rotation = tf.rotation.slerp(wanted.rotation, look_ease);
        }
        if drone.fire_cooldown <= 0.0
            && dist < 160.0
            && has_line_of_sight(&world, tf.translation, ship_tf.translation)
        {
            drone.fire_cooldown = (1.35 + drone_count as f32 * 0.22).min(4.2);
            spawn_ship_projectile(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut fx,
                tf.translation,
                target_dir,
                ProjectileOwner::Drone,
                Color::srgb(1.0, 0.08, 0.55),
                WeaponProfile {
                    speed: 135.0,
                    damage: 8.0,
                    radius: 2.0,
                    cooldown: 1.0,
                    size: Vec3::new(0.10, 0.10, 1.2),
                },
            );
        }
    }
}

fn drone_terrain_avoidance(world: &VoxelWorld, pos: Vec3, desired: Vec3) -> Vec3 {
    let dir = desired.normalize_or_zero();
    if dir.length_squared() <= 0.001 {
        return Vec3::ZERO;
    }
    let mut push = Vec3::ZERO;
    if crate::sculpt::raycast::dda_voxel(world, pos, dir, 18.0).is_some() {
        push += Vec3::Y * 48.0 - dir * 22.0;
    }
    if crate::sculpt::raycast::dda_voxel(world, pos, Vec3::NEG_Y, 14.0).is_some() {
        push += Vec3::Y * 26.0;
    }
    push
}

fn has_line_of_sight(world: &VoxelWorld, from: Vec3, to: Vec3) -> bool {
    let delta = to - from;
    let dist = delta.length();
    if dist <= 0.01 {
        return true;
    }
    crate::sculpt::raycast::dda_voxel(world, from, delta / dist, dist.min(180.0)).is_none()
}

fn spawn_ship_explosion(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    pos: Vec3,
    radius: f32,
) {
    let mesh = fx
        .explosion
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.1, 0.85, 1.0, 0.42),
        emissive: LinearRgba::rgb(1.5, 4.0, 5.0),
        alpha_mode: AlphaMode::Add,
        ..default()
    });
    commands.spawn((
        PbrBundle {
            mesh,
            material: mat,
            transform: Transform::from_translation(pos).with_scale(Vec3::splat(radius.max(1.0))),
            ..default()
        },
        ShipExplosion {
            life: 0.28,
            max_life: 0.28,
        },
        Name::new("ShipExplosion"),
    ));
}

fn update_ship_explosions(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut ShipExplosion)>,
) {
    for (entity, mut tf, mut exp) in q.iter_mut() {
        exp.life -= time.delta_seconds();
        let t = 1.0 - (exp.life / exp.max_life).clamp(0.0, 1.0);
        tf.scale *= 1.0 + t * 0.08;
        if exp.life <= 0.0 {
            despawn(&mut commands, entity);
        }
    }
}

fn draw_ship_cockpit_hud(
    mut contexts: EguiContexts,
    time: Res<Time>,
    settings: Res<WorldSettings>,
    pilot: Res<PilotState>,
    mode: Res<ModeContext>,
    director: Option<Res<SimulationDirector>>,
    camera_q: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
    drones: Query<&GlobalTransform, With<EnemyDrone>>,
) {
    if pilot.active_ship.is_none() || !matches!(mode.mode, ActiveMode::ShipFlight { .. }) {
        return;
    }
    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ship_cockpit_hud"),
    ));
    let colors = settings.theme.semantic();
    let cyan = colors.info;
    let magenta = egui::Color32::from_rgb(255, 40, 220);
    let amber = colors.warning;
    let glass = egui::Color32::from_rgba_unmultiplied(10, 36, 48, 108);
    if pilot.shield_flash > 0.01 {
        painter.rect_filled(
            screen,
            egui::Rounding::ZERO,
            egui::Color32::from_rgba_unmultiplied(
                255,
                35,
                90,
                (pilot.shield_flash.clamp(0.0, 1.0) * 58.0) as u8,
            ),
        );
    }

    let left = egui::Rect::from_min_max(
        screen.min,
        egui::pos2(screen.left() + 140.0, screen.bottom()),
    );
    let right =
        egui::Rect::from_min_max(egui::pos2(screen.right() - 140.0, screen.top()), screen.max);
    let bottom = egui::Rect::from_min_max(
        egui::pos2(screen.left() + 132.0, screen.bottom() - 142.0),
        egui::pos2(screen.right() - 132.0, screen.bottom()),
    );
    draw_liquid_cockpit_visor(
        &painter,
        screen,
        settings.theme,
        time.elapsed_seconds_wrapped(),
        cyan,
        magenta,
        amber,
    );
    crate::ui_kit::hud_panel(&painter, left.shrink(8.0), settings.theme, 0.62, cyan);
    crate::ui_kit::hud_panel(&painter, right.shrink(8.0), settings.theme, 0.62, magenta);
    crate::ui_kit::hud_panel(&painter, bottom.shrink(4.0), settings.theme, 0.72, cyan);
    draw_cockpit_dashboard(
        &painter,
        screen,
        settings.theme,
        cyan,
        magenta,
        amber,
        glass,
    );

    let cam_pos = camera_q
        .get_single()
        .map(|(_, g)| g.translation())
        .unwrap_or(Vec3::ZERO);
    if let Some(dir) = director.as_deref() {
        let (dest_name, dest_pt) = dir.navigation_dest();
        let dist_m = Vec2::new(cam_pos.x - dest_pt.x, cam_pos.z - dest_pt.z).length();
        let dist_km = dist_m / 1000.0;
        draw_hud_text(
            &painter,
            egui::pos2(screen.center().x - 120.0, screen.top() + 22.0),
            &format!("DEST // {}  {:.1} km", dest_name, dist_km),
            amber,
            15.0,
        );
        draw_hud_text(
            &painter,
            egui::pos2(screen.center().x - 118.0, screen.top() + 42.0),
            &dir.cockpit_line(),
            egui::Color32::from_rgba_unmultiplied(180, 235, 255, 220),
            11.0,
        );
    }

    let thrust = (pilot.speed / pilot.cruise_max_speed.max(1.0)).clamp(0.0, 1.0);
    let boost_bar = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, screen.bottom() - 152.0),
        egui::vec2(240.0, 7.0),
    );
    painter.rect_filled(
        boost_bar,
        egui::Rounding::same(3.0),
        egui::Color32::from_gray(22),
    );
    painter.rect_filled(
        boost_bar.with_max_x(boost_bar.left() + boost_bar.width() * thrust),
        egui::Rounding::same(3.0),
        cyan,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.center().x - 118.0, screen.bottom() - 168.0),
        "THRUST / BOOST",
        magenta,
        11.0,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.right() - 210.0, screen.top() + 58.0),
        if settings.ship_skirmish_ai {
            "KI-GEFECHT  AN"
        } else {
            "KI-GEFECHT  AUS  (E Inventar)"
        },
        if settings.ship_skirmish_ai {
            magenta
        } else {
            egui::Color32::from_gray(150)
        },
        10.0,
    );
    if pilot.entry_peace_timer > 0.15 {
        draw_hud_text(
            &painter,
            egui::pos2(screen.right() - 210.0, screen.top() + 74.0),
            &format!(
                "FRIEDEN  {:>4.1}s",
                pilot.entry_peace_timer.clamp(0.0, 999.0)
            ),
            egui::Color32::from_rgb(120, 255, 190),
            10.0,
        );
    }

    painter.line_segment(
        [
            egui::pos2(screen.left() + 145.0, screen.top()),
            egui::pos2(screen.left() + 220.0, screen.bottom() - 142.0),
        ],
        egui::Stroke::new(4.0, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 215)),
    );
    painter.line_segment(
        [
            egui::pos2(screen.right() - 145.0, screen.top()),
            egui::pos2(screen.right() - 220.0, screen.bottom() - 142.0),
        ],
        egui::Stroke::new(4.0, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 215)),
    );
    painter.line_segment(
        [
            egui::pos2(screen.left() + 220.0, screen.bottom() - 142.0),
            egui::pos2(screen.right() - 220.0, screen.bottom() - 142.0),
        ],
        egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 160)),
    );

    let center = screen.center();
    painter.circle_stroke(center, 22.0, egui::Stroke::new(1.5, cyan));
    painter.line_segment(
        [
            center - egui::vec2(45.0, 0.0),
            center - egui::vec2(12.0, 0.0),
        ],
        egui::Stroke::new(1.0, cyan),
    );
    painter.line_segment(
        [
            center + egui::vec2(12.0, 0.0),
            center + egui::vec2(45.0, 0.0),
        ],
        egui::Stroke::new(1.0, cyan),
    );
    if let Ok((camera, camera_tf)) = camera_q.get_single() {
        for drone_tf in drones.iter().take(8) {
            if let Some(viewport) = camera.world_to_viewport(camera_tf, drone_tf.translation()) {
                let target = egui::pos2(viewport.x, viewport.y);
                if screen.contains(target) {
                    draw_target_bracket(&painter, target, 20.0, magenta);
                }
            }
        }
    }

    draw_hud_text(
        &painter,
        egui::pos2(screen.left() + 35.0, screen.bottom() - 115.0),
        &format!("SPEED\n{:.0} u/s", pilot.speed),
        cyan,
        18.0,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.left() + 35.0, screen.bottom() - 55.0),
        &format!("SHIELD\n{:03}%", pilot.shield.round() as i32),
        if pilot.shield > 35.0 { cyan } else { amber },
        18.0,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.right() - 150.0, screen.bottom() - 115.0),
        pilot.weapon.label(),
        magenta,
        16.0,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.right() - 150.0, screen.bottom() - 55.0),
        &format!("DRONES\n{:02}", drones.iter().count()),
        amber,
        18.0,
    );
    if pilot.shield < 35.0 || pilot.shield_flash > 0.15 {
        draw_hud_text(
            &painter,
            egui::pos2(screen.center().x - 84.0, screen.bottom() - 122.0),
            &pilot.status,
            if pilot.shield < 35.0 { amber } else { magenta },
            14.0,
        );
    }

    let radar_center = egui::pos2(screen.center().x, screen.bottom() - 66.0);
    painter.circle_stroke(radar_center, 42.0, egui::Stroke::new(1.0, cyan));
    painter.circle_stroke(
        radar_center,
        21.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 90)),
    );
    draw_ship_silhouette(&painter, radar_center - egui::vec2(0.0, 4.0), cyan, amber);
    for (i, _) in drones.iter().enumerate().take(8) {
        let a = i as f32 * 1.93 + time.elapsed_seconds_wrapped() * 0.25;
        let r = 14.0 + (i % 3) as f32 * 9.0;
        painter.circle_filled(
            radar_center + egui::vec2(a.cos() * r, a.sin() * r),
            3.0,
            magenta,
        );
    }

    let weather = settings.weather;
    let weather_alpha = (weather.rain_intensity.max(weather.snow_intensity) * 160.0) as u8;
    if weather_alpha > 5 {
        for i in 0..36 {
            let t =
                time.elapsed_seconds_wrapped() * (0.25 + pilot.speed * 0.015) + i as f32 * 13.17;
            let x = screen.left() + (t.sin() * 0.5 + 0.5) * screen.width();
            let y = screen.top() + ((t * 0.37).fract()) * screen.height();
            painter.line_segment(
                [egui::pos2(x, y), egui::pos2(x + 8.0, y + 22.0)],
                egui::Stroke::new(
                    1.2,
                    egui::Color32::from_rgba_unmultiplied(150, 235, 255, weather_alpha),
                ),
            );
        }
    }
}

fn draw_hud_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    size: f32,
) {
    painter.text(
        pos,
        egui::Align2::LEFT_TOP,
        text,
        egui::FontId::monospace(size),
        color,
    );
}

fn draw_liquid_cockpit_visor(
    painter: &egui::Painter,
    screen: egui::Rect,
    theme: crate::theme::ThemeSettings,
    time: f32,
    cyan: egui::Color32,
    magenta: egui::Color32,
    amber: egui::Color32,
) {
    let colors = theme.semantic();
    let top = egui::Rect::from_min_max(
        screen.left_top(),
        egui::pos2(screen.right(), screen.top() + 96.0),
    );
    painter.rect_filled(
        top,
        egui::Rounding::ZERO,
        egui::Color32::from_rgba_unmultiplied(218, 246, 255, 22),
    );

    let horizon_y = screen.center().y + (time * 0.7).sin() * 2.0;
    painter.line_segment(
        [
            egui::pos2(screen.left() + 210.0, horizon_y),
            egui::pos2(screen.right() - 210.0, horizon_y),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(210, 250, 255, 96),
        ),
    );
    for i in 0..9 {
        let t = i as f32 / 8.0;
        let x = screen.left() + 250.0 + t * (screen.width() - 500.0);
        let alpha = if i == 4 { 165 } else { 82 };
        let height = if i == 4 { 18.0 } else { 9.0 };
        painter.line_segment(
            [
                egui::pos2(x, horizon_y - height),
                egui::pos2(x, horizon_y + height),
            ],
            egui::Stroke::new(
                1.0,
                egui::Color32::from_rgba_unmultiplied(cyan.r(), cyan.g(), cyan.b(), alpha),
            ),
        );
    }

    let capsule = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, screen.top() + 84.0),
        egui::vec2(360.0, 42.0),
    );
    crate::ui_kit::hud_panel(painter, capsule, theme, 0.48, amber);
    draw_hud_text(
        painter,
        capsule.left_top() + egui::vec2(18.0, 10.0),
        "LIQUID FLIGHT CORE   AUTO STREAM / SMART TARGET / CREW LINK",
        colors.text,
        11.0,
    );

    for side in [-1.0_f32, 1.0] {
        let x0 = if side < 0.0 {
            screen.left() + 156.0
        } else {
            screen.right() - 156.0
        };
        let stroke = egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(magenta.r(), magenta.g(), magenta.b(), 100),
        );
        for row in 0..6 {
            let y = screen.top() + 126.0 + row as f32 * 48.0;
            painter.line_segment(
                [
                    egui::pos2(x0, y),
                    egui::pos2(x0 + side * (28.0 + row as f32 * 3.0), y + 22.0),
                ],
                stroke,
            );
        }
    }
}

fn draw_cockpit_dashboard(
    painter: &egui::Painter,
    screen: egui::Rect,
    theme: crate::theme::ThemeSettings,
    cyan: egui::Color32,
    magenta: egui::Color32,
    amber: egui::Color32,
    glass: egui::Color32,
) {
    let center_x = screen.center().x;
    let bottom = screen.bottom();
    let main = egui::Rect::from_min_max(
        egui::pos2(center_x - 250.0, bottom - 128.0),
        egui::pos2(center_x + 250.0, bottom - 10.0),
    );
    crate::ui_kit::hud_panel(painter, main, theme, 0.76, cyan);
    painter.rect_filled(main.shrink(10.0), egui::Rounding::same(6.0), glass);

    let map = egui::Rect::from_min_max(
        egui::pos2(center_x - 150.0, bottom - 114.0),
        egui::pos2(center_x + 150.0, bottom - 36.0),
    );
    crate::ui_kit::hud_panel(painter, map, theme, 0.54, magenta);
    for i in 0..8 {
        let x = map.left() + i as f32 * map.width() / 7.0;
        painter.line_segment(
            [egui::pos2(x, map.top()), egui::pos2(x, map.bottom())],
            egui::Stroke::new(0.6, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 38)),
        );
    }
    for i in 0..5 {
        let y = map.top() + i as f32 * map.height() / 4.0;
        painter.line_segment(
            [egui::pos2(map.left(), y), egui::pos2(map.right(), y)],
            egui::Stroke::new(0.6, egui::Color32::from_rgba_unmultiplied(0, 235, 255, 38)),
        );
    }

    for side in [-1.0_f32, 1.0] {
        let panel = egui::Rect::from_min_max(
            egui::pos2(
                center_x + side * 285.0 - if side > 0.0 { 0.0 } else { 170.0 },
                bottom - 122.0,
            ),
            egui::pos2(
                center_x + side * 285.0 + if side > 0.0 { 170.0 } else { 0.0 },
                bottom - 22.0,
            ),
        );
        crate::ui_kit::hud_panel(
            painter,
            panel,
            theme,
            0.62,
            if side > 0.0 { magenta } else { cyan },
        );
        for row in 0..3 {
            for col in 0..4 {
                let idx = row * 4 + col;
                let color = match idx % 4 {
                    0 => cyan,
                    1 => magenta,
                    2 => amber,
                    _ => egui::Color32::from_rgb(70, 255, 120),
                };
                let p = egui::pos2(
                    panel.left() + 18.0 + col as f32 * 34.0,
                    panel.top() + 18.0 + row as f32 * 27.0,
                );
                let r = egui::Rect::from_min_size(p, egui::vec2(22.0, 14.0));
                painter.rect_filled(
                    r,
                    egui::Rounding::same(2.0),
                    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 172),
                );
                painter.rect_stroke(r, egui::Rounding::same(2.0), egui::Stroke::new(0.8, color));
            }
        }
    }
}

fn draw_target_bracket(
    painter: &egui::Painter,
    center: egui::Pos2,
    half: f32,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.4, color);
    let gap = half * 0.48;
    let corners = [
        (
            egui::vec2(-half, -half),
            egui::vec2(-gap, -half),
            egui::vec2(-half, -gap),
        ),
        (
            egui::vec2(half, -half),
            egui::vec2(gap, -half),
            egui::vec2(half, -gap),
        ),
        (
            egui::vec2(-half, half),
            egui::vec2(-gap, half),
            egui::vec2(-half, gap),
        ),
        (
            egui::vec2(half, half),
            egui::vec2(gap, half),
            egui::vec2(half, gap),
        ),
    ];
    for (corner, horizontal, vertical) in corners {
        painter.line_segment([center + corner, center + horizontal], stroke);
        painter.line_segment([center + corner, center + vertical], stroke);
    }
    painter.circle_stroke(center, 3.5, egui::Stroke::new(1.0, color));
}

fn draw_ship_silhouette(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    accent: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.1, color);
    let nose = center + egui::vec2(0.0, -24.0);
    let tail = center + egui::vec2(0.0, 25.0);
    let left_wing = center + egui::vec2(-23.0, 9.0);
    let right_wing = center + egui::vec2(23.0, 9.0);
    let left_fin = center + egui::vec2(-11.0, 22.0);
    let right_fin = center + egui::vec2(11.0, 22.0);
    painter.add(egui::Shape::closed_line(
        vec![nose, right_wing, right_fin, tail, left_fin, left_wing],
        stroke,
    ));
    painter.line_segment([nose, tail], egui::Stroke::new(0.8, color));
    painter.line_segment(
        [
            center + egui::vec2(-7.0, 18.0),
            center + egui::vec2(7.0, 18.0),
        ],
        egui::Stroke::new(2.0, accent),
    );
}

fn despawn(commands: &mut Commands, entity: Entity) {
    if let Some(entity_commands) = commands.get_entity(entity) {
        entity_commands.despawn_recursive();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blueprints_are_not_empty() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            assert!(!bp.voxels.is_empty());
            assert!(bp.max_speed > 0.0);
            assert!(bp.shield > 0.0);
        }
    }

    #[test]
    fn blueprints_stay_inside_runtime_bounds() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            for voxel in &bp.voxels {
                assert!(voxel.pos.x.abs() <= 10, "{kind:?} x bound {:?}", voxel.pos);
                assert!(
                    voxel.pos.y >= -4 && voxel.pos.y <= 6,
                    "{kind:?} y bound {:?}",
                    voxel.pos
                );
                assert!(voxel.pos.z.abs() <= 14, "{kind:?} z bound {:?}", voxel.pos);
            }
            assert!(bp.hardpoints.iter().all(|p| p.length() < 18.0));
            assert!(bp.cockpit_offset.length() < 18.0);
            assert!(bp.exit_offset.length() < 20.0);
        }
    }

    #[test]
    fn blueprints_have_future_shuttle_reference_traits() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            assert!(
                bp.voxels
                    .iter()
                    .any(|v| v.block == BlockType::CockpitGlass && v.pos.z < -3),
                "{kind:?} should have a solid smoked cockpit nose"
            );
            let hull_block = if kind == ShipKind::ScoutShuttle {
                BlockType::PlatingWhite
            } else {
                BlockType::ShipHullAlloy
            };
            assert!(
                bp.voxels.iter().filter(|v| v.block == hull_block).count() >= 48,
                "{kind:?} should read as a bright shuttle hull, not a sparse wireframe"
            );
            assert!(
                bp.voxels
                    .iter()
                    .filter(|v| v.block == BlockType::EngineCore)
                    .count()
                    >= 2,
                "{kind:?} should expose visible engine cores for the cyan wake"
            );
        }
    }

    #[test]
    fn all_ship_exteriors_have_luminous_detail_language() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            if kind == ShipKind::ScoutShuttle {
                let white = bp
                    .voxels
                    .iter()
                    .filter(|v| v.block == BlockType::PlatingWhite)
                    .count();
                let orange = bp
                    .voxels
                    .iter()
                    .filter(|v| v.block == BlockType::NeonAmber)
                    .count();
                let glass = bp
                    .voxels
                    .iter()
                    .filter(|v| v.block == BlockType::CockpitGlass)
                    .count();
                assert!(white >= 48, "scout hull should be white plating, got {white}");
                assert!(orange >= 8, "scout needs orange livery, got {orange}");
                assert!(glass >= 4, "scout needs a cockpit glass strip, got {glass}");
                continue;
            }
            let luminite = bp
                .voxels
                .iter()
                .filter(|v| v.block == BlockType::LuminiteCrystal)
                .count();
            let magenta = bp
                .voxels
                .iter()
                .filter(|v| v.block == BlockType::NeonMagenta)
                .count();
            assert!(
                luminite >= 6,
                "{kind:?} exterior should have liquid-glass luminite edge markers, got {luminite}"
            );
            assert!(
                magenta >= 4,
                "{kind:?} exterior should have secondary magenta signal accents, got {magenta}"
            );
        }
    }

    #[test]
    fn all_ship_cockpits_have_rich_kind_specific_interiors() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            let panels = cockpit_panel_specs(kind, &bp);
            let lit = panels
                .iter()
                .filter(|panel| {
                    matches!(
                        panel.tone,
                        CockpitPanelTone::Cyan
                            | CockpitPanelTone::Magenta
                            | CockpitPanelTone::Amber
                    )
                })
                .count();
            let structure = panels
                .iter()
                .filter(|panel| {
                    matches!(
                        panel.tone,
                        CockpitPanelTone::Shell
                            | CockpitPanelTone::Seat
                            | CockpitPanelTone::Frame
                            | CockpitPanelTone::Glass
                    )
                })
                .count();
            assert!(
                panels.len() >= 28,
                "{kind:?} cockpit should have a dense interior layout, got {} panels",
                panels.len()
            );
            assert!(
                lit >= 13,
                "{kind:?} cockpit should have layered holographic controls, got {lit}"
            );
            assert!(
                structure >= 10,
                "{kind:?} cockpit should have visible seats, frames, ribs and glass structure, got {structure}"
            );
        }
    }

    #[test]
    fn scout_shuttle_reads_as_a_winged_white_orange_craft() {
        let shell = realistic_ship_exterior_specs(ShipKind::ScoutShuttle);
        let plates = shell
            .iter()
            .filter(|part| part.mesh == RealShipMeshKind::AeroPlate)
            .count();
        assert!(
            plates >= 8,
            "scout should be a cuboid shuttle silhouette, got {plates} plates"
        );
        assert!(
            shell
                .iter()
                .any(|part| part.tone == RealShipTone::ShuttleWhite),
            "scout hull should be readable shuttle white"
        );
        assert!(
            shell
                .iter()
                .any(|part| part.tone == RealShipTone::ShuttleOrange),
            "scout needs opaque orange livery, not only additive heat"
        );
        assert!(
            shell
                .iter()
                .any(|part| part.tone == RealShipTone::CyanEmission && part.offset.z < 0.0),
            "scout needs a cyan cockpit strip on the nose"
        );
        assert!(
            shell
                .iter()
                .filter(|part| part.mesh == RealShipMeshKind::RoundNozzle)
                .count()
                >= 2,
            "scout still needs round engine nozzles"
        );
        let xs: Vec<f32> = shell.iter().map(|part| part.offset.x.abs() + part.scale.x * 0.5).collect();
        let zs: Vec<f32> = shell.iter().map(|part| part.offset.z.abs() + part.scale.z * 0.5).collect();
        let wingspan = xs.into_iter().fold(0.0_f32, f32::max);
        let length = zs.into_iter().fold(0.0_f32, f32::max);
        assert!(wingspan >= 4.0, "wings too stubby to read, span={wingspan}");
        assert!(length >= 4.5, "fuselage too short to read, length={length}");
    }

    #[test]
    fn visible_ship_renderer_uses_smooth_realistic_meshes_not_voxel_blocks() {
        for kind in [ShipKind::StrikeFighter, ShipKind::HeavyDropship] {
            let shell = realistic_ship_exterior_specs(kind);
            assert!(
                shell
                    .iter()
                    .filter(|part| part.mesh == RealShipMeshKind::SmoothEllipsoid)
                    .count()
                    >= 3,
                "{kind:?} should be built from smooth body/canopy ellipsoids"
            );
            assert!(
                shell
                    .iter()
                    .any(|part| part.tone == RealShipTone::CeramicWhite),
                "{kind:?} needs the white aircraft-like body from the reference"
            );
            assert!(
                shell
                    .iter()
                    .any(|part| part.tone == RealShipTone::SmokedGlass),
                "{kind:?} needs a black smoked real cockpit canopy"
            );
            assert!(
                shell
                    .iter()
                    .filter(|part| part.mesh == RealShipMeshKind::RoundNozzle)
                    .count()
                    >= 2,
                "{kind:?} needs round engine nozzles, not cube engines"
            );
        }
    }

    #[test]
    fn visible_cockpit_renderer_has_real_seat_glass_and_controls() {
        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            let real = realistic_cockpit_part_specs(kind, &bp);
            assert!(
                real.iter()
                    .any(|part| part.tone == RealShipTone::SeatLeather),
                "{kind:?} cockpit should have a real pilot seat"
            );
            assert!(
                real.iter()
                    .any(|part| part.tone == RealShipTone::SmokedGlass),
                "{kind:?} cockpit should have a real inner canopy glass layer"
            );
            assert!(
                real.iter()
                    .filter(|part| part.tone == RealShipTone::ConsoleBlack)
                    .count()
                    >= 2,
                "{kind:?} cockpit should have physical console surfaces"
            );
            assert!(
                real.iter()
                    .filter(|part| part.mesh == RealShipMeshKind::RoundNozzle)
                    .count()
                    >= 2,
                "{kind:?} cockpit should have yoke/throttle cylinders"
            );
        }
    }

    #[test]
    fn ceramic_hull_stays_below_the_bloom_threshold() {
        // Matches RealShipTone::CeramicWhite base colour. Linear peak
        // must sit under Bevy OLD_SCHOOL's ~0.6-0.74 prefilter so midday
        // sun does not turn the orbiter into a white blob.
        let lin = Color::srgb(0.42, 0.37, 0.30).to_linear();
        let peak = lin.red.max(lin.green).max(lin.blue);
        assert!(
            peak < 0.40,
            "ceramic peak {peak:.3} will bloom into a white hull blob"
        );
    }

    #[test]
    fn all_ship_kinds_spawn_cyan_and_amber_energy_wakes() {
        for kind in ShipKind::ALL {
            let specs = ship_trail_specs(kind);
            assert!(
                specs
                    .iter()
                    .filter(|spec| spec.tone == ShipTrailTone::Cyan)
                    .count()
                    >= 2,
                "{kind:?} should leave dual cyan energy trails"
            );
            assert!(
                specs.iter().any(|spec| spec.tone == ShipTrailTone::Amber),
                "{kind:?} should have an amber heat bloom around high thrust"
            );
            assert!(
                specs.iter().any(|spec| spec.base_scale.z >= 4.0),
                "{kind:?} trails should be long enough to read at flight speed"
            );
        }
    }

    #[test]
    fn ship_wave_response_is_idle_safe_and_visible_at_speed() {
        let idle = ship_wave_response(ShipKind::ScoutShuttle, 0.0, 90.0, 2.0);
        assert_eq!(idle.vertical_velocity, 0.0);
        assert_eq!(idle.pitch, 0.0);
        assert_eq!(idle.roll, 0.0);

        let cruise = ship_wave_response(ShipKind::ScoutShuttle, 72.0, 90.0, 2.0);
        assert!(cruise.vertical_velocity.abs() > 0.01);
        assert!(cruise.pitch.abs() > 0.001 || cruise.roll.abs() > 0.001);
        assert!(cruise.vertical_velocity.abs() <= 4.0);
        assert!(cruise.pitch.abs() <= 0.045);
        assert!(cruise.roll.abs() <= 0.065);
    }

    #[test]
    fn ship_keyboard_turning_is_deliberately_cinematic_not_twitchy() {
        let (yaw, pitch) = ship_target_angular_rates(0.0, 0.0, 1.0, 0.0, 1.0 / 60.0);

        assert!(yaw > 0.0);
        assert!(yaw <= 0.45, "A/D yaw should stay smooth, got {yaw}");
        assert_eq!(pitch, 0.0);
    }

    #[test]
    fn ship_mouse_yaw_is_a_small_aiming_trim() {
        let (yaw, _) = ship_target_angular_rates(18.0, 0.0, 0.0, 0.0, 1.0 / 60.0);

        assert!(
            yaw.abs() < 0.28,
            "mouse left/right should only trim the shuttle nose, got {yaw}"
        );
    }

    #[test]
    fn ship_inventory_defaults_to_unlocked_ships() {
        let inv = ShipInventory::default();
        assert_eq!(inv.unlocked.len(), ShipKind::ALL.len());
    }

    #[test]
    fn saved_ship_instance_defaults_missing_shield() {
        let text = "(kind: ScoutShuttle, pos: (1.0, 2.0, 3.0), yaw: 0.5)";
        let saved: SavedShipInstance = ron::from_str(text).unwrap();
        assert_eq!(saved.kind, ShipKind::ScoutShuttle);
        assert_eq!(saved.shield, 100.0);
    }

    #[test]
    fn hero_flyby_crosses_in_front_of_the_new_world_look() {
        let origin = Vec3::new(64.0, 58.0, -79.0);
        let (forward, _forward_h, _right_h) = super::new_world_look_basis();
        for u in [0.10, 0.20, 0.32, 0.48] {
            let (pos, yaw, roll) = super::hero_flyby_pose(origin, u);
            let rel = pos - origin;
            let ahead = rel.dot(forward);
            let dist = rel.length();
            assert!(
                ahead > 36.0,
                "flyby at u={u} is not ahead of the camera (ahead={ahead}, pos={pos})"
            );
            assert!(
                ahead < 80.0,
                "flyby at u={u} leaves the opening sky (ahead={ahead})"
            );
            assert!(
                dist > 40.0 && dist < 95.0,
                "flyby at u={u} should sit in the open sky above the look, dist={dist}"
            );
            assert!(
                pos.y > origin.y + 14.0,
                "flyby at u={u} is not in the open sky (y={})",
                pos.y
            );
            assert!(
                pos.y < origin.y + 22.0,
                "flyby at u={u} sits above the opening frustum (y={})",
                pos.y
            );
            assert!(yaw.abs() > 0.20, "flyby should bank across the look, yaw={yaw}");
            assert!(roll.abs() < 1.2);
        }
        assert!(
            (2.5..6.5).contains(&super::HERO_FLYBY_SCALE),
            "hero scale {} is a hull wall or a dot",
            super::HERO_FLYBY_SCALE
        );
    }

    #[test]
    fn sky_traffic_is_bounded_and_loops() {
        assert_eq!(
            super::sky_traffic_count(GraphicsMode::Fast, false),
            2
        );
        assert_eq!(
            super::sky_traffic_count(GraphicsMode::Balanced, false),
            4
        );
        assert_eq!(
            super::sky_traffic_count(GraphicsMode::High, false),
            5
        );
        assert_eq!(
            super::sky_traffic_count(GraphicsMode::High, true),
            6
        );
        let lanes = super::sky_traffic_lanes();
        assert_eq!(lanes.len(), 6);
        let (origin, span, _, _, t0, _) = lanes[0];
        let (a, _) = super::sky_traffic_pose(origin, span, t0);
        let (b, _) = super::sky_traffic_pose(origin, span, t0 + 1.0);
        let (c, _) = super::sky_traffic_pose(origin, span, t0 + 2.0);
        assert!(a.distance(b) < 0.05, "traffic must wrap, not accumulate");
        assert!(a.distance(c) < 0.05);
        let simple = super::ambient_traffic_specs(0, false);
        let detailed = super::ambient_traffic_specs(0, true);
        assert!(simple.len() <= 5, "Fast traffic must stay cheap, got {}", simple.len());
        assert!(detailed.len() > simple.len());
        assert!(simple.iter().any(|part| part.tone == RealShipTone::CyanEmission));
    }

    #[test]
    fn shuttle_paint_stays_opaque_and_readable() {
        let white = Color::srgb(0.78, 0.72, 0.64).to_linear();
        let orange = Color::srgb(0.74, 0.32, 0.07).to_linear();
        let white_peak = white.red.max(white.green).max(white.blue);
        let orange_peak = orange.red.max(orange.green).max(orange.blue);
        assert!(
            white_peak > 0.45,
            "shuttle white {white_peak:.3} will read as grey ceramic"
        );
        assert!(
            white_peak < 0.72,
            "shuttle white {white_peak:.3} will bloom into a blob"
        );
        assert!(
            orange_peak > 0.35,
            "shuttle orange {orange_peak:.3} will not read as livery"
        );
    }
}
