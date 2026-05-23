//! STADT — City-Builder layer on top of the voxel world.
//!
//! Slim Cut 1 of the plan-v3 city system:
//!
//! * **CA Road-Grid-Tool** — choose Road in the STADT tab or Toolbelt,
//!   click once to set corner A, click again to commit a straight road
//!   that follows the terrain surface. Width with `[` / `]` (1..=9).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoadStyle {
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

/// Placed straight road segment. Kept in-memory for gizmo drawing and
/// road-snap queries; serialization lands in a later cut.
#[derive(Debug, Clone, Copy)]
pub struct RoadSegment {
    pub a: IVec3,
    pub b: IVec3,
    pub width: u8,
    pub style: RoadStyle,
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
            .add_systems(Update, (city_input, city_draw_gizmos, draw_hint_hud));
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

// ---------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn city_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    editor: Res<EditorState>,
    mode: Res<crate::mode::ModeContext>,
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
        return;
    }
    if live_city_active {
        let cursor_locked = windows
            .get_single()
            .map(|w| w.cursor.grab_mode == bevy::window::CursorGrabMode::Locked)
            .unwrap_or(false);
        if !cursor_locked {
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
    let snapped = snap_cell(ground, city.snap, &city.roads);

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
                    let seg = RoadSegment {
                        a,
                        b: snapped,
                        width: city.road_width,
                        style: city.road_style,
                    };
                    let n = stamp_road(&mut world, &seg);
                    city.roads.push(seg);
                    city.pending_road_a = None;
                    city.status = format!("Strasse {} ({} Bloecke)", city.road_style.label(), n);
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
                } else if city.roads.pop().is_some() {
                    city.status = "Letzte Strasse entfernt (nur Liste — Voxel bleiben).".into();
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
            let px = p.x as f32 + 0.5;
            let pz = p.z as f32 + 0.5;
            let mut best: Option<(f32, (f32, f32))> = None;
            for r in roads {
                let ax = r.a.x as f32 + 0.5;
                let az = r.a.z as f32 + 0.5;
                let bx = r.b.x as f32 + 0.5;
                let bz = r.b.z as f32 + 0.5;
                let dx = bx - ax;
                let dz = bz - az;
                let len2 = dx * dx + dz * dz;
                if len2 < 1e-3 {
                    continue;
                }
                let t = (((px - ax) * dx + (pz - az) * dz) / len2).clamp(0.0, 1.0);
                let qx = ax + t * dx;
                let qz = az + t * dz;
                let d2 = (qx - px).powi(2) + (qz - pz).powi(2);
                if d2 < 64.0 && best.map_or(true, |(bd, _)| d2 < bd) {
                    best = Some((d2, (qx, qz)));
                }
            }
            match best {
                Some((_, (qx, qz))) => IVec3::new(qx.floor() as i32, p.y, qz.floor() as i32),
                None => p,
            }
        }
    }
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

/// Stamp a straight road along `seg` onto the terrain surface. Returns
/// the number of voxels actually changed (so the UI can show a count).
fn stamp_road(world: &mut VoxelWorld, seg: &RoadSegment) -> usize {
    let cells = line_xz(IVec2::new(seg.a.x, seg.a.z), IVec2::new(seg.b.x, seg.b.z));
    if cells.is_empty() {
        return 0;
    }
    // Width axis: perpendicular in XZ to the primary direction.
    let dx = seg.b.x - seg.a.x;
    let dz = seg.b.z - seg.a.z;
    let (perp_x, perp_z) = if dx.abs() >= dz.abs() { (0, 1) } else { (1, 0) };
    let half = (seg.width as i32) / 2;

    let surface: Voxel = seg.style.surface_block().into();
    let stripe: Option<Voxel> = seg.style.stripe_block().map(|b| b.into());

    let mut changed = 0usize;
    for (i, c) in cells.iter().enumerate() {
        for w in -half..=half {
            let wx = c.x + perp_x * w;
            let wz = c.y + perp_z * w;
            let sy = world.surface_height_at(wx, wz);
            // Carve up to 3 blocks of air above so we don't bury the
            // road under trees / hills that just caught the edge.
            for dy in 1..=3 {
                if world.is_solid(wx, sy + dy, wz) && world.edit_set_voxel(wx, sy + dy, wz, AIR) {
                    changed += 1;
                }
            }
            if world.edit_set_voxel(wx, sy, wz, surface) {
                changed += 1;
            }
        }
        // Centre stripe every 3 cells along the length axis.
        if let Some(s) = stripe {
            if i % 3 == 0 {
                let sy = world.surface_height_at(c.x, c.y);
                if world.edit_set_voxel(c.x, sy, c.y, s) {
                    changed += 1;
                }
            }
        }
    }
    changed
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
    for r in &city.roads {
        let col = r.style.gizmo_color();
        let a = r.a.as_vec3() + Vec3::new(0.5, 1.2, 0.5);
        let b = r.b.as_vec3() + Vec3::new(0.5, 1.2, 0.5);
        gizmos.line(a, b, col);
        // Width flanks (two parallel lines). Draw them 1 block higher
        // so they sit clearly above the road surface.
        let dx = (b.x - a.x).abs();
        let dz = (b.z - a.z).abs();
        let (px, pz) = if dx >= dz {
            (0.0, (r.width as f32) * 0.5)
        } else {
            ((r.width as f32) * 0.5, 0.0)
        };
        let faint = col.with_alpha(0.55);
        gizmos.line(
            a + Vec3::new(px, 0.0, pz),
            b + Vec3::new(px, 0.0, pz),
            faint,
        );
        gizmos.line(
            a - Vec3::new(px, 0.0, pz),
            b - Vec3::new(px, 0.0, pz),
            faint,
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
            let cursor = snap_cell(
                IVec3::new(hit_cell.x, sy, hit_cell.z),
                city.snap,
                &city.roads,
            );
            let c_world = cursor.as_vec3() + Vec3::new(0.5, 1.5, 0.5);
            // Cursor marker — pulses so the user never loses it.
            gizmos.sphere(
                c_world,
                Quat::IDENTITY,
                0.8 + pulse * 0.3,
                city.road_style.gizmo_color(),
            );
            if let Some(a) = city.pending_road_a {
                let a_world = a.as_vec3() + Vec3::new(0.5, 1.5, 0.5);
                gizmos.line(a_world, c_world, city.road_style.gizmo_color());
                gizmos.cuboid(
                    Transform::from_translation(a_world).with_scale(Vec3::splat(1.1)),
                    city.road_style.gizmo_color(),
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

/// Draw a rectangular building footprint outline at ground height + a
/// vertical beacon for easy spotting. Used for previews (pending A →
/// cursor) and for committed buildings.
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
                    lines.push(("LMB".into(), "Strassenende setzen".into()));
                    lines.push(("RMB / Esc".into(), "Abbrechen".into()));
                } else {
                    lines.push(("LMB".into(), "Strassenstart setzen".into()));
                    lines.push(("RMB".into(), "Letzte Strasse loeschen".into()));
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
