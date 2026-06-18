//! Compact in-game toolbelt for mouse-look building.
//!
//! F3 opens the fast build/edit layer: pick a tool from icon chips, then
//! keep moving/flying while LMB/RMB works directly in the world. Weapons
//! are holstered for the whole edit state, including the tool picker.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::animation::AnimationStudio;
use crate::blocks::{block_label, block_palette_catalog, BlockPaletteEntry, BlockType};
use crate::builder::{BuilderHistory, BuilderState};
use crate::city::{CityState, CityTool};
use crate::icons::{paint_icon, Icon};
use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::settings::WorldSettings;
use crate::theme::{AMBER, TEXT};
use crate::world::VoxelWorld;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbeltTool {
    Navigate,
    /// Draw a rectangular block area directly in the world: LMB-drag a
    /// square/rectangle on the hovered face, release to fill, Esc to cancel.
    DrawRect,
    /// SketchUp-style direct-manipulation sculpting. Hover a flat face
    /// to highlight it, drag to push/pull. See [`crate::sculpt`].
    Sculpt,
    /// Intent-first high-rise generator: two corners become a detailed tower.
    SmartTower,
    BrushPlace,
    BrushCut,
    CityRoad,
    CityDistrict,
    CityBuilding,
    CityFacade,
    AnimationPick,
}

impl ToolbeltTool {
    pub const ALL: [ToolbeltTool; 11] = [
        ToolbeltTool::Navigate,
        ToolbeltTool::DrawRect,
        ToolbeltTool::Sculpt,
        ToolbeltTool::SmartTower,
        ToolbeltTool::BrushPlace,
        ToolbeltTool::BrushCut,
        ToolbeltTool::CityRoad,
        ToolbeltTool::CityDistrict,
        ToolbeltTool::CityBuilding,
        ToolbeltTool::CityFacade,
        ToolbeltTool::AnimationPick,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "Navigate / Inspect",
            ToolbeltTool::DrawRect => "Sketch Draw",
            ToolbeltTool::Sculpt => "Push Pull Face",
            ToolbeltTool::SmartTower => "Smart Tower",
            ToolbeltTool::BrushPlace => "Smart Builder",
            ToolbeltTool::BrushCut => "Smart Cut",
            ToolbeltTool::CityRoad => "Road Tool",
            ToolbeltTool::CityDistrict => "Bot City Area",
            ToolbeltTool::CityBuilding => "Building Shell",
            ToolbeltTool::CityFacade => "Facade Stamp",
            ToolbeltTool::AnimationPick => "Animation Picker",
        }
    }

    pub fn chip_label(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "NAV",
            ToolbeltTool::DrawRect => "SKETCH",
            ToolbeltTool::Sculpt => "PUSH",
            ToolbeltTool::SmartTower => "TOWER",
            ToolbeltTool::BrushPlace => "BUILD",
            ToolbeltTool::BrushCut => "CUT",
            ToolbeltTool::CityRoad => "ROAD",
            ToolbeltTool::CityDistrict => "AREA",
            ToolbeltTool::CityBuilding => "SHELL",
            ToolbeltTool::CityFacade => "STAMP",
            ToolbeltTool::AnimationPick => "ANIM",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            ToolbeltTool::Navigate => Icon::ModeNavigate,
            ToolbeltTool::DrawRect => Icon::Grid,
            ToolbeltTool::Sculpt => Icon::Builder,
            ToolbeltTool::SmartTower => Icon::City,
            ToolbeltTool::BrushPlace => Icon::Brush,
            ToolbeltTool::BrushCut => Icon::Eraser,
            ToolbeltTool::CityRoad => Icon::Road,
            ToolbeltTool::CityDistrict => Icon::District,
            ToolbeltTool::CityBuilding => Icon::City,
            ToolbeltTool::CityFacade => Icon::Open,
            ToolbeltTool::AnimationPick => Icon::Animation,
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "Move, inspect, and keep weapons off while Build Studio is open.",
            ToolbeltTool::DrawRect => "SketchUp-style draw-first tool: LMB draws exact snapped faces, hold RMB orbits the camera, Ctrl+LMB cuts openings, Shift+LMB hollows room depth. Ctrl+Z/Ctrl+Y undo/redo.",
            ToolbeltTool::Sculpt => "LMB Push/Pulls faces. Alt+LMB temporarily fills rectangles. G swaps Fill/Push.",
            ToolbeltTool::SmartTower => "Two LMB clicks create a detailed skyscraper shell with floors, windows, crown, and undo.",
            ToolbeltTool::BrushPlace => "LMB starts a block point, drag to an endpoint, release to build; RMB uses the same gesture to cut.",
            ToolbeltTool::BrushCut => "LMB or RMB starts a cut point, drag to an endpoint, release to remove exact snapped blocks.",
            ToolbeltTool::CityRoad => "LMB draws roads: auto-snaps to endpoints/branches, continues from the last point, and inherits width, texture, and bridge height. Wheel edits selected roads: body width/radius, handle bridge height. Middle mouse retextures the selected component.",
            ToolbeltTool::CityDistrict => "Two LMB clicks mark the exact bot city footprint. Bots stay parked until an area or explicit task is placed, then plan roads and buildings inside that space.",
            ToolbeltTool::CityBuilding => "LMB sets two corners for a solid building shell.",
            ToolbeltTool::CityFacade => "LMB stamps the active facade onto the targeted wall.",
            ToolbeltTool::AnimationPick => "LMB/RMB pick voxels for animation authoring.",
        }
    }

    fn action_hints(self, picker_open: bool) -> [Option<ToolActionHint>; 4] {
        match self {
            ToolbeltTool::Navigate => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Inspect",
                    Icon::Eye,
                    ActionTone::Dim,
                    "Inspect the world without editing.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "HOLD",
                    "Orbit",
                    Icon::ModeNavigate,
                    ActionTone::Primary,
                    "Hold right mouse to move the camera.",
                )),
                None,
                None,
            ],
            ToolbeltTool::DrawRect => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Draw",
                    Icon::Grid,
                    ActionTone::Tool,
                    "Drag from a snapped voxel endpoint to draw a face.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "HOLD",
                    "Orbit",
                    Icon::ModeNavigate,
                    ActionTone::Info,
                    "Hold right mouse to orbit without leaving Sketch Draw.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "CTRL",
                    "Cut",
                    Icon::Eraser,
                    ActionTone::Danger,
                    "Hold Ctrl and drag left mouse to cut an opening.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "SHIFT",
                    "Room",
                    Icon::Cube,
                    ActionTone::Warning,
                    "Hold Shift and drag left mouse to hollow a livable room depth.",
                )),
            ],
            ToolbeltTool::Sculpt => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Push",
                    Icon::Move,
                    ActionTone::Tool,
                    "Drag a face to push or pull it.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Ref",
                    Icon::Snap,
                    ActionTone::Info,
                    "Set Push/Pull reference points.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "ALT",
                    "Fill",
                    Icon::Grid,
                    ActionTone::Warning,
                    "Hold Alt and drag left mouse for temporary rectangle fill.",
                )),
                None,
            ],
            ToolbeltTool::SmartTower => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "2X",
                    "Tower",
                    Icon::City,
                    ActionTone::Tool,
                    "Pick two corners to build a detailed tower shell.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Cancel",
                    Icon::Close,
                    ActionTone::Warning,
                    "Cancel the tower preview.",
                )),
                None,
                None,
            ],
            ToolbeltTool::BrushPlace => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Build",
                    Icon::Brush,
                    ActionTone::Tool,
                    "Drag from a point to an endpoint to build exact blocks.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Cut",
                    Icon::Eraser,
                    ActionTone::Danger,
                    "Right mouse uses the same endpoint gesture to cut.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Wheel,
                    "",
                    "Size",
                    Icon::Scale,
                    ActionTone::Info,
                    "Mouse wheel resizes the live brush.",
                )),
                None,
            ],
            ToolbeltTool::BrushCut => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Cut",
                    Icon::Eraser,
                    ActionTone::Danger,
                    "Drag from a point to an endpoint to cut exact blocks.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Cut",
                    Icon::Eraser,
                    ActionTone::Danger,
                    "Right mouse also starts a cut gesture.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Wheel,
                    "",
                    "Size",
                    Icon::Scale,
                    ActionTone::Info,
                    "Mouse wheel resizes the live brush.",
                )),
                None,
            ],
            ToolbeltTool::CityRoad => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Road",
                    Icon::Road,
                    ActionTone::Tool,
                    "Draw road components with endpoint and branch snapping.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Delete",
                    Icon::Delete,
                    ActionTone::Danger,
                    "Delete the selected road component or cancel the current road.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Wheel,
                    "",
                    "Shape",
                    Icon::Scale,
                    if picker_open {
                        ActionTone::Primary
                    } else {
                        ActionTone::Info
                    },
                    "Wheel edits selected road width, roundabout radius, or bridge height.",
                )),
                None,
            ],
            ToolbeltTool::CityDistrict => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "2X",
                    "Area",
                    Icon::District,
                    ActionTone::Tool,
                    "Mark two corners for the bot city area.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Cancel",
                    Icon::Close,
                    ActionTone::Warning,
                    "Cancel or remove the last city area.",
                )),
                None,
                None,
            ],
            ToolbeltTool::CityBuilding => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "2X",
                    "Shell",
                    Icon::City,
                    ActionTone::Tool,
                    "Choose two corners for a building shell.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Cancel",
                    Icon::Close,
                    ActionTone::Warning,
                    "Remove or cancel the current building shell.",
                )),
                None,
                None,
            ],
            ToolbeltTool::CityFacade => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Stamp",
                    Icon::Open,
                    ActionTone::Tool,
                    "Stamp the active facade onto the targeted wall.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Undo",
                    Icon::Undo,
                    ActionTone::Warning,
                    "Remove the last facade stamp.",
                )),
                None,
                None,
            ],
            ToolbeltTool::AnimationPick => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Add",
                    Icon::Animation,
                    ActionTone::Tool,
                    "Add a voxel to the animation selection.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "",
                    "Remove",
                    Icon::Delete,
                    ActionTone::Danger,
                    "Remove a voxel from the animation selection.",
                )),
                None,
                None,
            ],
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "NAV",
            ToolbeltTool::DrawRect | ToolbeltTool::Sculpt => "SHAPE",
            ToolbeltTool::SmartTower => "SMART",
            ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut => "SMART",
            ToolbeltTool::CityRoad
            | ToolbeltTool::CityDistrict
            | ToolbeltTool::CityBuilding
            | ToolbeltTool::CityFacade => "CITY",
            ToolbeltTool::AnimationPick => "ANIM",
        }
    }

    fn category_color(self) -> egui::Color32 {
        match self {
            ToolbeltTool::Navigate => egui::Color32::from_rgb(180, 210, 190),
            ToolbeltTool::DrawRect | ToolbeltTool::Sculpt => egui::Color32::from_rgb(80, 170, 255),
            ToolbeltTool::SmartTower => egui::Color32::from_rgb(130, 255, 125),
            ToolbeltTool::BrushPlace | ToolbeltTool::BrushCut => {
                egui::Color32::from_rgb(255, 184, 70)
            }
            ToolbeltTool::CityRoad
            | ToolbeltTool::CityDistrict
            | ToolbeltTool::CityBuilding
            | ToolbeltTool::CityFacade => egui::Color32::from_rgb(80, 235, 225),
            ToolbeltTool::AnimationPick => egui::Color32::from_rgb(255, 105, 255),
        }
    }

    pub fn city_tool(self) -> Option<CityTool> {
        match self {
            ToolbeltTool::CityRoad => Some(CityTool::Road),
            ToolbeltTool::CityDistrict => Some(CityTool::District),
            ToolbeltTool::CityBuilding => Some(CityTool::Building),
            ToolbeltTool::CityFacade => Some(CityTool::Facade),
            _ => None,
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    pub fn quick_slot(slot: u8) -> Option<Self> {
        Some(match slot {
            1 => ToolbeltTool::DrawRect,
            2 => ToolbeltTool::Sculpt,
            3 => ToolbeltTool::SmartTower,
            4 => ToolbeltTool::BrushPlace,
            5 => ToolbeltTool::BrushCut,
            6 => ToolbeltTool::CityRoad,
            7 => ToolbeltTool::CityDistrict,
            8 => ToolbeltTool::CityBuilding,
            9 => ToolbeltTool::CityFacade,
            0 => ToolbeltTool::AnimationPick,
            _ => return None,
        })
    }

    pub fn quick_slot_label(self) -> &'static str {
        match self {
            ToolbeltTool::DrawRect => "1",
            ToolbeltTool::Sculpt => "2",
            ToolbeltTool::SmartTower => "3",
            ToolbeltTool::BrushPlace => "4",
            ToolbeltTool::BrushCut => "5",
            ToolbeltTool::CityRoad => "6",
            ToolbeltTool::CityDistrict => "7",
            ToolbeltTool::CityBuilding => "8",
            ToolbeltTool::CityFacade => "9",
            ToolbeltTool::AnimationPick => "0",
            ToolbeltTool::Navigate => "-",
        }
    }

    pub fn stepped(self, delta: isize) -> Self {
        let len = Self::ALL.len() as isize;
        let next = (self.index() as isize + delta).rem_euclid(len) as usize;
        Self::ALL[next]
    }
}

#[derive(Resource, Debug, Clone)]
pub struct ToolbeltState {
    pub live: bool,
    pub palette_open: bool,
    pub tool: ToolbeltTool,
    pub status: String,
}

impl Default for ToolbeltState {
    fn default() -> Self {
        Self {
            live: false,
            palette_open: false,
            tool: ToolbeltTool::DrawRect,
            status:
                "Creative Sketch Builder: LMB draws, hold RMB orbits, Ctrl+LMB cuts, Shift+LMB hollows, Ctrl+Z undo."
                    .into(),
        }
    }
}

impl ToolbeltState {
    #[allow(dead_code)]
    pub fn live_city_tool(&self) -> Option<CityTool> {
        if self.live && !self.palette_open {
            self.tool.city_tool()
        } else {
            None
        }
    }

    pub fn blocks_weapons(&self) -> bool {
        self.palette_open || self.live
    }
}

pub struct ToolbeltPlugin;

impl Plugin for ToolbeltPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ToolbeltState::default())
            .add_systems(Update, draw_toolbelt.run_if(in_state(GameState::InGame)));
    }
}

#[allow(dead_code)]
fn toolbelt_hotkeys(
    keys: Res<ButtonInput<KeyCode>>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut city: ResMut<CityState>,
    mut studio: ResMut<AnimationStudio>,
    mut builder: ResMut<BuilderState>,
) {
    let mut changed = false;

    if keys.just_pressed(KeyCode::F3) {
        if toolbelt.palette_open || toolbelt.live {
            toolbelt.palette_open = false;
            toolbelt.live = false;
            toolbelt.status = "Weapons armed explicitly. Build tools stay one click away.".into();
        } else {
            if toolbelt.tool == ToolbeltTool::Navigate {
                toolbelt.tool = ToolbeltTool::DrawRect;
            }
            toolbelt.live = true;
            toolbelt.palette_open = true;
            toolbelt.status =
                "Build Studio picker: choose a named tool, then build with LMB.".into();
        }
        changed = true;
    }

    if keys.just_pressed(KeyCode::Tab) {
        if !toolbelt.live {
            toolbelt.live = true;
            if toolbelt.tool == ToolbeltTool::Navigate {
                toolbelt.tool = ToolbeltTool::DrawRect;
            }
            changed = true;
        }
        toolbelt.palette_open = !toolbelt.palette_open;
        if toolbelt.palette_open && toolbelt.tool == ToolbeltTool::Navigate {
            toolbelt.tool = ToolbeltTool::DrawRect;
        }
        toolbelt.status = if toolbelt.palette_open {
            "Build Studio picker: click a tool, Q/E cycles tools, Tab closes.".into()
        } else {
            format!(
                "Build Live: {}. {}",
                toolbelt.tool.label(),
                toolbelt.tool.hint()
            )
        };
    }

    if keys.just_pressed(KeyCode::F7) {
        toolbelt.live = true;
        toolbelt.palette_open = false;
        changed = true;
        toolbelt.status = if toolbelt.tool == ToolbeltTool::Navigate {
            toolbelt.tool = ToolbeltTool::DrawRect;
            format!(
                "Build Live: {}. {}",
                toolbelt.tool.label(),
                toolbelt.tool.hint()
            )
        } else {
            format!(
                "Build Live: {}. {}",
                toolbelt.tool.label(),
                toolbelt.tool.hint()
            )
        };
    }

    if toolbelt.palette_open || toolbelt.live {
        if keys.just_pressed(KeyCode::KeyQ) {
            toolbelt.tool = toolbelt.tool.stepped(-1);
            changed = true;
        }
        if keys.just_pressed(KeyCode::KeyE) {
            toolbelt.tool = toolbelt.tool.stepped(1);
            changed = true;
        }
    }

    if keys.just_pressed(KeyCode::Escape) {
        if toolbelt.palette_open {
            toolbelt.palette_open = false;
            toolbelt.status = format!("Picker hidden. Build Live: {}.", toolbelt.tool.label());
        } else if toolbelt.live && toolbelt.tool == ToolbeltTool::DrawRect {
            toolbelt.status =
                "Sketch drag cancelled. LMB starts another snapped face; G swaps Push/Pull.".into();
        } else if toolbelt.live {
            toolbelt.live = false;
            changed = true;
            toolbelt.status = "Weapons armed explicitly. Build tools stay one click away.".into();
        }
    }

    if changed {
        sync_tool_selection(&mut toolbelt, &mut city, &mut studio, &mut builder);
    }
}

fn draw_toolbelt(
    mut contexts: EguiContexts,
    settings: Res<WorldSettings>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut mode: ResMut<ModeContext>,
    mut builder: ResMut<BuilderState>,
    mut history: ResMut<BuilderHistory>,
    mut world: ResMut<VoxelWorld>,
    mut wheel: EventReader<MouseWheel>,
) {
    if !mode.is_build() {
        wheel.clear();
        return;
    }

    let ctx = contexts.ctx_mut();
    let theme = settings.theme;
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let expanded = mode.is_build_picker();
    let live = mode.is_build();
    let mut active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if wheel_delta.abs() >= 0.5 {
        if expanded {
            let step = if wheel_delta > 0.0 { -1 } else { 1 };
            active_tool = normalized_tool_step(active_tool, step);
            toolbelt.tool = active_tool;
            mode.set(
                ActiveMode::BuildPicker { tool: active_tool },
                format!("Build Picker: {}.", active_tool.label()),
            );
            toolbelt.status = mode.status.clone();
        } else if live && active_tool.uses_live_brush() {
            let step = if wheel_delta > 0.0 { 1 } else { -1 };
            builder.brush = step_brush_uniform(builder.brush, step);
            builder.status = format!(
                "Live Brush {}x{}x{}",
                builder.brush.x, builder.brush.y, builder.brush.z
            );
            toolbelt.status = builder.status.clone();
            mode.status = toolbelt.status.clone();
        }
    }
    let status = compact_status(&toolbelt.status, active_tool);
    let brush = builder.brush;

    let dock = draw_build_dock(
        active_tool,
        expanded,
        &status,
        builder.block,
        brush,
        history.undo_len(),
        history.redo_len(),
        theme,
        primary,
        dim,
        ctx,
    );

    if let Some(tool) = dock.clicked_tool {
        toolbelt.tool = tool;
        mode.set(
            ActiveMode::BuildLive { tool },
            format!("Build Live: {}. {}", tool.label(), tool.hint()),
        );
        toolbelt.status = mode.status.clone();
    }
    if let Some(preset) = dock.workflow_preset {
        let tool = preset.tool();
        toolbelt.tool = tool;
        if let Some(brush) = preset.brush() {
            builder.brush = brush;
            builder.status = format!("Live Brush {}x{}x{}", brush.x, brush.y, brush.z);
        }
        if let Some(block) = preset.block() {
            builder.block = block;
            builder.status = format!("Material: {}", block_label(block));
        }
        mode.set(ActiveMode::BuildLive { tool }, preset.status());
        toolbelt.status = mode.status.clone();
    }
    if let Some(block) = dock.block_choice {
        builder.block = block;
        builder.status = format!("Material: {}", block_label(block));
        toolbelt.status = builder.status.clone();
        mode.status = builder.status.clone();
    }
    if dock.toggle_picker {
        let tool = mode.build_tool().unwrap_or(toolbelt.tool);
        if mode.is_build_picker() {
            mode.set(
                ActiveMode::BuildLive { tool },
                format!("Build Live: {}. {}", tool.label(), tool.hint()),
            );
            toolbelt.status = mode.status.clone();
        } else if mode.is_build_live() {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Build Studio picker visible. Pick a tool or press Tab to hide it.",
            );
            toolbelt.status = mode.status.clone();
        } else {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Build Studio picker visible. Pick a tool or press Tab to hide it.",
            );
            toolbelt.status = mode.status.clone();
        }
    }
    if let Some(size) = dock.brush_preset {
        builder.brush = size;
        builder.status = format!("Live Brush {}x{}x{}", size.x, size.y, size.z);
        toolbelt.status = builder.status.clone();
    }
    if let Some(command) = dock.history_command {
        let result = match command {
            HistoryCommand::Undo => history.pop_undo(&mut world),
            HistoryCommand::Redo => history.pop_redo(&mut world),
        };
        let status = format_history_command_status(command, result);
        toolbelt.status = status.clone();
        mode.status = status;
    }
}

fn sync_tool_selection(
    toolbelt: &mut ToolbeltState,
    city: &mut CityState,
    studio: &mut AnimationStudio,
    builder: &mut BuilderState,
) {
    if let Some(city_tool) = toolbelt.tool.city_tool() {
        city.tool = city_tool;
        city.pending_road_a = None;
        city.pending_building_a = None;
    } else {
        city.tool = CityTool::None;
        city.pending_road_a = None;
        city.pending_building_a = None;
    }

    studio.picking = toolbelt.live && toolbelt.tool == ToolbeltTool::AnimationPick;

    if toolbelt.tool == ToolbeltTool::BrushCut && builder.brush == IVec3::ONE {
        builder.brush = IVec3::new(2, 3, 1);
    }

    toolbelt.status = if toolbelt.live {
        if toolbelt.palette_open {
            format!(
                "Build Picker: {}. {}",
                toolbelt.tool.label(),
                toolbelt.tool.hint()
            )
        } else {
            format!(
                "Build Live: {}. {}",
                toolbelt.tool.label(),
                toolbelt.tool.hint()
            )
        }
    } else {
        format!(
            "{} selected. Creative Build stays active.",
            toolbelt.tool.label()
        )
    };
}

impl ToolbeltTool {
    fn uses_live_brush(self) -> bool {
        matches!(self, Self::BrushPlace | Self::BrushCut)
    }
}

fn normalized_tool_step(tool: ToolbeltTool, delta: isize) -> ToolbeltTool {
    let stepped = tool.stepped(delta);
    if stepped == ToolbeltTool::Navigate {
        stepped.stepped(delta.signum())
    } else {
        stepped
    }
}

fn step_brush_uniform(brush: IVec3, delta: i32) -> IVec3 {
    let next = brush + IVec3::splat(delta);
    IVec3::new(
        next.x.clamp(1, 32),
        next.y.clamp(1, 32),
        next.z.clamp(1, 32),
    )
}

fn compact_status(status: &str, tool: ToolbeltTool) -> String {
    if status.len() <= 96 {
        status.to_owned()
    } else if tool == ToolbeltTool::DrawRect {
        format!(
            "{} ready. LMB draw, RMB orbit, Ctrl/Shift+LMB cut.",
            tool.label()
        )
    } else {
        format!(
            "{} ready. LMB endpoint build, RMB cut, Tab tools.",
            tool.label()
        )
    }
}

#[derive(Default)]
struct BuildDockResult {
    clicked_tool: Option<ToolbeltTool>,
    toggle_picker: bool,
    brush_preset: Option<IVec3>,
    workflow_preset: Option<BuildWorkflowPreset>,
    block_choice: Option<BlockType>,
    history_command: Option<HistoryCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryCommand {
    Undo,
    Redo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildWorkflowPreset {
    Sketch,
    Room,
    PushPull,
    ModernHouse,
    Roads,
    Landscape,
    CityShell,
    Skyline,
    Spacecraft,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionTone {
    Tool,
    Primary,
    Info,
    Warning,
    Danger,
    Dim,
}

#[derive(Clone, Copy)]
struct ToolActionHint {
    glyph: MouseGlyph,
    modifier: &'static str,
    label: &'static str,
    icon: Icon,
    tone: ActionTone,
    hint: &'static str,
}

impl ToolActionHint {
    const fn new(
        glyph: MouseGlyph,
        modifier: &'static str,
        label: &'static str,
        icon: Icon,
        tone: ActionTone,
        hint: &'static str,
    ) -> Self {
        Self {
            glyph,
            modifier,
            label,
            icon,
            tone,
            hint,
        }
    }
}

impl BuildWorkflowPreset {
    const ALL: [Self; 9] = [
        Self::Sketch,
        Self::Room,
        Self::PushPull,
        Self::ModernHouse,
        Self::Roads,
        Self::Landscape,
        Self::CityShell,
        Self::Skyline,
        Self::Spacecraft,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Sketch => "SKETCH",
            Self::Room => "ROOM",
            Self::PushPull => "PUSH",
            Self::ModernHouse => "HOUSE",
            Self::Roads => "ROADS",
            Self::Landscape => "GARDEN",
            Self::CityShell => "CITY",
            Self::Skyline => "TOWER",
            Self::Spacecraft => "SHIP",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Sketch => Icon::Grid,
            Self::Room => Icon::Open,
            Self::PushPull => Icon::Move,
            Self::ModernHouse => Icon::Builder,
            Self::Roads => Icon::Road,
            Self::Landscape => Icon::Brush,
            Self::CityShell => Icon::City,
            Self::Skyline => Icon::Wand,
            Self::Spacecraft => Icon::Cube,
        }
    }

    fn tool(self) -> ToolbeltTool {
        match self {
            Self::Sketch => ToolbeltTool::DrawRect,
            Self::Room => ToolbeltTool::DrawRect,
            Self::PushPull => ToolbeltTool::Sculpt,
            Self::ModernHouse => ToolbeltTool::DrawRect,
            Self::Roads => ToolbeltTool::CityRoad,
            Self::Landscape => ToolbeltTool::DrawRect,
            Self::CityShell => ToolbeltTool::CityBuilding,
            Self::Skyline => ToolbeltTool::SmartTower,
            Self::Spacecraft => ToolbeltTool::DrawRect,
        }
    }

    fn brush(self) -> Option<IVec3> {
        match self {
            Self::Sketch => Some(IVec3::new(4, 1, 1)),
            Self::Room => Some(IVec3::new(8, 1, 1)),
            Self::PushPull => Some(IVec3::ONE),
            Self::ModernHouse => Some(IVec3::new(8, 1, 1)),
            Self::Landscape => Some(IVec3::new(8, 1, 8)),
            Self::Spacecraft => Some(IVec3::new(6, 1, 1)),
            Self::Roads | Self::CityShell | Self::Skyline => None,
        }
    }

    fn block(self) -> Option<BlockType> {
        match self {
            Self::Sketch => Some(BlockType::Stone),
            Self::Room => Some(BlockType::Limestone),
            Self::PushPull => Some(BlockType::Limestone),
            Self::ModernHouse => Some(BlockType::Limestone),
            Self::Roads => Some(BlockType::Stone),
            Self::Landscape => Some(BlockType::Grass),
            Self::CityShell => Some(BlockType::Limestone),
            Self::Skyline => Some(BlockType::CockpitGlass),
            Self::Spacecraft => Some(BlockType::ShipHullAlloy),
        }
    }

    fn status(self) -> String {
        match self {
            Self::Sketch => "Sketch workflow: LMB drag a snapped rectangle, RMB orbits, Ctrl+LMB cuts, Shift+LMB hollows; Alt turns it into Push/Pull.".into(),
            Self::Room => "Room workflow: build a solid mass, then Shift+LMB drag a wall face to hollow livable depth; Ctrl+LMB cuts doors and windows.".into(),
            Self::PushPull => "Push workflow: hover a face, LMB drag depth, release to commit; Alt gives temporary Fill.".into(),
            Self::ModernHouse => "Modern house workflow: white wall material, wide wall brush, locked-plane sketching, then Push/Pull details.".into(),
            Self::Roads => "Road and traffic workflow: draw road components with endpoint snap; wheel edits width/bridge height; middle mouse retextures.".into(),
            Self::Landscape => "Garden workflow: large ground brush for lawns, paths, pools, and planted courtyards.".into(),
            Self::CityShell => "City workflow: LMB two corners for a building shell; roads and frontage stay component-aware.".into(),
            Self::Skyline => "Tower workflow: two clicks create a varied skyscraper shell with floors, crown, and undo.".into(),
            Self::Spacecraft => "Spacecraft workflow: alloy material and long hull brush for shuttles, fins, and cockpit follow-up.".into(),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Sketch => "One click switches to rectangle sketching and a flat 4x1 brush.",
            Self::Room => {
                "One click switches to Sketch Draw with hollow-room guidance for interiors."
            }
            Self::PushPull => "One click switches to SketchUp-style face push/pull.",
            Self::ModernHouse => "White plaster, broad wall brush, fast modern-house massing.",
            Self::Roads => {
                "One click switches to road components: draw, branch, adjust, retexture."
            }
            Self::Landscape => {
                "Grass material and ground brush for gardens, lawns, and terrain detail."
            }
            Self::CityShell => {
                "One click switches to component building shells for fast city blocks."
            }
            Self::Skyline => "One click switches to smart tower generation.",
            Self::Spacecraft => {
                "Alloy material and hull brush for shuttle bodies and sci-fi details."
            }
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Sketch => egui::Color32::from_rgb(80, 170, 255),
            Self::Room => egui::Color32::from_rgb(80, 235, 190),
            Self::PushPull => egui::Color32::from_rgb(110, 210, 255),
            Self::ModernHouse => egui::Color32::from_rgb(240, 245, 230),
            Self::Roads => egui::Color32::from_rgb(80, 235, 225),
            Self::Landscape => egui::Color32::from_rgb(130, 235, 95),
            Self::CityShell => egui::Color32::from_rgb(130, 255, 125),
            Self::Skyline => egui::Color32::from_rgb(255, 184, 70),
            Self::Spacecraft => egui::Color32::from_rgb(150, 205, 230),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_build_dock(
    active_tool: ToolbeltTool,
    picker_open: bool,
    status: &str,
    active_block: BlockType,
    brush: IVec3,
    undo_count: usize,
    redo_count: usize,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    dim: egui::Color32,
    ctx: &egui::Context,
) -> BuildDockResult {
    let mut result = BuildDockResult::default();
    let colors = theme.semantic();
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            colors.surface_strong.r(),
            colors.surface_strong.g(),
            colors.surface_strong.b(),
            if picker_open { 218 } else { 186 },
        ))
        .stroke(egui::Stroke::new(
            1.15,
            if picker_open {
                colors.info
            } else {
                active_tool.category_color()
            },
        ))
        .inner_margin(egui::Margin::symmetric(12.0, 9.0))
        .rounding(egui::Rounding::same(8.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 10.0),
            blur: 24.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(132),
        });

    egui::Area::new(egui::Id::new("voxel_native_build_dock"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -18.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_max_width(900.0);
                ui.spacing_mut().item_spacing = egui::vec2(7.0, 5.0);

                ui.horizontal(|ui| {
                    selected_tool_badge(ui, active_tool, picker_open, primary);
                    contextual_action_strip(ui, active_tool, picker_open, primary, dim);
                    ui.separator();
                    metric_chip(
                        ui,
                        Icon::Cube,
                        block_label(active_block),
                        active_tool.category_color(),
                        "Active build material",
                    );
                    if active_tool.uses_live_brush() {
                        metric_chip(
                            ui,
                            Icon::Brush,
                            &format!("{}x{}x{}", brush.x, brush.y, brush.z),
                            primary,
                            "Active brush size",
                        );
                    } else {
                        metric_chip(ui, Icon::Snap, "SNAP", primary, "Endpoint snap is active");
                    }
                    if history_chip(
                        ui,
                        Icon::Undo,
                        &undo_count.to_string(),
                        primary,
                        undo_count > 0,
                        "Undo last build edit",
                    ) {
                        result.history_command = Some(HistoryCommand::Undo);
                    }
                    if history_chip(
                        ui,
                        Icon::Redo,
                        &redo_count.to_string(),
                        dim,
                        redo_count > 0,
                        "Redo last undone build edit",
                    ) {
                        result.history_command = Some(HistoryCommand::Redo);
                    }
                    ui.separator();
                    if live_chip(ui, true, picker_open, primary) {
                        result.toggle_picker = true;
                    }
                });

                if !picker_open {
                    compact_hud_status(ui, status, active_tool, theme);
                }

                if picker_open {
                    crate::ui_kit::compact_separator(ui, theme);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                        for preset in BuildWorkflowPreset::ALL {
                            if workflow_preset_chip(ui, preset, active_tool == preset.tool()) {
                                result.workflow_preset = Some(preset);
                            }
                        }
                    });
                    crate::ui_kit::compact_separator(ui, theme);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                        let mut last_category = "";
                        for tool in ToolbeltTool::ALL {
                            if last_category != tool.category() {
                                category_mark(ui, tool);
                                last_category = tool.category();
                            }
                            if tool_chip(ui, tool, active_tool == tool, picker_open, primary, dim) {
                                result.clicked_tool = Some(tool);
                            }
                        }
                    });
                    crate::ui_kit::compact_separator(ui, theme);
                    material_catalog_panel(ui, active_block, theme, &mut result);
                }

                if picker_open && active_tool.uses_live_brush() {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                        for (label, size) in brush_presets() {
                            if brush_preset_chip(ui, label, size, brush) {
                                result.brush_preset = Some(size);
                            }
                        }
                    });
                }

                if picker_open {
                    ui.label(
                        egui::RichText::new(status)
                            .monospace()
                            .size(10.5)
                            .color(TEXT),
                    );
                }
            });
        });

    result
}

fn selected_tool_badge(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    picker_open: bool,
    primary: egui::Color32,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(154.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let glass = egui::Color32::from_rgba_unmultiplied(12, 34, 45, 188);
    let sheen = egui::Color32::from_rgba_unmultiplied(220, 250, 255, 34);
    painter.rect(
        rect,
        egui::Rounding::same(8.0),
        glass,
        egui::Stroke::new(1.0, tool.category_color()),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.center().y)),
        egui::Rounding::same(8.0),
        sheen,
    );
    let icon_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 7.0), egui::vec2(20.0, 20.0));
    paint_icon(&painter, icon_rect, tool.icon(), tool.category_color());
    painter.text(
        rect.min + egui::vec2(34.0, 9.0),
        egui::Align2::LEFT_CENTER,
        if picker_open { "PICKER" } else { "LIVE" },
        egui::FontId::monospace(9.5),
        AMBER,
    );
    painter.text(
        rect.min + egui::vec2(34.0, 23.0),
        egui::Align2::LEFT_CENTER,
        tool.chip_label(),
        egui::FontId::monospace(11.5),
        primary,
    );
    response.on_hover_text(tool.hint());
}

fn contextual_action_strip(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    picker_open: bool,
    primary: egui::Color32,
    dim: egui::Color32,
) {
    for action in tool.action_hints(picker_open).into_iter().flatten() {
        action_card(ui, tool, action, primary, dim);
    }
}

fn action_tone_color(
    tool: ToolbeltTool,
    tone: ActionTone,
    primary: egui::Color32,
    dim: egui::Color32,
) -> egui::Color32 {
    match tone {
        ActionTone::Tool => tool.category_color(),
        ActionTone::Primary => primary,
        ActionTone::Info => egui::Color32::from_rgb(82, 230, 255),
        ActionTone::Warning => AMBER,
        ActionTone::Danger => egui::Color32::from_rgb(255, 84, 96),
        ActionTone::Dim => dim,
    }
}

fn action_card(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    action: ToolActionHint,
    primary: egui::Color32,
    dim: egui::Color32,
) {
    let color = action_tone_color(tool, action.tone, primary, dim);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(86.0, 40.0), egui::Sense::hover());
    let hovered = response.hovered();
    let painter = ui.painter_at(rect);
    let fill = if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 58)
    } else {
        egui::Color32::from_rgba_unmultiplied(8, 20, 28, 172)
    };
    painter.rect(
        rect,
        egui::Rounding::same(7.0),
        fill,
        egui::Stroke::new(
            1.0,
            if hovered {
                color
            } else {
                color.linear_multiply(0.70)
            },
        ),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.top() + 13.0)),
        egui::Rounding::same(7.0),
        egui::Color32::from_white_alpha(if hovered { 34 } else { 20 }),
    );
    let mouse_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 8.0), egui::vec2(17.0, 23.0));
    paint_mouse_glyph(&painter, mouse_rect, action.glyph, color);
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(27.0, 10.0), egui::vec2(16.0, 16.0)),
        action.icon,
        color,
    );
    if !action.modifier.is_empty() {
        painter.text(
            rect.min + egui::vec2(45.0, 11.0),
            egui::Align2::LEFT_CENTER,
            action.modifier,
            egui::FontId::monospace(7.2),
            color,
        );
    }
    painter.text(
        rect.min + egui::vec2(45.0, 26.0),
        egui::Align2::LEFT_CENTER,
        action.label,
        egui::FontId::monospace(9.7),
        TEXT,
    );
    response.on_hover_text(action.hint);
}

fn compact_hud_status(
    ui: &mut egui::Ui,
    status: &str,
    active_tool: ToolbeltTool,
    theme: crate::theme::ThemeSettings,
) {
    let colors = theme.semantic();
    let text = compact_status(status, active_tool);
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(0, 8, 12, 112))
        .stroke(egui::Stroke::new(
            1.0,
            active_tool.category_color().linear_multiply(0.45),
        ))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0));
    ui.add_space(2.0);
    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            paint_icon(ui.painter(), rect, Icon::Hud, active_tool.category_color());
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .size(9.4)
                    .color(colors.text_muted),
            );
        });
    });
}

fn category_mark(ui: &mut egui::Ui, tool: ToolbeltTool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(58.0, 34.0), egui::Sense::hover());
    let color = tool.category_color();
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(5.0),
        egui::Color32::from_rgba_premultiplied(color.r() / 5, color.g() / 5, color.b() / 5, 220),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(5.0),
        egui::Stroke::new(1.0, color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        tool.category(),
        egui::FontId::monospace(9.0),
        color,
    );
    response.on_hover_text(tool.category());
}

fn block_egui_color(block: BlockType) -> egui::Color32 {
    let c = block.color().to_srgba();
    egui::Color32::from_rgba_unmultiplied(
        (c.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.alpha.clamp(0.35, 1.0) * 255.0).round() as u8,
    )
}

fn material_catalog_panel(
    ui: &mut egui::Ui,
    active_block: BlockType,
    theme: crate::theme::ThemeSettings,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("MATERIALS")
                .monospace()
                .size(10.0)
                .strong()
                .color(colors.info),
        );
        ui.label(
            egui::RichText::new(block_label(active_block))
                .monospace()
                .size(10.5)
                .color(AMBER),
        );
    });
    egui::ScrollArea::vertical()
        .id_source("build_studio_material_catalog")
        .max_height(176.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for category in block_palette_catalog() {
                let selected_category = category
                    .entries
                    .iter()
                    .any(|entry| entry.block == active_block);
                egui::CollapsingHeader::new(format!("{} - {}", category.label, category.hint))
                    .default_open(selected_category)
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                            for entry in category.entries {
                                if material_swatch_chip(ui, *entry, active_block, theme) {
                                    result.block_choice = Some(entry.block);
                                }
                            }
                        });
                    });
            }
        });
}

fn material_swatch_chip(
    ui: &mut egui::Ui,
    entry: BlockPaletteEntry,
    active_block: BlockType,
    theme: crate::theme::ThemeSettings,
) -> bool {
    let selected = active_block == entry.block;
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(128.0, 40.0), egui::Sense::click());
    let fill = if selected {
        egui::Color32::from_rgba_premultiplied(70, 45, 0, 226)
    } else if response.hovered() {
        colors.surface_strong
    } else {
        egui::Color32::from_rgba_premultiplied(0, 12, 8, 184)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(5.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(5.0),
        egui::Stroke::new(1.0, if selected { AMBER } else { colors.stroke }),
    );
    let swatch = egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 6.0), egui::vec2(28.0, 28.0));
    painter.rect_filled(
        swatch,
        egui::Rounding::same(4.0),
        block_egui_color(entry.block),
    );
    painter.rect_stroke(
        swatch,
        egui::Rounding::same(4.0),
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(70)),
    );
    painter.text(
        rect.min + egui::vec2(40.0, 15.0),
        egui::Align2::LEFT_CENTER,
        entry.label,
        egui::FontId::monospace(9.5),
        if selected { AMBER } else { TEXT },
    );
    painter.text(
        rect.min + egui::vec2(40.0, 29.0),
        egui::Align2::LEFT_CENTER,
        entry.role,
        egui::FontId::monospace(7.5),
        colors.text_muted,
    );
    response
        .on_hover_text(format!("{}: {}", entry.label, entry.role))
        .clicked()
}

fn brush_presets() -> [(&'static str, IVec3); 6] {
    [
        ("1x1", IVec3::new(1, 1, 1)),
        ("2x3", IVec3::new(2, 3, 1)),
        ("4x2", IVec3::new(4, 2, 1)),
        ("4x1", IVec3::new(4, 1, 1)),
        ("2x4", IVec3::new(2, 4, 1)),
        ("3x3", IVec3::new(3, 3, 1)),
    ]
}

fn brush_preset_chip(ui: &mut egui::Ui, label: &'static str, size: IVec3, brush: IVec3) -> bool {
    let selected = brush == size;
    let text = egui::RichText::new(label)
        .monospace()
        .size(10.0)
        .color(if selected { egui::Color32::BLACK } else { TEXT });
    let fill = if selected {
        AMBER
    } else {
        egui::Color32::from_rgba_premultiplied(0, 20, 12, 185)
    };
    ui.add(
        egui::Button::new(text)
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, AMBER.linear_multiply(0.70)))
            .rounding(egui::Rounding::same(4.0))
            .min_size(egui::vec2(44.0, 24.0)),
    )
    .on_hover_text("Live brush footprint")
    .clicked()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseGlyph {
    Left,
    Right,
    Wheel,
}

fn paint_mouse_glyph(
    painter: &egui::Painter,
    rect: egui::Rect,
    button: MouseGlyph,
    color: egui::Color32,
) {
    painter.rect(
        rect,
        egui::Rounding::same(8.0),
        egui::Color32::from_rgba_premultiplied(0, 0, 0, 120),
        egui::Stroke::new(1.0, color.linear_multiply(0.8)),
    );
    let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 11.0));
    let mid_x = top.center().x;
    painter.line_segment(
        [
            egui::pos2(mid_x, top.top()),
            egui::pos2(mid_x, top.bottom()),
        ],
        egui::Stroke::new(1.0, color.linear_multiply(0.55)),
    );
    match button {
        MouseGlyph::Left => {
            let fill = egui::Rect::from_min_max(
                top.min + egui::vec2(2.0, 2.0),
                egui::pos2(mid_x - 1.0, top.bottom() - 1.0),
            );
            painter.rect_filled(fill, egui::Rounding::same(3.0), color);
        }
        MouseGlyph::Right => {
            let fill = egui::Rect::from_min_max(
                egui::pos2(mid_x + 1.0, top.top() + 2.0),
                top.max - egui::vec2(2.0, 1.0),
            );
            painter.rect_filled(fill, egui::Rounding::same(3.0), color);
        }
        MouseGlyph::Wheel => {
            painter.circle_filled(egui::pos2(mid_x, top.center().y), 2.3, color);
            painter.line_segment(
                [
                    egui::pos2(mid_x, rect.min.y + 3.0),
                    egui::pos2(mid_x, rect.min.y + 8.5),
                ],
                egui::Stroke::new(1.0, egui::Color32::BLACK),
            );
        }
    }
}

fn metric_chip(
    ui: &mut egui::Ui,
    icon: Icon,
    value: &str,
    color: egui::Color32,
    hint: &'static str,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(70.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(4.0),
        egui::Color32::from_rgba_premultiplied(0, 8, 6, 180),
        egui::Stroke::new(1.0, color.linear_multiply(0.55)),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 8.0), egui::vec2(17.0, 17.0)),
        icon,
        color,
    );
    painter.text(
        rect.right_center() - egui::vec2(7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        value,
        egui::FontId::monospace(10.5),
        TEXT,
    );
    response.on_hover_text(hint);
}

fn history_chip(
    ui: &mut egui::Ui,
    icon: Icon,
    value: &str,
    color: egui::Color32,
    enabled: bool,
    hint: &'static str,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(70.0, 34.0), sense);
    let hovered = response.hovered() && enabled;
    let painter = ui.painter_at(rect);
    let visible_color = if enabled {
        color
    } else {
        color.linear_multiply(0.35)
    };
    let fill = if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 54)
    } else {
        egui::Color32::from_rgba_premultiplied(0, 8, 6, 180)
    };
    painter.rect(
        rect,
        egui::Rounding::same(4.0),
        fill,
        egui::Stroke::new(
            if enabled { 1.15 } else { 1.0 },
            visible_color.linear_multiply(if hovered { 0.95 } else { 0.55 }),
        ),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 8.0), egui::vec2(17.0, 17.0)),
        icon,
        visible_color,
    );
    painter.text(
        rect.right_center() - egui::vec2(7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        value,
        egui::FontId::monospace(10.5),
        if enabled {
            TEXT
        } else {
            egui::Color32::from_white_alpha(92)
        },
    );
    response
        .on_hover_text(if enabled {
            hint
        } else {
            "No build history for this command yet."
        })
        .clicked()
        && enabled
}

fn format_history_command_status(
    command: HistoryCommand,
    result: Option<(String, usize)>,
) -> String {
    match (command, result) {
        (HistoryCommand::Undo, Some((label, n))) => {
            format!("Undo '{label}': {n} voxels restored. Click Redo or press Ctrl+Y.")
        }
        (HistoryCommand::Redo, Some((label, n))) => {
            format!("Redo '{label}': {n} voxels applied. Click Undo or press Ctrl+Z.")
        }
        (HistoryCommand::Undo, None) => "Undo: no build edits to rewind yet.".into(),
        (HistoryCommand::Redo, None) => "Redo: no undone build edits to replay yet.".into(),
    }
}

fn tool_chip(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    selected: bool,
    expanded: bool,
    primary: egui::Color32,
    dim: egui::Color32,
) -> bool {
    let size = if expanded {
        egui::vec2(48.0, 48.0)
    } else {
        egui::vec2(36.0, 36.0)
    };
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = response.hovered();
    let stroke = if selected {
        AMBER
    } else if hovered {
        primary
    } else {
        dim
    };
    let bg = if selected {
        active_tool_bg(tool)
    } else if hovered {
        egui::Color32::from_rgba_premultiplied(0, 35, 20, 210)
    } else {
        egui::Color32::from_rgba_premultiplied(0, 10, 6, 190)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(4.0),
        bg,
        egui::Stroke::new(1.0, stroke),
    );
    let stripe = egui::Rect::from_min_size(
        rect.min + egui::vec2(3.0, 3.0),
        egui::vec2(4.0, rect.height() - 6.0),
    );
    painter.rect_filled(stripe, egui::Rounding::same(2.0), tool.category_color());
    if tool != ToolbeltTool::Navigate {
        painter.text(
            rect.left_top() + egui::vec2(10.0, 5.0),
            egui::Align2::LEFT_TOP,
            tool.quick_slot_label(),
            egui::FontId::monospace(8.0),
            if selected { AMBER } else { dim },
        );
    }
    let glyph = rect.shrink(if expanded { 11.0 } else { 8.0 });
    paint_icon(
        &painter,
        glyph,
        tool.icon(),
        if selected { AMBER } else { stroke },
    );
    if expanded {
        painter.text(
            rect.center_bottom() + egui::vec2(0.0, -3.0),
            egui::Align2::CENTER_BOTTOM,
            tool.chip_label(),
            egui::FontId::monospace(8.5),
            TEXT,
        );
    }
    let clicked = response.clicked();
    response.on_hover_text(format!(
        "{} [{}]\n{}",
        tool.label(),
        tool.quick_slot_label(),
        tool.hint()
    ));
    clicked
}

fn workflow_preset_chip(ui: &mut egui::Ui, preset: BuildWorkflowPreset, selected: bool) -> bool {
    let color = preset.color();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(82.0, 38.0), egui::Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 88)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 52)
    } else {
        egui::Color32::from_rgba_unmultiplied(10, 26, 34, 188)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(7.0),
        fill,
        egui::Stroke::new(1.0, if selected { AMBER } else { color }),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 9.0), egui::vec2(19.0, 19.0)),
        preset.icon(),
        if selected { AMBER } else { color },
    );
    painter.text(
        rect.right_center() - egui::vec2(8.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        preset.label(),
        egui::FontId::monospace(10.0),
        TEXT,
    );
    let clicked = response.clicked();
    response.on_hover_text(preset.hint());
    clicked
}

fn live_chip(ui: &mut egui::Ui, live: bool, expanded: bool, primary: egui::Color32) -> bool {
    let size = if expanded {
        egui::vec2(50.0, 48.0)
    } else {
        egui::vec2(38.0, 36.0)
    };
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let color = if live { AMBER } else { primary };
    let bg = if live {
        egui::Color32::from_rgba_premultiplied(80, 40, 0, 230)
    } else {
        egui::Color32::from_rgba_premultiplied(0, 12, 8, 190)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(4.0),
        bg,
        egui::Stroke::new(1.0, color),
    );
    paint_icon(
        &painter,
        rect.shrink(if expanded { 12.0 } else { 8.0 }),
        Icon::Pin,
        color,
    );
    if expanded {
        painter.text(
            rect.center_bottom() + egui::vec2(0.0, -3.0),
            egui::Align2::CENTER_BOTTOM,
            if live { "PICK" } else { "BUILD" },
            egui::FontId::monospace(9.0),
            TEXT,
        );
    }
    let clicked = response.clicked();
    response.on_hover_text("Show/hide Build Studio picker.");
    clicked
}

fn active_tool_bg(tool: ToolbeltTool) -> egui::Color32 {
    let c = tool.category_color();
    egui::Color32::from_rgba_premultiplied(c.r() / 2, c.g() / 3, c.b() / 3, 230)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toolbelt_enters_sketch_draw_first() {
        let toolbelt = ToolbeltState::default();

        assert_eq!(toolbelt.tool, ToolbeltTool::DrawRect);
        assert!(toolbelt.status.contains("Sketch"));
        assert!(ToolbeltTool::DrawRect.hint().contains("draw-first"));
        assert!(ToolbeltTool::DrawRect.hint().contains("RMB"));
    }

    #[test]
    fn sketch_action_cards_match_non_destructive_orbit_workflow() {
        let actions: Vec<ToolActionHint> = ToolbeltTool::DrawRect
            .action_hints(false)
            .into_iter()
            .flatten()
            .collect();

        assert_eq!(actions.len(), 4);
        assert!(actions
            .iter()
            .any(|a| a.glyph == MouseGlyph::Left && a.modifier.is_empty() && a.label == "Draw"));
        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Right
            && a.modifier == "HOLD"
            && a.label == "Orbit"
            && a.tone == ActionTone::Info));
        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier == "CTRL"
            && a.label == "Cut"
            && a.tone == ActionTone::Danger));
        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier == "SHIFT"
            && a.label == "Room"
            && a.tone == ActionTone::Warning));
    }

    #[test]
    fn road_action_cards_surface_component_editing() {
        let actions: Vec<ToolActionHint> = ToolbeltTool::CityRoad
            .action_hints(false)
            .into_iter()
            .flatten()
            .collect();

        assert!(actions.iter().any(|a| a.label == "Road"));
        assert!(actions
            .iter()
            .any(|a| a.label == "Delete" && a.tone == ActionTone::Danger));
        assert!(actions
            .iter()
            .any(|a| a.glyph == MouseGlyph::Wheel && a.label == "Shape"));
    }

    #[test]
    fn history_command_statuses_are_direct_and_actionable() {
        let undo = format_history_command_status(
            HistoryCommand::Undo,
            Some(("Sketch Fill 12 cells".into(), 12)),
        );
        let redo = format_history_command_status(HistoryCommand::Redo, None);

        assert!(undo.contains("Undo 'Sketch Fill 12 cells'"));
        assert!(undo.contains("Click Redo"));
        assert!(redo.contains("no undone build edits"));
    }

    #[test]
    fn live_brush_size_controls_only_attach_to_brush_tools() {
        assert!(ToolbeltTool::BrushPlace.uses_live_brush());
        assert!(ToolbeltTool::BrushCut.uses_live_brush());
        assert!(!ToolbeltTool::DrawRect.uses_live_brush());
        assert!(!ToolbeltTool::CityRoad.uses_live_brush());
    }

    #[test]
    fn city_road_hint_exposes_smart_road_workflow() {
        let hint = ToolbeltTool::CityRoad.hint();

        assert!(hint.contains("auto-snaps"));
        assert!(hint.contains("continues"));
        assert!(hint.contains("inherits"));
        assert!(hint.contains("bridge height"));
        assert!(hint.contains("Wheel edits"));
    }

    #[test]
    fn city_road_action_hints_explain_fast_branching_and_component_delete() {
        let actions: Vec<ToolActionHint> = ToolbeltTool::CityRoad
            .action_hints(false)
            .into_iter()
            .flatten()
            .collect();
        let road = actions
            .iter()
            .find(|a| a.label == "Road")
            .expect("road action");
        let delete = actions
            .iter()
            .find(|a| a.label == "Delete")
            .expect("delete action");

        assert!(road.hint.contains("endpoint"));
        assert!(road.hint.contains("branch"));
        assert!(delete.hint.contains("selected road component"));
        assert!(delete.hint.contains("cancel"));
    }

    #[test]
    fn workflow_presets_collapse_multi_step_builder_modes() {
        assert_eq!(BuildWorkflowPreset::Sketch.tool(), ToolbeltTool::DrawRect);
        assert_eq!(
            BuildWorkflowPreset::Sketch.brush(),
            Some(IVec3::new(4, 1, 1))
        );
        assert_eq!(BuildWorkflowPreset::Room.tool(), ToolbeltTool::DrawRect);
        assert!(BuildWorkflowPreset::Room.status().contains("Shift+LMB"));
        assert_eq!(BuildWorkflowPreset::Roads.tool(), ToolbeltTool::CityRoad);
        assert!(BuildWorkflowPreset::Roads
            .status()
            .contains("endpoint snap"));
        assert_eq!(
            BuildWorkflowPreset::CityShell.tool(),
            ToolbeltTool::CityBuilding
        );
        assert_eq!(
            BuildWorkflowPreset::Skyline.tool(),
            ToolbeltTool::SmartTower
        );
    }

    #[test]
    fn workflow_presets_pick_architecture_materials() {
        assert_eq!(
            BuildWorkflowPreset::ModernHouse.tool(),
            ToolbeltTool::DrawRect
        );
        assert_eq!(
            BuildWorkflowPreset::ModernHouse.block(),
            Some(crate::blocks::BlockType::Limestone)
        );
        assert_eq!(
            BuildWorkflowPreset::Room.block(),
            Some(crate::blocks::BlockType::Limestone)
        );
        assert_eq!(
            BuildWorkflowPreset::Roads.block(),
            Some(crate::blocks::BlockType::Stone)
        );
        assert_eq!(
            BuildWorkflowPreset::Landscape.block(),
            Some(crate::blocks::BlockType::Grass)
        );
        assert_eq!(
            BuildWorkflowPreset::Spacecraft.block(),
            Some(crate::blocks::BlockType::ShipHullAlloy)
        );
    }
}
