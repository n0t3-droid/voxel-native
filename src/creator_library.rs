//! One canonical asset library for building materials and placeable vehicles.
//!
//! The library owns discovery and selection only. Applying an action goes
//! through [`apply_creator_library_action`] so the inventory and in-world
//! editor cannot drift into different placement workflows again.

use bevy::prelude::*;
use bevy_egui::egui;

use crate::blocks::{block_palette_catalog, block_palette_entry, BlockPaletteEntry, BlockType};
use crate::builder::BuilderState;
use crate::mode::{ActiveMode, ModeContext};
use crate::ships::{ShipInventory, ShipKind, ShipPlacementState};
use crate::theme::ThemeSettings;

pub struct CreatorLibraryPlugin;

impl Plugin for CreatorLibraryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CreatorLibraryState>();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorAssetId {
    Material(BlockType),
    Ship(ShipKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreatorCategory {
    #[default]
    All,
    Materials,
    Vehicles,
}

impl CreatorCategory {
    const ALL: [(Self, &'static str); 3] = [
        (Self::All, "ALL"),
        (Self::Materials, "MATERIALS"),
        (Self::Vehicles, "SHUTTLES"),
    ];
}

#[derive(Resource, Debug, Clone)]
pub struct CreatorLibraryState {
    pub query: String,
    pub category: CreatorCategory,
    pub selected: CreatorAssetId,
    pub status: String,
}

impl Default for CreatorLibraryState {
    fn default() -> Self {
        Self {
            query: String::new(),
            category: CreatorCategory::All,
            selected: CreatorAssetId::Material(BlockType::Stone),
            status: "Choose a material or drag a shuttle into the world.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorLibraryAction {
    SelectMaterial(BlockType),
    BeginShipPlacement { kind: ShipKind, drag: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreatorLibraryEffect {
    MaterialSelected,
    PlacementStarted,
}

#[derive(Debug, Default)]
pub struct CreatorLibraryUiResult {
    pub action: Option<CreatorLibraryAction>,
}

/// Apply a library action through one state transition shared by every UI.
pub fn apply_creator_library_action(
    action: CreatorLibraryAction,
    library: &mut CreatorLibraryState,
    builder: &mut BuilderState,
    ships: &mut ShipInventory,
    placement: &mut ShipPlacementState,
    mode: &mut ModeContext,
) -> CreatorLibraryEffect {
    match action {
        CreatorLibraryAction::SelectMaterial(block) => {
            builder.block = block;
            library.selected = CreatorAssetId::Material(block);
            library.status = format!(
                "{} is active for every build surface.",
                material_name(block)
            );
            builder.status = library.status.clone();
            CreatorLibraryEffect::MaterialSelected
        }
        CreatorLibraryAction::BeginShipPlacement { kind, drag } => {
            // Switching assets while a preview is already active must still
            // return to the mode that opened placement, not to ShipPlacement.
            let return_mode = if placement.is_active() {
                placement.return_mode
            } else {
                mode.mode
            };
            ships.selected = kind;
            library.selected = CreatorAssetId::Ship(kind);
            library.status = if drag {
                format!("Drag {} onto a valid surface.", kind.label())
            } else {
                format!("Move {} onto a surface and click to place.", kind.label())
            };
            if drag {
                placement.start_drag(kind, return_mode);
            } else {
                placement.start_ready(kind, return_mode);
            }
            mode.set(
                ActiveMode::ShipPlacement { kind },
                format!(
                    "Placing {} // wheel rotates // RMB or Esc cancels.",
                    kind.label()
                ),
            );
            CreatorLibraryEffect::PlacementStarted
        }
    }
}

pub fn draw_creator_library(
    ui: &mut egui::Ui,
    state: &mut CreatorLibraryState,
    active_block: BlockType,
    ships: &ShipInventory,
    compact: bool,
    theme: ThemeSettings,
) -> CreatorLibraryUiResult {
    if matches!(state.selected, CreatorAssetId::Material(block) if block != active_block) {
        state.selected = CreatorAssetId::Material(active_block);
    }

    let colors = theme.semantic();
    let mut result = CreatorLibraryUiResult::default();

    ui.horizontal_wrapped(|ui| {
        let search_width = if compact {
            ui.available_width().max(150.0)
        } else {
            ui.available_width().min(310.0).max(180.0)
        };
        ui.allocate_ui(egui::vec2(search_width, 30.0), |ui| {
            ui.set_width(search_width);
            crate::ui_kit::search_box(
                ui,
                &mut state.query,
                "Search materials or shuttles...",
                theme,
            );
        });
        for (category, label) in CreatorCategory::ALL {
            let width = if compact { 91.0 } else { 104.0 };
            if crate::ui_kit::choice_chip_sized(ui, label, state.category == category, width, theme)
                .clicked()
            {
                state.category = category;
            }
        }
    });

    ui.add_space(8.0);
    crate::ui_kit::compact_separator(ui, theme);
    ui.add_space(8.0);

    let query = state.query.trim().to_ascii_lowercase();
    let available = ui.available_width().max(160.0);
    let columns: usize = if compact && available < 300.0 {
        1
    } else if compact {
        2
    } else if available >= 760.0 {
        4
    } else if available >= 510.0 {
        3
    } else {
        2
    };
    let gap = 7.0;
    let card_width =
        ((available - gap * (columns.saturating_sub(1)) as f32) / columns as f32).max(132.0);

    let mut visible_any = false;
    egui::Grid::new(("creator_library_grid", compact))
        .num_columns(columns)
        .spacing(egui::vec2(gap, 7.0))
        .show(ui, |ui| {
            let mut cell = 0usize;
            if state.category != CreatorCategory::Vehicles {
                for category in block_palette_catalog() {
                    for entry in category.entries {
                        if !material_matches(entry, category.label, category.hint, &query) {
                            continue;
                        }
                        visible_any = true;
                        let selected = state.selected == CreatorAssetId::Material(entry.block);
                        let response = crate::ui_kit::swatch_card(
                            ui,
                            block_color(entry.block),
                            entry.label,
                            entry.role,
                            selected,
                            egui::vec2(card_width, 66.0),
                            theme,
                        );
                        if response.clicked() {
                            state.selected = CreatorAssetId::Material(entry.block);
                            result.action = Some(CreatorLibraryAction::SelectMaterial(entry.block));
                        }
                        cell += 1;
                        if cell % columns == 0 {
                            ui.end_row();
                        }
                    }
                }
            }

            if state.category != CreatorCategory::Materials {
                for kind in ShipKind::ALL {
                    if !ship_matches(kind, &query) {
                        continue;
                    }
                    visible_any = true;
                    let unlocked = ships.unlocked.contains(&kind);
                    let selected = state.selected == CreatorAssetId::Ship(kind);
                    let response = ship_asset_card(
                        ui,
                        kind,
                        selected,
                        unlocked,
                        egui::vec2(card_width, 78.0),
                        theme,
                    );
                    if unlocked && response.drag_started() {
                        state.selected = CreatorAssetId::Ship(kind);
                        result.action =
                            Some(CreatorLibraryAction::BeginShipPlacement { kind, drag: true });
                    } else if unlocked && response.clicked() {
                        state.selected = CreatorAssetId::Ship(kind);
                        result.action =
                            Some(CreatorLibraryAction::BeginShipPlacement { kind, drag: false });
                    }
                    cell += 1;
                    if cell % columns == 0 {
                        ui.end_row();
                    }
                }
            }
        });

    if !visible_any {
        ui.add_space(16.0);
        ui.label(
            egui::RichText::new("No creator assets match this search.")
                .monospace()
                .size(11.0)
                .color(colors.text_muted),
        );
    }

    ui.add_space(10.0);
    crate::ui_kit::compact_separator(ui, theme);
    ui.add_space(8.0);
    draw_selected_asset_summary(ui, state, ships, theme);

    result
}

fn draw_selected_asset_summary(
    ui: &mut egui::Ui,
    state: &CreatorLibraryState,
    ships: &ShipInventory,
    theme: ThemeSettings,
) {
    let colors = theme.semantic();
    crate::ui_kit::surface_panel_animated(
        ui,
        theme,
        egui::Id::new("creator_library_selected_asset"),
        true,
        |ui| {
            ui.horizontal(|ui| {
                let (label, detail, color) = match state.selected {
                    CreatorAssetId::Material(block) => {
                        let entry = block_palette_entry(block);
                        (
                            entry.map(|item| item.label).unwrap_or("Material"),
                            entry
                                .map(|item| item.role)
                                .unwrap_or("Active build material"),
                            block_color(block),
                        )
                    }
                    CreatorAssetId::Ship(kind) => {
                        let available = ships.unlocked.contains(&kind);
                        (
                            kind.label(),
                            if available {
                                "Drag into the world, or click and place"
                            } else {
                                "Blueprint locked"
                            },
                            ship_accent(kind),
                        )
                    }
                };
                let (swatch, _) =
                    ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
                crate::ui_kit::paint_material_swatch(ui.painter(), swatch, color, 4.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(label)
                            .monospace()
                            .size(11.0)
                            .strong()
                            .color(colors.text),
                    );
                    ui.label(
                        egui::RichText::new(detail)
                            .monospace()
                            .size(9.5)
                            .color(colors.text_muted),
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(&state.status)
                    .monospace()
                    .size(9.5)
                    .color(colors.info),
            );
        },
    );
}

fn ship_asset_card(
    ui: &mut egui::Ui,
    kind: ShipKind,
    selected: bool,
    unlocked: bool,
    size: egui::Vec2,
    theme: ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let sense = if unlocked {
        egui::Sense::click_and_drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let hovered = unlocked && response.hovered();
    let fill = if !unlocked {
        colors.surface_disabled
    } else if selected {
        colors.surface_active
    } else if hovered {
        colors.surface_hover
    } else {
        colors.surface
    };
    let outline = if !unlocked {
        colors.outline_disabled
    } else if selected {
        colors.outline_active
    } else if hovered {
        colors.outline_hover
    } else {
        colors.outline
    };
    let painter = ui.painter_at(rect.expand(3.0));
    painter.rect_filled(rect, egui::Rounding::same(6.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(6.0),
        egui::Stroke::new(if selected { 1.7 } else { 1.0 }, outline),
    );
    if selected || hovered {
        crate::theme::paint_neon_outline(
            &painter,
            rect.expand(2.0),
            7.0,
            colors.focus_glow,
            outline,
            if selected { 0.9 } else { 0.5 },
        );
    }

    let preview = egui::Rect::from_min_max(
        rect.min + egui::vec2(8.0, 7.0),
        egui::pos2(rect.right() - 8.0, rect.center().y + 9.0),
    );
    paint_shuttle_top_view(&painter, preview, kind, if unlocked { 1.0 } else { 0.35 });
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 18.0),
        egui::Align2::CENTER_CENTER,
        kind.short(),
        egui::FontId::monospace(10.5),
        if unlocked {
            colors.text
        } else {
            colors.text_disabled
        },
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 7.0),
        egui::Align2::CENTER_CENTER,
        if unlocked { "DRAG OR CLICK" } else { "LOCKED" },
        egui::FontId::monospace(7.5),
        if unlocked {
            colors.text_muted
        } else {
            colors.text_disabled
        },
    );

    response.on_hover_text(if unlocked {
        format!(
            "{}: drag directly onto terrain, or click then place. Mouse wheel rotates.",
            kind.label()
        )
    } else {
        format!("{} blueprint is locked.", kind.label())
    })
}

fn paint_shuttle_top_view(painter: &egui::Painter, rect: egui::Rect, kind: ShipKind, opacity: f32) {
    let center = rect.center();
    let half_w = rect.width() * 0.44;
    let half_h = rect.height() * 0.38;
    let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let accent = ship_accent(kind);
    let accent = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha);
    let hull = egui::Color32::from_rgba_unmultiplied(184, 211, 220, alpha);
    let glass = egui::Color32::from_rgba_unmultiplied(35, 218, 240, alpha);
    let points = [
        egui::pos2(center.x, center.y - half_h),
        egui::pos2(center.x + half_w * 0.22, center.y - half_h * 0.2),
        egui::pos2(center.x + half_w, center.y + half_h * 0.42),
        egui::pos2(center.x + half_w * 0.2, center.y + half_h * 0.22),
        egui::pos2(center.x + half_w * 0.28, center.y + half_h),
        egui::pos2(center.x, center.y + half_h * 0.58),
        egui::pos2(center.x - half_w * 0.28, center.y + half_h),
        egui::pos2(center.x - half_w * 0.2, center.y + half_h * 0.22),
        egui::pos2(center.x - half_w, center.y + half_h * 0.42),
        egui::pos2(center.x - half_w * 0.22, center.y - half_h * 0.2),
    ];
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        hull,
        egui::Stroke::new(1.2, accent),
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x, center.y - half_h * 0.56),
            egui::pos2(center.x + half_w * 0.14, center.y - half_h * 0.05),
            egui::pos2(center.x, center.y + half_h * 0.18),
            egui::pos2(center.x - half_w * 0.14, center.y - half_h * 0.05),
        ],
        glass,
        egui::Stroke::new(0.8, accent),
    ));
    painter.line_segment(
        [
            egui::pos2(center.x - half_w * 0.42, center.y + half_h * 0.46),
            egui::pos2(center.x + half_w * 0.42, center.y + half_h * 0.46),
        ],
        egui::Stroke::new(2.0, accent),
    );
}

fn block_color(block: BlockType) -> egui::Color32 {
    let color = block.color().to_srgba();
    egui::Color32::from_rgba_unmultiplied(
        (color.red * 255.0).round() as u8,
        (color.green * 255.0).round() as u8,
        (color.blue * 255.0).round() as u8,
        255,
    )
}

fn ship_accent(kind: ShipKind) -> egui::Color32 {
    match kind {
        ShipKind::ScoutShuttle => egui::Color32::from_rgb(65, 225, 245),
        ShipKind::StrikeFighter => egui::Color32::from_rgb(255, 92, 176),
        ShipKind::HeavyDropship => egui::Color32::from_rgb(255, 190, 67),
    }
}

fn material_name(block: BlockType) -> &'static str {
    block_palette_entry(block)
        .map(|entry| entry.label)
        .unwrap_or("Material")
}

fn material_matches(entry: &BlockPaletteEntry, category: &str, hint: &str, query: &str) -> bool {
    query.is_empty()
        || entry.label.to_ascii_lowercase().contains(query)
        || entry.role.to_ascii_lowercase().contains(query)
        || category.to_ascii_lowercase().contains(query)
        || hint.to_ascii_lowercase().contains(query)
}

fn ship_matches(kind: ShipKind, query: &str) -> bool {
    query.is_empty()
        || kind.label().to_ascii_lowercase().contains(query)
        || kind.short().to_ascii_lowercase().contains(query)
        || "shuttle vehicle spacecraft hangar"
            .split_whitespace()
            .any(|word| word.contains(query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_search_matches_name_role_and_category() {
        let stone = block_palette_entry(BlockType::Stone).unwrap();
        assert!(material_matches(
            &stone,
            "Asphalt & Concrete",
            "roads and foundations",
            "stone"
        ));
        assert!(material_matches(
            &stone,
            "Asphalt & Concrete",
            "roads and foundations",
            "foundation"
        ));
        assert!(!material_matches(
            &stone,
            "Asphalt & Concrete",
            "roads and foundations",
            "sakura"
        ));
    }

    #[test]
    fn ship_search_uses_human_and_workflow_terms() {
        assert!(ship_matches(ShipKind::ScoutShuttle, "scout"));
        assert!(ship_matches(ShipKind::ScoutShuttle, "spacecraft"));
        assert!(!ship_matches(ShipKind::ScoutShuttle, "limestone"));
    }

    #[test]
    fn material_action_updates_canonical_builder_selection() {
        let mut library = CreatorLibraryState::default();
        let mut builder = BuilderState::default();
        let mut ships = ShipInventory::default();
        let mut placement = ShipPlacementState::default();
        let mut mode = ModeContext::default();

        let effect = apply_creator_library_action(
            CreatorLibraryAction::SelectMaterial(BlockType::ZenStone),
            &mut library,
            &mut builder,
            &mut ships,
            &mut placement,
            &mut mode,
        );

        assert_eq!(effect, CreatorLibraryEffect::MaterialSelected);
        assert_eq!(builder.block, BlockType::ZenStone);
        assert_eq!(builder.status, library.status);
        assert_eq!(
            library.selected,
            CreatorAssetId::Material(BlockType::ZenStone)
        );
    }

    #[test]
    fn ship_action_preserves_mode_for_cancel_and_starts_drag() {
        let mut library = CreatorLibraryState::default();
        let mut builder = BuilderState::default();
        let mut ships = ShipInventory::default();
        let mut placement = ShipPlacementState::default();
        let mut mode = ModeContext::default();
        let return_mode = mode.mode;

        let effect = apply_creator_library_action(
            CreatorLibraryAction::BeginShipPlacement {
                kind: ShipKind::StrikeFighter,
                drag: true,
            },
            &mut library,
            &mut builder,
            &mut ships,
            &mut placement,
            &mut mode,
        );

        assert_eq!(effect, CreatorLibraryEffect::PlacementStarted);
        assert_eq!(ships.selected, ShipKind::StrikeFighter);
        assert_eq!(placement.return_mode, return_mode);
        assert_eq!(
            placement.phase,
            crate::ships::ShipPlacementPhase::PointerHeld(
                crate::ships::PlacementPointerSource::CreatorLibrary
            )
        );
        assert_eq!(
            mode.mode,
            ActiveMode::ShipPlacement {
                kind: ShipKind::StrikeFighter
            }
        );
    }

    #[test]
    fn changing_ship_during_placement_keeps_the_original_return_mode() {
        let mut library = CreatorLibraryState::default();
        let mut builder = BuilderState::default();
        let mut ships = ShipInventory::default();
        let mut placement = ShipPlacementState::default();
        let mut mode = ModeContext::default();
        let return_mode = ActiveMode::BuildLive {
            tool: crate::toolbelt::ToolbeltTool::DrawRect,
        };
        mode.set(return_mode, "Editing.");

        apply_creator_library_action(
            CreatorLibraryAction::BeginShipPlacement {
                kind: ShipKind::ScoutShuttle,
                drag: false,
            },
            &mut library,
            &mut builder,
            &mut ships,
            &mut placement,
            &mut mode,
        );
        apply_creator_library_action(
            CreatorLibraryAction::BeginShipPlacement {
                kind: ShipKind::HeavyDropship,
                drag: true,
            },
            &mut library,
            &mut builder,
            &mut ships,
            &mut placement,
            &mut mode,
        );

        assert_eq!(placement.return_mode, return_mode);
        assert_eq!(placement.kind, ShipKind::HeavyDropship);
    }
}
