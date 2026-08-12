//! Shuttle battles: local voxel ships, cockpit flight, ship weapons and drones.
//!
//! Ships are moving entity hierarchies with smooth meshes for the visible
//! shuttle hulls. They do not mutate terrain chunks while flying, which keeps
//! chunk streaming and the mesher stable.

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::{MouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts, EguiSet};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use crate::blocks::BlockType;
use crate::director::{SimulationDirector, UnifiedTelemetry};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::neurocore::RuntimeProfile;
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
            .insert_resource(ShipInputCapture::default())
            .add_systems(OnEnter(GameState::MainMenu), cleanup_ship_runtime)
            .add_systems(OnEnter(GameState::InGame), spawn_saved_ships_once)
            .add_systems(
                PreUpdate,
                capture_ship_input
                    .after(EguiSet::BeginFrame)
                    .run_if(in_state(GameState::InGame)),
            )
            .add_systems(
                Update,
                (
                    ship_placement_input,
                    ship_interaction_input,
                    draw_ship_boarding_hud,
                    update_cockpit_transition,
                    ship_flight_input,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementPointerSource {
    CreatorLibrary,
    World,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShipPlacementPhase {
    #[default]
    Inactive,
    Ready,
    PointerHeld(PlacementPointerSource),
}

#[derive(Resource, Debug, Clone)]
pub struct ShipPlacementState {
    pub phase: ShipPlacementPhase,
    pub kind: ShipKind,
    pub yaw: f32,
    pub preview: Option<Entity>,
    pub target: Option<Vec3>,
    pub return_mode: ActiveMode,
    pub status: String,
    preview_kind: Option<ShipKind>,
    retired_previews: Vec<Entity>,
}

impl Default for ShipPlacementState {
    fn default() -> Self {
        Self {
            phase: ShipPlacementPhase::Inactive,
            kind: ShipKind::ScoutShuttle,
            yaw: 0.0,
            preview: None,
            target: None,
            return_mode: ActiveMode::Combat,
            status: "Hangar ready.".into(),
            preview_kind: None,
            retired_previews: Vec::new(),
        }
    }
}

impl ShipPlacementState {
    pub fn start_ready(&mut self, kind: ShipKind, return_mode: ActiveMode) {
        self.start_with_phase(kind, return_mode, ShipPlacementPhase::Ready);
    }

    pub fn start_drag(&mut self, kind: ShipKind, return_mode: ActiveMode) {
        self.start_with_phase(
            kind,
            return_mode,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::CreatorLibrary),
        );
    }

    pub fn is_active(&self) -> bool {
        self.phase != ShipPlacementPhase::Inactive
    }

    /// Compatibility entry point for the legacy inventory hangar.
    fn start_with_phase(
        &mut self,
        kind: ShipKind,
        return_mode: ActiveMode,
        phase: ShipPlacementPhase,
    ) {
        if self.kind != kind {
            self.retire_preview();
        }
        self.kind = kind;
        self.phase = phase;
        self.yaw = 0.0;
        self.target = None;
        self.return_mode = return_mode;
        self.status = format!("Placing {}.", kind.label());
    }

    fn retire_preview(&mut self) {
        if let Some(entity) = self.preview.take() {
            self.retired_previews.push(entity);
        }
        self.preview_kind = None;
    }

    fn sync_preview_kind(&mut self) {
        if self.preview.is_some() && self.preview_kind != Some(self.kind) {
            self.retire_preview();
        }
    }

    fn handle_pointer_edges(&mut self, just_pressed: bool, just_released: bool) -> bool {
        if just_pressed && self.phase == ShipPlacementPhase::Ready {
            self.phase = ShipPlacementPhase::PointerHeld(PlacementPointerSource::World);
        }

        if just_released && matches!(self.phase, ShipPlacementPhase::PointerHeld(_)) {
            self.phase = ShipPlacementPhase::Ready;
            return true;
        }

        false
    }

    fn deactivate(&mut self) -> ActiveMode {
        self.phase = ShipPlacementPhase::Inactive;
        self.target = None;
        self.return_mode
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
    /// Short, timer-based reticle bloom after firing. Kept in the HUD so
    /// weapon response remains visible without spawning extra scene FX.
    pub weapon_flash: f32,
    /// Hit-confirm timer driven by projectile impacts on enemy drones.
    pub hit_confirm: f32,
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
            weapon_flash: 0.0,
            hit_confirm: 0.0,
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

    fn hud_color(self) -> egui::Color32 {
        match self {
            ShipWeaponKind::IonRocket => egui::Color32::from_rgb(26, 242, 255),
            ShipWeaponKind::PlasmaFlak => egui::Color32::from_rgb(255, 51, 219),
            ShipWeaponKind::RailLance => egui::Color32::from_rgb(255, 166, 36),
        }
    }

    fn fx_tone(self) -> ShipFxTone {
        match self {
            ShipWeaponKind::IonRocket => ShipFxTone::Ion,
            ShipWeaponKind::PlasmaFlak => ShipFxTone::Plasma,
            ShipWeaponKind::RailLance => ShipFxTone::Rail,
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
    roll_rate: f32,
    /// Smoothed lateral velocity component (m/s) for inertial banking drift.
    lateral_speed: f32,
}

#[derive(Resource, Debug, Default, Clone, Copy)]
struct ShipInputCapture {
    pointer: bool,
    keyboard: bool,
    pointer_over_ui: bool,
}

#[derive(SystemParam)]
struct ShipPlacementFrame<'w, 's> {
    time: Res<'w, Time>,
    mouse: Res<'w, ButtonInput<MouseButton>>,
    keys: Res<'w, ButtonInput<KeyCode>>,
    input_capture: Res<'w, ShipInputCapture>,
    wheel: EventReader<'w, 's, MouseWheel>,
    world: Res<'w, VoxelWorld>,
    settings: Res<'w, WorldSettings>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    camera:
        Query<'w, 's, (&'static Camera, &'static GlobalTransform), (With<Camera3d>, With<Player>)>,
}

#[derive(SystemParam)]
struct ShipFlightFrame<'w> {
    time: Res<'w, Time>,
    input_capture: Res<'w, ShipInputCapture>,
}

impl ShipInputCapture {
    fn any(self) -> bool {
        self.pointer || self.keyboard
    }
}

fn capture_ship_input(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mode: Res<ModeContext>,
    ui_focus: Option<Res<crate::toolbelt::SketchEditorUiFocus>>,
    mut egui_contexts: Query<&mut bevy_egui::EguiContext, With<PrimaryWindow>>,
    mut capture: ResMut<ShipInputCapture>,
) {
    let (egui_pointer, egui_keyboard, egui_pointer_over_ui) = egui_contexts
        .get_single_mut()
        .map(|mut context| {
            let context = context.get_mut();
            (
                context.wants_pointer_input() || context.is_pointer_over_area(),
                context.wants_keyboard_input(),
                context.is_pointer_over_area(),
            )
        })
        .unwrap_or((false, false, false));
    let pointer_over_editor_ui = ui_focus
        .as_deref()
        .is_some_and(|focus| focus.pointer_over_editor_ui);
    capture.pointer = egui_pointer || pointer_over_editor_ui;
    capture.keyboard = egui_keyboard;
    capture.pointer_over_ui = egui_pointer_over_ui || pointer_over_editor_ui;

    // E is a continuous roll axis in the cockpit, while the global menu uses
    // its pressed edge to open inventory. Egui has already seen the raw event.
    if mode.is_ship_flight() {
        keys.clear_just_pressed(KeyCode::KeyE);
    }
}

fn ship_controls_allowed(capture: ShipInputCapture) -> bool {
    !capture.any()
}

#[derive(Debug, Clone, Copy)]
struct ShipHandlingProfile {
    look_response: f32,
    roll_response: f32,
    bank_scale: f32,
    lateral_drift: f32,
    pitch_level_response: f32,
}

fn ship_handling_profile(kind: ShipKind) -> ShipHandlingProfile {
    match kind {
        ShipKind::ScoutShuttle => ShipHandlingProfile {
            look_response: 7.4,
            roll_response: 3.0,
            bank_scale: 1.05,
            lateral_drift: 0.030,
            pitch_level_response: 0.42,
        },
        ShipKind::StrikeFighter => ShipHandlingProfile {
            look_response: 8.2,
            roll_response: 3.5,
            bank_scale: 1.20,
            lateral_drift: 0.026,
            pitch_level_response: 0.34,
        },
        ShipKind::HeavyDropship => ShipHandlingProfile {
            look_response: 4.8,
            roll_response: 1.8,
            bank_scale: 0.78,
            lateral_drift: 0.018,
            pitch_level_response: 0.54,
        },
    }
}

const SHIP_MOUSE_DEADZONE_RAD_PER_S: f32 = 0.008;
const SHIP_MOUSE_YAW_SENS_RAD_PER_PIXEL: f32 = 0.00016;
const SHIP_MOUSE_PITCH_SENS_RAD_PER_PIXEL: f32 = 0.00048;
const SHIP_KEY_YAW_RATE_RAD_PER_S: f32 = 0.30;
const SHIP_KEY_PITCH_RATE_RAD_PER_S: f32 = 0.56;
const SHIP_YAW_RATE_LIMIT_RAD_PER_S: f32 = 0.95;
const SHIP_PITCH_RATE_LIMIT_RAD_PER_S: f32 = 0.80;
const SHIP_ROLL_RATE_LIMIT_RAD_PER_S: f32 = 1.10;
const SHIP_YAW_ACCEL_RAD_PER_S2: f32 = 3.8;
const SHIP_PITCH_ACCEL_RAD_PER_S2: f32 = 3.2;
const SHIP_ROLL_ACCEL_RAD_PER_S2: f32 = 3.6;
const SHIP_ROLL_LEVEL_GAIN_PER_S: f32 = 2.8;
const SHIP_ROLL_LEVEL_DEADZONE_RAD: f32 = 0.002;
const SHIP_BANK_YAW_GAIN_PER_S: f32 = 0.10;
const SHIP_RUDDER_YAW_RATE_RAD_PER_S: f32 = 0.06;

fn apply_ship_mouse_deadzone(value: Vec2) -> Vec2 {
    let magnitude = value.length();
    if magnitude <= SHIP_MOUSE_DEADZONE_RAD_PER_S || magnitude <= f32::EPSILON {
        Vec2::ZERO
    } else {
        value * ((magnitude - SHIP_MOUSE_DEADZONE_RAD_PER_S) / magnitude)
    }
}

fn ship_target_angular_rates(
    mouse_dx: f32,
    mouse_dy: f32,
    turn_input: f32,
    pitch_input: f32,
    dt: f32,
) -> (f32, f32) {
    let mouse_rates = if dt.is_finite() && dt > f32::EPSILON {
        apply_ship_mouse_deadzone(Vec2::new(
            -mouse_dx * SHIP_MOUSE_YAW_SENS_RAD_PER_PIXEL / dt,
            -mouse_dy * SHIP_MOUSE_PITCH_SENS_RAD_PER_PIXEL / dt,
        ))
    } else {
        Vec2::ZERO
    };
    let yaw = mouse_rates.x + turn_input * SHIP_KEY_YAW_RATE_RAD_PER_S;
    let pitch = mouse_rates.y + pitch_input * SHIP_KEY_PITCH_RATE_RAD_PER_S;
    (
        yaw.clamp(
            -SHIP_YAW_RATE_LIMIT_RAD_PER_S,
            SHIP_YAW_RATE_LIMIT_RAD_PER_S,
        ),
        pitch.clamp(
            -SHIP_PITCH_RATE_LIMIT_RAD_PER_S,
            SHIP_PITCH_RATE_LIMIT_RAD_PER_S,
        ),
    )
}

fn ship_bank_and_rudder_yaw_rate(turn_input: f32, roll: f32) -> f32 {
    roll * SHIP_BANK_YAW_GAIN_PER_S + turn_input * SHIP_RUDDER_YAW_RATE_RAD_PER_S
}

fn integrate_ship_angular_rate(
    current_rad_per_s: f32,
    target_rad_per_s: f32,
    max_accel_rad_per_s2: f32,
    dt_seconds: f32,
) -> (f32, f32) {
    if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
        return (current_rad_per_s, 0.0);
    }
    let acceleration = max_accel_rad_per_s2.max(0.0);
    let difference = target_rad_per_s - current_rad_per_s;
    if difference.abs() <= f32::EPSILON {
        return (target_rad_per_s, target_rad_per_s * dt_seconds);
    }
    if acceleration <= f32::EPSILON {
        return (current_rad_per_s, current_rad_per_s * dt_seconds);
    }

    let signed_acceleration = difference.signum() * acceleration;
    let time_to_target = (difference.abs() / acceleration).min(dt_seconds);
    let reached_rate = current_rad_per_s + signed_acceleration * time_to_target;
    let accelerated_angle = current_rad_per_s * time_to_target
        + 0.5 * signed_acceleration * time_to_target * time_to_target;
    let remaining_time = dt_seconds - time_to_target;
    let angle_delta = accelerated_angle + target_rad_per_s * remaining_time;
    let next_rate = if remaining_time > 0.0 {
        target_rad_per_s
    } else {
        reached_rate
    };
    (next_rate, angle_delta)
}

fn wrap_ship_angle(angle: f32) -> f32 {
    (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

fn ship_roll_input(left: bool, right: bool) -> f32 {
    match (left, right) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

fn step_ship_attitude(
    motion: &mut ShipMotion,
    mouse_delta_pixels: Vec2,
    turn_input: f32,
    pitch_input: f32,
    roll_input: f32,
    dt_seconds: f32,
    handling: ShipHandlingProfile,
) {
    if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
        return;
    }

    let (mouse_and_key_yaw, target_pitch_rate) = ship_target_angular_rates(
        mouse_delta_pixels.x,
        mouse_delta_pixels.y,
        turn_input,
        pitch_input,
        dt_seconds,
    );
    let target_yaw_rate = (mouse_and_key_yaw
        + ship_bank_and_rudder_yaw_rate(turn_input, motion.roll) * handling.bank_scale)
        .clamp(
            -SHIP_YAW_RATE_LIMIT_RAD_PER_S,
            SHIP_YAW_RATE_LIMIT_RAD_PER_S,
        );
    let target_roll_rate = if roll_input.abs() > f32::EPSILON {
        roll_input.clamp(-1.0, 1.0) * SHIP_ROLL_RATE_LIMIT_RAD_PER_S
    } else if motion.roll.abs() > SHIP_ROLL_LEVEL_DEADZONE_RAD {
        (-wrap_ship_angle(motion.roll) * SHIP_ROLL_LEVEL_GAIN_PER_S).clamp(
            -SHIP_ROLL_RATE_LIMIT_RAD_PER_S,
            SHIP_ROLL_RATE_LIMIT_RAD_PER_S,
        )
    } else {
        0.0
    };

    let look_accel_scale = (handling.look_response / 7.4).clamp(0.5, 1.5);
    let roll_accel_scale = (handling.roll_response / 3.0).clamp(0.5, 1.5);
    let yaw_accel = SHIP_YAW_ACCEL_RAD_PER_S2 * look_accel_scale;
    let pitch_accel = SHIP_PITCH_ACCEL_RAD_PER_S2 * look_accel_scale;
    let roll_accel = SHIP_ROLL_ACCEL_RAD_PER_S2 * roll_accel_scale;
    let (yaw_rate, yaw_delta) =
        integrate_ship_angular_rate(motion.yaw_rate, target_yaw_rate, yaw_accel, dt_seconds);
    let (pitch_rate, pitch_delta) = integrate_ship_angular_rate(
        motion.pitch_rate,
        target_pitch_rate,
        pitch_accel,
        dt_seconds,
    );
    let (roll_rate, roll_delta) =
        integrate_ship_angular_rate(motion.roll_rate, target_roll_rate, roll_accel, dt_seconds);
    motion.yaw_rate = yaw_rate;
    motion.pitch_rate = pitch_rate;
    motion.roll_rate = roll_rate;
    motion.yaw = wrap_ship_angle(motion.yaw + yaw_delta);

    let unclamped_pitch = motion.pitch + pitch_delta;
    motion.pitch = unclamped_pitch.clamp(-0.85, 0.72);
    if motion.pitch != unclamped_pitch && motion.pitch_rate.signum() == pitch_delta.signum() {
        motion.pitch_rate = 0.0;
    }
    if target_pitch_rate.abs() <= f32::EPSILON {
        motion.pitch *= (-dt_seconds * handling.pitch_level_response).exp();
    }

    motion.roll = wrap_ship_angle(motion.roll + roll_delta);
    if roll_input.abs() <= f32::EPSILON
        && motion.roll.abs() <= SHIP_ROLL_LEVEL_DEADZONE_RAD
        && motion.roll_rate.abs() <= roll_accel * dt_seconds
    {
        motion.roll = 0.0;
        motion.roll_rate = 0.0;
    }
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
    tone: ShipFxTone,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ShipFxTone {
    Pulse,
    Ion,
    Plasma,
    Rail,
    Hostile,
}

impl ShipFxTone {
    fn color(self) -> Color {
        match self {
            Self::Pulse => Color::srgb(0.08, 0.96, 1.0),
            Self::Ion => Color::srgb(0.10, 0.95, 1.00),
            Self::Plasma => Color::srgb(1.00, 0.20, 0.86),
            Self::Rail => Color::srgb(1.00, 0.65, 0.14),
            Self::Hostile => Color::srgb(1.0, 0.08, 0.55),
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShipVisualDetail {
    Core,
    Full,
}

impl ShipVisualDetail {
    fn for_profile(profile: RuntimeProfile) -> Self {
        if profile == RuntimeProfile::LowSpec {
            Self::Core
        } else {
            Self::Full
        }
    }

    fn includes_decorative_parts(self) -> bool {
        self == Self::Full
    }

    fn includes_cockpit_panel(self, index: usize, tone: CockpitPanelTone) -> bool {
        if self == Self::Full {
            return true;
        }
        match tone {
            CockpitPanelTone::Shell
            | CockpitPanelTone::Seat
            | CockpitPanelTone::Frame
            | CockpitPanelTone::Glass => true,
            CockpitPanelTone::Cyan | CockpitPanelTone::Magenta | CockpitPanelTone::Amber => {
                index % 2 == 0
            }
        }
    }

    fn includes_energy_trail(self, index: usize, tone: ShipTrailTone) -> bool {
        self == Self::Full || tone == ShipTrailTone::Amber || index < 2
    }

    fn dynamic_light_budget(self) -> usize {
        match self {
            Self::Core => 0,
            Self::Full => 2,
        }
    }
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
    projectile_mats: std::collections::HashMap<ShipFxTone, Handle<StandardMaterial>>,
    explosion_mats: std::collections::HashMap<ShipFxTone, Handle<StandardMaterial>>,
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
    add_scout_realism(&mut voxels);
    add_future_wave_shuttle_skin(&mut voxels, ShipKind::ScoutShuttle);
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

fn add_scout_realism(voxels: &mut Vec<ShipVoxel>) {
    // Opaque canopy ribs, pilot shell, landing gear and panel breaks.
    for sx in [-1, 1] {
        push_box(
            voxels,
            IVec3::new(sx * 2, 1, -6),
            IVec3::new(sx * 2, 3, -3),
            BlockType::ShipHullDark,
        );
        push_box(
            voxels,
            IVec3::new(sx * 2, 0, -9),
            IVec3::new(sx * 2, 1, -7),
            BlockType::ShipHullAlloy,
        );
        push_box(
            voxels,
            IVec3::new(sx * 5, 1, -1),
            IVec3::new(sx * 8, 1, -1),
            BlockType::NeonCyan,
        );
        push_box(
            voxels,
            IVec3::new(sx * 8, 0, -4),
            IVec3::new(sx * 8, 0, -3),
            BlockType::NeonAmber,
        );
        push_box(
            voxels,
            IVec3::new(sx * 3, -3, 2),
            IVec3::new(sx * 4, -3, 5),
            BlockType::ShipHullDark,
        );
    }
    push_box(
        voxels,
        IVec3::new(-1, 3, -6),
        IVec3::new(1, 3, -5),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-1, 3, -3),
        IVec3::new(1, 3, -2),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(0, 1, -5),
        IVec3::new(0, 1, -4),
        BlockType::ShipHullDark,
    );
    push_box(
        voxels,
        IVec3::new(-1, 1, -6),
        IVec3::new(1, 1, -6),
        BlockType::NeonAmber,
    );
    push_box(
        voxels,
        IVec3::new(-1, 0, -10),
        IVec3::new(1, 0, -9),
        BlockType::ShipHullAlloy,
    );
    push_box(
        voxels,
        IVec3::new(0, 1, -10),
        IVec3::new(0, 1, -10),
        BlockType::NeonCyan,
    );
    push_box(
        voxels,
        IVec3::new(-2, 2, 6),
        IVec3::new(2, 2, 7),
        BlockType::EngineCore,
    );
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
            push_box(
                voxels,
                IVec3::new(-1, -1, -6),
                IVec3::new(1, -1, 5),
                BlockType::ShipHullDark,
            );
            for sx in [-1, 1] {
                push_box(
                    voxels,
                    IVec3::new(sx * 4, -1, 0),
                    IVec3::new(sx * 7, -1, 5),
                    BlockType::ShipHullAlloy,
                );
            }
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

fn realistic_ship_exterior_specs(
    kind: ShipKind,
    detail: ShipVisualDetail,
) -> Vec<RealShipPartSpec> {
    let mut parts = Vec::with_capacity(36);
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
            RealShipTone::CarbonBlack,
            Vec3::new(sx * 1.34, 1.38, -4.92),
            Vec3::new(0.16, 0.18, 2.08),
            Quat::from_rotation_y(-sx * 0.04),
        );
    }
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CarbonBlack,
        Vec3::new(0.0, 1.78, -4.94),
        Vec3::new(0.13, 0.12, 2.04),
        Quat::from_rotation_x(-0.05),
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CarbonBlack,
        Vec3::new(0.0, 1.50, -3.36),
        Vec3::new(2.70, 0.16, 0.18),
        Quat::IDENTITY,
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

    if detail.includes_decorative_parts() {
        for sx in [-1.0, 1.0] {
            push_real_part(
                &mut parts,
                RealShipMeshKind::AeroPlate,
                RealShipTone::CarbonBlack,
                Vec3::new(sx * 2.42, 0.94, -0.55),
                Vec3::new(0.34, 0.10, 4.15),
                Quat::from_rotation_y(-sx * 0.08),
            );
            push_real_part(
                &mut parts,
                RealShipMeshKind::AeroPlate,
                RealShipTone::CeramicWhite,
                Vec3::new(sx * 4.45, 0.62, 1.20),
                Vec3::new(2.65, 0.14, 0.64),
                Quat::from_rotation_y(-sx * 0.12),
            );
            push_real_part(
                &mut parts,
                RealShipMeshKind::AeroPlate,
                RealShipTone::AmberHeat,
                Vec3::new(sx * 2.12, 0.18, 4.78),
                Vec3::new(0.52, 0.08, 1.22),
                Quat::IDENTITY,
            );
        }
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::CarbonBlack,
            Vec3::new(0.0, 0.78, 5.56),
            Vec3::new(3.72, 0.20, 0.68),
            Quat::IDENTITY,
        );
    }

    match kind {
        ShipKind::ScoutShuttle => {
            push_real_part(
                &mut parts,
                RealShipMeshKind::AeroPlate,
                RealShipTone::CeramicWhite,
                Vec3::new(0.0, -0.24, 1.70),
                Vec3::new(1.35, 0.10, 3.4),
                Quat::IDENTITY,
            );
        }
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

fn realistic_cockpit_part_specs(
    kind: ShipKind,
    bp: &ShipBlueprint,
    detail: ShipVisualDetail,
) -> Vec<RealShipPartSpec> {
    let mut parts = Vec::with_capacity(20);
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
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::ConsoleBlack,
        Vec3::new(0.0, -1.48, 0.02),
        Vec3::new(3.02, 0.12, 2.82),
        Quat::IDENTITY,
    );
    push_real_part(
        &mut parts,
        RealShipMeshKind::AeroPlate,
        RealShipTone::CarbonBlack,
        Vec3::new(0.0, -0.16, 1.52),
        Vec3::new(2.72, 2.34, 0.14),
        Quat::from_rotation_x(0.06),
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
    if detail.includes_decorative_parts() {
        for sx in [-1.0, 1.0] {
            push_real_part(
                &mut parts,
                RealShipMeshKind::AeroPlate,
                RealShipTone::CarbonBlack,
                Vec3::new(sx * 1.54, 0.12, -0.18),
                Vec3::new(0.14, 1.62, 2.30),
                Quat::from_rotation_z(sx * 0.08),
            );
        }
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::CarbonBlack,
            Vec3::new(0.0, 1.13, -0.58),
            Vec3::new(3.02, 0.14, 0.18),
            Quat::IDENTITY,
        );
        push_real_part(
            &mut parts,
            RealShipMeshKind::AeroPlate,
            RealShipTone::AmberHeat,
            Vec3::new(0.0, 1.03, -0.72),
            Vec3::new(1.18, 0.04, 0.06),
            Quat::IDENTITY,
        );
    }
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
    detail: ShipVisualDetail,
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
                roll_rate: 0.0,
                lateral_speed: 0.0,
            },
        ));
    }

    let cube = fx
        .cube
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    commands.entity(root).with_children(|p| {
        spawn_realistic_ship_exterior(p, meshes, materials, fx, kind, preview, detail);
        if !preview {
            spawn_cockpit_holograms(p, meshes, materials, fx, &cube, kind, &bp, detail);
            spawn_ship_energy_trails(p, materials, fx, &cube, kind, detail);
            if detail.dynamic_light_budget() >= 1 {
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
            }
            if detail.dynamic_light_budget() >= 2 {
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
    detail: ShipVisualDetail,
) {
    for part in realistic_ship_exterior_specs(kind, detail) {
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
    detail: ShipVisualDetail,
) {
    for part in realistic_cockpit_part_specs(kind, bp, detail) {
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

fn real_ship_surface_alpha(preview: bool, preview_alpha: f32) -> f32 {
    if preview {
        preview_alpha
    } else {
        1.0
    }
}

fn real_ship_material_spec(tone: RealShipTone, preview: bool) -> StandardMaterial {
    let alpha_mode = if preview {
        AlphaMode::Blend
    } else {
        AlphaMode::Opaque
    };
    let (base_color, emissive, metallic, roughness, reflectance) = match tone {
        RealShipTone::CeramicWhite => (
            Color::srgba(0.72, 0.78, 0.82, real_ship_surface_alpha(preview, 0.38)),
            LinearRgba::rgb(0.035, 0.045, 0.055),
            0.45,
            0.32,
            0.55,
        ),
        RealShipTone::CarbonBlack => (
            Color::srgba(0.015, 0.022, 0.028, real_ship_surface_alpha(preview, 0.38)),
            LinearRgba::rgb(0.004, 0.018, 0.022),
            0.58,
            0.30,
            0.52,
        ),
        RealShipTone::SmokedGlass => (
            Color::srgba(0.004, 0.025, 0.034, real_ship_surface_alpha(preview, 0.42)),
            LinearRgba::rgb(0.01, 0.12, 0.16),
            0.30,
            0.16,
            0.78,
        ),
        RealShipTone::CyanEmission => (
            Color::srgba(0.02, 0.86, 1.0, real_ship_surface_alpha(preview, 0.52)),
            LinearRgba::rgb(0.22, 7.8, 9.4),
            0.0,
            0.14,
            0.62,
        ),
        RealShipTone::AmberHeat => (
            Color::srgba(1.0, 0.36, 0.06, real_ship_surface_alpha(preview, 0.34)),
            LinearRgba::rgb(8.5, 2.2, 0.10),
            0.0,
            0.18,
            0.52,
        ),
        RealShipTone::LuminiteGlass => (
            Color::srgba(0.56, 1.0, 1.0, real_ship_surface_alpha(preview, 0.44)),
            LinearRgba::rgb(1.6, 6.8, 7.2),
            0.05,
            0.12,
            0.72,
        ),
        RealShipTone::MagentaSignal => (
            Color::srgba(1.0, 0.12, 0.82, real_ship_surface_alpha(preview, 0.46)),
            LinearRgba::rgb(6.4, 0.18, 4.8),
            0.0,
            0.14,
            0.62,
        ),
        RealShipTone::SeatLeather => (
            Color::srgba(0.018, 0.016, 0.014, real_ship_surface_alpha(preview, 0.38)),
            LinearRgba::BLACK,
            0.15,
            0.42,
            0.35,
        ),
        RealShipTone::ConsoleBlack => (
            Color::srgba(0.006, 0.012, 0.018, real_ship_surface_alpha(preview, 0.38)),
            LinearRgba::rgb(0.0, 0.05, 0.08),
            0.48,
            0.30,
            0.55,
        ),
    };
    StandardMaterial {
        base_color,
        emissive,
        alpha_mode,
        metallic,
        perceptual_roughness: roughness,
        reflectance,
        ..default()
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
    let mat = materials.add(real_ship_material_spec(tone, preview));
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
            specs.push(ShipTrailSpec {
                base_translation: Vec3::new(0.0, -0.20, 12.0),
                base_scale: Vec3::new(0.24, 0.16, 5.2),
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
    detail: ShipVisualDetail,
) {
    for (index, spec) in ship_trail_specs(kind).into_iter().enumerate() {
        if !detail.includes_energy_trail(index, spec.tone) {
            continue;
        }
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
    detail: ShipVisualDetail,
) {
    spawn_realistic_cockpit_parts(parent, meshes, materials, fx, kind, bp, detail);
    for (index, panel) in cockpit_panel_specs(kind, bp).into_iter().enumerate() {
        if !detail.includes_cockpit_panel(index, panel.tone) {
            continue;
        }
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
    let generator = crate::terrain::TerrainGenerator::new(active.meta.seed)
        .with_world_profile(settings.effective_world_profile());
    let (player_anchor, player_yaw) = resolved_world_entry_anchor(&active, &settings, &generator);
    let visual_detail = ShipVisualDetail::for_profile(settings.runtime_profile);

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
            visual_detail,
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
            visual_detail,
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
    if settings.effective_world_profile() == crate::settings::WorldProfile::Natural {
        let bx = crate::chunk::floor_to_i32_safe(anchor.x);
        let bz = crate::chunk::floor_to_i32_safe(anchor.z);
        let surface = generator.surface_height_at(bx, bz);
        if generator.biome_at(bx, bz).is_showcase_terrain() || anchor.y > surface as f32 + 90.0 {
            if let Some(spawn) = generator.find_natural_spawn(0, 0, 4096) {
                anchor = Vec3::new(spawn.x as f32 + 0.5, spawn.y as f32, spawn.z as f32 + 0.5);
                yaw = 0.0;
            }
        }
    } else if settings.effective_world_profile() == crate::settings::WorldProfile::AstralFrontier {
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
    mut frame: ShipPlacementFrame,
    mut placement: ResMut<ShipPlacementState>,
    mut mode: ResMut<ModeContext>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut fx: ResMut<ShipFxCache>,
    mut preview_q: Query<&mut Transform, With<ShipPreview>>,
) {
    if !placement.is_active() {
        let ActiveMode::ShipPlacement { kind } = mode.mode else {
            return;
        };
        placement.start_ready(kind, mode.last_mode);
    } else {
        let placement_mode = ActiveMode::ShipPlacement {
            kind: placement.kind,
        };
        if mode.mode == placement.return_mode {
            mode.set(
                placement_mode,
                format!("Placing {}.", placement.kind.label()),
            );
        } else if matches!(mode.mode, ActiveMode::ShipPlacement { .. })
            && mode.mode != placement_mode
        {
            mode.set(
                placement_mode,
                format!("Placing {}.", placement.kind.label()),
            );
        } else if mode.mode != placement_mode {
            return;
        }
    }

    placement.sync_preview_kind();
    for entity in std::mem::take(&mut placement.retired_previews) {
        despawn(&mut commands, entity);
    }

    let wheel_delta: f32 = frame.wheel.read().map(|ev| ev.y).sum();

    // Cancellation must remain available even while egui owns pointer or
    // keyboard focus. In particular, RMB over the Creator Library must not
    // leave placement armed behind the overlay.
    if frame.mouse.just_pressed(MouseButton::Right) || frame.keys.just_pressed(KeyCode::Escape) {
        remove_placement_preview(&mut commands, &mut placement);
        let return_mode = placement.deactivate();
        placement.status = "Ship placement cancelled.".into();
        mode.set(return_mode, "Ship placement cancelled.");
        return;
    }

    if !placement_controls_allowed(*frame.input_capture, placement.phase) {
        if frame.input_capture.pointer_over_ui {
            placement.target = None;
            remove_placement_preview(&mut commands, &mut placement);
        }
        if placement.handle_pointer_edges(false, frame.mouse.just_released(MouseButton::Left)) {
            placement.status = "Move the pointer back over the world to place the ship.".into();
        }
        return;
    }

    if wheel_delta.abs() > 0.1 {
        placement.yaw += wheel_delta.signum() * 15.0_f32.to_radians();
    }

    placement.target = frame
        .windows
        .get_single()
        .ok()
        .zip(frame.camera.get_single().ok())
        .and_then(|(window, (camera, camera_transform))| {
            placement_pointer_ray(window, camera, camera_transform)
        })
        .and_then(|(origin, dir)| placement_target(&frame.world, origin, dir));

    if let Some(pos) = placement.target {
        let kind = placement.kind;
        let preview = match placement.preview {
            Some(entity)
                if placement.preview_kind == Some(kind) && preview_q.get_mut(entity).is_ok() =>
            {
                entity
            }
            _ => {
                remove_placement_preview(&mut commands, &mut placement);
                let entity = spawn_ship_entity(
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut images,
                    &mut fx,
                    kind,
                    pos,
                    placement.yaw,
                    true,
                    ShipVisualDetail::Core,
                    None,
                );
                placement.preview = Some(entity);
                placement.preview_kind = Some(kind);
                entity
            }
        };
        if let Ok(mut transform) = preview_q.get_mut(preview) {
            let hover = (frame.time.elapsed_seconds_wrapped() * 4.0).sin() * 0.12;
            transform.translation = pos + Vec3::Y * hover;
            transform.rotation = Quat::from_rotation_y(placement.yaw);
        }
    } else {
        remove_placement_preview(&mut commands, &mut placement);
    }

    let commit_requested = placement.handle_pointer_edges(
        frame.mouse.just_pressed(MouseButton::Left),
        frame.mouse.just_released(MouseButton::Left),
    );
    if !commit_requested {
        return;
    }

    let Some(pos) = placement.target else {
        placement.status = "Ships require a visible top-facing terrain surface.".into();
        mode.set(
            ActiveMode::ShipPlacement {
                kind: placement.kind,
            },
            placement.status.clone(),
        );
        return;
    };

    remove_placement_preview(&mut commands, &mut placement);
    let kind = placement.kind;
    let ship = spawn_ship_entity(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut fx,
        kind,
        pos,
        placement.yaw,
        false,
        ShipVisualDetail::for_profile(frame.settings.runtime_profile),
        None,
    );
    let return_mode = placement.deactivate();
    placement.status = format!("{} placed.", kind.label());
    mode.set(
        return_mode,
        format!(
            "{} placed. Aim at cockpit and click to enter.",
            kind.label()
        ),
    );
    commands.entity(ship).insert(Name::new(kind.label()));
}

fn placement_controls_allowed(capture: ShipInputCapture, phase: ShipPlacementPhase) -> bool {
    !capture.any()
        || (matches!(
            phase,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::CreatorLibrary)
        ) && !capture.pointer_over_ui)
}

fn placement_pointer_ray(
    window: &Window,
    camera: &Camera,
    camera_transform: &GlobalTransform,
) -> Option<(Vec3, Vec3)> {
    if !window.cursor.visible {
        return None;
    }

    let cursor = window.cursor_position()?;
    let ray = camera.viewport_to_world(camera_transform, cursor)?;
    Some((ray.origin, *ray.direction))
}

fn remove_placement_preview(commands: &mut Commands, placement: &mut ShipPlacementState) {
    if let Some(entity) = placement.preview.take() {
        despawn(commands, entity);
    }
    placement.preview_kind = None;
}

fn placement_target(world: &VoxelWorld, origin: Vec3, dir: Vec3) -> Option<Vec3> {
    let (hit, prev) = crate::sculpt::raycast::dda_voxel(world, origin, dir, 180.0)?;
    top_face_placement_target(hit, prev)
}

fn top_face_placement_target(hit: IVec3, prev: IVec3) -> Option<Vec3> {
    if prev - hit != IVec3::Y {
        return None;
    }
    Some(prev.as_vec3() + Vec3::new(0.5, 1.1, 0.5))
}

fn ship_interaction_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    input_capture: Res<ShipInputCapture>,
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

    let controls_allowed = ship_controls_allowed(*input_capture);
    if pilot.active_ship.is_some() && (!controls_allowed || !keys.just_pressed(KeyCode::KeyX)) {
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
    let board = controls_allowed
        && (mouse.just_pressed(MouseButton::Left) || keys.just_pressed(KeyCode::KeyH));
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
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
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
    frame: ShipFlightFrame,
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
        mouse_motion.clear();
        wheel.clear();
        return;
    }
    let Ok((mut ship_tf, mut ship, mut motion)) = ship_q.get_mut(active) else {
        pilot.active_ship = None;
        return;
    };
    let Ok((mut player_tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let dt = frame.time.delta_seconds().min(1.0 / 20.0);
    pilot.shield_flash = (pilot.shield_flash - dt * 1.8).max(0.0);
    pilot.weapon_flash = (pilot.weapon_flash - dt).max(0.0);
    pilot.hit_confirm = (pilot.hit_confirm - dt).max(0.0);

    if pilot.shield <= 0.0 {
        let bp = blueprint(ship.kind);
        spawn_ship_explosion(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            ship_tf.translation,
            bp.hull_radius * 1.35,
            ShipFxTone::Pulse,
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

    let controls_allowed = ship_controls_allowed(*frame.input_capture);
    let raw_mouse_delta = mouse_motion
        .read()
        .fold(Vec2::ZERO, |sum, event| sum + event.delta);
    let mouse_delta = if controls_allowed {
        raw_mouse_delta
    } else {
        Vec2::ZERO
    };
    let wheel_delta: f32 = wheel.read().map(|event| event.y).sum();

    // Mouse and keyboard command target angular rates. The attitude step
    // applies bounded acceleration, so pressing or releasing an axis cannot
    // introduce a one-frame rotation jump.
    let key_turn_left =
        controls_allowed && (keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft));
    let key_turn_right =
        controls_allowed && (keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight));
    let turn_input = match (key_turn_left, key_turn_right) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    };
    let key_pitch_up =
        controls_allowed && (keys.pressed(KeyCode::Space) || keys.pressed(KeyCode::ArrowUp));
    let key_pitch_down = controls_allowed
        && (keys.pressed(KeyCode::ShiftLeft)
            || keys.pressed(KeyCode::ShiftRight)
            || keys.pressed(KeyCode::ArrowDown));
    let pitch_input = match (key_pitch_up, key_pitch_down) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    };
    let roll_input = if controls_allowed {
        ship_roll_input(keys.pressed(KeyCode::KeyQ), keys.pressed(KeyCode::KeyE))
    } else {
        0.0
    };
    let handling = ship_handling_profile(ship.kind);
    step_ship_attitude(
        &mut motion,
        mouse_delta,
        turn_input,
        pitch_input,
        roll_input,
        dt,
        handling,
    );

    if controls_allowed && wheel_delta.abs() > 0.1 {
        pilot.weapon = pilot.weapon.next(if wheel_delta > 0.0 { -1 } else { 1 });
    }

    let bp = blueprint(ship.kind);

    // --- Throttle with cruise inertia. ------------------------------------
    // W accelerates toward cruise, S brakes, and releasing both keeps most of
    // the current speed so long flights do not require holding W forever.
    let accelerating = controls_allowed && keys.pressed(KeyCode::KeyW);
    let braking = controls_allowed && keys.pressed(KeyCode::KeyS);
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
        frame.time.elapsed_seconds_wrapped(),
    );
    ship_tf.rotation = Quat::from_rotation_y(motion.yaw)
        * Quat::from_rotation_x(motion.pitch)
        * Quat::from_rotation_z(motion.roll)
        * Quat::from_rotation_x(wave.pitch)
        * Quat::from_rotation_z(wave.roll);
    let forward = *ship_tf.forward();
    let right = *ship_tf.right();
    // Keep only a whisper of bank drift; direct roll should rotate the ship,
    // not slide it sideways across the terrain.
    let target_lateral = -motion.roll * motion.speed * handling.lateral_drift;
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

    if controls_allowed && mouse.pressed(MouseButton::Left) && pilot.primary_cooldown <= 0.0 {
        fire_ship_pulse(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut fx,
            &ship_tf,
            &bp,
        );
        pilot.primary_cooldown = 0.12;
        pilot.weapon_flash = 0.10;
        telemetry.ship_shots = telemetry.ship_shots.saturating_add(1);
    }
    if controls_allowed && mouse.pressed(MouseButton::Right) && pilot.secondary_cooldown <= 0.0 {
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
        pilot.weapon_flash = 0.18;
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
            ShipFxTone::Pulse,
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
        weapon.fx_tone(),
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
    tone: ShipFxTone,
    profile: WeaponProfile,
) {
    let mesh = fx
        .projectile
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let mat = if let Some(mat) = fx.projectile_mats.get(&tone) {
        mat.clone()
    } else {
        let color = tone.color();
        let lin = color.to_linear();
        let mat = materials.add(StandardMaterial {
            base_color: color,
            emissive: LinearRgba::rgb(lin.red * 4.0, lin.green * 4.0, lin.blue * 4.0),
            alpha_mode: AlphaMode::Add,
            ..default()
        });
        fx.projectile_mats.insert(tone, mat.clone());
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
            tone,
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
                p.tone,
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
                            p.tone,
                        );
                        let destroyed = drone.hp <= 0.0;
                        pilot.hit_confirm = if destroyed { 0.32 } else { 0.18 };
                        pilot.status = if destroyed {
                            "Target destroyed.".into()
                        } else {
                            "Hit confirmed.".into()
                        };
                        if destroyed {
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
                                ShipFxTone::Hostile,
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
                ShipFxTone::Hostile,
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
    tone: ShipFxTone,
) {
    let mesh = fx
        .explosion
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let mat = if let Some(mat) = fx.explosion_mats.get(&tone) {
        mat.clone()
    } else {
        let color = tone.color();
        let lin = color.to_linear();
        let mat = materials.add(StandardMaterial {
            base_color: Color::srgba(lin.red, lin.green, lin.blue, 0.42),
            emissive: LinearRgba::rgb(
                lin.red * 5.0 + 0.4,
                lin.green * 5.0 + 0.4,
                lin.blue * 5.0 + 0.4,
            ),
            alpha_mode: AlphaMode::Add,
            unlit: true,
            ..default()
        });
        fx.explosion_mats.insert(tone, mat.clone());
        mat
    };
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
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ship_cockpit_hud"),
    ));
    let colors = settings.theme.semantic();
    let cyan = colors.info;
    let magenta = egui::Color32::from_rgb(255, 40, 220);
    let amber = colors.warning;
    let weapon_color = pilot.weapon.hud_color();
    let glass = egui::Color32::from_rgba_unmultiplied(10, 36, 48, 108);
    let target_budget = if settings.runtime_profile == RuntimeProfile::LowSpec {
        4
    } else {
        8
    };
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
    let shot_pulse = (pilot.weapon_flash / 0.18).clamp(0.0, 1.0);
    let reticle_color = if pilot.hit_confirm > 0.0 {
        egui::Color32::WHITE
    } else {
        weapon_color
    };
    painter.circle_stroke(
        center,
        22.0 + shot_pulse * 8.0,
        egui::Stroke::new(1.5 + shot_pulse * 0.8, reticle_color),
    );
    painter.line_segment(
        [
            center - egui::vec2(45.0, 0.0),
            center - egui::vec2(12.0, 0.0),
        ],
        egui::Stroke::new(1.0, reticle_color),
    );
    painter.line_segment(
        [
            center + egui::vec2(12.0, 0.0),
            center + egui::vec2(45.0, 0.0),
        ],
        egui::Stroke::new(1.0, reticle_color),
    );
    if pilot.hit_confirm > 0.0 {
        let hit_alpha = (pilot.hit_confirm / 0.32).clamp(0.0, 1.0);
        let hit_color =
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, (90.0 + hit_alpha * 165.0) as u8);
        for (a, b) in [
            (egui::vec2(-13.0, -13.0), egui::vec2(-5.0, -5.0)),
            (egui::vec2(13.0, -13.0), egui::vec2(5.0, -5.0)),
            (egui::vec2(-13.0, 13.0), egui::vec2(-5.0, 5.0)),
            (egui::vec2(13.0, 13.0), egui::vec2(5.0, 5.0)),
        ] {
            painter.line_segment([center + a, center + b], egui::Stroke::new(2.0, hit_color));
        }
    }
    if let Ok((camera, camera_tf)) = camera_q.get_single() {
        for drone_tf in drones.iter().take(target_budget) {
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
        weapon_color,
        16.0,
    );
    let secondary_ready = 1.0
        - (pilot.secondary_cooldown / pilot.weapon.profile().cooldown.max(0.01)).clamp(0.0, 1.0);
    let ready_bar = egui::Rect::from_min_size(
        egui::pos2(screen.right() - 150.0, screen.bottom() - 91.0),
        egui::vec2(112.0, 4.0),
    );
    painter.rect_filled(
        ready_bar,
        egui::Rounding::same(2.0),
        egui::Color32::from_gray(28),
    );
    painter.rect_filled(
        ready_bar.with_max_x(ready_bar.left() + ready_bar.width() * secondary_ready),
        egui::Rounding::same(2.0),
        weapon_color,
    );
    draw_hud_text(
        &painter,
        egui::pos2(screen.right() - 150.0, screen.bottom() - 55.0),
        &format!("DRONES\n{:02}", drones.iter().count()),
        amber,
        18.0,
    );
    if pilot.shield < 35.0 || pilot.shield_flash > 0.15 || pilot.hit_confirm > 0.0 {
        draw_hud_text(
            &painter,
            egui::pos2(screen.center().x - 84.0, screen.bottom() - 122.0),
            &pilot.status,
            if pilot.shield < 35.0 {
                amber
            } else if pilot.hit_confirm > 0.0 {
                weapon_color
            } else {
                magenta
            },
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
    for (i, _) in drones.iter().enumerate().take(target_budget) {
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
        let streak_budget = if settings.runtime_profile == RuntimeProfile::LowSpec {
            12
        } else {
            36
        };
        for i in 0..streak_budget {
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

    fn test_ship_motion() -> ShipMotion {
        ShipMotion {
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            speed: 0.0,
            yaw_rate: 0.0,
            pitch_rate: 0.0,
            roll_rate: 0.0,
            lateral_speed: 0.0,
        }
    }

    #[test]
    fn ship_placement_state_tracks_ready_and_creator_drag_phases() {
        let return_mode = ActiveMode::BuildLive {
            tool: crate::toolbelt::ToolbeltTool::DrawRect,
        };
        let mut placement = ShipPlacementState::default();
        assert_eq!(placement.phase, ShipPlacementPhase::Inactive);
        assert!(!placement.is_active());

        placement.start_ready(ShipKind::StrikeFighter, return_mode);
        assert_eq!(placement.phase, ShipPlacementPhase::Ready);
        assert_eq!(placement.kind, ShipKind::StrikeFighter);
        assert_eq!(placement.return_mode, return_mode);
        assert!(placement.is_active());

        placement.start_drag(ShipKind::HeavyDropship, return_mode);
        assert_eq!(
            placement.phase,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::CreatorLibrary)
        );
        assert_eq!(placement.kind, ShipKind::HeavyDropship);
        assert_eq!(placement.return_mode, return_mode);
    }

    #[test]
    fn ship_placement_world_click_and_drag_release_request_commit() {
        let mut placement = ShipPlacementState::default();
        placement.start_ready(ShipKind::ScoutShuttle, ActiveMode::Combat);

        assert!(!placement.handle_pointer_edges(false, true));
        assert_eq!(placement.phase, ShipPlacementPhase::Ready);
        assert!(!placement.handle_pointer_edges(true, false));
        assert_eq!(
            placement.phase,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::World)
        );
        assert!(placement.handle_pointer_edges(false, true));
        assert_eq!(placement.phase, ShipPlacementPhase::Ready);

        placement.start_drag(ShipKind::ScoutShuttle, ActiveMode::Combat);
        assert!(placement.handle_pointer_edges(false, true));
        assert_eq!(placement.phase, ShipPlacementPhase::Ready);
    }

    #[test]
    fn ship_placement_deactivation_restores_saved_mode() {
        let return_mode = ActiveMode::BuildLive {
            tool: crate::toolbelt::ToolbeltTool::Sculpt,
        };
        let mut placement = ShipPlacementState::default();
        placement.start_ready(ShipKind::ScoutShuttle, return_mode);

        assert_eq!(placement.deactivate(), return_mode);
        assert_eq!(placement.phase, ShipPlacementPhase::Inactive);
        assert!(!placement.is_active());
    }

    #[test]
    fn changing_ship_kind_retires_the_old_preview() {
        let old_preview = Entity::from_raw(42);
        let mut placement = ShipPlacementState::default();
        placement.preview = Some(old_preview);
        placement.preview_kind = Some(ShipKind::ScoutShuttle);

        placement.start_ready(ShipKind::StrikeFighter, ActiveMode::Combat);

        assert_eq!(placement.preview, None);
        assert_eq!(placement.preview_kind, None);
        assert_eq!(placement.retired_previews, vec![old_preview]);
    }

    #[test]
    fn placement_capture_allows_only_creator_originated_drag() {
        let captured = ShipInputCapture {
            pointer: true,
            keyboard: false,
            pointer_over_ui: false,
        };
        assert!(!placement_controls_allowed(
            captured,
            ShipPlacementPhase::Ready
        ));
        assert!(!placement_controls_allowed(
            captured,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::World)
        ));
        assert!(placement_controls_allowed(
            captured,
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::CreatorLibrary)
        ));
        assert!(!placement_controls_allowed(
            ShipInputCapture {
                pointer_over_ui: true,
                ..captured
            },
            ShipPlacementPhase::PointerHeld(PlacementPointerSource::CreatorLibrary)
        ));
    }

    #[test]
    fn ship_placement_accepts_only_top_facing_terrain_hits() {
        let hit = IVec3::new(4, 9, -2);
        assert_eq!(
            top_face_placement_target(hit, hit + IVec3::Y),
            Some(Vec3::new(4.5, 11.1, -1.5))
        );
        assert_eq!(top_face_placement_target(hit, hit + IVec3::X), None);
        assert_eq!(top_face_placement_target(hit, hit - IVec3::Y), None);
    }

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
            assert!(
                bp.voxels
                    .iter()
                    .filter(|v| v.block == BlockType::ShipHullAlloy)
                    .count()
                    >= 48,
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
    fn visible_ship_renderer_uses_smooth_realistic_meshes_not_voxel_blocks() {
        for kind in ShipKind::ALL {
            let shell = realistic_ship_exterior_specs(kind, ShipVisualDetail::Full);
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
            let real = realistic_cockpit_part_specs(kind, &bp, ShipVisualDetail::Full);
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
    fn physical_ship_materials_are_opaque_while_previews_remain_translucent() {
        let tones = [
            RealShipTone::CeramicWhite,
            RealShipTone::CarbonBlack,
            RealShipTone::SmokedGlass,
            RealShipTone::CyanEmission,
            RealShipTone::AmberHeat,
            RealShipTone::LuminiteGlass,
            RealShipTone::MagentaSignal,
            RealShipTone::SeatLeather,
            RealShipTone::ConsoleBlack,
        ];

        for tone in tones {
            let physical = real_ship_material_spec(tone, false);
            let preview = real_ship_material_spec(tone, true);
            assert!(
                matches!(physical.alpha_mode, AlphaMode::Opaque),
                "{tone:?} should be solid on a placed shuttle"
            );
            assert!(
                matches!(preview.alpha_mode, AlphaMode::Blend),
                "{tone:?} should remain translucent in placement preview"
            );
        }
        assert_eq!(real_ship_surface_alpha(false, 0.2), 1.0);
        assert_eq!(real_ship_surface_alpha(true, 0.42), 0.42);
    }

    #[test]
    fn low_spec_keeps_structural_layers_with_a_bounded_detail_budget() {
        assert_eq!(
            ShipVisualDetail::for_profile(RuntimeProfile::LowSpec),
            ShipVisualDetail::Core
        );
        assert_eq!(
            ShipVisualDetail::for_profile(RuntimeProfile::Balanced),
            ShipVisualDetail::Full
        );
        assert_eq!(ShipVisualDetail::Core.dynamic_light_budget(), 0);
        assert_eq!(ShipVisualDetail::Full.dynamic_light_budget(), 2);

        for kind in ShipKind::ALL {
            let bp = blueprint(kind);
            let core = realistic_ship_exterior_specs(kind, ShipVisualDetail::Core);
            let full = realistic_ship_exterior_specs(kind, ShipVisualDetail::Full);
            let canopy_frame_parts = core
                .iter()
                .filter(|part| {
                    part.mesh == RealShipMeshKind::AeroPlate
                        && part.tone == RealShipTone::CarbonBlack
                        && part.offset.y > 1.2
                        && part.offset.z < -3.0
                })
                .count();
            assert!(
                canopy_frame_parts >= 4,
                "{kind:?} should keep its solid canopy frame on LowSpec"
            );
            assert_eq!(
                full.len() - core.len(),
                7,
                "{kind:?} full exterior detail should stay tightly bounded"
            );

            let core_cockpit = realistic_cockpit_part_specs(kind, &bp, ShipVisualDetail::Core);
            let full_cockpit = realistic_cockpit_part_specs(kind, &bp, ShipVisualDetail::Full);
            assert!(core_cockpit.iter().any(|part| {
                part.mesh == RealShipMeshKind::AeroPlate
                    && part.tone == RealShipTone::ConsoleBlack
                    && part.scale.x >= 3.0
                    && part.scale.z >= 2.5
            }));
            assert_eq!(
                full_cockpit.len() - core_cockpit.len(),
                4,
                "{kind:?} full cockpit detail should stay tightly bounded"
            );

            let panels = cockpit_panel_specs(kind, &bp);
            let core_panels = panels
                .iter()
                .enumerate()
                .filter(|(index, panel)| {
                    ShipVisualDetail::Core.includes_cockpit_panel(*index, panel.tone)
                })
                .count();
            let structural = panels
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
            assert!(core_panels >= structural + 4, "{kind:?}");
            assert!(core_panels < panels.len(), "{kind:?}");
        }
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
            let core_count = specs
                .iter()
                .enumerate()
                .filter(|(index, spec)| {
                    ShipVisualDetail::Core.includes_energy_trail(*index, spec.tone)
                })
                .count();
            assert!((2..=3).contains(&core_count), "{kind:?}");
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
        let sustained_yaw = yaw + ship_bank_and_rudder_yaw_rate(1.0, 0.0);
        let reverse_yaw = ship_target_angular_rates(0.0, 0.0, -1.0, 0.0, 1.0 / 60.0).0
            + ship_bank_and_rudder_yaw_rate(-1.0, 0.0);

        assert!(yaw > 0.0);
        assert!(
            sustained_yaw > 0.30 && sustained_yaw <= 0.40,
            "combined A/D yaw should stay deliberate, got {sustained_yaw}"
        );
        assert!((sustained_yaw + reverse_yaw).abs() <= f32::EPSILON);
        assert_eq!(pitch, 0.0);

        let dt = 0.05;
        let mut motion = test_ship_motion();
        step_ship_attitude(
            &mut motion,
            Vec2::ZERO,
            1.0,
            0.0,
            0.0,
            dt,
            ship_handling_profile(ShipKind::ScoutShuttle),
        );
        assert!(motion.yaw_rate > 0.0);
        assert!(motion.yaw_rate <= SHIP_YAW_ACCEL_RAD_PER_S2 * dt + 1e-6);
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
    fn ship_q_and_e_are_direct_bounded_opposite_roll_controls() {
        assert_eq!(ship_roll_input(true, false), 1.0);
        assert_eq!(ship_roll_input(false, true), -1.0);
        assert_eq!(ship_roll_input(true, true), 0.0);

        let handling = ship_handling_profile(ShipKind::ScoutShuttle);
        let mut q_roll = test_ship_motion();
        let mut e_roll = test_ship_motion();
        step_ship_attitude(
            &mut q_roll,
            Vec2::ZERO,
            0.0,
            0.0,
            ship_roll_input(true, false),
            0.1,
            handling,
        );
        step_ship_attitude(
            &mut e_roll,
            Vec2::ZERO,
            0.0,
            0.0,
            ship_roll_input(false, true),
            0.1,
            handling,
        );

        assert!(q_roll.roll_rate > 0.0 && q_roll.roll > 0.0);
        assert!(e_roll.roll_rate < 0.0 && e_roll.roll < 0.0);
        assert!((q_roll.roll_rate + e_roll.roll_rate).abs() < 1e-6);
        assert!((q_roll.roll + e_roll.roll).abs() < 1e-6);
        assert!(q_roll.roll_rate <= SHIP_ROLL_ACCEL_RAD_PER_S2 * 0.1 + 1e-6);
    }

    #[test]
    fn ship_angular_response_has_a_hard_acceleration_bound() {
        let (rate, angle) = integrate_ship_angular_rate(0.0, 10.0, 4.0, 0.25);
        assert!((rate - 1.0).abs() < 1e-6);
        assert!((angle - 0.125).abs() < 1e-6);

        let dt = 0.05;
        let mut motion = test_ship_motion();
        step_ship_attitude(
            &mut motion,
            Vec2::new(10_000.0, -10_000.0),
            1.0,
            1.0,
            1.0,
            dt,
            ship_handling_profile(ShipKind::ScoutShuttle),
        );
        assert!(motion.yaw_rate.abs() <= SHIP_YAW_ACCEL_RAD_PER_S2 * dt + 1e-6);
        assert!(motion.pitch_rate.abs() <= SHIP_PITCH_ACCEL_RAD_PER_S2 * dt + 1e-6);
        assert!(motion.roll_rate.abs() <= SHIP_ROLL_ACCEL_RAD_PER_S2 * dt + 1e-6);
        assert!(motion.yaw_rate != 0.0 && motion.pitch_rate != 0.0 && motion.roll_rate != 0.0);
    }

    fn simulate_constant_ship_mouse(dt: f32, steps: usize) -> ShipMotion {
        let mut motion = test_ship_motion();
        let mouse_pixels_per_s = Vec2::new(900.0, -360.0);
        let handling = ship_handling_profile(ShipKind::ScoutShuttle);
        for _ in 0..steps {
            step_ship_attitude(
                &mut motion,
                mouse_pixels_per_s * dt,
                0.0,
                0.0,
                0.0,
                dt,
                handling,
            );
        }
        motion
    }

    #[test]
    fn ship_mouse_steering_is_frame_rate_independent() {
        let slow = simulate_constant_ship_mouse(1.0 / 30.0, 30);
        let fast = simulate_constant_ship_mouse(1.0 / 120.0, 120);
        assert!((slow.yaw - fast.yaw).abs() < 2e-5);
        assert!((slow.pitch - fast.pitch).abs() < 2e-5);
        assert!((slow.yaw_rate - fast.yaw_rate).abs() < 2e-5);
        assert!((slow.pitch_rate - fast.pitch_rate).abs() < 2e-5);
    }

    #[test]
    fn ship_release_decays_rates_and_auto_levels_roll_without_snapping() {
        let handling = ship_handling_profile(ShipKind::ScoutShuttle);
        let mut motion = test_ship_motion();
        motion.roll = 0.45;
        motion.yaw_rate = 0.40;
        motion.pitch_rate = -0.30;
        motion.roll_rate = 0.55;
        let initial_roll = motion.roll;
        let initial_roll_rate = motion.roll_rate;
        let dt = 1.0 / 120.0;

        step_ship_attitude(&mut motion, Vec2::ZERO, 0.0, 0.0, 0.0, dt, handling);
        assert_ne!(motion.roll, 0.0);
        assert!((motion.roll - initial_roll).abs() < 0.01);
        assert!(
            (motion.roll_rate - initial_roll_rate).abs() <= SHIP_ROLL_ACCEL_RAD_PER_S2 * dt + 1e-6
        );

        for _ in 0..1_200 {
            step_ship_attitude(&mut motion, Vec2::ZERO, 0.0, 0.0, 0.0, dt, handling);
        }
        assert!(motion.yaw_rate.abs() < 0.01);
        assert!(motion.pitch_rate.abs() < 0.01);
        assert!(motion.roll.abs() < 0.005);
        assert!(motion.roll_rate.abs() < 0.01);
    }

    #[test]
    fn ship_ui_capture_blocks_pointer_and_keyboard_control_channels() {
        assert!(ship_controls_allowed(ShipInputCapture::default()));
        assert!(!ship_controls_allowed(ShipInputCapture {
            pointer: true,
            keyboard: false,
            ..default()
        }));
        assert!(!ship_controls_allowed(ShipInputCapture {
            pointer: false,
            keyboard: true,
            ..default()
        }));
    }

    #[test]
    fn handling_profiles_keep_ship_classes_distinct_and_bounded() {
        let scout = ship_handling_profile(ShipKind::ScoutShuttle);
        let strike = ship_handling_profile(ShipKind::StrikeFighter);
        let heavy = ship_handling_profile(ShipKind::HeavyDropship);

        assert!(strike.look_response > scout.look_response);
        assert!(scout.look_response > heavy.look_response);
        assert!(strike.bank_scale > scout.bank_scale);
        assert!(scout.bank_scale > heavy.bank_scale);
        for profile in [scout, strike, heavy] {
            assert!(profile.look_response.is_finite() && profile.look_response > 0.0);
            assert!(profile.roll_response.is_finite() && profile.roll_response > 0.0);
            assert!((0.0..=0.05).contains(&profile.lateral_drift));
            assert!((0.0..=1.0).contains(&profile.pitch_level_response));
        }
    }

    #[test]
    fn ship_weapons_keep_distinct_fixed_fx_tones() {
        assert_eq!(ShipWeaponKind::IonRocket.fx_tone(), ShipFxTone::Ion);
        assert_eq!(ShipWeaponKind::PlasmaFlak.fx_tone(), ShipFxTone::Plasma);
        assert_eq!(ShipWeaponKind::RailLance.fx_tone(), ShipFxTone::Rail);
        assert_ne!(ShipFxTone::Pulse, ShipFxTone::Hostile);
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
