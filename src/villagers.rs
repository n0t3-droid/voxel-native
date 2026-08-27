//! Civic Ecology: deterministic, non-economic settlement inhabitants.
//!
//! The construction companions in `bots.rs` own voxel-edit commands.  Civic
//! residents are a separate saved social simulation: they observe terrain,
//! time, weather, and settlement anchors, but never buy, sell, price, offer,
//! or mutate a voxel.  ECS entities are disposable visual projections of the
//! bounded records embedded in `BotWorldSave`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};

use crate::blocks::BlockType;
use crate::bots::{BotSettlement, FriendlyWorldBrain};
use crate::menu::GameState;
use crate::player::Player;
use crate::settings::{
    ActiveWorld, SceneryQuality, TerrainGrammarVersion, WorldGenerationIdentity, WorldProfile,
    WorldSettings,
};
use crate::terrain::{Biome, EnvironmentSample};
use crate::world::VoxelWorld;

const CIVIC_SCHEMA_VERSION: u32 = 1;
const CIVIC_SEED_POPULATION: usize = 12;
const CIVIC_WORLD_HARD_LIMIT: usize = 128;
const CIVIC_SETTLEMENT_HARD_LIMIT: usize = 32;
const CIVIC_ACTIVE_LOGICAL_LIMIT: usize = 64;
const CIVIC_FULL_RIG_LIMIT: usize = 8;
const CIVIC_PROXY_LIMIT: usize = 24;
const CIVIC_VISUAL_BUILD_LIMIT: usize = 2;
const CIVIC_VISUAL_REMOVE_LIMIT: usize = 4;
const CIVIC_ROOT_SYNC_LIMIT: usize = 16;
const CIVIC_ANIMATION_LIMIT: usize = 8;
const CIVIC_RECONCILE_LIMIT: usize = 2;
const CIVIC_MEMORY_HARD_LIMIT: usize = 12;
const CIVIC_RELATIONSHIP_HARD_LIMIT: usize = 12;
const CIVIC_NOTICE_HARD_LIMIT: usize = 16;
const CIVIC_DECISIONS_PER_TICK: usize = 8;
const CIVIC_FIXED_STEP_SECONDS: f32 = 0.2;
const CIVIC_MAX_CATCHUP_STEPS: u8 = 2;
const CIVIC_MAX_FRAME_DELTA_SECONDS: f32 =
    CIVIC_FIXED_STEP_SECONDS * CIVIC_MAX_CATCHUP_STEPS as f32;
const CIVIC_PATH_QUEUE_LIMIT: usize = 32;
const CIVIC_PATH_EXPANSION_LIMIT: usize = 768;
const CIVIC_PATH_CELL_LIMIT: usize = 96;
const CIVIC_PATH_CACHE_LIMIT: usize = 64;
const CIVIC_MAX_ROUTE_RADIUS: i32 = 48;
const CIVIC_WALK_SPEED_MM_PER_SECOND: u16 = 1_400;
const CIVIC_POSITION_CHECKPOINT_SECONDS: f32 = 5.0;
const CIVIC_LOD_REFRESH_SECONDS: f32 = 0.25;
const CIVIC_ACTIVE_DISTANCE: f32 = 320.0;
const CIVIC_VISUAL_DISTANCE: f32 = 220.0;
const CIVIC_COVERAGE_RETRY_BASE_TICKS: u64 = 10;
const CIVIC_ROUTE_RETRY_BASE_TICKS: u64 = 40;
const CIVIC_ROUTE_RETRY_MAX_SHIFT: u32 = 4;

const HOME_OFFSETS: [[i32; 2]; CIVIC_SEED_POPULATION] = [
    [-12, -7],
    [-7, -13],
    [1, -14],
    [9, -11],
    [14, -3],
    [13, 6],
    [7, 13],
    [-1, 15],
    [-9, 12],
    [-14, 5],
    [-15, -2],
    [-8, -8],
];

const WORK_OFFSETS: [[i32; 2]; 8] = [
    [-20, 2],
    [-14, 16],
    [1, 21],
    [17, 14],
    [22, -1],
    [15, -17],
    [-1, -22],
    [-17, -14],
];

const COMMONS_OFFSETS: [[i32; 2]; 8] = [
    [-3, -1],
    [-2, -3],
    [1, -3],
    [3, -1],
    [3, 2],
    [1, 3],
    [-2, 3],
    [-3, 1],
];

const NAV_OFFSETS: [[i32; 2]; 4] = [[0, -1], [-1, 0], [1, 0], [0, 1]];
const SAFE_CELL_OFFSETS: [[i32; 2]; 9] = [
    [0, 0],
    [0, -2],
    [-2, 0],
    [2, 0],
    [0, 2],
    [-2, -2],
    [2, -2],
    [-2, 2],
    [2, 2],
];

pub struct VillagersPlugin;

impl Plugin for VillagersPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CivicRuntime::from_environment())
            .insert_resource(CivicVisualCache::default())
            .add_systems(
                OnEnter(GameState::MainMenu),
                cleanup_civic_visuals.after(crate::bots::save_bot_world_on_world_unload),
            )
            .add_systems(
                Update,
                (
                    ensure_civic_authority,
                    reconcile_civic_cells,
                    refresh_civic_lod,
                    tick_civic_clock,
                    service_civic_path_queue,
                    advance_civic_residents,
                    remove_stale_civic_visuals,
                    spawn_missing_civic_visuals,
                    sync_civic_visuals,
                    animate_civic_visuals,
                )
                    .chain()
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CivicAuthorityState {
    #[default]
    Uninitialized,
    Active,
    IdentityBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CivicFailureCode {
    #[default]
    None,
    GenerationIdentityMismatch,
    InvalidSettlement,
    PathBudgetExhausted,
    CoverageUnresolved,
    NoRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CivicCulture {
    #[default]
    Riverglass,
    Canopy,
    Sunstone,
    Highland,
    Frostweave,
    Astral,
}

impl CivicCulture {
    const ALL: [Self; 6] = [
        Self::Riverglass,
        Self::Canopy,
        Self::Sunstone,
        Self::Highland,
        Self::Frostweave,
        Self::Astral,
    ];

    fn from_environment(
        profile: WorldProfile,
        biome: Biome,
        environment: EnvironmentSample,
    ) -> Self {
        if profile == WorldProfile::AstralFrontier
            || matches!(
                biome,
                Biome::CrystalSpires | Biome::VolcanicWaste | Biome::AlienReef
            )
        {
            return Self::Astral;
        }
        if matches!(
            biome,
            Biome::Tundra | Biome::SnowyMountains | Biome::GlacierShards
        ) || environment.temperature_norm < 0.25
        {
            return Self::Frostweave;
        }
        if matches!(biome, Biome::Mountains | Biome::Karst) || environment.mineral_resonance > 0.72
        {
            return Self::Highland;
        }
        if matches!(biome, Biome::Desert | Biome::Savanna | Biome::Mesa)
            || environment.temperature_norm > 0.76
        {
            return Self::Sunstone;
        }
        if matches!(biome, Biome::Forest | Biome::Jungle) || environment.flowering_resonance > 0.68
        {
            return Self::Canopy;
        }
        Self::Riverglass
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CivicCalling {
    #[default]
    HabitatSteward,
    Waterkeeper,
    Cultivator,
    Pathwright,
    Archivist,
    Caretaker,
    Wayfinder,
    Watcher,
}

impl CivicCalling {
    const ALL: [Self; 8] = [
        Self::HabitatSteward,
        Self::Waterkeeper,
        Self::Cultivator,
        Self::Pathwright,
        Self::Archivist,
        Self::Caretaker,
        Self::Wayfinder,
        Self::Watcher,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CivicLifeStage {
    Youth,
    #[default]
    Adult,
    Elder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum CivicActivity {
    #[default]
    RestAtHome,
    Prepare,
    Work,
    Socialize,
    ShareKnowledge,
    Play,
    InspectSettlement,
    WanderLocally,
    SeekShelter,
    Recover,
    WaitForCoverage,
}

impl CivicActivity {
    const DECISION_SET: [Self; 10] = [
        Self::RestAtHome,
        Self::Prepare,
        Self::Work,
        Self::Socialize,
        Self::ShareKnowledge,
        Self::Play,
        Self::InspectSettlement,
        Self::WanderLocally,
        Self::SeekShelter,
        Self::Recover,
    ];

    const fn stable_tag(self) -> u8 {
        match self {
            Self::RestAtHome => 0,
            Self::Prepare => 1,
            Self::Work => 2,
            Self::Socialize => 3,
            Self::ShareKnowledge => 4,
            Self::Play => 5,
            Self::InspectSettlement => 6,
            Self::WanderLocally => 7,
            Self::SeekShelter => 8,
            Self::Recover => 9,
            Self::WaitForCoverage => 10,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::RestAtHome => "REST",
            Self::Prepare => "PREPARE",
            Self::Work => "STEWARDSHIP",
            Self::Socialize => "COMMONS",
            Self::ShareKnowledge => "TEACH",
            Self::Play => "PLAY",
            Self::InspectSettlement => "INSPECT",
            Self::WanderLocally => "EXPLORE",
            Self::SeekShelter => "SHELTER",
            Self::Recover => "RECOVER",
            Self::WaitForCoverage => "WAIT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicNeeds {
    /// Deficit scales: zero is fulfilled, 1000 is urgent.
    pub energy: u16,
    pub belonging: u16,
    pub safety: u16,
    pub purpose: u16,
    pub curiosity: u16,
}

impl Default for CivicNeeds {
    fn default() -> Self {
        Self {
            energy: 240,
            belonging: 260,
            safety: 100,
            purpose: 300,
            curiosity: 320,
        }
    }
}

impl CivicNeeds {
    fn normalize(&mut self) {
        self.energy = self.energy.min(1_000);
        self.belonging = self.belonging.min(1_000);
        self.safety = self.safety.min(1_000);
        self.purpose = self.purpose.min(1_000);
        self.curiosity = self.curiosity.min(1_000);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CivicMemoryKind {
    #[default]
    ShelterReached,
    CommunityGathering,
    KnowledgeShared,
    RouteBlocked,
    StewardshipCompleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicMemory {
    pub kind: CivicMemoryKind,
    pub logical_tick: u64,
    pub cell: [i32; 3],
    pub confidence: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicRelationship {
    pub resident_id: u64,
    pub familiarity: u16,
    pub trust: u16,
    pub last_shared_tick: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicResident {
    pub id: u64,
    pub settlement_id: u64,
    pub name: String,
    pub culture: CivicCulture,
    pub calling: CivicCalling,
    pub life_stage: CivicLifeStage,
    pub logical_cell: [i32; 3],
    pub home_cell: [i32; 3],
    pub work_cell: [i32; 3],
    pub commons_cell: [i32; 3],
    pub shelter_cell: [i32; 3],
    pub target_cell: [i32; 3],
    pub activity: CivicActivity,
    pub committed_until_tick: u64,
    pub needs: CivicNeeds,
    #[serde(default)]
    pub memories: Vec<CivicMemory>,
    #[serde(default)]
    pub relationships: Vec<CivicRelationship>,
    #[serde(default)]
    pub movement_progress_mm: u16,
    #[serde(default)]
    pub route_failures: u8,
    #[serde(default)]
    pub route_retry_after_tick: u64,
    #[serde(default)]
    pub deferred_activity: Option<CivicActivity>,
    #[serde(default)]
    pub route_failure: CivicFailureCode,
    #[serde(default)]
    pub last_decision_tick: u64,
}

impl CivicResident {
    fn normalize_shallow(&mut self) {
        self.name = self.name.chars().take(48).collect();
        if self.name.trim().is_empty() {
            self.name = format!("Resident-{:04X}", self.id as u16);
        }
        self.needs.normalize();
        self.movement_progress_mm %= 1_000;
        self.route_failures = self.route_failures.min(15);
        for memory in &mut self.memories {
            memory.confidence = memory.confidence.min(1_000);
        }
        for relationship in &mut self.relationships {
            relationship.familiarity = relationship.familiarity.min(1_000);
            relationship.trust = relationship.trust.min(1_000);
        }
        self.memories.sort_by_key(|memory| {
            (
                std::cmp::Reverse(memory.logical_tick),
                memory.kind as u8,
                memory.cell,
            )
        });
        self.memories.truncate(CIVIC_MEMORY_HARD_LIMIT);
        let mut canonical_relationships = BTreeMap::<u64, CivicRelationship>::new();
        for relationship in self.relationships.drain(..) {
            let score = (
                relationship.familiarity,
                relationship.trust,
                relationship.last_shared_tick,
            );
            match canonical_relationships.entry(relationship.resident_id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(relationship);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get();
                    let existing_score = (
                        existing.familiarity,
                        existing.trust,
                        existing.last_shared_tick,
                    );
                    if score > existing_score {
                        entry.insert(relationship);
                    }
                }
            }
        }
        self.relationships = canonical_relationships.into_values().collect();
        self.relationships.sort_by_key(|relationship| {
            (
                std::cmp::Reverse(relationship.familiarity),
                std::cmp::Reverse(relationship.trust),
                std::cmp::Reverse(relationship.last_shared_tick),
                relationship.resident_id,
            )
        });
        self.relationships.truncate(CIVIC_RELATIONSHIP_HARD_LIMIT);
        if !matches!(
            self.route_failure,
            CivicFailureCode::None
                | CivicFailureCode::PathBudgetExhausted
                | CivicFailureCode::CoverageUnresolved
                | CivicFailureCode::NoRoute
        ) {
            self.route_failure = CivicFailureCode::None;
            self.route_retry_after_tick = 0;
        }
        if self.activity != CivicActivity::WaitForCoverage
            || self.deferred_activity == Some(CivicActivity::WaitForCoverage)
        {
            self.deferred_activity = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicNotice {
    pub logical_tick: u64,
    pub code: CivicMemoryKind,
    pub cell: [i32; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CivicBlackboard {
    #[serde(default)]
    pub notices: Vec<CivicNotice>,
    #[serde(default)]
    pub blocked_routes: u32,
    #[serde(default)]
    pub successful_gatherings: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CivicPopulation {
    #[serde(default = "civic_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub authority: CivicAuthorityState,
    #[serde(default)]
    pub generation_identity: Option<WorldGenerationIdentity>,
    #[serde(default)]
    pub logical_tick: u64,
    #[serde(default = "default_next_civic_id")]
    pub next_resident_id: u64,
    #[serde(default)]
    pub residents: Vec<CivicResident>,
    #[serde(default)]
    pub blackboard: CivicBlackboard,
    #[serde(default)]
    pub last_failure: CivicFailureCode,
}

const fn civic_schema_version() -> u32 {
    CIVIC_SCHEMA_VERSION
}

const fn default_next_civic_id() -> u64 {
    1_u64 << 63
}

impl Default for CivicPopulation {
    fn default() -> Self {
        Self {
            schema_version: CIVIC_SCHEMA_VERSION,
            authority: CivicAuthorityState::Uninitialized,
            generation_identity: None,
            logical_tick: 0,
            next_resident_id: default_next_civic_id(),
            residents: Vec::new(),
            blackboard: CivicBlackboard::default(),
            last_failure: CivicFailureCode::None,
        }
    }
}

impl CivicPopulation {
    pub(crate) fn normalize(&mut self, settlement_ids: impl IntoIterator<Item = u64>) {
        self.schema_version = CIVIC_SCHEMA_VERSION;
        let valid_settlements = settlement_ids.into_iter().collect::<BTreeSet<_>>();
        self.blackboard
            .notices
            .sort_by_key(|notice| std::cmp::Reverse(notice.logical_tick));
        self.blackboard.notices.truncate(CIVIC_NOTICE_HARD_LIMIT);
        if valid_settlements.is_empty() {
            self.residents.clear();
            self.authority = CivicAuthorityState::Uninitialized;
            self.last_failure = CivicFailureCode::InvalidSettlement;
            return;
        }

        self.residents.retain(|resident| {
            resident.id != 0 && valid_settlements.contains(&resident.settlement_id)
        });
        self.residents
            .sort_by_key(|resident| (resident.settlement_id, resident.id));
        let mut unique_ids = BTreeSet::new();
        self.residents
            .retain(|resident| unique_ids.insert(resident.id));
        let mut per_settlement = BTreeMap::<u64, usize>::new();
        self.residents.retain(|resident| {
            let count = per_settlement.entry(resident.settlement_id).or_default();
            if *count >= CIVIC_SETTLEMENT_HARD_LIMIT {
                false
            } else {
                *count += 1;
                true
            }
        });
        self.residents.truncate(CIVIC_WORLD_HARD_LIMIT);
        for resident in &mut self.residents {
            resident.normalize_shallow();
        }

        let resident_ids = self
            .residents
            .iter()
            .map(|resident| resident.id)
            .collect::<BTreeSet<_>>();
        for resident in &mut self.residents {
            resident.relationships.retain(|relationship| {
                relationship.resident_id != resident.id
                    && resident_ids.contains(&relationship.resident_id)
            });
            resident
                .relationships
                .truncate(CIVIC_RELATIONSHIP_HARD_LIMIT);
        }
        let max_id = self
            .residents
            .iter()
            .map(|resident| resident.id)
            .max()
            .unwrap_or(default_next_civic_id() - 1);
        self.next_resident_id = self
            .next_resident_id
            .max(max_id.saturating_add(1))
            .max(default_next_civic_id());
    }

    pub(crate) fn telemetry_line(&self) -> String {
        let mut counts = [0usize; 11];
        for resident in &self.residents {
            counts[resident.activity.stable_tag() as usize] += 1;
        }
        format!(
            "{} residents // {} {} // {} {} // {} {} // blocked {}",
            self.residents.len(),
            CivicActivity::Work.label(),
            counts[CivicActivity::Work.stable_tag() as usize],
            CivicActivity::Socialize.label(),
            counts[CivicActivity::Socialize.stable_tag() as usize],
            CivicActivity::SeekShelter.label(),
            counts[CivicActivity::SeekShelter.stable_tag() as usize],
            self.blackboard.blocked_routes,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CivicVisualMode {
    Detailed,
    Proxy,
}

impl CivicVisualMode {
    const fn stable_tag(self) -> u8 {
        match self {
            Self::Detailed => 0,
            Self::Proxy => 1,
        }
    }
}

#[derive(Component)]
struct CivicVisualRoot {
    resident_id: u64,
    mode: CivicVisualMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CivicBodyPartKind {
    Head,
    Torso,
    LeftArm,
    RightArm,
    LeftLeg,
    RightLeg,
    Collar,
    Sash,
    EyeLeft,
    EyeRight,
}

#[derive(Component)]
struct CivicBodyPart {
    resident_id: u64,
    kind: CivicBodyPartKind,
    base_translation: Vec3,
    base_rotation: Quat,
    base_scale: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivicVisualSelection {
    resident_id: u64,
    mode: CivicVisualMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CivicLodPlan {
    logical_active: Vec<u64>,
    visuals: Vec<CivicVisualSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CivicVisualDelta {
    removals: Vec<CivicVisualSelection>,
    builds: Vec<CivicVisualSelection>,
}

fn plan_civic_visual_delta(
    existing: &[CivicVisualSelection],
    desired: &[CivicVisualSelection],
) -> CivicVisualDelta {
    let desired_pairs = desired
        .iter()
        .map(|selection| (selection.resident_id, selection.mode))
        .collect::<BTreeSet<_>>();
    let mut sorted_existing = existing.to_vec();
    sorted_existing.sort_by_key(|selection| (selection.resident_id, selection.mode.stable_tag()));
    let mut retained_residents = BTreeSet::new();
    let mut removals = sorted_existing
        .iter()
        .copied()
        .filter(|selection| {
            !desired_pairs.contains(&(selection.resident_id, selection.mode))
                || !retained_residents.insert(selection.resident_id)
        })
        .collect::<Vec<_>>();
    removals.truncate(CIVIC_VISUAL_REMOVE_LIMIT);

    let mut existing_residents = existing
        .iter()
        .map(|selection| selection.resident_id)
        .collect::<BTreeSet<_>>();
    let mut desired_sorted = desired.to_vec();
    desired_sorted.sort_by_key(|selection| (selection.resident_id, selection.mode.stable_tag()));
    let mut projected_roots = existing.len();
    let mut projected_detailed = existing
        .iter()
        .filter(|selection| selection.mode == CivicVisualMode::Detailed)
        .count();
    let mut builds = Vec::with_capacity(CIVIC_VISUAL_BUILD_LIMIT);
    for selection in desired_sorted {
        if builds.len() >= CIVIC_VISUAL_BUILD_LIMIT || projected_roots >= CIVIC_PROXY_LIMIT {
            break;
        }
        if existing_residents.contains(&selection.resident_id)
            || (selection.mode == CivicVisualMode::Detailed
                && projected_detailed >= CIVIC_FULL_RIG_LIMIT)
        {
            continue;
        }
        existing_residents.insert(selection.resident_id);
        projected_roots += 1;
        if selection.mode == CivicVisualMode::Detailed {
            projected_detailed += 1;
        }
        builds.push(selection);
    }
    CivicVisualDelta { removals, builds }
}

fn plan_civic_lod(candidates: &[(u64, Vec3)], player: Option<Vec3>) -> CivicLodPlan {
    let mut ranked = candidates
        .iter()
        .map(|(resident_id, position)| {
            let distance_squared = player
                .map(|player| position.distance_squared(player))
                .unwrap_or(0.0);
            (*resident_id, distance_squared)
        })
        .filter(|(_, distance_squared)| {
            player.is_none() || *distance_squared <= CIVIC_ACTIVE_DISTANCE * CIVIC_ACTIVE_DISTANCE
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });

    let mut logical_active = ranked
        .iter()
        .take(CIVIC_ACTIVE_LOGICAL_LIMIT)
        .map(|(resident_id, _)| *resident_id)
        .collect::<Vec<_>>();
    logical_active.sort_unstable();
    let visuals = ranked
        .into_iter()
        .filter(|(_, distance_squared)| {
            player.is_none() || *distance_squared <= CIVIC_VISUAL_DISTANCE * CIVIC_VISUAL_DISTANCE
        })
        .take(CIVIC_PROXY_LIMIT)
        .enumerate()
        .map(|(visual_index, (resident_id, _))| CivicVisualSelection {
            resident_id,
            mode: if visual_index < CIVIC_FULL_RIG_LIMIT {
                CivicVisualMode::Detailed
            } else {
                CivicVisualMode::Proxy
            },
        })
        .collect();
    CivicLodPlan {
        logical_active,
        visuals,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CivicPathRequest {
    resident_id: u64,
    start: [i32; 3],
    goal: [i32; 3],
    requested_tick: u64,
}

#[derive(Debug, Clone)]
struct CivicPath {
    cells: VecDeque<[i32; 3]>,
    goal: [i32; 3],
}

fn install_civic_path(
    paths: &mut BTreeMap<u64, CivicPath>,
    resident_id: u64,
    cells: Vec<[i32; 3]>,
    goal: [i32; 3],
) -> Option<u64> {
    if cells.is_empty() {
        paths.remove(&resident_id);
        return None;
    }
    let evicted = if paths.len() >= CIVIC_PATH_CACHE_LIMIT && !paths.contains_key(&resident_id) {
        let evicted = paths.keys().next().copied();
        if let Some(evicted) = evicted {
            paths.remove(&evicted);
        }
        evicted
    } else {
        None
    };
    paths.insert(
        resident_id,
        CivicPath {
            cells: cells.into(),
            goal,
        },
    );
    evicted
}

#[derive(Resource)]
struct CivicRuntime {
    enabled: bool,
    authority_identity: Option<WorldGenerationIdentity>,
    accumulator: f32,
    pending_step_ticks: VecDeque<u64>,
    decision_cursor: usize,
    logical_active: Vec<u64>,
    visual_selection: Vec<CivicVisualSelection>,
    path_queue: VecDeque<CivicPathRequest>,
    paths: BTreeMap<u64, CivicPath>,
    dirty_checkpoint: f32,
    changed_since_checkpoint: bool,
    lod_timer: f32,
    sync_cursor: usize,
    animation_cursor: usize,
    reconcile_cursor: usize,
    path_expansions_last: usize,
    path_expansions_peak: usize,
    path_budget_failures: u64,
}

impl CivicRuntime {
    fn from_environment() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var("VOXEL_NATIVE_CIVIC_ECOLOGY")
            .ok()
            .map(|value| {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "disabled"
                )
            })
            .unwrap_or(true);
        #[cfg(target_arch = "wasm32")]
        let enabled = true;

        Self {
            enabled,
            authority_identity: None,
            accumulator: 0.0,
            pending_step_ticks: VecDeque::with_capacity(usize::from(CIVIC_MAX_CATCHUP_STEPS)),
            decision_cursor: 0,
            logical_active: Vec::new(),
            visual_selection: Vec::new(),
            path_queue: VecDeque::new(),
            paths: BTreeMap::new(),
            dirty_checkpoint: 0.0,
            changed_since_checkpoint: false,
            lod_timer: 0.0,
            sync_cursor: 0,
            animation_cursor: 0,
            reconcile_cursor: 0,
            path_expansions_last: 0,
            path_expansions_peak: 0,
            path_budget_failures: 0,
        }
    }

    fn reset_world(&mut self) {
        let enabled = self.enabled;
        *self = Self::from_environment();
        self.enabled = enabled;
    }

    fn suspend_authority(&mut self) {
        self.authority_identity = None;
        self.accumulator = 0.0;
        self.pending_step_ticks.clear();
        self.logical_active.clear();
        self.visual_selection.clear();
        self.path_queue.clear();
        self.paths.clear();
        self.lod_timer = 0.0;
    }

    fn activate_authority(&mut self, identity: WorldGenerationIdentity) {
        if self.authority_identity != Some(identity) {
            self.reset_world();
            self.authority_identity = Some(identity);
        }
    }

    fn authority_ready(&self) -> bool {
        self.enabled && self.authority_identity.is_some()
    }

    fn enqueue_path(&mut self, request: CivicPathRequest) -> bool {
        self.path_queue
            .retain(|queued| queued.resident_id != request.resident_id);
        if self.path_queue.len() >= CIVIC_PATH_QUEUE_LIMIT {
            return false;
        }
        self.paths.remove(&request.resident_id);
        self.path_queue.push_back(request);
        true
    }
}

fn prune_civic_path_queue(
    queue: &mut VecDeque<CivicPathRequest>,
    resident_state: &BTreeMap<u64, ([i32; 3], [i32; 3])>,
    active_residents: &BTreeSet<u64>,
    logical_tick: u64,
) {
    queue.retain(|request| {
        active_residents.contains(&request.resident_id)
            && request.requested_tick <= logical_tick
            && resident_state.get(&request.resident_id).is_some_and(
                |(logical_cell, target_cell)| {
                    *logical_cell == request.start && *target_cell == request.goal
                },
            )
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CivicIdentityDisposition {
    Ready,
    NeedsSeed,
    Blocked,
}

fn reconcile_civic_identity(
    population: &mut CivicPopulation,
    identity: WorldGenerationIdentity,
) -> (CivicIdentityDisposition, bool) {
    match population.generation_identity {
        Some(saved_identity) if saved_identity != identity => {
            let changed = population.authority != CivicAuthorityState::IdentityBlocked
                || population.last_failure != CivicFailureCode::GenerationIdentityMismatch;
            population.authority = CivicAuthorityState::IdentityBlocked;
            population.last_failure = CivicFailureCode::GenerationIdentityMismatch;
            (CivicIdentityDisposition::Blocked, changed)
        }
        Some(_) if population.residents.is_empty() => {
            let changed = population.authority != CivicAuthorityState::Uninitialized
                || population.last_failure == CivicFailureCode::GenerationIdentityMismatch;
            population.authority = CivicAuthorityState::Uninitialized;
            if population.last_failure == CivicFailureCode::GenerationIdentityMismatch {
                population.last_failure = CivicFailureCode::None;
            }
            (CivicIdentityDisposition::NeedsSeed, changed)
        }
        Some(_) => {
            let changed = population.authority != CivicAuthorityState::Active
                || population.last_failure == CivicFailureCode::GenerationIdentityMismatch;
            population.authority = CivicAuthorityState::Active;
            if population.last_failure == CivicFailureCode::GenerationIdentityMismatch {
                population.last_failure = CivicFailureCode::None;
            }
            (CivicIdentityDisposition::Ready, changed)
        }
        None if population.residents.is_empty() => (CivicIdentityDisposition::NeedsSeed, false),
        None => {
            population.generation_identity = Some(identity);
            population.authority = CivicAuthorityState::Active;
            population.last_failure = CivicFailureCode::None;
            (CivicIdentityDisposition::Ready, true)
        }
    }
}

#[derive(Resource, Default)]
struct CivicVisualCache {
    cube: Option<Handle<Mesh>>,
    sphere: Option<Handle<Mesh>>,
    cylinder: Option<Handle<Mesh>>,
    sole: Option<Handle<StandardMaterial>>,
    culture_materials: HashMap<CivicCulture, CivicMaterialSet>,
}

#[derive(Clone)]
struct CivicMaterialSet {
    fabric: Handle<StandardMaterial>,
    accent: Handle<StandardMaterial>,
    skin: Handle<StandardMaterial>,
    eye: Handle<StandardMaterial>,
}

fn stable_mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn scenery_tag(value: SceneryQuality) -> u64 {
    match value {
        SceneryQuality::Off => 0x11,
        SceneryQuality::Lean => 0x22,
        SceneryQuality::Balanced => 0x33,
        SceneryQuality::Lush => 0x44,
    }
}

fn grammar_tag(value: TerrainGrammarVersion) -> u64 {
    match value {
        TerrainGrammarVersion::V1 => 0xA1,
        TerrainGrammarVersion::V2 => 0xB2,
        TerrainGrammarVersion::V3 => 0xC3,
    }
}

fn profile_tag(value: WorldProfile) -> u64 {
    match value {
        WorldProfile::Natural => 0x4E41_5455_5241_4C01,
        WorldProfile::AstralFrontier => 0x4153_5452_414C_0002,
    }
}

fn stable_resident_id(
    identity: WorldGenerationIdentity,
    settlement_id: u64,
    ordinal: usize,
    collision_nonce: u64,
) -> u64 {
    let mut value = 0x564E_4349_5649_4301_u64;
    for part in [
        u64::from(identity.seed),
        profile_tag(identity.world_profile),
        scenery_tag(identity.scenery_quality),
        grammar_tag(identity.terrain_grammar),
        settlement_id,
        ordinal as u64,
        collision_nonce,
    ] {
        value = stable_mix64(value ^ stable_mix64(part));
    }
    (value | (1_u64 << 63)).max(1_u64 << 63)
}

fn stable_name(seed: u64, ordinal: usize) -> String {
    const FIRST: [&str; 16] = [
        "Ari", "Bel", "Cira", "Daro", "Eli", "Fara", "Iven", "Kea", "Lio", "Mira", "Neri", "Orin",
        "Pela", "Rian", "Sela", "Tavi",
    ];
    const SECOND: [&str; 16] = [
        "Aster", "Bran", "Cairn", "Dawn", "Esker", "Fern", "Glen", "Hearth", "Isle", "Juniper",
        "Kestrel", "Lumen", "Morrow", "Reed", "Vale", "Wren",
    ];
    let first = FIRST[(stable_mix64(seed ^ 0x4E41_4D45_0001) as usize) % FIRST.len()];
    let second = SECOND[(stable_mix64(seed ^ 0x4E41_4D45_0002) as usize) % SECOND.len()];
    format!("{first} {second} {:02}", ordinal + 1)
}

fn round_i32_saturating(value: f32) -> i32 {
    if !value.is_finite() {
        0
    } else if value >= i32::MAX as f32 {
        i32::MAX
    } else if value <= i32::MIN as f32 {
        i32::MIN
    } else {
        value.round() as i32
    }
}

fn surface_cell(world: &VoxelWorld, x: i32, z: i32) -> [i32; 3] {
    [x, world.surface_height_at(x, z).saturating_add(1), z]
}

fn offset_surface_cell(
    world: &VoxelWorld,
    center_x: i32,
    center_z: i32,
    offset: [i32; 2],
) -> [i32; 3] {
    let x = center_x.saturating_add(offset[0]);
    let z = center_z.saturating_add(offset[1]);
    surface_cell(world, x, z)
}

fn seed_civic_population(
    population: &mut CivicPopulation,
    settlement: &BotSettlement,
    identity: WorldGenerationIdentity,
    world: &VoxelWorld,
) {
    let hub_x = round_i32_saturating(settlement.hub[0]);
    let hub_z = round_i32_saturating(settlement.hub[2]);
    let hub = surface_cell(world, hub_x, hub_z);
    let mut claimed_ids = BTreeSet::new();
    let mut residents = Vec::with_capacity(CIVIC_SEED_POPULATION);

    for ordinal in 0..CIVIC_SEED_POPULATION {
        let mut collision_nonce = 0_u64;
        let id = loop {
            let candidate = stable_resident_id(identity, settlement.id, ordinal, collision_nonce);
            if claimed_ids.insert(candidate) {
                break candidate;
            }
            collision_nonce = collision_nonce.saturating_add(1);
        };
        let home = offset_surface_cell(world, hub_x, hub_z, HOME_OFFSETS[ordinal]);
        let calling = CivicCalling::ALL[ordinal % CivicCalling::ALL.len()];
        let work = offset_surface_cell(
            world,
            hub_x,
            hub_z,
            WORK_OFFSETS[(ordinal + calling as usize) % WORK_OFFSETS.len()],
        );
        let commons = offset_surface_cell(
            world,
            hub_x,
            hub_z,
            COMMONS_OFFSETS[ordinal % COMMONS_OFFSETS.len()],
        );
        let local_environment = world.environment_sample_at(home[0], home[2]);
        let local_biome = world.biome_at(home[0], home[2]);
        let culture =
            CivicCulture::from_environment(identity.world_profile, local_biome, local_environment);
        let stage = match ordinal {
            0 | 7 => CivicLifeStage::Youth,
            10 | 11 => CivicLifeStage::Elder,
            _ => CivicLifeStage::Adult,
        };
        let personality = stable_mix64(id ^ 0x5045_5253_4F4E_4101);
        let needs = CivicNeeds {
            energy: 180 + (personality & 0x7F) as u16,
            belonging: 210 + ((personality >> 8) & 0x9F) as u16,
            safety: 70 + ((personality >> 16) & 0x5F) as u16,
            purpose: 220 + ((personality >> 24) & 0xAF) as u16,
            curiosity: 240 + ((personality >> 32) & 0xBF) as u16,
        };
        residents.push(CivicResident {
            id,
            settlement_id: settlement.id,
            name: stable_name(id, ordinal),
            culture,
            calling,
            life_stage: stage,
            logical_cell: home,
            home_cell: home,
            work_cell: work,
            commons_cell: commons,
            shelter_cell: hub,
            target_cell: home,
            activity: CivicActivity::RestAtHome,
            committed_until_tick: 0,
            needs,
            memories: Vec::new(),
            relationships: Vec::new(),
            movement_progress_mm: 0,
            route_failures: 0,
            route_retry_after_tick: 0,
            deferred_activity: None,
            route_failure: CivicFailureCode::None,
            last_decision_tick: 0,
        });
    }

    population.schema_version = CIVIC_SCHEMA_VERSION;
    population.authority = CivicAuthorityState::Active;
    population.generation_identity = Some(identity);
    population.logical_tick = 0;
    population.residents = residents;
    population.blackboard = CivicBlackboard::default();
    population.last_failure = CivicFailureCode::None;
    population.normalize([settlement.id]);
}

fn ensure_civic_authority(
    active: Option<Res<ActiveWorld>>,
    world: Res<VoxelWorld>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
) {
    if !runtime.enabled {
        runtime.suspend_authority();
        return;
    }
    let Some(active) = active else {
        runtime.suspend_authority();
        return;
    };
    let identity = active.meta.generation_identity();
    let Some(settlement) = brain.save.settlements.first().cloned() else {
        runtime.suspend_authority();
        return;
    };

    let (disposition, identity_changed) =
        reconcile_civic_identity(&mut brain.save.civic_population, identity);
    if identity_changed {
        brain.mark_dirty();
    }
    match disposition {
        CivicIdentityDisposition::Blocked => {
            runtime.suspend_authority();
        }
        CivicIdentityDisposition::NeedsSeed => {
            seed_civic_population(
                &mut brain.save.civic_population,
                &settlement,
                identity,
                &world,
            );
            runtime.activate_authority(identity);
            brain.mark_dirty();
        }
        CivicIdentityDisposition::Ready => runtime.activate_authority(identity),
    }
}

fn refresh_civic_lod(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    brain: Res<FriendlyWorldBrain>,
    player_q: Query<&Transform, With<Player>>,
    mut runtime: ResMut<CivicRuntime>,
) {
    runtime.lod_timer -= time.delta_seconds().min(0.1);
    if runtime.lod_timer > 0.0 {
        return;
    }
    runtime.lod_timer = CIVIC_LOD_REFRESH_SECONDS;
    runtime.logical_active.clear();
    runtime.visual_selection.clear();
    if !runtime.authority_ready()
        || brain.save.civic_population.authority != CivicAuthorityState::Active
    {
        return;
    }

    let player = player_q
        .get_single()
        .ok()
        .map(|transform| transform.translation);
    let candidates = brain
        .save
        .civic_population
        .residents
        .iter()
        .filter_map(|resident| {
            let resolved = standable_cell(
                &world,
                resident.logical_cell[0],
                resident.logical_cell[2],
                resident.logical_cell[1],
            )
            .ok()?;
            if resolved != resident.logical_cell {
                return None;
            }
            let position = cell_vec3(resolved);
            Some((resident.id, position))
        })
        .collect::<Vec<_>>();
    let plan = plan_civic_lod(&candidates, player);
    runtime.logical_active = plan.logical_active;
    runtime.visual_selection = plan.visuals;
    let active = runtime
        .logical_active
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    runtime
        .path_queue
        .retain(|request| active.contains(&request.resident_id));
    runtime
        .paths
        .retain(|resident_id, _| active.contains(resident_id));
}

fn admit_civic_step_ticks(
    accumulator: &mut f32,
    frame_delta_seconds: f32,
    logical_tick: &mut u64,
    pending_step_ticks: &mut VecDeque<u64>,
) {
    pending_step_ticks.clear();
    *accumulator = (*accumulator + frame_delta_seconds.min(CIVIC_MAX_FRAME_DELTA_SECONDS))
        .min(CIVIC_FIXED_STEP_SECONDS * f32::from(CIVIC_MAX_CATCHUP_STEPS));
    while *accumulator >= CIVIC_FIXED_STEP_SECONDS
        && pending_step_ticks.len() < usize::from(CIVIC_MAX_CATCHUP_STEPS)
    {
        *accumulator -= CIVIC_FIXED_STEP_SECONDS;
        *logical_tick = logical_tick.saturating_add(1);
        pending_step_ticks.push_back(*logical_tick);
    }
}

fn tick_civic_clock(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
) {
    if !runtime.authority_ready()
        || brain.save.civic_population.authority != CivicAuthorityState::Active
        || runtime.logical_active.is_empty()
    {
        runtime.pending_step_ticks.clear();
        return;
    }

    let CivicRuntime {
        accumulator,
        pending_step_ticks,
        ..
    } = &mut *runtime;
    admit_civic_step_ticks(
        accumulator,
        time.delta_seconds(),
        &mut brain.save.civic_population.logical_tick,
        pending_step_ticks,
    );
    let mut step_ticks = [0_u64; CIVIC_MAX_CATCHUP_STEPS as usize];
    let step_count = pending_step_ticks.len();
    for (slot, logical_tick) in step_ticks
        .iter_mut()
        .zip(pending_step_ticks.iter().copied())
    {
        *slot = logical_tick;
    }
    let mut important_change = false;
    for tick in step_ticks.into_iter().take(step_count) {
        let minute = quantized_minute(settings.time_of_day);
        let precipitation = quantized_precipitation(&settings);
        let active_ids = runtime.logical_active.clone();
        let population = &mut brain.save.civic_population;
        let active_set = active_ids.iter().copied().collect::<HashSet<_>>();
        for resident in population
            .residents
            .iter_mut()
            .filter(|resident| active_set.contains(&resident.id))
        {
            advance_needs(resident, precipitation);
        }

        let decision_count = active_ids.len().min(CIVIC_DECISIONS_PER_TICK);
        for decision_offset in 0..decision_count {
            let active_index = (runtime.decision_cursor + decision_offset) % active_ids.len();
            let resident_id = active_ids[active_index];
            let Some(resident_index) = population
                .residents
                .iter()
                .position(|resident| resident.id == resident_id)
            else {
                continue;
            };
            let mut queue_budget_failed = false;
            {
                let resident = &mut population.residents[resident_index];
                if resident.activity == CivicActivity::WaitForCoverage
                    && tick < resident.route_retry_after_tick
                {
                    continue;
                }
                let selected = choose_activity(resident, minute, precipitation, tick);
                let target = target_for_activity(resident, selected, tick);
                let activity_changed = selected != resident.activity;
                let target_changed = target != resident.target_cell;
                if activity_changed || target_changed {
                    resident.activity = selected;
                    resident.deferred_activity = None;
                    resident.route_retry_after_tick = 0;
                    resident.target_cell = target;
                    resident.committed_until_tick = tick.saturating_add(commitment_ticks(selected));
                    resident.last_decision_tick = tick;
                    resident.movement_progress_mm = 0;
                    if resident.logical_cell == target {
                        clear_route_failure(resident);
                    } else {
                        let enqueued = runtime.enqueue_path(CivicPathRequest {
                            resident_id,
                            start: resident.logical_cell,
                            goal: target,
                            requested_tick: tick,
                        });
                        if !enqueued {
                            register_route_failure(
                                resident,
                                CivicFailureCode::PathBudgetExhausted,
                                tick,
                            );
                            queue_budget_failed = true;
                        }
                    }
                    important_change = true;
                } else if resident.logical_cell != resident.target_cell
                    && tick >= resident.route_retry_after_tick
                    && !runtime.paths.contains_key(&resident_id)
                    && !runtime
                        .path_queue
                        .iter()
                        .any(|request| request.resident_id == resident_id)
                {
                    let enqueued = runtime.enqueue_path(CivicPathRequest {
                        resident_id,
                        start: resident.logical_cell,
                        goal: resident.target_cell,
                        requested_tick: tick,
                    });
                    if !enqueued {
                        register_route_failure(
                            resident,
                            CivicFailureCode::PathBudgetExhausted,
                            tick,
                        );
                        queue_budget_failed = true;
                    }
                }
            }
            if queue_budget_failed {
                population.last_failure = CivicFailureCode::PathBudgetExhausted;
                runtime.path_budget_failures = runtime.path_budget_failures.saturating_add(1);
            }
        }
        runtime.decision_cursor = if active_ids.is_empty() {
            0
        } else {
            (runtime.decision_cursor + decision_count) % active_ids.len()
        };
        runtime.changed_since_checkpoint = true;
    }
    runtime.dirty_checkpoint += time.delta_seconds().min(0.1);
    if important_change
        || (runtime.changed_since_checkpoint
            && runtime.dirty_checkpoint >= CIVIC_POSITION_CHECKPOINT_SECONDS)
    {
        brain.mark_dirty();
        runtime.changed_since_checkpoint = false;
        runtime.dirty_checkpoint = 0.0;
    }
}

fn quantized_minute(time_of_day: f32) -> u16 {
    if !time_of_day.is_finite() {
        return 0;
    }
    let wrapped = time_of_day.rem_euclid(24.0);
    ((wrapped * 60.0).round() as u16).min(1_439)
}

fn quantized_precipitation(settings: &WorldSettings) -> u16 {
    let intensity = settings
        .weather
        .rain_intensity
        .max(settings.weather.snow_intensity)
        .clamp(0.0, 1.0);
    (intensity * 1_000.0).round() as u16
}

fn bounded_add(value: u16, delta: i16) -> u16 {
    if delta >= 0 {
        value.saturating_add(delta as u16).min(1_000)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

fn advance_needs(resident: &mut CivicResident, precipitation: u16) {
    let resting = matches!(
        resident.activity,
        CivicActivity::RestAtHome | CivicActivity::Recover
    );
    let social = matches!(
        resident.activity,
        CivicActivity::Socialize | CivicActivity::ShareKnowledge | CivicActivity::Play
    );
    let purposeful = matches!(
        resident.activity,
        CivicActivity::Work | CivicActivity::InspectSettlement
    );
    let exploring = matches!(
        resident.activity,
        CivicActivity::WanderLocally | CivicActivity::InspectSettlement | CivicActivity::Play
    );
    let sheltered = matches!(
        resident.activity,
        CivicActivity::RestAtHome | CivicActivity::SeekShelter | CivicActivity::Recover
    );
    resident.needs.energy = bounded_add(resident.needs.energy, if resting { -12 } else { 3 });
    resident.needs.belonging = bounded_add(resident.needs.belonging, if social { -10 } else { 2 });
    resident.needs.purpose = bounded_add(resident.needs.purpose, if purposeful { -9 } else { 2 });
    resident.needs.curiosity =
        bounded_add(resident.needs.curiosity, if exploring { -8 } else { 1 });
    let weather_pressure = if precipitation >= 550 && !sheltered {
        7
    } else {
        -5
    };
    resident.needs.safety = bounded_add(resident.needs.safety, weather_pressure);
}

fn schedule_compatibility(activity: CivicActivity, life_stage: CivicLifeStage, minute: u16) -> i32 {
    let night = !(360..1_200).contains(&minute);
    let dawn = (360..450).contains(&minute);
    let work_window = (450..720).contains(&minute) || (840..1_080).contains(&minute);
    let commons_window = (720..840).contains(&minute) || (1_080..1_200).contains(&minute);
    match activity {
        CivicActivity::RestAtHome if night => 1_200,
        CivicActivity::Prepare if dawn => 850,
        CivicActivity::Work if work_window && life_stage == CivicLifeStage::Adult => 1_000,
        CivicActivity::Socialize if commons_window => 900,
        CivicActivity::ShareKnowledge if commons_window && life_stage == CivicLifeStage::Elder => {
            1_050
        }
        CivicActivity::Play if !night && life_stage == CivicLifeStage::Youth => 1_000,
        CivicActivity::InspectSettlement if work_window => 600,
        CivicActivity::WanderLocally if !night => 420,
        CivicActivity::Recover if night => 500,
        _ => 0,
    }
}

fn activity_target(resident: &CivicResident, activity: CivicActivity) -> [i32; 3] {
    match activity {
        CivicActivity::RestAtHome | CivicActivity::Prepare | CivicActivity::Recover => {
            resident.home_cell
        }
        CivicActivity::Work | CivicActivity::InspectSettlement => resident.work_cell,
        CivicActivity::Socialize | CivicActivity::ShareKnowledge | CivicActivity::Play => {
            resident.commons_cell
        }
        CivicActivity::SeekShelter => resident.shelter_cell,
        CivicActivity::WanderLocally | CivicActivity::WaitForCoverage => resident.target_cell,
    }
}

fn manhattan_xz(left: [i32; 3], right: [i32; 3]) -> i64 {
    (i64::from(left[0]) - i64::from(right[0])).abs()
        + (i64::from(left[2]) - i64::from(right[2])).abs()
}

fn activity_utility(
    resident: &CivicResident,
    activity: CivicActivity,
    minute: u16,
    precipitation: u16,
) -> i64 {
    let mut utility = i64::from(schedule_compatibility(
        activity,
        resident.life_stage,
        minute,
    )) * 1_000;
    utility = utility.saturating_add(match activity {
        CivicActivity::RestAtHome | CivicActivity::Recover => {
            i64::from(resident.needs.energy) * 1_900 + i64::from(resident.needs.safety) * 500
        }
        CivicActivity::Prepare => i64::from(resident.needs.purpose) * 500,
        CivicActivity::Work => i64::from(resident.needs.purpose) * 1_750,
        CivicActivity::Socialize => i64::from(resident.needs.belonging) * 1_850,
        CivicActivity::ShareKnowledge => {
            i64::from(resident.needs.belonging) * 1_100 + i64::from(resident.needs.purpose) * 850
        }
        CivicActivity::Play => {
            i64::from(resident.needs.curiosity) * 1_500 + i64::from(resident.needs.belonging) * 750
        }
        CivicActivity::InspectSettlement | CivicActivity::WanderLocally => {
            i64::from(resident.needs.curiosity) * 1_500 + i64::from(resident.needs.purpose) * 400
        }
        CivicActivity::SeekShelter => {
            i64::from(resident.needs.safety) * 2_200 + i64::from(precipitation) * 2_400
        }
        CivicActivity::WaitForCoverage => 0,
    });
    if activity == resident.activity {
        utility = utility.saturating_add(256_000);
    }
    let target = activity_target(resident, activity);
    utility = utility.saturating_sub(manhattan_xz(resident.logical_cell, target) * 8_000);
    let outdoor = matches!(
        activity,
        CivicActivity::Work
            | CivicActivity::Socialize
            | CivicActivity::ShareKnowledge
            | CivicActivity::Play
            | CivicActivity::InspectSettlement
            | CivicActivity::WanderLocally
    );
    if outdoor {
        utility = utility.saturating_sub(i64::from(precipitation) * 1_250);
    }
    utility
}

fn choose_activity(
    resident: &CivicResident,
    minute: u16,
    precipitation: u16,
    logical_tick: u64,
) -> CivicActivity {
    if precipitation >= 700 && resident.activity != CivicActivity::SeekShelter {
        return CivicActivity::SeekShelter;
    }
    if logical_tick < resident.committed_until_tick
        && resident.activity != CivicActivity::WaitForCoverage
    {
        return resident.activity;
    }
    CivicActivity::DECISION_SET
        .into_iter()
        .max_by(|left, right| {
            activity_utility(resident, *left, minute, precipitation)
                .cmp(&activity_utility(resident, *right, minute, precipitation))
                .then_with(|| right.stable_tag().cmp(&left.stable_tag()))
        })
        .unwrap_or(CivicActivity::RestAtHome)
}

fn target_for_activity(
    resident: &CivicResident,
    activity: CivicActivity,
    logical_tick: u64,
) -> [i32; 3] {
    if activity != CivicActivity::WanderLocally {
        return activity_target(resident, activity);
    }
    let phase = stable_mix64(resident.id ^ (logical_tick / 25));
    let dx = ((phase & 0x7) as i32).saturating_sub(3);
    let dz = (((phase >> 3) & 0x7) as i32).saturating_sub(3);
    [
        resident.commons_cell[0].saturating_add(dx),
        resident.commons_cell[1],
        resident.commons_cell[2].saturating_add(dz),
    ]
}

const fn commitment_ticks(activity: CivicActivity) -> u64 {
    match activity {
        CivicActivity::RestAtHome => 35,
        CivicActivity::Prepare => 15,
        CivicActivity::Work => 30,
        CivicActivity::Socialize | CivicActivity::ShareKnowledge => 24,
        CivicActivity::Play => 20,
        CivicActivity::InspectSettlement | CivicActivity::WanderLocally => 18,
        CivicActivity::SeekShelter => 30,
        CivicActivity::Recover => 20,
        CivicActivity::WaitForCoverage => 5,
    }
}

fn route_retry_delay(failure: CivicFailureCode, route_failures: u8) -> u64 {
    let shift = route_failures
        .saturating_sub(1)
        .min(CIVIC_ROUTE_RETRY_MAX_SHIFT as u8) as u32;
    let base = match failure {
        CivicFailureCode::CoverageUnresolved => CIVIC_COVERAGE_RETRY_BASE_TICKS,
        CivicFailureCode::PathBudgetExhausted | CivicFailureCode::NoRoute => {
            CIVIC_ROUTE_RETRY_BASE_TICKS
        }
        _ => 0,
    };
    base.saturating_mul(1_u64 << shift)
}

fn register_route_failure(
    resident: &mut CivicResident,
    failure: CivicFailureCode,
    logical_tick: u64,
) {
    let intended_activity = if resident.activity == CivicActivity::WaitForCoverage {
        resident.deferred_activity
    } else {
        Some(resident.activity)
    };
    resident.route_failures = resident.route_failures.saturating_add(1).min(15);
    resident.route_failure = failure;
    resident.route_retry_after_tick =
        logical_tick.saturating_add(route_retry_delay(failure, resident.route_failures));
    resident.deferred_activity =
        intended_activity.filter(|activity| *activity != CivicActivity::WaitForCoverage);
    resident.activity = CivicActivity::WaitForCoverage;
    resident.committed_until_tick = resident.route_retry_after_tick;
    resident.movement_progress_mm = 0;
}

fn clear_route_failure(resident: &mut CivicResident) {
    resident.route_failures = 0;
    resident.route_retry_after_tick = 0;
    resident.deferred_activity = None;
    resident.route_failure = CivicFailureCode::None;
}

fn cell_vec3(cell: [i32; 3]) -> Vec3 {
    Vec3::new(cell[0] as f32 + 0.5, cell[1] as f32, cell[2] as f32 + 0.5)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PathOpenNode {
    cell: [i32; 3],
    g: u32,
    h: u32,
    f: u32,
}

impl Ord for PathOpenNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f
            .cmp(&self.f)
            .then_with(|| other.h.cmp(&self.h))
            .then_with(|| other.cell[2].cmp(&self.cell[2]))
            .then_with(|| other.cell[0].cmp(&self.cell[0]))
            .then_with(|| other.cell[1].cmp(&self.cell[1]))
    }
}

impl PartialOrd for PathOpenNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CivicPathFailure {
    CoverageUnresolved,
    NoRoute,
    BudgetExhausted,
    PathTooLong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CivicPathSolution {
    resolved_start: [i32; 3],
    resolved_goal: [i32; 3],
    cells: Vec<[i32; 3]>,
    expansions: usize,
}

fn exact_block(world: &VoxelWorld, x: i32, y: i32, z: i32) -> Option<BlockType> {
    world
        .voxel_at_if_resolved(x, y, z)
        .map(BlockType::from_voxel)
}

fn standable_cell(
    world: &VoxelWorld,
    x: i32,
    z: i32,
    root_y_hint: i32,
) -> Result<[i32; 3], CivicPathFailure> {
    const HEIGHT_PROBES: [i32; 9] = [0, 1, -1, 2, -2, 3, -3, 4, -4];
    let mut unresolved = false;
    for delta_y in HEIGHT_PROBES {
        let Some(root_y) = root_y_hint.checked_add(delta_y) else {
            continue;
        };
        let Some(support_y) = root_y.checked_sub(1) else {
            continue;
        };
        let Some(head_y) = root_y.checked_add(1) else {
            continue;
        };
        let support = exact_block(world, x, support_y, z);
        let feet = exact_block(world, x, root_y, z);
        let head = exact_block(world, x, head_y, z);
        if support.is_none() || feet.is_none() || head.is_none() {
            unresolved = true;
            continue;
        }
        let support = support.expect("checked above");
        let feet = feet.expect("checked above");
        let head = head.expect("checked above");
        if support.is_solid() && feet == BlockType::Air && head == BlockType::Air {
            return Ok([x, root_y, z]);
        }
    }
    if unresolved {
        Err(CivicPathFailure::CoverageUnresolved)
    } else {
        Err(CivicPathFailure::NoRoute)
    }
}

fn nearest_standable_cell(
    world: &VoxelWorld,
    requested: [i32; 3],
) -> Result<[i32; 3], CivicPathFailure> {
    let mut saw_unresolved = false;
    for [dx, dz] in SAFE_CELL_OFFSETS {
        let (Some(x), Some(z)) = (requested[0].checked_add(dx), requested[2].checked_add(dz))
        else {
            continue;
        };
        match standable_cell(world, x, z, requested[1]) {
            Ok(cell) => return Ok(cell),
            Err(CivicPathFailure::CoverageUnresolved) => saw_unresolved = true,
            Err(_) => {}
        }
    }
    if saw_unresolved {
        Err(CivicPathFailure::CoverageUnresolved)
    } else {
        Err(CivicPathFailure::NoRoute)
    }
}

fn reconcile_resident_cell(resident: &mut CivicResident, resolved: [i32; 3]) -> bool {
    let old = resident.logical_cell;
    if resolved == old {
        return false;
    }
    resident.logical_cell = resolved;
    resident.movement_progress_mm = 0;
    if resident.home_cell == old {
        resident.home_cell = resolved;
    }
    if resident.work_cell == old {
        resident.work_cell = resolved;
    }
    if resident.commons_cell == old {
        resident.commons_cell = resolved;
    }
    if resident.shelter_cell == old {
        resident.shelter_cell = resolved;
    }
    if resident.target_cell == old {
        resident.target_cell = resolved;
    }
    true
}

fn reconcile_civic_cells(
    world: Res<VoxelWorld>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
) {
    if !runtime.authority_ready()
        || brain.save.civic_population.authority != CivicAuthorityState::Active
        || brain.save.civic_population.residents.is_empty()
    {
        return;
    }
    let len = brain.save.civic_population.residents.len();
    let count = len.min(CIVIC_RECONCILE_LIMIT);
    let indices = (0..count)
        .map(|offset| (runtime.reconcile_cursor + offset) % len)
        .collect::<Vec<_>>();
    runtime.reconcile_cursor = (runtime.reconcile_cursor + count) % len;
    let mut changed = false;
    for index in indices {
        let resident = &mut brain.save.civic_population.residents[index];
        let old = resident.logical_cell;
        let Ok(resolved) = nearest_standable_cell(&world, old) else {
            continue;
        };
        if resolved == old {
            continue;
        }
        let _ = reconcile_resident_cell(resident, resolved);
        runtime.paths.remove(&resident.id);
        runtime
            .path_queue
            .retain(|request| request.resident_id != resident.id);
        changed = true;
    }
    if changed {
        runtime.changed_since_checkpoint = true;
        brain.mark_dirty();
    }
}

fn path_heuristic(cell: [i32; 3], goal: [i32; 3]) -> u32 {
    let distance = (i64::from(cell[0]) - i64::from(goal[0])).abs()
        + (i64::from(cell[2]) - i64::from(goal[2])).abs();
    distance.saturating_mul(10).min(i64::from(u32::MAX)) as u32
}

fn inside_path_radius(start: [i32; 3], candidate: [i32; 3]) -> bool {
    (i64::from(start[0]) - i64::from(candidate[0])).abs() <= i64::from(CIVIC_MAX_ROUTE_RADIUS)
        && (i64::from(start[2]) - i64::from(candidate[2])).abs()
            <= i64::from(CIVIC_MAX_ROUTE_RADIUS)
}

fn checked_neighbor_column(cell: [i32; 3], offset: [i32; 2]) -> Option<(i32, i32)> {
    Some((
        cell[0].checked_add(offset[0])?,
        cell[2].checked_add(offset[1])?,
    ))
}

fn reconstruct_path(
    start: [i32; 3],
    goal: [i32; 3],
    parents: &BTreeMap<[i32; 3], [i32; 3]>,
) -> Result<Vec<[i32; 3]>, CivicPathFailure> {
    let mut reverse = Vec::new();
    let mut current = goal;
    while current != start {
        if reverse.len() >= CIVIC_PATH_CELL_LIMIT {
            return Err(CivicPathFailure::PathTooLong);
        }
        reverse.push(current);
        current = *parents.get(&current).ok_or(CivicPathFailure::NoRoute)?;
    }
    reverse.reverse();
    Ok(reverse)
}

fn solve_loaded_path(
    world: &VoxelWorld,
    requested_start: [i32; 3],
    requested_goal: [i32; 3],
) -> Result<CivicPathSolution, CivicPathFailure> {
    let start = standable_cell(
        world,
        requested_start[0],
        requested_start[2],
        requested_start[1],
    )?;
    let goal = standable_cell(
        world,
        requested_goal[0],
        requested_goal[2],
        requested_goal[1],
    )?;
    if start == goal {
        return Ok(CivicPathSolution {
            resolved_start: start,
            resolved_goal: goal,
            cells: Vec::new(),
            expansions: 0,
        });
    }
    if !inside_path_radius(start, goal) {
        return Err(CivicPathFailure::NoRoute);
    }

    let start_h = path_heuristic(start, goal);
    let mut open = BinaryHeap::new();
    open.push(PathOpenNode {
        cell: start,
        g: 0,
        h: start_h,
        f: start_h,
    });
    let mut best_cost = BTreeMap::from([(start, 0_u32)]);
    let mut parents = BTreeMap::new();
    let mut expansions = 0_usize;
    let mut saw_unresolved = false;

    while let Some(node) = open.pop() {
        if best_cost.get(&node.cell).copied() != Some(node.g) {
            continue;
        }
        if expansions >= CIVIC_PATH_EXPANSION_LIMIT {
            return Err(CivicPathFailure::BudgetExhausted);
        }
        expansions += 1;
        if node.cell == goal {
            return Ok(CivicPathSolution {
                resolved_start: start,
                resolved_goal: goal,
                cells: reconstruct_path(start, goal, &parents)?,
                expansions,
            });
        }

        for [dx, dz] in NAV_OFFSETS {
            let Some((x, z)) = checked_neighbor_column(node.cell, [dx, dz]) else {
                continue;
            };
            let candidate = match standable_cell(world, x, z, node.cell[1]) {
                Ok(cell) => cell,
                Err(CivicPathFailure::CoverageUnresolved) => {
                    saw_unresolved = true;
                    continue;
                }
                Err(_) => continue,
            };
            if !inside_path_radius(start, candidate) || (candidate[1] - node.cell[1]).abs() > 1 {
                continue;
            }
            let vertical_cost = (candidate[1] - node.cell[1])
                .unsigned_abs()
                .saturating_mul(4);
            let candidate_g = node.g.saturating_add(10).saturating_add(vertical_cost);
            if best_cost
                .get(&candidate)
                .is_some_and(|known| *known <= candidate_g)
            {
                continue;
            }
            best_cost.insert(candidate, candidate_g);
            parents.insert(candidate, node.cell);
            let h = path_heuristic(candidate, goal);
            open.push(PathOpenNode {
                cell: candidate,
                g: candidate_g,
                h,
                f: candidate_g.saturating_add(h),
            });
        }
    }
    if saw_unresolved {
        Err(CivicPathFailure::CoverageUnresolved)
    } else {
        Err(CivicPathFailure::NoRoute)
    }
}

fn record_civic_route_failure(
    population: &mut CivicPopulation,
    resident_id: u64,
    failure: CivicFailureCode,
) -> bool {
    let logical_tick = population.logical_tick;
    let Some(resident) = population
        .residents
        .iter_mut()
        .find(|resident| resident.id == resident_id)
    else {
        return false;
    };
    register_route_failure(resident, failure, logical_tick);
    population.last_failure = failure;
    true
}

fn service_civic_path_queue(
    world: Res<VoxelWorld>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
) {
    runtime.path_expansions_last = 0;
    if !runtime.authority_ready()
        || brain.save.civic_population.authority != CivicAuthorityState::Active
    {
        return;
    }
    let resident_state = brain
        .save
        .civic_population
        .residents
        .iter()
        .map(|resident| (resident.id, (resident.logical_cell, resident.target_cell)))
        .collect::<BTreeMap<_, _>>();
    let active_residents = runtime
        .logical_active
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    prune_civic_path_queue(
        &mut runtime.path_queue,
        &resident_state,
        &active_residents,
        brain.save.civic_population.logical_tick,
    );
    let Some(request) = runtime.path_queue.pop_front() else {
        return;
    };
    let Some((resident_id, logical_cell, target_cell, activity)) = brain
        .save
        .civic_population
        .residents
        .iter()
        .find(|resident| resident.id == request.resident_id)
        .map(|resident| {
            (
                resident.id,
                resident.logical_cell,
                resident.target_cell,
                resident.activity,
            )
        })
    else {
        return;
    };
    if logical_cell != request.start
        || target_cell != request.goal
        || request.requested_tick > brain.save.civic_population.logical_tick
    {
        return;
    }

    let resolve_endpoint = |requested| match nearest_standable_cell(&world, requested) {
        Ok(cell) => Ok(cell),
        Err(CivicPathFailure::CoverageUnresolved) => Err(CivicFailureCode::CoverageUnresolved),
        Err(_) => Err(CivicFailureCode::NoRoute),
    };
    let resolved_start = match resolve_endpoint(logical_cell) {
        Ok(start) => start,
        Err(failure) => {
            if record_civic_route_failure(&mut brain.save.civic_population, resident_id, failure) {
                runtime.changed_since_checkpoint = true;
                brain.mark_dirty();
            }
            return;
        }
    };
    let resolved_goal = match resolve_endpoint(target_cell) {
        Ok(goal) => goal,
        Err(failure) => {
            if record_civic_route_failure(&mut brain.save.civic_population, resident_id, failure) {
                runtime.changed_since_checkpoint = true;
                brain.mark_dirty();
            }
            return;
        }
    };

    match solve_loaded_path(&world, resolved_start, resolved_goal) {
        Ok(solution) => {
            runtime.path_expansions_last = solution.expansions;
            runtime.path_expansions_peak = runtime
                .path_expansions_peak
                .max(solution.expansions)
                .min(CIVIC_PATH_EXPANSION_LIMIT);
            let mut persistent_change = false;
            if let Some(resident) = brain
                .save
                .civic_population
                .residents
                .iter_mut()
                .find(|resident| resident.id == resident_id)
            {
                persistent_change |= reconcile_resident_cell(resident, solution.resolved_start);
                if resident.target_cell != solution.resolved_goal {
                    resident.target_cell = solution.resolved_goal;
                    match activity {
                        CivicActivity::RestAtHome
                        | CivicActivity::Prepare
                        | CivicActivity::Recover => resident.home_cell = solution.resolved_goal,
                        CivicActivity::Work | CivicActivity::InspectSettlement => {
                            resident.work_cell = solution.resolved_goal
                        }
                        CivicActivity::Socialize
                        | CivicActivity::ShareKnowledge
                        | CivicActivity::Play => resident.commons_cell = solution.resolved_goal,
                        CivicActivity::SeekShelter => {
                            resident.shelter_cell = solution.resolved_goal
                        }
                        CivicActivity::WanderLocally | CivicActivity::WaitForCoverage => {}
                    }
                    persistent_change = true;
                }
                persistent_change |= resident.route_failures != 0
                    || resident.route_retry_after_tick != 0
                    || resident.deferred_activity.is_some()
                    || resident.route_failure != CivicFailureCode::None;
                clear_route_failure(resident);
            }
            if persistent_change {
                runtime.changed_since_checkpoint = true;
            }
            let _ = install_civic_path(
                &mut runtime.paths,
                resident_id,
                solution.cells,
                solution.resolved_goal,
            );
            brain.save.civic_population.last_failure = CivicFailureCode::None;
            if persistent_change {
                brain.mark_dirty();
            }
        }
        Err(CivicPathFailure::CoverageUnresolved) => {
            if record_civic_route_failure(
                &mut brain.save.civic_population,
                resident_id,
                CivicFailureCode::CoverageUnresolved,
            ) {
                runtime.changed_since_checkpoint = true;
                brain.mark_dirty();
            }
        }
        Err(CivicPathFailure::BudgetExhausted | CivicPathFailure::PathTooLong) => {
            runtime.path_budget_failures = runtime.path_budget_failures.saturating_add(1);
            if record_civic_route_failure(
                &mut brain.save.civic_population,
                resident_id,
                CivicFailureCode::PathBudgetExhausted,
            ) {
                runtime.changed_since_checkpoint = true;
                brain.mark_dirty();
            }
        }
        Err(CivicPathFailure::NoRoute) => {
            if record_civic_route_failure(
                &mut brain.save.civic_population,
                resident_id,
                CivicFailureCode::NoRoute,
            ) {
                runtime.changed_since_checkpoint = true;
                brain.mark_dirty();
            }
        }
    }
}

fn add_memory(resident: &mut CivicResident, kind: CivicMemoryKind, logical_tick: u64) {
    resident.memories.push(CivicMemory {
        kind,
        logical_tick,
        cell: resident.logical_cell,
        confidence: 1_000,
    });
    resident
        .memories
        .sort_by_key(|memory| std::cmp::Reverse(memory.logical_tick));
    resident.memories.truncate(CIVIC_MEMORY_HARD_LIMIT);
}

fn update_relationship(resident: &mut CivicResident, other_id: u64, logical_tick: u64) {
    if let Some(relationship) = resident
        .relationships
        .iter_mut()
        .find(|relationship| relationship.resident_id == other_id)
    {
        relationship.familiarity = relationship.familiarity.saturating_add(18).min(1_000);
        relationship.trust = relationship.trust.saturating_add(8).min(1_000);
        relationship.last_shared_tick = logical_tick;
    } else {
        if resident.relationships.len() >= CIVIC_RELATIONSHIP_HARD_LIMIT {
            resident.relationships.sort_by_key(|relationship| {
                (
                    relationship.familiarity,
                    relationship.last_shared_tick,
                    relationship.resident_id,
                )
            });
            resident.relationships.remove(0);
        }
        resident.relationships.push(CivicRelationship {
            resident_id: other_id,
            familiarity: 120,
            trust: 100,
            last_shared_tick: logical_tick,
        });
    }
    resident.relationships.sort_by_key(|relationship| {
        (
            std::cmp::Reverse(relationship.familiarity),
            relationship.resident_id,
        )
    });
}

fn social_pair_plan(mut resident_ids: Vec<u64>, round: u64) -> Vec<(u64, u64)> {
    resident_ids.sort_unstable();
    resident_ids.dedup();
    if resident_ids.len() < 2 {
        return Vec::new();
    }
    let rotation = ((round as usize).saturating_mul(8)) % resident_ids.len();
    resident_ids.rotate_left(rotation);
    resident_ids
        .chunks_exact(2)
        .take(4)
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

fn update_social_pairs(population: &mut CivicPopulation, logical_tick: u64) -> bool {
    if !logical_tick.is_multiple_of(10) {
        return false;
    }
    let social_indices = population
        .residents
        .iter()
        .enumerate()
        .filter(|(_, resident)| {
            matches!(
                resident.activity,
                CivicActivity::Socialize | CivicActivity::ShareKnowledge | CivicActivity::Play
            ) && manhattan_xz(resident.logical_cell, resident.commons_cell) <= 1
        })
        .map(|(index, resident)| (resident.id, index))
        .collect::<BTreeMap<_, _>>();
    let pair_plan = social_pair_plan(social_indices.keys().copied().collect(), logical_tick / 10);
    let mut changed = false;
    for (left_id, right_id) in pair_plan {
        let left_index = social_indices[&left_id];
        let right_index = social_indices[&right_id];
        let (left, right) = if left_index < right_index {
            let (before_right, at_right) = population.residents.split_at_mut(right_index);
            (&mut before_right[left_index], &mut at_right[0])
        } else {
            let (before_left, at_left) = population.residents.split_at_mut(left_index);
            (&mut at_left[0], &mut before_left[right_index])
        };
        update_relationship(left, right_id, logical_tick);
        update_relationship(right, left_id, logical_tick);
        add_memory(left, CivicMemoryKind::KnowledgeShared, logical_tick);
        add_memory(right, CivicMemoryKind::KnowledgeShared, logical_tick);
        population.blackboard.successful_gatherings = population
            .blackboard
            .successful_gatherings
            .saturating_add(1);
        changed = true;
    }
    changed
}

fn advance_civic_residents(
    world: Res<VoxelWorld>,
    mut brain: ResMut<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
) {
    if runtime.pending_step_ticks.is_empty()
        || !runtime.authority_ready()
        || brain.save.civic_population.authority != CivicAuthorityState::Active
    {
        return;
    }
    let active = runtime
        .logical_active
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut blocked_events = Vec::new();
    let mut arrived_events = Vec::new();
    let mut movement_changed = false;
    while let Some(logical_tick) = runtime.pending_step_ticks.pop_front() {
        for resident in brain
            .save
            .civic_population
            .residents
            .iter_mut()
            .filter(|resident| active.contains(&resident.id))
        {
            let Some(path) = runtime.paths.get_mut(&resident.id) else {
                continue;
            };
            if path.goal != resident.target_cell {
                runtime.paths.remove(&resident.id);
                continue;
            }
            resident.movement_progress_mm = resident.movement_progress_mm.saturating_add(
                (u32::from(CIVIC_WALK_SPEED_MM_PER_SECOND)
                    * (CIVIC_FIXED_STEP_SECONDS * 1_000.0) as u32
                    / 1_000) as u16,
            );
            if resident.movement_progress_mm < 1_000 {
                continue;
            }
            let Some(next) = path.cells.front().copied() else {
                resident.movement_progress_mm = 0;
                continue;
            };
            match standable_cell(&world, next[0], next[2], next[1]) {
                Ok(resolved) if resolved == next => {
                    path.cells.pop_front();
                    resident.logical_cell = next;
                    resident.movement_progress_mm -= 1_000;
                    clear_route_failure(resident);
                    movement_changed = true;
                    if path.cells.is_empty() && resident.logical_cell == resident.target_cell {
                        arrived_events.push((resident.id, resident.activity, logical_tick));
                    }
                }
                _ => {
                    resident.movement_progress_mm = 0;
                    blocked_events.push((resident.id, resident.logical_cell, logical_tick));
                }
            }
        }
        for (resident_id, _, _) in &blocked_events {
            runtime.paths.remove(resident_id);
        }
        if update_social_pairs(&mut brain.save.civic_population, logical_tick) {
            movement_changed = true;
        }
    }

    if movement_changed {
        runtime.changed_since_checkpoint = true;
    }

    if !blocked_events.is_empty() || !arrived_events.is_empty() {
        let had_blocked_events = !blocked_events.is_empty();
        let population = &mut brain.save.civic_population;
        for (resident_id, cell, logical_tick) in blocked_events {
            if let Some(resident) = population
                .residents
                .iter_mut()
                .find(|resident| resident.id == resident_id)
            {
                register_route_failure(resident, CivicFailureCode::NoRoute, logical_tick);
                add_memory(resident, CivicMemoryKind::RouteBlocked, logical_tick);
            }
            population.blackboard.blocked_routes =
                population.blackboard.blocked_routes.saturating_add(1);
            population.blackboard.notices.push(CivicNotice {
                logical_tick,
                code: CivicMemoryKind::RouteBlocked,
                cell,
            });
        }
        if had_blocked_events {
            population.last_failure = CivicFailureCode::NoRoute;
        }
        for (resident_id, activity, logical_tick) in arrived_events {
            if let Some(resident) = population
                .residents
                .iter_mut()
                .find(|resident| resident.id == resident_id)
            {
                let memory_kind = match activity {
                    CivicActivity::SeekShelter => CivicMemoryKind::ShelterReached,
                    CivicActivity::Socialize
                    | CivicActivity::ShareKnowledge
                    | CivicActivity::Play => CivicMemoryKind::CommunityGathering,
                    CivicActivity::Work | CivicActivity::InspectSettlement => {
                        CivicMemoryKind::StewardshipCompleted
                    }
                    _ => continue,
                };
                add_memory(resident, memory_kind, logical_tick);
            }
        }
        population
            .blackboard
            .notices
            .sort_by_key(|notice| std::cmp::Reverse(notice.logical_tick));
        population
            .blackboard
            .notices
            .truncate(CIVIC_NOTICE_HARD_LIMIT);
        runtime.changed_since_checkpoint = true;
        brain.mark_dirty();
    }
}

fn culture_palette(culture: CivicCulture) -> ([f32; 3], [f32; 3], [f32; 3], [f32; 3], f32) {
    match culture {
        CivicCulture::Riverglass => (
            [0.18, 0.34, 0.38],
            [0.72, 0.55, 0.25],
            [0.58, 0.38, 0.27],
            [0.22, 0.92, 0.84],
            0.66,
        ),
        CivicCulture::Canopy => (
            [0.20, 0.33, 0.22],
            [0.72, 0.45, 0.20],
            [0.50, 0.31, 0.22],
            [0.55, 0.94, 0.42],
            0.74,
        ),
        CivicCulture::Sunstone => (
            [0.46, 0.25, 0.16],
            [0.87, 0.64, 0.28],
            [0.63, 0.39, 0.25],
            [1.00, 0.70, 0.30],
            0.78,
        ),
        CivicCulture::Highland => (
            [0.27, 0.29, 0.34],
            [0.65, 0.48, 0.23],
            [0.55, 0.36, 0.27],
            [0.46, 0.80, 1.00],
            0.82,
        ),
        CivicCulture::Frostweave => (
            [0.50, 0.60, 0.64],
            [0.26, 0.47, 0.62],
            [0.64, 0.45, 0.38],
            [0.62, 0.92, 1.00],
            0.70,
        ),
        CivicCulture::Astral => (
            [0.20, 0.16, 0.29],
            [0.55, 0.30, 0.72],
            [0.47, 0.32, 0.38],
            [0.72, 0.40, 1.00],
            0.58,
        ),
    }
}

fn civic_material_set(
    culture: CivicCulture,
    cache: &mut CivicVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> CivicMaterialSet {
    if let Some(existing) = cache.culture_materials.get(&culture) {
        return existing.clone();
    }
    debug_assert!(cache.culture_materials.len() < CivicCulture::ALL.len());
    let (fabric, accent, skin, eye, roughness) = culture_palette(culture);
    let set = CivicMaterialSet {
        fabric: materials.add(StandardMaterial {
            base_color: Color::srgb(fabric[0], fabric[1], fabric[2]),
            metallic: 0.02,
            perceptual_roughness: roughness,
            reflectance: 0.24,
            ..default()
        }),
        accent: materials.add(StandardMaterial {
            base_color: Color::srgb(accent[0], accent[1], accent[2]),
            metallic: if culture == CivicCulture::Astral {
                0.34
            } else {
                0.08
            },
            perceptual_roughness: (roughness - 0.12).max(0.28),
            ..default()
        }),
        skin: materials.add(StandardMaterial {
            base_color: Color::srgb(skin[0], skin[1], skin[2]),
            metallic: 0.0,
            perceptual_roughness: 0.88,
            reflectance: 0.18,
            ..default()
        }),
        eye: materials.add(StandardMaterial {
            base_color: Color::srgb(eye[0] * 0.35, eye[1] * 0.35, eye[2] * 0.35),
            emissive: LinearRgba::rgb(eye[0] * 2.2, eye[1] * 2.2, eye[2] * 2.2),
            metallic: 0.12,
            perceptual_roughness: 0.22,
            ..default()
        }),
    };
    cache.culture_materials.insert(culture, set.clone());
    set
}

fn shared_civic_meshes(
    cache: &mut CivicVisualCache,
    meshes: &mut Assets<Mesh>,
) -> (Handle<Mesh>, Handle<Mesh>, Handle<Mesh>) {
    let cube = cache
        .cube
        .get_or_insert_with(|| meshes.add(Cuboid::new(1.0, 1.0, 1.0)))
        .clone();
    let sphere = cache
        .sphere
        .get_or_insert_with(|| meshes.add(Sphere::new(0.5)))
        .clone();
    let cylinder = cache
        .cylinder
        .get_or_insert_with(|| meshes.add(Cylinder::new(0.5, 1.0)))
        .clone();
    (cube, sphere, cylinder)
}

fn sole_material(
    cache: &mut CivicVisualCache,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    cache
        .sole
        .get_or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: Color::srgb(0.055, 0.060, 0.064),
                metallic: 0.04,
                perceptual_roughness: 0.92,
                ..default()
            })
        })
        .clone()
}

#[allow(clippy::too_many_arguments)]
fn spawn_civic_part(
    parent: &mut ChildBuilder,
    resident_id: u64,
    kind: CivicBodyPartKind,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
) {
    parent.spawn((
        PbrBundle {
            mesh,
            material,
            transform: Transform {
                translation,
                rotation,
                scale,
            },
            ..default()
        },
        CivicBodyPart {
            resident_id,
            kind,
            base_translation: translation,
            base_rotation: rotation,
            base_scale: scale,
        },
    ));
}

fn resident_scale(stage: CivicLifeStage) -> f32 {
    match stage {
        CivicLifeStage::Youth => 0.76,
        CivicLifeStage::Adult => 1.0,
        CivicLifeStage::Elder => 0.94,
    }
}

fn spawn_detailed_civic_visual(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut CivicVisualCache,
    resident: &CivicResident,
) {
    let (cube, sphere, cylinder) = shared_civic_meshes(cache, meshes);
    let material = civic_material_set(resident.culture, cache, materials);
    let sole = sole_material(cache, materials);
    let scale = resident_scale(resident.life_stage);
    let resident_id = resident.id;
    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(cell_vec3(resident.logical_cell))
                    .with_scale(Vec3::splat(scale)),
                ..default()
            },
            CivicVisualRoot {
                resident_id,
                mode: CivicVisualMode::Detailed,
            },
            Name::new(format!("Civic Weaver // {}", resident.name)),
        ))
        .with_children(|root| {
            spawn_civic_part(
                root,
                resident_id,
                CivicBodyPartKind::Torso,
                cube.clone(),
                material.fabric.clone(),
                Vec3::new(0.0, 0.98, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.52, 0.62, 0.30),
            );
            spawn_civic_part(
                root,
                resident_id,
                CivicBodyPartKind::Head,
                sphere.clone(),
                material.skin.clone(),
                Vec3::new(0.0, 1.50, -0.01),
                Quat::IDENTITY,
                Vec3::new(0.48, 0.55, 0.46),
            );
            for (kind, x) in [
                (CivicBodyPartKind::LeftArm, -0.35),
                (CivicBodyPartKind::RightArm, 0.35),
            ] {
                spawn_civic_part(
                    root,
                    resident_id,
                    kind,
                    cylinder.clone(),
                    material.skin.clone(),
                    Vec3::new(x, 0.99, 0.0),
                    Quat::IDENTITY,
                    Vec3::new(0.15, 0.54, 0.15),
                );
            }
            for (kind, x) in [
                (CivicBodyPartKind::LeftLeg, -0.15),
                (CivicBodyPartKind::RightLeg, 0.15),
            ] {
                spawn_civic_part(
                    root,
                    resident_id,
                    kind,
                    cube.clone(),
                    sole.clone(),
                    Vec3::new(x, 0.35, 0.0),
                    Quat::IDENTITY,
                    Vec3::new(0.20, 0.58, 0.24),
                );
            }
            spawn_civic_part(
                root,
                resident_id,
                CivicBodyPartKind::Collar,
                cylinder.clone(),
                material.accent.clone(),
                Vec3::new(0.0, 1.29, 0.0),
                Quat::IDENTITY,
                Vec3::new(0.64, 0.055, 0.64),
            );
            spawn_civic_part(
                root,
                resident_id,
                CivicBodyPartKind::Sash,
                cube.clone(),
                material.accent.clone(),
                Vec3::new(0.02, 1.00, -0.165),
                Quat::from_rotation_z(0.38),
                Vec3::new(0.095, 0.62, 0.045),
            );
            for (kind, x) in [
                (CivicBodyPartKind::EyeLeft, -0.09),
                (CivicBodyPartKind::EyeRight, 0.09),
            ] {
                spawn_civic_part(
                    root,
                    resident_id,
                    kind,
                    sphere.clone(),
                    material.eye.clone(),
                    Vec3::new(x, 1.54, -0.225),
                    Quat::IDENTITY,
                    Vec3::new(0.065, 0.075, 0.045),
                );
            }
        });
}

fn spawn_proxy_civic_visual(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut CivicVisualCache,
    resident: &CivicResident,
) {
    let (cube, _, _) = shared_civic_meshes(cache, meshes);
    let material = civic_material_set(resident.culture, cache, materials);
    let scale = resident_scale(resident.life_stage);
    commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(cell_vec3(resident.logical_cell))
                    .with_scale(Vec3::splat(scale)),
                ..default()
            },
            CivicVisualRoot {
                resident_id: resident.id,
                mode: CivicVisualMode::Proxy,
            },
            Name::new(format!("Civic Weaver proxy // {}", resident.name)),
        ))
        .with_children(|root| {
            root.spawn(PbrBundle {
                mesh: cube,
                material: material.fabric,
                transform: Transform::from_translation(Vec3::new(0.0, 0.82, 0.0))
                    .with_scale(Vec3::new(0.48, 1.32, 0.28)),
                ..default()
            });
        });
}

fn remove_stale_civic_visuals(
    mut commands: Commands,
    runtime: Res<CivicRuntime>,
    roots: Query<(Entity, &CivicVisualRoot)>,
) {
    let mut existing_entities = roots
        .iter()
        .map(|(entity, root)| {
            (
                CivicVisualSelection {
                    resident_id: root.resident_id,
                    mode: root.mode,
                },
                entity,
            )
        })
        .collect::<Vec<_>>();
    existing_entities.sort_by_key(|(selection, entity)| {
        (
            selection.resident_id,
            selection.mode.stable_tag(),
            entity.index(),
        )
    });
    let existing = existing_entities
        .iter()
        .map(|(selection, _)| *selection)
        .collect::<Vec<_>>();
    let delta = plan_civic_visual_delta(&existing, &runtime.visual_selection);
    let mut removal_counts = BTreeMap::<(u64, CivicVisualMode), usize>::new();
    for selection in delta.removals {
        *removal_counts
            .entry((selection.resident_id, selection.mode))
            .or_default() += 1;
    }
    for (selection, entity) in existing_entities {
        let key = (selection.resident_id, selection.mode);
        if let Some(remaining) = removal_counts.get_mut(&key) {
            if *remaining > 0 {
                commands.entity(entity).despawn_recursive();
                *remaining -= 1;
            }
        }
    }
}

fn spawn_missing_civic_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<CivicVisualCache>,
    brain: Res<FriendlyWorldBrain>,
    runtime: Res<CivicRuntime>,
    roots: Query<&CivicVisualRoot>,
) {
    if !runtime.authority_ready() {
        return;
    }
    let existing = roots
        .iter()
        .map(|root| CivicVisualSelection {
            resident_id: root.resident_id,
            mode: root.mode,
        })
        .collect::<Vec<_>>();
    let delta = plan_civic_visual_delta(&existing, &runtime.visual_selection);
    for selection in &delta.builds {
        let Some(resident) = brain
            .save
            .civic_population
            .residents
            .iter()
            .find(|resident| resident.id == selection.resident_id)
        else {
            continue;
        };
        match selection.mode {
            CivicVisualMode::Detailed => spawn_detailed_civic_visual(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                resident,
            ),
            CivicVisualMode::Proxy => spawn_proxy_civic_visual(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                resident,
            ),
        }
    }
}

fn move_toward(current: Vec3, target: Vec3, max_distance: f32) -> Vec3 {
    let delta = target - current;
    let distance = delta.length();
    if distance <= max_distance || distance <= f32::EPSILON {
        target
    } else {
        current + delta * (max_distance / distance)
    }
}

fn sync_civic_visuals(
    time: Res<Time>,
    brain: Res<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
    mut roots: Query<(&CivicVisualRoot, &mut Transform)>,
) {
    if !runtime.authority_ready() || runtime.visual_selection.is_empty() {
        return;
    }
    let count = runtime.visual_selection.len().min(CIVIC_ROOT_SYNC_LIMIT);
    let scheduled = (0..count)
        .map(|offset| {
            let index = (runtime.sync_cursor + offset) % runtime.visual_selection.len();
            let selection = runtime.visual_selection[index];
            (selection.resident_id, selection.mode)
        })
        .collect::<HashSet<_>>();
    runtime.sync_cursor = (runtime.sync_cursor + count) % runtime.visual_selection.len();
    let max_step =
        CIVIC_WALK_SPEED_MM_PER_SECOND as f32 / 1_000.0 * time.delta_seconds().min(0.1) * 1.35;
    let mut updated_roots = 0_usize;
    for (root, mut transform) in &mut roots {
        if updated_roots >= CIVIC_ROOT_SYNC_LIMIT
            || !scheduled.contains(&(root.resident_id, root.mode))
        {
            continue;
        }
        let Some(resident) = brain
            .save
            .civic_population
            .residents
            .iter()
            .find(|resident| resident.id == root.resident_id)
        else {
            continue;
        };
        let target = cell_vec3(resident.logical_cell);
        transform.translation = move_toward(transform.translation, target, max_step);
        let facing = cell_vec3(resident.target_cell) - target;
        if facing.length_squared() > 0.01 {
            let desired_yaw = (-facing.x).atan2(-facing.z);
            transform.rotation = transform.rotation.slerp(
                Quat::from_rotation_y(desired_yaw),
                (time.delta_seconds() * 5.0).clamp(0.0, 1.0),
            );
        }
        updated_roots += 1;
    }
}

fn animate_civic_visuals(
    time: Res<Time>,
    brain: Res<FriendlyWorldBrain>,
    mut runtime: ResMut<CivicRuntime>,
    mut parts: Query<(&CivicBodyPart, &mut Transform)>,
) {
    if !runtime.authority_ready() {
        return;
    }
    let detailed = runtime
        .visual_selection
        .iter()
        .filter(|selection| selection.mode == CivicVisualMode::Detailed)
        .map(|selection| selection.resident_id)
        .collect::<Vec<_>>();
    if detailed.is_empty() {
        return;
    }
    let count = detailed.len().min(CIVIC_ANIMATION_LIMIT);
    let scheduled = (0..count)
        .map(|offset| detailed[(runtime.animation_cursor + offset) % detailed.len()])
        .collect::<HashSet<_>>();
    runtime.animation_cursor = (runtime.animation_cursor + count) % detailed.len();
    let elapsed = time.elapsed_seconds();
    for (part, mut transform) in &mut parts {
        if !scheduled.contains(&part.resident_id) {
            continue;
        }
        let Some(resident) = brain
            .save
            .civic_population
            .residents
            .iter()
            .find(|resident| resident.id == part.resident_id)
        else {
            continue;
        };
        transform.translation = part.base_translation;
        transform.rotation = part.base_rotation;
        transform.scale = part.base_scale;
        let phase = elapsed * 5.4 + (stable_mix64(resident.id) & 0xFF) as f32 * 0.03125;
        let moving = runtime
            .paths
            .get(&resident.id)
            .is_some_and(|path| !path.cells.is_empty());
        let gait = if moving { phase.sin() } else { 0.0 };
        match part.kind {
            CivicBodyPartKind::LeftArm => {
                let work = if matches!(
                    resident.activity,
                    CivicActivity::Work | CivicActivity::InspectSettlement
                ) {
                    (phase * 1.7).sin() * 0.38
                } else {
                    gait * 0.42
                };
                transform.rotation *= Quat::from_rotation_x(work);
            }
            CivicBodyPartKind::RightArm => {
                let work = if matches!(
                    resident.activity,
                    CivicActivity::Work | CivicActivity::InspectSettlement
                ) {
                    (phase * 1.7 + 1.4).sin() * 0.48
                } else {
                    -gait * 0.42
                };
                transform.rotation *= Quat::from_rotation_x(work);
            }
            CivicBodyPartKind::LeftLeg => {
                transform.rotation *= Quat::from_rotation_x(-gait * 0.34);
            }
            CivicBodyPartKind::RightLeg => {
                transform.rotation *= Quat::from_rotation_x(gait * 0.34);
            }
            CivicBodyPartKind::Head => {
                let social_nod = if matches!(
                    resident.activity,
                    CivicActivity::Socialize | CivicActivity::ShareKnowledge
                ) {
                    (phase * 0.65).sin() * 0.10
                } else {
                    0.0
                };
                transform.rotation *= Quat::from_rotation_x(social_nod);
            }
            CivicBodyPartKind::Torso => {
                let breath = 1.0 + (phase * 0.32).sin() * 0.012;
                transform.scale.y *= breath;
                if matches!(
                    resident.activity,
                    CivicActivity::RestAtHome | CivicActivity::Recover
                ) {
                    transform.translation.y -= 0.08;
                }
            }
            CivicBodyPartKind::Collar
            | CivicBodyPartKind::Sash
            | CivicBodyPartKind::EyeLeft
            | CivicBodyPartKind::EyeRight => {}
        }
    }
}

fn cleanup_civic_visuals(
    mut commands: Commands,
    roots: Query<Entity, With<CivicVisualRoot>>,
    mut runtime: ResMut<CivicRuntime>,
) {
    for entity in &roots {
        commands.entity(entity).despawn_recursive();
    }
    runtime.reset_world();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::Voxel;

    fn identity(seed: u32, profile: WorldProfile) -> WorldGenerationIdentity {
        WorldGenerationIdentity {
            seed,
            world_profile: profile,
            scenery_quality: SceneryQuality::Lush,
            terrain_grammar: TerrainGrammarVersion::V3,
        }
    }

    fn resident(id: u64, settlement_id: u64) -> CivicResident {
        CivicResident {
            id,
            settlement_id,
            name: format!("Test Resident {id}"),
            culture: CivicCulture::Riverglass,
            calling: CivicCalling::HabitatSteward,
            life_stage: CivicLifeStage::Adult,
            logical_cell: [0, 20, 0],
            home_cell: [0, 20, 0],
            work_cell: [5, 20, 0],
            commons_cell: [0, 20, 5],
            shelter_cell: [0, 20, 1],
            target_cell: [0, 20, 0],
            activity: CivicActivity::RestAtHome,
            committed_until_tick: 0,
            needs: CivicNeeds::default(),
            memories: Vec::new(),
            relationships: Vec::new(),
            movement_progress_mm: 0,
            route_failures: 0,
            route_retry_after_tick: 0,
            deferred_activity: None,
            route_failure: CivicFailureCode::None,
            last_decision_tick: 0,
        }
    }

    fn flat_loaded_world(radius: i32) -> VoxelWorld {
        let mut world = VoxelWorld::new();
        for z in -radius..=radius {
            for x in -radius..=radius {
                let _ = world.edit_set_voxel(x, 19, z, BlockType::Stone as Voxel);
            }
        }
        world
    }

    #[test]
    fn resident_ids_are_stable_namespaced_and_order_independent() {
        let exact = identity(0x48D2_09A1, WorldProfile::Natural);
        let first = (0..32)
            .map(|ordinal| stable_resident_id(exact, 7, ordinal, 0))
            .collect::<Vec<_>>();
        let replay = (0..32)
            .rev()
            .map(|ordinal| (ordinal, stable_resident_id(exact, 7, ordinal, 0)))
            .collect::<BTreeMap<_, _>>();
        assert!(first.iter().all(|id| id & (1_u64 << 63) != 0));
        assert_eq!(first.iter().copied().collect::<BTreeSet<_>>().len(), 32);
        for (ordinal, id) in first.into_iter().enumerate() {
            assert_eq!(replay[&ordinal], id);
        }
        assert_ne!(
            stable_resident_id(exact, 7, 0, 0),
            stable_resident_id(exact, 8, 0, 0)
        );
        assert_ne!(
            stable_resident_id(exact, 7, 0, 0),
            stable_resident_id(identity(0x48D2_09A2, WorldProfile::Natural), 7, 0, 0)
        );
    }

    #[test]
    fn population_normalization_enforces_global_per_settlement_and_sparse_caps() {
        let mut population = CivicPopulation::default();
        population.blackboard.notices = (0..40)
            .map(|tick| CivicNotice {
                logical_tick: tick,
                code: CivicMemoryKind::RouteBlocked,
                cell: [0, 20, 0],
            })
            .collect();
        for settlement_id in 1..=5_u64 {
            for ordinal in 0..40_u64 {
                let id = (1_u64 << 63) | (settlement_id << 16) | ordinal.saturating_add(1);
                let mut entry = resident(id, settlement_id);
                entry.memories = (0..30)
                    .map(|tick| CivicMemory {
                        kind: CivicMemoryKind::RouteBlocked,
                        logical_tick: tick,
                        cell: [tick as i32, 20, 0],
                        confidence: u16::MAX,
                    })
                    .collect();
                entry.relationships = (0..30)
                    .map(|peer| CivicRelationship {
                        resident_id: (1_u64 << 63) | (settlement_id << 16) | (peer + 1),
                        familiarity: u16::MAX,
                        trust: u16::MAX,
                        last_shared_tick: peer,
                    })
                    .collect();
                population.residents.push(entry);
            }
        }
        let duplicate_id = population.residents[0].id;
        population.residents.push(resident(duplicate_id, 5));
        population.residents[0].route_failures = u8::MAX;
        population.residents.reverse();
        population.normalize(1..=5);

        assert_eq!(population.residents.len(), CIVIC_WORLD_HARD_LIMIT);
        assert_eq!(
            population
                .residents
                .iter()
                .map(|resident| resident.id)
                .collect::<BTreeSet<_>>()
                .len(),
            population.residents.len()
        );
        assert_eq!(population.blackboard.notices.len(), CIVIC_NOTICE_HARD_LIMIT);
        for settlement_id in 1..=5_u64 {
            assert!(
                population
                    .residents
                    .iter()
                    .filter(|resident| resident.settlement_id == settlement_id)
                    .count()
                    <= CIVIC_SETTLEMENT_HARD_LIMIT
            );
        }
        assert!(population
            .residents
            .iter()
            .all(
                |resident| resident.memories.len() <= CIVIC_MEMORY_HARD_LIMIT
                    && resident.relationships.len() <= CIVIC_RELATIONSHIP_HARD_LIMIT
                    && resident.route_failures <= 15
                    && resident
                        .memories
                        .iter()
                        .all(|memory| memory.confidence <= 1_000)
                    && resident.relationships.iter().all(|relationship| {
                        relationship.familiarity <= 1_000 && relationship.trust <= 1_000
                    })
            ));
        assert!(population.residents.windows(2).all(|pair| {
            (pair[0].settlement_id, pair[0].id) < (pair[1].settlement_id, pair[1].id)
        }));
    }

    #[test]
    fn normalization_caps_notices_even_without_a_valid_settlement() {
        let mut population = CivicPopulation::default();
        population.residents.push(resident(1_u64 << 63, 1));
        population.blackboard.notices = (0..100)
            .map(|tick| CivicNotice {
                logical_tick: tick,
                code: CivicMemoryKind::RouteBlocked,
                cell: [0, 20, 0],
            })
            .collect();
        population.normalize([]);
        assert!(population.residents.is_empty());
        assert_eq!(population.blackboard.notices.len(), CIVIC_NOTICE_HARD_LIMIT);
        assert_eq!(population.authority, CivicAuthorityState::Uninitialized);
    }

    #[test]
    fn cognition_is_fixed_point_weather_preemptive_and_commitment_stable() {
        let mut entry = resident(1_u64 << 63, 1);
        entry.activity = CivicActivity::RestAtHome;
        assert_eq!(choose_activity(&entry, 600, 0, 100), CivicActivity::Work);
        assert_eq!(
            choose_activity(&entry, 600, 900, 100),
            CivicActivity::SeekShelter
        );
        assert_eq!(
            choose_activity(&entry, 60, 0, 100),
            CivicActivity::RestAtHome
        );
        entry.activity = CivicActivity::Socialize;
        entry.committed_until_tick = 200;
        assert_eq!(
            choose_activity(&entry, 600, 0, 150),
            CivicActivity::Socialize
        );
        assert!(CivicActivity::DECISION_SET
            .iter()
            .all(|activity| activity.stable_tag() <= 9));
    }

    #[test]
    fn needs_remain_bounded_under_long_adversarial_progression() {
        let mut entry = resident(1_u64 << 63, 1);
        entry.needs = CivicNeeds {
            energy: 1_000,
            belonging: 1_000,
            safety: 1_000,
            purpose: 1_000,
            curiosity: 1_000,
        };
        entry.activity = CivicActivity::Work;
        for _ in 0..100_000 {
            advance_needs(&mut entry, 1_000);
        }
        assert!(entry.needs.energy <= 1_000);
        assert!(entry.needs.belonging <= 1_000);
        assert!(entry.needs.safety <= 1_000);
        assert!(entry.needs.purpose <= 1_000);
        assert!(entry.needs.curiosity <= 1_000);
    }

    #[test]
    fn sparse_social_pairing_never_builds_a_complete_graph() {
        let mut population = CivicPopulation {
            authority: CivicAuthorityState::Active,
            logical_tick: 10,
            ..default()
        };
        for ordinal in 0..32_u64 {
            let mut entry = resident((1_u64 << 63) | (ordinal + 1), 1);
            entry.activity = CivicActivity::Socialize;
            entry.logical_cell = [0, 20, 5];
            entry.commons_cell = [0, 20, 5];
            population.residents.push(entry);
        }
        for tick in 1..=1_000_u64 {
            population.logical_tick = tick * 10;
            let logical_tick = population.logical_tick;
            let _ = update_social_pairs(&mut population, logical_tick);
        }
        assert!(population
            .residents
            .iter()
            .all(
                |resident| resident.relationships.len() <= CIVIC_RELATIONSHIP_HARD_LIMIT
                    && resident.memories.len() <= CIVIC_MEMORY_HARD_LIMIT
            ));
        let total_edges: usize = population
            .residents
            .iter()
            .map(|resident| resident.relationships.len())
            .sum();
        assert!(total_edges <= 32 * CIVIC_RELATIONSHIP_HARD_LIMIT);
        assert!(total_edges < 32 * 31);
        assert!(population
            .residents
            .iter()
            .all(|resident| !resident.relationships.is_empty()));
    }

    #[test]
    fn catchup_batching_preserves_exact_social_ticks_and_persistent_state() {
        let mut initial = CivicPopulation {
            authority: CivicAuthorityState::Active,
            logical_tick: 8,
            ..default()
        };
        for ordinal in 0..2_u64 {
            let mut entry = resident((1_u64 << 63) | (ordinal + 1), 1);
            entry.activity = CivicActivity::Socialize;
            entry.logical_cell = entry.commons_cell;
            initial.residents.push(entry);
        }

        let mut batched = initial.clone();
        let mut batched_accumulator = 0.0;
        let mut batched_ticks = VecDeque::with_capacity(2);
        admit_civic_step_ticks(
            &mut batched_accumulator,
            CIVIC_FIXED_STEP_SECONDS * 2.0,
            &mut batched.logical_tick,
            &mut batched_ticks,
        );
        for logical_tick in batched_ticks.iter().copied() {
            let _ = update_social_pairs(&mut batched, logical_tick);
        }

        let mut partitioned = initial;
        let mut partitioned_accumulator = 0.0;
        let mut partitioned_ticks = VecDeque::new();
        let mut ticks = VecDeque::with_capacity(2);
        for _ in 0..2 {
            admit_civic_step_ticks(
                &mut partitioned_accumulator,
                CIVIC_FIXED_STEP_SECONDS,
                &mut partitioned.logical_tick,
                &mut ticks,
            );
            for logical_tick in ticks.iter().copied() {
                let _ = update_social_pairs(&mut partitioned, logical_tick);
            }
            partitioned_ticks.extend(ticks.iter().copied());
        }

        assert_eq!(batched_ticks, VecDeque::from([9, 10]));
        assert_eq!(batched_ticks, partitioned_ticks);
        assert!((batched_accumulator - partitioned_accumulator).abs() <= f32::EPSILON);
        assert_eq!(batched.blackboard.successful_gatherings, 1);
        assert_eq!(
            ron::to_string(&batched).unwrap(),
            ron::to_string(&partitioned).unwrap()
        );
    }

    #[test]
    fn loaded_voxel_path_is_deterministic_and_detours_around_edits() {
        let mut world = flat_loaded_world(8);
        let direct = solve_loaded_path(&world, [0, 20, 0], [5, 20, 0]).unwrap();
        assert_eq!(direct.cells.len(), 5);
        assert!(direct.expansions <= CIVIC_PATH_EXPANSION_LIMIT);

        let _ = world.edit_set_voxel(2, 20, 0, BlockType::Stone as Voxel);
        let _ = world.edit_set_voxel(2, 21, 0, BlockType::Stone as Voxel);
        let detour_a = solve_loaded_path(&world, [0, 20, 0], [5, 20, 0]).unwrap();
        let detour_b = solve_loaded_path(&world, [0, 20, 0], [5, 20, 0]).unwrap();
        assert_eq!(detour_a, detour_b);
        assert!(detour_a.cells.len() > direct.cells.len());
        assert!(!detour_a.cells.contains(&[2, 20, 0]));
        assert!(detour_a.cells.len() <= CIVIC_PATH_CELL_LIMIT);
    }

    #[test]
    fn pathfinding_fails_closed_for_unresolved_and_extreme_coordinates() {
        let world = VoxelWorld::new();
        assert_eq!(
            solve_loaded_path(&world, [0, 20, 0], [1, 20, 0]),
            Err(CivicPathFailure::CoverageUnresolved)
        );
        assert!(solve_loaded_path(
            &world,
            [i32::MAX, i32::MAX, i32::MAX],
            [i32::MIN, i32::MIN, i32::MIN]
        )
        .is_err());
        assert_eq!(checked_neighbor_column([i32::MAX, 0, 0], [1, 0]), None);
        assert_eq!(checked_neighbor_column([i32::MIN, 0, 0], [-1, 0]), None);
        assert_eq!(checked_neighbor_column([0, 0, i32::MAX], [0, 1]), None);
        assert_eq!(checked_neighbor_column([0, 0, i32::MIN], [0, -1]), None);
    }

    #[test]
    fn exact_astar_expansion_ceiling_returns_budget_failure() {
        let mut world = flat_loaded_world(20);
        for [x, z] in [[1, 0], [-1, 0], [0, 1], [0, -1]] {
            let _ = world.edit_set_voxel(x, 20, z, BlockType::Stone as Voxel);
            let _ = world.edit_set_voxel(x, 21, z, BlockType::Stone as Voxel);
        }
        let result = solve_loaded_path(&world, [-18, 20, -18], [0, 20, 0]);
        assert_eq!(result, Err(CivicPathFailure::BudgetExhausted));
    }

    #[test]
    fn culture_is_causally_driven_by_profile_biome_and_environment() {
        let temperate = EnvironmentSample {
            temperature_norm: 0.55,
            atmospheric_moisture: 0.55,
            soil_moisture: 0.60,
            river_strength: 0.2,
            mineral_resonance: 0.2,
            flowering_resonance: 0.3,
            flow_direction: [1.0, 0.0],
        };
        assert_eq!(
            CivicCulture::from_environment(WorldProfile::Natural, Biome::Forest, temperate),
            CivicCulture::Canopy
        );
        assert_eq!(
            CivicCulture::from_environment(WorldProfile::Natural, Biome::Desert, temperate),
            CivicCulture::Sunstone
        );
        assert_eq!(
            CivicCulture::from_environment(WorldProfile::AstralFrontier, Biome::Forest, temperate),
            CivicCulture::Astral
        );
    }

    #[test]
    fn serialized_schema_has_no_commerce_state_and_round_trips() {
        let mut population = CivicPopulation {
            authority: CivicAuthorityState::Active,
            generation_identity: Some(identity(7, WorldProfile::Natural)),
            ..default()
        };
        population.residents.push(resident(1_u64 << 63, 1));
        let text = ron::ser::to_string(&population).unwrap();
        let lowercase = text.to_ascii_lowercase();
        for forbidden in ["trade", "price", "currency", "merchant", "offer", "market"] {
            assert!(
                !lowercase.contains(forbidden),
                "found forbidden {forbidden}"
            );
        }
        let replay: CivicPopulation = ron::from_str(&text).unwrap();
        assert_eq!(replay.residents.len(), 1);
        assert_eq!(replay.generation_identity, population.generation_identity);
        assert_eq!(replay.residents[0].id, population.residents[0].id);
    }

    #[test]
    fn identity_block_restores_when_matching_world_returns() {
        let expected = identity(77, WorldProfile::Natural);
        let other = identity(78, WorldProfile::Natural);
        let resident_id = 1_u64 << 63;
        let mut population = CivicPopulation {
            generation_identity: Some(expected),
            ..default()
        };
        population.residents.push(resident(resident_id, 1));

        let (blocked, changed) = reconcile_civic_identity(&mut population, other);
        assert_eq!(blocked, CivicIdentityDisposition::Blocked);
        assert!(changed);
        assert_eq!(population.authority, CivicAuthorityState::IdentityBlocked);
        assert_eq!(
            population.last_failure,
            CivicFailureCode::GenerationIdentityMismatch
        );

        let persisted = ron::ser::to_string(&population).unwrap();
        let mut replay: CivicPopulation = ron::from_str(&persisted).unwrap();
        let (ready, restored) = reconcile_civic_identity(&mut replay, expected);
        assert_eq!(ready, CivicIdentityDisposition::Ready);
        assert!(restored);
        assert_eq!(replay.authority, CivicAuthorityState::Active);
        assert_eq!(replay.last_failure, CivicFailureCode::None);
        assert_eq!(replay.generation_identity, Some(expected));
        assert_eq!(replay.residents.len(), 1);
        assert_eq!(replay.residents[0].id, resident_id);
    }

    #[test]
    fn authority_suspension_clears_transient_work_and_preserves_rollback_setting() {
        let mut runtime = CivicRuntime::from_environment();
        runtime.enabled = true;
        runtime.authority_identity = Some(identity(5, WorldProfile::Natural));
        runtime.logical_active = vec![1, 2];
        runtime.visual_selection.push(CivicVisualSelection {
            resident_id: 1,
            mode: CivicVisualMode::Detailed,
        });
        runtime.path_queue.push_back(CivicPathRequest {
            resident_id: 1,
            start: [0, 20, 0],
            goal: [1, 20, 0],
            requested_tick: 1,
        });
        runtime.paths.insert(
            1,
            CivicPath {
                cells: VecDeque::from([[1, 20, 0]]),
                goal: [1, 20, 0],
            },
        );
        runtime.pending_step_ticks = VecDeque::from([9, 10]);
        runtime.accumulator = 0.3;

        runtime.suspend_authority();
        assert!(runtime.enabled);
        assert!(!runtime.authority_ready());
        assert!(runtime.logical_active.is_empty());
        assert!(runtime.visual_selection.is_empty());
        assert!(runtime.path_queue.is_empty());
        assert!(runtime.paths.is_empty());
        assert!(runtime.pending_step_ticks.is_empty());
        assert_eq!(runtime.accumulator, 0.0);
    }

    #[test]
    fn route_resolves_obstructed_start_before_astar_without_teleporting() {
        let mut world = flat_loaded_world(8);
        for y in 15..=25 {
            let _ = world.edit_set_voxel(0, y, 0, BlockType::Stone as Voxel);
        }
        let requested_start = [0, 20, 0];
        let resolved_start = nearest_standable_cell(&world, requested_start).unwrap();
        assert_eq!(resolved_start, [0, 20, -2]);
        let solution = solve_loaded_path(&world, resolved_start, [5, 20, 0]).unwrap();
        assert_eq!(solution.resolved_start, resolved_start);
        assert_eq!(solution.resolved_goal, [5, 20, 0]);
        let first = solution.cells.first().copied().unwrap();
        assert_eq!(manhattan_xz(first, resolved_start), 1);
        assert_eq!(solution.cells.last().copied(), Some(solution.resolved_goal));

        let mut entry = resident(1_u64 << 63, 1);
        entry.logical_cell = requested_start;
        entry.home_cell = requested_start;
        entry.target_cell = requested_start;
        assert!(reconcile_resident_cell(&mut entry, resolved_start));
        assert_eq!(entry.logical_cell, resolved_start);
        assert_eq!(entry.home_cell, resolved_start);
        assert_eq!(entry.target_cell, resolved_start);
    }

    #[test]
    fn route_retry_backoff_is_monotonic_capped_and_overflow_safe() {
        assert_eq!(
            route_retry_delay(CivicFailureCode::CoverageUnresolved, 1),
            10
        );
        assert_eq!(
            route_retry_delay(CivicFailureCode::CoverageUnresolved, 2),
            20
        );
        assert_eq!(
            route_retry_delay(CivicFailureCode::CoverageUnresolved, 15),
            160
        );
        assert_eq!(route_retry_delay(CivicFailureCode::NoRoute, 1), 40);
        assert_eq!(route_retry_delay(CivicFailureCode::NoRoute, 2), 80);
        assert_eq!(route_retry_delay(CivicFailureCode::NoRoute, 15), 640);

        let mut entry = resident(1_u64 << 63, 1);
        entry.activity = CivicActivity::Work;
        register_route_failure(&mut entry, CivicFailureCode::NoRoute, u64::MAX);
        assert_eq!(entry.route_retry_after_tick, u64::MAX);
        assert_eq!(entry.activity, CivicActivity::WaitForCoverage);
        assert_eq!(entry.deferred_activity, Some(CivicActivity::Work));
        assert_eq!(entry.route_failure, CivicFailureCode::NoRoute);
    }

    #[test]
    fn path_queue_prunes_invalid_work_preserves_fifo_and_replaces_at_capacity() {
        let mut queue = VecDeque::from([
            CivicPathRequest {
                resident_id: 99,
                start: [0, 20, 0],
                goal: [1, 20, 0],
                requested_tick: 2,
            },
            CivicPathRequest {
                resident_id: 1,
                start: [0, 20, 0],
                goal: [1, 20, 0],
                requested_tick: 2,
            },
            CivicPathRequest {
                resident_id: 2,
                start: [9, 20, 0],
                goal: [2, 20, 0],
                requested_tick: 2,
            },
            CivicPathRequest {
                resident_id: 2,
                start: [1, 20, 0],
                goal: [2, 20, 0],
                requested_tick: 3,
            },
            CivicPathRequest {
                resident_id: 3,
                start: [2, 20, 0],
                goal: [3, 20, 0],
                requested_tick: 99,
            },
        ]);
        let states = BTreeMap::from([
            (1, ([0, 20, 0], [1, 20, 0])),
            (2, ([1, 20, 0], [2, 20, 0])),
            (3, ([2, 20, 0], [3, 20, 0])),
        ]);
        prune_civic_path_queue(&mut queue, &states, &BTreeSet::from([1, 2]), 3);
        assert_eq!(
            queue
                .iter()
                .map(|request| request.resident_id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let mut runtime = CivicRuntime::from_environment();
        runtime.path_queue = (0..CIVIC_PATH_QUEUE_LIMIT as u64)
            .map(|resident_id| CivicPathRequest {
                resident_id,
                start: [resident_id as i32, 20, 0],
                goal: [resident_id as i32, 20, 1],
                requested_tick: 1,
            })
            .collect();
        assert!(runtime.enqueue_path(CivicPathRequest {
            resident_id: 7,
            start: [7, 20, 0],
            goal: [7, 20, 2],
            requested_tick: 2,
        }));
        assert_eq!(runtime.path_queue.len(), CIVIC_PATH_QUEUE_LIMIT);
        assert_eq!(
            runtime
                .path_queue
                .iter()
                .filter(|request| request.resident_id == 7)
                .count(),
            1
        );
        assert_eq!(runtime.path_queue.back().unwrap().requested_tick, 2);
    }

    #[test]
    fn normalization_dedups_nonadjacent_relationship_ids_and_keeps_best() {
        let mut entry = resident(1_u64 << 63, 1);
        let peer_a = (1_u64 << 63) | 2;
        let peer_b = (1_u64 << 63) | 3;
        entry.relationships = vec![
            CivicRelationship {
                resident_id: peer_a,
                familiarity: 900,
                trust: 100,
                last_shared_tick: 2,
            },
            CivicRelationship {
                resident_id: peer_b,
                familiarity: 700,
                trust: 700,
                last_shared_tick: 3,
            },
            CivicRelationship {
                resident_id: peer_a,
                familiarity: 900,
                trust: 800,
                last_shared_tick: 4,
            },
        ];
        entry.normalize_shallow();
        assert_eq!(entry.relationships.len(), 2);
        let kept = entry
            .relationships
            .iter()
            .find(|relationship| relationship.resident_id == peer_a)
            .unwrap();
        assert_eq!(kept.familiarity, 900);
        assert_eq!(kept.trust, 800);
        assert_eq!(kept.last_shared_tick, 4);
    }

    #[test]
    fn lod_plan_is_permutation_invariant_exactly_capped_and_boundary_safe() {
        let player = Vec3::ZERO;
        let forward = (1..=80_u64)
            .map(|resident_id| (resident_id, Vec3::ZERO))
            .collect::<Vec<_>>();
        let mut reverse = forward.clone();
        reverse.reverse();
        let first = plan_civic_lod(&forward, Some(player));
        let replay = plan_civic_lod(&reverse, Some(player));
        assert_eq!(first, replay);
        assert_eq!(first.logical_active, (1..=64_u64).collect::<Vec<_>>());
        assert_eq!(first.visuals.len(), CIVIC_PROXY_LIMIT);
        assert_eq!(
            first
                .visuals
                .iter()
                .filter(|selection| selection.mode == CivicVisualMode::Detailed)
                .count(),
            CIVIC_FULL_RIG_LIMIT
        );
        assert_eq!(
            first
                .visuals
                .iter()
                .filter(|selection| selection.mode == CivicVisualMode::Proxy)
                .count(),
            CIVIC_PROXY_LIMIT - CIVIC_FULL_RIG_LIMIT
        );

        let boundary = plan_civic_lod(
            &[
                (1, Vec3::new(CIVIC_VISUAL_DISTANCE, 0.0, 0.0)),
                (2, Vec3::new(CIVIC_VISUAL_DISTANCE + 0.25, 0.0, 0.0)),
                (3, Vec3::new(CIVIC_ACTIVE_DISTANCE, 0.0, 0.0)),
                (4, Vec3::new(CIVIC_ACTIVE_DISTANCE + 0.25, 0.0, 0.0)),
            ],
            Some(player),
        );
        assert_eq!(boundary.logical_active, vec![1, 2, 3]);
        assert_eq!(
            boundary
                .visuals
                .iter()
                .map(|selection| selection.resident_id)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn visual_delta_is_permutation_invariant_and_mode_transition_budgeted() {
        let existing = (1..=10_u64)
            .map(|resident_id| CivicVisualSelection {
                resident_id,
                mode: CivicVisualMode::Detailed,
            })
            .collect::<Vec<_>>();
        let desired = vec![
            CivicVisualSelection {
                resident_id: 1,
                mode: CivicVisualMode::Proxy,
            },
            CivicVisualSelection {
                resident_id: 20,
                mode: CivicVisualMode::Detailed,
            },
            CivicVisualSelection {
                resident_id: 21,
                mode: CivicVisualMode::Proxy,
            },
            CivicVisualSelection {
                resident_id: 22,
                mode: CivicVisualMode::Proxy,
            },
        ];
        let mut reversed = existing.clone();
        reversed.reverse();
        let first = plan_civic_visual_delta(&existing, &desired);
        let replay = plan_civic_visual_delta(&reversed, &desired);
        assert_eq!(first, replay);
        assert_eq!(first.removals.len(), CIVIC_VISUAL_REMOVE_LIMIT);
        assert!(first.removals.contains(&CivicVisualSelection {
            resident_id: 1,
            mode: CivicVisualMode::Detailed,
        }));
        assert_eq!(first.builds.len(), CIVIC_VISUAL_BUILD_LIMIT);
        assert!(first
            .builds
            .iter()
            .all(|selection| selection.resident_id >= 20));
        assert!(!first
            .builds
            .iter()
            .any(|selection| selection.resident_id == 1));

        let after_old_mode_removal = plan_civic_visual_delta(&existing[1..], &desired);
        assert!(after_old_mode_removal
            .builds
            .contains(&CivicVisualSelection {
                resident_id: 1,
                mode: CivicVisualMode::Proxy,
            }));

        let mut cap_guard_existing = (1..=4_u64)
            .map(|resident_id| CivicVisualSelection {
                resident_id,
                mode: CivicVisualMode::Proxy,
            })
            .collect::<Vec<_>>();
        cap_guard_existing.extend((100..108_u64).map(|resident_id| CivicVisualSelection {
            resident_id,
            mode: CivicVisualMode::Detailed,
        }));
        let requested_detailed = vec![
            CivicVisualSelection {
                resident_id: 20,
                mode: CivicVisualMode::Detailed,
            },
            CivicVisualSelection {
                resident_id: 21,
                mode: CivicVisualMode::Detailed,
            },
        ];
        let capped = plan_civic_visual_delta(&cap_guard_existing, &requested_detailed);
        assert_eq!(capped.removals.len(), CIVIC_VISUAL_REMOVE_LIMIT);
        assert!(capped.builds.is_empty());

        let one_detailed_slot =
            plan_civic_visual_delta(&cap_guard_existing[5..], &requested_detailed);
        assert_eq!(one_detailed_slot.builds.len(), 1);
        assert_eq!(one_detailed_slot.builds[0].resident_id, 20);

        let full_root_set = (1..=CIVIC_PROXY_LIMIT as u64)
            .map(|resident_id| CivicVisualSelection {
                resident_id,
                mode: CivicVisualMode::Proxy,
            })
            .collect::<Vec<_>>();
        let no_total_slot = plan_civic_visual_delta(
            &full_root_set,
            &[CivicVisualSelection {
                resident_id: 100,
                mode: CivicVisualMode::Proxy,
            }],
        );
        assert!(no_total_slot.builds.is_empty());
    }

    #[test]
    fn path_cache_eviction_is_deterministic_and_empty_routes_do_not_evict() {
        let mut paths = (1..=CIVIC_PATH_CACHE_LIMIT as u64)
            .map(|resident_id| {
                (
                    resident_id,
                    CivicPath {
                        cells: VecDeque::from([[resident_id as i32, 20, 1]]),
                        goal: [resident_id as i32, 20, 1],
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            install_civic_path(&mut paths, 100, vec![[100, 20, 1]], [100, 20, 1]),
            Some(1)
        );
        assert_eq!(paths.len(), CIVIC_PATH_CACHE_LIMIT);
        assert!(!paths.contains_key(&1));
        assert!(paths.contains_key(&100));

        assert_eq!(
            install_civic_path(&mut paths, 200, Vec::new(), [200, 20, 0]),
            None
        );
        assert_eq!(paths.len(), CIVIC_PATH_CACHE_LIMIT);
        assert!(!paths.contains_key(&200));
        assert_eq!(
            install_civic_path(&mut paths, 100, vec![[100, 20, 2]], [100, 20, 2]),
            None
        );
        assert_eq!(paths.len(), CIVIC_PATH_CACHE_LIMIT);
        assert_eq!(paths[&100].goal, [100, 20, 2]);
    }

    #[test]
    fn visual_and_simulation_budget_constants_are_internally_bounded() {
        assert_eq!(CIVIC_SEED_POPULATION, 12);
        assert_eq!(CIVIC_WORLD_HARD_LIMIT, 128);
        assert_eq!(CIVIC_SETTLEMENT_HARD_LIMIT, 32);
        assert_eq!(CIVIC_ACTIVE_LOGICAL_LIMIT, 64);
        assert_eq!(CIVIC_FULL_RIG_LIMIT, 8);
        assert_eq!(CIVIC_PROXY_LIMIT, 24);
        const {
            assert!(CIVIC_SEED_POPULATION <= CIVIC_SETTLEMENT_HARD_LIMIT);
            assert!(CIVIC_SETTLEMENT_HARD_LIMIT <= CIVIC_ACTIVE_LOGICAL_LIMIT);
            assert!(CIVIC_FULL_RIG_LIMIT <= CIVIC_PROXY_LIMIT);
        }
        assert_eq!(CIVIC_VISUAL_BUILD_LIMIT, 2);
        assert_eq!(CIVIC_VISUAL_REMOVE_LIMIT, 4);
        assert_eq!(CIVIC_ROOT_SYNC_LIMIT, 16);
        assert_eq!(CIVIC_ANIMATION_LIMIT, 8);
        assert_eq!(CIVIC_RECONCILE_LIMIT, 2);
        assert_eq!(CIVIC_MEMORY_HARD_LIMIT, 12);
        assert_eq!(CIVIC_RELATIONSHIP_HARD_LIMIT, 12);
        assert_eq!(CIVIC_NOTICE_HARD_LIMIT, 16);
        assert_eq!(CIVIC_DECISIONS_PER_TICK, 8);
        assert_eq!(CIVIC_FIXED_STEP_SECONDS, 0.2);
        assert_eq!(CIVIC_MAX_CATCHUP_STEPS, 2);
        assert_eq!(CIVIC_MAX_FRAME_DELTA_SECONDS, 0.4);
        assert_eq!(CIVIC_PATH_QUEUE_LIMIT, 32);
        assert_eq!(CIVIC_PATH_EXPANSION_LIMIT, 768);
        assert_eq!(CIVIC_PATH_CELL_LIMIT, 96);
        assert_eq!(CIVIC_PATH_CACHE_LIMIT, 64);
        assert_eq!(CIVIC_MAX_ROUTE_RADIUS, 48);
        assert_eq!(CIVIC_WALK_SPEED_MM_PER_SECOND, 1_400);
        assert_eq!(CIVIC_POSITION_CHECKPOINT_SECONDS, 5.0);
        assert_eq!(CIVIC_LOD_REFRESH_SECONDS, 0.25);
        assert_eq!(CIVIC_ACTIVE_DISTANCE, 320.0);
        assert_eq!(CIVIC_VISUAL_DISTANCE, 220.0);
        assert_eq!(CIVIC_COVERAGE_RETRY_BASE_TICKS, 10);
        assert_eq!(CIVIC_ROUTE_RETRY_BASE_TICKS, 40);
        assert_eq!(CIVIC_ROUTE_RETRY_MAX_SHIFT, 4);
        assert_eq!(CivicCulture::ALL.len() * 4 + 1, 25);
    }
}
