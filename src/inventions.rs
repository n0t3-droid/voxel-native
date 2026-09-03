//! First-class inventions: definable, placeable, simulated machines.
//!
//! Players stamp voxel blueprints (generators, turrets, portals, monorail,
//! hover pads) into the live world. Placed inventions form an energy
//! network in SI units (watts / joules) and actually run: generators
//! harvest nearby crystals, turrets spend joules to fire, portals
//! teleport when powered, rails convey, hover pads lift.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};

use crate::blocks::{BlockType, Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::editor::SimPause;
use crate::icons::{paint_icon, Icon};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::ModeContext;
use crate::player::Player;
use crate::sculpt::raycast::dda_voxel;
use crate::settings::ActiveWorld;
use crate::toolbelt::{ToolbeltState, ToolbeltTool};
use crate::world::{VoxelWorld, WorldEditBatch};

/// One voxel = one metre so the 1.62 m eye height already used by the
/// player controller stays an adult standing-eye anthropometric mean
/// (Pheasant / ISO 7250-1 order of magnitude). Hover-pad lift is a conveyor
/// velocity (7 m/s), not Earth-g acceleration (g0 = 9.806 65 m/s², CGPM 1901),
/// because this engine's player gravity is the Minecraft-like 34–52 m/s² curve.
const VOXEL_METRE: f64 = 1.0;
const GENERATOR_BASE_W: f64 = 12_000.0;
/// Compact industrial laser while firing.
const TURRET_FIRE_W: f64 = 3_000.0;
/// Idle turret keep-alive (aiming gyros / capacitors).
const TURRET_IDLE_W: f64 = 250.0;
/// Sci-fi portal keep-alive. Sized to need a generator neighbour.
const PORTAL_IDLE_W: f64 = 8_000.0;
/// Small linear motor per monorail node.
const RAIL_IDLE_W: f64 = 400.0;
/// Freight-elevator class lift while occupied.
const HOVER_PAD_W: f64 = 2_500.0;

/// Generator buffer: 20 s at base power. 12 kW × 20 s = 240 kJ.
const GENERATOR_STORAGE_J: f64 = 240_000.0;
const CONSUMER_STORAGE_J: f64 = 15_000.0;

/// Euclidean link radius for the energy union-find (metres = voxels).
const ENERGY_LINK_M: f64 = 24.0 * VOXEL_METRE;
/// Crystal harvest scan radius.
const HARVEST_RADIUS_M: i32 = 6;
const HARVEST_CRYSTAL_W: f64 = 400.0;
const HARVEST_LUMINITE_W: f64 = 800.0;
const HARVEST_LAVA_W: f64 = 200.0;
const HARVEST_BONUS_CAP_W: f64 = 20_000.0;

const TURRET_RANGE_M: f32 = 28.0;
const TURRET_COOLDOWN_S: f32 = 0.35;
const TURRET_DAMAGE: f32 = 18.0;
/// 3 kW × 0.35 s = 1 050 J per shot.
const TURRET_SHOT_J: f64 = TURRET_FIRE_W * TURRET_COOLDOWN_S as f64;

const PORTAL_TRIGGER_M: f32 = 1.6;
const PORTAL_COOLDOWN_S: f32 = 1.5;
/// 8 kW × 3 s of stored work per hop.
const PORTAL_TELEPORT_J: f64 = 24_000.0;

const RAIL_LINK_M: f64 = 18.0;
const RAIL_SPEED_M_PER_S: f32 = 10.0;
const HOVER_LIFT_M_PER_S: f32 = 7.0;

const PLACE_REACH: f32 = 96.0;
const MAX_PESTS: usize = 8;
const PEST_SPAWN_INTERVAL_S: f32 = 4.5;
const PEST_HP: f32 = 36.0;

const INVENTION_SAVE_VERSION: u32 = 1;

// ---------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InventionKind {
    CrystalGenerator,
    EnergyTurret,
    PortalGate,
    MonorailNode,
    HoverPad,
}

impl InventionKind {
    pub const ALL: [Self; 5] = [
        Self::CrystalGenerator,
        Self::EnergyTurret,
        Self::PortalGate,
        Self::MonorailNode,
        Self::HoverPad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::CrystalGenerator => "Crystal Generator",
            Self::EnergyTurret => "Energy Turret",
            Self::PortalGate => "Portal Gate",
            Self::MonorailNode => "Monorail Node",
            Self::HoverPad => "Hover Pad",
        }
    }

    pub fn chip(self) -> &'static str {
        match self {
            Self::CrystalGenerator => "GEN",
            Self::EnergyTurret => "TURRET",
            Self::PortalGate => "PORTAL",
            Self::MonorailNode => "RAIL",
            Self::HoverPad => "HOVER",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Self::CrystalGenerator => {
                "12 kW crystal genset. Harvests nearby luminite / crystal / lava into a 240 kJ buffer and powers the local invention network."
            }
            Self::EnergyTurret => {
                "Automated laser turret. Draws 3 kW while firing at energy mites and spends 1.05 kJ per shot."
            }
            Self::PortalGate => {
                "Paired teleporter frame. Needs ~8 kW idle and 24 kJ per hop; walk in to jump to the linked gate."
            }
            Self::MonorailNode => {
                "Transport node. Axis-aligns with nearby rails, shares power, and conveys anyone standing on the deck."
            }
            Self::HoverPad => {
                "Vertical lift pad. When powered it boosts anyone standing on it upward at 7 m/s."
            }
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::CrystalGenerator => Icon::LightBulb,
            Self::EnergyTurret => Icon::Magnet,
            Self::PortalGate => Icon::Teleport,
            Self::MonorailNode => Icon::Road,
            Self::HoverPad => Icon::Move,
        }
    }

    pub fn accent(self) -> Color {
        match self {
            Self::CrystalGenerator => Color::srgb(0.20, 0.95, 1.00),
            Self::EnergyTurret => Color::srgb(1.00, 0.55, 0.12),
            Self::PortalGate => Color::srgb(0.85, 0.25, 1.00),
            Self::MonorailNode => Color::srgb(0.55, 0.85, 1.00),
            Self::HoverPad => Color::srgb(0.35, 1.00, 0.55),
        }
    }

    pub fn egui_accent(self) -> egui::Color32 {
        match self {
            Self::CrystalGenerator => egui::Color32::from_rgb(40, 230, 255),
            Self::EnergyTurret => egui::Color32::from_rgb(255, 150, 40),
            Self::PortalGate => egui::Color32::from_rgb(220, 80, 255),
            Self::MonorailNode => egui::Color32::from_rgb(140, 210, 255),
            Self::HoverPad => egui::Color32::from_rgb(80, 255, 140),
        }
    }

    pub fn stepped(self, delta: isize) -> Self {
        let len = Self::ALL.len() as isize;
        let idx = Self::ALL.iter().position(|&k| k == self).unwrap_or(0) as isize;
        Self::ALL[(idx + delta).rem_euclid(len) as usize]
    }

    fn idle_load_w(self) -> f64 {
        match self {
            Self::CrystalGenerator => 0.0,
            Self::EnergyTurret => TURRET_IDLE_W,
            Self::PortalGate => PORTAL_IDLE_W,
            Self::MonorailNode => RAIL_IDLE_W,
            Self::HoverPad => 80.0,
        }
    }

    fn storage_j(self) -> f64 {
        match self {
            Self::CrystalGenerator => GENERATOR_STORAGE_J,
            _ => CONSUMER_STORAGE_J,
        }
    }

    fn is_generator(self) -> bool {
        matches!(self, Self::CrystalGenerator)
    }
}

#[derive(Debug, Clone, Copy)]
struct VoxelSpec {
    offset: IVec3,
    block: BlockType,
}

fn blueprint(kind: InventionKind) -> Vec<VoxelSpec> {
    match kind {
        InventionKind::CrystalGenerator => vec![
            VoxelSpec {
                offset: IVec3::new(0, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 1),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 1),
                block: BlockType::EngineCore,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 1),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 2),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 2),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 2),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 1, 1),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 0),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 1),
                block: BlockType::EngineCore,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 2),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(2, 1, 1),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(1, 2, 1),
                block: BlockType::CockpitGlass,
            },
            VoxelSpec {
                offset: IVec3::new(1, 3, 1),
                block: BlockType::LuminiteCrystal,
            },
            VoxelSpec {
                offset: IVec3::new(1, 4, 1),
                block: BlockType::NeonAmber,
            },
        ],
        InventionKind::EnergyTurret => vec![
            VoxelSpec {
                offset: IVec3::new(0, 0, 0),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 0),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 0),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 1),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 1),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 1),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 2),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 2),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 2),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 1),
                block: BlockType::EngineCore,
            },
            VoxelSpec {
                offset: IVec3::new(1, 2, 1),
                block: BlockType::NeonAmber,
            },
            VoxelSpec {
                offset: IVec3::new(1, 3, 1),
                block: BlockType::NeonAmber,
            },
            VoxelSpec {
                offset: IVec3::new(1, 3, 0),
                block: BlockType::NeonCyan,
            },
        ],
        InventionKind::PortalGate => vec![
            VoxelSpec {
                offset: IVec3::new(0, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 1, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(2, 1, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(0, 2, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(2, 2, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(0, 3, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 3, 0),
                block: BlockType::NeonMagenta,
            },
            VoxelSpec {
                offset: IVec3::new(2, 3, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 0),
                block: BlockType::CockpitGlass,
            },
            VoxelSpec {
                offset: IVec3::new(1, 2, 0),
                block: BlockType::CockpitGlass,
            },
        ],
        InventionKind::MonorailNode => vec![
            VoxelSpec {
                offset: IVec3::new(0, 0, 0),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 0),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 0),
                block: BlockType::ShipHullDark,
            },
            VoxelSpec {
                offset: IVec3::new(1, 1, 0),
                block: BlockType::NeonAmber,
            },
        ],
        InventionKind::HoverPad => vec![
            VoxelSpec {
                offset: IVec3::new(0, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 0),
                block: BlockType::EngineCore,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 0),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 1),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 1),
                block: BlockType::NeonCyan,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 1),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(0, 0, 2),
                block: BlockType::ShipHullAlloy,
            },
            VoxelSpec {
                offset: IVec3::new(1, 0, 2),
                block: BlockType::EngineCore,
            },
            VoxelSpec {
                offset: IVec3::new(2, 0, 2),
                block: BlockType::ShipHullAlloy,
            },
        ],
    }
}

fn rotate_offset(offset: IVec3, yaw_quarter: u8) -> IVec3 {
    match yaw_quarter % 4 {
        1 => IVec3::new(offset.z, offset.y, -offset.x),
        2 => IVec3::new(-offset.x, offset.y, -offset.z),
        3 => IVec3::new(-offset.z, offset.y, offset.x),
        _ => offset,
    }
}

fn footprint_cells(kind: InventionKind, origin: IVec3, yaw_quarter: u8) -> Vec<IVec3> {
    blueprint(kind)
        .iter()
        .map(|spec| origin + rotate_offset(spec.offset, yaw_quarter))
        .collect()
}

fn footprint_aabb(kind: InventionKind, origin: IVec3, yaw_quarter: u8) -> (IVec3, IVec3) {
    let cells = footprint_cells(kind, origin, yaw_quarter);
    let mut min = cells[0];
    let mut max = cells[0];
    for c in cells {
        min = min.min(c);
        max = max.max(c);
    }
    (min, max)
}

fn aabbs_overlap(a_min: IVec3, a_max: IVec3, b_min: IVec3, b_max: IVec3) -> bool {
    a_min.x <= b_max.x
        && a_max.x >= b_min.x
        && a_min.y <= b_max.y
        && a_max.y >= b_min.y
        && a_min.z <= b_max.z
        && a_max.z >= b_min.z
}

// ---------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedInvention {
    pub id: u64,
    pub kind: InventionKind,
    pub origin: [i32; 3],
    pub yaw_quarter: u8,
    pub energy_j: f64,
    #[serde(default)]
    pub linked: Vec<u64>,
    #[serde(default)]
    pub powered: bool,
    #[serde(skip)]
    pub fire_cooldown_s: f32,
    #[serde(skip)]
    pub replaced: Vec<(IVec3, Voxel)>,
}

impl PlacedInvention {
    fn origin_ivec(&self) -> IVec3 {
        IVec3::from_array(self.origin)
    }

    fn center(&self) -> Vec3 {
        let (min, max) = footprint_aabb(self.kind, self.origin_ivec(), self.yaw_quarter);
        (min.as_vec3() + max.as_vec3() + Vec3::ONE) * 0.5
    }

    fn muzzle(&self) -> Vec3 {
        let origin = self.origin_ivec().as_vec3() + Vec3::new(0.5, 0.5, 0.5);
        let forward = yaw_forward(self.yaw_quarter);
        origin + Vec3::Y * 3.2 + forward * 0.8
    }
}

fn yaw_forward(yaw_quarter: u8) -> Vec3 {
    match yaw_quarter % 4 {
        1 => Vec3::X,
        2 => -Vec3::Z,
        3 => -Vec3::X,
        _ => -Vec3::Z,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InventionSave {
    #[serde(default = "invention_save_version")]
    version: u32,
    placed: Vec<PlacedInvention>,
    next_id: u64,
}

fn invention_save_version() -> u32 {
    INVENTION_SAVE_VERSION
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnergyNetworkSnapshot {
    pub members: usize,
    pub generation_w: f64,
    pub load_w: f64,
    pub stored_j: f64,
    pub capacity_j: f64,
}

#[derive(Debug, Clone, Copy)]
struct FxBeam {
    from: Vec3,
    to: Vec3,
    life: f32,
    color: Color,
}

#[derive(Resource)]
pub struct InventionWorkshop {
    pub selected: InventionKind,
    pub yaw_quarter: u8,
    pub placed: Vec<PlacedInvention>,
    pub next_id: u64,
    pub status: String,
    pub last_network: EnergyNetworkSnapshot,
    loaded_world: String,
    dirty: bool,
    portal_cooldown_s: f32,
    pest_spawn_s: f32,
    fx: Vec<FxBeam>,
}

impl Default for InventionWorkshop {
    fn default() -> Self {
        Self {
            selected: InventionKind::CrystalGenerator,
            yaw_quarter: 0,
            placed: Vec::new(),
            next_id: 1,
            status: "Invention Workshop: LMB places, RMB removes, [ ] cycles, R rotates.".into(),
            last_network: EnergyNetworkSnapshot::default(),
            loaded_world: String::new(),
            dirty: false,
            portal_cooldown_s: 0.0,
            pest_spawn_s: 0.0,
            fx: Vec::new(),
        }
    }
}

impl InventionWorkshop {
    pub fn select_kind(&mut self, kind: InventionKind) {
        self.selected = kind;
        self.status = format!(
            "Armed {}. LMB stamps the voxel machine; RMB removes. {}",
            kind.label(),
            kind.blurb()
        );
    }
}

#[derive(Component)]
struct InventionPest {
    hp: f32,
    vel: Vec3,
}

pub struct InventionPlugin;

impl Plugin for InventionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(InventionWorkshop::default())
            .add_systems(
                Update,
                (
                    load_inventions_for_pending_world,
                    invention_input.run_if(in_state(GameState::InGame)),
                    simulate_inventions.run_if(in_state(GameState::InGame)),
                    apply_player_machines.run_if(in_state(GameState::InGame)),
                    spawn_and_update_pests.run_if(in_state(GameState::InGame)),
                    turret_combat.run_if(in_state(GameState::InGame)),
                    draw_invention_gizmos.run_if(in_state(GameState::InGame)),
                    draw_invention_hud.run_if(in_state(GameState::InGame)),
                    manual_save_inventions,
                )
                    .chain(),
            )
            .add_systems(OnEnter(GameState::Paused), save_inventions_on_pause);
    }
}

// ---------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
fn inventions_file(world_name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(crate::settings::SAVES_DIR)
        .join(format!(
            "{}_inventions",
            crate::settings::world_storage_stem(world_name)
        ))
        .join("workshop.ron")
}

#[cfg(target_arch = "wasm32")]
fn browser_inventions_key(world_name: &str) -> String {
    format!(
        "voxel_native.inventions.{}",
        crate::settings::world_storage_stem(world_name)
    )
}

pub fn save_inventions_for_world(world_name: &str, workshop: &InventionWorkshop) {
    let save = InventionSave {
        version: INVENTION_SAVE_VERSION,
        placed: workshop.placed.clone(),
        next_id: workshop.next_id,
    };
    let Ok(text) = ron::ser::to_string_pretty(&save, ron::ser::PrettyConfig::default()) else {
        warn!("inventions: failed serialising workshop for '{world_name}'");
        return;
    };

    #[cfg(target_arch = "wasm32")]
    {
        if let Err(e) =
            crate::platform::browser_storage_set(&browser_inventions_key(world_name), &text)
        {
            warn!("{e}");
        }
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = inventions_file(world_name);
        if let Err(e) = crate::settings::atomic_write_text(&path, &text) {
            warn!("inventions: failed writing {}: {e}", path.display());
        }
    }
}

pub fn load_inventions_for_world(world_name: &str) -> Option<(Vec<PlacedInvention>, u64)> {
    #[cfg(target_arch = "wasm32")]
    {
        let text = crate::platform::browser_storage_get(&browser_inventions_key(world_name))?;
        return match ron::from_str::<InventionSave>(&text) {
            Ok(save) => Some((save.placed, save.next_id.max(1))),
            Err(e) => {
                warn!("inventions: failed parsing browser workshop: {e}");
                None
            }
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = inventions_file(world_name);
        let text = std::fs::read_to_string(path).ok()?;
        match ron::from_str::<InventionSave>(&text) {
            Ok(save) => Some((save.placed, save.next_id.max(1))),
            Err(e) => {
                warn!("inventions: failed parsing workshop: {e}");
                None
            }
        }
    }
}

fn load_inventions_for_pending_world(
    pending: Res<PendingWorldLoad>,
    active: Option<Res<ActiveWorld>>,
    mut workshop: ResMut<InventionWorkshop>,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active else {
        return;
    };
    if workshop.loaded_world == active.meta.name {
        return;
    }
    let (placed, next_id) = load_inventions_for_world(&active.meta.name).unwrap_or_default();
    workshop.placed = placed;
    workshop.next_id = next_id.max(1);
    workshop.loaded_world = active.meta.name.clone();
    workshop.dirty = false;
    workshop.status = format!(
        "Loaded {} inventions in {}.",
        workshop.placed.len(),
        active.meta.name
    );
}

fn save_inventions_for_active(active: Option<&ActiveWorld>, workshop: &InventionWorkshop) {
    if let Some(active) = active {
        save_inventions_for_world(&active.meta.name, workshop);
    }
}

fn manual_save_inventions(
    keys: Res<ButtonInput<KeyCode>>,
    active: Option<Res<ActiveWorld>>,
    mut workshop: ResMut<InventionWorkshop>,
) {
    if keys.just_pressed(KeyCode::F5) || workshop.dirty {
        save_inventions_for_active(active.as_deref(), &workshop);
        workshop.dirty = false;
    }
}

fn save_inventions_on_pause(active: Option<Res<ActiveWorld>>, workshop: Res<InventionWorkshop>) {
    save_inventions_for_active(active.as_deref(), &workshop);
}

// ---------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------

fn live_invent_active(mode: &ModeContext) -> bool {
    mode.is_build_live() && mode.build_tool() == Some(ToolbeltTool::Invent)
}

#[allow(clippy::too_many_arguments)]
fn invention_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: EventReader<MouseWheel>,
    mode: Res<ModeContext>,
    mut workshop: ResMut<InventionWorkshop>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut history: ResMut<BuilderHistory>,
    mut world: ResMut<VoxelWorld>,
    mut telemetry: ResMut<crate::director::UnifiedTelemetry>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    active: Option<Res<ActiveWorld>>,
) {
    if !live_invent_active(&mode) {
        wheel.clear();
        return;
    }
    let cursor_locked = windows
        .get_single()
        .map(crate::mode::cursor_is_captured)
        .unwrap_or(false);
    if !cursor_locked {
        wheel.clear();
        return;
    }

    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let bare = !ctrl && !shift && !alt;

    if bare && keys.just_pressed(KeyCode::KeyR) {
        workshop.yaw_quarter = (workshop.yaw_quarter + 1) % 4;
        workshop.status = format!(
            "{} rotated ({}°).",
            workshop.selected.label(),
            workshop.yaw_quarter as u32 * 90
        );
        toolbelt.status = workshop.status.clone();
    }
    if bare && (keys.just_pressed(KeyCode::BracketLeft) || keys.just_pressed(KeyCode::Minus)) {
        let kind = workshop.selected.stepped(-1);
        workshop.select_kind(kind);
        toolbelt.status = workshop.status.clone();
    }
    if bare && (keys.just_pressed(KeyCode::BracketRight) || keys.just_pressed(KeyCode::Equal)) {
        let kind = workshop.selected.stepped(1);
        workshop.select_kind(kind);
        toolbelt.status = workshop.status.clone();
    }

    let wheel_delta: f32 = wheel.read().map(|e| e.y).sum();
    if wheel_delta.abs() > f32::EPSILON {
        let step = if wheel_delta > 0.0 { 1 } else { -1 };
        let kind = workshop.selected.stepped(step);
        workshop.select_kind(kind);
        toolbelt.status = workshop.status.clone();
    }

    let Ok(cam_tf) = cam_q.get_single() else {
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some((hit, prev)) = dda_voxel(&world, origin, dir, PLACE_REACH) else {
        if mouse.just_pressed(MouseButton::Left) {
            workshop.status = "No block face under crosshair. Aim at terrain or a platform.".into();
            toolbelt.status = workshop.status.clone();
        }
        return;
    };

    let selected = workshop.selected;
    let yaw = workshop.yaw_quarter;
    if bare && mouse.just_pressed(MouseButton::Left) {
        let place_origin = prev;
        match place_invention(
            &mut workshop,
            &mut world,
            &mut history,
            selected,
            place_origin,
            yaw,
        ) {
            Ok(id) => {
                workshop.status = format!(
                    "Placed {} #{} at {},{},{}.",
                    selected.label(),
                    id,
                    place_origin.x,
                    place_origin.y,
                    place_origin.z
                );
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.invention_actions = telemetry.invention_actions.saturating_add(1);
                save_inventions_for_active(active.as_deref(), &workshop);
                workshop.dirty = false;
            }
            Err(msg) => {
                workshop.status = msg;
            }
        }
        toolbelt.status = workshop.status.clone();
    }

    if bare && mouse.just_pressed(MouseButton::Right) {
        if let Some(id) =
            invention_at(&workshop.placed, hit).or_else(|| invention_at(&workshop.placed, prev))
        {
            if let Some(removed) = remove_invention(&mut workshop, &mut world, &mut history, id) {
                workshop.status = format!("Removed {} #{id}.", removed.kind.label());
                telemetry.build_actions = telemetry.build_actions.saturating_add(1);
                telemetry.invention_actions = telemetry.invention_actions.saturating_add(1);
                save_inventions_for_active(active.as_deref(), &workshop);
                workshop.dirty = false;
            }
        } else {
            workshop.status = "No invention under crosshair to remove.".into();
        }
        toolbelt.status = workshop.status.clone();
    }
}

fn invention_at(placed: &[PlacedInvention], cell: IVec3) -> Option<u64> {
    placed.iter().find_map(|inv| {
        let cells = footprint_cells(inv.kind, inv.origin_ivec(), inv.yaw_quarter);
        cells.contains(&cell).then_some(inv.id)
    })
}

fn occupancy_blocked(
    placed: &[PlacedInvention],
    kind: InventionKind,
    origin: IVec3,
    yaw: u8,
) -> bool {
    let (min, max) = footprint_aabb(kind, origin, yaw);
    placed.iter().any(|inv| {
        let (omin, omax) = footprint_aabb(inv.kind, inv.origin_ivec(), inv.yaw_quarter);
        aabbs_overlap(min, max, omin, omax)
    })
}

fn place_invention(
    workshop: &mut InventionWorkshop,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    kind: InventionKind,
    origin: IVec3,
    yaw_quarter: u8,
) -> Result<u64, String> {
    if occupancy_blocked(&workshop.placed, kind, origin, yaw_quarter) {
        return Err("That footprint overlaps an existing invention.".into());
    }
    let mut replaced = Vec::new();
    let mut changes = Vec::new();
    let mut batch = WorldEditBatch::default();
    for spec in blueprint(kind) {
        let pos = origin + rotate_offset(spec.offset, yaw_quarter);
        let before = world.voxel_at(pos.x, pos.y, pos.z);
        let after: Voxel = spec.block.into();
        replaced.push((pos, before));
        if let Some((prev, next)) =
            world.edit_set_voxel_batched(pos.x, pos.y, pos.z, after, &mut batch)
        {
            changes.push((pos, prev, next));
        }
    }
    world.finish_edit_batch(batch);
    history.record_external(format!("Place {}", kind.label()), changes);
    let id = workshop.next_id;
    workshop.next_id += 1;
    workshop.placed.push(PlacedInvention {
        id,
        kind,
        origin: origin.to_array(),
        yaw_quarter: yaw_quarter % 4,
        energy_j: if kind.is_generator() {
            kind.storage_j() * 0.25
        } else {
            0.0
        },
        linked: Vec::new(),
        powered: kind.is_generator(),
        fire_cooldown_s: 0.0,
        replaced,
    });
    relink_inventions(&mut workshop.placed);
    workshop.dirty = true;
    Ok(id)
}

fn remove_invention(
    workshop: &mut InventionWorkshop,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    id: u64,
) -> Option<PlacedInvention> {
    let idx = workshop.placed.iter().position(|inv| inv.id == id)?;
    let inv = workshop.placed.remove(idx);
    let mut changes = Vec::new();
    let mut batch = WorldEditBatch::default();
    if inv.replaced.is_empty() {
        for spec in blueprint(inv.kind) {
            let pos = inv.origin_ivec() + rotate_offset(spec.offset, inv.yaw_quarter);
            if let Some((prev, next)) =
                world.edit_set_voxel_batched(pos.x, pos.y, pos.z, AIR, &mut batch)
            {
                changes.push((pos, prev, next));
            }
        }
    } else {
        for (pos, voxel) in &inv.replaced {
            if let Some((prev, next)) =
                world.edit_set_voxel_batched(pos.x, pos.y, pos.z, *voxel, &mut batch)
            {
                changes.push((*pos, prev, next));
            }
        }
    }
    world.finish_edit_batch(batch);
    history.record_external(format!("Remove {}", inv.kind.label()), changes);
    for other in &mut workshop.placed {
        other.linked.retain(|linked| *linked != id);
    }
    relink_inventions(&mut workshop.placed);
    workshop.dirty = true;
    Some(inv)
}

fn relink_inventions(placed: &mut [PlacedInvention]) {
    for inv in placed.iter_mut() {
        if matches!(
            inv.kind,
            InventionKind::PortalGate | InventionKind::MonorailNode
        ) {
            inv.linked.clear();
        }
    }
    pair_portals(placed);
    link_rails(placed);
}

fn pair_portals(placed: &mut [PlacedInvention]) {
    let portals: Vec<u64> = placed
        .iter()
        .filter(|inv| inv.kind == InventionKind::PortalGate)
        .map(|inv| inv.id)
        .collect();
    for chunk in portals.chunks(2) {
        if chunk.len() == 2 {
            let a = chunk[0];
            let b = chunk[1];
            if let Some(inv) = placed.iter_mut().find(|inv| inv.id == a) {
                inv.linked = vec![b];
            }
            if let Some(inv) = placed.iter_mut().find(|inv| inv.id == b) {
                inv.linked = vec![a];
            }
        }
    }
}

fn link_rails(placed: &mut [PlacedInvention]) {
    let rails: Vec<(u64, Vec3, IVec3)> = placed
        .iter()
        .filter(|inv| inv.kind == InventionKind::MonorailNode)
        .map(|inv| (inv.id, inv.center(), inv.origin_ivec()))
        .collect();
    for i in 0..rails.len() {
        for j in (i + 1)..rails.len() {
            let (id_a, ca, oa) = rails[i];
            let (id_b, cb, ob) = rails[j];
            let delta = cb - ca;
            let dist = delta.length() as f64;
            if dist > RAIL_LINK_M || dist < 0.5 {
                continue;
            }
            let dx = (oa.x - ob.x).unsigned_abs();
            let dz = (oa.z - ob.z).unsigned_abs();
            let dy = (oa.y - ob.y).unsigned_abs();
            let aligned = dy <= 4 && (dx == 0 || dz == 0 || dx.max(dz) >= 2 * dx.min(dz));
            if !aligned {
                continue;
            }
            if let Some(inv) = placed.iter_mut().find(|inv| inv.id == id_a) {
                if !inv.linked.contains(&id_b) {
                    inv.linked.push(id_b);
                }
            }
            if let Some(inv) = placed.iter_mut().find(|inv| inv.id == id_b) {
                if !inv.linked.contains(&id_a) {
                    inv.linked.push(id_a);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------

fn harvest_bonus_w(world: &VoxelWorld, origin: IVec3) -> f64 {
    let mut bonus = 0.0;
    for dy in -HARVEST_RADIUS_M..=HARVEST_RADIUS_M {
        for dz in -HARVEST_RADIUS_M..=HARVEST_RADIUS_M {
            for dx in -HARVEST_RADIUS_M..=HARVEST_RADIUS_M {
                if dx.abs() + dy.abs() + dz.abs() > HARVEST_RADIUS_M {
                    continue;
                }
                let v = world.voxel_at(origin.x + dx, origin.y + dy, origin.z + dz);
                bonus += match BlockType::from_voxel(v) {
                    BlockType::LuminiteCrystal => HARVEST_LUMINITE_W,
                    BlockType::Crystal => HARVEST_CRYSTAL_W,
                    BlockType::Lava => HARVEST_LAVA_W,
                    _ => 0.0,
                };
            }
        }
    }
    bonus.min(HARVEST_BONUS_CAP_W)
}

fn generation_w(world: &VoxelWorld, inv: &PlacedInvention) -> f64 {
    if !inv.kind.is_generator() {
        return 0.0;
    }
    GENERATOR_BASE_W + harvest_bonus_w(world, inv.origin_ivec())
}

fn energy_groups(placed: &[PlacedInvention]) -> Vec<Vec<usize>> {
    let n = placed.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let union = |a: usize, b: usize, parent: &mut [usize]| {
        let pa = find(parent, a);
        let pb = find(parent, b);
        if pa != pb {
            parent[pb] = pa;
        }
    };
    for i in 0..n {
        for j in (i + 1)..n {
            let d = placed[i].center().distance(placed[j].center()) as f64;
            if d <= ENERGY_LINK_M {
                union(i, j, &mut parent);
            }
        }
    }
    let mut buckets: ahash::AHashMap<usize, Vec<usize>> = ahash::AHashMap::new();
    for i in 0..n {
        buckets.entry(find(&mut parent, i)).or_default().push(i);
    }
    buckets.into_values().collect()
}

#[derive(Debug, Clone, Copy)]
struct TickDemand {
    hover_occupied: bool,
    rail_occupied: bool,
    turret_firing: bool,
}

fn tick_energy_networks(
    world: &VoxelWorld,
    placed: &mut [PlacedInvention],
    dt: f64,
    demand: &[TickDemand],
) -> EnergyNetworkSnapshot {
    let dt = dt.max(0.0);
    let mut snapshot = EnergyNetworkSnapshot::default();
    if placed.is_empty() {
        return snapshot;
    }
    let groups = energy_groups(placed);
    for group in groups {
        let mut gen = 0.0;
        let mut load = 0.0;
        let mut stored = 0.0;
        let mut cap = 0.0;
        for &i in &group {
            gen += generation_w(world, &placed[i]);
            load += placed[i].kind.idle_load_w();
            let extra = match placed[i].kind {
                InventionKind::EnergyTurret
                    if demand.get(i).map(|d| d.turret_firing).unwrap_or(false) =>
                {
                    TURRET_FIRE_W
                }
                InventionKind::HoverPad
                    if demand.get(i).map(|d| d.hover_occupied).unwrap_or(false) =>
                {
                    HOVER_PAD_W
                }
                InventionKind::MonorailNode
                    if demand.get(i).map(|d| d.rail_occupied).unwrap_or(false) =>
                {
                    900.0
                }
                _ => 0.0,
            };
            load += extra;
            stored += placed[i].energy_j;
            cap += placed[i].kind.storage_j();
        }
        let net = gen - load;
        let mut next_stored = (stored + net * dt).clamp(0.0, cap);
        let powered = next_stored > 1.0 || net >= 0.0;
        if !powered {
            next_stored = 0.0;
        }
        for &i in &group {
            let share = if cap > 0.0 {
                placed[i].kind.storage_j() / cap
            } else {
                0.0
            };
            placed[i].energy_j = next_stored * share;
            placed[i].powered = powered;
        }
        snapshot.members += group.len();
        snapshot.generation_w += gen;
        snapshot.load_w += load;
        snapshot.stored_j += next_stored;
        snapshot.capacity_j += cap;
    }
    snapshot
}

fn simulate_inventions(
    time: Res<Time>,
    pause: Res<SimPause>,
    world: Res<VoxelWorld>,
    mut workshop: ResMut<InventionWorkshop>,
    player_q: Query<&Transform, With<Player>>,
) {
    if pause.paused {
        return;
    }
    let dt = time.delta_seconds() as f64;
    workshop.portal_cooldown_s = (workshop.portal_cooldown_s - time.delta_seconds()).max(0.0);
    for beam in &mut workshop.fx {
        beam.life -= time.delta_seconds();
    }
    workshop.fx.retain(|b| b.life > 0.0);

    let player_pos = player_q
        .get_single()
        .map(|t| t.translation)
        .unwrap_or(Vec3::ZERO);
    let n = workshop.placed.len();
    let mut demand = vec![
        TickDemand {
            hover_occupied: false,
            rail_occupied: false,
            turret_firing: false,
        };
        n
    ];
    for (i, inv) in workshop.placed.iter().enumerate() {
        let center = inv.center();
        match inv.kind {
            InventionKind::HoverPad => {
                demand[i].hover_occupied = player_pos.distance(center) < 2.2;
            }
            InventionKind::MonorailNode => {
                demand[i].rail_occupied = player_pos.distance(center) < 2.0;
            }
            _ => {}
        }
    }
    workshop.last_network = tick_energy_networks(&world, &mut workshop.placed, dt, &demand);
}

fn apply_player_machines(
    pause: Res<SimPause>,
    mut workshop: ResMut<InventionWorkshop>,
    mut player_q: Query<(&mut Transform, &mut Player), With<Player>>,
) {
    if pause.paused {
        return;
    }
    let Ok((mut tf, mut player)) = player_q.get_single_mut() else {
        return;
    };
    let feet = tf.translation - Vec3::Y * 1.4;
    let mut lift = 0.0_f32;
    let mut rail_push = Vec3::ZERO;

    let placed = workshop.placed.clone();
    for inv in &placed {
        if !inv.powered {
            continue;
        }
        let center = inv.center();
        match inv.kind {
            InventionKind::HoverPad => {
                if feet.distance(center) < 2.1 {
                    lift = lift.max(HOVER_LIFT_M_PER_S);
                }
            }
            InventionKind::MonorailNode => {
                if feet.distance(center) < 1.8 {
                    if let Some(next_id) = inv.linked.first() {
                        if let Some(next) = placed.iter().find(|o| o.id == *next_id) {
                            let dir = (next.center() - center).normalize_or_zero();
                            rail_push += dir * RAIL_SPEED_M_PER_S;
                        }
                    }
                }
            }
            InventionKind::PortalGate
                if workshop.portal_cooldown_s <= 0.0
                    && inv.energy_j >= PORTAL_TELEPORT_J
                    && tf.translation.distance(center) < PORTAL_TRIGGER_M =>
            {
                if let Some(dest_id) = inv.linked.first() {
                    if let Some(dest) = placed.iter().find(|o| o.id == *dest_id) {
                        if dest.powered {
                            let dest_pos =
                                dest.center() + Vec3::Y * 0.6 + yaw_forward(dest.yaw_quarter) * 1.4;
                            tf.translation = dest_pos;
                            player.velocity = Vec3::ZERO;
                            workshop.portal_cooldown_s = PORTAL_COOLDOWN_S;
                            if let Some(src) = workshop.placed.iter_mut().find(|o| o.id == inv.id) {
                                src.energy_j = (src.energy_j - PORTAL_TELEPORT_J).max(0.0);
                            }
                            workshop.status = format!("Portal hop {} → {}.", inv.id, dest.id);
                            workshop.fx.push(FxBeam {
                                from: center,
                                to: dest.center(),
                                life: 0.45,
                                color: InventionKind::PortalGate.accent(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if lift > 0.0 {
        player.velocity.y = player.velocity.y.max(lift);
        player.flying = false;
    }
    if rail_push.length_squared() > 0.01 {
        player.velocity.x = rail_push.x;
        player.velocity.z = rail_push.z;
    }
}

fn spawn_and_update_pests(
    time: Res<Time>,
    pause: Res<SimPause>,
    mut commands: Commands,
    mut workshop: ResMut<InventionWorkshop>,
    mut pests: Query<(Entity, &mut Transform, &mut InventionPest)>,
) {
    if pause.paused {
        return;
    }
    let dt = time.delta_seconds();
    workshop.pest_spawn_s -= dt;
    let generators: Vec<Vec3> = workshop
        .placed
        .iter()
        .filter(|inv| inv.kind.is_generator())
        .map(|inv| inv.center())
        .collect();
    let count = pests.iter().count();
    if workshop.pest_spawn_s <= 0.0 && count < MAX_PESTS && !generators.is_empty() {
        workshop.pest_spawn_s = PEST_SPAWN_INTERVAL_S;
        let anchor = generators[count % generators.len()];
        let phase = (workshop.next_id as f32) * 1.7 + count as f32;
        let offset = Vec3::new(phase.sin() * 10.0, 1.2, phase.cos() * 10.0);
        commands.spawn((
            InventionPest {
                hp: PEST_HP,
                vel: Vec3::new(-offset.z, 0.0, offset.x).normalize_or_zero() * 2.4,
            },
            SpatialBundle {
                transform: Transform::from_translation(anchor + offset),
                ..default()
            },
            Name::new("InventionPest"),
        ));
    }

    for (_, mut tf, mut pest) in pests.iter_mut() {
        let wobble = (time.elapsed_seconds() * 2.4 + tf.translation.x * 0.05).sin();
        pest.vel = Vec3::new(
            pest.vel.x * 0.98 + wobble * 0.4,
            0.0,
            pest.vel.z * 0.98 - wobble * 0.35,
        );
        tf.translation += pest.vel * dt;
        tf.translation.y += (time.elapsed_seconds() * 3.1).sin() * 0.01;
    }
}

fn turret_combat(
    time: Res<Time>,
    pause: Res<SimPause>,
    mut workshop: ResMut<InventionWorkshop>,
    mut pests: Query<(Entity, &Transform, &mut InventionPest)>,
    mut commands: Commands,
) {
    if pause.paused {
        return;
    }
    let dt = time.delta_seconds();
    let pest_snapshot: Vec<(Entity, Vec3)> =
        pests.iter().map(|(e, tf, _)| (e, tf.translation)).collect();
    let mut hits: Vec<(Entity, f32, Vec3, Vec3)> = Vec::new();

    for inv in workshop.placed.iter_mut() {
        inv.fire_cooldown_s = (inv.fire_cooldown_s - dt).max(0.0);
        if inv.kind != InventionKind::EnergyTurret || !inv.powered || inv.energy_j < TURRET_SHOT_J {
            continue;
        }
        if inv.fire_cooldown_s > 0.0 {
            continue;
        }
        let muzzle = inv.muzzle();
        let Some((target_e, target_pos)) = pest_snapshot
            .iter()
            .map(|(e, p)| (*e, *p))
            .filter(|(_, p)| muzzle.distance(*p) <= TURRET_RANGE_M)
            .min_by(|a, b| {
                muzzle
                    .distance(a.1)
                    .partial_cmp(&muzzle.distance(b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            continue;
        };
        inv.energy_j -= TURRET_SHOT_J;
        inv.fire_cooldown_s = TURRET_COOLDOWN_S;
        hits.push((target_e, TURRET_DAMAGE, muzzle, target_pos));
    }
    for (_, _, muzzle, target_pos) in &hits {
        workshop.fx.push(FxBeam {
            from: *muzzle,
            to: *target_pos,
            life: 0.12,
            color: InventionKind::EnergyTurret.accent(),
        });
    }

    for (entity, damage, _, _) in hits {
        if let Ok((_, _, mut pest)) = pests.get_mut(entity) {
            pest.hp -= damage;
            if pest.hp <= 0.0 {
                commands.entity(entity).despawn();
            }
        }
    }
}

fn draw_invention_gizmos(
    mut gizmos: Gizmos,
    workshop: Res<InventionWorkshop>,
    mode: Res<ModeContext>,
    world: Res<VoxelWorld>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    pests: Query<&Transform, With<InventionPest>>,
) {
    for inv in &workshop.placed {
        let color = if inv.powered {
            inv.kind.accent()
        } else {
            Color::srgb(0.45, 0.45, 0.48)
        };
        let (min, max) = footprint_aabb(inv.kind, inv.origin_ivec(), inv.yaw_quarter);
        let a = min.as_vec3();
        let b = max.as_vec3() + Vec3::ONE;
        gizmos.cuboid(
            Transform::from_translation((a + b) * 0.5).with_scale(b - a),
            color,
        );
        for linked in &inv.linked {
            if let Some(other) = workshop.placed.iter().find(|o| o.id == *linked) {
                if other.id > inv.id {
                    gizmos.line(inv.center(), other.center(), color);
                }
            }
        }
    }
    for beam in &workshop.fx {
        gizmos.line(beam.from, beam.to, beam.color);
    }
    for tf in pests.iter() {
        gizmos.sphere(
            tf.translation,
            Quat::IDENTITY,
            0.45,
            Color::srgb(0.45, 1.0, 0.2),
        );
        gizmos.sphere(
            tf.translation + Vec3::new(0.28, -0.2, 0.1),
            Quat::IDENTITY,
            0.16,
            Color::srgb(0.25, 0.85, 0.15),
        );
        gizmos.sphere(
            tf.translation + Vec3::new(-0.28, -0.2, -0.08),
            Quat::IDENTITY,
            0.16,
            Color::srgb(0.25, 0.85, 0.15),
        );
    }

    if !live_invent_active(&mode) {
        return;
    }
    let Ok(cam_tf) = cam_q.get_single() else {
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let Some((_, prev)) = dda_voxel(&world, origin, dir, PLACE_REACH) else {
        return;
    };
    let kind = workshop.selected;
    let yaw = workshop.yaw_quarter;
    let blocked = occupancy_blocked(&workshop.placed, kind, prev, yaw);
    let preview = if blocked {
        Color::srgb(1.0, 0.25, 0.2)
    } else {
        kind.accent()
    };
    for spec in blueprint(kind) {
        let pos = (prev + rotate_offset(spec.offset, yaw)).as_vec3() + Vec3::splat(0.5);
        gizmos.cuboid(
            Transform::from_translation(pos).with_scale(Vec3::splat(0.92)),
            preview,
        );
    }
}

fn draw_invention_hud(
    mut contexts: EguiContexts,
    workshop: Res<InventionWorkshop>,
    mode: Res<ModeContext>,
    settings: Res<crate::settings::WorldSettings>,
) {
    let invent_tool = live_invent_active(&mode);
    if !invent_tool && workshop.placed.is_empty() {
        return;
    }
    let ctx = contexts.ctx_mut();
    let theme = settings.theme;
    let net = workshop.last_network;
    let kw_in = net.generation_w / 1000.0;
    let kw_out = net.load_w / 1000.0;
    let kj = net.stored_j / 1000.0;
    let kj_cap = net.capacity_j / 1000.0;
    let text = if invent_tool {
        format!(
            "{}  yaw {}°  |  net {:.1} kW in / {:.1} kW out  |  {:.0}/{:.0} kJ  |  {} machines\n{}",
            workshop.selected.label(),
            workshop.yaw_quarter as u32 * 90,
            kw_in,
            kw_out,
            kj,
            kj_cap,
            workshop.placed.len(),
            workshop.status
        )
    } else {
        format!(
            "INVENTIONS  {:.1} kW in / {:.1} kW out  ·  {:.0} kJ  ·  {} machines",
            kw_in,
            kw_out,
            kj,
            workshop.placed.len()
        )
    };
    egui::Area::new(egui::Id::new("invention_hud"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(18.0, -92.0))
        .show(ctx, |ui| {
            egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(4, 12, 18, 180))
                .rounding(egui::Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        paint_icon(
                            ui.painter(),
                            icon_rect,
                            workshop.selected.icon(),
                            workshop.selected.egui_accent(),
                        );
                        ui.label(
                            egui::RichText::new(text)
                                .monospace()
                                .size(12.0)
                                .color(theme.color.primary()),
                        );
                    });
                });
        });
}

pub fn arm_invention_tool(
    workshop: &mut InventionWorkshop,
    toolbelt: &mut ToolbeltState,
    mode: &mut ModeContext,
    kind: InventionKind,
) {
    workshop.select_kind(kind);
    toolbelt.tool = ToolbeltTool::Invent;
    toolbelt.live = true;
    toolbelt.palette_open = false;
    let status = format!("Build Live: [INVENT] {}. {}", kind.label(), kind.blurb());
    toolbelt.status = status.clone();
    mode.set(
        crate::mode::ActiveMode::BuildLive {
            tool: ToolbeltTool::Invent,
        },
        status,
    );
}

// ---------------------------------------------------------------------
// Tests — simulation is the product; these lock SI energy + linking.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_world() -> VoxelWorld {
        VoxelWorld::new()
    }

    fn stamp_cell(world: &mut VoxelWorld, pos: IVec3, block: BlockType) {
        world.edit_set_voxel(pos.x, pos.y, pos.z, block.into());
    }

    fn placed(kind: InventionKind, origin: IVec3, id: u64) -> PlacedInvention {
        PlacedInvention {
            id,
            kind,
            origin: origin.to_array(),
            yaw_quarter: 0,
            energy_j: 0.0,
            linked: Vec::new(),
            powered: false,
            fire_cooldown_s: 0.0,
            replaced: Vec::new(),
        }
    }

    #[test]
    fn catalog_covers_the_sci_fi_invention_set() {
        let labels: Vec<_> = InventionKind::ALL.iter().map(|k| k.label()).collect();
        assert!(labels.iter().any(|l| l.contains("Generator")));
        assert!(labels.iter().any(|l| l.contains("Turret")));
        assert!(labels.iter().any(|l| l.contains("Portal")));
        assert!(labels.iter().any(|l| l.contains("Monorail")));
        assert!(labels.iter().any(|l| l.contains("Hover")));
        for kind in InventionKind::ALL {
            assert!(!blueprint(kind).is_empty(), "{kind:?} needs voxels");
        }
    }

    #[test]
    fn generator_base_power_is_industrial_genset_class() {
        // 12 kW sits in the 5–20 kW portable industrial genset band.
        assert_eq!(GENERATOR_BASE_W, 12_000.0);
        assert_eq!(GENERATOR_STORAGE_J, GENERATOR_BASE_W * 20.0);
        assert!((TURRET_SHOT_J - TURRET_FIRE_W * TURRET_COOLDOWN_S as f64).abs() < 1e-9);
        assert_eq!(PORTAL_TELEPORT_J, PORTAL_IDLE_W * 3.0);
    }

    #[test]
    fn yaw_rotation_is_right_handed_90_degree_steps() {
        let p = IVec3::new(2, 1, 0);
        assert_eq!(rotate_offset(p, 0), IVec3::new(2, 1, 0));
        assert_eq!(rotate_offset(p, 1), IVec3::new(0, 1, -2));
        assert_eq!(rotate_offset(p, 2), IVec3::new(-2, 1, 0));
        assert_eq!(rotate_offset(p, 3), IVec3::new(0, 1, 2));
        assert_eq!(rotate_offset(p, 4), p);
    }

    #[test]
    fn occupancy_rejects_overlapping_footprints() {
        let existing = vec![placed(InventionKind::HoverPad, IVec3::new(0, 4, 0), 1)];
        assert!(occupancy_blocked(
            &existing,
            InventionKind::HoverPad,
            IVec3::new(1, 4, 1),
            0
        ));
        assert!(!occupancy_blocked(
            &existing,
            InventionKind::HoverPad,
            IVec3::new(8, 4, 8),
            0
        ));
    }

    #[test]
    fn generator_powers_nearby_turret_after_one_second() {
        let world = empty_world();
        let mut placed = vec![
            placed(InventionKind::CrystalGenerator, IVec3::new(0, 8, 0), 1),
            placed(InventionKind::EnergyTurret, IVec3::new(6, 8, 0), 2),
        ];
        let demand = vec![
            TickDemand {
                hover_occupied: false,
                rail_occupied: false,
                turret_firing: false,
            },
            TickDemand {
                hover_occupied: false,
                rail_occupied: false,
                turret_firing: false,
            },
        ];
        let snap = tick_energy_networks(&world, &mut placed, 1.0, &demand);
        assert!(snap.generation_w >= GENERATOR_BASE_W);
        assert!(placed[0].powered);
        assert!(placed[1].powered);
        assert!(placed[0].energy_j > 0.0);
        assert!(placed[1].energy_j > 0.0);
    }

    #[test]
    fn isolated_turret_stays_unpowered_without_a_generator() {
        let world = empty_world();
        let mut placed = vec![placed(InventionKind::EnergyTurret, IVec3::ZERO, 1)];
        let demand = vec![TickDemand {
            hover_occupied: false,
            rail_occupied: false,
            turret_firing: false,
        }];
        let snap = tick_energy_networks(&world, &mut placed, 2.0, &demand);
        assert!(snap.generation_w < 1.0);
        assert!(!placed[0].powered);
        assert_eq!(placed[0].energy_j, 0.0);
    }

    #[test]
    fn distant_machines_do_not_share_energy() {
        let world = empty_world();
        let mut placed = vec![
            placed(InventionKind::CrystalGenerator, IVec3::new(0, 4, 0), 1),
            placed(InventionKind::EnergyTurret, IVec3::new(80, 4, 0), 2),
        ];
        let demand = vec![
            TickDemand {
                hover_occupied: false,
                rail_occupied: false,
                turret_firing: false,
            },
            TickDemand {
                hover_occupied: false,
                rail_occupied: false,
                turret_firing: false,
            },
        ];
        tick_energy_networks(&world, &mut placed, 1.0, &demand);
        assert!(placed[0].powered);
        assert!(!placed[1].powered);
    }

    #[test]
    fn luminite_within_harvest_radius_increases_generation() {
        let mut world = empty_world();
        stamp_cell(&mut world, IVec3::new(2, 9, 0), BlockType::LuminiteCrystal);
        stamp_cell(&mut world, IVec3::new(3, 8, 1), BlockType::Crystal);
        let gen = placed(InventionKind::CrystalGenerator, IVec3::new(0, 8, 0), 1);
        let bonus = harvest_bonus_w(&world, gen.origin_ivec());
        assert!(bonus >= HARVEST_LUMINITE_W + HARVEST_CRYSTAL_W);
        let base = generation_w(&empty_world(), &gen);
        let boosted = generation_w(&world, &gen);
        assert!(boosted > base);
    }

    #[test]
    fn portals_pair_in_placement_order() {
        let mut placed = vec![
            placed(InventionKind::PortalGate, IVec3::new(0, 4, 0), 1),
            placed(InventionKind::HoverPad, IVec3::new(4, 4, 0), 2),
            placed(InventionKind::PortalGate, IVec3::new(10, 4, 0), 3),
        ];
        pair_portals(&mut placed);
        assert_eq!(placed[0].linked, vec![3]);
        assert_eq!(placed[2].linked, vec![1]);
        assert!(placed[1].linked.is_empty());
    }

    #[test]
    fn rails_link_when_axis_aligned_and_in_range() {
        let mut placed = vec![
            placed(InventionKind::MonorailNode, IVec3::new(0, 10, 0), 1),
            placed(InventionKind::MonorailNode, IVec3::new(12, 10, 0), 2),
            placed(InventionKind::MonorailNode, IVec3::new(40, 10, 8), 3),
        ];
        link_rails(&mut placed);
        assert!(placed[0].linked.contains(&2));
        assert!(placed[1].linked.contains(&1));
        assert!(placed[2].linked.is_empty());
    }

    #[test]
    fn place_and_remove_restores_overwritten_voxels() {
        let mut world = empty_world();
        stamp_cell(&mut world, IVec3::new(1, 5, 1), BlockType::Grass);
        let mut workshop = InventionWorkshop::default();
        let mut history = BuilderHistory::default();
        let id = place_invention(
            &mut workshop,
            &mut world,
            &mut history,
            InventionKind::HoverPad,
            IVec3::new(0, 5, 0),
            0,
        )
        .expect("place");
        assert_eq!(workshop.placed.len(), 1);
        let core = world.voxel_at(1, 5, 1);
        let grass: Voxel = BlockType::Grass.into();
        assert_ne!(core, grass);
        remove_invention(&mut workshop, &mut world, &mut history, id);
        assert!(workshop.placed.is_empty());
        assert_eq!(world.voxel_at(1, 5, 1), grass);
    }

    #[test]
    fn save_roundtrip_keeps_kind_origin_and_energy() {
        let original = PlacedInvention {
            id: 7,
            kind: InventionKind::PortalGate,
            origin: [3, 12, -4],
            yaw_quarter: 2,
            energy_j: 12_500.0,
            linked: vec![8],
            powered: true,
            fire_cooldown_s: 0.2,
            replaced: Vec::new(),
        };
        let save = InventionSave {
            version: INVENTION_SAVE_VERSION,
            placed: vec![original.clone()],
            next_id: 9,
        };
        let text = ron::ser::to_string(&save).unwrap();
        let loaded: InventionSave = ron::from_str(&text).unwrap();
        assert_eq!(loaded.placed[0].id, 7);
        assert_eq!(loaded.placed[0].kind, InventionKind::PortalGate);
        assert_eq!(loaded.placed[0].origin, [3, 12, -4]);
        assert_eq!(loaded.placed[0].yaw_quarter, 2);
        assert!((loaded.placed[0].energy_j - 12_500.0).abs() < 1e-6);
        assert_eq!(loaded.next_id, 9);
    }

    #[test]
    fn firing_turret_cannot_spend_energy_it_does_not_have() {
        let world = empty_world();
        let mut placed = vec![placed(InventionKind::EnergyTurret, IVec3::ZERO, 1)];
        placed[0].energy_j = TURRET_SHOT_J * 0.5;
        placed[0].powered = true;
        let demand = vec![TickDemand {
            hover_occupied: false,
            rail_occupied: false,
            turret_firing: true,
        }];
        tick_energy_networks(&world, &mut placed, 0.35, &demand);
        assert!(placed[0].energy_j < TURRET_SHOT_J);
    }
}
