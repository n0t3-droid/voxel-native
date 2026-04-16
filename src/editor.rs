//! Futuristic in-game editor (F3 to toggle). Tabbed cyberpunk-styled
//! egui panel with smooth open/close animation, ESC + click-outside close.
//!
//! Sections: WELT / GRAFIK / WETTER / ZEIT / SPIELER / SYSTEM.

use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPlugin};

use crate::player::Player;
use crate::settings::{GraphicsMode, TimeMode, WeatherPreset, WorldSettings};
use crate::world::{ChunkStreamer, VoxelWorld};

#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum EditorTab {
    #[default]
    World,
    Graphics,
    Weather,
    Time,
    Player,
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
            EditorTab::System => "SYSTEM",
        }
    }
    fn all() -> [EditorTab; 6] {
        [
            EditorTab::World,
            EditorTab::Graphics,
            EditorTab::Weather,
            EditorTab::Time,
            EditorTab::Player,
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
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin)
            .insert_resource(EditorState::default())
            .add_systems(Startup, style_egui)
            .add_systems(
                Update,
                (
                    toggle_editor,
                    draw_editor,
                    handle_regen,
                    handle_screenshot,
                )
                    .chain(),
            );
    }
}

/// Set up a dark neon egui theme once at startup.
fn style_egui(mut contexts: EguiContexts) {
    let ctx = contexts.ctx_mut();
    let mut visuals = egui::Visuals::dark();
    let bg = egui::Color32::from_rgba_premultiplied(8, 12, 22, 235);
    let panel = egui::Color32::from_rgba_premultiplied(14, 20, 34, 240);
    let cyan = egui::Color32::from_rgb(0, 230, 255);
    let cyan_dim = egui::Color32::from_rgb(0, 140, 180);
    let accent = egui::Color32::from_rgb(255, 75, 155);

    visuals.window_fill = bg;
    visuals.panel_fill = panel;
    visuals.window_stroke = egui::Stroke::new(1.2, cyan_dim);
    visuals.window_rounding = egui::Rounding::same(12.0);
    visuals.widgets.noninteractive.bg_fill = panel;
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_gray(210));
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgba_premultiplied(22, 30, 48, 255);
    visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_gray(230));
    visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgba_premultiplied(40, 70, 110, 255);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, cyan);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.2, cyan);
    visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgba_premultiplied(0, 120, 160, 255);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.2, accent);
    visuals.widgets.active.rounding = egui::Rounding::same(6.0);
    visuals.selection.bg_fill = cyan_dim.linear_multiply(0.45);
    visuals.selection.stroke = egui::Stroke::new(1.2, cyan);
    visuals.hyperlink_color = cyan;
    visuals.override_text_color = Some(egui::Color32::from_gray(235));

    ctx.set_visuals(visuals);

    let mut style: egui::Style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.slider_width = 220.0;
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size *= 1.05;
    }
    ctx.set_style(style);
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

fn draw_editor(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    mut settings: ResMut<WorldSettings>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
    diagnostics: Res<bevy::diagnostic::DiagnosticsStore>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
) {
    let ctx = contexts.ctx_mut();

    let anim = ctx.animate_bool_with_time(egui::Id::new("editor_open"), state.open, 0.18);
    if anim <= 0.001 {
        return;
    }
    let eased = 1.0 - (1.0 - anim).powi(3);

    let screen_rect = ctx.screen_rect();
    let panel_w = 560.0_f32.min(screen_rect.width() - 40.0);
    let panel_h = 520.0_f32.min(screen_rect.height() - 60.0);
    let center = screen_rect.center();
    let target_pos = egui::pos2(center.x - panel_w * 0.5, center.y - panel_h * 0.5);
    let slide_y = (1.0 - eased) * 22.0;
    let pos = egui::pos2(target_pos.x, target_pos.y + slide_y);
    let alpha = (eased * 255.0) as u8;

    // Dim the background behind the panel.
    let bg_layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("editor_dim"));
    ctx.layer_painter(bg_layer).rect_filled(
        screen_rect,
        0.0,
        egui::Color32::from_black_alpha((eased * 120.0) as u8),
    );

    let mut frame = egui::Frame::window(&ctx.style());
    frame.fill = frame.fill.linear_multiply(alpha as f32 / 255.0);

    let response = egui::Window::new("voxel_native_editor")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .frame(frame)
        .fixed_pos(pos)
        .fixed_size(egui::vec2(panel_w, panel_h))
        .show(ctx, |ui| {
            draw_header(ui, &mut state);
            ui.add_space(4.0);
            draw_tab_bar(ui, &mut state);
            ui.add_space(10.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match state.tab {
                    EditorTab::World => draw_world_tab(ui, &mut state, &mut settings),
                    EditorTab::Graphics => draw_graphics_tab(ui, &mut settings),
                    EditorTab::Weather => draw_weather_tab(ui, &mut settings),
                    EditorTab::Time => draw_time_tab(ui, &mut settings),
                    EditorTab::Player => draw_player_tab(ui, &mut player_q),
                    EditorTab::System => draw_system_tab(
                        ui,
                        &mut state,
                        &mut settings,
                        &diagnostics,
                        &world,
                        &streamer,
                    ),
                });
            ui.add_space(6.0);
            draw_footer(ui, &mut state, &mut settings);
        });

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

fn draw_header(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(">> VOXEL-NATIVE CONTROL")
                .size(20.0)
                .color(egui::Color32::from_rgb(0, 230, 255))
                .strong(),
        );
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("// F3 toggle  //  ESC close")
                .size(12.0)
                .color(egui::Color32::from_gray(150)),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = egui::Button::new(
                egui::RichText::new(" X ").color(egui::Color32::from_rgb(255, 120, 160)),
            )
            .fill(egui::Color32::from_rgba_premultiplied(40, 20, 30, 255));
            if ui.add(btn).clicked() {
                state.open = false;
            }
        });
    });
    ui.separator();
}

fn draw_tab_bar(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.horizontal_wrapped(|ui| {
        for tab in EditorTab::all() {
            let selected = state.tab == tab;
            let text = egui::RichText::new(tab.label())
                .size(14.0)
                .color(if selected {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(210)
                });
            let fill = if selected {
                egui::Color32::from_rgb(0, 120, 160)
            } else {
                egui::Color32::from_rgba_premultiplied(22, 30, 48, 255)
            };
            let stroke = egui::Stroke::new(
                1.0,
                if selected {
                    egui::Color32::from_rgb(0, 230, 255)
                } else {
                    egui::Color32::from_rgb(40, 60, 80)
                },
            );
            let btn = egui::Button::new(text)
                .fill(fill)
                .stroke(stroke)
                .rounding(egui::Rounding::same(8.0));
            if ui.add(btn).clicked() {
                state.tab = tab;
            }
        }
    });
}

fn section_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(text)
            .size(13.0)
            .color(egui::Color32::from_rgb(0, 230, 255))
            .strong(),
    );
    ui.separator();
}

fn neon_button(text: &str, selected: bool) -> egui::Button {
    let color = if selected {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_gray(220)
    };
    egui::Button::new(egui::RichText::new(text).color(color))
        .fill(if selected {
            egui::Color32::from_rgb(0, 120, 160)
        } else {
            egui::Color32::from_rgba_premultiplied(22, 30, 48, 255)
        })
        .rounding(egui::Rounding::same(8.0))
}

fn draw_world_tab(ui: &mut egui::Ui, state: &mut EditorState, settings: &mut WorldSettings) {
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
        if ui.button("Zufall").clicked() {
            state.pending_seed = Some(rand_seed());
        }
        if ui.button(">> Neu generieren").clicked() {
            if let Some(s) = state.pending_seed.take() {
                settings.seed = s;
            }
            state.regen_requested = true;
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "STREAMING");
    ui.add(egui::Slider::new(&mut settings.render_distance, 2..=20).text("Render-Distanz (Chunks)"));
    ui.add(egui::Slider::new(&mut settings.vertical_chunks, 4..=16).text("Vertikale Chunks"));
    ui.add(egui::Slider::new(&mut settings.chunks_per_frame, 1..=20).text("Chunks / Frame"));
    ui.add(egui::Slider::new(&mut settings.meshes_per_frame, 1..=20).text("Meshes / Frame"));
}

fn draw_graphics_tab(ui: &mut egui::Ui, settings: &mut WorldSettings) {
    section_heading(ui, "PRESET");
    ui.horizontal(|ui| {
        for (mode, label) in [
            (GraphicsMode::Fast, "[>] Fast"),
            (GraphicsMode::Balanced, "[=] Balanced"),
            (GraphicsMode::High, "[*] High"),
        ] {
            let selected = settings.graphics == mode;
            if ui.add(neon_button(label, selected)).clicked() {
                settings.graphics = mode;
            }
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "SICHTFELD");
    ui.add(egui::Slider::new(&mut settings.fov_deg, 50.0..=110.0).text("FOV (Grad)"));
    ui.label(
        egui::RichText::new("Sprint-Kick wird automatisch oben draufgepackt.")
            .size(11.0)
            .color(egui::Color32::from_gray(160)),
    );
}

fn draw_weather_tab(ui: &mut egui::Ui, settings: &mut WorldSettings) {
    section_heading(ui, "PRESET");
    let mut preset = settings.weather.preset;
    ui.horizontal_wrapped(|ui| {
        for (p, label) in [
            (WeatherPreset::Clear, "Klar"),
            (WeatherPreset::LightRain, "Regen"),
            (WeatherPreset::Storm, "Sturm"),
            (WeatherPreset::Snow, "Schnee"),
            (WeatherPreset::Fog, "Nebel"),
            (WeatherPreset::Custom, "Custom"),
        ] {
            let selected = preset == p;
            if ui.add(neon_button(label, selected)).clicked() {
                preset = p;
                settings.weather.apply_preset(p);
            }
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "FEINTUNING");
    ui.add(egui::Slider::new(&mut settings.weather.rain_intensity, 0.0..=1.0).text("Regen"));
    ui.add(egui::Slider::new(&mut settings.weather.snow_intensity, 0.0..=1.0).text("Schnee"));
    ui.add(egui::Slider::new(&mut settings.weather.fog_density, 0.0..=1.0).text("Nebel"));
    ui.add(egui::Slider::new(&mut settings.weather.wind_x, -10.0..=10.0).text("Wind X"));
    ui.add(egui::Slider::new(&mut settings.weather.wind_z, -10.0..=10.0).text("Wind Z"));
}

fn draw_time_tab(ui: &mut egui::Ui, settings: &mut WorldSettings) {
    section_heading(ui, "MODUS");
    ui.horizontal(|ui| {
        for (mode, label) in [(TimeMode::Cycle, "Zyklus"), (TimeMode::Fixed, "Fest")] {
            let selected = settings.time_mode == mode;
            if ui.add(neon_button(label, selected)).clicked() {
                settings.time_mode = mode;
            }
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "UHRZEIT");
    ui.add(
        egui::Slider::new(&mut settings.time_of_day, 0.0..=24.0)
            .text("Stunde")
            .fixed_decimals(2),
    );
    ui.add(egui::Slider::new(&mut settings.cycle_speed, 0.0..=1.0).text("Zyklus-Tempo (min/s)"));
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for (label, t) in [
            ("Morgen 06:00", 6.0),
            ("Mittag 12:00", 12.0),
            ("Sunset 19:00", 19.0),
            ("Nacht 23:00", 23.0),
        ] {
            if ui.add(neon_button(label, false)).clicked() {
                settings.time_of_day = t;
            }
        }
    });
}

fn draw_player_tab(ui: &mut egui::Ui, player_q: &mut Query<(&mut Transform, &mut Player)>) {
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
        if ui.add(neon_button("Teleport -> Origin", false)).clicked() {
            tf.translation = Vec3::new(0.0, 120.0, 0.0);
            player.velocity = Vec3::ZERO;
            player.placed_on_surface = false;
        }
        if ui.add(neon_button("Y = 200", false)).clicked() {
            tf.translation.y = 200.0;
            player.velocity = Vec3::ZERO;
        }
    });
    ui.add_space(6.0);
    section_heading(ui, "VERHALTEN");
    ui.checkbox(&mut player.flying, "Fliegen (F)");
    ui.add(egui::Slider::new(&mut player.walk_speed, 1.0..=20.0).text("Gehtempo"));
    ui.add(egui::Slider::new(&mut player.fly_speed, 4.0..=80.0).text("Flugtempo"));
    ui.add(
        egui::Slider::new(&mut player.sensitivity, 0.0005..=0.01).text("Maus-Sensitivitaet"),
    );
}

fn draw_system_tab(
    ui: &mut egui::Ui,
    state: &mut EditorState,
    settings: &mut WorldSettings,
    diagnostics: &bevy::diagnostic::DiagnosticsStore,
    world: &VoxelWorld,
    streamer: &ChunkStreamer,
) {
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

    ui.add_space(6.0);
    section_heading(ui, "SPEICHERN");
    ui.horizontal(|ui| {
        if ui.add(neon_button("Speichern (F5)", false)).clicked() {
            settings.save();
        }
        if ui.add(neon_button("Screenshot (F2)", false)).clicked() {
            state.screenshot_requested = true;
        }
    });

    ui.add_space(6.0);
    section_heading(ui, "HINWEISE");
    ui.label(
        egui::RichText::new(
            "WASD bewegen  //  Space springen  //  Ctrl Sprint  //  F Fliegen  //  1-9 Hotbar",
        )
        .size(12.0)
        .color(egui::Color32::from_gray(190)),
    );
    ui.label(
        egui::RichText::new("F3 Editor  //  F2 Screenshot  //  F5 Speichern  //  ESC zu")
            .size(12.0)
            .color(egui::Color32::from_gray(190)),
    );
}

fn draw_footer(ui: &mut egui::Ui, state: &mut EditorState, settings: &mut WorldSettings) {
    ui.separator();
    ui.horizontal(|ui| {
        if ui.add(neon_button("Speichern", false)).clicked() {
            settings.save();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = egui::Button::new(
                egui::RichText::new("X  Schliessen").color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_rgb(180, 30, 80))
            .rounding(egui::Rounding::same(8.0));
            if ui.add(btn).clicked() {
                state.open = false;
            }
        });
    });
}

fn handle_regen(
    mut state: ResMut<EditorState>,
    mut world: ResMut<VoxelWorld>,
    mut streamer: ResMut<ChunkStreamer>,
    settings: Res<WorldSettings>,
    mut commands: Commands,
) {
    if !state.regen_requested {
        return;
    }
    state.regen_requested = false;

    world.generator = crate::terrain::TerrainGenerator::new(settings.seed);
    world.chunks.clear();
    for (_, entity) in streamer.entities.drain() {
        commands.entity(entity).despawn_recursive();
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
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = format!("screenshot_{ts}.png");
    match screenshots.save_screenshot_to_disk(window, &path) {
        Ok(_) => info!("Screenshot saved to {path}"),
        Err(e) => warn!("Screenshot failed: {e}"),
    }
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    (n as u32) ^ ((n >> 32) as u32) ^ 0x9E3779B1
}
