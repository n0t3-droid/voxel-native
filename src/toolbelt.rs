//! Compact in-game Sketch Editor for mouse-look building.
//!
//! The editor is mouse-first: pick a workflow from the toolbox, then keep
//! moving/flying while LMB/RMB works directly in the world. Weapons are
//! holstered for the whole edit state, including the drawer.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::blocks::{block_label, block_palette_catalog, BlockPaletteEntry, BlockType};
use crate::builder::{BuilderHistory, BuilderState};
use crate::city::CityTool;
use crate::icons::{paint_icon, Icon};
use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::settings::WorldSettings;
use crate::theme::{AMBER, TEXT};
use crate::world::VoxelWorld;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbeltTool {
    Navigate,
    /// Draw directly in the world: click a snapped start point, move to
    /// preview the locked-plane endpoint, click again to commit.
    DrawRect,
    /// SketchUp-style direct-manipulation sculpting. Hover a flat face
    /// to highlight it, click, move to push/pull, click again to commit.
    Sculpt,
    /// Move selected semantic geometry/components with snapped references.
    TransformMove,
    /// Scale selected semantic geometry/components around inference handles.
    TransformScale,
    /// Rotate selected semantic geometry/components around snapped axes.
    TransformRotate,
    /// Pick or apply material/style to the active tool or selected component.
    MaterialPicker,
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
    pub fn label(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "Navigate / Inspect",
            ToolbeltTool::DrawRect => "Sketch Draw",
            ToolbeltTool::Sculpt => "Push Pull Face",
            ToolbeltTool::TransformMove => "Move Selection",
            ToolbeltTool::TransformScale => "Scale Selection",
            ToolbeltTool::TransformRotate => "Rotate Selection",
            ToolbeltTool::MaterialPicker => "Material Style",
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
            ToolbeltTool::Navigate => "SELECT",
            ToolbeltTool::DrawRect => "RECT",
            ToolbeltTool::Sculpt => "PUSH/PULL",
            ToolbeltTool::TransformMove => "MOVE",
            ToolbeltTool::TransformScale => "SCALE",
            ToolbeltTool::TransformRotate => "ROTATE",
            ToolbeltTool::MaterialPicker => "PAINT",
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
            ToolbeltTool::TransformMove => Icon::Move,
            ToolbeltTool::TransformScale => Icon::Scale,
            ToolbeltTool::TransformRotate => Icon::Rotate,
            ToolbeltTool::MaterialPicker => Icon::Textures,
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
            ToolbeltTool::Navigate => "Move, inspect, and keep weapons off while the Sketch Editor is open.",
            ToolbeltTool::DrawRect => "SketchUp-style draw-first tool: click start, move to a snapped endpoint, click again to commit. Floors, roofs, and wall faces build; Opening cuts doors/windows. RMB orbits. Ctrl+Z/Ctrl+Y undo/redo.",
            ToolbeltTool::Sculpt => "SketchUp-style Push/Pull: click a face, move to choose depth, click again to commit. Use the toolbox to return to Rectangle or Pencil.",
            ToolbeltTool::TransformMove => "Select a drawn face/component, then drag along endpoint, midpoint, face-center, or axis inference to move it.",
            ToolbeltTool::TransformScale => "Select a drawn face/component, then drag a corner/edge handle to resize it from a snapped pivot.",
            ToolbeltTool::TransformRotate => "Select a drawn face/component, then drag the rotate ring; snaps favor clean 15/45/90 degree axes.",
            ToolbeltTool::MaterialPicker => "Pick a material/style for the selected component or for the next draw/pull/opening tool.",
            ToolbeltTool::SmartTower => "Two LMB clicks create a detailed skyscraper shell with floors, windows, crown, and undo.",
            ToolbeltTool::BrushPlace => "LMB starts a block point, drag to an endpoint, release to build; RMB uses the same gesture to cut.",
            ToolbeltTool::BrushCut => "LMB or RMB starts a cut point, drag to an endpoint, release to remove exact snapped blocks.",
            ToolbeltTool::CityRoad => "Click road endpoints to draw component roads: auto-snaps to endpoints/branches, continues from the last point, and inherits width, texture, and bridge height. Wheel edits selected roads: body width/radius, handle bridge height. Middle mouse retextures the selected component.",
            ToolbeltTool::CityDistrict => "Click two snapped corners to mark the exact bot city footprint. Bots stay parked until an area or explicit task is placed, then plan roads and buildings inside that space.",
            ToolbeltTool::CityBuilding => "Click two snapped corners to create a solid building shell; use Room and Opening cuts for interiors, doors, and windows.",
            ToolbeltTool::CityFacade => "LMB stamps the active facade onto the targeted wall.",
            ToolbeltTool::AnimationPick => "LMB/RMB pick voxels for animation authoring.",
        }
    }

    #[cfg(test)]
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
                    "Click a snapped start point, move to an endpoint, click again.",
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
                    "Use Opening from the toolbox for doors, windows, and exact wall cuts.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "SHIFT",
                    "Room",
                    Icon::Cube,
                    ActionTone::Warning,
                    "Use Room from the toolbox to hollow livable interior volume.",
                )),
            ],
            ToolbeltTool::Sculpt => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Push",
                    Icon::Move,
                    ActionTone::Tool,
                    "Click a face, move to choose depth, click again to commit.",
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
                    "Use Rectangle from the toolbox for temporary fill instead.",
                )),
                None,
            ],
            ToolbeltTool::TransformMove => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "DRAG",
                    "Move",
                    Icon::Move,
                    ActionTone::Tool,
                    "Drag selected geometry along endpoint, midpoint, face-center, or axis inference.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "HOLD",
                    "Orbit",
                    Icon::ModeNavigate,
                    ActionTone::Primary,
                    "Orbit without losing the selected transform tool.",
                )),
                None,
                None,
            ],
            ToolbeltTool::TransformScale => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "DRAG",
                    "Scale",
                    Icon::Scale,
                    ActionTone::Tool,
                    "Drag a corner or edge handle to resize from a snapped pivot.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "HOLD",
                    "Orbit",
                    Icon::ModeNavigate,
                    ActionTone::Primary,
                    "Orbit without cancelling the scale tool.",
                )),
                None,
                None,
            ],
            ToolbeltTool::TransformRotate => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "DRAG",
                    "Rotate",
                    Icon::Rotate,
                    ActionTone::Tool,
                    "Drag a rotate ring; clean axes and 15/45/90 degree snaps are preferred.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Right,
                    "HOLD",
                    "Orbit",
                    Icon::ModeNavigate,
                    ActionTone::Primary,
                    "Orbit while keeping rotate selected.",
                )),
                None,
                None,
            ],
            ToolbeltTool::MaterialPicker => [
                Some(ToolActionHint::new(
                    MouseGlyph::Left,
                    "",
                    "Apply",
                    Icon::Textures,
                    ActionTone::Tool,
                    "Apply the active style/material to the selected component or next tool.",
                )),
                Some(ToolActionHint::new(
                    MouseGlyph::Wheel,
                    "",
                    "Style",
                    Icon::Scale,
                    ActionTone::Info,
                    "Scroll material styles while the pointer is over the editor UI.",
                )),
                None,
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
                    "Click road endpoints with branch snapping; selected roads stay editable.",
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
                    "2-PT",
                    "Area",
                    Icon::District,
                    ActionTone::Tool,
                    "Click two corners for the bot city area; same-point two-click keeps radius placement.",
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
                    "2-PT",
                    "Shell",
                    Icon::City,
                    ActionTone::Tool,
                    "Click two corners for a building shell footprint.",
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

    fn category_color(self) -> egui::Color32 {
        match self {
            ToolbeltTool::Navigate => egui::Color32::from_rgb(180, 210, 190),
            ToolbeltTool::DrawRect | ToolbeltTool::Sculpt => egui::Color32::from_rgb(80, 170, 255),
            ToolbeltTool::TransformMove
            | ToolbeltTool::TransformScale
            | ToolbeltTool::TransformRotate => egui::Color32::from_rgb(255, 205, 92),
            ToolbeltTool::MaterialPicker => egui::Color32::from_rgb(180, 235, 255),
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
}

#[derive(Resource, Debug, Clone)]
pub struct ToolbeltState {
    pub live: bool,
    pub palette_open: bool,
    pub tool: ToolbeltTool,
    pub status: String,
    active_workflow: Option<BuildWorkflowPreset>,
    selection_generation: u64,
}

impl Default for ToolbeltState {
    fn default() -> Self {
        Self {
            live: false,
            palette_open: false,
            tool: ToolbeltTool::DrawRect,
            status:
                "Creative Sketch Builder: click start, move to a snapped endpoint, click again to commit; RMB orbits; toolbox picks Pencil, Rectangle, Room, Opening, and Push/Pull."
                    .into(),
            active_workflow: Some(BuildWorkflowPreset::Sketch),
            selection_generation: 0,
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

    pub fn room_workflow_active(&self) -> bool {
        self.live
            && !self.palette_open
            && self.tool == ToolbeltTool::DrawRect
            && self.active_workflow == Some(BuildWorkflowPreset::Room)
    }

    pub fn pencil_workflow_active(&self) -> bool {
        self.live
            && !self.palette_open
            && self.tool == ToolbeltTool::DrawRect
            && self.active_workflow == Some(BuildWorkflowPreset::Pencil)
    }

    pub(crate) fn drafting_shape_workflow(&self) -> Option<BuildWorkflowPreset> {
        (self.live && !self.palette_open && self.tool == ToolbeltTool::DrawRect)
            .then_some(self.active_workflow)
            .flatten()
            .filter(|workflow| {
                matches!(
                    workflow,
                    BuildWorkflowPreset::Circle
                        | BuildWorkflowPreset::Polygon
                        | BuildWorkflowPreset::Arc
                        | BuildWorkflowPreset::Freehand
                )
            })
    }

    pub fn opening_workflow_active(&self) -> bool {
        self.live
            && !self.palette_open
            && self.tool == ToolbeltTool::DrawRect
            && self.active_workflow == Some(BuildWorkflowPreset::Opening)
    }

    #[cfg(test)]
    pub(crate) fn active_workflow(&self) -> Option<BuildWorkflowPreset> {
        self.active_workflow
    }

    pub fn selection_generation(&self) -> u64 {
        self.selection_generation
    }

    pub fn clear_contextual_workflow(&mut self) {
        if self.active_workflow.is_some() {
            self.active_workflow = None;
            self.selection_generation = self.selection_generation.wrapping_add(1);
        }
    }

    fn select_tool(&mut self, tool: ToolbeltTool) {
        let next_workflow = if tool == ToolbeltTool::DrawRect {
            Some(BuildWorkflowPreset::Sketch)
        } else {
            None
        };
        if self.tool == tool && self.active_workflow == next_workflow {
            return;
        }
        self.tool = tool;
        self.active_workflow = next_workflow;
        self.selection_generation = self.selection_generation.wrapping_add(1);
    }

    fn select_workflow(&mut self, preset: BuildWorkflowPreset) {
        if self.tool == preset.tool() && self.active_workflow == Some(preset) {
            return;
        }
        self.tool = preset.tool();
        self.active_workflow = Some(preset);
        self.selection_generation = self.selection_generation.wrapping_add(1);
    }

    #[cfg(test)]
    pub(crate) fn select_workflow_for_test(&mut self, preset: BuildWorkflowPreset) {
        self.select_workflow(preset);
    }

    pub(crate) fn sync_workflow_to_tool(&mut self) {
        if self
            .active_workflow
            .is_some_and(|preset| preset.tool() != self.tool)
        {
            self.active_workflow = None;
            self.selection_generation = self.selection_generation.wrapping_add(1);
        }
    }
}

pub struct ToolbeltPlugin;

#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct SketchEditorUiFocus {
    pub pointer_over_editor_ui: bool,
    pub hover_drawer_open: bool,
    pub hover_drawer_grace_remaining: f32,
    hover_drawer_selection: Option<ToolboxSelection>,
}

impl Plugin for ToolbeltPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ToolbeltState::default())
            .insert_resource(SketchEditorUiFocus::default())
            .add_systems(Update, draw_toolbelt.run_if(in_state(GameState::InGame)));
    }
}

fn draw_toolbelt(
    mut contexts: EguiContexts,
    time: Res<Time>,
    settings: Res<WorldSettings>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut mode: ResMut<ModeContext>,
    mut builder: ResMut<BuilderState>,
    mut history: ResMut<BuilderHistory>,
    mut world: ResMut<VoxelWorld>,
    mut ui_focus: ResMut<SketchEditorUiFocus>,
    sketch_doc: Res<crate::sketch_model::SketchDocument>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
    mut wheel: EventReader<MouseWheel>,
) {
    if !mode.is_build() {
        ui_focus.pointer_over_editor_ui = false;
        ui_focus.hover_drawer_open = false;
        ui_focus.hover_drawer_grace_remaining = 0.0;
        ui_focus.hover_drawer_selection = None;
        wheel.clear();
        return;
    }

    let ctx = contexts.ctx_mut();
    let theme = settings.theme;
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let expanded = mode.is_build_picker();
    let live = mode.is_build();
    let active_tool = mode.build_tool().unwrap_or(toolbelt.tool);
    toolbelt.sync_workflow_to_tool();
    let status = compact_status_for_controller(
        &toolbelt.status,
        active_tool,
        toolbelt.active_workflow,
        &tool_controller,
    );
    let brush = builder.brush;

    let dock = draw_build_dock(
        active_tool,
        expanded,
        ui_focus.hover_drawer_open,
        &status,
        builder.block,
        brush,
        history.undo_len(),
        history.redo_len(),
        toolbelt.active_workflow,
        ui_focus.hover_drawer_selection,
        theme,
        primary,
        dim,
        ctx,
    );
    ui_focus.pointer_over_editor_ui = dock.wheel_navigation_hovered || dock.hover_bridge_hovered;
    let hover_state = next_hover_drawer_state(
        expanded,
        dock.toolbox_hovered,
        dock.drawer_hovered,
        dock.hover_bridge_hovered,
        ui_focus.hover_drawer_open,
        ui_focus.hover_drawer_grace_remaining,
        time.delta_seconds(),
    );
    ui_focus.hover_drawer_open = hover_state.open;
    ui_focus.hover_drawer_grace_remaining = hover_state.grace_remaining;
    if let Some(selection) = dock.hovered_selection {
        ui_focus.hover_drawer_selection = Some(selection);
    } else if !hover_state.open {
        ui_focus.hover_drawer_selection = None;
    }

    let wheel_delta: f32 = wheel.read().map(|ev| ev.y).sum();
    if live {
        if let Some(selection) = toolbox_wheel_selection_from_zone(
            active_tool,
            toolbelt.active_workflow,
            wheel_delta,
            dock.wheel_navigation_hovered,
        ) {
            apply_toolbox_selection(
                selection,
                &mut toolbelt,
                &mut mode,
                &mut builder,
                &mut tool_controller,
                sketch_doc.default_material(),
            );
        }
    }

    if let Some(tool) = dock.clicked_tool {
        apply_toolbox_selection(
            ToolboxSelection::Tool(tool),
            &mut toolbelt,
            &mut mode,
            &mut builder,
            &mut tool_controller,
            sketch_doc.default_material(),
        );
    }
    if let Some(preset) = dock.workflow_preset {
        apply_toolbox_selection(
            ToolboxSelection::Workflow(preset),
            &mut toolbelt,
            &mut mode,
            &mut builder,
            &mut tool_controller,
            sketch_doc.default_material(),
        );
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
                format!("Sketch Editor: {}. {}", tool.label(), tool.hint()),
            );
            toolbelt.status = mode.status.clone();
        } else if mode.is_build_live() {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Sketch Editor drawer visible. Pick a workflow, style, or material.",
            );
            toolbelt.status = mode.status.clone();
        } else {
            mode.set(
                ActiveMode::BuildPicker { tool },
                "Sketch Editor drawer visible. Pick a workflow, style, or material.",
            );
            toolbelt.status = mode.status.clone();
        }
    }
    if dock.exit_editor {
        mode.set(
            ActiveMode::Combat,
            "Play mode armed. Reopen Sketch Editor from the toolbox when building.",
        );
        toolbelt.status = mode.status.clone();
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

impl ToolbeltTool {
    pub(crate) fn uses_live_brush(self) -> bool {
        matches!(self, Self::BrushPlace | Self::BrushCut)
    }

    pub(crate) fn uses_pointer_editor_cursor(self) -> bool {
        !self.uses_live_brush()
    }
}

fn compact_status(
    status: &str,
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> String {
    if active_workflow == Some(BuildWorkflowPreset::ModernHouse) && tool == ToolbeltTool::DrawRect {
        return "HOUSE: Footprint first. Draw Rectangle/Pencil, then Push/Pull walls, Opening cuts, Room hollow.".to_owned();
    }
    if status.len() <= 96 {
        status.to_owned()
    } else if active_workflow == Some(BuildWorkflowPreset::Room) && tool == ToolbeltTool::DrawRect {
        "Room ready. Click two snapped corners to hollow interior volume; RMB orbits.".to_owned()
    } else if active_workflow == Some(BuildWorkflowPreset::Opening)
        && tool == ToolbeltTool::DrawRect
    {
        "Opening ready. Click two snapped corners to cut doors/windows; RMB orbits.".to_owned()
    } else if active_workflow == Some(BuildWorkflowPreset::Pencil) && tool == ToolbeltTool::DrawRect
    {
        "Pencil ready. Click endpoint to endpoint; each line chains from the last point.".to_owned()
    } else if tool == ToolbeltTool::DrawRect {
        format!(
            "{} ready. Click start, move, click finish; toolbox switches Room/Opening.",
            tool.label()
        )
    } else {
        format!(
            "{} ready. Click endpoints or faces; toolbox changes workflows.",
            tool.label()
        )
    }
}

fn compact_status_for_controller(
    status: &str,
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    tool_controller: &crate::sketch_model::ToolController,
) -> String {
    match tool_controller.tool_phase() {
        crate::sketch_model::EditorToolPhase::Previewing
        | crate::sketch_model::EditorToolPhase::Committed
        | crate::sketch_model::EditorToolPhase::Cancelled => {
            let lifecycle_status = format!(
                "{}: {}",
                tool_controller.active_tool_label(),
                tool_controller.active_tool_hint()
            );
            compact_single_line_status(&lifecycle_status)
        }
        crate::sketch_model::EditorToolPhase::Idle => compact_status(status, tool, active_workflow),
    }
}

fn compact_single_line_status(status: &str) -> String {
    const MAX_STATUS_LEN: usize = 132;
    if status.len() <= MAX_STATUS_LEN {
        return status.to_owned();
    }
    let mut compacted = status
        .chars()
        .take(MAX_STATUS_LEN.saturating_sub(3))
        .collect::<String>();
    compacted.push_str("...");
    compacted
}

fn workflow_preset_selected(
    preset: BuildWorkflowPreset,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> bool {
    if active_workflow == Some(preset) {
        return active_tool == preset.tool();
    }

    active_workflow.is_none()
        && preset == BuildWorkflowPreset::Sketch
        && active_tool == ToolbeltTool::DrawRect
}

#[derive(Default)]
struct BuildDockResult {
    clicked_tool: Option<ToolbeltTool>,
    wheel_navigation_hovered: bool,
    toolbox_hovered: bool,
    drawer_hovered: bool,
    hover_bridge_hovered: bool,
    hovered_selection: Option<ToolboxSelection>,
    toggle_picker: bool,
    exit_editor: bool,
    brush_preset: Option<IVec3>,
    workflow_preset: Option<BuildWorkflowPreset>,
    block_choice: Option<BlockType>,
    history_command: Option<HistoryCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorDrawerSurface {
    Hidden,
    HoverFlyout,
    FullDrawer,
}

fn editor_drawer_surface(picker_open: bool, hover_open: bool) -> EditorDrawerSurface {
    if picker_open {
        EditorDrawerSurface::FullDrawer
    } else if hover_open {
        EditorDrawerSurface::HoverFlyout
    } else {
        EditorDrawerSurface::Hidden
    }
}

const HOVER_DRAWER_GRACE_SECONDS: f32 = 0.85;

#[derive(Debug, Clone, Copy, PartialEq)]
struct HoverDrawerState {
    open: bool,
    grace_remaining: f32,
}

fn next_hover_drawer_state(
    picker_open: bool,
    toolbox_hovered: bool,
    drawer_hovered: bool,
    bridge_hovered: bool,
    was_open: bool,
    grace_remaining: f32,
    delta_seconds: f32,
) -> HoverDrawerState {
    if picker_open || toolbox_hovered || drawer_hovered || bridge_hovered {
        return HoverDrawerState {
            open: true,
            grace_remaining: HOVER_DRAWER_GRACE_SECONDS,
        };
    }

    let remaining = if was_open {
        (grace_remaining - delta_seconds.max(0.0)).max(0.0)
    } else {
        0.0
    };
    HoverDrawerState {
        open: remaining > 0.0,
        grace_remaining: remaining,
    }
}

fn hover_drawer_bridge_rect(screen: egui::Rect) -> egui::Rect {
    let center_y = screen.center().y;
    egui::Rect::from_min_max(
        egui::pos2(64.0, center_y - 420.0),
        egui::pos2(238.0, center_y + 420.0),
    )
}

fn hover_drawer_bridge_hovered(ctx: &egui::Context) -> bool {
    let Some(pointer) = ctx.pointer_hover_pos() else {
        return false;
    };
    hover_drawer_bridge_rect(ctx.screen_rect()).contains(pointer)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolboxSelection {
    Tool(ToolbeltTool),
    Workflow(BuildWorkflowPreset),
}

impl ToolboxSelection {
    const ORDER: [Self; 21] = [
        Self::Tool(ToolbeltTool::Navigate),
        Self::Workflow(BuildWorkflowPreset::Pencil),
        Self::Workflow(BuildWorkflowPreset::Sketch),
        Self::Workflow(BuildWorkflowPreset::Circle),
        Self::Workflow(BuildWorkflowPreset::PushPull),
        Self::Tool(ToolbeltTool::TransformMove),
        Self::Tool(ToolbeltTool::TransformRotate),
        Self::Tool(ToolbeltTool::TransformScale),
        Self::Tool(ToolbeltTool::MaterialPicker),
        Self::Workflow(BuildWorkflowPreset::Arc),
        Self::Workflow(BuildWorkflowPreset::Polygon),
        Self::Workflow(BuildWorkflowPreset::Freehand),
        Self::Workflow(BuildWorkflowPreset::Opening),
        Self::Workflow(BuildWorkflowPreset::Room),
        Self::Workflow(BuildWorkflowPreset::ModernHouse),
        Self::Workflow(BuildWorkflowPreset::Roads),
        Self::Workflow(BuildWorkflowPreset::BotArea),
        Self::Workflow(BuildWorkflowPreset::CityShell),
        Self::Workflow(BuildWorkflowPreset::Landscape),
        Self::Workflow(BuildWorkflowPreset::Skyline),
        Self::Workflow(BuildWorkflowPreset::Spacecraft),
    ];
}

const PRIMARY_TOOLBOX_ITEMS: [ToolboxSelection; 9] = [
    ToolboxSelection::Tool(ToolbeltTool::Navigate),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Circle),
    ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
    ToolboxSelection::Tool(ToolbeltTool::TransformMove),
    ToolboxSelection::Tool(ToolbeltTool::TransformRotate),
    ToolboxSelection::Tool(ToolbeltTool::TransformScale),
    ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryCommand {
    Undo,
    Redo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InferenceCue {
    Point,
    Corner,
    Center,
    Face,
    Plane,
    Axis,
    Path,
    Area,
    Volume,
}

impl InferenceCue {
    fn label(self) -> &'static str {
        match self {
            Self::Point => "Point",
            Self::Corner => "Corner",
            Self::Center => "Center",
            Self::Face => "Face",
            Self::Plane => "Plane",
            Self::Axis => "Axis",
            Self::Path => "Path",
            Self::Area => "Area",
            Self::Volume => "Volume",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Point => "snaps to endpoints and midpoints",
            Self::Corner => "snaps to block corners and opposite corners",
            Self::Center => "snaps from a center point to a radius",
            Self::Face => "locks onto the face under the cursor",
            Self::Plane => "locks drawing onto the floor, wall, or roof plane",
            Self::Axis => "keeps movement along one clean direction",
            Self::Path => "continues from road/path endpoints and branches",
            Self::Area => "uses two corners to mark a build zone",
            Self::Volume => "works with shell depth, rooms, and hollow space",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Point => Icon::Snap,
            Self::Corner => Icon::Grid,
            Self::Center => Icon::Magnet,
            Self::Face => Icon::Cube,
            Self::Plane => Icon::Layout,
            Self::Axis => Icon::Move,
            Self::Path => Icon::Road,
            Self::Area => Icon::District,
            Self::Volume => Icon::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildWorkflowPreset {
    Pencil,
    Sketch,
    Circle,
    Polygon,
    Arc,
    Freehand,
    Room,
    Opening,
    PushPull,
    ModernHouse,
    Roads,
    BotArea,
    Landscape,
    CityShell,
    Skyline,
    Spacecraft,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ActionTone {
    Tool,
    Primary,
    Info,
    Warning,
    Danger,
    Dim,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ToolActionHint {
    glyph: MouseGlyph,
    modifier: &'static str,
    label: &'static str,
    _icon: Icon,
    tone: ActionTone,
    hint: &'static str,
}

#[cfg(test)]
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
            _icon: icon,
            tone,
            hint,
        }
    }
}

impl BuildWorkflowPreset {
    const ALL: [Self; 16] = [
        Self::Pencil,
        Self::Sketch,
        Self::Circle,
        Self::Polygon,
        Self::Arc,
        Self::Freehand,
        Self::PushPull,
        Self::Room,
        Self::Opening,
        Self::Roads,
        Self::BotArea,
        Self::CityShell,
        Self::ModernHouse,
        Self::Landscape,
        Self::Skyline,
        Self::Spacecraft,
    ];
    #[cfg(test)]
    const TOOLBOX: [Self; 9] = [
        Self::Pencil,
        Self::Sketch,
        Self::Circle,
        Self::PushPull,
        Self::Opening,
        Self::Room,
        Self::Roads,
        Self::BotArea,
        Self::ModernHouse,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Pencil => "PENCIL",
            Self::Sketch => "RECTANGLE",
            Self::Circle => "CIRCLE",
            Self::Polygon => "POLYGON",
            Self::Arc => "ARC",
            Self::Freehand => "FREEHAND",
            Self::Room => "ROOM",
            Self::Opening => "OPENING",
            Self::PushPull => "PUSH/PULL",
            Self::ModernHouse => "HOUSE",
            Self::Roads => "ROADS",
            Self::BotArea => "AREA",
            Self::Landscape => "GARDEN",
            Self::CityShell => "CITY",
            Self::Skyline => "TOWER",
            Self::Spacecraft => "SHIP",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Pencil => Icon::Pipette,
            Self::Sketch => Icon::Grid,
            Self::Circle => Icon::Magnet,
            Self::Polygon => Icon::Cube,
            Self::Arc => Icon::Wand,
            Self::Freehand => Icon::Brush,
            Self::Room => Icon::Open,
            Self::Opening => Icon::Eraser,
            Self::PushPull => Icon::Move,
            Self::ModernHouse => Icon::Builder,
            Self::Roads => Icon::Road,
            Self::BotArea => Icon::District,
            Self::Landscape => Icon::Brush,
            Self::CityShell => Icon::City,
            Self::Skyline => Icon::Wand,
            Self::Spacecraft => Icon::Cube,
        }
    }

    fn tool(self) -> ToolbeltTool {
        match self {
            Self::Pencil => ToolbeltTool::DrawRect,
            Self::Sketch => ToolbeltTool::DrawRect,
            Self::Circle => ToolbeltTool::DrawRect,
            Self::Polygon => ToolbeltTool::DrawRect,
            Self::Arc => ToolbeltTool::DrawRect,
            Self::Freehand => ToolbeltTool::DrawRect,
            Self::Room => ToolbeltTool::DrawRect,
            Self::Opening => ToolbeltTool::DrawRect,
            Self::PushPull => ToolbeltTool::Sculpt,
            Self::ModernHouse => ToolbeltTool::DrawRect,
            Self::Roads => ToolbeltTool::CityRoad,
            Self::BotArea => ToolbeltTool::CityDistrict,
            Self::Landscape => ToolbeltTool::DrawRect,
            Self::CityShell => ToolbeltTool::CityBuilding,
            Self::Skyline => ToolbeltTool::SmartTower,
            Self::Spacecraft => ToolbeltTool::DrawRect,
        }
    }

    fn brush(self) -> Option<IVec3> {
        match self {
            Self::Pencil => Some(IVec3::new(1, 1, 1)),
            Self::Sketch => Some(IVec3::new(4, 1, 1)),
            Self::Circle => Some(IVec3::new(6, 1, 6)),
            Self::Polygon => Some(IVec3::new(6, 1, 6)),
            Self::Arc => Some(IVec3::new(1, 1, 1)),
            Self::Freehand => Some(IVec3::new(1, 1, 1)),
            Self::Room => Some(IVec3::new(8, 1, 1)),
            Self::Opening => Some(IVec3::new(2, 3, 1)),
            Self::PushPull => Some(IVec3::ONE),
            Self::ModernHouse => Some(IVec3::new(8, 1, 1)),
            Self::Landscape => Some(IVec3::new(8, 1, 8)),
            Self::Spacecraft => Some(IVec3::new(6, 1, 1)),
            Self::Roads | Self::BotArea | Self::CityShell | Self::Skyline => None,
        }
    }

    fn block(self) -> Option<BlockType> {
        match self {
            Self::Pencil => Some(BlockType::Limestone),
            Self::Sketch => Some(BlockType::Stone),
            Self::Circle => Some(BlockType::Limestone),
            Self::Polygon => Some(BlockType::Limestone),
            Self::Arc => Some(BlockType::Limestone),
            Self::Freehand => Some(BlockType::Limestone),
            Self::Room => Some(BlockType::Limestone),
            Self::Opening => Some(BlockType::Limestone),
            Self::PushPull => Some(BlockType::Limestone),
            Self::ModernHouse => Some(BlockType::Limestone),
            Self::Roads => Some(BlockType::Stone),
            Self::BotArea => None,
            Self::Landscape => Some(BlockType::Grass),
            Self::CityShell => Some(BlockType::Limestone),
            Self::Skyline => Some(BlockType::CockpitGlass),
            Self::Spacecraft => Some(BlockType::ShipHullAlloy),
        }
    }

    fn status(self) -> String {
        match self {
            Self::Pencil => "Pencil workflow: click a snapped start point, move to an endpoint, click again; lines chain from the last endpoint. RMB orbits; Ctrl+Z undo.".into(),
            Self::Sketch => "Rectangle workflow: click a snapped start point, move to the opposite corner, click again. Floors, roofs, and wall faces build; Opening cuts doors/windows. RMB orbits.".into(),
            Self::Circle => "Circle workflow: click a snapped center, move to the radius endpoint, click again to place a filled circular face on the locked plane. RMB orbits.".into(),
            Self::Polygon => "Polygon workflow: click a snapped center, move to the radius endpoint, click again to place a hex face on the locked plane. RMB orbits.".into(),
            Self::Arc => "Arc workflow: click a snapped center, move to the radius endpoint, click again to trace a curved guide/edge on the locked plane. RMB orbits.".into(),
            Self::Freehand => "Freehand workflow: click a snapped start, move to the endpoint, click again to place a quick drawn voxel stroke. RMB orbits.".into(),
            Self::Room => "Room workflow: click two snapped wall/floor corners to hollow livable depth behind the selected face. RMB orbits.".into(),
            Self::Opening => "Opening workflow: click two snapped door/window corners on the locked face plane; the cut drills through wall thickness. RMB orbits.".into(),
            Self::PushPull => "Push/Pull workflow: click a face, move to choose extrusion depth, click again to commit. RMB orbits.".into(),
            Self::ModernHouse => "HOUSE workflow: 1 Footprint with Rectangle/Pencil, 2 Push/Pull walls and roof massing, 3 Opening cuts doors/windows, 4 Room hollows the livable interior.".into(),
            Self::Roads => "Road and traffic workflow: click component endpoints with branch snap; wheel edits width/bridge height; middle mouse retextures.".into(),
            Self::BotArea => "Bot city area workflow: click two snapped corners where bots may plan roads, buildings, and city work.".into(),
            Self::Landscape => "Garden workflow: large ground brush for lawns, paths, pools, and planted courtyards.".into(),
            Self::CityShell => "City workflow: click two snapped corners for a building shell footprint; roads and frontage stay component-aware.".into(),
            Self::Skyline => "Tower workflow: two clicks create a varied skyscraper shell with floors, crown, and undo.".into(),
            Self::Spacecraft => "Spacecraft workflow: alloy material and long hull brush for shuttles, fins, and cockpit follow-up.".into(),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Pencil => "Draw connected voxel edges and wall lines like SketchUp's pencil.",
            Self::Sketch => "One click switches to rectangle sketching and a flat 4x1 brush.",
            Self::Circle => "Draw a circular face on the locked floor, wall, or roof plane.",
            Self::Polygon => "Draw a six-sided polygon face without leaving Sketch Editor.",
            Self::Arc => "Trace a curved edge on the locked plane for rounded details.",
            Self::Freehand => "Draw a quick freehand-style voxel stroke on the locked plane.",
            Self::Room => {
                "One click switches to direct room hollowing: click two corners to carve usable interior space."
            }
            Self::Opening => "One click switches to direct door/window cutting without modifier keys.",
            Self::PushPull => "One click switches to SketchUp-style face push/pull.",
            Self::ModernHouse => "Guided house workflow: footprint, Push/Pull massing, openings, and room hollowing.",
            Self::Roads => {
                "One click switches to road components: draw, branch, adjust, retexture."
            }
            Self::BotArea => {
                "One click switches to bot city area drawing: mark where bots are allowed to build."
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

    fn inference_cue(self) -> InferenceCue {
        match self {
            Self::Pencil => InferenceCue::Point,
            Self::Sketch => InferenceCue::Corner,
            Self::Circle => InferenceCue::Center,
            Self::Polygon => InferenceCue::Center,
            Self::Arc => InferenceCue::Path,
            Self::Freehand => InferenceCue::Path,
            Self::Room => InferenceCue::Volume,
            Self::Opening => InferenceCue::Face,
            Self::PushPull => InferenceCue::Face,
            Self::ModernHouse => InferenceCue::Volume,
            Self::Roads => InferenceCue::Path,
            Self::BotArea => InferenceCue::Area,
            Self::Landscape => InferenceCue::Plane,
            Self::CityShell => InferenceCue::Corner,
            Self::Skyline => InferenceCue::Axis,
            Self::Spacecraft => InferenceCue::Axis,
        }
    }

    fn inference_hover_text(self) -> String {
        let cue = self.inference_cue();
        format!(
            "{}\nInference: {} - {}\n{}",
            self.label(),
            cue.label(),
            cue.hint(),
            self.hint()
        )
    }

    fn color(self) -> egui::Color32 {
        match self {
            Self::Pencil => egui::Color32::from_rgb(255, 182, 102),
            Self::Sketch => egui::Color32::from_rgb(255, 138, 184),
            Self::Circle => egui::Color32::from_rgb(255, 170, 204),
            Self::Polygon => egui::Color32::from_rgb(232, 196, 220),
            Self::Arc => egui::Color32::from_rgb(255, 203, 112),
            Self::Freehand => egui::Color32::from_rgb(255, 166, 146),
            Self::Room => egui::Color32::from_rgb(215, 230, 206),
            Self::Opening => egui::Color32::from_rgb(255, 94, 130),
            Self::PushPull => egui::Color32::from_rgb(246, 190, 130),
            Self::ModernHouse => egui::Color32::from_rgb(255, 242, 224),
            Self::Roads => egui::Color32::from_rgb(84, 220, 205),
            Self::BotArea => egui::Color32::from_rgb(142, 226, 150),
            Self::Landscape => egui::Color32::from_rgb(118, 206, 120),
            Self::CityShell => egui::Color32::from_rgb(188, 226, 196),
            Self::Skyline => egui::Color32::from_rgb(255, 176, 80),
            Self::Spacecraft => egui::Color32::from_rgb(192, 210, 224),
        }
    }
}

#[derive(Clone, Copy)]
struct WorkflowDrawerGroup {
    label: &'static str,
    hint: &'static str,
    icon: Icon,
    presets: &'static [BuildWorkflowPreset],
}

#[derive(Clone, Copy)]
struct ToolboxContextGroup {
    label: &'static str,
    hint: &'static str,
    icon: Icon,
    items: &'static [ToolboxSelection],
}

const DRAW_WORKFLOWS: [BuildWorkflowPreset; 6] = [
    BuildWorkflowPreset::Pencil,
    BuildWorkflowPreset::Sketch,
    BuildWorkflowPreset::Circle,
    BuildWorkflowPreset::Polygon,
    BuildWorkflowPreset::Arc,
    BuildWorkflowPreset::Freehand,
];

const SHAPE_WORKFLOWS: [BuildWorkflowPreset; 4] = [
    BuildWorkflowPreset::PushPull,
    BuildWorkflowPreset::Opening,
    BuildWorkflowPreset::Room,
    BuildWorkflowPreset::ModernHouse,
];

const WORLD_WORKFLOWS: [BuildWorkflowPreset; 6] = [
    BuildWorkflowPreset::Roads,
    BuildWorkflowPreset::BotArea,
    BuildWorkflowPreset::CityShell,
    BuildWorkflowPreset::Landscape,
    BuildWorkflowPreset::Skyline,
    BuildWorkflowPreset::Spacecraft,
];

const DRAW_CONTEXT_ITEMS: [ToolboxSelection; 6] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Circle),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Polygon),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Arc),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Freehand),
];

const EDIT_CONTEXT_ITEMS: [ToolboxSelection; 6] = [
    ToolboxSelection::Tool(ToolbeltTool::Navigate),
    ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
    ToolboxSelection::Tool(ToolbeltTool::TransformMove),
    ToolboxSelection::Tool(ToolbeltTool::TransformScale),
    ToolboxSelection::Tool(ToolbeltTool::TransformRotate),
    ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
];

const OPEN_CONTEXT_ITEMS: [ToolboxSelection; 5] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Opening),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Room),
    ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
    ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
    ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
];

const HOUSE_CONTEXT_ITEMS: [ToolboxSelection; 6] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
    ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Opening),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Room),
    ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
];

const CITY_CONTEXT_ITEMS: [ToolboxSelection; 5] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Roads),
    ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea),
    ToolboxSelection::Workflow(BuildWorkflowPreset::CityShell),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Landscape),
    ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
];

const SCENE_CONTEXT_ITEMS: [ToolboxSelection; 5] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Landscape),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Skyline),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Spacecraft),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Roads),
    ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea),
];

fn context_group_for_selection(selection: ToolboxSelection) -> ToolboxContextGroup {
    match selection {
        ToolboxSelection::Tool(ToolbeltTool::DrawRect)
        | ToolboxSelection::Workflow(
            BuildWorkflowPreset::Pencil
            | BuildWorkflowPreset::Sketch
            | BuildWorkflowPreset::Circle
            | BuildWorkflowPreset::Polygon
            | BuildWorkflowPreset::Arc
            | BuildWorkflowPreset::Freehand,
        ) => ToolboxContextGroup {
            label: "Draw",
            hint: "Start with points, corners, and clean planar faces.",
            icon: Icon::Pipette,
            items: &DRAW_CONTEXT_ITEMS,
        },
        ToolboxSelection::Tool(ToolbeltTool::Sculpt)
        | ToolboxSelection::Tool(
            ToolbeltTool::Navigate
            | ToolbeltTool::TransformMove
            | ToolbeltTool::TransformScale
            | ToolbeltTool::TransformRotate
            | ToolbeltTool::MaterialPicker,
        )
        | ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull) => ToolboxContextGroup {
            label: "Edit Selected",
            hint: "Select a face/component, then pull, move, scale, rotate, or style it.",
            icon: Icon::Move,
            items: &EDIT_CONTEXT_ITEMS,
        },
        ToolboxSelection::Workflow(BuildWorkflowPreset::Opening | BuildWorkflowPreset::Room) => {
            ToolboxContextGroup {
                label: "Openings",
                hint: "Cut doors/windows, hollow rooms, then finish walls and materials.",
                icon: Icon::Open,
                items: &OPEN_CONTEXT_ITEMS,
            }
        }
        ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse) => ToolboxContextGroup {
            label: "House Builder",
            hint: "Footprint, pull walls, cut openings, hollow the room, then style it.",
            icon: Icon::Builder,
            items: &HOUSE_CONTEXT_ITEMS,
        },
        ToolboxSelection::Workflow(
            BuildWorkflowPreset::Roads
            | BuildWorkflowPreset::BotArea
            | BuildWorkflowPreset::CityShell,
        ) => ToolboxContextGroup {
            label: "City Layout",
            hint: "Draw roads and mark bot/city areas after the building shell is clear.",
            icon: Icon::Road,
            items: &CITY_CONTEXT_ITEMS,
        },
        ToolboxSelection::Workflow(
            BuildWorkflowPreset::Landscape
            | BuildWorkflowPreset::Skyline
            | BuildWorkflowPreset::Spacecraft,
        ) => ToolboxContextGroup {
            label: "Scene",
            hint: "Add gardens, skyline massing, and spacecraft once the main build reads cleanly.",
            icon: Icon::City,
            items: &SCENE_CONTEXT_ITEMS,
        },
        ToolboxSelection::Tool(
            ToolbeltTool::SmartTower
            | ToolbeltTool::BrushPlace
            | ToolbeltTool::BrushCut
            | ToolbeltTool::CityRoad
            | ToolbeltTool::CityDistrict
            | ToolbeltTool::CityBuilding
            | ToolbeltTool::CityFacade
            | ToolbeltTool::AnimationPick,
        ) => ToolboxContextGroup {
            label: "City Layout",
            hint: "Advanced world tools stay grouped away from the first building flow.",
            icon: Icon::City,
            items: &CITY_CONTEXT_ITEMS,
        },
    }
}

fn workflow_drawer_groups() -> [WorkflowDrawerGroup; 3] {
    [
        WorkflowDrawerGroup {
            label: "Draw",
            hint: "Lines, boxes, circles, polygons, arcs, and freehand strokes.",
            icon: Icon::Pipette,
            presets: &DRAW_WORKFLOWS,
        },
        WorkflowDrawerGroup {
            label: "Shape",
            hint: "Push faces, cut windows, hollow rooms, and guide house massing.",
            icon: Icon::Builder,
            presets: &SHAPE_WORKFLOWS,
        },
        WorkflowDrawerGroup {
            label: "World",
            hint: "Roads, bot areas, city shells, gardens, towers, and spacecraft.",
            icon: Icon::City,
            presets: &WORLD_WORKFLOWS,
        },
    ]
}

fn active_toolbox_selection(
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> ToolboxSelection {
    if let Some(preset) = active_workflow {
        return ToolboxSelection::Workflow(preset);
    }
    match active_tool {
        ToolbeltTool::Navigate
        | ToolbeltTool::TransformMove
        | ToolbeltTool::TransformScale
        | ToolbeltTool::TransformRotate
        | ToolbeltTool::MaterialPicker => return ToolboxSelection::Tool(active_tool),
        _ => {}
    }
    BuildWorkflowPreset::ALL
        .into_iter()
        .find(|preset| preset.tool() == active_tool)
        .map(ToolboxSelection::Workflow)
        .unwrap_or(ToolboxSelection::Tool(ToolbeltTool::Navigate))
}

fn toolbox_wheel_selection(
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    wheel_delta: f32,
) -> Option<ToolboxSelection> {
    if wheel_delta.abs() < 0.5 {
        return None;
    }
    let step = if wheel_delta > 0.0 { -1 } else { 1 };
    let active = active_toolbox_selection(active_tool, active_workflow);
    let index = ToolboxSelection::ORDER
        .iter()
        .position(|selection| *selection == active)
        .unwrap_or(0);
    let next = (index as isize + step).rem_euclid(ToolboxSelection::ORDER.len() as isize) as usize;
    Some(ToolboxSelection::ORDER[next])
}

fn toolbox_wheel_selection_from_zone(
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    wheel_delta: f32,
    pointer_over_editor_ui: bool,
) -> Option<ToolboxSelection> {
    if !pointer_over_editor_ui {
        return None;
    }
    toolbox_wheel_selection(active_tool, active_workflow, wheel_delta)
}

fn apply_toolbox_selection(
    selection: ToolboxSelection,
    toolbelt: &mut ToolbeltState,
    mode: &mut ModeContext,
    builder: &mut BuilderState,
    tool_controller: &mut crate::sketch_model::ToolController,
    default_material: crate::sketch_model::SketchId,
) {
    if selection == ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse) {
        tool_controller.start_house_workflow(default_material);
    } else {
        tool_controller.activate(toolbox_selection_editor_tool(selection));
        tool_controller.cancel_transaction();
    }
    match selection {
        ToolboxSelection::Tool(tool) => {
            toolbelt.select_tool(tool);
            if tool == ToolbeltTool::MaterialPicker {
                mode.set(
                    ActiveMode::BuildPicker { tool },
                    "Material Style: pick a material for the selected component or next tool.",
                );
            } else {
                mode.set(
                    ActiveMode::BuildLive { tool },
                    format!("Sketch Editor: {}. {}", tool.label(), tool.hint()),
                );
            }
            toolbelt.status = mode.status.clone();
        }
        ToolboxSelection::Workflow(preset) => {
            let tool = preset.tool();
            toolbelt.select_workflow(preset);
            if let Some(brush) = preset.brush() {
                builder.brush = brush;
                builder.status = format!("Live Brush {}x{}x{}", brush.x, brush.y, brush.z);
            }
            if let Some(block) = preset.block() {
                builder.block = block;
                builder.status = format!("Material: {}", block_label(block));
            }
            mode.set(ActiveMode::BuildLive { tool }, preset.status());
            if let Some(guide) = tool_controller.house_guide() {
                mode.status = guide.status().into();
            }
            toolbelt.status = mode.status.clone();
        }
    }
}

fn toolbox_selection_editor_tool(selection: ToolboxSelection) -> crate::sketch_model::EditorToolId {
    match selection {
        ToolboxSelection::Tool(tool) => editor_tool_for_tool(tool),
        ToolboxSelection::Workflow(preset) => editor_tool_for_workflow(preset),
    }
}

fn editor_tool_for_tool(tool: ToolbeltTool) -> crate::sketch_model::EditorToolId {
    match tool {
        ToolbeltTool::Navigate => crate::sketch_model::EditorToolId::Select,
        ToolbeltTool::DrawRect => crate::sketch_model::EditorToolId::Rectangle,
        ToolbeltTool::Sculpt => crate::sketch_model::EditorToolId::PushPull,
        ToolbeltTool::TransformMove => crate::sketch_model::EditorToolId::Move,
        ToolbeltTool::TransformScale => crate::sketch_model::EditorToolId::Scale,
        ToolbeltTool::TransformRotate => crate::sketch_model::EditorToolId::Rotate,
        ToolbeltTool::MaterialPicker => crate::sketch_model::EditorToolId::Material,
        ToolbeltTool::CityRoad => crate::sketch_model::EditorToolId::Road,
        ToolbeltTool::CityDistrict => crate::sketch_model::EditorToolId::BotArea,
        _ => crate::sketch_model::EditorToolId::Rectangle,
    }
}

fn editor_tool_for_workflow(preset: BuildWorkflowPreset) -> crate::sketch_model::EditorToolId {
    match preset {
        BuildWorkflowPreset::Pencil => crate::sketch_model::EditorToolId::Pencil,
        BuildWorkflowPreset::Sketch => crate::sketch_model::EditorToolId::Rectangle,
        BuildWorkflowPreset::Circle => crate::sketch_model::EditorToolId::Circle,
        BuildWorkflowPreset::Polygon => crate::sketch_model::EditorToolId::Polygon,
        BuildWorkflowPreset::Arc => crate::sketch_model::EditorToolId::Arc,
        BuildWorkflowPreset::Freehand => crate::sketch_model::EditorToolId::Freehand,
        BuildWorkflowPreset::ModernHouse => crate::sketch_model::EditorToolId::House,
        BuildWorkflowPreset::PushPull => crate::sketch_model::EditorToolId::PushPull,
        BuildWorkflowPreset::Room => crate::sketch_model::EditorToolId::Room,
        BuildWorkflowPreset::Opening => crate::sketch_model::EditorToolId::CutOpening,
        BuildWorkflowPreset::Roads => crate::sketch_model::EditorToolId::Road,
        BuildWorkflowPreset::BotArea => crate::sketch_model::EditorToolId::BotArea,
        BuildWorkflowPreset::Landscape
        | BuildWorkflowPreset::CityShell
        | BuildWorkflowPreset::Skyline
        | BuildWorkflowPreset::Spacecraft => crate::sketch_model::EditorToolId::Rectangle,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_build_dock(
    active_tool: ToolbeltTool,
    picker_open: bool,
    hover_drawer_open: bool,
    status: &str,
    active_block: BlockType,
    brush: IVec3,
    undo_count: usize,
    redo_count: usize,
    active_workflow: Option<BuildWorkflowPreset>,
    retained_hover_selection: Option<ToolboxSelection>,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    dim: egui::Color32,
    ctx: &egui::Context,
) -> BuildDockResult {
    let mut result = BuildDockResult::default();
    let colors = theme.semantic();

    draw_editor_toolbox(
        ctx,
        active_tool,
        active_workflow,
        picker_open,
        undo_count,
        redo_count,
        theme,
        primary,
        dim,
        &mut result,
    );
    result.hover_bridge_hovered = hover_drawer_bridge_hovered(ctx);
    let hover_visible =
        picker_open || hover_drawer_open || result.toolbox_hovered || result.hover_bridge_hovered;
    let surface = editor_drawer_surface(picker_open, hover_visible);
    draw_editor_status_bar(
        ctx,
        active_tool,
        active_workflow,
        picker_open,
        status,
        active_block,
        brush,
        undo_count,
        redo_count,
        theme,
        primary,
        dim,
        &mut result,
    );
    match surface {
        EditorDrawerSurface::Hidden => {}
        EditorDrawerSurface::HoverFlyout => {
            let hovered = hover_drawer_selection(
                result.hovered_selection,
                retained_hover_selection,
                active_tool,
                active_workflow,
            );
            draw_editor_hover_flyout(
                ctx,
                hovered,
                active_tool,
                active_workflow,
                theme,
                primary,
                &mut result,
            );
        }
        EditorDrawerSurface::FullDrawer => {
            draw_editor_drawer(
                ctx,
                active_tool,
                active_workflow,
                active_block,
                brush,
                theme,
                colors.info,
                &mut result,
            );
        }
    }

    result
}

fn hover_drawer_selection(
    current_hover: Option<ToolboxSelection>,
    retained_hover: Option<ToolboxSelection>,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> ToolboxSelection {
    current_hover
        .or(retained_hover)
        .unwrap_or_else(|| active_toolbox_selection(active_tool, active_workflow))
}

#[allow(clippy::too_many_arguments)]
fn draw_editor_toolbox(
    ctx: &egui::Context,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    picker_open: bool,
    undo_count: usize,
    redo_count: usize,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    dim: egui::Color32,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            colors.surface_strong.r(),
            colors.surface_strong.g(),
            colors.surface_strong.b(),
            204,
        ))
        .stroke(egui::Stroke::new(
            1.15,
            if picker_open {
                colors.info
            } else {
                active_editor_color(active_tool, active_workflow)
            },
        ))
        .inner_margin(egui::Margin::symmetric(7.0, 8.0))
        .rounding(egui::Rounding::same(8.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 20.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(118),
        });

    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_toolbox"))
        .anchor(egui::Align2::LEFT_CENTER, egui::vec2(14.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(66.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 5.0);
                ui.vertical_centered(|ui| {
                    for (index, item) in PRIMARY_TOOLBOX_ITEMS.into_iter().enumerate() {
                        if index == 1 || index == 4 || index == 8 {
                            editor_toolbox_separator(ui, colors.stroke);
                        }
                        match item {
                            ToolboxSelection::Tool(tool) => {
                                let (clicked, hovered) = toolbox_tool_button(
                                    ui,
                                    tool,
                                    toolbox_tool_label(tool),
                                    active_tool == tool && active_workflow.is_none(),
                                    primary,
                                    dim,
                                );
                                if hovered {
                                    result.hovered_selection = Some(ToolboxSelection::Tool(tool));
                                }
                                if clicked {
                                    result.clicked_tool = Some(tool);
                                }
                            }
                            ToolboxSelection::Workflow(preset) => {
                                let (clicked, hovered) = toolbox_workflow_button(
                                    ui,
                                    preset,
                                    workflow_preset_selected(preset, active_tool, active_workflow),
                                );
                                if hovered {
                                    result.hovered_selection =
                                        Some(ToolboxSelection::Workflow(preset));
                                }
                                if clicked {
                                    result.workflow_preset = Some(preset);
                                }
                            }
                        }
                    }
                    editor_toolbox_separator(ui, colors.stroke);
                    if toolbox_command_button(
                        ui,
                        Icon::Textures,
                        "STYLE",
                        picker_open,
                        primary,
                        true,
                        "Open the material and workflow drawer.",
                    ) {
                        result.toggle_picker = true;
                    }
                    if toolbox_command_button(
                        ui,
                        Icon::Undo,
                        "UNDO",
                        false,
                        primary,
                        undo_count > 0,
                        "Undo the last build edit.",
                    ) {
                        result.history_command = Some(HistoryCommand::Undo);
                    }
                    if toolbox_command_button(
                        ui,
                        Icon::Redo,
                        "REDO",
                        false,
                        dim,
                        redo_count > 0,
                        "Redo the last undone build edit.",
                    ) {
                        result.history_command = Some(HistoryCommand::Redo);
                    }
                    if toolbox_command_button(
                        ui,
                        Icon::Play,
                        "PLAY",
                        false,
                        AMBER,
                        true,
                        "Exit Sketch Editor and return to play mode.",
                    ) {
                        result.exit_editor = true;
                    }
                });
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.toolbox_hovered |= area.response.hovered();
}

#[allow(clippy::too_many_arguments)]
fn draw_editor_status_bar(
    ctx: &egui::Context,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    picker_open: bool,
    status: &str,
    active_block: BlockType,
    brush: IVec3,
    undo_count: usize,
    redo_count: usize,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    dim: egui::Color32,
    result: &mut BuildDockResult,
) {
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
            offset: egui::vec2(0.0, 8.0),
            blur: 18.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(116),
        });

    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_status"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_max_width(930.0);
                ui.spacing_mut().item_spacing = egui::vec2(7.0, 0.0);
                ui.horizontal(|ui| {
                    selected_tool_badge(ui, active_tool, picker_open, active_workflow, primary);
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
                    if drawer_chip(ui, picker_open, primary) {
                        result.toggle_picker = true;
                    }
                    ui.label(
                        egui::RichText::new(status)
                            .monospace()
                            .size(10.5)
                            .color(colors.text_muted),
                    );
                });
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
}

fn draw_editor_drawer(
    ctx: &egui::Context,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    active_block: BlockType,
    brush: IVec3,
    theme: crate::theme::ThemeSettings,
    accent: egui::Color32,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let open_t = ctx
        .animate_bool(
            egui::Id::new("voxel_native_sketch_editor_full_drawer_anim"),
            true,
        )
        .clamp(0.0, 1.0);
    let fill_alpha = (168.0 + 58.0 * open_t).round() as u8;
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            colors.surface_strong.r(),
            colors.surface_strong.g(),
            colors.surface_strong.b(),
            fill_alpha,
        ))
        .stroke(egui::Stroke::new(1.1, accent))
        .inner_margin(egui::Margin::symmetric(10.0, 10.0))
        .rounding(egui::Rounding::same(8.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 9.0),
            blur: 22.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(128),
        });

    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_drawer"))
        .anchor(
            egui::Align2::LEFT_CENTER,
            egui::vec2(76.0 + 16.0 * open_t, 0.0),
        )
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(386.0);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 7.0);
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    paint_icon(ui.painter(), rect, Icon::Drawer, accent);
                    ui.label(
                        egui::RichText::new("SKETCH EDITOR")
                            .monospace()
                            .size(11.0)
                            .strong()
                            .color(accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if toolbox_command_button(
                            ui,
                            Icon::Close,
                            "CLOSE",
                            false,
                            accent,
                            true,
                            "Close the drawer.",
                        ) {
                            result.toggle_picker = true;
                        }
                    });
                });
                crate::ui_kit::compact_separator(ui, theme);
                ui.label(
                    egui::RichText::new("ADVANCED WORKFLOWS")
                        .monospace()
                        .size(9.5)
                        .strong()
                        .color(colors.text_muted),
                );
                for group in workflow_drawer_groups() {
                    workflow_group_header(ui, group, colors.text_muted, accent);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                        for preset in group.presets {
                            if workflow_preset_chip(
                                ui,
                                *preset,
                                workflow_preset_selected(*preset, active_tool, active_workflow),
                            ) {
                                result.workflow_preset = Some(*preset);
                            }
                        }
                    });
                }
                if active_tool.uses_live_brush() {
                    crate::ui_kit::compact_separator(ui, theme);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                        for (label, size) in brush_presets() {
                            if brush_preset_chip(ui, label, size, brush) {
                                result.brush_preset = Some(size);
                            }
                        }
                    });
                }
                crate::ui_kit::compact_separator(ui, theme);
                material_catalog_panel(ui, active_block, theme, result);
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.drawer_hovered |= area.response.hovered();
}

fn draw_editor_hover_flyout(
    ctx: &egui::Context,
    hovered: ToolboxSelection,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    theme: crate::theme::ThemeSettings,
    accent: egui::Color32,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let group = context_group_for_selection(hovered);
    let open_t = ctx
        .animate_bool(
            egui::Id::new("voxel_native_sketch_editor_context_flyout"),
            true,
        )
        .clamp(0.0, 1.0);
    let frame = egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            colors.surface_strong.r(),
            colors.surface_strong.g(),
            colors.surface_strong.b(),
            210,
        ))
        .stroke(egui::Stroke::new(1.0, accent))
        .inner_margin(egui::Margin::symmetric(9.0, 8.0))
        .rounding(egui::Rounding::same(8.0))
        .shadow(egui::epaint::Shadow {
            offset: egui::vec2(0.0, 7.0),
            blur: 18.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(112),
        });

    let area = egui::Area::new(egui::Id::new(
        "voxel_native_sketch_editor_context_flyout_area",
    ))
    .anchor(
        egui::Align2::LEFT_CENTER,
        egui::vec2(92.0 + 12.0 * open_t, -34.0),
    )
    .order(egui::Order::Foreground)
    .show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(286.0);
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                paint_icon(ui.painter(), rect, group.icon, accent);
                ui.label(
                    egui::RichText::new(group.label)
                        .monospace()
                        .size(11.0)
                        .strong()
                        .color(accent),
                );
            });
            ui.label(
                egui::RichText::new(group.hint)
                    .monospace()
                    .size(9.0)
                    .color(colors.text_muted),
            );
            crate::ui_kit::compact_separator(ui, theme);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(5.0, 5.0);
                for item in group.items {
                    let selected = match *item {
                        ToolboxSelection::Tool(tool) => {
                            active_tool == tool && active_workflow.is_none()
                        }
                        ToolboxSelection::Workflow(preset) => {
                            workflow_preset_selected(preset, active_tool, active_workflow)
                        }
                    };
                    if toolbox_selection_chip(ui, *item, selected, accent) {
                        match *item {
                            ToolboxSelection::Tool(tool) => result.clicked_tool = Some(tool),
                            ToolboxSelection::Workflow(preset) => {
                                result.workflow_preset = Some(preset);
                            }
                        }
                    }
                }
            });
        });
    });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.drawer_hovered |= area.response.hovered();
}

fn editor_toolbox_separator(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(1.0), color.linear_multiply(0.75));
}

fn active_editor_workflow(
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> Option<BuildWorkflowPreset> {
    active_workflow.filter(|workflow| workflow.tool() == tool)
}

fn active_editor_label(
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> &'static str {
    active_editor_workflow(tool, active_workflow)
        .map(BuildWorkflowPreset::label)
        .unwrap_or_else(|| tool.chip_label())
}

fn active_editor_icon(tool: ToolbeltTool, active_workflow: Option<BuildWorkflowPreset>) -> Icon {
    active_editor_workflow(tool, active_workflow)
        .map(BuildWorkflowPreset::icon)
        .unwrap_or_else(|| tool.icon())
}

fn active_editor_color(
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> egui::Color32 {
    active_editor_workflow(tool, active_workflow)
        .map(BuildWorkflowPreset::color)
        .unwrap_or_else(|| tool.category_color())
}

fn active_editor_hint(tool: ToolbeltTool, active_workflow: Option<BuildWorkflowPreset>) -> String {
    active_editor_workflow(tool, active_workflow)
        .map(BuildWorkflowPreset::status)
        .unwrap_or_else(|| tool.hint().to_owned())
}

fn toolbox_selection_chip(
    ui: &mut egui::Ui,
    selection: ToolboxSelection,
    selected: bool,
    fallback: egui::Color32,
) -> bool {
    let (icon, label, base_color) = match selection {
        ToolboxSelection::Tool(tool) => {
            (tool.icon(), toolbox_tool_label(tool), tool.category_color())
        }
        ToolboxSelection::Workflow(preset) => (
            preset.icon(),
            workflow_toolbox_label(preset),
            preset.color(),
        ),
    };
    let color = if selected { AMBER } else { base_color };
    let stroke = if selected {
        AMBER
    } else {
        fallback.linear_multiply(0.75)
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(86.0, 34.0), egui::Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 72)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(base_color.r(), base_color.g(), base_color.b(), 40)
    } else {
        egui::Color32::from_rgba_unmultiplied(5, 20, 28, 168)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(6.0),
        fill,
        egui::Stroke::new(1.0, stroke),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 8.0), egui::vec2(16.0, 16.0)),
        icon,
        color,
    );
    painter.text(
        rect.right_center() - egui::vec2(7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        label,
        egui::FontId::monospace(8.0),
        TEXT,
    );
    response.clicked()
}

fn toolbox_tool_label(tool: ToolbeltTool) -> &'static str {
    match tool {
        ToolbeltTool::Navigate => "Select",
        ToolbeltTool::TransformMove => "Move",
        ToolbeltTool::TransformScale => "Scale",
        ToolbeltTool::TransformRotate => "Rotate",
        ToolbeltTool::MaterialPicker => "Paint",
        _ => tool.chip_label(),
    }
}

fn workflow_toolbox_label(preset: BuildWorkflowPreset) -> &'static str {
    match preset {
        BuildWorkflowPreset::Pencil => "Line",
        BuildWorkflowPreset::Sketch => "Rect",
        BuildWorkflowPreset::Circle => "Circle",
        BuildWorkflowPreset::Polygon => "Polygon",
        BuildWorkflowPreset::Arc => "Arc",
        BuildWorkflowPreset::Freehand => "Freehand",
        BuildWorkflowPreset::PushPull => "Push/Pull",
        BuildWorkflowPreset::Room => "Room",
        BuildWorkflowPreset::Opening => "Opening",
        BuildWorkflowPreset::Roads => "Road",
        BuildWorkflowPreset::BotArea => "Bots",
        BuildWorkflowPreset::CityShell => "City",
        BuildWorkflowPreset::ModernHouse => "House",
        BuildWorkflowPreset::Landscape => "Garden",
        BuildWorkflowPreset::Skyline => "Tower",
        BuildWorkflowPreset::Spacecraft => "Ship",
    }
}

fn toolbox_workflow_button(
    ui: &mut egui::Ui,
    preset: BuildWorkflowPreset,
    selected: bool,
) -> (bool, bool) {
    let color = preset.color();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(58.0, 41.0), egui::Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 74)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 42)
    } else {
        egui::Color32::from_rgba_unmultiplied(8, 22, 30, 174)
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
        egui::Rect::from_center_size(
            rect.center_top() + egui::vec2(-2.0, 12.0),
            egui::vec2(16.0, 16.0),
        ),
        preset.icon(),
        if selected { AMBER } else { color },
    );
    let cue = preset.inference_cue();
    let cue_rect = egui::Rect::from_min_size(
        rect.right_top() + egui::vec2(-19.0, 4.0),
        egui::vec2(14.0, 14.0),
    );
    painter.circle_filled(
        cue_rect.center(),
        7.5,
        egui::Color32::from_rgba_unmultiplied(0, 12, 18, 178),
    );
    paint_icon(
        &painter,
        cue_rect.shrink(2.0),
        cue.icon(),
        if selected { AMBER } else { color },
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 14.5),
        egui::Align2::CENTER_BOTTOM,
        workflow_toolbox_label(preset),
        egui::FontId::monospace(8.0),
        TEXT,
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 4.0),
        egui::Align2::CENTER_BOTTOM,
        cue.label(),
        egui::FontId::monospace(6.8),
        egui::Color32::from_white_alpha(150),
    );
    let clicked = response.clicked();
    let hovered = response.hovered();
    (clicked, hovered)
}

fn toolbox_tool_button(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    label: &'static str,
    selected: bool,
    primary: egui::Color32,
    dim: egui::Color32,
) -> (bool, bool) {
    let color = if selected { AMBER } else { primary };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(58.0, 41.0), egui::Sense::click());
    let hovered = response.hovered();
    let fill = if selected {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 70)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38)
    } else {
        egui::Color32::from_rgba_unmultiplied(6, 18, 24, 170)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(7.0),
        fill,
        egui::Stroke::new(1.0, if selected { AMBER } else { dim }),
    );
    paint_icon(
        &painter,
        egui::Rect::from_center_size(
            rect.center_top() + egui::vec2(0.0, 12.0),
            egui::vec2(16.0, 16.0),
        ),
        tool.icon(),
        if selected { AMBER } else { color },
    );
    painter.text(
        rect.center_bottom() - egui::vec2(0.0, 5.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(8.1),
        TEXT,
    );
    let clicked = response.clicked();
    let hovered = response.hovered();
    (clicked, hovered)
}

fn toolbox_command_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &'static str,
    selected: bool,
    color: egui::Color32,
    enabled: bool,
    hint: &'static str,
) -> bool {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(58.0, 32.0), sense);
    let hovered = response.hovered() && enabled;
    let visible = if enabled {
        color
    } else {
        color.linear_multiply(0.36)
    };
    let fill = if selected {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 72)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 42)
    } else {
        egui::Color32::from_rgba_unmultiplied(6, 18, 24, 168)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(7.0),
        fill,
        egui::Stroke::new(
            1.0,
            if selected {
                AMBER
            } else {
                visible.linear_multiply(0.8)
            },
        ),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(6.0, 9.0), egui::vec2(16.0, 16.0)),
        icon,
        visible,
    );
    painter.text(
        rect.right_center() - egui::vec2(5.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        label,
        egui::FontId::monospace(7.8),
        if enabled {
            TEXT
        } else {
            egui::Color32::from_white_alpha(86)
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

fn drawer_chip(ui: &mut egui::Ui, open: bool, primary: egui::Color32) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(78.0, 34.0), egui::Sense::click());
    let color = if open { AMBER } else { primary };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(5.0),
        egui::Color32::from_rgba_premultiplied(0, 8, 6, 180),
        egui::Stroke::new(1.0, color.linear_multiply(0.75)),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 8.0), egui::vec2(17.0, 17.0)),
        Icon::Drawer,
        color,
    );
    painter.text(
        rect.right_center() - egui::vec2(7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        if open { "CLOSE" } else { "STYLE" },
        egui::FontId::monospace(9.0),
        TEXT,
    );
    let clicked = response.clicked();
    response.on_hover_text("Open or close the style drawer.");
    clicked
}

fn selected_tool_badge(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    picker_open: bool,
    active_workflow: Option<BuildWorkflowPreset>,
    primary: egui::Color32,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(154.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let glass = egui::Color32::from_rgba_unmultiplied(12, 34, 45, 188);
    let sheen = egui::Color32::from_rgba_unmultiplied(220, 250, 255, 34);
    let color = active_editor_color(tool, active_workflow);
    painter.rect(
        rect,
        egui::Rounding::same(8.0),
        glass,
        egui::Stroke::new(1.0, color),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.right(), rect.center().y)),
        egui::Rounding::same(8.0),
        sheen,
    );
    let icon_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 7.0), egui::vec2(20.0, 20.0));
    paint_icon(
        &painter,
        icon_rect,
        active_editor_icon(tool, active_workflow),
        color,
    );
    painter.text(
        rect.min + egui::vec2(34.0, 9.0),
        egui::Align2::LEFT_CENTER,
        if picker_open { "DRAWER" } else { "EDITOR" },
        egui::FontId::monospace(9.5),
        AMBER,
    );
    painter.text(
        rect.min + egui::vec2(34.0, 23.0),
        egui::Align2::LEFT_CENTER,
        active_editor_label(tool, active_workflow),
        egui::FontId::monospace(11.5),
        primary,
    );
    response.on_hover_text(active_editor_hint(tool, active_workflow));
}

#[cfg(test)]
fn contextual_action_hints(
    tool: ToolbeltTool,
    picker_open: bool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> [Option<ToolActionHint>; 4] {
    if tool == ToolbeltTool::DrawRect && active_workflow == Some(BuildWorkflowPreset::Pencil) {
        return [
            Some(ToolActionHint::new(
                MouseGlyph::Left,
                "",
                "Pencil",
                Icon::Pipette,
                ActionTone::Tool,
                "Click endpoint to endpoint; after each line, the next line starts there.",
            )),
            Some(ToolActionHint::new(
                MouseGlyph::Right,
                "HOLD",
                "Orbit",
                Icon::ModeNavigate,
                ActionTone::Info,
                "Hold right mouse to orbit while the Pencil workflow stays armed.",
            )),
            Some(ToolActionHint::new(
                MouseGlyph::Left,
                "CTRL",
                "Cut",
                Icon::Eraser,
                ActionTone::Danger,
                "Switch to Opening for clean door/window cuts through wall thickness.",
            )),
            None,
        ];
    }

    if tool == ToolbeltTool::DrawRect && active_workflow == Some(BuildWorkflowPreset::Room) {
        return [
            Some(ToolActionHint::new(
                MouseGlyph::Left,
                "",
                "Hollow",
                Icon::Open,
                ActionTone::Warning,
                "Click two corners to carve a livable room volume behind the face.",
            )),
            Some(ToolActionHint::new(
                MouseGlyph::Right,
                "HOLD",
                "Orbit",
                Icon::ModeNavigate,
                ActionTone::Info,
                "Hold right mouse to orbit while the Room workflow stays armed.",
            )),
            Some(ToolActionHint::new(
                MouseGlyph::Left,
                "CTRL",
                "Opening",
                Icon::Eraser,
                ActionTone::Danger,
                "Switch to Opening for doors, windows, and exact through-cuts.",
            )),
            None,
        ];
    }

    if tool == ToolbeltTool::DrawRect && active_workflow == Some(BuildWorkflowPreset::Opening) {
        return [
            Some(ToolActionHint::new(
                MouseGlyph::Left,
                "",
                "Opening",
                Icon::Eraser,
                ActionTone::Danger,
                "Click two door/window corners on the locked face plane.",
            )),
            Some(ToolActionHint::new(
                MouseGlyph::Right,
                "HOLD",
                "Orbit",
                Icon::ModeNavigate,
                ActionTone::Info,
                "Hold right mouse to orbit while the Opening workflow stays armed.",
            )),
            None,
            None,
        ];
    }

    tool.action_hints(picker_open)
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseGlyph {
    Left,
    Right,
    Wheel,
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

fn workflow_group_header(
    ui: &mut egui::Ui,
    group: WorkflowDrawerGroup,
    muted: egui::Color32,
    accent: egui::Color32,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(366.0, 22.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        egui::Rounding::same(5.0),
        egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 4.0), egui::vec2(14.0, 14.0)),
        group.icon,
        accent,
    );
    painter.text(
        rect.min + egui::vec2(27.0, 11.0),
        egui::Align2::LEFT_CENTER,
        group.label,
        egui::FontId::monospace(9.5),
        TEXT,
    );
    painter.text(
        rect.right_center() - egui::vec2(7.0, 0.0),
        egui::Align2::RIGHT_CENTER,
        group.hint,
        egui::FontId::monospace(7.5),
        muted,
    );
    response.on_hover_text(group.hint);
}

fn workflow_preset_chip(ui: &mut egui::Ui, preset: BuildWorkflowPreset, selected: bool) -> bool {
    let color = preset.color();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(108.0, 42.0), egui::Sense::click());
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
    let cue = preset.inference_cue();
    let cue_rect = egui::Rect::from_min_size(
        rect.right_top() + egui::vec2(-22.0, 5.0),
        egui::vec2(15.0, 15.0),
    );
    painter.circle_filled(
        cue_rect.center(),
        8.0,
        egui::Color32::from_rgba_unmultiplied(0, 12, 18, 170),
    );
    paint_icon(
        &painter,
        cue_rect.shrink(2.0),
        cue.icon(),
        if selected { AMBER } else { color },
    );
    painter.text(
        rect.min + egui::vec2(32.0, 14.0),
        egui::Align2::LEFT_CENTER,
        workflow_toolbox_label(preset),
        egui::FontId::monospace(10.0),
        TEXT,
    );
    painter.text(
        rect.min + egui::vec2(32.0, 29.0),
        egui::Align2::LEFT_CENTER,
        cue.label(),
        egui::FontId::monospace(8.0),
        egui::Color32::from_white_alpha(152),
    );
    let clicked = response.clicked();
    response.on_hover_text(preset.inference_hover_text());
    clicked
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
        assert!(workflow_preset_selected(
            BuildWorkflowPreset::Sketch,
            toolbelt.tool,
            toolbelt.active_workflow
        ));
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
    fn room_workflow_action_cards_show_plain_left_mouse_hollowing() {
        let actions: Vec<ToolActionHint> = contextual_action_hints(
            ToolbeltTool::DrawRect,
            false,
            Some(BuildWorkflowPreset::Room),
        )
        .into_iter()
        .flatten()
        .collect();

        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier.is_empty()
            && a.label == "Hollow"
            && a.tone == ActionTone::Warning));
        assert!(actions
            .iter()
            .any(|a| a.glyph == MouseGlyph::Right && a.modifier == "HOLD" && a.label == "Orbit"));
        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier == "CTRL"
            && a.label == "Opening"
            && a.tone == ActionTone::Danger));
    }

    #[test]
    fn room_workflow_state_only_activates_in_live_sketch_draw() {
        let mut toolbelt = ToolbeltState::default();
        toolbelt.live = true;
        toolbelt.palette_open = false;
        toolbelt.select_workflow(BuildWorkflowPreset::Room);

        assert!(toolbelt.room_workflow_active());
        assert!(workflow_preset_selected(
            BuildWorkflowPreset::Room,
            toolbelt.tool,
            toolbelt.active_workflow
        ));

        toolbelt.tool = ToolbeltTool::Sculpt;
        toolbelt.sync_workflow_to_tool();
        assert!(!toolbelt.room_workflow_active());
        assert!(!workflow_preset_selected(
            BuildWorkflowPreset::Room,
            toolbelt.tool,
            toolbelt.active_workflow
        ));
    }

    #[test]
    fn pencil_workflow_action_cards_show_direct_line_drawing_and_orbit() {
        let actions: Vec<ToolActionHint> = contextual_action_hints(
            ToolbeltTool::DrawRect,
            false,
            Some(BuildWorkflowPreset::Pencil),
        )
        .into_iter()
        .flatten()
        .collect();

        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier.is_empty()
            && a.label == "Pencil"
            && a.tone == ActionTone::Tool));
        assert!(actions
            .iter()
            .any(|a| a.glyph == MouseGlyph::Right && a.modifier == "HOLD" && a.label == "Orbit"));
        assert!(actions.iter().any(|a| a.glyph == MouseGlyph::Left
            && a.modifier == "CTRL"
            && a.label == "Cut"
            && a.tone == ActionTone::Danger));
    }

    #[test]
    fn pencil_workflow_state_only_activates_in_live_sketch_draw() {
        let mut toolbelt = ToolbeltState::default();
        toolbelt.live = true;
        toolbelt.palette_open = false;
        toolbelt.select_workflow(BuildWorkflowPreset::Pencil);

        assert!(toolbelt.pencil_workflow_active());
        assert!(workflow_preset_selected(
            BuildWorkflowPreset::Pencil,
            toolbelt.tool,
            toolbelt.active_workflow
        ));

        toolbelt.palette_open = true;
        assert!(!toolbelt.pencil_workflow_active());

        toolbelt.palette_open = false;
        toolbelt.tool = ToolbeltTool::Sculpt;
        toolbelt.sync_workflow_to_tool();
        assert!(!toolbelt.pencil_workflow_active());
    }

    #[test]
    fn toolbox_selection_generation_tracks_mouse_workflow_changes() {
        let mut toolbelt = ToolbeltState::default();
        let initial = toolbelt.selection_generation();

        toolbelt.select_workflow(BuildWorkflowPreset::Pencil);
        let after_pencil = toolbelt.selection_generation();
        assert!(
            after_pencil > initial,
            "selecting Pencil from the toolbox must invalidate any active Rectangle preview"
        );

        toolbelt.select_workflow(BuildWorkflowPreset::Pencil);
        assert_eq!(
            toolbelt.selection_generation(),
            after_pencil,
            "clicking the already-active workflow should not cancel an in-progress line"
        );

        toolbelt.select_workflow(BuildWorkflowPreset::Sketch);
        assert!(
            toolbelt.selection_generation() > after_pencil,
            "switching Pencil to Rectangle uses the same DrawRect tool but must still cancel the active operation"
        );
    }

    #[test]
    fn toolbox_selection_updates_shared_tool_controller() {
        let mut toolbelt = ToolbeltState::default();
        let mut mode = ModeContext::default();
        let mut builder = BuilderState::default();
        let mut controller = crate::sketch_model::ToolController::default();

        apply_toolbox_selection(
            ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil),
            &mut toolbelt,
            &mut mode,
            &mut builder,
            &mut controller,
            crate::sketch_model::SketchId::new_for_test(4),
        );
        assert_eq!(
            controller.active_tool(),
            crate::sketch_model::EditorToolId::Pencil
        );

        apply_toolbox_selection(
            ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
            &mut toolbelt,
            &mut mode,
            &mut builder,
            &mut controller,
            crate::sketch_model::SketchId::new_for_test(4),
        );
        assert_eq!(
            controller.active_tool(),
            crate::sketch_model::EditorToolId::House
        );
        assert_eq!(
            controller.house_guide().map(|guide| guide.stage),
            Some(crate::sketch_model::HouseBuildStage::Footprint)
        );
        assert!(toolbelt.status.contains("Footprint"));
    }

    #[test]
    fn drafting_shape_workflows_route_through_shared_tool_controller() {
        let mut toolbelt = ToolbeltState::default();
        let mut mode = ModeContext::default();
        let mut builder = BuilderState::default();
        let mut controller = crate::sketch_model::ToolController::default();

        for (preset, expected) in [
            (
                BuildWorkflowPreset::Circle,
                crate::sketch_model::EditorToolId::Circle,
            ),
            (
                BuildWorkflowPreset::Polygon,
                crate::sketch_model::EditorToolId::Polygon,
            ),
            (
                BuildWorkflowPreset::Arc,
                crate::sketch_model::EditorToolId::Arc,
            ),
            (
                BuildWorkflowPreset::Freehand,
                crate::sketch_model::EditorToolId::Freehand,
            ),
        ] {
            apply_toolbox_selection(
                ToolboxSelection::Workflow(preset),
                &mut toolbelt,
                &mut mode,
                &mut builder,
                &mut controller,
                crate::sketch_model::SketchId::new_for_test(4),
            );
            assert_eq!(controller.active_tool(), expected);
            assert_eq!(toolbelt.active_workflow(), Some(preset));
        }
    }

    #[test]
    fn mouse_wheel_cycles_editor_tools_without_clicking() {
        assert_eq!(
            toolbox_wheel_selection(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Pencil),
                -1.0
            ),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch))
        );
        assert_eq!(
            toolbox_wheel_selection(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Sketch),
                1.0
            ),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil))
        );
        assert_eq!(
            toolbox_wheel_selection(ToolbeltTool::Navigate, None, -1.0),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil))
        );
    }

    #[test]
    fn mouse_wheel_wraps_and_includes_extended_editor_workflows() {
        assert_eq!(
            toolbox_wheel_selection(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Spacecraft),
                -1.0
            ),
            Some(ToolboxSelection::Tool(ToolbeltTool::Navigate))
        );
        assert_eq!(
            toolbox_wheel_selection(ToolbeltTool::Navigate, None, 1.0),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Spacecraft))
        );
        assert_eq!(
            toolbox_wheel_selection(
                ToolbeltTool::CityBuilding,
                Some(BuildWorkflowPreset::CityShell),
                -1.0
            ),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Landscape))
        );
        assert_eq!(
            toolbox_wheel_selection(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Pencil),
                0.2
            ),
            None
        );
    }

    #[test]
    fn mouse_wheel_does_not_switch_tools_in_world_drawing_area() {
        assert_eq!(
            toolbox_wheel_selection_from_zone(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Pencil),
                -1.0,
                false,
            ),
            None,
            "wheel movement over the 3D world must stay available for navigation and tool-specific editing"
        );
    }

    #[test]
    fn mouse_wheel_cycles_tools_when_hovering_editor_ui() {
        assert_eq!(
            toolbox_wheel_selection_from_zone(
                ToolbeltTool::DrawRect,
                Some(BuildWorkflowPreset::Pencil),
                -1.0,
                true,
            ),
            Some(ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch))
        );
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
        assert!(hint.contains("Click road endpoints"));
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
    fn city_area_action_hints_explain_two_point_bot_zone() {
        let actions: Vec<ToolActionHint> = ToolbeltTool::CityDistrict
            .action_hints(false)
            .into_iter()
            .flatten()
            .collect();
        let area = actions
            .iter()
            .find(|a| a.label == "Area")
            .expect("area action");

        assert_eq!(area.glyph, MouseGlyph::Left);
        assert_eq!(area.modifier, "2-PT");
        assert!(area.hint.contains("Click two corners"));
        assert!(ToolbeltTool::CityDistrict
            .hint()
            .contains("Click two snapped corners"));
    }

    #[test]
    fn building_shell_action_hints_explain_two_point_footprint() {
        let actions: Vec<ToolActionHint> = ToolbeltTool::CityBuilding
            .action_hints(false)
            .into_iter()
            .flatten()
            .collect();
        let shell = actions
            .iter()
            .find(|a| a.label == "Shell")
            .expect("shell action");

        assert_eq!(shell.glyph, MouseGlyph::Left);
        assert_eq!(shell.modifier, "2-PT");
        assert!(shell.hint.contains("Click two corners"));
        assert!(ToolbeltTool::CityBuilding
            .hint()
            .contains("Click two snapped corners"));
    }

    #[test]
    fn workflow_presets_collapse_multi_step_builder_modes() {
        assert_eq!(BuildWorkflowPreset::Pencil.tool(), ToolbeltTool::DrawRect);
        assert_eq!(BuildWorkflowPreset::Pencil.label(), "PENCIL");
        assert_eq!(BuildWorkflowPreset::Sketch.label(), "RECTANGLE");
        assert_eq!(
            BuildWorkflowPreset::Pencil.brush(),
            Some(IVec3::new(1, 1, 1))
        );
        assert!(BuildWorkflowPreset::Pencil.status().contains("lines chain"));
        assert_eq!(BuildWorkflowPreset::Sketch.tool(), ToolbeltTool::DrawRect);
        assert_eq!(
            BuildWorkflowPreset::Sketch.brush(),
            Some(IVec3::new(4, 1, 1))
        );
        assert_eq!(BuildWorkflowPreset::Opening.tool(), ToolbeltTool::DrawRect);
        assert_eq!(BuildWorkflowPreset::Opening.label(), "OPENING");
        assert!(BuildWorkflowPreset::Opening
            .status()
            .contains("door/window"));
        assert!(!BuildWorkflowPreset::Opening.status().contains("Ctrl+LMB"));
        assert_eq!(BuildWorkflowPreset::Room.tool(), ToolbeltTool::DrawRect);
        assert!(BuildWorkflowPreset::Room
            .status()
            .contains("click two snapped"));
        assert!(!BuildWorkflowPreset::Room.status().contains("Shift+LMB"));
        assert_eq!(BuildWorkflowPreset::Roads.tool(), ToolbeltTool::CityRoad);
        assert!(BuildWorkflowPreset::Roads.status().contains("branch snap"));
        assert_eq!(
            BuildWorkflowPreset::BotArea.tool(),
            ToolbeltTool::CityDistrict
        );
        assert!(BuildWorkflowPreset::BotArea
            .status()
            .contains("click two snapped"));
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
    fn editor_toolbox_exposes_mouse_first_core_actions() {
        assert_eq!(
            BuildWorkflowPreset::TOOLBOX,
            [
                BuildWorkflowPreset::Pencil,
                BuildWorkflowPreset::Sketch,
                BuildWorkflowPreset::Circle,
                BuildWorkflowPreset::PushPull,
                BuildWorkflowPreset::Opening,
                BuildWorkflowPreset::Room,
                BuildWorkflowPreset::Roads,
                BuildWorkflowPreset::BotArea,
                BuildWorkflowPreset::ModernHouse,
            ]
        );
    }

    #[test]
    fn workflow_toolbox_labels_are_plain_language_not_internal_codes() {
        assert_eq!(workflow_toolbox_label(BuildWorkflowPreset::Pencil), "Line");
        assert_eq!(workflow_toolbox_label(BuildWorkflowPreset::Sketch), "Rect");
        assert_eq!(
            workflow_toolbox_label(BuildWorkflowPreset::PushPull),
            "Push/Pull"
        );
        assert_eq!(
            workflow_toolbox_label(BuildWorkflowPreset::Opening),
            "Opening"
        );
        assert_eq!(toolbox_tool_label(ToolbeltTool::MaterialPicker), "Paint");
        assert_eq!(workflow_toolbox_label(BuildWorkflowPreset::BotArea), "Bots");
    }

    #[test]
    fn workflow_presets_expose_inference_cues_for_icons_and_hover_help() {
        assert_eq!(
            BuildWorkflowPreset::Pencil.inference_cue(),
            InferenceCue::Point
        );
        assert_eq!(
            BuildWorkflowPreset::Sketch.inference_cue(),
            InferenceCue::Corner
        );
        assert_eq!(
            BuildWorkflowPreset::PushPull.inference_cue(),
            InferenceCue::Face
        );
        assert_eq!(
            BuildWorkflowPreset::Roads.inference_cue(),
            InferenceCue::Path
        );
        assert!(BuildWorkflowPreset::Opening
            .inference_hover_text()
            .contains("Inference: Face"));
    }

    #[test]
    fn workflow_drawer_groups_keep_subtools_sorted_under_hover_sections() {
        let groups = workflow_drawer_groups();

        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].label, "Draw");
        assert!(groups[0].presets.contains(&BuildWorkflowPreset::Circle));
        assert_eq!(groups[1].label, "Shape");
        assert!(groups[1].presets.contains(&BuildWorkflowPreset::PushPull));
        assert_eq!(groups[2].label, "World");
        assert!(groups[2].presets.contains(&BuildWorkflowPreset::Spacecraft));
    }

    #[test]
    fn hover_drawer_grace_keeps_panel_open_across_toolbox_gap() {
        let state = next_hover_drawer_state(false, false, false, false, true, 0.18, 0.05);

        assert!(state.open);
        assert!(state.grace_remaining > 0.12);
    }

    #[test]
    fn hover_drawer_grace_is_long_enough_for_human_mouse_travel() {
        let state = next_hover_drawer_state(
            false,
            false,
            false,
            false,
            true,
            HOVER_DRAWER_GRACE_SECONDS,
            0.50,
        );

        assert!(state.open);
        assert!(state.grace_remaining > 0.30);
    }

    #[test]
    fn hover_drawer_reuses_retained_group_while_pointer_crosses_gap() {
        let retained = ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse);
        let selected = hover_drawer_selection(None, Some(retained), ToolbeltTool::Navigate, None);

        assert_eq!(selected, retained);
    }

    #[test]
    fn primary_editor_order_keeps_only_high_value_mouse_first_tools() {
        assert_eq!(
            PRIMARY_TOOLBOX_ITEMS,
            [
                ToolboxSelection::Tool(ToolbeltTool::Navigate),
                ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil),
                ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
                ToolboxSelection::Workflow(BuildWorkflowPreset::Circle),
                ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
                ToolboxSelection::Tool(ToolbeltTool::TransformMove),
                ToolboxSelection::Tool(ToolbeltTool::TransformRotate),
                ToolboxSelection::Tool(ToolbeltTool::TransformScale),
                ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
            ]
        );
    }

    #[test]
    fn expanded_editor_order_starts_with_the_primary_toolbar() {
        assert_eq!(
            &ToolboxSelection::ORDER[..PRIMARY_TOOLBOX_ITEMS.len()],
            PRIMARY_TOOLBOX_ITEMS
        );
    }

    #[test]
    fn voxel_specific_workflows_stay_in_contextual_flyouts_not_primary_rail() {
        for advanced in [
            ToolboxSelection::Workflow(BuildWorkflowPreset::Opening),
            ToolboxSelection::Workflow(BuildWorkflowPreset::Room),
            ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
            ToolboxSelection::Workflow(BuildWorkflowPreset::Roads),
            ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea),
        ] {
            assert!(
                !PRIMARY_TOOLBOX_ITEMS.contains(&advanced),
                "{advanced:?} should be available through hover drawers, not the primary rail"
            );
        }

        let house = context_group_for_selection(ToolboxSelection::Workflow(
            BuildWorkflowPreset::ModernHouse,
        ));
        assert!(house
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::Opening)));
        let city =
            context_group_for_selection(ToolboxSelection::Workflow(BuildWorkflowPreset::Roads));
        assert!(city
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea)));
    }

    #[test]
    fn transform_toolbox_selection_routes_to_editor_tools() {
        assert_eq!(
            toolbox_selection_editor_tool(ToolboxSelection::Tool(ToolbeltTool::TransformMove)),
            crate::sketch_model::EditorToolId::Move
        );
        assert_eq!(
            toolbox_selection_editor_tool(ToolboxSelection::Tool(ToolbeltTool::TransformScale)),
            crate::sketch_model::EditorToolId::Scale
        );
        assert_eq!(
            toolbox_selection_editor_tool(ToolboxSelection::Tool(ToolbeltTool::TransformRotate)),
            crate::sketch_model::EditorToolId::Rotate
        );
    }

    #[test]
    fn hover_surface_is_compact_not_full_style_drawer() {
        assert_eq!(
            editor_drawer_surface(false, false),
            EditorDrawerSurface::Hidden
        );
        assert_eq!(
            editor_drawer_surface(false, true),
            EditorDrawerSurface::HoverFlyout
        );
        assert_eq!(
            editor_drawer_surface(true, true),
            EditorDrawerSurface::FullDrawer
        );
    }

    #[test]
    fn contextual_hover_groups_do_not_mix_unrelated_tools() {
        let edit = context_group_for_selection(ToolboxSelection::Tool(ToolbeltTool::TransformMove));
        assert_eq!(edit.label, "Edit Selected");
        assert!(edit
            .items
            .contains(&ToolboxSelection::Tool(ToolbeltTool::TransformScale)));
        assert!(edit
            .items
            .contains(&ToolboxSelection::Tool(ToolbeltTool::TransformRotate)));
        assert!(!edit
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::Roads)));

        let draw =
            context_group_for_selection(ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil));
        assert_eq!(draw.label, "Draw");
        assert!(draw.items.len() <= 6);
        assert!(!draw
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea)));

        let house = context_group_for_selection(ToolboxSelection::Workflow(
            BuildWorkflowPreset::ModernHouse,
        ));
        assert_eq!(house.label, "House Builder");
        assert!(house
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull)));
        assert!(house
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::Opening)));
        assert!(!house
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea)));
    }

    #[test]
    fn active_editor_badge_names_workflow_not_internal_tool() {
        assert_eq!(
            active_editor_label(ToolbeltTool::DrawRect, Some(BuildWorkflowPreset::Pencil)),
            "PENCIL"
        );
        assert_eq!(
            active_editor_label(ToolbeltTool::DrawRect, Some(BuildWorkflowPreset::Sketch)),
            "RECTANGLE"
        );
        assert_eq!(
            active_editor_label(ToolbeltTool::Sculpt, Some(BuildWorkflowPreset::PushPull)),
            "PUSH/PULL"
        );
        assert_eq!(active_editor_label(ToolbeltTool::CityRoad, None), "ROAD");
    }

    #[test]
    fn house_workflow_status_overrides_generic_sketch_draw_hint() {
        let status = compact_status(
            "Sketch Draw ready. Click start, move, click finish; toolbox switches Room/Opening.",
            ToolbeltTool::DrawRect,
            Some(BuildWorkflowPreset::ModernHouse),
        );

        assert!(status.starts_with("HOUSE: Footprint"));
        assert!(status.contains("Push/Pull"));
        assert!(!status.contains("Sketch Draw ready"));
    }

    #[test]
    fn compact_status_uses_shared_tool_lifecycle_preview_hint() {
        let mut controller = crate::sketch_model::ToolController::default();
        controller.activate(crate::sketch_model::EditorToolId::Pencil);
        controller.begin_transaction("Pencil line");

        let status = compact_status_for_controller(
            "Sketch Draw ready. Click start, move, click finish; toolbox switches Room/Opening.",
            ToolbeltTool::DrawRect,
            Some(BuildWorkflowPreset::Pencil),
            &controller,
        );

        assert!(status.starts_with("PENCIL:"));
        assert!(status.contains("snapped endpoint"));
        assert!(!status.contains("Sketch Draw ready"));
    }

    #[test]
    fn workflow_presets_pick_architecture_materials() {
        assert_eq!(
            BuildWorkflowPreset::ModernHouse.tool(),
            ToolbeltTool::DrawRect
        );
        assert!(BuildWorkflowPreset::ModernHouse
            .status()
            .contains("Footprint"));
        assert!(BuildWorkflowPreset::ModernHouse
            .status()
            .contains("Push/Pull"));
        assert!(BuildWorkflowPreset::ModernHouse
            .status()
            .contains("Opening"));
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
