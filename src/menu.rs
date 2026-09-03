//! Menu system: Main-Menu, Pause-Menu, Inventory, Game-State transitions.
//!
//! Minecraft-style flow:
//!   * Start -> MainMenu (Neue Welt / Welt laden / Einstellungen / Beenden)
//!   * InGame + ESC -> Paused (Weiter / Speichern / Einstellungen / Hauptmenue / Beenden)
//!   * InGame + E   -> Inventory (block palette grid)
//!   * F3           -> build toolbelt / editor mode (via toolbelt.rs)
//!   * Shift+F3     -> debug overlay toggle (via hud.rs)
//!   * Space double -> toggle fly (via player.rs)

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
use crate::settings::{self, ActiveWorld, WorldMeta, WorldSettings};
use crate::theme::{command_frame, draw_neural_backdrop, metric_pill, CYAN, TEXT};
use crate::world::{ChunkStreamer, VoxelWorld};

#[derive(Clone, Copy, PartialEq, Eq)]
enum InventoryPage {
    Blocks,
    Ships,
    Companions,
    Inventions,
    Hotbar,
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
            .insert_resource(PendingWorldLoad::default())
            .add_systems(Update, handle_keys.run_if(not_in_menu_text_edit))
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

fn not_in_menu_text_edit() -> bool {
    true
}

/// ESC and E drive the state machine. The editor window close button also
/// flips PauseScreen back to Menu, but key handling lives here for clarity.
fn handle_keys(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    mut editor: ResMut<EditorState>,
    command_palette: Option<ResMut<CommandPaletteState>>,
    mode: Option<Res<ModeContext>>,
) {
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
            } else if keys.just_pressed(KeyCode::KeyE) {
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
            if keys.just_pressed(KeyCode::KeyE)
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

fn draw_main_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<GameState>>,
    mut commands: Commands,
    mut form: ResMut<NewWorldForm>,
    mut settings: ResMut<WorldSettings>,
    mut editor: ResMut<EditorState>,
    mut command_palette: ResMut<CommandPaletteState>,
    mut pending: ResMut<PendingWorldLoad>,
    mut exit: EventWriter<AppExit>,
) {
    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();
    // Drive animations from egui's internal time (wall-clock seconds).
    let t_anim = ctx.input(|i| i.time) as f32;
    ctx.request_repaint(); // keep backdrop animated

    draw_neural_backdrop(ctx, settings.theme, t_anim);
    let painter = ctx.layer_painter(egui::LayerId::background());
    let theme = settings.theme;
    let primary = theme.color.primary();
    let dim = theme.color.dim();

    // ---- Title with neon glow + animated underline ----
    let title_pos = egui::pos2(screen.center().x, screen.top() + 100.0);
    let glow_pulse = (t_anim * 1.5).sin() * 0.25 + 0.75;
    // Glow halo behind text.
    for g in (1..=4).rev() {
        let a = (30.0 * glow_pulse / g as f32) as u8;
        painter.text(
            title_pos + egui::vec2(0.0, 0.0),
            egui::Align2::CENTER_CENTER,
            "VOXEL-NATIVE",
            egui::FontId::monospace(64.0 + g as f32 * 2.0),
            egui::Color32::from_rgba_unmultiplied(primary.r(), primary.g(), primary.b(), a),
        );
    }
    painter.text(
        title_pos,
        egui::Align2::CENTER_CENTER,
        "VOXEL-NATIVE",
        egui::FontId::monospace(64.0),
        TEXT,
    );
    painter.text(
        egui::pos2(title_pos.x, title_pos.y + 50.0),
        egui::Align2::CENTER_CENTER,
        "// COMMAND DECK // RUST + BEVY + WGPU // ZERO BROWSER TAX",
        egui::FontId::monospace(15.0),
        dim,
    );
    // Animated accent bar under title.
    let bar_w = 360.0;
    let bar_y = title_pos.y + 68.0;
    let bar_rect = egui::Rect::from_min_size(
        egui::pos2(title_pos.x - bar_w * 0.5, bar_y),
        egui::vec2(bar_w, 2.0),
    );
    painter.rect_filled(
        bar_rect,
        egui::Rounding::same(1.0),
        egui::Color32::from_rgba_unmultiplied(dim.r(), dim.g(), dim.b(), 180),
    );
    // Moving glow on the bar.
    let glow_x = (t_anim * 0.8).sin() * 0.5 + 0.5;
    let gx = bar_rect.left() + glow_x * (bar_w - 60.0);
    painter.rect_filled(
        egui::Rect::from_min_size(egui::pos2(gx, bar_y - 0.5), egui::vec2(60.0, 3.0)),
        egui::Rounding::same(1.5),
        primary,
    );

    // Centered card panel.
    let worlds = settings::list_worlds();
    let latest_world = worlds
        .iter()
        .max_by_key(|meta| meta.last_played_epoch)
        .cloned();
    let panel_w = 720.0_f32.min(screen.width() - 40.0);
    let panel_h = 590.0_f32.min(screen.height() - 190.0);
    let pos = egui::pos2(screen.center().x - panel_w * 0.5, screen.top() + 180.0);

    egui::Window::new("voxel_native_main_menu")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(crate::ui_kit::toolbench_frame(theme))
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("LIQUID GLASS ENGINE")
                            .size(23.0)
                            .color(primary)
                            .strong()
                            .monospace(),
                    );
                    ui.label(
                        egui::RichText::new("Starten, fliegen, bauen - mit smarter Konfiguration.")
                            .size(11.0)
                            .color(dim)
                            .monospace(),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    metric_pill(ui, theme, "WORLDS", &worlds.len().to_string());
                    metric_pill(ui, theme, "SEED", &format!("{:08X}", settings.seed));
                });
            });
            crate::ui_kit::compact_separator(ui, theme);
            ui.add_space(10.0);

            if let Some(meta) = latest_world.as_ref() {
                crate::ui_kit::surface_panel(ui, theme, |ui| {
                    ui.horizontal(|ui| {
                        if crate::ui_kit::major_action(
                            ui,
                            Icon::Resume,
                            "Continue",
                            &format!("{}  seed {}", meta.name, meta.seed),
                            false,
                            theme,
                        )
                        .clicked()
                        {
                            apply_world_to_settings(meta, &mut settings);
                            commands.insert_resource(ActiveWorld { meta: meta.clone() });
                            editor.open = false;
                            pending.0 = true;
                            next.set(GameState::InGame);
                        }
                        ui.vertical(|ui| {
                            crate::ui_kit::status_chip(ui, Icon::Globe, "WORLD", &meta.name, theme);
                            crate::ui_kit::status_chip(
                                ui,
                                Icon::Teleport,
                                "POS",
                                &format!(
                                    "{:.0}/{:.0}/{:.0}",
                                    meta.player_pos[0], meta.player_pos[1], meta.player_pos[2]
                                ),
                                theme,
                            );
                        });
                    });
                });
                ui.add_space(8.0);
            }

            crate::ui_kit::surface_panel(ui, theme, |ui| {
                ui.horizontal(|ui| {
                    crate::ui_kit::status_chip(ui, Icon::New, "NEW WORLD", "seed + name", theme);
                    ui.add(
                        egui::TextEdit::singleline(&mut form.name)
                            .hint_text(auto_world_name(&worlds))
                            .desired_width(190.0),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut form.seed_text)
                            .hint_text("Seed")
                            .desired_width(86.0),
                    );
                    if crate::ui_kit::icon_square(ui, Icon::Seed, false, theme, "Random seed")
                        .clicked()
                    {
                        form.seed_text = rand_seed().to_string();
                    }
                    if crate::ui_kit::icon_action(ui, Icon::Play, "Start", false, theme).clicked() {
                        let seed = form
                            .seed_text
                            .parse::<u32>()
                            .unwrap_or_else(|_| rand_seed());
                        let name = if form.name.trim().is_empty() {
                            auto_world_name(&worlds)
                        } else {
                            form.name.trim().to_string()
                        };
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
                });
            });

            ui.add_space(10.0);
            crate::ui_kit::status_chip(ui, Icon::Open, "WORLDS", &worlds.len().to_string(), theme);
            if worlds.is_empty() {
                ui.label(
                    egui::RichText::new("Noch keine Welten gespeichert.")
                        .color(egui::Color32::from_gray(150)),
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for meta in worlds.iter() {
                            crate::ui_kit::surface_panel(ui, theme, |ui| {
                                ui.horizontal(|ui| {
                                    crate::ui_kit::status_chip(
                                        ui,
                                        Icon::Globe,
                                        "WORLD",
                                        &meta.name,
                                        theme,
                                    );
                                    crate::ui_kit::status_chip(
                                        ui,
                                        Icon::Seed,
                                        "SEED",
                                        &meta.seed.to_string(),
                                        theme,
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if crate::ui_kit::danger_action(
                                                ui,
                                                Icon::Delete,
                                                "Delete",
                                                theme,
                                            )
                                            .clicked()
                                            {
                                                settings::delete_world(&meta.name);
                                            }
                                            if crate::ui_kit::icon_action(
                                                ui,
                                                Icon::Play,
                                                "Play",
                                                false,
                                                theme,
                                            )
                                            .clicked()
                                            {
                                                apply_world_to_settings(meta, &mut settings);
                                                commands.insert_resource(ActiveWorld {
                                                    meta: meta.clone(),
                                                });
                                                editor.open = false;
                                                pending.0 = true;
                                                next.set(GameState::InGame);
                                            }
                                        },
                                    );
                                });
                            });
                            ui.add_space(2.0);
                        }
                    });
            }

            ui.add_space(16.0);
            crate::ui_kit::compact_separator(ui, theme);
            ui.add_space(8.0);
            // Evenly-split footer so buttons can never overlap on
            // narrow panels or long translations.
            ui.columns(3, |cols| {
                cols[0].vertical_centered_justified(|ui| {
                    if crate::ui_kit::icon_action(ui, Icon::Layout, "Toolbench", false, theme)
                        .clicked()
                    {
                        editor.open = true;
                    }
                });
                cols[1].vertical_centered_justified(|ui| {
                    if crate::ui_kit::icon_action(ui, Icon::Search, "Command Deck", false, theme)
                        .clicked()
                    {
                        command_palette.open();
                    }
                });
                cols[2].vertical_centered_justified(|ui| {
                    if crate::ui_kit::danger_action(ui, Icon::Quit, "Quit", theme).clicked() {
                        exit.send(AppExit::Success);
                    }
                });
            });
            ui.add_space(4.0);
        });
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
    let ctx = contexts.ctx_mut();
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
    mut ship_inventory: ResMut<crate::ships::ShipInventory>,
    mut ship_placement: ResMut<crate::ships::ShipPlacementState>,
    mut mode: ResMut<ModeContext>,
    mut brain: ResMut<crate::bots::FriendlyWorldBrain>,
    mut workshop: ResMut<crate::inventions::InventionWorkshop>,
    mut toolbelt: ResMut<crate::toolbelt::ToolbeltState>,
) {
    if *pause_screen != PauseScreen::Inventory {
        return;
    }
    let ctx = contexts.ctx_mut();
    draw_inventory(
        ctx,
        &mut hotbar,
        &mut pause_screen,
        &mut next,
        &mut settings,
        &mut ship_inventory,
        &mut ship_placement,
        &mut mode,
        &mut brain,
        &mut workshop,
        &mut toolbelt,
    );
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
            ui.add_space(12.0);
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
                }
                ui.add_space(6.0);
                if crate::ui_kit::major_action(
                    ui,
                    Icon::Cube,
                    "Inventory",
                    "Blocks, ships, inventions",
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
                            if let Some(entity_commands) = commands.get_entity(entry.entity) {
                                entity_commands.despawn_recursive();
                            }
                        }
                    }
                    next.set(GameState::MainMenu);
                }
                ui.add_space(6.0);
                if crate::ui_kit::danger_action(ui, Icon::Quit, "Quit", settings.theme).clicked() {
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
}

fn draw_inventory(
    ctx: &egui::Context,
    hotbar: &mut HotbarState,
    pause_screen: &mut PauseScreen,
    next: &mut ResMut<NextState<GameState>>,
    settings: &mut WorldSettings,
    ship_inventory: &mut crate::ships::ShipInventory,
    ship_placement: &mut crate::ships::ShipPlacementState,
    mode: &mut ModeContext,
    brain: &mut crate::bots::FriendlyWorldBrain,
    workshop: &mut crate::inventions::InventionWorkshop,
    toolbelt: &mut crate::toolbelt::ToolbeltState,
) {
    let theme = settings.theme;
    // Glassmorphism backdrop: deep gradient + subtle vignette so the
    // panel reads as a focused modal, not a flat overlay.
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
                egui::Color32::from_rgba_unmultiplied(2, 5, 5, 232),
            );
            let mut y = rect.top();
            while y < rect.bottom() {
                p.line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    egui::Stroke::new(1.0, egui::Color32::from_black_alpha(42)),
                );
                y += 5.0;
            }
        });

    let screen = ctx.screen_rect();
    let panel_w = 980.0_f32.min(screen.width() - 40.0);
    let panel_h = 700.0_f32.min(screen.height() - 60.0);
    let pos = egui::pos2(
        screen.center().x - panel_w * 0.5,
        screen.center().y - panel_h * 0.5,
    );

    use crate::blocks::BlockType::*;
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Cat {
        All,
        Natural,
        Stone,
        Wood,
        Liquid,
        Tech,
    }
    let cats: [(Cat, &str); 6] = [
        (Cat::All, "ALLE"),
        (Cat::Natural, "NATUR"),
        (Cat::Stone, "GESTEIN"),
        (Cat::Wood, "HOLZ & LAUB"),
        (Cat::Liquid, "FLUESSIG"),
        (Cat::Tech, "HANGAR"),
    ];
    let palette: [(crate::blocks::BlockType, &str, Cat); 30] = [
        (Grass, "Grass", Cat::Natural),
        (Dirt, "Dirt", Cat::Natural),
        (Sand, "Sand", Cat::Natural),
        (Gravel, "Kies", Cat::Natural),
        (Snow, "Schnee", Cat::Natural),
        (TundraGrass, "Tundra", Cat::Natural),
        (SavannaGrass, "Savanne", Cat::Natural),
        (Stone, "Stone", Cat::Stone),
        (Bedrock, "Bedrock", Cat::Stone),
        (RedSand, "Rotsand", Cat::Natural),
        (RedStone, "Rotstein", Cat::Stone),
        (MesaClay, "Mesa-Ton", Cat::Stone),
        (MossStone, "Moosstein", Cat::Stone),
        (Limestone, "Kalkstein", Cat::Stone),
        (Wood, "Holz", Cat::Wood),
        (Leaves, "Laub", Cat::Wood),
        (JungleLeaves, "Dschungel", Cat::Wood),
        (Water, "Wasser", Cat::Liquid),
        (Ice, "Eis", Cat::Liquid),
        (ShipHullDark, "Hull Dark", Cat::Tech),
        (ShipHullAlloy, "Alloy Hull", Cat::Tech),
        (CockpitGlass, "Cockpit", Cat::Tech),
        (NeonCyan, "Neon Cyan", Cat::Tech),
        (NeonMagenta, "Neon Magenta", Cat::Tech),
        (NeonAmber, "Neon Amber", Cat::Tech),
        (EngineCore, "Engine Core", Cat::Tech),
        (Crystal, "Crystal", Cat::Stone),
        (LuminiteCrystal, "Luminite", Cat::Stone),
        (MagnetiteOre, "Magnetite", Cat::Stone),
        (IridiumVein, "Iridium", Cat::Stone),
    ];

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
                    egui::Color32::from_rgb(0, 220, 255),
                );
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("INVENTAR")
                            .size(28.0)
                            .color(theme.color.primary())
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Block waehlen ▸ Slot zuweisen ▸ bauen")
                            .size(11.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new("E / ESC ▸ schliessen")
                            .size(11.0)
                            .color(egui::Color32::from_gray(120))
                            .italics(),
                    );
                });
            });
            ui.add_space(14.0);

            // -------- Persisted UI state --------
            let mut active_page: InventoryPage = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_page")))
                .unwrap_or(InventoryPage::Blocks);
            let mut selected: u8 = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_selected")))
                .unwrap_or(0);
            let mut active_cat: Cat = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_cat")))
                .unwrap_or(Cat::All);
            let mut search: String = ui
                .data_mut(|d| d.get_temp(egui::Id::new("inv_search")))
                .unwrap_or_default();

            ui.horizontal_wrapped(|ui| {
                for (page, icon, label) in [
                    (InventoryPage::Blocks, Icon::Cube, "Blocks"),
                    (InventoryPage::Ships, Icon::Globe, "Ships"),
                    (InventoryPage::Companions, Icon::Follow, "Companions"),
                    (InventoryPage::Inventions, Icon::LightBulb, "Inventions"),
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
            ui.horizontal(|ui| {
                let te = egui::TextEdit::singleline(&mut search)
                    .hint_text("🔍  Block suchen…")
                    .desired_width(220.0)
                    .font(egui::FontId::proportional(13.0));
                ui.add(te);
                ui.add_space(14.0);
                for (c, name) in cats.iter() {
                    let sel = active_cat == *c;
                    let btn = egui::Button::new(
                        egui::RichText::new(*name)
                            .size(11.5)
                            .color(if sel {
                                egui::Color32::from_rgb(8, 14, 22)
                            } else {
                                egui::Color32::from_gray(200)
                            })
                            .strong(),
                    )
                    .fill(if sel {
                        egui::Color32::from_rgb(0, 220, 255)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(40, 50, 66, 200)
                    })
                    .stroke(egui::Stroke::new(
                        1.0,
                        if sel {
                            egui::Color32::from_rgb(0, 240, 255)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(70, 90, 110, 180)
                        },
                    ))
                    .rounding(egui::Rounding::same(20.0))
                    .min_size(egui::vec2(92.0, 28.0));
                    if ui.add(btn).clicked() {
                        active_cat = *c;
                    }
                }
            });
            ui.add_space(16.0);

            // -------- Filter block list --------
            let search_lc = search.to_lowercase();
            let visible: Vec<(usize, crate::blocks::BlockType, &str)> = palette
                .iter()
                .enumerate()
                .filter(|(_, (_, name, cat))| {
                    (active_cat == Cat::All || *cat == active_cat)
                        && (search_lc.is_empty() || name.to_lowercase().contains(&search_lc))
                })
                .map(|(i, (b, n, _))| (i, *b, *n))
                .collect();

            // -------- Block grid (5 columns of cinematic cards) --------
            egui::ScrollArea::vertical()
                .max_height((panel_h - 340.0).max(220.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("inv_grid")
                        .num_columns(5)
                        .spacing([12.0, 12.0])
                        .show(ui, |ui| {
                            for (col_idx, (i, b, name)) in visible.iter().enumerate() {
                                draw_block_tile(
                                    ui,
                                    b,
                                    name,
                                    selected as usize == *i,
                                    |sel_idx| {
                                        selected = sel_idx as u8;
                                    },
                                    *i,
                                );
                                if (col_idx + 1) % 5 == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                });
            }

            ui.add_space(14.0);
            // Decorative gradient separator.
            let sep_rect = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 2.0), egui::Sense::hover())
                .0;
            for x in 0..(sep_rect.width() as i32) {
                let t = x as f32 / sep_rect.width().max(1.0);
                let alpha = ((1.0 - (t * 2.0 - 1.0).abs()) * 100.0) as u8;
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(sep_rect.min.x + x as f32, sep_rect.min.y),
                        egui::vec2(1.0, 2.0),
                    ),
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(0, 220, 255, alpha),
                );
            }
            ui.add_space(12.0);

            // -------- Selected-block info strip --------
            let (sel_b, sel_name, _) = palette[selected.min((palette.len() - 1) as u8) as usize];
            let sel_rgba = crate::blocks::voxel_color(sel_b.into());
            if matches!(active_page, InventoryPage::Blocks | InventoryPage::Hotbar) {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
                draw_gradient_swatch(ui.painter(), rect, sel_rgba, 8.0);
                ui.painter().rect_stroke(
                    rect,
                    egui::Rounding::same(8.0),
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 220, 80)),
                );
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(sel_name)
                            .size(20.0)
                            .color(egui::Color32::from_rgb(240, 250, 255))
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Aktive Auswahl ▸ klick einen Hotbar-Slot unten")
                            .size(11.0)
                            .color(egui::Color32::from_gray(150)),
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
                    .color(egui::Color32::from_rgb(255, 80, 230))
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
                .color(egui::Color32::from_gray(145)),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                for kind in crate::ships::ShipKind::ALL {
                    let unlocked = ship_inventory.unlocked.contains(&kind);
                    let selected_ship = ship_inventory.selected == kind;
                    let fill = if selected_ship {
                        egui::Color32::from_rgb(0, 210, 235)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(24, 30, 45, 220)
                    };
                    let text_color = if selected_ship {
                        egui::Color32::from_rgb(5, 10, 18)
                    } else {
                        egui::Color32::from_rgb(230, 245, 255)
                    };
                    let resp = ui.add_enabled(
                        unlocked,
                        egui::Button::new(
                            egui::RichText::new(kind.short())
                                .size(13.0)
                                .strong()
                                .color(text_color),
                        )
                        .fill(fill)
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(255, 80, 230),
                        ))
                        .rounding(egui::Rounding::same(8.0))
                        .min_size(egui::vec2(130.0, 42.0)),
                    );
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

            if active_page == InventoryPage::Inventions {
                ui.label(
                    egui::RichText::new("INVENTION WORKSHOP")
                        .size(11.5)
                        .color(egui::Color32::from_rgb(40, 230, 255))
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Place working machines into the voxel world. Generators harvest crystal and feed turrets, portals, rails, and hover pads.",
                    )
                    .size(10.5)
                    .color(egui::Color32::from_gray(145)),
                );
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for kind in crate::inventions::InventionKind::ALL {
                        let selected = workshop.selected == kind;
                        let fill = if selected {
                            kind.egui_accent()
                        } else {
                            egui::Color32::from_rgba_unmultiplied(24, 30, 45, 220)
                        };
                        let text_color = if selected {
                            egui::Color32::from_rgb(5, 10, 18)
                        } else {
                            egui::Color32::from_rgb(230, 245, 255)
                        };
                        let resp = ui.add(
                            egui::Button::new(
                                egui::RichText::new(kind.chip())
                                    .size(13.0)
                                    .strong()
                                    .color(text_color),
                            )
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, kind.egui_accent()))
                            .rounding(egui::Rounding::same(8.0))
                            .min_size(egui::vec2(130.0, 48.0)),
                        );
                        if resp.clicked() {
                            crate::inventions::arm_invention_tool(workshop, toolbelt, mode, kind);
                            *pause_screen = PauseScreen::Menu;
                            next.set(GameState::InGame);
                        }
                        resp.on_hover_text(format!("{} — {}", kind.label(), kind.blurb()));
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(workshop.status.as_str())
                        .size(11.0)
                        .color(egui::Color32::from_gray(180)),
                );
            }

            ui.add_space(14.0);

            // -------- Hotbar assignment row --------
            if matches!(active_page, InventoryPage::Blocks | InventoryPage::Hotbar) {
            ui.label(
                egui::RichText::new("HOTBAR  1 — 9")
                    .size(11.5)
                    .color(egui::Color32::from_rgb(0, 200, 230))
                    .strong(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for i in 0..9 {
                    let slot = hotbar.slots[i];
                    let is_active = hotbar.active == i;
                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(62.0, 62.0), egui::Sense::click());
                    let hovered = resp.hovered();
                    // Slot background (deep panel).
                    ui.painter().rect_filled(
                        rect,
                        egui::Rounding::same(8.0),
                        egui::Color32::from_rgba_unmultiplied(20, 26, 36, 230),
                    );
                    // Inner gradient swatch with the slot's block colour.
                    let inner = rect.shrink(6.0);
                    let c = slot.color.to_srgba();
                    draw_gradient_swatch(ui.painter(), inner, [c.red, c.green, c.blue, 1.0], 5.0);
                    // Number badge in the corner.
                    ui.painter().text(
                        rect.min + egui::vec2(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}", i + 1),
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_white_alpha(220),
                    );
                    // Active / hover ring.
                    let ring_color = if is_active {
                        egui::Color32::from_rgb(255, 220, 80)
                    } else if hovered {
                        egui::Color32::from_rgb(0, 220, 255)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(60, 80, 100, 200)
                    };
                    ui.painter().rect_stroke(
                        rect,
                        egui::Rounding::same(8.0),
                        egui::Stroke::new(if is_active { 2.5 } else { 1.0 }, ring_color),
                    );
                    if resp.clicked() {
                        let cc = crate::blocks::voxel_color(sel_b.into());
                        hotbar.slots[i] = crate::hud::HotbarBlock {
                            color: Color::srgb(cc[0], cc[1], cc[2]),
                        };
                        hotbar.active = i;
                    }
                    ui.add_space(4.0);
                }
            });
            }

            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                if ui
                    .add(big_button("Schliessen  (E)", [40, 140, 80]))
                    .clicked()
                {
                    *pause_screen = PauseScreen::Menu;
                    next.set(GameState::InGame);
                }
            });

            // Persist UI state so reopening preserves selection/search.
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::new("inv_selected"), selected);
                d.insert_temp(egui::Id::new("inv_cat"), active_cat);
                d.insert_temp(egui::Id::new("inv_search"), search);
                d.insert_temp(egui::Id::new("inv_page"), active_page);
            });
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

fn draw_gradient_swatch(painter: &egui::Painter, rect: egui::Rect, rgba: [f32; 4], radius: f32) {
    let r = rgba[0];
    let g = rgba[1];
    let b = rgba[2];
    let top = egui::Color32::from_rgb(
        ((r * 1.18).min(1.0) * 255.0) as u8,
        ((g * 1.18).min(1.0) * 255.0) as u8,
        ((b * 1.18).min(1.0) * 255.0) as u8,
    );
    let bot = egui::Color32::from_rgb(
        ((r * 0.65) * 255.0) as u8,
        ((g * 0.65) * 255.0) as u8,
        ((b * 0.65) * 255.0) as u8,
    );
    // Approximate a gradient by stacking ~16 horizontal bands. egui has
    // no native gradient primitive, but 16 bands at 1-3 px each are
    // visually indistinguishable from a true gradient at this scale.
    let bands = 16i32;
    let band_h = rect.height() / bands as f32;
    for i in 0..bands {
        let t = i as f32 / (bands - 1).max(1) as f32;
        let cr = top.r() as f32 * (1.0 - t) + bot.r() as f32 * t;
        let cg = top.g() as f32 * (1.0 - t) + bot.g() as f32 * t;
        let cb = top.b() as f32 * (1.0 - t) + bot.b() as f32 * t;
        let band = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y + i as f32 * band_h),
            egui::vec2(rect.width(), band_h + 0.5),
        );
        // Only round the very first / last band so the rounding wraps
        // the swatch as a whole.
        let rounding = if i == 0 {
            egui::Rounding {
                nw: radius,
                ne: radius,
                sw: 0.0,
                se: 0.0,
            }
        } else if i == bands - 1 {
            egui::Rounding {
                nw: 0.0,
                ne: 0.0,
                sw: radius,
                se: radius,
            }
        } else {
            egui::Rounding::ZERO
        };
        painter.rect_filled(
            band,
            rounding,
            egui::Color32::from_rgb(cr as u8, cg as u8, cb as u8),
        );
    }
    // Subtle inner top highlight (specular hint).
    painter.line_segment(
        [
            egui::pos2(rect.min.x + 4.0, rect.min.y + 2.0),
            egui::pos2(rect.max.x - 4.0, rect.min.y + 2.0),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(60)),
    );
}

/// Render a single block tile in the inventory grid. Cinematic card with
/// a gradient swatch, hover/selection ring, and crisp typography.
fn draw_block_tile(
    ui: &mut egui::Ui,
    b: &crate::blocks::BlockType,
    name: &str,
    selected: bool,
    mut on_click: impl FnMut(usize),
    idx: usize,
) {
    let col = crate::blocks::voxel_color((*b).into());

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(168.0, 96.0), egui::Sense::click());
    let hovered = resp.hovered();

    // Card background — deep glass with gentle gradient.
    let bg_top = if selected {
        egui::Color32::from_rgba_unmultiplied(38, 60, 86, 240)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(28, 38, 54, 230)
    } else {
        egui::Color32::from_rgba_unmultiplied(22, 28, 40, 215)
    };
    let bg_bot = if selected {
        egui::Color32::from_rgba_unmultiplied(20, 32, 50, 240)
    } else if hovered {
        egui::Color32::from_rgba_unmultiplied(16, 22, 32, 230)
    } else {
        egui::Color32::from_rgba_unmultiplied(12, 16, 24, 215)
    };
    // Two-band gradient for the card body (cheaper than the swatch's
    // 16 bands — at 96 px tall the eye doesn't notice).
    ui.painter().rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.center().y)),
        egui::Rounding {
            nw: 10.0,
            ne: 10.0,
            sw: 0.0,
            se: 0.0,
        },
        bg_top,
    );
    ui.painter().rect_filled(
        egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.center().y), rect.max),
        egui::Rounding {
            nw: 0.0,
            ne: 0.0,
            sw: 10.0,
            se: 10.0,
        },
        bg_bot,
    );

    // Color swatch (left rounded square with gradient + highlight).
    let swatch =
        egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 10.0), egui::vec2(76.0, 76.0));
    draw_gradient_swatch(ui.painter(), swatch, col, 7.0);
    // Subtle dark inner border to seat the swatch in the card.
    ui.painter().rect_stroke(
        swatch,
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140)),
    );

    // Name + index label.
    ui.painter().text(
        rect.min + egui::vec2(96.0, 30.0),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(15.0),
        egui::Color32::from_rgb(235, 245, 255),
    );
    ui.painter().text(
        rect.min + egui::vec2(96.0, 52.0),
        egui::Align2::LEFT_CENTER,
        format!("ID  {:02}", idx + 1),
        egui::FontId::proportional(10.5),
        egui::Color32::from_gray(140),
    );
    if selected {
        ui.painter().text(
            rect.min + egui::vec2(96.0, 72.0),
            egui::Align2::LEFT_CENTER,
            "● AKTIV",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(255, 220, 80),
        );
    }

    // Outer selection / hover ring.
    let ring = egui::Stroke::new(
        if selected { 2.0 } else { 1.0 },
        if selected {
            egui::Color32::from_rgb(255, 220, 80)
        } else if hovered {
            egui::Color32::from_rgb(0, 220, 255)
        } else {
            egui::Color32::from_rgba_unmultiplied(60, 80, 100, 200)
        },
    );
    ui.painter()
        .rect_stroke(rect, egui::Rounding::same(10.0), ring);

    if resp.clicked() {
        on_click(idx);
    }
}

// ============================ Helpers =====================================

fn big_button(label: &str, fill: [u8; 3]) -> egui::Button<'_> {
    // Command button: darkened semantic fill, high-contrast type,
    // tight radius and a cool outline shared with the command deck.
    let bg = egui::Color32::from_rgb(
        (fill[0] as f32 * 0.85) as u8,
        (fill[1] as f32 * 0.85) as u8,
        (fill[2] as f32 * 0.85) as u8,
    );
    egui::Button::new(egui::RichText::new(label).size(15.5).color(TEXT).strong())
        .fill(bg)
        .stroke(egui::Stroke::new(1.0, CYAN.linear_multiply(0.72)))
        // 0 min-width so the button flexes to whatever column/layout is
        // giving us (prevents overlap in narrow two-column footers), but
        // keeps a crisp tall profile.
        .min_size(egui::vec2(0.0, 42.0))
        .rounding(egui::Rounding::same(6.0))
}

fn auto_world_name(worlds: &[WorldMeta]) -> String {
    for n in 1..1000 {
        let candidate = format!("world_{n:02}");
        if !worlds.iter().any(|world| world.name == candidate) {
            return candidate;
        }
    }
    format!("world_{}", rand_seed())
}

fn apply_world_to_settings(meta: &WorldMeta, settings: &mut WorldSettings) {
    settings.seed = meta.seed;
    settings.time_of_day = meta.time_of_day;
    settings.time_mode = meta.time_mode;
    settings.cycle_speed = meta.cycle_speed;
    settings.weather = meta.weather;
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
