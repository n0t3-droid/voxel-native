//! Compact in-game Sketch Editor for mouse-look building.
//!
//! The editor is mouse-first: pick a workflow from the toolbox, then keep
//! moving/flying while LMB/RMB works directly in the world. Weapons are
//! holstered for the whole edit state, including the drawer.

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::blocks::{block_label, BlockType};
use crate::builder::{BuilderHistory, BuilderState};
use crate::city::CityTool;
use crate::creator_library::{
    apply_creator_library_action, draw_creator_library, CreatorLibraryAction, CreatorLibraryEffect,
    CreatorLibraryState,
};
use crate::icons::{paint_icon, Icon};
use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::settings::WorldSettings;
use crate::ships::{ShipInventory, ShipPlacementState};
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
    #[cfg(test)]
    pub(crate) const ALL: [Self; 15] = [
        Self::Navigate,
        Self::DrawRect,
        Self::Sculpt,
        Self::TransformMove,
        Self::TransformScale,
        Self::TransformRotate,
        Self::MaterialPicker,
        Self::SmartTower,
        Self::BrushPlace,
        Self::BrushCut,
        Self::CityRoad,
        Self::CityDistrict,
        Self::CityBuilding,
        Self::CityFacade,
        Self::AnimationPick,
    ];

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

    pub(crate) fn select_tool(&mut self, tool: ToolbeltTool) {
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

    pub(crate) fn select_workflow(&mut self, preset: BuildWorkflowPreset) {
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
    hover_drawer_group: Option<ToolboxGroupId>,
}

impl Plugin for ToolbeltPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ToolbeltState::default())
            .insert_resource(SketchEditorUiFocus::default())
            .add_systems(Update, draw_toolbelt.run_if(in_state(GameState::InGame)));
    }
}

#[derive(SystemParam)]
struct CreatorLibraryParams<'w> {
    library: ResMut<'w, CreatorLibraryState>,
    ships: ResMut<'w, ShipInventory>,
    placement: ResMut<'w, ShipPlacementState>,
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
    mut sketch_doc: ResMut<crate::sketch_model::SketchDocument>,
    mut sketch_links: ResMut<crate::sketch_model::SketchVoxelLinkIndex>,
    mut tool_controller: ResMut<crate::sketch_model::ToolController>,
    mut semantic_hover: ResMut<crate::sketch_model::SemanticHoverHit>,
    mut wheel: EventReader<MouseWheel>,
    mut creator: CreatorLibraryParams,
) {
    if !mode.is_build() {
        ui_focus.pointer_over_editor_ui = false;
        ui_focus.hover_drawer_open = false;
        ui_focus.hover_drawer_grace_remaining = 0.0;
        ui_focus.hover_drawer_group = None;
        wheel.clear();
        return;
    }

    let Some(ctx) = contexts.try_ctx_mut() else {
        ui_focus.pointer_over_editor_ui = false;
        wheel.clear();
        return;
    };
    let theme = settings.theme;
    let primary = theme.color.primary();
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
        tool_controller.tool_phase(),
        ui_focus.hover_drawer_group,
        theme,
        primary,
        &mut creator.library,
        &creator.ships,
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
    if let Some(group) = dock.hovered_group {
        ui_focus.hover_drawer_group = Some(group);
    } else if !hover_state.open {
        ui_focus.hover_drawer_group = None;
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
    let library_action = dock
        .creator_action
        .or_else(|| dock.block_choice.map(CreatorLibraryAction::SelectMaterial));
    if let Some(action) = library_action {
        let effect = apply_creator_library_action(
            action,
            &mut creator.library,
            &mut builder,
            &mut creator.ships,
            &mut creator.placement,
            &mut mode,
        );
        match effect {
            CreatorLibraryEffect::MaterialSelected => {
                builder.status = creator.library.status.clone();
                toolbelt.status = builder.status.clone();
                mode.status = builder.status.clone();
            }
            CreatorLibraryEffect::PlacementStarted => {
                toolbelt.status = mode.status.clone();
            }
        }
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
            HistoryCommand::Undo => {
                history.undo_with_sketch(&mut world, &mut sketch_doc, &mut sketch_links)
            }
            HistoryCommand::Redo => {
                history.redo_with_sketch(&mut world, &mut sketch_doc, &mut sketch_links)
            }
        };
        let changed_history = result.as_ref().is_ok_and(|step| step.is_some());
        let status = match &result {
            Err(step) => format!(
                "{} '{}' blocked: history mismatch; world left unchanged.",
                command.label(),
                step.label
            ),
            Ok(step) => format_history_command_status(
                command,
                step.as_ref().map(|step| step.label_and_voxels()),
            ),
        };
        if changed_history {
            clear_editor_selection_after_toolbelt_history_step(
                &mut tool_controller,
                &mut semantic_hover,
            );
        }
        toolbelt.status = status.clone();
        mode.status = status;
    }
}

fn clear_editor_selection_after_toolbelt_history_step(
    tool_controller: &mut crate::sketch_model::ToolController,
    semantic_hover: &mut crate::sketch_model::SemanticHoverHit,
) {
    let _ = tool_controller.clear_selection_context();
    semantic_hover.0 = None;
}

impl ToolbeltTool {
    pub(crate) fn uses_live_brush(self) -> bool {
        matches!(self, Self::BrushPlace | Self::BrushCut)
    }
}

fn compact_status(
    status: &str,
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> String {
    if active_workflow == Some(BuildWorkflowPreset::ModernHouse) && tool == ToolbeltTool::DrawRect {
        return "House ready. Draw the footprint, pull the walls, cut openings, then hollow the room.".to_owned();
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
        crate::sketch_model::EditorToolPhase::Previewing => {
            let lifecycle_status = format!(
                "{} in progress. {}",
                active_editor_label(tool, active_workflow),
                tool_controller.active_tool_hint()
            );
            compact_single_line_status(&lifecycle_status)
        }
        crate::sketch_model::EditorToolPhase::Committed => format!(
            "{} applied. Continue in the world or choose another tool.",
            active_editor_label(tool, active_workflow)
        ),
        crate::sketch_model::EditorToolPhase::Cancelled => format!(
            "{} cancelled. Click in the world to start again.",
            active_editor_label(tool, active_workflow)
        ),
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
    hovered_group: Option<ToolboxGroupId>,
    toolbox_rect: Option<egui::Rect>,
    drawer_rect: Option<egui::Rect>,
    toggle_picker: bool,
    exit_editor: bool,
    brush_preset: Option<IVec3>,
    workflow_preset: Option<BuildWorkflowPreset>,
    block_choice: Option<BlockType>,
    creator_action: Option<CreatorLibraryAction>,
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

fn hover_drawer_bridge_rect(toolbox: egui::Rect, drawer: egui::Rect) -> egui::Rect {
    let rail_edge = toolbox.right();
    let drawer_edge = drawer.left();
    egui::Rect::from_min_max(
        egui::pos2(
            rail_edge.min(drawer_edge) - 4.0,
            toolbox.top().min(drawer.top()) - 4.0,
        ),
        egui::pos2(
            rail_edge.max(drawer_edge) + 4.0,
            toolbox.bottom().max(drawer.bottom()) + 4.0,
        ),
    )
}

fn hover_drawer_bridge_hovered(
    ctx: &egui::Context,
    enabled: bool,
    toolbox: Option<egui::Rect>,
    drawer: Option<egui::Rect>,
) -> bool {
    if !enabled {
        return false;
    }
    let (Some(toolbox), Some(drawer)) = (toolbox, drawer) else {
        return false;
    };
    let Some(pointer) = ctx.pointer_hover_pos() else {
        return false;
    };
    hover_drawer_bridge_rect(toolbox, drawer).contains(pointer)
}

/// One semantic editor choice, shared by the visible toolbox and deterministic
/// automation/QA inputs. Keeping both paths on the same type prevents a tool
/// from looking selected while a different controller owns the world cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolboxSelection {
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

    pub(crate) fn tool(self) -> ToolbeltTool {
        match self {
            Self::Tool(tool) => tool,
            Self::Workflow(preset) => preset.tool(),
        }
    }

    pub(crate) fn editor_tool(self) -> crate::sketch_model::EditorToolId {
        toolbox_selection_editor_tool(self)
    }

    pub(crate) fn live_status(self) -> String {
        match self {
            Self::Tool(tool) => format!("Sketch Editor: {}. {}", tool.label(), tool.hint()),
            Self::Workflow(preset) => preset.status(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolboxGroupId {
    Core,
    Draw,
    Shape,
    Transform,
    World,
}

#[derive(Debug, Clone, Copy)]
struct ToolboxGroup {
    id: ToolboxGroupId,
    label: &'static str,
    hint: &'static str,
    icon: Icon,
    primary: ToolboxSelection,
    items: &'static [ToolboxSelection],
}

const CORE_TOOLBOX_ITEMS: [ToolboxSelection; 2] = [
    ToolboxSelection::Tool(ToolbeltTool::Navigate),
    ToolboxSelection::Tool(ToolbeltTool::MaterialPicker),
];

const DRAW_TOOLBOX_ITEMS: [ToolboxSelection; 6] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Pencil),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Circle),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Polygon),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Arc),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Freehand),
];

const SHAPE_TOOLBOX_ITEMS: [ToolboxSelection; 4] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Opening),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Room),
    ToolboxSelection::Workflow(BuildWorkflowPreset::ModernHouse),
];

const TRANSFORM_TOOLBOX_ITEMS: [ToolboxSelection; 3] = [
    ToolboxSelection::Tool(ToolbeltTool::TransformMove),
    ToolboxSelection::Tool(ToolbeltTool::TransformRotate),
    ToolboxSelection::Tool(ToolbeltTool::TransformScale),
];

const WORLD_TOOLBOX_ITEMS: [ToolboxSelection; 6] = [
    ToolboxSelection::Workflow(BuildWorkflowPreset::Roads),
    ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea),
    ToolboxSelection::Workflow(BuildWorkflowPreset::CityShell),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Landscape),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Skyline),
    ToolboxSelection::Workflow(BuildWorkflowPreset::Spacecraft),
];

const TOOLBOX_GROUP_IDS: [ToolboxGroupId; 5] = [
    ToolboxGroupId::Core,
    ToolboxGroupId::Draw,
    ToolboxGroupId::Shape,
    ToolboxGroupId::Transform,
    ToolboxGroupId::World,
];

fn toolbox_group(id: ToolboxGroupId) -> ToolboxGroup {
    match id {
        ToolboxGroupId::Core => ToolboxGroup {
            id,
            label: "Core",
            hint: "Select geometry or apply material styling.",
            icon: Icon::ModeNavigate,
            primary: ToolboxSelection::Tool(ToolbeltTool::Navigate),
            items: &CORE_TOOLBOX_ITEMS,
        },
        ToolboxGroupId::Draw => ToolboxGroup {
            id,
            label: "Draw",
            hint: "Create lines and planar shapes from snapped points.",
            icon: Icon::Grid,
            primary: ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch),
            items: &DRAW_TOOLBOX_ITEMS,
        },
        ToolboxGroupId::Shape => ToolboxGroup {
            id,
            label: "Shape",
            hint: "Pull faces, cut openings, make rooms, or guide a house.",
            icon: Icon::Builder,
            primary: ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull),
            items: &SHAPE_TOOLBOX_ITEMS,
        },
        ToolboxGroupId::Transform => ToolboxGroup {
            id,
            label: "Transform",
            hint: "Move, rotate, or scale selected geometry.",
            icon: Icon::Move,
            primary: ToolboxSelection::Tool(ToolbeltTool::TransformMove),
            items: &TRANSFORM_TOOLBOX_ITEMS,
        },
        ToolboxGroupId::World => ToolboxGroup {
            id,
            label: "World",
            hint: "Lay out roads, areas, buildings, landscape, and landmarks.",
            icon: Icon::City,
            primary: ToolboxSelection::Workflow(BuildWorkflowPreset::Roads),
            items: &WORLD_TOOLBOX_ITEMS,
        },
    }
}

fn toolbox_group_for_selection(selection: ToolboxSelection) -> ToolboxGroupId {
    match selection {
        ToolboxSelection::Tool(ToolbeltTool::Navigate | ToolbeltTool::MaterialPicker) => {
            ToolboxGroupId::Core
        }
        ToolboxSelection::Tool(ToolbeltTool::DrawRect)
        | ToolboxSelection::Workflow(
            BuildWorkflowPreset::Pencil
            | BuildWorkflowPreset::Sketch
            | BuildWorkflowPreset::Circle
            | BuildWorkflowPreset::Polygon
            | BuildWorkflowPreset::Arc
            | BuildWorkflowPreset::Freehand,
        ) => ToolboxGroupId::Draw,
        ToolboxSelection::Tool(ToolbeltTool::Sculpt)
        | ToolboxSelection::Workflow(
            BuildWorkflowPreset::PushPull
            | BuildWorkflowPreset::Opening
            | BuildWorkflowPreset::Room
            | BuildWorkflowPreset::ModernHouse,
        ) => ToolboxGroupId::Shape,
        ToolboxSelection::Tool(
            ToolbeltTool::TransformMove
            | ToolbeltTool::TransformRotate
            | ToolbeltTool::TransformScale,
        ) => ToolboxGroupId::Transform,
        ToolboxSelection::Tool(
            ToolbeltTool::SmartTower
            | ToolbeltTool::BrushPlace
            | ToolbeltTool::BrushCut
            | ToolbeltTool::CityRoad
            | ToolbeltTool::CityDistrict
            | ToolbeltTool::CityBuilding
            | ToolbeltTool::CityFacade
            | ToolbeltTool::AnimationPick,
        )
        | ToolboxSelection::Workflow(
            BuildWorkflowPreset::Roads
            | BuildWorkflowPreset::BotArea
            | BuildWorkflowPreset::CityShell
            | BuildWorkflowPreset::Landscape
            | BuildWorkflowPreset::Skyline
            | BuildWorkflowPreset::Spacecraft,
        ) => ToolboxGroupId::World,
    }
}

fn toolbox_group_color(id: ToolboxGroupId, theme: crate::theme::ThemeSettings) -> egui::Color32 {
    let colors = theme.semantic();
    match id {
        ToolboxGroupId::Core => colors.text_muted,
        ToolboxGroupId::Draw => colors.info,
        ToolboxGroupId::Shape => colors.warning,
        ToolboxGroupId::Transform => colors.accent,
        ToolboxGroupId::World => colors.success,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryCommand {
    Undo,
    Redo,
}

impl HistoryCommand {
    fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
        }
    }
}

#[cfg(test)]
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

#[cfg(test)]
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

    pub(crate) fn tool(self) -> ToolbeltTool {
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

    pub(crate) fn status(self) -> String {
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

    #[cfg(test)]
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

    #[cfg(test)]
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

fn active_toolbox_group(
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> ToolboxGroupId {
    let selection = active_workflow
        .map(ToolboxSelection::Workflow)
        .unwrap_or(ToolboxSelection::Tool(active_tool));
    toolbox_group_for_selection(selection)
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

pub(crate) fn apply_toolbox_selection(
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
        let editor_tool = toolbox_selection_editor_tool(selection);
        if tool_controller.active_tool() == editor_tool {
            tool_controller.restart_active_tool();
        } else {
            tool_controller.activate(editor_tool);
        }
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

pub(crate) fn editor_tool_for_tool(tool: ToolbeltTool) -> crate::sketch_model::EditorToolId {
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

pub(crate) fn editor_tool_for_workflow(
    preset: BuildWorkflowPreset,
) -> crate::sketch_model::EditorToolId {
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
    tool_phase: crate::sketch_model::EditorToolPhase,
    retained_hover_group: Option<ToolboxGroupId>,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    creator_library: &mut CreatorLibraryState,
    ships: &ShipInventory,
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
        &mut result,
    );
    let hover_visible = picker_open || hover_drawer_open || result.toolbox_hovered;
    let surface = editor_drawer_surface(picker_open, hover_visible);
    draw_editor_status_bar(
        ctx,
        active_tool,
        active_workflow,
        picker_open,
        status,
        active_block,
        brush,
        tool_phase,
        theme,
        primary,
        &mut result,
    );
    match surface {
        EditorDrawerSurface::Hidden => {}
        EditorDrawerSurface::HoverFlyout => {
            let group = hover_drawer_group(
                result.hovered_group,
                retained_hover_group,
                active_tool,
                active_workflow,
            );
            draw_editor_hover_flyout(ctx, group, active_tool, active_workflow, theme, &mut result);
        }
        EditorDrawerSurface::FullDrawer => {
            draw_editor_drawer(
                ctx,
                active_tool,
                active_block,
                brush,
                theme,
                colors.info,
                creator_library,
                ships,
                &mut result,
            );
        }
    }

    result.hover_bridge_hovered =
        hover_drawer_bridge_hovered(ctx, hover_visible, result.toolbox_rect, result.drawer_rect);

    result
}

fn hover_drawer_group(
    current_hover: Option<ToolboxGroupId>,
    retained_hover: Option<ToolboxGroupId>,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> ToolboxGroupId {
    current_hover
        .or(retained_hover)
        .unwrap_or_else(|| active_toolbox_group(active_tool, active_workflow))
}

fn draw_editor_toolbox(
    ctx: &egui::Context,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    picker_open: bool,
    undo_count: usize,
    redo_count: usize,
    theme: crate::theme::ThemeSettings,
    result: &mut BuildDockResult,
) {
    let frame = editor_toolbox_frame(theme);
    let active_group = active_toolbox_group(active_tool, active_workflow);

    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_toolbox"))
        .anchor(egui::Align2::LEFT_CENTER, egui::vec2(14.0, 0.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(crate::theme::KANSO_LAYOUT.icon_square_size);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 5.0);
                ui.vertical_centered(|ui| {
                    for id in TOOLBOX_GROUP_IDS {
                        let group = toolbox_group(id);
                        let response = toolbox_group_button(ui, group, active_group == id, theme);
                        if response.hovered() {
                            result.hovered_group = Some(id);
                            result.toolbox_hovered = true;
                        }
                        if response.clicked() {
                            set_toolbox_result_selection(result, group.primary);
                        }
                    }
                    crate::ui_kit::compact_separator(ui, theme);
                    if toolbox_icon_command(
                        ui,
                        Icon::Textures,
                        picker_open,
                        true,
                        theme,
                        "Open style and materials.",
                    ) {
                        result.toggle_picker = true;
                    }
                    if toolbox_icon_command(
                        ui,
                        Icon::Undo,
                        false,
                        undo_count > 0,
                        theme,
                        "Undo the last build edit.",
                    ) {
                        result.history_command = Some(HistoryCommand::Undo);
                    }
                    if toolbox_icon_command(
                        ui,
                        Icon::Redo,
                        false,
                        redo_count > 0,
                        theme,
                        "Redo the last undone build edit.",
                    ) {
                        result.history_command = Some(HistoryCommand::Redo);
                    }
                    if toolbox_icon_command(
                        ui,
                        Icon::Play,
                        false,
                        true,
                        theme,
                        "Exit Sketch Editor and return to play mode.",
                    ) {
                        result.exit_editor = true;
                    }
                });
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.toolbox_rect = Some(area.response.rect);
}

fn toolbox_group_button(
    ui: &mut egui::Ui,
    group: ToolboxGroup,
    selected: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    let tooltip = format!(
        "{} tools. {} Click to activate {}.",
        group.label,
        group.hint,
        toolbox_selection_label(group.primary)
    );
    let response = crate::ui_kit::icon_square(ui, group.icon, selected, theme, &tooltip);
    let color = toolbox_group_color(group.id, theme);
    let marker = egui::Rect::from_min_max(
        response.rect.left_top() + egui::vec2(2.0, 7.0),
        response.rect.left_bottom() + egui::vec2(5.0, -7.0),
    );
    ui.painter()
        .rect_filled(marker, egui::Rounding::same(2.0), color);
    if selected {
        ui.painter().circle_filled(
            response.rect.right_bottom() - egui::vec2(7.0, 7.0),
            2.5,
            color,
        );
    }
    response
}

fn toolbox_icon_command(
    ui: &mut egui::Ui,
    icon: Icon,
    selected: bool,
    enabled: bool,
    theme: crate::theme::ThemeSettings,
    tooltip: &str,
) -> bool {
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            crate::ui_kit::icon_square(ui, icon, selected, theme, tooltip)
        })
        .inner;
    enabled && response.clicked()
}

fn set_toolbox_result_selection(result: &mut BuildDockResult, selection: ToolboxSelection) {
    match selection {
        ToolboxSelection::Tool(tool) => result.clicked_tool = Some(tool),
        ToolboxSelection::Workflow(preset) => result.workflow_preset = Some(preset),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorStatusDensity {
    Compact,
    Stacked,
    Wide,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EditorStatusBarMetrics {
    inner_width: f32,
    density: EditorStatusDensity,
    clears_left_rail: bool,
}

fn editor_status_bar_metrics(viewport_width: f32, viewport_height: f32) -> EditorStatusBarMetrics {
    let width = if viewport_width.is_finite() {
        viewport_width.max(320.0)
    } else {
        1280.0
    };
    let height = if viewport_height.is_finite() {
        viewport_height.max(240.0)
    } else {
        720.0
    };
    // A short window makes the vertically-centred toolbox reach the bottom
    // dock even when horizontal space is plentiful. In that case reserve the
    // complete left rail and right-align the status surface beside it.
    let clears_left_rail = width < 420.0 || height < 560.0;
    let inner_width = if clears_left_rail {
        (width - TOOLBOX_DRAWER_LEFT - 32.0).clamp(180.0, 930.0)
    } else {
        (width - 32.0).clamp(288.0, 930.0)
    };
    let density = if inner_width < 300.0 {
        EditorStatusDensity::Compact
    } else if inner_width < 860.0 {
        EditorStatusDensity::Stacked
    } else {
        EditorStatusDensity::Wide
    };
    EditorStatusBarMetrics {
        inner_width,
        density,
        clears_left_rail,
    }
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
    tool_phase: crate::sketch_model::EditorToolPhase,
    theme: crate::theme::ThemeSettings,
    primary: egui::Color32,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let viewport = ctx.screen_rect().size();
    let metrics = editor_status_bar_metrics(viewport.x, viewport.y);
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

    let (anchor, offset) = if metrics.clears_left_rail {
        (egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
    } else {
        (egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
    };
    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_status"))
        .anchor(anchor, offset)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(metrics.inner_width);
                ui.spacing_mut().item_spacing = egui::vec2(7.0, 4.0);
                match metrics.density {
                    EditorStatusDensity::Wide => {
                        ui.horizontal(|ui| {
                            selected_tool_badge(
                                ui,
                                active_tool,
                                active_workflow,
                                tool_phase,
                                theme,
                            );
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
                                metric_chip(
                                    ui,
                                    Icon::Snap,
                                    "SNAP",
                                    primary,
                                    "Endpoint snap is active",
                                );
                            }
                            ui.separator();
                            if drawer_chip(ui, picker_open, primary) {
                                result.toggle_picker = true;
                            }
                            editor_status_message(ui, status, colors.text_muted);
                        });
                    }
                    EditorStatusDensity::Stacked => {
                        ui.horizontal(|ui| {
                            selected_tool_badge(
                                ui,
                                active_tool,
                                active_workflow,
                                tool_phase,
                                theme,
                            );
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
                                metric_chip(
                                    ui,
                                    Icon::Snap,
                                    "SNAP",
                                    primary,
                                    "Endpoint snap is active",
                                );
                            }
                            ui.separator();
                            if drawer_chip(ui, picker_open, primary) {
                                result.toggle_picker = true;
                            }
                        });
                        editor_status_message(ui, status, colors.text_muted);
                    }
                    EditorStatusDensity::Compact => {
                        ui.horizontal(|ui| {
                            selected_tool_badge_compact(
                                ui,
                                active_tool,
                                active_workflow,
                                tool_phase,
                                theme,
                            );
                            if drawer_chip(ui, picker_open, primary) {
                                result.toggle_picker = true;
                            }
                        });
                        ui.horizontal(|ui| {
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
                                metric_chip(
                                    ui,
                                    Icon::Snap,
                                    "SNAP",
                                    primary,
                                    "Endpoint snap is active",
                                );
                            }
                        });
                        editor_status_message_compact(ui, status, colors.text_muted);
                    }
                }
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
}

fn editor_status_message(ui: &mut egui::Ui, status: &str, color: egui::Color32) {
    let width = ui.available_width().max(96.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
    ui.painter()
        .with_clip_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
        .text(
            rect.left_center() + egui::vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            status,
            egui::FontId::monospace(10.5),
            color,
        );
    response.on_hover_text(status);
}

fn editor_status_message_compact(ui: &mut egui::Ui, status: &str, color: egui::Color32) {
    let visible = status_excerpt(status, 29);
    let width = ui.available_width().max(96.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
    ui.painter()
        .with_clip_rect(rect.shrink2(egui::vec2(4.0, 0.0)))
        .text(
            rect.left_center() + egui::vec2(4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            visible,
            egui::FontId::monospace(10.5),
            color,
        );
    response.on_hover_text(status);
}

fn status_excerpt(status: &str, max_characters: usize) -> String {
    let count = status.chars().count();
    if count <= max_characters {
        return status.to_owned();
    }
    let visible_characters = max_characters.saturating_sub(3);
    let mut excerpt = status.chars().take(visible_characters).collect::<String>();
    if let Some((byte_index, _)) = excerpt
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
    {
        let word_boundary_characters = excerpt[..byte_index].chars().count();
        if word_boundary_characters * 3 >= visible_characters * 2 {
            excerpt.truncate(byte_index);
        }
    }
    excerpt.truncate(excerpt.trim_end().len());
    excerpt.push_str("...");
    excerpt
}

const TOOLBOX_DRAWER_LEFT: f32 = 76.0;
const TOOLBOX_VIEWPORT_MARGIN: f32 = 8.0;
const TOOLBOX_RAIL_LEFT_MARGIN: f32 = 14.0;
const TOOLBOX_FLYOUT_GAP: f32 = 12.0;
const TOOLBOX_FLYOUT_MAX_CONTENT_WIDTH: f32 = 300.0;
const TOOLBOX_FLYOUT_HEADER_HEIGHT: f32 = 26.0;
const TOOLBOX_FLYOUT_SEPARATOR_HEIGHT: f32 = 1.0;
const TOOLBOX_FLYOUT_ITEM_SPACING: f32 = 6.0;
const TOOLBOX_FLYOUT_TWO_COLUMN_MIN_VIEWPORT_WIDTH: f32 = 600.0;
const TOOLBOX_FLYOUT_SLOTS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
struct HoverFlyoutLayout {
    safe_rect: egui::Rect,
    outer_rect: egui::Rect,
    content_width: f32,
    items_viewport_height: f32,
    columns: usize,
}

fn editor_toolbox_frame(theme: crate::theme::ThemeSettings) -> egui::Frame {
    crate::ui_kit::toolbench_frame(theme).inner_margin(egui::Margin::symmetric(7.0, 8.0))
}

fn editor_hover_flyout_frame(theme: crate::theme::ThemeSettings) -> egui::Frame {
    crate::ui_kit::toolbench_frame(theme).inner_margin(egui::Margin::symmetric(9.0, 8.0))
}

fn hover_flyout_layout(
    viewport: egui::Rect,
    toolbox_right: f32,
    frame_margin: egui::Margin,
    item_height: f32,
) -> HoverFlyoutLayout {
    let viewport = if viewport.is_finite() && viewport.width() > 0.0 && viewport.height() > 0.0 {
        viewport
    } else {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))
    };
    let safe_inset = TOOLBOX_VIEWPORT_MARGIN
        .min(viewport.width() * 0.25)
        .min(viewport.height() * 0.25);
    let safe_rect = viewport.shrink(safe_inset);
    let frame_width = frame_margin.left + frame_margin.right;
    let frame_height = frame_margin.top + frame_margin.bottom;
    let minimum_outer_width = crate::theme::KANSO_LAYOUT.icon_action_min_width + frame_width;
    let toolbox_right = if toolbox_right.is_finite() {
        toolbox_right
    } else {
        safe_rect.left()
    };
    let preferred_left = toolbox_right + TOOLBOX_FLYOUT_GAP;
    let left = preferred_left
        .min(safe_rect.right() - minimum_outer_width)
        .max(safe_rect.left());
    let available_outer_width = (safe_rect.right() - left).max(minimum_outer_width);
    let content_width = (available_outer_width - frame_width).clamp(
        crate::theme::KANSO_LAYOUT.icon_action_min_width,
        TOOLBOX_FLYOUT_MAX_CONTENT_WIDTH,
    );
    let outer_width = content_width + frame_width;

    let columns = if viewport.width() >= TOOLBOX_FLYOUT_TWO_COLUMN_MIN_VIEWPORT_WIDTH {
        2
    } else {
        1
    };
    let rows = TOOLBOX_FLYOUT_SLOTS.div_ceil(columns);
    let item_height = if item_height.is_finite() {
        item_height.max(1.0)
    } else {
        36.0
    };
    let natural_items_height =
        rows as f32 * item_height + rows.saturating_sub(1) as f32 * TOOLBOX_FLYOUT_ITEM_SPACING;
    // Header -> separator -> scroll area each receive the same deterministic
    // vertical spacing. The scroll viewport consumes only the remainder of
    // the safe rect, so adding future rows cannot push the frame off-screen.
    let fixed_content_height = TOOLBOX_FLYOUT_HEADER_HEIGHT
        + TOOLBOX_FLYOUT_SEPARATOR_HEIGHT
        + 2.0 * TOOLBOX_FLYOUT_ITEM_SPACING;
    let max_items_height = (safe_rect.height() - frame_height - fixed_content_height).max(1.0);
    let items_viewport_height = natural_items_height.min(max_items_height);
    let outer_height = frame_height + fixed_content_height + items_viewport_height;
    let outer_rect = egui::Rect::from_center_size(
        egui::pos2(left + outer_width * 0.5, safe_rect.center().y),
        egui::vec2(outer_width, outer_height),
    );

    HoverFlyoutLayout {
        safe_rect,
        outer_rect,
        content_width,
        items_viewport_height,
        columns,
    }
}

fn draw_editor_drawer(
    ctx: &egui::Context,
    active_tool: ToolbeltTool,
    active_block: BlockType,
    brush: IVec3,
    theme: crate::theme::ThemeSettings,
    accent: egui::Color32,
    creator_library: &mut CreatorLibraryState,
    ships: &ShipInventory,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let frame =
        crate::ui_kit::toolbench_frame(theme).inner_margin(egui::Margin::symmetric(10.0, 10.0));

    let drawer_width = (ctx.screen_rect().width() - TOOLBOX_DRAWER_LEFT - 28.0)
        .max(180.0)
        .min(520.0);
    let compact_library = drawer_width < 430.0;
    let area = egui::Area::new(egui::Id::new("voxel_native_sketch_editor_drawer"))
        .anchor(
            egui::Align2::LEFT_CENTER,
            egui::vec2(TOOLBOX_DRAWER_LEFT, 0.0),
        )
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_width(drawer_width);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 7.0);
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    paint_icon(ui.painter(), rect, Icon::Drawer, accent);
                    ui.label(
                        egui::RichText::new("CREATOR LIBRARY")
                            .monospace()
                            .size(11.0)
                            .strong()
                            .color(accent),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if toolbox_icon_command(
                            ui,
                            Icon::Close,
                            false,
                            true,
                            theme,
                            "Close the drawer.",
                        ) {
                            result.toggle_picker = true;
                        }
                    });
                });
                if active_tool.uses_live_brush() {
                    crate::ui_kit::compact_separator(ui, theme);
                    ui.label(
                        egui::RichText::new("BRUSH SIZE")
                            .monospace()
                            .size(9.5)
                            .strong()
                            .color(colors.text_muted),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(5.0, 4.0);
                        for (label, size) in brush_presets() {
                            if brush_preset_chip(ui, label, size, brush, theme) {
                                result.brush_preset = Some(size);
                            }
                        }
                    });
                }
                crate::ui_kit::compact_separator(ui, theme);
                let max_height = (ctx.screen_rect().height() - 150.0).clamp(260.0, 680.0);
                let library_result = egui::ScrollArea::vertical()
                    .id_source("sketch_editor_creator_library")
                    .max_height(max_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        draw_creator_library(
                            ui,
                            creator_library,
                            active_block,
                            ships,
                            compact_library,
                            theme,
                        )
                    })
                    .inner;
                if library_result.action.is_some() {
                    result.creator_action = library_result.action;
                }
            });
        });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.drawer_hovered |= area.response.hovered();
    result.drawer_rect = Some(area.response.rect);
}

fn draw_editor_hover_flyout(
    ctx: &egui::Context,
    group_id: ToolboxGroupId,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    theme: crate::theme::ThemeSettings,
    result: &mut BuildDockResult,
) {
    let colors = theme.semantic();
    let group = toolbox_group(group_id);
    let accent = toolbox_group_color(group_id, theme);
    let frame = editor_hover_flyout_frame(theme);
    let fallback_toolbox_frame_width = editor_toolbox_frame(theme).total_margin().sum().x;
    let toolbox_right = result.toolbox_rect.map_or_else(
        || {
            ctx.screen_rect().left()
                + TOOLBOX_RAIL_LEFT_MARGIN
                + fallback_toolbox_frame_width
                + crate::theme::KANSO_LAYOUT.icon_square_size
        },
        |rect| rect.right(),
    );
    let layout = hover_flyout_layout(
        ctx.screen_rect(),
        toolbox_right,
        frame.total_margin(),
        theme.density.row_height(),
    );
    let item_width = if layout.columns == 1 {
        layout.content_width
    } else {
        (layout.content_width - TOOLBOX_FLYOUT_ITEM_SPACING) * 0.5
    };

    let area = egui::Area::new(egui::Id::new(
        "voxel_native_sketch_editor_context_flyout_area",
    ))
    .fixed_pos(layout.outer_rect.min)
    .default_size(layout.outer_rect.size())
    .constrain_to(layout.safe_rect)
    .order(egui::Order::Foreground)
    .show(ctx, |ui| {
        frame.show(ui, |ui| {
            ui.set_width(layout.content_width);
            ui.set_max_width(layout.content_width);
            ui.spacing_mut().item_spacing =
                egui::vec2(TOOLBOX_FLYOUT_ITEM_SPACING, TOOLBOX_FLYOUT_ITEM_SPACING);
            toolbox_group_header(ui, group, accent, colors.text_muted, layout.content_width);
            crate::ui_kit::compact_separator(ui, theme);
            egui::ScrollArea::vertical()
                .id_source(("stable_toolbox_group_scroll", group.label))
                .max_width(layout.content_width)
                .max_height(layout.items_viewport_height)
                .min_scrolled_height(layout.items_viewport_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new(("stable_toolbox_group", group.label))
                        .num_columns(layout.columns)
                        .spacing(egui::vec2(
                            TOOLBOX_FLYOUT_ITEM_SPACING,
                            TOOLBOX_FLYOUT_ITEM_SPACING,
                        ))
                        .show(ui, |ui| {
                            for slot in 0..TOOLBOX_FLYOUT_SLOTS {
                                if let Some(selection) = group.items.get(slot).copied() {
                                    let selected = toolbox_selection_is_active(
                                        selection,
                                        active_tool,
                                        active_workflow,
                                    );
                                    if toolbox_selection_action(
                                        ui, selection, selected, accent, theme, item_width,
                                    ) {
                                        set_toolbox_result_selection(result, selection);
                                    }
                                } else {
                                    ui.allocate_exact_size(
                                        egui::vec2(item_width, theme.density.row_height()),
                                        egui::Sense::hover(),
                                    );
                                }
                                if (slot + 1) % layout.columns == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
        });
    });
    result.wheel_navigation_hovered |= area.response.hovered();
    result.drawer_hovered |= area.response.hovered();
    result.drawer_rect = Some(area.response.rect);
}

fn toolbox_group_header(
    ui: &mut egui::Ui,
    group: ToolboxGroup,
    accent: egui::Color32,
    text: egui::Color32,
    width: f32,
) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, TOOLBOX_FLYOUT_HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    paint_icon(
        &painter,
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 11.0, rect.center().y),
            egui::vec2(18.0, 18.0),
        ),
        group.icon,
        accent,
    );
    painter.text(
        egui::pos2(rect.left() + 28.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        group.label,
        egui::FontId::monospace(11.5),
        text,
    );
    response.on_hover_text(group.hint);
}

fn toolbox_selection_is_active(
    selection: ToolboxSelection,
    active_tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
) -> bool {
    match selection {
        ToolboxSelection::Tool(tool) => active_tool == tool && active_workflow.is_none(),
        ToolboxSelection::Workflow(preset) => {
            workflow_preset_selected(preset, active_tool, active_workflow)
        }
    }
}

fn toolbox_selection_action(
    ui: &mut egui::Ui,
    selection: ToolboxSelection,
    selected: bool,
    accent: egui::Color32,
    theme: crate::theme::ThemeSettings,
    width: f32,
) -> bool {
    let response = crate::ui_kit::icon_action_sized(
        ui,
        toolbox_selection_icon(selection),
        toolbox_selection_label(selection),
        selected,
        width,
        theme,
    );
    let marker = egui::Rect::from_min_max(
        response.rect.left_top() + egui::vec2(2.0, 6.0),
        response.rect.left_bottom() + egui::vec2(5.0, -6.0),
    );
    ui.painter()
        .rect_filled(marker, egui::Rounding::same(2.0), accent);
    let clicked = response.clicked();
    response.on_hover_text(toolbox_selection_hint(selection));
    clicked
}

fn toolbox_selection_icon(selection: ToolboxSelection) -> Icon {
    match selection {
        ToolboxSelection::Tool(tool) => tool.icon(),
        ToolboxSelection::Workflow(preset) => preset.icon(),
    }
}

fn toolbox_selection_label(selection: ToolboxSelection) -> &'static str {
    match selection {
        ToolboxSelection::Tool(tool) => toolbox_tool_label(tool),
        ToolboxSelection::Workflow(preset) => workflow_toolbox_label(preset),
    }
}

fn toolbox_selection_hint(selection: ToolboxSelection) -> String {
    match selection {
        ToolboxSelection::Tool(tool) => format!("{}: {}", toolbox_tool_label(tool), tool.hint()),
        ToolboxSelection::Workflow(preset) => {
            format!("{}: {}", workflow_toolbox_label(preset), preset.hint())
        }
    }
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
        .map(workflow_toolbox_label)
        .unwrap_or_else(|| toolbox_tool_label(tool))
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

fn toolbox_tool_label(tool: ToolbeltTool) -> &'static str {
    match tool {
        ToolbeltTool::Navigate => "Select",
        ToolbeltTool::DrawRect => "Rectangle",
        ToolbeltTool::Sculpt => "Push/Pull",
        ToolbeltTool::TransformMove => "Move",
        ToolbeltTool::TransformScale => "Scale",
        ToolbeltTool::TransformRotate => "Rotate",
        ToolbeltTool::MaterialPicker => "Paint",
        ToolbeltTool::SmartTower => "Tower",
        ToolbeltTool::BrushPlace => "Build",
        ToolbeltTool::BrushCut => "Cut",
        ToolbeltTool::CityRoad => "Road",
        ToolbeltTool::CityDistrict => "Bot Area",
        ToolbeltTool::CityBuilding => "City Shell",
        ToolbeltTool::CityFacade => "Facade",
        ToolbeltTool::AnimationPick => "Animation",
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
    active_workflow: Option<BuildWorkflowPreset>,
    phase: crate::sketch_model::EditorToolPhase,
    theme: crate::theme::ThemeSettings,
) {
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(178.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let color = active_editor_color(tool, active_workflow);
    crate::ui_kit::hud_panel(&painter, rect, theme, 0.84, color);
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
        format!("ACTIVE / {}", editor_phase_label(phase)),
        egui::FontId::monospace(9.5),
        editor_phase_color(phase, theme),
    );
    painter.text(
        rect.min + egui::vec2(34.0, 23.0),
        egui::Align2::LEFT_CENTER,
        active_editor_label(tool, active_workflow),
        egui::FontId::monospace(11.5),
        colors.text,
    );
    response.on_hover_text(format!(
        "{} - {}. {}",
        active_editor_label(tool, active_workflow),
        editor_phase_label(phase).to_ascii_lowercase(),
        active_editor_hint(tool, active_workflow)
    ));
}

fn selected_tool_badge_compact(
    ui: &mut egui::Ui,
    tool: ToolbeltTool,
    active_workflow: Option<BuildWorkflowPreset>,
    phase: crate::sketch_model::EditorToolPhase,
    theme: crate::theme::ThemeSettings,
) {
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(118.0, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let color = active_editor_color(tool, active_workflow);
    crate::ui_kit::hud_panel(&painter, rect, theme, 0.84, color);
    let icon_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 7.0), egui::vec2(20.0, 20.0));
    paint_icon(
        &painter,
        icon_rect,
        active_editor_icon(tool, active_workflow),
        color,
    );
    painter.text(
        rect.min + egui::vec2(34.0, 10.0),
        egui::Align2::LEFT_CENTER,
        editor_phase_label(phase),
        egui::FontId::monospace(8.5),
        editor_phase_color(phase, theme),
    );
    painter.text(
        rect.min + egui::vec2(34.0, 23.0),
        egui::Align2::LEFT_CENTER,
        active_editor_label(tool, active_workflow),
        egui::FontId::monospace(10.0),
        colors.text,
    );
    response.on_hover_text(format!(
        "{} - {}. {}",
        active_editor_label(tool, active_workflow),
        editor_phase_label(phase).to_ascii_lowercase(),
        active_editor_hint(tool, active_workflow)
    ));
}

fn editor_phase_label(phase: crate::sketch_model::EditorToolPhase) -> &'static str {
    match phase {
        crate::sketch_model::EditorToolPhase::Idle => "READY",
        crate::sketch_model::EditorToolPhase::Previewing => "IN PROGRESS",
        crate::sketch_model::EditorToolPhase::Committed => "APPLIED",
        crate::sketch_model::EditorToolPhase::Cancelled => "CANCELLED",
    }
}

fn editor_phase_color(
    phase: crate::sketch_model::EditorToolPhase,
    theme: crate::theme::ThemeSettings,
) -> egui::Color32 {
    let colors = theme.semantic();
    match phase {
        crate::sketch_model::EditorToolPhase::Idle => colors.text_muted,
        crate::sketch_model::EditorToolPhase::Previewing => colors.info,
        crate::sketch_model::EditorToolPhase::Committed => colors.success,
        crate::sketch_model::EditorToolPhase::Cancelled => colors.warning,
    }
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

fn brush_preset_chip(
    ui: &mut egui::Ui,
    label: &'static str,
    size: IVec3,
    brush: IVec3,
    theme: crate::theme::ThemeSettings,
) -> bool {
    let selected = brush == size;
    crate::ui_kit::choice_chip_sized(ui, label, selected, 52.0, theme)
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

fn format_history_command_status(
    command: HistoryCommand,
    result: Option<(String, usize)>,
) -> String {
    match (command, result) {
        (HistoryCommand::Undo, Some((label, _))) => {
            format!("Undid '{label}'. Redo is available.")
        }
        (HistoryCommand::Redo, Some((label, _))) => {
            format!("Redid '{label}'. Undo is available.")
        }
        (HistoryCommand::Undo, None) => "Undo: no build edits to rewind yet.".into(),
        (HistoryCommand::Redo, None) => "Redo: no undone build edits to replay yet.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_status_bar_adapts_without_covering_the_toolbox_rail() {
        let portrait = editor_status_bar_metrics(320.0, 480.0);
        assert_eq!(portrait.density, EditorStatusDensity::Compact);
        assert!(portrait.clears_left_rail);
        assert_eq!(portrait.inner_width, 212.0);
        // Inner width + 24px frame margins fits to the right of the 76px rail
        // with an 8px outside margin.
        assert!(portrait.inner_width + 24.0 <= 320.0 - TOOLBOX_DRAWER_LEFT - 8.0);

        let small_desktop = editor_status_bar_metrics(800.0, 600.0);
        assert_eq!(small_desktop.density, EditorStatusDensity::Stacked);
        assert!(!small_desktop.clears_left_rail);
        assert_eq!(small_desktop.inner_width, 768.0);

        let short_desktop = editor_status_bar_metrics(800.0, 480.0);
        assert_eq!(short_desktop.density, EditorStatusDensity::Stacked);
        assert!(short_desktop.clears_left_rail);
        assert_eq!(short_desktop.inner_width, 692.0);

        let standard = editor_status_bar_metrics(1280.0, 720.0);
        assert_eq!(standard.density, EditorStatusDensity::Wide);
        assert_eq!(standard.inner_width, 930.0);
        assert!(!standard.clears_left_rail);

        let ultrawide = editor_status_bar_metrics(3440.0, 1440.0);
        assert_eq!(ultrawide.density, EditorStatusDensity::Wide);
        assert_eq!(ultrawide.inner_width, 930.0);
        assert!(!ultrawide.clears_left_rail);

        assert_eq!(editor_status_bar_metrics(f32::NAN, f32::INFINITY), standard);
    }

    #[test]
    fn compact_status_excerpt_marks_truncation_without_splitting_unicode() {
        assert_eq!(status_excerpt("Short status", 29), "Short status");
        assert_eq!(
            status_excerpt("Sketch Draw ready. Click start, move, click finish", 29),
            "Sketch Draw ready. Click..."
        );
        assert_eq!(
            status_excerpt("Grunflache schon auswahlen", 12),
            "Grunflach..."
        );
        assert_eq!(
            status_excerpt("Grünfläche schön auswählen", 12),
            "Grünfläch..."
        );
    }

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

        assert!(undo.contains("Undid 'Sketch Fill 12 cells'"));
        assert!(undo.contains("Redo is available"));
        assert!(!undo.contains("Ctrl+"));
        assert!(redo.contains("no undone build edits"));
    }

    #[test]
    fn toolbelt_history_step_clears_stale_selection_and_hover() {
        let entity = crate::sketch_model::SketchId::new_for_test(144);
        let mut controller = crate::sketch_model::ToolController::default();
        let hit = crate::sketch_model::HitRecord::new(
            entity,
            [],
            crate::sketch_model::HitKind::Face,
            Vec3::new(2.0, 3.0, 4.0),
            0.0,
        );
        controller.selection_mut().select(entity);
        let mut hover = crate::sketch_model::SemanticHoverHit(Some(hit));

        clear_editor_selection_after_toolbelt_history_step(&mut controller, &mut hover);

        assert!(controller.selection().is_empty());
        assert!(hover.0.is_none());
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
    fn editor_toolbox_groups_are_stable_and_prioritized() {
        assert_eq!(
            TOOLBOX_GROUP_IDS.map(|id| toolbox_group(id).label),
            ["Core", "Draw", "Shape", "Transform", "World",]
        );
        assert_eq!(
            toolbox_group(ToolboxGroupId::Draw).primary,
            ToolboxSelection::Workflow(BuildWorkflowPreset::Sketch)
        );
        assert_eq!(
            toolbox_group(ToolboxGroupId::Shape).primary,
            ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull)
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
    fn visible_toolbox_labels_do_not_expose_number_shortcuts() {
        for id in TOOLBOX_GROUP_IDS {
            let group = toolbox_group(id);
            assert!(!group.label.chars().any(|ch| ch.is_ascii_digit()));
            for selection in group.items {
                assert!(!toolbox_selection_label(*selection)
                    .chars()
                    .any(|ch| ch.is_ascii_digit()));
            }
        }
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
    fn toolbox_groups_partition_every_selection_without_duplicates() {
        let item_count: usize = TOOLBOX_GROUP_IDS
            .into_iter()
            .map(|id| toolbox_group(id).items.len())
            .sum();
        assert_eq!(item_count, ToolboxSelection::ORDER.len());

        for selection in ToolboxSelection::ORDER {
            let occurrences = TOOLBOX_GROUP_IDS
                .into_iter()
                .filter(|id| toolbox_group(*id).items.contains(&selection))
                .count();
            assert_eq!(
                occurrences, 1,
                "{selection:?} must appear in exactly one group"
            );
        }
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
    fn hover_drawer_bridge_is_a_narrow_gap_not_an_invisible_world_panel() {
        let toolbox = egui::Rect::from_min_size(egui::pos2(14.0, 160.0), egui::vec2(50.0, 380.0));
        let drawer = egui::Rect::from_min_size(egui::pos2(76.0, 245.0), egui::vec2(318.0, 206.0));
        let bridge = hover_drawer_bridge_rect(toolbox, drawer);

        assert!(bridge.width() <= 20.0);
        assert!(bridge.contains(egui::pos2(70.0, drawer.center().y)));
        assert!(bridge.top() <= toolbox.top());
        assert!(bridge.bottom() >= toolbox.bottom());
    }

    #[test]
    fn every_visible_editor_tool_keeps_pointer_control_available() {
        for selection in ToolboxSelection::ORDER {
            assert!(
                toolbox_selection_editor_tool(selection).uses_pointer_surface(),
                "{selection:?} must map to a canonical mouse-first editor tool"
            );
        }
    }

    #[test]
    fn hover_drawer_reuses_retained_group_while_pointer_crosses_gap() {
        let retained = ToolboxGroupId::Shape;
        let selected = hover_drawer_group(None, Some(retained), ToolbeltTool::Navigate, None);

        assert_eq!(selected, retained);
    }

    #[test]
    fn every_group_primary_is_a_directly_selectable_item() {
        for id in TOOLBOX_GROUP_IDS {
            let group = toolbox_group(id);
            assert!(group.items.contains(&group.primary));

            let mut result = BuildDockResult::default();
            set_toolbox_result_selection(&mut result, group.primary);
            assert!(result.clicked_tool.is_some() || result.workflow_preset.is_some());
        }
    }

    fn assert_flyout_layout_is_safe(viewport_size: egui::Vec2, expected_columns: usize) {
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport_size);
        let toolbox = editor_toolbox_frame(crate::theme::ThemeSettings::default());
        let toolbox_right = viewport.left()
            + TOOLBOX_RAIL_LEFT_MARGIN
            + toolbox.total_margin().sum().x
            + crate::theme::KANSO_LAYOUT.icon_square_size;
        let frame = editor_hover_flyout_frame(crate::theme::ThemeSettings::default());
        let layout = hover_flyout_layout(viewport, toolbox_right, frame.total_margin(), 36.0);

        assert_eq!(layout.columns, expected_columns);
        assert!(layout.safe_rect.is_finite());
        assert!(layout.outer_rect.is_finite());
        assert!(layout.content_width.is_finite());
        assert!(layout.items_viewport_height.is_finite());
        assert!(layout.safe_rect.contains_rect(layout.outer_rect));
        assert!(viewport.contains_rect(layout.outer_rect));
        assert!(layout.content_width >= crate::theme::KANSO_LAYOUT.icon_action_min_width);
        assert!(layout.content_width <= TOOLBOX_FLYOUT_MAX_CONTENT_WIDTH);
        assert!(layout.items_viewport_height > 0.0);
        assert!(layout.outer_rect.left() >= toolbox_right + TOOLBOX_FLYOUT_GAP);
    }

    #[test]
    fn viewport_derived_flyout_capacity_covers_every_group() {
        assert_flyout_layout_is_safe(egui::vec2(320.0, 480.0), 1);
        assert_flyout_layout_is_safe(egui::vec2(800.0, 600.0), 2);
        assert_flyout_layout_is_safe(egui::vec2(3440.0, 1440.0), 2);
        for id in TOOLBOX_GROUP_IDS {
            assert!(toolbox_group(id).items.len() <= TOOLBOX_FLYOUT_SLOTS);
        }
    }

    #[test]
    fn viewport_derived_flyout_fails_safe_for_non_finite_input() {
        let frame = editor_hover_flyout_frame(crate::theme::ThemeSettings::default());
        let layout = hover_flyout_layout(
            egui::Rect::from_min_max(
                egui::pos2(f32::NAN, f32::NEG_INFINITY),
                egui::pos2(f32::INFINITY, f32::NAN),
            ),
            f32::NAN,
            frame.total_margin(),
            f32::NAN,
        );

        assert!(layout.safe_rect.is_finite());
        assert!(layout.outer_rect.is_finite());
        assert!(layout.safe_rect.contains_rect(layout.outer_rect));
        assert_eq!(layout.columns, 2);
    }

    #[test]
    fn rendered_flyout_stays_inside_safe_viewport_across_dpi_matrix() {
        for viewport_size in [
            egui::vec2(320.0, 480.0),
            egui::vec2(800.0, 600.0),
            egui::vec2(3440.0, 1440.0),
        ] {
            for pixels_per_point in [1.0_f32, 1.5, 2.0] {
                let ctx = egui::Context::default();
                let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport_size);
                let mut rendered_rect = None;
                let mut output = None;
                // egui Areas intentionally use their first frame for sizing.
                // Rendering twice verifies the measured, stable outer rect.
                for frame_index in 0..2 {
                    let mut raw_input = egui::RawInput {
                        screen_rect: Some(viewport),
                        time: Some(frame_index as f64 / 60.0),
                        ..Default::default()
                    };
                    raw_input
                        .viewports
                        .entry(egui::ViewportId::ROOT)
                        .or_default()
                        .native_pixels_per_point = Some(pixels_per_point);
                    let frame_output = ctx.run(raw_input, |ctx| {
                        let mut result = BuildDockResult::default();
                        result.toolbox_rect = Some(egui::Rect::from_min_max(
                            egui::pos2(
                                ctx.screen_rect().left() + TOOLBOX_RAIL_LEFT_MARGIN,
                                ctx.screen_rect().center().y - 120.0,
                            ),
                            egui::pos2(
                                ctx.screen_rect().left()
                                    + TOOLBOX_RAIL_LEFT_MARGIN
                                    + editor_toolbox_frame(crate::theme::ThemeSettings::default())
                                        .total_margin()
                                        .sum()
                                        .x
                                    + crate::theme::KANSO_LAYOUT.icon_square_size,
                                ctx.screen_rect().center().y + 120.0,
                            ),
                        ));
                        draw_editor_hover_flyout(
                            ctx,
                            ToolboxGroupId::Draw,
                            ToolbeltTool::DrawRect,
                            Some(BuildWorkflowPreset::Sketch),
                            crate::theme::ThemeSettings::default(),
                            &mut result,
                        );
                        rendered_rect = result.drawer_rect;
                    });
                    output = Some(frame_output);
                }

                let rendered_rect = rendered_rect.expect("flyout area rect");
                let frame = editor_hover_flyout_frame(crate::theme::ThemeSettings::default());
                let toolbox = editor_toolbox_frame(crate::theme::ThemeSettings::default());
                let toolbox_right = viewport.left()
                    + TOOLBOX_RAIL_LEFT_MARGIN
                    + toolbox.total_margin().sum().x
                    + crate::theme::KANSO_LAYOUT.icon_square_size;
                let layout =
                    hover_flyout_layout(viewport, toolbox_right, frame.total_margin(), 36.0);
                assert!(rendered_rect.is_finite());
                assert!(layout.safe_rect.contains_rect(rendered_rect),
                    "{viewport_size:?} at {pixels_per_point} ppp rendered {rendered_rect:?} outside {:?}",
                    layout.safe_rect);
                assert!(rendered_rect.left() >= toolbox_right + TOOLBOX_FLYOUT_GAP);
                let output = output.expect("egui output");
                assert_eq!(output.pixels_per_point, pixels_per_point);
                let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
                assert!(!primitives.is_empty());
                for primitive in primitives {
                    assert!(primitive.clip_rect.is_finite());
                    if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                        assert!(mesh
                            .vertices
                            .iter()
                            .all(|vertex| vertex.pos.is_finite() && vertex.uv.is_finite()));
                    }
                }
            }
        }
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
    fn stable_groups_do_not_mix_unrelated_tools() {
        let transform = toolbox_group(ToolboxGroupId::Transform);
        assert!(transform
            .items
            .contains(&ToolboxSelection::Tool(ToolbeltTool::TransformScale)));
        assert!(transform
            .items
            .contains(&ToolboxSelection::Tool(ToolbeltTool::TransformRotate)));
        assert!(!transform
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::Roads)));

        let draw = toolbox_group(ToolboxGroupId::Draw);
        assert_eq!(draw.label, "Draw");
        assert_eq!(draw.items.len(), TOOLBOX_FLYOUT_SLOTS);
        assert!(!draw
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea)));

        let shape = toolbox_group(ToolboxGroupId::Shape);
        assert!(shape
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::PushPull)));
        assert!(shape
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::Opening)));
        assert!(!shape
            .items
            .contains(&ToolboxSelection::Workflow(BuildWorkflowPreset::BotArea)));
    }

    #[test]
    fn active_group_tracks_direct_tools_without_a_workflow() {
        assert_eq!(
            active_toolbox_group(ToolbeltTool::DrawRect, None),
            ToolboxGroupId::Draw
        );
        assert_eq!(
            active_toolbox_group(ToolbeltTool::Sculpt, None),
            ToolboxGroupId::Shape
        );
        assert_eq!(
            active_toolbox_group(ToolbeltTool::BrushPlace, None),
            ToolboxGroupId::World
        );
    }

    #[test]
    fn active_editor_badge_names_workflow_not_internal_tool() {
        assert_eq!(
            active_editor_label(ToolbeltTool::DrawRect, Some(BuildWorkflowPreset::Pencil)),
            "Line"
        );
        assert_eq!(
            active_editor_label(ToolbeltTool::DrawRect, Some(BuildWorkflowPreset::Sketch)),
            "Rect"
        );
        assert_eq!(
            active_editor_label(ToolbeltTool::Sculpt, Some(BuildWorkflowPreset::PushPull)),
            "Push/Pull"
        );
        assert_eq!(active_editor_label(ToolbeltTool::CityRoad, None), "Road");
    }

    #[test]
    fn house_workflow_status_overrides_generic_sketch_draw_hint() {
        let status = compact_status(
            "Sketch Draw ready. Click start, move, click finish; toolbox switches Room/Opening.",
            ToolbeltTool::DrawRect,
            Some(BuildWorkflowPreset::ModernHouse),
        );

        assert!(status.starts_with("House ready"));
        assert!(status.contains("pull the walls"));
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

        assert!(status.starts_with("Line in progress."));
        assert!(status.contains("snapped endpoint"));
        assert!(!status.contains("PENCIL:"));
        assert!(!status.contains("Sketch Draw ready"));
    }

    #[test]
    fn committed_status_hides_internal_transaction_text() {
        let mut controller = crate::sketch_model::ToolController::default();
        controller.activate(crate::sketch_model::EditorToolId::Pencil);
        controller.begin_transaction("internal pencil transaction");
        assert!(controller.commit_transaction().is_some());

        let status = compact_status_for_controller(
            "unused internal status",
            ToolbeltTool::DrawRect,
            Some(BuildWorkflowPreset::Pencil),
            &controller,
        );

        assert_eq!(
            status,
            "Line applied. Continue in the world or choose another tool."
        );
        assert!(!status.contains("internal pencil transaction"));
    }

    #[test]
    fn active_state_phases_use_plain_user_facing_words() {
        assert_eq!(
            editor_phase_label(crate::sketch_model::EditorToolPhase::Idle),
            "READY"
        );
        assert_eq!(
            editor_phase_label(crate::sketch_model::EditorToolPhase::Previewing),
            "IN PROGRESS"
        );
        assert_eq!(
            editor_phase_label(crate::sketch_model::EditorToolPhase::Committed),
            "APPLIED"
        );
        assert_eq!(
            editor_phase_label(crate::sketch_model::EditorToolPhase::Cancelled),
            "CANCELLED"
        );
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
