//! STADT — City-Builder layer on top of the voxel world.
//!
//! Slim Cut 1 of the plan-v3 city system:
//!
//! * **CA Road-Grid-Tool** - choose Road in the STADT tab or Toolbelt,
//!   hold LMB, drag, and release to commit an editable road component
//!   that follows terrain. Two-click placement still works. Axis drags create straights,
//!   diagonal drags create clean corner roads, and same-point clicks
//!   create roundabouts. Width with `[` / `]` (1..=17).
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
//! All mutation goes through the [`VoxelWorld`] edit APIs so the existing
//! async mesher picks changes up within a frame or two; road operations use
//! one batch per component edit.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use std::collections::{HashMap, HashSet};

use crate::blocks::{voxel_is_solid, BlockType, Voxel, AIR, DEFAULT_MATERIAL};
use crate::director::UnifiedTelemetry;
use crate::editor::{EditorState, EditorTab};
use crate::neurocore::{RuntimeBudget, RuntimeProfile};
use crate::player::Player;
use crate::world::{VoxelWorld, WorldEditBatch};

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

const ROAD_MIN_ELEVATION: i16 = -12;
const ROAD_MAX_ELEVATION: i16 = 48;
const ROAD_MAX_WIDTH: usize = 17;
const ROAD_MAX_ROUNDABOUT_RADIUS: u8 = 48;
const ROAD_MAX_CENTERLINE_SAMPLES: usize = 513;
const ROAD_MAX_COMPONENTS: usize = 4_096;
const ROAD_MAX_SUPPORT_VOXELS: usize = 24_576;
const ROAD_MAX_FURNITURE_VOXELS: usize = 256;

// Smoothstep reaches 1.5 times the average slope. Limiting a control span to
// 4/3 blocks of rise per centerline interval therefore caps the sampled deck
// at two vertical blocks per horizontal interval, including short ramps.
const ROAD_MAX_SAMPLED_RISE_PER_INTERVAL: usize = 2;
const SMOOTHSTEP_MAX_SLOPE_NUMERATOR: usize = 3;
const SMOOTHSTEP_MAX_SLOPE_DENOMINATOR: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RoadRuntimeBudget {
    max_component_run: usize,
    max_roundabout_radius: u8,
    max_components: usize,
    max_visible_components: usize,
    max_gizmo_segments: usize,
    max_preview_gizmo_segments: usize,
}

impl RoadRuntimeBudget {
    fn for_profile(profile: RuntimeProfile) -> Self {
        match profile {
            RuntimeProfile::LowSpec => Self {
                max_component_run: 192,
                max_roundabout_radius: 24,
                max_components: 512,
                max_visible_components: 64,
                max_gizmo_segments: 2_048,
                max_preview_gizmo_segments: 128,
            },
            RuntimeProfile::Auto | RuntimeProfile::Balanced => Self {
                max_component_run: ROAD_MAX_CENTERLINE_SAMPLES - 1,
                max_roundabout_radius: ROAD_MAX_ROUNDABOUT_RADIUS,
                max_components: 2_048,
                max_visible_components: 256,
                max_gizmo_segments: 16_384,
                max_preview_gizmo_segments: 512,
            },
            RuntimeProfile::Cinematic => Self {
                max_component_run: ROAD_MAX_CENTERLINE_SAMPLES - 1,
                max_roundabout_radius: ROAD_MAX_ROUNDABOUT_RADIUS,
                max_components: ROAD_MAX_COMPONENTS,
                max_visible_components: 512,
                max_gizmo_segments: 32_768,
                max_preview_gizmo_segments: 512,
            },
            RuntimeProfile::Benchmark => Self {
                max_component_run: ROAD_MAX_CENTERLINE_SAMPLES - 1,
                max_roundabout_radius: ROAD_MAX_ROUNDABOUT_RADIUS,
                max_components: ROAD_MAX_COMPONENTS,
                max_visible_components: 1_024,
                max_gizmo_segments: 65_536,
                max_preview_gizmo_segments: 512,
            },
        }
    }

    #[cfg(test)]
    fn max_centerline_samples(self) -> usize {
        self.max_component_run.saturating_add(1)
    }

    #[cfg(test)]
    fn max_surface_voxels(self) -> usize {
        self.max_centerline_samples().saturating_mul(ROAD_MAX_WIDTH)
    }
}

impl Default for RoadRuntimeBudget {
    fn default() -> Self {
        Self::for_profile(RuntimeProfile::Auto)
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
        let b = road_target_within_run_budget(a, b, ROAD_MAX_CENTERLINE_SAMPLES - 1);
        if a.x == b.x && a.z == b.z {
            let radius = ((width.clamp(1, ROAD_MAX_WIDTH as u8) as i32) * 2)
                .clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS as i32) as u8;
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
        .with_smart_shape()
        .with_endpoint_heights(0, 0)
    }

    pub fn roundabout(center: IVec3, radius: u8, width: u8, style: RoadStyle) -> Self {
        let radius = radius.clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS);
        Self {
            a: center,
            b: IVec3::new(center.x.saturating_add(radius as i32), center.y, center.z),
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
        self.width = width.clamp(1, ROAD_MAX_WIDTH as u8);
        self
    }

    pub fn retextured(mut self, style: RoadStyle) -> Self {
        self.style = style;
        self
    }

    pub fn with_endpoint_heights(mut self, a: i16, b: i16) -> Self {
        self.elevation_a = a.clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
        self.elevation_b = b.clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
        limit_road_endpoint_grade(&mut self);
        self
    }

    pub fn with_turn_height(mut self, via: i16) -> Self {
        self.elevation_via = via.clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
        limit_road_turn_grade(&mut self);
        self
    }

    fn with_smart_shape(mut self) -> Self {
        if self.a.x != self.b.x && self.a.z != self.b.z {
            self.shape = RoadShape::Corner;
            self.via = Some(deterministic_corner_via(self.a, self.b));
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
    7
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
            roads: roads
                .iter()
                .take(ROAD_MAX_COMPONENTS)
                .copied()
                .map(SavedRoadSegment::from)
                .collect(),
        }
    }

    fn into_roads(self) -> Vec<RoadSegment> {
        self.roads
            .into_iter()
            .take(ROAD_MAX_COMPONENTS)
            .map(RoadSegment::from)
            .collect()
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
            width: saved.width.clamp(1, ROAD_MAX_WIDTH as u8),
            style: saved.style,
            elevation_a: saved
                .elevation_a
                .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION),
            elevation_via: saved
                .elevation_via
                .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION),
            elevation_b: saved
                .elevation_b
                .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION),
        };

        road.b = road_target_within_run_budget(road.a, road.b, ROAD_MAX_CENTERLINE_SAMPLES - 1);
        match road.shape {
            RoadShape::Corner => {
                if road.a.x == road.b.x || road.a.z == road.b.z {
                    road.shape = RoadShape::Straight;
                    road.via = None;
                } else {
                    road.via = Some(deterministic_corner_via(road.a, road.b));
                }
                road.roundabout_radius = 0;
            }
            RoadShape::Roundabout => {
                let fallback =
                    ((road.width as i32) * 2).clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS as i32) as u8;
                let radius = if road.roundabout_radius == 0 {
                    fallback
                } else {
                    road.roundabout_radius
                };
                road.roundabout_radius = radius.clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS);
                road.b = IVec3::new(
                    road.a.x.saturating_add(road.roundabout_radius as i32),
                    road.a.y,
                    road.a.z,
                );
                road.via = None;
            }
            RoadShape::Straight => {
                if road.a.x != road.b.x && road.a.z != road.b.z {
                    road.shape = RoadShape::Corner;
                    road.via = Some(deterministic_corner_via(road.a, road.b));
                } else {
                    road.via = None;
                }
                road.roundabout_radius = 0;
            }
        }
        if road.shape == RoadShape::Corner {
            limit_road_turn_grade(&mut road);
        } else {
            limit_road_endpoint_grade(&mut road);
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
            BuildingStyle::Tower => BlockType::ShipHullDark,
        }
    }
    pub fn floor_block(self) -> BlockType {
        match self {
            BuildingStyle::Residential => BlockType::Wood,
            BuildingStyle::Commercial => BlockType::Limestone,
            BuildingStyle::Industrial => BlockType::Gravel,
            BuildingStyle::Tower => BlockType::ShipHullAlloy,
        }
    }
    pub fn roof(self) -> BlockType {
        match self {
            BuildingStyle::Residential => BlockType::Grass,
            BuildingStyle::Commercial => BlockType::NeonMagenta,
            BuildingStyle::Industrial => BlockType::Basalt,
            BuildingStyle::Tower => BlockType::CockpitGlass,
        }
    }
    /// Suggested floor count range `(min, max)` per style.
    pub fn default_floors(self) -> (u8, u8) {
        match self {
            BuildingStyle::Residential => (4, 12),
            BuildingStyle::Commercial => (15, 35),
            BuildingStyle::Industrial => (3, 8),
            BuildingStyle::Tower => (30, 80),
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
    pub road_width: u8, // 1..=17
    pub district_kind: DistrictKind,
    pub district_radius: i32, // 2..=24
    pub building_style: BuildingStyle,
    pub building_floors: u8, // 2..=20
    pub snap: SnapMode,
    /// First click of a road in progress. Cleared when the segment
    /// commits or the user cancels with Esc.
    pub pending_road_a: Option<IVec3>,
    /// First corner of a placed bot city area / district footprint.
    pub pending_district_a: Option<IVec3>,
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
            road_width: 7,
            district_kind: DistrictKind::Residential,
            district_radius: 6,
            building_style: BuildingStyle::Residential,
            building_floors: 4,
            snap: SnapMode::Off,
            pending_road_a: None,
            pending_district_a: None,
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
    city.pending_district_a = None;
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
    runtime: Res<RuntimeBudget>,
    active: Option<Res<crate::settings::ActiveWorld>>,
    mut city: ResMut<CityState>,
    mut bots: Option<ResMut<crate::bots::FriendlyWorldBrain>>,
    mut telemetry: ResMut<UnifiedTelemetry>,
    mut world: ResMut<VoxelWorld>,
    mut contexts: EguiContexts,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    cam_q: Query<(&Camera, &GlobalTransform), (With<Camera3d>, With<Player>)>,
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
    let road_budget = RoadRuntimeBudget::for_profile(runtime.profile);

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
        city.pending_district_a = None;
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
        city.pending_district_a = None;
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
        city.pending_district_a = None;
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
        city.pending_district_a = None;
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
                city.road_width = (city.road_width + 1).min(ROAD_MAX_WIDTH as u8);
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
        } else if city.pending_district_a.take().is_some() {
            city.status = "Bot-Stadtflaeche abgebrochen.".into();
        } else if city.pending_building_a.take().is_some() {
            city.status = "Gebaeude abgebrochen.".into();
        }
    }

    // --- Pointer / crosshair pick -------------------------------------
    let Ok((camera, cam_tf)) = cam_q.get_single() else {
        wheel.clear();
        return;
    };
    let window = windows.get_single().ok();
    let cursor_visible = window.is_some_and(|window| window.cursor.visible);
    let pointer_over_ui = cursor_visible && {
        let ctx = contexts.ctx_mut();
        ctx.is_pointer_over_area() || ctx.wants_pointer_input()
    };
    let Some((origin, dir)) = city_placement_ray(window, camera, cam_tf) else {
        wheel.clear();
        city.selected_road = None;
        return;
    };
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

    city.selected_road = if city.tool == CityTool::Road && !pointer_over_ui {
        nearest_road_component(&city.roads, snapped, 5.0)
    } else {
        None
    };

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if pointer_over_ui {
        return;
    }
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

            if bare && city.pending_road_a.is_none() {
                let (edited, kind) = road_plain_wheel_component_edit_with_budget(
                    before,
                    snapped,
                    steps,
                    road_budget,
                );
                next = edited;
                label = Some(road_component_edit_label(next, kind));
            } else if ctrl && !shift && !alt {
                next = road_with_size_delta_for_budget(before, steps * 2, road_budget);
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
                next = road_with_texture_delta(before, steps);
                label = Some(format!("Textur {}", next.style.label()));
            }

            if let Some(label) = label {
                let n =
                    restamp_road_component_in_network(&mut world, &city.roads, idx, &before, &next);
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

    if bare
        && city.tool == CityTool::Road
        && city.pending_road_a.is_none()
        && mouse.just_pressed(MouseButton::Middle)
    {
        if let Some(idx) = city.selected_road.filter(|idx| *idx < city.roads.len()) {
            let before = city.roads[idx];
            let next = road_with_texture_delta(before, 1);
            let n = restamp_road_component_in_network(&mut world, &city.roads, idx, &before, &next);
            city.roads[idx] = next;
            sync_road_brush_from_component(&mut city, next);
            save_city_roads_for_active(active.as_deref(), &city.roads);
            city.status = format!(
                "Strassenkomponente {}: Textur {} ({} Bloecke)",
                idx + 1,
                next.style.label(),
                n
            );
            telemetry.city_actions = telemetry.city_actions.saturating_add(1);
            telemetry.build_blocks_changed =
                telemetry.build_blocks_changed.saturating_add(n as u64);
            return;
        }
        city.status = "Textur: auf eine Strassenkomponente zielen.".into();
    }

    if bare
        && city.tool == CityTool::Road
        && mouse.just_released(MouseButton::Left)
        && city.pending_road_a.is_some()
    {
        let start = city.pending_road_a.unwrap();
        if let Some(target) = road_drag_release_target(start, snapped, &city.roads) {
            commit_road_segment_from_points(
                &mut city,
                &mut telemetry,
                &mut world,
                active.as_deref(),
                start,
                target,
                road_budget,
            );
            return;
        }
    }

    if bare
        && city.tool == CityTool::District
        && mouse.just_released(MouseButton::Left)
        && city.pending_district_a.is_some()
    {
        let start = city.pending_district_a.unwrap();
        if let Some((min, max)) =
            city_area_drag_release_corners(start, snapped, city.district_radius)
        {
            commit_city_area_from_corners(&mut city, &mut bots, &mut telemetry, &world, min, max);
            return;
        }
    }

    if bare
        && city.tool == CityTool::Building
        && mouse.just_released(MouseButton::Left)
        && city.pending_building_a.is_some()
    {
        let start = city.pending_building_a.unwrap();
        if let Some((min, max)) = building_shell_drag_release_corners(start, snapped) {
            commit_building_shell_from_corners(&mut city, &mut telemetry, &mut world, min, max);
            return;
        }
    }

    // --- Mouse: commit action -----------------------------------------
    if bare && mouse.just_pressed(MouseButton::Left) {
        match city.tool {
            CityTool::Road => match city.pending_road_a {
                None => {
                    city.pending_road_a = Some(snapped);
                    city.status = format!(
                        "Start @ {},{} - drag/release draws road, or click endpoint.",
                        snapped.x, snapped.z
                    );
                }
                Some(a) => {
                    let target = smart_road_drag_target(a, snapped, &city.roads);
                    commit_road_segment_from_points(
                        &mut city,
                        &mut telemetry,
                        &mut world,
                        active.as_deref(),
                        a,
                        target,
                        road_budget,
                    );
                }
            },
            CityTool::District => match city.pending_district_a {
                None => {
                    city.pending_district_a = Some(snapped);
                    city.status = format!(
                        "Bot city area start @ {},{} - drag/release places the build zone, or click endpoint.",
                        snapped.x, snapped.z
                    );
                }
                Some(a) => {
                    let (min, max) = city_area_corners(a, snapped, city.district_radius);
                    commit_city_area_from_corners(
                        &mut city,
                        &mut bots,
                        &mut telemetry,
                        &world,
                        min,
                        max,
                    );
                }
            },
            CityTool::Building => match city.pending_building_a {
                None => {
                    city.pending_building_a = Some(snapped);
                    city.status = format!(
                        "Building shell start @ {},{} - drag/release places the footprint, or click endpoint.",
                        snapped.x, snapped.z
                    );
                }
                Some(a) => {
                    let (min, max) = building_shell_corners(a, snapped);
                    commit_building_shell_from_corners(
                        &mut city,
                        &mut telemetry,
                        &mut world,
                        min,
                        max,
                    );
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
                if city.pending_district_a.take().is_some() {
                    city.status = "Bot-Stadtflaeche verworfen.".into();
                } else if city.districts.pop().is_some() {
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
    let n = n as i64;
    let step = step.max(1) as i64;
    let half = step / 2;
    let rounded = if n >= 0 {
        ((n + half) / step) * step
    } else {
        -(((-n + half) / step) * step)
    };
    rounded.clamp(i32::MIN as i64, i32::MAX as i64) as i32
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
            nearest_road_path_cell(roads, point, 8.0)
                .map(|cell| IVec3::new(cell.x, p.y, cell.y))
                .unwrap_or(p)
        }
    }
}

fn road_tool_snap_cell(p: IVec3, mode: SnapMode, roads: &[RoadSegment]) -> IVec3 {
    let base = snap_cell(p, mode, roads);
    contextual_road_snap_cell(base, roads).unwrap_or(base)
}

fn city_area_corners(a: IVec3, b: IVec3, fallback_radius: i32) -> (IVec3, IVec3) {
    if a.x == b.x && a.z == b.z {
        let r = fallback_radius.max(8);
        return (
            IVec3::new(a.x - r, a.y, a.z - r),
            IVec3::new(a.x + r, a.y, a.z + r),
        );
    }
    (
        IVec3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        IVec3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

fn city_area_drag_release_corners(
    start: IVec3,
    raw: IVec3,
    fallback_radius: i32,
) -> Option<(IVec3, IVec3)> {
    (start.x != raw.x || start.z != raw.z).then(|| city_area_corners(start, raw, fallback_radius))
}

fn building_shell_corners(a: IVec3, b: IVec3) -> (IVec3, IVec3) {
    (
        IVec3::new(a.x.min(b.x), a.y, a.z.min(b.z)),
        IVec3::new(a.x.max(b.x), a.y, a.z.max(b.z)),
    )
}

fn building_shell_drag_release_corners(start: IVec3, raw: IVec3) -> Option<(IVec3, IVec3)> {
    (start.x != raw.x || start.z != raw.z).then(|| building_shell_corners(start, raw))
}

fn commit_building_shell_from_corners(
    city: &mut CityState,
    telemetry: &mut UnifiedTelemetry,
    world: &mut VoxelWorld,
    min: IVec3,
    max: IVec3,
) {
    let bld = Building {
        min,
        max,
        floors: city.building_floors,
        style: city.building_style,
    };
    let n = stamp_building(world, &bld);
    city.buildings.push(bld);
    city.pending_building_a = None;
    city.status = format!(
        "{} shell {}x{} x {} floors ({} blocks). Use Room/Sketch cuts for interiors, doors, and windows.",
        city.building_style.label(),
        max.x - min.x + 1,
        max.z - min.z + 1,
        city.building_floors,
        n
    );
    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
    telemetry.build_blocks_changed = telemetry.build_blocks_changed.saturating_add(n as u64);
}

fn commit_city_area_from_corners(
    city: &mut CityState,
    bots: &mut Option<ResMut<crate::bots::FriendlyWorldBrain>>,
    telemetry: &mut UnifiedTelemetry,
    world: &VoxelWorld,
    min: IVec3,
    max: IVec3,
) {
    let center = IVec3::new((min.x + max.x) / 2, min.y, (min.z + max.z) / 2);
    let size = max - min + IVec3::ONE;
    let radius = (((size.x as f32 * 0.5).powi(2) + (size.z as f32 * 0.5).powi(2))
        .sqrt()
        .ceil() as i32)
        .max(city.district_radius);
    let kind = city.district_kind;
    city.districts.push(District {
        center,
        radius,
        kind,
    });
    city.pending_district_a = None;
    let queued = bots
        .as_deref_mut()
        .map(|brain| crate::bots::queue_city_area_masterplan_with_world(brain, world, min, max))
        .unwrap_or(0);
    city.status = if queued > 0 {
        format!(
            "Bot city area {}x{} placed: {} project(s) queued inside this zone.",
            size.x, size.z, queued
        )
    } else {
        format!(
            "Bot city area {}x{} placed. Bots wait until the zone is valid and loaded.",
            size.x, size.z
        )
    };
    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
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
    let mut best: Option<(f32, RoadSegment)> = None;
    for road in roads {
        let Some((_, d2)) = road_nearest_point_xz(*road, point) else {
            continue;
        };
        if d2 <= max_d2 && best.map_or(true, |(best_d2, _)| d2 < best_d2) {
            best = Some((d2, *road));
        }
    }
    let (_, road) = best?;
    road_path_xz(&road)
        .into_iter()
        .filter_map(|cell| {
            let d2 = point.distance_squared(road_cell_xz(cell));
            (d2 <= max_d2).then_some((d2, cell))
        })
        .min_by(|(a, a_cell), (b, b_cell)| {
            a.total_cmp(b)
                .then_with(|| (a_cell.x, a_cell.y).cmp(&(b_cell.x, b_cell.y)))
        })
        .map(|(_, cell)| cell)
}

const SMART_ROAD_AXIS_JITTER: i64 = 4;
const SMART_ROAD_AXIS_RATIO: f32 = 1.6;
const SMART_ROAD_LENGTH_TOLERANCE: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoadDragAxis {
    X,
    Z,
}

fn smart_road_drag_target(start: IVec3, raw: IVec3, roads: &[RoadSegment]) -> IVec3 {
    let dx = raw.x as i64 - start.x as i64;
    let dz = raw.z as i64 - start.z as i64;
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
        let shifted = |base: i32, direction: i64| -> i32 {
            (base as i64 + direction.saturating_mul(span)).clamp(i32::MIN as i64, i32::MAX as i64)
                as i32
        };
        match axis {
            RoadDragAxis::X => target.x = shifted(start.x, dx.signum()),
            RoadDragAxis::Z => target.z = shifted(start.z, dz.signum()),
        }
    }
    target
}

fn dominant_road_drag_axis(dx: i64, dz: i64) -> Option<RoadDragAxis> {
    let ax = dx.abs();
    let az = dz.abs();
    if ax == 0 && az == 0 {
        return None;
    }
    if ax >= az && (az <= SMART_ROAD_AXIS_JITTER || ax as f32 >= az as f32 * SMART_ROAD_AXIS_RATIO)
    {
        return Some(RoadDragAxis::X);
    }
    if az >= ax && (ax <= SMART_ROAD_AXIS_JITTER || az as f32 >= ax as f32 * SMART_ROAD_AXIS_RATIO)
    {
        return Some(RoadDragAxis::Z);
    }
    None
}

fn matching_reference_road_span(raw_len: i64, roads: &[RoadSegment]) -> Option<i64> {
    let mut best: Option<(i64, i64)> = None;
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

fn visit_road_reference_spans(road: RoadSegment, mut visit: impl FnMut(i64)) {
    match road.shape {
        RoadShape::Straight => {
            visit(
                (road.b.x as i64 - road.a.x as i64)
                    .abs()
                    .max((road.b.z as i64 - road.a.z as i64).abs()),
            );
        }
        RoadShape::Corner => {
            let via = road_corner_via(road);
            visit((via.x as i64 - road.a.x as i64).abs() + (via.z as i64 - road.a.z as i64).abs());
            visit((road.b.x as i64 - via.x as i64).abs() + (road.b.z as i64 - via.z as i64).abs());
        }
        RoadShape::Roundabout => {
            visit(road.roundabout_radius.max(4) as i64 * 2);
        }
    }
}

fn road_continuation_start(road: &RoadSegment) -> Option<IVec3> {
    match road.shape {
        RoadShape::Roundabout => None,
        RoadShape::Straight | RoadShape::Corner => Some(road.b),
    }
}

#[cfg(test)]
fn road_segment_from_drag(
    start: IVec3,
    target: IVec3,
    width: u8,
    style: RoadStyle,
    roads: &[RoadSegment],
) -> RoadSegment {
    road_segment_from_drag_with_budget(
        start,
        target,
        width,
        style,
        roads,
        RoadRuntimeBudget::default(),
    )
}

fn road_segment_from_drag_with_budget(
    start: IVec3,
    target: IVec3,
    width: u8,
    style: RoadStyle,
    roads: &[RoadSegment],
    budget: RoadRuntimeBudget,
) -> RoadSegment {
    let target = road_target_within_run_budget(start, target, budget.max_component_run);
    let start_sample = road_connection_sample_at(roads, start);
    let target_sample = road_connection_sample_at(roads, target);
    let (width, style) = road_drag_appearance(start_sample, target_sample, width, style);
    let mut segment = RoadSegment::new(start, target, width, style);
    if segment.shape == RoadShape::Roundabout
        && segment.roundabout_radius > budget.max_roundabout_radius
    {
        segment.roundabout_radius = budget.max_roundabout_radius;
        segment.b = IVec3::new(
            segment.a.x.saturating_add(segment.roundabout_radius as i32),
            segment.a.y,
            segment.a.z,
        );
    }
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

fn road_target_within_run_budget(start: IVec3, target: IVec3, max_run: usize) -> IVec3 {
    let dx = target.x as i64 - start.x as i64;
    let dz = target.z as i64 - start.z as i64;
    let run = dx.unsigned_abs().saturating_add(dz.unsigned_abs());
    let max_run = max_run.max(1) as u64;
    if run <= max_run {
        return target;
    }

    let dy = target.y as i64 - start.y as i64;
    let scaled = |delta: i64| -> i64 {
        delta.signum() * ((delta.unsigned_abs().saturating_mul(max_run) / run) as i64)
    };
    let bounded_coord = |base: i32, delta: i64| -> i32 {
        (base as i64 + delta).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    };
    IVec3::new(
        bounded_coord(start.x, scaled(dx)),
        bounded_coord(start.y, scaled(dy)),
        bounded_coord(start.z, scaled(dz)),
    )
}

fn road_drag_release_target(start: IVec3, raw: IVec3, roads: &[RoadSegment]) -> Option<IVec3> {
    let target = smart_road_drag_target(start, raw, roads);
    (target.x != start.x || target.z != start.z).then_some(target)
}

fn commit_road_segment_from_points(
    city: &mut CityState,
    telemetry: &mut UnifiedTelemetry,
    world: &mut VoxelWorld,
    active: Option<&crate::settings::ActiveWorld>,
    start: IVec3,
    target: IVec3,
    budget: RoadRuntimeBudget,
) {
    if city.roads.len() >= budget.max_components.min(ROAD_MAX_COMPONENTS) {
        city.pending_road_a = None;
        city.status = format!(
            "Strassenlimit fuer dieses Laufzeitprofil erreicht ({} Komponenten).",
            budget.max_components.min(ROAD_MAX_COMPONENTS)
        );
        return;
    }
    let seg = road_segment_from_drag_with_budget(
        start,
        target,
        city.road_width,
        city.road_style,
        &city.roads,
        budget,
    );
    let n = stamp_road(world, &seg);
    city.roads.push(seg);
    save_city_roads_for_active(active, &city.roads);
    city.road_width = seg.width;
    city.road_style = seg.style;
    city.pending_road_a = road_continuation_start(&seg);
    city.status = if let Some(next) = city.pending_road_a {
        format!(
            "Road {} {} ({} blocks) - continue from {},{}.",
            seg.shape.label(),
            seg.style.label(),
            n,
            next.x,
            next.z
        )
    } else {
        format!(
            "Road {} {} ({} blocks)",
            seg.shape.label(),
            seg.style.label(),
            n
        )
    };
    telemetry.city_actions = telemetry.city_actions.saturating_add(1);
    telemetry.build_blocks_changed = telemetry.build_blocks_changed.saturating_add(n as u64);
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
    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(road, &cells).min(last_index);
    let deck_y = road_component_y_at_sample(&road, idx, last_index, turn_index);
    Some(RoadConnectionSample {
        width: road.width,
        style: road.style,
        elevation: (deck_y - cell.y).clamp(ROAD_MIN_ELEVATION as i32, ROAD_MAX_ELEVATION as i32)
            as i16,
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
                IVec3::new(road.a.x.saturating_add(r), road.a.y, road.a.z),
                IVec3::new(road.a.x.saturating_sub(r), road.a.y, road.a.z),
                IVec3::new(road.a.x, road.a.y, road.a.z.saturating_add(r)),
                IVec3::new(road.a.x, road.a.y, road.a.z.saturating_sub(r)),
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
            road.b = IVec3::new(road.a.x.saturating_add(r), road.a.y, road.a.z);
            visit(road.a);
            visit(road.b);
            visit(IVec3::new(road.a.x.saturating_sub(r), road.a.y, road.a.z));
            visit(IVec3::new(road.a.x, road.a.y, road.a.z.saturating_add(r)));
            visit(IVec3::new(road.a.x, road.a.y, road.a.z.saturating_sub(r)));
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

fn deterministic_corner_via(a: IVec3, b: IVec3) -> IVec3 {
    let (first, second) = if (a.x, a.z) <= (b.x, b.z) {
        (a, b)
    } else {
        (b, a)
    };
    IVec3::new(second.x, first.y, first.z)
}

fn road_corner_via(road: RoadSegment) -> IVec3 {
    road.via
        .unwrap_or_else(|| deterministic_corner_via(road.a, road.b))
}

fn road_corner_turn_index(road: RoadSegment, cells: &[IVec2]) -> usize {
    if road.shape != RoadShape::Corner {
        return 0;
    }
    let via = road_corner_via(road);
    cells
        .iter()
        .enumerate()
        .min_by_key(|(_, cell)| {
            let dx = cell.x as i128 - via.x as i128;
            let dz = cell.y as i128 - via.z as i128;
            (
                dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)),
                cell.x,
                cell.y,
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn road_line_interval_count(a: IVec2, b: IVec2) -> usize {
    let dx = (b.x as i64 - a.x as i64).unsigned_abs();
    let dz = (b.y as i64 - a.y as i64).unsigned_abs();
    dx.saturating_add(dz)
        .min((ROAD_MAX_CENTERLINE_SAMPLES - 1) as u64) as usize
}

fn road_grade_interval_counts(road: RoadSegment) -> (usize, usize) {
    match road.shape {
        RoadShape::Straight => (
            road_line_interval_count(
                IVec2::new(road.a.x, road.a.z),
                IVec2::new(road.b.x, road.b.z),
            ),
            0,
        ),
        RoadShape::Corner => {
            let cells = road_path_xz(&road);
            let total = cells.len().saturating_sub(1);
            let first = road_corner_turn_index(road, &cells).min(total);
            (first, total.saturating_sub(first))
        }
        RoadShape::Roundabout => (0, 0),
    }
}

fn road_grade_delta_limit(intervals: usize) -> i32 {
    let rise = intervals
        .saturating_mul(ROAD_MAX_SAMPLED_RISE_PER_INTERVAL)
        .saturating_mul(SMOOTHSTEP_MAX_SLOPE_DENOMINATOR)
        / SMOOTHSTEP_MAX_SLOPE_NUMERATOR;
    rise.min((ROAD_MAX_ELEVATION - ROAD_MIN_ELEVATION) as usize) as i32
}

fn clamp_road_elevation_near(requested: i16, anchor: i16, max_delta: i32) -> i16 {
    (requested as i32)
        .clamp(anchor as i32 - max_delta, anchor as i32 + max_delta)
        .clamp(ROAD_MIN_ELEVATION as i32, ROAD_MAX_ELEVATION as i32) as i16
}

fn limit_road_endpoint_grade(road: &mut RoadSegment) {
    road.elevation_a = road
        .elevation_a
        .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
    road.elevation_b = road
        .elevation_b
        .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
    match road.shape {
        RoadShape::Straight => {
            let (run, _) = road_grade_interval_counts(*road);
            road.elevation_b = clamp_road_elevation_near(
                road.elevation_b,
                road.elevation_a,
                road_grade_delta_limit(run),
            );
            road.elevation_via = 0;
        }
        RoadShape::Corner => {
            let (run_a, run_b) = road_grade_interval_counts(*road);
            let limit_a = road_grade_delta_limit(run_a);
            let limit_b = road_grade_delta_limit(run_b);
            road.elevation_b = clamp_road_elevation_near(
                road.elevation_b,
                road.elevation_a,
                limit_a.saturating_add(limit_b),
            );
            let low = (road.elevation_a as i32 - limit_a).max(road.elevation_b as i32 - limit_b);
            let high = (road.elevation_a as i32 + limit_a).min(road.elevation_b as i32 + limit_b);
            road.elevation_via = (road.elevation_via as i32)
                .clamp(low, high)
                .clamp(ROAD_MIN_ELEVATION as i32, ROAD_MAX_ELEVATION as i32)
                as i16;
        }
        RoadShape::Roundabout => {
            let plateau = ((road.elevation_a as i32 + road.elevation_b as i32) / 2)
                .clamp(ROAD_MIN_ELEVATION as i32, ROAD_MAX_ELEVATION as i32)
                as i16;
            road.elevation_a = plateau;
            road.elevation_b = plateau;
            road.elevation_via = 0;
        }
    }
}

fn limit_road_turn_grade(road: &mut RoadSegment) {
    let requested = road
        .elevation_via
        .clamp(ROAD_MIN_ELEVATION, ROAD_MAX_ELEVATION);
    limit_road_endpoint_grade(road);
    if road.shape != RoadShape::Corner {
        return;
    }

    let (run_a, run_b) = road_grade_interval_counts(*road);
    let limit_a = road_grade_delta_limit(run_a);
    let limit_b = road_grade_delta_limit(run_b);
    let low = (road.elevation_a as i32 - limit_a).max(road.elevation_b as i32 - limit_b);
    let high = (road.elevation_a as i32 + limit_a).min(road.elevation_b as i32 + limit_b);
    road.elevation_via = (requested as i32).clamp(low, high) as i16;
}

fn road_with_endpoint_height_delta(road: RoadSegment, cursor: IVec3, delta: i16) -> RoadSegment {
    let shifted = |height: i16| -> i16 {
        (height as i32 + delta as i32).clamp(ROAD_MIN_ELEVATION as i32, ROAD_MAX_ELEVATION as i32)
            as i16
    };
    if road.shape == RoadShape::Roundabout {
        return road.with_endpoint_heights(shifted(road.elevation_a), shifted(road.elevation_b));
    }
    let p = Vec2::new(cursor.x as f32 + 0.5, cursor.z as f32 + 0.5);
    let a = Vec2::new(road.a.x as f32 + 0.5, road.a.z as f32 + 0.5);
    let b = Vec2::new(road.b.x as f32 + 0.5, road.b.z as f32 + 0.5);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoadComponentEditKind {
    Size,
    Height,
}

#[cfg(test)]
fn road_plain_wheel_component_edit(
    road: RoadSegment,
    cursor: IVec3,
    steps: i32,
) -> (RoadSegment, RoadComponentEditKind) {
    road_plain_wheel_component_edit_with_budget(road, cursor, steps, RoadRuntimeBudget::default())
}

fn road_plain_wheel_component_edit_with_budget(
    road: RoadSegment,
    cursor: IVec3,
    steps: i32,
    budget: RoadRuntimeBudget,
) -> (RoadSegment, RoadComponentEditKind) {
    if road_cursor_near_edit_handle(road, cursor, 4.5) {
        (
            road_with_endpoint_height_delta(road, cursor, (steps * 2) as i16),
            RoadComponentEditKind::Height,
        )
    } else {
        (
            road_with_size_delta_for_budget(road, steps * 2, budget),
            RoadComponentEditKind::Size,
        )
    }
}

fn road_cursor_near_edit_handle(road: RoadSegment, cursor: IVec3, max_distance: f32) -> bool {
    let point = road_point_xz(cursor);
    let max_d2 = max_distance * max_distance;
    let mut near = false;
    visit_road_snap_handles(road, |handle| {
        if point.distance_squared(road_point_xz(handle)) <= max_d2 {
            near = true;
        }
    });
    near
}

fn road_component_edit_label(road: RoadSegment, kind: RoadComponentEditKind) -> String {
    match kind {
        RoadComponentEditKind::Size => {
            if road.shape == RoadShape::Roundabout {
                format!("Radius {}", road.roundabout_radius)
            } else {
                format!("Breite {}", road.width)
            }
        }
        RoadComponentEditKind::Height => {
            if road.shape == RoadShape::Corner {
                format!(
                    "Hoehe A/T/B {}:{}:{}",
                    road.elevation_a, road.elevation_via, road.elevation_b
                )
            } else if road.shape == RoadShape::Roundabout {
                format!("Hoehe Ring {}", road.elevation_a)
            } else {
                format!("Hoehe A/B {}:{}", road.elevation_a, road.elevation_b)
            }
        }
    }
}

#[cfg(test)]
fn road_with_size_delta(road: RoadSegment, delta: i32) -> RoadSegment {
    road_with_size_delta_for_budget(road, delta, RoadRuntimeBudget::default())
}

fn road_with_size_delta_for_budget(
    road: RoadSegment,
    delta: i32,
    budget: RoadRuntimeBudget,
) -> RoadSegment {
    if road.shape == RoadShape::Roundabout {
        let mut next = road;
        let max_radius = budget.max_roundabout_radius.max(road.roundabout_radius);
        next.roundabout_radius =
            (road.roundabout_radius as i32 + delta).clamp(4, max_radius as i32) as u8;
        next.b = IVec3::new(
            next.a.x.saturating_add(next.roundabout_radius as i32),
            next.a.y,
            next.a.z,
        );
        next
    } else {
        road.with_width((road.width as i32 + delta).clamp(1, ROAD_MAX_WIDTH as i32) as u8)
    }
}

fn road_with_texture_delta(road: RoadSegment, delta: i32) -> RoadSegment {
    road.retextured(next_road_style(road.style, delta))
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

/// Direction-stable 4-connected line in the XZ plane, inclusive of both
/// endpoints. At exact grid-corner crossings the lexicographically smaller
/// bridge cell wins, so reversing a component reverses the same voxel path.
fn line_xz(a: IVec2, b: IVec2) -> Vec<IVec2> {
    let mut out = Vec::with_capacity(road_line_interval_count(a, b).saturating_add(1));
    let (mut x, mut y) = (a.x as i64, a.y as i64);
    let (x1, y1) = (b.x as i64, b.y as i64);
    let dx = (x1 - x).unsigned_abs();
    let dy = (y1 - y).unsigned_abs();
    let sx = if x < x1 { 1 } else { -1 };
    let sy = if y < y1 { 1 } else { -1 };
    let (mut ix, mut iy) = (0u64, 0u64);
    out.push(IVec2::new(x as i32, y as i32));

    while (ix < dx || iy < dy) && out.len() < ROAD_MAX_CENTERLINE_SAMPLES {
        let x_error = ix.saturating_mul(2).saturating_add(1).saturating_mul(dy);
        let y_error = iy.saturating_mul(2).saturating_add(1).saturating_mul(dx);
        if x_error == y_error && ix < dx && iy < dy {
            let x_candidate = (x + sx, y);
            let y_candidate = (x, y + sy);
            let x_first = x_candidate <= y_candidate;
            if x_first {
                x += sx;
                ix += 1;
            } else {
                y += sy;
                iy += 1;
            }
            out.push(IVec2::new(x as i32, y as i32));
            if out.len() >= ROAD_MAX_CENTERLINE_SAMPLES {
                break;
            }
            if x_first {
                y += sy;
                iy += 1;
            } else {
                x += sx;
                ix += 1;
            }
        } else if (x_error < y_error && ix < dx) || iy >= dy {
            x += sx;
            ix += 1;
        } else {
            y += sy;
            iy += 1;
        }
        out.push(IVec2::new(x as i32, y as i32));
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

pub(crate) fn road_component_centerline_samples(seg: &RoadSegment) -> Vec<IVec3> {
    let cells = road_path_xz(seg);
    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(*seg, &cells).min(last_index);
    cells
        .into_iter()
        .enumerate()
        .map(|(idx, cell)| {
            IVec3::new(
                cell.x,
                road_component_y_at_sample(seg, idx, last_index, turn_index),
                cell.y,
            )
        })
        .collect()
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
    if path.is_empty() || into.len() >= ROAD_MAX_CENTERLINE_SAMPLES {
        return;
    }
    if into.last().copied() == path.first().copied() {
        path.remove(0);
    }
    let remaining = ROAD_MAX_CENTERLINE_SAMPLES.saturating_sub(into.len());
    into.extend(path.into_iter().take(remaining));
}

fn roundabout_path_xz(center: IVec3, radius: u8) -> Vec<IVec2> {
    let radius = radius.clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS) as i32;
    let mut offsets = Vec::with_capacity(radius as usize * 8 + 8);
    let (mut x, mut z) = (radius, 0);
    let mut decision = 1 - radius;
    while x >= z {
        offsets.extend([
            IVec2::new(x, z),
            IVec2::new(z, x),
            IVec2::new(-z, x),
            IVec2::new(-x, z),
            IVec2::new(-x, -z),
            IVec2::new(-z, -x),
            IVec2::new(z, -x),
            IVec2::new(x, -z),
        ]);
        z += 1;
        if decision < 0 {
            decision += 2 * z + 1;
        } else {
            x -= 1;
            decision += 2 * (z - x) + 1;
        }
    }

    offsets.sort_by(|a, b| {
        let half = |p: IVec2| usize::from(p.y < 0 || (p.y == 0 && p.x < 0));
        half(*a).cmp(&half(*b)).then_with(|| {
            let cross = a.x as i64 * b.y as i64 - a.y as i64 * b.x as i64;
            if cross > 0 {
                std::cmp::Ordering::Less
            } else if cross < 0 {
                std::cmp::Ordering::Greater
            } else {
                (a.x, a.y).cmp(&(b.x, b.y))
            }
        })
    });
    offsets.dedup();

    let perimeter: Vec<IVec2> = offsets
        .into_iter()
        .map(|offset| {
            IVec2::new(
                center.x.saturating_add(offset.x),
                center.z.saturating_add(offset.y),
            )
        })
        .collect();

    let mut cells = Vec::with_capacity((radius as usize * 8 + 1).min(ROAD_MAX_CENTERLINE_SAMPLES));
    for index in 0..perimeter.len() {
        let next = (index + 1) % perimeter.len();
        append_path_unique(&mut cells, line_xz(perimeter[index], perimeter[next]));
        if cells.len() >= ROAD_MAX_CENTERLINE_SAMPLES {
            break;
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RoadCrossSection {
    min_offset: i32,
    max_offset: i32,
}

impl RoadCrossSection {
    fn for_width(width: u8) -> Self {
        let width = width.clamp(1, ROAD_MAX_WIDTH as u8) as i32;
        let negative = width / 2;
        Self {
            min_offset: -negative,
            max_offset: width - negative - 1,
        }
    }

    fn offsets(self) -> std::ops::RangeInclusive<i32> {
        self.min_offset..=self.max_offset
    }

    fn width(self) -> usize {
        (self.max_offset - self.min_offset + 1) as usize
    }

    fn is_boulevard(self) -> bool {
        self.width() >= 9
    }

    fn is_outer_edge(self, offset: i32) -> bool {
        offset == self.min_offset || offset == self.max_offset
    }

    fn is_sidewalk(self, offset: i32) -> bool {
        self.is_boulevard() && (offset == self.min_offset + 1 || offset == self.max_offset - 1)
    }

    fn min_boundary(self) -> f32 {
        self.min_offset as f32 - 0.5
    }

    fn max_boundary(self) -> f32 {
        self.max_offset as f32 + 0.5
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoadRestampKind {
    ReassignStyle,
    RebuildGeometry,
}

fn road_restamp_kind(before: &RoadSegment, after: &RoadSegment) -> RoadRestampKind {
    if road_geometry_matches(before, after) {
        RoadRestampKind::ReassignStyle
    } else {
        RoadRestampKind::RebuildGeometry
    }
}

fn road_geometry_matches(a: &RoadSegment, b: &RoadSegment) -> bool {
    a.a == b.a
        && a.b == b.b
        && a.via == b.via
        && a.shape == b.shape
        && a.roundabout_radius == b.roundabout_radius
        && a.width == b.width
        && a.elevation_a == b.elevation_a
        && a.elevation_via == b.elevation_via
        && a.elevation_b == b.elevation_b
}

struct RoadEditTransaction<'a> {
    world: &'a mut VoxelWorld,
    batch: WorldEditBatch,
    changed: usize,
}

impl<'a> RoadEditTransaction<'a> {
    fn new(world: &'a mut VoxelWorld) -> Self {
        Self {
            world,
            batch: WorldEditBatch::default(),
            changed: 0,
        }
    }

    fn set_cell(&mut self, pos: IVec3, voxel: Voxel) -> bool {
        let changed = self
            .world
            .edit_set_cell_batched(
                pos.x,
                pos.y,
                pos.z,
                voxel,
                DEFAULT_MATERIAL,
                &mut self.batch,
            )
            .is_some();
        self.changed += usize::from(changed);
        changed
    }

    fn replace_owned_cell(
        &mut self,
        pos: IVec3,
        expected: Option<Voxel>,
        replacement: Option<Voxel>,
    ) -> bool {
        if expected == replacement {
            return false;
        }
        self.owns_cell(pos, expected) && self.set_cell(pos, replacement.unwrap_or(AIR))
    }

    fn owns_cell(&self, pos: IVec3, expected: Option<Voxel>) -> bool {
        let current = self.world.voxel_at(pos.x, pos.y, pos.z);
        match expected {
            Some(expected) => {
                current == expected
                    && self.world.material_at(pos.x, pos.y, pos.z) == DEFAULT_MATERIAL
            }
            None => current == AIR,
        }
    }

    fn finish(self) -> usize {
        let Self {
            world,
            batch,
            changed,
        } = self;
        world.finish_edit_batch(batch);
        changed
    }
}

#[derive(Debug, Clone, Copy)]
struct RoadBoundsXZ {
    min_x: i64,
    max_x: i64,
    min_z: i64,
    max_z: i64,
}

impl RoadBoundsXZ {
    fn for_component(road: RoadSegment) -> Self {
        let cross_section = RoadCrossSection::for_width(road.width);
        let margin = cross_section
            .min_offset
            .unsigned_abs()
            .max(cross_section.max_offset.unsigned_abs()) as i64;
        if road.shape == RoadShape::Roundabout {
            let radius = road.roundabout_radius.clamp(4, ROAD_MAX_ROUNDABOUT_RADIUS) as i64;
            return Self {
                min_x: road.a.x as i64 - radius - margin,
                max_x: road.a.x as i64 + radius + margin,
                min_z: road.a.z as i64 - radius - margin,
                max_z: road.a.z as i64 + radius + margin,
            };
        }

        let via = road_corner_via(road);
        let mut min_x = (road.a.x as i64).min(road.b.x as i64);
        let mut max_x = (road.a.x as i64).max(road.b.x as i64);
        let mut min_z = (road.a.z as i64).min(road.b.z as i64);
        let mut max_z = (road.a.z as i64).max(road.b.z as i64);
        if road.shape == RoadShape::Corner {
            min_x = min_x.min(via.x as i64);
            max_x = max_x.max(via.x as i64);
            min_z = min_z.min(via.z as i64);
            max_z = max_z.max(via.z as i64);
        }
        Self {
            min_x: min_x - margin,
            max_x: max_x + margin,
            min_z: min_z - margin,
            max_z: max_z + margin,
        }
    }

    fn from_cells(cells: &HashSet<IVec3>) -> Option<Self> {
        let mut cells = cells.iter();
        let first = cells.next()?;
        let mut bounds = Self {
            min_x: first.x as i64,
            max_x: first.x as i64,
            min_z: first.z as i64,
            max_z: first.z as i64,
        };
        for cell in cells {
            bounds.min_x = bounds.min_x.min(cell.x as i64);
            bounds.max_x = bounds.max_x.max(cell.x as i64);
            bounds.min_z = bounds.min_z.min(cell.z as i64);
            bounds.max_z = bounds.max_z.max(cell.z as i64);
        }
        Some(bounds)
    }

    fn intersects(self, other: Self) -> bool {
        self.min_x <= other.max_x
            && self.max_x >= other.min_x
            && self.min_z <= other.max_z
            && self.max_z >= other.min_z
    }
}

fn road_owned_plan_positions(world: &VoxelWorld, road: &RoadSegment) -> HashSet<IVec3> {
    road_full_plan(world, road)
        .into_iter()
        .filter_map(|(pos, expected)| {
            (world.voxel_at(pos.x, pos.y, pos.z) == expected
                && world.material_at(pos.x, pos.y, pos.z) == DEFAULT_MATERIAL)
                .then_some(pos)
        })
        .collect()
}

fn road_network_replacement_plan(
    world: &VoxelWorld,
    roads: &[RoadSegment],
    edited_index: usize,
    edited: &RoadSegment,
    affected: &HashSet<IVec3>,
    style_only: bool,
) -> HashMap<IVec3, Voxel> {
    let Some(affected_bounds) = RoadBoundsXZ::from_cells(affected) else {
        return HashMap::new();
    };
    let mut replacements = HashMap::with_capacity(affected.len());
    for (index, road) in roads.iter().enumerate() {
        let road = if index == edited_index { edited } else { road };
        if !RoadBoundsXZ::for_component(*road).intersects(affected_bounds) {
            continue;
        }
        let plan = if style_only {
            road_style_plan(world, road)
        } else {
            road_full_plan(world, road)
        };
        for (pos, voxel) in plan {
            if affected.contains(&pos) {
                replacements.insert(pos, voxel);
            }
        }
    }
    replacements
}

fn restamp_road_component_in_network(
    world: &mut VoxelWorld,
    roads: &[RoadSegment],
    edited_index: usize,
    before: &RoadSegment,
    after: &RoadSegment,
) -> usize {
    if roads.get(edited_index) != Some(before) {
        return restamp_road_component(world, before, after);
    }

    match road_restamp_kind(before, after) {
        RoadRestampKind::ReassignStyle => {
            let before_plan = road_style_plan(world, before);
            let after_plan = road_style_plan(world, after);
            let affected: HashSet<_> = before_plan
                .keys()
                .chain(after_plan.keys())
                .copied()
                .collect();
            let replacements =
                road_network_replacement_plan(world, roads, edited_index, after, &affected, true);
            let mut transaction = RoadEditTransaction::new(world);
            for pos in affected {
                transaction.replace_owned_cell(
                    pos,
                    before_plan.get(&pos).copied(),
                    replacements.get(&pos).copied(),
                );
            }
            transaction.finish()
        }
        RoadRestampKind::RebuildGeometry => {
            let mut affected = road_owned_plan_positions(world, before);
            affected.extend(road_full_plan(world, after).into_keys());
            let replacements =
                road_network_replacement_plan(world, roads, edited_index, after, &affected, false);
            let mut transaction = RoadEditTransaction::new(world);
            clear_road_component_in(&mut transaction, before);
            stamp_road_in(&mut transaction, after);
            for (pos, voxel) in replacements {
                transaction.set_cell(pos, voxel);
            }
            transaction.finish()
        }
    }
}

fn restamp_road_component(
    world: &mut VoxelWorld,
    before: &RoadSegment,
    after: &RoadSegment,
) -> usize {
    match road_restamp_kind(before, after) {
        RoadRestampKind::ReassignStyle => reassign_road_component_style(world, before, after),
        RoadRestampKind::RebuildGeometry => {
            let mut transaction = RoadEditTransaction::new(world);
            clear_road_component_in(&mut transaction, before);
            stamp_road_in(&mut transaction, after);
            transaction.finish()
        }
    }
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
    let affected = road_owned_plan_positions(world, &road);
    let affected_bounds = RoadBoundsXZ::from_cells(&affected);
    let mut replacements = HashMap::with_capacity(affected.len());
    if let Some(affected_bounds) = affected_bounds {
        for remaining in roads.iter() {
            if !RoadBoundsXZ::for_component(*remaining).intersects(affected_bounds) {
                continue;
            }
            for (pos, voxel) in road_full_plan(world, remaining) {
                if affected.contains(&pos) {
                    replacements.insert(pos, voxel);
                }
            }
        }
    }

    let mut transaction = RoadEditTransaction::new(world);
    clear_road_component_in(&mut transaction, &road);
    for (pos, voxel) in replacements {
        transaction.set_cell(pos, voxel);
    }
    Some(transaction.finish())
}

fn clear_road_component_in(transaction: &mut RoadEditTransaction<'_>, seg: &RoadSegment) {
    let surface_plan = road_surface_plan(transaction.world, seg);
    let furniture_plan = road_furniture_plan(transaction.world, seg);
    let support_plan = road_support_plan(transaction.world, seg);

    for (pos, expected) in furniture_plan {
        transaction.replace_owned_cell(pos, Some(expected), None);
    }

    for (pos, expected) in surface_plan {
        let surface_y = transaction.world.surface_height_at(pos.x, pos.z);
        if pos.y <= surface_y {
            let restore = terrain_surface_restore_voxel(transaction.world, pos.x, pos.z);
            if transaction.owns_cell(pos, Some(expected)) {
                transaction.set_cell(pos, restore);
                for y in (pos.y + 1)..=surface_y {
                    let fill = IVec3::new(pos.x, y, pos.z);
                    if transaction.world.voxel_at(fill.x, fill.y, fill.z) == AIR {
                        transaction.set_cell(fill, restore);
                    }
                }
            }
        } else {
            transaction.replace_owned_cell(pos, Some(expected), None);
        }
    }

    for (pos, expected) in support_plan {
        transaction.replace_owned_cell(pos, Some(expected), None);
    }
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
    let mut transaction = RoadEditTransaction::new(world);
    stamp_road_in(&mut transaction, seg);
    transaction.finish()
}

fn stamp_road_in(transaction: &mut RoadEditTransaction<'_>, seg: &RoadSegment) {
    let surface_plan = road_surface_plan(transaction.world, seg);
    if surface_plan.is_empty() {
        return;
    }
    let support_plan = road_support_plan(transaction.world, seg);
    let furniture_plan = road_furniture_plan(transaction.world, seg);

    for pos in surface_plan.keys().copied() {
        let surface_y = transaction.world.surface_height_at(pos.x, pos.z);
        for clear_y in (pos.y + 1)..=(pos.y + 3) {
            if transaction.world.is_solid(pos.x, clear_y, pos.z) {
                transaction.set_cell(IVec3::new(pos.x, clear_y, pos.z), AIR);
            }
        }
        if pos.y < surface_y {
            for cut_y in (pos.y + 1)..=surface_y {
                if transaction.world.is_solid(pos.x, cut_y, pos.z) {
                    transaction.set_cell(IVec3::new(pos.x, cut_y, pos.z), AIR);
                }
            }
        }
    }

    for (pos, voxel) in support_plan {
        transaction.set_cell(pos, voxel);
    }
    for (pos, voxel) in surface_plan {
        transaction.set_cell(pos, voxel);
    }
    for (pos, voxel) in furniture_plan {
        transaction.set_cell(pos, voxel);
    }
}

fn road_surface_plan(world: &VoxelWorld, seg: &RoadSegment) -> HashMap<IVec3, Voxel> {
    let cells = road_path_xz(seg);
    if cells.is_empty() {
        return HashMap::new();
    }
    let cross_section = RoadCrossSection::for_width(seg.width);
    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(*seg, &cells).min(last_index);
    let capacity = cells
        .len()
        .saturating_mul(cross_section.width())
        .min(ROAD_MAX_CENTERLINE_SAMPLES * ROAD_MAX_WIDTH);
    let mut plan = HashMap::with_capacity(capacity);
    for (i, c) in cells.iter().enumerate() {
        let (perp_x, perp_z) = road_width_axis_at(&cells, i);
        for w in cross_section.offsets() {
            let wx = c.x.saturating_add(perp_x.saturating_mul(w));
            let wz = c.y.saturating_add(perp_z.saturating_mul(w));
            let deck_y =
                road_deck_y_at_sample(world, seg, wx, wz, i, last_index, turn_index).max(1);
            plan.insert(
                IVec3::new(wx, deck_y, wz),
                road_surface_voxel(*seg, i, w, cross_section),
            );
        }
    }
    plan
}

fn road_support_plan(world: &VoxelWorld, seg: &RoadSegment) -> HashMap<IVec3, Voxel> {
    let cells = road_path_xz(seg);
    let cross_section = RoadCrossSection::for_width(seg.width);
    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(*seg, &cells).min(last_index);
    let mut plan = HashMap::new();
    'centerline: for (i, c) in cells.iter().enumerate() {
        let (perp_x, perp_z) = road_width_axis_at(&cells, i);
        for w in cross_section.offsets() {
            let wx = c.x.saturating_add(perp_x.saturating_mul(w));
            let wz = c.y.saturating_add(perp_z.saturating_mul(w));
            let surface_y = world.surface_height_at(wx, wz);
            let deck_y =
                road_deck_y_at_sample(world, seg, wx, wz, i, last_index, turn_index).max(1);
            let edge_or_pier = cross_section.is_outer_edge(w) || (w == 0 && i % 5 == 0);
            if deck_y > surface_y + 1 && edge_or_pier {
                for y in (surface_y + 1)..deck_y {
                    if plan.len() >= ROAD_MAX_SUPPORT_VOXELS {
                        break 'centerline;
                    }
                    plan.insert(IVec3::new(wx, y, wz), Voxel::from(BlockType::Basalt));
                }
            }
        }
    }
    plan
}

fn road_furniture_plan(world: &VoxelWorld, seg: &RoadSegment) -> HashMap<IVec3, Voxel> {
    let cells = road_path_xz(seg);
    let cross_section = RoadCrossSection::for_width(seg.width);
    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(*seg, &cells).min(last_index);
    let mut plan = HashMap::new();
    'centerline: for (i, c) in cells.iter().enumerate() {
        let (perp_x, perp_z) = road_width_axis_at(&cells, i);
        for w in cross_section.offsets() {
            let Some(_) = road_furniture_voxel(*seg, i, w, cross_section, 1) else {
                continue;
            };
            let wx = c.x.saturating_add(perp_x.saturating_mul(w));
            let wz = c.y.saturating_add(perp_z.saturating_mul(w));
            let deck_y =
                road_deck_y_at_sample(world, seg, wx, wz, i, last_index, turn_index).max(1);
            for y_offset in 1..=4 {
                if let Some(voxel) = road_furniture_voxel(*seg, i, w, cross_section, y_offset) {
                    if plan.len() >= ROAD_MAX_FURNITURE_VOXELS {
                        break 'centerline;
                    }
                    plan.insert(IVec3::new(wx, deck_y + y_offset, wz), voxel);
                }
            }
        }
    }
    plan
}

fn road_style_plan(world: &VoxelWorld, seg: &RoadSegment) -> HashMap<IVec3, Voxel> {
    let mut plan = road_surface_plan(world, seg);
    plan.extend(road_furniture_plan(world, seg));
    plan
}

fn road_full_plan(world: &VoxelWorld, seg: &RoadSegment) -> HashMap<IVec3, Voxel> {
    let mut plan = road_support_plan(world, seg);
    plan.extend(road_surface_plan(world, seg));
    plan.extend(road_furniture_plan(world, seg));
    plan
}

// Retexturing is deliberately narrower than a geometry rebuild: only cells
// still carrying this component's old default material are reassigned.
fn reassign_road_component_style(
    world: &mut VoxelWorld,
    before: &RoadSegment,
    after: &RoadSegment,
) -> usize {
    let before_plan = road_style_plan(world, before);
    let after_plan = road_style_plan(world, after);
    let mut transaction = RoadEditTransaction::new(world);

    for (&pos, &expected) in &before_plan {
        transaction.replace_owned_cell(pos, Some(expected), after_plan.get(&pos).copied());
    }
    for (&pos, &replacement) in &after_plan {
        if !before_plan.contains_key(&pos) {
            transaction.replace_owned_cell(pos, None, Some(replacement));
        }
    }

    transaction.finish()
}

fn road_lane_surface_voxel(
    seg: RoadSegment,
    w: i32,
    cross_section: RoadCrossSection,
    surface: Voxel,
) -> Voxel {
    if !cross_section.is_boulevard() || seg.style == RoadStyle::Dirt {
        return surface;
    }
    if cross_section.is_outer_edge(w) {
        Voxel::from(BlockType::ShipHullDark)
    } else if cross_section.is_sidewalk(w) {
        Voxel::from(BlockType::Limestone)
    } else {
        surface
    }
}

fn road_surface_voxel(
    seg: RoadSegment,
    index: usize,
    w: i32,
    cross_section: RoadCrossSection,
) -> Voxel {
    if w == 0 && index % 3 == 0 {
        if let Some(stripe) = seg.style.stripe_block() {
            return Voxel::from(stripe);
        }
    }
    road_lane_surface_voxel(
        seg,
        w,
        cross_section,
        Voxel::from(seg.style.surface_block()),
    )
}

fn road_furniture_voxel(
    seg: RoadSegment,
    index: usize,
    w: i32,
    cross_section: RoadCrossSection,
    y_offset: i32,
) -> Option<Voxel> {
    if !cross_section.is_boulevard()
        || seg.style == RoadStyle::Dirt
        || !cross_section.is_outer_edge(w)
        || index % 36 != 0
    {
        return None;
    }
    match y_offset {
        1..=3 => Some(Voxel::from(BlockType::ShipHullDark)),
        4 => Some(Voxel::from(BlockType::GlowSand)),
        _ => None,
    }
}

fn road_deck_y_at_sample(
    world: &VoxelWorld,
    seg: &RoadSegment,
    wx: i32,
    wz: i32,
    index: usize,
    last_index: usize,
    turn_index: usize,
) -> i32 {
    if !road_has_manual_grade(seg) {
        return world.surface_height_at(wx, wz).max(1);
    }
    match seg.shape {
        RoadShape::Roundabout => {
            let lift = (seg.elevation_a as i32 + seg.elevation_b as i32) as f32 * 0.5;
            (world.surface_height_at(seg.a.x, seg.a.z) as f32 + lift)
                .round()
                .max(1.0) as i32
        }
        RoadShape::Corner => {
            let via = road_corner_via(*seg);
            let a = world.surface_height_at(seg.a.x, seg.a.z) as f32 + seg.elevation_a as f32;
            let v = world.surface_height_at(via.x, via.z) as f32 + seg.elevation_via as f32;
            let b = world.surface_height_at(seg.b.x, seg.b.z) as f32 + seg.elevation_b as f32;
            if index <= turn_index {
                let t = sample_t(index, turn_index);
                lerp_grade(a, v, smoothstep(t)).round().max(1.0) as i32
            } else {
                let t = sample_t(
                    index.saturating_sub(turn_index),
                    last_index.saturating_sub(turn_index),
                );
                lerp_grade(v, b, smoothstep(t)).round().max(1.0) as i32
            }
        }
        RoadShape::Straight => {
            let t = smoothstep(sample_t(index, last_index));
            let a = world.surface_height_at(seg.a.x, seg.a.z) as f32 + seg.elevation_a as f32;
            let b = world.surface_height_at(seg.b.x, seg.b.z) as f32 + seg.elevation_b as f32;
            lerp_grade(a, b, t).round().max(1.0) as i32
        }
    }
}

fn road_component_y_at_sample(
    seg: &RoadSegment,
    index: usize,
    last_index: usize,
    turn_index: usize,
) -> i32 {
    match seg.shape {
        RoadShape::Roundabout => {
            let lift = (seg.elevation_a as i32 + seg.elevation_b as i32) as f32 * 0.5;
            (seg.a.y as f32 + lift).round().max(1.0) as i32
        }
        RoadShape::Corner => {
            let via = road_corner_via(*seg);
            let a = seg.a.y as f32 + seg.elevation_a as f32;
            let v = via.y as f32 + seg.elevation_via as f32;
            let b = seg.b.y as f32 + seg.elevation_b as f32;
            if index <= turn_index {
                let t = sample_t(index, turn_index);
                lerp_grade(a, v, smoothstep(t)).round().max(1.0) as i32
            } else {
                let t = sample_t(
                    index.saturating_sub(turn_index),
                    last_index.saturating_sub(turn_index),
                );
                lerp_grade(v, b, smoothstep(t)).round().max(1.0) as i32
            }
        }
        RoadShape::Straight => {
            let t = smoothstep(sample_t(index, last_index));
            let a = seg.a.y as f32 + seg.elevation_a as f32;
            let b = seg.b.y as f32 + seg.elevation_b as f32;
            lerp_grade(a, b, t).round().max(1.0) as i32
        }
    }
}

fn road_has_manual_grade(seg: &RoadSegment) -> bool {
    match seg.shape {
        RoadShape::Corner => seg.elevation_a != 0 || seg.elevation_via != 0 || seg.elevation_b != 0,
        RoadShape::Straight | RoadShape::Roundabout => seg.elevation_a != 0 || seg.elevation_b != 0,
    }
}

#[cfg(test)]
fn road_elevation_at_sample(seg: &RoadSegment, index: usize, last_index: usize) -> i32 {
    if last_index == 0 {
        return seg.elevation_a as i32;
    }
    if seg.shape == RoadShape::Corner {
        let cells = road_path_xz(seg);
        let turn_index = road_corner_turn_index(*seg, &cells).min(last_index);
        let (start, end, local_t) = if index <= turn_index {
            (
                seg.elevation_a as f32,
                seg.elevation_via as f32,
                sample_t(index, turn_index),
            )
        } else {
            (
                seg.elevation_via as f32,
                seg.elevation_b as f32,
                sample_t(
                    index.saturating_sub(turn_index),
                    last_index.saturating_sub(turn_index),
                ),
            )
        };
        return lerp_grade(start, end, smoothstep(local_t)).round() as i32;
    }
    let t = sample_t(index, last_index);
    let start = seg.elevation_a as f32;
    let end = seg.elevation_b as f32;
    lerp_grade(start, end, smoothstep(t)).round() as i32
}

fn sample_t(index: usize, last_index: usize) -> f32 {
    if last_index == 0 {
        0.0
    } else {
        (index as f32 / last_index as f32).clamp(0.0, 1.0)
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp_grade(start: f32, end: f32, t: f32) -> f32 {
    start + (end - start) * t
}

// ---------------------------------------------------------------------
// Building stamping (CB)
// ---------------------------------------------------------------------

/// Stamp a procedural rectangular building onto the terrain. Flat roof,
/// perimeter walls with simple door/window openings, hollow interior
/// with floor slabs every 3 blocks of height. Returns the number of
/// voxels changed.
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
                    let v = if voxel_on_door(h, wx, wz, &bld) || voxel_on_window(h, wx, wz, &bld) {
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

/// Heuristic door opening: carve a 1-wide, 2-high gap in the middle of
/// the shorter side. Returns true when `(wx, wz, h)` falls on the door
/// tile so [`stamp_building`] can skip the wall.
fn voxel_on_door(h: i32, wx: i32, wz: i32, bld: &Building) -> bool {
    if !(1..=2).contains(&h) {
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

/// Repeated cheap facade openings, aligned to the voxel storey height.
/// This gives one-drag building shells an immediately readable interior
/// without adding mesh complexity or per-building facade state.
fn voxel_on_window(h: i32, wx: i32, wz: i32, bld: &Building) -> bool {
    if h < 2 || (h % 3) != 2 {
        return false;
    }

    let w = bld.max.x - bld.min.x;
    let d = bld.max.z - bld.min.z;
    if w < 4 || d < 4 {
        return false;
    }

    let on_x_edge = wx == bld.min.x || wx == bld.max.x;
    let on_z_edge = wz == bld.min.z || wz == bld.max.z;
    if on_x_edge && wz > bld.min.z && wz < bld.max.z {
        return (wz - bld.min.z) % 4 == 2;
    }
    if on_z_edge && wx > bld.min.x && wx < bld.max.x {
        return (wx - bld.min.x) % 4 == 2;
    }
    false
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
    runtime: Res<RuntimeBudget>,
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
    let road_budget = RoadRuntimeBudget::for_profile(runtime.profile);

    // --- Committed roads ---------------------------------------------
    let mut remaining_gizmo_segments = road_budget.max_gizmo_segments;
    for idx in road_gizmo_component_indices(
        city.roads.len(),
        city.selected_road,
        road_budget.max_visible_components,
    ) {
        if remaining_gizmo_segments == 0 {
            break;
        }
        let r = &city.roads[idx];
        let selected = city.selected_road == Some(idx);
        let col = if selected {
            Color::srgb(1.0, 0.84, 0.22)
        } else {
            r.style.gizmo_color()
        };
        draw_road_component_gizmo(
            &mut gizmos,
            &world,
            r,
            col,
            selected,
            &mut remaining_gizmo_segments,
        );
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
                cursor = road_target_within_run_budget(a, cursor, road_budget.max_component_run);
            }
            let preview = city.pending_road_a.map(|a| {
                road_segment_from_drag_with_budget(
                    a,
                    cursor,
                    city.road_width,
                    city.road_style,
                    &city.roads,
                    road_budget,
                )
            });
            let preview_color = preview
                .as_ref()
                .map(|road| road.style.gizmo_color())
                .unwrap_or_else(|| city.road_style.gizmo_color());
            let c_world = cursor.as_vec3() + Vec3::new(0.5, 1.5, 0.5);
            // Cursor marker — pulses so the user never loses it.
            gizmos.sphere(c_world, Quat::IDENTITY, 0.8 + pulse * 0.3, preview_color);
            if let Some(preview) = preview {
                let mut preview_segments = road_budget.max_preview_gizmo_segments;
                draw_road_component_gizmo(
                    &mut gizmos,
                    &world,
                    &preview,
                    preview_color,
                    true,
                    &mut preview_segments,
                );
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
            let col = city.district_kind.color();
            if let Some(a) = city.pending_district_a {
                let (min, max) = city_area_corners(a, cursor, city.district_radius);
                draw_footprint(&mut gizmos, min, max, col, 3);
                let center = IVec3::new((min.x + max.x) / 2, min.y, (min.z + max.z) / 2);
                circle_xz(
                    &mut gizmos,
                    center,
                    (((max.x - min.x + 1) as f32 * 0.5).powi(2)
                        + ((max.z - min.z + 1) as f32 * 0.5).powi(2))
                    .sqrt(),
                    col.with_alpha(0.45),
                );
            } else {
                circle_xz(&mut gizmos, cursor, city.district_radius as f32, col);
            }
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

fn road_gizmo_component_indices(
    road_count: usize,
    selected: Option<usize>,
    max_components: usize,
) -> Vec<usize> {
    if max_components == 0 {
        return Vec::new();
    }
    let mut indices = Vec::with_capacity(max_components.min(road_count));
    let selected = selected.filter(|idx| *idx < road_count);
    if let Some(idx) = selected {
        indices.push(idx);
    }
    for idx in (0..road_count).rev() {
        if indices.len() >= max_components {
            break;
        }
        if Some(idx) != selected {
            indices.push(idx);
        }
    }
    indices
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
    remaining_segments: &mut usize,
) {
    let cells = road_path_xz(road);
    if cells.is_empty() {
        return;
    }

    let last_index = cells.len().saturating_sub(1);
    let turn_index = road_corner_turn_index(*road, &cells).min(last_index);
    let point_at = |i: usize, cell: IVec2| -> Vec3 {
        let deck_y =
            road_deck_y_at_sample(world, road, cell.x, cell.y, i, last_index, turn_index).max(1);
        Vec3::new(
            cell.x as f32 + 0.5,
            deck_y as f32 + 1.2,
            cell.y as f32 + 0.5,
        )
    };

    let available =
        ((*remaining_segments / 3).max(usize::from(*remaining_segments > 0))).min(last_index);
    let stride = if available == 0 {
        last_index.max(1)
    } else {
        last_index.saturating_add(available - 1) / available
    };
    let cross_section = RoadCrossSection::for_width(road.width);
    let mut i = 0;
    while i < last_index && *remaining_segments > 0 {
        let next = i.saturating_add(stride).min(last_index);
        let a = point_at(i, cells[i]);
        let b = point_at(next, cells[next]);
        gizmos.line(a, b, color);
        *remaining_segments -= 1;

        let faint = color.with_alpha(0.45);
        let (a_px, a_pz) = road_width_axis_at(&cells, i);
        let (b_px, b_pz) = road_width_axis_at(&cells, next);
        if *remaining_segments > 0 {
            let a_flank = Vec3::new(
                a_px as f32 * cross_section.min_boundary(),
                0.0,
                a_pz as f32 * cross_section.min_boundary(),
            );
            let b_flank = Vec3::new(
                b_px as f32 * cross_section.min_boundary(),
                0.0,
                b_pz as f32 * cross_section.min_boundary(),
            );
            gizmos.line(a + a_flank, b + b_flank, faint);
            *remaining_segments -= 1;
        }
        if *remaining_segments > 0 {
            let a_flank = Vec3::new(
                a_px as f32 * cross_section.max_boundary(),
                0.0,
                a_pz as f32 * cross_section.max_boundary(),
            );
            let b_flank = Vec3::new(
                b_px as f32 * cross_section.max_boundary(),
                0.0,
                b_pz as f32 * cross_section.max_boundary(),
            );
            gizmos.line(a + a_flank, b + b_flank, faint);
            *remaining_segments -= 1;
        }
        i = next;
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
            let via_y = road_deck_y_at_sample(
                world, road, via.x, via.z, turn_index, last_index, turn_index,
            )
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
                    lines.push((
                        "LMB release".into(),
                        "Strassenende zeichnen + weiterfuehren".into(),
                    ));
                    lines.push(("Auto".into(), "Laenge / Brueckenhoehe erben".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push((
                        "LMB hold".into(),
                        "Strassenstart setzen, ziehen, loslassen".into(),
                    ));
                    lines.push(("RMB".into(), "Letzte Strasse loeschen".into()));
                }
                if let Some(idx) = city.selected_road {
                    lines.push((
                        format!("Komponente {}", idx + 1),
                        "direkt editierbar".into(),
                    ));
                    lines.push((
                        "Wheel".into(),
                        "Koerper=Breite/Radius, Griff=Brueckenhoehe".into(),
                    ));
                    lines.push(("Ctrl+Wheel".into(), "Breite / Kreisradius".into()));
                    lines.push((
                        "Shift+Wheel".into(),
                        "Brueckenhoehe am naechsten Ende".into(),
                    ));
                    lines.push(("MMB / Alt+Wheel".into(), "Textur direkt wechseln".into()));
                }
                lines.push(("[ / ]".into(), format!("Breite ({})", city.road_width)));
                lines.push(("N".into(), "Strassen-Tool AUS".into()));
            }
            CityTool::District => {
                if city.pending_district_a.is_some() {
                    lines.push(("LMB release".into(), "Bot-Stadtflaeche zeichnen".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push(("LMB hold".into(), "Ecke A setzen, ziehen, loslassen".into()));
                    lines.push(("RMB".into(), "Letzten Bezirk loeschen".into()));
                }
                lines.push(("[ / ]".into(), format!("Radius ({})", city.district_radius)));
                lines.push(("T".into(), "Bezirks-Tool AUS".into()));
            }
            CityTool::Building => {
                if city.pending_building_a.is_some() {
                    lines.push(("LMB release".into(), "Gebaeude-Footprint zeichnen".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push(("LMB hold".into(), "Ecke A setzen, ziehen, loslassen".into()));
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

fn city_placement_ray(
    window: Option<&bevy::window::Window>,
    camera: &Camera,
    camera_transform: &GlobalTransform,
) -> Option<(Vec3, Vec3)> {
    if let Some(window) = window {
        if window.cursor.visible {
            return window
                .cursor_position()
                .and_then(|cursor| camera.viewport_to_world(camera_transform, cursor))
                .map(|ray| (ray.origin, *ray.direction));
        }
    }

    Some((
        camera_transform.translation(),
        camera_transform.forward().as_vec3(),
    ))
}

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
    fn road_restamp_classifies_style_changes_without_rebuilding_geometry() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            7,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(2, 8)
        .with_turn_height(5);

        assert_eq!(
            road_restamp_kind(&road, &road.retextured(RoadStyle::Cobble)),
            RoadRestampKind::ReassignStyle
        );
        assert_eq!(
            road_restamp_kind(&road, &road.with_width(9)),
            RoadRestampKind::RebuildGeometry
        );
        assert_eq!(
            road_restamp_kind(&road, &road.with_turn_height(6)),
            RoadRestampKind::RebuildGeometry
        );
    }

    #[test]
    fn road_surface_material_helper_distinguishes_lane_stripe_and_curb() {
        let asphalt = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 0),
            9,
            RoadStyle::Asphalt,
        );
        let dirt = asphalt.retextured(RoadStyle::Dirt);
        let cross_section = RoadCrossSection::for_width(asphalt.width);

        assert_eq!(
            road_surface_voxel(asphalt, 1, 0, cross_section),
            Voxel::from(BlockType::Stone)
        );
        assert_eq!(
            road_surface_voxel(asphalt, 3, 0, cross_section),
            Voxel::from(BlockType::Snow),
            "asphalt center stripes should win over the lane surface"
        );
        assert_eq!(
            road_surface_voxel(asphalt, 1, 4, cross_section),
            Voxel::from(BlockType::ShipHullDark)
        );
        assert_eq!(
            road_surface_voxel(asphalt, 1, 3, cross_section),
            Voxel::from(BlockType::Limestone)
        );
        assert_eq!(
            road_surface_voxel(dirt, 3, 4, cross_section),
            Voxel::from(BlockType::Dirt),
            "dirt roads should not inherit asphalt stripes or curb materials"
        );
    }

    #[test]
    fn road_edit_transaction_batches_and_counts_only_real_cell_changes() {
        let mut world = VoxelWorld::new();
        let pos = IVec3::new(3, 180, -4);
        let changed = {
            let mut transaction = RoadEditTransaction::new(&mut world);
            assert!(transaction.set_cell(pos, Voxel::from(BlockType::Stone)));
            assert!(!transaction.set_cell(pos, Voxel::from(BlockType::Stone)));
            transaction.finish()
        };

        assert_eq!(changed, 1);
        assert_eq!(
            world.voxel_at(pos.x, pos.y, pos.z),
            Voxel::from(BlockType::Stone)
        );
        assert_eq!(world.material_at(pos.x, pos.y, pos.z), DEFAULT_MATERIAL);
    }

    #[test]
    fn road_style_reassignment_preserves_foreign_and_custom_material_cells() {
        let mut world = VoxelWorld::new();
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 0),
            5,
            RoadStyle::Asphalt,
        );
        let cobble = road.retextured(RoadStyle::Cobble);
        stamp_road(&mut world, &road);

        let before_plan = road_surface_plan(&world, &road);
        let after_plan = road_surface_plan(&world, &cobble);
        let mut candidates: Vec<IVec3> = before_plan
            .iter()
            .filter_map(|(pos, voxel)| {
                (*voxel == Voxel::from(BlockType::Stone)
                    && after_plan.get(pos) == Some(&Voxel::from(BlockType::MossStone)))
                .then_some(*pos)
            })
            .collect();
        candidates.sort_by_key(|pos| (pos.x, pos.y, pos.z));
        assert!(candidates.len() >= 3);
        let foreign = candidates[0];
        let custom = candidates[1];
        let owned = candidates[2];
        let foreign_above = owned + IVec3::Y * 2;

        world.edit_set_voxel(
            foreign.x,
            foreign.y,
            foreign.z,
            Voxel::from(BlockType::Wood),
        );
        world.edit_set_voxel(
            foreign_above.x,
            foreign_above.y,
            foreign_above.z,
            Voxel::from(BlockType::Wood),
        );
        let mut batch = WorldEditBatch::default();
        world.edit_set_cell_batched(
            custom.x,
            custom.y,
            custom.z,
            Voxel::from(BlockType::Stone),
            crate::blocks::CUSTOM_MATERIAL_BASE,
            &mut batch,
        );
        world.finish_edit_batch(batch);

        let changed = restamp_road_component(&mut world, &road, &cobble);

        assert!(changed > 0);
        assert_eq!(
            world.voxel_at(owned.x, owned.y, owned.z),
            Voxel::from(BlockType::MossStone)
        );
        assert_eq!(
            world.voxel_at(foreign.x, foreign.y, foreign.z),
            Voxel::from(BlockType::Wood),
            "a style-only edit must not clear or overwrite a foreign voxel"
        );
        assert_eq!(
            world.voxel_at(foreign_above.x, foreign_above.y, foreign_above.z),
            Voxel::from(BlockType::Wood),
            "a style-only edit must not recarve the space above the road"
        );
        assert_eq!(
            world.voxel_at(custom.x, custom.y, custom.z),
            Voxel::from(BlockType::Stone)
        );
        assert_eq!(
            world.material_at(custom.x, custom.y, custom.z),
            crate::blocks::CUSTOM_MATERIAL_BASE,
            "explicit custom material overrides should survive component retexturing"
        );
    }

    #[test]
    fn raised_road_style_reassignment_keeps_the_existing_deck_geometry() {
        let mut world = VoxelWorld::new();
        let start = IVec3::new(0, world.surface_height_at(0, 0), 0);
        let end = IVec3::new(96, world.surface_height_at(96, 0), 0);
        let road = RoadSegment::new(start, end, 5, RoadStyle::Asphalt).with_endpoint_heights(0, 24);
        let cobble = road.retextured(RoadStyle::Cobble);
        let before_plan = road_surface_plan(&world, &road);
        let after_plan = road_surface_plan(&world, &cobble);

        assert_eq!(before_plan.len(), after_plan.len());
        assert!(before_plan.keys().all(|pos| after_plan.contains_key(pos)));
        let edited_cell = before_plan
            .iter()
            .find_map(|(pos, voxel)| {
                (*voxel == Voxel::from(BlockType::Stone)
                    && after_plan.get(pos) == Some(&Voxel::from(BlockType::MossStone)))
                .then_some(*pos)
            })
            .expect("raised asphalt deck should contain an editable surface cell");

        stamp_road(&mut world, &road);
        let changed = restamp_road_component(&mut world, &road, &cobble);

        assert!(changed > 0);
        assert_eq!(
            world.voxel_at(edited_cell.x, edited_cell.y, edited_cell.z),
            Voxel::from(BlockType::MossStone),
            "style edits must reuse the raised component footprint instead of rebuilding its grade"
        );
    }

    #[test]
    fn quick_road_texture_cycle_preserves_component_edit_state() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(24, 72, 16),
            7,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(3, 9)
        .with_turn_height(6);

        let retextured = road_with_texture_delta(road, 1);

        assert_eq!(retextured.a, road.a);
        assert_eq!(retextured.b, road.b);
        assert_eq!(retextured.via, road.via);
        assert_eq!(retextured.shape, road.shape);
        assert_eq!(retextured.width, road.width);
        assert_eq!(retextured.elevation_a, road.elevation_a);
        assert_eq!(retextured.elevation_via, road.elevation_via);
        assert_eq!(retextured.elevation_b, road.elevation_b);
        assert_eq!(retextured.style, RoadStyle::Cobble);
        assert_eq!(road_with_texture_delta(retextured, -1).style, road.style);
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
    fn default_road_tool_starts_at_boulevard_scale() {
        let city = CityState::default();
        let roundabout = RoadSegment::new(
            IVec3::new(16, 72, 16),
            IVec3::new(16, 72, 16),
            city.road_width,
            RoadStyle::Asphalt,
        );

        assert_eq!(city.road_width, 7);
        assert_eq!(roundabout.roundabout_radius, 14);
        assert_eq!(roundabout.width, 7);
    }

    #[test]
    fn road_cross_sections_stamp_exact_requested_width_in_both_directions() {
        let world = VoxelWorld::new();

        for width in 1..=ROAD_MAX_WIDTH as u8 {
            let eastbound = RoadSegment::new(
                IVec3::new(-16, 72, 0),
                IVec3::new(16, 72, 0),
                width,
                RoadStyle::Asphalt,
            );
            let westbound = RoadSegment::new(eastbound.b, eastbound.a, width, RoadStyle::Asphalt);
            let east_plan = road_surface_plan(&world, &eastbound);
            let west_plan = road_surface_plan(&world, &westbound);
            let east_slice: std::collections::HashSet<_> = east_plan
                .keys()
                .filter(|pos| pos.x == 0)
                .map(|pos| (pos.y, pos.z))
                .collect();
            let west_slice: std::collections::HashSet<_> = west_plan
                .keys()
                .filter(|pos| pos.x == 0)
                .map(|pos| (pos.y, pos.z))
                .collect();

            assert_eq!(
                east_slice.len(),
                width as usize,
                "stored width {width} must stamp exactly {width} cells"
            );
            assert_eq!(
                east_slice, west_slice,
                "reversing a road must not move an even-width footprint"
            );
        }
    }

    #[test]
    fn asymmetric_corner_grade_reaches_turn_height_at_the_actual_curve() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(48, 72, 16),
            7,
            RoadStyle::Cobble,
        )
        .with_turn_height(12);
        let path = road_path_xz(&road);
        let last_index = path.len().saturating_sub(1);
        let turn_index = road_corner_turn_index(road, &path);
        let heights: Vec<i32> = (0..=last_index)
            .map(|index| road_elevation_at_sample(&road, index, last_index))
            .collect();

        assert!(turn_index > last_index / 2);
        assert_eq!(heights[turn_index], road.elevation_via as i32);
        assert!(heights[last_index / 2] < heights[turn_index]);
        assert_eq!(heights.first().copied(), Some(0));
        assert_eq!(heights.last().copied(), Some(0));
        assert!(heights.windows(2).all(|pair| {
            (pair[1] - pair[0]).abs() <= ROAD_MAX_SAMPLED_RISE_PER_INTERVAL as i32
        }));
    }

    #[test]
    fn loaded_roads_are_normalized_to_component_and_grade_budgets() {
        let oversized = SavedRoadSegment {
            a: [0, 72, 0],
            b: [10_000, 172, 10_000],
            via: Some([-10_000, 999, 10_000]),
            shape: RoadShape::Straight,
            roundabout_radius: u8::MAX,
            width: u8::MAX,
            style: RoadStyle::Neon,
            elevation_a: ROAD_MIN_ELEVATION,
            elevation_via: ROAD_MAX_ELEVATION,
            elevation_b: ROAD_MAX_ELEVATION,
        };
        let bounded = RoadSegment::from(oversized);
        let run = (bounded.b.x as i64 - bounded.a.x as i64)
            .unsigned_abs()
            .saturating_add((bounded.b.z as i64 - bounded.a.z as i64).unsigned_abs())
            as usize;

        assert_eq!(bounded.width, ROAD_MAX_WIDTH as u8);
        assert_eq!(bounded.shape, RoadShape::Corner);
        assert_eq!(
            bounded.via,
            Some(deterministic_corner_via(bounded.a, bounded.b))
        );
        assert!(run <= ROAD_MAX_CENTERLINE_SAMPLES - 1);
        assert!(road_path_xz(&bounded).len() <= ROAD_MAX_CENTERLINE_SAMPLES);

        let steep = RoadSegment::from(SavedRoadSegment {
            b: [4, 72, 4],
            shape: RoadShape::Corner,
            ..oversized
        });
        let path = road_path_xz(&steep);
        let last_index = path.len().saturating_sub(1);
        let heights: Vec<i32> = (0..=last_index)
            .map(|index| road_elevation_at_sample(&steep, index, last_index))
            .collect();
        assert!(heights.windows(2).all(|pair| {
            (pair[1] - pair[0]).abs() <= ROAD_MAX_SAMPLED_RISE_PER_INTERVAL as i32
        }));
    }

    #[test]
    fn extreme_component_coordinates_are_bounded_without_integer_overflow() {
        let road = RoadSegment::new(
            IVec3::new(i32::MIN, 72, i32::MIN),
            IVec3::new(i32::MAX, 72, i32::MAX),
            ROAD_MAX_WIDTH as u8,
            RoadStyle::Dirt,
        );
        let path = road_path_xz(&road);
        let run = (road.b.x as i64 - road.a.x as i64)
            .unsigned_abs()
            .saturating_add((road.b.z as i64 - road.a.z as i64).unsigned_abs())
            as usize;

        assert!(run <= ROAD_MAX_CENTERLINE_SAMPLES - 1);
        assert!(!path.is_empty());
        assert!(path.len() <= ROAD_MAX_CENTERLINE_SAMPLES);

        let ring = RoadSegment::roundabout(
            IVec3::new(i32::MAX, 72, i32::MIN),
            ROAD_MAX_ROUNDABOUT_RADIUS,
            7,
            RoadStyle::Asphalt,
        );
        assert!(!road_path_xz(&ring).is_empty());
    }

    #[test]
    fn low_spec_road_budget_bounds_geometry_details_and_gizmo_work() {
        let world = VoxelWorld::new();
        let low = RoadRuntimeBudget::for_profile(RuntimeProfile::LowSpec);
        let balanced = RoadRuntimeBudget::for_profile(RuntimeProfile::Balanced);
        let start = IVec3::new(0, world.surface_height_at(0, 0), 0);
        let target = IVec3::new(10_000, world.surface_height_at(10_000, 10_000), 10_000);
        let road = road_segment_from_drag_with_budget(
            start,
            target,
            ROAD_MAX_WIDTH as u8,
            RoadStyle::Neon,
            &[],
            low,
        )
        .with_endpoint_heights(ROAD_MAX_ELEVATION, ROAD_MAX_ELEVATION)
        .with_turn_height(ROAD_MAX_ELEVATION);
        let run = (road.b.x as i64 - road.a.x as i64)
            .unsigned_abs()
            .saturating_add((road.b.z as i64 - road.a.z as i64).unsigned_abs())
            as usize;
        let path = road_path_xz(&road);

        assert!(run <= low.max_component_run);
        assert!(path.len() <= low.max_centerline_samples());
        assert!(road_surface_plan(&world, &road).len() <= low.max_surface_voxels());
        assert!(road_support_plan(&world, &road).len() <= ROAD_MAX_SUPPORT_VOXELS);
        assert!(road_furniture_plan(&world, &road).len() <= ROAD_MAX_FURNITURE_VOXELS);

        let roundabout = road_segment_from_drag_with_budget(
            start,
            start,
            ROAD_MAX_WIDTH as u8,
            RoadStyle::Asphalt,
            &[],
            low,
        );
        assert_eq!(roundabout.shape, RoadShape::Roundabout);
        assert_eq!(roundabout.roundabout_radius, low.max_roundabout_radius);
        assert!(road_path_xz(&roundabout).len() <= low.max_centerline_samples());
        assert!(road_surface_plan(&world, &roundabout).len() <= low.max_surface_voxels());

        let visible = road_gizmo_component_indices(1_000, Some(3), low.max_visible_components);
        assert_eq!(visible.len(), low.max_visible_components);
        assert_eq!(visible.first().copied(), Some(3));
        assert!(low.max_component_run < balanced.max_component_run);
        assert!(low.max_components < balanced.max_components);
        assert!(low.max_gizmo_segments < balanced.max_gizmo_segments);
    }

    #[test]
    fn road_component_edit_clears_old_width_before_restamping() {
        let mut world = VoxelWorld::new();
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(16, 72, 0),
            9,
            RoadStyle::Neon,
        );
        let old_flank_y = world.surface_height_at(8, 3);
        let old_edge_y = world.surface_height_at(0, 4);
        let center_y = world.surface_height_at(8, 0);

        stamp_road(&mut world, &road);
        assert_eq!(
            world.voxel_at(8, old_flank_y, 3),
            Voxel::from(BlockType::Limestone)
        );
        assert_eq!(
            world.voxel_at(0, old_edge_y + 4, 4),
            Voxel::from(BlockType::GlowSand),
            "wide editable roads should stamp lightweight lamp furniture"
        );

        let narrow_dirt = road.with_width(1).retextured(RoadStyle::Dirt);
        restamp_road_component(&mut world, &road, &narrow_dirt);

        assert_eq!(
            world.voxel_at(8, old_flank_y, 3),
            terrain_surface_restore_voxel(&world, 8, 3)
        );
        assert_eq!(
            world.voxel_at(0, old_edge_y + 4, 4),
            AIR,
            "editing road width should clear old lamp furniture outside the new component"
        );
        assert_eq!(world.voxel_at(8, center_y, 0), Voxel::from(BlockType::Dirt));
    }

    #[test]
    fn wide_road_component_stamps_curbs_sidewalks_and_lit_furniture() {
        let mut world = VoxelWorld::new();
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(48, 72, 0),
            9,
            RoadStyle::Asphalt,
        );
        let curb_y = world.surface_height_at(12, 4);
        let sidewalk_y = world.surface_height_at(12, 3);
        let lane_y = world.surface_height_at(12, 2);
        let lamp_y = world.surface_height_at(0, 4);

        stamp_road(&mut world, &road);

        assert_eq!(
            world.voxel_at(12, curb_y, 4),
            Voxel::from(BlockType::ShipHullDark),
            "wide road edge should read as a curb, not plain asphalt"
        );
        assert_eq!(
            world.voxel_at(12, sidewalk_y, 3),
            Voxel::from(BlockType::Limestone),
            "wide roads need walkable sidewalk shoulders for city-scale building"
        );
        assert_eq!(
            world.voxel_at(12, lane_y, 2),
            Voxel::from(BlockType::Stone),
            "drivable lane interior should keep the road surface material"
        );
        assert_eq!(
            world.voxel_at(0, lamp_y + 4, 4),
            Voxel::from(BlockType::GlowSand),
            "manual road components should carry lightweight city lamp furniture"
        );
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
    fn short_road_ramps_clamp_control_heights_to_the_sampled_grade_limit() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(4, 72, 0),
            5,
            RoadStyle::Asphalt,
        )
        .with_endpoint_heights(0, ROAD_MAX_ELEVATION);
        let path = road_path_xz(&road);
        let last_index = path.len().saturating_sub(1);
        let heights: Vec<i32> = (0..=last_index)
            .map(|idx| road_elevation_at_sample(&road, idx, last_index))
            .collect();

        assert_eq!(
            road.elevation_b as i32,
            road_grade_delta_limit(last_index),
            "the stored edit handle should report the reachable ramp height"
        );
        assert_eq!(heights.first().copied(), Some(0));
        assert_eq!(heights.last().copied(), Some(road.elevation_b as i32));
        assert!(heights.windows(2).all(|pair| {
            (pair[1] - pair[0]).abs() <= ROAD_MAX_SAMPLED_RISE_PER_INTERVAL as i32
        }));

        let corner = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(3, 72, 3),
            5,
            RoadStyle::Cobble,
        )
        .with_turn_height(ROAD_MAX_ELEVATION);
        let corner_path = road_path_xz(&corner);
        let corner_last = corner_path.len().saturating_sub(1);
        let corner_heights: Vec<i32> = (0..=corner_last)
            .map(|idx| road_elevation_at_sample(&corner, idx, corner_last))
            .collect();
        assert!(corner_heights.windows(2).all(|pair| {
            (pair[1] - pair[0]).abs() <= ROAD_MAX_SAMPLED_RISE_PER_INTERVAL as i32
        }));
    }

    #[test]
    fn raised_road_component_stamps_absolute_smooth_bridge_deck() {
        let mut world = VoxelWorld::new();
        let start = IVec3::new(0, world.surface_height_at(0, 0), 0);
        let end = IVec3::new(96, world.surface_height_at(96, 0), 0);
        let road = RoadSegment::new(start, end, 5, RoadStyle::Cobble).with_endpoint_heights(0, 24);
        let cells = road_path_xz(&road);
        let last_index = cells.len().saturating_sub(1);
        let start_deck = world.surface_height_at(road.a.x, road.a.z) + road.elevation_a as i32;
        let end_deck = world.surface_height_at(road.b.x, road.b.z) + road.elevation_b as i32;

        let sample = (1..last_index)
            .find_map(|idx| {
                let t = (idx as f32 / last_index as f32).clamp(0.0, 1.0);
                let eased = t * t * (3.0 - 2.0 * t);
                let expected =
                    (start_deck as f32 + (end_deck - start_deck) as f32 * eased).round() as i32;
                let c = cells[idx];
                let terrain_relative = world.surface_height_at(c.x, c.y)
                    + road_elevation_at_sample(&road, idx, last_index);
                (expected != terrain_relative).then_some((idx, c, expected, terrain_relative))
            })
            .expect("terrain fixture should expose rough ground under a raised road");

        stamp_road(&mut world, &road);

        let (_idx, c, expected, terrain_relative) = sample;
        assert_eq!(
            world.voxel_at(c.x, expected, c.y),
            Voxel::from(BlockType::MossStone),
            "raised road decks should stamp on the smooth component grade, not on per-cell terrain"
        );
        assert_ne!(
            world.voxel_at(c.x, terrain_relative, c.y),
            Voxel::from(BlockType::MossStone),
            "old terrain-relative grade left a stair-stepped deck at y={terrain_relative}"
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
    fn smart_road_drag_axis_locks_clear_straight_intent() {
        let target = smart_road_drag_target(IVec3::new(0, 72, 0), IVec3::new(30, 72, 18), &[]);

        assert_eq!(
            target,
            IVec3::new(30, 72, 0),
            "a clearly dominant drag direction should become a straight road even when the mouse is not perfectly aligned"
        );
    }

    #[test]
    fn same_point_city_area_uses_radius_square() {
        let (min, max) = city_area_corners(IVec3::new(10, 72, 20), IVec3::new(10, 72, 20), 12);

        assert_eq!(min, IVec3::new(-2, 72, 8));
        assert_eq!(max, IVec3::new(22, 72, 32));
    }

    #[test]
    fn city_area_drag_release_commits_only_after_real_drag() {
        let start = IVec3::new(10, 72, 20);
        let end = IVec3::new(42, 72, 52);

        assert_eq!(
            city_area_drag_release_corners(start, end, 8),
            Some((IVec3::new(10, 72, 20), IVec3::new(42, 72, 52))),
            "drag-release should place the exact bot city footprint the player drew"
        );
        assert_eq!(
            city_area_drag_release_corners(start, start, 8),
            None,
            "a click without drag should remain available for the deliberate radius-square workflow"
        );
    }

    #[test]
    fn building_shell_drag_release_commits_only_after_real_drag() {
        let start = IVec3::new(4, 72, 8);
        let end = IVec3::new(18, 72, 26);

        assert_eq!(
            building_shell_drag_release_corners(start, end),
            Some((IVec3::new(4, 72, 8), IVec3::new(18, 72, 26))),
            "drag-release should draw the exact shell footprint the player aimed"
        );
        assert_eq!(
            building_shell_drag_release_corners(start, start),
            None,
            "a click without drag should stay available for deliberate tiny-shell placement"
        );
    }

    #[test]
    fn building_shell_stamps_livable_doors_and_windows() {
        let mut world = VoxelWorld::new();
        let bld = Building {
            min: IVec3::new(0, 72, 0),
            max: IVec3::new(10, 72, 8),
            floors: 4,
            style: BuildingStyle::Commercial,
        };
        let mut ground = i32::MIN;
        for x in bld.min.x..=bld.max.x {
            for z in bld.min.z..=bld.max.z {
                ground = ground.max(world.surface_height_at(x, z));
            }
        }
        let base_y = ground + 1;

        stamp_building(&mut world, &bld);

        assert_eq!(
            world.voxel_at(5, base_y + 1, 0),
            AIR,
            "front door should be open at player height"
        );
        assert_eq!(
            world.voxel_at(5, base_y + 2, 0),
            AIR,
            "front door should not be a one-block slit"
        );
        assert_eq!(
            world.voxel_at(0, base_y + 5, 2),
            AIR,
            "upper floors should get repeated window openings"
        );
        assert_eq!(
            world.voxel_at(0, base_y + 5, 1),
            Voxel::from(BuildingStyle::Commercial.wall()),
            "window carving should leave normal wall cells between openings"
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
    fn road_drag_release_commits_when_cursor_reaches_a_different_endpoint() {
        let start = IVec3::new(0, 72, 0);
        let raw = IVec3::new(24, 72, 3);

        assert_eq!(
            road_drag_release_target(start, raw, &[]),
            Some(IVec3::new(24, 72, 0)),
            "drag-release should behave like drawing the road, with axis snap applied"
        );
    }

    #[test]
    fn road_drag_release_keeps_same_point_available_for_roundabouts() {
        let start = IVec3::new(16, 72, 16);

        assert_eq!(
            road_drag_release_target(start, start, &[]),
            None,
            "same-point clicks should still allow the deliberate two-click roundabout workflow"
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
    fn road_branch_from_smoothed_corner_inherits_turn_profile_and_style() {
        let arterial = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(48, 72, 16),
            11,
            RoadStyle::Neon,
        )
        .with_turn_height(12);
        let path = road_path_xz(&arterial);
        let turn_index = road_corner_turn_index(arterial, &path);
        let turn = path[turn_index];
        let start = IVec3::new(turn.x, 72, turn.y);

        let branch = road_segment_from_drag(
            start,
            start + IVec3::new(0, 0, 20),
            3,
            RoadStyle::Dirt,
            &[arterial],
        );

        assert_eq!(branch.width, arterial.width);
        assert_eq!(branch.style, arterial.style);
        assert_eq!(branch.elevation_a, arterial.elevation_via);
        assert_eq!(branch.elevation_b, arterial.elevation_via);
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
    fn plain_wheel_near_road_handle_edits_bridge_height_without_modifier_keys() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        let (edited, kind) = road_plain_wheel_component_edit(road, IVec3::new(31, 72, 1), 2);

        assert_eq!(kind, RoadComponentEditKind::Height);
        assert_eq!(edited.width, 5);
        assert_eq!(edited.elevation_a, 0);
        assert_eq!(
            edited.elevation_b, 4,
            "aiming at the endpoint handle should raise a smooth bridge endpoint without Ctrl/Shift/Alt"
        );
    }

    #[test]
    fn plain_wheel_on_road_body_edits_width_without_modifier_keys() {
        let road = RoadSegment::new(
            IVec3::new(0, 72, 0),
            IVec3::new(32, 72, 0),
            5,
            RoadStyle::Asphalt,
        );

        let (edited, kind) = road_plain_wheel_component_edit(road, IVec3::new(14, 72, 0), 1);

        assert_eq!(kind, RoadComponentEditKind::Size);
        assert_eq!(
            edited.width, 7,
            "wheel on the road body should resize the component directly instead of needing a panel knob"
        );
        assert_eq!(edited.elevation_a, 0);
        assert_eq!(edited.elevation_b, 0);
    }

    #[test]
    fn roundabout_plain_wheel_handle_lifts_whole_component_as_bridge_plateau() {
        let roundabout = RoadSegment::roundabout(IVec3::new(16, 72, 16), 10, 5, RoadStyle::Neon);

        let (edited, kind) = road_plain_wheel_component_edit(roundabout, IVec3::new(26, 72, 16), 3);

        assert_eq!(kind, RoadComponentEditKind::Height);
        assert_eq!(edited.shape, RoadShape::Roundabout);
        assert_eq!(edited.roundabout_radius, 10);
        assert_eq!(edited.elevation_a, 6);
        assert_eq!(
            edited.elevation_b, 6,
            "roundabout height edits should lift the full editable component, not twist the ring"
        );
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
        assert!(path.windows(2).all(|pair| {
            let delta = pair[1] - pair[0];
            delta.x.abs() + delta.y.abs() == 1
        }));
        assert_eq!(
            snap_cell(IVec3::new(21, 72, 3), SnapMode::Road, &[road]),
            IVec3::new(21, 72, 3),
            "road snap should follow the smoothed transition path"
        );
    }

    #[test]
    fn road_shape_paths_are_canonical_and_direction_stable() {
        let straight = RoadSegment::new(
            IVec3::new(-12, 72, 4),
            IVec3::new(28, 72, 4),
            5,
            RoadStyle::Asphalt,
        );
        let straight_reversed =
            RoadSegment::new(straight.b, straight.a, straight.width, straight.style);
        let corner = RoadSegment::new(
            IVec3::new(-12, 72, 4),
            IVec3::new(28, 72, 26),
            5,
            RoadStyle::Cobble,
        );
        let corner_reversed = RoadSegment::new(corner.b, corner.a, corner.width, corner.style);

        assert_eq!(straight.shape, RoadShape::Straight);
        assert_eq!(corner.shape, RoadShape::Corner);
        assert_eq!(corner.via, corner_reversed.via);

        let mut reversed_straight_path = road_path_xz(&straight_reversed);
        reversed_straight_path.reverse();
        assert_eq!(road_path_xz(&straight), reversed_straight_path);

        let mut reversed_corner_path = road_path_xz(&corner_reversed);
        reversed_corner_path.reverse();
        assert_eq!(
            road_path_xz(&corner),
            reversed_corner_path,
            "reversing a drag must not select the other L-shaped corner"
        );
    }

    #[test]
    fn roundabout_path_is_integer_deterministic_contiguous_and_closed() {
        let road = RoadSegment::roundabout(
            IVec3::new(37, 72, -19),
            ROAD_MAX_ROUNDABOUT_RADIUS,
            7,
            RoadStyle::Neon,
        );
        let first = road_path_xz(&road);
        let second = road_path_xz(&road);

        assert_eq!(first, second);
        assert_eq!(first.first(), first.last());
        assert!(first.len() <= ROAD_MAX_CENTERLINE_SAMPLES);
        assert!(first.windows(2).all(|pair| {
            let delta = pair[1] - pair[0];
            delta.x.abs() + delta.y.abs() == 1
        }));
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
    fn deleting_branch_restores_same_style_road_at_intersection() {
        let mut world = VoxelWorld::new();
        let arterial = RoadSegment::new(
            IVec3::new(-12, 72, 0),
            IVec3::new(12, 72, 0),
            5,
            RoadStyle::Asphalt,
        );
        let branch = RoadSegment::new(
            IVec3::new(0, 72, -12),
            IVec3::new(0, 72, 12),
            5,
            RoadStyle::Asphalt,
        );
        let mut roads = vec![arterial, branch];
        stamp_road(&mut world, &arterial);
        stamp_road(&mut world, &branch);

        let center_y = world.surface_height_at(0, 0);
        let branch_only_y = world.surface_height_at(0, 8);
        delete_road_component(&mut world, &mut roads, 1).unwrap();

        assert_eq!(roads, vec![arterial]);
        assert_eq!(
            world.voxel_at(0, center_y, 0),
            road_surface_plan(&world, &arterial)[&IVec3::new(0, center_y, 0)],
            "deleting a branch must reconstruct the arterial under the shared cells"
        );
        assert_eq!(
            world.voxel_at(0, branch_only_y, 8),
            terrain_surface_restore_voxel(&world, 0, 8)
        );
    }

    #[test]
    fn component_style_edit_preserves_later_branch_material_at_intersection() {
        let mut world = VoxelWorld::new();
        let arterial = RoadSegment::new(
            IVec3::new(-12, 72, 0),
            IVec3::new(12, 72, 0),
            5,
            RoadStyle::Asphalt,
        );
        let branch = RoadSegment::new(
            IVec3::new(0, 72, -12),
            IVec3::new(0, 72, 12),
            5,
            RoadStyle::Asphalt,
        );
        let roads = vec![arterial, branch];
        stamp_road(&mut world, &arterial);
        stamp_road(&mut world, &branch);

        let cobble = arterial.retextured(RoadStyle::Cobble);
        restamp_road_component_in_network(&mut world, &roads, 0, &arterial, &cobble);
        let center_y = world.surface_height_at(0, 0);
        let arterial_only_y = world.surface_height_at(8, 0);

        assert_eq!(
            world.voxel_at(0, center_y, 0),
            road_surface_plan(&world, &branch)[&IVec3::new(0, center_y, 0)],
            "the later branch remains the material authority at its intersection"
        );
        assert_eq!(
            world.voxel_at(8, arterial_only_y, 0),
            road_surface_plan(&world, &cobble)[&IVec3::new(8, arterial_only_y, 0)]
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
