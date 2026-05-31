//! Friendly bot settlement brain.
//!
//! This is the first "smart world" slice: deterministic friendly agents
//! plan and build a saved voxel settlement while the player flies, shoots
//! training targets, and uses the existing editor/build systems.

use ahash::AHashMap;
use bevy::app::AppExit;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiSet};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use crate::blocks::{BlockType, Voxel, AIR};
use crate::builder::BuilderHistory;
use crate::editor::{EditorState, EditorTab};
use crate::menu::{GameState, PendingWorldLoad};
use crate::neurocore::RuntimeBudget;
use crate::player::Player;
#[cfg(not(target_arch = "wasm32"))]
use crate::settings::SAVES_DIR;
use crate::settings::{ActiveWorld, WorldSettings};
use crate::ships::ShipInstance;
use crate::world::{
    save_edited_overrides_for_world, save_edited_overrides_snapshot, EditedChunkOverride,
    VoxelWorld, WorldEditBatch,
};

const MEGA_CITY_RADIUS: i32 = 1024;
const DEFAULT_MAX_ACTIVE_PROJECTS: usize = 8;
const AUTONOMY_BURST_ACTIVE_PROJECTS: usize = 12;
const MAX_ACTIVE_PROJECTS_LIMIT: usize = 48;
const MAX_CREW_BOTS_PER_PROJECT: usize = 32;
const COMPANION_WORKERS_PER_LEADER: u8 = 4;
const VISIBLE_MESSAGE_COOLDOWN: f32 = 10.0;
const CONVERSATION_INTERVAL: f32 = 14.0;
const BOT_MEET_DISTANCE: f32 = 58.0;
const BOT_MEET_OFFSET: f32 = 11.0;
const BOT_BUSY_RETARGET_INTERVAL: f32 = 3.5;
const BOT_GREETER_INTERVAL: f32 = 4.0;
const BOT_PLAYER_EDIT_RADIUS: f32 = 14.0;
const BOT_PLAYER_PROJECT_MARGIN: f32 = 128.0;
const BOT_SHIP_EDIT_RADIUS: f32 = 14.0;
const BOT_SHIP_PROJECT_MARGIN: f32 = 32.0;
const BOT_MAX_FRAME_EDITS: usize = 420;
const BOT_MAX_PROJECT_SLICE_EDITS: usize = 160;
const COMPANION_FOLLOW_DEFAULT: f32 = 3.2;
const COMPANION_FOLLOW_MIN: f32 = 1.25;
const COMPANION_FOLLOW_MAX: f32 = 22.0;
const COMPANION_FOLLOW_STEP: f32 = 2.25;

#[cfg(not(target_arch = "wasm32"))]
static BOT_SAVE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn bot_save_version() -> u32 {
    2
}

fn default_next_id() -> u64 {
    1
}

fn default_city_radius() -> i32 {
    MEGA_CITY_RADIUS
}

fn default_max_active_projects() -> usize {
    DEFAULT_MAX_ACTIVE_PROJECTS
}

fn default_autonomy_enabled() -> bool {
    true
}

fn default_bots_active() -> bool {
    true
}

fn default_autonomy_intensity() -> u8 {
    9
}

fn default_trust() -> f32 {
    0.55
}

fn default_curiosity() -> f32 {
    0.65
}

fn default_work_focus() -> f32 {
    0.70
}

fn default_companion_follow_distance() -> f32 {
    COMPANION_FOLLOW_DEFAULT
}

fn default_project_concept() -> BotProjectConcept {
    BotProjectConcept::default()
}

pub struct BotsPlugin;

impl Plugin for BotsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FriendlyWorldBrain::default())
            .insert_resource(BotVisualCache::default())
            .add_systems(OnEnter(GameState::InGame), load_or_seed_bot_world)
            .add_systems(OnEnter(GameState::MainMenu), cleanup_bot_entities)
            .add_systems(
                Update,
                (
                    spawn_missing_bot_entities,
                    tick_friendly_world,
                    process_bot_visit_request,
                    process_companion_command,
                    draw_companion_preview_gizmos,
                    sync_bot_visuals,
                    animate_worker_bots,
                    manual_save_bot_world,
                    autosave_bot_world,
                )
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            )
            .add_systems(
                Update,
                draw_companion_quick_dock
                    .after(EguiSet::InitContexts)
                    .run_if(in_state(GameState::InGame)),
            )
            .add_systems(Last, save_bot_world_on_exit);
    }
}

#[derive(Resource, Debug, Clone)]
pub struct FriendlyWorldBrain {
    pub save: BotWorldSave,
    pub selected_bot: u64,
    pub selected_district: u64,
    pub command_draft: BotTaskCommand,
    pub queued_commands: Vec<BotTaskCommand>,
    pub visit_request: Option<BotVisitTarget>,
    pub companion_command: Option<CompanionCommand>,
    pub hud_message: String,
    pub autosave_timer: f32,
    pub force_city_idea: bool,
    message_cooldown: f32,
    conversation_timer: f32,
    greeter_timer: f32,
    busy_timer: f32,
    plan_timer: f32,
    project_scan_cursor: usize,
    world_name: String,
    dirty: bool,
}

impl Default for FriendlyWorldBrain {
    fn default() -> Self {
        Self {
            save: BotWorldSave::default(),
            selected_bot: 0,
            selected_district: 1,
            command_draft: BotTaskCommand::default(),
            queued_commands: Vec::new(),
            visit_request: None,
            companion_command: None,
            hud_message: "BOT CITY // waiting for world".into(),
            autosave_timer: 30.0,
            force_city_idea: false,
            message_cooldown: 0.0,
            conversation_timer: 5.0,
            greeter_timer: 1.0,
            busy_timer: 1.0,
            plan_timer: 2.0,
            project_scan_cursor: 0,
            world_name: String::new(),
            dirty: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotVisitTarget {
    CityHub,
    ActiveBuild,
    NearestBot,
    SelectedBot(u64),
    SelectedDistrict(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionCommand {
    PlaceBothNearPlayer,
    PlaceSelectedNearPlayer,
    FollowBoth,
    FollowSelected,
    CloserBoth,
    CloserSelected,
    FartherBoth,
    FartherSelected,
    HoldBoth,
    HoldSelected,
    ScanBoth,
    ScanSelected,
    PatrolBoth,
    PatrolSelected,
    SurveyBoth,
    SurveySelected,
    MarkWaypointSelected,
    PreviewAssist(CompanionAssistKind),
    ExecutePreview,
    ClearPreview,
    BuildCityAutonomy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompanionAssistKind {
    Road,
    LandingPad,
    Lights,
    ClearFlatten,
    Recolor,
    Repair,
    Beautify,
    TargetRange,
}

impl CompanionAssistKind {
    pub const ALL: [Self; 8] = [
        Self::Road,
        Self::LandingPad,
        Self::Lights,
        Self::ClearFlatten,
        Self::Recolor,
        Self::Repair,
        Self::Beautify,
        Self::TargetRange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Road => "Road",
            Self::LandingPad => "Landing Pad",
            Self::Lights => "Lights",
            Self::ClearFlatten => "Clear / Flatten",
            Self::Recolor => "Recolor",
            Self::Repair => "Repair",
            Self::Beautify => "Beautify",
            Self::TargetRange => "Target Range",
        }
    }

    fn command(self) -> BotTaskCommand {
        match self {
            Self::Road => BotTaskCommand {
                task_type: BotTaskKind::BuildRoad,
                theme: BotTheme::CyanAlloy,
                width: 7,
                height: 1,
                priority: 8,
                ..default()
            },
            Self::LandingPad => BotTaskCommand {
                task_type: BotTaskKind::LandingPad,
                theme: BotTheme::WhiteAlloy,
                width: 25,
                height: 1,
                priority: 9,
                ..default()
            },
            Self::Lights => BotTaskCommand {
                task_type: BotTaskKind::AddLights,
                theme: BotTheme::AmberStreet,
                width: 7,
                height: 7,
                priority: 7,
                ..default()
            },
            Self::ClearFlatten => BotTaskCommand {
                task_type: BotTaskKind::ClearFlatten,
                theme: BotTheme::CyanAlloy,
                width: 14,
                height: 6,
                priority: 9,
                ..default()
            },
            Self::Recolor => BotTaskCommand {
                task_type: BotTaskKind::RecolorRoad,
                theme: BotTheme::MagentaGlass,
                width: 7,
                height: 1,
                priority: 6,
                ..default()
            },
            Self::Repair => BotTaskCommand {
                task_type: BotTaskKind::UpgradeDistrict,
                theme: BotTheme::WhiteAlloy,
                width: 16,
                height: 8,
                priority: 8,
                ..default()
            },
            Self::Beautify => BotTaskCommand {
                task_type: BotTaskKind::BuildPlaza,
                theme: BotTheme::GreenPark,
                width: 18,
                height: 8,
                priority: 7,
                ..default()
            },
            Self::TargetRange => BotTaskCommand {
                task_type: BotTaskKind::TargetRange,
                theme: BotTheme::AmberStreet,
                width: 18,
                height: 7,
                priority: 7,
                ..default()
            },
        }
    }
}

impl FriendlyWorldBrain {
    pub fn cockpit_line(&self) -> String {
        let active = self
            .save
            .projects
            .iter()
            .filter(|p| !p.status.is_done())
            .count();
        format!(
            "COMPANIONS {:02} online // {} instructed builds // R{} used {:.0} // {}",
            self.save.agents.len(),
            active,
            self.save.primary_bounds().radius,
            self.save.primary_bounds().used_radius,
            self.hud_message
        )
    }

    pub fn navigation_dest(&self) -> (&'static str, Vec3) {
        if let Some(project) = self
            .save
            .projects
            .iter()
            .filter(|p| !p.status.is_done())
            .max_by_key(|p| p.priority)
        {
            return (
                "COMPANION BUILD",
                project_center(project.origin, project.size),
            );
        }
        let pos = self
            .save
            .settlements
            .first()
            .map(|s| vec3_from_arr(s.hub))
            .unwrap_or(Vec3::ZERO);
        ("COMPANION HUB", pos)
    }

    pub fn bot_count(&self) -> usize {
        self.save.agents.len()
    }

    pub fn active_project_count(&self) -> usize {
        self.save
            .projects
            .iter()
            .filter(|p| !p.status.is_done())
            .count()
    }

    pub fn nearest_bot_line(&self, player_pos: Vec3) -> String {
        let Some(bot) = self.save.agents.iter().min_by(|a, b| {
            let da = vec3_from_arr(a.position).distance_squared(player_pos);
            let db = vec3_from_arr(b.position).distance_squared(player_pos);
            da.total_cmp(&db)
        }) else {
            return "No friendly bots online yet".into();
        };
        let bot_dist = vec3_from_arr(bot.position).distance(player_pos);
        let (nav_label, nav_pos) = self.navigation_dest();
        let nav_dist = nav_pos.distance(player_pos);
        format!(
            "Nearest companion {} {:.0}m // {} {:.0}m // F3 -> BOTS",
            bot.name, bot_dist, nav_label, nav_dist
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotWorldSave {
    #[serde(default = "bot_save_version")]
    pub version: u32,
    #[serde(default = "default_next_id")]
    pub next_bot_id: u64,
    #[serde(default = "default_next_id")]
    pub next_project_id: u64,
    #[serde(default = "default_next_id")]
    pub next_district_id: u64,
    #[serde(default = "default_next_id")]
    pub next_idea_id: u64,
    #[serde(default = "default_next_id")]
    pub next_conversation_id: u64,
    #[serde(default = "default_next_id")]
    pub next_crew_id: u64,
    #[serde(default)]
    pub settlements: Vec<BotSettlement>,
    #[serde(default)]
    pub agents: Vec<BotAgent>,
    #[serde(default)]
    pub projects: Vec<BotProject>,
    #[serde(default)]
    pub districts: Vec<BotDistrict>,
    #[serde(default)]
    pub user_roads: Vec<BotRoadGuide>,
    #[serde(default)]
    pub ideas: Vec<BotIdea>,
    #[serde(default)]
    pub conversations: Vec<BotConversation>,
    #[serde(default)]
    pub crews: Vec<BotCrew>,
    #[serde(default)]
    pub journal: Vec<BotJournalEntry>,
    #[serde(default)]
    pub completed_projects: u32,
    #[serde(default)]
    pub autonomy: BotAutonomySettings,
    #[serde(default)]
    pub last_blocked_reason: String,
    #[serde(default)]
    pub companion_preview: Option<CompanionBuildPreview>,
}

impl Default for BotWorldSave {
    fn default() -> Self {
        Self {
            version: bot_save_version(),
            next_bot_id: 1,
            next_project_id: 1,
            next_district_id: 1,
            next_idea_id: 1,
            next_conversation_id: 1,
            next_crew_id: 1,
            settlements: Vec::new(),
            agents: Vec::new(),
            projects: Vec::new(),
            districts: Vec::new(),
            user_roads: Vec::new(),
            ideas: Vec::new(),
            conversations: Vec::new(),
            crews: Vec::new(),
            journal: Vec::new(),
            completed_projects: 0,
            autonomy: BotAutonomySettings::default(),
            last_blocked_reason: String::new(),
            companion_preview: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRoadGuide {
    #[serde(default)]
    pub district_id: Option<u64>,
    #[serde(default)]
    pub points: Vec<[i32; 3]>,
    #[serde(default)]
    pub anchor: Option<[i32; 3]>,
    #[serde(default)]
    pub width: u8,
    #[serde(default)]
    pub theme: BotTheme,
    #[serde(default)]
    pub shape: BotRoadGuideShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BotRoadGuideShape {
    #[default]
    Straight,
    Corner,
    Roundabout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanionBuildPreview {
    pub id: u64,
    pub author_id: Option<u64>,
    pub assist: CompanionAssistKind,
    pub kind: BotTaskKind,
    pub origin: [i32; 3],
    pub size: [i32; 3],
    pub theme: BotTheme,
    pub priority: u8,
    pub status: CompanionPreviewStatus,
    pub message: String,
    #[serde(default)]
    pub created_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompanionPreviewStatus {
    Valid,
    Blocked,
}

impl CompanionPreviewStatus {
    pub fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl BotWorldSave {
    pub fn is_empty(&self) -> bool {
        self.settlements.is_empty() && self.agents.is_empty() && self.projects.is_empty()
    }

    fn primary_bounds(&self) -> BotCityBounds {
        self.settlements
            .first()
            .map(|s| s.bounds)
            .unwrap_or_default()
    }

    fn normalize(&mut self) {
        let legacy_version = self.version;
        self.version = bot_save_version();
        self.next_bot_id = self.next_bot_id.max(1);
        self.next_project_id = self.next_project_id.max(1);
        self.next_district_id = self.next_district_id.max(1);
        self.next_idea_id = self.next_idea_id.max(1);
        self.next_conversation_id = self.next_conversation_id.max(1);
        self.next_crew_id = self.next_crew_id.max(1);
        if legacy_version < 2 {
            self.autonomy.enabled = false;
        }

        for settlement in &mut self.settlements {
            settlement.radius = MEGA_CITY_RADIUS;
            settlement.bounds.center = settlement.hub;
            settlement.bounds.radius = MEGA_CITY_RADIUS;
            settlement.bounds.max_active_projects = settlement
                .bounds
                .max_active_projects
                .clamp(1, MAX_ACTIVE_PROJECTS_LIMIT);
        }
        ensure_city_districts(self);
        if legacy_version < 2 {
            normalize_legacy_companions(self);
        } else {
            normalize_companion_swarm(self);
        }
        self.next_bot_id = self
            .next_bot_id
            .max(self.agents.iter().map(|b| b.id).max().unwrap_or(0) + 1);
        if legacy_version >= 2 {
            ensure_companion_worker_swarms(self);
        }
        restore_project_assignments(self);
        normalize_relationships(self);
        self.user_roads.retain(|road| road.points.len() >= 2);

        self.next_bot_id = self
            .next_bot_id
            .max(self.agents.iter().map(|b| b.id).max().unwrap_or(0) + 1);
        self.next_project_id = self
            .next_project_id
            .max(self.projects.iter().map(|p| p.id).max().unwrap_or(0) + 1);
        self.next_district_id = self
            .next_district_id
            .max(self.districts.iter().map(|d| d.id).max().unwrap_or(0) + 1);
        self.next_idea_id = self
            .next_idea_id
            .max(self.ideas.iter().map(|i| i.id).max().unwrap_or(0) + 1);
        self.next_conversation_id = self
            .next_conversation_id
            .max(self.conversations.iter().map(|c| c.id).max().unwrap_or(0) + 1);
        self.next_crew_id = self
            .next_crew_id
            .max(self.crews.iter().map(|c| c.id).max().unwrap_or(0) + 1);

        self.ideas.truncate(96);
        self.conversations.truncate(96);
        self.journal.truncate(128);
    }

    fn seed(world_name: &str, hub: Vec3, world: &VoxelWorld) -> Self {
        let mut save = Self::default();
        let hub_x = hub.x.round() as i32;
        let hub_z = hub.z.round() as i32;
        let hub_y = world.surface_height_at(hub_x, hub_z) + 2;
        let hub = [hub_x as f32 + 0.5, hub_y as f32, hub_z as f32 + 0.5];
        save.settlements.push(BotSettlement {
            id: 1,
            name: format!("{} Bot City", world_name),
            hub,
            radius: MEGA_CITY_RADIUS,
            bounds: BotCityBounds {
                center: hub,
                radius: MEGA_CITY_RADIUS,
                used_radius: 0.0,
                max_active_projects: DEFAULT_MAX_ACTIVE_PROJECTS,
            },
            theme: BotTheme::CyanAlloy,
            road_count: 0,
            building_count: 0,
            park_count: 0,
        });
        for (name, role, offset, order) in [
            (
                "Iris",
                BotRole::CompanionGuide,
                Vec3::new(-3.0, 1.5, -5.0),
                0,
            ),
            (
                "Orion",
                BotRole::CompanionMaker,
                Vec3::new(3.0, 1.5, -5.0),
                1,
            ),
        ] {
            let id = save.next_bot_id;
            save.next_bot_id += 1;
            let p = vec3_from_arr(hub) + offset;
            save.agents.push(BotAgent {
                id,
                name: name.into(),
                role,
                state: BotState::Idle,
                position: [p.x, p.y, p.z],
                target: [p.x, p.y, p.z],
                home_id: 1,
                crew_id: None,
                last_interaction_epoch: 0,
                companion: true,
                companion_order: order,
                swarm_leader_id: None,
                swarm_index: 0,
                companion_mode: BotCompanionMode::AwaitingInstruction,
                current_task: None,
                memory: BotMemory {
                    completed_tasks: 0,
                    last_message: "Awaiting your instruction.".into(),
                    known_sites: vec![hub],
                    favorite_theme: role.default_theme(),
                    ..Default::default()
                },
            });
        }
        save.autonomy.enabled = false;
        ensure_city_districts(&mut save);
        normalize_relationships(&mut save);
        save.journal.push(BotJournalEntry::new(format!(
            "Companions online at {:.0}/{:.0}/{:.0}; waiting for instructions.",
            hub[0], hub[1], hub[2]
        )));
        save
    }
}

fn normalize_legacy_companions(save: &mut BotWorldSave) {
    let hub = save
        .settlements
        .first()
        .map(|s| vec3_from_arr(s.hub))
        .unwrap_or(Vec3::new(0.0, 120.0, 0.0));
    save.agents.clear();
    for (name, role, offset, order) in [
        (
            "Iris",
            BotRole::CompanionGuide,
            Vec3::new(-3.0, 1.5, -5.0),
            0_u8,
        ),
        (
            "Orion",
            BotRole::CompanionMaker,
            Vec3::new(3.0, 1.5, -5.0),
            1_u8,
        ),
    ] {
        let id = save.next_bot_id;
        save.next_bot_id += 1;
        let p = hub + offset;
        save.agents.push(BotAgent {
            id,
            name: name.into(),
            role,
            state: BotState::Idle,
            position: [p.x, p.y, p.z],
            target: [p.x, p.y, p.z],
            home_id: save.settlements.first().map(|s| s.id).unwrap_or(1),
            crew_id: None,
            last_interaction_epoch: 0,
            companion: true,
            companion_order: order,
            swarm_leader_id: None,
            swarm_index: 0,
            companion_mode: BotCompanionMode::AwaitingInstruction,
            current_task: None,
            memory: BotMemory {
                completed_tasks: 0,
                last_message: "Legacy companion restored; awaiting your instruction.".into(),
                known_sites: vec![[hub.x, hub.y, hub.z]],
                favorite_theme: role.default_theme(),
                ..Default::default()
            },
        });
    }
}

fn ensure_city_districts(save: &mut BotWorldSave) {
    let Some(settlement) = save.settlements.first() else {
        return;
    };
    let hub = vec3_from_arr(settlement.hub);
    let bounds = settlement.bounds;
    if !save.districts.is_empty() {
        return;
    }

    let specs = [
        (BotDistrictKind::HubCore, "Hub Core", Vec3::ZERO, 84),
        (
            BotDistrictKind::Residential,
            "Habitat Ring",
            Vec3::new(150.0, 0.0, 40.0),
            92,
        ),
        (
            BotDistrictKind::Skyline,
            "Glass Skyline",
            Vec3::new(-80.0, 0.0, 180.0),
            110,
        ),
        (
            BotDistrictKind::Park,
            "Green Commons",
            Vec3::new(-170.0, 0.0, -45.0),
            100,
        ),
        (
            BotDistrictKind::Service,
            "Shuttle Yard",
            Vec3::new(40.0, 0.0, -190.0),
            112,
        ),
        (
            BotDistrictKind::Training,
            "Target Basin",
            Vec3::new(230.0, 0.0, -120.0),
            110,
        ),
        (
            BotDistrictKind::Scenic,
            "Lookout Rim",
            Vec3::new(-260.0, 0.0, 160.0),
            120,
        ),
    ];

    for (kind, name, offset, radius) in specs {
        let center = clamp_to_bounds(bounds, hub + offset);
        let cx = center.x.round() as i32;
        let cy = center.y.round() as i32;
        let cz = center.z.round() as i32;
        let id = save.next_district_id;
        save.next_district_id += 1;
        save.districts.push(BotDistrict {
            id,
            kind,
            name: name.into(),
            center: [center.x, center.y, center.z],
            radius,
            road_anchors: vec![
                [
                    hub.x.round() as i32,
                    hub.y.round() as i32,
                    hub.z.round() as i32,
                ],
                [cx, cy, cz],
            ],
            build_slots: district_build_slots(cx, cy, cz, radius),
            completed_projects: 0,
        });
    }
}

fn district_build_slots(cx: i32, cy: i32, cz: i32, radius: i32) -> Vec<[i32; 3]> {
    let r = radius.max(32);
    [
        [cx + r / 3, cy, cz],
        [cx - r / 3, cy, cz],
        [cx, cy, cz + r / 3],
        [cx, cy, cz - r / 3],
        [cx + r / 4, cy, cz + r / 4],
        [cx - r / 4, cy, cz + r / 4],
        [cx + r / 4, cy, cz - r / 4],
        [cx - r / 4, cy, cz - r / 4],
    ]
    .to_vec()
}

fn normalize_relationships(save: &mut BotWorldSave) {
    let ids: Vec<u64> = save.agents.iter().map(|b| b.id).collect();
    for bot in &mut save.agents {
        bot.memory.fatigue = bot.memory.fatigue.clamp(0.0, 1.0);
        bot.memory.curiosity = bot.memory.curiosity.clamp(0.0, 1.0);
        bot.memory.work_focus = bot.memory.work_focus.clamp(0.0, 1.0);
        for other_id in &ids {
            if *other_id == bot.id {
                continue;
            }
            if bot
                .memory
                .relationships
                .iter()
                .all(|r| r.other_id != *other_id)
            {
                bot.memory.relationships.push(BotRelationship {
                    other_id: *other_id,
                    trust: default_trust(),
                    collaboration_score: 0.0,
                    last_interaction_epoch: 0,
                });
            }
        }
        bot.memory
            .relationships
            .retain(|r| ids.contains(&r.other_id));
        bot.memory.recent_conversation_keys.truncate(16);
    }
}

fn normalize_companion_swarm(save: &mut BotWorldSave) {
    let hub = save
        .settlements
        .first()
        .map(|s| vec3_from_arr(s.hub))
        .unwrap_or(Vec3::new(0.0, 120.0, 0.0));
    let existing = std::mem::take(&mut save.agents);
    let specs = [
        (
            "Emma",
            BotRole::CompanionGuide,
            Vec3::new(-3.0, 1.5, -4.0),
            0_u8,
            "Team lead online. I can coordinate, follow close, and translate plans.",
        ),
        (
            "David",
            BotRole::CompanionMaker,
            Vec3::new(3.0, 1.5, -4.0),
            1_u8,
            "Build chief online. I can turn plans into streets, towers, and repairs.",
        ),
        (
            "Sofia",
            BotRole::Architect,
            Vec3::new(-5.5, 1.7, -7.5),
            2_u8,
            "Architect online. I watch skyline rhythm, facades, setbacks, and plazas.",
        ),
        (
            "Mona",
            BotRole::Planner,
            Vec3::new(5.5, 1.7, -7.5),
            3_u8,
            "Planner online. I maintain the build spreadsheet and city priorities.",
        ),
        (
            "Kai",
            BotRole::RoadCrew,
            Vec3::new(-8.0, 1.6, -2.0),
            4_u8,
            "Road crew online. I connect blocks before buildings sprawl.",
        ),
        (
            "Lina",
            BotRole::Surveyor,
            Vec3::new(8.0, 1.6, -2.0),
            5_u8,
            "Surveyor online. I scan terrain, slopes, loaded chunks, and build risk.",
        ),
        (
            "Iris",
            BotRole::CompanionGuide,
            Vec3::new(-10.0, 1.8, 5.0),
            6_u8,
            "Guide online. I keep close formation and route you through active builds.",
        ),
        (
            "Orion",
            BotRole::CompanionMaker,
            Vec3::new(10.0, 1.8, 5.0),
            7_u8,
            "Maker online. I assist heavy builds and emergency repairs.",
        ),
        (
            "Noah",
            BotRole::RepairTech,
            Vec3::new(-6.5, 1.8, 3.0),
            8_u8,
            "Systems tech online. I handle lights, utilities, and maintenance details.",
        ),
        (
            "Ava",
            BotRole::ParkKeeper,
            Vec3::new(6.5, 1.8, 3.0),
            9_u8,
            "Landscape lead online. I keep the city breathable with parks and waterfronts.",
        ),
    ];
    let core_names: HashSet<&'static str> = specs.iter().map(|(name, _, _, _, _)| *name).collect();

    for (name, role, offset, order, message) in specs {
        let mut bot = existing
            .iter()
            .find(|b| b.name == name)
            .cloned()
            .unwrap_or_else(|| {
                let id = save.next_bot_id;
                save.next_bot_id += 1;
                let p = hub + offset;
                BotAgent {
                    id,
                    name: name.into(),
                    role,
                    state: BotState::Idle,
                    position: [p.x, p.y, p.z],
                    target: [p.x, p.y, p.z],
                    home_id: save.settlements.first().map(|s| s.id).unwrap_or(1),
                    crew_id: None,
                    last_interaction_epoch: 0,
                    companion: true,
                    companion_order: order,
                    swarm_leader_id: None,
                    swarm_index: 0,
                    companion_mode: BotCompanionMode::AwaitingInstruction,
                    current_task: None,
                    memory: BotMemory {
                        last_message: message.into(),
                        known_sites: vec![[hub.x, hub.y, hub.z]],
                        favorite_theme: role.default_theme(),
                        ..Default::default()
                    },
                }
            });

        if !bot.companion {
            let p = hub + offset;
            bot.position = [p.x, p.y, p.z];
            bot.target = [p.x, p.y, p.z];
            bot.companion_mode = BotCompanionMode::AwaitingInstruction;
        }
        bot.name = name.into();
        bot.role = role;
        bot.home_id = save.settlements.first().map(|s| s.id).unwrap_or(1);
        bot.companion = true;
        bot.companion_order = order;
        bot.swarm_leader_id = None;
        bot.swarm_index = 0;
        bot.crew_id = None;
        bot.current_task = None;
        bot.state = BotState::Idle;
        bot.memory.preferred_follow_distance = bot
            .memory
            .preferred_follow_distance
            .clamp(COMPANION_FOLLOW_MIN, COMPANION_FOLLOW_MAX);
        if bot.memory.last_message.is_empty() {
            bot.memory.last_message = message.into();
        }
        bot.memory.favorite_theme = role.default_theme();
        save.agents.push(bot);
    }

    let mut existing_ids: HashSet<u64> = save.agents.iter().map(|bot| bot.id).collect();
    for mut bot in existing {
        if core_names.contains(bot.name.as_str()) || existing_ids.contains(&bot.id) {
            continue;
        }
        bot.memory.preferred_follow_distance = bot
            .memory
            .preferred_follow_distance
            .clamp(COMPANION_FOLLOW_MIN, COMPANION_FOLLOW_MAX);
        existing_ids.insert(bot.id);
        save.agents.push(bot);
    }
}

fn ensure_companion_worker_swarms(save: &mut BotWorldSave) {
    let leaders: Vec<(u64, String, u8, [f32; 3], u64)> = save
        .agents
        .iter()
        .filter(|bot| bot.companion)
        .map(|bot| {
            (
                bot.id,
                bot.name.clone(),
                bot.companion_order,
                bot.position,
                bot.home_id,
            )
        })
        .collect();
    let roles = [
        BotRole::Surveyor,
        BotRole::Builder,
        BotRole::RoadCrew,
        BotRole::RepairTech,
    ];

    for (leader_id, leader_name, order, leader_pos, home_id) in leaders {
        for index in 1..=COMPANION_WORKERS_PER_LEADER {
            let helper_name = format!("{leader_name} Swarm {index}");
            let existing_idx = save.agents.iter().position(|bot| {
                (bot.swarm_leader_id == Some(leader_id) && bot.swarm_index == index)
                    || bot.name == helper_name
            });
            if let Some(existing_idx) = existing_idx {
                let bot = &mut save.agents[existing_idx];
                bot.companion = false;
                bot.companion_order = order;
                bot.swarm_leader_id = Some(leader_id);
                bot.swarm_index = index;
                bot.role = roles[(index as usize - 1) % roles.len()];
                if bot.memory.last_message.is_empty() {
                    bot.memory.last_message = format!("Worker {index} linked to {leader_name}.");
                }
                continue;
            }

            let id = save.next_bot_id;
            save.next_bot_id += 1;
            let role = roles[(index as usize - 1) % roles.len()];
            let base = vec3_from_arr(leader_pos);
            let angle = (order as f32 + index as f32 * 1.7) * 2.399_963_1;
            let radius = 4.0 + index as f32 * 1.6;
            let p = base + Vec3::new(angle.cos() * radius, 0.8, angle.sin() * radius);
            save.agents.push(BotAgent {
                id,
                name: helper_name,
                role,
                state: BotState::Idle,
                position: [p.x, p.y, p.z],
                target: [p.x, p.y, p.z],
                home_id,
                crew_id: None,
                last_interaction_epoch: now_epoch(),
                companion: false,
                companion_order: order,
                swarm_leader_id: Some(leader_id),
                swarm_index: index,
                companion_mode: BotCompanionMode::SurveySweep,
                current_task: None,
                memory: BotMemory {
                    last_message: format!("{leader_name}'s worker {index} ready for field builds."),
                    known_sites: vec![leader_pos],
                    favorite_theme: role.default_theme(),
                    work_focus: 1.0,
                    curiosity: 0.85,
                    ..Default::default()
                },
            });
        }
    }
}

fn restore_project_assignments(save: &mut BotWorldSave) {
    let active_project_ids: HashSet<u64> = save
        .projects
        .iter()
        .filter(|project| !project.status.is_done())
        .map(|project| project.id)
        .collect();
    save.crews
        .retain(|crew| active_project_ids.contains(&crew.project_id));
    let crew_ids: HashSet<u64> = save.crews.iter().map(|crew| crew.id).collect();
    for bot in &mut save.agents {
        if bot.crew_id.is_some_and(|id| !crew_ids.contains(&id)) {
            bot.crew_id = None;
        }
        if bot
            .current_task
            .as_ref()
            .is_some_and(|task| !active_project_ids.contains(&task.project_id))
        {
            bot.current_task = None;
        }
    }

    let project_specs: Vec<_> = save
        .projects
        .iter()
        .enumerate()
        .filter(|(_, project)| !project.status.is_done())
        .map(|(idx, project)| {
            (
                idx,
                project.id,
                project.kind,
                project.assigned_bot,
                project.crew_id,
                project.origin,
                project.label.clone(),
            )
        })
        .collect();

    for (idx, project_id, kind, assigned_bot, crew_id, origin, label) in project_specs {
        let valid_crew = crew_id.filter(|id| save.crews.iter().any(|crew| crew.id == *id));
        let crew_id = valid_crew
            .or_else(|| create_project_crew(save, project_id, kind, assigned_bot, origin));
        if let Some(project) = save.projects.get_mut(idx) {
            project.crew_id = crew_id;
        }
        assign_crew_task(
            save,
            crew_id,
            assigned_bot,
            project_id,
            kind,
            &label,
            origin,
        );
    }
}

fn clamp_to_bounds(bounds: BotCityBounds, target: Vec3) -> Vec3 {
    let center = Vec3::new(bounds.center[0], target.y, bounds.center[2]);
    let delta = target - center;
    let flat = Vec2::new(delta.x, delta.z);
    let max_radius = bounds.radius as f32 - 24.0;
    if flat.length() <= max_radius {
        return target;
    }
    let dir = flat.normalize_or_zero() * max_radius;
    Vec3::new(center.x + dir.x, target.y, center.z + dir.y)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotSettlement {
    pub id: u64,
    pub name: String,
    pub hub: [f32; 3],
    #[serde(default = "default_city_radius")]
    pub radius: i32,
    #[serde(default)]
    pub bounds: BotCityBounds,
    pub theme: BotTheme,
    #[serde(default)]
    pub road_count: u32,
    #[serde(default)]
    pub building_count: u32,
    #[serde(default)]
    pub park_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotAgent {
    pub id: u64,
    pub name: String,
    pub role: BotRole,
    pub state: BotState,
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub home_id: u64,
    #[serde(default)]
    pub crew_id: Option<u64>,
    #[serde(default)]
    pub last_interaction_epoch: u64,
    #[serde(default)]
    pub companion: bool,
    #[serde(default)]
    pub companion_order: u8,
    #[serde(default)]
    pub swarm_leader_id: Option<u64>,
    #[serde(default)]
    pub swarm_index: u8,
    #[serde(default)]
    pub companion_mode: BotCompanionMode,
    pub current_task: Option<BotTask>,
    #[serde(default)]
    pub memory: BotMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotCompanionMode {
    AwaitingInstruction,
    FollowingPlayer,
    HoldingPosition,
    ScanningArea,
    PreviewingEdit,
    AssistingTask,
    Blocked,
    Patrolling,
    SurveySweep,
}

impl Default for BotCompanionMode {
    fn default() -> Self {
        Self::AwaitingInstruction
    }
}

impl BotCompanionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::AwaitingInstruction => "Awaiting instruction",
            Self::FollowingPlayer => "Following",
            Self::HoldingPosition => "Holding",
            Self::ScanningArea => "Scanning",
            Self::PreviewingEdit => "Previewing edit",
            Self::AssistingTask => "Assisting task",
            Self::Blocked => "Blocked",
            Self::Patrolling => "Patrolling",
            Self::SurveySweep => "Survey sweep",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotMemory {
    #[serde(default)]
    pub completed_tasks: u32,
    #[serde(default)]
    pub last_message: String,
    #[serde(default)]
    pub known_sites: Vec<[f32; 3]>,
    #[serde(default)]
    pub favorite_theme: BotTheme,
    #[serde(default)]
    pub relationships: Vec<BotRelationship>,
    #[serde(default)]
    pub recent_conversation_keys: Vec<String>,
    #[serde(default)]
    pub fatigue: f32,
    #[serde(default = "default_curiosity")]
    pub curiosity: f32,
    #[serde(default = "default_work_focus")]
    pub work_focus: f32,
    #[serde(default = "default_companion_follow_distance")]
    pub preferred_follow_distance: f32,
    #[serde(default)]
    pub last_idea_epoch: u64,
}

impl Default for BotMemory {
    fn default() -> Self {
        Self {
            completed_tasks: 0,
            last_message: String::new(),
            known_sites: Vec::new(),
            favorite_theme: BotTheme::CyanAlloy,
            relationships: Vec::new(),
            recent_conversation_keys: Vec::new(),
            fatigue: 0.0,
            curiosity: default_curiosity(),
            work_focus: default_work_focus(),
            preferred_follow_distance: default_companion_follow_distance(),
            last_idea_epoch: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotTask {
    pub task_type: BotTaskKind,
    pub project_id: u64,
    pub label: String,
    pub progress: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotProject {
    pub id: u64,
    pub kind: BotTaskKind,
    pub label: String,
    pub origin: [i32; 3],
    pub size: [i32; 3],
    pub theme: BotTheme,
    pub status: BotProjectStatus,
    pub cursor: u32,
    pub total_steps: u32,
    #[serde(default)]
    pub assigned_bot: Option<u64>,
    #[serde(default)]
    pub district_id: Option<u64>,
    #[serde(default)]
    pub crew_id: Option<u64>,
    #[serde(default)]
    pub idea_id: Option<u64>,
    #[serde(default)]
    pub blocked_reason: String,
    #[serde(default)]
    pub priority: u8,
    #[serde(default = "default_project_concept")]
    pub concept: BotProjectConcept,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotProjectConcept {
    #[serde(default)]
    pub brief: String,
    #[serde(default)]
    pub structure: String,
    #[serde(default)]
    pub material_plan: String,
    #[serde(default)]
    pub visual_goal: String,
    #[serde(default)]
    pub rows: Vec<BotPlanRow>,
    #[serde(default)]
    pub street_face: Option<BuildingStreetFace>,
    #[serde(default)]
    pub block_role: Option<CityBlockRole>,
    #[serde(default)]
    pub semantic_anchor_shape: Option<BotRoadGuideShape>,
}

impl Default for BotProjectConcept {
    fn default() -> Self {
        Self {
            brief: String::new(),
            structure: String::new(),
            material_plan: String::new(),
            visual_goal: String::new(),
            rows: Vec::new(),
            street_face: None,
            block_role: None,
            semantic_anchor_shape: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CityBlockRole {
    CornerLandmark,
    MidblockStreetWall,
    ResidentialCorner,
    CivicEdge,
    ServiceEdge,
}

impl CityBlockRole {
    fn label(self) -> &'static str {
        match self {
            Self::CornerLandmark => "corner landmark at a road intersection",
            Self::MidblockStreetWall => "midblock street wall with calmer frontage",
            Self::ResidentialCorner => "residential corner with visible side entry",
            Self::CivicEdge => "civic edge that opens to intersecting streets",
            Self::ServiceEdge => "service edge with clear utility access",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildingStreetFace {
    North,
    South,
    West,
    East,
}

impl BuildingStreetFace {
    fn label(self) -> &'static str {
        match self {
            Self::North => "north/min-z street face",
            Self::South => "south/max-z street face",
            Self::West => "west/min-x street face",
            Self::East => "east/max-x street face",
        }
    }

    fn contains_centered_entrance(self, local: IVec3, sx: i32, sz: i32, setback: i32) -> bool {
        match self {
            Self::North => local.z == setback && (local.x - sx / 2).abs() <= 2,
            Self::South => local.z == sz - setback && (local.x - sx / 2).abs() <= 2,
            Self::West => local.x == setback && (local.z - sz / 2).abs() <= 2,
            Self::East => local.x == sx - setback && (local.z - sz / 2).abs() <= 2,
        }
    }

    fn lobby_detail_cell(self, local: IVec3, sx: i32, sz: i32, setback: i32) -> bool {
        match self {
            Self::North => local.z == setback + 2 && (local.x - sx / 2).abs() <= 2,
            Self::South => local.z == sz - setback - 2 && (local.x - sx / 2).abs() <= 2,
            Self::West => local.x == setback + 2 && (local.z - sz / 2).abs() <= 2,
            Self::East => local.x == sx - setback - 2 && (local.z - sz / 2).abs() <= 2,
        }
    }

    fn raised_access_deck_cell(self, local: IVec3, sx: i32, sz: i32, setback: i32) -> bool {
        match self {
            Self::North => {
                local.z >= setback && local.z <= setback + 4 && (local.x - sx / 2).abs() <= 2
            }
            Self::South => {
                local.z <= sz - setback
                    && local.z >= sz - setback - 4
                    && (local.x - sx / 2).abs() <= 2
            }
            Self::West => {
                local.x >= setback && local.x <= setback + 4 && (local.z - sz / 2).abs() <= 2
            }
            Self::East => {
                local.x <= sx - setback
                    && local.x >= sx - setback - 4
                    && (local.z - sz / 2).abs() <= 2
            }
        }
    }

    fn skyline_corner_marker_cell(self, local: IVec3, sx: i32, sz: i32, setback: i32) -> bool {
        let near_min_x = (local.x - (setback + 1)).abs() <= 1;
        let near_max_x = (local.x - (sx - setback - 1)).abs() <= 1;
        let near_min_z = (local.z - (setback + 1)).abs() <= 1;
        let near_max_z = (local.z - (sz - setback - 1)).abs() <= 1;
        match self {
            Self::North => local.z == setback && (near_min_x || near_max_x),
            Self::South => local.z == sz - setback && (near_min_x || near_max_x),
            Self::West => local.x == setback && (near_min_z || near_max_z),
            Self::East => local.x == sx - setback && (near_min_z || near_max_z),
        }
    }

    fn residential_entry_cell(self, lx: i32, lz: i32, style: i32) -> bool {
        let entry_x = lx == 3 || lx == 4 || (style == 1 && lx == 5);
        let entry_z = lz == 3 || lz == 4 || (style == 1 && lz == 5);
        match self {
            Self::North => lz == 0 && entry_x,
            Self::South => lz == 7 && entry_x,
            Self::West => lx == 0 && entry_z,
            Self::East => lx == 8 && entry_z,
        }
    }

    fn residential_frontage_walk_cell(self, lx: i32, lz: i32) -> bool {
        match self {
            Self::North => lz == 0 && (1..=7).contains(&lx),
            Self::South => lz == 8 && (1..=7).contains(&lx),
            Self::West => lx == 0 && (1..=6).contains(&lz),
            Self::East => lx == 9 && (1..=6).contains(&lz),
        }
    }

    fn residential_stoop_cell(self, lx: i32, lz: i32, style: i32) -> bool {
        let entry_x = lx == 3 || lx == 4 || (style == 1 && lx == 5);
        let entry_z = lz == 3 || lz == 4 || (style == 1 && lz == 5);
        match self {
            Self::North => lz == 0 && entry_x,
            Self::South => lz == 8 && entry_x,
            Self::West => lx == 0 && entry_z,
            Self::East => lx == 9 && entry_z,
        }
    }

    fn residential_balcony_cell(self, lx: i32, lz: i32, style: i32) -> bool {
        if !matches!(style, 2 | 4) {
            return false;
        }
        match self {
            Self::North => lz == 0 && (2..=6).contains(&lx),
            Self::South => lz == 7 && (2..=6).contains(&lx),
            Self::West => lx == 0 && (2..=5).contains(&lz),
            Self::East => lx == 8 && (2..=5).contains(&lz),
        }
    }

    fn residential_corner_bay_cell(self, cell_x: i32, cell_z: i32, lx: i32, lz: i32) -> bool {
        let north_corner = cell_z == 0 && lz <= 2;
        let south_corner = cell_z == 2 && lz >= 5;
        let west_corner = cell_x == 0 && lx <= 2;
        let east_corner = cell_x == 2 && lx >= 6;
        match self {
            Self::North => lz == 0 && (west_corner || east_corner),
            Self::South => lz == 7 && (west_corner || east_corner),
            Self::West => lx == 0 && (north_corner || south_corner),
            Self::East => lx == 8 && (north_corner || south_corner),
        }
    }

    fn civic_gateway_surface_cell(self, local: IVec3, sx: i32, sz: i32) -> bool {
        let center_x = (sx + 1) / 2;
        let center_z = (sz + 1) / 2;
        match self {
            Self::North => local.z == 0 && (local.x - center_x).abs() <= 4,
            Self::South => local.z == sz && (local.x - center_x).abs() <= 4,
            Self::West => local.x == 0 && (local.z - center_z).abs() <= 4,
            Self::East => local.x == sx && (local.z - center_z).abs() <= 4,
        }
    }

    fn civic_gateway_marker_cell(self, local: IVec3, sx: i32, sz: i32) -> bool {
        let center_x = (sx + 1) / 2;
        let center_z = (sz + 1) / 2;
        match self {
            Self::North => local.z == 0 && (local.x == center_x - 3 || local.x == center_x + 3),
            Self::South => local.z == sz && (local.x == center_x - 3 || local.x == center_x + 3),
            Self::West => local.x == 0 && (local.z == center_z - 3 || local.z == center_z + 3),
            Self::East => local.x == sx && (local.z == center_z - 3 || local.z == center_z + 3),
        }
    }

    fn shuttle_approach_surface_cell(self, local: IVec3, sx: i32, sz: i32) -> bool {
        let center_x = (sx + 1) / 2;
        let center_z = (sz + 1) / 2;
        match self {
            Self::North => local.z <= 4 && (local.x - center_x).abs() <= 2,
            Self::South => local.z >= sz - 4 && (local.x - center_x).abs() <= 2,
            Self::West => local.x <= 4 && (local.z - center_z).abs() <= 2,
            Self::East => local.x >= sx - 4 && (local.z - center_z).abs() <= 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotPlanRow {
    pub phase: String,
    pub owner: String,
    pub material: String,
    pub detail: String,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BotCityBounds {
    pub center: [f32; 3],
    #[serde(default = "default_city_radius")]
    pub radius: i32,
    #[serde(default)]
    pub used_radius: f32,
    #[serde(default = "default_max_active_projects")]
    pub max_active_projects: usize,
}

impl Default for BotCityBounds {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0, 0.0],
            radius: MEGA_CITY_RADIUS,
            used_radius: 0.0,
            max_active_projects: DEFAULT_MAX_ACTIVE_PROJECTS,
        }
    }
}

impl BotCityBounds {
    fn contains_xz(self, x: f32, z: f32) -> bool {
        let dx = x - self.center[0];
        let dz = z - self.center[2];
        dx * dx + dz * dz <= (self.radius as f32).powi(2)
    }

    fn contains_block(self, pos: IVec3) -> bool {
        self.contains_xz(pos.x as f32 + 0.5, pos.z as f32 + 0.5)
    }

    fn contains_box(self, origin: [i32; 3], size: [i32; 3]) -> bool {
        let max_x = origin[0] + size[0].max(1) - 1;
        let max_z = origin[2] + size[2].max(1) - 1;
        [
            (origin[0] as f32, origin[2] as f32),
            (max_x as f32, origin[2] as f32),
            (origin[0] as f32, max_z as f32),
            (max_x as f32, max_z as f32),
        ]
        .into_iter()
        .all(|(x, z)| self.contains_xz(x, z))
    }

    fn distance_from_center(self, x: f32, z: f32) -> f32 {
        Vec2::new(x - self.center[0], z - self.center[2]).length()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotDistrict {
    pub id: u64,
    pub kind: BotDistrictKind,
    pub name: String,
    pub center: [f32; 3],
    pub radius: i32,
    #[serde(default)]
    pub road_anchors: Vec<[i32; 3]>,
    #[serde(default)]
    pub build_slots: Vec<[i32; 3]>,
    #[serde(default)]
    pub completed_projects: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotDistrictKind {
    HubCore,
    Residential,
    Skyline,
    Park,
    Service,
    Training,
    Scenic,
}

impl Default for BotDistrictKind {
    fn default() -> Self {
        Self::HubCore
    }
}

impl BotDistrictKind {
    fn label(self) -> &'static str {
        match self {
            Self::HubCore => "Hub Core",
            Self::Residential => "Residential",
            Self::Skyline => "Skyline",
            Self::Park => "Park",
            Self::Service => "Service",
            Self::Training => "Training",
            Self::Scenic => "Scenic",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotRelationship {
    pub other_id: u64,
    #[serde(default = "default_trust")]
    pub trust: f32,
    #[serde(default)]
    pub collaboration_score: f32,
    #[serde(default)]
    pub last_interaction_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotIdea {
    pub id: u64,
    pub author_id: u64,
    pub kind: BotTaskKind,
    pub target: [i32; 3],
    pub score: f32,
    pub status: BotIdeaStatus,
    pub summary: String,
    #[serde(default)]
    pub district_id: Option<u64>,
    #[serde(default)]
    pub created_epoch: u64,
    #[serde(default)]
    pub cooldown_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotIdeaStatus {
    Proposed,
    Discussing,
    Approved,
    Built,
    Rejected,
}

impl Default for BotIdeaStatus {
    fn default() -> Self {
        Self::Proposed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotConversation {
    pub id: u64,
    #[serde(default)]
    pub participants: Vec<u64>,
    pub topic: BotConversationTopic,
    pub summary: String,
    #[serde(default)]
    pub importance: u8,
    #[serde(default)]
    pub created_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotConversationTopic {
    RoadAccess,
    Skyline,
    ParkBalance,
    PadLighting,
    RangeReadiness,
    DistrictUpgrade,
    CityBoundary,
}

impl Default for BotConversationTopic {
    fn default() -> Self {
        Self::RoadAccess
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotCrew {
    pub id: u64,
    pub role_focus: BotRole,
    #[serde(default)]
    pub bot_ids: Vec<u64>,
    pub project_id: u64,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotAutonomySettings {
    #[serde(default = "default_autonomy_enabled")]
    pub enabled: bool,
    #[serde(default = "default_bots_active")]
    pub bots_active: bool,
    #[serde(default = "default_autonomy_intensity")]
    pub intensity: u8,
}

impl Default for BotAutonomySettings {
    fn default() -> Self {
        Self {
            enabled: default_autonomy_enabled(),
            bots_active: default_bots_active(),
            intensity: default_autonomy_intensity(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotJournalEntry {
    pub tick: u64,
    pub text: String,
}

impl BotJournalEntry {
    fn new(text: impl Into<String>) -> Self {
        Self {
            tick: now_epoch(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BotRole {
    CompanionGuide,
    CompanionMaker,
    Planner,
    Surveyor,
    Builder,
    Architect,
    RoadCrew,
    ParkKeeper,
    RepairTech,
}

impl BotRole {
    fn label(self) -> &'static str {
        match self {
            Self::CompanionGuide => "Companion Guide",
            Self::CompanionMaker => "Companion Maker",
            Self::Planner => "Planner",
            Self::Surveyor => "Surveyor",
            Self::Builder => "Builder",
            Self::Architect => "Architect",
            Self::RoadCrew => "Road Crew",
            Self::ParkKeeper => "Park Keeper",
            Self::RepairTech => "Repair Tech",
        }
    }

    fn default_theme(self) -> BotTheme {
        match self {
            Self::CompanionGuide => BotTheme::WhiteAlloy,
            Self::CompanionMaker => BotTheme::CyanAlloy,
            Self::Planner | Self::Surveyor => BotTheme::CyanAlloy,
            Self::Builder | Self::RoadCrew => BotTheme::AmberStreet,
            Self::Architect => BotTheme::MagentaGlass,
            Self::ParkKeeper => BotTheme::GreenPark,
            Self::RepairTech => BotTheme::WhiteAlloy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotState {
    Idle,
    Surveying,
    Planning,
    Building,
    Inspecting,
    Returning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotProjectStatus {
    Queued,
    Active,
    WaitingForChunks,
    WaitingForPlayer,
    Complete,
    Blocked,
}

impl BotProjectStatus {
    fn is_done(self) -> bool {
        matches!(self, Self::Complete | Self::Blocked)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotTaskKind {
    BuildRoad,
    RecolorRoad,
    BuildTower,
    BuildGlassTower,
    MakeTaller,
    BuildHome,
    BuildResidentialBlock,
    BuildPark,
    BuildPlaza,
    LandingPad,
    BuildServicePad,
    AddLights,
    DecorateStreet,
    ClearFlatten,
    TargetRange,
    ExpandRoadGrid,
    UpgradeDistrict,
}

impl BotTaskKind {
    pub const ALL: [Self; 17] = [
        Self::BuildRoad,
        Self::RecolorRoad,
        Self::BuildTower,
        Self::BuildGlassTower,
        Self::MakeTaller,
        Self::BuildHome,
        Self::BuildResidentialBlock,
        Self::BuildPark,
        Self::BuildPlaza,
        Self::LandingPad,
        Self::BuildServicePad,
        Self::AddLights,
        Self::DecorateStreet,
        Self::ClearFlatten,
        Self::TargetRange,
        Self::ExpandRoadGrid,
        Self::UpgradeDistrict,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::BuildRoad => "Build Road",
            Self::RecolorRoad => "Recolor Road",
            Self::BuildTower => "Build Tower",
            Self::BuildGlassTower => "Glass Tower",
            Self::MakeTaller => "Make Taller",
            Self::BuildHome => "Build Home",
            Self::BuildResidentialBlock => "Residential Block",
            Self::BuildPark => "Build Park",
            Self::BuildPlaza => "Build Plaza",
            Self::LandingPad => "Landing Pad",
            Self::BuildServicePad => "Service Pad",
            Self::AddLights => "Add Lights",
            Self::DecorateStreet => "Decorate Street",
            Self::ClearFlatten => "Clear / Flatten",
            Self::TargetRange => "Target Range",
            Self::ExpandRoadGrid => "Expand Road Grid",
            Self::UpgradeDistrict => "Upgrade District",
        }
    }

    fn preferred_role(self) -> BotRole {
        match self {
            Self::BuildRoad | Self::RecolorRoad | Self::ExpandRoadGrid | Self::DecorateStreet => {
                BotRole::RoadCrew
            }
            Self::BuildTower
            | Self::BuildGlassTower
            | Self::MakeTaller
            | Self::BuildHome
            | Self::BuildResidentialBlock
            | Self::BuildPlaza
            | Self::UpgradeDistrict => BotRole::Architect,
            Self::BuildPark => BotRole::ParkKeeper,
            Self::LandingPad | Self::BuildServicePad | Self::ClearFlatten => BotRole::Builder,
            Self::AddLights => BotRole::RepairTech,
            Self::TargetRange => BotRole::Surveyor,
        }
    }

    fn conversation_topic(self) -> BotConversationTopic {
        match self {
            Self::BuildRoad | Self::RecolorRoad | Self::DecorateStreet | Self::ExpandRoadGrid => {
                BotConversationTopic::RoadAccess
            }
            Self::BuildTower | Self::BuildGlassTower | Self::MakeTaller => {
                BotConversationTopic::Skyline
            }
            Self::BuildPark | Self::BuildPlaza => BotConversationTopic::ParkBalance,
            Self::LandingPad | Self::BuildServicePad | Self::AddLights => {
                BotConversationTopic::PadLighting
            }
            Self::TargetRange => BotConversationTopic::RangeReadiness,
            Self::BuildHome | Self::BuildResidentialBlock | Self::UpgradeDistrict => {
                BotConversationTopic::DistrictUpgrade
            }
            Self::ClearFlatten => BotConversationTopic::CityBoundary,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BotTheme {
    CyanAlloy,
    MagentaGlass,
    AmberStreet,
    WhiteAlloy,
    GreenPark,
}

impl Default for BotTheme {
    fn default() -> Self {
        Self::CyanAlloy
    }
}

impl BotTheme {
    pub const ALL: [Self; 5] = [
        Self::CyanAlloy,
        Self::MagentaGlass,
        Self::AmberStreet,
        Self::WhiteAlloy,
        Self::GreenPark,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::CyanAlloy => "Cyan Alloy",
            Self::MagentaGlass => "Magenta Glass",
            Self::AmberStreet => "Amber Street",
            Self::WhiteAlloy => "White Alloy",
            Self::GreenPark => "Green Park",
        }
    }

    fn wall(self) -> Voxel {
        match self {
            Self::CyanAlloy | Self::WhiteAlloy => Voxel::from(BlockType::ShipHullAlloy),
            Self::MagentaGlass => Voxel::from(BlockType::CockpitGlass),
            Self::AmberStreet => Voxel::from(BlockType::Stone),
            Self::GreenPark => Voxel::from(BlockType::MossStone),
        }
    }

    fn accent(self) -> Voxel {
        match self {
            Self::CyanAlloy => Voxel::from(BlockType::ShipHullDark),
            Self::MagentaGlass => Voxel::from(BlockType::ShipHullAlloy),
            Self::AmberStreet => Voxel::from(BlockType::Limestone),
            Self::WhiteAlloy => Voxel::from(BlockType::Snow),
            Self::GreenPark => Voxel::from(BlockType::Wood),
        }
    }

    fn signal(self) -> Voxel {
        match self {
            Self::CyanAlloy | Self::WhiteAlloy | Self::GreenPark => {
                Voxel::from(BlockType::NeonCyan)
            }
            Self::MagentaGlass => Voxel::from(BlockType::NeonMagenta),
            Self::AmberStreet => Voxel::from(BlockType::NeonAmber),
        }
    }

    fn floor(self) -> Voxel {
        match self {
            Self::CyanAlloy | Self::WhiteAlloy => Voxel::from(BlockType::Limestone),
            Self::MagentaGlass => Voxel::from(BlockType::ShipHullDark),
            Self::AmberStreet => Voxel::from(BlockType::Stone),
            Self::GreenPark => Voxel::from(BlockType::Grass),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BotTaskCommand {
    pub bot_id: u64,
    pub task_type: BotTaskKind,
    pub theme: BotTheme,
    pub width: u8,
    pub height: u8,
    pub priority: u8,
}

impl Default for BotTaskCommand {
    fn default() -> Self {
        Self {
            bot_id: 0,
            task_type: BotTaskKind::BuildRoad,
            theme: BotTheme::CyanAlloy,
            width: 9,
            height: 12,
            priority: 5,
        }
    }
}

#[derive(Component)]
struct FriendlyBotEntity {
    id: u64,
}

/// Marks a child of a companion bot so we can keep the saucer ring spinning
/// independently of the body. Stores spin speed (rad/sec) and a phase offset
/// so two companions don't pulse in lockstep.
#[derive(Component)]
struct CompanionRing {
    speed: f32,
    phase: f32,
}

/// Marks the underside scan eye / glow on a companion so we can pulse it.
#[derive(Component)]
struct CompanionEye {
    phase: f32,
}

/// Marks the small thruster lights around the saucer rim so we can blink them
/// in a chase pattern.
#[derive(Component)]
struct CompanionThruster {
    index: u32,
    base_y: f32,
}

/// Marks the head/body of a character-style companion so we can apply the
/// head-tilt + blink-pitch transform without touching the rest of the rig.
#[derive(Component)]
struct CompanionHead {
    bot_id: u64,
    kind: u8, // 0 = AURA, 1 = BOLT
}

/// One iris sphere inside a visor. We track which side of the face it's on
/// (-1 left, +1 right, 0 cyclops) and its rest-position relative to the head
/// so we can offset it for "looking" without losing the anchor.
#[derive(Component)]
struct CompanionEyeIris {
    bot_id: u64,
    side: i8,
    base: Vec3,
    base_scale: Vec3,
}

/// Glowing chest/mood light. Holds a *unique* per-bot material handle so we
/// can mutate it (color/emissive) without affecting the other companion.
#[derive(Component)]
struct CompanionMoodLight {
    bot_id: u64,
    mat: Handle<StandardMaterial>,
}

/// Tip of the slim antenna — pulses gently.
#[derive(Component)]
#[allow(dead_code)]
struct CompanionAntennaTip {
    bot_id: u64,
    base_scale: f32,
}

/// Side ear-cap on BOLT — slow rotation counter to hover bob.
#[derive(Component)]
#[allow(dead_code)]
struct CompanionEarCap {
    bot_id: u64,
    side: i8,
}

/// Sub-parts of the non-companion worker droid. We tag children individually
/// so an animator system can drive idle/walk/work poses without touching the
/// other rigs in the scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerPart {
    Head,
    Visor,
    EyeL,
    EyeR,
    AntennaTip,
    ShoulderL,
    ShoulderR,
    ArmUpperL,
    ArmUpperR,
    ArmForeL,
    ArmForeR,
    HoverRing,
    BackpackVent,
    ChestPanel,
    Torso,
    ToolL,
}

#[derive(Component)]
struct WorkerBotPart {
    bot_id: u64,
    part: WorkerPart,
    /// Resting local translation captured at spawn; the animator returns to
    /// this every frame before applying its delta.
    base_translation: Vec3,
    /// Resting local scale captured at spawn for pulsing parts (eyes, vents).
    base_scale: Vec3,
}

#[derive(Resource, Default)]
#[allow(dead_code)]
struct BotVisualCache {
    cube: Option<Handle<Mesh>>,
    mats: HashMap<BotRole, Handle<StandardMaterial>>,
    companion_shell: Option<Handle<StandardMaterial>>,
    saucer_disc: Option<Handle<Mesh>>,
    saucer_rim: Option<Handle<Mesh>>,
    saucer_dome: Option<Handle<Mesh>>,
    saucer_eye: Option<Handle<Mesh>>,
    saucer_thruster: Option<Handle<Mesh>>,
    companion_dome_mat: Option<Handle<StandardMaterial>>,
    companion_eye_mat: Option<Handle<StandardMaterial>>,
    companion_rim_mat: Option<Handle<StandardMaterial>>,
    companion_thruster_mat: Option<Handle<StandardMaterial>>,

    // Character (AURA / BOLT) shapes.
    char_body_egg: Option<Handle<Mesh>>,
    char_body_barrel: Option<Handle<Mesh>>,
    char_body_chamfer: Option<Handle<Mesh>>,
    char_visor: Option<Handle<Mesh>>,
    char_iris: Option<Handle<Mesh>>,
    char_iris_pupil: Option<Handle<Mesh>>,
    char_mood_disc: Option<Handle<Mesh>>,
    char_ear_cap: Option<Handle<Mesh>>,
    char_antenna: Option<Handle<Mesh>>,
    char_antenna_tip: Option<Handle<Mesh>>,
    char_eye_stalk: Option<Handle<Mesh>>,
    char_side_thruster: Option<Handle<Mesh>>,
    char_shadow: Option<Handle<Mesh>>,
    char_hover_ring: Option<Handle<Mesh>>,
    char_iris_highlight: Option<Handle<Mesh>>,
    char_panel_seam: Option<Handle<Mesh>>,
    char_rivet: Option<Handle<Mesh>>,
    char_holo_blade: Option<Handle<Mesh>>,
    char_orbit_dot: Option<Handle<Mesh>>,
    char_arm_segment: Option<Handle<Mesh>>,
    char_claw: Option<Handle<Mesh>>,
    char_leg_strut: Option<Handle<Mesh>>,
    char_foot_pad: Option<Handle<Mesh>>,
    char_sensor_bar: Option<Handle<Mesh>>,
    char_backpack: Option<Handle<Mesh>>,

    // Character materials.
    mat_aura_shell: Option<Handle<StandardMaterial>>,
    mat_bolt_shell: Option<Handle<StandardMaterial>>,
    mat_visor: Option<Handle<StandardMaterial>>,
    mat_iris_blue: Option<Handle<StandardMaterial>>,
    mat_iris_amber: Option<Handle<StandardMaterial>>,
    mat_pupil: Option<Handle<StandardMaterial>>,
    mat_trim: Option<Handle<StandardMaterial>>,
    mat_ear: Option<Handle<StandardMaterial>>,
    mat_antenna_tip: Option<Handle<StandardMaterial>>,
    mat_shadow: Option<Handle<StandardMaterial>>,
    mat_hover_ring_aura: Option<Handle<StandardMaterial>>,
    mat_hover_ring_bolt: Option<Handle<StandardMaterial>>,
    mat_iris_highlight: Option<Handle<StandardMaterial>>,
    mat_holo_aura: Option<Handle<StandardMaterial>>,
    mat_holo_bolt: Option<Handle<StandardMaterial>>,
}

fn load_or_seed_bot_world(
    pending: Res<PendingWorldLoad>,
    active: Option<Res<ActiveWorld>>,
    world: Res<VoxelWorld>,
    mut brain: ResMut<FriendlyWorldBrain>,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active else {
        return;
    };
    let world_name = active.meta.name.clone();
    let from_files = load_bot_world_files(&world_name);
    let mut save = from_files.unwrap_or_else(|| active.meta.bot_world.clone());
    if save.is_empty() {
        let hub = Vec3::new(
            active.meta.player_pos[0],
            active.meta.player_pos[1],
            active.meta.player_pos[2],
        );
        save = BotWorldSave::seed(&world_name, hub, &world);
    }
    save.normalize();
    prime_autonomous_city_defaults(&mut save);
    brain.selected_bot = save.agents.first().map(|b| b.id).unwrap_or(0);
    brain.selected_district = save.districts.first().map(|d| d.id).unwrap_or(1);
    brain.command_draft.bot_id = brain.selected_bot;
    brain.hud_message = save
        .journal
        .last()
        .map(|j| j.text.clone())
        .unwrap_or_else(|| "Friendly bots online.".into());
    brain.save = save;
    brain.world_name = world_name;
    brain.autosave_timer = 30.0;
    brain.plan_timer = 0.0;
    brain.conversation_timer = 4.0;
    brain.message_cooldown = 0.0;
    brain.greeter_timer = 0.5;
    brain.busy_timer = 0.5;
    brain.force_city_idea = true;
    brain.dirty = true;
}

fn prime_autonomous_city_defaults(save: &mut BotWorldSave) {
    save.autonomy.enabled = true;
    save.autonomy.bots_active = true;
    save.autonomy.intensity = save.autonomy.intensity.max(default_autonomy_intensity());
    if let Some(settlement) = save.settlements.first_mut() {
        settlement.bounds.max_active_projects = settlement
            .bounds
            .max_active_projects
            .max(DEFAULT_MAX_ACTIVE_PROJECTS)
            .min(MAX_ACTIVE_PROJECTS_LIMIT);
    }
    for bot in save.agents.iter_mut().filter(|bot| bot.companion) {
        bot.memory.work_focus = bot.memory.work_focus.max(0.9);
        bot.memory.curiosity = bot.memory.curiosity.max(0.8);
        if matches!(bot.companion_mode, BotCompanionMode::AwaitingInstruction) {
            bot.companion_mode = BotCompanionMode::SurveySweep;
        }
    }
}

fn cleanup_bot_entities(
    mut commands: Commands,
    query: Query<Entity, With<FriendlyBotEntity>>,
    mut brain: ResMut<FriendlyWorldBrain>,
) {
    for entity in &query {
        despawn(&mut commands, entity);
    }
    brain.world_name.clear();
}

fn spawn_missing_bot_entities(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<BotVisualCache>,
    brain: Res<FriendlyWorldBrain>,
    query: Query<&FriendlyBotEntity>,
) {
    let existing: HashSet<u64> = query.iter().map(|b| b.id).collect();
    for bot in &brain.save.agents {
        if existing.contains(&bot.id) {
            continue;
        }
        spawn_bot_entity(&mut commands, &mut meshes, &mut materials, &mut cache, bot);
    }
}

fn spawn_bot_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BotVisualCache,
    bot: &BotAgent,
) {
    if bot.companion {
        spawn_companion_entity(commands, meshes, materials, cache, bot);
        return;
    }
    // --- Cached primitives. The worker droid is a real character rig: hover
    // pod base, segmented torso, two-bone arms with claws, a sculpted head
    // with visor + binocular eyes, and a power backpack — all tagged with
    // WorkerBotPart so the animator can drive walk gait, head tracking,
    // arm swing, antenna sway and emissive pulses per-bot.
    let cube = cache
        .cube
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let head_sphere = cache
        .char_body_egg
        .get_or_insert_with(|| meshes.add(Sphere::new(0.55)))
        .clone();
    let visor_mesh = cache
        .char_visor
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.05, 0.32, 0.62)))
        .clone();
    let eye_mesh = cache
        .char_iris
        .get_or_insert_with(|| meshes.add(Sphere::new(0.16)))
        .clone();
    let antenna_mesh = cache
        .char_antenna
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.04, 0.55)))
        .clone();
    let antenna_tip_mesh = cache
        .char_antenna_tip
        .get_or_insert_with(|| meshes.add(Sphere::new(0.10)))
        .clone();
    let hover_ring = cache
        .char_hover_ring
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.42, 0.04)))
        .clone();
    let backpack = cache
        .char_backpack
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.46, 0.58, 0.22)))
        .clone();
    let claw_mesh = cache
        .char_claw
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.18, 0.12, 0.28)))
        .clone();
    let role_color = bot_role_color(bot.role);
    let mat = materials.add(StandardMaterial {
        base_color: role_color,
        metallic: 0.15,
        perceptual_roughness: 0.65, // Star Wars realistic painted metal (matte/worn)
        ..default()
    });
    let role_lin = role_color.to_linear();
    // Dark gunmetal trim material — re-used for visor, panel seams, joints.
    let trim_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.08, 0.09), // Imperial grey-steel
        emissive: LinearRgba::rgb(
            role_lin.red * 0.1,
            role_lin.green * 0.1,
            role_lin.blue * 0.1,
        ),
        metallic: 0.90, // Scraped metal
        perceptual_roughness: 0.55,
        ..default()
    });
    let visor_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.01, 0.01, 0.01, 1.0),
        emissive: LinearRgba::rgb(
            role_lin.red * 1.5,
            role_lin.green * 1.5,
            role_lin.blue * 1.5,
        ),
        metallic: 1.0,
        perceptual_roughness: 0.1, // Glassy but grim
        ..default()
    });
    let eye_mat = materials.add(StandardMaterial {
        base_color: role_color,
        emissive: LinearRgba::rgb(
            role_lin.red * 35.0,
            role_lin.green * 35.0,
            role_lin.blue * 35.0,
        ), // Blindingly bright optic
        ..default()
    });
    let thruster_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.15, 0.1, 0.05), // Scorched engine bell
        emissive: LinearRgba::rgb(
            role_lin.red * 12.0,
            role_lin.green * 12.0,
            role_lin.blue * 12.0,
        ),
        metallic: 0.8,
        perceptual_roughness: 0.8,
        ..default()
    });
    let chest_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.18, 0.18, 0.18), // Beskar / raw steel
        emissive: LinearRgba::rgb(
            role_lin.red * 3.0,
            role_lin.green * 3.0,
            role_lin.blue * 3.0,
        ),
        metallic: 0.85,
        perceptual_roughness: 0.45,
        ..default()
    });
    let p = vec3_from_arr(bot.position);
    let bot_id = bot.id;
    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(p),
                ..default()
            },
            FriendlyBotEntity { id: bot_id },
            Name::new(format!("FriendlyBot_{}_{}", bot.id, bot.name)),
        ))
        .with_children(|c| {
            // ----- Hover pod / skirt -----
            let ring_pos = Vec3::new(0.0, -0.55, 0.0);
            let ring_scale = Vec3::new(1.6, 1.0, 1.6);
            c.spawn((
                PbrBundle {
                    mesh: hover_ring.clone(),
                    material: thruster_mat.clone(),
                    transform: Transform::from_translation(ring_pos).with_scale(ring_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::HoverRing,
                    base_translation: ring_pos,
                    base_scale: ring_scale,
                },
            ));
            // Pod underside disc (the actual hover engine).
            c.spawn(PbrBundle {
                mesh: hover_ring.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.68, 0.0))
                    .with_scale(Vec3::new(1.05, 1.0, 1.05)),
                ..default()
            });
            // Hip ring above the pod (chamfered base of torso).
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.36, 0.0))
                    .with_scale(Vec3::new(0.96, 0.18, 0.70)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.22, 0.0))
                    .with_scale(Vec3::new(0.84, 0.22, 0.58)),
                ..default()
            });

            // ----- Torso -----
            let torso_pos = Vec3::new(0.0, 0.32, 0.0);
            let torso_scale = Vec3::new(1.08, 1.35, 0.82);
            c.spawn((
                PbrBundle {
                    mesh: head_sphere.clone(),
                    material: mat.clone(),
                    transform: Transform::from_translation(torso_pos).with_scale(torso_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::Torso,
                    base_translation: torso_pos,
                    base_scale: torso_scale,
                },
            ));
            // Torso seam belt.
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.10, 0.0))
                    .with_scale(Vec3::new(1.16, 0.06, 0.90)),
                ..default()
            });
            // Chest plate emissive (role badge).
            let chest_pos = Vec3::new(0.0, 0.50, 0.42);
            let chest_scale = Vec3::new(0.52, 0.46, 0.04);
            c.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: chest_mat.clone(),
                    transform: Transform::from_translation(chest_pos).with_scale(chest_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::ChestPanel,
                    base_translation: chest_pos,
                    base_scale: chest_scale,
                },
            ));
            // Chest seam rivets.
            for cx in [-0.20_f32, 0.20] {
                for cy in [0.34_f32, 0.66] {
                    c.spawn(PbrBundle {
                        mesh: eye_mesh.clone(),
                        material: trim_mat.clone(),
                        transform: Transform::from_translation(Vec3::new(cx, cy, 0.42))
                            .with_scale(Vec3::splat(0.18)),
                        ..default()
                    });
                }
            }
            // Backpack.
            c.spawn(PbrBundle {
                mesh: backpack.clone(),
                material: mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.56, -0.44))
                    .with_scale(Vec3::splat(1.0)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: backpack.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.56, -0.55))
                    .with_scale(Vec3::new(0.85, 0.90, 0.20)),
                ..default()
            });
            // Backpack vent (pulses with work intensity).
            let vent_pos = Vec3::new(0.0, 0.28, -0.58);
            let vent_scale = Vec3::new(0.38, 0.10, 0.04);
            c.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: thruster_mat.clone(),
                    transform: Transform::from_translation(vent_pos).with_scale(vent_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::BackpackVent,
                    base_translation: vent_pos,
                    base_scale: vent_scale,
                },
            ));
            // Side vents on the backpack.
            for sx in [-0.22_f32, 0.22] {
                c.spawn(PbrBundle {
                    mesh: cube.clone(),
                    material: thruster_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, 0.78, -0.55))
                        .with_scale(Vec3::new(0.06, 0.30, 0.04)),
                    ..default()
                });
            }

            // ----- Shoulders + arms (two-bone, animated) -----
            for (side_sign, side) in [(-1.0_f32, -1.0_f32), (1.0_f32, 1.0_f32)] {
                let shoulder_pos = Vec3::new(0.64 * side, 0.78, 0.0);
                let shoulder_part = if side_sign < 0.0 {
                    WorkerPart::ShoulderL
                } else {
                    WorkerPart::ShoulderR
                };
                c.spawn((
                    PbrBundle {
                        mesh: head_sphere.clone(),
                        material: mat.clone(),
                        transform: Transform::from_translation(shoulder_pos)
                            .with_scale(Vec3::splat(0.36)),
                        ..default()
                    },
                    WorkerBotPart {
                        bot_id,
                        part: shoulder_part,
                        base_translation: shoulder_pos,
                        base_scale: Vec3::splat(0.36),
                    },
                ));
                let upper_pos = Vec3::new(0.78 * side, 0.42, 0.0);
                let upper_part = if side_sign < 0.0 {
                    WorkerPart::ArmUpperL
                } else {
                    WorkerPart::ArmUpperR
                };
                c.spawn((
                    PbrBundle {
                        mesh: cube.clone(),
                        material: mat.clone(),
                        transform: Transform::from_translation(upper_pos)
                            .with_scale(Vec3::new(0.26, 0.55, 0.26)),
                        ..default()
                    },
                    WorkerBotPart {
                        bot_id,
                        part: upper_part,
                        base_translation: upper_pos,
                        base_scale: Vec3::new(0.26, 0.55, 0.26),
                    },
                ));
                // Elbow joint.
                c.spawn(PbrBundle {
                    mesh: head_sphere.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.80 * side, 0.12, 0.0))
                        .with_scale(Vec3::splat(0.22)),
                    ..default()
                });
                let fore_pos = Vec3::new(0.82 * side, -0.18, 0.04);
                let fore_part = if side_sign < 0.0 {
                    WorkerPart::ArmForeL
                } else {
                    WorkerPart::ArmForeR
                };
                c.spawn((
                    PbrBundle {
                        mesh: cube.clone(),
                        material: mat.clone(),
                        transform: Transform::from_translation(fore_pos)
                            .with_scale(Vec3::new(0.24, 0.50, 0.24)),
                        ..default()
                    },
                    WorkerBotPart {
                        bot_id,
                        part: fore_part,
                        base_translation: fore_pos,
                        base_scale: Vec3::new(0.24, 0.50, 0.24),
                    },
                ));
                // Wrist + claw / tool.
                c.spawn(PbrBundle {
                    mesh: head_sphere.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.84 * side, -0.44, 0.04))
                        .with_scale(Vec3::splat(0.22)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: claw_mesh.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.84 * side, -0.60, 0.10))
                        .with_scale(Vec3::splat(0.95)),
                    ..default()
                });
                let _ = side; // suppress unused warning for clarity.
            }

            // Glowing held tool on the left hand (welder / scanner — flicks
            // on while building/surveying).
            let tool_pos = Vec3::new(-0.84, -0.74, 0.18);
            c.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: eye_mat.clone(),
                    transform: Transform::from_translation(tool_pos)
                        .with_scale(Vec3::new(0.10, 0.10, 0.30)),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::ToolL,
                    base_translation: tool_pos,
                    base_scale: Vec3::new(0.10, 0.10, 0.30),
                },
            ));

            // ----- Neck -----
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.99, 0.0))
                    .with_scale(Vec3::new(0.22, 0.14, 0.22)),
                ..default()
            });

            // ----- Head (animated as a unit via its WorkerPart::Head tag) -----
            let head_pos = Vec3::new(0.0, 1.24, 0.0);
            let head_scale = Vec3::new(0.94, 0.96, 0.94);
            c.spawn((
                PbrBundle {
                    mesh: head_sphere.clone(),
                    material: mat.clone(),
                    transform: Transform::from_translation(head_pos).with_scale(head_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::Head,
                    base_translation: head_pos,
                    base_scale: head_scale,
                },
            ));
            // Jaw / chin trim.
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.05, 0.18))
                    .with_scale(Vec3::new(0.46, 0.10, 0.34)),
                ..default()
            });
            // Visor — large emissive band; animated to scan when surveying.
            let visor_pos = Vec3::new(0.0, 1.26, 0.34);
            let visor_scale = Vec3::new(0.54, 0.56, 0.46);
            c.spawn((
                PbrBundle {
                    mesh: visor_mesh.clone(),
                    material: visor_mat.clone(),
                    transform: Transform::from_translation(visor_pos).with_scale(visor_scale),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::Visor,
                    base_translation: visor_pos,
                    base_scale: visor_scale,
                },
            ));
            // Two binocular eyes — pulse-animated.
            let eye_l_pos = Vec3::new(-0.18, 1.28, 0.48);
            let eye_r_pos = Vec3::new(0.18, 1.28, 0.48);
            c.spawn((
                PbrBundle {
                    mesh: eye_mesh.clone(),
                    material: eye_mat.clone(),
                    transform: Transform::from_translation(eye_l_pos).with_scale(Vec3::splat(0.55)),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::EyeL,
                    base_translation: eye_l_pos,
                    base_scale: Vec3::splat(0.55),
                },
            ));
            c.spawn((
                PbrBundle {
                    mesh: eye_mesh.clone(),
                    material: eye_mat.clone(),
                    transform: Transform::from_translation(eye_r_pos).with_scale(Vec3::splat(0.55)),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::EyeR,
                    base_translation: eye_r_pos,
                    base_scale: Vec3::splat(0.55),
                },
            ));
            // Cheek vents.
            for sx in [-0.42_f32, 0.42] {
                c.spawn(PbrBundle {
                    mesh: cube.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, 1.22, 0.20))
                        .with_scale(Vec3::new(0.06, 0.18, 0.10)),
                    ..default()
                });
            }
            // Cranial fin.
            c.spawn(PbrBundle {
                mesh: cube.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.56, -0.10))
                    .with_scale(Vec3::new(0.12, 0.34, 0.42)),
                ..default()
            });
            // Antenna mast.
            c.spawn(PbrBundle {
                mesh: antenna_mesh.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.18, 1.72, -0.04))
                    .with_scale(Vec3::splat(0.95)),
                ..default()
            });
            let ant_pos = Vec3::new(0.18, 2.02, -0.04);
            c.spawn((
                PbrBundle {
                    mesh: antenna_tip_mesh.clone(),
                    material: eye_mat.clone(),
                    transform: Transform::from_translation(ant_pos).with_scale(Vec3::splat(0.95)),
                    ..default()
                },
                WorkerBotPart {
                    bot_id,
                    part: WorkerPart::AntennaTip,
                    base_translation: ant_pos,
                    base_scale: Vec3::splat(0.95),
                },
            ));

            // Soft role light for personality.
            c.spawn(PointLightBundle {
                point_light: PointLight {
                    color: role_color,
                    intensity: 320_000.0,
                    range: 26.0,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(Vec3::new(0.0, 1.4, 0.0)),
                ..default()
            });
        });
}

fn spawn_companion_entity(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BotVisualCache,
    bot: &BotAgent,
) {
    if bot.companion_order == 0 {
        spawn_aura_companion(commands, meshes, materials, cache, bot);
    } else {
        spawn_bolt_companion(commands, meshes, materials, cache, bot);
    }
}

/// AURA — pearl-white egg companion (EVE / Rodney style).
fn spawn_aura_companion(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BotVisualCache,
    bot: &BotAgent,
) {
    let body = cache
        .char_body_egg
        .get_or_insert_with(|| meshes.add(Sphere::new(0.55)))
        .clone();
    let visor = cache
        .char_visor
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.05, 0.32, 0.62)))
        .clone();
    let iris = cache
        .char_iris
        .get_or_insert_with(|| meshes.add(Sphere::new(0.16)))
        .clone();
    let pupil = cache
        .char_iris_pupil
        .get_or_insert_with(|| meshes.add(Sphere::new(0.06)))
        .clone();
    let mood_disc = cache
        .char_mood_disc
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.32, 0.05)))
        .clone();
    let antenna = cache
        .char_antenna
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.04, 0.55)))
        .clone();
    let antenna_tip = cache
        .char_antenna_tip
        .get_or_insert_with(|| meshes.add(Sphere::new(0.10)))
        .clone();
    let side_thruster = cache
        .char_side_thruster
        .get_or_insert_with(|| meshes.add(Sphere::new(0.14)))
        .clone();
    let shadow = cache
        .char_shadow
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.55, 0.01)))
        .clone();
    let hover_ring = cache
        .char_hover_ring
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.42, 0.04)))
        .clone();
    let iris_highlight = cache
        .char_iris_highlight
        .get_or_insert_with(|| meshes.add(Sphere::new(0.035)))
        .clone();
    let panel_seam = cache
        .char_panel_seam
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.05, 0.018, 1.05)))
        .clone();
    let holo_blade = cache
        .char_holo_blade
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.92, 0.035, 0.18)))
        .clone();
    let orbit_dot = cache
        .char_orbit_dot
        .get_or_insert_with(|| meshes.add(Sphere::new(0.055)))
        .clone();
    let arm_segment = cache
        .char_arm_segment
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.11, 0.52, 0.13)))
        .clone();
    let claw = cache
        .char_claw
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.08, 0.24, 0.08)))
        .clone();
    let leg_strut = cache
        .char_leg_strut
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.055, 0.62)))
        .clone();
    let foot_pad = cache
        .char_foot_pad
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.34, 0.10, 0.48)))
        .clone();
    let sensor_bar = cache
        .char_sensor_bar
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.48, 0.08, 0.08)))
        .clone();
    let backpack = cache
        .char_backpack
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.46, 0.58, 0.22)))
        .clone();
    let shell_mat = companion_aura_shell_material(cache, materials);
    let visor_mat = companion_visor_material(cache, materials);
    let iris_mat = companion_iris_blue_material(cache, materials);
    let pupil_mat = companion_pupil_material(cache, materials);
    let trim_mat = companion_trim_material(cache, materials);
    let antenna_tip_mat = companion_antenna_tip_material(cache, materials);
    let mood_mat = companion_mood_material_unique(materials);
    let thruster_mat = companion_thruster_material(cache, materials);
    let shadow_mat = companion_shadow_material(cache, materials);
    let hover_ring_mat = companion_hover_ring_aura_material(cache, materials);
    let highlight_mat = companion_iris_highlight_material(cache, materials);
    let holo_mat = companion_holo_aura_material(cache, materials);

    let p = vec3_from_arr(bot.position);
    let bot_id = bot.id;

    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(p),
                ..default()
            },
            FriendlyBotEntity { id: bot_id },
            Name::new(format!("AURA_{}_{}", bot_id, bot.name)),
        ))
        .with_children(|c| {
            // Ground shadow disc — fakes AO under the floating bot.
            c.spawn(PbrBundle {
                mesh: shadow.clone(),
                material: shadow_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.95, 0.0))
                    .with_scale(Vec3::new(1.0, 1.0, 1.4)),
                ..default()
            });
            // Hover-glow ring — emissive disc just below the body.
            c.spawn(PbrBundle {
                mesh: hover_ring.clone(),
                material: hover_ring_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.72, 0.0)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: hover_ring.clone(),
                material: holo_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.42, -0.05))
                    .with_scale(Vec3::new(0.72, 1.0, 0.72)),
                ..default()
            });
            for &(sx, yaw) in &[(-0.72_f32, 0.18_f32), (0.72, -0.18)] {
                c.spawn(PbrBundle {
                    mesh: holo_blade.clone(),
                    material: holo_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, 0.04, -0.05))
                        .with_rotation(Quat::from_rotation_z(yaw))
                        .with_scale(Vec3::new(1.15, 1.0, 1.0)),
                    ..default()
                });
            }
            for &(x, y, z) in &[
                (-0.38_f32, 1.28_f32, 0.22_f32),
                (0.38, 1.28, 0.22),
                (-0.28, 1.18, -0.34),
                (0.28, 1.18, -0.34),
            ] {
                c.spawn(PbrBundle {
                    mesh: orbit_dot.clone(),
                    material: holo_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(x, y, z)),
                    ..default()
                });
            }
            // Lower belly (mirrored egg).
            c.spawn(PbrBundle {
                mesh: body.clone(),
                material: shell_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.18, 0.0))
                    .with_scale(Vec3::new(0.78, 0.95, 0.78)),
                ..default()
            });
            // Shoulder seam — thin dark trim ring at belly join.
            c.spawn(PbrBundle {
                mesh: panel_seam.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.20, 0.0))
                    .with_scale(Vec3::new(0.60, 1.0, 0.60)),
                ..default()
            });

            // HEAD assembly — visor + iris + antenna ride together so we can
            // tilt the head independently.
            c.spawn(PbrBundle {
                mesh: backpack.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.10, -0.56))
                    .with_scale(Vec3::new(1.0, 1.0, 0.85)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: sensor_bar.clone(),
                material: visor_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.95, 0.48))
                    .with_scale(Vec3::new(1.25, 1.0, 1.0)),
                ..default()
            });
            for &sx in &[-1.0_f32, 1.0_f32] {
                c.spawn(PbrBundle {
                    mesh: arm_segment.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.72, -0.12, 0.12))
                        .with_rotation(Quat::from_rotation_z(sx * -0.42)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: claw.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.86, -0.54, 0.18))
                        .with_rotation(Quat::from_rotation_z(sx * -0.55)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: leg_strut.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.28, -0.86, 0.08))
                        .with_rotation(Quat::from_rotation_z(sx * 0.20)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: foot_pad.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.34, -1.18, 0.16))
                        .with_rotation(Quat::from_rotation_y(sx * 0.08)),
                    ..default()
                });
            }

            c.spawn((
                SpatialBundle {
                    transform: Transform::from_translation(Vec3::new(0.0, 0.42, 0.0)),
                    ..default()
                },
                CompanionHead { bot_id, kind: 0 },
            ))
            .with_children(|h| {
                // Head shell — taller egg.
                h.spawn(PbrBundle {
                    mesh: body.clone(),
                    material: shell_mat.clone(),
                    transform: Transform::from_scale(Vec3::new(0.92, 1.05, 0.85)),
                    ..default()
                });

                // Dark visor band.
                h.spawn(PbrBundle {
                    mesh: visor.clone(),
                    material: visor_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, 0.05, 0.36))
                        .with_scale(Vec3::new(0.78, 1.0, 0.55)),
                    ..default()
                });

                // Two glowing iris spheres (left + right).
                for &(sx, side) in &[(-0.30_f32, -1_i8), (0.30_f32, 1_i8)] {
                    let base = Vec3::new(sx, 0.06, 0.52);
                    let base_scale = Vec3::splat(1.0);
                    h.spawn((
                        PbrBundle {
                            mesh: iris.clone(),
                            material: iris_mat.clone(),
                            transform: Transform::from_translation(base).with_scale(base_scale),
                            ..default()
                        },
                        CompanionEyeIris {
                            bot_id,
                            side,
                            base,
                            base_scale,
                        },
                    ))
                    .with_children(|e| {
                        // Pupil dot — sits slightly forward on the iris.
                        e.spawn(PbrBundle {
                            mesh: pupil.clone(),
                            material: pupil_mat.clone(),
                            transform: Transform::from_translation(Vec3::new(0.0, 0.0, 0.13)),
                            ..default()
                        });
                        // Cartoon shine highlight — small white dot on top-front.
                        e.spawn(PbrBundle {
                            mesh: iris_highlight.clone(),
                            material: highlight_mat.clone(),
                            transform: Transform::from_translation(Vec3::new(-0.05, 0.07, 0.135)),
                            ..default()
                        });
                    });
                }

                // Slim antenna.
                h.spawn(PbrBundle {
                    mesh: antenna.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, 0.55, -0.05)),
                    ..default()
                });
                // Antenna tip — pulses.
                h.spawn((
                    PbrBundle {
                        mesh: antenna_tip.clone(),
                        material: antenna_tip_mat.clone(),
                        transform: Transform::from_translation(Vec3::new(0.0, 0.88, -0.05)),
                        ..default()
                    },
                    CompanionAntennaTip {
                        bot_id,
                        base_scale: 1.0,
                    },
                ));
            });

            // Chest mood disc — color is driven each frame from the active
            // companion mode.
            c.spawn((
                PbrBundle {
                    mesh: mood_disc.clone(),
                    material: mood_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, -0.05, 0.55))
                        .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
                        .with_scale(Vec3::splat(1.0)),
                    ..default()
                },
                CompanionMoodLight {
                    bot_id,
                    mat: mood_mat.clone(),
                },
            ));

            // Side hover-thrusters (warm glows tucked at the bot's hips).
            for &sx in &[-0.55_f32, 0.55_f32] {
                c.spawn(PbrBundle {
                    mesh: side_thruster.clone(),
                    material: thruster_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, -0.45, 0.0)),
                    ..default()
                });
            }

            // Soft cyan glow.
            c.spawn(PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(0.55, 0.92, 1.0),
                    intensity: 580_000.0,
                    range: 36.0,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(Vec3::new(0.0, -0.2, 0.0)),
                ..default()
            });
        });
}

/// BOLT — warm-yellow stalk-eyed companion (WALL-E / Fender style).
fn spawn_bolt_companion(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut BotVisualCache,
    bot: &BotAgent,
) {
    let body = cache
        .char_body_barrel
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 0.95, 0.95)))
        .clone();
    let bevel = cache
        .char_body_chamfer
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.96, 1.0, 0.92)))
        .clone();
    let stalk = cache
        .char_eye_stalk
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.12, 0.18)))
        .clone();
    let iris = cache
        .char_iris
        .get_or_insert_with(|| meshes.add(Sphere::new(0.16)))
        .clone();
    let pupil = cache
        .char_iris_pupil
        .get_or_insert_with(|| meshes.add(Sphere::new(0.06)))
        .clone();
    let ear = cache
        .char_ear_cap
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.30, 0.18)))
        .clone();
    let mood_disc = cache
        .char_mood_disc
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.32, 0.05)))
        .clone();
    let antenna = cache
        .char_antenna
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.04, 0.55)))
        .clone();
    let antenna_tip = cache
        .char_antenna_tip
        .get_or_insert_with(|| meshes.add(Sphere::new(0.10)))
        .clone();
    let side_thruster = cache
        .char_side_thruster
        .get_or_insert_with(|| meshes.add(Sphere::new(0.14)))
        .clone();
    let shadow = cache
        .char_shadow
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.55, 0.01)))
        .clone();
    let hover_ring = cache
        .char_hover_ring
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.42, 0.04)))
        .clone();
    let iris_highlight = cache
        .char_iris_highlight
        .get_or_insert_with(|| meshes.add(Sphere::new(0.035)))
        .clone();
    let rivet = cache
        .char_rivet
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.06, 0.06, 0.04)))
        .clone();
    let holo_blade = cache
        .char_holo_blade
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.92, 0.035, 0.18)))
        .clone();
    let orbit_dot = cache
        .char_orbit_dot
        .get_or_insert_with(|| meshes.add(Sphere::new(0.055)))
        .clone();
    let arm_segment = cache
        .char_arm_segment
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.11, 0.52, 0.13)))
        .clone();
    let claw = cache
        .char_claw
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.08, 0.24, 0.08)))
        .clone();
    let leg_strut = cache
        .char_leg_strut
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.055, 0.62)))
        .clone();
    let foot_pad = cache
        .char_foot_pad
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.34, 0.10, 0.48)))
        .clone();
    let sensor_bar = cache
        .char_sensor_bar
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.48, 0.08, 0.08)))
        .clone();
    let backpack = cache
        .char_backpack
        .get_or_insert_with(|| meshes.add(Cuboid::new(0.46, 0.58, 0.22)))
        .clone();

    let shell_mat = companion_bolt_shell_material(cache, materials);
    let trim_mat = companion_trim_material(cache, materials);
    let ear_mat = companion_ear_material(cache, materials);
    let iris_mat = companion_iris_amber_material(cache, materials);
    let pupil_mat = companion_pupil_material(cache, materials);
    let antenna_tip_mat = companion_antenna_tip_material(cache, materials);
    let mood_mat = companion_mood_material_unique(materials);
    let thruster_mat = companion_thruster_material(cache, materials);
    let shadow_mat = companion_shadow_material(cache, materials);
    let hover_ring_mat = companion_hover_ring_bolt_material(cache, materials);
    let highlight_mat = companion_iris_highlight_material(cache, materials);
    let holo_mat = companion_holo_bolt_material(cache, materials);

    let p = vec3_from_arr(bot.position);
    let bot_id = bot.id;

    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(p),
                ..default()
            },
            FriendlyBotEntity { id: bot_id },
            Name::new(format!("BOLT_{}_{}", bot_id, bot.name)),
        ))
        .with_children(|c| {
            // Ground shadow + hover ring (under the body).
            c.spawn(PbrBundle {
                mesh: shadow.clone(),
                material: shadow_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.95, 0.0))
                    .with_scale(Vec3::new(1.05, 1.0, 1.5)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: hover_ring.clone(),
                material: hover_ring_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, -0.65, 0.0))
                    .with_scale(Vec3::new(1.1, 1.0, 1.1)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: hover_ring.clone(),
                material: holo_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.78, -0.10))
                    .with_scale(Vec3::new(0.82, 1.0, 0.82)),
                ..default()
            });
            for &(sx, yaw) in &[(-0.70_f32, -0.14_f32), (0.70, 0.14)] {
                c.spawn(PbrBundle {
                    mesh: holo_blade.clone(),
                    material: holo_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, -0.10, -0.03))
                        .with_rotation(Quat::from_rotation_z(yaw))
                        .with_scale(Vec3::new(1.05, 1.0, 1.0)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: orbit_dot.clone(),
                    material: holo_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.58, 0.72, 0.30)),
                    ..default()
                });
            }
            // Body — main barrel shell.
            c.spawn(PbrBundle {
                mesh: body.clone(),
                material: shell_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.0, 0.0)),
                ..default()
            });
            // Subtle inner bevel for chamfered look.
            c.spawn(PbrBundle {
                mesh: bevel.clone(),
                material: trim_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.0, 0.0))
                    .with_scale(Vec3::new(1.02, 0.65, 1.05)),
                ..default()
            });
            // Chest rivets — 4 small dark cubes in a + pattern.
            for &(rx, ry) in &[(0.0_f32, 0.20_f32), (0.0, -0.20), (-0.20, 0.0), (0.20, 0.0)] {
                c.spawn(PbrBundle {
                    mesh: rivet.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(rx, ry, 0.49)),
                    ..default()
                });
            }

            // HEAD — stalk-eye head sits on top.
            c.spawn(PbrBundle {
                mesh: backpack.clone(),
                material: ear_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.02, -0.58))
                    .with_scale(Vec3::new(1.15, 1.05, 0.9)),
                ..default()
            });
            c.spawn(PbrBundle {
                mesh: sensor_bar.clone(),
                material: ear_mat.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 0.36, 0.54))
                    .with_scale(Vec3::new(1.55, 1.0, 1.0)),
                ..default()
            });
            for &sx in &[-1.0_f32, 1.0_f32] {
                c.spawn(PbrBundle {
                    mesh: arm_segment.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.68, -0.10, 0.16))
                        .with_rotation(Quat::from_rotation_z(sx * -0.28)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: claw.clone(),
                    material: ear_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.82, -0.50, 0.25))
                        .with_rotation(Quat::from_rotation_z(sx * -0.38)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: bevel.clone(),
                    material: ear_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.72, -0.40, -0.04))
                        .with_scale(Vec3::new(0.22, 0.42, 0.86)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: leg_strut.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.28, -0.82, 0.05))
                        .with_rotation(Quat::from_rotation_z(sx * 0.10)),
                    ..default()
                });
                c.spawn(PbrBundle {
                    mesh: foot_pad.clone(),
                    material: ear_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx * 0.34, -1.12, 0.10))
                        .with_scale(Vec3::new(1.15, 1.0, 0.95)),
                    ..default()
                });
            }

            c.spawn((
                SpatialBundle {
                    transform: Transform::from_translation(Vec3::new(0.0, 0.55, 0.05)),
                    ..default()
                },
                CompanionHead { bot_id, kind: 1 },
            ))
            .with_children(|h| {
                // Two stalk eyes (left + right), tipped forward.
                for &(sx, side) in &[(-0.22_f32, -1_i8), (0.22_f32, 1_i8)] {
                    h.spawn(PbrBundle {
                        mesh: stalk.clone(),
                        material: trim_mat.clone(),
                        transform: Transform::from_translation(Vec3::new(sx, 0.10, 0.10))
                            .with_rotation(Quat::from_rotation_x(0.35)),
                        ..default()
                    });
                    let base = Vec3::new(sx, 0.22, 0.20);
                    let base_scale = Vec3::splat(1.0);
                    h.spawn((
                        PbrBundle {
                            mesh: iris.clone(),
                            material: iris_mat.clone(),
                            transform: Transform::from_translation(base).with_scale(base_scale),
                            ..default()
                        },
                        CompanionEyeIris {
                            bot_id,
                            side,
                            base,
                            base_scale,
                        },
                    ))
                    .with_children(|e| {
                        e.spawn(PbrBundle {
                            mesh: pupil.clone(),
                            material: pupil_mat.clone(),
                            transform: Transform::from_translation(Vec3::new(0.0, 0.0, 0.13)),
                            ..default()
                        });
                        e.spawn(PbrBundle {
                            mesh: iris_highlight.clone(),
                            material: highlight_mat.clone(),
                            transform: Transform::from_translation(Vec3::new(-0.05, 0.07, 0.135)),
                            ..default()
                        });
                    });
                }

                // Headphone-style ear caps.
                for &(sx, side) in &[(-0.55_f32, -1_i8), (0.55_f32, 1_i8)] {
                    h.spawn((
                        PbrBundle {
                            mesh: ear.clone(),
                            material: ear_mat.clone(),
                            transform: Transform::from_translation(Vec3::new(sx, 0.0, 0.0))
                                .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)),
                            ..default()
                        },
                        CompanionEarCap { bot_id, side },
                    ));
                }

                // Tail-rotor antenna at the back of the head.
                h.spawn(PbrBundle {
                    mesh: antenna.clone(),
                    material: trim_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, 0.18, -0.35))
                        .with_rotation(Quat::from_rotation_x(0.6)),
                    ..default()
                });
                h.spawn((
                    PbrBundle {
                        mesh: antenna_tip.clone(),
                        material: antenna_tip_mat.clone(),
                        transform: Transform::from_translation(Vec3::new(0.0, 0.40, -0.55)),
                        ..default()
                    },
                    CompanionAntennaTip {
                        bot_id,
                        base_scale: 1.0,
                    },
                ));
            });

            // Chest mood plate — wide warm-amber on BOLT.
            c.spawn((
                PbrBundle {
                    mesh: mood_disc.clone(),
                    material: mood_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(0.0, -0.05, 0.50))
                        .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
                        .with_scale(Vec3::new(1.2, 1.0, 1.0)),
                    ..default()
                },
                CompanionMoodLight {
                    bot_id,
                    mat: mood_mat.clone(),
                },
            ));

            // Ducted side fans (small warm thruster glows under the body).
            for &sx in &[-0.55_f32, 0.55_f32] {
                c.spawn(PbrBundle {
                    mesh: side_thruster.clone(),
                    material: thruster_mat.clone(),
                    transform: Transform::from_translation(Vec3::new(sx, -0.50, 0.0)),
                    ..default()
                });
            }

            // Warm under-glow.
            c.spawn(PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(1.0, 0.78, 0.45),
                    intensity: 520_000.0,
                    range: 32.0,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_translation(Vec3::new(0.0, -0.2, 0.0)),
                ..default()
            });
        });
}

#[allow(dead_code)]
fn bot_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
    role: BotRole,
) -> Handle<StandardMaterial> {
    if let Some(h) = cache.mats.get(&role) {
        return h.clone();
    }
    let color = bot_role_color(role);
    let lin = color.to_linear();
    let handle = materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::rgb(lin.red * 7.5, lin.green * 7.5, lin.blue * 7.5),
        metallic: 0.65,
        perceptual_roughness: 0.18,
        ..default()
    });
    cache.mats.insert(role, handle.clone());
    handle
}

#[allow(dead_code)]
fn companion_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_shell {
        return handle.clone();
    }
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.4, 0.4, 0.42),
        emissive: LinearRgba::rgb(0.05, 0.05, 0.05),
        metallic: 0.85,
        perceptual_roughness: 0.7,
        ..default()
    });
    cache.companion_shell = Some(handle.clone());
    handle
}

#[allow(dead_code)]
fn companion_dome_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_dome_mat {
        return handle.clone();
    }
    // Slightly translucent-looking glass dome with a soft inner glow.
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgba(0.55, 0.85, 1.0, 1.0),
        emissive: LinearRgba::rgb(0.6, 1.4, 2.0),
        metallic: 0.25,
        perceptual_roughness: 0.06,
        reflectance: 0.7,
        ..default()
    });
    cache.companion_dome_mat = Some(handle.clone());
    handle
}

#[allow(dead_code)]
fn companion_eye_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_eye_mat {
        return handle.clone();
    }
    // Bright cyan emissive — the underside scan/tractor-beam emitter.
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.85, 1.0, 1.0),
        emissive: LinearRgba::rgb(2.5, 9.0, 12.0),
        metallic: 0.0,
        perceptual_roughness: 0.05,
        ..default()
    });
    cache.companion_eye_mat = Some(handle.clone());
    handle
}

#[allow(dead_code)]
fn companion_rim_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_rim_mat {
        return handle.clone();
    }
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.1, 0.12),
        emissive: LinearRgba::rgb(0.01, 0.01, 0.01),
        metallic: 0.95,
        perceptual_roughness: 0.82,
        ..default()
    });
    cache.companion_rim_mat = Some(handle.clone());
    handle
}

fn companion_thruster_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(handle) = &cache.companion_thruster_mat {
        return handle.clone();
    }
    // Deep orange/red scorched thruster with glow
    let handle = materials.add(StandardMaterial {
        base_color: Color::srgb(0.2, 0.1, 0.05),
        emissive: LinearRgba::rgb(4.0, 1.5, 0.5),
        metallic: 0.8,
        perceptual_roughness: 0.9,
        ..default()
    });
    cache.companion_thruster_mat = Some(handle.clone());
    handle
}

// --- Character (AURA / BOLT) materials -------------------------------------

fn companion_aura_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_aura_shell {
        return h.clone();
    }
    // Star Wars gritty realistic imperial white/grey
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.55, 0.55),
        emissive: LinearRgba::rgb(0.01, 0.01, 0.01),
        metallic: 0.85,
        perceptual_roughness: 0.65,
        reflectance: 0.3,
        ..default()
    });
    cache.mat_aura_shell = Some(h.clone());
    h
}

fn companion_bolt_shell_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_bolt_shell {
        return h.clone();
    }
    // Star Wars realistic rusted astromech yellow/orange.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.48, 0.35, 0.10),
        emissive: LinearRgba::rgb(0.02, 0.01, 0.0),
        metallic: 0.9,
        perceptual_roughness: 0.8,
        reflectance: 0.2,
        ..default()
    });
    cache.mat_bolt_shell = Some(h.clone());
    h
}

fn companion_visor_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_visor {
        return h.clone();
    }
    // Dark, glossy black astromech sensor visor
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.01, 0.01, 0.01),
        emissive: LinearRgba::rgb(0.005, 0.005, 0.005),
        metallic: 0.8,
        perceptual_roughness: 0.1,
        reflectance: 0.9,
        ..default()
    });
    cache.mat_visor = Some(h.clone());
    h
}

fn companion_iris_blue_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_iris_blue {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.55, 0.85, 1.0),
        emissive: LinearRgba::rgb(2.0, 7.0, 11.0),
        metallic: 0.0,
        perceptual_roughness: 0.05,
        ..default()
    });
    cache.mat_iris_blue = Some(h.clone());
    h
}

fn companion_iris_amber_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_iris_amber {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.85, 0.45),
        emissive: LinearRgba::rgb(9.0, 5.5, 1.5),
        metallic: 0.0,
        perceptual_roughness: 0.05,
        ..default()
    });
    cache.mat_iris_amber = Some(h.clone());
    h
}

fn companion_pupil_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_pupil {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.02, 0.04),
        emissive: LinearRgba::rgb(0.0, 0.0, 0.0),
        metallic: 0.1,
        perceptual_roughness: 0.4,
        ..default()
    });
    cache.mat_pupil = Some(h.clone());
    h
}

fn companion_trim_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_trim {
        return h.clone();
    }
    // Highly worn, dark structural metal.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.08, 0.09),
        emissive: LinearRgba::rgb(0.005, 0.005, 0.005),
        metallic: 1.0,
        perceptual_roughness: 0.85,
        ..default()
    });
    cache.mat_trim = Some(h.clone());
    h
}

fn companion_ear_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_ear {
        return h.clone();
    }
    // Scratched, oxidized dark metal.
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(0.12, 0.12, 0.12),
        emissive: LinearRgba::rgb(0.0, 0.0, 0.0),
        metallic: 0.95,
        perceptual_roughness: 0.75,
        ..default()
    });
    cache.mat_ear = Some(h.clone());
    h
}

fn companion_antenna_tip_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_antenna_tip {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.8, 0.6),
        emissive: LinearRgba::rgb(8.0, 5.0, 2.0),
        metallic: 0.0,
        perceptual_roughness: 0.05,
        ..default()
    });
    cache.mat_antenna_tip = Some(h.clone());
    h
}

/// Mood-light material — *not* cached; we deliberately make a fresh one per
/// companion so we can mutate one bot's mood color without affecting the
/// other. Initial color is a soft cyan that the animator overrides each frame.
fn companion_mood_material_unique(
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(0.7, 0.95, 1.0),
        emissive: LinearRgba::rgb(2.0, 5.0, 7.0),
        metallic: 0.0,
        perceptual_roughness: 0.10,
        ..default()
    })
}

fn companion_shadow_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_shadow {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgba(0.02, 0.03, 0.05, 0.55),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });
    cache.mat_shadow = Some(h.clone());
    h
}

fn companion_hover_ring_aura_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_hover_ring_aura {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgba(0.4, 0.85, 1.0, 0.65),
        emissive: LinearRgba::rgb(1.5, 5.0, 7.0),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    cache.mat_hover_ring_aura = Some(h.clone());
    h
}

fn companion_hover_ring_bolt_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_hover_ring_bolt {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.7, 0.35, 0.65),
        emissive: LinearRgba::rgb(7.0, 4.0, 1.5),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    cache.mat_hover_ring_bolt = Some(h.clone());
    h
}

fn companion_holo_aura_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_holo_aura {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgba(0.28, 0.95, 1.0, 0.58),
        emissive: LinearRgba::rgb(2.5, 8.0, 10.0),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    cache.mat_holo_aura = Some(h.clone());
    h
}

fn companion_holo_bolt_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_holo_bolt {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.62, 0.18, 0.62),
        emissive: LinearRgba::rgb(9.0, 4.2, 0.9),
        alpha_mode: AlphaMode::Add,
        unlit: true,
        ..default()
    });
    cache.mat_holo_bolt = Some(h.clone());
    h
}

fn companion_iris_highlight_material(
    cache: &mut BotVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    if let Some(h) = &cache.mat_iris_highlight {
        return h.clone();
    }
    let h = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 1.0, 1.0),
        emissive: LinearRgba::rgb(8.0, 8.0, 8.0),
        unlit: true,
        ..default()
    });
    cache.mat_iris_highlight = Some(h.clone());
    h
}

fn bot_role_color(role: BotRole) -> Color {
    match role {
        BotRole::CompanionGuide => Color::srgb(0.82, 0.96, 1.0),
        BotRole::CompanionMaker => Color::srgb(0.62, 0.88, 1.0),
        BotRole::Planner => Color::srgb(0.20, 0.95, 1.0),
        BotRole::Surveyor => Color::srgb(0.25, 1.0, 0.60),
        BotRole::Builder => Color::srgb(1.0, 0.70, 0.20),
        BotRole::Architect => Color::srgb(1.0, 0.15, 0.85),
        BotRole::RoadCrew => Color::srgb(0.90, 0.95, 1.0),
        BotRole::ParkKeeper => Color::srgb(0.20, 0.95, 0.35),
        BotRole::RepairTech => Color::srgb(1.0, 0.35, 0.20),
    }
}

#[allow(clippy::too_many_arguments)]
fn tick_friendly_world(
    time: Res<Time>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut world: ResMut<VoxelWorld>,
    mut history: ResMut<BuilderHistory>,
    budget: Res<RuntimeBudget>,
    active: Option<Res<ActiveWorld>>,
    city: Option<Res<crate::city::CityState>>,
    player_q: Query<&Transform, With<Player>>,
    ship_q: Query<&Transform, With<ShipInstance>>,
) {
    if brain.save.settlements.is_empty() {
        return;
    }
    let dt = time.delta_seconds().min(0.1);
    brain.plan_timer -= dt;
    brain.conversation_timer -= dt;
    brain.greeter_timer -= dt;
    brain.busy_timer -= dt;
    brain.message_cooldown = (brain.message_cooldown - dt).max(0.0);

    let player_pos = player_q.get_single().ok().map(|t| t.translation);
    let ship_positions: Vec<Vec3> = ship_q.iter().map(|t| t.translation).collect();

    if let (Some(active), Some(city)) = (active.as_deref(), city.as_deref()) {
        if city.roads_loaded_world == active.meta.name
            && sync_user_city_roads(&mut brain.save, &city.roads)
        {
            brain.force_city_idea = true;
            brain.dirty = true;
        }
    }

    keep_bots_visible_and_busy(&mut brain, &world, player_pos);
    if !brain.save.autonomy.bots_active {
        if brain.message_cooldown <= 0.0 {
            brain.hud_message = "Bot workers are OFF. Plans and progress are saved.".into();
            brain.message_cooldown = VISIBLE_MESSAGE_COOLDOWN;
        }
        return;
    }
    process_queued_commands(&mut brain, &world, player_pos, &ship_positions);
    let open_projects_before_planning = planner_project_count(&brain.save);
    let allow_new_city_work = bot_allows_new_city_work(&budget);
    let allow_road_front_infill =
        open_projects_before_planning <= 1 && open_access_road_project_count(&brain.save) > 0;
    let allow_planner_tick = allow_new_city_work
        || allow_road_front_infill
        || open_projects_before_planning == 0
        || brain.force_city_idea;
    if brain.force_city_idea || (brain.save.autonomy.enabled && brain.plan_timer <= 0.0) {
        brain.plan_timer = planner_interval(&brain.save);
        let urgent = brain.force_city_idea;
        brain.force_city_idea = false;
        if allow_planner_tick
            && run_city_planner(
                &mut brain,
                &world,
                player_pos,
                &ship_positions,
                urgent,
                &budget,
            )
        {
            brain.dirty = true;
        } else if !allow_new_city_work && brain.message_cooldown <= 0.0 {
            brain.hud_message = "Bot city paused while terrain and mesh streaming catch up.".into();
            brain.message_cooldown = VISIBLE_MESSAGE_COOLDOWN;
        }
    }

    move_bot_memories(&mut brain.save, &world, dt);

    let mut completed = Vec::new();
    let mut blocked = Vec::new();
    let open_projects = planner_project_count(&brain.save);
    let frame_edit_budget = bot_frame_edit_budget(&budget, open_projects);
    let per_project_budget = bot_project_slice_budget(frame_edit_budget, open_projects);
    let project_scan_budget = bot_project_scan_budget(open_projects);
    let mut changed_total = 0usize;
    let bounds = brain.save.primary_bounds();

    if frame_edit_budget > 0 {
        let project_len = brain.save.projects.len();
        let scan_limit = project_len.min(project_scan_budget);
        let start = if project_len == 0 {
            0
        } else {
            brain.project_scan_cursor % project_len
        };
        let mut scanned = 0usize;
        while scanned < scan_limit {
            let idx = (start + scanned) % project_len;
            scanned += 1;
            if brain.save.projects[idx].status.is_done() {
                continue;
            }
            let remaining = frame_edit_budget.saturating_sub(changed_total);
            if remaining == 0 {
                break;
            }
            let result = advance_project_slice(
                &mut brain.save.projects[idx],
                &mut world,
                &mut history,
                player_pos,
                &ship_positions,
                bounds,
                remaining.min(per_project_budget),
            );
            changed_total += result.changed;
            if result.completed {
                completed.push(idx);
            } else if result.blocked {
                blocked.push(idx);
            }
            if changed_total >= frame_edit_budget {
                break;
            }
        }
        brain.project_scan_cursor = if project_len == 0 {
            0
        } else {
            (start + scanned).min(start + project_len) % project_len
        };
    } else if brain.message_cooldown <= 0.0 {
        brain.hud_message = "Bot builders are yielding to max-distance world streaming.".into();
        brain.message_cooldown = VISIBLE_MESSAGE_COOLDOWN;
    }

    for idx in completed {
        let label = brain.save.projects[idx].label.clone();
        complete_project_at(&mut brain.save, idx);
        show_city_message(
            &mut brain,
            format!("{label} complete. Bot city is growing."),
            8,
        );
        if allow_new_city_work {
            brain.force_city_idea = true;
            brain.plan_timer = 0.0;
        } else {
            brain.plan_timer = brain.plan_timer.max(planner_interval(&brain.save) * 2.0);
        }
        brain.dirty = true;
    }

    for idx in blocked {
        let label = brain.save.projects[idx].label.clone();
        let reason = brain.save.projects[idx].blocked_reason.clone();
        show_city_message(&mut brain, format!("{label} blocked: {reason}"), 7);
        brain.dirty = true;
    }

    sync_bot_task_progress(&mut brain.save);
    brain.save.journal.truncate(128);
    if changed_total > 0 {
        brain.dirty = true;
    }
}

fn process_queued_commands(
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) {
    let commands: Vec<BotTaskCommand> = brain.queued_commands.drain(..).collect();
    for command in commands {
        let bot_id = if command.bot_id == 0 {
            brain.selected_bot
        } else {
            command.bot_id
        };
        let bot_base = brain
            .save
            .agents
            .iter()
            .find(|b| b.id == bot_id)
            .map(|b| vec3_from_arr(b.position))
            .or(player_pos)
            .unwrap_or_else(|| vec3_from_arr(brain.save.settlements[0].hub));
        let base = brain
            .save
            .districts
            .iter()
            .find(|d| d.id == brain.selected_district)
            .map(|d| vec3_from_arr(d.center))
            .unwrap_or(bot_base);
        let size = command_size(command);
        match add_project_with_site_search(
            &mut brain.save,
            world,
            command.task_type,
            size,
            command.theme,
            Some(bot_id),
            command.priority,
            true,
            base,
            bot_base,
            player_pos,
            ship_positions,
        ) {
            Ok(_) => {
                let message = format!(
                    "{} accepted {}.",
                    bot_label(&brain.save, bot_id),
                    command.task_type.label()
                );
                brain
                    .save
                    .journal
                    .push(BotJournalEntry::new(message.clone()));
                brain.hud_message = message;
                brain.dirty = true;
            }
            Err(reason) => {
                brain.save.last_blocked_reason = reason.clone();
                brain.hud_message = format!("Task rejected: {reason}");
                brain.dirty = true;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_project_with_site_search(
    save: &mut BotWorldSave,
    world: &VoxelWorld,
    kind: BotTaskKind,
    size: [i32; 3],
    theme: BotTheme,
    assigned_bot: Option<u64>,
    priority: u8,
    manual: bool,
    district_anchor: Vec3,
    bot_anchor: Vec3,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) -> Result<u64, String> {
    let seq = save.projects.len() + save.completed_projects as usize;
    let site_search_player_anchor = if manual { player_pos } else { None };
    let candidates = command_site_candidates(
        save,
        world,
        kind,
        size,
        district_anchor,
        bot_anchor,
        site_search_player_anchor,
        player_pos,
        seq,
    );
    let mut last_error = if manual {
        "no loaded safe build site near you yet".to_string()
    } else {
        "no loaded safe city lot near the road grid yet".to_string()
    };
    for origin in candidates {
        let district_id = nearest_district(save, project_center(origin, size)).map(|d| d.id);
        match add_project(
            save,
            world,
            kind,
            origin,
            size,
            theme,
            assigned_bot,
            district_id,
            None,
            priority,
            manual,
            player_pos,
            ship_positions,
        ) {
            Ok(id) => return Ok(id),
            Err(reason) => last_error = reason,
        }
    }
    Err(last_error)
}

#[allow(clippy::too_many_arguments)]
fn command_site_candidates(
    save: &BotWorldSave,
    world: &VoxelWorld,
    kind: BotTaskKind,
    size: [i32; 3],
    district_anchor: Vec3,
    bot_anchor: Vec3,
    player_anchor_pos: Option<Vec3>,
    player_clearance_pos: Option<Vec3>,
    seq: usize,
) -> Vec<[i32; 3]> {
    let bounds = save.primary_bounds();
    let half = Vec3::new(size[0] as f32 * 0.5, 0.0, size[2] as f32 * 0.5);
    let mut anchors = Vec::new();
    if let Some(player) = player_anchor_pos {
        anchors.push(player);
    }
    anchors.push(bot_anchor);
    anchors.push(district_anchor);
    if let Some(hub) = save.settlements.first().map(|s| vec3_from_arr(s.hub)) {
        anchors.push(hub);
    }

    let base_radius: f32 = match kind {
        BotTaskKind::ExpandRoadGrid => 72.0,
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::DecorateStreet => 42.0,
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => 54.0,
        BotTaskKind::BuildResidentialBlock
        | BotTaskKind::BuildPlaza
        | BotTaskKind::UpgradeDistrict => 58.0,
        BotTaskKind::ClearFlatten => 36.0,
        _ => 46.0,
    };

    let mut out = Vec::new();
    for (anchor_idx, anchor) in anchors.into_iter().enumerate() {
        for step in 0..24 {
            let player_clearance_ring =
                size[0].max(size[2]) as f32 * 0.72 + BOT_PLAYER_PROJECT_MARGIN;
            let anchor_needs_clearance_ring = player_clearance_pos
                .map(|player| {
                    Vec2::new(anchor.x - player.x, anchor.z - player.z).length()
                        < player_clearance_ring + base_radius
                })
                .unwrap_or(false);
            let ring_base = if anchor_needs_clearance_ring
                || (anchor_idx == 0 && player_anchor_pos.is_some())
            {
                base_radius.max(player_clearance_ring)
            } else {
                base_radius
            };
            let ring = ring_base + (step / 6) as f32 * 36.0;
            let angle = (seq + step + anchor_idx * 7) as f32 * 2.399_963_1;
            let center = anchor + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring);
            let target_origin = clamp_to_bounds(bounds, center - half);
            let origin = project_origin(world, target_origin);
            if !bounds.contains_box(origin, size) || !project_anchor_loaded(world, origin, size) {
                continue;
            }
            if !out.contains(&origin) {
                out.push(origin);
            }
        }
    }
    out
}

fn queue_mega_city_starter_projects(
    save: &mut BotWorldSave,
    world: &VoxelWorld,
    player_pos: Vec3,
    ship_positions: &[Vec3],
    max_to_queue: usize,
) -> usize {
    let district = nearest_district(save, player_pos).cloned();
    let district_anchor = district
        .as_ref()
        .map(|d| vec3_from_arr(d.center))
        .or_else(|| save.settlements.first().map(|s| vec3_from_arr(s.hub)))
        .unwrap_or(player_pos);
    let road_ready = district.as_ref().map_or_else(
        || settlement_has_access_roads(save),
        |district| district_has_road_access(save, district),
    );
    let specs = if road_ready {
        vec![
            (
                BotTaskKind::BuildGlassTower,
                autonomous_project_size(BotTaskKind::BuildGlassTower),
                BotTheme::MagentaGlass,
                BotRole::Architect,
                9,
            ),
            (
                BotTaskKind::BuildResidentialBlock,
                autonomous_project_size(BotTaskKind::BuildResidentialBlock),
                BotTheme::WhiteAlloy,
                BotRole::Planner,
                8,
            ),
            (
                BotTaskKind::BuildPlaza,
                autonomous_project_size(BotTaskKind::BuildPlaza),
                BotTheme::WhiteAlloy,
                BotRole::Architect,
                7,
            ),
            (
                BotTaskKind::DecorateStreet,
                [88, 7, 11],
                BotTheme::AmberStreet,
                BotRole::RepairTech,
                8,
            ),
        ]
    } else {
        vec![(
            BotTaskKind::ExpandRoadGrid,
            autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            BotTheme::AmberStreet,
            BotRole::RoadCrew,
            10,
        )]
    };
    let mut queued = 0usize;
    let mut last_error = String::new();
    for (kind, size, theme, role, priority) in specs {
        if queued >= max_to_queue {
            break;
        }
        let assigned = pick_bot(save, role).or_else(|| pick_bot(save, kind.preferred_role()));
        match add_project_with_site_search(
            save,
            world,
            kind,
            size,
            theme,
            assigned,
            priority,
            false,
            district_anchor,
            district_anchor,
            Some(player_pos),
            ship_positions,
        ) {
            Ok(_) => queued += 1,
            Err(reason) => last_error = reason,
        }
    }
    if queued == 0 && !last_error.is_empty() {
        save.last_blocked_reason = last_error;
    }
    queued
}

fn keep_bots_visible_and_busy(
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
) {
    if brain.save.agents.is_empty() {
        return;
    }
    if brain.save.agents.iter().any(|b| b.companion) {
        update_companion_targets(&mut brain.save, world, player_pos);
        return;
    }

    if brain.busy_timer <= 0.0 {
        brain.busy_timer = BOT_BUSY_RETARGET_INTERVAL;
        retarget_idle_bots_to_worksites(&mut brain.save, world);
    }

    let Some(player_pos) = player_pos else {
        return;
    };
    if brain.greeter_timer > 0.0 {
        return;
    }
    brain.greeter_timer = BOT_GREETER_INTERVAL;

    let nearest = brain
        .save
        .agents
        .iter()
        .map(|b| vec3_from_arr(b.position).distance(player_pos))
        .fold(f32::MAX, f32::min);
    if nearest <= BOT_MEET_DISTANCE {
        return;
    }

    let Some(bot_idx) = pick_greeter_bot(&brain.save) else {
        return;
    };
    let bot_name = brain.save.agents[bot_idx].name.clone();
    let offset_angle = brain.save.agents[bot_idx].id as f32 * 1.618_033_9;
    let target = player_pos
        + Vec3::new(
            offset_angle.cos() * BOT_MEET_OFFSET,
            0.0,
            offset_angle.sin() * BOT_MEET_OFFSET,
        );
    let target = clamp_to_bounds(brain.save.primary_bounds(), target);
    let tx = target.x.round() as i32;
    let tz = target.z.round() as i32;
    let ty = world.surface_height_at(tx, tz) as f32 + 2.3;

    if let Some(bot) = brain.save.agents.get_mut(bot_idx) {
        bot.target = [tx as f32 + 0.5, ty, tz as f32 + 0.5];
        bot.state = BotState::Surveying;
        bot.memory.last_message = "Coming over with a city update.".into();
    }
    show_city_message(
        brain,
        format!("{bot_name} is coming to meet you with bot city plans."),
        6,
    );
}

fn update_companion_targets(save: &mut BotWorldSave, world: &VoxelWorld, player_pos: Option<Vec3>) {
    let Some(player_pos) = player_pos else {
        return;
    };
    let elapsed = now_epoch() as f32 * 0.001;
    let companion_count = save.agents.iter().filter(|b| b.companion).count().max(1) as f32;
    for bot in save.agents.iter_mut().filter(|b| b.companion) {
        bot.memory.preferred_follow_distance = bot
            .memory
            .preferred_follow_distance
            .clamp(COMPANION_FOLLOW_MIN, COMPANION_FOLLOW_MAX);
        if bot.current_task.is_some() {
            bot.companion_mode = BotCompanionMode::AssistingTask;
            continue;
        }
        let order = bot.companion_order as f32;
        let row = (bot.companion_order % 3) as f32;
        match bot.companion_mode {
            BotCompanionMode::FollowingPlayer => {
                let pos = vec3_from_arr(bot.position);
                let to_bot = pos - player_pos;
                let flat = Vec2::new(to_bot.x, to_bot.z);
                let distance = flat.length();
                let desired = bot.memory.preferred_follow_distance;
                let slot_angle = order * std::f32::consts::TAU / companion_count + elapsed * 0.08;
                let fallback = Vec2::new(slot_angle.cos(), slot_angle.sin());
                let dir = if distance > 0.25 {
                    flat / distance
                } else {
                    fallback
                };
                let close_enough = distance <= desired + 1.25;
                let target = if close_enough {
                    // Do not flee when the player walks up to them. Hold the
                    // current conversational bubble and only rise if terrain
                    // clearance needs it.
                    Vec3::new(pos.x, player_pos.y + 2.2 + row * 0.42, pos.z)
                } else {
                    let side_spread =
                        Vec2::new(-dir.y, dir.x) * ((order - (companion_count - 1.0) * 0.5) * 1.15);
                    let flat_target =
                        Vec2::new(player_pos.x, player_pos.z) + dir * desired + side_spread;
                    Vec3::new(
                        flat_target.x,
                        player_pos.y + 2.5 + desired * 0.18 + row * 0.42,
                        flat_target.y,
                    )
                };
                set_bot_air_target(bot, world, target, 2.4);
                bot.memory.last_message = format!(
                    "Following at {:.1}m. Closer/farther commands are live.",
                    desired
                );
            }
            BotCompanionMode::ScanningArea => {
                // Slow orbit while sweeping the immediate area. Higher altitude
                // so the scan beam can reach the ground cleanly.
                let angle =
                    (now_epoch() as f32 * 0.6) + order * std::f32::consts::TAU / companion_count;
                let radius = 8.0 + row * 1.6;
                let target =
                    player_pos + Vec3::new(angle.cos() * radius, 6.0, angle.sin() * radius);
                set_bot_air_target(bot, world, target, 5.5);
                bot.memory.last_message =
                    "Scan beam active. Mapping nearby terrain and structures.".into();
            }
            BotCompanionMode::Patrolling => {
                // Wide circle around the player, faster orbit, higher altitude.
                let angle =
                    (now_epoch() as f32 * 0.9) + order * std::f32::consts::TAU / companion_count;
                let radius = 22.0 + row * 4.0;
                let target =
                    player_pos + Vec3::new(angle.cos() * radius, 9.0, angle.sin() * radius);
                set_bot_air_target(bot, world, target, 8.0);
                bot.memory.last_message =
                    "Patrol arc engaged. Sweeping the perimeter for threats.".into();
            }
            BotCompanionMode::SurveySweep => {
                // Lawnmower pattern — long figure-eight over a wide area to
                // map the surrounding chunks for the city planner.
                let t = elapsed * 0.7 + order * 0.82;
                let radius = 28.0 + row * 8.0;
                let target = player_pos
                    + Vec3::new((t).sin() * radius, 12.0, (t * 2.0).sin() * radius * 0.55);
                set_bot_air_target(bot, world, target, 10.0);
                bot.memory.last_message =
                    "Survey sweep underway. Logging chunk metadata for planners.".into();
            }
            BotCompanionMode::PreviewingEdit => {
                bot.state = BotState::Inspecting;
                bot.memory.last_message = "Projecting a safe build preview.".into();
            }
            BotCompanionMode::HoldingPosition | BotCompanionMode::AwaitingInstruction => {
                bot.state = BotState::Idle;
                if bot.memory.last_message.is_empty() {
                    bot.memory.last_message = "Awaiting your instruction.".into();
                }
            }
            BotCompanionMode::AssistingTask => {}
            BotCompanionMode::Blocked => {
                bot.state = BotState::Inspecting;
                bot.memory.last_message = "Blocked until you choose another safe area.".into();
            }
        }
    }
}

/// Set a free-flight target for a companion. Honors the requested altitude
/// directly (clamped above the surface) instead of snapping to ground hover.
fn set_bot_air_target(bot: &mut BotAgent, world: &VoxelWorld, target: Vec3, min_clearance: f32) {
    let tx = target.x.round() as i32;
    let tz = target.z.round() as i32;
    let floor = world.surface_height_at(tx, tz) as f32 + min_clearance;
    let ty = target.y.max(floor);
    bot.target = [tx as f32 + 0.5, ty, tz as f32 + 0.5];
    bot.state = BotState::Surveying;
}

fn draw_companion_preview_gizmos(
    brain: Res<FriendlyWorldBrain>,
    mut gizmos: Gizmos,
    time: Res<Time>,
) {
    let elapsed = time.elapsed_seconds();
    let pulse = (elapsed * 3.4).sin() * 0.5 + 0.5;
    for bot in brain.save.agents.iter().filter(|b| b.companion) {
        let pos = vec3_from_arr(bot.position);
        let target = vec3_from_arr(bot.target);
        match bot.companion_mode {
            BotCompanionMode::ScanningArea => {
                // Tractor-beam style cone: line from saucer down to ground.
                let ground = Vec3::new(pos.x, pos.y - 6.0, pos.z);
                let beam_color = Color::srgba(0.2, 0.95, 1.0, 0.55);
                gizmos.line(pos, ground, beam_color);
                // Concentric expanding scan rings on the ground.
                for ring in 0..3 {
                    let phase = (elapsed * 1.4 + ring as f32 * 0.45 + bot.companion_order as f32)
                        .rem_euclid(1.5);
                    let radius = 1.5 + phase * 5.0;
                    let alpha = 0.55 * (1.0 - phase / 1.5).clamp(0.0, 1.0);
                    gizmos.circle(
                        ground + Vec3::Y * 0.05,
                        Dir3::Y,
                        radius,
                        Color::srgba(0.3, 0.95, 1.0, alpha),
                    );
                }
                // Aura around the bot itself.
                let radius = 4.0 + pulse * 1.5 + bot.companion_order as f32;
                gizmos.sphere(
                    pos,
                    Quat::IDENTITY,
                    radius,
                    Color::srgba(0.25, 0.95, 1.0, 0.22),
                );
                gizmos.line(pos, target, Color::srgba(0.2, 0.9, 1.0, 0.45));
            }
            BotCompanionMode::SurveySweep => {
                // Long forward scan line + faint orbit hint.
                let forward = (target - pos).normalize_or_zero();
                let probe = pos + forward * 18.0 + Vec3::NEG_Y * 8.0;
                gizmos.line(pos, probe, Color::srgba(0.6, 1.0, 0.6, 0.6));
                gizmos.circle(
                    Vec3::new(pos.x, pos.y - 8.0, pos.z),
                    Dir3::Y,
                    8.0 + pulse * 1.5,
                    Color::srgba(0.5, 1.0, 0.6, 0.35),
                );
            }
            BotCompanionMode::Patrolling => {
                // Highlight the patrol arc beneath the bot.
                gizmos.circle(
                    Vec3::new(pos.x, pos.y - 3.0, pos.z),
                    Dir3::Y,
                    3.0,
                    Color::srgba(1.0, 0.85, 0.3, 0.45),
                );
                gizmos.line(pos, target, Color::srgba(1.0, 0.7, 0.2, 0.55));
            }
            BotCompanionMode::FollowingPlayer => {
                // Subtle trail line showing where the bot is heading next.
                gizmos.line(pos, target, Color::srgba(0.4, 0.9, 1.0, 0.30));
            }
            BotCompanionMode::HoldingPosition | BotCompanionMode::AwaitingInstruction => {
                // Idle hover indicator: small pulsing ring under the bot.
                gizmos.circle(
                    Vec3::new(pos.x, pos.y - 1.6, pos.z),
                    Dir3::Y,
                    1.0 + pulse * 0.2,
                    Color::srgba(0.7, 0.9, 1.0, 0.30),
                );
            }
            BotCompanionMode::PreviewingEdit | BotCompanionMode::Blocked => {
                gizmos.sphere(
                    pos,
                    Quat::IDENTITY,
                    1.1 + pulse * 0.35,
                    if matches!(bot.companion_mode, BotCompanionMode::Blocked) {
                        Color::srgba(1.0, 0.18, 0.12, 0.45)
                    } else {
                        Color::srgba(0.9, 1.0, 1.0, 0.45)
                    },
                );
            }
            BotCompanionMode::AssistingTask => {
                gizmos.line(pos, target, Color::srgba(0.0, 0.95, 0.6, 0.55));
            }
        }
    }

    let Some(preview) = &brain.save.companion_preview else {
        return;
    };
    let center = project_center(preview.origin, preview.size);
    let scale = Vec3::new(
        preview.size[0].max(1) as f32,
        preview.size[1].max(1) as f32,
        preview.size[2].max(1) as f32,
    );
    let color = if preview.status.is_valid() {
        Color::srgba(0.0, 0.92, 1.0, 0.62 + pulse * 0.22)
    } else {
        Color::srgba(1.0, 0.18, 0.08, 0.70)
    };
    gizmos.cuboid(Transform::from_translation(center).with_scale(scale), color);
    let floor = Vec3::new(center.x, preview.origin[1] as f32 + 0.08, center.z);
    gizmos.cuboid(
        Transform::from_translation(floor).with_scale(Vec3::new(scale.x, 0.1, scale.z)),
        color.with_alpha(0.38),
    );
}

fn pick_greeter_bot(save: &BotWorldSave) -> Option<usize> {
    [BotRole::Surveyor, BotRole::Planner, BotRole::RepairTech]
        .into_iter()
        .find_map(|role| {
            save.agents
                .iter()
                .position(|b| b.role == role && b.current_task.is_none())
        })
        .or_else(|| save.agents.iter().position(|b| b.current_task.is_none()))
        .or_else(|| {
            save.agents
                .iter()
                .position(|b| matches!(b.role, BotRole::Planner | BotRole::Surveyor))
        })
        .or(Some(0))
}

fn retarget_idle_bots_to_worksites(save: &mut BotWorldSave, world: &VoxelWorld) {
    let active_sites: Vec<Vec3> = save
        .projects
        .iter()
        .filter(|p| !p.status.is_done())
        .map(|p| project_center(p.origin, p.size))
        .collect();
    let district_sites: Vec<Vec3> = save
        .districts
        .iter()
        .map(|d| vec3_from_arr(d.center))
        .collect();
    let hub = save
        .settlements
        .first()
        .map(|s| vec3_from_arr(s.hub))
        .unwrap_or(Vec3::ZERO);
    let bounds = save.primary_bounds();

    for (idx, bot) in save.agents.iter_mut().enumerate() {
        if bot.current_task.is_some() {
            continue;
        }
        let anchor = active_sites
            .get(idx % active_sites.len().max(1))
            .copied()
            .or_else(|| {
                district_sites
                    .get(idx % district_sites.len().max(1))
                    .copied()
            })
            .unwrap_or(hub);
        let angle = (bot.id as f32 * 2.399_963_1) + idx as f32;
        let radius = 5.0 + (idx % 4) as f32 * 3.0;
        let target = clamp_to_bounds(
            bounds,
            anchor + Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius),
        );
        let tx = target.x.round() as i32;
        let tz = target.z.round() as i32;
        let ty = world.surface_height_at(tx, tz) as f32 + 2.2;
        bot.target = [tx as f32 + 0.5, ty, tz as f32 + 0.5];
        bot.state = BotState::Surveying;
        bot.memory.last_message = if active_sites.is_empty() {
            "Surveying a beautiful new city block.".into()
        } else {
            "Circling the worksite and checking details.".into()
        };
    }
}

fn planner_interval(save: &BotWorldSave) -> f32 {
    let intensity = save.autonomy.intensity.clamp(1, 10) as f32;
    (8.5 - intensity * 0.55).clamp(2.4, 8.0)
}

fn active_project_limit(save: &BotWorldSave) -> usize {
    let bounds = save.primary_bounds();
    let intensity_limit = 2 + save.autonomy.intensity.clamp(1, 10) as usize;
    bounds
        .max_active_projects
        .clamp(1, MAX_ACTIVE_PROJECTS_LIMIT)
        .min(intensity_limit)
}

fn active_project_limit_for_budget(save: &BotWorldSave, budget: &RuntimeBudget) -> usize {
    let base = active_project_limit(save);
    let pressure = bot_streaming_pressure(budget);
    if !bot_allows_new_city_work(budget) {
        if open_access_road_project_count(save) > 0 {
            return base.min(2);
        }
        return base.min(1);
    }
    if pressure > 0.55 {
        base.min(2)
    } else if pressure > 0.35 {
        base.min(4)
    } else {
        base
    }
}

fn planner_project_count(save: &BotWorldSave) -> usize {
    save.projects
        .iter()
        .filter(|project| !project.status.is_done())
        .count()
}

fn open_access_road_project_count(save: &BotWorldSave) -> usize {
    save.projects
        .iter()
        .filter(|project| !project.status.is_done() && is_access_road_project(project.kind))
        .count()
}

fn bot_streaming_pressure(budget: &RuntimeBudget) -> f32 {
    let rd_pressure = if budget.target_render_distance <= 2 {
        0.0
    } else {
        1.0 - budget.render_distance as f32 / budget.target_render_distance as f32
    };
    budget
        .queue_pressure
        .max(budget.frame_pressure)
        .max(rd_pressure)
        .clamp(0.0, 1.25)
}

fn bot_horizon_gap_ratio(budget: &RuntimeBudget) -> f32 {
    if budget.target_render_distance <= 2 {
        return 0.0;
    }
    let target = budget.target_render_distance.max(1) as f32;
    let gap = budget
        .target_render_distance
        .saturating_sub(budget.render_distance)
        .max(0) as f32;
    (gap / target).clamp(0.0, 1.0)
}

fn bot_allows_new_city_work(budget: &RuntimeBudget) -> bool {
    let pressure = bot_streaming_pressure(budget);
    let rd_gap = budget
        .target_render_distance
        .saturating_sub(budget.render_distance);
    pressure < 0.62 && rd_gap <= (budget.target_render_distance / 3).max(4)
}

fn bot_frame_edit_budget(budget: &RuntimeBudget, open_projects: usize) -> usize {
    let horizon_gap = bot_horizon_gap_ratio(budget);
    if budget.target_render_distance >= 24 && horizon_gap >= 0.30 {
        return 0;
    }
    let pressure = bot_streaming_pressure(budget);
    let mut edit_budget = if pressure >= 0.90 {
        0
    } else if pressure >= 0.72 {
        48
    } else if pressure >= 0.52 {
        96
    } else if pressure >= 0.32 {
        180
    } else {
        BOT_MAX_FRAME_EDITS
    };
    if budget.target_render_distance >= 24 && horizon_gap >= 0.20 {
        edit_budget = edit_budget.min(24);
    }
    if open_projects > DEFAULT_MAX_ACTIVE_PROJECTS {
        edit_budget /= 2;
    }
    edit_budget
}

fn bot_project_slice_budget(frame_budget: usize, open_projects: usize) -> usize {
    if frame_budget == 0 {
        return 0;
    }
    let divisor = open_projects.clamp(1, 4);
    (frame_budget / divisor)
        .clamp(24, BOT_MAX_PROJECT_SLICE_EDITS)
        .min(frame_budget)
}

fn bot_project_scan_budget(open_projects: usize) -> usize {
    if open_projects <= DEFAULT_MAX_ACTIVE_PROJECTS {
        96
    } else if open_projects <= MAX_ACTIVE_PROJECTS_LIMIT {
        128
    } else {
        160
    }
}

fn queue_visible_city_work(
    save: &mut BotWorldSave,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
    max_to_queue: usize,
) -> usize {
    if max_to_queue == 0 {
        return 0;
    }
    let anchor = player_pos
        .or_else(|| {
            save.settlements
                .first()
                .map(|settlement| vec3_from_arr(settlement.hub))
        })
        .unwrap_or(Vec3::ZERO);
    queue_mega_city_starter_projects(save, world, anchor, ship_positions, max_to_queue)
}

fn run_city_planner(
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
    urgent: bool,
    budget: &RuntimeBudget,
) -> bool {
    let active = planner_project_count(&brain.save);
    let limit = active_project_limit_for_budget(&brain.save, budget);
    if active >= limit && !urgent {
        return false;
    }

    let Some(idea_id) = propose_city_idea(&mut brain.save, world, urgent) else {
        let available = limit.saturating_sub(active).max(usize::from(urgent));
        let queued = queue_visible_city_work(
            &mut brain.save,
            world,
            player_pos,
            ship_positions,
            available.min(2),
        );
        if queued > 0 {
            show_city_message(
                brain,
                format!("Autonomy refilled {queued} city build(s) on the active city grid."),
                8,
            );
            return true;
        }
        return false;
    };
    let Some(idea_index) = brain.save.ideas.iter().position(|i| i.id == idea_id) else {
        return false;
    };

    let mut changed = false;
    if brain.conversation_timer <= 0.0 || urgent {
        if let Some(summary) = record_planning_conversation(&mut brain.save, idea_index) {
            brain.conversation_timer =
                CONVERSATION_INTERVAL + (10 - brain.save.autonomy.intensity.clamp(1, 10)) as f32;
            show_city_message(brain, summary, 6);
            changed = true;
        }
    }

    if active >= limit && !urgent {
        return changed;
    }

    let idea = brain.save.ideas[idea_index].clone();
    let size = autonomous_project_size(idea.kind);
    let assigned = pick_bot(&brain.save, idea.kind.preferred_role());
    let theme = idea
        .district_id
        .and_then(|id| district_theme(&brain.save, id))
        .unwrap_or_else(|| idea.kind.preferred_role().default_theme());
    match add_project(
        &mut brain.save,
        world,
        idea.kind,
        idea.target,
        size,
        theme,
        assigned,
        idea.district_id,
        Some(idea.id),
        if urgent { 10 } else { 5 },
        false,
        player_pos,
        ship_positions,
    ) {
        Ok(project_id) => {
            if let Some(saved) = brain.save.ideas.iter_mut().find(|i| i.id == idea.id) {
                saved.status = BotIdeaStatus::Approved;
            }
            show_city_message(
                brain,
                format!(
                    "Bots approved {} as project #{project_id}: {}",
                    idea.kind.label(),
                    idea.summary
                ),
                7,
            );
            changed = true;
        }
        Err(reason) => {
            if let Some(saved) = brain.save.ideas.iter_mut().find(|i| i.id == idea.id) {
                saved.status = BotIdeaStatus::Rejected;
            }
            brain.save.last_blocked_reason = reason.clone();
            show_city_message(brain, format!("City idea rejected: {reason}"), 6);
            if planner_project_count(&brain.save) == 0 {
                let queued = queue_visible_city_work(
                    &mut brain.save,
                    world,
                    player_pos,
                    ship_positions,
                    limit.min(2),
                );
                if queued > 0 {
                    show_city_message(
                        brain,
                        format!(
                            "Swarm recovered with {queued} loaded city build(s) on the city grid."
                        ),
                        8,
                    );
                }
            }
            changed = true;
        }
    }
    changed
}

fn propose_city_idea(save: &mut BotWorldSave, world: &VoxelWorld, urgent: bool) -> Option<u64> {
    let existing_open = save
        .ideas
        .iter()
        .filter(|i| {
            matches!(
                i.status,
                BotIdeaStatus::Proposed | BotIdeaStatus::Discussing
            )
        })
        .count();
    if existing_open >= 4 && !urgent {
        return save
            .ideas
            .iter()
            .filter(|i| {
                matches!(
                    i.status,
                    BotIdeaStatus::Proposed | BotIdeaStatus::Discussing
                )
            })
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .map(|i| i.id);
    }

    let seq = save.completed_projects as usize + save.projects.len() + save.ideas.len();
    let district = choose_planning_district(save, seq)?.clone();
    let mut kind = choose_district_project(save, &district, seq, urgent);
    let mut size = autonomous_project_size(kind);
    let mut origin = find_loaded_build_site(save, world, &district, kind, size, seq)?;
    let mut score = score_planned_site(save, world, &district, origin, size, kind);
    if score < 2.2
        && !matches!(
            kind,
            BotTaskKind::ClearFlatten
                | BotTaskKind::BuildRoad
                | BotTaskKind::RecolorRoad
                | BotTaskKind::ExpandRoadGrid
        )
    {
        kind = BotTaskKind::ClearFlatten;
        size = autonomous_project_size(kind);
        origin = find_loaded_build_site(save, world, &district, kind, size, seq).unwrap_or(origin);
        score = score_planned_site(save, world, &district, origin, size, kind) + 1.4;
    }
    let author = pick_bot(save, kind.preferred_role())
        .or_else(|| pick_bot(save, BotRole::Planner))
        .unwrap_or(0);
    let id = save.next_idea_id;
    save.next_idea_id += 1;
    let summary = format!(
        "{} proposes {} in {}",
        bot_label(save, author),
        kind.label(),
        district.name
    );
    save.ideas.push(BotIdea {
        id,
        author_id: author,
        kind,
        target: origin,
        score,
        status: BotIdeaStatus::Proposed,
        summary,
        district_id: Some(district.id),
        created_epoch: now_epoch(),
        cooldown_key: format!("{}:{:?}", district.id, kind),
    });
    Some(id)
}

fn choose_planning_district(save: &BotWorldSave, seq: usize) -> Option<&BotDistrict> {
    if save.districts.is_empty() {
        return None;
    }
    let mut districts: Vec<&BotDistrict> = save.districts.iter().collect();
    districts.sort_by_key(|d| (d.completed_projects, d.id));
    if seq % 3 != 1 {
        if let Some(road_ready) = districts.iter().copied().find(|district| {
            !matches!(district.kind, BotDistrictKind::HubCore)
                && district_has_road_access(save, district)
        }) {
            return Some(road_ready);
        }
    }
    let preferred = seq % districts.len();
    districts.get(preferred).copied()
}

fn choose_district_project(
    save: &BotWorldSave,
    district: &BotDistrict,
    seq: usize,
    urgent: bool,
) -> BotTaskKind {
    if !district_has_road_access(save, district) {
        return BotTaskKind::ExpandRoadGrid;
    }
    if let Some(kind) = user_road_shape_project_for_district(save, district) {
        return kind;
    }
    if urgent {
        return match district.kind {
            BotDistrictKind::HubCore => BotTaskKind::BuildPlaza,
            BotDistrictKind::Residential => BotTaskKind::BuildResidentialBlock,
            BotDistrictKind::Skyline => BotTaskKind::BuildGlassTower,
            BotDistrictKind::Park => BotTaskKind::BuildPark,
            BotDistrictKind::Service => BotTaskKind::BuildServicePad,
            BotDistrictKind::Training => BotTaskKind::TargetRange,
            BotDistrictKind::Scenic => BotTaskKind::BuildPlaza,
        };
    }
    let road_count = save.settlements.first().map(|s| s.road_count).unwrap_or(0);
    if road_count < 2 && seq % 3 == 1 {
        return BotTaskKind::ExpandRoadGrid;
    }
    if road_count < 4 && seq % 5 == 1 {
        return BotTaskKind::ExpandRoadGrid;
    }
    let candidates: &[BotTaskKind] = match district.kind {
        BotDistrictKind::HubCore => &[
            BotTaskKind::LandingPad,
            BotTaskKind::BuildPlaza,
            BotTaskKind::DecorateStreet,
        ],
        BotDistrictKind::Residential => &[
            BotTaskKind::BuildResidentialBlock,
            BotTaskKind::BuildHome,
            BotTaskKind::BuildPark,
        ],
        BotDistrictKind::Skyline => &[
            BotTaskKind::BuildGlassTower,
            BotTaskKind::BuildTower,
            BotTaskKind::BuildPlaza,
        ],
        BotDistrictKind::Park => &[
            BotTaskKind::BuildPark,
            BotTaskKind::BuildPlaza,
            BotTaskKind::DecorateStreet,
        ],
        BotDistrictKind::Service => &[
            BotTaskKind::BuildServicePad,
            BotTaskKind::LandingPad,
            BotTaskKind::AddLights,
        ],
        BotDistrictKind::Training => &[
            BotTaskKind::TargetRange,
            BotTaskKind::DecorateStreet,
            BotTaskKind::BuildServicePad,
        ],
        BotDistrictKind::Scenic => &[
            BotTaskKind::BuildPlaza,
            BotTaskKind::BuildGlassTower,
            BotTaskKind::BuildPark,
        ],
    };
    district_project_cycle_choice(save, district.id, candidates, seq)
}

fn user_road_shape_project_for_district(
    save: &BotWorldSave,
    district: &BotDistrict,
) -> Option<BotTaskKind> {
    semantic_user_roads_by_intent(save, district)
        .into_iter()
        .filter_map(|guide| semantic_project_kind_for_guide(district, guide))
        .find(|kind| !district_has_project_kind(save, district.id, *kind))
}

fn district_project_cycle_choice(
    save: &BotWorldSave,
    district_id: u64,
    candidates: &[BotTaskKind],
    seq: usize,
) -> BotTaskKind {
    let fallback = candidates[seq % candidates.len()];
    for offset in 0..candidates.len() {
        let kind = candidates[(seq + offset) % candidates.len()];
        if !district_has_project_kind(save, district_id, kind) {
            return kind;
        }
    }
    fallback
}

fn semantic_project_kind_for_guide(
    district: &BotDistrict,
    guide: &BotRoadGuide,
) -> Option<BotTaskKind> {
    match guide.shape {
        BotRoadGuideShape::Roundabout => Some(BotTaskKind::BuildPlaza),
        BotRoadGuideShape::Corner => Some(match district.kind {
            BotDistrictKind::Skyline | BotDistrictKind::Scenic => BotTaskKind::BuildGlassTower,
            BotDistrictKind::HubCore | BotDistrictKind::Park => BotTaskKind::BuildPlaza,
            BotDistrictKind::Residential => BotTaskKind::BuildResidentialBlock,
            BotDistrictKind::Service => BotTaskKind::BuildServicePad,
            BotDistrictKind::Training => BotTaskKind::TargetRange,
        }),
        BotRoadGuideShape::Straight => None,
    }
}

fn district_has_project_kind(save: &BotWorldSave, district_id: u64, kind: BotTaskKind) -> bool {
    save.projects.iter().any(|project| {
        project.district_id == Some(district_id)
            && project.kind == kind
            && !matches!(project.status, BotProjectStatus::Blocked)
    })
}

fn is_road_project(kind: BotTaskKind) -> bool {
    matches!(
        kind,
        BotTaskKind::BuildRoad
            | BotTaskKind::RecolorRoad
            | BotTaskKind::ExpandRoadGrid
            | BotTaskKind::DecorateStreet
            | BotTaskKind::AddLights
    )
}

fn is_access_road_project(kind: BotTaskKind) -> bool {
    matches!(
        kind,
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid
    )
}

fn settlement_has_access_roads(save: &BotWorldSave) -> bool {
    save.settlements.first().map(|s| s.road_count).unwrap_or(0) > 0
        || !save.user_roads.is_empty()
        || save.projects.iter().any(|project| {
            is_access_road_project(project.kind)
                && !matches!(project.status, BotProjectStatus::Blocked)
        })
}

fn district_has_road_access(save: &BotWorldSave, district: &BotDistrict) -> bool {
    if matches!(district.kind, BotDistrictKind::HubCore) {
        return true;
    }
    if save
        .user_roads
        .iter()
        .any(|road| road_guide_matches_district(road, district))
    {
        return true;
    }
    save.projects.iter().any(|project| {
        project.district_id == Some(district.id)
            && is_access_road_project(project.kind)
            && !matches!(project.status, BotProjectStatus::Blocked)
    })
}

fn find_loaded_build_site(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    kind: BotTaskKind,
    size: [i32; 3],
    seq: usize,
) -> Option<[i32; 3]> {
    let bounds = save.primary_bounds();
    let mut candidates = Vec::new();
    for slot in &district.build_slots {
        candidates.push(project_origin(
            world,
            clamp_to_bounds(
                bounds,
                Vec3::new(slot[0] as f32, slot[1] as f32, slot[2] as f32),
            ),
        ));
    }
    let center = vec3_from_arr(district.center);
    for step in 0..16 {
        let angle = (seq + step) as f32 * 2.399_963_1;
        let ring = 16.0 + (step / 4) as f32 * 18.0;
        candidates.push(project_origin(
            world,
            clamp_to_bounds(
                bounds,
                center + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring),
            ),
        ));
    }
    if let Some(hub) = save.settlements.first().map(|s| vec3_from_arr(s.hub)) {
        for step in 0..10 {
            let angle = (seq + step * 3) as f32 * 1.618_033_9;
            let ring = 24.0 + step as f32 * 8.0;
            candidates.push(project_origin(
                world,
                clamp_to_bounds(
                    bounds,
                    hub + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring),
                ),
            ));
        }
    }
    if !is_road_project(kind) {
        let semantic_candidates = semantic_road_site_origins(save, world, district, kind, size);
        if let Some(origin) =
            best_build_site_from_candidates(save, world, district, kind, size, semantic_candidates)
        {
            return Some(origin);
        }
    }
    if is_road_project(kind) {
        candidates.extend(district_road_origins(save, world, district, size));
    } else {
        candidates.extend(roadside_lot_origins(save, world, district, kind, size, seq));
    }

    best_build_site_from_candidates(save, world, district, kind, size, candidates)
}

fn best_build_site_from_candidates(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    kind: BotTaskKind,
    size: [i32; 3],
    candidates: Vec<[i32; 3]>,
) -> Option<[i32; 3]> {
    let bounds = save.primary_bounds();
    candidates
        .into_iter()
        .filter(|origin| bounds.contains_box(*origin, size))
        .filter(|origin| project_anchor_loaded(world, *origin, size))
        .filter(|origin| !project_footprint_reserved(save, *origin, size, kind))
        .filter(|origin| {
            !project_footprint_blocks_road_corridor(save, district, *origin, size, kind)
        })
        .filter(|origin| !road_project_blocks_city_footprint(save, *origin, size, kind))
        .filter(|origin| {
            !road_project_duplicates_existing_corridor(save, district, *origin, size, kind)
        })
        .max_by(|a, b| {
            let sa = score_planned_site(save, world, district, *a, size, kind);
            let sb = score_planned_site(save, world, district, *b, size, kind);
            sa.total_cmp(&sb)
        })
}

fn project_footprint_reserved(
    save: &BotWorldSave,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> bool {
    if !project_reserves_city_footprint(kind) {
        return false;
    }
    save.projects
        .iter()
        .filter(|project| project_reserves_city_footprint(project.kind))
        .filter(|project| !matches!(project.status, BotProjectStatus::Blocked))
        .any(|project| project_footprints_overlap(project.origin, project.size, origin, size, 2))
}

fn project_reserves_city_footprint(kind: BotTaskKind) -> bool {
    !matches!(
        kind,
        BotTaskKind::BuildRoad
            | BotTaskKind::RecolorRoad
            | BotTaskKind::ExpandRoadGrid
            | BotTaskKind::DecorateStreet
            | BotTaskKind::AddLights
            | BotTaskKind::ClearFlatten
    )
}

fn project_footprints_overlap(
    a_origin: [i32; 3],
    a_size: [i32; 3],
    b_origin: [i32; 3],
    b_size: [i32; 3],
    padding: i32,
) -> bool {
    let a_min_x = a_origin[0] - padding;
    let a_max_x = a_origin[0] + a_size[0].max(1) - 1 + padding;
    let a_min_z = a_origin[2] - padding;
    let a_max_z = a_origin[2] + a_size[2].max(1) - 1 + padding;
    let b_min_x = b_origin[0];
    let b_max_x = b_origin[0] + b_size[0].max(1) - 1;
    let b_min_z = b_origin[2];
    let b_max_z = b_origin[2] + b_size[2].max(1) - 1;
    a_min_x <= b_max_x && a_max_x >= b_min_x && a_min_z <= b_max_z && a_max_z >= b_min_z
}

fn project_footprint_blocks_road_corridor(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> bool {
    if is_road_project(kind)
        || project_is_semantic_roundabout_anchor(save, district, origin, size, kind)
    {
        return false;
    }
    road_corridor_segments(save, district)
        .into_iter()
        .any(|(a, b, half_width)| {
            road_segment_intersects_project_footprint(a, b, origin, size, half_width + 2.0)
        })
}

fn project_is_semantic_roundabout_anchor(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> bool {
    save.user_roads
        .iter()
        .filter(|guide| guide.shape == BotRoadGuideShape::Roundabout)
        .filter(|guide| road_guide_matches_district(guide, district))
        .filter(|guide| semantic_project_kind_for_guide(district, guide) == Some(kind))
        .map(road_guide_anchor)
        .any(|anchor| project_footprint_contains_xz(origin, size, Vec2::new(anchor.x, anchor.z)))
}

fn road_corridor_segments(save: &BotWorldSave, district: &BotDistrict) -> Vec<(Vec2, Vec2, f32)> {
    let mut segments = Vec::new();
    for guide in save
        .user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
    {
        let half_width = guide.width.max(1) as f32 * 0.5;
        segments.extend(
            road_guide_segments(guide)
                .into_iter()
                .map(|(a, b)| (a, b, half_width)),
        );
    }
    for pair in district.road_anchors.windows(2) {
        let a = Vec2::new(pair[0][0] as f32, pair[0][2] as f32);
        let b = Vec2::new(pair[1][0] as f32, pair[1][2] as f32);
        if a.distance_squared(b) > 1.0 {
            segments.push((a, b, 3.5));
        }
    }
    for project in &save.projects {
        if project.district_id != Some(district.id)
            || !is_access_road_project(project.kind)
            || matches!(project.status, BotProjectStatus::Blocked)
        {
            continue;
        }
        let half_width = project_road_corridor_half_width(project);
        segments.extend(
            project_road_segments(project)
                .into_iter()
                .map(|(a, b)| (a, b, half_width)),
        );
    }
    segments
}

fn project_road_corridor_half_width(project: &BotProject) -> f32 {
    match project.kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => project.size[2].max(1) as f32 * 0.5,
        BotTaskKind::ExpandRoadGrid => 5.5,
        _ => 3.5,
    }
}

fn road_segment_intersects_project_footprint(
    a: Vec2,
    b: Vec2,
    origin: [i32; 3],
    size: [i32; 3],
    padding: f32,
) -> bool {
    if a.distance_squared(b) <= 1.0 {
        return false;
    }
    let min_x = origin[0] as f32 - padding;
    let max_x = (origin[0] + size[0].max(1)) as f32 + padding;
    let min_z = origin[2] as f32 - padding;
    let max_z = (origin[2] + size[2].max(1)) as f32 + padding;
    if point_inside_project_rect(a, min_x, max_x, min_z, max_z)
        || point_inside_project_rect(b, min_x, max_x, min_z, max_z)
    {
        return true;
    }
    let nw = Vec2::new(min_x, min_z);
    let ne = Vec2::new(max_x, min_z);
    let se = Vec2::new(max_x, max_z);
    let sw = Vec2::new(min_x, max_z);
    segments_intersect(a, b, nw, ne)
        || segments_intersect(a, b, ne, se)
        || segments_intersect(a, b, se, sw)
        || segments_intersect(a, b, sw, nw)
}

fn point_inside_project_rect(point: Vec2, min_x: f32, max_x: f32, min_z: f32, max_z: f32) -> bool {
    point.x >= min_x && point.x <= max_x && point.y >= min_z && point.y <= max_z
}

fn project_footprint_contains_xz(origin: [i32; 3], size: [i32; 3], point: Vec2) -> bool {
    let min_x = origin[0] as f32;
    let max_x = (origin[0] + size[0].max(1)) as f32;
    let min_z = origin[2] as f32;
    let max_z = (origin[2] + size[2].max(1)) as f32;
    point_inside_project_rect(point, min_x, max_x, min_z, max_z)
}

fn road_project_blocks_city_footprint(
    save: &BotWorldSave,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> bool {
    if !is_access_road_project(kind) {
        return false;
    }
    let segments = planned_road_corridor_segments(origin, size, kind);
    if segments.is_empty() {
        return false;
    }
    save.projects
        .iter()
        .filter(|project| project_reserves_city_footprint(project.kind))
        .filter(|project| !matches!(project.status, BotProjectStatus::Blocked))
        .any(|project| {
            segments.iter().any(|(a, b, half_width)| {
                road_segment_intersects_project_footprint(
                    *a,
                    *b,
                    project.origin,
                    project.size,
                    half_width + 2.0,
                )
            })
        })
}

fn planned_road_corridor_segments(
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> Vec<(Vec2, Vec2, f32)> {
    let half_width = match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => size[2].max(1) as f32 * 0.5,
        BotTaskKind::ExpandRoadGrid => 5.5,
        _ => return Vec::new(),
    };
    planned_road_segments(origin, size, kind)
        .into_iter()
        .map(|(a, b)| (a, b, half_width))
        .collect()
}

fn road_project_duplicates_existing_corridor(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> bool {
    if !matches!(kind, BotTaskKind::BuildRoad | BotTaskKind::ExpandRoadGrid) {
        return false;
    }
    let planned = planned_road_corridor_segments(origin, size, kind);
    if planned.is_empty() {
        return false;
    }
    let existing = road_corridor_segments(save, district);
    planned.iter().any(|(a, b, planned_half_width)| {
        existing.iter().any(|(c, d, existing_half_width)| {
            road_segments_are_duplicate_corridors(
                *a,
                *b,
                *c,
                *d,
                planned_half_width + existing_half_width + 3.0,
            )
        })
    })
}

fn road_segments_are_duplicate_corridors(
    a: Vec2,
    b: Vec2,
    c: Vec2,
    d: Vec2,
    max_distance: f32,
) -> bool {
    let ab = b - a;
    let cd = d - c;
    let ab_len = ab.length();
    let cd_len = cd.length();
    if ab_len <= 1.0 || cd_len <= 1.0 {
        return false;
    }
    let alignment = (ab / ab_len).dot(cd / cd_len).abs();
    alignment >= 0.86 && segment_to_segment_distance(a, b, c, d) <= max_distance
}

fn semantic_road_site_origins(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    kind: BotTaskKind,
    size: [i32; 3],
) -> Vec<[i32; 3]> {
    let bounds = save.primary_bounds();
    save.user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
        .filter(|guide| semantic_project_kind_for_guide(district, guide) == Some(kind))
        .flat_map(|guide| semantic_road_lot_origins(world, bounds, guide, kind, size))
        .collect()
}

fn semantic_road_lot_origins(
    world: &VoxelWorld,
    bounds: BotCityBounds,
    guide: &BotRoadGuide,
    kind: BotTaskKind,
    size: [i32; 3],
) -> Vec<[i32; 3]> {
    if guide.shape == BotRoadGuideShape::Corner && project_uses_street_face(kind) {
        let centers = semantic_corner_frontage_centers(guide, size);
        if !centers.is_empty() {
            return centers
                .into_iter()
                .map(|center| {
                    let origin = project_origin_from_center(world, bounds, center, size);
                    align_lot_origin_to_guide_grade(guide, origin, size)
                })
                .collect();
        }
    }
    let origin = project_origin_from_center(world, bounds, road_guide_anchor(guide), size);
    vec![align_lot_origin_to_guide_grade(guide, origin, size)]
}

fn semantic_corner_frontage_centers(guide: &BotRoadGuide, size: [i32; 3]) -> Vec<Vec3> {
    let anchor = road_guide_anchor(guide);
    let setback_x = size[0].max(1) as f32 * 0.5 + guide.width.max(1) as f32 * 0.5 + 8.0;
    let setback_z = size[2].max(1) as f32 * 0.5 + guide.width.max(1) as f32 * 0.5 + 8.0;
    let mut centers = Vec::with_capacity(4);
    for (sx, sz) in semantic_corner_quadrant_order(guide) {
        let center = Vec3::new(
            anchor.x + sx as f32 * setback_x,
            anchor.y,
            anchor.z + sz as f32 * setback_z,
        );
        if centers.iter().all(|existing: &Vec3| {
            Vec2::new(existing.x, existing.z).distance_squared(Vec2::new(center.x, center.z)) > 4.0
        }) {
            centers.push(center);
        }
    }
    centers
}

fn semantic_corner_quadrant_order(guide: &BotRoadGuide) -> Vec<(i32, i32)> {
    let anchor = road_guide_anchor(guide);
    let anchor_xz = Vec2::new(anchor.x, anchor.z);
    let mut quadrants: Vec<(i32, i32, f32)> = [(-1, -1), (-1, 1), (1, -1), (1, 1)]
        .into_iter()
        .map(|(sx, sz)| {
            let probe = anchor_xz + Vec2::new(sx as f32, sz as f32);
            let nearest = road_guide_segments(guide)
                .into_iter()
                .map(|(a, b)| point_to_segment_distance(probe, a, b))
                .fold(f32::INFINITY, f32::min);
            (sx, sz, nearest)
        })
        .collect();
    quadrants.sort_by(|a, b| b.2.total_cmp(&a.2));
    quadrants.into_iter().map(|(sx, sz, _)| (sx, sz)).collect()
}

fn district_road_origins(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    size: [i32; 3],
) -> Vec<[i32; 3]> {
    let bounds = save.primary_bounds();
    let center = vec3_from_arr(district.center);
    let mut centers = vec![center];
    if let Some(hub) = save.settlements.first().map(|s| vec3_from_arr(s.hub)) {
        centers.push(hub.lerp(center, 0.50));
        centers.push(hub.lerp(center, 0.74));
    }
    centers
        .into_iter()
        .map(|center| project_origin_from_center(world, bounds, center, size))
        .collect()
}

fn roadside_lot_origins(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    kind: BotTaskKind,
    size: [i32; 3],
    seq: usize,
) -> Vec<[i32; 3]> {
    let bounds = save.primary_bounds();
    let lot_spacing = match kind {
        BotTaskKind::BuildGlassTower | BotTaskKind::BuildTower | BotTaskKind::MakeTaller => 32.0,
        BotTaskKind::BuildResidentialBlock => 34.0,
        BotTaskKind::BuildHome => 18.0,
        BotTaskKind::BuildPark | BotTaskKind::BuildPlaza => 28.0,
        _ => 24.0,
    };
    let dirs = [
        Vec2::new(1.0, 0.0),
        Vec2::new(-1.0, 0.0),
        Vec2::new(0.0, 1.0),
        Vec2::new(0.0, -1.0),
    ];
    let mut out = Vec::new();
    let road_segments: Vec<(usize, Vec2, Vec2, f32)> = road_network_segments(save, district)
        .into_iter()
        .enumerate()
        .filter_map(|(segment_idx, (a, b))| {
            let length = (b - a).length();
            (length >= 4.0).then_some((segment_idx, a, b, length))
        })
        .collect();
    let total_arc_length: f32 = road_segments.iter().map(|(_, _, _, length)| *length).sum();
    let total_samples = ((total_arc_length / lot_spacing).floor() as usize).min(48);
    for sample_idx in 0..total_samples {
        let target_distance =
            total_arc_length * (sample_idx + 1) as f32 / (total_samples + 1) as f32;
        let mut distance_before = 0.0;
        let Some((segment_idx, a, b, length, local_t)) =
            road_segments
                .iter()
                .find_map(|(segment_idx, a, b, length)| {
                    let next_distance = distance_before + *length;
                    let hit = target_distance <= next_distance;
                    let local_t = ((target_distance - distance_before) / *length).clamp(0.0, 1.0);
                    distance_before = next_distance;
                    hit.then_some((*segment_idx, *a, *b, *length, local_t))
                })
        else {
            continue;
        };
        let span = b - a;
        let dir = span / length;
        let normal = Vec2::new(-dir.y, dir.x);
        let base = a + span * local_t;
        for (side_idx, side) in [-1.0_f32, 1.0].into_iter().enumerate() {
            let stagger_seed = seq + segment_idx * 7 + sample_idx * 3 + side_idx;
            let stagger = (stagger_seed % 5) as f32 * 4.0 - 8.0;
            let center = base + normal * side * lot_spacing + dir * stagger;
            let origin =
                project_origin_from_center(world, bounds, Vec3::new(center.x, 0.0, center.y), size);
            out.push(align_lot_origin_to_road_grade(
                save, world, district, origin, size,
            ));
        }
    }
    for (idx, point) in road_network_points(save, district)
        .into_iter()
        .take(18)
        .enumerate()
    {
        for turn in 0..dirs.len() {
            let dir = dirs[(seq + idx + turn) % dirs.len()];
            let stagger = ((seq + idx * 3 + turn) % 3) as f32 * 8.0 - 8.0;
            let center = Vec3::new(
                point.x + dir.x * lot_spacing - dir.y * stagger,
                0.0,
                point.y + dir.y * lot_spacing + dir.x * stagger,
            );
            let origin = project_origin_from_center(world, bounds, center, size);
            out.push(align_lot_origin_to_road_grade(
                save, world, district, origin, size,
            ));
        }
    }
    reserve_roundabout_interiors_for_lots(save, district, size, out)
}

fn align_lot_origin_to_road_grade(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
) -> [i32; 3] {
    let Some(deck_y) = nearest_road_grade_y(save, world, district, origin, size) else {
        return origin;
    };
    let mut aligned = origin;
    aligned[1] = aligned[1].max(deck_y);
    aligned
}

fn align_lot_origin_to_guide_grade(
    guide: &BotRoadGuide,
    origin: [i32; 3],
    size: [i32; 3],
) -> [i32; 3] {
    let Some((_, deck_y)) = nearest_road_guide_grade_sample(guide, origin, size) else {
        return origin;
    };
    let deck_y = semantic_road_guide_deck_y(guide)
        .map(|semantic_y| deck_y.max(semantic_y as f32))
        .unwrap_or(deck_y);
    let mut aligned = origin;
    aligned[1] = aligned[1].max(deck_y.round() as i32);
    aligned
}

fn semantic_road_guide_deck_y(guide: &BotRoadGuide) -> Option<i32> {
    match guide.shape {
        BotRoadGuideShape::Corner | BotRoadGuideShape::Roundabout => {
            guide.points.iter().map(|point| point[1]).max()
        }
        BotRoadGuideShape::Straight => None,
    }
}

fn nearest_user_road_grade_sample(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for guide in save
        .user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
    {
        if let Some((distance, y)) = nearest_road_guide_grade_sample(guide, origin, size) {
            if best.map_or(true, |(best_distance, _)| distance < best_distance) {
                best = Some((distance, y));
            }
        }
    }
    best
}

fn nearest_road_grade_y(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<i32> {
    let mut best: Option<(f32, f32)> = nearest_user_road_grade_sample(save, district, origin, size);
    for project in &save.projects {
        if project.district_id != Some(district.id)
            || !is_access_road_project(project.kind)
            || matches!(project.status, BotProjectStatus::Blocked)
        {
            continue;
        }
        if let Some((distance, y)) = nearest_road_project_grade_sample(world, project, origin, size)
        {
            if best.map_or(true, |(best_distance, _)| distance < best_distance) {
                best = Some((distance, y));
            }
        }
    }
    best.map(|(_, y)| y.round() as i32)
}

fn nearest_road_project_grade_sample(
    world: &VoxelWorld,
    project: &BotProject,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(f32, f32)> {
    let probes = building_frontage_grade_probes(origin, size);
    let reach = 34.0 + project_road_corridor_half_width(project) * 2.0;
    let mut best: Option<(f32, f32)> = None;
    for (a, b) in project_road_segments(project) {
        let ab = b - a;
        let len_sq = ab.length_squared();
        if len_sq <= f32::EPSILON {
            continue;
        }
        for point in &probes {
            let t = ((*point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
            let nearest = a + ab * t;
            let distance = point.distance(nearest);
            if distance > reach {
                continue;
            }
            let y = road_project_grade_y_at(world, project, nearest);
            if best.map_or(true, |(best_distance, _)| distance < best_distance) {
                best = Some((distance, y));
            }
        }
    }
    best
}

fn road_project_grade_y_at(world: &VoxelWorld, project: &BotProject, point: Vec2) -> f32 {
    match project.kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid => {
            road_grade_y(world, point.x.round() as i32, point.y.round() as i32, true) as f32
        }
        _ => project.origin[1] as f32,
    }
}

fn nearest_road_guide_grade_sample(
    guide: &BotRoadGuide,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(f32, f32)> {
    let probes = building_frontage_grade_probes(origin, size);
    let reach = 34.0 + guide.width.max(1) as f32 * 3.0;
    let mut best: Option<(f32, f32)> = None;
    for pair in guide.points.windows(2) {
        let a = Vec2::new(pair[0][0] as f32, pair[0][2] as f32);
        let b = Vec2::new(pair[1][0] as f32, pair[1][2] as f32);
        let ab = b - a;
        let len_sq = ab.length_squared();
        if len_sq <= f32::EPSILON {
            continue;
        }
        for point in &probes {
            let t = ((*point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
            let nearest = a + ab * t;
            let distance = point.distance(nearest);
            if distance > reach {
                continue;
            }
            let y = pair[0][1] as f32 + (pair[1][1] - pair[0][1]) as f32 * t;
            if best.map_or(true, |(best_distance, _)| distance < best_distance) {
                best = Some((distance, y));
            }
        }
    }
    best
}

fn building_frontage_grade_probes(origin: [i32; 3], size: [i32; 3]) -> Vec<Vec2> {
    let width = size[0].max(1) as f32;
    let depth = size[2].max(1) as f32;
    let min_x = origin[0] as f32;
    let min_z = origin[2] as f32;
    let max_x = min_x + width - 1.0;
    let max_z = min_z + depth - 1.0;
    let center_x = min_x + width * 0.5;
    let center_z = min_z + depth * 0.5;
    let mut probes = vec![Vec2::new(center_x, center_z)];
    for t in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
        let x = min_x + (max_x - min_x) * t;
        let z = min_z + (max_z - min_z) * t;
        push_unique_probe(&mut probes, Vec2::new(x, min_z));
        push_unique_probe(&mut probes, Vec2::new(x, max_z));
        push_unique_probe(&mut probes, Vec2::new(min_x, z));
        push_unique_probe(&mut probes, Vec2::new(max_x, z));
    }
    probes
}

fn push_unique_probe(probes: &mut Vec<Vec2>, point: Vec2) {
    if probes
        .iter()
        .all(|existing| existing.distance_squared(point) > 0.25)
    {
        probes.push(point);
    }
}

fn reserve_roundabout_interiors_for_lots(
    save: &BotWorldSave,
    district: &BotDistrict,
    size: [i32; 3],
    candidates: Vec<[i32; 3]>,
) -> Vec<[i32; 3]> {
    let reservations = roundabout_lot_reservations(save, district, size);
    if reservations.is_empty() {
        return candidates;
    }
    candidates
        .into_iter()
        .filter(|origin| {
            let center = Vec2::new(
                origin[0] as f32 + size[0].max(1) as f32 * 0.5,
                origin[2] as f32 + size[2].max(1) as f32 * 0.5,
            );
            reservations
                .iter()
                .all(|(anchor, min_distance)| center.distance(*anchor) >= *min_distance)
        })
        .collect()
}

fn roundabout_lot_reservations(
    save: &BotWorldSave,
    district: &BotDistrict,
    size: [i32; 3],
) -> Vec<(Vec2, f32)> {
    save.user_roads
        .iter()
        .filter(|guide| guide.shape == BotRoadGuideShape::Roundabout)
        .filter(|guide| road_guide_matches_district(guide, district))
        .map(|guide| {
            let anchor = road_guide_anchor(guide);
            let footprint_half = size[0].max(size[2]).max(1) as f32 * 0.5;
            let min_distance = roundabout_guide_radius(guide)
                + guide.width.max(1) as f32 * 0.5
                + footprint_half
                + 4.0;
            (Vec2::new(anchor.x, anchor.z), min_distance)
        })
        .collect()
}

fn roundabout_guide_radius(guide: &BotRoadGuide) -> f32 {
    let anchor = road_guide_anchor(guide);
    guide
        .points
        .iter()
        .map(|point| {
            Vec2::new(point[0] as f32, point[2] as f32).distance(Vec2::new(anchor.x, anchor.z))
        })
        .fold(0.0, f32::max)
        .max(4.0)
}

fn project_origin_from_center(
    world: &VoxelWorld,
    bounds: BotCityBounds,
    center: Vec3,
    size: [i32; 3],
) -> [i32; 3] {
    let half_x = size[0] as f32 * 0.5;
    let half_z = size[2] as f32 * 0.5;
    let target = clamp_to_bounds(
        bounds,
        Vec3::new(center.x - half_x, center.y, center.z - half_z),
    );
    project_origin(world, target)
}

fn sync_user_city_roads(save: &mut BotWorldSave, roads: &[crate::city::RoadSegment]) -> bool {
    let guides: Vec<BotRoadGuide> = roads
        .iter()
        .filter_map(|road| bot_road_guide_from_city_component(save, road))
        .collect();
    if save.user_roads == guides {
        false
    } else {
        save.user_roads = guides;
        true
    }
}

fn bot_road_guide_from_city_component(
    save: &BotWorldSave,
    road: &crate::city::RoadSegment,
) -> Option<BotRoadGuide> {
    let points = sampled_city_road_points(road);
    if points.len() < 2 {
        return None;
    }
    let center = road_guide_center(&points);
    Some(BotRoadGuide {
        district_id: nearest_district(save, center).map(|district| district.id),
        points,
        anchor: semantic_anchor_from_city_road(road),
        width: road.width,
        theme: road_style_bot_theme(road.style),
        shape: road_shape_bot_guide(road.shape),
    })
}

fn semantic_anchor_from_city_road(road: &crate::city::RoadSegment) -> Option<[i32; 3]> {
    match road.shape {
        crate::city::RoadShape::Corner => {
            let via = road
                .via
                .unwrap_or_else(|| IVec3::new(road.b.x, road.a.y, road.a.z));
            Some([via.x, via.y, via.z])
        }
        crate::city::RoadShape::Roundabout => Some([road.a.x, road.a.y, road.a.z]),
        crate::city::RoadShape::Straight => None,
    }
}

fn road_style_bot_theme(style: crate::city::RoadStyle) -> BotTheme {
    match style {
        crate::city::RoadStyle::Neon => BotTheme::MagentaGlass,
        crate::city::RoadStyle::Cobble => BotTheme::WhiteAlloy,
        crate::city::RoadStyle::Dirt => BotTheme::GreenPark,
        crate::city::RoadStyle::Asphalt => BotTheme::AmberStreet,
    }
}

fn road_shape_bot_guide(shape: crate::city::RoadShape) -> BotRoadGuideShape {
    match shape {
        crate::city::RoadShape::Straight => BotRoadGuideShape::Straight,
        crate::city::RoadShape::Corner => BotRoadGuideShape::Corner,
        crate::city::RoadShape::Roundabout => BotRoadGuideShape::Roundabout,
    }
}

fn sampled_city_road_points(road: &crate::city::RoadSegment) -> Vec<[i32; 3]> {
    let cells = crate::city::road_component_centerline_samples(road);
    let sample_step = if road.shape == crate::city::RoadShape::Roundabout {
        6
    } else {
        12
    };
    let last = cells.len().saturating_sub(1);
    let mut points = Vec::new();
    for (idx, cell) in cells.into_iter().enumerate() {
        if idx == 0 || idx == last || idx % sample_step == 0 {
            push_road_guide_point(&mut points, [cell.x, cell.y, cell.z]);
        }
    }
    points
}

fn push_road_guide_point(points: &mut Vec<[i32; 3]>, point: [i32; 3]) {
    if points.last().copied() != Some(point) {
        points.push(point);
    }
}

fn road_guide_center(points: &[[i32; 3]]) -> Vec3 {
    if points.is_empty() {
        return Vec3::ZERO;
    }
    let inv = 1.0 / points.len() as f32;
    let mut sum = Vec3::ZERO;
    for point in points {
        sum += Vec3::new(point[0] as f32, point[1] as f32, point[2] as f32);
    }
    sum * inv
}

fn road_guide_anchor(guide: &BotRoadGuide) -> Vec3 {
    if let Some(anchor) = guide.anchor {
        return Vec3::new(anchor[0] as f32, anchor[1] as f32, anchor[2] as f32);
    }
    match guide.shape {
        BotRoadGuideShape::Corner => guide
            .points
            .get(guide.points.len() / 2)
            .map(|point| Vec3::new(point[0] as f32, point[1] as f32, point[2] as f32))
            .unwrap_or_else(|| road_guide_center(&guide.points)),
        BotRoadGuideShape::Roundabout => road_guide_bounds_center(&guide.points),
        BotRoadGuideShape::Straight => road_guide_center(&guide.points),
    }
}

fn road_guide_bounds_center(points: &[[i32; 3]]) -> Vec3 {
    let Some(first) = points.first() else {
        return Vec3::ZERO;
    };
    let mut min_x = first[0];
    let mut max_x = first[0];
    let mut min_z = first[2];
    let mut max_z = first[2];
    let mut sum_y = 0.0;
    for point in points {
        min_x = min_x.min(point[0]);
        max_x = max_x.max(point[0]);
        min_z = min_z.min(point[2]);
        max_z = max_z.max(point[2]);
        sum_y += point[1] as f32;
    }
    Vec3::new(
        (min_x + max_x) as f32 * 0.5,
        sum_y / points.len().max(1) as f32,
        (min_z + max_z) as f32 * 0.5,
    )
}

fn road_guide_matches_district(guide: &BotRoadGuide, district: &BotDistrict) -> bool {
    if guide.district_id == Some(district.id) {
        return true;
    }
    let center = Vec2::new(district.center[0], district.center[2]);
    let reach = district.radius as f32 + 96.0;
    guide
        .points
        .iter()
        .any(|point| Vec2::new(point[0] as f32, point[2] as f32).distance(center) <= reach)
}

fn road_guide_segments(guide: &BotRoadGuide) -> Vec<(Vec2, Vec2)> {
    guide
        .points
        .windows(2)
        .filter_map(|pair| {
            let a = Vec2::new(pair[0][0] as f32, pair[0][2] as f32);
            let b = Vec2::new(pair[1][0] as f32, pair[1][2] as f32);
            (a.distance_squared(b) > 1.0).then_some((a, b))
        })
        .collect()
}

fn road_guide_length(guide: &BotRoadGuide) -> f32 {
    road_guide_segments(guide)
        .into_iter()
        .map(|(a, b)| a.distance(b))
        .sum()
}

fn semantic_user_roads_by_intent<'a>(
    save: &'a BotWorldSave,
    district: &BotDistrict,
) -> Vec<&'a BotRoadGuide> {
    let mut guides: Vec<&BotRoadGuide> = save
        .user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
        .filter(|guide| guide.shape != BotRoadGuideShape::Straight)
        .collect();
    guides.sort_by(|a, b| {
        let sa = road_shape_intent_score(a);
        let sb = road_shape_intent_score(b);
        sb.total_cmp(&sa)
    });
    guides
}

fn road_shape_intent_score(guide: &BotRoadGuide) -> f32 {
    let shape_weight = match guide.shape {
        BotRoadGuideShape::Roundabout => 3.0,
        BotRoadGuideShape::Corner => 2.0,
        BotRoadGuideShape::Straight => 0.0,
    };
    shape_weight * 10_000.0 + road_guide_length(guide) * guide.width.max(1) as f32
}

fn road_network_points(save: &BotWorldSave, district: &BotDistrict) -> Vec<Vec2> {
    let mut points: Vec<Vec2> = district
        .road_anchors
        .iter()
        .map(|a| Vec2::new(a[0] as f32, a[2] as f32))
        .collect();
    for guide in save
        .user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
    {
        points.extend(
            guide
                .points
                .iter()
                .map(|point| Vec2::new(point[0] as f32, point[2] as f32)),
        );
    }
    for project in &save.projects {
        if project.district_id != Some(district.id)
            || !is_access_road_project(project.kind)
            || matches!(project.status, BotProjectStatus::Blocked)
        {
            continue;
        }
        let mut had_segment = false;
        for (a, b) in project_road_segments(project) {
            had_segment = true;
            points.push(a);
            points.push((a + b) * 0.5);
            points.push(b);
        }
        if !had_segment {
            let center = project_center(project.origin, project.size);
            points.push(Vec2::new(center.x, center.z));
        }
    }
    if points.is_empty() {
        let center = vec3_from_arr(district.center);
        points.push(Vec2::new(center.x, center.z));
    }
    points
}

fn project_road_segments(project: &BotProject) -> Vec<(Vec2, Vec2)> {
    planned_road_segments(project.origin, project.size, project.kind)
}

fn planned_road_segments(origin: [i32; 3], size: [i32; 3], kind: BotTaskKind) -> Vec<(Vec2, Vec2)> {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => {
            build_road_centerline_segments(origin, size)
        }
        BotTaskKind::ExpandRoadGrid => expand_road_grid_segments(origin, size),
        _ => Vec::new(),
    }
}

fn expand_road_grid_segments(origin: [i32; 3], size: [i32; 3]) -> Vec<(Vec2, Vec2)> {
    let mut segments = Vec::new();
    let width = size[0].max(1);
    let depth = size[2].max(1);
    for target_plan_x in road_grid_targets(width, width / 2) {
        add_grid_road_segments_along_z(origin, size, target_plan_x, &mut segments);
    }
    for target_plan_z in road_grid_targets(depth, depth / 2) {
        add_grid_road_segments_along_x(origin, size, target_plan_z, &mut segments);
    }
    segments
}

fn road_grid_targets(size: i32, mid: i32) -> Vec<i32> {
    let mut targets = Vec::new();
    let mut target = 2;
    while target < size {
        targets.push(target);
        target += 28;
    }
    targets.push(mid);
    targets.sort_unstable();
    targets.dedup();
    targets
}

fn add_grid_road_segments_along_z(
    origin: [i32; 3],
    size: [i32; 3],
    target_plan_x: i32,
    segments: &mut Vec<(Vec2, Vec2)>,
) {
    let depth = size[2].max(1);
    let width = size[0].max(1);
    let mut prev: Option<Vec2> = None;
    for local_z in road_grid_sample_axis(depth) {
        let local_x = target_plan_x - road_grid_bend_x(origin, local_z);
        let point = if (0..width).contains(&local_x) {
            Some(Vec2::new(
                origin[0] as f32 + local_x as f32,
                origin[2] as f32 + local_z as f32,
            ))
        } else {
            None
        };
        push_optional_road_segment(&mut prev, point, segments);
    }
}

fn add_grid_road_segments_along_x(
    origin: [i32; 3],
    size: [i32; 3],
    target_plan_z: i32,
    segments: &mut Vec<(Vec2, Vec2)>,
) {
    let width = size[0].max(1);
    let depth = size[2].max(1);
    let mut prev: Option<Vec2> = None;
    for local_x in road_grid_sample_axis(width) {
        let local_z = target_plan_z - road_grid_bend_z(origin, local_x);
        let point = if (0..depth).contains(&local_z) {
            Some(Vec2::new(
                origin[0] as f32 + local_x as f32,
                origin[2] as f32 + local_z as f32,
            ))
        } else {
            None
        };
        push_optional_road_segment(&mut prev, point, segments);
    }
}

fn road_grid_sample_axis(size: i32) -> Vec<i32> {
    let last = (size - 1).max(0);
    let mut samples = Vec::new();
    let mut pos = 0;
    while pos < last {
        samples.push(pos);
        pos = (pos + 12).min(last);
    }
    samples.push(last);
    samples.sort_unstable();
    samples.dedup();
    samples
}

fn push_optional_road_segment(
    prev: &mut Option<Vec2>,
    point: Option<Vec2>,
    segments: &mut Vec<(Vec2, Vec2)>,
) {
    if let Some(point) = point {
        if let Some(previous) = *prev {
            if previous.distance_squared(point) > 1.0 {
                segments.push((previous, point));
            }
        }
        *prev = Some(point);
    } else {
        *prev = None;
    }
}

fn road_grid_bend_x(origin: [i32; 3], local_z: i32) -> i32 {
    road_grid_bend_x_from_origin(origin[0], local_z)
}

fn road_grid_bend_z(origin: [i32; 3], local_x: i32) -> i32 {
    road_grid_bend_z_from_origin(origin[2], local_x)
}

fn road_grid_bend_x_from_origin(origin_x: i32, local_z: i32) -> i32 {
    ((local_z as f32 * 0.095 + origin_x as f32 * 0.017).sin() * 4.0).round() as i32
}

fn road_grid_bend_z_from_origin(origin_z: i32, local_x: i32) -> i32 {
    ((local_x as f32 * 0.083 + origin_z as f32 * 0.013).sin() * 4.0).round() as i32
}

#[derive(Debug, Clone, Copy, Default)]
struct RoadGridProfile {
    road_x: bool,
    road_z: bool,
    sidewalk_x: bool,
    sidewalk_z: bool,
    lane: bool,
    crosswalk: bool,
    intersection: bool,
    intersection_corner: bool,
    road_like: bool,
    boulevard: bool,
    roundabout: bool,
    roundabout_center: bool,
    median: bool,
    structural_edge: bool,
}

fn road_grid_profile(origin: [i32; 3], size: [i32; 3], local: IVec3) -> RoadGridProfile {
    let width = size[0].max(1);
    let depth = size[2].max(1);
    let mid_x = width / 2;
    let mid_z = depth / 2;
    let bend_x = road_grid_bend_x_from_origin(origin[0], local.z);
    let bend_z = road_grid_bend_z_from_origin(origin[2], local.x);
    let plan_x = local.x + bend_x;
    let plan_z = local.z + bend_z;
    let cell_x = plan_x.rem_euclid(28);
    let cell_z = plan_z.rem_euclid(28);
    let boulevard_x = (plan_x - mid_x).abs() <= 5;
    let boulevard_z = (plan_z - mid_z).abs() <= 5;
    let boulevard_sidewalk_x = (plan_x - mid_x).abs() == 6;
    let boulevard_sidewalk_z = (plan_z - mid_z).abs() == 6;
    let boulevard_dx = plan_x - mid_x;
    let boulevard_dz = plan_z - mid_z;
    let boulevard_center_dist2 = boulevard_dx * boulevard_dx + boulevard_dz * boulevard_dz;
    let roundabout = boulevard_x && boulevard_z && boulevard_center_dist2 <= 36;
    let roundabout_island = roundabout && boulevard_center_dist2 <= 8;
    let roundabout_center = roundabout && boulevard_center_dist2 <= 1;
    let road_x = cell_x <= 5 || boulevard_x;
    let road_z = cell_z <= 5 || boulevard_z;
    let sidewalk_x = cell_x == 6 || cell_x == 27 || boulevard_sidewalk_x;
    let sidewalk_z = cell_z == 6 || cell_z == 27 || boulevard_sidewalk_z;
    let intersection = road_x && road_z;
    let crosswalk = intersection
        && (cell_x == 4
            || cell_z == 4
            || (plan_x - mid_x).abs() == 3
            || (plan_z - mid_z).abs() == 3);
    let intersection_corner = (sidewalk_x && sidewalk_z)
        || ((local.x - mid_x).abs() == 5 && (local.z - mid_z).abs() == 5);
    let boulevard = boulevard_x || boulevard_z;
    let median = roundabout_island
        || (!intersection
            && ((boulevard_x && (plan_x - mid_x).abs() <= 1)
                || (boulevard_z && (plan_z - mid_z).abs() <= 1)));
    let road_like = road_x || road_z || sidewalk_x || sidewalk_z || intersection_corner;
    let structural_edge =
        sidewalk_x || sidewalk_z || intersection_corner || boulevard || roundabout;
    let lane = !median
        && ((road_x && (cell_x == 2 || boulevard_x) && plan_z.rem_euclid(12) < 5)
            || (road_z && (cell_z == 2 || boulevard_z) && plan_x.rem_euclid(12) < 5)
            || (roundabout && boulevard_center_dist2 >= 12));
    RoadGridProfile {
        road_x,
        road_z,
        sidewalk_x,
        sidewalk_z,
        lane,
        crosswalk,
        intersection,
        intersection_corner,
        road_like,
        boulevard,
        roundabout,
        roundabout_center,
        median,
        structural_edge,
    }
}

fn build_road_centerline_segments(origin: [i32; 3], size: [i32; 3]) -> Vec<(Vec2, Vec2)> {
    let length = size[0].max(1);
    let last_x = (length - 1).max(0);
    let step = 12;
    let mut segments = Vec::new();
    let mut prev_x = 0;
    let mut prev = build_road_centerline_point(origin, size, prev_x);
    while prev_x < last_x {
        let next_x = (prev_x + step).min(last_x);
        let next = build_road_centerline_point(origin, size, next_x);
        if prev.distance_squared(next) > 1.0 {
            segments.push((prev, next));
        }
        prev_x = next_x;
        prev = next;
    }
    segments
}

fn build_road_centerline_point(origin: [i32; 3], size: [i32; 3], local_x: i32) -> Vec2 {
    let width = size[2].max(1);
    Vec2::new(
        origin[0] as f32 + local_x as f32,
        build_road_center_z(origin[2], width, local_x) as f32,
    )
}

fn build_road_center_z(origin_z: i32, width: i32, local_x: i32) -> i32 {
    origin_z + width.max(1) / 2 + ((local_x as f32 * 0.16).sin() * 2.0).round() as i32
}

fn road_network_segments(save: &BotWorldSave, district: &BotDistrict) -> Vec<(Vec2, Vec2)> {
    let mut segments = Vec::new();
    for guide in save
        .user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
    {
        segments.extend(road_guide_segments(guide));
    }
    for pair in district.road_anchors.windows(2) {
        let a = Vec2::new(pair[0][0] as f32, pair[0][2] as f32);
        let b = Vec2::new(pair[1][0] as f32, pair[1][2] as f32);
        if a.distance_squared(b) > 1.0 {
            segments.push((a, b));
        }
    }
    for project in &save.projects {
        if project.district_id != Some(district.id)
            || !is_access_road_project(project.kind)
            || matches!(project.status, BotProjectStatus::Blocked)
        {
            continue;
        }
        segments.extend(project_road_segments(project));
    }
    if segments.is_empty() {
        let center = vec3_from_arr(district.center);
        let c = Vec2::new(center.x, center.z);
        segments.push((c, c));
    }
    segments
}

fn road_access_score(save: &BotWorldSave, district: &BotDistrict, x: i32, z: i32) -> f32 {
    let here = Vec2::new(x as f32, z as f32);
    let best = road_network_points(save, district)
        .into_iter()
        .map(|p| p.distance(here))
        .fold(f32::INFINITY, f32::min);
    (1.0 - best / 140.0).clamp(0.0, 1.0)
}

fn point_to_segment_distance(point: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    if len_sq <= f32::EPSILON {
        return point.distance(a);
    }
    let t = ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    point.distance(a + ab * t)
}

fn cross_2d(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

fn point_on_segment(point: Vec2, a: Vec2, b: Vec2) -> bool {
    let eps = 0.001;
    cross_2d(point - a, b - a).abs() <= eps
        && point.x >= a.x.min(b.x) - eps
        && point.x <= a.x.max(b.x) + eps
        && point.y >= a.y.min(b.y) - eps
        && point.y <= a.y.max(b.y) + eps
}

fn segments_intersect(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> bool {
    let ab = b - a;
    let cd = d - c;
    let o1 = cross_2d(ab, c - a);
    let o2 = cross_2d(ab, d - a);
    let o3 = cross_2d(cd, a - c);
    let o4 = cross_2d(cd, b - c);
    let eps = 0.001;
    if ((o1 > eps && o2 < -eps) || (o1 < -eps && o2 > eps))
        && ((o3 > eps && o4 < -eps) || (o3 < -eps && o4 > eps))
    {
        return true;
    }
    point_on_segment(c, a, b)
        || point_on_segment(d, a, b)
        || point_on_segment(a, c, d)
        || point_on_segment(b, c, d)
}

fn segment_to_segment_distance(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f32 {
    if segments_intersect(a, b, c, d) {
        return 0.0;
    }
    point_to_segment_distance(a, c, d)
        .min(point_to_segment_distance(b, c, d))
        .min(point_to_segment_distance(c, a, b))
        .min(point_to_segment_distance(d, a, b))
}

fn segment_intersection_point(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> Option<Vec2> {
    let ab = b - a;
    let cd = d - c;
    let denom = cross_2d(ab, cd);
    if denom.abs() <= 0.001 {
        return None;
    }
    let ca = c - a;
    let t = cross_2d(ca, cd) / denom;
    let u = cross_2d(ca, ab) / denom;
    let eps = 0.001;
    if t >= -eps && t <= 1.0 + eps && u >= -eps && u <= 1.0 + eps {
        Some(a + ab * t.clamp(0.0, 1.0))
    } else {
        None
    }
}

fn road_intersection_points(segments: &[(Vec2, Vec2)]) -> Vec<Vec2> {
    let mut points = Vec::new();
    for (idx, (a, b)) in segments.iter().enumerate() {
        if a.distance_squared(*b) <= 1.0 {
            continue;
        }
        for (c, d) in segments.iter().skip(idx + 1) {
            if c.distance_squared(*d) <= 1.0 {
                continue;
            }
            if let Some(point) = segment_intersection_point(*a, *b, *c, *d) {
                if points
                    .iter()
                    .all(|existing: &Vec2| existing.distance_squared(point) > 9.0)
                {
                    points.push(point);
                }
            }
        }
    }
    points
}

fn building_edge_segments(
    origin: [i32; 3],
    size: [i32; 3],
) -> [(BuildingStreetFace, Vec2, Vec2); 4] {
    let min_x = origin[0] as f32;
    let max_x = (origin[0] + size[0].max(1)) as f32;
    let min_z = origin[2] as f32;
    let max_z = (origin[2] + size[2].max(1)) as f32;
    [
        (
            BuildingStreetFace::North,
            Vec2::new(min_x, min_z),
            Vec2::new(max_x, min_z),
        ),
        (
            BuildingStreetFace::South,
            Vec2::new(min_x, max_z),
            Vec2::new(max_x, max_z),
        ),
        (
            BuildingStreetFace::West,
            Vec2::new(min_x, min_z),
            Vec2::new(min_x, max_z),
        ),
        (
            BuildingStreetFace::East,
            Vec2::new(max_x, min_z),
            Vec2::new(max_x, max_z),
        ),
    ]
}

fn building_edge_distance_to_point(point: Vec2, origin: [i32; 3], size: [i32; 3]) -> f32 {
    building_edge_segments(origin, size)
        .iter()
        .map(|(_, a, b)| point_to_segment_distance(point, *a, *b))
        .fold(f32::INFINITY, f32::min)
}

fn nearest_road_building_edge(
    road_segments: &[(Vec2, Vec2)],
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(BuildingStreetFace, f32)> {
    let edges = building_edge_segments(origin, size);
    road_segments
        .iter()
        .flat_map(|(road_a, road_b)| {
            edges.iter().map(move |(face, edge_a, edge_b)| {
                (
                    *face,
                    segment_to_segment_distance(*edge_a, *edge_b, *road_a, *road_b),
                )
            })
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
}

fn road_intersection_score_from_segments(
    road_segments: &[(Vec2, Vec2)],
    origin: [i32; 3],
    size: [i32; 3],
) -> f32 {
    let best = road_intersection_points(road_segments)
        .into_iter()
        .map(|point| building_edge_distance_to_point(point, origin, size))
        .fold(f32::INFINITY, f32::min);
    if !best.is_finite() {
        return 0.0;
    }
    if best <= 18.0 {
        1.0
    } else {
        (1.0 - (best - 18.0) / 42.0).clamp(0.0, 1.0)
    }
}

fn city_block_fit_score(
    road_segments: &[(Vec2, Vec2)],
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> f32 {
    let intersection = road_intersection_score_from_segments(road_segments, origin, size);
    match kind {
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => {
            intersection
        }
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => 1.0 - intersection,
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => 0.45 + intersection * 0.55,
        BotTaskKind::BuildPark => 0.65 - intersection * 0.25,
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad | BotTaskKind::TargetRange => 0.45,
        _ => 0.0,
    }
}

fn city_block_role_from_segments(
    road_segments: &[(Vec2, Vec2)],
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> CityBlockRole {
    let intersection = road_intersection_score_from_segments(road_segments, origin, size);
    match kind {
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => {
            if intersection >= 0.55 {
                CityBlockRole::CornerLandmark
            } else {
                CityBlockRole::MidblockStreetWall
            }
        }
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => {
            if intersection >= 0.55 {
                CityBlockRole::ResidentialCorner
            } else {
                CityBlockRole::MidblockStreetWall
            }
        }
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict | BotTaskKind::BuildPark => {
            CityBlockRole::CivicEdge
        }
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad | BotTaskKind::TargetRange => {
            CityBlockRole::ServiceEdge
        }
        _ => CityBlockRole::MidblockStreetWall,
    }
}

#[cfg(test)]
fn road_facing_building_edge(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<BuildingStreetFace> {
    let segments = road_network_segments(save, district);
    if !segments.iter().any(|(a, b)| a.distance_squared(*b) > 1.0) {
        return None;
    }
    nearest_road_building_edge(&segments, origin, size).map(|(face, _)| face)
}

fn road_segments_any_district(save: &BotWorldSave) -> Vec<(Vec2, Vec2)> {
    save.districts
        .iter()
        .flat_map(|district| road_network_segments(save, district))
        .filter(|(a, b)| a.distance_squared(*b) > 1.0)
        .collect()
}

fn road_facing_building_edge_any_district(
    save: &BotWorldSave,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<BuildingStreetFace> {
    save.districts
        .iter()
        .filter_map(|district| {
            let segments = road_network_segments(save, district);
            if !segments.iter().any(|(a, b)| a.distance_squared(*b) > 1.0) {
                return None;
            }
            nearest_road_building_edge(&segments, origin, size)
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(face, _)| face)
}

fn city_block_role_any_district(
    save: &BotWorldSave,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> Option<CityBlockRole> {
    let segments = road_segments_any_district(save);
    if segments.is_empty() {
        None
    } else {
        Some(city_block_role_from_segments(&segments, origin, size, kind))
    }
}

#[cfg(test)]
fn road_frontage_score(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
) -> f32 {
    let road_segments = road_network_segments(save, district);
    let best = nearest_road_building_edge(&road_segments, origin, size)
        .map(|(_, distance)| distance)
        .unwrap_or(f32::INFINITY);
    if !best.is_finite() {
        return 0.0;
    }
    if best <= 18.0 {
        1.0
    } else {
        (1.0 - (best - 18.0) / 44.0).clamp(0.0, 1.0)
    }
}

fn score_planned_site(
    save: &BotWorldSave,
    world: &VoxelWorld,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> f32 {
    let bounds = save.primary_bounds();
    let center_x = origin[0] + size[0] / 2;
    let center_z = origin[2] + size[2] / 2;
    let flatness = terrain_flatness(world, center_x, center_z, size[0].max(size[2]).min(36));
    let road_segments = if is_road_project(kind) {
        Vec::new()
    } else {
        road_network_segments(save, district)
    };
    let road_access = if is_road_project(kind) {
        1.0
    } else {
        let proximity = road_access_score(save, district, center_x, center_z);
        let frontage = nearest_road_building_edge(&road_segments, origin, size)
            .map(|(_, distance)| distance)
            .map(|best| {
                if best <= 18.0 {
                    1.0
                } else {
                    (1.0 - (best - 18.0) / 44.0).clamp(0.0, 1.0)
                }
            })
            .unwrap_or(0.0);
        (proximity * 0.35 + frontage * 0.65).clamp(0.0, 1.0)
    };
    let block_fit = if is_road_project(kind) {
        0.0
    } else {
        city_block_fit_score(&road_segments, origin, size, kind)
    };
    let route_fit = if is_access_road_project(kind) {
        road_route_fit_score(world, origin, size, kind)
    } else {
        0.0
    };
    let balance = match (district.kind, kind) {
        (BotDistrictKind::Park, BotTaskKind::BuildPark | BotTaskKind::BuildPlaza) => 1.0,
        (BotDistrictKind::Skyline, BotTaskKind::BuildGlassTower | BotTaskKind::BuildTower) => 1.0,
        (BotDistrictKind::Training, BotTaskKind::TargetRange) => 1.0,
        (BotDistrictKind::Service, BotTaskKind::LandingPad | BotTaskKind::BuildServicePad) => 1.0,
        (
            BotDistrictKind::Residential,
            BotTaskKind::BuildHome | BotTaskKind::BuildResidentialBlock,
        ) => 1.0,
        _ => 0.65,
    };
    let inside = bounds.contains_box(origin, size);
    score_city_slot_with_route_fit(flatness, road_access, inside, balance, true, route_fit)
        + block_fit.clamp(0.0, 1.0) * 0.55
        + semantic_road_anchor_score(save, district, origin, size, kind) * 2.5
        - bounds.distance_from_center(center_x as f32, center_z as f32) * 0.0005
}

fn semantic_road_anchor_score(
    save: &BotWorldSave,
    district: &BotDistrict,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> f32 {
    if is_road_project(kind) {
        return 0.0;
    }
    let center = project_center(origin, size);
    let center_xz = Vec2::new(center.x, center.z);
    save.user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
        .filter(|guide| semantic_project_kind_for_guide(district, guide) == Some(kind))
        .map(|guide| {
            let anchor = road_guide_anchor(guide);
            let dist = center_xz.distance(Vec2::new(anchor.x, anchor.z));
            let reach = match guide.shape {
                BotRoadGuideShape::Roundabout => 42.0,
                BotRoadGuideShape::Corner => 54.0,
                BotRoadGuideShape::Straight => 0.0,
            };
            if reach <= 0.0 {
                0.0
            } else {
                (1.0 - dist / reach).clamp(0.0, 1.0)
            }
        })
        .fold(0.0, f32::max)
}

fn score_city_slot(
    flatness: f32,
    road_access: f32,
    inside_bounds: bool,
    district_balance: f32,
    player_clearance: bool,
) -> f32 {
    if !inside_bounds || !player_clearance {
        return -10_000.0;
    }
    flatness * 2.5 + road_access.clamp(0.0, 1.0) * 2.4 + district_balance.clamp(0.0, 1.0) * 1.8
}

fn score_city_slot_with_route_fit(
    flatness: f32,
    road_access: f32,
    inside_bounds: bool,
    district_balance: f32,
    player_clearance: bool,
    road_route_fit: f32,
) -> f32 {
    let base = score_city_slot(
        flatness,
        road_access,
        inside_bounds,
        district_balance,
        player_clearance,
    );
    if base < 0.0 {
        return base;
    }
    base + road_route_fit.clamp(0.0, 1.0) * 1.35
}

fn terrain_flatness(world: &VoxelWorld, x: i32, z: i32, radius: i32) -> f32 {
    let r = radius.clamp(4, 36);
    let samples = [
        world.surface_height_at(x, z),
        world.surface_height_at(x + r, z),
        world.surface_height_at(x - r, z),
        world.surface_height_at(x, z + r),
        world.surface_height_at(x, z - r),
        world.surface_height_at(x + r, z + r),
        world.surface_height_at(x + r, z - r),
        world.surface_height_at(x - r, z + r),
        world.surface_height_at(x - r, z - r),
    ];
    let min = samples.iter().min().copied().unwrap_or(0);
    let max = samples.iter().max().copied().unwrap_or(0);
    let center = samples[0];
    let average_delta = samples
        .iter()
        .skip(1)
        .map(|h| (h - center).abs() as f32)
        .sum::<f32>()
        / 8.0;
    let range_score = (1.0 - (max - min) as f32 / 18.0).clamp(0.0, 1.0);
    let slope_score = (1.0 - average_delta / 8.0).clamp(0.0, 1.0);
    range_score * 0.72 + slope_score * 0.28
}

fn record_planning_conversation(save: &mut BotWorldSave, idea_index: usize) -> Option<String> {
    let idea = save.ideas.get(idea_index)?.clone();
    let key = idea.cooldown_key.clone();
    let participants = conversation_participants(save, &idea);
    if participants.is_empty() {
        return None;
    }
    if participants.iter().any(|id| {
        save.agents
            .iter()
            .find(|b| b.id == *id)
            .map(|b| b.memory.recent_conversation_keys.contains(&key))
            .unwrap_or(false)
    }) {
        return None;
    }

    let summary = conversation_summary(save, &idea, &participants);
    let id = save.next_conversation_id;
    save.next_conversation_id += 1;
    save.conversations.push(BotConversation {
        id,
        participants: participants.clone(),
        topic: idea.kind.conversation_topic(),
        summary: summary.clone(),
        importance: 6,
        created_epoch: now_epoch(),
    });
    if let Some(saved) = save.ideas.iter_mut().find(|i| i.id == idea.id) {
        saved.status = BotIdeaStatus::Discussing;
    }
    let meeting = save
        .settlements
        .first()
        .map(|s| vec3_from_arr(s.hub))
        .unwrap_or(Vec3::ZERO);
    for (n, bot_id) in participants.iter().enumerate() {
        if let Some(bot) = save.agents.iter_mut().find(|b| b.id == *bot_id) {
            bot.state = BotState::Planning;
            let offset = Vec3::new((n as f32 - 1.0) * 2.2, 0.0, 3.0);
            let target = meeting + offset;
            bot.target = [target.x, target.y, target.z];
            bot.last_interaction_epoch = now_epoch();
            bot.memory.last_message = summary.clone();
            bot.memory.recent_conversation_keys.insert(0, key.clone());
            bot.memory.recent_conversation_keys.truncate(8);
            for rel in &mut bot.memory.relationships {
                if participants.contains(&rel.other_id) {
                    rel.trust = (rel.trust + 0.015).clamp(0.0, 1.0);
                    rel.collaboration_score += 1.0;
                    rel.last_interaction_epoch = now_epoch();
                }
            }
        }
    }
    Some(summary)
}

fn conversation_participants(save: &BotWorldSave, idea: &BotIdea) -> Vec<u64> {
    let mut out = Vec::new();
    if idea.author_id != 0 {
        out.push(idea.author_id);
    }
    for role in [
        BotRole::Planner,
        idea.kind.preferred_role(),
        BotRole::Surveyor,
    ] {
        if let Some(id) = pick_bot(save, role) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out.truncate(3);
    out
}

fn conversation_summary(save: &BotWorldSave, idea: &BotIdea, participants: &[u64]) -> String {
    let names = participants
        .iter()
        .map(|id| bot_label(save, *id))
        .collect::<Vec<_>>()
        .join(" + ");
    let district = idea
        .district_id
        .and_then(|id| save.districts.iter().find(|d| d.id == id))
        .map(|d| d.name.clone())
        .unwrap_or_else(|| "the city".into());
    match idea.kind.conversation_topic() {
        BotConversationTopic::RoadAccess => {
            format!("{names} agree {district} needs connected roads before bigger builds.")
        }
        BotConversationTopic::Skyline => {
            format!("{names} sketch a glass skyline idea for {district}.")
        }
        BotConversationTopic::ParkBalance => {
            format!("{names} reserve green space so {district} does not become all concrete.")
        }
        BotConversationTopic::PadLighting => {
            format!("{names} coordinate shuttle pads, repair access, and guide lights.")
        }
        BotConversationTopic::RangeReadiness => {
            format!("{names} design a friendly target range with clear firing lanes.")
        }
        BotConversationTopic::DistrictUpgrade => {
            format!("{names} tune {district} with homes, routes, and useful details.")
        }
        BotConversationTopic::CityBoundary => {
            format!("{names} keep the mega city inside the 1024 block boundary.")
        }
    }
}

fn show_city_message(brain: &mut FriendlyWorldBrain, message: String, importance: u8) {
    if importance >= 7 {
        let duplicate = brain
            .save
            .journal
            .last()
            .map(|j| j.text == message)
            .unwrap_or(false);
        if !duplicate {
            brain
                .save
                .journal
                .push(BotJournalEntry::new(message.clone()));
        }
    }
    if brain.message_cooldown <= 0.0 || importance >= 9 {
        brain.hud_message = message;
        brain.message_cooldown = VISIBLE_MESSAGE_COOLDOWN + (10 - importance.min(10)) as f32;
    }
}

#[allow(clippy::too_many_arguments)]
fn add_project(
    save: &mut BotWorldSave,
    world: &VoxelWorld,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
    theme: BotTheme,
    assigned_bot: Option<u64>,
    district_id: Option<u64>,
    idea_id: Option<u64>,
    priority: u8,
    manual: bool,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) -> Result<u64, String> {
    validate_project_request(save, world, origin, size, player_pos, ship_positions)?;
    add_project_unchecked(
        save,
        kind,
        origin,
        size,
        theme,
        assigned_bot,
        district_id,
        idea_id,
        priority,
        manual,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_project_unchecked(
    save: &mut BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
    theme: BotTheme,
    assigned_bot: Option<u64>,
    district_id: Option<u64>,
    idea_id: Option<u64>,
    priority: u8,
    manual: bool,
) -> Result<u64, String> {
    let id = save.next_project_id;
    save.next_project_id += 1;
    let total_steps = (size[0].max(1) * size[1].max(1) * size[2].max(1)) as u32;
    let label = if manual {
        format!("Manual {} #{id}", kind.label())
    } else {
        format!("Auto {} #{id}", kind.label())
    };
    let crew_id = create_project_crew(save, id, kind, assigned_bot, origin);
    let concept = build_project_concept(
        save,
        kind,
        theme,
        origin,
        size,
        &label,
        manual,
        assigned_bot,
        crew_id,
    );
    save.projects.push(BotProject {
        id,
        kind,
        label: label.clone(),
        origin,
        size,
        theme,
        status: BotProjectStatus::Queued,
        cursor: 0,
        total_steps,
        assigned_bot,
        district_id,
        crew_id,
        idea_id,
        blocked_reason: String::new(),
        priority,
        concept,
    });
    assign_crew_task(save, crew_id, assigned_bot, id, kind, &label, origin);
    save.journal
        .push(BotJournalEntry::new(format!("Queued {label}.")));
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
fn build_project_concept(
    save: &BotWorldSave,
    kind: BotTaskKind,
    theme: BotTheme,
    origin: [i32; 3],
    size: [i32; 3],
    label: &str,
    manual: bool,
    assigned_bot: Option<u64>,
    crew_id: Option<u64>,
) -> BotProjectConcept {
    let team = project_owner_label(save, assigned_bot, crew_id);
    let (structure, material_plan, visual_goal) = project_design_language(kind, theme);
    let architecture_variant = project_architecture_variant(kind, origin, size);
    let street_face = project_uses_street_face(kind)
        .then(|| road_facing_building_edge_any_district(save, origin, size))
        .flatten();
    let block_role = project_uses_city_block_role(kind)
        .then(|| city_block_role_any_district(save, origin, size, kind))
        .flatten();
    let semantic_anchor_shape = semantic_road_anchor_shape_for_project(save, kind, origin, size);
    let source = if manual {
        "player request"
    } else {
        "autonomous city planner"
    };
    let brief = format!(
        "{label}: {source}; footprint {}x{}x{} at {},{},{}; owned by {team}; style sheet: {architecture_variant}.",
        size[0], size[1], size[2], origin[0], origin[1], origin[2]
    );
    let mut rows = vec![
        BotPlanRow {
            phase: "Site".into(),
            owner: role_owner_label(save, BotRole::Surveyor, &team),
            material: "survey grid".into(),
            detail: format!(
                "Load chunks, check slope, mark safe origin {},{}.",
                origin[0], origin[2]
            ),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Access".into(),
            owner: role_owner_label(save, BotRole::RoadCrew, &team),
            material: "road-front lots".into(),
            detail: project_access_detail(kind).into(),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Sequence".into(),
            owner: role_owner_label(save, BotRole::Planner, &team),
            material: "city dependency rule".into(),
            detail: project_sequence_rule(kind).into(),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Structure".into(),
            owner: role_owner_label(save, kind.preferred_role(), &team),
            material: material_plan.into(),
            detail: structure.into(),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Architecture".into(),
            owner: role_owner_label(save, BotRole::Architect, &team),
            material: "deterministic style sheet".into(),
            detail: architecture_variant.into(),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Texture".into(),
            owner: role_owner_label(save, BotRole::Architect, &team),
            material: theme.label().into(),
            detail: visual_goal.into(),
            status: "queued".into(),
        },
        BotPlanRow {
            phase: "Detail".into(),
            owner: role_owner_label(save, BotRole::RepairTech, &team),
            material: "lights, signs, roof gear".into(),
            detail: "Add human-scale edges, entries, utilities, and readable city detail.".into(),
            status: "queued".into(),
        },
    ];
    let semantic_anchor_row = semantic_road_anchor_plan_row(save, kind, origin, size, &team);
    let player_frontage_row = if semantic_anchor_row.is_none() {
        player_road_frontage_plan_row(save, kind, origin, size, &team)
    } else {
        None
    };
    let bot_frontage_row = if semantic_anchor_row.is_none() && player_frontage_row.is_none() {
        bot_road_frontage_plan_row(save, kind, origin, size, &team)
    } else {
        None
    };
    if is_road_project(kind) {
        rows.insert(
            2,
            BotPlanRow {
                phase: "Road Grade".into(),
                owner: role_owner_label(save, BotRole::Surveyor, &team),
                material: "terrain-aware grade sheet".into(),
                detail: "Roads score the full planned route, follow continuous terrain grades, bend through corners, lift shallow valleys, keep hilltops intact, and give later buildings real street-edge frontage.".into(),
                status: "queued".into(),
            },
        );
    } else if let Some(detail) = project_city_sheet_detail(kind) {
        let mut detail = detail.to_owned();
        if let Some(face) = street_face {
            detail.push_str(" Street face: ");
            detail.push_str(face.label());
            detail.push_str(" so doors, podiums, and service edges meet the road graph.");
        }
        if let Some(role) = block_role {
            detail.push_str(" Block role: ");
            detail.push_str(role.label());
            detail.push_str(
                " so massing, entries, and public edges fit the intersection or midblock condition.",
            );
            if matches!(role, CityBlockRole::CornerLandmark) {
                detail.push_str(
                    " Corner landmark towers get a signal crown and lit vertical spine on the street-facing corner.",
                );
            } else if matches!(role, CityBlockRole::ResidentialCorner) {
                detail.push_str(
                    " Residential corners get a glass corner bay, side-street visibility, and a lit awning so homes read as part of an intersection.",
                );
            } else if matches!(role, CityBlockRole::CivicEdge) {
                detail.push_str(
                    " Civic edges get a public gateway on the road-facing edge so plazas and parks open directly into the street network.",
                );
            } else if matches!(role, CityBlockRole::ServiceEdge) {
                detail.push_str(
                    " Service edges get a service gate and road-facing utility access so pads connect to the street network instead of floating as isolated platforms.",
                );
                if matches!(kind, BotTaskKind::LandingPad) {
                    detail.push_str(
                        " Landing pads reserve a shuttle approach stripe from the road-facing edge into the pad center.",
                    );
                } else if matches!(kind, BotTaskKind::TargetRange) {
                    detail.push_str(
                        " Target ranges reserve a range gate and safe entry lane from the road-facing edge before the firing lanes begin.",
                    );
                }
            }
        }
        rows.insert(
            2,
            BotPlanRow {
                phase: "City Sheet".into(),
                owner: role_owner_label(save, BotRole::Planner, &team),
                material: "frontage / height / style matrix".into(),
                detail,
                status: "queued".into(),
            },
        );
    }
    if let Some(row) = semantic_anchor_row {
        rows.insert(2, row);
    } else if let Some(row) = player_frontage_row {
        rows.insert(2, row);
    } else if let Some(row) = bot_frontage_row {
        rows.insert(2, row);
    }
    BotProjectConcept {
        brief,
        structure: structure.into(),
        material_plan: material_plan.into(),
        visual_goal: visual_goal.into(),
        rows,
        street_face,
        block_role,
        semantic_anchor_shape,
    }
}

fn semantic_road_anchor_shape_for_project(
    save: &BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<BotRoadGuideShape> {
    semantic_road_anchor_match(save, kind, origin, size).map(|(_, guide, _)| guide.shape)
}

fn semantic_road_anchor_match<'a>(
    save: &'a BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(f32, &'a BotRoadGuide, &'a BotDistrict)> {
    if is_road_project(kind) {
        return None;
    }
    let project_center = project_center(origin, size);
    let project_xz = Vec2::new(project_center.x, project_center.z);
    let mut best: Option<(f32, &BotRoadGuide, &BotDistrict)> = None;
    for district in &save.districts {
        for guide in &save.user_roads {
            if !road_guide_matches_district(guide, district)
                || semantic_project_kind_for_guide(district, guide) != Some(kind)
            {
                continue;
            }
            let anchor = road_guide_anchor(guide);
            let distance = project_xz.distance(Vec2::new(anchor.x, anchor.z));
            let reach = match guide.shape {
                BotRoadGuideShape::Roundabout => 64.0,
                BotRoadGuideShape::Corner => 72.0,
                BotRoadGuideShape::Straight => 0.0,
            };
            if reach > 0.0
                && distance <= reach
                && best.map_or(true, |(best_distance, _, _)| distance < best_distance)
            {
                best = Some((distance, guide, district));
            }
        }
    }
    best
}

fn semantic_road_anchor_plan_row(
    save: &BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
    team: &str,
) -> Option<BotPlanRow> {
    let (_, guide, district) = semantic_road_anchor_match(save, kind, origin, size)?;
    let anchor = road_guide_anchor(guide);
    let (phase, detail) = match guide.shape {
        BotRoadGuideShape::Roundabout => (
            "Roundabout Anchor",
            format!(
                "Use the player road roundabout at {},{} as the civic center for {}; align plaza ring, entries, seating, and connecting streets around that editable component.",
                anchor.x.round() as i32,
                anchor.z.round() as i32,
                district.name
            ),
        ),
        BotRoadGuideShape::Corner => (
            "Corner Landmark",
            format!(
                "Use the player road corner at {},{} as the landmark hinge for {}; face the podium, entry light, and skyline detail toward both streets.",
                anchor.x.round() as i32,
                anchor.z.round() as i32,
                district.name
            ),
        ),
        BotRoadGuideShape::Straight => return None,
    };
    Some(BotPlanRow {
        phase: phase.into(),
        owner: role_owner_label(save, BotRole::Planner, team),
        material: format!("{} road component", guide.theme.label()),
        detail,
        status: "queued".into(),
    })
}

fn player_road_frontage_plan_row(
    save: &BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
    team: &str,
) -> Option<BotPlanRow> {
    if is_road_project(kind) || !project_uses_street_face(kind) {
        return None;
    }
    let center = project_center(origin, size);
    let project_xz = Vec2::new(center.x, center.z);
    let mut best: Option<(f32, &BotRoadGuide, &BotDistrict)> = None;
    for district in &save.districts {
        for guide in &save.user_roads {
            if !road_guide_matches_district(guide, district) {
                continue;
            }
            let distance = road_guide_segments(guide)
                .into_iter()
                .map(|(a, b)| point_to_segment_distance(project_xz, a, b))
                .fold(f32::INFINITY, f32::min);
            let reach = 48.0 + guide.width.max(1) as f32 * 4.0;
            if distance <= reach
                && best.map_or(true, |(best_distance, _, _)| distance < best_distance)
            {
                best = Some((distance, guide, district));
            }
        }
    }
    let (_, guide, district) = best?;
    let grade = road_guide_grade_detail(guide);
    Some(BotPlanRow {
        phase: "Player Road Frontage".into(),
        owner: role_owner_label(save, BotRole::Planner, team),
        material: format!("{} road component", guide.theme.label()),
        detail: format!(
            "Use the {} player road in {} as frontage; keep entries, doors, height rhythm, setbacks, and later retexture choices tied to this width {} editable road component. {}",
            road_guide_shape_label(guide.shape),
            district.name,
            guide.width,
            grade
        ),
        status: "queued".into(),
    })
}

fn bot_road_frontage_plan_row(
    save: &BotWorldSave,
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
    team: &str,
) -> Option<BotPlanRow> {
    if is_road_project(kind) || !project_uses_street_face(kind) {
        return None;
    }
    let mut best: Option<(f32, &BotProject, &BotDistrict)> = None;
    for project in &save.projects {
        if !is_access_road_project(project.kind)
            || matches!(project.status, BotProjectStatus::Blocked)
        {
            continue;
        }
        let Some(district) = project
            .district_id
            .and_then(|id| save.districts.iter().find(|district| district.id == id))
        else {
            continue;
        };
        let Some(distance) = nearest_building_edge_distance_to_road_project(project, origin, size)
        else {
            continue;
        };
        let reach = 48.0 + project_road_corridor_half_width(project) * 4.0;
        if distance <= reach && best.map_or(true, |(best_distance, _, _)| distance < best_distance)
        {
            best = Some((distance, project, district));
        }
    }
    let (_, project, district) = best?;
    let width = (project_road_corridor_half_width(project) * 2.0).round() as i32;
    let face = nearest_building_edge_to_road_project(project, origin, size)
        .map(|(face, _)| face.label())
        .unwrap_or("nearest street face");
    Some(BotPlanRow {
        phase: "Bot Road Frontage".into(),
        owner: role_owner_label(save, BotRole::Planner, team),
        material: format!("{} road project", project.theme.label()),
        detail: format!(
            "Use the autonomous {} '{}' in {} as frontage from the bot road graph; bind entries, sidewalks, setbacks, and height rhythm to the {face}; keep deck grade sampling tied to this width {width} corridor with target deck y={}.",
            project.kind.label(),
            project.label,
            district.name,
            project.origin[1]
        ),
        status: "queued".into(),
    })
}

fn nearest_building_edge_distance_to_road_project(
    project: &BotProject,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<f32> {
    nearest_building_edge_to_road_project(project, origin, size).map(|(_, distance)| distance)
}

fn nearest_building_edge_to_road_project(
    project: &BotProject,
    origin: [i32; 3],
    size: [i32; 3],
) -> Option<(BuildingStreetFace, f32)> {
    let edges = building_edge_segments(origin, size);
    project_road_segments(project)
        .into_iter()
        .flat_map(|(road_a, road_b)| {
            edges.iter().map(move |(face, edge_a, edge_b)| {
                (
                    *face,
                    segment_to_segment_distance(*edge_a, *edge_b, road_a, road_b),
                )
            })
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
}

fn road_guide_shape_label(shape: BotRoadGuideShape) -> &'static str {
    match shape {
        BotRoadGuideShape::Straight => "straight",
        BotRoadGuideShape::Corner => "corner",
        BotRoadGuideShape::Roundabout => "roundabout",
    }
}

fn road_guide_grade_detail(guide: &BotRoadGuide) -> String {
    let mut ys = guide.points.iter().map(|point| point[1]);
    let Some(first) = ys.next() else {
        return "Road deck grade is unknown; keep building entries conservative.".into();
    };
    let (mut min_y, mut max_y) = (first, first);
    for y in ys {
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if max_y == min_y {
        format!("Road deck is level at y={min_y}; align thresholds and frontage pads to that curb.")
    } else {
        format!(
            "Road bridge grade {min_y}->{max_y}; terrace podiums, stairs, ramps, and skyline bases so architecture blends into the smooth road height change."
        )
    }
}

fn project_uses_street_face(kind: BotTaskKind) -> bool {
    matches!(
        kind,
        BotTaskKind::BuildHome
            | BotTaskKind::BuildTower
            | BotTaskKind::BuildGlassTower
            | BotTaskKind::MakeTaller
            | BotTaskKind::BuildResidentialBlock
            | BotTaskKind::BuildPark
            | BotTaskKind::BuildPlaza
            | BotTaskKind::UpgradeDistrict
            | BotTaskKind::LandingPad
            | BotTaskKind::BuildServicePad
            | BotTaskKind::TargetRange
    )
}

fn project_uses_city_block_role(kind: BotTaskKind) -> bool {
    matches!(
        kind,
        BotTaskKind::BuildHome
            | BotTaskKind::BuildResidentialBlock
            | BotTaskKind::BuildTower
            | BotTaskKind::BuildGlassTower
            | BotTaskKind::MakeTaller
            | BotTaskKind::BuildPark
            | BotTaskKind::BuildPlaza
            | BotTaskKind::UpgradeDistrict
            | BotTaskKind::LandingPad
            | BotTaskKind::BuildServicePad
            | BotTaskKind::TargetRange
    )
}

fn project_owner_label(
    save: &BotWorldSave,
    assigned_bot: Option<u64>,
    crew_id: Option<u64>,
) -> String {
    let mut names: Vec<String> = crew_id
        .and_then(|id| save.crews.iter().find(|c| c.id == id))
        .map(|crew| {
            crew.bot_ids
                .iter()
                .filter_map(|id| save.agents.iter().find(|b| b.id == *id))
                .map(|b| b.name.clone())
                .collect()
        })
        .unwrap_or_default();
    if let Some(id) = assigned_bot {
        if let Some(bot) = save.agents.iter().find(|b| b.id == id) {
            if !names.iter().any(|name| name == &bot.name) {
                names.push(bot.name.clone());
            }
        }
    }
    if names.is_empty() {
        "the companion swarm".into()
    } else {
        names.truncate(6);
        names.join(", ")
    }
}

fn role_owner_label(save: &BotWorldSave, role: BotRole, fallback: &str) -> String {
    save.agents
        .iter()
        .find(|bot| bot.role == role && bot.companion)
        .or_else(|| save.agents.iter().find(|bot| bot.role == role))
        .map(|bot| bot.name.clone())
        .unwrap_or_else(|| fallback.into())
}

fn project_plan_seed(kind: BotTaskKind, origin: [i32; 3], size: [i32; 3]) -> i32 {
    let mut seed = (kind as i32).wrapping_mul(97);
    for value in [origin[0], origin[1], origin[2], size[0], size[1], size[2]] {
        seed = seed.wrapping_mul(31).wrapping_add(value);
    }
    seed
}

fn project_plan_variant(kind: BotTaskKind, origin: [i32; 3], size: [i32; 3], count: i32) -> i32 {
    if count <= 1 {
        0
    } else {
        project_plan_seed(kind, origin, size).rem_euclid(count)
    }
}

fn project_architecture_variant(
    kind: BotTaskKind,
    origin: [i32; 3],
    size: [i32; 3],
) -> &'static str {
    let variant = project_plan_variant(kind, origin, size, 5);
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid => {
            match variant {
                0 => "curved boulevard grid with supported sidewalks and sparse signal corners",
                1 => "service-road lattice with tighter lane paint and planted pocket breaks",
                2 => "wide civic avenue with heavier curb structure and open crosswalk rhythm",
                3 => "terrain-following hillside roads with short retaining runs",
                _ => "mixed avenue plan with staggered corners and quiet lamp spacing",
            }
        }
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => {
            match variant {
                0 => "stepped glass tower with framed podium and clean roof machinery",
                1 => "chamfered corner tower with vertical glass fins and compact crown",
                2 => "ribbed alloy tower with offset window rhythm and planted roof corners",
                3 => "slender skyline shard with asymmetric setbacks and signal crown",
                _ => "broad mixed-use tower with civic podium, terraces, and service core",
            }
        }
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => match variant {
            0 => "courtyard housing with stoops, roof tanks, and varied window spacing",
            1 => "compact modern row homes with glass stair slots and shared green pocket",
            2 => "futuristic low-rise block with alloy trim, balconies, and roof gardens",
            3 => "dense street wall with fire escapes, shopfront doors, and warm roof gear",
            _ => "mixed residential cluster with alternating heights and corner entries",
        },
        BotTaskKind::BuildPark => match variant {
            0 => "cross-path grove with benches and low skyline sight lines",
            1 => "pocket park with tree clusters around a clear central lawn",
            2 => "linear green relief that frames adjacent roads",
            3 => "quiet plaza-garden with staggered trees and seating edges",
            _ => "small urban forest breaks between denser projects",
        },
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => match variant {
            0 => "civic square with clear axis paths and a glass-water center",
            1 => "market plaza with edge kiosks, bollards, and corner lights",
            2 => "monument court with framed approaches from the road grid",
            3 => "transit-like public room with dark trim and bright wayfinding",
            _ => "open gathering square that leaves skyline views through the block",
        },
        _ => "structured city project with deterministic variation from its site plan",
    }
}

fn project_access_detail(kind: BotTaskKind) -> &'static str {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid => {
            "Lay roads first, follow terrain height, and add supports only where slopes need them."
        }
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => {
            "Face podium doors toward the nearest road and reserve a readable service edge."
        }
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => {
            "Keep homes on road-front lots with paths, stoops, and courtyards tied to the street."
        }
        BotTaskKind::BuildPark | BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => {
            "Leave public edges open so roads, paths, and landmarks read as one city fabric."
        }
        _ => "Fit the project into the nearest road, path, or service approach.",
    }
}

fn project_city_sheet_detail(kind: BotTaskKind) -> Option<&'static str> {
    match kind {
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => Some(
            "Choose the road segment, podium face, height band, setbacks, crown, and service edge before placing skyline mass.",
        ),
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => Some(
            "Choose the road segment, lot side, height band, stoops, courtyard pocket, and facade rhythm before building homes.",
        ),
        BotTaskKind::BuildPark | BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => Some(
            "Choose the road segment, public edge, view corridor, height band, seating rhythm, and landmark focus before paving.",
        ),
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad | BotTaskKind::TargetRange => Some(
            "Choose the road segment, service approach, clearance edge, height band, beacons, and utility rhythm before construction.",
        ),
        _ => None,
    }
}

fn project_sequence_rule(kind: BotTaskKind) -> &'static str {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid => {
            "Road geometry is the city skeleton: complete access before skyline, housing, or public detail expands."
        }
        BotTaskKind::ClearFlatten => {
            "Prepare only the minimum footprint; do not erase terrain character that roads can follow."
        }
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => {
            "Build after road access exists; podium, entrance, roof, and service core must read from the street."
        }
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => {
            "Use road-front lots, then vary heights, entries, courtyards, and roof detail per block."
        }
        BotTaskKind::BuildPark | BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => {
            "Place public space where it connects roads, sight lines, and district identity."
        }
        BotTaskKind::AddLights | BotTaskKind::DecorateStreet => {
            "Decorate only after a true road grid exists; keep lights sparse enough for readable streets."
        }
        _ => "Fit into the district dependency chain before adding extra detail.",
    }
}

fn project_style_seed(project: &BotProject) -> i32 {
    project_plan_seed(project.kind, project.origin, project.size)
        .wrapping_add((project.id as i32).wrapping_mul(131))
}

fn project_style_variant(project: &BotProject, count: i32) -> i32 {
    if count <= 1 {
        0
    } else {
        project_style_seed(project).rem_euclid(count)
    }
}

fn project_design_language(
    kind: BotTaskKind,
    theme: BotTheme,
) -> (&'static str, &'static str, &'static str) {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => (
            "Asphalt carriageway with curbs, sidewalks, lane markings, and repeatable lamp posts.",
            "stone asphalt, limestone lane paint, dark alloy curbs",
            "Readable city street first; neon only as signal accents.",
        ),
        BotTaskKind::ExpandRoadGrid => (
            "Terrain-aware curved avenues with cross streets, intersection markings, sidewalks, and planted pockets.",
            "stone asphalt, limestone sidewalks, alloy curbs",
            "A readable road grammar that gives future towers and homes a real frontage.",
        ),
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => (
            "Setback tower with podium, window grid, floor bands, roof parapet, HVAC blocks, and antenna detail.",
            "alloy frame, glass windows, stone podium, restrained signs",
            "Dense skyline massing attached to streets, with readable facade rhythm and roof equipment.",
        ),
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => (
            "Perimeter housing block with entries, courtyards, stoops, windows, fire-escape rhythm, and roof tanks.",
            "limestone walls, glass windows, wood doors, dark roof trim",
            "Human-scale residential streets with doors and courtyards oriented toward the road network.",
        ),
        BotTaskKind::BuildPark => (
            "Paths, grass panels, trees, seating, low lights, and a central open pocket.",
            "grass, leaves, wood, limestone paths",
            "Green relief between dense city blocks.",
        ),
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => (
            "Paved civic square with fountain/monument core, benches, bollards, lamps, and edge storefronts.",
            "limestone paving, alloy bollards, glass kiosks",
            "A public landmark that organizes nearby streets.",
        ),
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad => (
            "Reinforced pad with service bays, striped edges, beacon lights, and utility wall detail.",
            "ship alloy deck, dark hull trim, amber beacons",
            "Functional sci-fi infrastructure with believable maintenance access.",
        ),
        BotTaskKind::AddLights | BotTaskKind::DecorateStreet => (
            "Repeatable lamp posts, benches, signs, bollards, and corner accents placed on an existing street.",
            "dark alloy posts, wood benches, sparse neon signage",
            "Small details that make streets feel occupied and usable.",
        ),
        BotTaskKind::ClearFlatten => (
            "Terrain preparation pass that cuts excess blocks, fills gaps, and leaves a stable work platform.",
            "local terrain with limestone construction guides",
            "A clean foundation for the next swarm project.",
        ),
        _ => match theme {
            BotTheme::GreenPark => (
                "Landscape-first build with paths, planted edges, and low civic detail.",
                "grass, leaves, wood, limestone",
                "Natural breakpoints inside the city fabric.",
            ),
            _ => (
                "Structured voxel build with clear massing, material bands, and readable details.",
                "stone, alloy, glass, restrained accent blocks",
                "A planned district element that connects to the city grammar.",
            ),
        },
    }
}

fn validate_project_request(
    save: &BotWorldSave,
    world: &VoxelWorld,
    origin: [i32; 3],
    size: [i32; 3],
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) -> Result<(), String> {
    validate_project_shape_and_bounds(save, origin, size)?;
    if !project_anchor_loaded(world, origin, size) {
        return Err("target center is not loaded yet".into());
    }
    let center = project_center(origin, size);
    let center_block = IVec3::new(
        center.x.round() as i32,
        center.y.round() as i32,
        center.z.round() as i32,
    );
    if protected_position(center_block, player_pos, ship_positions) {
        return Err("too close to player or shuttle".into());
    }
    if protected_project_area(origin, size, player_pos, ship_positions) {
        return Err("project footprint would build too close to player or shuttle".into());
    }
    Ok(())
}

fn validate_project_shape_and_bounds(
    save: &BotWorldSave,
    origin: [i32; 3],
    size: [i32; 3],
) -> Result<(), String> {
    if size.iter().any(|v| *v <= 0) {
        return Err("project dimensions must be positive".into());
    }
    let volume = size[0] as i64 * size[1] as i64 * size[2] as i64;
    if volume <= 0 || volume > 96_000 {
        return Err("project is too large for one safe bot job".into());
    }
    let bounds = save.primary_bounds();
    if !bounds.contains_box(origin, size) {
        return Err("outside the 1024 block bot city boundary".into());
    }
    Ok(())
}

fn project_anchor_loaded(world: &VoxelWorld, origin: [i32; 3], size: [i32; 3]) -> bool {
    let center_x = origin[0] + size[0].max(1) / 2;
    let center_z = origin[2] + size[2].max(1) / 2;
    world.is_column_loaded(center_x, center_z)
}

fn project_center(origin: [i32; 3], size: [i32; 3]) -> Vec3 {
    Vec3::new(
        origin[0] as f32 + size[0].max(1) as f32 * 0.5,
        origin[1] as f32 + size[1].max(1) as f32 * 0.5,
        origin[2] as f32 + size[2].max(1) as f32 * 0.5,
    )
}

fn create_project_crew(
    save: &mut BotWorldSave,
    project_id: u64,
    kind: BotTaskKind,
    assigned_bot: Option<u64>,
    origin: [i32; 3],
) -> Option<u64> {
    let bot_ids = pick_crew_bots(save, kind.preferred_role(), assigned_bot);
    if bot_ids.is_empty() {
        return None;
    }
    let id = save.next_crew_id;
    save.next_crew_id += 1;
    save.crews.push(BotCrew {
        id,
        role_focus: kind.preferred_role(),
        bot_ids: bot_ids.clone(),
        project_id,
        active: true,
    });
    for (n, bot_id) in bot_ids.into_iter().enumerate() {
        if let Some(bot) = save.agents.iter_mut().find(|b| b.id == bot_id) {
            bot.crew_id = Some(id);
            bot.state = BotState::Planning;
            let offset = crew_offset(n);
            bot.target = [
                origin[0] as f32 + offset.x,
                origin[1] as f32 + 2.0,
                origin[2] as f32 + offset.z,
            ];
        }
    }
    Some(id)
}

fn pick_crew_bots(save: &BotWorldSave, preferred: BotRole, assigned_bot: Option<u64>) -> Vec<u64> {
    let mut out = Vec::new();
    if let Some(id) = assigned_bot {
        out.push(id);
        let leader_id = save
            .agents
            .iter()
            .find(|bot| bot.id == id)
            .and_then(|bot| bot.swarm_leader_id)
            .unwrap_or(id);
        if leader_id != id && !out.contains(&leader_id) {
            out.push(leader_id);
        }
        let mut swarm: Vec<&BotAgent> = save
            .agents
            .iter()
            .filter(|bot| bot.swarm_leader_id == Some(leader_id) && bot.current_task.is_none())
            .collect();
        swarm.sort_by_key(|bot| (bot.role != preferred, bot.swarm_index));
        for bot in swarm {
            if out.len() >= MAX_CREW_BOTS_PER_PROJECT {
                break;
            }
            if !out.contains(&bot.id) {
                out.push(bot.id);
            }
        }
    }
    for role in [
        preferred,
        BotRole::Planner,
        BotRole::Surveyor,
        BotRole::Architect,
        BotRole::RoadCrew,
        BotRole::Builder,
        BotRole::ParkKeeper,
        BotRole::RepairTech,
    ] {
        if let Some(id) = pick_bot(save, role) {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    for bot in &save.agents {
        if out.len() >= MAX_CREW_BOTS_PER_PROJECT {
            break;
        }
        if bot.current_task.is_none() && !bot.companion && !out.contains(&bot.id) {
            out.push(bot.id);
        }
    }
    out.truncate(MAX_CREW_BOTS_PER_PROJECT);
    out
}

fn assign_crew_task(
    save: &mut BotWorldSave,
    crew_id: Option<u64>,
    assigned_bot: Option<u64>,
    project_id: u64,
    kind: BotTaskKind,
    label: &str,
    origin: [i32; 3],
) {
    let mut bot_ids = crew_id
        .and_then(|id| save.crews.iter().find(|c| c.id == id))
        .map(|c| c.bot_ids.clone())
        .unwrap_or_default();
    if let Some(bot_id) = assigned_bot {
        if !bot_ids.contains(&bot_id) {
            bot_ids.push(bot_id);
        }
    }
    for (n, bot_id) in bot_ids.into_iter().enumerate() {
        if let Some(bot) = save.agents.iter_mut().find(|b| b.id == bot_id) {
            bot.state = BotState::Planning;
            let offset = crew_offset(n);
            bot.target = [
                origin[0] as f32 + offset.x,
                origin[1] as f32 + 2.0,
                origin[2] as f32 + offset.z,
            ];
            bot.current_task = Some(BotTask {
                task_type: kind,
                project_id,
                label: label.into(),
                progress: 0.0,
            });
            if bot.companion {
                bot.companion_mode = BotCompanionMode::AssistingTask;
            }
            bot.memory.last_message = format!("Crew planning {label}.");
        }
    }
}

fn crew_offset(n: usize) -> Vec3 {
    match n % 6 {
        0 => Vec3::new(0.0, 0.0, -3.0),
        1 => Vec3::new(3.0, 0.0, 1.5),
        2 => Vec3::new(-3.0, 0.0, 1.5),
        3 => Vec3::new(5.0, 0.0, -2.5),
        4 => Vec3::new(-5.0, 0.0, -2.5),
        _ => Vec3::new(0.0, 0.0, 4.5),
    }
}

fn complete_project_at(save: &mut BotWorldSave, idx: usize) {
    let Some(project) = save.projects.get(idx).cloned() else {
        return;
    };
    save.completed_projects = save.completed_projects.saturating_add(1);
    if let Some(idea_id) = project.idea_id {
        if let Some(idea) = save.ideas.iter_mut().find(|i| i.id == idea_id) {
            idea.status = BotIdeaStatus::Built;
        }
    }
    if let Some(crew_id) = project.crew_id {
        if let Some(crew) = save.crews.iter_mut().find(|c| c.id == crew_id) {
            crew.active = false;
        }
    }
    let crew_bot_ids = project
        .crew_id
        .and_then(|id| save.crews.iter().find(|c| c.id == id))
        .map(|c| c.bot_ids.clone())
        .unwrap_or_else(|| project.assigned_bot.into_iter().collect());
    for bot_id in crew_bot_ids {
        if let Some(bot) = save.agents.iter_mut().find(|b| b.id == bot_id) {
            bot.state = BotState::Inspecting;
            bot.crew_id = None;
            bot.memory.completed_tasks = bot.memory.completed_tasks.saturating_add(1);
            bot.memory.fatigue = (bot.memory.fatigue + 0.08).clamp(0.0, 1.0);
            bot.memory.last_message = format!("Finished {}.", project.label);
            bot.current_task = None;
            if bot.companion {
                bot.companion_mode = if save.autonomy.enabled {
                    BotCompanionMode::SurveySweep
                } else {
                    BotCompanionMode::AwaitingInstruction
                };
            }
        }
    }
    if let Some(district_id) = project.district_id {
        if let Some(district) = save.districts.iter_mut().find(|d| d.id == district_id) {
            district.completed_projects = district.completed_projects.saturating_add(1);
        }
    }
    if let Some(settlement) = save.settlements.first_mut() {
        match project.kind {
            kind if is_access_road_project(kind) => {
                settlement.road_count = settlement.road_count.saturating_add(1)
            }
            BotTaskKind::BuildPark | BotTaskKind::BuildPlaza => {
                settlement.park_count = settlement.park_count.saturating_add(1)
            }
            BotTaskKind::ClearFlatten | BotTaskKind::AddLights | BotTaskKind::DecorateStreet => {}
            _ => settlement.building_count = settlement.building_count.saturating_add(1),
        }
        let center = project_center(project.origin, project.size);
        settlement.bounds.used_radius = settlement
            .bounds
            .used_radius
            .max(settlement.bounds.distance_from_center(center.x, center.z));
    }
}

fn nearest_district(save: &BotWorldSave, pos: Vec3) -> Option<&BotDistrict> {
    save.districts.iter().min_by(|a, b| {
        let da = vec3_from_arr(a.center).distance_squared(pos);
        let db = vec3_from_arr(b.center).distance_squared(pos);
        da.total_cmp(&db)
    })
}

fn district_theme(save: &BotWorldSave, district_id: u64) -> Option<BotTheme> {
    let district = save.districts.iter().find(|d| d.id == district_id)?;
    if let Some(theme) = user_road_theme_for_district(save, district) {
        return Some(theme);
    }
    let kind = district.kind;
    Some(match kind {
        BotDistrictKind::HubCore | BotDistrictKind::Service => BotTheme::CyanAlloy,
        BotDistrictKind::Residential => BotTheme::WhiteAlloy,
        BotDistrictKind::Skyline | BotDistrictKind::Scenic => BotTheme::MagentaGlass,
        BotDistrictKind::Park => BotTheme::GreenPark,
        BotDistrictKind::Training => BotTheme::AmberStreet,
    })
}

fn user_road_theme_for_district(save: &BotWorldSave, district: &BotDistrict) -> Option<BotTheme> {
    save.user_roads
        .iter()
        .filter(|guide| road_guide_matches_district(guide, district))
        .map(|guide| {
            let weight = road_guide_length(guide) * guide.width.max(1) as f32;
            (guide.theme, weight)
        })
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(theme, _)| theme)
}

fn autonomous_project_size(kind: BotTaskKind) -> [i32; 3] {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => [88, 7, 11],
        BotTaskKind::ExpandRoadGrid => [96, 7, 96],
        BotTaskKind::DecorateStreet | BotTaskKind::AddLights => [64, 7, 9],
        BotTaskKind::LandingPad => [25, 1, 25],
        BotTaskKind::BuildServicePad => [31, 8, 31],
        BotTaskKind::BuildHome => [13, 10, 13],
        BotTaskKind::BuildResidentialBlock => [44, 16, 38],
        BotTaskKind::BuildTower | BotTaskKind::MakeTaller => [19, 42, 19],
        BotTaskKind::BuildGlassTower => [21, 58, 21],
        BotTaskKind::BuildPark => [30, 8, 30],
        BotTaskKind::BuildPlaza => [42, 8, 42],
        BotTaskKind::ClearFlatten => [28, 8, 28],
        BotTaskKind::TargetRange => [40, 9, 24],
        BotTaskKind::UpgradeDistrict => [42, 10, 42],
    }
}

#[derive(Default)]
struct ProjectAdvance {
    changed: usize,
    completed: bool,
    blocked: bool,
}

fn advance_project_slice(
    project: &mut BotProject,
    world: &mut VoxelWorld,
    history: &mut BuilderHistory,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
    bounds: BotCityBounds,
    budget: usize,
) -> ProjectAdvance {
    let mut out = ProjectAdvance::default();
    if budget == 0 {
        return out;
    }
    if protected_project_area(project.origin, project.size, player_pos, ship_positions) {
        project.status = BotProjectStatus::WaitingForPlayer;
        project.blocked_reason =
            "waiting until player and shuttle clear the build footprint".into();
        return out;
    }
    project.status = BotProjectStatus::Active;
    project.blocked_reason.clear();
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let mut attempts = 0usize;
    while project.cursor < project.total_steps && out.changed < budget && attempts < budget * 3 {
        attempts += 1;
        let local = cursor_to_local(project.cursor, project.size);
        let Some((pos, voxel)) = project_voxel(project, local, world) else {
            project.cursor += 1;
            continue;
        };
        if !bounds.contains_block(pos) {
            project.status = BotProjectStatus::Blocked;
            project.blocked_reason = "edit would leave the 1024 block city boundary".into();
            out.blocked = true;
            break;
        }
        if !world.is_column_loaded(pos.x, pos.z) {
            project.status = BotProjectStatus::WaitingForChunks;
            project.blocked_reason = "waiting for the next build column to stream in".into();
            break;
        }
        if protected_position(pos, player_pos, ship_positions) {
            project.cursor += 1;
            continue;
        }
        let before = world.voxel_at(pos.x, pos.y, pos.z);
        if before != voxel {
            if world
                .edit_set_voxel_batched(pos.x, pos.y, pos.z, voxel, &mut batch)
                .is_some()
            {
                changes.push((pos, before, voxel));
                out.changed += 1;
            }
        }
        project.cursor += 1;
    }
    world.finish_edit_batch(batch);
    if !changes.is_empty() {
        history.record_external(project.label.clone(), changes);
    }
    if project.cursor >= project.total_steps {
        project.status = BotProjectStatus::Complete;
        out.completed = true;
    }
    out
}

fn road_surface_span(world: &VoxelWorld, x: i32, z: i32) -> i32 {
    let samples = [
        world.surface_height_at(x, z),
        world.surface_height_at(x + 1, z),
        world.surface_height_at(x - 1, z),
        world.surface_height_at(x, z + 1),
        world.surface_height_at(x, z - 1),
    ];
    let min = samples.iter().min().copied().unwrap_or(0);
    let max = samples.iter().max().copied().unwrap_or(0);
    max - min
}

fn smoothed_road_grade_y(
    surface_y: i32,
    neighbor_surfaces: [i32; 8],
    structural_edge: bool,
) -> i32 {
    let total = surface_y * 3 + neighbor_surfaces.iter().sum::<i32>();
    let target = ((total as f32) / 11.0).round() as i32;
    if target <= surface_y {
        return surface_y;
    }
    let max_lift = if structural_edge { 6 } else { 4 };
    target.min(surface_y + max_lift)
}

fn road_grade_y(world: &VoxelWorld, x: i32, z: i32, structural_edge: bool) -> i32 {
    let sample = |dx: i32, dz: i32| world.surface_height_at(x + dx, z + dz) + 1;
    smoothed_road_grade_y(
        sample(0, 0),
        [
            sample(4, 0),
            sample(-4, 0),
            sample(0, 4),
            sample(0, -4),
            sample(4, 4),
            sample(4, -4),
            sample(-4, 4),
            sample(-4, -4),
        ],
        structural_edge,
    )
}

fn road_support_depth(surface_y: i32, road_y: i32, slope: i32, structural_edge: bool) -> i32 {
    let slope_depth = if structural_edge {
        if slope >= 3 {
            3
        } else {
            2
        }
    } else if slope >= 4 {
        2
    } else if slope >= 3 {
        1
    } else {
        0
    };
    let lift = (road_y - surface_y).max(0);
    let lift_depth = if lift > 1 {
        if structural_edge {
            lift
        } else {
            lift.min(3)
        }
    } else {
        0
    };
    let max_depth = if structural_edge { 6 } else { 3 };
    slope_depth.max(lift_depth).clamp(0, max_depth)
}

fn road_route_fit_score(
    world: &VoxelWorld,
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> f32 {
    let heights: Vec<i32> = road_route_sample_points(origin, size, kind)
        .into_iter()
        .map(|(x, z)| world.surface_height_at(x, z) + 1)
        .collect();
    road_route_profile_score(&heights)
}

fn road_route_profile_score(heights: &[i32]) -> f32 {
    if heights.len() < 2 {
        return 0.5;
    }
    let min = heights.iter().min().copied().unwrap_or(0);
    let max = heights.iter().max().copied().unwrap_or(0);
    let mut max_step = 0;
    let mut step_total = 0;
    let mut step_count = 0;
    for pair in heights.windows(2) {
        let step = (pair[1] - pair[0]).abs();
        max_step = max_step.max(step);
        step_total += step;
        step_count += 1;
    }
    let average_step = if step_count > 0 {
        step_total as f32 / step_count as f32
    } else {
        0.0
    };
    let average_penalty = (average_step / 5.0).clamp(0.0, 1.0);
    let peak_penalty = (max_step as f32 / 9.0).clamp(0.0, 1.0);
    let range_penalty = (((max - min) - 18).max(0) as f32 / 34.0).clamp(0.0, 1.0);
    (1.0 - average_penalty * 0.55 - peak_penalty * 0.30 - range_penalty * 0.15).clamp(0.0, 1.0)
}

fn road_route_sample_points(
    origin: [i32; 3],
    size: [i32; 3],
    kind: BotTaskKind,
) -> Vec<(i32, i32)> {
    match kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => {
            build_road_route_sample_points(origin, size)
        }
        BotTaskKind::ExpandRoadGrid => expand_road_grid_route_sample_points(origin, size),
        _ => Vec::new(),
    }
}

fn build_road_route_sample_points(origin: [i32; 3], size: [i32; 3]) -> Vec<(i32, i32)> {
    let length = size[0].max(1);
    let last_x = (length - 1).max(0);
    let width = size[2].max(1);
    let mut points = Vec::new();
    let mut local_x = 0;
    while local_x < last_x {
        points.push((
            origin[0] + local_x,
            build_road_center_z(origin[2], width, local_x),
        ));
        local_x = (local_x + 8).min(last_x);
    }
    points.push((
        origin[0] + last_x,
        build_road_center_z(origin[2], width, last_x),
    ));
    points
}

fn expand_road_grid_route_sample_points(origin: [i32; 3], size: [i32; 3]) -> Vec<(i32, i32)> {
    let width = size[0].max(1);
    let depth = size[2].max(1);
    let mut points = Vec::new();
    for target_plan_x in road_grid_targets(width, width / 2) {
        for local_z in road_grid_sample_axis(depth) {
            let local_x = target_plan_x - road_grid_bend_x_from_origin(origin[0], local_z);
            if (0..width).contains(&local_x) {
                points.push((origin[0] + local_x, origin[2] + local_z));
            }
        }
    }
    for target_plan_z in road_grid_targets(depth, depth / 2) {
        for local_x in road_grid_sample_axis(width) {
            let local_z = target_plan_z - road_grid_bend_z_from_origin(origin[2], local_x);
            if (0..depth).contains(&local_z) {
                points.push((origin[0] + local_x, origin[2] + local_z));
            }
        }
    }
    points
}

fn road_support_voxel(
    world: &VoxelWorld,
    x: i32,
    z: i32,
    road_y: i32,
    local_y: i32,
    structural_edge: bool,
) -> Option<(IVec3, Voxel)> {
    if local_y <= 0 || local_y > 6 {
        return None;
    }
    let slope = road_surface_span(world, x, z);
    let surface_y = world.surface_height_at(x, z) + 1;
    let depth = road_support_depth(surface_y, road_y, slope, structural_edge);
    if local_y > depth {
        return None;
    }
    let block = if structural_edge {
        BlockType::Basalt
    } else if local_y == 1 {
        BlockType::Stone
    } else {
        BlockType::Limestone
    };
    Some((IVec3::new(x, road_y - local_y, z), Voxel::from(block)))
}

fn building_foundation_voxel(
    world: &VoxelWorld,
    x: i32,
    z: i32,
    base_y: i32,
    local_y: i32,
    structural: bool,
) -> Option<(IVec3, Voxel)> {
    if local_y <= 0 {
        return None;
    }
    let surface = world.surface_height_at(x, z) + 1;
    let gap = base_y - surface;
    if gap <= 0 {
        return None;
    }
    let max_depth = if structural { gap.min(48) } else { gap.min(3) };
    if local_y > max_depth {
        return None;
    }
    let y = base_y - local_y;
    let block = if structural {
        BlockType::Basalt
    } else {
        BlockType::Limestone
    };
    Some((IVec3::new(x, y, z), Voxel::from(block)))
}

fn civic_deck_base_y(world: &VoxelWorld, origin: IVec3, x: i32, z: i32) -> i32 {
    let terrain_base = world.surface_height_at(x, z) + 1;
    if origin.y - terrain_base >= 4 {
        origin.y
    } else {
        terrain_base
    }
}

fn project_voxel(project: &BotProject, local: IVec3, world: &VoxelWorld) -> Option<(IVec3, Voxel)> {
    let origin = IVec3::new(project.origin[0], project.origin[1], project.origin[2]);
    match project.kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => {
            let x = origin.x + local.x;
            let width = project.size[2].max(1);
            let z = origin.z + local.z + build_road_center_z(origin.z, width, local.x)
                - (origin.z + width / 2);
            let sidewalk = local.z <= 1 || local.z >= width - 2;
            let curb = local.z == 2 || local.z == width - 3;
            let y = road_grade_y(world, x, z, sidewalk || curb);
            let lane = local.z == width / 2 && local.x.rem_euclid(10) < 5;
            let crosswalk = local.x.rem_euclid(34) < 4 && local.z > 2 && local.z < width - 3;
            let signal = (local.x.rem_euclid(48) == 0) && (local.z == 1 || local.z == width - 2);
            if local.y == 0 {
                let voxel = if signal {
                    project.theme.signal()
                } else if sidewalk || crosswalk || lane {
                    Voxel::from(BlockType::Limestone)
                } else if curb {
                    Voxel::from(BlockType::ShipHullDark)
                } else {
                    Voxel::from(BlockType::Stone)
                };
                return Some((IVec3::new(x, y, z), voxel));
            }
            let pole = signal && local.y <= 5;
            let lamp = local.x.rem_euclid(36) == 0
                && (local.z == 1 || local.z == width - 2)
                && local.y <= 4;
            let bench = local.y == 1
                && local.x.rem_euclid(40) <= 3
                && (local.z == 0 || local.z == width - 1);
            let guard = local.y == 1
                && (sidewalk || curb)
                && !pole
                && !lamp
                && !bench
                && road_surface_span(world, x, z) >= 3
                && local.x.rem_euclid(7) == 0;
            if guard {
                return Some((
                    IVec3::new(x, y + local.y, z),
                    Voxel::from(BlockType::ShipHullDark),
                ));
            }
            if !pole && !lamp && !bench {
                if let Some(support) = road_support_voxel(world, x, z, y, local.y, sidewalk || curb)
                {
                    return Some(support);
                }
            }
            if pole {
                let voxel = if local.y == 5 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, y + local.y, z), voxel))
            } else if lamp {
                let voxel = if local.y == 4 {
                    Voxel::from(BlockType::GlowSand)
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, y + local.y, z), voxel))
            } else if bench {
                Some((IVec3::new(x, y + local.y, z), Voxel::from(BlockType::Wood)))
            } else {
                None
            }
        }
        BotTaskKind::ExpandRoadGrid => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let profile = road_grid_profile(project.origin, project.size, local);
            let y = if profile.road_like {
                road_grade_y(world, x, z, profile.structural_edge)
            } else {
                world.surface_height_at(x, z) + 1
            };
            if local.y == 0 && (profile.road_x || profile.road_z) {
                Some((
                    IVec3::new(x, y, z),
                    if profile.median {
                        Voxel::from(BlockType::Leaves)
                    } else if profile.lane || profile.crosswalk {
                        Voxel::from(BlockType::Limestone)
                    } else {
                        Voxel::from(BlockType::Stone)
                    },
                ))
            } else if local.y == 0
                && (profile.sidewalk_x || profile.sidewalk_z || profile.intersection_corner)
            {
                Some((IVec3::new(x, y, z), Voxel::from(BlockType::Limestone)))
            } else if local.y == 0 && (local.x + local.z).rem_euclid(31) == 0 {
                Some((IVec3::new(x, y, z), Voxel::from(BlockType::Leaves)))
            } else if local.y == 0 && (local.x * 13 + local.z * 7).rem_euclid(149) == 0 {
                Some((IVec3::new(x, y, z), project.theme.signal()))
            } else {
                let roundabout_marker = profile.roundabout_center && local.y <= 4;
                let traffic_light = !profile.roundabout
                    && profile.intersection_corner
                    && profile.intersection
                    && (local.x + local.z).rem_euclid(5) == 0
                    && local.y <= 5;
                let lamp = (profile.sidewalk_x || profile.sidewalk_z || profile.boulevard)
                    && (local.x * 5 + local.z * 3).rem_euclid(97) == 0
                    && local.y <= 4;
                let bench = local.y == 1
                    && (profile.sidewalk_x || profile.sidewalk_z)
                    && (local.x * 7 + local.z * 11).rem_euclid(89) <= 1;
                let guard = local.y == 1
                    && (profile.sidewalk_x || profile.sidewalk_z || profile.intersection_corner)
                    && road_surface_span(world, x, z) >= 3
                    && (local.x * 3 + local.z * 5).rem_euclid(11) == 0;
                if roundabout_marker {
                    let voxel = if local.y == 4 {
                        project.theme.signal()
                    } else {
                        Voxel::from(BlockType::Crystal)
                    };
                    Some((IVec3::new(x, y + local.y, z), voxel))
                } else if traffic_light {
                    let voxel = if local.y == 5 {
                        project.theme.signal()
                    } else {
                        Voxel::from(BlockType::ShipHullDark)
                    };
                    Some((IVec3::new(x, y + local.y, z), voxel))
                } else if lamp {
                    let voxel = if local.y == 4 {
                        Voxel::from(BlockType::GlowSand)
                    } else {
                        Voxel::from(BlockType::ShipHullDark)
                    };
                    Some((IVec3::new(x, y + local.y, z), voxel))
                } else if bench {
                    Some((IVec3::new(x, y + local.y, z), Voxel::from(BlockType::Wood)))
                } else if guard {
                    Some((
                        IVec3::new(x, y + local.y, z),
                        Voxel::from(BlockType::ShipHullDark),
                    ))
                } else if profile.road_like {
                    road_support_voxel(world, x, z, y, local.y, profile.structural_edge)
                } else {
                    None
                }
            }
        }
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let y = civic_deck_base_y(world, origin, x, z);
            let edge = local.x == 0
                || local.z == 0
                || local.x == project.size[0] - 1
                || local.z == project.size[2] - 1;
            let cross = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let beacon = edge && (local.x + local.z).rem_euclid(10) == 0;
            let sx = project.size[0] - 1;
            let sz = project.size[2] - 1;
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let service_edge =
                matches!(project.concept.block_role, Some(CityBlockRole::ServiceEdge));
            let gate_surface =
                service_edge && street_face.civic_gateway_surface_cell(local, sx, sz);
            let gate_marker = service_edge
                && street_face.civic_gateway_marker_cell(local, sx, sz)
                && local.y <= 4;
            let shuttle_approach = matches!(project.kind, BotTaskKind::LandingPad)
                && service_edge
                && street_face.shuttle_approach_surface_cell(local, sx, sz);
            if local.y == 0 {
                let voxel = if beacon {
                    project.theme.signal()
                } else if gate_surface || shuttle_approach {
                    Voxel::from(BlockType::Limestone)
                } else if edge || cross {
                    project.theme.accent()
                } else {
                    Voxel::from(BlockType::ShipHullAlloy)
                };
                Some((IVec3::new(x, y, z), voxel))
            } else if matches!(project.kind, BotTaskKind::BuildServicePad) && gate_marker {
                let voxel = if local.y == 4 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, y + local.y, z), voxel))
            } else if matches!(project.kind, BotTaskKind::BuildServicePad)
                && local.y <= 5
                && (local.x < 5 || local.x > project.size[0] - 6)
                && local.z > project.size[2] / 2
            {
                let wall = local.x == 0
                    || local.x == 4
                    || local.x == project.size[0] - 5
                    || local.x == project.size[0] - 1
                    || local.z == project.size[2] - 1
                    || local.y == 5;
                if wall {
                    Some((IVec3::new(x, y + local.y, z), project.theme.wall()))
                } else {
                    Some((IVec3::new(x, y + local.y, z), AIR))
                }
            } else {
                building_foundation_voxel(
                    world,
                    x,
                    z,
                    y,
                    local.y,
                    edge || cross || gate_surface || shuttle_approach,
                )
            }
        }
        BotTaskKind::BuildHome
        | BotTaskKind::BuildTower
        | BotTaskKind::BuildGlassTower
        | BotTaskKind::MakeTaller => {
            let p = origin + local;
            let sx = project.size[0] - 1;
            let sy = project.size[1] - 1;
            let sz = project.size[2] - 1;
            let variant = project_style_variant(project, 5);
            let tower_kind = matches!(
                project.kind,
                BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller
            );
            let perimeter = local.x == 0 || local.z == 0 || local.x == sx || local.z == sz;
            let foundation_core = (local.x - sx / 2).abs() <= 1 && (local.z - sz / 2).abs() <= 1;
            let foundation_grid = (local.x + variant).rem_euclid(7) == 0
                && (local.z + variant * 2).rem_euclid(7) == 0;
            if let Some(foundation) = building_foundation_voxel(
                world,
                p.x,
                p.z,
                origin.y,
                local.y,
                perimeter || foundation_core || foundation_grid,
            ) {
                return Some(foundation);
            }
            let upper = local.y > sy * 2 / 3;
            let mid = local.y > sy / 3;
            let setback = if tower_kind && upper {
                match variant {
                    1 | 3 => 3,
                    4 => 1,
                    _ => 2,
                }
            } else if tower_kind && mid {
                match variant {
                    2 | 4 => 2,
                    _ => 1,
                }
            } else {
                0
            };
            let at_left = local.x == setback;
            let at_right = local.x == sx - setback;
            let at_front = local.z == setback;
            let at_back = local.z == sz - setback;
            let corner_cut = tower_kind
                && matches!(variant, 1 | 3)
                && local.y > 3
                && (at_left || at_right)
                && (at_front || at_back);
            let in_mass = local.x >= setback
                && local.x <= sx - setback
                && local.z >= setback
                && local.z <= sz - setback
                && !corner_cut;
            if !in_mass && local.y > 0 {
                return Some((p, AIR));
            }
            let shell = at_left || at_right || local.y == 0 || local.y == sy || at_front || at_back;
            let podium = local.y <= 3;
            let floor_stride = match variant {
                1 => 4,
                2 => 6,
                3 => 5,
                4 => 3,
                _ => 5,
            };
            let window_pitch = match variant {
                1 => 3,
                2 => 5,
                4 => 6,
                _ => 4,
            };
            let floor_band = local.y > 3 && local.y % floor_stride == 0;
            let window_slot = ((local.x + setback + variant).rem_euclid(window_pitch) == 1)
                || ((local.z + setback + variant * 2).rem_euclid(window_pitch) == 1);
            let window = shell && !podium && local.y < sy && !floor_band && window_slot;
            let glass_tower = matches!(project.kind, BotTaskKind::BuildGlassTower);
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let corner_landmark = matches!(
                project.concept.block_role,
                Some(CityBlockRole::CornerLandmark)
            ) && street_face
                .skyline_corner_marker_cell(local, sx, sz, setback);
            let entrance = podium
                && local.y <= 2
                && street_face.contains_centered_entrance(local, sx, sz, setback);
            let vertical_fin = shell
                && !podium
                && matches!(variant, 1 | 3)
                && (at_left || at_right)
                && local.y.rem_euclid(8) <= 4;
            let terrace = tower_kind
                && matches!(variant, 2 | 4)
                && (local.y == sy / 3 || local.y == sy * 2 / 3)
                && !shell
                && ((local.x - setback).abs() <= 2
                    || (local.z - setback).abs() <= 2
                    || (local.x - (sx - setback)).abs() <= 2
                    || (local.z - (sz - setback)).abs() <= 2);
            let core = !shell
                && (local.x - sx / 2).abs() <= 1
                && (local.z - sz / 2).abs() <= 1
                && local.y < sy;
            let interior_floor = !shell && floor_band;
            let interior_wall = !shell
                && local.y > 4
                && local.y < sy
                && local.y % floor_stride != 0
                && local.y % floor_stride <= (floor_stride - 2)
                && ((local.x - setback + variant).rem_euclid(7) == 0
                    || (local.z - setback + variant).rem_euclid(7) == 0);
            let lobby_detail = !shell
                && podium
                && local.y == 2
                && street_face.lobby_detail_cell(local, sx, sz, setback);
            let raised_from_terrain = origin.y - (world.surface_height_at(p.x, p.z) + 1) >= 4;
            let raised_access_deck = tower_kind
                && raised_from_terrain
                && local.y == 0
                && street_face.raised_access_deck_cell(local, sx, sz, setback);
            let voxel = if local.y == 0 {
                if raised_access_deck {
                    project.theme.accent()
                } else {
                    Voxel::from(BlockType::Limestone)
                }
            } else if local.y == sy {
                let hvac = match variant {
                    1 => (local.x - sx / 2).abs() <= 1 && (local.z - sz / 2).abs() <= 3,
                    2 => (local.x - sx / 2).abs() <= 3 && (local.z - sz / 2).abs() <= 1,
                    _ => (local.x - sx / 2).abs() <= 2 && (local.z - sz / 2).abs() <= 1,
                };
                let antenna = local.x == sx / 2 && local.z == sz / 2 && !matches!(variant, 2);
                let roof_garden = matches!(variant, 2 | 4)
                    && (local.x - setback).abs() > 1
                    && (local.z - setback).abs() > 1
                    && (local.x - (sx - setback)).abs() > 1
                    && (local.z - (sz - setback)).abs() > 1
                    && (local.x + local.z).rem_euclid(9) == 0;
                if corner_landmark {
                    project.theme.signal()
                } else if antenna {
                    project.theme.signal()
                } else if roof_garden {
                    Voxel::from(BlockType::Leaves)
                } else if hvac || at_left || at_right || at_front || at_back {
                    Voxel::from(BlockType::ShipHullDark)
                } else {
                    Voxel::from(BlockType::Basalt)
                }
            } else if entrance {
                Voxel::from(BlockType::CockpitGlass)
            } else if corner_landmark && local.y > sy / 2 {
                project.theme.signal()
            } else if vertical_fin {
                project.theme.accent()
            } else if terrace {
                Voxel::from(BlockType::Limestone)
            } else if window || (glass_tower && shell && local.y > 3 && !floor_band) {
                Voxel::from(BlockType::CockpitGlass)
            } else if shell {
                if podium {
                    Voxel::from(BlockType::Stone)
                } else if floor_band {
                    project.theme.accent()
                } else {
                    project.theme.wall()
                }
            } else if core {
                Voxel::from(BlockType::ShipHullDark)
            } else if interior_floor {
                Voxel::from(BlockType::Limestone)
            } else if interior_wall {
                Voxel::from(BlockType::CockpitGlass)
            } else if lobby_detail {
                Voxel::from(BlockType::Wood)
            } else {
                AIR
            };
            Some((p, voxel))
        }
        BotTaskKind::BuildResidentialBlock => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let terrain_column_base = world.surface_height_at(x, z) + 1;
            let column_base = if origin.y - terrain_column_base >= 4 {
                origin.y
            } else {
                terrain_column_base
            };
            let cell_x = local.x / 11;
            let cell_z = local.z / 10;
            let lx = local.x % 11;
            let lz = local.z % 10;
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let path = local.x == project.size[0] / 2
                || local.z == project.size[2] / 2
                || street_face.residential_frontage_walk_cell(lx, lz);
            let courtyard = cell_x == 1 && cell_z == 1;
            let lot_center_x = origin.x + cell_x * 11 + 4;
            let lot_center_z = origin.z + cell_z * 10 + 3;
            let terrain_lot_base = world.surface_height_at(lot_center_x, lot_center_z) + 1;
            let lot_base = if origin.y - terrain_lot_base >= 4 {
                origin.y
            } else {
                terrain_lot_base
            };
            let style = (project_style_seed(project) + cell_x * 17 + cell_z * 29).rem_euclid(5);
            let lot_shell = cell_x <= 2 && cell_z <= 2 && lx <= 8 && lz <= 7;
            let ground_y = if lot_shell && !courtyard && !path {
                lot_base
            } else {
                column_base
            };
            if local.y == 0 {
                return Some((
                    IVec3::new(x, ground_y, z),
                    if path {
                        Voxel::from(BlockType::Limestone)
                    } else if courtyard {
                        Voxel::from(BlockType::Grass)
                    } else if lz == 8 || lx == 9 {
                        Voxel::from(BlockType::Grass)
                    } else {
                        project.theme.floor()
                    },
                ));
            }
            if path {
                let bollard = local.y <= 2 && (local.x * 3 + local.z * 5).rem_euclid(31) == 0;
                if bollard {
                    return Some((
                        IVec3::new(x, column_base + local.y, z),
                        if local.y == 2 {
                            project.theme.signal()
                        } else {
                            Voxel::from(BlockType::ShipHullDark)
                        },
                    ));
                }
                if let Some(foundation) =
                    building_foundation_voxel(world, x, z, column_base, local.y, false)
                {
                    return Some(foundation);
                }
                return None;
            }
            if cell_x > 2 || cell_z > 2 {
                return None;
            }
            if courtyard {
                if local.y <= 4 && (lx == 1 || lx == 7) && (lz == 1 || lz == 6) {
                    return Some((
                        IVec3::new(x, column_base + local.y, z),
                        Voxel::from(BlockType::Wood),
                    ));
                }
                if local.y == 5 && (lx == 1 || lx == 7) && (lz == 1 || lz == 6) {
                    return Some((
                        IVec3::new(x, column_base + local.y, z),
                        Voxel::from(BlockType::Leaves),
                    ));
                }
                if local.y == 1 && (lx - 4).abs() <= 1 && (lz - 4).abs() <= 1 {
                    return Some((
                        IVec3::new(x, column_base + local.y, z),
                        Voxel::from(BlockType::Water),
                    ));
                }
                return None;
            }
            let building_h =
                (7 + (cell_x * 2 + cell_z + style).rem_euclid(5)).min(project.size[1] - 2);
            let residential_corner = matches!(
                project.concept.block_role,
                Some(CityBlockRole::ResidentialCorner)
            );
            let corner_bay = residential_corner
                && street_face.residential_corner_bay_cell(cell_x, cell_z, lx, lz);
            if corner_bay && (1..=3).contains(&local.y) {
                return Some((
                    IVec3::new(x, lot_base + local.y, z),
                    if local.y == 3 {
                        project.theme.signal()
                    } else {
                        Voxel::from(BlockType::CockpitGlass)
                    },
                ));
            }
            let stoop = local.y == 1 && street_face.residential_stoop_cell(lx, lz, style);
            let balcony = matches!(style, 2 | 4)
                && street_face.residential_balcony_cell(lx, lz, style)
                && local.y > 3
                && local.y < building_h
                && local.y.rem_euclid(3) == 1;
            if stoop || balcony {
                return Some((
                    IVec3::new(x, lot_base + local.y, z),
                    if balcony {
                        Voxel::from(BlockType::ShipHullDark)
                    } else {
                        Voxel::from(BlockType::Limestone)
                    },
                ));
            }
            if lx > 8 || lz > 7 {
                return None;
            }
            if local.y > building_h {
                return None;
            }
            let wall = lx == 0 || lx == 8 || lz == 0 || lz == 7 || local.y == building_h;
            let door = local.y <= 2 && street_face.residential_entry_cell(lx, lz, style);
            if door {
                return Some((
                    IVec3::new(x, lot_base + local.y, z),
                    Voxel::from(BlockType::Wood),
                ));
            }
            let structural = wall || (lx - 4).abs() <= 1 && (lz - 3).abs() <= 1;
            if let Some(foundation) =
                building_foundation_voxel(world, x, z, lot_base, local.y, structural)
            {
                return Some(foundation);
            }
            let window_cycle = if matches!(style, 1 | 3) { 3 } else { 2 };
            let window = wall
                && local.y > 2
                && local.y < building_h
                && local.y % window_cycle == 0
                && ((lx + style).rem_euclid(3) == 2 || (lz + style).rem_euclid(3) == 2);
            let fire_escape = matches!(style, 0 | 3)
                && lz == 7
                && local.y > 3
                && local.y < building_h
                && local.y % 3 == 0
                && lx >= 2
                && lx <= 6;
            let roof_tank = local.y == building_h
                && matches!(style, 0 | 3)
                && (lx - 4).abs() <= 1
                && (lz - 4).abs() <= 1;
            let solar = local.y == building_h
                && style == 2
                && (2..=6).contains(&lx)
                && (2..=5).contains(&lz);
            let roof_garden = local.y == building_h && style == 4 && (lx + lz).rem_euclid(3) == 0;
            let wall_voxel = match style {
                1 => Voxel::from(BlockType::Limestone),
                2 => Voxel::from(BlockType::ShipHullAlloy),
                3 => Voxel::from(BlockType::Stone),
                _ => project.theme.wall(),
            };
            let voxel = if fire_escape {
                Voxel::from(BlockType::ShipHullDark)
            } else if window {
                Voxel::from(BlockType::CockpitGlass)
            } else if roof_tank {
                Voxel::from(BlockType::Wood)
            } else if solar {
                Voxel::from(BlockType::CockpitGlass)
            } else if roof_garden {
                Voxel::from(BlockType::Leaves)
            } else if wall {
                if local.y == building_h {
                    project.theme.accent()
                } else {
                    wall_voxel
                }
            } else {
                AIR
            };
            Some((IVec3::new(x, lot_base + local.y, z), voxel))
        }
        BotTaskKind::BuildPark => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let base = civic_deck_base_y(world, origin, x, z);
            let sx = project.size[0] - 1;
            let sz = project.size[2] - 1;
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let civic_edge = matches!(project.concept.block_role, Some(CityBlockRole::CivicEdge));
            let gateway_surface =
                civic_edge && street_face.civic_gateway_surface_cell(local, sx, sz);
            let gateway_marker =
                civic_edge && street_face.civic_gateway_marker_cell(local, sx, sz) && local.y <= 4;
            let center_path =
                local.x == project.size[0] / 2 || local.z == project.size[2] / 2 || gateway_surface;
            let tree = !gateway_surface
                && !gateway_marker
                && (local.x * 17 + local.z * 23).rem_euclid(31) == 0;
            if local.y == 0 {
                Some((
                    IVec3::new(x, base, z),
                    if center_path {
                        Voxel::from(BlockType::Limestone)
                    } else {
                        Voxel::from(BlockType::Grass)
                    },
                ))
            } else if gateway_marker {
                let voxel = if local.y == 4 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::Wood)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if tree && local.y <= 3 {
                Some((
                    IVec3::new(x, base + local.y, z),
                    Voxel::from(BlockType::Wood),
                ))
            } else if tree && local.y <= 5 {
                Some((
                    IVec3::new(x, base + local.y, z),
                    Voxel::from(BlockType::Leaves),
                ))
            } else if !tree && local.y == 1 && (local.x + local.z).rem_euclid(13) == 0 {
                Some((IVec3::new(x, base + 1, z), Voxel::from(BlockType::Wood)))
            } else if !tree && local.y == 2 && (local.x * 3 + local.z).rem_euclid(29) == 0 {
                Some((IVec3::new(x, base + 2, z), project.theme.signal()))
            } else {
                building_foundation_voxel(
                    world,
                    x,
                    z,
                    base,
                    local.y,
                    center_path || gateway_surface,
                )
            }
        }
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let base = civic_deck_base_y(world, origin, x, z);
            let sx = project.size[0] - 1;
            let sz = project.size[2] - 1;
            let edge = local.x == 0 || local.z == 0 || local.x == sx || local.z == sz;
            let cross = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let center = (local.x - project.size[0] / 2).abs() <= 2
                && (local.z - project.size[2] / 2).abs() <= 2;
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let civic_edge = matches!(project.concept.block_role, Some(CityBlockRole::CivicEdge));
            let gateway_surface =
                civic_edge && street_face.civic_gateway_surface_cell(local, sx, sz);
            let gateway_marker =
                civic_edge && street_face.civic_gateway_marker_cell(local, sx, sz) && local.y <= 4;
            let roundabout_anchor = matches!(
                project.concept.semantic_anchor_shape,
                Some(BotRoadGuideShape::Roundabout)
            );
            let half_x = sx as f32 * 0.5;
            let half_z = sz as f32 * 0.5;
            let dx = local.x as f32 - half_x;
            let dz = local.z as f32 - half_z;
            let radial_distance = (dx * dx + dz * dz).sqrt();
            let outer_radius = sx.min(sz) as f32 * 0.5;
            let ring_radius = (outer_radius * 0.60).max(8.0);
            let roundabout_inside = radial_distance <= outer_radius - 1.5;
            let roundabout_ring = (radial_distance - ring_radius).abs() <= 2.25;
            if local.y == 0 {
                let voxel = if roundabout_anchor && !roundabout_inside {
                    Voxel::from(BlockType::Grass)
                } else if roundabout_anchor && (roundabout_ring || center) {
                    project.theme.accent()
                } else if edge || cross || center {
                    if gateway_surface {
                        Voxel::from(BlockType::Limestone)
                    } else {
                        project.theme.accent()
                    }
                } else {
                    Voxel::from(BlockType::Limestone)
                };
                Some((IVec3::new(x, base, z), voxel))
            } else if center && local.y <= 4 {
                let voxel = if local.y <= 2 {
                    Voxel::from(BlockType::Water)
                } else if local.y == 4 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::CockpitGlass)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if gateway_marker {
                let voxel = if local.y == 4 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::CockpitGlass)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if edge && local.y <= 5 && (local.x + local.z).rem_euclid(9) == 0 {
                let voxel = if local.y == 5 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else {
                building_foundation_voxel(
                    world,
                    x,
                    z,
                    base,
                    local.y,
                    edge || cross || center || gateway_surface || roundabout_ring,
                )
            }
        }
        BotTaskKind::AddLights | BotTaskKind::DecorateStreet => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let edge = local.z == 0 || local.z == project.size[2] - 1;
            let base = civic_deck_base_y(world, origin, x, z);
            let center_lane = local.z == project.size[2] / 2;
            let lamp = edge && local.x % 16 == 0;
            let bench = matches!(project.kind, BotTaskKind::DecorateStreet)
                && local.y == 1
                && local.x % 12 <= 3
                && (local.z == 1 || local.z == project.size[2] - 2);
            if lamp {
                let voxel = if local.y + 1 >= project.size[1] {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if bench {
                Some((
                    IVec3::new(x, base + local.y, z),
                    Voxel::from(BlockType::Wood),
                ))
            } else {
                building_foundation_voxel(world, x, z, base, local.y, edge || center_lane)
            }
        }
        BotTaskKind::ClearFlatten => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let surface = world.surface_height_at(x, z) + 1;
            let edge = local.x == 0
                || local.z == 0
                || local.x == project.size[0] - 1
                || local.z == project.size[2] - 1;
            let raised_from_terrain = origin.y - surface >= 4;
            if raised_from_terrain {
                if local.y == 0 {
                    return Some((IVec3::new(x, origin.y, z), project.theme.floor()));
                }
                if let Some(foundation) = building_foundation_voxel(
                    world,
                    x,
                    z,
                    origin.y,
                    local.y,
                    edge || local.x == project.size[0] / 2 || local.z == project.size[2] / 2,
                ) {
                    return Some(foundation);
                }
                return Some((IVec3::new(x, origin.y + local.y, z), AIR));
            }
            let terrain_delta = (surface - origin.y).clamp(-4, 4);
            let pad_y = origin.y + terrain_delta;
            let y = pad_y + local.y;
            let voxel = if local.y == 0 {
                project.theme.floor()
            } else if edge && local.y <= terrain_delta.unsigned_abs() as i32 + 1 {
                Voxel::from(BlockType::Basalt)
            } else {
                AIR
            };
            Some((IVec3::new(x, y, z), voxel))
        }
        BotTaskKind::TargetRange => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let base = civic_deck_base_y(world, origin, x, z);
            let sx = project.size[0] - 1;
            let sz = project.size[2] - 1;
            let street_face = project
                .concept
                .street_face
                .unwrap_or(BuildingStreetFace::North);
            let service_edge =
                matches!(project.concept.block_role, Some(CityBlockRole::ServiceEdge));
            let gate_surface =
                service_edge && street_face.civic_gateway_surface_cell(local, sx, sz);
            let gate_marker = service_edge
                && street_face.civic_gateway_marker_cell(local, sx, sz)
                && local.y <= 4;
            let safe_lane =
                service_edge && street_face.shuttle_approach_surface_cell(local, sx, sz);
            if local.y == 0 {
                let lane = local.x % 6 == 0;
                return Some((
                    IVec3::new(x, base, z),
                    if gate_surface || safe_lane {
                        Voxel::from(BlockType::Limestone)
                    } else if lane {
                        project.theme.accent()
                    } else {
                        Voxel::from(BlockType::Stone)
                    },
                ));
            }
            let target_wall = local.z == project.size[2] - 2 && local.y <= 5 && local.x % 5 <= 2;
            let cover = local.z == project.size[2] / 2 && local.y <= 2 && local.x % 7 <= 2;
            if gate_marker {
                let voxel = if local.y == 4 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if target_wall {
                let voxel = if local.y == 3 {
                    Voxel::from(BlockType::NeonMagenta)
                } else {
                    Voxel::from(BlockType::NeonAmber)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else if cover {
                Some((
                    IVec3::new(x, base + local.y, z),
                    Voxel::from(BlockType::Basalt),
                ))
            } else {
                building_foundation_voxel(
                    world,
                    x,
                    z,
                    base,
                    local.y,
                    gate_surface || safe_lane || target_wall || cover,
                )
            }
        }
    }
}

fn cursor_to_local(cursor: u32, size: [i32; 3]) -> IVec3 {
    let sx = size[0].max(1) as u32;
    let sz = size[2].max(1) as u32;
    let x = cursor % sx;
    let z = (cursor / sx) % sz;
    let y = cursor / (sx * sz);
    IVec3::new(x as i32, y as i32, z as i32)
}

fn project_origin(world: &VoxelWorld, target: Vec3) -> [i32; 3] {
    let x = target.x.round() as i32;
    let z = target.z.round() as i32;
    [x, world.surface_height_at(x, z) + 1, z]
}

fn command_size(command: BotTaskCommand) -> [i32; 3] {
    let w = command.width.clamp(3, 64) as i32;
    let h = command.height.clamp(1, 48) as i32;
    match command.task_type {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => [64, 7, w.max(7)],
        BotTaskKind::ExpandRoadGrid => [w.max(17) * 2, 7, w.max(17) * 2],
        BotTaskKind::BuildTower | BotTaskKind::MakeTaller => [w.max(9), h.max(12), w.max(9)],
        BotTaskKind::BuildGlassTower => [w.max(11), h.max(24), w.max(11)],
        BotTaskKind::BuildHome => [w.max(9), h.clamp(6, 16), w.max(9)],
        BotTaskKind::BuildResidentialBlock => [w.max(17) * 2, h.clamp(8, 18), w.max(15) * 2],
        BotTaskKind::BuildPark => [w.max(13) * 2, 7, w.max(13) * 2],
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => [w.max(15) * 2, 8, w.max(15) * 2],
        BotTaskKind::LandingPad => [w.max(17), 1, w.max(17)],
        BotTaskKind::BuildServicePad => [w.max(21), 8, w.max(21)],
        BotTaskKind::AddLights | BotTaskKind::DecorateStreet => [56, 7, w.max(5)],
        BotTaskKind::ClearFlatten => [w.max(9) * 2, h.clamp(3, 12), w.max(9) * 2],
        BotTaskKind::TargetRange => [w.max(17) * 2, 7, w.max(11)],
    }
}

fn protected_position(pos: IVec3, player: Option<Vec3>, ships: &[Vec3]) -> bool {
    let p = pos.as_vec3() + Vec3::splat(0.5);
    if player
        .map(|x| x.distance(p) < BOT_PLAYER_EDIT_RADIUS)
        .unwrap_or(false)
    {
        return true;
    }
    ships.iter().any(|s| s.distance(p) < BOT_SHIP_EDIT_RADIUS)
}

fn xz_distance_to_project(origin: [i32; 3], size: [i32; 3], point: Vec3) -> f32 {
    let min_x = origin[0] as f32;
    let max_x = (origin[0] + size[0].max(1)) as f32;
    let min_z = origin[2] as f32;
    let max_z = (origin[2] + size[2].max(1)) as f32;
    let dx = if point.x < min_x {
        min_x - point.x
    } else if point.x > max_x {
        point.x - max_x
    } else {
        0.0
    };
    let dz = if point.z < min_z {
        min_z - point.z
    } else if point.z > max_z {
        point.z - max_z
    } else {
        0.0
    };
    (dx * dx + dz * dz).sqrt()
}

fn protected_project_area(
    origin: [i32; 3],
    size: [i32; 3],
    player: Option<Vec3>,
    ships: &[Vec3],
) -> bool {
    if player
        .map(|p| xz_distance_to_project(origin, size, p) < BOT_PLAYER_PROJECT_MARGIN)
        .unwrap_or(false)
    {
        return true;
    }
    ships
        .iter()
        .any(|s| xz_distance_to_project(origin, size, *s) < BOT_SHIP_PROJECT_MARGIN)
}

fn move_bot_memories(save: &mut BotWorldSave, world: &VoxelWorld, dt: f32) {
    for bot in &mut save.agents {
        let pos = vec3_from_arr(bot.position);
        let target = vec3_from_arr(bot.target);
        let delta = target - pos;
        let dist = delta.length();
        // Companion bots are airborne helpers: fast enough to rejoin the team,
        // gentle enough near the player that they do not feel like they are
        // dodging away from conversation distance.
        let speed = if bot.companion {
            if dist > 36.0 {
                28.0
            } else if dist > 12.0 {
                19.0
            } else {
                8.5
            }
        } else {
            7.5
        };
        let mut next = if dist > 0.2 {
            pos + delta.normalize_or_zero() * (dt * speed).min(dist)
        } else {
            pos
        };
        if !bot.companion {
            let sx = next.x.round() as i32;
            let sz = next.z.round() as i32;
            next.y = world.surface_height_at(sx, sz) as f32 + 2.1;
        } else {
            // Soft floor: never let a companion clip the terrain.
            let sx = next.x.round() as i32;
            let sz = next.z.round() as i32;
            let floor = world.surface_height_at(sx, sz) as f32 + 2.5;
            if next.y < floor {
                next.y = floor;
            }
        }
        bot.position = [next.x, next.y, next.z];
        if bot.current_task.is_some() && dist < 2.5 {
            bot.state = BotState::Building;
        } else if dist < 1.5 && matches!(bot.state, BotState::Returning | BotState::Inspecting) {
            bot.state = BotState::Idle;
        } else if dist > 3.0 && !matches!(bot.state, BotState::Building | BotState::Planning) {
            bot.state = BotState::Surveying;
        }
        if let Some(task) = &mut bot.current_task {
            task.progress = task.progress.clamp(0.0, 1.0);
        }
        match bot.state {
            BotState::Building => {
                bot.memory.fatigue = (bot.memory.fatigue + dt * 0.015).clamp(0.0, 1.0);
                bot.memory.work_focus = (bot.memory.work_focus + dt * 0.02).clamp(0.0, 1.0);
            }
            BotState::Idle | BotState::Inspecting => {
                bot.memory.fatigue = (bot.memory.fatigue - dt * 0.025).clamp(0.0, 1.0);
                bot.memory.curiosity = (bot.memory.curiosity + dt * 0.01).clamp(0.0, 1.0);
            }
            _ => {}
        }
    }
}

fn sync_bot_task_progress(save: &mut BotWorldSave) {
    let progress_by_project: HashMap<u64, f32> = save
        .projects
        .iter()
        .map(|p| {
            let progress = if p.total_steps == 0 {
                1.0
            } else {
                p.cursor as f32 / p.total_steps as f32
            };
            (p.id, progress.clamp(0.0, 1.0))
        })
        .collect();
    for bot in &mut save.agents {
        if let Some(task) = &mut bot.current_task {
            if let Some(progress) = progress_by_project.get(&task.project_id) {
                task.progress = *progress;
            }
        }
    }
}

fn process_companion_command(
    mut brain: ResMut<FriendlyWorldBrain>,
    world: Res<VoxelWorld>,
    player_q: Query<&Transform, With<Player>>,
    ship_q: Query<&Transform, With<ShipInstance>>,
) {
    let Some(command) = brain.companion_command.take() else {
        return;
    };
    let Ok(player_tf) = player_q.get_single() else {
        brain.hud_message = "Player not ready for companion command.".into();
        return;
    };
    let player_pos = player_tf.translation;
    let ship_positions: Vec<Vec3> = ship_q.iter().map(|t| t.translation).collect();

    match command {
        CompanionCommand::BuildCityAutonomy => {
            brain.save.autonomy.enabled = true;
            brain.save.autonomy.bots_active = true;
            brain.save.autonomy.intensity = brain.save.autonomy.intensity.max(8);
            if let Some(settlement) = brain.save.settlements.first_mut() {
                settlement.bounds.max_active_projects = AUTONOMY_BURST_ACTIVE_PROJECTS;
            }
            let queued_now = queue_mega_city_starter_projects(
                &mut brain.save,
                &world,
                player_pos,
                &ship_positions,
                DEFAULT_MAX_ACTIVE_PROJECTS.min(3),
            );
            brain.force_city_idea = true;
            brain.plan_timer = 0.0;
            for bot in brain.save.agents.iter_mut().filter(|b| b.companion) {
                bot.companion_mode = BotCompanionMode::SurveySweep;
                bot.memory.work_focus = 1.0;
                bot.memory.curiosity = 1.0;
                bot.memory.last_message =
                    "Team autonomy online. Surveying terrain and queuing city builds.".into();
            }
            brain.hud_message = if queued_now > 0 {
                format!("Mega city started: {queued_now} starter build(s) queued on the city grid.")
            } else {
                "Mega city autonomy online, but no loaded safe starter site was found yet.".into()
            };
            brain.dirty = true;
            return;
        }
        CompanionCommand::PreviewAssist(assist) => {
            let selected = selected_companion_id(&brain.save, brain.selected_bot);
            let preview = create_companion_preview(
                &brain.save,
                &world,
                player_tf,
                &ship_positions,
                assist,
                selected,
            );
            let valid = preview.status.is_valid();
            let msg = preview.message.clone();
            brain.save.companion_preview = Some(preview);
            set_companion_preview_mode(&mut brain.save, selected, valid);
            brain.hud_message = msg;
            brain.dirty = true;
            return;
        }
        CompanionCommand::ExecutePreview => {
            execute_companion_preview(&mut brain, &world, player_pos, &ship_positions);
            return;
        }
        CompanionCommand::ClearPreview => {
            brain.save.companion_preview = None;
            for bot in brain.save.agents.iter_mut().filter(|b| b.companion) {
                if matches!(
                    bot.companion_mode,
                    BotCompanionMode::PreviewingEdit | BotCompanionMode::Blocked
                ) {
                    bot.companion_mode = BotCompanionMode::AwaitingInstruction;
                    bot.memory.last_message = "Preview cleared. Awaiting instruction.".into();
                }
            }
            brain.hud_message = "Companion preview cleared.".into();
            brain.dirty = true;
            return;
        }
        _ => {}
    }

    let selected = brain.selected_bot;
    let mut affected = 0usize;
    for bot in &mut brain.save.agents {
        if !bot.companion {
            continue;
        }
        if companion_command_selected_only(command) && bot.id != selected {
            continue;
        }
        affected += 1;
        apply_companion_command(bot, command, &world, player_pos);
    }
    brain.hud_message = if affected == 0 {
        "No companion selected for that command.".into()
    } else {
        format!("Companion command applied to {affected} helper(s).")
    };
    brain.dirty = true;
}

fn apply_companion_command(
    bot: &mut BotAgent,
    command: CompanionCommand,
    world: &VoxelWorld,
    player_pos: Vec3,
) {
    let order = bot.companion_order as f32;
    let angle = order * std::f32::consts::TAU / 10.0 - std::f32::consts::FRAC_PI_2;
    let radius = 4.5 + (bot.companion_order / 5) as f32 * 2.0;
    let formation = player_pos
        + Vec3::new(
            angle.cos() * radius,
            4.6 + (bot.companion_order % 3) as f32 * 0.45,
            angle.sin() * radius,
        );
    match command {
        CompanionCommand::PlaceBothNearPlayer | CompanionCommand::PlaceSelectedNearPlayer => {
            let tx = formation.x.round() as i32;
            let tz = formation.z.round() as i32;
            let ty = world.surface_height_at(tx, tz) as f32 + 5.0;
            bot.position = [tx as f32 + 0.5, ty, tz as f32 + 0.5];
            bot.target = bot.position;
            bot.companion_mode = BotCompanionMode::AwaitingInstruction;
            bot.memory.last_message = "Touched down beside you. Awaiting orders.".into();
        }
        CompanionCommand::FollowBoth | CompanionCommand::FollowSelected => {
            bot.companion_mode = BotCompanionMode::FollowingPlayer;
            set_bot_air_target(bot, world, formation, 4.5);
            bot.memory.last_message = format!(
                "Locked on at {:.1}m. Tell me closer or farther anytime.",
                bot.memory.preferred_follow_distance
            );
        }
        CompanionCommand::CloserBoth | CompanionCommand::CloserSelected => {
            bot.memory.preferred_follow_distance = (bot.memory.preferred_follow_distance
                - COMPANION_FOLLOW_STEP)
                .clamp(COMPANION_FOLLOW_MIN, COMPANION_FOLLOW_MAX);
            bot.companion_mode = BotCompanionMode::FollowingPlayer;
            bot.memory.last_message = format!(
                "Coming closer. New follow distance {:.1}m.",
                bot.memory.preferred_follow_distance
            );
        }
        CompanionCommand::FartherBoth | CompanionCommand::FartherSelected => {
            bot.memory.preferred_follow_distance = (bot.memory.preferred_follow_distance
                + COMPANION_FOLLOW_STEP)
                .clamp(COMPANION_FOLLOW_MIN, COMPANION_FOLLOW_MAX);
            bot.companion_mode = BotCompanionMode::FollowingPlayer;
            bot.memory.last_message = format!(
                "Expanding formation. New follow distance {:.1}m.",
                bot.memory.preferred_follow_distance
            );
        }
        CompanionCommand::HoldBoth | CompanionCommand::HoldSelected => {
            bot.companion_mode = BotCompanionMode::HoldingPosition;
            bot.target = bot.position;
            bot.state = BotState::Idle;
            bot.memory.last_message = "Holding altitude. Standing by.".into();
        }
        CompanionCommand::ScanBoth | CompanionCommand::ScanSelected => {
            bot.companion_mode = BotCompanionMode::ScanningArea;
            bot.memory.last_message = "Scan beam online.".into();
        }
        CompanionCommand::PatrolBoth | CompanionCommand::PatrolSelected => {
            bot.companion_mode = BotCompanionMode::Patrolling;
            bot.memory.last_message = "Patrol arc engaged.".into();
        }
        CompanionCommand::SurveyBoth | CompanionCommand::SurveySelected => {
            bot.companion_mode = BotCompanionMode::SurveySweep;
            bot.memory.last_message = "Wide-area survey sweep underway.".into();
        }
        CompanionCommand::MarkWaypointSelected => {
            // Drop a waypoint at the player's current position into the bot's
            // memory so future plans can refer back to it.
            let wp = [player_pos.x, player_pos.y, player_pos.z];
            bot.memory.known_sites.push(wp);
            if bot.memory.known_sites.len() > 32 {
                let drop = bot.memory.known_sites.len() - 32;
                bot.memory.known_sites.drain(0..drop);
            }
            bot.memory.last_message = format!(
                "Waypoint logged at {:.0}, {:.0}, {:.0}.",
                wp[0], wp[1], wp[2]
            );
        }
        CompanionCommand::PreviewAssist(_)
        | CompanionCommand::ExecutePreview
        | CompanionCommand::ClearPreview
        | CompanionCommand::BuildCityAutonomy => {}
    }
}

fn companion_command_selected_only(command: CompanionCommand) -> bool {
    matches!(
        command,
        CompanionCommand::PlaceSelectedNearPlayer
            | CompanionCommand::FollowSelected
            | CompanionCommand::CloserSelected
            | CompanionCommand::FartherSelected
            | CompanionCommand::HoldSelected
            | CompanionCommand::ScanSelected
            | CompanionCommand::PatrolSelected
            | CompanionCommand::SurveySelected
            | CompanionCommand::MarkWaypointSelected
    )
}

fn selected_companion_id(save: &BotWorldSave, selected: u64) -> Option<u64> {
    save.agents
        .iter()
        .find(|b| b.companion && b.id == selected)
        .or_else(|| save.agents.iter().find(|b| b.companion))
        .map(|b| b.id)
}

fn create_companion_preview(
    save: &BotWorldSave,
    world: &VoxelWorld,
    player_tf: &Transform,
    ship_positions: &[Vec3],
    assist: CompanionAssistKind,
    author_id: Option<u64>,
) -> CompanionBuildPreview {
    let mut command = assist.command();
    command.bot_id = author_id.unwrap_or(0);
    let size = command_size(command);
    let target = companion_preview_target(player_tf, assist);
    let origin = centered_project_origin(world, target, size);
    let validation = validate_project_request(
        save,
        world,
        origin,
        size,
        Some(player_tf.translation),
        ship_positions,
    );
    let (status, message) = match validation {
        Ok(()) => (
            CompanionPreviewStatus::Valid,
            format!(
                "{} preview ready. Approve it to let the companions build safely.",
                assist.label()
            ),
        ),
        Err(reason) => (
            CompanionPreviewStatus::Blocked,
            format!("{} preview blocked: {reason}", assist.label()),
        ),
    };
    CompanionBuildPreview {
        id: now_epoch(),
        author_id,
        assist,
        kind: command.task_type,
        origin,
        size,
        theme: command.theme,
        priority: command.priority,
        status,
        message,
        created_epoch: now_epoch(),
    }
}

fn companion_preview_target(player_tf: &Transform, assist: CompanionAssistKind) -> Vec3 {
    let forward = player_tf.rotation.mul_vec3(Vec3::NEG_Z);
    let flat = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
    let forward = if flat.length_squared() > 0.0 {
        flat
    } else {
        Vec3::Z
    };
    let distance = match assist {
        CompanionAssistKind::Road | CompanionAssistKind::Lights | CompanionAssistKind::Recolor => {
            14.0
        }
        CompanionAssistKind::LandingPad
        | CompanionAssistKind::Repair
        | CompanionAssistKind::Beautify
        | CompanionAssistKind::TargetRange => 22.0,
        CompanionAssistKind::ClearFlatten => 12.0,
    };
    player_tf.translation + forward * distance
}

fn centered_project_origin(world: &VoxelWorld, target: Vec3, size: [i32; 3]) -> [i32; 3] {
    let x = target.x.round() as i32 - size[0].max(1) / 2;
    let z = target.z.round() as i32 - size[2].max(1) / 2;
    [x, world.surface_height_at(x, z) + 1, z]
}

fn set_companion_preview_mode(save: &mut BotWorldSave, selected: Option<u64>, valid: bool) {
    for bot in save.agents.iter_mut().filter(|b| b.companion) {
        if selected.map(|id| id != bot.id).unwrap_or(false) {
            continue;
        }
        bot.companion_mode = if valid {
            BotCompanionMode::PreviewingEdit
        } else {
            BotCompanionMode::Blocked
        };
        bot.memory.last_message = if valid {
            "Preview projected. Waiting for your approval.".into()
        } else {
            "Preview blocked. Pick a safer or loaded area.".into()
        };
    }
}

fn execute_companion_preview(
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Vec3,
    ship_positions: &[Vec3],
) {
    let Some(preview) = brain.save.companion_preview.clone() else {
        brain.hud_message = "No companion preview is waiting for approval.".into();
        return;
    };
    if !preview.status.is_valid() {
        brain.hud_message = preview.message;
        return;
    }
    let district_id =
        nearest_district(&brain.save, project_center(preview.origin, preview.size)).map(|d| d.id);
    match add_project(
        &mut brain.save,
        world,
        preview.kind,
        preview.origin,
        preview.size,
        preview.theme,
        preview.author_id,
        district_id,
        None,
        preview.priority,
        true,
        Some(player_pos),
        ship_positions,
    ) {
        Ok(project_id) => {
            let label = preview.assist.label();
            brain.save.companion_preview = None;
            for bot in brain.save.agents.iter_mut().filter(|b| b.companion) {
                if preview.author_id.map(|id| id != bot.id).unwrap_or(false) {
                    continue;
                }
                bot.companion_mode = BotCompanionMode::AssistingTask;
                bot.memory.last_message = format!("{label} approved. Building with undo safety.");
            }
            brain.hud_message = format!("{label} approved as project #{project_id}.");
            brain.dirty = true;
        }
        Err(reason) => {
            let author_id = preview.author_id;
            brain.save.companion_preview = Some(CompanionBuildPreview {
                status: CompanionPreviewStatus::Blocked,
                message: format!("Preview blocked: {reason}"),
                ..preview
            });
            set_companion_preview_mode(&mut brain.save, author_id, false);
            brain.hud_message = format!("Preview blocked: {reason}");
            brain.dirty = true;
        }
    }
}

fn process_bot_visit_request(
    mut brain: ResMut<FriendlyWorldBrain>,
    world: Res<VoxelWorld>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    let Some(request) = brain.visit_request.take() else {
        return;
    };
    let Ok((mut transform, mut player)) = player_q.get_single_mut() else {
        brain.hud_message = "Player not ready for bot visit.".into();
        return;
    };
    let Some((label, target)) = visit_destination(&brain.save, request, transform.translation)
    else {
        brain.hud_message = "No bot build destination available yet.".into();
        return;
    };
    let visit_pos = safe_visit_position(&world, transform.translation, target);
    transform.translation = visit_pos;
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    face_player_toward(&mut transform, &mut player, target);
    brain.hud_message = format!("Visiting {label}. Friendly builders are nearby.");
}

fn visit_destination(
    save: &BotWorldSave,
    request: BotVisitTarget,
    current_player_pos: Vec3,
) -> Option<(String, Vec3)> {
    match request {
        BotVisitTarget::CityHub => save
            .settlements
            .first()
            .map(|s| (s.name.clone(), vec3_from_arr(s.hub))),
        BotVisitTarget::ActiveBuild => save
            .projects
            .iter()
            .filter(|p| !p.status.is_done())
            .max_by_key(|p| p.priority)
            .map(|p| (p.label.clone(), project_center(p.origin, p.size)))
            .or_else(|| {
                save.projects
                    .iter()
                    .rev()
                    .find(|p| p.status == BotProjectStatus::Complete)
                    .map(|p| (p.label.clone(), project_center(p.origin, p.size)))
            }),
        BotVisitTarget::NearestBot => save
            .agents
            .iter()
            .min_by(|a, b| {
                let da = vec3_from_arr(a.position).distance_squared(current_player_pos);
                let db = vec3_from_arr(b.position).distance_squared(current_player_pos);
                da.total_cmp(&db)
            })
            .map(|b| {
                (
                    format!("{} // {}", b.name, b.role.label()),
                    vec3_from_arr(b.position),
                )
            }),
        BotVisitTarget::SelectedBot(id) => save.agents.iter().find(|b| b.id == id).map(|b| {
            (
                format!("{} // {}", b.name, b.role.label()),
                vec3_from_arr(b.position),
            )
        }),
        BotVisitTarget::SelectedDistrict(id) => save
            .districts
            .iter()
            .find(|d| d.id == id)
            .map(|d| (d.name.clone(), vec3_from_arr(d.center))),
    }
}

fn safe_visit_position(world: &VoxelWorld, current: Vec3, target: Vec3) -> Vec3 {
    let mut flat = Vec2::new(current.x - target.x, current.z - target.z);
    if flat.length_squared() < 0.01 {
        flat = Vec2::new(0.0, -1.0);
    }
    let dir = flat.normalize_or_zero();
    let x = (target.x + dir.x * 14.0).round() as i32;
    let z = (target.z + dir.y * 14.0).round() as i32;
    let y = world.surface_height_at(x, z) as f32 + 5.0;
    Vec3::new(x as f32 + 0.5, y, z as f32 + 0.5)
}

fn face_player_toward(transform: &mut Transform, player: &mut Player, target: Vec3) {
    let dir = target - transform.translation;
    let flat = Vec2::new(dir.x, dir.z);
    if flat.length_squared() > 0.001 {
        player.yaw = (-dir.x).atan2(-dir.z);
    }
    let horizontal = flat.length().max(0.001);
    player.pitch = (-dir.y.atan2(horizontal)).clamp(-1.2, 0.9);
    transform.rotation =
        Quat::from_axis_angle(Vec3::Y, player.yaw) * Quat::from_axis_angle(Vec3::X, player.pitch);
}

/// Drive per-frame motion for the worker droid rig: bob, head tracking, arm
/// swing while moving, antenna sway, eye + vent pulses tied to each bot's
/// current `BotState`. Runs against tagged `WorkerBotPart` children only,
/// so it never touches companions or the player.
fn animate_worker_bots(
    time: Res<Time>,
    brain: Res<FriendlyWorldBrain>,
    player_q: Query<&Transform, (With<Player>, Without<WorkerBotPart>)>,
    mut parts: Query<(&WorkerBotPart, &mut Transform), Without<Player>>,
) {
    let elapsed = time.elapsed_seconds();
    let player_pos = player_q.get_single().ok().map(|t| t.translation);
    for (part, mut tf) in &mut parts {
        let Some(bot) = brain.save.agents.iter().find(|b| b.id == part.bot_id) else {
            continue;
        };
        if bot.companion {
            continue;
        }
        let p = vec3_from_arr(bot.position);
        let target = vec3_from_arr(bot.target);
        let to_target = target - p;
        let move_speed = Vec3::new(to_target.x, 0.0, to_target.z).length().min(6.0);
        let phase = elapsed * 2.4 + part.bot_id as f32 * 0.73;
        let walking = move_speed > 0.25;
        let gait = if walking {
            (elapsed * 7.5 + part.bot_id as f32 * 1.31).sin()
        } else {
            0.0
        };
        // Activity intensity for emissive / vent pulses.
        let activity = match bot.state {
            BotState::Building => 1.0,
            BotState::Surveying | BotState::Inspecting => 0.75,
            BotState::Planning | BotState::Returning => 0.4,
            BotState::Idle => 0.18,
        };
        let pulse = (elapsed * 4.2 + part.bot_id as f32 * 1.7).sin() * 0.5 + 0.5;

        // What the bot wants to look at: nearest player when idle/returning,
        // otherwise its work target.
        let look_world = match bot.state {
            BotState::Idle | BotState::Returning => player_pos.unwrap_or(target),
            _ => target,
        };
        let to_look = look_world - p;
        let head_yaw_world = if to_look.length_squared() > 0.04 {
            f32::atan2(to_look.x, to_look.z)
        } else {
            0.0
        };
        // Body yaw (face movement direction or look target).
        let body_yaw = if walking {
            f32::atan2(to_target.x, to_target.z)
        } else {
            head_yaw_world
        };
        let head_yaw_local = (head_yaw_world - body_yaw).rem_euclid(std::f32::consts::TAU);
        let head_yaw_local = if head_yaw_local > std::f32::consts::PI {
            head_yaw_local - std::f32::consts::TAU
        } else {
            head_yaw_local
        };
        let head_yaw_local = head_yaw_local.clamp(-0.9, 0.9);

        match part.part {
            WorkerPart::Head => {
                tf.translation =
                    part.base_translation + Vec3::Y * (phase.sin() * 0.018 + gait.abs() * 0.04);
                let pitch = (elapsed * 1.1 + part.bot_id as f32).sin() * 0.05;
                tf.rotation =
                    Quat::from_rotation_y(head_yaw_local * 0.85) * Quat::from_rotation_x(pitch);
                tf.scale = part.base_scale;
            }
            WorkerPart::Visor => {
                tf.translation =
                    part.base_translation + Vec3::Y * (phase.sin() * 0.018 + gait.abs() * 0.04);
                tf.rotation = Quat::from_rotation_y(head_yaw_local * 0.85);
                // Scanning shimmer when surveying/inspecting.
                let scan = if matches!(bot.state, BotState::Surveying | BotState::Inspecting) {
                    1.0 + pulse * 0.18
                } else {
                    1.0 + pulse * 0.04
                };
                tf.scale = part.base_scale * Vec3::new(scan, 1.0, 1.0);
            }
            WorkerPart::EyeL | WorkerPart::EyeR => {
                tf.translation =
                    part.base_translation + Vec3::Y * (phase.sin() * 0.018 + gait.abs() * 0.04);
                tf.rotation = Quat::from_rotation_y(head_yaw_local * 0.85);
                let s = 1.0 + 0.18 * pulse * activity + 0.05 * (elapsed * 9.0).sin();
                tf.scale = part.base_scale * s;
            }
            WorkerPart::AntennaTip => {
                let sway = Vec3::new(
                    (elapsed * 2.3 + part.bot_id as f32).sin() * 0.05,
                    (elapsed * 3.1).sin() * 0.02,
                    (elapsed * 1.9 + part.bot_id as f32 * 0.5).cos() * 0.05,
                );
                tf.translation = part.base_translation + sway;
                let s = 1.0 + 0.35 * pulse * activity;
                tf.scale = part.base_scale * s;
            }
            WorkerPart::ShoulderL => {
                tf.translation = part.base_translation;
                tf.rotation = Quat::from_rotation_x(gait * 0.18);
                tf.scale = part.base_scale;
            }
            WorkerPart::ShoulderR => {
                tf.translation = part.base_translation;
                tf.rotation = Quat::from_rotation_x(-gait * 0.18);
                tf.scale = part.base_scale;
            }
            WorkerPart::ArmUpperL => {
                tf.translation = part.base_translation;
                let work_swing = if matches!(bot.state, BotState::Building) {
                    (elapsed * 9.5).sin() * 0.7
                } else {
                    gait * 0.55
                };
                tf.rotation = Quat::from_rotation_x(work_swing);
                tf.scale = part.base_scale;
            }
            WorkerPart::ArmUpperR => {
                tf.translation = part.base_translation;
                let work_swing = if matches!(bot.state, BotState::Building) {
                    (elapsed * 9.5 + std::f32::consts::PI).sin() * 0.7
                } else {
                    -gait * 0.55
                };
                tf.rotation = Quat::from_rotation_x(work_swing);
                tf.scale = part.base_scale;
            }
            WorkerPart::ArmForeL => {
                tf.translation = part.base_translation;
                let bend = if matches!(bot.state, BotState::Building) {
                    0.6 + (elapsed * 9.5).sin().abs() * 0.4
                } else {
                    0.25 + gait.abs() * 0.15
                };
                tf.rotation = Quat::from_rotation_x(bend);
                tf.scale = part.base_scale;
            }
            WorkerPart::ArmForeR => {
                tf.translation = part.base_translation;
                let bend = if matches!(bot.state, BotState::Building) {
                    0.6 + (elapsed * 9.5 + std::f32::consts::PI).sin().abs() * 0.4
                } else {
                    0.25 + gait.abs() * 0.15
                };
                tf.rotation = Quat::from_rotation_x(bend);
                tf.scale = part.base_scale;
            }
            WorkerPart::HoverRing => {
                let breathe = 1.0 + 0.06 * (elapsed * 2.0 + part.bot_id as f32).sin();
                tf.translation = part.base_translation + Vec3::Y * ((elapsed * 1.4).sin() * 0.015);
                tf.scale = part.base_scale * Vec3::new(breathe, 1.0, breathe);
            }
            WorkerPart::BackpackVent => {
                let intensity = 0.85 + 0.55 * pulse * activity;
                tf.translation = part.base_translation;
                tf.scale = part.base_scale * Vec3::new(intensity, intensity, 1.0);
            }
            WorkerPart::ChestPanel => {
                let s = 1.0 + 0.06 * pulse * activity;
                tf.translation = part.base_translation;
                tf.scale = part.base_scale * Vec3::new(s, s, 1.0);
            }
            WorkerPart::Torso => {
                tf.translation =
                    part.base_translation + Vec3::Y * (gait.abs() * 0.03 + phase.sin() * 0.01);
                let lean = if walking { gait * 0.05 } else { 0.0 };
                tf.rotation = Quat::from_rotation_z(lean);
                tf.scale = part.base_scale;
            }
            WorkerPart::ToolL => {
                tf.translation = part.base_translation;
                let visible = matches!(
                    bot.state,
                    BotState::Building | BotState::Surveying | BotState::Inspecting
                );
                let s = if visible { 1.0 + 0.45 * pulse } else { 0.001 };
                tf.scale = part.base_scale * s;
                if visible {
                    tf.rotation = Quat::from_rotation_z((elapsed * 6.0).sin() * 0.4);
                }
            }
        }
    }
}

fn sync_bot_visuals(
    time: Res<Time>,
    brain: Res<FriendlyWorldBrain>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    player_q: Query<&Transform, (With<Player>, Without<FriendlyBotEntity>)>,
    mut bot_q: Query<
        (&FriendlyBotEntity, &mut Transform),
        (
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut ring_q: Query<
        (&CompanionRing, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut eye_q: Query<
        (&CompanionEye, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut thruster_q: Query<
        (&CompanionThruster, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut head_q: Query<
        (&CompanionHead, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut iris_q: Query<
        (&CompanionEyeIris, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mood_q: Query<&CompanionMoodLight>,
    mut antenna_q: Query<
        (&CompanionAntennaTip, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionEarCap>,
            Without<Player>,
        ),
    >,
    mut ear_q: Query<
        (&CompanionEarCap, &mut Transform),
        (
            Without<FriendlyBotEntity>,
            Without<CompanionRing>,
            Without<CompanionEye>,
            Without<CompanionThruster>,
            Without<CompanionHead>,
            Without<CompanionEyeIris>,
            Without<CompanionMoodLight>,
            Without<CompanionAntennaTip>,
            Without<Player>,
        ),
    >,
) {
    let elapsed = time.elapsed_seconds();
    let dt = time.delta_seconds().clamp(0.0, 0.1);
    let player_pos = player_q.get_single().ok().map(|t| t.translation);

    // Per-bot world rotation cache so head/iris systems can transform world
    // vectors into local space (used for eye tracking + head tilt direction).
    let mut bot_world_rot: HashMap<u64, Quat> = HashMap::new();
    let mut bot_pos: HashMap<u64, Vec3> = HashMap::new();

    for (entity, mut transform) in &mut bot_q {
        let Some(bot) = brain.save.agents.iter().find(|b| b.id == entity.id) else {
            continue;
        };
        let p = vec3_from_arr(bot.position);
        let target = vec3_from_arr(bot.target);

        if bot.companion {
            // Hover bob — small idle vertical oscillation, larger when scanning.
            let bob_amp = match bot.companion_mode {
                BotCompanionMode::ScanningArea | BotCompanionMode::SurveySweep => 0.32,
                BotCompanionMode::HoldingPosition => 0.10,
                BotCompanionMode::Blocked => 0.05,
                _ => 0.20,
            };
            let phase = elapsed * 1.7 + entity.id as f32 * 1.13;
            let bob = phase.sin() * bob_amp;
            let desired = p + Vec3::Y * bob;
            transform.translation = transform
                .translation
                .lerp(desired, (10.0 * dt).clamp(0.0, 1.0));

            // Smooth body rotation. Saucer banks slightly into its motion.
            let to = target - p;
            let to_flat = Vec3::new(to.x, 0.0, to.z);
            let speed = to_flat.length();
            let yaw = if speed > 0.05 {
                f32::atan2(to_flat.x, to_flat.z)
            } else {
                // Lazy slow yaw when idle.
                elapsed * 0.25 + entity.id as f32 * 0.6
            };
            // Bank: tilt a few degrees toward direction of travel.
            let bank = (speed * 0.06).clamp(0.0, 0.25);
            let bank_axis = if speed > 0.05 {
                Vec3::new(to_flat.z, 0.0, -to_flat.x).normalize_or_zero()
            } else {
                Vec3::ZERO
            };
            let yaw_q = Quat::from_rotation_y(yaw);
            let bank_q = if bank_axis.length_squared() > 0.0 {
                Quat::from_axis_angle(bank_axis, bank)
            } else {
                Quat::IDENTITY
            };
            // Subtle wobble on roll for that "hovercraft" feel.
            let wobble = (elapsed * 0.9 + entity.id as f32).sin() * 0.04;
            let wobble_q = Quat::from_rotation_z(wobble);
            let desired_rot = bank_q * yaw_q * wobble_q;
            transform.rotation = transform
                .rotation
                .slerp(desired_rot, (8.0 * dt).clamp(0.0, 1.0));

            bot_world_rot.insert(entity.id, transform.rotation);
            bot_pos.insert(entity.id, transform.translation);
        } else {
            // Worker droid body — smooth follow toward sim position, slerp
            // rotation toward movement direction (or the player when idle so
            // the bot visibly turns to face you when it has nothing to do).
            let current = transform.translation;
            transform.translation = current + (p - current) * (8.0 * dt).clamp(0.0, 1.0);
            let to_target = target - p;
            let walking = Vec3::new(to_target.x, 0.0, to_target.z).length() > 0.25;
            let face = if walking {
                to_target
            } else if let Some(pp) = player_pos {
                Vec3::new(pp.x - p.x, 0.0, pp.z - p.z)
            } else {
                to_target
            };
            if face.length_squared() > 0.0025 {
                let yaw = f32::atan2(face.x, face.z);
                let desired_rot = Quat::from_rotation_y(yaw);
                transform.rotation = transform
                    .rotation
                    .slerp(desired_rot, (6.0 * dt).clamp(0.0, 1.0));
            }
        }
    }

    // Spin saucer rings (legacy — none of the new chars use these, but
    // keeping the system future-proof in case rings are added back later).
    for (ring, mut tf) in &mut ring_q {
        tf.rotate_local_y(ring.speed * dt);
        let s = 1.0 + (elapsed * 1.3 + ring.phase).sin() * 0.04;
        tf.scale = Vec3::new(s, 1.0, s);
    }

    for (eye, mut tf) in &mut eye_q {
        let pulse = 1.0 + (elapsed * 3.0 + eye.phase).sin() * 0.18;
        tf.scale = Vec3::new(pulse, 0.65 * pulse, pulse);
    }

    for (thr, mut tf) in &mut thruster_q {
        let phase = elapsed * 4.0 - thr.index as f32 * 0.55;
        let glow = 0.55 + 0.45 * phase.sin().max(0.0);
        tf.scale = Vec3::splat(glow);
        tf.translation.y = thr.base_y + (elapsed * 1.6 + thr.index as f32 * 0.4).sin() * 0.02;
    }

    // -------- character animation --------------------------------------
    // Pick what each companion is "looking at": the player when following,
    // their movement target otherwise.
    let look_target = |bot: &BotAgent| -> Vec3 {
        match bot.companion_mode {
            BotCompanionMode::FollowingPlayer => {
                player_pos.unwrap_or_else(|| vec3_from_arr(bot.target))
            }
            _ => vec3_from_arr(bot.target),
        }
    };

    // Head tilt — pitch toward the look target a few degrees.
    for (head, mut tf) in &mut head_q {
        let Some(bot) = brain.save.agents.iter().find(|b| b.id == head.bot_id) else {
            continue;
        };
        let Some(world_rot) = bot_world_rot.get(&head.bot_id).copied() else {
            continue;
        };
        let Some(world_pos) = bot_pos.get(&head.bot_id).copied() else {
            continue;
        };
        let to = look_target(bot) - world_pos;
        // Convert into bot-local space.
        let local = world_rot.inverse() * to;
        let local_flat = Vec3::new(local.x, 0.0, local.z).length();
        let pitch = if local_flat > 0.05 {
            (-local.y / local_flat.max(0.5)).clamp(-0.5, 0.5) * 0.35
        } else {
            0.0
        };
        let yaw_local = if local_flat > 0.05 {
            f32::atan2(local.x, local.z).clamp(-0.6, 0.6) * 0.30
        } else {
            0.0
        };
        let target_rot = Quat::from_rotation_y(yaw_local) * Quat::from_rotation_x(pitch);
        // BOLT (kind=1) gets a slightly more bobbly head — extra twitch.
        let twitch = if head.kind == 1 {
            Quat::from_rotation_z((elapsed * 1.6 + head.bot_id as f32 * 0.5).sin() * 0.04)
        } else {
            Quat::IDENTITY
        };
        let desired = target_rot * twitch;
        tf.rotation = tf.rotation.slerp(desired, (6.0 * dt).clamp(0.0, 1.0));
    }

    // Iris tracking + blink. Each bot blinks on its own deterministic phase.
    for (iris, mut tf) in &mut iris_q {
        let Some(bot) = brain.save.agents.iter().find(|b| b.id == iris.bot_id) else {
            continue;
        };
        let Some(world_rot) = bot_world_rot.get(&iris.bot_id).copied() else {
            continue;
        };
        let Some(world_pos) = bot_pos.get(&iris.bot_id).copied() else {
            continue;
        };
        let to = look_target(bot) - world_pos;
        let local = world_rot.inverse() * to;
        let len = local.length().max(0.001);
        let dir = local / len;
        // Cap iris movement to a small visor-relative offset.
        let max_off = 0.06;
        let mut off = Vec3::new(
            dir.x * max_off,
            dir.y * (max_off * 0.6),
            dir.z * (max_off * 0.4).abs(),
        );
        // Asymmetry: slight outward bias per side so the bot looks expressive.
        off.x += iris.side as f32 * 0.005;
        let target_pos = iris.base + off;
        tf.translation = tf.translation.lerp(target_pos, (12.0 * dt).clamp(0.0, 1.0));

        // Blink: every ~4s a 0.10s squash. Suspended while scanning.
        let suspend_blink = matches!(
            bot.companion_mode,
            BotCompanionMode::ScanningArea | BotCompanionMode::SurveySweep
        );
        let cycle_len = 4.0 + ((iris.bot_id % 7) as f32) * 0.3;
        let phase = (elapsed + (iris.bot_id as f32) * 0.7) % cycle_len;
        let blinking = !suspend_blink && phase < 0.10;
        let target_y = if blinking {
            iris.base_scale.y * 0.10
        } else {
            iris.base_scale.y
        };
        tf.scale.x = iris.base_scale.x;
        tf.scale.z = iris.base_scale.z;
        tf.scale.y = lerp_f32(tf.scale.y, target_y, (20.0 * dt).clamp(0.0, 1.0));
    }

    // Mood-light: drive the per-bot material's emissive + base color from the
    // active companion mode. Each `CompanionMoodLight` has its OWN material so
    // mutating one doesn't affect the other companion.
    for mood in &mood_q {
        let Some(bot) = brain.save.agents.iter().find(|b| b.id == mood.bot_id) else {
            continue;
        };
        let Some(mat) = materials.get_mut(&mood.mat) else {
            continue;
        };
        let c = companion_mode_color(bot.companion_mode, bot.role);
        let r = c.r() as f32 / 255.0;
        let g = c.g() as f32 / 255.0;
        let b = c.b() as f32 / 255.0;
        // Gentle pulse — faster while scanning, slower while waiting.
        let pulse_speed = match bot.companion_mode {
            BotCompanionMode::ScanningArea | BotCompanionMode::SurveySweep => 4.5,
            BotCompanionMode::HoldingPosition => 1.2,
            BotCompanionMode::Blocked => 6.0,
            _ => 2.4,
        };
        let pulse = 0.7 + 0.3 * (elapsed * pulse_speed + mood.bot_id as f32).sin();
        mat.base_color = Color::srgb(r, g, b);
        mat.emissive = LinearRgba::rgb(r * 6.0 * pulse, g * 6.0 * pulse, b * 6.0 * pulse);
    }

    // Antenna tip — soft pulse that matches the mood-light cadence.
    for (tip, mut tf) in &mut antenna_q {
        let s = tip.base_scale * (1.0 + (elapsed * 2.6 + tip.bot_id as f32).sin() * 0.10);
        tf.scale = Vec3::splat(s);
    }

    // Ear caps — slow rotation around their axis (chunky, characterful).
    for (ear, mut tf) in &mut ear_q {
        let speed = 0.6 * ear.side as f32;
        tf.rotate_local_y(speed * dt);
    }
}

fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn manual_save_bot_world(
    keys: Res<ButtonInput<KeyCode>>,
    active: Option<Res<ActiveWorld>>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut world: ResMut<VoxelWorld>,
) {
    if !keys.just_pressed(KeyCode::F5) {
        return;
    }
    let Some(active) = active else {
        return;
    };
    save_bot_world_files(&active.meta.name, &brain.save);
    save_edited_overrides_for_world(&active.meta.name, &world);
    brain.dirty = false;
    world.edit_save_dirty = false;
}

fn autosave_bot_world(
    time: Res<Time>,
    active: Option<Res<ActiveWorld>>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut world: ResMut<VoxelWorld>,
) {
    brain.autosave_timer -= time.delta_seconds();
    if brain.autosave_timer > 0.0 {
        return;
    }
    brain.autosave_timer = 30.0;
    let Some(active) = active else {
        return;
    };
    if !brain.dirty && !world.edit_save_dirty {
        return;
    }

    let edited_overrides = if world.edit_save_dirty {
        Some(world.edited_overrides.clone())
    } else {
        None
    };
    if queue_bot_world_save(
        active.meta.name.clone(),
        brain.save.clone(),
        edited_overrides,
    ) {
        brain.dirty = false;
        world.edit_save_dirty = false;
    } else {
        // A previous save is still flushing to disk. Try again soon,
        // but do not serialize on the gameplay frame.
        brain.autosave_timer = 4.0;
    }
}

fn save_bot_world_on_exit(
    mut exit: EventReader<AppExit>,
    active: Option<Res<ActiveWorld>>,
    brain: Res<FriendlyWorldBrain>,
    world: Res<VoxelWorld>,
) {
    if exit.read().next().is_none() {
        return;
    }
    let Some(active) = active else {
        return;
    };
    save_bot_world_files(&active.meta.name, &brain.save);
    save_edited_overrides_for_world(&active.meta.name, &world);
}

pub fn save_bot_world_files(world_name: &str, save: &BotWorldSave) {
    #[cfg(target_arch = "wasm32")]
    {
        match ron::ser::to_string_pretty(save, ron::ser::PrettyConfig::default()) {
            Ok(text) => {
                if let Err(e) =
                    crate::platform::browser_storage_set(&browser_bot_world_key(world_name), &text)
                {
                    warn!("{e}");
                }
            }
            Err(e) => warn!("bots: failed serialising browser bot state: {e}"),
        }
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let root = bot_root(world_name);
        let agents = root.join("agents");
        let projects = root.join("projects");
        if let Err(e) = fs::create_dir_all(&agents) {
            warn!("bots: failed creating {}: {e}", agents.display());
            return;
        }
        if let Err(e) = fs::create_dir_all(&projects) {
            warn!("bots: failed creating {}: {e}", projects.display());
            return;
        }
        if let Ok(text) = ron::ser::to_string_pretty(save, ron::ser::PrettyConfig::default()) {
            let _ = crate::settings::atomic_write_text(&root.join("journal.ron"), &text);
        }
        let mut expected_agents = HashSet::new();
        for bot in &save.agents {
            let path = agents.join(format!("bot_{}.ron", bot.id));
            expected_agents.insert(path.clone());
            if let Ok(text) = ron::ser::to_string_pretty(bot, ron::ser::PrettyConfig::default()) {
                let _ = crate::settings::atomic_write_text(&path, &text);
            }
        }
        cleanup_stale_ron(&agents, &expected_agents);

        let mut expected_projects = HashSet::new();
        for project in &save.projects {
            let path = projects.join(format!("project_{}.ron", project.id));
            expected_projects.insert(path.clone());
            if let Ok(text) = ron::ser::to_string_pretty(project, ron::ser::PrettyConfig::default())
            {
                let _ = crate::settings::atomic_write_text(&path, &text);
            }
        }
        cleanup_stale_ron(&projects, &expected_projects);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn queue_bot_world_save(
    world_name: String,
    save: BotWorldSave,
    edited_overrides: Option<AHashMap<crate::chunk::ChunkPos, EditedChunkOverride>>,
) -> bool {
    if BOT_SAVE_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return false;
    }

    let spawn = thread::Builder::new()
        .name("voxel-native-autosave".into())
        .spawn(move || {
            save_bot_world_files(&world_name, &save);
            if let Some(overrides) = edited_overrides {
                save_edited_overrides_snapshot(&world_name, overrides);
            }
            BOT_SAVE_IN_FLIGHT.store(false, Ordering::SeqCst);
        });

    if let Err(e) = spawn {
        BOT_SAVE_IN_FLIGHT.store(false, Ordering::SeqCst);
        warn!("bots: failed starting background autosave: {e}");
        return false;
    }

    true
}

#[cfg(target_arch = "wasm32")]
fn queue_bot_world_save(
    world_name: String,
    save: BotWorldSave,
    edited_overrides: Option<AHashMap<crate::chunk::ChunkPos, EditedChunkOverride>>,
) -> bool {
    save_bot_world_files(&world_name, &save);
    if let Some(overrides) = edited_overrides {
        save_edited_overrides_snapshot(&world_name, overrides);
    }
    true
}

pub fn load_bot_world_files(world_name: &str) -> Option<BotWorldSave> {
    #[cfg(target_arch = "wasm32")]
    {
        return crate::platform::browser_storage_get(&browser_bot_world_key(world_name))
            .and_then(|text| ron::from_str::<BotWorldSave>(&text).ok());
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = bot_root(world_name).join("journal.ron");
        let text = fs::read_to_string(path).ok()?;
        ron::from_str::<BotWorldSave>(&text).ok()
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_bot_world_key(world_name: &str) -> String {
    format!(
        "voxel_native.bot_world.{}",
        crate::settings::world_storage_stem(world_name)
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn cleanup_stale_ron(dir: &Path, expected: &HashSet<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("ron") && !expected.contains(&path) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bot_root(world_name: &str) -> PathBuf {
    PathBuf::from(SAVES_DIR).join(format!(
        "{}_bots",
        crate::settings::world_storage_stem(world_name)
    ))
}

fn draw_companion_quick_dock(
    mut contexts: EguiContexts,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut settings: ResMut<WorldSettings>,
    mut editor: ResMut<EditorState>,
) {
    if !settings.companion_ui.show_companion_dock {
        return;
    }
    let ctx = contexts.ctx_mut();
    let theme = settings.theme;
    let (anchor, offset) = match settings.companion_ui.dock_position {
        crate::settings::CompanionDockPosition::Left => {
            (egui::Align2::LEFT_CENTER, egui::vec2(14.0, 0.0))
        }
        crate::settings::CompanionDockPosition::Right => {
            (egui::Align2::RIGHT_CENTER, egui::vec2(-14.0, 0.0))
        }
        crate::settings::CompanionDockPosition::Bottom => {
            (egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
        }
    };
    let menu_key = egui::Id::new("companion_dock_open_id");
    let mut open_id = ctx.data(|d| d.get_temp::<u64>(menu_key)).unwrap_or(0);
    let companions: Vec<(u64, String, BotCompanionMode, BotRole, u8, String)> = brain
        .save
        .agents
        .iter()
        .filter(|b| b.companion)
        .map(|b| {
            (
                b.id,
                b.name.clone(),
                b.companion_mode,
                b.role,
                b.companion_order,
                b.memory.last_message.clone(),
            )
        })
        .collect();

    egui::Area::new(egui::Id::new("voxel_native_companion_dock"))
        .anchor(anchor, offset)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let colors = theme.semantic();
            let frame = egui::Frame::none()
                .fill(egui::Color32::from_rgba_unmultiplied(
                    colors.surface_strong.r(),
                    colors.surface_strong.g(),
                    colors.surface_strong.b(),
                    188,
                ))
                .stroke(egui::Stroke::new(1.15, colors.info))
                .inner_margin(egui::Margin::symmetric(8.0, 8.0))
                .rounding(egui::Rounding::same(10.0))
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 10.0),
                    blur: 24.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(126),
                });
            frame.show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                let horizontal = matches!(
                    settings.companion_ui.dock_position,
                    crate::settings::CompanionDockPosition::Bottom
                );
                if horizontal {
                    ui.horizontal(|ui| {
                        draw_editor_dock_icon(ui, &mut editor, theme);
                        for (id, name, mode, role, order, last) in &companions {
                            if companion_dock_icon(
                                ui,
                                name,
                                *mode,
                                *role,
                                *order,
                                open_id == *id,
                                &last,
                            )
                            .clicked()
                            {
                                brain.selected_bot = *id;
                                open_id = if open_id == *id { 0 } else { *id };
                            }
                        }
                        draw_companion_dock_settings(ui, &mut settings);
                    });
                } else {
                    draw_editor_dock_icon(ui, &mut editor, theme);
                    for (id, name, mode, role, order, last) in &companions {
                        if companion_dock_icon(
                            ui,
                            name,
                            *mode,
                            *role,
                            *order,
                            open_id == *id,
                            &last,
                        )
                        .clicked()
                        {
                            brain.selected_bot = *id;
                            open_id = if open_id == *id { 0 } else { *id };
                        }
                    }
                    draw_companion_dock_settings(ui, &mut settings);
                }

                if open_id != 0 {
                    ui.separator();
                    draw_companion_quick_menu(ui, &mut brain, open_id, theme);
                }
            });
        });
    ctx.data_mut(|d| d.insert_temp(menu_key, open_id));
}

fn draw_editor_dock_icon(
    ui: &mut egui::Ui,
    editor: &mut ResMut<EditorState>,
    theme: crate::theme::ThemeSettings,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(54.0, 54.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let fill = if editor.open {
        egui::Color32::from_rgb(0, 210, 235)
    } else {
        egui::Color32::from_rgba_unmultiplied(16, 36, 48, 202)
    };
    painter.rect_filled(rect, egui::Rounding::same(8.0), fill);
    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.center().y)),
        egui::Rounding::same(8.0),
        egui::Color32::from_rgba_unmultiplied(230, 250, 255, 30),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(8.0),
        egui::Stroke::new(1.4, theme.color.primary()),
    );
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(25.0, 25.0));
    crate::icons::paint_icon(
        &painter,
        icon_rect,
        crate::icons::Icon::Wand,
        if editor.open {
            egui::Color32::from_rgb(5, 12, 18)
        } else {
            theme.color.primary()
        },
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 7.0),
        egui::Align2::CENTER_BOTTOM,
        "ED",
        egui::FontId::monospace(9.0),
        if editor.open {
            egui::Color32::from_rgb(5, 12, 18)
        } else {
            egui::Color32::from_gray(210)
        },
    );
    if response.clicked() {
        editor.open = true;
        editor.tab = EditorTab::Bots;
    }
    response.on_hover_text("Open editor on the companion tab");
}

fn companion_dock_icon(
    ui: &mut egui::Ui,
    name: &str,
    mode: BotCompanionMode,
    role: BotRole,
    order: u8,
    selected: bool,
    last_message: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(54.0, 54.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let color = companion_mode_color(mode, role);
    painter.rect_filled(
        rect,
        egui::Rounding::same(8.0),
        egui::Color32::from_rgba_unmultiplied(16, 36, 48, 202),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.center().y)),
        egui::Rounding::same(8.0),
        egui::Color32::from_rgba_unmultiplied(230, 250, 255, 28),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(8.0),
        egui::Stroke::new(if selected { 2.4 } else { 1.2 }, color),
    );
    painter.circle_filled(
        rect.center() + egui::vec2(0.0, -3.0),
        14.0,
        egui::Color32::from_rgb(232, 240, 242),
    );
    painter.rect_filled(
        egui::Rect::from_center_size(rect.center() + egui::vec2(0.0, -3.0), egui::vec2(21.0, 9.0)),
        egui::Rounding::same(5.0),
        color,
    );
    painter.circle_filled(
        rect.center() + egui::vec2(if order == 0 { -4.0 } else { 4.0 }, -3.0),
        2.0,
        egui::Color32::from_rgb(2, 8, 12),
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 6.0),
        egui::Align2::CENTER_BOTTOM,
        name.chars().take(2).collect::<String>().to_uppercase(),
        egui::FontId::monospace(9.0),
        egui::Color32::from_gray(230),
    );
    response.on_hover_text(format!("{} // {}\n{}", name, mode.label(), last_message))
}

fn companion_mode_color(mode: BotCompanionMode, role: BotRole) -> egui::Color32 {
    match mode {
        BotCompanionMode::AwaitingInstruction => egui::Color32::from_rgb(230, 245, 255),
        BotCompanionMode::FollowingPlayer => egui::Color32::from_rgb(0, 220, 255),
        BotCompanionMode::HoldingPosition => egui::Color32::from_rgb(255, 220, 90),
        BotCompanionMode::ScanningArea => egui::Color32::from_rgb(90, 255, 220),
        BotCompanionMode::PreviewingEdit => egui::Color32::from_rgb(190, 255, 255),
        BotCompanionMode::AssistingTask => match role {
            BotRole::CompanionMaker => egui::Color32::from_rgb(255, 90, 230),
            _ => egui::Color32::from_rgb(0, 235, 180),
        },
        BotCompanionMode::Blocked => egui::Color32::from_rgb(255, 70, 45),
        BotCompanionMode::Patrolling => egui::Color32::from_rgb(255, 200, 80),
        BotCompanionMode::SurveySweep => egui::Color32::from_rgb(120, 255, 140),
    }
}

fn draw_companion_dock_settings(ui: &mut egui::Ui, settings: &mut ResMut<WorldSettings>) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(54.0, 28.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(6.0),
        egui::Color32::from_rgba_unmultiplied(20, 30, 38, 220),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(6.0),
        egui::Stroke::new(1.0, egui::Color32::from_gray(110)),
    );
    crate::icons::paint_icon(
        &painter,
        rect.shrink(6.0),
        crate::icons::Icon::Layout,
        egui::Color32::from_gray(210),
    );
    if response.clicked() {
        settings.companion_ui.dock_position = match settings.companion_ui.dock_position {
            crate::settings::CompanionDockPosition::Left => {
                crate::settings::CompanionDockPosition::Right
            }
            crate::settings::CompanionDockPosition::Right => {
                crate::settings::CompanionDockPosition::Bottom
            }
            crate::settings::CompanionDockPosition::Bottom => {
                crate::settings::CompanionDockPosition::Left
            }
        };
        settings.save();
    }
    response.on_hover_text("Move companion dock");
}

fn draw_companion_quick_menu(
    ui: &mut egui::Ui,
    brain: &mut FriendlyWorldBrain,
    companion_id: u64,
    theme: crate::theme::ThemeSettings,
) {
    let Some((name, mode, role)) = brain
        .save
        .agents
        .iter()
        .find(|b| b.id == companion_id)
        .map(|b| (b.name.clone(), b.companion_mode, b.role))
    else {
        return;
    };
    ui.set_min_width(296.0);
    let mood_color = companion_mode_color(mode, role);
    let mood_glyph = companion_mode_glyph(mode);
    let mood_word = companion_mode_word(mode);

    // ---- Mood header strip ----
    let (rect, _) = ui.allocate_exact_size(egui::vec2(280.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(8.0),
        egui::Color32::from_rgba_unmultiplied(14, 22, 30, 230),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(8.0),
        egui::Stroke::new(1.4, mood_color.gamma_multiply(0.85)),
    );
    // Pulse dot (left side).
    let dot_c = egui::pos2(rect.left() + 18.0, rect.center().y);
    painter.circle_filled(dot_c, 7.0, mood_color);
    painter.circle_stroke(
        dot_c,
        9.0,
        egui::Stroke::new(1.0, mood_color.gamma_multiply(0.55)),
    );
    // Name (white) + mood label (mood-color).
    painter.text(
        egui::pos2(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name.to_uppercase(),
        egui::FontId::monospace(13.0),
        theme.color.primary(),
    );
    painter.text(
        egui::pos2(rect.right() - 10.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{mood_glyph} {mood_word}"),
        egui::FontId::monospace(11.0),
        mood_color,
    );

    ui.add_space(4.0);

    // ---- 5 BIG primary buttons ----
    let primary: [(&str, &str, &str, BotCompanionMode, CompanionCommand); 5] = [
        (
            "⤓",
            "HERE",
            "Come stand next to me",
            BotCompanionMode::AwaitingInstruction,
            CompanionCommand::PlaceSelectedNearPlayer,
        ),
        (
            "➤",
            "FOLLOW",
            "Fly with me",
            BotCompanionMode::FollowingPlayer,
            CompanionCommand::FollowSelected,
        ),
        (
            "■",
            "WAIT",
            "Stay put right here",
            BotCompanionMode::HoldingPosition,
            CompanionCommand::HoldSelected,
        ),
        (
            "◎",
            "SCAN",
            "Look around for stuff",
            BotCompanionMode::ScanningArea,
            CompanionCommand::ScanSelected,
        ),
        (
            "⚑",
            "MARK",
            "Drop a flag at my spot",
            BotCompanionMode::AwaitingInstruction, // never highlighted
            CompanionCommand::MarkWaypointSelected,
        ),
    ];
    ui.horizontal_wrapped(|ui| {
        for (glyph, caption, tooltip, active_mode, cmd) in primary {
            // Don't highlight "MARK" — it's a one-shot action, not a mode.
            let is_active = caption != "MARK" && mode == active_mode;
            if dock_big_button(ui, glyph, caption, tooltip, is_active, mood_color).clicked() {
                brain.selected_bot = companion_id;
                brain.companion_command = Some(cmd);
            }
        }
    });

    // ---- Collapsible MORE drawer ----
    egui::CollapsingHeader::new(
        egui::RichText::new("MORE…")
            .monospace()
            .size(11.0)
            .color(egui::Color32::from_rgb(170, 200, 220)),
    )
    .default_open(false)
    .show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if dock_command_button(ui, "CLOSER", "Tighten follow distance").clicked() {
                brain.selected_bot = companion_id;
                brain.companion_command = Some(CompanionCommand::CloserSelected);
            }
            if dock_command_button(ui, "FARTHER", "Widen follow distance").clicked() {
                brain.selected_bot = companion_id;
                brain.companion_command = Some(CompanionCommand::FartherSelected);
            }
            if dock_command_button(ui, "CITY TEAM", "Enable autonomous city and road building")
                .clicked()
            {
                brain.companion_command = Some(CompanionCommand::BuildCityAutonomy);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if dock_command_button(ui, "PATROL", "Orbit me on a wide patrol arc").clicked() {
                brain.selected_bot = companion_id;
                brain.companion_command = Some(CompanionCommand::PatrolSelected);
            }
            if dock_command_button(ui, "SURVEY", "Wide-area survey sweep for the planner").clicked()
            {
                brain.selected_bot = companion_id;
                brain.companion_command = Some(CompanionCommand::SurveySelected);
            }
        });
        ui.horizontal_wrapped(|ui| {
            for assist in [
                CompanionAssistKind::Road,
                CompanionAssistKind::LandingPad,
                CompanionAssistKind::Lights,
                CompanionAssistKind::ClearFlatten,
            ] {
                if dock_command_button(ui, assist.label(), "Preview editor assist").clicked() {
                    brain.selected_bot = companion_id;
                    brain.companion_command = Some(CompanionCommand::PreviewAssist(assist));
                }
            }
        });
    });

    if let Some(preview) = &brain.save.companion_preview {
        ui.label(egui::RichText::new(&preview.message).size(10.5).color(
            if preview.status.is_valid() {
                egui::Color32::from_rgb(120, 240, 255)
            } else {
                egui::Color32::from_rgb(255, 120, 90)
            },
        ));
        ui.horizontal(|ui| {
            let can_approve = preview.status.is_valid();
            let approve = crate::ui_kit::icon_action(
                ui,
                crate::icons::Icon::Approve,
                "Approve",
                can_approve,
                theme,
            );
            if can_approve && approve.clicked() {
                brain.companion_command = Some(CompanionCommand::ExecutePreview);
            }
            if crate::ui_kit::danger_action(ui, crate::icons::Icon::Delete, "Clear", theme)
                .clicked()
            {
                brain.companion_command = Some(CompanionCommand::ClearPreview);
            }
        });
    }
}

fn companion_mode_glyph(mode: BotCompanionMode) -> &'static str {
    match mode {
        BotCompanionMode::AwaitingInstruction => "◉",
        BotCompanionMode::FollowingPlayer => "➤",
        BotCompanionMode::HoldingPosition => "■",
        BotCompanionMode::ScanningArea => "◎",
        BotCompanionMode::PreviewingEdit => "✦",
        BotCompanionMode::AssistingTask => "✧",
        BotCompanionMode::Blocked => "✕",
        BotCompanionMode::Patrolling => "↻",
        BotCompanionMode::SurveySweep => "⌬",
    }
}

fn companion_mode_word(mode: BotCompanionMode) -> &'static str {
    match mode {
        BotCompanionMode::AwaitingInstruction => "READY",
        BotCompanionMode::FollowingPlayer => "FLY",
        BotCompanionMode::HoldingPosition => "WAIT",
        BotCompanionMode::ScanningArea => "SCAN",
        BotCompanionMode::PreviewingEdit => "PLAN",
        BotCompanionMode::AssistingTask => "BUILD",
        BotCompanionMode::Blocked => "STUCK",
        BotCompanionMode::Patrolling => "PATROL",
        BotCompanionMode::SurveySweep => "SURVEY",
    }
}

fn dock_big_button(
    ui: &mut egui::Ui,
    glyph: &str,
    caption: &str,
    tooltip: &str,
    active: bool,
    accent: egui::Color32,
) -> egui::Response {
    let icon = match caption {
        "HERE" => crate::icons::Icon::Teleport,
        "FOLLOW" => crate::icons::Icon::Follow,
        "WAIT" => crate::icons::Icon::Hold,
        "SCAN" => crate::icons::Icon::Scan,
        "MARK" => crate::icons::Icon::Pin,
        _ => crate::icons::Icon::Help,
    };
    let _ = glyph;
    let size = egui::vec2(52.0, 52.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter_at(rect);
    let bg = if active {
        egui::Color32::from_rgba_unmultiplied(
            (accent.r() as u16 * 60 / 255) as u8,
            (accent.g() as u16 * 60 / 255) as u8,
            (accent.b() as u16 * 60 / 255) as u8,
            230,
        )
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(34, 50, 64, 240)
    } else {
        egui::Color32::from_rgba_unmultiplied(20, 30, 40, 230)
    };
    let stroke = if active {
        egui::Stroke::new(2.0, accent)
    } else {
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(70, 100, 120, 200),
        )
    };
    painter.rect_filled(rect, egui::Rounding::same(8.0), bg);
    painter.rect_stroke(rect, egui::Rounding::same(8.0), stroke);
    let glyph_color = if active {
        accent
    } else {
        egui::Color32::from_rgb(220, 240, 255)
    };
    crate::icons::paint_icon(
        &painter,
        egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 19.0),
            egui::vec2(20.0, 20.0),
        ),
        icon,
        glyph_color,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 11.0),
        egui::Align2::CENTER_CENTER,
        caption,
        egui::FontId::monospace(9.0),
        egui::Color32::from_rgb(220, 240, 255),
    );
    response.on_hover_text(tooltip)
}

fn dock_command_button(ui: &mut egui::Ui, label: &str, tooltip: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .monospace()
                .size(10.0)
                .strong()
                .color(egui::Color32::from_rgb(230, 245, 255)),
        )
        .fill(egui::Color32::from_rgba_unmultiplied(22, 34, 44, 230))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(80, 210, 230, 190),
        ))
        .rounding(egui::Rounding::same(5.0))
        .min_size(egui::vec2(56.0, 26.0)),
    )
    .on_hover_text(tooltip)
}

fn queue_smart_editor_task(
    brain: &mut FriendlyWorldBrain,
    kind: BotTaskKind,
    theme: BotTheme,
    width: u8,
    height: u8,
    priority: u8,
) {
    let bot_id =
        selected_companion_id(&brain.save, brain.selected_bot).unwrap_or(brain.selected_bot);
    let cmd = BotTaskCommand {
        bot_id,
        task_type: kind,
        theme,
        width,
        height,
        priority,
    };
    brain.command_draft = cmd;
    brain.queued_commands.push(cmd);
    brain.hud_message = format!(
        "{} queued for {}.",
        kind.label(),
        bot_label(&brain.save, bot_id)
    );
}

fn add_extra_companion(save: &mut BotWorldSave) -> String {
    let id = save.next_bot_id;
    save.next_bot_id += 1;
    let order = save
        .agents
        .iter()
        .filter(|bot| bot.companion)
        .map(|bot| bot.companion_order)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let roles = [
        BotRole::Planner,
        BotRole::Surveyor,
        BotRole::Architect,
        BotRole::RoadCrew,
        BotRole::Builder,
        BotRole::RepairTech,
        BotRole::ParkKeeper,
        BotRole::CompanionMaker,
        BotRole::CompanionGuide,
    ];
    let role = roles[(order as usize) % roles.len()];
    let names = [
        "Mira", "Jonas", "Nina", "Omar", "Lea", "Sam", "Eva", "Noel", "Rina", "Marco", "Tara",
        "Eli", "Yara", "Milan", "Sara", "Ben",
    ];
    let name = format!("{}-{id}", names[(order as usize) % names.len()]);
    let hub = save
        .settlements
        .first()
        .map(|s| vec3_from_arr(s.hub))
        .unwrap_or(Vec3::new(0.0, 120.0, 0.0));
    let angle = order as f32 * 2.399_963_1;
    let radius = 6.0 + (order % 8) as f32 * 1.7;
    let p = hub
        + Vec3::new(
            angle.cos() * radius,
            2.0 + (order % 3) as f32,
            angle.sin() * radius,
        );
    save.agents.push(BotAgent {
        id,
        name: name.clone(),
        role,
        state: BotState::Idle,
        position: [p.x, p.y, p.z],
        target: [p.x, p.y, p.z],
        home_id: save.settlements.first().map(|s| s.id).unwrap_or(1),
        crew_id: None,
        last_interaction_epoch: now_epoch(),
        companion: true,
        companion_order: order,
        swarm_leader_id: None,
        swarm_index: 0,
        companion_mode: BotCompanionMode::SurveySweep,
        current_task: None,
        memory: BotMemory {
            last_message: "New specialist online. I will join the city sheet and take field work."
                .into(),
            known_sites: vec![[hub.x, hub.y, hub.z]],
            favorite_theme: role.default_theme(),
            work_focus: 1.0,
            curiosity: 1.0,
            ..Default::default()
        },
    });
    name
}

fn queue_selected_area_masterplan(
    brain: &mut FriendlyWorldBrain,
    selection: Option<(IVec3, IVec3)>,
) -> usize {
    let Some((lo, hi)) = selection else {
        brain.hud_message = "No A/B area selected. Use the editor selection first.".into();
        return 0;
    };
    let min = IVec3::new(lo.x.min(hi.x), lo.y.min(hi.y), lo.z.min(hi.z));
    let max = IVec3::new(lo.x.max(hi.x), lo.y.max(hi.y), lo.z.max(hi.z));
    let size = max - min + IVec3::ONE;
    if size.x <= 0 || size.z <= 0 {
        brain.hud_message = "Selected area is too small for a bot masterplan.".into();
        return 0;
    }

    brain.save.autonomy.bots_active = true;
    brain.save.autonomy.enabled = true;
    brain.save.autonomy.intensity = 10;
    let center = Vec3::new(
        (min.x + max.x) as f32 * 0.5,
        min.y as f32,
        (min.z + max.z) as f32 * 0.5,
    );
    let district_id = nearest_district(&brain.save, center).map(|d| d.id);
    let bounds = brain.save.primary_bounds();
    let mut queued = 0usize;
    let mut push_project = |save: &mut BotWorldSave,
                            kind: BotTaskKind,
                            origin: [i32; 3],
                            size: [i32; 3],
                            theme: BotTheme,
                            priority: u8| {
        if size.iter().any(|v| *v <= 0) || !bounds.contains_box(origin, size) {
            return;
        }
        let assigned = pick_bot(save, kind.preferred_role());
        if add_project_unchecked(
            save,
            kind,
            origin,
            size,
            theme,
            assigned,
            district_id,
            None,
            priority,
            true,
        )
        .is_ok()
        {
            queued += 1;
        }
    };

    let mut x = min.x;
    while x <= max.x {
        let chunk_w = (max.x - x + 1).min(44);
        let mut z = min.z;
        while z <= max.z {
            let chunk_d = (max.z - z + 1).min(44);
            push_project(
                &mut brain.save,
                BotTaskKind::ClearFlatten,
                [x, min.y, z],
                [chunk_w, 8, chunk_d],
                BotTheme::WhiteAlloy,
                10,
            );

            if chunk_w >= 28 && chunk_d >= 28 {
                let skyline = ((x / 44) + (z / 44)).rem_euclid(4) == 0;
                let civic = ((x / 44) + (z / 44)).rem_euclid(5) == 0;
                let (kind, theme, h) = if skyline {
                    (BotTaskKind::BuildGlassTower, BotTheme::MagentaGlass, 58)
                } else if civic {
                    (BotTaskKind::BuildPlaza, BotTheme::WhiteAlloy, 8)
                } else {
                    (BotTaskKind::BuildResidentialBlock, BotTheme::WhiteAlloy, 16)
                };
                let build_w = (chunk_w - 8).clamp(17, 44);
                let build_d = (chunk_d - 8).clamp(17, 44);
                push_project(
                    &mut brain.save,
                    kind,
                    [x + 4, min.y + 1, z + 4],
                    [build_w, h, build_d],
                    theme,
                    8,
                );
            }
            z += 44;
        }
        x += 44;
    }

    let road_y = min.y + 1;
    let mut road_x = min.x;
    while road_x <= max.x {
        let w = (max.x - road_x + 1).min(88);
        push_project(
            &mut brain.save,
            BotTaskKind::BuildRoad,
            [road_x, road_y, min.z],
            [w, 7, size.z.clamp(7, 11)],
            BotTheme::AmberStreet,
            10,
        );
        road_x += 88;
    }
    let mut road_z = min.z;
    while road_z <= max.z {
        let d = (max.z - road_z + 1).min(88);
        push_project(
            &mut brain.save,
            BotTaskKind::DecorateStreet,
            [min.x, road_y, road_z],
            [size.x.clamp(32, 88), 7, d.clamp(7, 11)],
            BotTheme::AmberStreet,
            9,
        );
        road_z += 88;
    }

    if queued > 0 {
        brain.hud_message = format!(
            "Area masterplan accepted: {queued} bot field project(s) queued from A/B selection."
        );
        brain.dirty = true;
    } else {
        brain.hud_message = "Selected area is outside bot city bounds or too small.".into();
    }
    queued
}

fn draw_city_control_center(
    ui: &mut egui::Ui,
    brain: &mut FriendlyWorldBrain,
    selection: Option<(IVec3, IVec3)>,
    theme: crate::theme::ThemeSettings,
) {
    use crate::icons::Icon;

    crate::theme::section_box(ui, theme, "CITY CONTROL CENTER");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Follow,
                "BOT",
                &bot_label(&brain.save, brain.selected_bot),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Builder,
                "ACTIVE",
                &brain.active_project_count().to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::District,
                "PROJECTS",
                &brain.save.projects.len().to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Approve,
                "WORKERS",
                if brain.save.autonomy.bots_active {
                    "ON"
                } else {
                    "OFF"
                },
                theme,
            );
        });

        ui.horizontal_wrapped(|ui| {
            for bot in brain.save.agents.iter().filter(|b| b.companion) {
                let selected = brain.selected_bot == bot.id;
                let label = format!("{} / {}", bot.name, bot.role.label());
                if crate::ui_kit::tab_chip(ui, Icon::Follow, &label, selected, theme).clicked() {
                    brain.selected_bot = bot.id;
                    brain.command_draft.bot_id = bot.id;
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            if crate::ui_kit::icon_action(
                ui,
                Icon::Approve,
                if brain.save.autonomy.bots_active {
                    "Bots On"
                } else {
                    "Bots Off"
                },
                brain.save.autonomy.bots_active,
                theme,
            )
            .clicked()
            {
                brain.save.autonomy.bots_active = !brain.save.autonomy.bots_active;
                brain.dirty = true;
                brain.hud_message = if brain.save.autonomy.bots_active {
                    "Bot workers resumed. They continue from saved project cursors.".into()
                } else {
                    "Bot workers paused. Project sheets and progress are saved.".into()
                };
            }
            if crate::ui_kit::icon_action(ui, Icon::City, "Start Mega City", false, theme).clicked()
            {
                brain.save.autonomy.bots_active = true;
                brain.companion_command = Some(CompanionCommand::BuildCityAutonomy);
            }
            if crate::ui_kit::icon_action(ui, Icon::Grid, "Selected Area", false, theme).clicked() {
                queue_selected_area_masterplan(brain, selection);
            }
            if crate::ui_kit::icon_action(ui, Icon::Follow, "Add Bot", false, theme).clicked() {
                let name = add_extra_companion(&mut brain.save);
                brain.dirty = true;
                brain.hud_message = format!("{name} joined the swarm as a city specialist.");
            }
            if crate::ui_kit::icon_action(ui, Icon::Grid, "Road Grid", false, theme).clicked() {
                queue_smart_editor_task(
                    brain,
                    BotTaskKind::ExpandRoadGrid,
                    BotTheme::AmberStreet,
                    40,
                    7,
                    10,
                );
            }
            if crate::ui_kit::icon_action(ui, Icon::Builder, "Tower + Interior", false, theme)
                .clicked()
            {
                queue_smart_editor_task(
                    brain,
                    BotTaskKind::BuildGlassTower,
                    BotTheme::MagentaGlass,
                    21,
                    58,
                    9,
                );
            }
            if crate::ui_kit::icon_action(ui, Icon::District, "Residential Block", false, theme)
                .clicked()
            {
                queue_smart_editor_task(
                    brain,
                    BotTaskKind::BuildResidentialBlock,
                    BotTheme::WhiteAlloy,
                    22,
                    16,
                    8,
                );
            }
            if crate::ui_kit::icon_action(ui, Icon::Wand, "Traffic + Benches", false, theme)
                .clicked()
            {
                queue_smart_editor_task(
                    brain,
                    BotTaskKind::DecorateStreet,
                    BotTheme::AmberStreet,
                    11,
                    7,
                    8,
                );
            }
            if crate::ui_kit::icon_action(ui, Icon::Optimize, "Fit Hills", false, theme).clicked() {
                queue_smart_editor_task(
                    brain,
                    BotTaskKind::ClearFlatten,
                    BotTheme::WhiteAlloy,
                    20,
                    8,
                    10,
                );
            }
        });

        ui.horizontal_wrapped(|ui| {
            if crate::ui_kit::icon_action(
                ui,
                Icon::Optimize,
                "Continuous Autonomy",
                brain.save.autonomy.enabled,
                theme,
            )
            .clicked()
            {
                brain.save.autonomy.enabled = !brain.save.autonomy.enabled;
                brain.dirty = true;
            }
            ui.add(
                egui::Slider::new(&mut brain.save.autonomy.intensity, 1..=10)
                    .text("intelligence / speed"),
            );
            if let Some(settlement) = brain.save.settlements.first_mut() {
                ui.add(
                    egui::Slider::new(
                        &mut settlement.bounds.max_active_projects,
                        1..=MAX_ACTIVE_PROJECTS_LIMIT,
                    )
                    .text("simultaneous builds"),
                );
            }
        });

        if !brain.save.last_blocked_reason.is_empty() {
            crate::ui_kit::status_chip(
                ui,
                Icon::Help,
                "LAST BLOCKED",
                &brain.save.last_blocked_reason,
                theme,
            );
        }
    });
}

fn draw_build_spreadsheet(
    ui: &mut egui::Ui,
    brain: &FriendlyWorldBrain,
    theme: crate::theme::ThemeSettings,
) {
    use crate::icons::Icon;

    crate::theme::section_box(ui, theme, "BOT BUILD SPREADSHEET");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Grid,
                "ROWS",
                &brain
                    .save
                    .projects
                    .iter()
                    .filter(|p| !p.status.is_done())
                    .map(|p| p.concept.rows.len().max(1))
                    .sum::<usize>()
                    .to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Builder,
                "SWARM",
                &brain
                    .save
                    .agents
                    .iter()
                    .filter(|b| b.companion)
                    .count()
                    .to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Approve,
                "MODE",
                if !brain.save.autonomy.bots_active {
                    "paused"
                } else if brain.save.autonomy.enabled {
                    "continuous"
                } else {
                    "manual"
                },
                theme,
            );
        });
        egui::ScrollArea::both()
            .max_height(260.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("bot_build_spreadsheet_grid")
                    .striped(true)
                    .num_columns(7)
                    .spacing(egui::vec2(12.0, 6.0))
                    .show(ui, |ui| {
                        ui.strong("PROJECT");
                        ui.strong("STATUS");
                        ui.strong("PHASE");
                        ui.strong("OWNER");
                        ui.strong("MATERIAL");
                        ui.strong("DETAIL");
                        ui.strong("PROGRESS");
                        ui.end_row();

                        for project in brain
                            .save
                            .projects
                            .iter()
                            .filter(|p| !p.status.is_done())
                            .take(10)
                        {
                            let progress = if project.total_steps == 0 {
                                0.0
                            } else {
                                project.cursor as f32 / project.total_steps as f32
                            };
                            let rows: Vec<BotPlanRow> = if project.concept.rows.is_empty() {
                                vec![BotPlanRow {
                                    phase: "Concept".into(),
                                    owner: project_owner_label(
                                        &brain.save,
                                        project.assigned_bot,
                                        project.crew_id,
                                    ),
                                    material: project.theme.label().into(),
                                    detail: project.concept.brief.clone(),
                                    status: "queued".into(),
                                }]
                            } else {
                                project.concept.rows.clone()
                            };
                            for (idx, row) in rows.iter().enumerate() {
                                ui.label(if idx == 0 { project.label.as_str() } else { "" });
                                ui.label(if idx == 0 {
                                    format!("{:?}", project.status)
                                } else {
                                    row.status.clone()
                                });
                                ui.label(&row.phase);
                                ui.label(&row.owner);
                                ui.label(&row.material);
                                ui.label(&row.detail);
                                ui.label(if idx == 0 {
                                    format!("{:.0}%", progress * 100.0)
                                } else {
                                    String::new()
                                });
                                ui.end_row();
                            }
                        }
                    });
            });
        if brain.save.projects.iter().all(|p| p.status.is_done()) {
            ui.label("No active spreadsheet rows yet. Use Build City Team or Queue Idea.");
        }
    });
}

pub fn draw_bots_editor(
    ui: &mut egui::Ui,
    brain: &mut FriendlyWorldBrain,
    settings: &mut WorldSettings,
    selection: Option<(IVec3, IVec3)>,
) {
    use crate::icons::Icon;
    let theme = settings.theme;

    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Follow,
                "COMPANIONS",
                &brain.save.agents.len().to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Builder,
                "ACTIVE",
                &brain.active_project_count().to_string(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Approve,
                "DONE",
                &brain.save.completed_projects.to_string(),
                theme,
            );
        });
    });
    ui.add_space(8.0);

    draw_city_control_center(ui, brain, selection, theme);
    ui.add_space(8.0);

    egui::CollapsingHeader::new("Advanced companion controls")
        .default_open(false)
        .show(ui, |ui| {
            crate::theme::section_box(ui, theme, "COMPANION COMMANDS");
            crate::ui_kit::surface_panel(ui, theme, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if crate::ui_kit::icon_action(ui, Icon::Teleport, "Swarm Here", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::PlaceBothNearPlayer);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Teleport, "Selected Here", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::PlaceSelectedNearPlayer);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Follow Swarm", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::FollowBoth);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Follow One", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::FollowSelected);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Closer Swarm", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::CloserBoth);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Closer One", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::CloserSelected);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Farther Swarm", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::FartherBoth);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Follow, "Farther One", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::FartherSelected);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Hold, "Hold Swarm", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::HoldBoth);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Hold, "Hold One", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::HoldSelected);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Scan, "Scan Swarm", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::ScanBoth);
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Scan, "Scan One", false, theme)
                        .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::ScanSelected);
                    }
                    if crate::ui_kit::icon_action(
                        ui,
                        Icon::Builder,
                        "Build City Team",
                        false,
                        theme,
                    )
                    .clicked()
                    {
                        brain.companion_command = Some(CompanionCommand::BuildCityAutonomy);
                    }
                });
                crate::ui_kit::status_chip(
                    ui,
                    Icon::Help,
                    "MODE",
                    "manual unless autonomy is enabled",
                    theme,
                );
            });
        });
    ui.add_space(8.0);

    egui::CollapsingHeader::new("Dock and inventory assist")
        .default_open(false)
        .show(ui, |ui| {
            crate::theme::section_box(ui, theme, "SCREEN DOCK + INVENTORY ASSIST");
            crate::ui_kit::surface_panel(ui, theme, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if crate::ui_kit::icon_action(
                        ui,
                        Icon::Layout,
                        "Screen Dock",
                        settings.companion_ui.show_companion_dock,
                        theme,
                    )
                    .clicked()
                    {
                        settings.companion_ui.show_companion_dock =
                            !settings.companion_ui.show_companion_dock;
                        settings.save();
                    }
                    if crate::ui_kit::icon_action(
                        ui,
                        Icon::Wand,
                        "Assist Cards",
                        settings.companion_ui.editor_assist_enabled,
                        theme,
                    )
                    .clicked()
                    {
                        settings.companion_ui.editor_assist_enabled =
                            !settings.companion_ui.editor_assist_enabled;
                        settings.save();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Dock:");
                    for (pos, label) in [
                        (crate::settings::CompanionDockPosition::Left, "LEFT"),
                        (crate::settings::CompanionDockPosition::Right, "RIGHT"),
                        (crate::settings::CompanionDockPosition::Bottom, "BOTTOM"),
                    ] {
                        let selected = settings.companion_ui.dock_position == pos;
                        if crate::ui_kit::tab_chip(ui, Icon::Layout, label, selected, theme)
                            .clicked()
                        {
                            settings.companion_ui.dock_position = pos;
                            settings.save();
                        }
                    }
                });
                if let Some(preview) = &brain.save.companion_preview {
                    crate::ui_kit::status_chip(
                        ui,
                        Icon::Approve,
                        "PREVIEW",
                        &format!(
                            "{} / {:?} / {}x{}x{}",
                            preview.assist.label(),
                            preview.status,
                            preview.size[0],
                            preview.size[1],
                            preview.size[2]
                        ),
                        theme,
                    );
                }
            });
        });
    ui.add_space(8.0);

    if !brain.save.settlements.is_empty() {
        let bounds = brain.save.settlements[0].bounds;
        crate::theme::section_box(ui, theme, "CITY AUTONOMY");
        crate::ui_kit::surface_panel(ui, theme, |ui| {
            ui.horizontal_wrapped(|ui| {
                crate::ui_kit::status_chip(
                    ui,
                    Icon::Globe,
                    "BOUNDARY",
                    &format!("{} blocks", bounds.radius),
                    theme,
                );
                crate::ui_kit::status_chip(
                    ui,
                    Icon::Grid,
                    "USED",
                    &format!("{:.0}", bounds.used_radius),
                    theme,
                );
                crate::ui_kit::status_chip(
                    ui,
                    Icon::District,
                    "DISTRICTS",
                    &brain.save.districts.len().to_string(),
                    theme,
                );
            });
            ui.horizontal_wrapped(|ui| {
                if crate::ui_kit::icon_action(
                    ui,
                    Icon::Optimize,
                    "Autonomy",
                    brain.save.autonomy.enabled,
                    theme,
                )
                .clicked()
                {
                    brain.save.autonomy.enabled = !brain.save.autonomy.enabled;
                    brain.hud_message = if brain.save.autonomy.enabled {
                        "Bot city autonomy resumed.".into()
                    } else {
                        "Bot city autonomy paused.".into()
                    };
                    brain.dirty = true;
                }
                if crate::ui_kit::icon_action(ui, Icon::Wand, "Queue Idea", false, theme).clicked()
                {
                    brain.force_city_idea = true;
                    brain.hud_message = "Companions queued an optional city idea.".into();
                }
            });
            ui.add(
                egui::Slider::new(&mut brain.save.autonomy.intensity, 1..=10)
                    .text("autonomy intensity"),
            );
            ui.add(
                egui::Slider::new(
                    &mut brain.save.settlements[0].bounds.max_active_projects,
                    1..=MAX_ACTIVE_PROJECTS_LIMIT,
                )
                .text("max active projects"),
            );
            if !brain.save.last_blocked_reason.is_empty() {
                crate::ui_kit::status_chip(
                    ui,
                    Icon::Help,
                    "BLOCKED",
                    &brain.save.last_blocked_reason,
                    theme,
                );
            }
        });
    }
    ui.add_space(8.0);

    draw_build_spreadsheet(ui, brain, theme);
    ui.add_space(8.0);

    crate::theme::section_box(ui, theme, "VISIT");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            if crate::ui_kit::icon_action(ui, Icon::Teleport, "Active Build", false, theme)
                .clicked()
            {
                brain.visit_request = Some(BotVisitTarget::ActiveBuild);
                brain.hud_message = "Preparing visit to active bot build.".into();
            }
            if crate::ui_kit::icon_action(ui, Icon::Teleport, "Nearest Bot", false, theme).clicked()
            {
                brain.visit_request = Some(BotVisitTarget::NearestBot);
                brain.hud_message = "Preparing visit to nearest friendly bot.".into();
            }
            if crate::ui_kit::icon_action(ui, Icon::Teleport, "Selected Bot", false, theme)
                .clicked()
            {
                brain.visit_request = Some(BotVisitTarget::SelectedBot(brain.selected_bot));
                brain.hud_message = "Preparing visit to selected bot.".into();
            }
            if crate::ui_kit::icon_action(ui, Icon::City, "City Hub", false, theme).clicked() {
                brain.visit_request = Some(BotVisitTarget::CityHub);
                brain.hud_message = "Preparing visit to bot city hub.".into();
            }
            if crate::ui_kit::icon_action(ui, Icon::District, "District", false, theme).clicked() {
                brain.visit_request =
                    Some(BotVisitTarget::SelectedDistrict(brain.selected_district));
                brain.hud_message = "Preparing visit to selected district.".into();
            }
        });
        crate::ui_kit::status_chip(ui, Icon::Help, "VISIT", "teleport facing the target", theme);
    });
    ui.add_space(8.0);

    crate::theme::section_box(ui, theme, "DISTRICTS");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            for district in &brain.save.districts {
                let selected = brain.selected_district == district.id;
                let label = format!(
                    "{} // {} // {} done",
                    district.name,
                    district.kind.label(),
                    district.completed_projects
                );
                if crate::ui_kit::tab_chip(ui, Icon::District, &label, selected, theme).clicked() {
                    brain.selected_district = district.id;
                }
            }
        });
        if let Some(district) = brain
            .save
            .districts
            .iter()
            .find(|d| d.id == brain.selected_district)
        {
            crate::ui_kit::status_chip(
                ui,
                Icon::Pin,
                "SELECTED",
                &format!(
                    "X {:.0} / Z {:.0} / {} slots",
                    district.center[0],
                    district.center[2],
                    district.build_slots.len()
                ),
                theme,
            );
        }
    });
    ui.add_space(8.0);

    crate::theme::section_box(ui, theme, "COMPANION SELECT");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            for bot in &brain.save.agents {
                let selected = brain.selected_bot == bot.id;
                let label = format!(
                    "{} // {} // {}",
                    bot.name,
                    bot.role.label(),
                    bot.companion_mode.label()
                );
                if crate::ui_kit::tab_chip(ui, Icon::Follow, &label, selected, theme).clicked() {
                    brain.selected_bot = bot.id;
                    brain.command_draft.bot_id = bot.id;
                }
            }
        });

        if let Some(bot) = brain
            .save
            .agents
            .iter()
            .find(|b| b.id == brain.selected_bot)
        {
            crate::ui_kit::status_chip(
                ui,
                Icon::Help,
                &bot.name,
                &format!(
                    "{} // focus {:.0}% // fatigue {:.0}%",
                    bot.memory.last_message,
                    bot.memory.work_focus * 100.0,
                    bot.memory.fatigue * 100.0
                ),
                theme,
            );
        }
    });

    crate::theme::section_box(ui, theme, "QUEUE TASK");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        egui::ComboBox::from_label("Task")
            .selected_text(brain.command_draft.task_type.label())
            .show_ui(ui, |ui| {
                for kind in BotTaskKind::ALL {
                    ui.selectable_value(&mut brain.command_draft.task_type, kind, kind.label());
                }
            });
        egui::ComboBox::from_label("Theme / Color")
            .selected_text(brain.command_draft.theme.label())
            .show_ui(ui, |ui| {
                for theme in BotTheme::ALL {
                    ui.selectable_value(&mut brain.command_draft.theme, theme, theme.label());
                }
            });
        ui.add(egui::Slider::new(&mut brain.command_draft.width, 3..=40).text("width / footprint"));
        ui.add(egui::Slider::new(&mut brain.command_draft.height, 1..=48).text("height"));
        ui.add(egui::Slider::new(&mut brain.command_draft.priority, 1..=10).text("priority"));
        if crate::ui_kit::icon_action(ui, Icon::Wand, "Queue Task", false, theme).clicked() {
            let mut cmd = brain.command_draft;
            cmd.bot_id = brain.selected_bot;
            brain.queued_commands.push(cmd);
            brain.hud_message = "Manual bot task queued.".into();
        }
    });

    crate::theme::section_box(ui, theme, "PROJECTS");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        egui::Grid::new("bot_projects")
            .striped(true)
            .show(ui, |ui| {
                ui.label("Project");
                ui.label("Status");
                ui.label("Progress");
                ui.end_row();
                for project in brain.save.projects.iter().rev().take(12) {
                    let progress = if project.total_steps == 0 {
                        1.0
                    } else {
                        project.cursor as f32 / project.total_steps as f32
                    };
                    ui.label(&project.label);
                    ui.label(format!("{:?}", project.status));
                    ui.label(format!("{:>3.0}%", progress.clamp(0.0, 1.0) * 100.0));
                    ui.end_row();
                }
            });
    });
}

fn pick_bot(save: &BotWorldSave, preferred: BotRole) -> Option<u64> {
    save.agents
        .iter()
        .find(|b| b.role == preferred && b.current_task.is_none())
        .or_else(|| save.agents.iter().find(|b| b.current_task.is_none()))
        .or_else(|| save.agents.first())
        .map(|b| b.id)
}

fn bot_label(save: &BotWorldSave, id: u64) -> String {
    save.agents
        .iter()
        .find(|b| b.id == id)
        .map(|b| b.name.clone())
        .unwrap_or_else(|| "Bot".into())
}

fn vec3_from_arr(v: [f32; 3]) -> Vec3 {
    Vec3::new(v[0], v[1], v[2])
}

fn now_epoch() -> u64 {
    crate::platform::now_epoch()
}

fn despawn(commands: &mut Commands, entity: Entity) {
    if let Some(entity_commands) = commands.get_entity(entity) {
        entity_commands.despawn_recursive();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_test_city_columns_loaded(world: &mut VoxelWorld, min: i32, max: i32) {
        for cx in min..=max {
            for cz in min..=max {
                world.loaded_column_counts.insert((cx, cz), 1);
            }
        }
    }

    #[test]
    fn bot_world_save_round_trips() {
        let mut save = BotWorldSave::default();
        save.next_bot_id = 8;
        save.agents.push(BotAgent {
            id: 7,
            name: "Ada".into(),
            role: BotRole::Planner,
            state: BotState::Idle,
            position: [1.0, 2.0, 3.0],
            target: [4.0, 5.0, 6.0],
            home_id: 1,
            crew_id: Some(3),
            last_interaction_epoch: 44,
            companion: true,
            companion_order: 0,
            swarm_leader_id: None,
            swarm_index: 0,
            companion_mode: BotCompanionMode::FollowingPlayer,
            current_task: None,
            memory: BotMemory {
                completed_tasks: 2,
                last_message: "remembered".into(),
                known_sites: vec![[1.0, 2.0, 3.0]],
                favorite_theme: BotTheme::CyanAlloy,
                relationships: vec![BotRelationship {
                    other_id: 8,
                    trust: 0.7,
                    collaboration_score: 3.0,
                    last_interaction_epoch: 40,
                }],
                recent_conversation_keys: vec!["hub:road".into()],
                fatigue: 0.2,
                curiosity: 0.8,
                work_focus: 0.6,
                preferred_follow_distance: COMPANION_FOLLOW_DEFAULT,
                last_idea_epoch: 33,
            },
        });
        save.next_district_id = 4;
        save.next_idea_id = 5;
        save.next_conversation_id = 6;
        save.next_crew_id = 7;
        let text = ron::ser::to_string(&save).unwrap();
        let back: BotWorldSave = ron::from_str(&text).unwrap();
        assert_eq!(back.next_bot_id, 8);
        assert_eq!(back.next_district_id, 4);
        assert_eq!(back.next_idea_id, 5);
        assert_eq!(back.next_conversation_id, 6);
        assert_eq!(back.next_crew_id, 7);
        assert_eq!(back.agents[0].id, 7);
        assert_eq!(back.agents[0].memory.completed_tasks, 2);
        assert_eq!(back.agents[0].crew_id, Some(3));
        assert!(back.agents[0].companion);
        assert_eq!(
            back.agents[0].companion_mode,
            BotCompanionMode::FollowingPlayer
        );
        assert_eq!(back.agents[0].memory.relationships[0].other_id, 8);
    }

    #[test]
    fn legacy_v1_bot_world_loads_with_v2_defaults() {
        let text = r#"(
            version: 1,
            next_bot_id: 2,
            next_project_id: 1,
            settlements: [(
                id: 1,
                name: "Old Bot City",
                hub: (0.0, 100.0, 0.0),
                radius: 96,
                theme: CyanAlloy,
                road_count: 0,
                building_count: 0,
                park_count: 0,
            )],
            agents: [(
                id: 1,
                name: "Ada",
                role: Planner,
                state: Idle,
                position: (0.0, 102.0, 0.0),
                target: (0.0, 102.0, 0.0),
                home_id: 1,
                current_task: None,
                memory: (
                    completed_tasks: 0,
                    last_message: "legacy",
                    known_sites: [],
                    favorite_theme: CyanAlloy,
                ),
            )],
            projects: [],
            journal: [],
            completed_projects: 0,
        )"#;
        let mut save: BotWorldSave = ron::from_str(text).unwrap();
        save.normalize();
        assert_eq!(save.version, 2);
        assert_eq!(save.settlements[0].radius, MEGA_CITY_RADIUS);
        assert_eq!(save.settlements[0].bounds.radius, MEGA_CITY_RADIUS);
        assert!(!save.districts.is_empty());
        assert!(!save.autonomy.enabled);
        assert_eq!(save.agents.len(), 2);
        assert_eq!(save.agents[0].name, "Iris");
        assert_eq!(save.agents[1].name, "Orion");
        assert!(save.agents.iter().all(|b| b.companion));
        assert_eq!(save.agents[0].crew_id, None);
        assert_eq!(save.agents[0].memory.curiosity, default_curiosity());
        assert_eq!(save.agents[0].memory.work_focus, default_work_focus());
    }

    #[test]
    fn project_validator_rejects_out_of_bounds_and_empty_jobs() {
        let mut save = BotWorldSave::default();
        save.settlements.push(BotSettlement {
            id: 1,
            name: "Bounds".into(),
            hub: [0.0, 90.0, 0.0],
            radius: MEGA_CITY_RADIUS,
            bounds: BotCityBounds {
                center: [0.0, 90.0, 0.0],
                radius: MEGA_CITY_RADIUS,
                used_radius: 0.0,
                max_active_projects: DEFAULT_MAX_ACTIVE_PROJECTS,
            },
            theme: BotTheme::CyanAlloy,
            road_count: 0,
            building_count: 0,
            park_count: 0,
        });
        assert!(validate_project_shape_and_bounds(&save, [10, 90, 10], [8, 8, 8]).is_ok());
        assert!(validate_project_shape_and_bounds(&save, [2000, 90, 0], [8, 8, 8]).is_err());
        assert!(validate_project_shape_and_bounds(&save, [0, 90, 0], [0, 8, 8]).is_err());
    }

    #[test]
    fn conversation_cooldown_suppresses_repeated_planning_spam() {
        let mut save = BotWorldSave::default();
        save.agents.push(test_bot(1, "Ada", BotRole::Planner));
        save.agents.push(test_bot(2, "Mira", BotRole::Surveyor));
        save.agents.push(test_bot(3, "Nova", BotRole::Architect));
        normalize_relationships(&mut save);
        save.ideas.push(BotIdea {
            id: 1,
            author_id: 1,
            kind: BotTaskKind::BuildGlassTower,
            target: [8, 90, 8],
            score: 10.0,
            status: BotIdeaStatus::Proposed,
            summary: "Ada proposes Glass Tower".into(),
            district_id: None,
            created_epoch: 1,
            cooldown_key: "skyline:tower".into(),
        });
        assert!(record_planning_conversation(&mut save, 0).is_some());
        assert!(record_planning_conversation(&mut save, 0).is_none());
        assert_eq!(save.conversations.len(), 1);
    }

    #[test]
    fn connected_city_slots_score_above_isolated_slots() {
        let connected = score_city_slot(0.8, 1.0, true, 0.7, true);
        let isolated = score_city_slot(0.8, 0.0, true, 0.7, true);
        assert!(connected > isolated);
        assert!(score_city_slot(1.0, 1.0, false, 1.0, true) < 0.0);
    }

    #[test]
    fn road_grade_lifts_valleys_without_cutting_hilltops() {
        let valley_grade = smoothed_road_grade_y(72, [80, 82, 81, 79, 83, 80, 82, 81], true);
        assert!(valley_grade > 72);
        assert!(valley_grade <= 78);

        let hilltop_grade = smoothed_road_grade_y(92, [82, 84, 85, 83, 81, 86, 84, 82], true);
        assert_eq!(hilltop_grade, 92);
    }

    #[test]
    fn road_support_depth_reaches_raised_terrain_grades() {
        assert_eq!(road_support_depth(72, 77, 1, true), 5);
        assert_eq!(road_support_depth(72, 77, 1, false), 3);
        assert_eq!(road_support_depth(72, 73, 0, false), 0);
        assert_eq!(road_support_depth(72, 73, 4, false), 2);
    }

    #[test]
    fn road_route_profile_prefers_continuous_grades_over_jagged_hill_cuts() {
        let rolling_contour = [72, 73, 74, 74, 75, 76, 76, 77, 78];
        let jagged_cut = [72, 86, 71, 90, 69, 88, 70, 91, 73];

        let contour_score = road_route_profile_score(&rolling_contour);
        let jagged_score = road_route_profile_score(&jagged_cut);

        assert!(
            contour_score > 0.80,
            "rolling contour route should stay high, got {contour_score}"
        );
        assert!(
            jagged_score < 0.35,
            "jagged route should be rejected, got {jagged_score}"
        );
        assert!(
            contour_score > jagged_score + 0.50,
            "road planner should strongly prefer continuous grades: {contour_score} vs {jagged_score}"
        );
    }

    #[test]
    fn access_road_site_score_includes_route_grade_fit() {
        let smooth = score_city_slot_with_route_fit(0.7, 1.0, true, 0.8, true, 0.95);
        let jagged = score_city_slot_with_route_fit(0.7, 1.0, true, 0.8, true, 0.10);

        assert!(
            smooth > jagged + 1.0,
            "access road scoring should reward terrain-following routes: smooth {smooth}, jagged {jagged}"
        );
    }

    #[test]
    fn frontage_score_prefers_lots_with_a_real_road_edge() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Frontage Test".into(),
            center: [0.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![[0, 90, 0]],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Road".into(),
            origin: [-48, 90, 0],
            size: [96, 7, 12],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let road_front = road_frontage_score(&save, &district, [-12, 90, 14], [24, 20, 24]);
        let vague_nearby = road_frontage_score(&save, &district, [-12, 90, 42], [24, 20, 24]);

        assert!(road_front > 0.85, "frontage score was {road_front}");
        assert!(
            road_front > vague_nearby + 0.30,
            "frontage {road_front} should beat vague nearby {vague_nearby}"
        );
    }

    #[test]
    fn frontage_score_uses_full_road_segments_not_only_sample_points() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Long Road Test".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Long Boulevard".into(),
            origin: [-96, 90, 0],
            size: [192, 7, 12],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let score = road_frontage_score(&save, &district, [30, 90, 14], [20, 36, 20]);

        assert!(score > 0.90, "long-road frontage score was {score}");
    }

    #[test]
    fn user_drawn_road_components_become_bot_frontage_guides() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Player Boulevard".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        );

        assert!(sync_user_city_roads(&mut save, &[road]));

        assert!(district_has_road_access(&save, &district));
        let score = road_frontage_score(&save, &district, [-12, 90, 14], [24, 36, 24]);
        assert!(
            score > 0.85,
            "bot planner should treat player road components as real build frontage, got {score}"
        );
    }

    #[test]
    fn user_drawn_road_texture_guides_nearby_architecture_theme() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Neon Residential Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district);
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        );

        sync_user_city_roads(&mut save, &[road]);

        assert_eq!(district_theme(&save, 7), Some(BotTheme::MagentaGlass));
    }

    #[test]
    fn straight_player_road_project_concept_records_frontage_intent() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Player Frontage".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district);
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        );
        sync_user_city_roads(&mut save, &[road]);
        let size = autonomous_project_size(BotTaskKind::BuildResidentialBlock);

        add_project_unchecked(
            &mut save,
            BotTaskKind::BuildResidentialBlock,
            [-22, 90, 14],
            size,
            BotTheme::MagentaGlass,
            None,
            Some(7),
            None,
            8,
            false,
        )
        .unwrap();

        let project = save.projects.last().unwrap();
        assert!(
            project
                .concept
                .rows
                .iter()
                .any(|row| row.phase == "Player Road Frontage"
                    && row.detail.contains("player road")
                    && row.detail.contains("width 7")),
            "bot spreadsheet should expose the straight player road frontage: {:?}",
            project.concept.rows
        );
    }

    #[test]
    fn player_road_frontage_concept_records_bridge_grade_from_component() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Bridge District".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district);
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        )
        .with_endpoint_heights(0, 18);
        sync_user_city_roads(&mut save, &[road]);
        let guide = save.user_roads.first().unwrap();
        assert!(
            guide.points.iter().map(|point| point[1]).max().unwrap()
                > guide.points.iter().map(|point| point[1]).min().unwrap(),
            "bot road guide should keep sampled bridge grade heights: {:?}",
            guide.points
        );
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        add_project_unchecked(
            &mut save,
            BotTaskKind::BuildGlassTower,
            [-22, 90, 14],
            size,
            BotTheme::MagentaGlass,
            None,
            Some(7),
            None,
            8,
            false,
        )
        .unwrap();

        let project = save.projects.last().unwrap();
        assert!(
            project.concept.rows.iter().any(|row| {
                row.phase == "Player Road Frontage"
                    && row.detail.contains("bridge grade")
                    && row.detail.contains("90->108")
            }),
            "bot concept should tell builders how the road bridge grade blends into architecture: {:?}",
            project.concept.rows
        );
    }

    #[test]
    fn roadside_lot_origins_align_to_nearby_raised_road_grade() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Bridge Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        )
        .with_endpoint_heights(0, 18);
        sync_user_city_roads(&mut save, &[road]);
        let world = VoxelWorld::new();
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        let lots = roadside_lot_origins(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
            0,
        );
        let raised_lot = lots
            .iter()
            .filter(|origin| {
                let center_x = origin[0] as f32 + size[0] as f32 * 0.5;
                let center_z = origin[2] as f32 + size[2] as f32 * 0.5;
                center_x > 28.0 && center_z.abs() <= 44.0
            })
            .max_by_key(|origin| origin[1])
            .copied()
            .expect("raised bridge should create a road-front skyline lot");

        assert!(
            raised_lot[1] >= 104,
            "road-front lot should inherit the nearby raised bridge deck instead of terrain height, got {raised_lot:?}"
        );
    }

    #[test]
    fn road_grade_alignment_uses_frontage_edge_for_deep_lots() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Deep Tower Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let road = crate::city::RoadSegment::new(
            IVec3::new(-48, 90, 0),
            IVec3::new(48, 90, 0),
            7,
            crate::city::RoadStyle::Neon,
        )
        .with_endpoint_heights(18, 18);
        sync_user_city_roads(&mut save, &[road]);
        let size = [42, 58, 42];
        let origin = [-21, 80, 44];

        let world = VoxelWorld::new();
        let aligned = align_lot_origin_to_road_grade(&save, &world, &district, origin, size);

        assert!(
            aligned[1] >= 108,
            "deep road-front lots should align from the building edge even when the center is far from the road, got {aligned:?}"
        );
    }

    #[test]
    fn road_grade_alignment_uses_bot_built_road_deck() {
        let world = VoxelWorld::new();
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Bot Road Grade Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let road_size = [96, 7, 12];
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Bot Road".into(),
            origin: [-48, 90, 0],
            size: road_size,
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });
        let size = autonomous_project_size(BotTaskKind::BuildResidentialBlock);
        let origin = [-18, 1, 20];
        let road_x = 0;
        let road_z = build_road_center_z(0, road_size[2], road_x - save.projects[0].origin[0]);
        let expected_deck_y = road_grade_y(&world, road_x, road_z, true);

        let aligned = align_lot_origin_to_road_grade(&save, &world, &district, origin, size);

        assert!(
            aligned[1] >= expected_deck_y,
            "lots beside bot-built roads should inherit the road deck y={expected_deck_y}, got {aligned:?}"
        );
    }

    #[test]
    fn user_drawn_roundabout_guides_civic_plaza_planning() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Roundabout Civic Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );

        sync_user_city_roads(&mut save, &[roundabout]);

        assert_eq!(
            choose_district_project(&save, &district, 0, false),
            BotTaskKind::BuildPlaza,
            "roundabouts should become civic anchors instead of another generic block"
        );
    }

    #[test]
    fn semantic_road_guides_yield_after_their_anchor_project_exists() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Roundabout Civic Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        sync_user_city_roads(&mut save, &[roundabout]);
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildPlaza,
            label: "Roundabout Plaza".into(),
            origin: [-21, 90, -21],
            size: autonomous_project_size(BotTaskKind::BuildPlaza),
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 8,
            concept: BotProjectConcept::default(),
        });

        assert_eq!(
            choose_district_project(&save, &district, 0, false),
            BotTaskKind::BuildResidentialBlock,
            "after a roundabout plaza exists, bots should diversify into district infill"
        );
    }

    #[test]
    fn semantic_road_guides_progress_from_roundabout_to_corner_landmark() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Layered Semantic Roads".into(),
            center: [16.0, 90.0, 8.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        let corner = crate::city::RoadSegment::new(
            IVec3::new(24, 90, -24),
            IVec3::new(56, 90, 18),
            7,
            crate::city::RoadStyle::Neon,
        );
        sync_user_city_roads(&mut save, &[roundabout, corner]);
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildPlaza,
            label: "Roundabout Plaza".into(),
            origin: [-21, 90, -21],
            size: autonomous_project_size(BotTaskKind::BuildPlaza),
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 8,
            concept: BotProjectConcept::default(),
        });

        assert_eq!(
            choose_district_project(&save, &district, 2, false),
            BotTaskKind::BuildGlassTower,
            "after the roundabout anchor is built, bots should continue to the remaining corner landmark before generic skyline infill"
        );
    }

    #[test]
    fn roundabout_plaza_site_centers_on_user_road_anchor() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Roundabout Civic Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        sync_user_city_roads(&mut save, &[roundabout]);
        let size = autonomous_project_size(BotTaskKind::BuildPlaza);

        let origin =
            find_loaded_build_site(&save, &world, &district, BotTaskKind::BuildPlaza, size, 0)
                .unwrap();
        let center = project_center(origin, size);
        let distance = Vec2::new(center.x, center.z).distance(Vec2::ZERO);

        assert!(
            distance <= 8.0,
            "roundabout plaza should center on the user road anchor, got center {center:?}"
        );
    }

    #[test]
    fn roundabout_anchor_project_concept_records_player_road_intent() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Roundabout Civic Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district);
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        sync_user_city_roads(&mut save, &[roundabout]);
        let size = autonomous_project_size(BotTaskKind::BuildPlaza);

        add_project_unchecked(
            &mut save,
            BotTaskKind::BuildPlaza,
            [-21, 90, -21],
            size,
            BotTheme::WhiteAlloy,
            None,
            Some(7),
            None,
            8,
            false,
        )
        .unwrap();

        let project = save.projects.last().unwrap();
        assert!(
            project
                .concept
                .rows
                .iter()
                .any(|row| row.phase == "Roundabout Anchor"
                    && row.detail.contains("player road roundabout")),
            "bot spreadsheet should expose the user roundabout as the reason for this plaza: {:?}",
            project.concept.rows
        );
    }

    #[test]
    fn roundabout_anchor_project_concept_records_structured_anchor_shape() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Roundabout Civic Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district);
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        sync_user_city_roads(&mut save, &[roundabout]);
        let size = autonomous_project_size(BotTaskKind::BuildPlaza);

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildPlaza,
            BotTheme::WhiteAlloy,
            [-21, 90, -21],
            size,
            "Roundabout Plaza",
            false,
            None,
            None,
        );

        assert_eq!(
            concept.semantic_anchor_shape,
            Some(BotRoadGuideShape::Roundabout),
            "semantic roundabout intent should survive into the project concept for voxel stamping"
        );
    }

    #[test]
    fn user_drawn_corner_guides_skyline_landmark_planning() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Corner Skyline".into(),
            center: [16.0, 90.0, 8.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let corner = crate::city::RoadSegment::new(
            IVec3::new(0, 90, 0),
            IVec3::new(32, 90, 24),
            7,
            crate::city::RoadStyle::Neon,
        );

        sync_user_city_roads(&mut save, &[corner]);

        assert_eq!(
            choose_district_project(&save, &district, 1, false),
            BotTaskKind::BuildGlassTower,
            "street corners should unlock skyline landmarks instead of asking for another grid"
        );
    }

    #[test]
    fn corner_landmark_site_sets_back_from_user_road_turn() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Corner Skyline".into(),
            center: [16.0, 90.0, 8.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let corner = crate::city::RoadSegment::new(
            IVec3::new(0, 90, 0),
            IVec3::new(32, 90, 24),
            7,
            crate::city::RoadStyle::Neon,
        );
        sync_user_city_roads(&mut save, &[corner]);
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        let origin = find_loaded_build_site(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
            1,
        )
        .unwrap();
        let center = project_center(origin, size);
        let turn = Vec2::new(32.0, 0.0);
        let distance = Vec2::new(center.x, center.z).distance(turn);
        let frontage = road_frontage_score(&save, &district, origin, size);

        assert!(
            distance >= 18.0,
            "corner tower should become an adjacent street-facing lot, not occupy the road turn; got center {center:?}"
        );
        assert!(
            frontage > 0.85,
            "set-back corner tower should still read as road frontage, got score {frontage} at origin {origin:?}"
        );
    }

    #[test]
    fn semantic_corner_landmark_origin_aligns_to_raised_turn_grade() {
        let world = VoxelWorld::new();
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Raised Corner Skyline".into(),
            center: [16.0, 90.0, 8.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let corner = crate::city::RoadSegment::new(
            IVec3::new(0, 90, 0),
            IVec3::new(32, 90, 24),
            7,
            crate::city::RoadStyle::Neon,
        )
        .with_turn_height(24);
        sync_user_city_roads(&mut save, &[corner]);
        let guide_turn_y = save
            .user_roads
            .first()
            .and_then(|guide| guide.points.iter().map(|point| point[1]).max())
            .expect("raised corner guide should keep sampled turn height");
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        let origins = semantic_road_site_origins(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
        );

        assert!(
            origins.iter().any(|origin| origin[1] >= guide_turn_y),
            "semantic corner landmarks should inherit the raised road turn deck {guide_turn_y}, got {origins:?}"
        );
    }

    #[test]
    fn segment_distance_is_zero_when_segments_cross_between_endpoints() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(10.0, 0.0);
        let c = Vec2::new(5.0, -5.0);
        let d = Vec2::new(5.0, 5.0);

        assert_eq!(segment_to_segment_distance(a, b, c, d), 0.0);
    }

    #[test]
    fn road_network_segments_follow_build_road_curves() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Curved Road Test".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Curved Access".into(),
            origin: [0, 90, 0],
            size: [88, 7, 11],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let segments = road_network_segments(&save, &district);
        let mut z_samples: Vec<i32> = segments
            .iter()
            .flat_map(|(a, b)| [a.y.round() as i32, b.y.round() as i32])
            .collect();
        z_samples.sort_unstable();
        z_samples.dedup();

        assert!(
            segments.len() > 4,
            "curved road should be represented by a polyline, got {segments:?}"
        );
        assert!(
            z_samples.len() > 1,
            "road graph should preserve the visual road curve, got {z_samples:?}"
        );
    }

    #[test]
    fn roadside_lot_origins_sample_midblocks_along_long_roads() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Midblock Test".into(),
            center: [0.0, 90.0, 0.0],
            radius: 160,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Long Boulevard".into(),
            origin: [-96, 90, 0],
            size: [192, 7, 11],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });
        let world = VoxelWorld::new();
        let size = autonomous_project_size(BotTaskKind::BuildHome);

        let lots = roadside_lot_origins(&save, &world, &district, BotTaskKind::BuildHome, size, 0);
        let centers: Vec<Vec2> = lots
            .iter()
            .map(|origin| {
                Vec2::new(
                    origin[0] as f32 + size[0] as f32 * 0.5,
                    origin[2] as f32 + size[2] as f32 * 0.5,
                )
            })
            .collect();

        assert!(
            centers.iter().any(|center| (center.x - 48.0).abs() <= 10.0
                && (center.y - 5.0).abs() >= 12.0
                && (center.y - 5.0).abs() <= 32.0),
            "long roads need buildable midblock frontage, got {centers:?}"
        );
    }

    #[test]
    fn roadside_lot_origins_keep_roundabout_interiors_reserved() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Roundabout Edge".into(),
            center: [0.0, 90.0, 0.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let roundabout = crate::city::RoadSegment::roundabout(
            IVec3::new(0, 90, 0),
            16,
            7,
            crate::city::RoadStyle::Cobble,
        );
        sync_user_city_roads(&mut save, &[roundabout]);
        let world = VoxelWorld::new();
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        let lots = roadside_lot_origins(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
            2,
        );
        let centers: Vec<Vec2> = lots
            .iter()
            .map(|origin| {
                Vec2::new(
                    origin[0] as f32 + size[0] as f32 * 0.5,
                    origin[2] as f32 + size[2] as f32 * 0.5,
                )
            })
            .collect();
        let protected_radius = 16.0 + 7.0 * 0.5 + size[0].max(size[2]) as f32 * 0.5 + 4.0;

        assert!(
            centers.len() >= 6,
            "roundabout should create a useful outer frontage ring, got {centers:?}"
        );
        assert!(
            centers
                .iter()
                .all(|center| center.distance(Vec2::ZERO) >= protected_radius),
            "roundabout lots must stay outside the protected civic/traffic circle radius {protected_radius}, got {centers:?}"
        );
    }

    #[test]
    fn build_site_selection_avoids_existing_project_footprints() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Reserved Skyline".into(),
            center: [0.0, 90.0, 0.0],
            radius: 160,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildGlassTower,
            label: "Existing Tower".into(),
            origin: [0, 90, 0],
            size,
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let picked = best_build_site_from_candidates(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
            vec![[0, 90, 0], [36, 90, 0]],
        );

        assert_eq!(
            picked,
            Some([36, 90, 0]),
            "bot planner should reserve occupied project footprints instead of stacking new towers"
        );
    }

    #[test]
    fn build_site_selection_rejects_buildings_on_road_corridors() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Road Protected Skyline".into(),
            center: [0.0, 90.0, 0.0],
            radius: 160,
            road_anchors: vec![[-48, 90, 0], [48, 90, 0]],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let size = autonomous_project_size(BotTaskKind::BuildGlassTower);

        let picked = best_build_site_from_candidates(
            &save,
            &world,
            &district,
            BotTaskKind::BuildGlassTower,
            size,
            vec![[-10, 90, -10]],
        );

        assert_eq!(
            picked,
            None,
            "bot planner should preserve road corridors instead of accepting a tower footprint on top of the road"
        );
    }

    #[test]
    fn road_site_selection_rejects_routes_through_existing_city_footprints() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Road Around Skyline".into(),
            center: [0.0, 90.0, 0.0],
            radius: 180,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildGlassTower,
            label: "Existing Tower".into(),
            origin: [24, 90, -10],
            size: autonomous_project_size(BotTaskKind::BuildGlassTower),
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });
        let road_size = autonomous_project_size(BotTaskKind::BuildRoad);

        let picked = best_build_site_from_candidates(
            &save,
            &world,
            &district,
            BotTaskKind::BuildRoad,
            road_size,
            vec![[0, 90, -5]],
        );

        assert_eq!(
            picked, None,
            "road crews should reject routes that cut through completed building footprints"
        );
    }

    #[test]
    fn road_site_selection_rejects_duplicate_existing_road_corridors() {
        let mut world = VoxelWorld::new();
        mark_test_city_columns_loaded(&mut world, -8, 8);
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "No Duplicate Streets".into(),
            center: [0.0, 90.0, 0.0],
            radius: 160,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.districts.push(district.clone());
        let road_size = autonomous_project_size(BotTaskKind::BuildRoad);
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Existing Road".into(),
            origin: [0, 90, 0],
            size: road_size,
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let picked = best_build_site_from_candidates(
            &save,
            &world,
            &district,
            BotTaskKind::BuildRoad,
            road_size,
            vec![[0, 90, 0]],
        );

        assert_eq!(
            picked, None,
            "road crews should extend the city graph instead of restamping a duplicate road corridor"
        );
    }

    #[test]
    fn road_grid_segments_include_both_street_axes() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Cross Axis Grid".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Access Grid".into(),
            origin: [-48, 90, -48],
            size: [96, 7, 96],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let grid_segments = road_network_segments(&save, &district);
        let horizontalish = grid_segments
            .iter()
            .filter(|(a, b)| (a.x - b.x).abs() > (a.y - b.y).abs())
            .count();
        let verticalish = grid_segments
            .iter()
            .filter(|(a, b)| (a.y - b.y).abs() > (a.x - b.x).abs())
            .count();
        let min_x = grid_segments
            .iter()
            .flat_map(|(a, b)| [a.x, b.x])
            .fold(f32::INFINITY, f32::min);
        let max_x = grid_segments
            .iter()
            .flat_map(|(a, b)| [a.x, b.x])
            .fold(f32::NEG_INFINITY, f32::max);
        let min_z = grid_segments
            .iter()
            .flat_map(|(a, b)| [a.y, b.y])
            .fold(f32::INFINITY, f32::min);
        let max_z = grid_segments
            .iter()
            .flat_map(|(a, b)| [a.y, b.y])
            .fold(f32::NEG_INFINITY, f32::max);

        assert!(
            grid_segments.len() >= 50,
            "road grid graph should expose every street line, got {} segments",
            grid_segments.len()
        );
        assert!(
            horizontalish >= 20,
            "missing east-west streets: {grid_segments:?}"
        );
        assert!(
            verticalish >= 20,
            "missing north-south streets: {grid_segments:?}"
        );
        assert!(
            min_x <= -44.0 && max_x >= 44.0,
            "grid x coverage was {min_x}..{max_x}"
        );
        assert!(
            min_z <= -44.0 && max_z >= 44.0,
            "grid z coverage was {min_z}..{max_z}"
        );

        save.projects[0].kind = BotTaskKind::BuildRoad;
        save.projects[0].size = [96, 7, 12];
        let single_axis_segments = road_network_segments(&save, &district);

        assert!(single_axis_segments.len() > 1);
        assert!(single_axis_segments
            .iter()
            .all(|(a, b)| (a.x - b.x).abs() > (a.y - b.y).abs()));
    }

    #[test]
    fn blocked_road_projects_do_not_contribute_to_frontage_graph() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Blocked Road Test".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Blocked Access".into(),
            origin: [900, 90, 900],
            size: [96, 7, 12],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Blocked,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let points = road_network_points(&save, &district);
        let segments = road_network_segments(&save, &district);

        assert!(points.iter().all(|point| point.length() < 10.0));
        assert!(segments
            .iter()
            .flat_map(|(a, b)| [a, b])
            .all(|point| point.length() < 10.0));
    }

    #[test]
    fn project_concept_keeps_road_grade_on_road_work_only() {
        let save = BotWorldSave::default();
        let road = build_project_concept(
            &save,
            BotTaskKind::ExpandRoadGrid,
            BotTheme::AmberStreet,
            [0, 90, 0],
            [96, 7, 96],
            "Grid",
            false,
            None,
            None,
        );
        let tower = build_project_concept(
            &save,
            BotTaskKind::BuildGlassTower,
            BotTheme::CyanAlloy,
            [0, 90, 0],
            [18, 44, 18],
            "Tower",
            false,
            None,
            None,
        );

        assert!(road.rows.iter().any(|row| row.phase == "Road Grade"));
        assert!(!tower.rows.iter().any(|row| row.phase == "Road Grade"));
    }

    #[test]
    fn project_concept_exposes_city_sheet_for_major_buildings() {
        let save = BotWorldSave::default();
        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildResidentialBlock,
            BotTheme::WhiteAlloy,
            [24, 90, 42],
            [44, 16, 38],
            "Residential Block",
            false,
            None,
            None,
        );

        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("major buildings should expose their planning sheet");
        assert_eq!(city_sheet.material, "frontage / height / style matrix");
        assert!(city_sheet.detail.contains("road segment"));
        assert!(city_sheet.detail.contains("height band"));
    }

    #[test]
    fn building_street_face_selects_nearest_road_edge() {
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Street Face".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[44, 90, 2], [44, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        };
        let save = BotWorldSave::default();

        assert_eq!(
            road_facing_building_edge(&save, &district, [16, 90, -8], [22, 58, 22]),
            Some(BuildingStreetFace::East)
        );
    }

    #[test]
    fn project_concept_records_street_face_in_city_sheet() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Street Face District".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[44, 90, 2], [44, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildGlassTower,
            BotTheme::CyanAlloy,
            [16, 90, -8],
            [22, 58, 22],
            "Road-Facing Tower",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("tower should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert!(city_sheet.detail.contains("east/max-x street face"));
        assert!(city_sheet.detail.contains("doors"));
        assert!(city_sheet.detail.contains("road graph"));
    }

    #[test]
    fn project_concept_records_bot_road_frontage_intent() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Autonomous Frontage District".into(),
            center: [0.0, 90.0, 0.0],
            radius: 140,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        });
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::BuildRoad,
            label: "Bot-Built Road".into(),
            origin: [-48, 90, 0],
            size: [96, 7, 12],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildResidentialBlock,
            BotTheme::WhiteAlloy,
            [-18, 90, 18],
            [44, 16, 38],
            "Road-Facing Homes",
            false,
            None,
            None,
        );
        let frontage = concept
            .rows
            .iter()
            .find(|row| row.phase == "Bot Road Frontage")
            .expect("buildings beside autonomous roads should expose a bot road frontage plan row");

        assert!(frontage.detail.contains("autonomous Build Road"));
        assert!(frontage.detail.contains("Autonomous Frontage District"));
        assert!(frontage.detail.contains("road graph"));
        assert!(frontage.detail.contains("deck grade"));
        assert!(frontage.detail.contains("north/min-z street face"));
        assert!(frontage.detail.contains("target deck y=90"));
    }

    #[test]
    fn city_block_fit_prefers_tower_corners_and_residential_midblocks() {
        let roads = [
            (Vec2::new(-40.0, 0.0), Vec2::new(40.0, 0.0)),
            (Vec2::new(0.0, -40.0), Vec2::new(0.0, 40.0)),
        ];
        let corner_lot = [8, 90, 8];
        let midblock_lot = [36, 90, 8];
        let tower_size = [21, 58, 21];
        let home_size = [44, 16, 38];

        let corner_tower =
            city_block_fit_score(&roads, corner_lot, tower_size, BotTaskKind::BuildGlassTower);
        let midblock_tower = city_block_fit_score(
            &roads,
            midblock_lot,
            tower_size,
            BotTaskKind::BuildGlassTower,
        );
        let corner_homes = city_block_fit_score(
            &roads,
            corner_lot,
            home_size,
            BotTaskKind::BuildResidentialBlock,
        );
        let midblock_homes = city_block_fit_score(
            &roads,
            midblock_lot,
            home_size,
            BotTaskKind::BuildResidentialBlock,
        );

        assert!(
            corner_tower > midblock_tower + 0.25,
            "corner tower {corner_tower} should beat midblock {midblock_tower}"
        );
        assert!(
            midblock_homes > corner_homes + 0.15,
            "midblock homes {midblock_homes} should beat corner {corner_homes}"
        );
    }

    #[test]
    fn project_concept_records_city_block_role() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Corner District".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[-40, 90, 0], [40, 90, 0], [0, 90, -40], [0, 90, 40]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildGlassTower,
            BotTheme::CyanAlloy,
            [8, 90, 8],
            [21, 58, 21],
            "Corner Tower",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("tower should expose its city planning sheet");

        assert_eq!(concept.block_role, Some(CityBlockRole::CornerLandmark));
        assert!(city_sheet.detail.contains("corner landmark"));
        assert!(city_sheet.detail.contains("intersection"));
        assert!(city_sheet.detail.contains("crown"));
        assert!(city_sheet.detail.contains("vertical spine"));
    }

    #[test]
    fn project_concept_records_residential_corner_bay() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Corner Homes".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[-40, 90, 0], [40, 90, 0], [0, 90, -40], [0, 90, 40]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildResidentialBlock,
            BotTheme::WhiteAlloy,
            [8, 90, 8],
            [44, 16, 38],
            "Corner Homes",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("residential block should expose its city planning sheet");

        assert_eq!(concept.block_role, Some(CityBlockRole::ResidentialCorner));
        assert!(city_sheet.detail.contains("corner bay"));
        assert!(city_sheet.detail.contains("side-street"));
    }

    #[test]
    fn project_concept_records_civic_edge_gateway() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Park,
            name: "Civic Plaza".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[44, 90, 2], [44, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildPlaza,
            BotTheme::WhiteAlloy,
            [0, 90, -16],
            [42, 8, 42],
            "Road-Facing Plaza",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("plaza should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert_eq!(concept.block_role, Some(CityBlockRole::CivicEdge));
        assert!(city_sheet.detail.contains("public gateway"));
        assert!(city_sheet.detail.contains("road-facing edge"));
    }

    #[test]
    fn civic_plaza_opens_gateway_toward_planned_street() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(41, 21) + 1;
        let project = BotProject {
            id: 5,
            kind: BotTaskKind::BuildPlaza,
            label: "Road-Facing Plaza".into(),
            origin: [0, base_y, 0],
            size: [42, 8, 42],
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CivicEdge),
                ..default()
            },
        };

        let gateway_floor =
            project_voxel(&project, IVec3::new(41, 0, 21), &world).map(|(_, voxel)| voxel);
        let side_edge =
            project_voxel(&project, IVec3::new(41, 0, 8), &world).map(|(_, voxel)| voxel);
        let gateway_marker =
            project_voxel(&project, IVec3::new(41, 4, 18), &world).map(|(_, voxel)| voxel);

        assert_eq!(gateway_floor, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(side_edge, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(gateway_marker, Some(project.theme.signal()));
    }

    #[test]
    fn roundabout_anchor_plaza_uses_circular_civic_ring() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(21, 21) + 1;
        let project = BotProject {
            id: 9,
            kind: BotTaskKind::BuildPlaza,
            label: "Roundabout Plaza".into(),
            origin: [0, base_y, 0],
            size: [42, 8, 42],
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                semantic_anchor_shape: Some(BotRoadGuideShape::Roundabout),
                ..default()
            },
        };

        let square_corner =
            project_voxel(&project, IVec3::new(0, 0, 0), &world).map(|(_, voxel)| voxel);
        let ring_paving =
            project_voxel(&project, IVec3::new(33, 0, 21), &world).map(|(_, voxel)| voxel);
        let fountain =
            project_voxel(&project, IVec3::new(21, 1, 21), &world).map(|(_, voxel)| voxel);

        assert_eq!(
            square_corner,
            Some(Voxel::from(BlockType::Grass)),
            "roundabout plaza corners should soften into terrain instead of a square pad"
        );
        assert_eq!(
            ring_paving,
            Some(project.theme.accent()),
            "roundabout plaza should expose a visible circular civic ring"
        );
        assert_eq!(fountain, Some(Voxel::from(BlockType::Water)));
    }

    #[test]
    fn raised_civic_plaza_gateway_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(41, 21) + 1;
        let road_grade_base = terrain_base + 24;
        let project = BotProject {
            id: 10,
            kind: BotTaskKind::BuildPlaza,
            label: "Raised Road-Facing Plaza".into(),
            origin: [0, road_grade_base, 0],
            size: [42, 8, 42],
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CivicEdge),
                ..default()
            },
        };

        let gateway_floor = project_voxel(&project, IVec3::new(41, 0, 21), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(10, 1, 21), &world);

        assert_eq!(
            gateway_floor,
            Some((
                IVec3::new(41, road_grade_base, 21),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(10, road_grade_base - 1, 21))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn project_concept_records_park_public_gateway() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Park,
            name: "Street Park".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[34, 90, 2], [34, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildPark,
            BotTheme::GreenPark,
            [0, 90, -12],
            [30, 8, 30],
            "Road-Facing Park",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("park should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert_eq!(concept.block_role, Some(CityBlockRole::CivicEdge));
        assert!(city_sheet.detail.contains("public gateway"));
    }

    #[test]
    fn road_facing_park_gets_lit_tree_free_gateway() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(29, 15) + 1;
        let project = BotProject {
            id: 6,
            kind: BotTaskKind::BuildPark,
            label: "Road-Facing Park".into(),
            origin: [0, base_y, 0],
            size: [30, 8, 30],
            theme: BotTheme::GreenPark,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CivicEdge),
                ..default()
            },
        };

        let gateway_floor =
            project_voxel(&project, IVec3::new(29, 0, 15), &world).map(|(_, voxel)| voxel);
        let gateway_marker =
            project_voxel(&project, IVec3::new(29, 4, 12), &world).map(|(_, voxel)| voxel);
        let side_edge_tree =
            project_voxel(&project, IVec3::new(29, 4, 4), &world).map(|(_, voxel)| voxel);

        assert_eq!(gateway_floor, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(gateway_marker, Some(project.theme.signal()));
        assert_ne!(side_edge_tree, Some(project.theme.signal()));
    }

    #[test]
    fn raised_park_gateway_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(29, 15) + 1;
        let road_grade_base = terrain_base + 20;
        let project = BotProject {
            id: 11,
            kind: BotTaskKind::BuildPark,
            label: "Raised Road-Facing Park".into(),
            origin: [0, road_grade_base, 0],
            size: [30, 8, 30],
            theme: BotTheme::GreenPark,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CivicEdge),
                ..default()
            },
        };

        let gateway_floor = project_voxel(&project, IVec3::new(29, 0, 15), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(15, 1, 15), &world);

        assert_eq!(
            gateway_floor,
            Some((
                IVec3::new(29, road_grade_base, 15),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(15, road_grade_base - 1, 15))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn project_concept_records_service_pad_road_gate() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Service,
            name: "Service Yard".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[36, 90, 2], [36, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::BuildServicePad,
            BotTheme::CyanAlloy,
            [0, 90, -12],
            [31, 8, 31],
            "Road-Facing Service Pad",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("service pad should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert_eq!(concept.block_role, Some(CityBlockRole::ServiceEdge));
        assert!(city_sheet.detail.contains("service gate"));
        assert!(city_sheet.detail.contains("road-facing utility access"));
    }

    #[test]
    fn road_facing_service_pad_opens_utility_gate() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(30, 15) + 1;
        let project = BotProject {
            id: 7,
            kind: BotTaskKind::BuildServicePad,
            label: "Road-Facing Service Pad".into(),
            origin: [0, base_y, 0],
            size: [31, 8, 31],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let gate_floor =
            project_voxel(&project, IVec3::new(30, 0, 15), &world).map(|(_, voxel)| voxel);
        let side_edge =
            project_voxel(&project, IVec3::new(30, 0, 4), &world).map(|(_, voxel)| voxel);
        let gate_marker =
            project_voxel(&project, IVec3::new(30, 4, 12), &world).map(|(_, voxel)| voxel);

        assert_eq!(gate_floor, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(side_edge, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(gate_marker, Some(project.theme.signal()));
    }

    #[test]
    fn raised_service_pad_gate_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(30, 15) + 1;
        let road_grade_base = terrain_base + 22;
        let project = BotProject {
            id: 12,
            kind: BotTaskKind::BuildServicePad,
            label: "Raised Road-Facing Service Pad".into(),
            origin: [0, road_grade_base, 0],
            size: [31, 8, 31],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let gate_floor = project_voxel(&project, IVec3::new(30, 0, 15), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(15, 1, 15), &world);

        assert_eq!(
            gate_floor,
            Some((
                IVec3::new(30, road_grade_base, 15),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(15, road_grade_base - 1, 15))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn project_concept_records_landing_pad_shuttle_approach() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Service,
            name: "Shuttle Yard".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[30, 90, 2], [30, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::LandingPad,
            BotTheme::CyanAlloy,
            [0, 90, -10],
            [25, 1, 25],
            "Road-Facing Landing Pad",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("landing pad should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert_eq!(concept.block_role, Some(CityBlockRole::ServiceEdge));
        assert!(city_sheet.detail.contains("shuttle approach"));
        assert!(city_sheet.detail.contains("road-facing"));
    }

    #[test]
    fn road_facing_landing_pad_paints_shuttle_approach_stripes() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(24, 12) + 1;
        let project = BotProject {
            id: 8,
            kind: BotTaskKind::LandingPad,
            label: "Road-Facing Landing Pad".into(),
            origin: [0, base_y, 0],
            size: [25, 1, 25],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let edge_gate =
            project_voxel(&project, IVec3::new(24, 0, 12), &world).map(|(_, voxel)| voxel);
        let inner_approach =
            project_voxel(&project, IVec3::new(21, 0, 10), &world).map(|(_, voxel)| voxel);
        let side_deck =
            project_voxel(&project, IVec3::new(21, 0, 4), &world).map(|(_, voxel)| voxel);

        assert_eq!(edge_gate, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(inner_approach, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(side_deck, Some(Voxel::from(BlockType::Limestone)));
    }

    #[test]
    fn raised_landing_pad_approach_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(24, 12) + 1;
        let road_grade_base = terrain_base + 18;
        let project = BotProject {
            id: 13,
            kind: BotTaskKind::LandingPad,
            label: "Raised Road-Facing Landing Pad".into(),
            origin: [0, road_grade_base, 0],
            size: [25, 1, 25],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let edge_gate = project_voxel(&project, IVec3::new(24, 0, 12), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(24, 1, 12), &world);

        assert_eq!(
            edge_gate,
            Some((
                IVec3::new(24, road_grade_base, 12),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(24, road_grade_base - 1, 12))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn raised_street_lights_use_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(16, 0) + 1;
        let road_grade_base = terrain_base + 16;
        let project = BotProject {
            id: 14,
            kind: BotTaskKind::AddLights,
            label: "Raised Street Lights".into(),
            origin: [0, road_grade_base, 0],
            size: [64, 7, 9],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        };

        let lamp_base = project_voxel(&project, IVec3::new(16, 0, 0), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(16, 1, 4), &world);

        assert_eq!(
            lamp_base,
            Some((
                IVec3::new(16, road_grade_base, 0),
                Voxel::from(BlockType::ShipHullDark)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(16, road_grade_base - 1, 4))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn raised_clear_flatten_pad_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(0, 0) + 1;
        let road_grade_base = terrain_base + 22;
        let project = BotProject {
            id: 16,
            kind: BotTaskKind::ClearFlatten,
            label: "Raised Prep Pad".into(),
            origin: [0, road_grade_base, 0],
            size: [28, 8, 28],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        };

        let pad_floor = project_voxel(&project, IVec3::new(0, 0, 0), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(0, 1, 0), &world);

        assert_eq!(
            pad_floor,
            Some((IVec3::new(0, road_grade_base, 0), project.theme.floor()))
        );
        assert_eq!(
            underdeck_support,
            Some((
                IVec3::new(0, road_grade_base - 1, 0),
                Voxel::from(BlockType::Basalt)
            ))
        );
    }

    #[test]
    fn project_concept_records_target_range_road_gate() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Training,
            name: "Training Yard".into(),
            center: [0.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![[46, 90, 2], [46, 90, 4]],
            build_slots: vec![],
            completed_projects: 0,
        });

        let concept = build_project_concept(
            &save,
            BotTaskKind::TargetRange,
            BotTheme::AmberStreet,
            [0, 90, -10],
            [40, 9, 24],
            "Road-Facing Target Range",
            false,
            None,
            None,
        );
        let city_sheet = concept
            .rows
            .iter()
            .find(|row| row.phase == "City Sheet")
            .expect("target range should expose its city planning sheet");

        assert_eq!(concept.street_face, Some(BuildingStreetFace::East));
        assert_eq!(concept.block_role, Some(CityBlockRole::ServiceEdge));
        assert!(city_sheet.detail.contains("range gate"));
        assert!(city_sheet.detail.contains("safe entry lane"));
    }

    #[test]
    fn road_facing_target_range_paints_safe_entry_lane() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(39, 12) + 1;
        let project = BotProject {
            id: 9,
            kind: BotTaskKind::TargetRange,
            label: "Road-Facing Target Range".into(),
            origin: [0, base_y, 0],
            size: [40, 9, 24],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let edge_gate =
            project_voxel(&project, IVec3::new(39, 0, 12), &world).map(|(_, voxel)| voxel);
        let inner_safe_lane =
            project_voxel(&project, IVec3::new(35, 0, 10), &world).map(|(_, voxel)| voxel);
        let side_lane =
            project_voxel(&project, IVec3::new(35, 0, 4), &world).map(|(_, voxel)| voxel);
        let gate_marker =
            project_voxel(&project, IVec3::new(39, 4, 9), &world).map(|(_, voxel)| voxel);

        assert_eq!(edge_gate, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(inner_safe_lane, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(side_lane, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(gate_marker, Some(project.theme.signal()));
    }

    #[test]
    fn raised_target_range_safe_lane_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(39, 12) + 1;
        let road_grade_base = terrain_base + 18;
        let project = BotProject {
            id: 15,
            kind: BotTaskKind::TargetRange,
            label: "Raised Road-Facing Target Range".into(),
            origin: [0, road_grade_base, 0],
            size: [40, 9, 24],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::ServiceEdge),
                ..default()
            },
        };

        let edge_gate = project_voxel(&project, IVec3::new(39, 0, 12), &world);
        let underdeck_support = project_voxel(&project, IVec3::new(35, 1, 10), &world);

        assert_eq!(
            edge_gate,
            Some((
                IVec3::new(39, road_grade_base, 12),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            underdeck_support.map(|(pos, _)| pos),
            Some(IVec3::new(35, road_grade_base - 1, 10))
        );
        assert_ne!(underdeck_support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn tower_podium_entrance_faces_planned_street() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(20, 10) + 1;
        let project = BotProject {
            id: 1,
            kind: BotTaskKind::BuildGlassTower,
            label: "Street Facing Tower".into(),
            origin: [0, base_y, 0],
            size: [21, 58, 21],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                ..default()
            },
        };

        let east_entrance =
            project_voxel(&project, IVec3::new(20, 1, 10), &world).map(|(_, voxel)| voxel);
        let old_north_entrance =
            project_voxel(&project, IVec3::new(10, 1, 0), &world).map(|(_, voxel)| voxel);

        assert_eq!(east_entrance, Some(Voxel::from(BlockType::CockpitGlass)));
        assert_ne!(
            old_north_entrance,
            Some(Voxel::from(BlockType::CockpitGlass))
        );
    }

    #[test]
    fn raised_tower_foundation_supports_descend_from_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(10, 10) + 1;
        let road_grade_base = terrain_base + 32;
        let project = BotProject {
            id: 2,
            kind: BotTaskKind::BuildGlassTower,
            label: "Raised Bridge Tower".into(),
            origin: [0, road_grade_base, 0],
            size: [21, 58, 21],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                ..default()
            },
        };

        let upper_support = project_voxel(&project, IVec3::new(10, 1, 10), &world);
        let lower_support = project_voxel(&project, IVec3::new(10, 30, 10), &world);

        assert_eq!(
            upper_support,
            Some((
                IVec3::new(10, road_grade_base - 1, 10),
                Voxel::from(BlockType::Basalt)
            )),
            "raised foundations should start directly below the road-grade deck"
        );
        assert_eq!(
            lower_support,
            Some((
                IVec3::new(10, road_grade_base - 30, 10),
                Voxel::from(BlockType::Basalt)
            )),
            "raised foundations should continue downward instead of leaving a floating tower"
        );
    }

    #[test]
    fn raised_tower_deck_marks_road_facing_entry_lane() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(18, 10) + 1;
        let road_grade_base = terrain_base + 28;
        let project = BotProject {
            id: 4,
            kind: BotTaskKind::BuildGlassTower,
            label: "Raised Access Tower".into(),
            origin: [0, road_grade_base, 0],
            size: [21, 58, 21],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                ..default()
            },
        };

        let entry_lane =
            project_voxel(&project, IVec3::new(18, 0, 10), &world).map(|(_, voxel)| voxel);
        let quiet_deck =
            project_voxel(&project, IVec3::new(4, 0, 10), &world).map(|(_, voxel)| voxel);

        assert_eq!(
            entry_lane,
            Some(project.theme.accent()),
            "raised towers should visibly mark the road-facing deck lane into the entrance"
        );
        assert_eq!(quiet_deck, Some(Voxel::from(BlockType::Limestone)));
    }

    #[test]
    fn corner_landmark_tower_marks_street_facing_roof_corners() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(20, 10) + 1;
        let project = BotProject {
            id: 3,
            kind: BotTaskKind::BuildGlassTower,
            label: "Corner Landmark Tower".into(),
            origin: [0, base_y, 0],
            size: [21, 58, 21],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CornerLandmark),
                ..default()
            },
        };
        let variant = project_style_variant(&project, 5);
        let setback = match variant {
            1 | 3 => 3,
            4 => 1,
            _ => 2,
        };
        let sx = project.size[0] - 1;
        let sy = project.size[1] - 1;

        let street_corner =
            project_voxel(&project, IVec3::new(sx - setback, sy, setback + 1), &world)
                .map(|(_, voxel)| voxel);
        let quiet_back_corner =
            project_voxel(&project, IVec3::new(setback, sy, setback + 1), &world)
                .map(|(_, voxel)| voxel);
        let mut midblock = project.clone();
        midblock.concept.block_role = Some(CityBlockRole::MidblockStreetWall);
        let midblock_corner =
            project_voxel(&midblock, IVec3::new(sx - setback, sy, setback + 1), &world)
                .map(|(_, voxel)| voxel);

        assert_eq!(street_corner, Some(project.theme.signal()));
        assert_ne!(quiet_back_corner, Some(project.theme.signal()));
        assert_ne!(midblock_corner, Some(project.theme.signal()));
    }

    #[test]
    fn corner_landmark_tower_adds_lit_vertical_corner_spine() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(20, 10) + 1;
        let project = BotProject {
            id: 3,
            kind: BotTaskKind::BuildGlassTower,
            label: "Corner Landmark Tower".into(),
            origin: [0, base_y, 0],
            size: [21, 58, 21],
            theme: BotTheme::CyanAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(BuildingStreetFace::East),
                block_role: Some(CityBlockRole::CornerLandmark),
                ..default()
            },
        };
        let sx = project.size[0] - 1;
        let sy = project.size[1] - 1;
        let variant = project_style_variant(&project, 5);
        let setback = match variant {
            1 | 3 => 3,
            4 => 1,
            _ => 2,
        };
        let upper_y = sy - 7;

        let spine = project_voxel(
            &project,
            IVec3::new(sx - setback, upper_y, setback + 1),
            &world,
        )
        .map(|(_, voxel)| voxel);
        let mut midblock = project.clone();
        midblock.concept.block_role = Some(CityBlockRole::MidblockStreetWall);
        let midblock_same_cell = project_voxel(
            &midblock,
            IVec3::new(sx - setback, upper_y, setback + 1),
            &world,
        )
        .map(|(_, voxel)| voxel);

        assert_eq!(spine, Some(project.theme.signal()));
        assert_ne!(midblock_same_cell, Some(project.theme.signal()));
    }

    fn residential_test_project(street_face: BuildingStreetFace, base_y: i32) -> BotProject {
        BotProject {
            id: 1,
            kind: BotTaskKind::BuildResidentialBlock,
            label: "Street Facing Homes".into(),
            origin: [0, base_y, 0],
            size: [44, 16, 38],
            theme: BotTheme::WhiteAlloy,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept {
                street_face: Some(street_face),
                ..default()
            },
        }
    }

    #[test]
    fn residential_block_entries_follow_planned_street_face() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(8, 4) + 1;
        let project = residential_test_project(BuildingStreetFace::East, base_y);

        let east_entry =
            project_voxel(&project, IVec3::new(8, 2, 4), &world).map(|(_, voxel)| voxel);
        let old_north_entry =
            project_voxel(&project, IVec3::new(3, 2, 0), &world).map(|(_, voxel)| voxel);

        assert_eq!(east_entry, Some(Voxel::from(BlockType::Wood)));
        assert_ne!(old_north_entry, Some(Voxel::from(BlockType::Wood)));
    }

    #[test]
    fn residential_corner_block_adds_glass_corner_bay() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(8, 1) + 1;
        let mut corner = residential_test_project(BuildingStreetFace::East, base_y);
        corner.concept.block_role = Some(CityBlockRole::ResidentialCorner);
        let mut midblock = corner.clone();
        midblock.concept.block_role = Some(CityBlockRole::MidblockStreetWall);

        let corner_bay =
            project_voxel(&corner, IVec3::new(8, 2, 1), &world).map(|(_, voxel)| voxel);
        let midblock_same_cell =
            project_voxel(&midblock, IVec3::new(8, 2, 1), &world).map(|(_, voxel)| voxel);

        assert_eq!(corner_bay, Some(Voxel::from(BlockType::CockpitGlass)));
        assert_ne!(
            midblock_same_cell,
            Some(Voxel::from(BlockType::CockpitGlass))
        );
    }

    #[test]
    fn residential_corner_block_adds_lit_corner_awning() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(8, 1) + 1;
        let mut corner = residential_test_project(BuildingStreetFace::East, base_y);
        corner.concept.block_role = Some(CityBlockRole::ResidentialCorner);
        let mut midblock = corner.clone();
        midblock.concept.block_role = Some(CityBlockRole::MidblockStreetWall);

        let awning = project_voxel(&corner, IVec3::new(8, 3, 1), &world).map(|(_, voxel)| voxel);
        let midblock_same_cell =
            project_voxel(&midblock, IVec3::new(8, 3, 1), &world).map(|(_, voxel)| voxel);

        assert_eq!(awning, Some(corner.theme.signal()));
        assert_ne!(midblock_same_cell, Some(corner.theme.signal()));
    }

    #[test]
    fn residential_block_frontage_walk_faces_planned_street() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(8, 4) + 1;
        let project = residential_test_project(BuildingStreetFace::East, base_y);

        let east_walk =
            project_voxel(&project, IVec3::new(9, 0, 4), &world).map(|(_, voxel)| voxel);

        assert_eq!(east_walk, Some(Voxel::from(BlockType::Limestone)));
    }

    #[test]
    fn residential_block_frontage_walk_mirrors_north_and_west_faces() {
        let world = VoxelWorld::new();
        let base_y = world.surface_height_at(3, 0) + 1;
        let north = residential_test_project(BuildingStreetFace::North, base_y);
        let west = residential_test_project(BuildingStreetFace::West, base_y);

        let north_walk = project_voxel(&north, IVec3::new(3, 0, 0), &world).map(|(_, voxel)| voxel);
        let north_back_strip =
            project_voxel(&north, IVec3::new(3, 0, 8), &world).map(|(_, voxel)| voxel);
        let west_walk = project_voxel(&west, IVec3::new(0, 0, 4), &world).map(|(_, voxel)| voxel);
        let west_back_strip =
            project_voxel(&west, IVec3::new(9, 0, 4), &world).map(|(_, voxel)| voxel);

        assert_eq!(north_walk, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(north_back_strip, Some(Voxel::from(BlockType::Limestone)));
        assert_eq!(west_walk, Some(Voxel::from(BlockType::Limestone)));
        assert_ne!(west_back_strip, Some(Voxel::from(BlockType::Limestone)));
    }

    #[test]
    fn raised_residential_frontage_walk_uses_road_grade_deck() {
        let world = VoxelWorld::new();
        let terrain_base = world.surface_height_at(0, 4) + 1;
        let road_grade_base = terrain_base + 24;
        let project = residential_test_project(BuildingStreetFace::West, road_grade_base);

        let deck = project_voxel(&project, IVec3::new(0, 0, 4), &world);
        let support = project_voxel(&project, IVec3::new(0, 1, 4), &world);

        assert_eq!(
            deck,
            Some((
                IVec3::new(0, road_grade_base, 4),
                Voxel::from(BlockType::Limestone)
            ))
        );
        assert_eq!(
            support.map(|(pos, _)| pos),
            Some(IVec3::new(0, road_grade_base - 1, 4))
        );
        assert_ne!(support.map(|(_, voxel)| voxel), Some(AIR));
    }

    #[test]
    fn road_grid_intersection_corners_get_grounded_surface_voxels() {
        let project = BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Access Grid".into(),
            origin: [0, 90, 0],
            size: [96, 7, 96],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        };
        let world = VoxelWorld::new();

        let corner_voxel = project_voxel(&project, IVec3::new(53, 0, 53), &world);
        let corner_support = project_voxel(&project, IVec3::new(53, 1, 53), &world);

        assert!(
            corner_voxel.is_some(),
            "intersection corner should receive a visible sidewalk voxel"
        );
        assert!(
            corner_support.is_some(),
            "intersection corner should use the same terrain-aware support path"
        );
    }

    #[test]
    fn road_grid_profile_distinguishes_boulevards_from_local_streets() {
        let origin = [0, 90, 0];
        let size = [96, 7, 96];
        let boulevard_median = road_grid_profile(origin, size, IVec3::new(44, 0, 20));
        let local_street = road_grid_profile(origin, size, IVec3::new(26, 0, 20));

        assert!(boulevard_median.road_like);
        assert!(boulevard_median.boulevard);
        assert!(boulevard_median.median);
        assert!(local_street.road_like);
        assert!(!local_street.boulevard);
        assert!(!local_street.median);
    }

    #[test]
    fn road_grid_boulevard_medians_render_as_planted_surface() {
        let project = BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Access Grid".into(),
            origin: [0, 90, 0],
            size: [96, 7, 96],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        };
        let world = VoxelWorld::new();

        let median = project_voxel(&project, IVec3::new(44, 0, 20), &world).map(|(_, voxel)| voxel);
        let local_lane =
            project_voxel(&project, IVec3::new(26, 0, 20), &world).map(|(_, voxel)| voxel);

        assert_eq!(median, Some(Voxel::from(BlockType::Leaves)));
        assert_ne!(local_lane, Some(Voxel::from(BlockType::Leaves)));
    }

    #[test]
    fn boulevard_intersection_gets_planted_roundabout_center() {
        let project = BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Boulevard Roundabout".into(),
            origin: [0, 90, 0],
            size: [96, 7, 96],
            theme: BotTheme::AmberStreet,
            status: BotProjectStatus::Active,
            cursor: 0,
            total_steps: 1,
            assigned_bot: None,
            district_id: Some(7),
            crew_id: None,
            idea_id: None,
            blocked_reason: String::new(),
            priority: 5,
            concept: BotProjectConcept::default(),
        };
        let world = VoxelWorld::new();

        let center = project_voxel(&project, IVec3::new(52, 0, 52), &world).map(|(_, voxel)| voxel);
        let ring_lane =
            project_voxel(&project, IVec3::new(57, 0, 52), &world).map(|(_, voxel)| voxel);
        let approach =
            project_voxel(&project, IVec3::new(52, 0, 62), &world).map(|(_, voxel)| voxel);

        assert_eq!(center, Some(Voxel::from(BlockType::Leaves)));
        assert_ne!(ring_lane, Some(Voxel::from(BlockType::Leaves)));
        assert_ne!(approach, Some(Voxel::from(BlockType::Leaves)));
    }

    #[test]
    fn road_grid_profile_identifies_roundabout_geometry() {
        let origin = [0, 90, 0];
        let size = [96, 7, 96];
        let island = road_grid_profile(origin, size, IVec3::new(52, 0, 52));
        let ring = road_grid_profile(origin, size, IVec3::new(57, 0, 52));
        let approach = road_grid_profile(origin, size, IVec3::new(52, 0, 62));

        assert!(island.roundabout);
        assert!(island.median);
        assert!(ring.roundabout);
        assert!(!ring.median);
        assert!(approach.boulevard);
        assert!(!approach.roundabout);
    }

    #[test]
    fn districts_require_road_access_before_major_builds() {
        let mut save = BotWorldSave::default();
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Test Habitat".into(),
            center: [64.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![[0, 90, 0], [64, 90, 0]],
            build_slots: vec![],
            completed_projects: 0,
        };
        assert!(!district_has_road_access(&save, &district));
        assert_eq!(
            choose_district_project(&save, &district, 0, false),
            BotTaskKind::ExpandRoadGrid
        );
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Access Grid".into(),
            origin: [32, 90, -32],
            size: autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        assert!(district_has_road_access(&save, &district));
    }

    #[test]
    fn street_details_do_not_grant_road_access() {
        let mut save = BotWorldSave::default();
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Test Habitat".into(),
            center: [64.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::DecorateStreet,
            label: "Decor Only".into(),
            origin: [32, 90, -32],
            size: autonomous_project_size(BotTaskKind::DecorateStreet),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        assert!(!district_has_road_access(&save, &district));
        save.projects[0].kind = BotTaskKind::ExpandRoadGrid;
        assert!(district_has_road_access(&save, &district));
    }

    #[test]
    fn districts_mix_buildings_after_first_real_access_road() {
        let mut save = BotWorldSave::default();
        save.settlements.push(BotSettlement {
            id: 1,
            name: "Mixed City".into(),
            hub: [0.0, 90.0, 0.0],
            radius: MEGA_CITY_RADIUS,
            bounds: BotCityBounds {
                center: [0.0, 90.0, 0.0],
                radius: MEGA_CITY_RADIUS,
                used_radius: 0.0,
                max_active_projects: DEFAULT_MAX_ACTIVE_PROJECTS,
            },
            theme: BotTheme::CyanAlloy,
            road_count: 0,
            building_count: 0,
            park_count: 0,
        });
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Habitat".into(),
            center: [64.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        };
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "First Access Grid".into(),
            origin: [32, 90, -32],
            size: autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::Active,
            cursor: 128,
            total_steps: 1_000,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        assert!(district_has_road_access(&save, &district));
        assert!(settlement_has_access_roads(&save));
        let mut pressured_budget = RuntimeBudget::default();
        pressured_budget.target_render_distance = 50;
        pressured_budget.render_distance = 10;
        pressured_budget.queue_pressure = 1.0;
        assert_eq!(active_project_limit_for_budget(&save, &pressured_budget), 2);
        assert_eq!(
            choose_district_project(&save, &district, 0, false),
            BotTaskKind::BuildResidentialBlock
        );
        assert_eq!(
            choose_district_project(&save, &district, 1, false),
            BotTaskKind::ExpandRoadGrid
        );
    }

    #[test]
    fn district_project_choice_diversifies_before_repeating_building_kind() {
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Access Grid".into(),
            origin: [0, 90, -48],
            size: autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        save.projects.push(BotProject {
            id: 2,
            kind: BotTaskKind::BuildGlassTower,
            label: "First Tower".into(),
            origin: [8, 90, 8],
            size: autonomous_project_size(BotTaskKind::BuildGlassTower),
            theme: BotTheme::CyanAlloy,
            assigned_bot: Some(2),
            status: BotProjectStatus::Complete,
            cursor: 0,
            total_steps: 1,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        let district = BotDistrict {
            id: 7,
            kind: BotDistrictKind::Skyline,
            name: "Diverse Skyline".into(),
            center: [64.0, 90.0, 0.0],
            radius: 120,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 1,
        };

        assert_eq!(
            choose_district_project(&save, &district, 0, false),
            BotTaskKind::BuildTower,
            "skyline districts should choose a different architecture kind before repeating another glass tower"
        );
    }

    #[test]
    fn planner_prefers_road_ready_district_for_infill() {
        let mut save = BotWorldSave::default();
        save.districts.push(BotDistrict {
            id: 1,
            kind: BotDistrictKind::HubCore,
            name: "Hub".into(),
            center: [0.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        });
        save.districts.push(BotDistrict {
            id: 7,
            kind: BotDistrictKind::Residential,
            name: "Habitat".into(),
            center: [64.0, 90.0, 0.0],
            radius: 80,
            road_anchors: vec![],
            build_slots: vec![],
            completed_projects: 0,
        });
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "First Access Grid".into(),
            origin: [32, 90, -32],
            size: autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::Active,
            cursor: 128,
            total_steps: 1_000,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        let picked = choose_planning_district(&save, 2).unwrap();
        assert_eq!(picked.id, 7);
    }

    #[test]
    fn city_counters_ignore_prep_and_street_detail_projects() {
        let mut save = BotWorldSave::default();
        save.settlements.push(BotSettlement {
            id: 1,
            name: "Counter City".into(),
            hub: [0.0, 90.0, 0.0],
            radius: MEGA_CITY_RADIUS,
            bounds: BotCityBounds {
                center: [0.0, 90.0, 0.0],
                radius: MEGA_CITY_RADIUS,
                used_radius: 0.0,
                max_active_projects: DEFAULT_MAX_ACTIVE_PROJECTS,
            },
            theme: BotTheme::CyanAlloy,
            road_count: 0,
            building_count: 0,
            park_count: 0,
        });
        for kind in [
            BotTaskKind::ClearFlatten,
            BotTaskKind::DecorateStreet,
            BotTaskKind::ExpandRoadGrid,
            BotTaskKind::BuildGlassTower,
        ] {
            let idx = save.projects.len();
            save.projects.push(BotProject {
                id: idx as u64 + 1,
                kind,
                label: kind.label().into(),
                origin: [idx as i32 * 8, 90, 0],
                size: autonomous_project_size(kind),
                theme: BotTheme::CyanAlloy,
                assigned_bot: None,
                status: BotProjectStatus::Complete,
                cursor: 0,
                total_steps: 1,
                blocked_reason: String::new(),
                priority: 5,
                district_id: None,
                idea_id: None,
                crew_id: None,
                concept: BotProjectConcept::default(),
            });
            complete_project_at(&mut save, idx);
        }
        assert_eq!(save.settlements[0].road_count, 1);
        assert_eq!(save.settlements[0].building_count, 1);
        assert_eq!(save.settlements[0].park_count, 0);
    }

    #[test]
    fn waiting_projects_still_count_against_planner_capacity() {
        let mut save = BotWorldSave::default();
        save.projects.push(BotProject {
            id: 1,
            kind: BotTaskKind::ExpandRoadGrid,
            label: "Waiting Grid".into(),
            origin: [32, 90, -32],
            size: autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            theme: BotTheme::AmberStreet,
            assigned_bot: Some(1),
            status: BotProjectStatus::WaitingForChunks,
            cursor: 0,
            total_steps: 1,
            blocked_reason: String::new(),
            priority: 5,
            district_id: Some(7),
            idea_id: None,
            crew_id: None,
            concept: BotProjectConcept::default(),
        });
        assert_eq!(planner_project_count(&save), 1);
        save.projects[0].status = BotProjectStatus::WaitingForPlayer;
        assert_eq!(planner_project_count(&save), 1);
        save.projects[0].status = BotProjectStatus::Complete;
        assert_eq!(planner_project_count(&save), 0);
    }

    #[test]
    fn project_footprints_respect_player_clearance() {
        assert!(protected_project_area(
            [0, 90, 0],
            [40, 8, 40],
            Some(Vec3::new(10.0, 100.0, 10.0)),
            &[]
        ));
        assert!(protected_project_area(
            [0, 90, 0],
            [40, 8, 40],
            Some(Vec3::new(70.0, 100.0, 70.0)),
            &[]
        ));
        assert!(!protected_project_area(
            [0, 90, 0],
            [40, 8, 40],
            Some(Vec3::new(180.0, 100.0, 180.0)),
            &[]
        ));
    }

    #[test]
    fn bot_edits_pause_while_max_distance_horizon_recovers() {
        let mut budget = RuntimeBudget::default();
        budget.target_render_distance = 50;
        budget.render_distance = 11;
        budget.queue_pressure = 0.0;
        budget.frame_pressure = 0.0;
        assert_eq!(bot_frame_edit_budget(&budget, 2), 0);

        budget.render_distance = 41;
        assert!(bot_frame_edit_budget(&budget, 2) > 0);
    }

    #[test]
    fn impossible_command_sizes_are_clamped() {
        let size = command_size(BotTaskCommand {
            task_type: BotTaskKind::BuildTower,
            width: 1,
            height: 99,
            ..default()
        });
        assert_eq!(size[0], 9);
        assert_eq!(size[1], 48);
        assert_eq!(size[2], 9);
    }

    #[test]
    fn edited_chunk_override_has_expected_volume() {
        assert_eq!(crate::chunk::CHUNK_VOLUME, 4096);
        let cp = crate::chunk::world_to_chunk(0, 0, 0).0;
        assert_eq!(cp.x, 0);
        assert_eq!(cp.y, 0);
        assert_eq!(cp.z, 0);
    }

    fn test_bot(id: u64, name: &str, role: BotRole) -> BotAgent {
        BotAgent {
            id,
            name: name.into(),
            role,
            state: BotState::Idle,
            position: [0.0, 90.0, 0.0],
            target: [0.0, 90.0, 0.0],
            home_id: 1,
            crew_id: None,
            last_interaction_epoch: 0,
            companion: false,
            companion_order: 0,
            swarm_leader_id: None,
            swarm_index: 0,
            companion_mode: BotCompanionMode::AwaitingInstruction,
            current_task: None,
            memory: BotMemory {
                favorite_theme: role.default_theme(),
                ..Default::default()
            },
        }
    }
}
