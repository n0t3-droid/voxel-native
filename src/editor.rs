//! Futuristic in-game editor (F3 to toggle). Tabbed cyberpunk-styled
//! egui panel with smooth open/close animation, ESC + click-outside close.
//!
//! Sections: WELT / GRAFIK / WETTER / ZEIT / SPIELER / SYSTEM.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPlugin};

use crate::animation::{AnimationStudio, Interp, KeyFrame};
use crate::blocks::{block_label, block_palette_catalog, BlockPaletteEntry, BlockType};
use crate::builder::{BuildAction, BuilderClipboard, BuilderHistory, BuilderState};
use crate::icons::Icon;
use crate::neurocore::RuntimeProfile;
use crate::player::Player;
use crate::settings::{
    GraphicsMode, HudProfile, SceneryQuality, TimeMode, WeatherPreset, WorldModeCard,
    WorldSettings, SAFE_MAX_CHUNKS_PER_FRAME, SAFE_MAX_IN_FLIGHT_MESHES,
    SAFE_MAX_IN_FLIGHT_TERRAIN, SAFE_MAX_MESHES_PER_FRAME, SAFE_MAX_MESH_APPLIES_PER_FRAME,
    SAFE_MAX_RENDER_DISTANCE, SAFE_MAX_VERTICAL_CHUNKS, SAFE_MIN_CHUNKS_PER_FRAME,
    SAFE_MIN_IN_FLIGHT_MESHES, SAFE_MIN_IN_FLIGHT_TERRAIN, SAFE_MIN_MESHES_PER_FRAME,
    SAFE_MIN_MESH_APPLIES_PER_FRAME, SAFE_MIN_RENDER_DISTANCE, SAFE_MIN_VERTICAL_CHUNKS,
};
use crate::textures::{bake_all_block_swatches, BlockSwatch, TEX_DIR};
use crate::theme::{
    apply_hacker_theme, draw_banner, draw_status_bar, draw_theme_preview_card, paint_scanlines,
    section_box, selected_theme_preset, set_reduced_motion, status_line, term_button, ThemeColor,
    ThemeStyle, UiDensity, ALERT, AMBER, THEME_PRESETS,
};
use crate::world::{ChunkStreamer, StreamingGovernor, VoxelWorld};

#[derive(Default, Debug, PartialEq, Eq, Clone, Copy)]
pub enum EditorTab {
    #[default]
    World,
    Graphics,
    Weather,
    Time,
    Player,
    Textures,
    Builder,
    Animation,
    City,
    Bots,
    System,
}

impl EditorTab {
    fn label(self) -> &'static str {
        match self {
            EditorTab::World => "WELT",
            EditorTab::Graphics => "GRAFIK",
            EditorTab::Weather => "WETTER",
            EditorTab::Time => "ZEIT",
            EditorTab::Player => "SPIELER",
            EditorTab::Textures => "TEXTUREN",
            EditorTab::Builder => "BAUEN",
            EditorTab::Animation => "ANIM",
            EditorTab::City => "STADT",
            EditorTab::Bots => "COMP",
            EditorTab::System => "SYSTEM",
        }
    }
    fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            EditorTab::World => Icon::World,
            EditorTab::Graphics => Icon::Graphics,
            EditorTab::Weather => Icon::Weather,
            EditorTab::Time => Icon::Time,
            EditorTab::Player => Icon::Player,
            EditorTab::Textures => Icon::Textures,
            EditorTab::Builder => Icon::Builder,
            EditorTab::Animation => Icon::Animation,
            EditorTab::City => Icon::City,
            EditorTab::Bots => Icon::Wand,
            EditorTab::System => Icon::System,
        }
    }
    fn all() -> [EditorTab; 11] {
        [
            EditorTab::World,
            EditorTab::Graphics,
            EditorTab::Weather,
            EditorTab::Time,
            EditorTab::Player,
            EditorTab::Textures,
            EditorTab::Builder,
            EditorTab::Animation,
            EditorTab::City,
            EditorTab::Bots,
            EditorTab::System,
        ]
    }
}

#[derive(Resource, Default)]
pub struct EditorState {
    pub open: bool,
    pub pending_seed: Option<u32>,
    pub regen_requested: bool,
    pub tab: EditorTab,
    pub screenshot_requested: bool,
    /// Staging text field for "save current position as bookmark" in WELT.
    pub new_bookmark_name: String,
    /// Staging text field for "save clipboard under this name" in BAUEN.
    pub prefab_save_name: String,
    /// When `true`, the legacy "BAUEN" precision-cuboid builder is exposed
    /// (its tab in the F3 panel is visible and its **B / C / V / X** /
    /// mirror hotkeys are active). Default `false` since the new
    /// SketchUp-style direct-manipulation system supersedes it.
    /// Toggle lives in the SYSTEM tab.
    pub show_classic_builder: bool,
}

/// Global simulation-pause flag. When `paused = true` the day/night
/// cycle is frozen (see `daynight::advance_time`) and the editor
/// status bar shows `[MODE: EDIT]` instead of `[MODE: PLAY]`.
/// Toggled with **F6** any time, even with the editor closed.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct SimPause {
    pub paused: bool,
}

/// Rolling FPS history used for the GRAFIK tab sparkline. 120
/// samples ≈ 2 s at 60 fps — enough to spot hitches visually.
#[derive(Resource)]
pub struct FpsHistory {
    pub samples: std::collections::VecDeque<f32>,
}

impl Default for FpsHistory {
    fn default() -> Self {
        Self {
            samples: std::collections::VecDeque::with_capacity(120),
        }
    }
}

/// Baked at startup, shown as a tile grid in the TEXTUREN tab.
#[derive(Resource, Default)]
pub struct TextureLibrary {
    pub swatches: Vec<BlockSwatch>,
    /// egui texture handles, one per swatch, uploaded lazily so we don't
    /// touch the GPU on the first frame the editor is open.
    pub handles: Vec<Option<egui::TextureHandle>>,
    pub last_status: String,
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin)
            .insert_resource(EditorState::default())
            .insert_resource(TextureLibrary::default())
            .insert_resource(SimPause::default())
            .insert_resource(FpsHistory::default())
            .add_systems(Startup, style_egui)
            .add_systems(
                Update,
                (
                    restyle_on_change,
                    toggle_editor,
                    toggle_sim_pause,
                    sample_fps_history,
                    draw_editor,
                    handle_regen,
                    handle_screenshot,
                )
                    .chain(),
            );
    }
}

#[derive(SystemParam)]
struct EditorWorldTools<'w> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    city: ResMut<'w, crate::city::CityState>,
    bots: ResMut<'w, crate::bots::FriendlyWorldBrain>,
    selection: Res<'w, crate::selection::SelectionState>,
}

/// Apply the hacker terminal theme. Re-runs whenever the user picks a
/// new colour variant in the SYSTEM tab so the change is instant.
fn style_egui(mut contexts: EguiContexts, settings: Res<WorldSettings>) {
    let ctx = contexts.ctx_mut();
    set_reduced_motion(ctx, settings.reduce_motion);
    apply_hacker_theme(ctx, settings.theme);
}

/// Re-apply theme when the user changes the colour / scanline / beep
/// settings. Cheap (one-shot Visuals + Style replacement, no per-frame
/// cost) so we just listen for `WorldSettings` changes.
fn restyle_on_change(mut contexts: EguiContexts, settings: Res<WorldSettings>) {
    if settings.is_changed() {
        let ctx = contexts.ctx_mut();
        set_reduced_motion(ctx, settings.reduce_motion);
        apply_hacker_theme(ctx, settings.theme);
    }
}

fn toggle_editor(
    _keys: Res<ButtonInput<KeyCode>>,
    state: Res<EditorState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    // F3/ESC handling is owned by `menu.rs` now. This system only keeps
    // the cursor released while the editor panel is visible.
    if state.open {
        if let Ok(mut window) = windows.get_single_mut() {
            window.cursor.grab_mode = CursorGrabMode::None;
            window.cursor.visible = true;
        }
    }
}

/// F6 toggles [`SimPause`] from anywhere (editor open or closed), so
/// the player can freeze the world for a screenshot or precision
/// building without leaving the game view. The binding is handled
/// here rather than in `menu.rs` because the pause state lives with
/// the editor resources and wants to be published the same frame.
fn toggle_sim_pause(keys: Res<ButtonInput<KeyCode>>, mut pause: ResMut<SimPause>) {
    if keys.just_pressed(KeyCode::F6) {
        pause.paused = !pause.paused;
        info!("sim pause = {}", pause.paused);
    }
}

/// Push the current smoothed FPS into the rolling buffer every frame
/// so the GRAFIK sparkline always has fresh data even before the
/// editor has ever been opened.
fn sample_fps_history(
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    mut hist: ResMut<FpsHistory>,
) {
    let fps = diagnostics
        .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0) as f32;
    if hist.samples.len() >= 120 {
        hist.samples.pop_front();
    }
    hist.samples.push_back(fps);
}

#[allow(clippy::too_many_arguments)]
fn draw_editor(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    mut settings: ResMut<WorldSettings>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    governor: Res<StreamingGovernor>,
    mut library: ResMut<TextureLibrary>,
    mut builder: ResMut<BuilderState>,
    clipboard: Res<BuilderClipboard>,
    history: Res<BuilderHistory>,
    mut studio: ResMut<AnimationStudio>,
    mut pause: ResMut<SimPause>,
    fps_hist: Res<FpsHistory>,
    mut tools: EditorWorldTools,
) {
    let ctx = contexts.ctx_mut();
    handle_editor_keyboard(&tools.keys, &mut state);

    let anim = ctx.animate_bool_with_time(egui::Id::new("editor_open"), state.open, 0.18);
    if anim <= 0.001 {
        return;
    }
    let eased = 1.0 - (1.0 - anim).powi(3);

    let screen_rect = ctx.screen_rect();
    let panel_w = (screen_rect.width() * 0.60)
        .clamp(640.0, 860.0)
        .min(screen_rect.width() - 32.0);
    let panel_h = (screen_rect.height() * 0.78)
        .clamp(560.0, 720.0)
        .min(screen_rect.height() - 42.0);
    let center = screen_rect.center();
    let target_pos = egui::pos2(
        (screen_rect.right() - panel_w - 24.0).max(screen_rect.left() + 16.0),
        center.y - panel_h * 0.5,
    );
    let slide_x = (1.0 - eased) * 32.0;
    let pos = egui::pos2(target_pos.x + slide_x, target_pos.y);
    let panel_rect = egui::Rect::from_min_size(pos, egui::vec2(panel_w, panel_h));
    let ui_time = ctx.input(|i| i.time) as f32;

    // Keep the game readable behind the editor; the panel is an in-world
    // hologram, not a modal desktop window.
    let bg_layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("editor_dim"));
    ctx.layer_painter(bg_layer).rect_filled(
        screen_rect,
        0.0,
        egui::Color32::from_black_alpha((eased * 54.0) as u8),
    );

    let mut frame = crate::ui_kit::toolbench_frame(settings.theme);
    frame.fill = settings.theme.panel_fill(0.58 + eased * 0.18);
    frame.stroke = egui::Stroke::new(
        1.2,
        settings
            .theme
            .color
            .primary()
            .linear_multiply(0.55 + eased * 0.35),
    );

    draw_editor_hologram_backplate(ctx, panel_rect, settings.theme, ui_time, eased);

    let response = egui::Window::new("voxel_native_editor")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(frame)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            // Stash theme into ctx data so legacy helpers (`section_heading`)
            // can pick it up without threading through every signature.
            let theme = settings.theme;
            ui.ctx().data_mut(|d| {
                d.insert_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme"), theme);
            });

            draw_header(ui, &mut state, theme);
            ui.add_space(4.0);
            draw_tab_bar(ui, &mut state, theme);
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match state.tab {
                    EditorTab::World => {
                        draw_world_tab(ui, &mut state, &mut settings, &governor, &mut player_q)
                    }
                    EditorTab::Graphics => draw_graphics_tab(ui, &mut settings, &fps_hist),
                    EditorTab::Weather => draw_weather_tab(ui, &mut settings),
                    EditorTab::Time => draw_time_tab(ui, &mut settings),
                    EditorTab::Player => draw_player_tab(ui, &mut player_q),
                    EditorTab::Textures => draw_textures_tab(ui, &mut library),
                    EditorTab::Builder => draw_builder_tab(
                        ui,
                        &mut state,
                        &mut builder,
                        &clipboard,
                        &history,
                        &mut player_q,
                    ),
                    EditorTab::Animation => draw_animation_tab(ui, &mut studio),
                    EditorTab::City => draw_city_tab(ui, &mut tools.city),
                    EditorTab::Bots => {
                        let selected_area = tools.selection.aabb();
                        crate::bots::draw_bots_editor(
                            ui,
                            &mut tools.bots,
                            &mut settings,
                            selected_area,
                        )
                    }
                    EditorTab::System => draw_system_tab(
                        ui,
                        &mut state,
                        &mut settings,
                        &diagnostics,
                        &world,
                        &streamer,
                        &governor,
                        &mut pause,
                    ),
                });
            ui.add_space(6.0);
            // Status bar (terminal-style) just above the action footer.
            let fps = diagnostics
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0) as f32;
            let mode = if pause.paused { "EDIT" } else { "PLAY" };
            let base = status_line(
                fps,
                world.chunks.len(),
                settings.seed,
                settings.time_of_day,
                None,
            );
            let full = format!("[MODE {}] {}", mode, base);
            draw_status_bar(ui, theme, &full);
            draw_footer(ui, &mut state, &mut settings);
        });

    // CRT scanline overlay across the panel rect (no-op when disabled).
    if let Some(r) = response.as_ref() {
        paint_scanlines(ctx, r.response.rect, settings.theme);
    }

    if state.open && anim > 0.99 {
        let pointer_clicked = ctx.input(|i| i.pointer.any_click());
        let pointer_pos = ctx.pointer_hover_pos().unwrap_or_default();
        let over_panel = response
            .as_ref()
            .map(|r| r.response.rect.contains(pointer_pos))
            .unwrap_or(false);
        if pointer_clicked && !over_panel {
            state.open = false;
        }
    }
}

fn handle_editor_keyboard(keys: &ButtonInput<KeyCode>, state: &mut EditorState) {
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    if alt {
        for (key, tab) in [
            (KeyCode::Digit1, EditorTab::World),
            (KeyCode::Digit2, EditorTab::Graphics),
            (KeyCode::Digit3, EditorTab::Weather),
            (KeyCode::Digit4, EditorTab::Time),
            (KeyCode::Digit5, EditorTab::Player),
            (KeyCode::Digit6, EditorTab::Textures),
            (KeyCode::Digit7, EditorTab::Animation),
            (KeyCode::Digit8, EditorTab::City),
            (KeyCode::Digit9, EditorTab::Bots),
            (KeyCode::Digit0, EditorTab::System),
        ] {
            if keys.just_pressed(key) {
                state.tab = tab;
                return;
            }
        }
        if keys.just_pressed(KeyCode::KeyB) && state.show_classic_builder {
            state.tab = EditorTab::Builder;
            return;
        }
    }

    if keys.just_pressed(KeyCode::PageDown) || keys.just_pressed(KeyCode::PageUp) {
        let tabs: Vec<EditorTab> = EditorTab::all()
            .into_iter()
            .filter(|tab| *tab != EditorTab::Builder || state.show_classic_builder)
            .collect();
        if tabs.is_empty() {
            return;
        }
        let current = tabs.iter().position(|tab| *tab == state.tab).unwrap_or(0);
        let next = if keys.just_pressed(KeyCode::PageUp) {
            current.checked_sub(1).unwrap_or(tabs.len() - 1)
        } else {
            (current + 1) % tabs.len()
        };
        if let Some(tab) = tabs.get(next).copied() {
            state.tab = tab;
        }
    }
}

fn draw_editor_hologram_backplate(
    ctx: &egui::Context,
    rect: egui::Rect,
    theme: crate::theme::ThemeSettings,
    time: f32,
    alpha: f32,
) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("editor_hologram_backplate"),
    ));
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let a = alpha.clamp(0.0, 1.0);
    let glow = egui::Color32::from_rgba_unmultiplied(
        primary.r(),
        primary.g(),
        primary.b(),
        (a * 130.0) as u8,
    );
    let faint = egui::Color32::from_rgba_unmultiplied(dim.r(), dim.g(), dim.b(), (a * 46.0) as u8);
    let outer = rect.expand(12.0);
    painter.rect_stroke(
        outer,
        egui::Rounding::same(10.0),
        egui::Stroke::new(1.0, faint),
    );
    painter.rect_stroke(
        rect.expand(4.0),
        egui::Rounding::same(8.0),
        egui::Stroke::new(1.4, glow),
    );

    let corner = 46.0;
    for (x, y, sx, sy) in [
        (outer.left(), outer.top(), 1.0, 1.0),
        (outer.right(), outer.top(), -1.0, 1.0),
        (outer.left(), outer.bottom(), 1.0, -1.0),
        (outer.right(), outer.bottom(), -1.0, -1.0),
    ] {
        let p = egui::pos2(x, y);
        painter.line_segment(
            [p, p + egui::vec2(corner * sx, 0.0)],
            egui::Stroke::new(2.0, glow),
        );
        painter.line_segment(
            [p, p + egui::vec2(0.0, corner * sy)],
            egui::Stroke::new(2.0, glow),
        );
    }

    let sweep_x = rect.left() + ((time * 0.18).fract()) * rect.width();
    painter.line_segment(
        [
            egui::pos2(sweep_x, rect.top()),
            egui::pos2(sweep_x + 38.0, rect.bottom()),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(
                primary.r(),
                primary.g(),
                primary.b(),
                (a * 70.0) as u8,
            ),
        ),
    );

    let rows = 9;
    for i in 0..=rows {
        let k = i as f32 / rows as f32;
        let y = rect.top() + k * rect.height();
        let pulse = ((time * 2.0 + i as f32 * 0.7).sin() * 0.5 + 0.5) * 28.0;
        painter.line_segment(
            [
                egui::pos2(rect.left() - 8.0, y),
                egui::pos2(rect.right() + 8.0, y),
            ],
            egui::Stroke::new(
                0.7,
                egui::Color32::from_rgba_unmultiplied(
                    primary.r(),
                    primary.g(),
                    primary.b(),
                    (a * (18.0 + pulse)) as u8,
                ),
            ),
        );
    }

    let anchor = egui::pos2(rect.left() - 54.0, rect.center().y);
    painter.circle_stroke(anchor, 22.0, egui::Stroke::new(1.0, glow));
    painter.line_segment(
        [anchor, egui::pos2(rect.left(), rect.center().y)],
        egui::Stroke::new(1.0, glow),
    );
}

fn draw_header(ui: &mut egui::Ui, state: &mut EditorState, theme: crate::theme::ThemeSettings) {
    draw_banner(ui, theme, "LIQUID TOOLBENCH");
    let style_label = match theme.style {
        ThemeStyle::LiquidGlass => "Liquid Glass",
        ThemeStyle::NeonToolbench => "Neon Toolbench",
        ThemeStyle::ClassicCrt => "Classic CRT",
    };
    ui.horizontal(|ui| {
        crate::ui_kit::status_chip(ui, state.tab.icon(), "TAB", state.tab.label(), theme);
        crate::ui_kit::status_chip(ui, Icon::Hud, "STYLE", style_label, theme);
        crate::ui_kit::status_chip(ui, Icon::Help, "TOOLS", "sidebar tabs / page step", theme);
    });
    // Tiny inline close "x" so the panel still has a visible close.
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = egui::Button::new(egui::RichText::new("[ X ]").color(ALERT).monospace())
                .fill(egui::Color32::BLACK)
                .stroke(egui::Stroke::new(1.0, ALERT))
                .rounding(egui::Rounding::ZERO);
            if ui.add(btn).clicked() {
                state.open = false;
            }
        });
    });
}

fn draw_tab_bar(ui: &mut egui::Ui, state: &mut EditorState, theme: crate::theme::ThemeSettings) {
    // Icon-first tab strip (Phase A): each tab is a 56×52 icon chip with
    // the German caption underneath. Works for pre-readers and for users
    // who know the legacy labels alike. Wraps to a second row on narrow
    // windows via `horizontal_wrapped`.
    //
    // The legacy "BAUEN" cuboid-builder tab is hidden by default since
    // the SketchUp-style direct-manipulation system replaces it. Users
    // who still want the precision-cuboid workflow can re-enable it
    // from the SYSTEM tab; if they then land on it and disable the
    // toggle, we silently bounce them back to WELT.
    if state.tab == EditorTab::Builder && !state.show_classic_builder {
        state.tab = EditorTab::World;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for tab in EditorTab::all() {
            if tab == EditorTab::Builder && !state.show_classic_builder {
                continue;
            }
            let selected = state.tab == tab;
            if crate::icons::icon_tab_chip(ui, tab.icon(), tab.label(), selected, theme).clicked() {
                state.tab = tab;
            }
        }
    });
}

fn section_heading(ui: &mut egui::Ui, text: &str) {
    // Backwards-compat shim: callers haven't been updated yet, so we
    // route through the global theme via egui's data store. We pull
    // the theme out of `ui.ctx().data()` if present, else fall back
    // to defaults — costs nothing.
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    section_box(ui, theme, text);
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

fn material_swatch_chip(
    ui: &mut egui::Ui,
    entry: BlockPaletteEntry,
    selected: bool,
    theme: crate::theme::ThemeSettings,
) -> egui::Response {
    let colors = theme.semantic();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(158.0, 50.0), egui::Sense::click());
    let fill = if selected {
        colors.selected
    } else if response.hovered() {
        colors.surface_strong
    } else {
        colors.surface
    };
    let stroke = if selected {
        colors.accent
    } else {
        colors.stroke.linear_multiply(0.68)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::Rounding::same(7.0), fill);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(7.0),
        egui::Stroke::new(1.0, stroke),
    );

    let swatch = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(34.0, 34.0));
    painter.rect_filled(
        swatch,
        egui::Rounding::same(4.0),
        block_egui_color(entry.block),
    );
    painter.rect_stroke(
        swatch,
        egui::Rounding::same(4.0),
        egui::Stroke::new(1.0, egui::Color32::from_white_alpha(80)),
    );
    painter.text(
        rect.min + egui::vec2(50.0, 17.0),
        egui::Align2::LEFT_CENTER,
        entry.label,
        egui::FontId::monospace(11.0),
        colors.text,
    );
    painter.text(
        rect.min + egui::vec2(50.0, 34.0),
        egui::Align2::LEFT_CENTER,
        entry.role,
        egui::FontId::monospace(8.5),
        colors.text_muted,
    );
    response.on_hover_text(format!("{}: {}", entry.label, entry.role))
}

fn world_mode_card_icon(mode: WorldModeCard) -> Icon {
    match mode {
        WorldModeCard::ExploreFar => Icon::Globe,
        WorldModeCard::SmoothBuild => Icon::Brush,
        WorldModeCard::FastLaptop => Icon::Optimize,
        WorldModeCard::Cinematic => Icon::Eye,
    }
}

fn world_mode_card_active(settings: &WorldSettings, mode: WorldModeCard) -> bool {
    match mode {
        WorldModeCard::ExploreFar => {
            settings.render_distance >= 60 && settings.graphics == GraphicsMode::Balanced
        }
        WorldModeCard::SmoothBuild => {
            (36..=44).contains(&settings.render_distance)
                && settings.runtime_profile == RuntimeProfile::Balanced
        }
        WorldModeCard::FastLaptop => {
            settings.render_distance <= 24 && settings.graphics == GraphicsMode::Fast
        }
        WorldModeCard::Cinematic => settings.graphics == GraphicsMode::High,
    }
}

fn draw_world_tab(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    settings: &mut WorldSettings,
    governor: &StreamingGovernor,
    player_q: &mut Query<(&mut Transform, &mut Player)>,
) {
    section_heading(ui, "WORLD MODES");
    ui.horizontal_wrapped(|ui| {
        for mode in WorldModeCard::ALL {
            let active = world_mode_card_active(settings, mode);
            if crate::ui_kit::mode_card(
                ui,
                world_mode_card_icon(mode),
                mode.label(),
                mode.detail(),
                active,
                settings.theme,
            )
            .clicked()
            {
                settings.apply_world_mode_card(mode);
            }
        }
    });
    ui.add_space(8.0);

    section_heading(ui, "WELT-GENERATION");
    ui.horizontal(|ui| {
        ui.label("Seed:");
        let mut seed_text = state
            .pending_seed
            .map(|s| s.to_string())
            .unwrap_or_else(|| settings.seed.to_string());
        if ui
            .add(egui::TextEdit::singleline(&mut seed_text).desired_width(120.0))
            .changed()
        {
            if let Ok(n) = seed_text.parse::<u32>() {
                state.pending_seed = Some(n);
            }
        }
        if crate::ui_kit::icon_action(ui, Icon::Seed, "Random", false, settings.theme).clicked() {
            state.pending_seed = Some(rand_seed());
        }
        if crate::ui_kit::icon_action(ui, Icon::Optimize, "Regenerate", false, settings.theme)
            .clicked()
        {
            if let Some(s) = state.pending_seed.take() {
                settings.seed = s;
            }
            state.regen_requested = true;
        }
        ui.label(
            egui::RichText::new(format!("aktuell 0x{:08X}", settings.seed))
                .size(11.0)
                .monospace()
                .color(egui::Color32::from_gray(170)),
        );
    });
    ui.add_space(6.0);
    let mut advanced = settings.show_advanced_settings;
    let theme = settings.theme;
    crate::ui_kit::advanced_section(ui, theme, "engine tuning", &mut advanced, |ui| {
        section_heading(ui, "STREAMING");
        ui.horizontal_wrapped(|ui| {
            if crate::ui_kit::icon_action(
                ui,
                Icon::Optimize,
                "NeuroCore",
                settings.neurocore_enabled,
                theme,
            )
            .clicked()
            {
                settings.neurocore_enabled = !settings.neurocore_enabled;
            }
            ui.label("Profil:");
            for profile in RuntimeProfile::ALL {
                let selected = settings.runtime_profile == profile;
                if crate::ui_kit::tab_chip(ui, Icon::Optimize, profile.label(), selected, theme)
                    .clicked()
                {
                    settings.runtime_profile = profile;
                }
            }
        });
        ui.horizontal(|ui| {
            if crate::ui_kit::icon_action(
                ui,
                Icon::Animation,
                "Shuttle AI",
                settings.ship_skirmish_ai,
                theme,
            )
            .clicked()
            {
                settings.ship_skirmish_ai = !settings.ship_skirmish_ai;
                settings.save();
            }
            crate::ui_kit::status_chip(
                ui,
                Icon::Help,
                "COMBAT",
                if settings.ship_skirmish_ai {
                    "relaxed waves"
                } else {
                    "free flight"
                },
                theme,
            );
        });
        ui.add(egui::Slider::new(&mut settings.target_fps, 30.0..=144.0).text("Target FPS"));
        ui.add(
            egui::Slider::new(
                &mut settings.render_distance,
                SAFE_MIN_RENDER_DISTANCE..=SAFE_MAX_RENDER_DISTANCE,
            )
            .text("Render-Distanz (Chunks)"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.vertical_chunks,
                SAFE_MIN_VERTICAL_CHUNKS..=SAFE_MAX_VERTICAL_CHUNKS,
            )
            .text("Vertikale Chunks"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.chunks_per_frame,
                SAFE_MIN_CHUNKS_PER_FRAME..=SAFE_MAX_CHUNKS_PER_FRAME,
            )
            .text("Terrain-Jobs / Frame"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.meshes_per_frame,
                SAFE_MIN_MESHES_PER_FRAME..=SAFE_MAX_MESHES_PER_FRAME,
            )
            .text("Mesh-Jobs / Frame"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.mesh_applies_per_frame,
                SAFE_MIN_MESH_APPLIES_PER_FRAME..=SAFE_MAX_MESH_APPLIES_PER_FRAME,
            )
            .text("GPU-Mesh Uploads / Frame"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.max_in_flight_terrain,
                SAFE_MIN_IN_FLIGHT_TERRAIN..=SAFE_MAX_IN_FLIGHT_TERRAIN,
            )
            .text("Max Terrain-Jobs aktiv"),
        );
        ui.add(
            egui::Slider::new(
                &mut settings.max_in_flight_meshes,
                SAFE_MIN_IN_FLIGHT_MESHES..=SAFE_MAX_IN_FLIGHT_MESHES,
            )
            .text("Max Mesh-Jobs aktiv"),
        );
        ui.label(
            egui::RichText::new(format!(
                "NeuroCore: {} {} {}  |  RD {} -> {}  |  {:.0} FPS  |  P {:.0}% Q {:.0}%  |  {}",
                governor.profile.label(),
                governor.intent.label(),
                governor.quality.label(),
                governor
                    .target_render_distance
                    .max(settings.render_distance as i32),
                governor.active_render_distance(settings.render_distance),
                governor.smoothed_fps,
                governor.frame_pressure * 100.0,
                governor.queue_pressure * 100.0,
                governor.status
            ))
            .size(11.5)
            .color(egui::Color32::from_rgb(120, 230, 255))
            .monospace(),
        );
        ui.horizontal_wrapped(|ui| {
            if crate::ui_kit::mode_card(
                ui,
                Icon::Detail,
                "RD 50 Smooth",
                "long view",
                settings.render_distance == 50,
                theme,
            )
            .clicked()
            {
                settings.render_distance = 50;
                settings.chunks_per_frame = 14;
                settings.meshes_per_frame = 12;
                settings.mesh_applies_per_frame = 8;
                settings.max_in_flight_terrain = 192;
                settings.max_in_flight_meshes = 144;
            }
            if crate::ui_kit::mode_card(
                ui,
                Icon::Optimize,
                "RD 32 Fast",
                "steady laptop",
                settings.render_distance == 32,
                theme,
            )
            .clicked()
            {
                settings.render_distance = 32;
                settings.chunks_per_frame = 16;
                settings.meshes_per_frame = 14;
                settings.mesh_applies_per_frame = 8;
                settings.max_in_flight_terrain = 168;
                settings.max_in_flight_meshes = 128;
            }
            if crate::ui_kit::mode_card(
                ui,
                Icon::Animation,
                "Combat 18",
                "lowest spikes",
                settings.render_distance == 18,
                theme,
            )
            .clicked()
            {
                settings.render_distance = 18;
                settings.chunks_per_frame = 18;
                settings.meshes_per_frame = 16;
                settings.mesh_applies_per_frame = 8;
                settings.max_in_flight_terrain = 128;
                settings.max_in_flight_meshes = 96;
            }
        });
    });
    settings.show_advanced_settings = advanced;

    ui.add_space(8.0);
    section_heading(ui, "BOOKMARKS");
    ui.label(
        egui::RichText::new(
            "Speichere aktuelle Position + Blickrichtung als Bookmark. Klicke spaeter zum Teleport.",
        )
        .size(11.0)
        .color(egui::Color32::from_gray(180)),
    );
    // Row: name input + save button.
    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.add(
            egui::TextEdit::singleline(&mut state.new_bookmark_name)
                .desired_width(200.0)
                .hint_text("z.B. basis_01"),
        );
        let disabled = state.new_bookmark_name.trim().is_empty();
        let resp = ui.add_enabled(
            !disabled,
            term_button("+ Hier speichern", false, settings.theme),
        );
        if resp.clicked() {
            if let Ok((t, p)) = player_q.get_single() {
                let bm = crate::settings::Bookmark {
                    name: state.new_bookmark_name.trim().to_string(),
                    pos: t.translation.to_array(),
                    yaw: p.yaw,
                    pitch: p.pitch,
                };
                settings.bookmarks.push(bm);
                state.new_bookmark_name.clear();
            }
        }
    });
    // List with teleport + delete buttons.
    let mut remove_idx: Option<usize> = None;
    let mut tp_idx: Option<usize> = None;
    for (i, bm) in settings.bookmarks.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "[{:02}] {:<16}  ({:>6.1}, {:>6.1}, {:>6.1})",
                    i, bm.name, bm.pos[0], bm.pos[1], bm.pos[2]
                ))
                .monospace()
                .size(12.0),
            );
            if ui
                .add(term_button(">> TP", false, settings.theme))
                .clicked()
            {
                tp_idx = Some(i);
            }
            if ui.add(term_button("X", false, settings.theme)).clicked() {
                remove_idx = Some(i);
            }
        });
    }
    if let Some(i) = tp_idx {
        let bm = settings.bookmarks[i].clone();
        if let Ok((mut t, mut p)) = player_q.get_single_mut() {
            t.translation = Vec3::from_array(bm.pos);
            p.yaw = bm.yaw;
            p.pitch = bm.pitch;
        }
    }
    if let Some(i) = remove_idx {
        settings.bookmarks.remove(i);
    }
}

fn draw_graphics_tab(ui: &mut egui::Ui, settings: &mut WorldSettings, fps_hist: &FpsHistory) {
    section_heading(ui, "VISUAL MODES");
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::mode_card(
            ui,
            Icon::Sun,
            "Zen Garden",
            "Sakura glass UI, blossom scenery and bright golden-hour sky.",
            settings.scenery_quality == SceneryQuality::Lush
                && settings.theme.color == ThemeColor::Sakura
                && settings.graphics == GraphicsMode::High
                && settings.time_mode == TimeMode::Fixed
                && (settings.time_of_day - 17.8).abs() < 0.25,
            settings.theme,
        )
        .clicked()
        {
            settings.apply_zen_garden_look();
        }
        if crate::ui_kit::mode_card(
            ui,
            Icon::Eye,
            "Clear",
            "Readable distance and calm contrast.",
            settings.graphics == GraphicsMode::Balanced
                && settings.theme.color == ThemeColor::Green,
            settings.theme,
        )
        .clicked()
        {
            settings.graphics = GraphicsMode::Balanced;
            settings.theme.color = ThemeColor::Green;
        }
        if crate::ui_kit::mode_card(
            ui,
            Icon::Intensity,
            "Vivid",
            "Brighter accent and stronger showcase colors.",
            settings.graphics == GraphicsMode::High && settings.theme.color == ThemeColor::Blue,
            settings.theme,
        )
        .clicked()
        {
            settings.graphics = GraphicsMode::High;
            settings.theme.color = ThemeColor::Blue;
        }
        if crate::ui_kit::mode_card(
            ui,
            Icon::Moon,
            "Night Glow",
            "Blue HUD accent for dusk and night play.",
            settings.theme.color == ThemeColor::Blue,
            settings.theme,
        )
        .clicked()
        {
            settings.theme.color = ThemeColor::Blue;
            settings.time_mode = TimeMode::Fixed;
            settings.time_of_day = 22.0;
        }
        if crate::ui_kit::mode_card(
            ui,
            Icon::Accessibility,
            "High Contrast",
            "Amber accents and clearer panel separation.",
            settings.theme.color == ThemeColor::Amber,
            settings.theme,
        )
        .clicked()
        {
            settings.theme.color = ThemeColor::Amber;
            settings.hud_panel_opacity = 0.86;
        }
    });
    ui.add_space(8.0);

    section_heading(ui, "PRESET");
    ui.horizontal(|ui| {
        for (mode, icon, label) in [
            (GraphicsMode::Fast, Icon::Optimize, "Fast"),
            (GraphicsMode::Balanced, Icon::Detail, "Balanced"),
            (GraphicsMode::High, Icon::Intensity, "High"),
        ] {
            let selected = settings.graphics == mode;
            if crate::ui_kit::tab_chip(ui, icon, label, selected, settings.theme).clicked() {
                settings.graphics = mode;
            }
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "SCENERY");
    ui.horizontal_wrapped(|ui| {
        for quality in SceneryQuality::ALL {
            let selected = settings.scenery_quality == quality;
            if crate::ui_kit::tab_chip(ui, Icon::Detail, quality.label(), selected, settings.theme)
                .on_hover_text(quality.detail())
                .clicked()
            {
                settings.scenery_quality = quality;
            }
        }
    });
    ui.label(
        egui::RichText::new("Regenerate refreshes trees and blossom density for the current seed.")
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
    );
    ui.add_space(6.0);
    section_heading(ui, "SICHTFELD");
    ui.add(egui::Slider::new(&mut settings.fov_deg, 50.0..=110.0).text("FOV (Grad)"));
    ui.label(
        egui::RichText::new("Sprint-Kick wird automatisch oben draufgepackt.")
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
    );

    ui.add_space(8.0);
    section_heading(ui, "FPS-HISTORIE (120 Samples ~ 2 s)");
    draw_fps_sparkline(ui, fps_hist, settings.theme);
}

/// Custom painter that renders the FPS rolling buffer as a small bar
/// graph with 30/60/120 fps grid lines. Cheap — ~120 line segments.
fn draw_fps_sparkline(ui: &mut egui::Ui, hist: &FpsHistory, theme: crate::theme::ThemeSettings) {
    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let primary = theme.color.primary();
    let dim = theme.color.dim();
    let deep = theme.color.deep();
    // Frame background.
    painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
    painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, dim));
    // Grid lines at 30 / 60 / 120 fps. Max shown = 144 so 120 is
    // visible as a near-top line.
    let max_fps = 144.0_f32;
    for (y_fps, color) in [(30.0, deep), (60.0, dim), (120.0, dim)] {
        let y = rect.bottom() - (y_fps / max_fps) * rect.height();
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, color),
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{}", y_fps as i32),
            egui::FontId::monospace(9.0),
            dim,
        );
    }
    // Plot line — connect samples.
    let n = hist.samples.len().max(1);
    if n > 1 {
        let step = rect.width() / 119.0;
        let mut prev: Option<egui::Pos2> = None;
        for (i, &fps) in hist.samples.iter().enumerate() {
            let x = rect.left() + (i as f32) * step;
            let clamped = fps.clamp(0.0, max_fps);
            let y = rect.bottom() - (clamped / max_fps) * rect.height();
            let p = egui::pos2(x, y);
            if let Some(pp) = prev {
                painter.line_segment([pp, p], egui::Stroke::new(1.2, primary));
            }
            prev = Some(p);
        }
    }
    // Current value in top-right.
    let cur = hist.samples.back().copied().unwrap_or(0.0);
    let avg = if hist.samples.is_empty() {
        0.0
    } else {
        hist.samples.iter().sum::<f32>() / hist.samples.len() as f32
    };
    let lo = hist.samples.iter().cloned().fold(f32::INFINITY, f32::min);
    let hi = hist
        .samples
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    let label = format!(
        "cur {:>3.0}  avg {:>3.0}  lo {:>3.0}  hi {:>3.0}",
        cur,
        avg,
        if lo.is_finite() { lo } else { 0.0 },
        if hi.is_finite() { hi } else { 0.0 }
    );
    painter.text(
        egui::pos2(rect.right() - 4.0, rect.top() + 4.0),
        egui::Align2::RIGHT_TOP,
        label,
        egui::FontId::monospace(11.0),
        primary,
    );
}

fn draw_weather_tab(ui: &mut egui::Ui, settings: &mut WorldSettings) {
    section_heading(ui, "PRESET");
    let mut preset = settings.weather.preset;
    ui.horizontal_wrapped(|ui| {
        for (p, icon, label) in [
            (WeatherPreset::Clear, Icon::Sun, "Clear"),
            (WeatherPreset::LightRain, Icon::Rain, "Rain"),
            (WeatherPreset::Storm, Icon::Intensity, "Storm"),
            (WeatherPreset::Snow, Icon::Snow, "Snow"),
            (WeatherPreset::Fog, Icon::Fog, "Fog"),
            (WeatherPreset::Custom, Icon::Gear, "Custom"),
        ] {
            let selected = preset == p;
            if crate::ui_kit::tab_chip(ui, icon, label, selected, settings.theme).clicked() {
                preset = p;
                settings.weather.apply_preset(p);
            }
        }
    });
    ui.add_space(6.0);
    let mut advanced = settings.show_advanced_settings;
    let theme = settings.theme;
    crate::ui_kit::advanced_section(ui, theme, "weather tuning", &mut advanced, |ui| {
        section_heading(ui, "FEINTUNING");
        ui.add(egui::Slider::new(&mut settings.weather.rain_intensity, 0.0..=1.0).text("Regen"));
        ui.add(egui::Slider::new(&mut settings.weather.snow_intensity, 0.0..=1.0).text("Schnee"));
        ui.add(egui::Slider::new(&mut settings.weather.fog_density, 0.0..=1.0).text("Nebel"));
        ui.add(egui::Slider::new(&mut settings.weather.wind_x, -10.0..=10.0).text("Wind X"));
        ui.add(egui::Slider::new(&mut settings.weather.wind_z, -10.0..=10.0).text("Wind Z"));
    });
    settings.show_advanced_settings = advanced;
}

fn draw_time_tab(ui: &mut egui::Ui, settings: &mut WorldSettings) {
    section_heading(ui, "TIME CARDS");
    ui.horizontal_wrapped(|ui| {
        for (label, detail, icon, mode, hour) in [
            (
                "Morning",
                "Clean daylight for exploring.",
                Icon::Sun,
                TimeMode::Fixed,
                6.0,
            ),
            (
                "Noon",
                "Maximum visibility.",
                Icon::Sun,
                TimeMode::Fixed,
                12.0,
            ),
            (
                "Sunset",
                "Warm cinematic shadows.",
                Icon::Intensity,
                TimeMode::Fixed,
                19.0,
            ),
            (
                "Night",
                "Neon contrast and glow.",
                Icon::Moon,
                TimeMode::Fixed,
                23.0,
            ),
            (
                "Cycle",
                "Let the world move naturally.",
                Icon::Loop,
                TimeMode::Cycle,
                settings.time_of_day,
            ),
        ] {
            let active = settings.time_mode == mode
                && (mode == TimeMode::Cycle || (settings.time_of_day - hour).abs() < 0.25);
            if crate::ui_kit::mode_card(ui, icon, label, detail, active, settings.theme).clicked() {
                settings.time_mode = mode;
                if mode == TimeMode::Fixed {
                    settings.time_of_day = hour;
                }
            }
        }
    });
    ui.add_space(8.0);

    section_heading(ui, "MODUS");
    ui.horizontal(|ui| {
        for (mode, icon, label) in [
            (TimeMode::Cycle, Icon::Loop, "Cycle"),
            (TimeMode::Fixed, Icon::Pin, "Fixed"),
        ] {
            let selected = settings.time_mode == mode;
            if crate::ui_kit::tab_chip(ui, icon, label, selected, settings.theme).clicked() {
                settings.time_mode = mode;
            }
        }
    });
    ui.add_space(6.0);
    let mut advanced = settings.show_advanced_settings;
    let theme = settings.theme;
    crate::ui_kit::advanced_section(ui, theme, "time tuning", &mut advanced, |ui| {
        section_heading(ui, "UHRZEIT");
        ui.add(
            egui::Slider::new(&mut settings.time_of_day, 0.0..=24.0)
                .text("Stunde")
                .fixed_decimals(2),
        );
        ui.add(
            egui::Slider::new(&mut settings.cycle_speed, 0.0..=1.0).text("Zyklus-Tempo (min/s)"),
        );
    });
    settings.show_advanced_settings = advanced;
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (label, icon, t) in [
            ("Morning 06:00", Icon::Sun, 6.0),
            ("Noon 12:00", Icon::Sun, 12.0),
            ("Sunset 19:00", Icon::Intensity, 19.0),
            ("Night 23:00", Icon::Moon, 23.0),
        ] {
            if crate::ui_kit::tab_chip(ui, icon, label, false, settings.theme).clicked() {
                settings.time_of_day = t;
            }
        }
    });
}

fn draw_player_tab(ui: &mut egui::Ui, player_q: &mut Query<(&mut Transform, &mut Player)>) {
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    let Ok((mut tf, mut player)) = player_q.get_single_mut() else {
        ui.label("Spieler nicht bereit.");
        return;
    };
    section_heading(ui, "POSITION");
    ui.label(format!(
        "X {:.1}   Y {:.1}   Z {:.1}",
        tf.translation.x, tf.translation.y, tf.translation.z
    ));
    ui.horizontal(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Teleport, "Origin", false, theme).clicked() {
            tf.translation = Vec3::new(0.0, 120.0, 0.0);
            player.velocity = Vec3::ZERO;
            player.placed_on_surface = false;
        }
        if crate::ui_kit::icon_action(ui, Icon::Move, "Y 200", false, theme).clicked() {
            tf.translation.y = 200.0;
            player.velocity = Vec3::ZERO;
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "VERHALTEN");
    if crate::ui_kit::icon_action(ui, Icon::Move, "Flying", player.flying, theme).clicked() {
        player.flying = !player.flying;
    }
    ui.add(egui::Slider::new(&mut player.walk_speed, 1.0..=20.0).text("Gehtempo"));
    ui.add(egui::Slider::new(&mut player.fly_speed, 4.0..=80.0).text("Flugtempo"));
    ui.add(egui::Slider::new(&mut player.sensitivity, 0.0005..=0.01).text("Maus-Sensitivitaet"));
}

fn draw_system_tab(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    settings: &mut WorldSettings,
    diagnostics: &bevy::diagnostic::DiagnosticsStore,
    world: &VoxelWorld,
    streamer: &ChunkStreamer,
    governor: &StreamingGovernor,
    pause: &mut SimPause,
) {
    section_heading(ui, "MODUS");
    ui.horizontal(|ui| {
        let play_selected = !pause.paused;
        let edit_selected = pause.paused;
        if ui
            .add(term_button("[>] PLAY", play_selected, settings.theme))
            .clicked()
        {
            pause.paused = false;
        }
        if ui
            .add(term_button(
                "[||] EDIT (pausiert)",
                edit_selected,
                settings.theme,
            ))
            .clicked()
        {
            pause.paused = true;
        }
        ui.label(
            egui::RichText::new("Pause toggle available")
                .size(11.0)
                .color(egui::Color32::from_gray(160))
                .monospace(),
        );
    });
    ui.label(
        egui::RichText::new(
            "EDIT: Tageszyklus ist eingefroren. Perfekt fuer Screenshots, Prefab-Bau oder Animationen.",
        )
        .size(11.0)
        .color(AMBER),
    );
    ui.add_space(6.0);

    section_heading(ui, "HUD + READABILITY");
    ui.horizontal_wrapped(|ui| {
        for profile in HudProfile::ALL {
            if crate::ui_kit::setting_card(
                ui,
                Icon::Hud,
                profile.label(),
                profile.detail(),
                settings.hud_profile == profile,
                settings.theme,
            )
            .clicked()
            {
                settings.hud_profile = profile;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(
            ui,
            Icon::Accessibility,
            "Reduce Motion",
            settings.reduce_motion,
            settings.theme,
        )
        .clicked()
        {
            settings.reduce_motion = !settings.reduce_motion;
        }
        ui.add(
            egui::Slider::new(&mut settings.hud_panel_opacity, 0.35..=0.95)
                .text("HUD panel opacity"),
        );
        if crate::ui_kit::icon_action(
            ui,
            Icon::Drawer,
            "Advanced",
            settings.show_advanced_settings,
            settings.theme,
        )
        .clicked()
        {
            settings.show_advanced_settings = !settings.show_advanced_settings;
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("Density:");
        for (density, label) in [
            (UiDensity::Compact, "Compact"),
            (UiDensity::Comfortable, "Comfortable"),
            (UiDensity::Spacious, "Spacious"),
        ] {
            let selected = settings.theme.density == density;
            if crate::ui_kit::icon_action(ui, Icon::Density, label, selected, settings.theme)
                .clicked()
            {
                settings.theme.density = density;
            }
        }
    });
    ui.add_space(4.0);
    let selected_theme_name = selected_theme_preset(settings.theme)
        .map(|preset| preset.name)
        .unwrap_or("Custom Mix");
    ui.label(
        egui::RichText::new(format!("Theme concept: {selected_theme_name}"))
            .size(11.0)
            .color(settings.theme.semantic().text_muted)
            .monospace(),
    );
    let preview_time = ui.input(|input| input.time as f32);
    ui.horizontal_wrapped(|ui| {
        for preset in THEME_PRESETS.iter() {
            let selected =
                settings.theme.style == preset.style && settings.theme.color == preset.color;
            if draw_theme_preview_card(ui, preset, selected, preview_time).clicked() {
                settings.theme.style = preset.style;
                settings.theme.color = preset.color;
            }
        }
    });
    ui.add_space(6.0);

    section_heading(ui, "BAUEN-PARADIGMA");
    if crate::ui_kit::icon_action(
        ui,
        Icon::Builder,
        "Classic Builder",
        state.show_classic_builder,
        settings.theme,
    )
    .clicked()
    {
        state.show_classic_builder = !state.show_classic_builder;
    }
    ui.label(
        egui::RichText::new(
            "Aus = neues SketchUp-artiges Direkt-Manipulations-System ist aktiv. \
             Ein = klassischer Praezisions-Cuboid-Builder zusaetzlich verfuegbar.",
        )
        .size(11.0)
        .color(AMBER),
    );
    ui.add_space(6.0);

    let mut perf_advanced = settings.show_advanced_settings;
    let perf_theme = settings.theme;
    crate::ui_kit::advanced_section(
        ui,
        perf_theme,
        "performance telemetry",
        &mut perf_advanced,
        |ui| {
            section_heading(ui, "PERFORMANCE");
            let fps = diagnostics
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            let ft = diagnostics
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FRAME_TIME)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            ui.label(format!("FPS:           {:>6.1}", fps));
            ui.label(format!("Frametime:     {:>6.2} ms", ft));
            ui.label(format!("Chunks geladen: {}", world.chunks.len()));
            ui.label(format!("Chunk-Meshes:   {}", streamer.entities.len()));
            ui.label(format!(
                "NeuroCore:     {} / {} / {}",
                governor.profile.label(),
                governor.intent.label(),
                governor.quality.label()
            ));
            ui.label(format!(
                "Budget RD:     Ziel {:>3} / aktiv {:>3}  ({})",
                settings.render_distance,
                governor.active_render_distance(settings.render_distance),
                governor.status
            ));
            ui.label(format!(
                "Budget Jobs:   terrain {:>3}/{:<3} mesh {:>3}/{:<3} upload {:>3}",
                governor.chunks_per_frame,
                governor.max_in_flight_terrain,
                governor.meshes_per_frame,
                governor.max_in_flight_meshes,
                governor.mesh_applies_per_frame
            ));
            ui.label(format!(
                "Quality FX:    shadow {:>2}  weather {:>3.0}%  weapon {:>3.0}%  cadence {:.1}s",
                governor.shadow_radius,
                governor.weather_fx_scale * 100.0,
                governor.weapon_fx_scale * 100.0,
                governor.update_cadence
            ));
            ui.label(format!(
                "Streaming: terrain {:>4} / mesh {:>4} / dirty {:>4}",
                streamer.pending_terrain.len(),
                streamer.pending_meshes.len(),
                streamer.dirty_queue.len()
            ));
            ui.label(format!(
                "Frontier: {}  cursor {}/{}",
                if streamer.frontier_complete {
                    "ready"
                } else {
                    "scan"
                },
                streamer.load_cursor,
                streamer.load_offsets.len()
            ));
        },
    );
    settings.show_advanced_settings = perf_advanced;

    ui.add_space(6.0);
    section_heading(ui, "SPEICHERN");
    ui.horizontal(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Save, "Save", false, settings.theme).clicked() {
            settings.save();
        }
        if crate::ui_kit::icon_action(ui, Icon::SaveAs, "Screenshot", false, settings.theme)
            .clicked()
        {
            state.screenshot_requested = true;
        }
    });

    ui.add_space(6.0);
    section_heading(ui, "HINWEISE");
    ui.label(
        egui::RichText::new(editor_navigation_hint())
            .size(12.0)
            .color(egui::Color32::from_gray(190)),
    );
    ui.label(
        egui::RichText::new(editor_action_hint())
            .size(12.0)
            .color(egui::Color32::from_gray(190)),
    );

    ui.add_space(8.0);
    section_heading(ui, "ADMIN");
    // here mirrors that so the editor can flip it without leaving the
    // panel. When `admin_mode` is off the cheat toggles below are
    // shown as disabled so casual players see they exist but cannot
    // flip them by accident.
    let mut admin = settings.cheats.admin_mode;
    if crate::ui_kit::icon_action(ui, Icon::Gear, "Admin", admin, settings.theme).clicked() {
        admin = !admin;
        settings.cheats.admin_mode = admin;
    }
    ui.add_enabled_ui(settings.cheats.admin_mode, |ui| {
        let mut infinite = settings.cheats.infinite_ammo;
        if crate::ui_kit::icon_action(
            ui,
            Icon::Intensity,
            "Infinite Ammo",
            infinite,
            settings.theme,
        )
        .clicked()
        {
            infinite = !infinite;
            settings.cheats.infinite_ammo = infinite;
        }
        ui.label(
            egui::RichText::new(
                "Aus = echte Magazine + Nachladen (R). An = Magazine fuellen sich sofort.",
            )
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
        );
    });

    ui.add_space(8.0);
    section_heading(ui, "THEME / ACCENT");
    ui.horizontal(|ui| {
        ui.label("Accent:");
        for (variant, label) in [
            (ThemeColor::Sakura, "SAKURA"),
            (ThemeColor::Green, "GREEN"),
            (ThemeColor::Amber, "AMBER"),
            (ThemeColor::Blue, "BLUE"),
            (ThemeColor::Red, "RED"),
        ] {
            let selected = settings.theme.color == variant;
            if crate::ui_kit::tab_chip(ui, Icon::LightBulb, label, selected, settings.theme)
                .clicked()
            {
                settings.theme.color = variant;
            }
        }
    });
    let mut scan = settings.theme.scanlines;
    if crate::ui_kit::icon_action(ui, Icon::Eye, "Scanlines", scan, settings.theme).clicked() {
        scan = !scan;
        settings.theme.scanlines = scan;
    }
    let mut beeps = settings.theme.beeps;
    if crate::ui_kit::icon_action(ui, Icon::Help, "Beeps", beeps, settings.theme).clicked() {
        beeps = !beeps;
        settings.theme.beeps = beeps;
    }
    ui.label(
        egui::RichText::new(
            "Aenderungen wirken sofort. AMBER schont Augen bei langen Sessions; RED ist ALERT-Modus.",
        )
        .size(11.0)
        .color(AMBER),
    );
}

fn draw_footer(ui: &mut egui::Ui, state: &mut EditorState, settings: &mut WorldSettings) {
    crate::ui_kit::compact_separator(ui, settings.theme);
    ui.horizontal(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Save, "Save", false, settings.theme).clicked() {
            settings.save();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui_kit::danger_action(ui, Icon::Close, "Close", settings.theme).clicked() {
                state.open = false;
            }
        });
    });
}

fn editor_navigation_hint() -> &'static str {
    "WASD bewegen  //  Space springen  //  Doppeltipp-W sprintet  //  F fliegt  //  Maus fuer Blick und Sketch-Orbit"
}

fn editor_action_hint() -> &'static str {
    "Toolbox waehlt Werkzeuge  //  Pencil/Rect/Push arbeiten direkt im Spiel  //  Save button speichert  //  ESC schliesst"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_hints_are_mouse_first_without_function_keys() {
        let hints = [editor_navigation_hint(), editor_action_hint()];

        for hint in hints {
            assert!(
                !["F1", "F2", "F3", "F5", "F7", "F8", "Tab", "1-9", "1-0"]
                    .iter()
                    .any(|token| hint.contains(token)),
                "editor hint still advertises old key workflow: {hint}"
            );
        }

        assert!(editor_action_hint().contains("Toolbox"));
        assert!(editor_action_hint().contains("Save"));
    }
}

fn handle_regen(
    mut state: ResMut<EditorState>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    settings: Res<WorldSettings>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !state.regen_requested {
        return;
    }
    state.regen_requested = false;

    world.generator = crate::terrain::TerrainGenerator::new(settings.seed)
        .with_scenery_quality(settings.scenery_quality);
    world.clear_chunks();
    world.column_top_cy.clear();
    world.edit_dirty_chunks.clear();
    world.edit_save_dirty = false;
    streamer.pending_terrain.clear();
    streamer.pending_meshes.clear();
    streamer.dirty_queue.clear();
    streamer.mesh_candidates_scratch.clear();
    streamer.load_cursor = 0;
    streamer.load_offsets_rd = -1;
    streamer.load_offsets.clear();
    streamer.frontier_complete = false;
    streamer.last_anchor_cxz = None;
    streamer.needs_orphan_scan = true;
    for (_, group) in streamer.entities.drain() {
        for entry in group {
            if let Some(entity_commands) = commands.get_entity(entry.entity) {
                entity_commands.despawn_recursive();
            }
            let _ = meshes.remove(&entry.handle);
        }
    }
    info!("World regenerated with seed {}", settings.seed);
}

fn handle_screenshot(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
    mut screenshots: ResMut<ScreenshotManager>,
    windows: Query<Entity, With<PrimaryWindow>>,
) {
    let triggered = keys.just_pressed(KeyCode::F2) || state.screenshot_requested;
    if !triggered {
        return;
    }
    state.screenshot_requested = false;
    let Ok(window) = windows.get_single() else {
        return;
    };
    let ts = crate::platform::now_epoch();
    let path = format!("screenshot_{ts}.png");
    match screenshots.save_screenshot_to_disk(window, &path) {
        Ok(_) => info!("Screenshot saved to {path}"),
        Err(e) => warn!("Screenshot failed: {e}"),
    }
}

fn rand_seed() -> u32 {
    let n = crate::platform::now_nanos_seed();
    (n as u32) ^ ((n >> 32) as u32) ^ 0x9E3779B1
}

// ---------------------------------------------------------------------------
// TEXTUREN tab --------------------------------------------------------------
// ---------------------------------------------------------------------------

fn draw_textures_tab(ui: &mut egui::Ui, library: &mut TextureLibrary) {
    // Lazy bake on first open — keeps startup instant. 25 swatches @
    // 128² at ~10 ms each = ~250 ms of wall time, but deferred until
    // the user actually navigates to this tab.
    if library.swatches.is_empty() {
        library.swatches = bake_all_block_swatches(128);
        library.handles = vec![None; library.swatches.len()];
        info!(
            "textures: baked {} photorealistic swatches (override dir: ./{})",
            library.swatches.len(),
            TEX_DIR
        );
    }
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();

    section_heading(ui, "TEXTURE TOOLBENCH");
    crate::ui_kit::surface_panel(ui, theme, |ui| {
        ui.horizontal_wrapped(|ui| {
            crate::ui_kit::status_chip(
                ui,
                Icon::Textures,
                "SOURCE",
                "procedural + optional PNG overrides",
                theme,
            );
            crate::ui_kit::status_chip(ui, Icon::Detail, "GRAIN", "FBM / Worley / sparkle", theme);
            crate::ui_kit::status_chip(ui, Icon::Open, "FOLDER", TEX_DIR, theme);
        });
    });
    ui.add_space(6.0);

    let status_text = library.last_status.clone();

    // Upload any missing egui textures lazily.
    let ctx = ui.ctx().clone();
    for i in 0..library.swatches.len() {
        if library.handles[i].is_some() {
            continue;
        }
        let sw = &library.swatches[i];
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [sw.width as usize, sw.height as usize],
            &sw.rgba,
        );
        library.handles[i] = Some(ctx.load_texture(
            format!("tex_{}", sw.name),
            img,
            egui::TextureOptions::LINEAR,
        ));
    }

    // Top-level bulk actions.
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::SaveAs, "Export PNGs", false, theme).clicked() {
            let mut saved = 0;
            let mut err: Option<String> = None;
            for sw in &library.swatches {
                match sw.save_png(TEX_DIR) {
                    Ok(_) => saved += 1,
                    Err(e) => err = Some(format!("{}: {e}", sw.name)),
                }
            }
            library.last_status = match err {
                Some(e) => format!("Export unvollstaendig ({saved} ok): {e}"),
                None => format!("{saved} Swatches nach ./{}/ exportiert.", TEX_DIR),
            };
        }
        ui.label(
            egui::RichText::new(format!("Ziel-Ordner: ./{}/", TEX_DIR))
                .size(11.5)
                .color(egui::Color32::from_gray(160)),
        );
    });
    if !status_text.is_empty() {
        ui.label(
            egui::RichText::new(&status_text)
                .size(11.5)
                .color(egui::Color32::from_rgb(120, 230, 255)),
        );
    }
    ui.add_space(8.0);

    section_heading(ui, "BLOCK-SWATCHES");
    let swatch_size = egui::vec2(84.0, 84.0);
    let per_row = ((ui.available_width() / (swatch_size.x + 18.0)).floor() as usize).max(1);
    egui::Grid::new("tex_grid")
        .num_columns(per_row)
        .spacing(egui::vec2(14.0, 14.0))
        .show(ui, |ui| {
            for (i, sw) in library.swatches.iter().enumerate() {
                if i > 0 && i % per_row == 0 {
                    ui.end_row();
                }
                ui.vertical(|ui| {
                    if let Some(h) = &library.handles[i] {
                        let resp =
                            ui.add(egui::Image::from_texture(h).fit_to_exact_size(swatch_size));
                        if resp.clicked() {
                            match sw.save_png(TEX_DIR) {
                                Ok(p) => {
                                    library.last_status = format!("Exportiert: {}", p.display())
                                }
                                Err(e) => {
                                    library.last_status = format!("Fehler bei {}: {e}", sw.name)
                                }
                            }
                        }
                    }
                    ui.label(
                        egui::RichText::new(sw.name)
                            .size(11.0)
                            .color(egui::Color32::from_gray(225)),
                    );
                });
            }
        });
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("Tipp: Klick auf eine Kachel exportiert nur diesen Block als PNG.")
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
    );
}

// ---------------------------------------------------------------------------
// BAUEN tab -----------------------------------------------------------------
// ---------------------------------------------------------------------------

fn draw_builder_tab(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    builder: &mut BuilderState,
    clipboard: &BuilderClipboard,
    history: &BuilderHistory,
    player_q: &mut Query<(&mut Transform, &mut Player)>,
) {
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    section_heading(ui, "BUILD TOOLBENCH");
    ui.horizontal_wrapped(|ui| {
        crate::ui_kit::setting_card(
            ui,
            Icon::Brush,
            "Brush",
            &format!(
                "{}x{}x{}",
                builder.brush.x, builder.brush.y, builder.brush.z
            ),
            true,
            theme,
        );
        crate::ui_kit::setting_card(
            ui,
            Icon::Undo,
            "Undo",
            &history.undo_len().to_string(),
            false,
            theme,
        );
        crate::ui_kit::setting_card(
            ui,
            Icon::Redo,
            "Redo",
            &history.redo_len().to_string(),
            false,
            theme,
        );
        crate::ui_kit::setting_card(
            ui,
            Icon::Cube,
            "Material",
            block_label(builder.block),
            true,
            theme,
        );
    });
    ui.add_space(8.0);

    section_heading(ui, "MATERIALIEN / TEXTUREN");
    crate::ui_kit::status_chip(ui, Icon::Cube, "Active", block_label(builder.block), theme);
    ui.add_space(4.0);
    for category in block_palette_catalog() {
        let open = category
            .entries
            .iter()
            .any(|entry| entry.block == builder.block);
        egui::CollapsingHeader::new(format!("{} - {}", category.label, category.hint))
            .default_open(open)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for entry in category.entries {
                        let selected = builder.block == entry.block;
                        if material_swatch_chip(ui, *entry, selected, theme).clicked() {
                            builder.block = entry.block;
                            builder.status = format!("Material: {} ({})", entry.label, entry.role);
                        }
                    }
                });
            });
    }
    ui.add_space(6.0);

    section_heading(ui, "PINSEL (Cuboid, jede Achse frei)");
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut builder.brush.x, 1..=32).text("X"));
        ui.add(egui::Slider::new(&mut builder.brush.y, 1..=32).text("Y"));
        ui.add(egui::Slider::new(&mut builder.brush.z, 1..=32).text("Z"));
    });
    ui.horizontal_wrapped(|ui| {
        for (label, s) in [
            ("1x1x1", IVec3::splat(1)),
            ("2x2x2", IVec3::splat(2)),
            ("4x4x4", IVec3::splat(4)),
            ("8x8x8", IVec3::splat(8)),
            ("Wand 8x4x1", IVec3::new(8, 4, 1)),
            ("Boden 8x1x8", IVec3::new(8, 1, 8)),
            ("Turm 4x16x4", IVec3::new(4, 16, 4)),
            ("Fenster 2x3x1", IVec3::new(2, 3, 1)),
            ("Fenster breit 4x2x1", IVec3::new(4, 2, 1)),
            ("Schlitz 4x1x1", IVec3::new(4, 1, 1)),
            ("Tuer 2x4x1", IVec3::new(2, 4, 1)),
        ] {
            if crate::ui_kit::tab_chip(ui, Icon::Brush, label, builder.brush == s, theme).clicked()
            {
                builder.brush = s;
            }
        }
    });
    ui.add_space(6.0);

    let player_ip = player_q
        .get_single()
        .ok()
        .map(|(t, _)| {
            IVec3::new(
                t.translation.x as i32,
                t.translation.y as i32,
                t.translation.z as i32,
            )
        })
        .unwrap_or(IVec3::ZERO);

    section_heading(ui, "PLATZIEREN");
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Pin, "At Player", false, theme).clicked() {
            // Anchor the brush one block in front of the player's feet so
            // we don't overwrite the block under them.
            let origin = player_ip + IVec3::new(1, 0, 0) - builder.brush / 2;
            builder.pending.push(BuildAction::PlaceBrush { origin });
        }
        if crate::ui_kit::icon_action(ui, Icon::Teleport, "Ahead", false, theme).clicked() {
            let origin = player_ip + IVec3::new(3, 0, 0) - builder.brush / 2;
            builder.pending.push(BuildAction::PlaceBrush { origin });
        }
        if crate::ui_kit::danger_action(ui, Icon::Eraser, "Remove", theme).clicked() {
            let origin = player_ip - builder.brush / 2;
            builder.pending.push(BuildAction::RemoveBrush { origin });
        }
    });
    ui.add_space(6.0);

    section_heading(ui, "SCHNELLBAU");
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Grid, "Platform", false, theme).clicked() {
            builder.pending.push(BuildAction::SmartPlatform);
        }
        if crate::ui_kit::icon_action(ui, Icon::Builder, "Shelter", false, theme).clicked() {
            builder.pending.push(BuildAction::SmartShelter);
        }
        if crate::ui_kit::icon_action(ui, Icon::Road, "Bridge", false, theme).clicked() {
            builder.pending.push(BuildAction::SmartBridge);
        }
        if crate::ui_kit::icon_action(ui, Icon::Move, "Ramp", false, theme).clicked() {
            builder.pending.push(BuildAction::SmartRamp);
        }
        if crate::ui_kit::icon_action(ui, Icon::Eraser, "Tunnel", false, theme).clicked() {
            builder.pending.push(BuildAction::SmartTunnel);
        }
    });
    ui.add_space(6.0);

    section_heading(ui, "BOX A -> B  (Fuellen / Loeschen / Kopieren)");
    ui.horizontal(|ui| {
        ui.label("A:");
        ui.add(
            egui::DragValue::new(&mut builder.a.x)
                .speed(1.0)
                .prefix("x "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.a.y)
                .speed(1.0)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.a.z)
                .speed(1.0)
                .prefix("z "),
        );
        if crate::ui_kit::icon_action(ui, Icon::Pin, "Use Player", false, theme).clicked() {
            builder.a = player_ip;
        }
    });
    ui.horizontal(|ui| {
        ui.label("B:");
        ui.add(
            egui::DragValue::new(&mut builder.b.x)
                .speed(1.0)
                .prefix("x "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.b.y)
                .speed(1.0)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.b.z)
                .speed(1.0)
                .prefix("z "),
        );
        if crate::ui_kit::icon_action(ui, Icon::Pin, "Use Player", false, theme).clicked() {
            builder.b = player_ip;
        }
    });
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::Brush, "Fill", false, theme).clicked() {
            builder.pending.push(BuildAction::FillBox);
        }
        if crate::ui_kit::icon_action(ui, Icon::Cube, "Hollow", false, theme).clicked() {
            builder.pending.push(BuildAction::HollowBox);
        }
        if crate::ui_kit::danger_action(ui, Icon::Eraser, "Clear", theme).clicked() {
            builder.pending.push(BuildAction::ClearBox);
        }
        if crate::ui_kit::icon_action(ui, Icon::Copy, "Copy", false, theme).clicked() {
            builder.pending.push(BuildAction::Copy);
        }
    });
    ui.add_space(6.0);

    section_heading(ui, "CLIPBOARD / EINFUEGEN");
    ui.label(format!(
        "Clipboard: {}x{}x{}  ({} Bloecke)   Undo {} / Redo {}",
        clipboard.size.x,
        clipboard.size.y,
        clipboard.size.z,
        clipboard.voxels.len(),
        history.undo_len(),
        history.redo_len()
    ));
    ui.horizontal(|ui| {
        ui.label("Paste-Ursprung:");
        ui.add(
            egui::DragValue::new(&mut builder.paste_origin.x)
                .speed(1.0)
                .prefix("x "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.paste_origin.y)
                .speed(1.0)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut builder.paste_origin.z)
                .speed(1.0)
                .prefix("z "),
        );
        if crate::ui_kit::icon_action(ui, Icon::Pin, "Use Player", false, theme).clicked() {
            builder.paste_origin = player_ip;
        }
    });
    ui.horizontal_wrapped(|ui| {
        // Pull theme from the global store (same trick section_heading uses)
        // so the icons pick up the user's phosphor colour without adding
        // another function parameter.
        let theme = ui
            .ctx()
            .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
            .unwrap_or_default();
        use crate::icons::{icon_button, Icon};
        if icon_button(ui, Icon::Undo, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::Undo);
        }
        if icon_button(ui, Icon::Redo, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::Redo);
        }
        if icon_button(ui, Icon::Paste, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::Paste);
        }
        if crate::ui_kit::icon_action(ui, Icon::Paste, "Paste Air", false, theme).clicked() {
            builder.pending.push(BuildAction::PasteIncludingAir);
        }
        // Clipboard transforms — all non-destructive, run on the
        // clipboard voxels before the next paste.
        if icon_button(ui, Icon::RotateY90, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::RotateClipboardY);
        }
        if icon_button(ui, Icon::FlipX, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::FlipClipboardX);
        }
        if icon_button(ui, Icon::FlipY, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::FlipClipboardY);
        }
        if icon_button(ui, Icon::FlipZ, 28.0, false, theme).clicked() {
            builder.pending.push(BuildAction::FlipClipboardZ);
        }
    });
    ui.add_space(6.0);

    section_heading(ui, "PREFAB (Gebaeude speichern / laden)");
    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.add(egui::TextEdit::singleline(&mut builder.prefab_name).desired_width(220.0));
    });
    ui.horizontal_wrapped(|ui| {
        if crate::ui_kit::icon_action(ui, Icon::SaveAs, "Save Prefab", false, theme).clicked() {
            builder.pending.push(BuildAction::SavePrefab);
        }
        if crate::ui_kit::icon_action(ui, Icon::Open, "Load Prefab", false, theme).clicked() {
            builder.pending.push(BuildAction::LoadPrefab);
        }
        ui.label(
            egui::RichText::new("Ordner ./prefabs/")
                .size(11.0)
                .color(egui::Color32::from_gray(160)),
        );
    });
    ui.add_space(4.0);

    // Prefab browser — live-scans ./prefabs/ and shows every .ron as a
    // clickable row. Click = set name + queue LoadPrefab so the user
    // can paste immediately. `state.prefab_save_name` is not used here
    // yet; retained for future "save under new name" quick-action.
    let prefabs = crate::builder::list_prefabs();
    ui.label(
        egui::RichText::new(format!("Bibliothek: {} Eintraege", prefabs.len()))
            .size(11.0)
            .color(egui::Color32::from_gray(180))
            .monospace(),
    );
    let _ = &state.prefab_save_name; // silence unused warn until refactor
    egui::ScrollArea::vertical()
        .max_height(140.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if prefabs.is_empty() {
                ui.label(
                    egui::RichText::new("(leer — speichere erst einen Prefab)")
                        .size(11.0)
                        .color(egui::Color32::from_gray(140))
                        .monospace(),
                );
            } else {
                for name in prefabs {
                    ui.horizontal(|ui| {
                        let selected = builder.prefab_name == name;
                        if ui
                            .add(term_button(
                                &format!(">> {:<24}", name),
                                selected,
                                crate::theme::ThemeSettings::default(),
                            ))
                            .clicked()
                        {
                            builder.prefab_name = name.clone();
                            builder.pending.push(BuildAction::LoadPrefab);
                        }
                    });
                }
            }
        });
    ui.add_space(6.0);

    ui.separator();
    ui.label(
        egui::RichText::new(format!("Status: {}", builder.status))
            .size(12.0)
            .color(egui::Color32::from_rgb(120, 230, 255)),
    );
    ui.label(
        egui::RichText::new(format!(
            "Spieler @ ({}, {}, {})",
            player_ip.x, player_ip.y, player_ip.z
        ))
        .size(11.0)
        .color(egui::Color32::from_gray(170)),
    );
}

// ---------------------------------------------------------------------------
// ANIM tab ------------------------------------------------------------------
// ---------------------------------------------------------------------------

fn draw_animation_tab(ui: &mut egui::Ui, studio: &mut AnimationStudio) {
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    section_heading(ui, "ANIMATION TOOLBENCH");
    ui.horizontal(|ui| {
        let mut picking = studio.picking;
        if crate::ui_kit::icon_action(ui, Icon::Pipette, "Picker", picking, theme).clicked() {
            picking = !picking;
            studio.picking = picking;
        }
        if crate::ui_kit::icon_action(ui, Icon::Delete, "Clear", false, theme).clicked() {
            studio.selection.clear();
        }
        crate::ui_kit::status_chip(
            ui,
            Icon::Cube,
            "SELECTION",
            &format!("{} blocks", studio.selection.len()),
            theme,
        );
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let can_capture = !studio.selection.is_empty();
        let capture = crate::ui_kit::icon_action(ui, Icon::Save, "Capture", false, theme);
        if can_capture && capture.clicked() {
            studio.pending_capture = true;
        }
    });

    ui.add_space(8.0);
    section_heading(ui, "CLIPS");
    if studio.clips.is_empty() {
        ui.label(
            egui::RichText::new("Keine Clips. Erfasse erst eine Auswahl.")
                .size(12.0)
                .color(egui::Color32::from_gray(180)),
        );
    } else {
        let mut active = studio.active.unwrap_or(0).min(studio.clips.len() - 1);
        ui.horizontal_wrapped(|ui| {
            for (i, c) in studio.clips.iter().enumerate() {
                let label = format!("{} ({}b)", c.name, c.blocks.len());
                if crate::ui_kit::tab_chip(ui, Icon::Clip, &label, i == active, theme).clicked() {
                    active = i;
                }
            }
        });
        studio.active = Some(active);
    }

    if let Some(idx) = studio.active {
        if idx < studio.clips.len() {
            ui.add_space(6.0);
            section_heading(ui, "WIEDERGABE");
            let clip = &mut studio.clips[idx];
            ui.horizontal_wrapped(|ui| {
                if crate::ui_kit::icon_action(ui, Icon::Play, "Play", clip.playing, theme).clicked()
                {
                    clip.playing = !clip.playing;
                }
                if crate::ui_kit::icon_action(ui, Icon::Loop, "Loop", clip.looping, theme).clicked()
                {
                    clip.looping = !clip.looping;
                }
                ui.add(egui::Slider::new(&mut clip.speed, 0.0..=4.0).text("Tempo"));
            });
            ui.horizontal(|ui| {
                ui.label(format!("t = {:.2}s / {:.2}s", clip.t, clip_duration(clip)));
                if crate::ui_kit::icon_action(ui, Icon::Resume, "Restart", false, theme).clicked() {
                    clip.t = 0.0;
                }
            });

            ui.add_space(6.0);
            section_heading(ui, "KEYFRAMES");
            let mut remove: Option<usize> = None;
            let n_keys = clip.keys.len();
            for (i, k) in clip.keys.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("#{i}"));
                    ui.add(
                        egui::DragValue::new(&mut k.time)
                            .speed(0.05)
                            .range(0.0..=120.0)
                            .prefix("t "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut k.offset.x)
                            .speed(0.1)
                            .prefix("x "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut k.offset.y)
                            .speed(0.1)
                            .prefix("y "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut k.offset.z)
                            .speed(0.1)
                            .prefix("z "),
                    );
                    ui.add(
                        egui::DragValue::new(&mut k.yaw_deg)
                            .speed(2.0)
                            .suffix(" deg"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut k.scale)
                            .speed(0.02)
                            .range(0.05..=8.0)
                            .prefix("s "),
                    );
                    // Per-key outgoing easing (used to reach key i+1).
                    // The last key has nothing to "leave to", so we
                    // show the combo but label it as informational.
                    let tail = i + 1 >= n_keys;
                    let id = egui::Id::new(("interp_combo", i));
                    egui::ComboBox::from_id_source(id)
                        .selected_text(if tail {
                            format!("→ {} (Ende)", k.interp.label())
                        } else {
                            format!("→ {}", k.interp.label())
                        })
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for mode in Interp::all() {
                                ui.selectable_value(&mut k.interp, mode, mode.label());
                            }
                        });
                    if ui.small_button("X").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                if clip.keys.len() > 1 {
                    clip.keys.remove(i);
                }
            }
            if crate::ui_kit::icon_action(ui, Icon::Key, "Keyframe", false, theme).clicked() {
                let last = clip.keys.last().copied().unwrap_or(KeyFrame::identity(0.0));
                clip.keys.push(KeyFrame {
                    time: last.time + 1.0,
                    ..last
                });
            }
            // Keep keys time-sorted so the linear interpolator sees a
            // monotonic timeline.
            clip.keys.sort_by(|a, b| {
                a.time
                    .partial_cmp(&b.time)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            // --- Ease-Presets (NI) ---------------------------------
            // One-click row that retags the outgoing interp on every
            // key at once. Lets the user stage an entire motion in
            // "bouncy" or "ease-out" mood without editing each row.
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Presets →")
                        .monospace()
                        .color(crate::theme::TEXT),
                );
                for mode in Interp::all() {
                    if crate::ui_kit::tab_chip(ui, Icon::Animation, mode.label(), false, theme)
                        .clicked()
                    {
                        for k in clip.keys.iter_mut() {
                            k.interp = mode;
                        }
                    }
                }
            });

            ui.add_space(6.0);
            section_heading(ui, "AKTIONEN");
            ui.horizontal_wrapped(|ui| {
                if crate::ui_kit::icon_action(ui, Icon::SaveAs, "Bake Pose", false, theme).clicked()
                {
                    studio.pending_bake = Some(idx);
                }
                if crate::ui_kit::icon_action(ui, Icon::Undo, "Restore", false, theme).clicked() {
                    studio.pending_restore = Some(idx);
                }
                if crate::ui_kit::danger_action(ui, Icon::Delete, "Discard", theme).clicked() {
                    studio.pending_discard = Some(idx);
                }
            });
        }
    }

    ui.add_space(8.0);
    ui.separator();
    ui.label(
        egui::RichText::new(format!("Status: {}", studio.status))
            .size(12.0)
            .color(egui::Color32::from_rgb(120, 230, 255)),
    );
}

fn clip_duration(clip: &crate::animation::AnimClip) -> f32 {
    clip.keys
        .iter()
        .map(|k| k.time)
        .fold(0.0_f32, f32::max)
        .max(0.001)
}

// ---------------------------------------------------------------------
// STADT (city) tab
// ---------------------------------------------------------------------

fn draw_city_tab(ui: &mut egui::Ui, city: &mut crate::city::CityState) {
    use crate::city::{BuildingStyle, CityTool, DistrictKind, RoadStyle, SnapMode};
    let theme = ui
        .ctx()
        .data(|d| d.get_temp::<crate::theme::ThemeSettings>(egui::Id::new("hacker_theme")))
        .unwrap_or_default();
    section_heading(ui, "CITY TOOLBENCH");
    ui.horizontal_wrapped(|ui| {
        crate::ui_kit::status_chip(ui, Icon::Road, "LMB", "place", theme);
        crate::ui_kit::status_chip(ui, Icon::Eraser, "RMB", "delete", theme);
        crate::ui_kit::status_chip(ui, Icon::Snap, "SNAP", city.snap.label(), theme);
        crate::ui_kit::status_chip(ui, Icon::Grid, "SIZE", "[ / ]", theme);
    });
    ui.add_space(6.0);

    // --- Active tool --------------------------------------------------
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("Werkzeug:")
                .monospace()
                .color(crate::theme::TEXT),
        );
        for (label, tool) in [
            ("AUS", CityTool::None),
            ("STRASSE (N)", CityTool::Road),
            ("BEZIRK (T)", CityTool::District),
            ("GEBAEUDE (U)", CityTool::Building),
            ("FASSADE (F)", CityTool::Facade),
        ] {
            let icon = match tool {
                CityTool::None => Icon::Close,
                CityTool::Road => Icon::Road,
                CityTool::District => Icon::District,
                CityTool::Building => Icon::City,
                CityTool::Facade => Icon::Grid,
            };
            if crate::ui_kit::tab_chip(ui, icon, label, city.tool == tool, theme).clicked() {
                city.tool = tool;
                city.pending_road_a = None;
                city.pending_building_a = None;
            }
        }
    });
    ui.add_space(4.0);

    // --- Snap ---------------------------------------------------------
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Snap:")
                .monospace()
                .color(crate::theme::TEXT),
        );
        for mode in [
            SnapMode::Off,
            SnapMode::Grid1,
            SnapMode::Grid4,
            SnapMode::Grid16,
            SnapMode::Road,
        ] {
            if crate::ui_kit::tab_chip(ui, Icon::Snap, mode.label(), city.snap == mode, theme)
                .clicked()
            {
                city.snap = mode;
            }
        }
    });
    ui.add_space(6.0);

    // --- Road settings -----------------------------------------------
    ui.group(|ui| {
        ui.label(
            egui::RichText::new("STRASSE")
                .monospace()
                .strong()
                .color(crate::theme::TEXT),
        );
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Stil:").monospace());
            for s in RoadStyle::all() {
                if crate::ui_kit::tab_chip(ui, Icon::Road, s.label(), city.road_style == s, theme)
                    .clicked()
                {
                    city.road_style = s;
                }
            }
        });
        let mut w = city.road_width as i32;
        ui.add(egui::Slider::new(&mut w, 1..=9).text("Breite"));
        city.road_width = w.clamp(1, 9) as u8;
        ui.label(
            egui::RichText::new(format!("Plaziert: {} Komponenten", city.roads.len()))
                .monospace()
                .color(crate::theme::TEXT),
        );
        if crate::ui_kit::danger_action(ui, Icon::Delete, "Forget Roads", theme).clicked() {
            city.roads.clear();
        }
    });
    ui.add_space(4.0);

    // --- District settings -------------------------------------------
    ui.group(|ui| {
        ui.label(
            egui::RichText::new("BEZIRK")
                .monospace()
                .strong()
                .color(crate::theme::TEXT),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Art:").monospace());
            for k in DistrictKind::all() {
                if crate::ui_kit::tab_chip(
                    ui,
                    Icon::District,
                    k.label(),
                    city.district_kind == k,
                    theme,
                )
                .clicked()
                {
                    city.district_kind = k;
                }
            }
        });
        ui.add(egui::Slider::new(&mut city.district_radius, 2..=24).text("Radius"));
        ui.label(
            egui::RichText::new(format!("Plaziert: {} Bezirke", city.districts.len()))
                .monospace()
                .color(crate::theme::TEXT),
        );
        if crate::ui_kit::danger_action(ui, Icon::Delete, "Clear Districts", theme).clicked() {
            city.districts.clear();
        }
    });
    ui.add_space(4.0);

    // --- Building settings -------------------------------------------
    ui.group(|ui| {
        ui.label(
            egui::RichText::new("GEBAEUDE")
                .monospace()
                .strong()
                .color(crate::theme::TEXT),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Stil:").monospace());
            for s in BuildingStyle::all() {
                if crate::ui_kit::tab_chip(
                    ui,
                    Icon::City,
                    s.label(),
                    city.building_style == s,
                    theme,
                )
                .clicked()
                {
                    city.building_style = s;
                    let (lo, hi) = s.default_floors();
                    city.building_floors = city.building_floors.clamp(lo, hi).max(2);
                }
            }
        });
        let mut f = city.building_floors as i32;
        ui.add(egui::Slider::new(&mut f, 2..=20).text("Etagen (je 3 Bloecke)"));
        city.building_floors = f.clamp(2, 20) as u8;
        ui.label(
            egui::RichText::new(format!("Plaziert: {} Gebaeude", city.buildings.len()))
                .monospace()
                .color(crate::theme::TEXT),
        );
        ui.label(
            egui::RichText::new(
                "Gebaeude sind solide Shells. Fenster/Oeffnungen per Toolbelt CUT oder BAUEN -> Leeren selbst schneiden.",
            )
            .size(11.0)
            .color(AMBER),
        );
        if crate::ui_kit::danger_action(ui, Icon::Delete, "Forget Buildings", theme).clicked() {
            city.buildings.clear();
        }
    });
    ui.add_space(4.0);

    // --- Facade library ----------------------------------------------
    ui.group(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "FASSADEN-BIBLIOTHEK ({} Eintraege)",
                city.facades.len()
            ))
            .monospace()
            .strong()
            .color(crate::theme::TEXT),
        );
        if city.facades.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Keine Fassaden geladen. Lege .ron Dateien in ./facades/ ab \
                     oder der Built-in-Satz wird beim Start geladen.",
                )
                .size(11.5)
                .color(egui::Color32::from_gray(170)),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .show(ui, |ui| {
                    let n = city.facades.len();
                    for i in 0..n {
                        let (name, cat, size) = {
                            let f = &city.facades[i];
                            (f.name.clone(), f.category.clone(), f.size)
                        };
                        let label = format!(
                            "{:>2}. {:<14} [{:<6}]  {}x{}x{}",
                            i, name, cat, size.x, size.y, size.z
                        );
                        if crate::ui_kit::tab_chip(
                            ui,
                            Icon::Grid,
                            &label,
                            city.facade_selected == i,
                            theme,
                        )
                        .clicked()
                        {
                            city.facade_selected = i;
                            city.tool = CityTool::Facade;
                        }
                    }
                });
        }
    });
    ui.add_space(6.0);

    // --- Status bar ---------------------------------------------------
    ui.separator();
    ui.label(
        egui::RichText::new(format!("Status: {}", city.status))
            .size(12.0)
            .color(egui::Color32::from_rgb(120, 230, 255)),
    );
}
