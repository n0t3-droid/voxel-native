//! STADT — City-Builder layer on top of the voxel world.
//!
//! Slim Cut 1 of the plan-v3 city system:
//!
//! * **CA Road-Grid-Tool** — choose Road in the STADT tab or Toolbelt,
//!   click once to set start, click again to commit an editable road
//!   component that follows terrain. Axis drags create straights,
//!   diagonal drags create clean corner roads, and same-point clicks
//!   create roundabouts. Width with `[` / `]` (1..=9).
//! * **CC District-Theming** — choose Zone to paint district discs on
//!   the ground; each disc is a tagged decoration, visualized as a
//!   coloured ring gizmo. Auto-fill with prefabs comes in a later cut.
//! * **X1 Smart-Snap** — press `.` to cycle OFF → GRID-1 → GRID-4 →
//!   GRID-16 → ROAD. Snapping is applied to every placed point so
//!   blocks land on clean grid lines or snap to nearby roads.
//! * **X5 Contextual Hints HUD** — bottom-left panel rendered while
//!   the editor or live Toolbelt city tools are active. Shows what
//!   each mouse button / key is bound to.
//!
//! All mutation goes through [`VoxelWorld::edit_set_voxel`] so the
//! existing async mesher picks changes up within a frame or two; no
//! coupling to the builder state machine needed.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::blocks::{voxel_is_solid, BlockType, Voxel, AIR};
use crate::director::UnifiedTelemetry;
use crate::editor::{EditorState, EditorTab};
use crate::player::Player;
use crate::world::VoxelWorld;

// ---------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------

/// Which sub-tool inside the STADT tab is currently armed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CityTool {
    #[default]
    None,
    Road,
    District,
    Building,
    Facade,
}

impl CityTool {
    pub fn label(self) -> &'static str {
        match self {
            CityTool::None => "AUS",
            CityTool::Road => "STRASSE",
            CityTool::District => "BEZIRK",
            CityTool::Building => "GEBAEUDE",
            CityTool::Facade => "FASSADE",
        }
    }
}

/// Road surface material. Maps to a concrete [`BlockType`] at stamp
/// time. More exotic surfaces (neon glass, animated texture) will
/// arrive in later cuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RoadStyle {
    #[default]
    Asphalt,
    Cobble,
    Neon,
    Dirt,
}

impl RoadStyle {
    pub fn label(self) -> &'static str {
        match self {
            RoadStyle::Asphalt => "Asphalt",
            RoadStyle::Cobble => "Kopfstein",
            RoadStyle::Neon => "Neon",
            RoadStyle::Dirt => "Erde",
        }
    }
    pub fn surface_block(self) -> BlockType {
        match self {
            RoadStyle::Asphalt => BlockType::Stone,
            RoadStyle::Cobble => BlockType::MossStone,
            RoadStyle::Neon => BlockType::Limestone,
            RoadStyle::Dirt => BlockType::Dirt,
        }
    }
    pub fn stripe_block(self) -> Option<BlockType> {
        match self {
            RoadStyle::Asphalt => Some(BlockType::Snow),
            RoadStyle::Neon => Some(BlockType::Snow),
            _ => None,
        }
    }
    /// Gizmo colour — used by the minimap and the draw preview.
    pub fn gizmo_color(self) -> Color {
        match self {
            RoadStyle::Asphalt => Color::srgb(0.78, 0.78, 0.82),
            RoadStyle::Cobble => Color::srgb(0.55, 0.52, 0.48),
            RoadStyle::Neon => Color::srgb(0.25, 1.00, 0.92),
            RoadStyle::Dirt => Color::srgb(0.62, 0.45, 0.28),
        }
    }
    pub fn all() -> [RoadStyle; 4] {
        [
            RoadStyle::Asphalt,
            RoadStyle::Cobble,
            RoadStyle::Neon,
            RoadStyle::Dirt,
        ]
    }
}

/// Thematic tag applied to a disc-shaped area on the ground. Currently
/// visual-only (coloured ring gizmo). Later cuts fill the disc with
/// style-specific prefabs, control vegetation, spawn traffic props etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistrictKind {
    Residential,
    Commercial,
    Industrial,
    Nature,
    Wasteland,
}

impl DistrictKind {
    pub fn label(self) -> &'static str {
        match self {
            DistrictKind::Residential => "Wohn",
            DistrictKind::Commercial => "Gewerbe",
            DistrictKind::Industrial => "Industrie",
            DistrictKind::Nature => "Natur",
            DistrictKind::Wasteland => "Ödland",
        }
    }
    pub fn color(self) -> Color {
        match self {
            DistrictKind::Residential => Color::srgb(0.90, 0.70, 0.35),
            DistrictKind::Commercial => Color::srgb(0.30, 0.75, 1.00),
            DistrictKind::Industrial => Color::srgb(0.80, 0.35, 0.25),
            DistrictKind::Nature => Color::srgb(0.30, 0.85, 0.40),
            DistrictKind::Wasteland => Color::srgb(0.55, 0.55, 0.52),
        }
    }
    pub fn all() -> [DistrictKind; 5] {
        [
            DistrictKind::Residential,
            DistrictKind::Commercial,
            DistrictKind::Industrial,
            DistrictKind::Nature,
            DistrictKind::Wasteland,
        ]
    }
}

/// Semantic shape for a placed road component. Straight is the cheap
/// axis-aligned case; Corner keeps a two-leg turn as one editable object;
/// Roundabout is reserved for circular junction components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RoadShape {
    #[default]
    Straight,
    Corner,
    Roundabout,
}

impl RoadShape {
    pub fn label(self) -> &'static str {
        match self {
            RoadShape::Straight => "Gerade",
            RoadShape::Corner => "Kurve",
            RoadShape::Roundabout => "Kreisel",
        }
    }
}

/// Placed road component. Kept in-memory for gizmo drawing, direct edits,
/// road-snap queries, and a compact per-world city road save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoadSegment {
    pub a: IVec3,
    pub b: IVec3,
    pub via: Option<IVec3>,
    pub shape: RoadShape,
    pub roundabout_radius: u8,
    pub width: u8,
    pub style: RoadStyle,
    pub elevation_a: i16,
    pub elevation_via: i16,
    pub elevation_b: i16,
}

impl RoadSegment {
    pub fn new(a: IVec3, b: IVec3, width: u8, style: RoadStyle) -> Self {
        if a.x == b.x && a.z == b.z {
            let radius = ((width.clamp(1, 17) as i32) * 2).clamp(4, 48) as u8;
            return Self::roundabout(a, radius, width, style);
        }

        Self {
            a,
            b,
            via: None,
            shape: RoadShape::Straight,
            roundabout_radius: 0,
            width: 1,
            style: RoadStyle::Asphalt,
            elevation_a: 0,
            elevation_via: 0,
            elevation_b: 0,
        }
        .with_width(width)
        .retextured(style)
        .with_endpoint_heights(0, 0)
        .with_smart_shape()
    }

    pub fn roundabout(center: IVec3, radius: u8, width: u8, style: RoadStyle) -> Self {
        let radius = radius.clamp(4, 48);
        Self {
            a: center,
            b: IVec3::new(center.x + radius as i32, center.y, center.z),
            via: None,
            shape: RoadShape::Roundabout,
            roundabout_radius: radius,
            width: 1,
            style: RoadStyle::Asphalt,
            elevation_a: 0,
            elevation_via: 0,
            elevation_b: 0,
        }
        .with_width(width)
        .retextured(style)
        .with_endpoint_heights(0, 0)
    }

    pub fn with_width(mut self, width: u8) -> Self {
        self.width = width.clamp(1, 17);
        self
    }

    pub fn retextured(mut self, style: RoadStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_endpoint_heights(mut self, a: i16, b: i16) -> Self {
        self.elevation_a = a.clamp(-12, 48);
        self.elevation_b = b.clamp(-12, 48);
        self
    }

    pub fn with_turn_height(mut self, via: i16) -> Self {
        self.elevation_via = via.clamp(-12, 48);
        self
    }

    fn with_smart_shape(mut self) -> Self {
        if self.a.x != self.b.x && self.a.z != self.b.z {
            self.shape = RoadShape::Corner;
            self.via = Some(IVec3::new(self.b.x, self.a.y, self.a.z));
            self.roundabout_radius = 0;
        }
        self
    }
}

const CITY_ROAD_SAVE_VERSION: u32 = 1;

fn city_road_save_version() -> u32 {
    CITY_ROAD_SAVE_VERSION
}

fn default_saved_road_width() -> u8 {
    3
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CityRoadSave {
    #[serde(default = "city_road_save_version")]
    version: u32,
    #[serde(default)]
    roads: Vec<SavedRoadSegment>,
}

impl CityRoadSave {
    fn from_roads(roads: &[RoadSegment]) -> Self {
        Self {
            version: CITY_ROAD_SAVE_VERSION,
            roads: roads.iter().copied().map(SavedRoadSegment::from).collect(),
        }
    }

    fn into_roads(self) -> Vec<RoadSegment> {
        self.roads.into_iter().map(RoadSegment::from).collect()
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct SavedRoadSegment {
    a: [i32; 3],
    b: [i32; 3],
    #[serde(default)]
    via: Option<[i32; 3]>,
    #[serde(default)]
    shape: RoadShape,
    #[serde(default)]
    roundabout_radius: u8,
    #[serde(default = "default_saved_road_width")]
    width: u8,
    #[serde(default)]
    style: RoadStyle,
    #[serde(default)]
    elevation_a: i16,
    #[serde(default)]
    elevation_via: i16,
    #[serde(default)]
    elevation_b: i16,
}

impl From<RoadSegment> for SavedRoadSegment {
    fn from(road: RoadSegment) -> Self {
        Self {
            a: ivec3_to_array(road.a),
            b: ivec3_to_array(road.b),
            via: road.via.map(ivec3_to_array),
            shape: road.shape,
            roundabout_radius: road.roundabout_radius,
            width: road.width,
            style: road.style,
            elevation_a: road.elevation_a,
            elevation_via: road.elevation_via,
            elevation_b: road.elevation_b,
        }
    }
}

impl From<SavedRoadSegment> for RoadSegment {
    fn from(saved: SavedRoadSegment) -> Self {
        let mut road = RoadSegment {
            a: array_to_ivec3(saved.a),
            b: array_to_ivec3(saved.b),
            via: saved.via.map(array_to_ivec3),
            shape: saved.shape,
            roundabout_radius: saved.roundabout_radius,
            width: saved.width.clamp(1, 17),
            style: saved.style,
            elevation_a: saved.elevation_a.clamp(-12, 48),
            elevation_via: saved.elevation_via.clamp(-12, 48),
            elevation_b: saved.elevation_b.clamp(-12, 48),
        };

        match road.shape {
            RoadShape::Corner => {
                if road.via.is_none() {
                    road.via = Some(IVec3::new(road.b.x, road.a.y, road.a.z));
                }
                road.roundabout_radius = 0;
            }
            RoadShape::Roundabout => {
                let fallback = ((road.width as i32) * 2).clamp(4, 48) as u8;
                let radius = if road.roundabout_radius == 0 {
                    fallback
                } else {
                    road.roundabout_radius
                };
                road.roundabout_radius = radius.clamp(4, 48);
                road.b = IVec3::new(road.a.x + road.roundabout_radius as i32, road.a.y, road.a.z);
                road.via = None;
            }
            RoadShape::Straight => {
                road.via = None;
                road.roundabout_radius = 0;
            }
        }
        road
    }
}

fn ivec3_to_array(p: IVec3) -> [i32; 3] {
    [p.x, p.y, p.z]
}

fn array_to_ivec3(p: [i32; 3]) -> IVec3 {
    IVec3::new(p[0], p[1], p[2])
}

pub fn save_city_roads_for_world(world_name: &str, roads: &[RoadSegment]) {
    let save = CityRoadSave::from_roads(roads);
    let Ok(text) = ron::ser::to_string_pretty(&save, ron::ser::PrettyConfig::default()) else {
        warn!("city roads: failed serialising road components for '{world_name}'");
        return;
    };

    #[cfg(target_arch = "wasm32")]
    {
        if let Err(e) =
            crate::platform::browser_storage_set(&browser_city_roads_key(world_name), &text)
        {
            warn!("{e}");
        }
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = city_roads_file(world_name);
        if let Err(e) = crate::settings::atomic_write_text(&path, &text) {
            warn!("city roads: failed writing {}: {e}", path.display());
        }
    }
}

pub fn load_city_roads_for_world(world_name: &str) -> Option<Vec<RoadSegment>> {
    #[cfg(target_arch = "wasm32")]
    {
        let text = crate::platform::browser_storage_get(&browser_city_roads_key(world_name))?;
        return match ron::from_str::<CityRoadSave>(&text) {
            Ok(save) => Some(save.into_roads()),
            Err(e) => {
                warn!("city roads: failed parsing browser road components: {e}");
                None
            }
        };
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = city_roads_file(world_name);
        let text = std::fs::read_to_string(&path).ok()?;
        match ron::from_str::<CityRoadSave>(&text) {
            Ok(save) => Some(save.into_roads()),
            Err(e) => {
                warn!("city roads: failed parsing {}: {e}", path.display());
                None
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_city_roads_key(world_name: &str) -> String {
    format!(
        "voxel_native.city_roads.{}",
        crate::settings::world_storage_stem(world_name)
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn city_roads_file(world_name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(crate::settings::SAVES_DIR)
        .join(format!(
            "{}_city",
            crate::settings::world_storage_stem(world_name)
        ))
        .join("roads.ron")
}

/// Decorative district marker. Acts as a visual anchor + a handle for
/// later auto-fill passes; no voxel effect yet.
#[derive(Debug, Clone, Copy)]
pub struct District {
    pub center: IVec3,
    pub radius: i32,
    pub kind: DistrictKind,
}

// ---------------------------------------------------------------------
// Buildings (CB)
// ---------------------------------------------------------------------

/// Procedural building palette. Each style maps to block roles
/// (wall / floor / roof) and a default floor-count range so the STADT
/// tab can offer "residential = 3..8, tower = 8..18" out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildingStyle {
    Residential,
    Commercial,
    Industrial,
    Tower,
}

impl BuildingStyle {
    pub fn label(self) -> &'static str {
        match self {
            BuildingStyle::Residential => "Wohnhaus",
            BuildingStyle::Commercial => "Geschaeft",
            BuildingStyle::Industrial => "Industrie",
            BuildingStyle::Tower => "Turm",
        }
    }
    pub fn wall(self) -> BlockType {
        match self {
            BuildingStyle::Residential => BlockType::Wood,
            BuildingStyle::Commercial => BlockType::Limestone,
            BuildingStyle::Industrial => BlockType::Stone,
            BuildingStyle::Tower => BlockType::Stone,
        }
    }
    pub fn floor_block(self) -> BlockType {
        match self {
            BuildingStyle::Residential => BlockType::Wood,
            BuildingStyle::Commercial => BlockType::Limestone,
            BuildingStyle::Industrial => BlockType::Gravel,
            BuildingStyle::Tower => BlockType::MossStone,
        }
    }
    pub fn roof(self) -> BlockType {
        match self {
            BuildingStyle::Residential => BlockType::RedStone,
            BuildingStyle::Commercial => BlockType::Snow,
            BuildingStyle::Industrial => BlockType::Basalt,
            BuildingStyle::Tower => BlockType::MossStone,
        }
    }
    /// Suggested floor count range `(min, max)` per style.
    pub fn default_floors(self) -> (u8, u8) {
        match self {
            BuildingStyle::Residential => (3, 6),
            BuildingStyle::Commercial => (4, 10),
            BuildingStyle::Industrial => (2, 4),
            BuildingStyle::Tower => (10, 18),
        }
    }
    pub fn gizmo_color(self) -> Color {
        match self {
            BuildingStyle::Residential => Color::srgb(0.85, 0.68, 0.35),
            BuildingStyle::Commercial => Color::srgb(0.45, 0.90, 1.00),
            BuildingStyle::Industrial => Color::srgb(0.70, 0.55, 0.35),
            BuildingStyle::Tower => Color::srgb(0.75, 0.85, 0.95),
        }
    }
    pub fn all() -> [BuildingStyle; 4] {
        [
            BuildingStyle::Residential,
            BuildingStyle::Commercial,
            BuildingStyle::Industrial,
            BuildingStyle::Tower,
        ]
    }
}

/// Placed building footprint — kept in memory so we can draw a gizmo
/// outline and eventually re-stamp / re-theme it without re-picking
/// the voxels from the world.
#[derive(Debug, Clone, Copy)]
pub struct Building {
    /// Min corner of the footprint in world space (Y = chosen ground
    /// level where the building sits).
    pub min: IVec3,
    /// Max corner of the footprint (inclusive, same Y as `min`).
    pub max: IVec3,
    pub floors: u8,
    pub style: BuildingStyle,
}

// ---------------------------------------------------------------------
// Facade library (CD)
// ---------------------------------------------------------------------

/// On-disk RON schema for a small reusable voxel prefab. Uses plain
/// arrays instead of `IVec3` so we don't depend on the bevy `serialize`
/// feature flag. See [`FacadePrefab`] for the runtime form.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg(not(target_arch = "wasm32"))]
pub struct FacadeFile {
    pub name: String,
    pub category: String,
    pub size: [i32; 3],
    /// `(x, y, z, voxel_id)` — air cells are omitted.
    pub voxels: Vec<(i32, i32, i32, u16)>,
}

/// Runtime form of a facade prefab: a tiny sparse voxel volume with a
/// name and category tag. The STADT tab lists them; the FASSADE tool
/// stamps the selected one at the cursor.
#[derive(Debug, Clone)]
pub struct FacadePrefab {
    pub name: String,
    pub category: String,
    pub size: IVec3,
    pub voxels: Vec<(IVec3, Voxel)>,
}

impl FacadePrefab {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_file(f: FacadeFile) -> Self {
        let voxels = f
            .voxels
            .into_iter()
            .map(|(x, y, z, v)| (IVec3::new(x, y, z), v as Voxel))
            .collect();
        Self {
            name: f.name,
            category: f.category,
            size: IVec3::new(f.size[0], f.size[1], f.size[2]),
            voxels,
        }
    }
}

/// World-space snapping strategy applied to every picked cell in the
/// STADT tab. Cycled with `.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnapMode {
    #[default]
    Off,
    Grid1,
    Grid4,
    Grid16,
    Road,
}

impl SnapMode {
    pub fn label(self) -> &'static str {
        match self {
            SnapMode::Off => "AUS",
            SnapMode::Grid1 => "Grid 1",
            SnapMode::Grid4 => "Grid 4",
            SnapMode::Grid16 => "Grid 16",
            SnapMode::Road => "Strassen",
        }
    }
    pub fn cycle(self) -> Self {
        match self {
            SnapMode::Off => SnapMode::Grid1,
            SnapMode::Grid1 => SnapMode::Grid4,
            SnapMode::Grid4 => SnapMode::Grid16,
            SnapMode::Grid16 => SnapMode::Road,
            SnapMode::Road => SnapMode::Off,
        }
    }
}

// ---------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------

/// Top-level state for the STADT tab. Single resource so UI + input +
/// gizmo rendering can all read / write without any event plumbing.
#[derive(Resource, Debug)]
pub struct CityState {
    pub tool: CityTool,
    pub road_style: RoadStyle,
    pub road_width: u8, // 1..=9
    pub district_kind: DistrictKind,
    pub district_radius: i32, // 2..=24
    pub building_style: BuildingStyle,
    pub building_floors: u8, // 2..=20
    pub snap: SnapMode,
    /// First click of a road in progress. Cleared when the segment
    /// commits or the user cancels with Esc.
    pub pending_road_a: Option<IVec3>,
    /// First click of a building footprint in progress.
    pub pending_building_a: Option<IVec3>,
    pub roads: Vec<RoadSegment>,
    pub roads_loaded_world: String,
    pub selected_road: Option<usize>,
    pub districts: Vec<District>,
    pub buildings: Vec<Building>,
    pub facades: Vec<FacadePrefab>,
    pub facade_selected: usize,
    pub status: String,
}

impl Default for CityState {
    fn default() -> Self {
        Self {
            tool: CityTool::None,
            road_style: RoadStyle::Asphalt,
            road_width: 3,
            district_kind: DistrictKind::Residential,
            district_radius: 6,
            building_style: BuildingStyle::Residential,
            building_floors: 4,
            snap: SnapMode::Off,
            pending_road_a: None,
            pending_building_a: None,
            roads: Vec::new(),
            roads_loaded_world: String::new(),
            selected_road: None,
            districts: Vec::new(),
            buildings: Vec::new(),
            facades: Vec::new(),
            facade_selected: 0,
            status: "Bereit.".into(),
        }
    }
}

// ---------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------

pub struct CityPlugin;

impl Plugin for CityPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CityState::default())
            .add_systems(Startup, setup_facade_library)
            .add_systems(
                Update,
                (
                    load_city_roads_for_pending_world,
                    city_input,
                    manual_save_city_roads,
                    city_draw_gizmos,
                    draw_hint_hud,
                )
                    .chain(),
            );
    }
}

/// Populate [`CityState::facades`] at startup. Always seeds a small
/// set of built-in prefabs so the library is never empty, then tries
/// to augment from `./facades/*.ron` on disk. Missing directory or
/// malformed files are silently ignored (logged at `warn!`) so the
/// game still boots into a usable state.
fn setup_facade_library(mut city: ResMut<CityState>) {
    city.facades = builtin_facades();
    #[cfg(target_arch = "wasm32")]
    {
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    match load_facade_library("./facades") {
        Ok(mut extra) => {
            let n = extra.len();
            city.facades.append(&mut extra);
            if n > 0 {
                info!("facades: loaded {} from disk", n);
            }
        }
        Err(e) => {
            // Not fatal — the built-in set keeps the tool usable.
            warn!("facades: disk scan skipped ({})", e);
        }
    }
}

fn load_city_roads_for_pending_world(
    pending: Res<crate::menu::PendingWorldLoad>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    mut city: ResMut<CityState>,
) {
    if !pending.0 {
        return;
    }
    let Some(active) = active else {
        return;
    };

    let roads = load_city_roads_for_world(&active.meta.name).unwrap_or_default();
    let count = roads.len();
    city.roads = roads;
    city.roads_loaded_world = active.meta.name.clone();
    city.selected_road = None;
    city.pending_road_a = None;
    city.pending_building_a = None;
    city.districts.clear();
    city.buildings.clear();
    city.status = if count > 0 {
        format!("{count} Strassenkomponenten geladen.")
    } else {
        "Stadtwerkzeuge bereit.".into()
    };
}

fn manual_save_city_roads(
    keys: Res<ButtonInput<KeyCode>>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    city: Res<CityState>,
) {
    if keys.just_pressed(KeyCode::F5) {
        save_city_roads_for_active(active.as_deref(), &city.roads);
    }
}

fn save_city_roads_for_active(
    active: Option<&crate::settings::ActiveWorld>,
    roads: &[RoadSegment],
) {
    if let Some(active) = active {
        save_city_roads_for_world(&active.meta.name, roads);
    }
}

// ---------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn city_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: EventReader<MouseWheel>,
    editor: Res<EditorState>,
    mode: Res<crate::mode::ModeContext>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    mut city: ResMut<CityState>,
    mut telemetry: ResMut<UnifiedTelemetry>,
    mut world: ResMut<VoxelWorld>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
) {
    let editor_city_active = editor.open && editor.tab == EditorTab::City;
    let live_city_active = mode
        .build_tool()
        .and_then(crate::toolbelt::ToolbeltTool::city_tool)
        .is_some()
        && mode.is_build_live();
    if !editor_city_active && !live_city_active {
        wheel.clear();
        return;
    }
    if live_city_active {
        let cursor_locked = windows
            .get_single()
            .map(|w| w.cursor.grab_mode == bevy::window::CursorGrabMode::Locked)
            .unwrap_or(false);
        if !cursor_locked {
            wheel.clear();
            return;
        }
    }

    // Modifier state — we use a no-mods guard so Ctrl+N etc. stay free
    // for future shortcuts (save scene, new macro, …).
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let bare = !ctrl && !shift && !alt;

    // --- Tool toggles --------------------------------------------------
    if editor_city_active && bare && keys.just_pressed(KeyCode::KeyN) {
        city.tool = if city.tool == CityTool::Road {
            CityTool::None
        } else {
            CityTool::Road
        };
        city.pending_road_a = None;
        city.pending_building_a = None;
        city.status = format!("Werkzeug: {}", city.tool.label());
    }
    if editor_city_active && bare && keys.just_pressed(KeyCode::KeyT) {
        city.tool = if city.tool == CityTool::District {
            CityTool::None
        } else {
            CityTool::District
        };
        city.pending_road_a = None;
        city.pending_building_a = None;
        city.status = format!("Werkzeug: {}", city.tool.label());
    }
    if editor_city_active && bare && keys.just_pressed(KeyCode::KeyU) {
        city.tool = if city.tool == CityTool::Building {
            CityTool::None
        } else {
            CityTool::Building
        };
        city.pending_road_a = None;
        city.pending_building_a = None;
        city.status = format!("Werkzeug: {}", city.tool.label());
    }
    if editor_city_active && bare && keys.just_pressed(KeyCode::KeyF) {
        city.tool = if city.tool == CityTool::Facade {
            CityTool::None
        } else {
            CityTool::Facade
        };
        city.pending_road_a = None;
        city.pending_building_a = None;
        city.status = format!("Werkzeug: {}", city.tool.label());
    }

    // --- Snap cycle ----------------------------------------------------
    if bare && keys.just_pressed(KeyCode::Period) {
        city.snap = city.snap.cycle();
        city.status = format!("Snap: {}", city.snap.label());
    }

    // --- Width / radius adjust ----------------------------------------
    if bare && keys.just_pressed(KeyCode::BracketLeft) {
        match city.tool {
            CityTool::Road => {
                city.road_width = (city.road_width.saturating_sub(1)).max(1);
                city.status = format!("Breite {}", city.road_width);
            }
            CityTool::District => {
                city.district_radius = (city.district_radius - 1).max(2);
                city.status = format!("Radius {}", city.district_radius);
            }
            CityTool::Building => {
                city.building_floors = city.building_floors.saturating_sub(1).max(2);
                city.status = format!("Etagen {}", city.building_floors);
            }
            CityTool::Facade | CityTool::None => {}
        }
    }
    if bare && keys.just_pressed(KeyCode::BracketRight) {
        match city.tool {
            CityTool::Road => {
                city.road_width = (city.road_width + 1).min(9);
                city.status = format!("Breite {}", city.road_width);
            }
            CityTool::District => {
                city.district_radius = (city.district_radius + 1).min(24);
                city.status = format!("Radius {}", city.district_radius);
            }
            CityTool::Building => {
                city.building_floors = (city.building_floors + 1).min(20);
                city.status = format!("Etagen {}", city.building_floors);
            }
            CityTool::Facade | CityTool::None => {}
        }
    }

    // --- Cancel -------------------------------------------------------
    if keys.just_pressed(KeyCode::Escape) {
        if city.pending_road_a.take().is_some() {
            city.status = "Strasse abgebrochen.".into();
        } else if city.pending_building_a.take().is_some() {
            city.status = "Gebaeude abgebrochen.".into();
        }
    }

    // --- Crosshair pick -----------------------------------------------
    let Ok(cam_tf) = cam_q.get_single() else {
        wheel.clear();
        return;
    };
    let origin = cam_tf.translation();
    let dir = cam_tf.forward().as_vec3();
    let picked = raycast_voxel(&world, origin, dir, 100.0).unwrap_or_else(|| {
        let fwd = origin + dir * 12.0;
        let c = IVec3::new(
            fwd.x.floor() as i32,
            fwd.y.floor() as i32,
            fwd.z.floor() as i32,
        );
        (c, c)
    });
    let (hit_cell, _adj_cell) = picked;

    // Project the hit onto its column surface so roads/districts always
    // land on terrain regardless of where the reticle was.
    let surface_y = world.surface_height_at(hit_cell.x, hit_cell.z);
    let ground = IVec3::new(hit_cell.x, surface_y, hit_cell.z);
    let snapped = if city.tool == CityTool::Road {
        road_tool_snap_cell(ground, city.snap, &city.roads)
    } else {
        snap_cell(ground, city.snap, &city.roads)
    };

    city.selected_road = if city.tool == CityTool::Road {
        nearest_road_component(&city.roads, snapped, 5.0)
    } else {
        None
    };

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if city.tool == CityTool::Road && wheel_delta.abs() > f32::EPSILON {
        let mut steps = wheel_delta.round() as i32;
        if steps == 0 {
            steps = wheel_delta.signum() as i32;
        }
        steps = steps.clamp(-4, 4);

        if let Some(idx) = city.selected_road {
            let before = city.roads[idx];
            let mut next = before;
            let mut label = None;

            if ctrl && !shift && !alt {
                next = road_with_size_delta(before, steps * 2);
                label = Some(if next.shape == RoadShape::Roundabout {
                    format!("Radius {}", next.roundabout_radius)
                } else {
                    format!("Breite {}", next.width)
                });
            } else if shift && !ctrl && !alt {
                next = road_with_endpoint_height_delta(before, snapped, (steps * 2) as i16);
                label = Some(if next.shape == RoadShape::Corner {
                    format!(
                        "Hoehe A/T/B {}:{}:{}",
                        next.elevation_a, next.elevation_via, next.elevation_b
                    )
                } else {
                    format!("Hoehe A/B {}:{}", next.elevation_a, next.elevation_b)
                });
            } else if alt && !ctrl && !shift {
                next = before.retextured(next_road_style(before.style, steps));
                label = Some(format!("Textur {}", next.style.label()));
            }

            if let Some(label) = label {
                let n = restamp_road_component(&mut world, &before, &next);
                city.roads[idx] = next;
                sync_road_brush_from_component(&mut city, next);
                save_city_roads_for_active(active.as_deref(), &city.roads);
                city.status = format!("Strassenkomponente {}: {} ({} Bloecke)", idx + 1, label, n);
                telemetry.city_actions = telemetry.city_actions.saturating_add(1);
                telemetry.build_blocks_changed =
                    telemetry.build_blocks_changed.saturating_add(n as u64);
                return;
            }
        } else if ctrl || shift || alt {
            city.status = "Strassen-Edit: auf eine Strassenkomponente zielen.".into();
        }
    }

    // --- Mouse: commit action -----------------------------------------
    if bare && mouse.just_pressed(MouseButton::Left) {
        match city.tool {
            CityTool::Road => match city.pending_road_a {
                None => {
                    city.pending_road_a = Some(snapped);
                    city.status =
                        format!("Start @ {},{} — 2. Klick setzt Ende.", snapped.x, snapped.z);
                }
                Some(a) => {
                    let target = smart_road_drag_target(a, snapped, &city.roads);
                    let seg = road_segment_from_drag(
                        a,
                        target,
                        city.road_width,
                        city.road_style,
                        &city.roads,
                    );
                    let n = stamp_road(&mut world, &seg);
                    city.roads.push(seg);
                    save_city_roads_for_active(active.as_deref(), &city.roads);
                    city.road_width = seg.width;
                    city.road_style = seg.style;
                    city.pending_road_a = road_continuation_start(&seg);
                    city.status = if let Some(next) = city.pending_road_a {
                        format!(
                            "Strasse {} {} ({} Bloecke) - weiter ab {},{}.",
                            seg.shape.label(),
                            seg.style.label(),
                            n,
                            next.x,
                            next.z
                        )
                    } else {
                        format!(
                            "Strasse {} {} ({} Bloecke)",
                            seg.shape.label(),
                            seg.style.label(),
                            n
                        )
                    };
                    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
                    telemetry.build_blocks_changed =
                        telemetry.build_blocks_changed.saturating_add(n as u64);
                }
            },
            CityTool::District => {
                let d = District {
                    center: snapped,
                    radius: city.district_radius,
                    kind: city.district_kind,
                };
                city.districts.push(d);
                city.status = format!(
                    "Bezirk {} r={}",
                    city.district_kind.label(),
                    city.district_radius
                );
                telemetry.city_actions = telemetry.city_actions.saturating_add(1);
            }
            CityTool::Building => match city.pending_building_a {
                None => {
                    city.pending_building_a = Some(snapped);
                    city.status = format!(
                        "Gebaeudeecke A @ {},{} — 2. Klick setzt gegenueberliegende Ecke.",
                        snapped.x, snapped.z
                    );
                }
                Some(a) => {
                    let min = IVec3::new(a.x.min(snapped.x), a.y, a.z.min(snapped.z));
                    let max = IVec3::new(a.x.max(snapped.x), a.y, a.z.max(snapped.z));
                    let bld = Building {
                        min,
                        max,
                        floors: city.building_floors,
                        style: city.building_style,
                    };
                    let n = stamp_building(&mut world, &bld);
                    city.buildings.push(bld);
                    city.pending_building_a = None;
                    city.status = format!(
                        "{} {}x{} @ {} Etagen ({} Bloecke) — Rohbau ohne Fenster; Einschnitte im BAUEN-Tab selbst schneiden.",
                        city.building_style.label(),
                        max.x - min.x + 1,
                        max.z - min.z + 1,
                        city.building_floors,
                        n
                    );
                    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
                    telemetry.build_blocks_changed =
                        telemetry.build_blocks_changed.saturating_add(n as u64);
                }
            },
            CityTool::Facade => {
                if city.facades.is_empty() {
                    city.status = "Keine Fassaden geladen.".into();
                } else {
                    let idx = city.facade_selected.min(city.facades.len() - 1);
                    let prefab = city.facades[idx].clone();
                    let n = stamp_facade(&mut world, snapped, &prefab);
                    city.status =
                        format!("Fassade \"{}\" gestempelt ({} Bloecke).", prefab.name, n);
                    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
                    telemetry.build_blocks_changed =
                        telemetry.build_blocks_changed.saturating_add(n as u64);
                }
            }
            CityTool::None => {}
        }
    }

    // --- Mouse RMB: delete last of the active tool --------------------
    if bare && mouse.just_pressed(MouseButton::Right) {
        match city.tool {
            CityTool::Road => {
                if city.pending_road_a.take().is_some() {
                    city.status = "Startpunkt verworfen.".into();
                } else if let Some(idx) = city
                    .selected_road
                    .filter(|idx| *idx < city.roads.len())
                    .or_else(|| city.roads.len().checked_sub(1))
                {
                    let changed =
                        delete_road_component(&mut world, &mut city.roads, idx).unwrap_or_default();
                    city.selected_road = None;
                    save_city_roads_for_active(active.as_deref(), &city.roads);
                    city.status = format!(
                        "Strassenkomponente {} geloescht ({} Bloecke bereinigt).",
                        idx + 1,
                        changed
                    );
                    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
                    telemetry.build_blocks_changed = telemetry
                        .build_blocks_changed
                        .saturating_add(changed as u64);
                }
            }
            CityTool::District => {
                if city.districts.pop().is_some() {
                    city.status = "Letzter Bezirk entfernt.".into();
                }
            }
            CityTool::Building => {
                if city.pending_building_a.take().is_some() {
                    city.status = "Gebaeudeecke verworfen.".into();
                } else if city.buildings.pop().is_some() {
                    city.status = "Letztes Gebaeude aus Liste entfernt (Voxel bleiben).".into();
                }
            }
            CityTool::Facade => {
                // Facade stamps are one-shot and not tracked — nothing
                // to undo beyond the normal builder undo stack.
            }
            CityTool::None => {}
        }
    }
}

// ---------------------------------------------------------------------
// Snap helpers
// ---------------------------------------------------------------------

fn round_to(n: i32, step: i32) -> i32 {
    let half = step / 2;
    if n >= 0 {
        ((n + half) / step) * step
    } else {
        -(((-n + half) / step) * step)
    }
}

/// Snap a world cell to the active [`SnapMode`].
fn snap_cell(p: IVec3, mode: SnapMode, roads: &[RoadSegment]) -> IVec3 {
    match mode {
        SnapMode::Off | SnapMode::Grid1 => p,
        SnapMode::Grid4 => IVec3::new(round_to(p.x, 4), p.y, round_to(p.z, 4)),
        SnapMode::Grid16 => IVec3::new(round_to(p.x, 16), p.y, round_to(p.z, 16)),
        SnapMode::Road => {
            // Find the nearest point on any existing road within 8
            // blocks. Falls back to p if nothing is close.
            let point = Vec2::new(p.x as f32 + 0.5, p.z as f32 + 0.5);
            if let Some(handle) = nearest_road_snap_handle(roads, point, 4.0) {
                return IVec3::new(handle.x, p.y, handle.z);
            }
            let mut best: Option<(f32, Vec2)> = None;
            for r in roads {
                let Some((q, d2)) = road_nearest_point_xz(*r, point) else {
                    continue;
                };
                if d2 < 64.0 && best.map_or(true, |(bd, _)| d2 < bd) {
                    best = Some((d2, q));
                }
            }
            match best {
                Some((_, q)) => IVec3::new(q.x.floor() as i32, p.y, q.y.floor() as i32),
                None => p,
            }
        }
    }
}

fn road_tool_snap_cell(p: IVec3, mode: SnapMode, roads: &[RoadSegment]) -> IVec3 {
    let base = snap_cell(p, mode, roads);
    contextual_road_snap_cell(base, roads).unwrap_or(base)
}

fn contextual_road_snap_cell(p: IVec3, roads: &[RoadSegment]) -> Option<IVec3> {
    let point = Vec2::new(p.x as f32 + 0.5, p.z as f32 + 0.5);
    if let Some(handle) = nearest_road_snap_handle(roads, point, 4.0) {
        return Some(IVec3::new(handle.x, p.y, handle.z));
    }
    nearest_road_path_cell(roads, point, 4.5).map(|cell| IVec3::new(cell.x, p.y, cell.y))
}

fn nearest_road_path_cell(roads: &[RoadSegment], point: Vec2, max_distance: f32) -> Option<IVec2> {
    let max_d2 = max_distance * max_distance;
    let mut best: Option<(f32, Vec2)> = None;
    for road in roads {
        let Some((q, d2)) = road_nearest_point_xz(*road, point) else {
            continue;
        };
        if d2 <= max_d2 && best.map_or(true, |(best_d2, _)| d2 < best_d2) {
            best = Some((d2, q));
        }
    }
    best.map(|(_, q)| IVec2::new(q.x.floor() as i32, q.y.floor() as i32))
}

const SMART_ROAD_AXIS_JITTER: i32 = 3;
const SMART_ROAD_AXIS_RATIO: i32 = 3;
const SMART_ROAD_LENGTH_TOLERANCE: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoadDragAxis {
    X,
    Z,
}

fn smart_road_drag_target(start: IVec3, raw: IVec3, roads: &[RoadSegment]) -> IVec3 {
    let dx = raw.x - start.x;
    let dz = raw.z - start.z;
    let Some(axis) = dominant_road_drag_axis(dx, dz) else {
        return raw;
    };

    let mut target = raw;
    let raw_len = match axis {
        RoadDragAxis::X => {
            target.z = start.z;
            dx.abs()
        }
        RoadDragAxis::Z => {
            target.x = start.x;
            dz.abs()
        }
    };
    if raw_len == 0 {
        return raw;
    }

    if let Some(span) = matching_reference_road_span(raw_len, roads) {
        match axis {
            RoadDragAxis::X => target.x = start.x + dx.signum() * span,
            RoadDragAxis::Z => target.z = start.z + dz.signum() * span,
        }
    }
    target
}

fn dominant_road_drag_axis(dx: i32, dz: i32) -> Option<RoadDragAxis> {
    let ax = dx.abs();
    let az = dz.abs();
    if ax == 0 && az == 0 {
        return None;
    }
    if ax >= az && (az <= SMART_ROAD_AXIS_JITTER || ax >= az.saturating_mul(SMART_ROAD_AXIS_RATIO))
    {
        return Some(RoadDragAxis::X);
    }
    if az >= ax && (ax <= SMART_ROAD_AXIS_JITTER || az >= ax.saturating_mul(SMART_ROAD_AXIS_RATIO))
    {
        return Some(RoadDragAxis::Z);
    }
    None
}

fn matching_reference_road_span(raw_len: i32, roads: &[RoadSegment]) -> Option<i32> {
    let mut best: Option<(i32, i32)> = None;
    for road in roads.iter().rev().take(8) {
        visit_road_reference_spans(*road, |span| {
            if span < 4 {
                return;
            }
            let delta = (span - raw_len).abs();
            if delta <= SMART_ROAD_LENGTH_TOLERANCE
                && best.map_or(true, |(best_delta, _)| delta < best_delta)
            {
                best = Some((delta, span));
            }
        });
    }
    best.map(|(_, span)| span)
}

fn visit_road_reference_spans(road: RoadSegment, mut visit: impl FnMut(i32)) {
    match road.shape {
        RoadShape::Straight => {
            visit((road.b.x - road.a.x).abs().max((road.b.z - road.a.z).abs()));
        }
        RoadShape::Corner => {
            let via = road_corner_via(road);
            visit((via.x - road.a.x).abs() + (via.z - road.a.z).abs());
            visit((road.b.x - via.x).abs() + (road.b.z - via.z).abs());
        }
        RoadShape::Roundabout => {
            visit(road.roundabout_radius.max(4) as i32 * 2);
        }
    }
}

fn road_continuation_start(road: &RoadSegment) -> Option<IVec3> {
    match road.shape {
        RoadShape::Roundabout => None,
        RoadShape::Straight | RoadShape::Corner => Some(road.b),
    }
}

fn road_segment_from_drag(
    start: IVec3,
    target: IVec3,
    width: u8,
    style: RoadStyle,
    roads: &[RoadSegment],
) -> RoadSegment {
    let start_sample = road_connection_sample_at(roads, start);
    let target_sample = road_connection_sample_at(roads, target);
    let (width, style) = road_drag_appearance(start_sample, target_sample, width, style);
    let mut segment = RoadSegment::new(start, target, width, style);
    let start_height = start_sample.map(|sample| sample.elevation);
    let end_height = target_sample.map(|sample| sample.elevation);
    let a = start_height.unwrap_or(0);
    let b = end_height.unwrap_or(a);
    segment = segment.with_endpoint_heights(a, b);
    if segment.shape == RoadShape::Corner {
        let via = road_corner_via(segment);
        let turn = road_handle_height_at(roads, via).unwrap_or(((a as i32 + b as i32) / 2) as i16);
        segment = segment.with_turn_height(turn);
    }
    segment
}

fn sync_road_brush_from_component(city: &mut CityState, road: RoadSegment) {
    city.road_width = road.width;
    city.road_style = road.style;
}

fn road_drag_appearance(
    start_sample: Option<RoadConnectionSample>,
    target_sample: Option<RoadConnectionSample>,
    fallback_width: u8,
    fallback_style: RoadStyle,
) -> (u8, RoadStyle) {
    start_sample
        .or(target_sample)
        .map(|sample| (sample.width, sample.style))
        .unwrap_or((fallback_width, fallback_style))
}

#[derive(Debug, Clone, Copy)]
struct RoadConnectionSample {
    width: u8,
    style: RoadStyle,
    elevation: i16,
}

fn road_connection_sample_at(roads: &[RoadSegment], cell: IVec3) -> Option<RoadConnectionSample> {
    roads
        .iter()
        .rev()
        .find_map(|road| road_connection_sample(*road, cell))
}

fn road_handle_height_at(roads: &[RoadSegment], handle: IVec3) -> Option<i16> {
    roads
        .iter()
        .rev()
        .find_map(|road| road_handle_height(*road, handle))
}

fn road_connection_sample(road: RoadSegment, cell: IVec3) -> Option<RoadConnectionSample> {
    if let Some(elevation) = road_handle_height(road, cell) {
        return Some(RoadConnectionSample {
            width: road.width,
            style: road.style,
            elevation,
        });
    }
    let cells = road_path_xz(&road);
    let idx = cells
        .iter()
        .position(|candidate| candidate.x == cell.x && candidate.y == cell.z)?;
    Some(RoadConnectionSample {
        width: road.width,
        style: road.style,
        elevation: road_elevation_at_sample(&road, idx, cells.len().saturating_sub(1)) as i16,
    })
}

fn road_handle_height(road: RoadSegment, handle: IVec3) -> Option<i16> {
    match road.shape {
        RoadShape::Straight => {
            if same_road_handle_xz(handle, road.a) {
                Some(road.elevation_a)
            } else if same_road_handle_xz(handle, road.b) {
                Some(road.elevation_b)
            } else {
                None
            }
        }
        RoadShape::Corner => {
            let via = road_corner_via(road);
            if same_road_handle_xz(handle, road.a) {
                Some(road.elevation_a)
            } else if same_road_handle_xz(handle, via) {
                Some(road.elevation_via)
            } else if same_road_handle_xz(handle, road.b) {
                Some(road.elevation_b)
            } else {
                None
            }
        }
        RoadShape::Roundabout => {
            let r = road.roundabout_radius.max(4) as i32;
            let handles = [
                road.a,
                IVec3::new(road.a.x + r, road.a.y, road.a.z),
                IVec3::new(road.a.x - r, road.a.y, road.a.z),
                IVec3::new(road.a.x, road.a.y, road.a.z + r),
                IVec3::new(road.a.x, road.a.y, road.a.z - r),
            ];
            handles
                .iter()
                .any(|candidate| same_road_handle_xz(handle, *candidate))
                .then_some(((road.elevation_a as i32 + road.elevation_b as i32) / 2) as i16)
        }
    }
}

fn same_road_handle_xz(a: IVec3, b: IVec3) -> bool {
    a.x == b.x && a.z == b.z
}

fn nearest_road_snap_handle(
    roads: &[RoadSegment],
    point: Vec2,
    max_distance: f32,
) -> Option<IVec3> {
    let max_d2 = max_distance * max_distance;
    let mut best: Option<(f32, IVec3)> = None;
    for road in roads {
        visit_road_snap_handles(*road, |handle| {
            let d2 = point.distance_squared(road_point_xz(handle));
            if d2 <= max_d2 && best.map_or(true, |(best_d2, _)| d2 < best_d2) {
                best = Some((d2, handle));
            }
        });
    }
    best.map(|(_, handle)| handle)
}

fn visit_road_snap_handles(mut road: RoadSegment, mut visit: impl FnMut(IVec3)) {
    match road.shape {
        RoadShape::Straight => {
            visit(road.a);
            visit(road.b);
        }
        RoadShape::Corner => {
            visit(road.a);
            visit(road_corner_via(road));
            visit(road.b);
        }
        RoadShape::Roundabout => {
            let r = road.roundabout_radius.max(4) as i32;
            road.b = IVec3::new(road.a.x + r, road.a.y, road.a.z);
            visit(road.a);
            visit(road.b);
            visit(IVec3::new(road.a.x - r, road.a.y, road.a.z));
            visit(IVec3::new(road.a.x, road.a.y, road.a.z + r));
            visit(IVec3::new(road.a.x, road.a.y, road.a.z - r));
        }
    }
}

fn nearest_road_component(roads: &[RoadSegment], p: IVec3, max_distance: f32) -> Option<usize> {
    let point = Vec2::new(p.x as f32 + 0.5, p.z as f32 + 0.5);
    let mut best: Option<(usize, f32)> = None;
    for (idx, road) in roads.iter().enumerate() {
        let distance = road_distance_xz(*road, point);
        let pick_radius = max_distance + road.width as f32 * 0.5;
        if distance <= pick_radius
            && best.map_or(true, |(_, best_distance)| distance < best_distance)
        {
            best = Some((idx, distance));
        }
    }
    best.map(|(idx, _)| idx)
}

fn road_distance_xz(road: RoadSegment, point: Vec2) -> f32 {
    road_nearest_point_xz(road, point)
        .map(|(_, d2)| d2.sqrt())
        .unwrap_or(f32::MAX)
}

fn road_nearest_point_xz(road: RoadSegment, point: Vec2) -> Option<(Vec2, f32)> {
    match road.shape {
        RoadShape::Straight => {
            let a = road_point_xz(road.a);
            let b = road_point_xz(road.b);
            Some(point_segment_nearest_xz(a, b, point))
        }
        RoadShape::Corner => {
            let via = road_corner_via(road);
            road_segments_nearest_point_xz(&smooth_corner_segments_xz(road.a, via, road.b), point)
        }
        RoadShape::Roundabout => {
            let center = road_point_xz(road.a);
            let radius = road.roundabout_radius.max(4) as f32;
            let from_center = point - center;
            let distance = from_center.length();
            let nearest = if distance <= f32::EPSILON {
                center + Vec2::X * radius
            } else {
                center + from_center / distance * radius
            };
            Some((nearest, nearest.distance_squared(point)))
        }
    }
}

fn road_point_xz(cell: IVec3) -> Vec2 {
    Vec2::new(cell.x as f32 + 0.5, cell.z as f32 + 0.5)
}

fn point_segment_nearest_xz(a: Vec2, b: Vec2, point: Vec2) -> (Vec2, f32) {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= f32::EPSILON {
        return (a, point.distance_squared(a));
    }
    let t = ((point - a).dot(ab) / len2).clamp(0.0, 1.0);
    let nearest = a + ab * t;
    (nearest, point.distance_squared(nearest))
}

fn road_segments_nearest_point_xz(segments: &[(IVec2, IVec2)], point: Vec2) -> Option<(Vec2, f32)> {
    let mut best: Option<(Vec2, f32)> = None;
    for (a, b) in segments {
        let candidate = point_segment_nearest_xz(road_cell_xz(*a), road_cell_xz(*b), point);
        if best.map_or(true, |(_, best_d2)| candidate.1 < best_d2) {
            best = Some(candidate);
        }
    }
    best
}

fn road_cell_xz(cell: IVec2) -> Vec2 {
    Vec2::new(cell.x as f32 + 0.5, cell.y as f32 + 0.5)
}

fn road_corner_via(road: RoadSegment) -> IVec3 {
    road.via
        .unwrap_or_else(|| IVec3::new(road.b.x, road.a.y, road.a.z))
}

fn road_with_endpoint_height_delta(road: RoadSegment, cursor: IVec3, delta: i16) -> RoadSegment {
    let p = Vec2::new(cursor.x as f32 + 0.5, cursor.z as f32 + 0.5);
    let a = Vec2::new(road.a.x as f32 + 0.5, road.a.z as f32 + 0.5);
    let b = Vec2::new(road.b.x as f32 + 0.5, road.b.z as f32 + 0.5);
    let shifted = |height: i16| -> i16 { (height as i32 + delta as i32).clamp(-12, 48) as i16 };
    if road.shape == RoadShape::Corner {
        let via = road_corner_via(road);
        let via = Vec2::new(via.x as f32 + 0.5, via.z as f32 + 0.5);
        let da = p.distance_squared(a);
        let db = p.distance_squared(b);
        let dv = p.distance_squared(via);
        if dv <= da && dv <= db {
            return road.with_turn_height(shifted(road.elevation_via));
        }
    }
    if p.distance_squared(b) <= p.distance_squared(a) {
        road.with_endpoint_heights(road.elevation_a, shifted(road.elevation_b))
    } else {
        road.with_endpoint_heights(shifted(road.elevation_a), road.elevation_b)
    }
}

fn road_with_size_delta(road: RoadSegment, delta: i32) -> RoadSegment {
    if road.shape == RoadShape::Roundabout {
        let mut next = road;
        next.roundabout_radius = (road.roundabout_radius as i32 + delta).clamp(4, 48) as u8;
        next.b = IVec3::new(next.a.x + next.roundabout_radius as i32, next.a.y, next.a.z);
        next
    } else {
        road.with_width((road.width as i32 + delta).clamp(1, 17) as u8)
    }
}

fn next_road_style(style: RoadStyle, steps: i32) -> RoadStyle {
    let all = RoadStyle::all();
    let index = all
        .iter()
        .position(|candidate| *candidate == style)
        .unwrap_or(0) as i32;
    let next = (index + steps).rem_euclid(all.len() as i32) as usize;
    all[next]
}

// ---------------------------------------------------------------------
// Road stamping
// ---------------------------------------------------------------------

/// 2-D Bresenham line in the XZ plane. Inclusive of both endpoints.
fn line_xz(a: IVec2, b: IVec2) -> Vec<IVec2> {
    let mut out = Vec::new();
    let (mut x, mut y) = (a.x, a.y);
    let (x1, y1) = (b.x, b.y);
    let dx = (x1 - x).abs();
    let dy = -(y1 - y).abs();
    let sx = if x < x1 { 1 } else { -1 };
    let sy = if y < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        out.push(IVec2::new(x, y));
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
        if out.len() > 4096 {
            break; // hard guard against runaway
        }
    }
    out
}

fn road_path_xz(seg: &RoadSegment) -> Vec<IVec2> {
    match seg.shape {
        RoadShape::Straight => line_xz(IVec2::new(seg.a.x, seg.a.z), IVec2::new(seg.b.x, seg.b.z)),
        RoadShape::Corner => {
            let via = road_corner_via(*seg);
            smooth_corner_path_xz(seg.a, via, seg.b)
        }
        RoadShape::Roundabout => roundabout_path_xz(seg.a, seg.roundabout_radius),
    }
}

pub(crate) fn road_component_centerline_xz(seg: &RoadSegment) -> Vec<IVec2> {
    road_path_xz(seg)
}

fn smooth_corner_path_xz(a: IVec3, via: IVec3, b: IVec3) -> Vec<IVec2> {
    let segments = smooth_corner_segments_xz(a, via, b);
    let mut cells = Vec::new();
    for (a, b) in segments {
        append_path_unique(&mut cells, line_xz(a, b));
    }
    cells
}

fn smooth_corner_segments_xz(a: IVec3, via: IVec3, b: IVec3) -> Vec<(IVec2, IVec2)> {
    let a2 = IVec2::new(a.x, a.z);
    let via2 = IVec2::new(via.x, via.z);
    let b2 = IVec2::new(b.x, b.z);
    let len_a = (via2.x - a2.x).abs() + (via2.y - a2.y).abs();
    let len_b = (b2.x - via2.x).abs() + (b2.y - via2.y).abs();
    let turn = len_a.min(len_b).min(6);
    if turn < 3 {
        return vec![(a2, via2), (via2, b2)];
    }

    let step_from_a = IVec2::new((via2.x - a2.x).signum(), (via2.y - a2.y).signum());
    let step_to_b = IVec2::new((b2.x - via2.x).signum(), (b2.y - via2.y).signum());
    let turn_start = via2 - step_from_a * turn;
    let turn_end = via2 + step_to_b * turn;
    vec![(a2, turn_start), (turn_start, turn_end), (turn_end, b2)]
}

fn append_path_unique(into: &mut Vec<IVec2>, mut path: Vec<IVec2>) {
    if path.is_empty() {
        return;
    }
    if into.last().copied() == path.first().copied() {
        path.remove(0);
    }
    into.extend(path);
}

fn roundabout_path_xz(center: IVec3, radius: u8) -> Vec<IVec2> {
    let radius = radius.clamp(4, 48) as f32;
    let samples = ((radius as usize) * 12).clamp(48, 768);
    let mut cells = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f32 / samples as f32 * std::f32::consts::TAU;
        let cell = IVec2::new(
            center.x + (t.cos() * radius).round() as i32,
            center.z + (t.sin() * radius).round() as i32,
        );
        if cells.last().copied() != Some(cell) && !cells.contains(&cell) {
            cells.push(cell);
        }
    }
    if let Some(first) = cells.first().copied() {
        cells.push(first);
    }
    cells
}

fn road_width_axis_at(cells: &[IVec2], index: usize) -> (i32, i32) {
    let prev = if index > 0 {
        cells[index - 1]
    } else {
        cells[index]
    };
    let next = cells
        .get(index + 1)
        .copied()
        .unwrap_or_else(|| cells[index]);
    let dx = next.x - prev.x;
    let dz = next.y - prev.y;
    if dx.abs() >= dz.abs() {
        (0, 1)
    } else {
        (1, 0)
    }
}

fn restamp_road_component(
    world: &mut VoxelWorld,
    before: &RoadSegment,
    after: &RoadSegment,
) -> usize {
    clear_road_component(world, before) + stamp_road(world, after)
}

fn delete_road_component(
    world: &mut VoxelWorld,
    roads: &mut Vec<RoadSegment>,
    idx: usize,
) -> Option<usize> {
    if idx >= roads.len() {
        return None;
    }
    let road = roads.remove(idx);
    Some(clear_road_component(world, &road))
}

/// Remove a previously stamped road component footprint before applying
/// an edited width, texture, or height. Surface cells are restored to a
/// biome-appropriate top block; elevated decks/supports are cleared.
fn clear_road_component(world: &mut VoxelWorld, seg: &RoadSegment) -> usize {
    let cells = road_path_xz(seg);
    if cells.is_empty() {
        return 0;
    }
    let half = (seg.width as i32) / 2;
    let last_index = cells.len().saturating_sub(1);
    let mut changed = 0usize;
    for (i, c) in cells.iter().enumerate() {
        let elevation = road_elevation_at_sample(seg, i, last_index);
        let (perp_x, perp_z) = road_width_axis_at(&cells, i);
        for w in -half..=half {
            let wx = c.x + perp_x * w;
            let wz = c.y + perp_z * w;
            let sy = world.surface_height_at(wx, wz);
            let deck_y = (sy + elevation).max(1);
            if deck_y <= sy {
                let restore = terrain_surface_restore_voxel(world, wx, wz);
                for y in deck_y..=sy {
                    if world.edit_set_voxel(wx, y, wz, restore) {
                        changed += 1;
                    }
                }
            } else {
                if world.edit_set_voxel(wx, deck_y, wz, AIR) {
                    changed += 1;
                }
                for support_y in (sy + 1)..deck_y {
                    if world.edit_set_voxel(wx, support_y, wz, AIR) {
                        changed += 1;
                    }
                }
            }
        }
    }
    changed
}

fn terrain_surface_restore_voxel(world: &VoxelWorld, x: i32, z: i32) -> Voxel {
    let block = match world.biome_at(x, z) {
        crate::terrain::Biome::Ocean | crate::terrain::Biome::Beach => BlockType::Sand,
        crate::terrain::Biome::Desert => BlockType::Sand,
        crate::terrain::Biome::Savanna => BlockType::SavannaGrass,
        crate::terrain::Biome::Tundra => BlockType::TundraGrass,
        crate::terrain::Biome::SnowyMountains | crate::terrain::Biome::GlacierShards => {
            BlockType::Snow
        }
        crate::terrain::Biome::Mountains => BlockType::Stone,
        crate::terrain::Biome::Mesa => BlockType::RedSand,
        crate::terrain::Biome::Karst => BlockType::MossStone,
        crate::terrain::Biome::CrystalSpires => BlockType::GlowSand,
        crate::terrain::Biome::VolcanicWaste => BlockType::Basalt,
        crate::terrain::Biome::AlienReef => BlockType::AlienMoss,
        crate::terrain::Biome::Plains
        | crate::terrain::Biome::Forest
        | crate::terrain::Biome::Jungle => BlockType::Grass,
    };
    Voxel::from(block)
}

/// Stamp a road component onto the terrain surface. Returns the number
/// of voxels actually changed, so the UI can show a count.
fn stamp_road(world: &mut VoxelWorld, seg: &RoadSegment) -> usize {
    let cells = road_path_xz(seg);
    if cells.is_empty() {
        return 0;
    }
    let half = (seg.width as i32) / 2;

    let surface: Voxel = seg.style.surface_block().into();
    let stripe: Option<Voxel> = seg.style.stripe_block().map(|b| b.into());
    let support: Voxel = BlockType::Basalt.into();
    let last_index = cells.len().saturating_sub(1);

    let mut changed = 0usize;
    for (i, c) in cells.iter().enumerate() {
        let elevation = road_elevation_at_sample(seg, i, last_index);
        let (perp_x, perp_z) = road_width_axis_at(&cells, i);
        for w in -half..=half {
            let wx = c.x + perp_x * w;
            let wz = c.y + perp_z * w;
            let sy = world.surface_height_at(wx, wz);
            let deck_y = (sy + elevation).max(1);
            // Carve up to 3 blocks of air above so we don't bury the
            // road under trees / hills that just caught the edge.
            for clear_y in (deck_y + 1)..=(deck_y + 3) {
                if world.is_solid(wx, clear_y, wz) && world.edit_set_voxel(wx, clear_y, wz, AIR) {
                    changed += 1;
                }
            }
            if deck_y > sy + 1 {
                let edge_or_pier = w.abs() == half || (w == 0 && i % 5 == 0);
                if edge_or_pier {
                    for support_y in (sy + 1)..deck_y {
                        if world.edit_set_voxel(wx, support_y, wz, support) {
                            changed += 1;
                        }
                    }
                }
            } else if deck_y < sy {
                for cut_y in (deck_y + 1)..=sy {
                    if world.is_solid(wx, cut_y, wz) && world.edit_set_voxel(wx, cut_y, wz, AIR) {
                        changed += 1;
                    }
                }
            }
            if world.edit_set_voxel(wx, deck_y, wz, surface) {
                changed += 1;
            }
        }
        // Centre stripe every 3 cells along the length axis.
        if let Some(s) = stripe {
            if i % 3 == 0 {
                let sy = world.surface_height_at(c.x, c.y);
                let deck_y = (sy + elevation).max(1);
                if world.edit_set_voxel(c.x, deck_y, c.y, s) {
                    changed += 1;
                }
            }
        }
    }
    changed
}

fn road_elevation_at_sample(seg: &RoadSegment, index: usize, last_index: usize) -> i32 {
    if last_index == 0 {
        return seg.elevation_a as i32;
    }
    let t = (index as f32 / last_index as f32).clamp(0.0, 1.0);
    if seg.shape == RoadShape::Corner {
        let (start, end, local_t) = if t <= 0.5 {
            (seg.elevation_a as f32, seg.elevation_via as f32, t * 2.0)
        } else {
            (
                seg.elevation_via as f32,
                seg.elevation_b as f32,
                (t - 0.5) * 2.0,
            )
        };
        let eased = local_t * local_t * (3.0 - 2.0 * local_t);
        return (start + (end - start) * eased).round() as i32;
    }
    let eased = t * t * (3.0 - 2.0 * t);
    let start = seg.elevation_a as f32;
    let end = seg.elevation_b as f32;
    (start + (end - start) * eased).round() as i32
}

// ---------------------------------------------------------------------
// Building stamping (CB)
// ---------------------------------------------------------------------

/// Stamp a procedural rectangular building onto the terrain. Flat roof,
/// solid perimeter walls, hollow interior with floor slabs every 3
/// blocks of height. Returns the number of voxels changed.
///
/// This is **not** a WFC / facade-library placer — it's a deliberate
/// "1-click box" primitive that gets you from empty lot to skyline in
/// one move. Richer facade work happens through the FASSADE tool,
/// which pastes prefabs from [`CityState::facades`] at the cursor.
fn stamp_building(world: &mut VoxelWorld, bld: &Building) -> usize {
    let min = bld.min;
    let max = bld.max;
    if max.x < min.x || max.z < min.z {
        return 0;
    }
    // Ground level: pick the highest surface under the footprint so
    // the building sits on a pad instead of half-buried.
    let mut ground = i32::MIN;
    for wx in min.x..=max.x {
        for wz in min.z..=max.z {
            ground = ground.max(world.surface_height_at(wx, wz));
        }
    }
    if ground == i32::MIN {
        return 0;
    }

    let wall: Voxel = bld.style.wall().into();
    let floor_mat: Voxel = bld.style.floor_block().into();
    let roof: Voxel = bld.style.roof().into();

    let total_h = (bld.floors as i32) * 3;
    let mut changed = 0usize;

    for wx in min.x..=max.x {
        for wz in min.z..=max.z {
            // Level the pad: carve anything that sticks up above
            // ground within the footprint so the base is flat.
            let top = world.surface_height_at(wx, wz);
            for y in (ground + 1)..=(top + total_h + 3) {
                if world.is_solid(wx, y, wz) && world.edit_set_voxel(wx, y, wz, AIR) {
                    changed += 1;
                }
            }

            let on_x_edge = wx == min.x || wx == max.x;
            let on_z_edge = wz == min.z || wz == max.z;
            let on_perimeter = on_x_edge || on_z_edge;

            // Ground floor pad (first Y above surface).
            let base_y = ground + 1;
            if world.edit_set_voxel(wx, base_y, wz, floor_mat) {
                changed += 1;
            }

            for h in 1..total_h {
                let y = base_y + h;
                if on_perimeter {
                    let v = if voxel_on_door(h, wx, wz, &bld) {
                        AIR
                    } else {
                        wall
                    };
                    if world.edit_set_voxel(wx, y, wz, v) {
                        changed += 1;
                    }
                } else if h > 0 && (h % 3) == 0 {
                    // Interior storey floor every 3 blocks.
                    if world.edit_set_voxel(wx, y, wz, floor_mat) {
                        changed += 1;
                    }
                }
                // else: interior air — leave empty.
            }

            // Flat roof cap.
            let roof_y = base_y + total_h;
            if world.edit_set_voxel(wx, roof_y, wz, roof) {
                changed += 1;
            }
        }
    }

    changed
}

/// Heuristic door opening: carve a 1-wide gap in the middle of the
/// shorter side at ground level (h=1). Returns true when `(wx, wz, h)`
/// falls on the door tile so [`stamp_building`] can skip the wall.
fn voxel_on_door(h: i32, wx: i32, wz: i32, bld: &Building) -> bool {
    if h != 1 {
        return false;
    }
    let w = bld.max.x - bld.min.x;
    let d = bld.max.z - bld.min.z;
    if w <= 1 && d <= 1 {
        return false;
    }
    if d >= w {
        // door on the min-X side, middle of Z axis
        let mid_z = (bld.min.z + bld.max.z) / 2;
        wx == bld.min.x && wz == mid_z
    } else {
        let mid_x = (bld.min.x + bld.max.x) / 2;
        wz == bld.min.z && wx == mid_x
    }
}

// ---------------------------------------------------------------------
// Facade stamping + library (CD)
// ---------------------------------------------------------------------

/// Place every non-air voxel of `prefab` at `origin` (prefab (0,0,0)
/// lands on `origin`). Returns the number of voxels changed.
fn stamp_facade(world: &mut VoxelWorld, origin: IVec3, prefab: &FacadePrefab) -> usize {
    let mut changed = 0usize;
    for (off, v) in &prefab.voxels {
        let p = origin + *off;
        if world.edit_set_voxel(p.x, p.y, p.z, *v) {
            changed += 1;
        }
    }
    changed
}

/// Hardcoded baseline prefabs that ship with the engine. Keeps the
/// FASSADE tool usable even when `./facades/` is missing. All four
/// prefabs fit inside a 5×5×5 AABB so they preview cleanly.
fn builtin_facades() -> Vec<FacadePrefab> {
    let stone: Voxel = BlockType::Stone.into();
    let wood: Voxel = BlockType::Wood.into();
    let glass: Voxel = BlockType::Ice.into();
    let glow: Voxel = BlockType::Crystal.into();
    let moss: Voxel = BlockType::MossStone.into();

    // 1) Brunnen: 3x3 stone ring with a water-spigot post in the middle.
    let mut brunnen = Vec::new();
    for (x, z) in [
        (0, 0),
        (1, 0),
        (2, 0),
        (0, 1),
        (2, 1),
        (0, 2),
        (1, 2),
        (2, 2),
    ] {
        brunnen.push((IVec3::new(x, 1, z), stone));
    }
    brunnen.push((IVec3::new(1, 1, 1), stone));
    brunnen.push((IVec3::new(1, 2, 1), stone));
    brunnen.push((IVec3::new(1, 3, 1), glass));

    // 2) Ampel: 1x1 3-block pole, crystal head on top.
    let mut ampel = Vec::new();
    ampel.push((IVec3::new(0, 1, 0), stone));
    ampel.push((IVec3::new(0, 2, 0), stone));
    ampel.push((IVec3::new(0, 3, 0), stone));
    ampel.push((IVec3::new(0, 4, 0), glow));

    // 3) Parkbank: wooden bench + backrest (3 blocks long).
    let mut bank = Vec::new();
    for x in 0..3 {
        bank.push((IVec3::new(x, 1, 0), wood)); // seat
        bank.push((IVec3::new(x, 2, 0), wood)); // backrest lower
        bank.push((IVec3::new(x, 3, 0), wood)); // backrest upper
    }
    // two stone legs
    bank.push((IVec3::new(0, 0, 0), stone));
    bank.push((IVec3::new(2, 0, 0), stone));

    // 4) Baum: moss-stone trunk with glow-crystal leaves (mini tree).
    let mut baum = Vec::new();
    for y in 1..=3 {
        baum.push((IVec3::new(0, y, 0), moss));
    }
    for (x, y, z) in [
        (-1, 4, 0),
        (1, 4, 0),
        (0, 4, -1),
        (0, 4, 1),
        (0, 5, 0),
        (0, 4, 0),
    ] {
        baum.push((IVec3::new(x, y, z), glow));
    }

    vec![
        FacadePrefab {
            name: "Brunnen".into(),
            category: "decor".into(),
            size: IVec3::new(3, 4, 3),
            voxels: brunnen,
        },
        FacadePrefab {
            name: "Ampel".into(),
            category: "light".into(),
            size: IVec3::new(1, 5, 1),
            voxels: ampel,
        },
        FacadePrefab {
            name: "Parkbank".into(),
            category: "decor".into(),
            size: IVec3::new(3, 4, 1),
            voxels: bank,
        },
        FacadePrefab {
            name: "Mini-Baum".into(),
            category: "nature".into(),
            size: IVec3::new(3, 6, 3),
            voxels: baum,
        },
    ]
}

/// Scan a folder for `*.ron` facade files. Returns the parsed prefabs.
/// Unknown / malformed files produce a warning and are skipped; a
/// missing directory returns `Ok(vec![])` so callers can treat it as
/// "no user facades shipped yet" without an error path.
#[cfg(not(target_arch = "wasm32"))]
fn load_facade_library(dir: &str) -> std::io::Result<Vec<FacadePrefab>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ron") {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                warn!("facade read {}: {}", path.display(), e);
                continue;
            }
        };
        match ron::from_str::<FacadeFile>(&text) {
            Ok(f) => out.push(FacadePrefab::from_file(f)),
            Err(e) => warn!("facade parse {}: {}", path.display(), e),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Gizmo rendering
// ---------------------------------------------------------------------

fn city_draw_gizmos(
    editor: Res<EditorState>,
    mode: Res<crate::mode::ModeContext>,
    city: Res<CityState>,
    time: Res<Time>,
    cam_q: Query<&GlobalTransform, (With<Camera3d>, With<Player>)>,
    world: Res<VoxelWorld>,
    mut gizmos: Gizmos,
) {
    let editor_city_active = editor.open && editor.tab == EditorTab::City;
    let live_city_active = mode
        .build_tool()
        .and_then(crate::toolbelt::ToolbeltTool::city_tool)
        .is_some()
        && mode.is_build_live();
    if !editor_city_active && !live_city_active {
        return;
    }
    let phase = (time.elapsed_seconds() * 2.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let pulse = 0.45 + 0.55 * phase;

    // --- Committed roads ---------------------------------------------
    for (idx, r) in city.roads.iter().enumerate() {
        let selected = city.selected_road == Some(idx);
        let col = if selected {
            Color::srgb(1.0, 0.84, 0.22)
        } else {
            r.style.gizmo_color()
        };
        draw_road_component_gizmo(&mut gizmos, &world, r, col, selected);
    }

    // --- Districts ----------------------------------------------------
    for d in &city.districts {
        let col = d.kind.color();
        circle_xz(&mut gizmos, d.center, d.radius as f32, col);
        circle_xz(
            &mut gizmos,
            d.center,
            (d.radius as f32) * 0.5,
            col.with_alpha(0.5),
        );
        // Vertical beacon so the disc is spottable from the player's
        // eye-level (8 blocks tall — visible but not noisy).
        let c = d.center.as_vec3() + Vec3::new(0.5, 0.0, 0.5);
        gizmos.line(c, c + Vec3::new(0.0, 8.0, 0.0), col);
    }

    // --- Pending road preview ----------------------------------------
    if let Ok(cam_tf) = cam_q.get_single() {
        if city.tool == CityTool::Road {
            let origin = cam_tf.translation();
            let dir = cam_tf.forward().as_vec3();
            let picked = raycast_voxel(&world, origin, dir, 100.0).unwrap_or_else(|| {
                let fwd = origin + dir * 12.0;
                let c = IVec3::new(
                    fwd.x.floor() as i32,
                    fwd.y.floor() as i32,
                    fwd.z.floor() as i32,
                );
                (c, c)
            });
            let (hit_cell, _) = picked;
            let sy = world.surface_height_at(hit_cell.x, hit_cell.z);
            let mut cursor = road_tool_snap_cell(
                IVec3::new(hit_cell.x, sy, hit_cell.z),
                city.snap,
                &city.roads,
            );
            if let Some(a) = city.pending_road_a {
                cursor = smart_road_drag_target(a, cursor, &city.roads);
            }
            let preview = city.pending_road_a.map(|a| {
                road_segment_from_drag(a, cursor, city.road_width, city.road_style, &city.roads)
            });
            let preview_color = preview
                .as_ref()
                .map(|road| road.style.gizmo_color())
                .unwrap_or_else(|| city.road_style.gizmo_color());
            let c_world = cursor.as_vec3() + Vec3::new(0.5, 1.5, 0.5);
            // Cursor marker — pulses so the user never loses it.
            gizmos.sphere(c_world, Quat::IDENTITY, 0.8 + pulse * 0.3, preview_color);
            if let Some(preview) = preview {
                draw_road_component_gizmo(&mut gizmos, &world, &preview, preview_color, true);
            }
        }
        if city.tool == CityTool::District {
            let origin = cam_tf.translation();
            let dir = cam_tf.forward().as_vec3();
            let picked = raycast_voxel(&world, origin, dir, 100.0).unwrap_or_else(|| {
                let fwd = origin + dir * 12.0;
                let c = IVec3::new(
                    fwd.x.floor() as i32,
                    fwd.y.floor() as i32,
                    fwd.z.floor() as i32,
                );
                (c, c)
            });
            let (hit_cell, _) = picked;
            let sy = world.surface_height_at(hit_cell.x, hit_cell.z);
            let cursor = snap_cell(
                IVec3::new(hit_cell.x, sy, hit_cell.z),
                city.snap,
                &city.roads,
            );
            circle_xz(
                &mut gizmos,
                cursor,
                city.district_radius as f32,
                city.district_kind.color(),
            );
        }
        if city.tool == CityTool::Building {
            let origin = cam_tf.translation();
            let dir = cam_tf.forward().as_vec3();
            let picked = raycast_voxel(&world, origin, dir, 100.0).unwrap_or_else(|| {
                let fwd = origin + dir * 12.0;
                let c = IVec3::new(
                    fwd.x.floor() as i32,
                    fwd.y.floor() as i32,
                    fwd.z.floor() as i32,
                );
                (c, c)
            });
            let (hit_cell, _) = picked;
            let sy = world.surface_height_at(hit_cell.x, hit_cell.z);
            let cursor = snap_cell(
                IVec3::new(hit_cell.x, sy, hit_cell.z),
                city.snap,
                &city.roads,
            );
            let col = city.building_style.gizmo_color();
            if let Some(a) = city.pending_building_a {
                // Draw the rectangle being dragged out between A and cursor.
                let min = IVec3::new(a.x.min(cursor.x), a.y, a.z.min(cursor.z));
                let max = IVec3::new(a.x.max(cursor.x), a.y, a.z.max(cursor.z));
                draw_footprint(&mut gizmos, min, max, col, city.building_floors as i32);
            } else {
                // Solo cursor marker sized to the current floor count.
                let c_world = cursor.as_vec3() + Vec3::new(0.5, 1.0, 0.5);
                gizmos.cuboid(
                    Transform::from_translation(
                        c_world + Vec3::new(0.0, (city.building_floors as f32) * 1.5, 0.0),
                    )
                    .with_scale(Vec3::new(
                        2.0,
                        (city.building_floors as f32) * 3.0,
                        2.0,
                    )),
                    col.with_alpha(0.45),
                );
                gizmos.sphere(c_world, Quat::IDENTITY, 0.6 + pulse * 0.3, col);
            }
        }
        if city.tool == CityTool::Facade {
            let origin = cam_tf.translation();
            let dir = cam_tf.forward().as_vec3();
            let picked = raycast_voxel(&world, origin, dir, 100.0).unwrap_or_else(|| {
                let fwd = origin + dir * 12.0;
                let c = IVec3::new(
                    fwd.x.floor() as i32,
                    fwd.y.floor() as i32,
                    fwd.z.floor() as i32,
                );
                (c, c)
            });
            let (hit_cell, _) = picked;
            let sy = world.surface_height_at(hit_cell.x, hit_cell.z);
            let cursor = snap_cell(
                IVec3::new(hit_cell.x, sy, hit_cell.z),
                city.snap,
                &city.roads,
            );
            if let Some(prefab) = city.facades.get(
                city.facade_selected
                    .min(city.facades.len().saturating_sub(1)),
            ) {
                let col = Color::srgb(0.85, 0.95, 0.30);
                let size = prefab.size.as_vec3();
                let center = cursor.as_vec3() + size * 0.5;
                gizmos.cuboid(
                    Transform::from_translation(center).with_scale(size.max(Vec3::splat(0.5))),
                    col,
                );
            }
        }
    }

    // --- Committed buildings (outline only) --------------------------
    for b in &city.buildings {
        draw_footprint(
            &mut gizmos,
            b.min,
            b.max,
            b.style.gizmo_color().with_alpha(0.35),
            b.floors as i32,
        );
    }
}

/// Draw road component paths, selected handles, and a corner
/// vertical beacon for easy spotting. Used for previews (pending A →
/// cursor) so previews match what will become voxels.
fn draw_road_component_gizmo(
    gizmos: &mut Gizmos,
    world: &VoxelWorld,
    road: &RoadSegment,
    color: Color,
    selected: bool,
) {
    let cells = road_path_xz(road);
    if cells.is_empty() {
        return;
    }

    let last_index = cells.len().saturating_sub(1);
    let point_at = |i: usize, cell: IVec2| -> Vec3 {
        let elevation = road_elevation_at_sample(road, i, last_index);
        let deck_y = (world.surface_height_at(cell.x, cell.y) + elevation).max(1);
        Vec3::new(
            cell.x as f32 + 0.5,
            deck_y as f32 + 1.2,
            cell.y as f32 + 0.5,
        )
    };

    for (i, pair) in cells.windows(2).enumerate() {
        let a = point_at(i, pair[0]);
        let b = point_at(i + 1, pair[1]);
        gizmos.line(a, b, color);

        let (px, pz) = road_width_axis_at(&cells, i);
        let flank = Vec3::new(
            px as f32 * road.width as f32 * 0.5,
            0.0,
            pz as f32 * road.width as f32 * 0.5,
        );
        let faint = color.with_alpha(0.45);
        gizmos.line(a + flank, b + flank, faint);
        gizmos.line(a - flank, b - flank, faint);
    }

    if selected {
        let start = point_at(0, cells[0]);
        gizmos.sphere(start, Quat::IDENTITY, 0.65, color);
        if let Some(last) = cells.last().copied() {
            let end = point_at(last_index, last);
            gizmos.sphere(end, Quat::IDENTITY, 0.65, color);
        }
        if road.shape == RoadShape::Corner {
            let via = road_corner_via(*road);
            let via_y = (world.surface_height_at(via.x, via.z)
                + road_elevation_at_sample(road, cells.len() / 2, last_index))
            .max(1);
            gizmos.cuboid(
                Transform::from_translation(Vec3::new(
                    via.x as f32 + 0.5,
                    via_y as f32 + 1.2,
                    via.z as f32 + 0.5,
                ))
                .with_scale(Vec3::splat(1.15)),
                color,
            );
        }
    }
}

/// Draw a rectangular building footprint outline at ground height plus
/// a vertical beacon for committed buildings and previews.
fn draw_footprint(gizmos: &mut Gizmos, min: IVec3, max: IVec3, color: Color, floors: i32) {
    let y = min.y as f32 + 0.6;
    let ax = min.x as f32;
    let az = min.z as f32;
    let bx = max.x as f32 + 1.0;
    let bz = max.z as f32 + 1.0;
    let corners = [
        Vec3::new(ax, y, az),
        Vec3::new(bx, y, az),
        Vec3::new(bx, y, bz),
        Vec3::new(ax, y, bz),
        Vec3::new(ax, y, az),
    ];
    gizmos.linestrip(corners, color);
    // Top rectangle at the projected roof height.
    let top_y = y + (floors as f32) * 3.0;
    let top = [
        Vec3::new(ax, top_y, az),
        Vec3::new(bx, top_y, az),
        Vec3::new(bx, top_y, bz),
        Vec3::new(ax, top_y, bz),
        Vec3::new(ax, top_y, az),
    ];
    gizmos.linestrip(top, color.with_alpha(0.6));
    // Four vertical pillars at corners.
    for (cx, cz) in [(ax, az), (bx, az), (bx, bz), (ax, bz)] {
        gizmos.line(Vec3::new(cx, y, cz), Vec3::new(cx, top_y, cz), color);
    }
}

/// Draw a horizontal circle in the XZ-plane centred on `c`.
fn circle_xz(gizmos: &mut Gizmos, c: IVec3, radius: f32, color: Color) {
    const N: usize = 48;
    let center = c.as_vec3() + Vec3::new(0.5, 0.6, 0.5);
    let mut pts = [Vec3::ZERO; N + 1];
    for (i, p) in pts.iter_mut().enumerate() {
        let t = i as f32 / N as f32 * std::f32::consts::TAU;
        *p = center + Vec3::new(t.cos() * radius, 0.0, t.sin() * radius);
    }
    gizmos.linestrip(pts, color);
}

// ---------------------------------------------------------------------
// Contextual Hints HUD (X5)
// ---------------------------------------------------------------------

fn draw_hint_hud(
    mut contexts: EguiContexts,
    editor: Res<EditorState>,
    mode: Res<crate::mode::ModeContext>,
    city: Res<CityState>,
    sel: Res<crate::selection::SelectionState>,
    mirror: Res<crate::selection::MirrorState>,
) {
    let live_city_active = mode
        .build_tool()
        .and_then(crate::toolbelt::ToolbeltTool::city_tool)
        .is_some()
        && mode.is_build_live();
    if live_city_active && !editor.open {
        return;
    }
    if !editor.open {
        return;
    }
    let ctx = contexts.ctx_mut();
    let theme = ctx
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    let accent = theme.color.primary();

    let mut lines: Vec<(String, String)> = Vec::with_capacity(12);

    // --- City-tab hints ------------------------------------------------
    if editor.tab == EditorTab::City {
        lines.push((
            format!("{}  /  Snap: {}", city.tool.label(), city.snap.label()),
            String::new(),
        ));
        match city.tool {
            CityTool::Road => {
                if city.pending_road_a.is_some() {
                    lines.push(("LMB".into(), "Strassenende setzen + weiterzeichnen".into()));
                    lines.push(("Auto".into(), "Laenge / Brueckenhoehe erben".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push(("LMB".into(), "Strassenstart setzen".into()));
                    lines.push(("RMB".into(), "Letzte Strasse loeschen".into()));
                }
                if let Some(idx) = city.selected_road {
                    lines.push((
                        format!("Komponente {}", idx + 1),
                        "direkt editierbar".into(),
                    ));
                    lines.push(("Ctrl+Wheel".into(), "Breite / Kreisradius".into()));
                    lines.push((
                        "Shift+Wheel".into(),
                        "Brueckenhoehe am naechsten Ende".into(),
                    ));
                    lines.push(("Alt+Wheel".into(), "Textur".into()));
                }
                lines.push(("[ / ]".into(), format!("Breite ({})", city.road_width)));
                lines.push(("N".into(), "Strassen-Tool AUS".into()));
            }
            CityTool::District => {
                lines.push(("LMB".into(), "Bezirk platzieren".into()));
                lines.push(("RMB".into(), "Letzten Bezirk loeschen".into()));
                lines.push(("[ / ]".into(), format!("Radius ({})", city.district_radius)));
                lines.push(("T".into(), "Bezirks-Tool AUS".into()));
            }
            CityTool::Building => {
                if city.pending_building_a.is_some() {
                    lines.push(("LMB".into(), "Ecke B setzen (Gebaeude stampt)".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push(("LMB".into(), "Ecke A setzen".into()));
                    lines.push(("RMB".into(), "Letztes Gebaeude vergessen".into()));
                }
                lines.push(("[ / ]".into(), format!("Etagen ({})", city.building_floors)));
                lines.push(("U".into(), "Gebaeude-Tool AUS".into()));
            }
            CityTool::Facade => {
                lines.push(("LMB".into(), "Fassade stempeln".into()));
                let name = city
                    .facades
                    .get(
                        city.facade_selected
                            .min(city.facades.len().saturating_sub(1)),
                    )
                    .map(|f| f.name.as_str())
                    .unwrap_or("(leer)");
                lines.push(("Aktiv".into(), name.into()));
                lines.push(("F".into(), "Fassaden-Tool AUS".into()));
            }
            CityTool::None => {
                lines.push(("N".into(), "Strassen-Werkzeug".into()));
                lines.push(("T".into(), "Bezirks-Werkzeug".into()));
                lines.push(("U".into(), "Gebaeude-Werkzeug".into()));
                lines.push(("F".into(), "Fassaden-Werkzeug".into()));
            }
        }
        lines.push((".".into(), "Snap-Modus wechseln".into()));
    } else {
        // --- Selection / modelling hints (reuse existing state) -------
        if sel.ghosting {
            lines.push(("GEIST AKTIV".into(), String::new()));
            lines.push(("LMB / Enter".into(), "Einfuegen".into()));
            lines.push(("Shift+LMB".into(), "Stempel-Modus".into()));
            lines.push(("Mausrad / R".into(), "Y-Drehen".into()));
            lines.push(("X / Y / Z".into(), "Spiegeln".into()));
            lines.push(("Esc / RMB".into(), "Abbrechen".into()));
        } else if sel.a.is_some() {
            lines.push(("AUSWAHL".into(), String::new()));
            lines.push(("B".into(), "2. Ecke setzen".into()));
            lines.push(("C".into(), "Kopieren".into()));
            lines.push(("Ctrl+X".into(), "Ausschneiden".into()));
            lines.push(("V".into(), "Einfuegen".into()));
            lines.push(("Esc".into(), "Auswahl loeschen".into()));
        } else {
            lines.push(("EDITOR".into(), String::new()));
            lines.push(("B".into(), "Box-Auswahl starten".into()));
            lines.push(("V".into(), "Clipboard einfuegen".into()));
            lines.push(("M / Shift+M / Alt+M".into(), "Spiegel X / Y / Z".into()));
        }
        if mirror.x || mirror.y || mirror.z {
            let m = format!(
                "X={} Y={} Z={}",
                on_off(mirror.x),
                on_off(mirror.y),
                on_off(mirror.z)
            );
            lines.push(("SPIEGEL".into(), m));
        }
    }

    // --- Render ---------------------------------------------------------
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(3, 6, 3, 225))
        .stroke(egui::Stroke::new(1.0, accent))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .rounding(egui::Rounding::ZERO);
    egui::Window::new("voxel_native_hints")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(frame)
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(12.0, -12.0))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (key, desc) in &lines {
                ui.horizontal(|ui| {
                    let key_text = egui::RichText::new(format!("[{}]", key))
                        .color(accent)
                        .monospace()
                        .strong();
                    ui.label(key_text);
                    if !desc.is_empty() {
                        ui.label(
                            egui::RichText::new(desc)
                                .color(crate::theme::TEXT)
                                .monospace(),
                        );
                    }
                });
            }
        });
}

fn on_off(b: bool) -> &'static str {
    if b {
        "AN"
    } else {
        "AUS"
    }
}

// ---------------------------------------------------------------------
// DDA voxel raycast (Amanatides-Woo)
// ---------------------------------------------------------------------
//
// Duplicated from [`crate::selection::raycast_voxel`] /
// [`crate::weapons::dda_voxel`] to keep modules decoupled — the
// function is ~30 lines and lives here so the city module doesn't
// have a cyclic dependency with selection.

fn raycast_voxel(
    world: &VoxelWorld,
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
) -> Option<(IVec3, IVec3)> {
    if dir.length_squared() < 1e-6 {
        return None;
    }
    let mut x = origin.x.floor() as i32;
    let mut y = origin.y.floor() as i32;
    let mut z = origin.z.floor() as i32;
    let step_x = dir.x.signum() as i32;
    let step_y = dir.y.signum() as i32;
    let step_z = dir.z.signum() as i32;
    let t_delta_x = if dir.x != 0.0 {
        (1.0 / dir.x).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_y = if dir.y != 0.0 {
        (1.0 / dir.y).abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if dir.z != 0.0 {
        (1.0 / dir.z).abs()
    } else {
        f32::INFINITY
    };
    let nb = |p: f32, s: i32| -> f32 {
        if s > 0 {
            p.floor() + 1.0 - p
        } else if s < 0 {
            p - p.floor()
        } else {
            f32::INFINITY
        }
    };
    let mut tmx = nb(origin.x, step_x) * t_delta_x;
    let mut tmy = nb(origin.y, step_y) * t_delta_y;
    let mut tmz = nb(origin.z, step_z) * t_delta_z;
    let mut prev: IVec3;
    for _ in 0..4_096 {
        let t = tmx.min(tmy).min(tmz);
        if t > max_dist {
            return None;
        }
        prev = IVec3::new(x, y, z);
        if tmx <= tmy && tmx <= tmz {
            x += step_x;
            tmx += t_delta_x;
        } else if tmy <= tmz {
            y += step_y;
            tmy += t_delta_y;
        } else {
            z += step_z;
            tmz += t_delta_z;
        }
        if voxel_is_solid(world.voxel_at(x, y, z)) {
            return Some((IVec3::new(x, y, z), prev));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn road_component_adjustments_keep_geometry_and_change_style_or_width() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(48, 72, 0),
            3,
            RoadStyle::Asphalt,
        );

        let wider = road.with_width(11);
        let neon = road.retextured(RoadStyle::Neon);

        assert_eq!(wider.a, road.a);
        assert_eq!(wider.b, road.b);
        assert_eq!(wider.width, 11);
        assert_eq!(wider.style, road.style);
        assert_eq!(neon.a, road.a);
        assert_eq!(neon.b, road.b);
        assert_eq!(neon.width, road.width);
        assert_eq!(neon.style, RoadStyle::Neon);
        assert_eq!(neon.style.surface_block(), BlockType::Limestone);
    }

    #[test]
    fn edited_road_component_syncs_active_brush_width_and_texture() {
        let mut city = CityState::default();
        let edited = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            11,
            RoadStyle::Neon,
        )
        .with_endpoint_heights(0, 12);

        sync_road_brush_from_component(&mut city, edited);

        assert_eq!(city.road_width, 11);
        assert_eq!(
            city.road_style,
            RoadStyle::Neon,
            "after editing a road, the next free road should use the edited component look"
        );
    }

    #[test]
    fn edited_roundabout_radius_keeps_brush_on_component_width_and_texture() {
        let mut city = CityState::default();
        let roundabout = RoadSegment::roundabout(IVec3::new(16, 72, 16), 10, 5, RoadStyle::Cobble);
        let larger = road_with_size_delta(roundabout, 8);

        sync_road_brush_from_component(&mut city, larger);

        assert_eq!(city.road_width, 5);
        assert_eq!(city.road_style, RoadStyle::Cobble);
        assert_eq!(larger.roundabout_radius, 18);
    }

    #[test]
    fn road_component_edit_clears_old_width_before_restamping() {
        let mut world = VoxelWorld::new();
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(16, 72, 0),
            7,
            RoadStyle::Neon,
        );
        let old_flank_y = world.surface_height_at(8, 3);
        let center_y = world.surface_height_at(8, 0);

        stamp_road(&mut world, &road);
        assert_eq!(
            world.voxel_at(8, old_flank_y, 3),
            Voxel::from(BlockType::Limestone)
        );

        let narrow_dirt = road.with_width(1).retextured(RoadStyle::Dirt);
        restamp_road_component(&mut world, &road, &narrow_dirt);

        assert_eq!(
            world.voxel_at(8, old_flank_y, 3),
            terrain_surface_restore_voxel(&world, 8, 3)
        );
        assert_eq!(world.voxel_at(8, center_y, 0), Voxel::from(BlockType::Dirt));
    }

    #[test]
    fn raised_road_component_uses_smooth_bridge_grade() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(0, 14);

        let heights: Vec<i32> = (0..=16)
            .map(|idx| road_elevation_at_sample(&road, idx, 16))
            .collect();

        assert_eq!(heights.first().copied(), Some(0));
        assert_eq!(heights.last().copied(), Some(14));
        assert!(heights.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(
            heights
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() <= 2),
            "bridge grade should step smoothly, got {heights:?}"
        );
    }

    #[test]
    fn road_component_hover_picks_nearest_segment() {
        let roads = vec![
            RoadSegment::new(
                IVec3::new(0, 72, 0),
                IVec3::new(32, 72, 0),
                5,
                RoadStyle::Asphalt,
            ),
            RoadSegment::new(
                IVec3::new(0, 72, 24),
                IVec3::new(32, 72, 24),
                5,
                RoadStyle::Neon,
            ),
        ];

        assert_eq!(
            nearest_road_component(&roads, IVec3::new(9, 72, 2), 4.0),
            Some(0)
        );
        assert_eq!(
            nearest_road_component(&roads, IVec3::new(9, 72, 22), 4.0),
            Some(1)
        );
        assert_eq!(
            nearest_road_component(&roads, IVec3::new(9, 72, 12), 2.0),
            None
        );
    }

    #[test]
    fn road_snap_prefers_component_endpoint_handles() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        assert_eq!(
            snap_cell(IVec3::new(31, 72, 2), SnapMode::Road, &[road]),
            IVec3::new(32, 72, 0),
            "road snap should lock to nearby endpoints for fast exact connections"
        );
    }

    #[test]
    fn road_snap_prefers_corner_turn_handles() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            5,
            RoadStyle::Asphalt,
        );

        assert_eq!(
            snap_cell(IVec3::new(24, 72, 2), SnapMode::Road, &[road]),
            IVec3::new(24, 72, 0),
            "road snap should expose the editable turn handle even when the visible curve is smoothed"
        );
    }

    #[test]
    fn road_tool_snap_finds_existing_endpoints_without_snap_mode() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        assert_eq!(
            road_tool_snap_cell(IVec3::new(31, 72, 2), SnapMode::Off, &[road]),
            IVec3::new(32, 72, 0),
            "Road tool should snap to nearby endpoints without asking the player to cycle snap modes"
        );
    }

    #[test]
    fn road_tool_snap_finds_mid_road_branch_points_without_snap_mode() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Neon,
        );

        assert_eq!(
            road_tool_snap_cell(IVec3::new(15, 72, 3), SnapMode::Off, &[road]),
            IVec3::new(15, 72, 0),
            "Road tool should snap to the road path for fast branch drawing"
        );
    }

    #[test]
    fn road_tool_snap_respects_explicit_grid_before_contextual_road_snap() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        assert_eq!(
            road_tool_snap_cell(IVec3::new(18, 72, 10), SnapMode::Grid16, &[road]),
            IVec3::new(16, 72, 16),
            "explicit grid snapping should still win when it is not close enough to a road"
        );
    }

    #[test]
    fn smart_road_drag_snap_keeps_accidental_jitter_straight() {
        let target = smart_road_drag_target(IVec3::new(0, 72, 0), IVec3::new(31, 72, 2), &[]);

        assert_eq!(
            target,
            IVec3::new(31, 72, 0),
            "small hand jitter while drawing should stay a straight road instead of creating a corner"
        );
    }

    #[test]
    fn smart_road_drag_snap_reuses_previous_component_length() {
        let previous = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );
        let target =
            smart_road_drag_target(IVec3::new(0, 72, 16), IVec3::new(30, 72, 18), &[previous]);

        assert_eq!(
            target,
            IVec3::new(32, 72, 16),
            "drawing near an existing road length should snap to the same exact span"
        );
    }

    #[test]
    fn smart_road_drag_snap_preserves_deliberate_corner_intent() {
        let target = smart_road_drag_target(IVec3::new(0, 72, 0), IVec3::new(24, 72, 16), &[]);

        assert_eq!(
            target,
            IVec3::new(24, 72, 16),
            "deliberate two-axis drags should still create one editable corner component"
        );
    }

    #[test]
    fn road_chain_continues_from_component_end() {
        let straight = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );
        let corner = RoadSegment::new(
            IVec3::new(32, 72, 0),
            IVec3::new(48, 72, 24),
            5,
            RoadStyle::Neon,
        );
        let roundabout = RoadSegment::roundabout(IVec3::new(64, 72, 0), 10, 5, RoadStyle::Cobble);

        assert_eq!(road_continuation_start(&straight), Some(straight.b));
        assert_eq!(road_continuation_start(&corner), Some(corner.b));
        assert_eq!(road_continuation_start(&roundabout), None);
    }

    #[test]
    fn road_drag_from_raised_endpoint_inherits_bridge_height() {
        let previous = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(0, 14);

        let next = road_segment_from_drag(
            previous.b,
            IVec3::new(64, 72, 0),
            5,
            RoadStyle::Asphalt,
            &[previous],
        );

        assert_eq!(next.elevation_a, 14);
        assert_eq!(
            next.elevation_b, 14,
            "continuing from a raised bridge endpoint should keep the deck level until the player edits it down"
        );
    }

    #[test]
    fn road_drag_between_raised_handles_matches_both_bridge_heights() {
        let west = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(0, 10);
        let east = RoadSegment::new(
            IVec3::new(64, 72, 0),
            IVec3::new(96, 72, 0),
            5,
            RoadStyle::Neon,
        )
        .with_endpoint_heights(18, 18);

        let connector =
            road_segment_from_drag(west.b, east.a, 5, RoadStyle::Asphalt, &[west, east]);

        assert_eq!(connector.elevation_a, 10);
        assert_eq!(connector.elevation_b, 18);
    }

    #[test]
    fn road_drag_from_existing_handle_inherits_width_and_texture() {
        let previous = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            11,
            RoadStyle::Neon,
        );

        let next = road_segment_from_drag(
            previous.b,
            IVec3::new(64, 72, 0),
            3,
            RoadStyle::Asphalt,
            &[previous],
        );

        assert_eq!(
            next.width, 11,
            "a connected road should inherit the source component width instead of falling back to the global knob"
        );
        assert_eq!(
            next.style,
            RoadStyle::Neon,
            "a connected road should inherit the source texture so roads blend by default"
        );
    }

    #[test]
    fn road_drag_between_existing_handles_prefers_start_component_texture() {
        let west = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            9,
            RoadStyle::Cobble,
        );
        let east = RoadSegment::new(
            IVec3::new(64, 72, 0),
            IVec3::new(96, 72, 0),
            5,
            RoadStyle::Neon,
        );

        let connector =
            road_segment_from_drag(west.b, east.a, 3, RoadStyle::Asphalt, &[west, east]);

        assert_eq!(connector.width, 9);
        assert_eq!(
            connector.style,
            RoadStyle::Cobble,
            "source handle should be the style authority for fast road chaining"
        );
    }

    #[test]
    fn road_branch_from_mid_road_inherits_width_texture_and_height() {
        let arterial = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            9,
            RoadStyle::Neon,
        )
        .with_endpoint_heights(0, 12);

        let branch = road_segment_from_drag(
            IVec3::new(16, 72, 0),
            IVec3::new(16, 72, 24),
            3,
            RoadStyle::Asphalt,
            &[arterial],
        );

        assert_eq!(
            branch.width, 9,
            "branches started from a snapped road path should inherit road width"
        );
        assert_eq!(
            branch.style,
            RoadStyle::Neon,
            "branches started from a snapped road path should inherit road texture"
        );
        assert_eq!(
            branch.elevation_a, 6,
            "branch should start at the sampled bridge deck height, not ground zero"
        );
        assert_eq!(branch.elevation_b, 6);
    }

    #[test]
    fn road_branch_into_mid_road_inherits_target_bridge_height_when_start_is_free() {
        let arterial = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            7,
            RoadStyle::Cobble,
        )
        .with_endpoint_heights(4, 12);

        let branch = road_segment_from_drag(
            IVec3::new(16, 72, 24),
            IVec3::new(16, 72, 0),
            3,
            RoadStyle::Asphalt,
            &[arterial],
        );

        assert_eq!(branch.width, 7);
        assert_eq!(branch.style, RoadStyle::Cobble);
        assert_eq!(branch.elevation_a, 0);
        assert_eq!(
            branch.elevation_b, 8,
            "target end should meet the sampled mid-road bridge height"
        );
    }

    #[test]
    fn road_component_height_delta_targets_closest_endpoint() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        let raised_b = road_with_endpoint_height_delta(road, IVec3::new(31, 72, 1), 6);
        assert_eq!(raised_b.elevation_a, 0);
        assert_eq!(raised_b.elevation_b, 6);

        let lowered_a = road_with_endpoint_height_delta(raised_b, IVec3::new(1, 72, 1), -4);
        assert_eq!(lowered_a.elevation_a, -4);
        assert_eq!(lowered_a.elevation_b, 6);
    }

    #[test]
    fn corner_road_height_delta_targets_turn_control_point() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            5,
            RoadStyle::Asphalt,
        );

        let raised_turn = road_with_endpoint_height_delta(road, IVec3::new(21, 72, 3), 10);

        assert_eq!(raised_turn.elevation_a, 0);
        assert_eq!(raised_turn.elevation_via, 10);
        assert_eq!(raised_turn.elevation_b, 0);
    }

    #[test]
    fn corner_road_uses_turn_height_for_smooth_bridge_grade() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            5,
            RoadStyle::Asphalt,
        );
        let raised_turn = road_with_endpoint_height_delta(road, IVec3::new(21, 72, 3), 12);
        let path = road_path_xz(&raised_turn);
        let last_index = path.len().saturating_sub(1);
        let heights: Vec<i32> = (0..=last_index)
            .map(|idx| road_elevation_at_sample(&raised_turn, idx, last_index))
            .collect();

        assert_eq!(heights.first().copied(), Some(0));
        assert_eq!(heights.last().copied(), Some(0));
        assert_eq!(heights.iter().copied().max(), Some(12));
        assert!(
            heights
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() <= 2),
            "corner bridge grade should ease smoothly through the turn, got {heights:?}"
        );
    }

    #[test]
    fn diagonal_road_drag_behaves_like_single_editable_corner_component() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            5,
            RoadStyle::Asphalt,
        );
        let roads = vec![road];

        assert_eq!(
            nearest_road_component(&roads, IVec3::new(24, 72, 8), 3.0),
            Some(0),
            "the vertical leg of the corner should still select the same component"
        );
        assert_eq!(
            nearest_road_component(&roads, IVec3::new(12, 72, 8), 1.0),
            None,
            "the old diagonal chord should not be treated as road surface"
        );
        assert_eq!(
            snap_cell(IVec3::new(23, 72, 8), SnapMode::Road, &roads),
            IVec3::new(24, 72, 8),
            "road snap should land on the corner leg, not the diagonal chord"
        );
    }

    #[test]
    fn diagonal_road_drag_uses_smooth_corner_transition_path() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            5,
            RoadStyle::Asphalt,
        );
        let path = road_path_xz(&road);

        assert!(
            path.contains(&IVec2::new(21, 3)),
            "corner path should arc through a transition cell instead of snapping at the via point"
        );
        assert!(
            path.contains(&IVec2::new(24, 8)),
            "corner path should still continue along the second leg after the smooth turn"
        );
        assert!(
            !path.contains(&IVec2::new(24, 0)),
            "hard corner via cell should be replaced by a smoothed transition"
        );
        assert_eq!(
            snap_cell(IVec3::new(21, 72, 3), SnapMode::Road, &[road]),
            IVec3::new(21, 72, 3),
            "road snap should follow the smoothed transition path"
        );
    }

    #[test]
    fn same_point_road_drag_creates_editable_roundabout_component() {
        let road = RoadSegment::new(
            IVec3::new(16, 72, 16),
            IVec3::new(16, 72, 16),
            5,
            RoadStyle::Neon,
        );

        assert_eq!(road.shape, RoadShape::Roundabout);
        assert_eq!(road.roundabout_radius, 10);
        assert_eq!(road.width, 5);
        assert_eq!(road.style, RoadStyle::Neon);
        let path = road_path_xz(&road);
        assert!(path.len() > 40);
        assert_eq!(path.first(), path.last());
        assert_eq!(
            nearest_road_component(&[road], IVec3::new(26, 72, 16), 2.0),
            Some(0)
        );

        let larger = road_with_size_delta(road, 4);
        assert_eq!(larger.shape, RoadShape::Roundabout);
        assert_eq!(larger.roundabout_radius, 14);
        assert_eq!(larger.width, 5);
        assert_eq!(larger.style, RoadStyle::Neon);
    }

    #[test]
    fn deleting_selected_road_component_clears_its_voxels() {
        let mut world = VoxelWorld::new();
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(16, 72, 0),
            5,
            RoadStyle::Neon,
        );
        let center_y = world.surface_height_at(8, 0);
        let mut roads = vec![road];

        stamp_road(&mut world, &road);
        assert_eq!(
            world.voxel_at(8, center_y, 0),
            Voxel::from(BlockType::Limestone)
        );

        let changed = delete_road_component(&mut world, &mut roads, 0).unwrap();

        assert!(changed > 0);
        assert!(roads.is_empty());
        assert_eq!(
            world.voxel_at(8, center_y, 0),
            terrain_surface_restore_voxel(&world, 8, 0)
        );
    }

    #[test]
    fn road_component_save_roundtrip_preserves_editable_shapes() {
        let corner = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            7,
            RoadStyle::Neon,
        )
        .with_endpoint_heights(2, 6)
        .with_turn_height(11);
        let roundabout = RoadSegment::roundabout(IVec3::new(64, 80, -12), 6, 5, RoadStyle::Cobble)
            .with_endpoint_heights(4, 4);

        let snapshot = CityRoadSave::from_roads(&[corner, roundabout]);
        let text = ron::ser::to_string(&snapshot).unwrap();
        let parsed: CityRoadSave = ron::from_str(&text).unwrap();

        assert_eq!(parsed.into_roads(), vec![corner, roundabout]);
    }
}
