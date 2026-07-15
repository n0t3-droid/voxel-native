//! Menu system: Main-Menu, Pause-Menu, Inventory, Game-State transitions.
//!
//! Minecraft-style flow:
//!   * Start -> MainMenu (Neue Welt / Welt laden / Einstellungen / Beenden)
//!   * InGame + ESC -> Paused (Weiter / Speichern / Einstellungen / Hauptmenue / Beenden)
//!   * InGame + E   -> Inventory (block palette grid)
//!   * F3           -> build toolbelt / editor mode (via toolbelt.rs)
//!   * Shift+F3     -> debug overlay toggle (via hud.rs)
//!   * Space double -> toggle fly (via player.rs)

use std::time::Duration;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts};

use crate::commands::CommandPaletteState;
use crate::editor::EditorState;
use crate::hud::HotbarState;
use crate::icons::Icon;
use crate::mode::ModeContext;
use crate::player::Player;
use crate::player::PlayerProgressScratch;
use crate::settings::{self, ActiveWorld, SceneryQuality, WorldMeta, WorldSettings};
use crate::theme::{command_frame, metric_pill};
use crate::ui_kit::{ActionTone, LoadingState};
use crate::world::{ChunkStreamer, VoxelWorld};

const START_TITLE: &str = "R93G SAKURA ZEN";
const START_SUBTITLE: &str = "mouse-first sketch dojo // blossom worlds // fast low-end streaming";

#[derive(Clone, Copy, PartialEq, Eq)]
enum InventoryPage {
    Blocks,
    Ships,
    Companions,
    Hotbar,
}

const ALL_INVENTORY_CATEGORIES: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InventoryBlockEntry {
    block: crate::blocks::BlockType,
    label: &'static str,
    role: &'static str,
    category: usize,
}

fn inventory_block_entries() -> Vec<InventoryBlockEntry> {
    crate::blocks::block_palette_catalog()
        .iter()
        .enumerate()
        .flat_map(|(category, group)| {
            group.entries.iter().map(move |entry| InventoryBlockEntry {
                block: entry.block,
                label: entry.label,
                role: entry.role,
                category,
            })
        })
        .collect()
}

fn inventory_entry_matches(
    entry: InventoryBlockEntry,
    category_label: &str,
    query_lower: &str,
) -> bool {
    query_lower.is_empty()
        || [entry.label, entry.role, category_label]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains(query_lower))
}

fn apply_inventory_block_selection(
    builder: &mut crate::builder::BuilderState,
    block: crate::blocks::BlockType,
) {
    builder.block = block;
    builder.status = format!(
        "{} selected from inventory.",
        crate::blocks::block_label(block)
    );
}

fn menu_letter_shortcuts_enabled(wants_keyboard_input: bool) -> bool {
    !wants_keyboard_input
}

#[derive(States, Clone, Eq, PartialEq, Debug, Hash, Default)]
pub enum GameState {
    #[default]
    MainMenu,
    InGame,
    Paused,
}

#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PauseScreen {
    #[default]
    Menu,
    Inventory,
    // Options is handled via editor.rs (state.open == true).
}

#[derive(Resource, Default)]
pub struct NewWorldForm {
    pub name: String,
    pub seed_text: String,
}

#[derive(Resource, Debug, Default)]
struct StartMenuState {
    selected_world: Option<String>,
    text_field_focused: bool,
    show_blueprints: bool,
}

#[derive(Resource, Default)]
struct PendingWorldDelete(Option<String>);

impl PendingWorldDelete {
    fn arm_or_confirm(&mut self, world: &str) -> bool {
        if self.0.as_deref() == Some(world) {
            self.0 = None;
            true
        } else {
            self.0 = Some(world.to_owned());
            false
        }
    }

    fn is_armed(&self, world: &str) -> bool {
        self.0.as_deref() == Some(world)
    }
}

const PAUSE_NOTICE_ID: &str = "r93g_pause_notice";

#[derive(Clone, Debug, PartialEq)]
struct PauseNotice {
    label: String,
    value: String,
    expires_at: f64,
}

impl PauseNotice {
    fn is_active_at(&self, now: f64) -> bool {
        now.is_finite() && self.expires_at.is_finite() && now < self.expires_at
    }
}

fn set_pause_notice(ctx: &egui::Context, label: &str, value: impl Into<String>) {
    let now = ctx.input(|input| input.time);
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(PAUSE_NOTICE_ID),
            PauseNotice {
                label: label.to_owned(),
                value: value.into(),
                expires_at: now + 2.4,
            },
        );
    });
}

fn active_pause_notice(ctx: &egui::Context) -> Option<PauseNotice> {
    let id = egui::Id::new(PAUSE_NOTICE_ID);
    let now = ctx.input(|input| input.time);
    let notice = ctx.data(|data| data.get_temp::<PauseNotice>(id));
    match notice {
        Some(notice) if notice.is_active_at(now) => {
            ctx.request_repaint_after(Duration::from_secs_f64(
                (notice.expires_at - now).max(0.001),
            ));
            Some(notice)
        }
        Some(_) => {
            ctx.data_mut(|data| data.remove::<PauseNotice>(id));
            None
        }
        None => None,
    }
}

/// Set to `true` by the main menu when a fresh world is created/loaded,
/// so `OnEnter(InGame)` systems know to regenerate terrain and teleport
/// the player. Returning from Options/Pause leaves this `false`, so the
/// player stays exactly where they were.
#[derive(Resource, Default)]
pub struct PendingWorldLoad(pub bool);

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_state::<GameState>()
            .insert_resource(PauseScreen::default())
            .insert_resource(NewWorldForm::default())
            .insert_resource(StartMenuState::default())
            .insert_resource(PendingWorldDelete::default())
            .insert_resource(PendingWorldLoad::default())
            .add_systems(Update, handle_keys)
            .add_systems(
                Update,
                (
                    draw_main_menu.run_if(in_state(GameState::MainMenu)),
                    draw_pause_menu.run_if(in_state(GameState::Paused)),
                    draw_inventory_menu.run_if(in_state(GameState::Paused)),
                    on_game_start,
                ),
            )
            .add_systems(Last, clear_pending_load.run_if(in_state(GameState::InGame)));
    }
}

/// Clears the "fresh world load" flag after the first InGame frame, so
/// OnEnter(InGame) systems only teleport/regenerate on real world loads.
fn clear_pending_load(mut pending: ResMut<PendingWorldLoad>) {
    if pending.0 {
        pending.0 = false;
    }
}

/// ESC and E drive the state machine. The editor window close button also
/// flips PauseScreen back to Menu, but key handling lives here for clarity.
fn handle_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut contexts: EguiContexts,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    mut editor: ResMut<EditorState>,
    command_palette: Option<ResMut<CommandPaletteState>>,
    mode: Option<Res<ModeContext>>,
) {
    let allow_letter_shortcuts = contexts
        .try_ctx_mut()
        .map(|ctx| menu_letter_shortcuts_enabled(ctx.wants_keyboard_input()))
        .unwrap_or(false);

    if let Some(mut command_palette) = command_palette {
        if command_palette.open {
            if keys.just_pressed(KeyCode::Escape) {
                command_palette.close();
            }
            return;
        }
    }

    match state.get() {
        GameState::InGame => {
            // E cycles Build Studio tools — don't open inventory mid-build.
            // ESC always opens pause so you are never trapped in a tool gesture.
            if mode.as_deref().map(|m| m.is_build()).unwrap_or(false)
                && keys.just_pressed(KeyCode::KeyE)
            {
                return;
            }
            if keys.just_pressed(KeyCode::Escape) {
                *pause_screen = PauseScreen::Menu;
                editor.open = false;
                next.set(GameState::Paused);
            } else if allow_letter_shortcuts && keys.just_pressed(KeyCode::KeyE) {
                *pause_screen = PauseScreen::Inventory;
                editor.open = false;
                next.set(GameState::Paused);
            }
        }
        GameState::Paused => {
            if keys.just_pressed(KeyCode::Escape) {
                // ESC from Options -> pause menu; from pause menu / inventory -> InGame.
                if editor.open {
                    editor.open = false;
                    *pause_screen = PauseScreen::Menu;
                } else {
                    next.set(GameState::InGame);
                }
            }
            if allow_letter_shortcuts
                && keys.just_pressed(KeyCode::KeyE)
                && *pause_screen == PauseScreen::Inventory
                && !editor.open
            {
                next.set(GameState::InGame);
            }
        }
        GameState::MainMenu => {
            if keys.just_pressed(KeyCode::Escape) && editor.open {
                editor.open = false;
            }
        }
    }
}

/// Menus release the cursor. In-game capture is owned centrally by
/// `mode_cursor_guard`, so build/combat transitions are deterministic.
fn on_game_start(
    state: Res<State<GameState>>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if !state.is_changed() {
        return;
    }
    let Ok(mut window) = windows.get_single_mut() else {
        return;
    };
    match state.get() {
        GameState::InGame => { /* cursor captured by first click */ }
        _ => {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
    }
}

// ============================ Main Menu ===================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartMenuDensity {
    Compact,
    Standard,
    Wide,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StartMenuLayout {
    density: StartMenuDensity,
    content_width: f32,
    outer_margin: f32,
    title_size: f32,
    show_subtitle: bool,
    world_columns: usize,
    world_list_height: f32,
}

impl StartMenuLayout {
    fn uses_split_rows(self) -> bool {
        self.density != StartMenuDensity::Compact
    }
}

fn start_menu_layout(viewport: egui::Vec2, world_count: usize) -> StartMenuLayout {
    let safe_width = if viewport.x.is_finite() {
        viewport.x.max(1.0)
    } else {
        320.0
    };
    let safe_height = if viewport.y.is_finite() {
        viewport.y.max(1.0)
    } else {
        280.0
    };
    let density = if safe_width < 680.0 || safe_height < 560.0 {
        StartMenuDensity::Compact
    } else if safe_width < 1120.0 {
        StartMenuDensity::Standard
    } else {
        StartMenuDensity::Wide
    };
    let outer_margin = match density {
        StartMenuDensity::Compact => 12.0,
        StartMenuDensity::Standard => 22.0,
        StartMenuDensity::Wide => 32.0,
    };
    let available_content = (safe_width - outer_margin * 2.0).max(1.0);
    let content_width = match density {
        StartMenuDensity::Compact => safe_width - outer_margin * 2.0,
        StartMenuDensity::Standard => (safe_width - outer_margin * 2.0).min(820.0),
        StartMenuDensity::Wide => (safe_width - outer_margin * 2.0).min(1080.0),
    }
    .max(280.0)
    .min(available_content);
    let title_size = match density {
        StartMenuDensity::Compact => 25.0,
        StartMenuDensity::Standard => 30.0,
        StartMenuDensity::Wide => 34.0,
    };
    let world_columns = if density == StartMenuDensity::Wide
        || (density == StartMenuDensity::Standard && content_width >= 780.0)
    {
        2
    } else {
        1
    };
    let visible_rows = match density {
        StartMenuDensity::Compact => 2usize,
        StartMenuDensity::Standard => 3,
        StartMenuDensity::Wide => 4,
    };
    let world_rows = world_count.div_ceil(world_columns);
    let world_list_height = if world_count == 0 {
        72.0
    } else {
        (world_rows.min(visible_rows) as f32 * 94.0 + 4.0).min((safe_height * 0.36).max(98.0))
    };

    StartMenuLayout {
        density,
        content_width,
        outer_margin,
        title_size,
        show_subtitle: safe_width >= 520.0 && safe_height >= 440.0,
        world_columns,
        world_list_height,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorldQualityTier {
    Minimal,
    Efficient,
    Balanced,
    Immersive,
}

impl WorldQualityTier {
    fn from_quality(quality: SceneryQuality) -> Self {
        match quality {
            SceneryQuality::Off => Self::Minimal,
            SceneryQuality::Lean => Self::Efficient,
            SceneryQuality::Balanced => Self::Balanced,
            SceneryQuality::Lush => Self::Immersive,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Minimal => "MINIMAL",
            Self::Efficient => "EFFICIENT",
            Self::Balanced => "BALANCED",
            Self::Immersive => "IMMERSIVE",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Minimal => "SCENERY OFF",
            Self::Efficient => "LEAN SCENERY",
            Self::Balanced => "BALANCED SCENERY",
            Self::Immersive => "LUSH SCENERY",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorldSelectionMove {
    Previous,
    Next,
    First,
    Last,
}

fn sync_start_menu_selection(state: &mut StartMenuState, worlds: &[WorldMeta]) {
    let selection_is_valid = state
        .selected_world
        .as_deref()
        .is_some_and(|name| worlds.iter().any(|world| world.name == name));
    if selection_is_valid {
        return;
    }

    state.selected_world = worlds
        .iter()
        .max_by_key(|world| world.last_played_epoch)
        .map(|world| world.name.clone());
}

fn moved_world_selection(
    selected_world: Option<&str>,
    worlds: &[WorldMeta],
    movement: WorldSelectionMove,
) -> Option<String> {
    if worlds.is_empty() {
        return None;
    }

    let current = selected_world
        .and_then(|name| worlds.iter().position(|world| world.name == name))
        .unwrap_or(0);
    let index = match movement {
        WorldSelectionMove::Previous => current.checked_sub(1).unwrap_or(worlds.len() - 1),
        WorldSelectionMove::Next => (current + 1) % worlds.len(),
        WorldSelectionMove::First => 0,
        WorldSelectionMove::Last => worlds.len() - 1,
    };
    Some(worlds[index].name.clone())
}

fn start_menu_navigation_enabled(text_field_focused: bool, overlay_open: bool) -> bool {
    !text_field_focused && !overlay_open
}

fn fitted_start_text_size(text: &str, max_width: f32, preferred: f32, minimum: f32) -> f32 {
    if text.is_empty() || !max_width.is_finite() || max_width <= 0.0 {
        return preferred.max(minimum);
    }
    let estimated_width = text.chars().count() as f32 * preferred * 0.61;
    if estimated_width <= max_width {
        preferred
    } else {
        (preferred * max_width / estimated_width).clamp(minimum, preferred)
    }
}

fn inventory_grid_metrics(available_width: f32) -> (usize, f32) {
    const MAX_COLUMNS: usize = 5;
    const MIN_TILE_WIDTH: f32 = 148.0;
    const GAP: f32 = 10.0;

    let available_width = if available_width.is_finite() {
        available_width.max(112.0)
    } else {
        MIN_TILE_WIDTH
    };
    let columns =
        (((available_width + GAP) / (MIN_TILE_WIDTH + GAP)).floor() as usize).clamp(1, MAX_COLUMNS);
    let tile_width =
        ((available_width - GAP * columns.saturating_sub(1) as f32) / columns as f32).max(112.0);
    (columns, tile_width)
}

fn start_menu_command(
    ui: &mut egui::Ui,
    label: &str,
    detail: Option<&str>,
    primary: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    let height = if primary { 54.0 } else { 38.0 };
    crate::ui_kit::command_action(
        ui,
        label,
        detail,
        if primary {
            ActionTone::Primary
        } else {
            ActionTone::Standard
        },
        height,
        theme,
    )
}

fn start_menu_footer_action(
    ui: &mut egui::Ui,
    label: &str,
    danger: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    crate::ui_kit::command_action(
        ui,
        label,
        None,
        if danger {
            ActionTone::Danger
        } else {
            ActionTone::Standard
        },
        36.0,
        theme,
    )
}

const START_WORLD_CARD_HEIGHT: f32 = 88.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct StartWorldCardEvent {
    select: bool,
    open: bool,
}

fn world_edit_summary(edited_chunks: usize) -> String {
    match edited_chunks {
        0 => "PRISTINE".to_owned(),
        1 => "1 EDITED CHUNK".to_owned(),
        count => format!("{count} EDITED CHUNKS"),
    }
}

fn start_menu_alpha(color: egui::Color32, alpha: u8) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn draw_start_world_card(
    ui: &mut egui::Ui,
    meta: &WorldMeta,
    ordinal: usize,
    selected: bool,
    latest: bool,
    reveal_selection: bool,
    theme: crate::theme::ThemeSettings,
) -> StartWorldCardEvent {
    let colors = theme.semantic();
    let size = egui::vec2(ui.available_width().max(1.0), START_WORLD_CARD_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let focused = response.has_focus();
    let fill = if selected {
        colors.surface_active
    } else if response.hovered() || focused {
        colors.surface_hover
    } else {
        colors.surface
    };
    let outline = if selected {
        colors.outline_active
    } else if response.hovered() {
        colors.outline_hover
    } else {
        colors.outline
    };
    let painter = ui.painter_at(rect.expand(5.0));
    painter.rect_filled(rect, egui::Rounding::same(5.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(5.0),
        egui::Stroke::new(if selected { 1.5 } else { 1.0 }, outline),
    );
    if selected {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            egui::Rounding {
                nw: 5.0,
                sw: 5.0,
                ne: 0.0,
                se: 0.0,
            },
            colors.accent,
        );
    }
    crate::theme::paint_focus_outline(&painter, rect, colors, if focused { 1.0 } else { 0.0 });

    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 24.0, rect.top() + 27.0),
        egui::vec2(20.0, 20.0),
    );
    crate::icons::paint_icon(
        &painter,
        icon_rect,
        Icon::Globe,
        if selected {
            colors.accent
        } else {
            colors.text_muted
        },
    );

    let quality = WorldQualityTier::from_quality(meta.scenery_quality);
    let quality_color = match quality {
        WorldQualityTier::Minimal => colors.warning,
        WorldQualityTier::Efficient => colors.info,
        WorldQualityTier::Balanced => colors.success,
        WorldQualityTier::Immersive => colors.accent,
    };
    let quality_rect = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 98.0, rect.top() + 11.0),
        egui::vec2(86.0, 20.0),
    );
    painter.rect_filled(
        quality_rect,
        egui::Rounding::same(4.0),
        start_menu_alpha(quality_color, 28),
    );
    painter.rect_stroke(
        quality_rect,
        egui::Rounding::same(4.0),
        egui::Stroke::new(1.0, start_menu_alpha(quality_color, 142)),
    );
    painter.text(
        quality_rect.center(),
        egui::Align2::CENTER_CENTER,
        quality.label(),
        egui::FontId::monospace(9.0),
        quality_color,
    );

    let title_x = rect.left() + 42.0;
    let title_width = (quality_rect.left() - title_x - 8.0).max(54.0);
    painter
        .with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(title_x, rect.top() + 8.0),
            egui::pos2(quality_rect.left() - 6.0, rect.top() + 37.0),
        ))
        .text(
            egui::pos2(title_x, rect.top() + 21.0),
            egui::Align2::LEFT_CENTER,
            &meta.name,
            egui::FontId::monospace(fitted_start_text_size(&meta.name, title_width, 13.0, 8.5)),
            colors.text,
        );

    let edits = world_edit_summary(meta.world_edit_manifest.edited_chunks);
    let detail = format!("SEED {}  //  {edits}", meta.seed);
    painter
        .with_clip_rect(rect.shrink2(egui::vec2(12.0, 4.0)))
        .text(
            egui::pos2(rect.left() + 14.0, rect.top() + 51.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::monospace(9.5),
            colors.text_muted,
        );
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.bottom() - 13.0),
        egui::Align2::LEFT_CENTER,
        format!("WORLD {:02}", ordinal + 1),
        egui::FontId::monospace(9.0),
        if latest {
            colors.success
        } else {
            colors.text_muted
        },
    );
    painter.text(
        egui::pos2(rect.right() - 12.0, rect.bottom() - 13.0),
        egui::Align2::RIGHT_CENTER,
        if latest {
            "LATEST SESSION"
        } else {
            quality.detail()
        },
        egui::FontId::monospace(9.0),
        if latest {
            colors.success
        } else {
            quality_color
        },
    );

    let select = response.clicked();
    let open = response.double_clicked()
        || (focused
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)));
    if select {
        response.request_focus();
    }
    if selected && reveal_selection {
        response.scroll_to_me(Some(egui::Align::Center));
    }
    let _ = response.on_hover_text(format!(
        "Select {}. Double-click or press Enter to open.",
        meta.name
    ));

    StartWorldCardEvent { select, open }
}

fn draw_start_launch_summary(
    ui: &mut egui::Ui,
    selected: Option<&WorldMeta>,
    theme: crate::theme::ThemeSettings,
) {
    let colors = theme.semantic();
    ui.label(
        egui::RichText::new("LAUNCH CONSOLE")
            .size(10.0)
            .strong()
            .monospace()
            .color(colors.text_muted),
    );
    if let Some(meta) = selected {
        let title_size = fitted_start_text_size(&meta.name, ui.available_width(), 20.0, 11.0);
        ui.label(
            egui::RichText::new(&meta.name)
                .size(title_size)
                .strong()
                .monospace()
                .color(colors.text),
        );
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Detail,
                "QUALITY",
                WorldQualityTier::from_quality(meta.scenery_quality).label(),
                theme,
            );
            crate::ui_kit::status_chip(
                ui,
                Icon::Chunk,
                "EDITS",
                &meta.world_edit_manifest.edited_chunks.to_string(),
                theme,
            );
            crate::ui_kit::status_chip(ui, Icon::Seed, "SEED", &meta.seed.to_string(), theme);
        });
    } else {
        ui.label(
            egui::RichText::new("EMPTY GARDEN")
                .size(20.0)
                .strong()
                .monospace()
                .color(colors.text),
        );
        ui.label(
            egui::RichText::new("A generated name and seed are ready when you are.")
                .size(10.5)
                .monospace()
                .color(colors.text_muted),
        );
        crate::ui_kit::status_chip(ui, Icon::Globe, "STATE", "READY TO CREATE", theme);
    }
}

fn start_response_activated(ui: &mut egui::Ui, response: &egui::Response) -> bool {
    response.clicked()
        || (response.has_focus()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)))
}

fn draw_main_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<GameState>>,
    mut commands: Commands,
    mut form: ResMut<NewWorldForm>,
    mut start_state: ResMut<StartMenuState>,
    mut settings: ResMut<WorldSettings>,
    mut editor: ResMut<EditorState>,
    mut command_palette: ResMut<CommandPaletteState>,
    mut pending: ResMut<PendingWorldLoad>,
    mut pending_delete: ResMut<PendingWorldDelete>,
    mut exit: EventWriter<AppExit>,
) {
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let screen = ctx.screen_rect();
    let theme = settings.theme;
    let colors = theme.semantic();
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let worlds = settings::list_worlds();
    sync_start_menu_selection(&mut start_state, &worlds);

    let overlay_open = editor.open || command_palette.open;
    let navigation_enabled =
        start_menu_navigation_enabled(start_state.text_field_focused, overlay_open);
    let mut focus_new_world = false;
    let mut focus_primary = false;
    if navigation_enabled {
        let movement = ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                Some(WorldSelectionMove::Previous)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                Some(WorldSelectionMove::Next)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                Some(WorldSelectionMove::First)
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                Some(WorldSelectionMove::Last)
            } else {
                None
            }
        });
        if let Some(movement) = movement {
            start_state.selected_world =
                moved_world_selection(start_state.selected_world.as_deref(), &worlds, movement);
            pending_delete.0 = None;
            focus_primary = true;
        }
        focus_new_world =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::N));
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            pending_delete.0 = None;
            start_state.show_blueprints = false;
        }
    }

    let selected = start_state
        .selected_world
        .as_deref()
        .and_then(|name| worlds.iter().find(|world| world.name == name));
    let latest_name = worlds
        .iter()
        .max_by_key(|world| world.last_played_epoch)
        .map(|world| world.name.as_str());
    let layout = start_menu_layout(screen.size(), worlds.len());

    let mut open_requested = None::<WorldMeta>;
    let mut create_requested = false;
    let mut delete_requested = None::<String>;
    let mut open_toolbench = false;
    let mut open_commands = false;
    let mut quit_requested = false;
    let mut text_field_focused = false;

    // Static paint only: the menu idles without scheduling frame-by-frame work.
    draw_stable_start_backdrop(ctx, theme);
    egui::CentralPanel::default()
        .frame(egui::Frame::none())
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_source("r93g_start_scroll_v2")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(if layout.density == StartMenuDensity::Compact {
                        12.0
                    } else {
                        22.0
                    });
                    ui.horizontal(|ui| {
                        let side = ((ui.available_width() - layout.content_width) * 0.5).max(0.0);
                        ui.add_space(side);
                        ui.vertical(|ui| {
                            ui.set_width(layout.content_width);

                            ui.horizontal_wrapped(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new("R93G // ZEN OPERATING LAYER")
                                            .size(10.0)
                                            .strong()
                                            .monospace()
                                            .color(primary),
                                    );
                                    ui.label(
                                        egui::RichText::new(START_TITLE)
                                            .size(layout.title_size)
                                            .strong()
                                            .monospace()
                                            .color(colors.text),
                                    );
                                    if layout.show_subtitle {
                                        ui.label(
                                            egui::RichText::new(START_SUBTITLE)
                                                .size(10.5)
                                                .monospace()
                                                .color(dim),
                                        );
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        metric_pill(ui, theme, "WORLDS", &worlds.len().to_string());
                                        metric_pill(ui, theme, "STREAM", "READY");
                                    },
                                );
                            });
                            ui.add_space(8.0);
                            crate::ui_kit::compact_separator(ui, theme);
                            ui.add_space(10.0);

                            crate::ui_kit::surface_panel(ui, theme, |ui| {
                                let mut draw_primary = |ui: &mut egui::Ui| {
                                    let (label, detail) = if let Some(meta) = selected {
                                        ("ENTER WORLD", meta.name.as_str())
                                    } else {
                                        ("CREATE FIRST WORLD", "generated name + random seed")
                                    };
                                    let response =
                                        start_menu_command(ui, label, Some(detail), true, theme);
                                    if navigation_enabled
                                        && (focus_primary
                                            || ui.ctx().memory(|memory| memory.focused().is_none()))
                                    {
                                        response.request_focus();
                                    }
                                    if start_response_activated(ui, &response) {
                                        if let Some(meta) = selected {
                                            open_requested = Some(meta.clone());
                                        } else {
                                            create_requested = true;
                                        }
                                    }

                                    if let Some(meta) = selected {
                                        ui.add_space(6.0);
                                        let delete_armed = pending_delete.is_armed(&meta.name);
                                        let delete_label = if delete_armed {
                                            "CONFIRM DELETE"
                                        } else {
                                            "DELETE WORLD"
                                        };
                                        if start_menu_footer_action(ui, delete_label, true, theme)
                                            .clicked()
                                            && pending_delete.arm_or_confirm(&meta.name)
                                        {
                                            delete_requested = Some(meta.name.clone());
                                        }
                                    }
                                };

                                if layout.uses_split_rows() {
                                    ui.columns(2, |cols| {
                                        draw_start_launch_summary(&mut cols[0], selected, theme);
                                        draw_primary(&mut cols[1]);
                                    });
                                } else {
                                    draw_start_launch_summary(ui, selected, theme);
                                    ui.add_space(10.0);
                                    draw_primary(ui);
                                }
                            });

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("WORLD LIBRARY")
                                        .size(10.5)
                                        .strong()
                                        .monospace()
                                        .color(colors.text_muted),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("LOCAL SAVE INDEX")
                                                .size(9.0)
                                                .monospace()
                                                .color(dim),
                                        );
                                    },
                                );
                            });
                            egui::ScrollArea::vertical()
                                .id_source("r93g_world_library_v2")
                                .max_height(layout.world_list_height)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if worlds.is_empty() {
                                        crate::ui_kit::activity_status(
                                            ui,
                                            LoadingState::Idle,
                                            "WORLD LIBRARY",
                                            "No local worlds yet",
                                            theme,
                                        );
                                    } else if layout.world_columns == 1 {
                                        for (index, meta) in worlds.iter().enumerate() {
                                            let event = draw_start_world_card(
                                                ui,
                                                meta,
                                                index,
                                                start_state.selected_world.as_deref()
                                                    == Some(meta.name.as_str()),
                                                latest_name == Some(meta.name.as_str()),
                                                focus_primary,
                                                theme,
                                            );
                                            if event.select {
                                                start_state.selected_world =
                                                    Some(meta.name.clone());
                                                pending_delete.0 = None;
                                            }
                                            if event.open {
                                                open_requested = Some(meta.clone());
                                            }
                                            ui.add_space(6.0);
                                        }
                                    } else {
                                        for (row, pair) in worlds.chunks(2).enumerate() {
                                            ui.columns(2, |cols| {
                                                for (column, meta) in pair.iter().enumerate() {
                                                    let index = row * 2 + column;
                                                    let event = draw_start_world_card(
                                                        &mut cols[column],
                                                        meta,
                                                        index,
                                                        start_state.selected_world.as_deref()
                                                            == Some(meta.name.as_str()),
                                                        latest_name == Some(meta.name.as_str()),
                                                        focus_primary,
                                                        theme,
                                                    );
                                                    if event.select {
                                                        start_state.selected_world =
                                                            Some(meta.name.clone());
                                                        pending_delete.0 = None;
                                                    }
                                                    if event.open {
                                                        open_requested = Some(meta.clone());
                                                    }
                                                }
                                            });
                                            ui.add_space(6.0);
                                        }
                                    }
                                });

                            ui.add_space(8.0);
                            crate::ui_kit::surface_panel(ui, theme, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new("NEW WORLD")
                                            .size(10.5)
                                            .strong()
                                            .monospace()
                                            .color(colors.text_muted),
                                    );
                                    crate::ui_kit::status_chip(
                                        ui,
                                        Icon::Detail,
                                        "DEFAULT",
                                        "IMMERSIVE",
                                        theme,
                                    );
                                });
                                ui.add_space(5.0);

                                let name_hint = auto_world_name(&worlds);
                                if layout.uses_split_rows() {
                                    ui.columns(2, |cols| {
                                        let name_response = cols[0]
                                            .push_id("start_new_world_name_v2", |ui| {
                                                crate::ui_kit::search_box(
                                                    ui,
                                                    &mut form.name,
                                                    &name_hint,
                                                    theme,
                                                )
                                            })
                                            .inner;
                                        if focus_new_world {
                                            name_response.request_focus();
                                        }
                                        text_field_focused |= name_response.has_focus();

                                        let seed_response = cols[1]
                                            .push_id("start_new_world_seed_v2", |ui| {
                                                crate::ui_kit::search_box(
                                                    ui,
                                                    &mut form.seed_text,
                                                    "RANDOM SEED",
                                                    theme,
                                                )
                                            })
                                            .inner;
                                        text_field_focused |= seed_response.has_focus();
                                    });
                                } else {
                                    let name_response = ui
                                        .push_id("start_new_world_name_v2", |ui| {
                                            crate::ui_kit::search_box(
                                                ui,
                                                &mut form.name,
                                                &name_hint,
                                                theme,
                                            )
                                        })
                                        .inner;
                                    if focus_new_world {
                                        name_response.request_focus();
                                    }
                                    text_field_focused |= name_response.has_focus();
                                    let seed_response = ui
                                        .push_id("start_new_world_seed_v2", |ui| {
                                            crate::ui_kit::search_box(
                                                ui,
                                                &mut form.seed_text,
                                                "RANDOM SEED",
                                                theme,
                                            )
                                        })
                                        .inner;
                                    text_field_focused |= seed_response.has_focus();
                                }

                                ui.add_space(6.0);
                                ui.horizontal_wrapped(|ui| {
                                    if crate::ui_kit::icon_square(
                                        ui,
                                        Icon::Seed,
                                        false,
                                        theme,
                                        "Generate random seed",
                                    )
                                    .clicked()
                                    {
                                        form.seed_text = rand_seed().to_string();
                                    }
                                    if crate::ui_kit::icon_action(
                                        ui,
                                        Icon::Layout,
                                        "Blueprints",
                                        start_state.show_blueprints,
                                        theme,
                                    )
                                    .clicked()
                                    {
                                        start_state.show_blueprints = !start_state.show_blueprints;
                                    }
                                    if crate::ui_kit::icon_action(
                                        ui,
                                        Icon::Play,
                                        "Create",
                                        false,
                                        theme,
                                    )
                                    .clicked()
                                    {
                                        create_requested = true;
                                    }
                                });

                                if start_state.show_blueprints {
                                    ui.add_space(7.0);
                                    crate::ui_kit::compact_separator(ui, theme);
                                    ui.add_space(7.0);
                                    ui.horizontal_wrapped(|ui| {
                                        for (label, world_name, seed) in [
                                            ("NEO-KYOTO", "neo_kyoto_garden", "930514"),
                                            ("CYBER-ZEN", "cyber_zen_path", "440993"),
                                            ("SAKURA VOID", "sakura_void", "884499"),
                                            ("LOTUS DRIFT", "lotus_drift", "221177"),
                                        ] {
                                            let selected_blueprint = form.seed_text == seed;
                                            if crate::ui_kit::choice_chip_sized(
                                                ui,
                                                label,
                                                selected_blueprint,
                                                142.0,
                                                theme,
                                            )
                                            .clicked()
                                            {
                                                form.name = world_name.to_owned();
                                                form.seed_text = seed.to_owned();
                                            }
                                        }
                                    });
                                }
                            });

                            if pending.0 {
                                ui.add_space(8.0);
                                crate::ui_kit::activity_status(
                                    ui,
                                    LoadingState::Indeterminate,
                                    "WORLD LINK",
                                    "Opening selected world...",
                                    theme,
                                );
                            }

                            ui.add_space(8.0);
                            crate::ui_kit::compact_separator(ui, theme);
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                open_toolbench = crate::ui_kit::icon_action(
                                    ui,
                                    Icon::Gear,
                                    "Toolbench",
                                    false,
                                    theme,
                                )
                                .clicked();
                                open_commands = crate::ui_kit::icon_action(
                                    ui,
                                    Icon::Key,
                                    "Command deck",
                                    false,
                                    theme,
                                )
                                .clicked();
                                quit_requested =
                                    crate::ui_kit::danger_action(ui, Icon::Quit, "Quit", theme)
                                        .clicked();
                            });
                            ui.add_space(layout.outer_margin);
                        });
                    });
                });
        });

    start_state.text_field_focused = text_field_focused;

    if let Some(name) = delete_requested {
        settings::delete_world(&name);
        pending_delete.0 = None;
        if start_state.selected_world.as_deref() == Some(name.as_str()) {
            start_state.selected_world = None;
        }
    }

    if let Some(meta) = open_requested {
        apply_world_to_settings(&meta, &mut settings);
        commands.insert_resource(ActiveWorld { meta });
        editor.open = false;
        pending.0 = true;
        next.set(GameState::InGame);
    } else if create_requested {
        let seed = form
            .seed_text
            .parse::<u32>()
            .unwrap_or_else(|_| rand_seed());
        let name = clean_new_world_name(&form.name, &worlds);
        let meta = WorldMeta::new(name, seed);
        settings::save_world(&meta);
        apply_world_to_settings(&meta, &mut settings);
        commands.insert_resource(ActiveWorld { meta });
        editor.open = false;
        pending.0 = true;
        form.name.clear();
        form.seed_text.clear();
        next.set(GameState::InGame);
    }

    if open_toolbench {
        editor.open = true;
    }
    if open_commands {
        command_palette.open();
    }
    if quit_requested {
        settings.save();
        exit.send(AppExit::Success);
    }
}

fn draw_stable_start_backdrop(ctx: &egui::Context, theme: crate::theme::ThemeSettings) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::background());
    let colors = theme.semantic();
    let primary = theme.color.primary();
    let rose = egui::Color32::from_rgb(236, 104, 151);
    let warm = egui::Color32::from_rgb(232, 194, 124);
    let ink = egui::Color32::from_rgb(7, 7, 11);
    let horizon = screen.top() + screen.height() * 0.62;

    painter.rect_filled(screen, 0.0, ink);
    painter.rect_filled(
        egui::Rect::from_min_max(screen.min, egui::pos2(screen.right(), horizon)),
        0.0,
        egui::Color32::from_rgb(22, 13, 23),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(screen.left(), horizon), screen.max),
        0.0,
        egui::Color32::from_rgb(6, 10, 14),
    );

    // A static architectural grid gives the page depth without animation,
    // texture uploads, or a frame-by-frame repaint loop.
    for row in 0..6 {
        let y = horizon + row as f32 * 34.0;
        painter.line_segment(
            [egui::pos2(screen.left(), y), egui::pos2(screen.right(), y)],
            egui::Stroke::new(
                1.0,
                start_menu_alpha(primary, (46_i32 - row * 6).max(12) as u8),
            ),
        );
    }
    for column in 0..=10 {
        let x = screen.left() + screen.width() * column as f32 / 10.0;
        painter.line_segment(
            [egui::pos2(x, horizon), egui::pos2(x, screen.bottom())],
            egui::Stroke::new(1.0, start_menu_alpha(colors.outline, 34)),
        );
    }

    // Quiet skyline telemetry along the horizon.
    for index in 0..12 {
        let width = 18.0 + (index % 4) as f32 * 8.0;
        let height = 34.0 + ((index * 29) % 92) as f32;
        let x = screen.right() - 390.0 + index as f32 * 31.0;
        let rect =
            egui::Rect::from_min_size(egui::pos2(x, horizon - height), egui::vec2(width, height));
        painter.rect_filled(
            rect,
            0.0,
            egui::Color32::from_rgba_unmultiplied(2, 8, 14, 138),
        );
        painter.line_segment(
            [rect.left_top(), rect.right_top()],
            egui::Stroke::new(
                1.0,
                start_menu_alpha(if index % 3 == 0 { rose } else { primary }, 58),
            ),
        );
    }

    // An offset shoji portal is the visual anchor. Only corner strokes are
    // painted, leaving the content surface calm and readable.
    let portal = egui::Rect::from_center_size(
        egui::pos2(
            screen.center().x + screen.width() * 0.27,
            screen.top() + screen.height() * 0.35,
        ),
        egui::vec2(
            (screen.width() * 0.25).clamp(180.0, 360.0),
            (screen.height() * 0.34).clamp(180.0, 330.0),
        ),
    );
    let corner = portal.width().min(portal.height()) * 0.18;
    let portal_stroke = egui::Stroke::new(1.4, start_menu_alpha(warm, 108));
    for (origin, x_sign, y_sign) in [
        (portal.left_top(), 1.0, 1.0),
        (portal.right_top(), -1.0, 1.0),
        (portal.left_bottom(), 1.0, -1.0),
        (portal.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment(
            [origin, origin + egui::vec2(corner * x_sign, 0.0)],
            portal_stroke,
        );
        painter.line_segment(
            [origin, origin + egui::vec2(0.0, corner * y_sign)],
            portal_stroke,
        );
    }
    painter.line_segment(
        [
            egui::pos2(portal.center().x, portal.top() + 18.0),
            egui::pos2(portal.center().x, portal.bottom() - 18.0),
        ],
        egui::Stroke::new(1.0, start_menu_alpha(rose, 54)),
    );
    painter.line_segment(
        [
            egui::pos2(portal.left() + 18.0, portal.center().y),
            egui::pos2(portal.right() - 18.0, portal.center().y),
        ],
        egui::Stroke::new(1.0, start_menu_alpha(primary, 54)),
    );

    // A reduced torii signal on the opposite side balances the product shell.
    let gate_x = screen.left() + screen.width() * 0.18;
    let gate_y = horizon - 8.0;
    let gate = egui::Stroke::new(2.0, start_menu_alpha(rose, 112));
    painter.line_segment(
        [
            egui::pos2(gate_x - 68.0, gate_y),
            egui::pos2(gate_x + 68.0, gate_y),
        ],
        gate,
    );
    painter.line_segment(
        [
            egui::pos2(gate_x - 48.0, gate_y + 12.0),
            egui::pos2(gate_x + 48.0, gate_y + 12.0),
        ],
        egui::Stroke::new(1.0, start_menu_alpha(warm, 82)),
    );
    for x in [gate_x - 40.0, gate_x + 40.0] {
        painter.line_segment([egui::pos2(x, gate_y), egui::pos2(x, gate_y + 88.0)], gate);
    }

    // Sparse cross-signals replace particle decoration and remain deterministic.
    for index in 0..14 {
        let u = index as f32 / 14.0;
        let x = screen.left() + screen.width() * ((u * 5.73 + 0.17).fract());
        let y = screen.top() + screen.height() * (0.08 + ((u * 13.1).sin() * 0.5 + 0.5) * 0.38);
        let signal = if index % 3 == 0 { warm } else { primary };
        let alpha = 34 + (index % 4) as u8 * 10;
        painter.line_segment(
            [egui::pos2(x - 2.0, y), egui::pos2(x + 2.0, y)],
            egui::Stroke::new(1.0, start_menu_alpha(signal, alpha)),
        );
        painter.line_segment(
            [egui::pos2(x, y - 2.0), egui::pos2(x, y + 2.0)],
            egui::Stroke::new(1.0, start_menu_alpha(signal, alpha)),
        );
    }

    painter.rect_filled(
        egui::Rect::from_min_max(
            screen.min,
            egui::pos2(screen.left() + screen.width().min(140.0), screen.bottom()),
        ),
        0.0,
        egui::Color32::from_black_alpha(82),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(screen.right() - screen.width().min(140.0), screen.top()),
            screen.max,
        ),
        0.0,
        egui::Color32::from_black_alpha(82),
    );
}
// ============================ Pause / Inventory ===========================

fn draw_pause_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    settings: Res<WorldSettings>,
    mut editor: ResMut<EditorState>,
    ship_inventory: Res<crate::ships::ShipInventory>,
    brain: Res<crate::bots::FriendlyWorldBrain>,
    active: Option<Res<ActiveWorld>>,
    scratch: Res<PlayerProgressScratch>,
    player_q: Query<(&Transform, &Player)>,
    ship_q: Query<(Entity, &Transform, &crate::ships::ShipInstance)>,
    mut commands: Commands,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    mut command_palette: ResMut<CommandPaletteState>,
    mut exit: EventWriter<AppExit>,
) {
    if *pause_screen == PauseScreen::Inventory {
        return;
    }
    // While the editor window is open, it handles its own overlay -- we only
    // dim the background behind it.
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    let screen = ctx.screen_rect();
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("pause_dim"),
    ))
    .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));

    if editor.open {
        return; // editor.rs draws its own panel on top.
    }

    draw_pause_main(
        ctx,
        &mut next,
        &mut pause_screen,
        &mut editor,
        &settings,
        active.as_deref(),
        &scratch,
        &player_q,
        &ship_q,
        &ship_inventory,
        &brain,
        &mut commands,
        &mut world,
        &mut streamer,
        &mut command_palette,
        &mut exit,
    );
}

fn draw_inventory_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    mut settings: ResMut<WorldSettings>,
    mut hotbar: ResMut<HotbarState>,
    mut builder: ResMut<crate::builder::BuilderState>,
    mut ship_inventory: ResMut<crate::ships::ShipInventory>,
    mut ship_placement: ResMut<crate::ships::ShipPlacementState>,
    mut mode: ResMut<ModeContext>,
    mut brain: ResMut<crate::bots::FriendlyWorldBrain>,
) {
    if *pause_screen != PauseScreen::Inventory {
        return;
    }
    let Some(ctx) = contexts.try_ctx_mut() else {
        return;
    };
    draw_inventory(
        ctx,
        &mut hotbar,
        &mut builder,
        &mut pause_screen,
        &mut next,
        &mut settings,
        &mut ship_inventory,
        &mut ship_placement,
        &mut mode,
        &mut brain,
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::settings::{SceneryQuality, WorldMeta};

    #[test]
    fn main_menu_does_not_force_continuous_repaint() {
        let source = include_str!("menu.rs");
        assert!(
            !source.contains(concat!("request_", "repaint();")),
            "main menu should not force an every-frame repaint; startup/menu must idle cheaply"
        );
    }

    #[test]
    fn start_screen_uses_zen_neon_identity() {
        assert!(START_TITLE.contains("ZEN"));
        assert!(START_TITLE.contains("SAKURA"));
        assert!(START_SUBTITLE.contains("sketch dojo"));
        assert!(START_SUBTITLE.contains("blossom worlds"));
        assert!(START_SUBTITLE.contains("low-end"));
    }

    #[test]
    fn compact_start_layout_fits_small_windows_without_fixed_panel_clipping() {
        let viewport = egui::vec2(480.0, 360.0);
        let layout = start_menu_layout(viewport, 12);
        let tiny = start_menu_layout(egui::vec2(320.0, 480.0), 1);

        assert_eq!(layout.density, StartMenuDensity::Compact);
        assert!(!layout.uses_split_rows());
        assert!(!layout.show_subtitle);
        assert!(layout.content_width <= viewport.x - layout.outer_margin * 2.0);
        assert!(layout.content_width >= 300.0);
        assert_eq!(layout.world_columns, 1);
        assert!(layout.world_list_height <= viewport.y * 0.36);
        assert!(tiny.content_width <= 320.0 - tiny.outer_margin * 2.0);
    }

    #[test]
    fn wide_start_layout_prioritizes_readability_and_more_world_rows() {
        let standard = start_menu_layout(egui::vec2(800.0, 700.0), 20);
        let wide = start_menu_layout(egui::vec2(1440.0, 900.0), 20);

        assert_eq!(standard.density, StartMenuDensity::Standard);
        assert_eq!(wide.density, StartMenuDensity::Wide);
        assert!(standard.uses_split_rows());
        assert!(wide.show_subtitle);
        assert!(wide.content_width > standard.content_width);
        assert!(wide.world_list_height > standard.world_list_height);
        assert_eq!(standard.world_columns, 1);
        assert_eq!(wide.world_columns, 2);
        assert!(wide.content_width <= 1080.0);
    }

    #[test]
    fn empty_library_keeps_a_small_stable_placeholder() {
        let compact = start_menu_layout(egui::vec2(400.0, 320.0), 0);
        let wide = start_menu_layout(egui::vec2(1600.0, 1000.0), 0);

        assert_eq!(compact.world_list_height, 72.0);
        assert_eq!(wide.world_list_height, 72.0);
    }

    #[test]
    fn world_selection_defaults_to_latest_and_survives_reordering() {
        let mut archived = WorldMeta::new("archive".to_owned(), 11);
        archived.last_played_epoch = 10;
        let mut recent = WorldMeta::new("recent".to_owned(), 22);
        recent.last_played_epoch = 30;
        let mut middle = WorldMeta::new("middle".to_owned(), 33);
        middle.last_played_epoch = 20;
        let mut worlds = vec![archived, recent, middle];
        let mut state = StartMenuState::default();

        sync_start_menu_selection(&mut state, &worlds);
        assert_eq!(state.selected_world.as_deref(), Some("recent"));

        worlds.reverse();
        sync_start_menu_selection(&mut state, &worlds);
        assert_eq!(state.selected_world.as_deref(), Some("recent"));

        worlds.retain(|world| world.name != "recent");
        sync_start_menu_selection(&mut state, &worlds);
        assert_eq!(state.selected_world.as_deref(), Some("middle"));
    }

    #[test]
    fn keyboard_world_selection_wraps_and_supports_boundaries() {
        let worlds = vec![
            WorldMeta::new("alpha".to_owned(), 1),
            WorldMeta::new("beta".to_owned(), 2),
            WorldMeta::new("gamma".to_owned(), 3),
        ];

        assert_eq!(
            moved_world_selection(Some("alpha"), &worlds, WorldSelectionMove::Previous).as_deref(),
            Some("gamma")
        );
        assert_eq!(
            moved_world_selection(Some("gamma"), &worlds, WorldSelectionMove::Next).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            moved_world_selection(None, &worlds, WorldSelectionMove::Last).as_deref(),
            Some("gamma")
        );
        assert_eq!(
            moved_world_selection(Some("gamma"), &worlds, WorldSelectionMove::First).as_deref(),
            Some("alpha")
        );
        assert_eq!(
            moved_world_selection(Some("alpha"), &[], WorldSelectionMove::Next),
            None
        );
    }

    #[test]
    fn world_quality_tiers_cover_every_saved_scenery_profile() {
        assert_eq!(
            WorldQualityTier::from_quality(SceneryQuality::Off),
            WorldQualityTier::Minimal
        );
        assert_eq!(
            WorldQualityTier::from_quality(SceneryQuality::Lean),
            WorldQualityTier::Efficient
        );
        assert_eq!(
            WorldQualityTier::from_quality(SceneryQuality::Balanced),
            WorldQualityTier::Balanced
        );
        assert_eq!(
            WorldQualityTier::from_quality(SceneryQuality::Lush),
            WorldQualityTier::Immersive
        );
        assert_eq!(WorldQualityTier::Immersive.detail(), "LUSH SCENERY");
    }

    #[test]
    fn world_card_statuses_are_compact_and_grammatical() {
        assert_eq!(world_edit_summary(0), "PRISTINE");
        assert_eq!(world_edit_summary(1), "1 EDITED CHUNK");
        assert_eq!(world_edit_summary(42), "42 EDITED CHUNKS");
        assert_eq!(START_WORLD_CARD_HEIGHT, 88.0);
    }

    #[test]
    fn text_entry_and_overlays_suspend_global_world_navigation() {
        assert!(start_menu_navigation_enabled(false, false));
        assert!(!start_menu_navigation_enabled(true, false));
        assert!(!start_menu_navigation_enabled(false, true));
        assert!(!start_menu_navigation_enabled(true, true));
    }

    #[test]
    fn long_world_names_shrink_without_growing_card_geometry() {
        let size = fitted_start_text_size(
            "a_world_name_that_is_far_longer_than_the_available_card_width",
            120.0,
            13.0,
            8.5,
        );

        assert!((8.5..13.0).contains(&size));
        assert_eq!(fitted_start_text_size("zen", 120.0, 13.0, 8.5), 13.0);
    }

    #[test]
    fn inventory_grid_adapts_without_overflowing_narrow_panels() {
        let (narrow_columns, narrow_tile) = inventory_grid_metrics(250.0);
        let (medium_columns, medium_tile) = inventory_grid_metrics(500.0);
        let (wide_columns, wide_tile) = inventory_grid_metrics(940.0);

        assert_eq!(narrow_columns, 1);
        assert_eq!(medium_columns, 3);
        assert_eq!(wide_columns, 5);
        assert!(narrow_tile <= 250.0);
        assert!(medium_tile * medium_columns as f32 <= 500.0);
        assert!(wide_tile * wide_columns as f32 <= 940.0);
    }

    #[test]
    fn pause_notice_has_a_finite_lifetime() {
        let notice = PauseNotice {
            label: "WORLD SAVE".to_owned(),
            value: "Snapshot written".to_owned(),
            expires_at: 12.4,
        };

        assert!(notice.is_active_at(10.0));
        assert!(!notice.is_active_at(12.4));
        assert!(!notice.is_active_at(f64::NAN));
    }

    #[test]
    fn material_color_conversion_clamps_invalid_channels() {
        assert_eq!(
            rgba_color32([-1.0, 0.5, 2.0, f32::NAN]),
            egui::Color32::from_rgba_unmultiplied(0, 128, 255, 0)
        );
    }

    #[test]
    fn auto_world_name_skips_orphan_artifact_stems() {
        let worlds = Vec::<WorldMeta>::new();
        let reserved = HashSet::from(["world_01".to_string(), "world_02".to_string()]);

        assert_eq!(
            auto_world_name_with_reserved(&worlds, &reserved),
            "world_03"
        );
    }

    #[test]
    fn typed_new_world_name_is_made_unique_when_storage_stem_exists() {
        let worlds = Vec::<WorldMeta>::new();
        let reserved = HashSet::from(["dream_city".to_string()]);

        assert_eq!(
            clean_new_world_name_with_reserved("dream_city", &worlds, &reserved),
            "dream_city_02"
        );
    }

    #[test]
    fn world_delete_requires_two_matching_clicks() {
        let mut pending = PendingWorldDelete::default();

        assert!(!pending.arm_or_confirm("garden_a"));
        assert!(pending.is_armed("garden_a"));
        assert!(!pending.arm_or_confirm("garden_b"));
        assert!(!pending.is_armed("garden_a"));
        assert!(pending.is_armed("garden_b"));
        assert!(pending.arm_or_confirm("garden_b"));
        assert!(!pending.is_armed("garden_b"));
    }

    #[test]
    fn loading_world_applies_saved_scenery_quality() {
        let mut settings = WorldSettings::default();
        settings.scenery_quality = SceneryQuality::Lean;
        let mut meta = WorldMeta::new("garden".to_string(), 930514);
        meta.scenery_quality = SceneryQuality::Lush;

        apply_world_to_settings(&meta, &mut settings);

        assert_eq!(settings.scenery_quality, SceneryQuality::Lush);
    }

    #[test]
    fn creative_inventory_covers_the_canonical_buildable_catalog() {
        let entries = inventory_block_entries();
        let catalog = crate::blocks::block_palette_catalog();
        let mut actual: Vec<u16> = entries.iter().map(|entry| entry.block as u16).collect();
        let mut expected: Vec<u16> = crate::blocks::BUILDABLE_BLOCKS
            .iter()
            .map(|block| *block as u16)
            .collect();
        actual.sort_unstable();
        expected.sort_unstable();

        assert_eq!(actual, expected);
        assert!(entries.iter().all(|entry| entry.category < catalog.len()));
    }

    #[test]
    fn creative_inventory_search_includes_catalog_roles_and_categories() {
        let entry = inventory_block_entries()
            .into_iter()
            .find(|entry| entry.block == crate::blocks::BlockType::EngineCore)
            .expect("engine core should be in the canonical catalog");
        let category = crate::blocks::block_palette_catalog()[entry.category];

        assert!(inventory_entry_matches(entry, category.label, "machinery"));
        assert!(inventory_entry_matches(entry, category.label, "metal"));
    }

    #[test]
    fn creative_inventory_selection_changes_the_real_builder_material() {
        let mut builder = crate::builder::BuilderState::default();

        apply_inventory_block_selection(&mut builder, crate::blocks::BlockType::ShojiLamp);

        assert_eq!(builder.block, crate::blocks::BlockType::ShojiLamp);
        assert!(builder.status.contains("Lantern"));
    }

    #[test]
    fn keyboard_owned_by_search_disables_the_e_shortcut() {
        let context = egui::Context::default();
        let mut search = String::new();
        context.begin_frame(egui::RawInput::default());
        egui::CentralPanel::default().show(&context, |ui| {
            ui.add(egui::TextEdit::singleline(&mut search))
                .request_focus();
        });

        assert!(context.wants_keyboard_input());
        assert!(!menu_letter_shortcuts_enabled(
            context.wants_keyboard_input()
        ));
        assert!(menu_letter_shortcuts_enabled(false));
        let _ = context.end_frame();
    }
}

fn draw_pause_main(
    ctx: &egui::Context,
    next: &mut ResMut<NextState<GameState>>,
    pause_screen: &mut ResMut<PauseScreen>,
    editor: &mut ResMut<EditorState>,
    settings: &WorldSettings,
    active: Option<&ActiveWorld>,
    scratch: &PlayerProgressScratch,
    player_q: &Query<(&Transform, &Player)>,
    ship_q: &Query<(Entity, &Transform, &crate::ships::ShipInstance)>,
    ship_inventory: &crate::ships::ShipInventory,
    brain: &crate::bots::FriendlyWorldBrain,
    commands: &mut Commands,
    world: &mut VoxelWorld,
    streamer: &mut ChunkStreamer,
    command_palette: &mut CommandPaletteState,
    exit: &mut EventWriter<AppExit>,
) {
    let screen = ctx.screen_rect();
    let panel_w = 500.0_f32.min(screen.width() - 40.0);
    let panel_h = 640.0_f32.min(screen.height() - 80.0);
    let pos = egui::pos2(
        screen.center().x - panel_w * 0.5,
        screen.center().y - panel_h * 0.5,
    );

    egui::Window::new("voxel_native_pause")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(command_frame(settings.theme))
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("PAUSE // COMMAND HOLD")
                        .size(28.0)
                        .color(settings.theme.color.primary())
                        .strong()
                        .monospace(),
                );
                if let Some(active) = active {
                    ui.label(
                        egui::RichText::new(format!("Welt: {}", active.meta.name))
                            .color(settings.theme.color.dim())
                            .size(14.0),
                    );
                }
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);
            if let Some(notice) = active_pause_notice(ctx) {
                crate::ui_kit::activity_status(
                    ui,
                    LoadingState::Complete,
                    &notice.label,
                    &notice.value,
                    settings.theme,
                );
            } else {
                crate::ui_kit::activity_status(
                    ui,
                    LoadingState::Idle,
                    "SESSION HOLD",
                    "World simulation paused",
                    settings.theme,
                );
            }
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_source("pause_command_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Resume,
                            "Resume",
                            "Back to the world",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            next.set(GameState::InGame);
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Save,
                            "Save",
                            "Write current world",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            save_current_world(
                                settings,
                                active,
                                scratch,
                                player_q,
                                ship_q,
                                ship_inventory,
                                brain,
                                world,
                            );
                            set_pause_notice(ctx, "WORLD SAVE", "Snapshot written");
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Cube,
                            "Inventory",
                            "Blocks, ships, companions",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            **pause_screen = PauseScreen::Inventory;
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Wand,
                            "Repair Terrain",
                            "Remove old visual artifact chunks",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            let report = world.repair_visual_artifact_overrides();
                            if report.removed_chunks > 0 {
                                streamer.frontier_complete = false;
                                streamer.needs_orphan_scan = true;
                                save_current_world(
                                    settings,
                                    active,
                                    scratch,
                                    player_q,
                                    ship_q,
                                    ship_inventory,
                                    brain,
                                    world,
                                );
                            }
                            info!(
                                "Scanned {} edit chunks, repaired {}, refreshed {} loaded chunks.",
                                report.scanned_chunks,
                                report.removed_chunks,
                                report.refreshed_loaded_chunks
                            );
                            set_pause_notice(
                                ctx,
                                "TERRAIN SCAN",
                                if report.removed_chunks == 0 {
                                    format!("No artifacts in {} chunks", report.scanned_chunks)
                                } else {
                                    format!("{} artifact chunks repaired", report.removed_chunks)
                                },
                            );
                        }
                        if let Some(report) = world.last_repair_report {
                            let repair_text = if report.removed_chunks == 0 {
                                format!("0 fixed / {} scanned", report.scanned_chunks)
                            } else {
                                format!(
                                    "{} fixed / {} scanned / {} live refreshed",
                                    report.removed_chunks,
                                    report.scanned_chunks,
                                    report.refreshed_loaded_chunks
                                )
                            };
                            crate::ui_kit::status_chip(
                                ui,
                                Icon::Wand,
                                "REPAIR",
                                &repair_text,
                                settings.theme,
                            );
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Layout,
                            "Toolbench",
                            "HUD, world and visual settings",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            editor.open = true;
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Search,
                            "Command Deck",
                            "Search actions and keybinds",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            command_palette.open();
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Open,
                            "Main Menu",
                            "Save and leave this world",
                            false,
                            settings.theme,
                        )
                        .clicked()
                        {
                            save_current_world(
                                settings,
                                active,
                                scratch,
                                player_q,
                                ship_q,
                                ship_inventory,
                                brain,
                                world,
                            );
                            // Wipe chunk entities so the next world starts clean.
                            world.clear_chunks();
                            for (ship, _, _) in ship_q.iter() {
                                if let Some(entity_commands) = commands.get_entity(ship) {
                                    entity_commands.despawn_recursive();
                                }
                            }
                            for (_, group) in streamer.entities.drain() {
                                for entry in group {
                                    if let Some(entity_commands) = commands.get_entity(entry.entity)
                                    {
                                        entity_commands.despawn_recursive();
                                    }
                                }
                            }
                            next.set(GameState::MainMenu);
                        }
                        ui.add_space(6.0);
                        if crate::ui_kit::danger_action(ui, Icon::Quit, "Quit", settings.theme)
                            .clicked()
                        {
                            save_current_world(
                                settings,
                                active,
                                scratch,
                                player_q,
                                ship_q,
                                ship_inventory,
                                brain,
                                world,
                            );
                            exit.send(AppExit::Success);
                        }
                    });
                });
        });
}

fn draw_inventory(
    ctx: &egui::Context,
    hotbar: &mut HotbarState,
    builder: &mut crate::builder::BuilderState,
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
    settings: &mut WorldSettings,
    ship_inventory: &mut crate::ships::ShipInventory,
    ship_placement: &mut crate::ships::ShipPlacementState,
    mode: &mut ModeContext,
    brain: &mut crate::bots::FriendlyWorldBrain,
) {
    let theme = settings.theme;
    let colors = theme.semantic();
    // A single quiet scrim keeps the modal readable without a full-screen
    // stack of scanline draw calls.
    egui::Area::new(egui::Id::new("inv_backdrop"))
        .fixed_pos(egui::pos2(0.0, 0.0))
        .order(egui::Order::Background)
        .show(ctx, |ui| {
            let rect = ctx.screen_rect();
            let p = ui.painter();
            // Deep base — almost black with a hint of cool blue.
            p.rect_filled(
                rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    colors.background.r(),
                    colors.background.g(),
                    colors.background.b(),
                    238,
                ),
            );
            p.line_segment(
                [
                    egui::pos2(rect.left(), rect.top() + 1.0),
                    egui::pos2(rect.right(), rect.top() + 1.0),
                ],
                egui::Stroke::new(1.0, colors.outline),
            );
        });

    let screen = ctx.screen_rect();
    let panel_w = 980.0_f32.min(screen.width() - 40.0);
    let panel_h = 700.0_f32.min(screen.height() - 60.0);
    let pos = egui::pos2(
        screen.center().x - panel_w * 0.5,
        screen.center().y - panel_h * 0.5,
    );

    let catalog = crate::blocks::block_palette_catalog();
    let palette = inventory_block_entries();

    let mut frame = command_frame(theme);
    frame.inner_margin = egui::Margin::symmetric(20.0, 18.0);

    egui::Window::new("voxel_native_inventory")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .frame(frame)
        .show(ctx, |ui| {
            // -------- Header (title + accent bar) --------
            ui.horizontal(|ui| {
                // Cyan accent bar on the left.
                let (bar_rect, _) =
                    ui.allocate_exact_size(egui::vec2(4.0, 32.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    bar_rect,
                    egui::Rounding::same(2.0),
                    colors.accent,
                );
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("INVENTAR")
                            .size(24.0)
                            .color(colors.text)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Block waehlen ▸ Slot zuweisen ▸ bauen")
                            .size(11.0)
                            .color(colors.text_muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("E / ESC ▸ schliessen")
                            .size(11.0)
                            .color(colors.text_muted),
                    );
                });
            });
            ui.add_space(14.0);

            // -------- Persisted UI state --------
            let mut active_page: InventoryPage = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_page")))
                .unwrap_or(InventoryPage::Blocks);
            let default_block = if palette.iter().any(|entry| entry.block == builder.block) {
                builder.block
            } else {
                palette
                    .first()
                    .map(|entry| entry.block)
                    .unwrap_or(crate::blocks::BlockType::Stone)
            };
            let mut selected: crate::blocks::BlockType = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_selected")))
                .unwrap_or(default_block);
            if !palette.iter().any(|entry| entry.block == selected) {
                selected = default_block;
            }
            let mut active_category: usize = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_catalog_category")))
                .unwrap_or(ALL_INVENTORY_CATEGORIES);
            if active_category != ALL_INVENTORY_CATEGORIES && active_category >= catalog.len() {
                active_category = ALL_INVENTORY_CATEGORIES;
            }
            let mut search: String = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_search")))
                .unwrap_or_default();

            ui.horizontal_wrapped(|ui| {
                for (page, icon, label) in [
                    (InventoryPage::Blocks, Icon::Cube, "Blocks"),
                    (InventoryPage::Ships, Icon::Globe, "Ships"),
                    (InventoryPage::Companions, Icon::Follow, "Companions"),
                    (InventoryPage::Hotbar, Icon::Grid, "Hotbar"),
                ] {
                    if crate::ui_kit::tab_chip(ui, icon, label, active_page == page, theme)
                        .clicked()
                    {
                        active_page = page;
                    }
                }
            });
            ui.add_space(12.0);

            if active_page == InventoryPage::Companions {
                draw_inventory_companion_panel(ui, settings, brain, pause_screen, next);
                ui.add_space(14.0);
            }

            // -------- Search + category tabs --------
            if active_page == InventoryPage::Blocks {
                ui.horizontal_wrapped(|ui| {
                    ui.allocate_ui(egui::vec2(220.0, 32.0), |ui| {
                        ui.set_width(220.0);
                        crate::ui_kit::search_box(ui, &mut search, "Block suchen...", theme);
                    });
                    ui.add_space(8.0);
                    for (category, name) in
                        std::iter::once((ALL_INVENTORY_CATEGORIES, "ALLE")).chain(
                            catalog
                                .iter()
                                .enumerate()
                                .map(|(category, group)| (category, group.label)),
                        )
                    {
                        let selected_category = active_category == category;
                        if crate::ui_kit::choice_chip_sized(
                            ui,
                            name,
                            selected_category,
                            92.0,
                            theme,
                        )
                        .clicked()
                        {
                            active_category = category;
                        }
                    }
                });
                ui.add_space(16.0);

                // -------- Filter block list --------
                let search_lc = search.trim().to_lowercase();
                let visible: Vec<(usize, InventoryBlockEntry)> = palette
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, entry)| {
                        (active_category == ALL_INVENTORY_CATEGORIES
                            || entry.category == active_category)
                            && inventory_entry_matches(
                                *entry,
                                catalog[entry.category].label,
                                &search_lc,
                            )
                    })
                    .collect();

                let (grid_columns, tile_width) = inventory_grid_metrics(ui.available_width() - 8.0);
                // -------- Adaptive material grid --------
                egui::ScrollArea::vertical()
                    .max_height((panel_h - 340.0).max(220.0))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("inv_grid")
                            .num_columns(grid_columns)
                            .spacing([10.0, 10.0])
                            .show(ui, |ui| {
                                for (col_idx, (i, entry)) in visible.iter().enumerate() {
                                    draw_block_tile(
                                        ui,
                                        &entry.block,
                                        entry.label,
                                        selected == entry.block,
                                        |_| selected = entry.block,
                                        *i,
                                        tile_width,
                                        theme,
                                    );
                                    if (col_idx + 1) % grid_columns == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                    });
            }

            ui.add_space(14.0);
            crate::ui_kit::compact_separator(ui, theme);
            ui.add_space(12.0);

            // -------- Selected-block info strip --------
            let selected_entry = palette
                .iter()
                .find(|entry| entry.block == selected)
                .or_else(|| palette.first())
                .expect("canonical block palette must not be empty");
            let sel_b = selected_entry.block;
            let sel_name = selected_entry.label;
            let sel_rgba = crate::blocks::voxel_color(sel_b.into());
            if matches!(active_page, InventoryPage::Blocks | InventoryPage::Hotbar) {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
                crate::ui_kit::paint_material_swatch(
                    ui.painter(),
                    rect,
                    rgba_color32(sel_rgba),
                    5.0,
                );
                ui.painter().rect_stroke(
                    rect,
                    egui::Rounding::same(5.0),
                    egui::Stroke::new(1.5, colors.outline_active),
                );
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(sel_name)
                            .size(17.0)
                            .monospace()
                            .color(colors.text)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Aktive Auswahl ▸ klick einen Hotbar-Slot unten")
                            .size(11.0)
                            .monospace()
                            .color(colors.text_muted),
                    );
                });
            });
            }

            ui.add_space(14.0);

            // -------- Hangar shuttle row --------
            if active_page == InventoryPage::Ships {
            ui.label(
                egui::RichText::new("HANGAR  SHUTTLES")
                    .size(11.5)
                    .monospace()
                    .color(colors.accent)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let r = crate::ui_kit::icon_action(
                    ui,
                    Icon::Animation,
                    "Drone AI",
                    settings.ship_skirmish_ai,
                    theme,
                );
                if r.clicked() {
                    settings.ship_skirmish_ai = !settings.ship_skirmish_ai;
                    settings.save();
                }
            });
            ui.label(
                egui::RichText::new(
                    "Aus = keine Orbital-Gegner beim Fliegen. Ein = Wellen, mehr mit der Zeit, Pausen bei wenig Schild.",
                )
                .size(10.5)
                .monospace()
                .color(colors.text_muted),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for kind in crate::ships::ShipKind::ALL {
                    let unlocked = ship_inventory.unlocked.contains(&kind);
                    let selected_ship = ship_inventory.selected == kind;
                    let resp = ui
                        .add_enabled_ui(unlocked, |ui| {
                            crate::ui_kit::choice_chip_sized(
                                ui,
                                kind.short(),
                                selected_ship,
                                130.0,
                                theme,
                            )
                        })
                        .inner;
                    if resp.clicked() {
                        ship_inventory.selected = kind;
                        ship_placement.start(kind);
                        mode.set(
                            crate::mode::ActiveMode::ShipPlacement { kind },
                            format!("Placing {}.", kind.label()),
                        );
                        *pause_screen = PauseScreen::Menu;
                        next.set(GameState::InGame);
                    }
                    resp.on_hover_text(format!(
                        "{} blueprint: LMB place, RMB cancel, mouse wheel rotate.",
                        kind.label()
                    ));
                }
            });
            }

            ui.add_space(14.0);

            // -------- Hotbar assignment row --------
            if matches!(active_page, InventoryPage::Blocks | InventoryPage::Hotbar) {
            ui.label(
                egui::RichText::new("HOTBAR  1 — 9")
                    .size(11.5)
                    .monospace()
                    .color(colors.accent)
                    .strong(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for i in 0..9 {
                    let slot = hotbar.slots[i];
                    let is_active = hotbar.active == i;
                    let c = slot.color.to_srgba();
                    if crate::ui_kit::swatch_slot(
                        ui,
                        i,
                        rgba_color32([c.red, c.green, c.blue, 1.0]),
                        is_active,
                        theme,
                        slot.label(),
                    )
                    .clicked()
                    {
                        hotbar.assign_block(i, sel_b);
                        apply_inventory_block_selection(builder, sel_b);
                    }
                    ui.add_space(4.0);
                }
            });
            }

            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                if crate::ui_kit::command_action(
                    ui,
                    "Schliessen (E)",
                    None,
                    ActionTone::Standard,
                    40.0,
                    theme,
                )
                .clicked()
                {
                    *pause_screen = PauseScreen::Menu;
                    next.set(GameState::InGame);
                }
            });

            // Persist UI state so reopening preserves selection/search.
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::new("inv_selected"), selected);
                d.insert_temp(
                    egui::Id::new("inv_catalog_category"),
                    active_category,
                );
                d.insert_temp(egui::Id::new("inv_search"), search);
                d.insert_temp(egui::Id::new("inv_page"), active_page);
            });

            // The catalog is a real builder material picker, not a cosmetic
            // hotbar swatch. Selecting a tile updates the active voxel tool
            // immediately; assigning a slot keeps the same typed identity.
            if builder.block != selected {
                apply_inventory_block_selection(builder, selected);
            }
        });
}

/// Paint a vertical-gradient swatch — top is the block's true colour
/// brightened, bottom is the same colour darkened. Cheap fake "lit cube"
/// look that makes the inventory tiles feel 3D without any real geometry.
fn draw_inventory_companion_panel(
    ui: &mut egui::Ui,
    settings: &mut WorldSettings,
    brain: &mut crate::bots::FriendlyWorldBrain,
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
) {
    let theme = settings.theme;
    let companions: Vec<(u64, String, String, String)> = brain
        .save
        .agents
        .iter()
        .filter(|b| b.companion)
        .map(|b| {
            (
                b.id,
                b.name.clone(),
                b.companion_mode.label().to_owned(),
                b.memory.last_message.clone(),
            )
        })
        .collect();

    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Follow,
                "COMPANIONS",
                &companions.len().to_string(),
                theme,
            );
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
    });
    ui.add_space(8.0);

    ui.horizontal_wrapped(|ui| {
        for (id, name, mode, last) in companions {
            draw_inventory_companion_card(
                ui,
                brain,
                id,
                &name,
                &mode,
                &last,
                pause_screen,
                next,
                theme,
            );
        }
    });

    if settings.companion_ui.editor_assist_enabled {
        ui.add_space(8.0);
        crate::ui_kit::surface_panel(ui, theme, |ui| {
            ui.horizontal_wrapped(|ui| {
                for assist in crate::bots::CompanionAssistKind::ALL {
                    let icon = match assist {
                        crate::bots::CompanionAssistKind::Road => Icon::Road,
                        crate::bots::CompanionAssistKind::LandingPad => Icon::Teleport,
                        crate::bots::CompanionAssistKind::Lights => Icon::LightBulb,
                        crate::bots::CompanionAssistKind::ClearFlatten => Icon::Eraser,
                        crate::bots::CompanionAssistKind::Recolor => Icon::Pipette,
                        crate::bots::CompanionAssistKind::Repair => Icon::Wand,
                        crate::bots::CompanionAssistKind::Beautify => Icon::Brush,
                        crate::bots::CompanionAssistKind::TargetRange => Icon::Pin,
                    };
                    if crate::ui_kit::icon_action(ui, icon, assist.label(), false, theme).clicked()
                    {
                        brain.companion_command =
                            Some(crate::bots::CompanionCommand::PreviewAssist(assist));
                        close_inventory_for_companion_command(pause_screen, next);
                    }
                }
                if let Some(preview) = &brain.save.companion_preview {
                    let can_approve = preview.status.is_valid();
                    let approve = crate::ui_kit::icon_action(
                        ui,
                        Icon::Approve,
                        "Approve",
                        can_approve,
                        theme,
                    );
                    if can_approve && approve.clicked() {
                        brain.companion_command =
                            Some(crate::bots::CompanionCommand::ExecutePreview);
                        close_inventory_for_companion_command(pause_screen, next);
                    }
                    if crate::ui_kit::danger_action(ui, Icon::Delete, "Clear", theme).clicked() {
                        brain.companion_command = Some(crate::bots::CompanionCommand::ClearPreview);
                        close_inventory_for_companion_command(pause_screen, next);
                    }
                    crate::ui_kit::status_chip(ui, Icon::Help, "PREVIEW", &preview.message, theme);
                }
            });
        });
    }
}

fn draw_inventory_companion_card(
    ui: &mut egui::Ui,
    brain: &mut crate::bots::FriendlyWorldBrain,
    id: u64,
    name: &str,
    mode: &str,
    last: &str,
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
    theme: crate::theme::ThemeSettings,
) {
    let colors = theme.semantic();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(330.0, 166.0), egui::Sense::hover());
    let mut child = ui.child_ui(rect, egui::Layout::top_down(egui::Align::Min), None);
    child
        .painter()
        .rect_filled(rect, egui::Rounding::same(8.0), colors.surface);
    child.painter().rect_stroke(
        rect,
        egui::Rounding::same(8.0),
        egui::Stroke::new(1.2, colors.stroke),
    );
    child.set_clip_rect(rect.shrink(8.0));
    child.add_space(8.0);
    child.horizontal(|ui| {
        let (avatar, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
        let p = ui.painter_at(avatar);
        p.circle_filled(avatar.center(), 21.0, colors.surface_strong);
        crate::icons::paint_icon(&p, avatar.shrink(9.0), Icon::Follow, colors.accent);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(format!("{name} // {mode}"))
                    .size(14.0)
                    .strong()
                    .color(colors.text),
            );
            ui.label(
                egui::RichText::new(last)
                    .size(10.5)
                    .color(colors.text_muted),
            );
        });
    });
    child.add_space(6.0);
    child.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Teleport, "Here", false, theme)
            .on_hover_text("Place beside me")
            .clicked()
        {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::PlaceSelectedNearPlayer,
                pause_screen,
                next,
            );
        }
        if crate::ui_kit::icon_action(ui, Icon::Follow, "Follow", false, theme).clicked() {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::FollowSelected,
                pause_screen,
                next,
            );
        }
        if crate::ui_kit::icon_action(ui, Icon::Hold, "Hold", false, theme).clicked() {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::HoldSelected,
                pause_screen,
                next,
            );
        }
        if crate::ui_kit::icon_action(ui, Icon::Scan, "Scan", false, theme).clicked() {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::ScanSelected,
                pause_screen,
                next,
            );
        }
        if crate::ui_kit::icon_action(ui, Icon::Teleport, "Pad", false, theme).clicked() {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::PreviewAssist(
                    crate::bots::CompanionAssistKind::LandingPad,
                ),
                pause_screen,
                next,
            );
        }
        if crate::ui_kit::icon_action(ui, Icon::Road, "Road", false, theme).clicked() {
            issue_inventory_companion_command(
                brain,
                id,
                crate::bots::CompanionCommand::PreviewAssist(
                    crate::bots::CompanionAssistKind::Road,
                ),
                pause_screen,
                next,
            );
        }
    });
}

fn issue_inventory_companion_command(
    brain: &mut crate::bots::FriendlyWorldBrain,
    id: u64,
    command: crate::bots::CompanionCommand,
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
) {
    brain.selected_bot = id;
    brain.companion_command = Some(command);
    close_inventory_for_companion_command(pause_screen, next);
}

fn close_inventory_for_companion_command(
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
) {
    *pause_screen = PauseScreen::Menu;
    next.set(GameState::InGame);
}

fn rgba_color32(rgba: [f32; 4]) -> egui::Color32 {
    let channel = |value: f32| {
        if value.is_finite() {
            (value.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            0
        }
    };
    egui::Color32::from_rgba_unmultiplied(
        channel(rgba[0]),
        channel(rgba[1]),
        channel(rgba[2]),
        channel(rgba[3]),
    )
}

/// Render one responsive material card through the shared interaction system.
fn draw_block_tile(
    ui: &mut egui::Ui,
    b: &crate::blocks::BlockType,
    name: &str,
    selected: bool,
    mut on_click: impl FnMut(usize),
    idx: usize,
    width: f32,
    theme: crate::theme::ThemeSettings,
) {
    let detail = format!("ID {:02}", idx + 1);
    if crate::ui_kit::swatch_card(
        ui,
        rgba_color32(crate::blocks::voxel_color((*b).into())),
        name,
        &detail,
        selected,
        egui::vec2(width, 82.0),
        theme,
    )
    .clicked()
    {
        on_click(idx);
    }
}

// ============================ Helpers =====================================

fn auto_world_name(worlds: &[WorldMeta]) -> String {
    let reserved = settings::reserved_world_storage_stems();
    auto_world_name_with_reserved(worlds, &reserved)
}

fn clean_new_world_name(input: &str, worlds: &[WorldMeta]) -> String {
    let reserved = settings::reserved_world_storage_stems();
    clean_new_world_name_with_reserved(input, worlds, &reserved)
}

fn auto_world_name_with_reserved(
    worlds: &[WorldMeta],
    reserved: &std::collections::HashSet<String>,
) -> String {
    for n in 1..1000 {
        let candidate = format!("world_{n:02}");
        if !world_storage_stem_taken(&candidate, worlds, reserved) {
            return candidate;
        }
    }
    format!("world_{}", rand_seed())
}

fn clean_new_world_name_with_reserved(
    input: &str,
    worlds: &[WorldMeta],
    reserved: &std::collections::HashSet<String>,
) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return auto_world_name_with_reserved(worlds, reserved);
    }
    if !world_storage_stem_taken(trimmed, worlds, reserved) {
        return trimmed.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{trimmed}_{n:02}");
        if !world_storage_stem_taken(&candidate, worlds, reserved) {
            return candidate;
        }
    }
    format!("{}_{}", trimmed, rand_seed())
}

fn world_storage_stem_taken(
    name: &str,
    worlds: &[WorldMeta],
    reserved: &std::collections::HashSet<String>,
) -> bool {
    let stem = settings::world_storage_stem(name);
    reserved.contains(&stem)
        || worlds
            .iter()
            .any(|world| settings::world_storage_stem(&world.name) == stem)
}

fn apply_world_to_settings(meta: &WorldMeta, settings: &mut WorldSettings) {
    settings.seed = meta.seed;
    settings.time_of_day = meta.time_of_day;
    settings.time_mode = meta.time_mode;
    settings.cycle_speed = meta.cycle_speed;
    settings.weather = meta.weather;
    settings.scenery_quality = meta.scenery_quality;
    if settings.visual_preset == crate::settings::VisualPreset::NeonShuttle {
        settings.time_mode = crate::settings::TimeMode::Fixed;
        settings.time_of_day = 21.35;
        settings.fov_deg = settings.fov_deg.max(72.0);
        settings
            .weather
            .apply_preset(crate::settings::WeatherPreset::Clear);
    }
}

fn save_current_world(
    settings: &WorldSettings,
    active: Option<&ActiveWorld>,
    scratch: &PlayerProgressScratch,
    player_q: &Query<(&Transform, &Player)>,
    ship_q: &Query<(Entity, &Transform, &crate::ships::ShipInstance)>,
    ship_inventory: &crate::ships::ShipInventory,
    brain: &crate::bots::FriendlyWorldBrain,
    world: &VoxelWorld,
) {
    let Some(active) = active else {
        return;
    };
    let mut meta = active.meta.clone();
    meta.seed = settings.seed;
    meta.time_of_day = settings.time_of_day;
    meta.time_mode = settings.time_mode;
    meta.cycle_speed = settings.cycle_speed;
    meta.weather = settings.weather;
    meta.scenery_quality = settings.scenery_quality;
    meta.ship_inventory = ship_inventory.clone();
    meta.bot_world = brain.save.clone();
    crate::bots::save_bot_world_files(&meta.name, &brain.save);
    meta.world_edit_manifest = crate::world::save_edited_overrides_for_world(&meta.name, world);
    meta.ships = ship_q
        .iter()
        .map(|(_, tf, ship)| {
            crate::ships::SavedShipInstance::from_world(ship.kind, tf, ship.shield)
        })
        .collect();
    if let Ok((tf, player)) = player_q.get_single() {
        meta.player_pos = [tf.translation.x, tf.translation.y, tf.translation.z];
        meta.player_yaw = player.yaw;
        meta.player_pitch = player.pitch;
    }
    meta.player_mining = scratch.mining;
    meta.player_suit = scratch.suit;
    meta.last_played_epoch = crate::platform::now_epoch();
    settings::save_world(&meta);
    settings.save();
}

fn rand_seed() -> u32 {
    let n = crate::platform::now_nanos_seed();
    (n as u32) ^ ((n >> 32) as u32) ^ 0x9E3779B1
}
