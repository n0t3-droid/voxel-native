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
const MAX_ACTIVE_PROJECTS_LIMIT: usize = 48;
const MAX_CREW_BOTS_PER_PROJECT: usize = 32;
const COMPANION_WORKERS_PER_LEADER: u8 = 4;
const VISIBLE_MESSAGE_COOLDOWN: f32 = 10.0;
const CONVERSATION_INTERVAL: f32 = 14.0;
const BOT_MEET_DISTANCE: f32 = 58.0;
const BOT_MEET_OFFSET: f32 = 11.0;
const BOT_BUSY_RETARGET_INTERVAL: f32 = 3.5;
const BOT_GREETER_INTERVAL: f32 = 4.0;
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
        self.version = bot_save_version();
        self.next_bot_id = self.next_bot_id.max(1);
        self.next_project_id = self.next_project_id.max(1);
        self.next_district_id = self.next_district_id.max(1);
        self.next_idea_id = self.next_idea_id.max(1);
        self.next_conversation_id = self.next_conversation_id.max(1);
        self.next_crew_id = self.next_crew_id.max(1);

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
        normalize_companion_swarm(self);
        self.next_bot_id = self
            .next_bot_id
            .max(self.agents.iter().map(|b| b.id).max().unwrap_or(0) + 1);
        ensure_companion_worker_swarms(self);
        restore_project_assignments(self);
        normalize_relationships(self);

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
        .map(|bot| (bot.id, bot.name.clone(), bot.companion_order, bot.position, bot.home_id))
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
        let crew_id = valid_crew.or_else(|| create_project_crew(save, project_id, kind, assigned_bot, origin));
        if let Some(project) = save.projects.get_mut(idx) {
            project.crew_id = crew_id;
        }
        assign_crew_task(save, crew_id, assigned_bot, project_id, kind, &label, origin);
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
}

impl Default for BotProjectConcept {
    fn default() -> Self {
        Self {
            brief: String::new(),
            structure: String::new(),
            material_plan: String::new(),
            visual_goal: String::new(),
            rows: Vec::new(),
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

    keep_bots_visible_and_busy(&mut brain, &world, player_pos);
    if !brain.save.autonomy.bots_active {
        if brain.message_cooldown <= 0.0 {
            brain.hud_message = "Bot workers are OFF. Plans and progress are saved.".into();
            brain.message_cooldown = VISIBLE_MESSAGE_COOLDOWN;
        }
        return;
    }
    process_queued_commands(&mut brain, &world, player_pos, &ship_positions);
    if brain.force_city_idea || (brain.save.autonomy.enabled && brain.plan_timer <= 0.0) {
        brain.plan_timer = planner_interval(&brain.save);
        let urgent = brain.force_city_idea;
        brain.force_city_idea = false;
        if run_city_planner(&mut brain, &world, player_pos, &ship_positions, urgent) {
            brain.dirty = true;
        }
    }

    move_bot_memories(&mut brain.save, &world, dt);

    let mut completed = Vec::new();
    let mut blocked = Vec::new();
    let mut changed_total = 0usize;
    let bounds = brain.save.primary_bounds();

    for idx in 0..brain.save.projects.len() {
        if brain.save.projects[idx].status.is_done() {
            continue;
        }
        let result = advance_project_slice(
            &mut brain.save.projects[idx],
            &mut world,
            &mut history,
            player_pos,
            &ship_positions,
            bounds,
        );
        changed_total += result.changed;
        if result.completed {
            completed.push(idx);
        } else if result.blocked {
            blocked.push(idx);
        }
        if changed_total >= 1_400 {
            break;
        }
    }

    for idx in completed {
        let label = brain.save.projects[idx].label.clone();
        complete_project_at(&mut brain.save, idx);
        show_city_message(
            &mut brain,
            format!("{label} complete. Bot city is growing."),
            8,
        );
        brain.force_city_idea = true;
        brain.plan_timer = 0.0;
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
    let candidates = command_site_candidates(
        save,
        world,
        kind,
        size,
        district_anchor,
        bot_anchor,
        player_pos,
        seq,
    );
    let mut last_error = "no loaded safe build site near you yet".to_string();
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
    player_pos: Option<Vec3>,
    seq: usize,
) -> Vec<[i32; 3]> {
    let bounds = save.primary_bounds();
    let half = Vec3::new(size[0] as f32 * 0.5, 0.0, size[2] as f32 * 0.5);
    let mut anchors = Vec::new();
    if let Some(player) = player_pos {
        anchors.push(player);
    }
    anchors.push(bot_anchor);
    anchors.push(district_anchor);
    if let Some(hub) = save.settlements.first().map(|s| vec3_from_arr(s.hub)) {
        anchors.push(hub);
    }

    let base_radius = match kind {
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
        for step in 0..18 {
            let ring = base_radius + (step / 6) as f32 * 28.0;
            let angle = (seq + step + anchor_idx * 7) as f32 * 2.399_963_1;
            let center = anchor + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring);
            let target_origin = clamp_to_bounds(bounds, center - half);
            let origin = project_origin(world, target_origin);
            if !bounds.contains_box(origin, size) || !project_columns_loaded(world, origin, size) {
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
) -> usize {
    let district_anchor = nearest_district(save, player_pos)
        .map(|d| vec3_from_arr(d.center))
        .or_else(|| save.settlements.first().map(|s| vec3_from_arr(s.hub)))
        .unwrap_or(player_pos);
    let specs = [
        (
            BotTaskKind::ClearFlatten,
            [40, 8, 40],
            BotTheme::WhiteAlloy,
            BotRole::Builder,
            10,
        ),
        (
            BotTaskKind::ExpandRoadGrid,
            autonomous_project_size(BotTaskKind::ExpandRoadGrid),
            BotTheme::AmberStreet,
            BotRole::RoadCrew,
            10,
        ),
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
            BotTaskKind::DecorateStreet,
            [88, 7, 11],
            BotTheme::AmberStreet,
            BotRole::RepairTech,
            8,
        ),
    ];
    let mut queued = 0usize;
    let mut last_error = String::new();
    for (kind, size, theme, role, priority) in specs {
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
            player_pos,
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

fn planner_project_count(save: &BotWorldSave) -> usize {
    save.projects
        .iter()
        .filter(|project| matches!(project.status, BotProjectStatus::Queued | BotProjectStatus::Active))
        .count()
}

fn queue_visible_city_work(
    save: &mut BotWorldSave,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
) -> usize {
    let anchor = player_pos
        .or_else(|| save.settlements.first().map(|settlement| vec3_from_arr(settlement.hub)))
        .unwrap_or(Vec3::ZERO);
    queue_mega_city_starter_projects(save, world, anchor, ship_positions)
}

fn run_city_planner(
    brain: &mut FriendlyWorldBrain,
    world: &VoxelWorld,
    player_pos: Option<Vec3>,
    ship_positions: &[Vec3],
    urgent: bool,
) -> bool {
    let active = planner_project_count(&brain.save);
    let limit = active_project_limit(&brain.save);
    if active >= limit && !urgent {
        return false;
    }

    let Some(idea_id) = propose_city_idea(&mut brain.save, world, urgent) else {
        let queued = queue_visible_city_work(&mut brain.save, world, player_pos, ship_positions);
        if queued > 0 {
            show_city_message(
                brain,
                format!("Autonomy refilled {queued} city build(s) near the active area."),
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
                let queued = queue_visible_city_work(&mut brain.save, world, player_pos, ship_positions);
                if queued > 0 {
                    show_city_message(
                        brain,
                        format!("Swarm recovered with {queued} loaded city build(s) near you."),
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
    let preferred = seq % districts.len();
    districts.get(preferred).copied()
}

fn choose_district_project(
    save: &BotWorldSave,
    district: &BotDistrict,
    seq: usize,
    urgent: bool,
) -> BotTaskKind {
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
    if seq % 4 == 1 || save.settlements.first().map(|s| s.road_count).unwrap_or(0) < 4 {
        return BotTaskKind::ExpandRoadGrid;
    }
    match district.kind {
        BotDistrictKind::HubCore => [
            BotTaskKind::LandingPad,
            BotTaskKind::BuildPlaza,
            BotTaskKind::DecorateStreet,
        ][seq % 3],
        BotDistrictKind::Residential => [
            BotTaskKind::BuildResidentialBlock,
            BotTaskKind::BuildHome,
            BotTaskKind::BuildPark,
        ][seq % 3],
        BotDistrictKind::Skyline => [
            BotTaskKind::BuildGlassTower,
            BotTaskKind::BuildTower,
            BotTaskKind::BuildPlaza,
        ][seq % 3],
        BotDistrictKind::Park => [
            BotTaskKind::BuildPark,
            BotTaskKind::BuildPlaza,
            BotTaskKind::DecorateStreet,
        ][seq % 3],
        BotDistrictKind::Service => [
            BotTaskKind::BuildServicePad,
            BotTaskKind::LandingPad,
            BotTaskKind::AddLights,
        ][seq % 3],
        BotDistrictKind::Training => [
            BotTaskKind::TargetRange,
            BotTaskKind::DecorateStreet,
            BotTaskKind::BuildServicePad,
        ][seq % 3],
        BotDistrictKind::Scenic => [
            BotTaskKind::BuildPlaza,
            BotTaskKind::BuildGlassTower,
            BotTaskKind::BuildPark,
        ][seq % 3],
    }
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
        candidates.push(Vec3::new(slot[0] as f32, slot[1] as f32, slot[2] as f32));
    }
    let center = vec3_from_arr(district.center);
    for step in 0..16 {
        let angle = (seq + step) as f32 * 2.399_963_1;
        let ring = 16.0 + (step / 4) as f32 * 18.0;
        candidates.push(center + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring));
    }
    if let Some(hub) = save.settlements.first().map(|s| vec3_from_arr(s.hub)) {
        for step in 0..10 {
            let angle = (seq + step * 3) as f32 * 1.618_033_9;
            let ring = 24.0 + step as f32 * 8.0;
            candidates.push(hub + Vec3::new(angle.cos() * ring, 0.0, angle.sin() * ring));
        }
    }

    candidates
        .into_iter()
        .map(|target| project_origin(world, clamp_to_bounds(bounds, target)))
        .filter(|origin| bounds.contains_box(*origin, size))
        .filter(|origin| project_columns_loaded(world, *origin, size))
        .max_by(|a, b| {
            let sa = score_planned_site(save, world, district, *a, size, kind);
            let sb = score_planned_site(save, world, district, *b, size, kind);
            sa.total_cmp(&sb)
        })
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
    let connected = district
        .road_anchors
        .iter()
        .any(|a| Vec2::new((a[0] - center_x) as f32, (a[2] - center_z) as f32).length() < 96.0);
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
    score_city_slot(flatness, connected, inside, balance, true)
        - bounds.distance_from_center(center_x as f32, center_z as f32) * 0.0005
}

fn score_city_slot(
    flatness: f32,
    connected: bool,
    inside_bounds: bool,
    district_balance: f32,
    player_clearance: bool,
) -> f32 {
    if !inside_bounds || !player_clearance {
        return -10_000.0;
    }
    flatness * 2.5 + if connected { 2.0 } else { 0.0 } + district_balance.clamp(0.0, 1.0) * 1.8
}

fn terrain_flatness(world: &VoxelWorld, x: i32, z: i32, radius: i32) -> f32 {
    let r = radius.clamp(4, 36);
    let samples = [
        world.surface_height_at(x, z),
        world.surface_height_at(x + r, z),
        world.surface_height_at(x - r, z),
        world.surface_height_at(x, z + r),
        world.surface_height_at(x, z - r),
    ];
    let min = samples.iter().min().copied().unwrap_or(0);
    let max = samples.iter().max().copied().unwrap_or(0);
    (1.0 - (max - min) as f32 / 18.0).clamp(0.0, 1.0)
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
    let source = if manual {
        "player request"
    } else {
        "autonomous city planner"
    };
    let brief = format!(
        "{label}: {source}; footprint {}x{}x{} at {},{},{}; owned by {team}.",
        size[0], size[1], size[2], origin[0], origin[1], origin[2]
    );
    let rows = vec![
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
            phase: "Structure".into(),
            owner: role_owner_label(save, kind.preferred_role(), &team),
            material: material_plan.into(),
            detail: structure.into(),
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
    BotProjectConcept {
        brief,
        structure: structure.into(),
        material_plan: material_plan.into(),
        visual_goal: visual_goal.into(),
        rows,
    }
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
            "Orthogonal avenues with cross streets, intersection markings, sidewalks, and planted pockets.",
            "stone asphalt, limestone sidewalks, alloy curbs",
            "A Manhattan-like block grammar that future towers can snap to.",
        ),
        BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower | BotTaskKind::MakeTaller => (
            "Setback tower with podium, window grid, floor bands, roof parapet, HVAC blocks, and antenna detail.",
            "alloy frame, glass windows, stone podium, restrained signs",
            "Dense skyline massing with readable facade rhythm and roof equipment.",
        ),
        BotTaskKind::BuildResidentialBlock | BotTaskKind::BuildHome => (
            "Perimeter housing block with entries, courtyards, stoops, windows, fire-escape rhythm, and roof tanks.",
            "limestone walls, glass windows, wood doors, dark roof trim",
            "Human-scale residential streets rather than isolated boxes.",
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
    if !project_columns_loaded(world, origin, size) {
        return Err("target area is not loaded yet".into());
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

fn project_columns_loaded(world: &VoxelWorld, origin: [i32; 3], size: [i32; 3]) -> bool {
    let max_x = origin[0] + size[0].max(1) - 1;
    let max_z = origin[2] + size[2].max(1) - 1;
    [
        (origin[0], origin[2]),
        (max_x, origin[2]),
        (origin[0], max_z),
        (max_x, max_z),
        (origin[0] + size[0] / 2, origin[2] + size[2] / 2),
    ]
    .into_iter()
    .all(|(x, z)| world.is_column_loaded(x, z))
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
            BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad | BotTaskKind::ExpandRoadGrid => {
                settlement.road_count = settlement.road_count.saturating_add(1)
            }
            BotTaskKind::BuildPark | BotTaskKind::BuildPlaza => {
                settlement.park_count = settlement.park_count.saturating_add(1)
            }
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
    let kind = save.districts.iter().find(|d| d.id == district_id)?.kind;
    Some(match kind {
        BotDistrictKind::HubCore | BotDistrictKind::Service => BotTheme::CyanAlloy,
        BotDistrictKind::Residential => BotTheme::WhiteAlloy,
        BotDistrictKind::Skyline | BotDistrictKind::Scenic => BotTheme::MagentaGlass,
        BotDistrictKind::Park => BotTheme::GreenPark,
        BotDistrictKind::Training => BotTheme::AmberStreet,
    })
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
) -> ProjectAdvance {
    project.status = BotProjectStatus::Active;
    let mut out = ProjectAdvance::default();
    let mut batch = WorldEditBatch::default();
    let mut changes = Vec::new();
    let budget = 560usize;
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

fn project_voxel(project: &BotProject, local: IVec3, world: &VoxelWorld) -> Option<(IVec3, Voxel)> {
    let origin = IVec3::new(project.origin[0], project.origin[1], project.origin[2]);
    match project.kind {
        BotTaskKind::BuildRoad | BotTaskKind::RecolorRoad => {
            let x = origin.x + local.x;
            let curve = ((local.x as f32 * 0.16).sin() * 2.0).round() as i32;
            let z = origin.z + local.z + curve;
            let y = world.surface_height_at(x, z) + 1;
            let width = project.size[2].max(1);
            let sidewalk = local.z <= 1 || local.z >= width - 2;
            let curb = local.z == 2 || local.z == width - 3;
            let lane = local.z == width / 2 && local.x.rem_euclid(10) < 5;
            let crosswalk = local.x.rem_euclid(34) < 4 && local.z > 2 && local.z < width - 3;
            let signal = (local.x.rem_euclid(28) == 0) && (local.z == 1 || local.z == width - 2);
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
            let lamp = local.x.rem_euclid(16) == 0
                && (local.z == 1 || local.z == width - 2)
                && local.y <= 4;
            let bench = local.y == 1
                && local.x.rem_euclid(18) <= 3
                && (local.z == 0 || local.z == width - 1);
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
            let y = world.surface_height_at(x, z) + 1;
            let mid_x = project.size[0] / 2;
            let mid_z = project.size[2] / 2;
            let cell_x = local.x.rem_euclid(24);
            let cell_z = local.z.rem_euclid(24);
            let road_x = cell_x <= 7 || (local.x - mid_x).abs() <= 4;
            let road_z = cell_z <= 7 || (local.z - mid_z).abs() <= 4;
            let sidewalk_x = cell_x == 8 || cell_x == 23 || (local.x - mid_x).abs() == 5;
            let sidewalk_z = cell_z == 8 || cell_z == 23 || (local.z - mid_z).abs() == 5;
            let lane = (road_x && (cell_x == 3 || local.x == mid_x) && local.z.rem_euclid(10) < 5)
                || (road_z && (cell_z == 3 || local.z == mid_z) && local.x.rem_euclid(10) < 5);
            let intersection = road_x && road_z;
            let crosswalk = intersection
                && (cell_x == 6 || cell_z == 6 || local.x == mid_x || local.z == mid_z);
            if local.y == 0 && (road_x || road_z) {
                Some((
                    IVec3::new(x, y, z),
                    if lane || crosswalk {
                        Voxel::from(BlockType::Limestone)
                    } else {
                        Voxel::from(BlockType::Stone)
                    },
                ))
            } else if local.y == 0 && (sidewalk_x || sidewalk_z) {
                Some((IVec3::new(x, y, z), Voxel::from(BlockType::Limestone)))
            } else if local.y == 0 && (local.x + local.z).rem_euclid(31) == 0 {
                Some((IVec3::new(x, y, z), Voxel::from(BlockType::Leaves)))
            } else if local.y == 0 && (local.x * 13 + local.z * 7).rem_euclid(47) == 0 {
                Some((IVec3::new(x, y, z), project.theme.signal()))
            } else {
                let intersection_corner = (sidewalk_x && sidewalk_z)
                    || ((local.x - mid_x).abs() == 5 && (local.z - mid_z).abs() == 5);
                let traffic_light = intersection_corner && local.y <= 5;
                let lamp = (sidewalk_x || sidewalk_z)
                    && (local.x * 5 + local.z * 3).rem_euclid(29) == 0
                    && local.y <= 4;
                let bench = local.y == 1
                    && (sidewalk_x || sidewalk_z)
                    && (local.x * 7 + local.z * 11).rem_euclid(41) <= 3;
                if traffic_light {
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
        }
        BotTaskKind::LandingPad | BotTaskKind::BuildServicePad => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let y = world.surface_height_at(x, z) + 1;
            let edge = local.x == 0
                || local.z == 0
                || local.x == project.size[0] - 1
                || local.z == project.size[2] - 1;
            let cross = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let beacon = edge && (local.x + local.z).rem_euclid(10) == 0;
            if local.y == 0 {
                let voxel = if beacon {
                    project.theme.signal()
                } else if edge || cross {
                    project.theme.accent()
                } else {
                    Voxel::from(BlockType::ShipHullAlloy)
                };
                Some((IVec3::new(x, y, z), voxel))
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
                None
            }
        }
        BotTaskKind::BuildHome
        | BotTaskKind::BuildTower
        | BotTaskKind::BuildGlassTower
        | BotTaskKind::MakeTaller => {
            let p = origin + local;
            let surface = world.surface_height_at(p.x, p.z) + 1;
            if local.y <= 4 && surface + local.y < origin.y {
                let perimeter = local.x == 0
                    || local.z == 0
                    || local.x == project.size[0] - 1
                    || local.z == project.size[2] - 1;
                if perimeter || (local.x + local.z).rem_euclid(5) == 0 {
                    return Some((
                        IVec3::new(p.x, surface + local.y, p.z),
                        Voxel::from(BlockType::Basalt),
                    ));
                }
            }
            let sx = project.size[0] - 1;
            let sy = project.size[1] - 1;
            let sz = project.size[2] - 1;
            let upper = local.y > sy * 2 / 3;
            let mid = local.y > sy / 3;
            let setback = if matches!(
                project.kind,
                BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower
            ) && upper
            {
                2
            } else if matches!(
                project.kind,
                BotTaskKind::BuildTower | BotTaskKind::BuildGlassTower
            ) && mid
            {
                1
            } else {
                0
            };
            let in_mass = local.x >= setback
                && local.x <= sx - setback
                && local.z >= setback
                && local.z <= sz - setback;
            if !in_mass && local.y > 0 {
                return Some((p, AIR));
            }
            let shell = local.x == setback
                || local.x == sx - setback
                || local.y == 0
                || local.y == sy
                || local.z == setback
                || local.z == sz - setback;
            let podium = local.y <= 3;
            let floor_band = local.y > 3 && local.y % 5 == 0;
            let window_slot = ((local.x + setback).rem_euclid(4) == 1)
                || ((local.z + setback).rem_euclid(4) == 1);
            let window = shell && !podium && local.y < sy && !floor_band && window_slot;
            let glass_tower = matches!(project.kind, BotTaskKind::BuildGlassTower);
            let core = !shell
                && (local.x - sx / 2).abs() <= 1
                && (local.z - sz / 2).abs() <= 1
                && local.y < sy;
            let interior_floor = !shell && floor_band;
            let interior_wall = !shell
                && local.y > 4
                && local.y < sy
                && local.y % 5 != 0
                && local.y % 5 <= 3
                && ((local.x - setback).rem_euclid(7) == 0
                    || (local.z - setback).rem_euclid(7) == 0);
            let lobby_detail = !shell
                && podium
                && local.y == 2
                && (local.z == setback + 2 || local.x == sx - setback - 2);
            let voxel = if local.y == 0 {
                Voxel::from(BlockType::Limestone)
            } else if local.y == sy {
                let hvac = (local.x - sx / 2).abs() <= 2 && (local.z - sz / 2).abs() <= 1;
                let antenna = local.x == sx / 2 && local.z == sz / 2;
                if antenna {
                    project.theme.signal()
                } else if hvac
                    || local.x == setback
                    || local.x == sx - setback
                    || local.z == setback
                    || local.z == sz - setback
                {
                    Voxel::from(BlockType::ShipHullDark)
                } else {
                    Voxel::from(BlockType::Basalt)
                }
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
            let base = world.surface_height_at(x, z) + 1;
            let cell_x = local.x / 11;
            let cell_z = local.z / 10;
            let lx = local.x % 11;
            let lz = local.z % 10;
            let path = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let courtyard = cell_x == 1 && cell_z == 1;
            if local.y == 0 {
                return Some((
                    IVec3::new(x, base, z),
                    if path {
                        Voxel::from(BlockType::Limestone)
                    } else if courtyard {
                        Voxel::from(BlockType::Grass)
                    } else {
                        project.theme.floor()
                    },
                ));
            }
            if cell_x > 2 || cell_z > 2 || lx > 8 || lz > 7 {
                return None;
            }
            if courtyard {
                if local.y <= 4 && (lx == 1 || lx == 7) && (lz == 1 || lz == 6) {
                    return Some((
                        IVec3::new(x, base + local.y, z),
                        Voxel::from(BlockType::Wood),
                    ));
                }
                if local.y == 5 && (lx == 1 || lx == 7) && (lz == 1 || lz == 6) {
                    return Some((
                        IVec3::new(x, base + local.y, z),
                        Voxel::from(BlockType::Leaves),
                    ));
                }
                return None;
            }
            let building_h = (8 + (cell_x + cell_z).rem_euclid(3)).min(project.size[1] - 2);
            if local.y > building_h {
                return None;
            }
            let wall = lx == 0 || lx == 8 || lz == 0 || lz == 7 || local.y == building_h;
            let door = local.y <= 2 && lz == 0 && (lx == 3 || lx == 4);
            let window = wall
                && local.y > 2
                && local.y < building_h
                && local.y % 2 == 0
                && (lx == 2 || lx == 5 || lz == 2 || lz == 5);
            let fire_escape = lz == 7
                && local.y > 3
                && local.y < building_h
                && local.y % 3 == 0
                && lx >= 2
                && lx <= 6;
            let roof_tank = local.y == building_h && (lx - 4).abs() <= 1 && (lz - 4).abs() <= 1;
            let voxel = if door {
                Voxel::from(BlockType::Wood)
            } else if fire_escape {
                Voxel::from(BlockType::ShipHullDark)
            } else if window {
                Voxel::from(BlockType::CockpitGlass)
            } else if roof_tank {
                Voxel::from(BlockType::Wood)
            } else if wall {
                if local.y == building_h {
                    project.theme.accent()
                } else {
                    project.theme.wall()
                }
            } else {
                AIR
            };
            Some((IVec3::new(x, base + local.y, z), voxel))
        }
        BotTaskKind::BuildPark => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let base = world.surface_height_at(x, z) + 1;
            let center_path = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let tree = (local.x * 17 + local.z * 23).rem_euclid(31) == 0;
            if local.y == 0 {
                Some((
                    IVec3::new(x, base, z),
                    if center_path {
                        Voxel::from(BlockType::Limestone)
                    } else {
                        Voxel::from(BlockType::Grass)
                    },
                ))
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
                None
            }
        }
        BotTaskKind::BuildPlaza | BotTaskKind::UpgradeDistrict => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let base = world.surface_height_at(x, z) + 1;
            let sx = project.size[0] - 1;
            let sz = project.size[2] - 1;
            let edge = local.x == 0 || local.z == 0 || local.x == sx || local.z == sz;
            let cross = local.x == project.size[0] / 2 || local.z == project.size[2] / 2;
            let center = (local.x - project.size[0] / 2).abs() <= 2
                && (local.z - project.size[2] / 2).abs() <= 2;
            if local.y == 0 {
                let voxel = if edge || cross || center {
                    project.theme.accent()
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
            } else if edge && local.y <= 5 && (local.x + local.z).rem_euclid(9) == 0 {
                let voxel = if local.y == 5 {
                    project.theme.signal()
                } else {
                    Voxel::from(BlockType::ShipHullDark)
                };
                Some((IVec3::new(x, base + local.y, z), voxel))
            } else {
                None
            }
        }
        BotTaskKind::AddLights | BotTaskKind::DecorateStreet => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let edge = local.z == 0 || local.z == project.size[2] - 1;
            let base = world.surface_height_at(x, z) + 1;
            let lamp = edge && local.x % 8 == 0;
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
                None
            }
        }
        BotTaskKind::ClearFlatten => {
            let x = origin.x + local.x;
            let z = origin.z + local.z;
            let surface = world.surface_height_at(x, z) + 1;
            let terrain_delta = (surface - origin.y).clamp(-4, 4);
            let pad_y = origin.y + terrain_delta;
            let y = pad_y + local.y;
            let edge = local.x == 0
                || local.z == 0
                || local.x == project.size[0] - 1
                || local.z == project.size[2] - 1;
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
            let base = world.surface_height_at(x, z) + 1;
            if local.y == 0 {
                let lane = local.x % 6 == 0;
                return Some((
                    IVec3::new(x, base, z),
                    if lane {
                        project.theme.accent()
                    } else {
                        Voxel::from(BlockType::Stone)
                    },
                ));
            }
            let target_wall = local.z == project.size[2] - 2 && local.y <= 5 && local.x % 5 <= 2;
            let cover = local.z == project.size[2] / 2 && local.y <= 2 && local.x % 7 <= 2;
            if target_wall {
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
                None
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
    if player.map(|x| x.distance(p) < 7.0).unwrap_or(false) {
        return true;
    }
    ships.iter().any(|s| s.distance(p) < 10.0)
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
            brain.save.autonomy.intensity = 10;
            if let Some(settlement) = brain.save.settlements.first_mut() {
                settlement.bounds.max_active_projects = MAX_ACTIVE_PROJECTS_LIMIT;
            }
            let queued_now = queue_mega_city_starter_projects(
                &mut brain.save,
                &world,
                player_pos,
                &ship_positions,
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
                format!("Mega city started: {queued_now} visible starter build(s) queued near you.")
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
            let frame = egui::Frame::none()
                .fill(egui::Color32::from_rgba_premultiplied(3, 8, 13, 218))
                .stroke(egui::Stroke::new(1.0, theme.color.primary()))
                .inner_margin(egui::Margin::symmetric(8.0, 8.0))
                .rounding(egui::Rounding::same(8.0));
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
        egui::Color32::from_rgba_unmultiplied(15, 26, 34, 235)
    };
    painter.rect_filled(rect, egui::Rounding::same(8.0), fill);
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
        egui::Color32::from_rgba_unmultiplied(12, 20, 28, 238),
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
        let connected = score_city_slot(0.8, true, true, 0.7, true);
        let isolated = score_city_slot(0.8, false, true, 0.7, true);
        assert!(connected > isolated);
        assert!(score_city_slot(1.0, true, false, 1.0, true) < 0.0);
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
