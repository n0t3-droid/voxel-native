//! Compact in-game toolbelt for mouse-look building.
//!
//! F3 opens the fast build/edit layer: pick a tool from icon chips, then
//! keep moving/flying while LMB/RMB works directly in the world. Weapons
//! are holstered for the whole edit state, including the tool picker.

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use crate::animation::AnimationStudio;
use crate::builder::{BuilderHistory, BuilderState};
use crate::city::{CityState, CityTool};
use crate::icons::{paint_icon, Icon};
use crate::menu::GameState;
use crate::mode::{ActiveMode, ModeContext};
use crate::settings::WorldSettings;
use crate::theme::{AMBER, TEXT};

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
            ToolbeltTool::DrawRect => "Rectangle Fill",
            ToolbeltTool::Sculpt => "Push Pull Face",
            ToolbeltTool::SmartTower => "Smart Tower",
            ToolbeltTool::BrushPlace => "Smart Builder",
            ToolbeltTool::BrushCut => "Smart Cut",
            ToolbeltTool::CityRoad => "Road Tool",
            ToolbeltTool::CityDistrict => "District Zone",
            ToolbeltTool::CityBuilding => "Building Shell",
            ToolbeltTool::CityFacade => "Facade Stamp",
            ToolbeltTool::AnimationPick => "Animation Picker",
        }
    }

    pub fn chip_label(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "NAV",
            ToolbeltTool::DrawRect => "FILL",
            ToolbeltTool::Sculpt => "PUSH",
            ToolbeltTool::SmartTower => "TOWER",
            ToolbeltTool::BrushPlace => "BUILD",
            ToolbeltTool::BrushCut => "CUT",
            ToolbeltTool::CityRoad => "ROAD",
            ToolbeltTool::CityDistrict => "ZONE",
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
            ToolbeltTool::DrawRect => "LMB fills rectangles. Alt+LMB temporarily Push/Pulls. G swaps Fill/Push.",
            ToolbeltTool::Sculpt => "LMB Push/Pulls faces. Alt+LMB temporarily fills rectangles. G swaps Fill/Push.",
            ToolbeltTool::SmartTower => "Two LMB clicks create a detailed skyscraper shell with floors, windows, crown, and undo.",
            ToolbeltTool::BrushPlace => "LMB starts a block point, drag to an endpoint, release to build; RMB uses the same gesture to cut.",
            ToolbeltTool::BrushCut => "LMB or RMB starts a cut point, drag to an endpoint, release to remove exact snapped blocks.",
            ToolbeltTool::CityRoad => "LMB draws roads: auto-snaps to endpoints/branches, continues from the last point, and inherits width, texture, and bridge height.",
            ToolbeltTool::CityDistrict => "LMB places a district/zone circle.",
            ToolbeltTool::CityBuilding => "LMB sets two corners for a solid building shell.",
            ToolbeltTool::CityFacade => "LMB stamps the active facade onto the targeted wall.",
            ToolbeltTool::AnimationPick => "LMB/RMB pick voxels for animation authoring.",
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
            tool: ToolbeltTool::BrushPlace,
            status:
                "Creative Smart Builder: LMB start -> drag to endpoint -> release builds; RMB cuts."
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
                toolbelt.tool = ToolbeltTool::BrushPlace;
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
                toolbelt.tool = ToolbeltTool::BrushPlace;
            }
            changed = true;
        }
        toolbelt.palette_open = !toolbelt.palette_open;
        if toolbelt.palette_open && toolbelt.tool == ToolbeltTool::Navigate {
            toolbelt.tool = ToolbeltTool::BrushPlace;
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
            toolbelt.tool = ToolbeltTool::BrushPlace;
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
                "Rectangle Fill: active drag cancelled. Smart Builder is one click away.".into();
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
    history: Res<BuilderHistory>,
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
        false
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
}

#[allow(clippy::too_many_arguments)]
fn draw_build_dock(
    active_tool: ToolbeltTool,
    picker_open: bool,
    status: &str,
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
        .stroke(egui::Stroke::new(1.15, colors.info))
        .inner_margin(egui::Margin::symmetric(12.0, 9.0))
        .rounding(egui::Rounding::same(10.0))
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
                    mouse_action_badge(
                        ui,
                        MouseGlyph::Left,
                        active_tool.left_icon(),
                        active_tool.category_color(),
                        active_tool.left_hint(),
                    );
                    mouse_action_badge(
                        ui,
                        MouseGlyph::Right,
                        active_tool.right_icon(),
                        alert_or_dim(active_tool.right_is_cancel(), dim),
                        active_tool.right_hint(),
                    );
                    if let Some((icon, hint)) = active_tool.wheel_action(picker_open) {
                        mouse_action_badge(ui, MouseGlyph::Wheel, icon, primary, hint);
                    }
                    ui.separator();
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
                    metric_chip(
                        ui,
                        Icon::Undo,
                        &undo_count.to_string(),
                        primary,
                        "Undo stack",
                    );
                    metric_chip(ui, Icon::Redo, &redo_count.to_string(), dim, "Redo stack");
                    ui.separator();
                    if live_chip(ui, true, picker_open, primary) {
                        result.toggle_picker = true;
                    }
                });

                if picker_open {
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

#[derive(Clone, Copy)]
enum MouseGlyph {
    Left,
    Right,
    Wheel,
}

fn mouse_action_badge(
    ui: &mut egui::Ui,
    button: MouseGlyph,
    icon: Icon,
    color: egui::Color32,
    hint: &'static str,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(64.0, 34.0), egui::Sense::hover());
    let hovered = response.hovered();
    let bg = if hovered {
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 70)
    } else {
        egui::Color32::from_rgba_unmultiplied(12, 28, 38, 168)
    };
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        egui::Rounding::same(8.0),
        bg,
        egui::Stroke::new(
            1.0,
            if hovered {
                color
            } else {
                color.linear_multiply(0.75)
            },
        ),
    );

    let mouse_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(7.0, 5.0), egui::vec2(18.0, 24.0));
    paint_mouse_glyph(&painter, mouse_rect, button, color);
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(34.0, 8.0), egui::vec2(18.0, 18.0)),
        icon,
        color,
    );
    response.on_hover_text(hint);
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

fn alert_or_dim(alert: bool, dim: egui::Color32) -> egui::Color32 {
    if alert {
        AMBER
    } else {
        dim
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

impl ToolbeltTool {
    fn left_icon(self) -> Icon {
        match self {
            ToolbeltTool::Navigate => Icon::Eye,
            ToolbeltTool::DrawRect => Icon::Grid,
            ToolbeltTool::Sculpt => Icon::Move,
            ToolbeltTool::SmartTower => Icon::City,
            ToolbeltTool::BrushPlace => Icon::Brush,
            ToolbeltTool::BrushCut => Icon::Eraser,
            ToolbeltTool::CityRoad => Icon::Road,
            ToolbeltTool::CityDistrict => Icon::District,
            ToolbeltTool::CityBuilding => Icon::City,
            ToolbeltTool::CityFacade => Icon::Open,
            ToolbeltTool::AnimationPick => Icon::Eye,
        }
    }

    fn right_icon(self) -> Icon {
        match self {
            ToolbeltTool::BrushPlace => Icon::Eraser,
            ToolbeltTool::Sculpt => Icon::Snap,
            ToolbeltTool::AnimationPick => Icon::Delete,
            ToolbeltTool::Navigate => Icon::ModeNavigate,
            _ => Icon::Close,
        }
    }

    fn right_is_cancel(self) -> bool {
        !matches!(
            self,
            ToolbeltTool::BrushPlace | ToolbeltTool::Sculpt | ToolbeltTool::AnimationPick
        )
    }

    fn left_hint(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "Inspect without editing",
            ToolbeltTool::DrawRect => "Left mouse fills; hold Alt for temporary Push/Pull",
            ToolbeltTool::Sculpt => "Left mouse Push/Pulls; hold Alt for temporary Fill",
            ToolbeltTool::SmartTower => "Left mouse chooses tower corners",
            ToolbeltTool::BrushPlace => "Left mouse starts a snapped build endpoint",
            ToolbeltTool::BrushCut => "Left mouse starts a snapped cut endpoint",
            ToolbeltTool::CityRoad => {
                "Left mouse auto-snap road points and branch from existing roads"
            }
            ToolbeltTool::CityDistrict => "Left mouse places a district",
            ToolbeltTool::CityBuilding => "Left mouse chooses building corners",
            ToolbeltTool::CityFacade => "Left mouse stamps the active facade",
            ToolbeltTool::AnimationPick => "Left mouse adds a voxel to the animation selection",
        }
    }

    fn right_hint(self) -> &'static str {
        match self {
            ToolbeltTool::Navigate => "Right mouse is reserved for inspect mode",
            ToolbeltTool::BrushPlace => "Right mouse starts a snapped cut endpoint",
            ToolbeltTool::Sculpt => "Right mouse sets Push/Pull reference points",
            ToolbeltTool::AnimationPick => {
                "Right mouse removes a voxel from the animation selection"
            }
            ToolbeltTool::DrawRect => "Right mouse cancels Fill; G swaps to Push",
            ToolbeltTool::SmartTower => "Right mouse cancels the tower preview",
            ToolbeltTool::BrushCut => "Right mouse starts a snapped cut endpoint",
            ToolbeltTool::CityRoad => {
                "Right mouse deletes the selected road component or cancels the current road"
            }
            ToolbeltTool::CityDistrict => "Right mouse removes the last district",
            ToolbeltTool::CityBuilding => "Right mouse removes or cancels the current building",
            ToolbeltTool::CityFacade => "Right mouse removes the last facade stamp",
        }
    }

    fn wheel_action(self, picker_open: bool) -> Option<(Icon, &'static str)> {
        if picker_open {
            Some((Icon::Rotate, "Mouse wheel cycles tools"))
        } else if self.uses_live_brush() {
            Some((Icon::Scale, "Mouse wheel resizes the live brush"))
        } else {
            None
        }
    }
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
    fn city_road_hint_exposes_smart_road_workflow() {
        let hint = ToolbeltTool::CityRoad.hint();

        assert!(hint.contains("auto-snaps"));
        assert!(hint.contains("continues"));
        assert!(hint.contains("inherits"));
        assert!(hint.contains("bridge height"));
    }

    #[test]
    fn city_road_mouse_hints_explain_fast_branching_and_component_delete() {
        let left = ToolbeltTool::CityRoad.left_hint();
        let right = ToolbeltTool::CityRoad.right_hint();

        assert!(left.contains("auto-snap"));
        assert!(left.contains("branch"));
        assert!(right.contains("selected road component"));
        assert!(right.contains("cancel"));
    }
}
