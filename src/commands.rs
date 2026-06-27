//! Global Command Deck palette and keybind inspector.
//!
//! This is the first shared command layer: actions are described once
//! with label, access path, context and icon, then rendered as a searchable
//! command overlay. Later phases can attach executable callbacks and
//! conflict-aware remapping without scattering strings through UI code.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts};

use crate::animation::AnimationStudio;
use crate::builder::{BuildAction, BuilderState};
use crate::city::{CityState, CityTool, SnapMode};
use crate::editor::{EditorState, EditorTab, SimPause};
use crate::hud::DebugOverlay;
use crate::icons::{paint_icon, Icon};
use crate::menu::{GameState, PauseScreen};
use crate::player::{Player, PlayerProgressScratch};
use crate::settings::{self, ActiveWorld, CompanionDockPosition, HudProfile, WorldSettings};
use crate::theme::{command_frame, metric_pill, ThemeSettings, UiDensity, AMBER, CYAN, TEXT};
use crate::toolbelt::{ToolbeltState, ToolbeltTool};

pub struct CommandDeckPlugin;

impl Plugin for CommandDeckPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CommandPaletteState::default())
            .add_systems(
                Update,
                (
                    toggle_command_palette,
                    quick_save_hotkey,
                    draw_command_palette,
                    execute_command_action,
                )
                    .chain(),
            );
    }
}

#[derive(Clone, Copy)]
enum CommandAction {
    CloseDeck,
    ResumeGame,
    SaveGame,
    Screenshot,
    ToggleDebugOverlay,
    ToggleSimPause,
    OpenInventory,
    OpenEditor(EditorTab),
    SetBuildTool(ToolbeltTool),
    ArmWeapons,
    BuilderUndo,
    BuilderRedo,
    ToggleAnimationPicker,
    SetCityTool(CityTool),
    CycleCitySnap,
    ToggleAdminMode,
    ToggleInfiniteAmmo,
    SetHudProfile(HudProfile),
    CycleUiDensity,
    ToggleReduceMotion,
    ToggleAdvancedSettings,
    CycleCompanionDock,
}

#[derive(Resource)]
pub struct CommandPaletteState {
    pub open: bool,
    pub query: String,
    focus_query: bool,
    pending_action: Option<CommandAction>,
    status: Option<String>,
}

impl Default for CommandPaletteState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            focus_query: false,
            pending_action: None,
            status: None,
        }
    }
}

impl CommandPaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.focus_query = true;
        self.status = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focus_query = false;
        self.pending_action = None;
    }

    fn request(&mut self, action: CommandAction) {
        self.pending_action = Some(action);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandContext {
    Global,
    Menu,
    Gameplay,
    Combat,
    Builder,
    Editor,
    City,
    Animation,
    System,
}

impl CommandContext {
    fn label(self) -> &'static str {
        match self {
            CommandContext::Global => "GLOBAL",
            CommandContext::Menu => "MENU",
            CommandContext::Gameplay => "SPIEL",
            CommandContext::Combat => "KAMPF",
            CommandContext::Builder => "BAUEN",
            CommandContext::Editor => "EDITOR",
            CommandContext::City => "STADT",
            CommandContext::Animation => "ANIM",
            CommandContext::System => "SYSTEM",
        }
    }

    fn tint(self, theme: ThemeSettings) -> egui::Color32 {
        match self {
            CommandContext::Global => theme.color.primary(),
            CommandContext::Menu => CYAN,
            CommandContext::Gameplay => egui::Color32::from_rgb(0x9F, 0xE8, 0x7A),
            CommandContext::Combat => egui::Color32::from_rgb(0xFF, 0x70, 0x45),
            CommandContext::Builder => egui::Color32::from_rgb(0xFF, 0xC8, 0x4D),
            CommandContext::Editor => theme.color.dim(),
            CommandContext::City => egui::Color32::from_rgb(0x60, 0xD8, 0xFF),
            CommandContext::Animation => egui::Color32::from_rgb(0xD8, 0x85, 0xFF),
            CommandContext::System => egui::Color32::from_rgb(0xC8, 0xC8, 0xC8),
        }
    }
}

#[derive(Clone, Copy)]
struct CommandSpec {
    label: &'static str,
    detail: &'static str,
    key: &'static str,
    context: CommandContext,
    icon: Icon,
    essential: bool,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        label: "Command Deck oeffnen",
        detail: "Durchsuchbare Hilfe, Keybinds und Kontexte",
        key: "Command",
        context: CommandContext::Global,
        icon: Icon::Search,
        essential: true,
    },
    CommandSpec {
        label: "Pause / zurueck",
        detail: "Menue oeffnen, Overlay schliessen oder ins Spiel zurueck",
        key: "Esc",
        context: CommandContext::Global,
        icon: Icon::Pause,
        essential: true,
    },
    CommandSpec {
        label: "Schnellspeichern",
        detail: "Aktuelle Settings und aktive Welt sichern",
        key: "Save",
        context: CommandContext::Global,
        icon: Icon::Save,
        essential: true,
    },
    CommandSpec {
        label: "Screenshot",
        detail: "Bild des aktuellen Views speichern",
        key: "Screenshot",
        context: CommandContext::Global,
        icon: Icon::Eye,
        essential: false,
    },
    CommandSpec {
        label: "Debug Overlay umschalten",
        detail: "FPS, Position, Biome, Streaming und Key-Hinweise zeigen",
        key: "Overlay",
        context: CommandContext::System,
        icon: Icon::Eye,
        essential: true,
    },
    CommandSpec {
        label: "Editor oeffnen",
        detail: "Welt, Grafik, Wetter, Builder, City und System konfigurieren",
        key: "Deck / Pause",
        context: CommandContext::Editor,
        icon: Icon::System,
        essential: true,
    },
    CommandSpec {
        label: "Sketch Editor oeffnen",
        detail: "Mouse-first Toolbox nutzen: Pencil, Rectangle, Push/Pull, Room, Opening, Roads und Bot Area",
        key: "Toolbox",
        context: CommandContext::Builder,
        icon: Icon::ModeBuild,
        essential: true,
    },
    CommandSpec {
        label: "Play Mode aktivieren",
        detail: "Sketch Editor verlassen und Waffen/Spielsteuerung bewusst aktivieren",
        key: "PLAY",
        context: CommandContext::Builder,
        icon: Icon::ModeBuild,
        essential: true,
    },
    CommandSpec {
        label: "Workflow Rectangle",
        detail: "Rechtecke direkt in der Welt ziehen",
        key: "Toolbox",
        context: CommandContext::Builder,
        icon: Icon::Grid,
        essential: true,
    },
    CommandSpec {
        label: "Workflow Push Pull",
        detail: "Faces direkt herausziehen oder einschneiden",
        key: "Toolbox",
        context: CommandContext::Builder,
        icon: Icon::Move,
        essential: true,
    },
    CommandSpec {
        label: "Workflow Tower",
        detail: "Schneller Smart-Block fuer vertikale Formen",
        key: "Drawer",
        context: CommandContext::Builder,
        icon: Icon::City,
        essential: false,
    },
    CommandSpec {
        label: "Workflow Smart Builder",
        detail: "Startpunkt setzen, auf Block-Endpunkt ziehen, exakt bauen; RMB schneidet ohne Toolwechsel",
        key: "Drawer",
        context: CommandContext::Builder,
        icon: Icon::Brush,
        essential: true,
    },
    CommandSpec {
        label: "Workflow Brush Cut",
        detail: "Brush-Volumen direkt entfernen",
        key: "Drawer",
        context: CommandContext::Builder,
        icon: Icon::Eraser,
        essential: true,
    },
    CommandSpec {
        label: "Workflow Road",
        detail: "Road-Komponenten direkt aus dem Sketch Editor legen",
        key: "Toolbox",
        context: CommandContext::City,
        icon: Icon::Road,
        essential: false,
    },
    CommandSpec {
        label: "Workflow Bot Area",
        detail: "District-Zone direkt platzieren",
        key: "Toolbox",
        context: CommandContext::City,
        icon: Icon::District,
        essential: false,
    },
    CommandSpec {
        label: "Workflow Building Shell",
        detail: "Gebaeude-Corners direkt in der Welt setzen",
        key: "Toolbox",
        context: CommandContext::City,
        icon: Icon::City,
        essential: false,
    },
    CommandSpec {
        label: "Workflow Facade Stamp",
        detail: "Aktive Fassade direkt auf Waende stempeln",
        key: "Drawer",
        context: CommandContext::City,
        icon: Icon::Open,
        essential: false,
    },
    CommandSpec {
        label: "Workflow Animation Pick",
        detail: "Voxel-Auswahl fuer Animation Studio direkt aktivieren",
        key: "Drawer",
        context: CommandContext::Animation,
        icon: Icon::Animation,
        essential: false,
    },
    CommandSpec {
        label: "Simulation einfrieren",
        detail: "Zeit anhalten fuer Screenshots und Praezisionsbau",
        key: "Pause",
        context: CommandContext::Editor,
        icon: Icon::Time,
        essential: false,
    },
    CommandSpec {
        label: "Inventar oeffnen",
        detail: "Blockpalette und Tool-Auswahl",
        key: "E",
        context: CommandContext::Menu,
        icon: Icon::Cube,
        essential: true,
    },
    CommandSpec {
        label: "Maus fangen",
        detail: "Spielansicht aktivieren und Cursor sperren",
        key: "LMB",
        context: CommandContext::Gameplay,
        icon: Icon::ModeNavigate,
        essential: true,
    },
    CommandSpec {
        label: "Bewegen",
        detail: "Vorwaerts, seitwaerts und rueckwaerts laufen",
        key: "WASD",
        context: CommandContext::Gameplay,
        icon: Icon::Move,
        essential: true,
    },
    CommandSpec {
        label: "Springen / Flugmodus",
        detail: "Springen, Doppeltipp Space schaltet Flugmodus",
        key: "Space",
        context: CommandContext::Gameplay,
        icon: Icon::Player,
        essential: true,
    },
    CommandSpec {
        label: "Sprint",
        detail: "Schneller laufen mit Ctrl oder W-Doppeltipp",
        key: "Ctrl / W,W",
        context: CommandContext::Gameplay,
        icon: Icon::Teleport,
        essential: true,
    },
    CommandSpec {
        label: "Waffe wechseln",
        detail: "Nur wenn Waffen bewusst scharf sind",
        key: "1-9",
        context: CommandContext::Combat,
        icon: Icon::ModeBuild,
        essential: true,
    },
    CommandSpec {
        label: "Waffen scharf schalten",
        detail: "Explizit in Combat wechseln; Sketch Editor holstert wieder",
        key: "PLAY",
        context: CommandContext::Combat,
        icon: Icon::ModeBuild,
        essential: true,
    },
    CommandSpec {
        label: "Feuern",
        detail: "Nur im Combat-Modus; Sketch Editor nutzt LMB zum Editieren",
        key: "LMB",
        context: CommandContext::Combat,
        icon: Icon::LightBulb,
        essential: true,
    },
    CommandSpec {
        label: "Zielen / Scope",
        detail: "Aim-down-sight; Sniper zoomt mit Mausrad",
        key: "RMB / Wheel",
        context: CommandContext::Combat,
        icon: Icon::Eye,
        essential: true,
    },
    CommandSpec {
        label: "Reload",
        detail: "Nachladen im Survival-/Ammo-Modus",
        key: "R",
        context: CommandContext::Combat,
        icon: Icon::Redo,
        essential: false,
    },
    CommandSpec {
        label: "Builder Aktion rueckgaengig",
        detail: "Letzte Builder-Aenderung zuruecknehmen",
        key: "Ctrl+Z",
        context: CommandContext::Builder,
        icon: Icon::Undo,
        essential: true,
    },
    CommandSpec {
        label: "Builder Aktion wiederholen",
        detail: "Rueckgaengige Builder-Aenderung erneut anwenden",
        key: "Ctrl+Y",
        context: CommandContext::Builder,
        icon: Icon::Redo,
        essential: false,
    },
    CommandSpec {
        label: "Box-Auswahl starten",
        detail: "Zwei Ecken fuer Copy, Cut und Paste markieren",
        key: "B",
        context: CommandContext::Builder,
        icon: Icon::Grid,
        essential: true,
    },
    CommandSpec {
        label: "Auswahl kopieren",
        detail: "Markierte Box in die Clipboard-Palette uebernehmen",
        key: "C",
        context: CommandContext::Builder,
        icon: Icon::Copy,
        essential: true,
    },
    CommandSpec {
        label: "Auswahl ausschneiden",
        detail: "Kopieren und markierte Voxels aus der Welt loeschen",
        key: "Ctrl+X",
        context: CommandContext::Builder,
        icon: Icon::Delete,
        essential: false,
    },
    CommandSpec {
        label: "Paste-Ghost oeffnen",
        detail: "Clipboard als Vorschau im Raum platzieren",
        key: "V",
        context: CommandContext::Builder,
        icon: Icon::Paste,
        essential: true,
    },
    CommandSpec {
        label: "Paste-Ghost drehen",
        detail: "Clipboard um 90 Grad auf Y drehen",
        key: "Wheel / R",
        context: CommandContext::Builder,
        icon: Icon::RotateY90,
        essential: false,
    },
    CommandSpec {
        label: "Spiegelachsen toggeln",
        detail: "X, Y oder Z Mirror fuer Builder-Edits",
        key: "M / Shift+M / Alt+M",
        context: CommandContext::Builder,
        icon: Icon::FlipX,
        essential: false,
    },
    CommandSpec {
        label: "Animation Picker",
        detail: "Voxel-Auswahl fuer Animation Studio aktivieren",
        key: "Drawer",
        context: CommandContext::Animation,
        icon: Icon::Animation,
        essential: false,
    },
    CommandSpec {
        label: "City Strassen-Tool",
        detail: "Road-Grid Tool im STADT Tab aktivieren",
        key: "N",
        context: CommandContext::City,
        icon: Icon::Road,
        essential: false,
    },
    CommandSpec {
        label: "City Bezirks-Tool",
        detail: "District Disc im STADT Tab platzieren",
        key: "T",
        context: CommandContext::City,
        icon: Icon::District,
        essential: false,
    },
    CommandSpec {
        label: "City Gebaeude-Tool",
        detail: "Prozedurale Gebaeude-Footprints setzen",
        key: "U",
        context: CommandContext::City,
        icon: Icon::City,
        essential: false,
    },
    CommandSpec {
        label: "City Fassaden-Tool",
        detail: "Dekor-/Facade-Prefab an den Cursor stempeln",
        key: "F",
        context: CommandContext::City,
        icon: Icon::Open,
        essential: false,
    },
    CommandSpec {
        label: "City Snap wechseln",
        detail: "Grid-/Road-Snap fuer Stadtwerkzeuge umschalten",
        key: ".",
        context: CommandContext::City,
        icon: Icon::Snap,
        essential: false,
    },
    CommandSpec {
        label: "HUD Guided",
        detail: "Objective, compass, vitals and map visible",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Hud,
        essential: true,
    },
    CommandSpec {
        label: "HUD Focused",
        detail: "Minimal world-first gameplay HUD",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Eye,
        essential: false,
    },
    CommandSpec {
        label: "HUD Creator",
        detail: "Build, resource and companion status prioritized",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Builder,
        essential: false,
    },
    CommandSpec {
        label: "UI Dichte wechseln",
        detail: "Compact, comfortable and spacious spacing",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Density,
        essential: false,
    },
    CommandSpec {
        label: "Reduce Motion",
        detail: "Animated UI motion reduzieren",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Accessibility,
        essential: false,
    },
    CommandSpec {
        label: "Advanced Settings",
        detail: "Raw engine tuning ein- oder ausblenden",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Drawer,
        essential: false,
    },
    CommandSpec {
        label: "Companion Dock bewegen",
        detail: "Dock links, rechts oder unten platzieren",
        key: "Deck",
        context: CommandContext::System,
        icon: Icon::Layout,
        essential: false,
    },
    CommandSpec {
        label: "Toolbench HUD Settings",
        detail: "Open Toolbench readability controls",
        key: "Deck",
        context: CommandContext::Editor,
        icon: Icon::Layout,
        essential: false,
    },
    CommandSpec {
        label: "Admin Modus",
        detail: "Cheat-Gate fuer sensible Debug-Schalter",
        key: "Ctrl+Shift+A",
        context: CommandContext::System,
        icon: Icon::Gear,
        essential: false,
    },
    CommandSpec {
        label: "Infinite Ammo",
        detail: "Nur wenn Admin Modus aktiv ist",
        key: "Ctrl+I",
        context: CommandContext::System,
        icon: Icon::Loop,
        essential: false,
    },
];

fn toggle_command_palette(
    keys: Res<ButtonInput<KeyCode>>,
    game_state: Res<State<GameState>>,
    mut next_state: ResMut<NextState<GameState>>,
    mut palette: ResMut<CommandPaletteState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if command_palette_requested(&keys) {
        if palette.open {
            palette.close();
        } else {
            palette.open();
            if *game_state.get() == GameState::InGame {
                next_state.set(GameState::Paused);
            }
        }
    }

    if palette.open {
        if let Ok(mut window) = windows.get_single_mut() {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
    }
}

fn command_palette_requested(keys: &ButtonInput<KeyCode>) -> bool {
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    ctrl && keys.just_pressed(KeyCode::KeyP)
}

fn draw_command_palette(
    mut contexts: EguiContexts,
    settings: Res<WorldSettings>,
    game_state: Res<State<GameState>>,
    mut palette: ResMut<CommandPaletteState>,
) {
    if !palette.open {
        return;
    }

    let ctx = contexts.ctx_mut();
    let theme = settings.theme;
    let screen = ctx.screen_rect();
    let time = ctx.input(|i| i.time) as f32;
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("command_palette_dim"),
    ));
    painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(190));
    draw_data_ribs(ctx, theme, time);

    let width = screen.width().clamp(360.0, 860.0) - 32.0;
    let height = screen.height().clamp(420.0, 680.0) - 34.0;
    let pos = egui::pos2(
        screen.center().x - width * 0.5,
        screen.center().y - height * 0.5,
    );

    egui::Window::new("command_deck_palette")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(width, height))
        .frame(command_frame(theme))
        .show(ctx, |ui| {
            draw_palette_header(ui, theme, game_state.get());
            ui.add_space(10.0);

            let search = crate::ui_kit::search_box(
                ui,
                &mut palette.query,
                "Command, Taste oder Kontext suchen...",
                theme,
            );
            if palette.focus_query {
                search.request_focus();
                palette.focus_query = false;
            }

            ui.add_space(10.0);
            let query = palette.query.trim().to_ascii_lowercase();
            let matches: Vec<&CommandSpec> = COMMANDS
                .iter()
                .filter(|command| command_matches(command, &query))
                .collect();

            if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                if let Some(action) = matches.iter().find_map(|command| command_action(command)) {
                    palette.request(action);
                }
            }

            ui.horizontal(|ui| {
                metric_pill(ui, theme, "TREFFER", &matches.len().to_string());
                metric_pill(
                    ui,
                    theme,
                    "ESSENTIAL",
                    &essential_count(&matches).to_string(),
                );
                metric_pill(ui, theme, "KONTEXT", game_state_label(game_state.get()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::ui_kit::danger_action(ui, Icon::Close, "Close", theme).clicked() {
                        palette.close();
                    }
                });
            });

            if let Some(status) = palette.status.as_ref() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(status).monospace().small().color(AMBER));
            }

            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height((height - 170.0).max(160.0))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if matches.is_empty() {
                        empty_state(ui, theme);
                    } else {
                        for command in matches {
                            if let Some(action) = command_row(ui, theme, command) {
                                palette.request(action);
                            }
                            ui.add_space(4.0);
                        }
                    }
                });
        });
}

fn draw_palette_header(ui: &mut egui::Ui, theme: ThemeSettings, state: &GameState) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(42.0, 42.0), egui::Sense::hover());
        paint_icon(
            ui.painter(),
            rect.shrink(4.0),
            Icon::Search,
            theme.color.primary(),
        );
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("COMMAND DECK")
                    .heading()
                    .strong()
                    .monospace()
                    .color(theme.color.primary()),
            );
            ui.label(
                egui::RichText::new("Search commands  |  Esc schliesst dieses Deck")
                    .monospace()
                    .small()
                    .color(theme.color.dim()),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(game_state_label(state))
                    .monospace()
                    .strong()
                    .color(CYAN),
            );
        });
    });
}

fn command_row(
    ui: &mut egui::Ui,
    theme: ThemeSettings,
    command: &CommandSpec,
) -> Option<CommandAction> {
    let accent = command.context.tint(theme);
    let fill = if command.essential {
        egui::Color32::from_rgba_premultiplied(accent.r() / 7, accent.g() / 7, accent.b() / 7, 190)
    } else {
        egui::Color32::from_rgba_premultiplied(0, 0, 0, 150)
    };
    let action = command_action(command);
    let mut requested = None;

    egui::Frame::none()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, accent.linear_multiply(0.70)))
        .rounding(egui::Rounding::same(5.0))
        .inner_margin(egui::Margin::symmetric(9.0, 7.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                paint_icon(ui.painter(), rect.shrink(3.0), command.icon, accent);

                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(command.label)
                            .monospace()
                            .strong()
                            .color(TEXT),
                    );
                    ui.label(
                        egui::RichText::new(command.detail)
                            .monospace()
                            .small()
                            .color(theme.color.dim()),
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(action) = action {
                        if action_button(ui, theme, accent).clicked() {
                            requested = Some(action);
                        }
                    }
                    key_chip(ui, theme, command.key);
                    context_chip(ui, theme, command.context);
                });
            });
        });
    requested
}

fn action_button(ui: &mut egui::Ui, theme: ThemeSettings, accent: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(84.0, 26.0), egui::Sense::click());
    let fill = if response.hovered() {
        accent.linear_multiply(1.12)
    } else {
        accent
    };
    let text = theme.text_on(fill);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(5.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(5.0),
        egui::Stroke::new(1.0, theme.semantic().stroke),
    );
    paint_icon(
        &painter,
        egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 6.0), egui::vec2(14.0, 14.0)),
        Icon::Play,
        text,
    );
    painter.text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "RUN",
        egui::FontId::monospace(11.0),
        text,
    );
    response.on_hover_text("Run command")
}

fn context_chip(ui: &mut egui::Ui, theme: ThemeSettings, context: CommandContext) {
    let tint = context.tint(theme);
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(
            tint.r() / 9,
            tint.g() / 9,
            tint.b() / 9,
            160,
        ))
        .stroke(egui::Stroke::new(1.0, tint.linear_multiply(0.75)))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(7.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(context.label())
                    .monospace()
                    .small()
                    .strong()
                    .color(tint),
            );
        });
}

fn key_chip(ui: &mut egui::Ui, theme: ThemeSettings, key: &str) {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_premultiplied(0, 0, 0, 180))
        .stroke(egui::Stroke::new(1.0, theme.color.primary()))
        .rounding(egui::Rounding::same(4.0))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(key)
                    .monospace()
                    .small()
                    .strong()
                    .color(theme.color.primary()),
            );
        });
}

fn empty_state(ui: &mut egui::Ui, theme: ThemeSettings) {
    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.label(
            egui::RichText::new("KEIN COMMAND GEFUNDEN")
                .monospace()
                .strong()
                .color(theme.color.primary()),
        );
        ui.label(
            egui::RichText::new("Suche nach Taste, Kontext oder Aktion kuerzen.")
                .monospace()
                .small()
                .color(theme.color.dim()),
        );
    });
}

fn draw_data_ribs(ctx: &egui::Context, theme: ThemeSettings, time: f32) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("command_palette_ribs"),
    ));
    let primary = theme.color.primary();
    let alpha = (34.0 + ((time * 3.0).sin() * 0.5 + 0.5) * 24.0) as u8;
    let color = egui::Color32::from_rgba_unmultiplied(primary.r(), primary.g(), primary.b(), alpha);

    let mut x = screen.left() + 18.0;
    while x < screen.right() {
        painter.line_segment(
            [
                egui::pos2(x, screen.top()),
                egui::pos2(x + 120.0, screen.bottom()),
            ],
            egui::Stroke::new(1.0, color),
        );
        x += 92.0;
    }
}

fn command_action(command: &CommandSpec) -> Option<CommandAction> {
    match command.label {
        "Command Deck oeffnen" => Some(CommandAction::CloseDeck),
        "Pause / zurueck" | "Maus fangen" => Some(CommandAction::ResumeGame),
        "Schnellspeichern" => Some(CommandAction::SaveGame),
        "Screenshot" => Some(CommandAction::Screenshot),
        "Debug Overlay umschalten" => Some(CommandAction::ToggleDebugOverlay),
        "Editor oeffnen" => Some(CommandAction::OpenEditor(EditorTab::World)),
        "Simulation einfrieren" => Some(CommandAction::ToggleSimPause),
        "Inventar oeffnen" => Some(CommandAction::OpenInventory),
        "Waffen scharf schalten" => Some(CommandAction::ArmWeapons),
        "Sketch Editor oeffnen" => Some(CommandAction::SetBuildTool(ToolbeltTool::DrawRect)),
        "Workflow Rectangle" => Some(CommandAction::SetBuildTool(ToolbeltTool::DrawRect)),
        "Workflow Push Pull" => Some(CommandAction::SetBuildTool(ToolbeltTool::Sculpt)),
        "Workflow Tower" => Some(CommandAction::SetBuildTool(ToolbeltTool::SmartTower)),
        "Workflow Smart Builder" | "Tool 4 Power Brush" | "Tool 4 Brush Place" => {
            Some(CommandAction::SetBuildTool(ToolbeltTool::BrushPlace))
        }
        "Workflow Brush Cut" => Some(CommandAction::SetBuildTool(ToolbeltTool::BrushCut)),
        "Workflow Road" => Some(CommandAction::SetBuildTool(ToolbeltTool::CityRoad)),
        "Workflow Bot Area" => Some(CommandAction::SetBuildTool(ToolbeltTool::CityDistrict)),
        "Workflow Building Shell" => Some(CommandAction::SetBuildTool(ToolbeltTool::CityBuilding)),
        "Workflow Facade Stamp" => Some(CommandAction::SetBuildTool(ToolbeltTool::CityFacade)),
        "Workflow Animation Pick" => Some(CommandAction::SetBuildTool(ToolbeltTool::AnimationPick)),
        "Builder Aktion rueckgaengig" => Some(CommandAction::BuilderUndo),
        "Builder Aktion wiederholen" => Some(CommandAction::BuilderRedo),
        "Box-Auswahl starten"
        | "Auswahl kopieren"
        | "Auswahl ausschneiden"
        | "Paste-Ghost oeffnen"
        | "Paste-Ghost drehen"
        | "Spiegelachsen toggeln" => Some(CommandAction::OpenEditor(EditorTab::Builder)),
        "Animation Picker" => Some(CommandAction::ToggleAnimationPicker),
        "City Strassen-Tool" => Some(CommandAction::SetCityTool(CityTool::Road)),
        "City Bezirks-Tool" => Some(CommandAction::SetCityTool(CityTool::District)),
        "City Gebaeude-Tool" => Some(CommandAction::SetCityTool(CityTool::Building)),
        "City Fassaden-Tool" => Some(CommandAction::SetCityTool(CityTool::Facade)),
        "City Snap wechseln" => Some(CommandAction::CycleCitySnap),
        "HUD Guided" => Some(CommandAction::SetHudProfile(HudProfile::Guided)),
        "HUD Focused" => Some(CommandAction::SetHudProfile(HudProfile::Focused)),
        "HUD Creator" => Some(CommandAction::SetHudProfile(HudProfile::Creator)),
        "UI Dichte wechseln" => Some(CommandAction::CycleUiDensity),
        "Reduce Motion" => Some(CommandAction::ToggleReduceMotion),
        "Advanced Settings" => Some(CommandAction::ToggleAdvancedSettings),
        "Companion Dock bewegen" => Some(CommandAction::CycleCompanionDock),
        "Toolbench HUD Settings" => Some(CommandAction::OpenEditor(EditorTab::System)),
        "Admin Modus" => Some(CommandAction::ToggleAdminMode),
        "Infinite Ammo" => Some(CommandAction::ToggleInfiniteAmmo),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_command_action(
    mut palette: ResMut<CommandPaletteState>,
    state: Res<State<GameState>>,
    mut next_state: ResMut<NextState<GameState>>,
    mut pause_screen: ResMut<PauseScreen>,
    mut editor: ResMut<EditorState>,
    mut sim_pause: ResMut<SimPause>,
    mut overlay: ResMut<DebugOverlay>,
    mut settings: ResMut<WorldSettings>,
    active: Option<Res<ActiveWorld>>,
    scratch: Res<PlayerProgressScratch>,
    player_q: Query<(&Transform, &Player)>,
    mut builder: ResMut<BuilderState>,
    mut studio: ResMut<AnimationStudio>,
    mut city: ResMut<CityState>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut mode: ResMut<crate::mode::ModeContext>,
) {
    let Some(action) = palette.pending_action.take() else {
        return;
    };

    let blocked = match action {
        CommandAction::CloseDeck => {
            palette.close();
            return;
        }
        CommandAction::ResumeGame => {
            if *state.get() == GameState::MainMenu {
                Some("Erst eine Welt starten, dann kann das Deck ins Spiel zurueck.".into())
            } else {
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                None
            }
        }
        CommandAction::SaveGame => {
            save_current_world(&settings, active.as_deref(), &scratch, &player_q);
            settings.save();
            info!("Command Deck: save requested");
            None
        }
        CommandAction::Screenshot => {
            editor.screenshot_requested = true;
            info!("Command Deck: screenshot requested");
            None
        }
        CommandAction::ToggleDebugOverlay => {
            overlay.visible = !overlay.visible;
            info!("Command Deck: debug overlay = {}", overlay.visible);
            None
        }
        CommandAction::ToggleSimPause => {
            sim_pause.paused = !sim_pause.paused;
            info!("Command Deck: sim pause = {}", sim_pause.paused);
            None
        }
        CommandAction::OpenInventory => {
            if *state.get() == GameState::MainMenu {
                Some("Inventar ist erst in einer geladenen Welt verfuegbar.".into())
            } else {
                editor.open = false;
                *pause_screen = PauseScreen::Inventory;
                next_state.set(GameState::Paused);
                None
            }
        }
        CommandAction::OpenEditor(tab) => {
            open_editor_tab(&state, &mut next_state, &mut pause_screen, &mut editor, tab);
            None
        }
        CommandAction::SetBuildTool(tool) => {
            if *state.get() == GameState::MainMenu {
                Some("Sketch Editor braucht eine geladene Welt.".into())
            } else {
                toolbelt.tool = tool;
                let status = format!("Sketch Editor: {}. {}", tool.label(), tool.hint());
                mode.set(crate::mode::ActiveMode::BuildLive { tool }, status.clone());
                toolbelt.status = status;
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                None
            }
        }
        CommandAction::ArmWeapons => {
            if *state.get() == GameState::MainMenu {
                Some("Waffen brauchen eine geladene Welt.".into())
            } else {
                let status = "Weapons armed explicitly. Use the build toggle to holster again.";
                mode.set(crate::mode::ActiveMode::Combat, status);
                toolbelt.status = status.into();
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                None
            }
        }
        CommandAction::BuilderUndo => {
            if *state.get() == GameState::MainMenu {
                Some("Builder-History ist erst in einer Welt aktiv.".into())
            } else {
                builder.pending.push(BuildAction::Undo);
                None
            }
        }
        CommandAction::BuilderRedo => {
            if *state.get() == GameState::MainMenu {
                Some("Builder-History ist erst in einer Welt aktiv.".into())
            } else {
                builder.pending.push(BuildAction::Redo);
                None
            }
        }
        CommandAction::ToggleAnimationPicker => {
            if *state.get() == GameState::MainMenu {
                Some("Animation Picker braucht eine geladene Welt.".into())
            } else {
                studio.picking = !studio.picking;
                toolbelt.tool = ToolbeltTool::AnimationPick;
                if studio.picking {
                    mode.set(
                        crate::mode::ActiveMode::BuildLive {
                            tool: ToolbeltTool::AnimationPick,
                        },
                        "Sketch Editor: Animation Picker. LMB/RMB pick voxels for animation authoring.",
                    );
                } else {
                    mode.set(crate::mode::ActiveMode::Combat, "Animation Picker off.");
                }
                toolbelt.status = mode.status.clone();
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                info!("Command Deck: animation picker = {}", studio.picking);
                None
            }
        }
        CommandAction::SetCityTool(tool) => {
            if *state.get() == GameState::MainMenu {
                Some("City-Werkzeuge brauchen eine geladene Welt.".into())
            } else {
                city.tool = if city.tool == tool {
                    CityTool::None
                } else {
                    tool
                };
                city.pending_road_a = None;
                city.pending_building_a = None;
                city.status = format!("Live-Werkzeug: {}", city_tool_label(city.tool));
                toolbelt.tool = toolbelt_tool_for_city(city.tool);
                if city.tool != CityTool::None {
                    mode.set(
                        crate::mode::ActiveMode::BuildLive {
                            tool: toolbelt.tool,
                        },
                        format!(
                            "Sketch Editor: {}. {}",
                            toolbelt.tool.label(),
                            toolbelt.tool.hint()
                        ),
                    );
                } else {
                    mode.set(crate::mode::ActiveMode::Combat, "City tool off.");
                }
                toolbelt.status = mode.status.clone();
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                None
            }
        }
        CommandAction::CycleCitySnap => {
            if *state.get() == GameState::MainMenu {
                Some("City-Snap braucht eine geladene Welt.".into())
            } else {
                city.snap = cycle_city_snap(city.snap);
                city.status = format!("Snap: {}", snap_mode_label(city.snap));
                editor.open = false;
                *pause_screen = PauseScreen::Menu;
                next_state.set(GameState::InGame);
                None
            }
        }
        CommandAction::SetHudProfile(profile) => {
            settings.hud_profile = profile;
            info!("Command Deck: HUD profile = {}", profile.label());
            None
        }
        CommandAction::CycleUiDensity => {
            settings.theme.density = match settings.theme.density {
                UiDensity::Compact => UiDensity::Comfortable,
                UiDensity::Comfortable => UiDensity::Spacious,
                UiDensity::Spacious => UiDensity::Compact,
            };
            info!("Command Deck: UI density = {:?}", settings.theme.density);
            None
        }
        CommandAction::ToggleReduceMotion => {
            settings.reduce_motion = !settings.reduce_motion;
            info!("Command Deck: reduce motion = {}", settings.reduce_motion);
            None
        }
        CommandAction::ToggleAdvancedSettings => {
            settings.show_advanced_settings = !settings.show_advanced_settings;
            info!(
                "Command Deck: advanced settings = {}",
                settings.show_advanced_settings
            );
            None
        }
        CommandAction::CycleCompanionDock => {
            settings.companion_ui.dock_position = match settings.companion_ui.dock_position {
                CompanionDockPosition::Left => CompanionDockPosition::Right,
                CompanionDockPosition::Right => CompanionDockPosition::Bottom,
                CompanionDockPosition::Bottom => CompanionDockPosition::Left,
            };
            info!(
                "Command Deck: companion dock = {:?}",
                settings.companion_ui.dock_position
            );
            None
        }
        CommandAction::ToggleAdminMode => {
            settings.cheats.admin_mode = !settings.cheats.admin_mode;
            info!("Command Deck: admin mode = {}", settings.cheats.admin_mode);
            None
        }
        CommandAction::ToggleInfiniteAmmo => {
            if !settings.cheats.admin_mode {
                Some("Admin-Modus zuerst aktivieren, dann Infinite Ammo togglen.".into())
            } else {
                settings.cheats.infinite_ammo = !settings.cheats.infinite_ammo;
                info!(
                    "Command Deck: infinite ammo = {}",
                    settings.cheats.infinite_ammo
                );
                None
            }
        }
    };

    if let Some(status) = blocked {
        palette.status = Some(status);
    } else {
        palette.close();
    }
}

fn quick_save_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<WorldSettings>,
    active: Option<Res<ActiveWorld>>,
    scratch: Res<PlayerProgressScratch>,
    player_q: Query<(&Transform, &Player)>,
) {
    if !keys.just_pressed(KeyCode::F5) {
        return;
    }
    save_current_world(&settings, active.as_deref(), &scratch, &player_q);
    settings.save();
    info!("Quick-save: world pose and settings saved");
}

fn open_editor_tab(
    state: &State<GameState>,
    next_state: &mut NextState<GameState>,
    pause_screen: &mut PauseScreen,
    editor: &mut EditorState,
    tab: EditorTab,
) {
    editor.open = true;
    editor.tab = tab;
    *pause_screen = PauseScreen::Menu;
    if *state.get() == GameState::InGame {
        next_state.set(GameState::Paused);
    }
}

fn save_current_world(
    settings: &WorldSettings,
    active: Option<&ActiveWorld>,
    scratch: &PlayerProgressScratch,
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
        settings::save_player_pose_checkpoint(
            &meta,
            settings,
            [tf.translation.x, tf.translation.y, tf.translation.z],
            player.yaw,
            player.pitch,
            scratch.mining,
            scratch.suit,
        );
        return;
    }
    settings::save_world(&meta);
}

fn city_tool_label(tool: CityTool) -> &'static str {
    match tool {
        CityTool::None => "AUS",
        CityTool::Road => "STRASSE",
        CityTool::District => "BEZIRK",
        CityTool::Building => "GEBAEUDE",
        CityTool::Facade => "FASSADE",
    }
}

fn toolbelt_tool_for_city(tool: CityTool) -> ToolbeltTool {
    match tool {
        CityTool::Road => ToolbeltTool::CityRoad,
        CityTool::District => ToolbeltTool::CityDistrict,
        CityTool::Building => ToolbeltTool::CityBuilding,
        CityTool::Facade => ToolbeltTool::CityFacade,
        CityTool::None => ToolbeltTool::Navigate,
    }
}

fn cycle_city_snap(mode: SnapMode) -> SnapMode {
    match mode {
        SnapMode::Off => SnapMode::Grid1,
        SnapMode::Grid1 => SnapMode::Grid4,
        SnapMode::Grid4 => SnapMode::Grid16,
        SnapMode::Grid16 => SnapMode::Road,
        SnapMode::Road => SnapMode::Off,
    }
}

fn snap_mode_label(mode: SnapMode) -> &'static str {
    match mode {
        SnapMode::Off => "AUS",
        SnapMode::Grid1 => "Grid 1",
        SnapMode::Grid4 => "Grid 4",
        SnapMode::Grid16 => "Grid 16",
        SnapMode::Road => "Strassen",
    }
}

fn command_matches(command: &CommandSpec, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    command.label.to_ascii_lowercase().contains(query)
        || command.detail.to_ascii_lowercase().contains(query)
        || command.key.to_ascii_lowercase().contains(query)
        || command.context.label().to_ascii_lowercase().contains(query)
}

fn essential_count(commands: &[&CommandSpec]) -> usize {
    commands.iter().filter(|command| command.essential).count()
}

fn game_state_label(state: &GameState) -> &'static str {
    match state {
        GameState::MainMenu => "MAIN MENU",
        GameState::InGame => "IN GAME",
        GameState::Paused => "PAUSED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f1_no_longer_opens_command_palette() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::F1);

        assert!(!command_palette_requested(&keys));
    }

    #[test]
    fn ctrl_p_remains_hidden_command_palette_access() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        keys.press(KeyCode::KeyP);

        assert!(command_palette_requested(&keys));
    }

    #[test]
    fn command_deck_does_not_advertise_function_keys() {
        for command in COMMANDS {
            assert!(
                !["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",]
                    .iter()
                    .any(|token| command.key.contains(token)),
                "command still advertises a function-key workflow: {} -> {}",
                command.label,
                command.key
            );
        }
    }
}
