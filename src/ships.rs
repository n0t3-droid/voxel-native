//! Shuttle battles: local voxel ships, cockpit flight, ship weapons and drones.
//!
//! Ships are moving entity hierarchies built from voxel-colored cubes. They do
//! not mutate terrain chunks while flying, which keeps chunk streaming and the
//! mesher stable.

use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy_egui::{egui, EguiContexts};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::blocks::{voxel_color, voxel_is_emissive, BlockType, Voxel};
use crate::director::{SimulationDirector, UnifiedTelemetry};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::player::Player;
use crate::settings::{ActiveWorld, WorldSettings};
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
    projectile: Option<Handle<Mesh>>,
    explosion: Option<Handle<Mesh>>,
    mats: std::collections::HashMap<(u16, bool), Handle<StandardMaterial>>,
    textures: std::collections::HashMap<(u16, bool), Handle<Image>>,
    projectile_mats: std::collections::HashMap<u8, Handle<StandardMaterial>>,
    cockpit_mats: std::collections::HashMap<u8, Handle<StandardMaterial>>,
    drone_mat: Option<Handle<StandardMaterial>>,
}

#[derive(Clone)]
struct ShipBlueprint {
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

    // -- Star Wars X-Wing inspired Scout --

    // Long slender nose/fuselage
    push_box(
        &mut voxels,
        IVec3::new(-1, 0, -8),
        IVec3::new(1, 1, 3),
        BlockType::ShipHullAlloy,
    );

    // Astromech/Sensor slot stripe (Cyan) behind cockpit
    push_box(
        &mut voxels,
        IVec3::new(0, 1, 2),
        IVec3::new(0, 1, 3),
        BlockType::NeonCyan,
    );

    // Fighter canopy frame
    push_box(
        &mut voxels,
        IVec3::new(-1, 2, -1),
        IVec3::new(1, 2, 1),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 2, -1),
        IVec3::new(0, 2, 1),
        BlockType::CockpitGlass,
    );

    // Rear engine block
    push_box(
        &mut voxels,
        IVec3::new(-2, 0, 4),
        IVec3::new(2, 2, 5),
        BlockType::ShipHullDark,
    );

    // X-foils / S-foils (Wings)
    for &sx in &[-1, 1] {
        let root_x = if sx > 0 { 2 } else { -2 };
        let tip_x = if sx > 0 { 8 } else { -8 };

        // Top wings
        push_box(
            &mut voxels,
            IVec3::new(root_x, 2, 1),
            IVec3::new(tip_x, 2, 3),
            BlockType::ShipHullAlloy,
        );
        push_box(
            &mut voxels,
            IVec3::new(tip_x, 2, -3),
            IVec3::new(tip_x, 2, 3),
            BlockType::ShipHullDark,
        ); // Cannons

        // Bottom wings
        push_box(
            &mut voxels,
            IVec3::new(root_x, -1, 1),
            IVec3::new(tip_x, -1, 3),
            BlockType::ShipHullAlloy,
        );
        push_box(
            &mut voxels,
            IVec3::new(tip_x, -1, -3),
            IVec3::new(tip_x, -1, 3),
            BlockType::ShipHullDark,
        ); // Cannons

        // 4x Engine nozzles
        let ex_root = if sx > 0 { 2 } else { -3 };
        let ex_tip = if sx > 0 { 3 } else { -2 };

        push_box(
            &mut voxels,
            IVec3::new(ex_root, 2, 4),
            IVec3::new(ex_tip, 3, 4),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(ex_root, 2, 5),
            IVec3::new(ex_tip, 3, 5),
            BlockType::EngineCore,
        );

        push_box(
            &mut voxels,
            IVec3::new(ex_root, -1, 4),
            IVec3::new(ex_tip, 0, 4),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(ex_root, -1, 5),
            IVec3::new(ex_tip, 0, 5),
            BlockType::EngineCore,
        );
    }
    push_box(
        &mut voxels,
        IVec3::new(-1, 2, -6),
        IVec3::new(1, 2, -4),
        BlockType::CockpitGlass,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 2, -8),
        IVec3::new(0, 2, -7),
        BlockType::NeonCyan,
    );
    push_box(
        &mut voxels,
        IVec3::new(-2, 1, 1),
        IVec3::new(2, 1, 3),
        BlockType::ShipHullAlloy,
    );
    push_box(
        &mut voxels,
        IVec3::new(-1, 2, 1),
        IVec3::new(1, 2, 3),
        BlockType::ShipHullDark,
    );
    push_box(
        &mut voxels,
        IVec3::new(0, 3, 2),
        IVec3::new(0, 3, 3),
        BlockType::ShipHullAlloy,
    );
    for &(z, inner, outer, block) in &[
        (-3, 2, 3, BlockType::ShipHullDark),
        (-2, 3, 4, BlockType::ShipHullAlloy),
        (-1, 4, 6, BlockType::ShipHullDark),
        (0, 5, 8, BlockType::ShipHullAlloy),
        (1, 5, 8, BlockType::ShipHullDark),
        (2, 4, 7, BlockType::ShipHullAlloy),
    ] {
        push_box(
            &mut voxels,
            IVec3::new(-outer, 0, z),
            IVec3::new(-inner, 0, z),
            block,
        );
        push_box(
            &mut voxels,
            IVec3::new(inner, 0, z),
            IVec3::new(outer, 0, z),
            block,
        );
    }
    for &sx in &[-1, 1] {
        push_box(
            &mut voxels,
            IVec3::new(sx * 2, 0, -7),
            IVec3::new(sx * 3, 0, -4),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, 0, -6),
            IVec3::new(sx * 3, 0, -5),
            BlockType::ShipHullAlloy,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, 0, 2),
            IVec3::new(sx * 4, 1, 7),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, 2, 3),
            IVec3::new(sx * 4, 2, 5),
            BlockType::ShipHullAlloy,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, 0, 8),
            IVec3::new(sx * 4, 1, 8),
            BlockType::EngineCore,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 6, 0, 3),
            IVec3::new(sx * 7, 0, 6),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 7, 0, 6),
            IVec3::new(sx * 7, 0, 7),
            BlockType::NeonAmber,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 2, -2, -2),
            IVec3::new(sx * 2, -1, -1),
            BlockType::ShipHullDark,
        );
        push_box(
            &mut voxels,
            IVec3::new(sx * 3, -2, 3),
            IVec3::new(sx * 4, -2, 4),
            BlockType::ShipHullAlloy,
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

    // -- Star Wars TIE Interceptor inspired Strike Fighter --

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
            IVec3::new(sx, -5, -1),
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
            IVec3::new(sx, -5, -5),
            IVec3::new(sx, -5, -3),
            BlockType::NeonCyan,
        );
    }

    ShipBlueprint {
        voxels,
        cockpit_offset: Vec3::new(0.0, 0.0, -3.5),
        exit_offset: Vec3::new(0.0, -3.5, 0.0),
        hardpoints: [Vec3::new(-7.0, 5.0, -5.0), Vec3::new(7.0, -5.0, -5.0)],
        hull_radius: 8.0,
        max_speed: 105.0,
        accel: 58.0,
        shield: 115.0,
    }
}

fn dropship_blueprint() -> ShipBlueprint {
    let mut voxels = Vec::new();

    // -- Star Wars LAAT (Republic Gunship) inspired Dropship --

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

#[allow(clippy::too_many_arguments)]
fn spawn_ship_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
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
                "ShipPlacementPreview"
            } else {
                "VoxelShuttle"
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
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.96, 0.96, 0.96)))
        .clone();
    commands.entity(root).with_children(|p| {
        for voxel in &bp.voxels {
            let mat = material_for_block(fx, materials, images, voxel.block, preview);
            p.spawn(PbrBundle {
                mesh: cube.clone(),
                material: mat,
                transform: Transform::from_translation(voxel.pos.as_vec3()),
                ..default()
            });
        }
        if !preview {
            spawn_cockpit_holograms(p, materials, fx, &cube, &bp);
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

fn material_for_block(
    fx: &mut ShipFxCache,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    block: BlockType,
    preview: bool,
) -> Handle<StandardMaterial> {
    let key = (Voxel::from(block), preview);
    if let Some(mat) = fx.mats.get(&key) {
        return mat.clone();
    }
    let rgba = voxel_color(Voxel::from(block));
    let alpha = if preview {
        0.34
    } else if block == BlockType::CockpitGlass {
        0.48
    } else {
        1.0
    };
    let base = Color::srgba(rgba[0].min(1.0), rgba[1].min(1.0), rgba[2].min(1.0), alpha);
    let texture = ship_texture_for_block(fx, images, block, preview);
    let mat = materials.add(StandardMaterial {
        base_color: if preview {
            base
        } else {
            Color::WHITE.with_alpha(alpha)
        },
        base_color_texture: Some(texture),
        emissive: if block == BlockType::EngineCore {
            LinearRgba::rgb(rgba[0] * 9.0, rgba[1] * 6.2, rgba[2] * 3.4)
        } else if voxel_is_emissive(Voxel::from(block)) {
            LinearRgba::rgb(rgba[0] * 5.4, rgba[1] * 5.4, rgba[2] * 5.4)
        } else if block == BlockType::CockpitGlass {
            LinearRgba::rgb(0.18, 0.75, 0.95)
        } else {
            LinearRgba::BLACK
        },
        alpha_mode: if preview || block == BlockType::CockpitGlass {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        metallic: if matches!(block, BlockType::ShipHullDark | BlockType::ShipHullAlloy) {
            0.85
        } else {
            0.1
        },
        perceptual_roughness: if block == BlockType::CockpitGlass {
            0.05
        } else if voxel_is_emissive(Voxel::from(block)) {
            0.16
        } else {
            0.34
        },
        reflectance: if block == BlockType::CockpitGlass {
            0.9
        } else if matches!(block, BlockType::ShipHullDark | BlockType::ShipHullAlloy) {
            0.62
        } else {
            0.35
        },
        ..default()
    });
    fx.mats.insert(key, mat.clone());
    mat
}

fn ship_texture_for_block(
    fx: &mut ShipFxCache,
    images: &mut Assets<Image>,
    block: BlockType,
    preview: bool,
) -> Handle<Image> {
    let key = (Voxel::from(block), preview);
    if let Some(texture) = fx.textures.get(&key) {
        return texture.clone();
    }
    let texture = images.add(ship_texture_image(block, preview));
    fx.textures.insert(key, texture.clone());
    texture
}

fn ship_texture_image(block: BlockType, preview: bool) -> Image {
    let size = 64u32;
    let base = voxel_color(Voxel::from(block));
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;
            let panel = ((x / 16) + (y / 16)) % 2;
            let seam = x % 16 == 0 || y % 16 == 0 || x % 16 == 15 || y % 16 == 15;
            let diag = ((x as i32 - y as i32).rem_euclid(13)) == 0;
            let hash = (((x * 37 + y * 91 + Voxel::from(block) as u32 * 17) & 31) as f32) / 31.0;

            let (mut r, mut g, mut b, mut a): (f32, f32, f32, f32) =
                (base[0], base[1], base[2], 1.0);
            match block {
                BlockType::ShipHullDark => {
                    let shade = 0.58 + hash * 0.20 + panel as f32 * 0.06;
                    r = (0.035 + shade * 0.035).min(0.11);
                    g = (0.045 + shade * 0.040).min(0.12);
                    b = (0.070 + shade * 0.075).min(0.20);
                    if seam {
                        r += 0.05;
                        g += 0.14;
                        b += 0.18;
                    }
                }
                BlockType::ShipHullAlloy => {
                    let brushed = (u * 22.0).sin() * 0.035 + (v * 51.0 + hash).sin() * 0.025;
                    r = (0.34 + brushed + panel as f32 * 0.035).clamp(0.0, 1.0);
                    g = (0.42 + brushed + panel as f32 * 0.040).clamp(0.0, 1.0);
                    b = (0.49 + brushed + panel as f32 * 0.045).clamp(0.0, 1.0);
                    if seam || diag {
                        r += 0.08;
                        g += 0.12;
                        b += 0.15;
                    }
                }
                BlockType::CockpitGlass => {
                    let glare = if diag || (x + y) % 29 == 0 { 0.38 } else { 0.0 };
                    let grid = if x % 12 == 0 || y % 12 == 0 {
                        0.20
                    } else {
                        0.0
                    };
                    r = 0.03 + glare * 0.16;
                    g = 0.28 + grid + glare * 0.48;
                    b = 0.38 + grid + glare * 0.58;
                    a = if preview { 0.34 } else { 0.62 };
                }
                BlockType::NeonCyan
                | BlockType::NeonMagenta
                | BlockType::NeonAmber
                | BlockType::EngineCore
                | BlockType::LuminiteCrystal
                | BlockType::MagnetiteOre
                | BlockType::IridiumVein => {
                    let stripe = if x % 10 < 3 || y % 18 < 2 { 0.45 } else { 0.0 };
                    let core = (1.0 - ((u - 0.5).abs().max((v - 0.5).abs()) * 2.0)).clamp(0.0, 1.0);
                    let boost = 0.65 + stripe + core * 0.42 + hash * 0.12;
                    r = (r * boost).min(1.0);
                    g = (g * boost).min(1.0);
                    b = (b * boost).min(1.0);
                }
                _ => {
                    let shade = 0.84 + hash * 0.18 + panel as f32 * 0.04;
                    r = (r * shade).min(1.0);
                    g = (g * shade).min(1.0);
                    b = (b * shade).min(1.0);
                }
            }

            if preview {
                let grid = if x % 8 == 0 || y % 8 == 0 { 0.28 } else { 0.0 };
                r = (r + grid * 0.20).min(1.0);
                g = (g + grid * 0.72).min(1.0);
                b = (b + grid * 0.95).min(1.0);
                a = a.min(0.48);
            }

            data.push((r.clamp(0.0, 1.0) * 255.0).round() as u8);
            data.push((g.clamp(0.0, 1.0) * 255.0).round() as u8);
            data.push((b.clamp(0.0, 1.0) * 255.0).round() as u8);
            data.push((a.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        ..ImageSamplerDescriptor::linear()
    });
    image
}

fn spawn_cockpit_holograms(
    parent: &mut ChildBuilder,
    materials: &mut Assets<StandardMaterial>,
    fx: &mut ShipFxCache,
    cube: &Handle<Mesh>,
    bp: &ShipBlueprint,
) {
    let cyan = cockpit_material(
        fx,
        materials,
        1,
        Color::srgba(0.04, 0.95, 1.0, 0.52),
        LinearRgba::rgb(0.25, 4.5, 5.5),
    );
    let magenta = cockpit_material(
        fx,
        materials,
        2,
        Color::srgba(1.0, 0.10, 0.78, 0.46),
        LinearRgba::rgb(4.5, 0.25, 3.2),
    );
    let amber = cockpit_material(
        fx,
        materials,
        3,
        Color::srgba(1.0, 0.48, 0.08, 0.72),
        LinearRgba::rgb(4.8, 1.7, 0.18),
    );
    let dark_glass = cockpit_material(
        fx,
        materials,
        4,
        Color::srgba(0.0, 0.04, 0.07, 0.58),
        LinearRgba::rgb(0.0, 0.18, 0.25),
    );

    let c = bp.cockpit_offset;
    spawn_panel(
        parent,
        cube,
        dark_glass.clone(),
        c + Vec3::new(0.0, -0.82, -1.10),
        Vec3::new(3.1, 0.08, 1.35),
        Quat::from_rotation_x(-0.46),
    );
    spawn_panel(
        parent,
        cube,
        cyan.clone(),
        c + Vec3::new(0.0, -0.75, -1.18),
        Vec3::new(2.55, 0.035, 0.78),
        Quat::from_rotation_x(-0.46),
    );
    spawn_panel(
        parent,
        cube,
        dark_glass,
        c + Vec3::new(-2.15, -0.88, -0.55),
        Vec3::new(1.10, 0.08, 1.05),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(0.10),
    );
    spawn_panel(
        parent,
        cube,
        cyan.clone(),
        c + Vec3::new(-2.15, -0.80, -0.62),
        Vec3::new(0.74, 0.04, 0.68),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(0.10),
    );
    spawn_panel(
        parent,
        cube,
        magenta.clone(),
        c + Vec3::new(2.15, -0.84, -0.60),
        Vec3::new(0.78, 0.04, 0.78),
        Quat::from_rotation_x(-0.35) * Quat::from_rotation_z(-0.10),
    );

    for i in 0..6 {
        let x = -1.25 + i as f32 * 0.50;
        let mat = if i % 3 == 0 {
            amber.clone()
        } else if i % 2 == 0 {
            magenta.clone()
        } else {
            cyan.clone()
        };
        spawn_panel(
            parent,
            cube,
            mat,
            c + Vec3::new(x, -0.62, -0.56),
            Vec3::new(0.22, 0.065, 0.16),
            Quat::from_rotation_x(-0.36),
        );
    }

    for side in [-1.0, 1.0] {
        spawn_panel(
            parent,
            cube,
            amber.clone(),
            c + Vec3::new(side * 3.0, -0.72, 0.18),
            Vec3::new(0.30, 0.08, 1.65),
            Quat::from_rotation_z(side * 0.14),
        );
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
) -> Handle<StandardMaterial> {
    if let Some(mat) = fx.cockpit_mats.get(&key) {
        return mat.clone();
    }
    let mat = materials.add(StandardMaterial {
        base_color,
        emissive,
        alpha_mode: AlphaMode::Add,
        unlit: true,
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
    existing: Query<Entity, With<ShipInstance>>,
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
        let px = player_anchor.x.round() as i32 + 14;
        let pz = player_anchor.z.round() as i32 + 18;
        let py = generator.surface_height_at(px, pz) as f32 + 4.0;
        spawn_ship_entity(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut fx,
            ShipKind::ScoutShuttle,
            Vec3::new(px as f32 + 0.5, py, pz as f32 + 0.5),
            player_yaw + std::f32::consts::PI,
            false,
            None,
        );
    }
}

fn resolved_world_entry_anchor(
    active: &ActiveWorld,
    settings: &WorldSettings,
    generator: &crate::terrain::TerrainGenerator,
) -> (Vec3, f32) {
    let pos = active.meta.player_pos;
    let mut anchor = Vec3::new(pos[0], pos[1], pos[2]);
    let mut yaw = active.meta.player_yaw;
    if settings.visual_preset == crate::settings::VisualPreset::NeonShuttle {
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
) {
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
    let inv_dt = if dt > 1e-4 { 1.0 / dt } else { 60.0 };
    // Sensitivity scaled so a normal flick produces ~0.9 rad/s, similar to
    // before but properly integrated against dt.
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
    let target_yaw_rate = (-mouse_dx * 0.00085) * inv_dt.min(120.0) + turn_input * 0.95;
    let target_pitch_rate = (-mouse_dy * 0.00065) * inv_dt.min(120.0) + pitch_input * 0.78;
    let look_ease = 1.0 - (-dt * 10.0).exp();
    motion.yaw_rate += (target_yaw_rate - motion.yaw_rate) * look_ease;
    motion.pitch_rate += (target_pitch_rate - motion.pitch_rate) * look_ease;
    // Clamp angular velocities for a heavier, more cinematic feel.
    motion.yaw_rate = motion.yaw_rate.clamp(-1.8, 1.8);
    motion.pitch_rate = motion.pitch_rate.clamp(-1.1, 1.1);

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if wheel_delta.abs() > 0.1 {
        pilot.weapon = pilot.weapon.next(if wheel_delta > 0.0 { -1 } else { 1 });
    }

    let bp = blueprint(ship.kind);

    // --- Roll-coordinated turn: A/D visibly banks the shuttle, but the
    // keyboard rudder above is the primary authority. A is left, D is right.
    let roll_input = turn_input;
    let target_roll = roll_input * 0.55;
    let roll_ease = 1.0 - (-dt * 3.2).exp();
    motion.roll += (target_roll - motion.roll) * roll_ease;
    // Bank-driven turn now follows the same sign as keyboard yaw, so holding
    // A no longer slips/turns right while D no longer slips/turns left.
    let bank_yaw_rate = motion.roll * 0.35;
    let rudder = turn_input * 0.35;
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

    ship_tf.rotation = Quat::from_rotation_y(motion.yaw)
        * Quat::from_rotation_x(motion.pitch)
        * Quat::from_rotation_z(motion.roll);
    let forward = *ship_tf.forward();
    let right = *ship_tf.right();
    // Keep only a whisper of bank drift; A/D should feel like turning the ship,
    // not sliding sideways across the terrain.
    let target_lateral = -motion.roll * motion.speed * 0.025;
    motion.lateral_speed += (target_lateral - motion.lateral_speed) * (1.0 - (-dt * 4.5).exp());
    let lift_speed = pitch_input * bp.max_speed * 0.34;
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
    let cyan = egui::Color32::from_rgb(0, 235, 255);
    let magenta = egui::Color32::from_rgb(255, 40, 220);
    let amber = egui::Color32::from_rgb(255, 160, 35);
    let glass = egui::Color32::from_rgba_unmultiplied(0, 20, 34, 95);
    let dark = egui::Color32::from_rgba_unmultiplied(0, 4, 10, 190);
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
    painter.rect_filled(left, egui::Rounding::ZERO, dark);
    painter.rect_filled(right, egui::Rounding::ZERO, dark);
    painter.rect_filled(
        bottom,
        egui::Rounding::same(7.0),
        egui::Color32::from_rgba_unmultiplied(0, 8, 18, 216),
    );
    draw_cockpit_dashboard(&painter, screen, cyan, magenta, amber, glass);

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

fn draw_cockpit_dashboard(
    painter: &egui::Painter,
    screen: egui::Rect,
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
    painter.rect_filled(main, egui::Rounding::same(6.0), glass);
    painter.rect_stroke(
        main,
        egui::Rounding::same(6.0),
        egui::Stroke::new(1.0, cyan),
    );

    let map = egui::Rect::from_min_max(
        egui::pos2(center_x - 150.0, bottom - 114.0),
        egui::pos2(center_x + 150.0, bottom - 36.0),
    );
    painter.rect_filled(
        map,
        egui::Rounding::same(4.0),
        egui::Color32::from_rgba_unmultiplied(0, 24, 42, 160),
    );
    painter.rect_stroke(map, egui::Rounding::same(4.0), egui::Stroke::new(1.0, cyan));
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
        painter.rect_filled(
            panel,
            egui::Rounding::same(5.0),
            egui::Color32::from_rgba_unmultiplied(0, 10, 18, 184),
        );
        painter.rect_stroke(
            panel,
            egui::Rounding::same(5.0),
            egui::Stroke::new(1.0, if side > 0.0 { magenta } else { cyan }),
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
}
