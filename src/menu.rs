//! Menu system: Main-Menu, Pause-Menu, Inventory, Game-State transitions.
//!
//! Minecraft-style flow:
//!   * Start -> MainMenu (Neue Welt / Welt laden / Einstellungen / Beenden)
//!   * InGame + ESC -> Paused (Weiter / Speichern / Einstellungen / Hauptmenue / Beenden)
//!   * InGame + E   -> Inventory (block palette grid)
//!   * F3           -> debug overlay toggle (via hud.rs)
//!   * Space double -> toggle fly (via player.rs)

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts};

use crate::editor::EditorState;
use crate::hud::HotbarState;
use crate::player::Player;
use crate::settings::{self, ActiveWorld, WorldMeta, WorldSettings};
use crate::world::{ChunkStreamer, VoxelWorld};

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
) {
    match state.get() {
        GameState::InGame => {
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

/// On entering InGame, make sure the cursor is released -- the first LMB
/// click from grab_cursor will capture it.
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
    mut pending: ResMut<PendingWorldLoad>,
    mut exit: EventWriter<AppExit>,
) {
    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();

    // Animated backdrop gradient.
    let painter = ctx.layer_painter(egui::LayerId::background());
    painter.rect_filled(screen, 0.0, egui::Color32::from_rgb(4, 8, 16));
    for i in 0..20 {
        let t = i as f32 / 20.0;
        let y = screen.top() + t * screen.height();
        let alpha = (60.0 * (1.0 - t)) as u8;
        painter.line_segment(
            [egui::pos2(screen.left(), y), egui::pos2(screen.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 100, 140, alpha)),
        );
    }

    // Title.
    let title_pos = egui::pos2(screen.center().x, screen.top() + 90.0);
    painter.text(
        title_pos,
        egui::Align2::CENTER_CENTER,
        "VOXEL-NATIVE",
        egui::FontId::proportional(64.0),
        egui::Color32::from_rgb(0, 230, 255),
    );
    painter.text(
        egui::pos2(title_pos.x, title_pos.y + 50.0),
        egui::Align2::CENTER_CENTER,
        "// Rust + Bevy + wgpu",
        egui::FontId::proportional(18.0),
        egui::Color32::from_gray(160),
    );

    // Centered card panel.
    let panel_w = 520.0_f32.min(screen.width() - 40.0);
    let panel_h = 440.0_f32.min(screen.height() - 240.0);
    let pos = egui::pos2(screen.center().x - panel_w * 0.5, screen.top() + 180.0);

    egui::Window::new("voxel_native_main_menu")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("HAUPTMENUE")
                        .size(22.0)
                        .color(egui::Color32::from_rgb(0, 230, 255))
                        .strong(),
                );
            });
            ui.separator();
            ui.add_space(10.0);

            ui.heading("Neue Welt");
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut form.name).hint_text("My World"));
            });
            ui.horizontal(|ui| {
                ui.label("Seed:");
                ui.add(
                    egui::TextEdit::singleline(&mut form.seed_text)
                        .hint_text("Leer = Zufall")
                        .desired_width(140.0),
                );
                if ui.button("Zufall").clicked() {
                    form.seed_text = rand_seed().to_string();
                }
            });
            ui.add_space(6.0);
            let create_enabled = !form.name.trim().is_empty();
            ui.add_enabled_ui(create_enabled, |ui| {
                let btn = big_button(">>  Welt erstellen & spielen", [0, 150, 90]);
                if ui.add(btn).clicked() {
                    let seed = form
                        .seed_text
                        .parse::<u32>()
                        .unwrap_or_else(|_| rand_seed());
                    let name = form.name.trim().to_string();
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

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.heading("Gespeicherte Welten");
            let worlds = settings::list_worlds();
            if worlds.is_empty() {
                ui.label(
                    egui::RichText::new("Noch keine Welten gespeichert.")
                        .color(egui::Color32::from_gray(150)),
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(150.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for meta in worlds {
                            ui.horizontal(|ui| {
                                let label = format!(
                                    "[{}]  seed {}  pos ({:.0}, {:.0}, {:.0})",
                                    meta.name,
                                    meta.seed,
                                    meta.player_pos[0],
                                    meta.player_pos[1],
                                    meta.player_pos[2]
                                );
                                ui.label(label);
                                if ui.button(">> Spielen").clicked() {
                                    apply_world_to_settings(&meta, &mut settings);
                                    commands.insert_resource(ActiveWorld { meta: meta.clone() });
                                    editor.open = false;
                                    pending.0 = true;
                                    next.set(GameState::InGame);
                                }
                                if ui.button("X").clicked() {
                                    settings::delete_world(&meta.name);
                                }
                            });
                        }
                    });
            }

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(big_button("Einstellungen", [40, 80, 120])).clicked() {
                    editor.open = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(big_button("Beenden", [180, 40, 60])).clicked() {
                        exit.send(AppExit::Success);
                    }
                });
            });
        });
}

// ============================ Pause / Inventory ===========================

fn draw_pause_menu(
    mut contexts: EguiContexts,
    mut next: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    settings: Res<WorldSettings>,
    mut editor: ResMut<EditorState>,
    mut hotbar: ResMut<HotbarState>,
    active: Option<Res<ActiveWorld>>,
    player_q: Query<(&Transform, &Player)>,
    mut commands: Commands,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    mut exit: EventWriter<AppExit>,
) {
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

    match *pause_screen {
        PauseScreen::Menu => draw_pause_main(
            ctx,
            &mut next,
            &mut editor,
            &settings,
            active.as_deref(),
            &player_q,
            &mut commands,
            &mut world,
            &mut streamer,
            &mut exit,
        ),
        PauseScreen::Inventory => {
            draw_inventory(ctx, &mut hotbar, &mut pause_screen, &mut next);
        }
    }
}

fn draw_pause_main(
    ctx: &egui::Context,
    next: &mut ResMut<NextState<GameState>>,
    editor: &mut ResMut<EditorState>,
    settings: &WorldSettings,
    active: Option<&ActiveWorld>,
    player_q: &Query<(&Transform, &Player)>,
    commands: &mut Commands,
    world: &mut VoxelWorld,
    streamer: &mut ChunkStreamer,
    exit: &mut EventWriter<AppExit>,
) {
    let screen = ctx.screen_rect();
    let panel_w = 440.0_f32.min(screen.width() - 40.0);
    let panel_h = 420.0_f32.min(screen.height() - 80.0);
    let pos = egui::pos2(
        screen.center().x - panel_w * 0.5,
        screen.center().y - panel_h * 0.5,
    );

    egui::Window::new("voxel_native_pause")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("PAUSE")
                        .size(28.0)
                        .color(egui::Color32::from_rgb(0, 230, 255))
                        .strong(),
                );
                if let Some(active) = active {
                    ui.label(
                        egui::RichText::new(format!("Welt: {}", active.meta.name))
                            .color(egui::Color32::from_gray(180))
                            .size(14.0),
                    );
                }
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                if ui.add(big_button(">>  Weiter spielen  (ESC)", [0, 150, 90])).clicked() {
                    next.set(GameState::InGame);
                }
                ui.add_space(6.0);
                if ui.add(big_button("Speichern (F5)", [40, 110, 160])).clicked() {
                    save_current_world(settings, active, player_q);
                }
                ui.add_space(6.0);
                if ui.add(big_button("Einstellungen", [40, 110, 160])).clicked() {
                    editor.open = true;
                }
                ui.add_space(6.0);
                if ui.add(big_button("Zum Hauptmenue", [120, 80, 40])).clicked() {
                    save_current_world(settings, active, player_q);
                    // Wipe chunk entities so the next world starts clean.
                    world.chunks.clear();
                    for (_, (entity, _handle)) in streamer.entities.drain() {
                        commands.entity(entity).despawn_recursive();
                    }
                    next.set(GameState::MainMenu);
                }
                ui.add_space(6.0);
                if ui.add(big_button("Beenden", [180, 40, 60])).clicked() {
                    save_current_world(settings, active, player_q);
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
) {
    let screen = ctx.screen_rect();
    let panel_w = 620.0_f32.min(screen.width() - 40.0);
    let panel_h = 460.0_f32.min(screen.height() - 60.0);
    let pos = egui::pos2(
        screen.center().x - panel_w * 0.5,
        screen.center().y - panel_h * 0.5,
    );

    egui::Window::new("voxel_native_inventory")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("INVENTAR  /  BLOECKE")
                        .size(22.0)
                        .color(egui::Color32::from_rgb(0, 230, 255))
                        .strong(),
                );
                ui.label(
                    egui::RichText::new("E oder ESC = schliessen")
                        .size(11.0)
                        .color(egui::Color32::from_gray(160)),
                );
            });
            ui.separator();
            ui.add_space(8.0);

            use crate::blocks::BlockType::*;
            let palette: [(crate::blocks::BlockType, &str); 12] = [
                (Grass, "Grass"),
                (Dirt, "Dirt"),
                (Stone, "Stone"),
                (Sand, "Sand"),
                (Wood, "Holz"),
                (Leaves, "Laub"),
                (Snow, "Schnee"),
                (Gravel, "Kies"),
                (Bedrock, "Bedrock"),
                (Water, "Wasser"),
                (Ice, "Eis"),
                (TundraGrass, "Tundra"),
            ];

            ui.label("Auf Block klicken, dann auf Hotbar-Slot klicken zum Zuweisen:");
            ui.add_space(6.0);
            let mut selected = ui
                .data_mut(|d| d.get_temp::<u8>(egui::Id::new("inv_selected")))
                .unwrap_or(0);
            egui::Grid::new("inv_grid")
                .num_columns(6)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    for (i, (b, name)) in palette.iter().enumerate() {
                        let col = crate::blocks::voxel_color((*b).into());
                        let color = egui::Color32::from_rgb(
                            (col[0] * 255.0) as u8,
                            (col[1] * 255.0) as u8,
                            (col[2] * 255.0) as u8,
                        );
                        let is_sel = selected as usize == i;
                        let btn = egui::Button::new(
                            egui::RichText::new(format!(" {name} "))
                                .color(egui::Color32::BLACK)
                                .strong(),
                        )
                        .fill(color)
                        .stroke(egui::Stroke::new(
                            if is_sel { 2.5 } else { 1.0 },
                            if is_sel {
                                egui::Color32::from_rgb(255, 230, 0)
                            } else {
                                egui::Color32::BLACK
                            },
                        ))
                        .min_size(egui::vec2(90.0, 52.0));
                        if ui.add(btn).clicked() {
                            selected = i as u8;
                        }
                        if (i + 1) % 6 == 0 {
                            ui.end_row();
                        }
                    }
                });
            ui.data_mut(|d| d.insert_temp(egui::Id::new("inv_selected"), selected));

            ui.add_space(12.0);
            ui.separator();
            ui.label("Hotbar (1-9):");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for i in 0..9 {
                    let slot = hotbar.slots[i];
                    let is_active = hotbar.active == i;
                    let btn = egui::Button::new(
                        egui::RichText::new(format!(" {} ", i + 1))
                            .color(egui::Color32::BLACK),
                    )
                    .fill(egui::Color32::from_rgb(
                        (slot.color.to_srgba().red * 255.0) as u8,
                        (slot.color.to_srgba().green * 255.0) as u8,
                        (slot.color.to_srgba().blue * 255.0) as u8,
                    ))
                    .stroke(egui::Stroke::new(
                        if is_active { 2.5 } else { 1.0 },
                        if is_active {
                            egui::Color32::from_rgb(255, 230, 0)
                        } else {
                            egui::Color32::from_gray(20)
                        },
                    ))
                    .min_size(egui::vec2(50.0, 50.0));
                    if ui.add(btn).clicked() {
                        let (b, name) = palette[selected as usize];
                        let c = crate::blocks::voxel_color(b.into());
                        hotbar.slots[i] = crate::hud::HotbarBlock {
                            name,
                            color: Color::srgb(c[0], c[1], c[2]),
                        };
                    }
                }
            });

            ui.add_space(14.0);
            ui.vertical_centered(|ui| {
                if ui.add(big_button("Schliessen (E)", [60, 140, 80])).clicked() {
                    *pause_screen = PauseScreen::Menu;
                    next.set(GameState::InGame);
                }
            });
        });
}

// ============================ Helpers =====================================

fn big_button(label: &str, fill: [u8; 3]) -> egui::Button<'_> {
    egui::Button::new(
        egui::RichText::new(label)
            .size(15.0)
            .color(egui::Color32::WHITE)
            .strong(),
    )
    .fill(egui::Color32::from_rgb(fill[0], fill[1], fill[2]))
    .min_size(egui::vec2(360.0, 40.0))
    .rounding(egui::Rounding::same(8.0))
}

fn apply_world_to_settings(meta: &WorldMeta, settings: &mut WorldSettings) {
    settings.seed = meta.seed;
    settings.time_of_day = meta.time_of_day;
    settings.time_mode = meta.time_mode;
    settings.cycle_speed = meta.cycle_speed;
    settings.weather = meta.weather;
}

fn save_current_world(
    settings: &WorldSettings,
    active: Option<&ActiveWorld>,
    player_q: &Query<(&Transform, &Player)>,
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
    if let Ok((tf, player)) = player_q.get_single() {
        meta.player_pos = [tf.translation.x, tf.translation.y, tf.translation.z];
        meta.player_yaw = player.yaw;
        meta.player_pitch = player.pitch;
    }
    meta.last_played_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    settings::save_world(&meta);
    settings.save();
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    (n as u32) ^ ((n >> 32) as u32) ^ 0x9E3779B1
}
