//! On-screen HUD: crosshair, FPS/position/time/biome overlay, hotbar,
//! startup hint banner that fades once the cursor is captured.
//!
//! Port target: `components/Hotbar.tsx`, `components/InfoOverlay.tsx` and
//! the corner text in `components/VoxelEngine.tsx`.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};

use crate::chunk::to_i32_safe;
use crate::director::SimulationDirector;
use crate::director::UnifiedTelemetry;
use crate::icons::{paint_icon, Icon};
use crate::player::{Player, SuitVitals};
use crate::settings::{HudProfile, WorldSettings};
use crate::terrain::Biome;
use crate::toolbelt::ToolbeltTool;
use crate::weapons::DestructionStats;
use crate::world::{StreamingGovernor, VoxelWorld};

pub struct HudPlugin;

/// Tracks whether the F3 debug overlay (FPS + pos + biome + time) is shown.
#[derive(Resource)]
pub struct DebugOverlay {
    pub visible: bool,
}

impl Default for DebugOverlay {
    fn default() -> Self {
        Self { visible: false }
    }
}

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin)
            .insert_resource(HotbarState::default())
            .insert_resource(DebugOverlay::default())
            .add_systems(
                Startup,
                (
                    spawn_crosshair,
                    spawn_stats_text,
                    spawn_hint,
                    spawn_hotbar,
                    spawn_scope_overlay,
                    spawn_combo_text,
                ),
            )
            .add_systems(
                Update,
                (
                    toggle_debug_overlay,
                    update_stats_text,
                    draw_neon_combat_hud,
                    draw_workflow_rail,
                    update_hint,
                    hotbar_input.run_if(in_state(crate::menu::GameState::InGame)),
                    hotbar_highlight,
                    toggle_hud_visibility,
                    update_scope_overlay,
                    update_combo_text,
                    flash_crosshair_on_hit,
                ),
            );
    }
}

/// Shift+F3 toggles the debug stats overlay. Plain F3 is reserved for
/// build/edit mode in `toolbelt.rs`.
fn toggle_debug_overlay(
    keys: Res<ButtonInput<KeyCode>>,
    mut overlay: ResMut<DebugOverlay>,
    state: Res<State<crate::menu::GameState>>,
) {
    if *state.get() != crate::menu::GameState::InGame {
        return;
    }
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if shift && keys.just_pressed(KeyCode::F3) {
        overlay.visible = !overlay.visible;
    }
}

/// Hide the crosshair, stats text, hotbar and hint banner whenever we're
/// not actively playing (in a menu or paused).
fn toggle_hud_visibility(
    state: Res<State<crate::menu::GameState>>,
    overlay: Res<DebugOverlay>,
    mode: Option<Res<crate::mode::ModeContext>>,
    film: Option<Res<crate::film::FilmRuntime>>,
    mut crosshair_q: Query<
        &mut Visibility,
        (
            With<Crosshair>,
            Without<StatsText>,
            Without<HintBanner>,
            Without<HotbarSlot>,
        ),
    >,
    mut stats_q: Query<
        &mut Visibility,
        (
            With<StatsText>,
            Without<Crosshair>,
            Without<HintBanner>,
            Without<HotbarSlot>,
        ),
    >,
    mut slot_q: Query<
        &mut Visibility,
        (
            With<HotbarSlot>,
            Without<HotbarRoot>,
            Without<Crosshair>,
            Without<StatsText>,
            Without<HintBanner>,
        ),
    >,
    mut hotbar_root_q: Query<
        &mut Visibility,
        (
            With<HotbarRoot>,
            Without<HotbarSlot>,
            Without<Crosshair>,
            Without<StatsText>,
            Without<HintBanner>,
        ),
    >,
) {
    let in_game = *state.get() == crate::menu::GameState::InGame;
    let film_hide = film.as_ref().map(|f| f.hide_hud).unwrap_or(false);
    let build_mode = mode.as_deref().map(|m| m.is_build()).unwrap_or(false);
    let ship_mode = mode.as_deref().map(|m| m.is_ship()).unwrap_or(false);
    let build_picker = mode
        .as_deref()
        .map(|m| m.is_build_picker())
        .unwrap_or(false);
    let stats_vis = if in_game && overlay.visible && !film_hide {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let crosshair_vis = if in_game && !build_picker && !film_hide {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let hotbar_vis = if in_game && !build_mode && !ship_mode && !film_hide {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if let Ok(mut v) = crosshair_q.get_single_mut() {
        *v = crosshair_vis;
    }
    if let Ok(mut v) = stats_q.get_single_mut() {
        *v = stats_vis;
    }
    for mut v in slot_q.iter_mut() {
        *v = hotbar_vis;
    }
    if let Ok(mut v) = hotbar_root_q.get_single_mut() {
        *v = hotbar_vis;
    }
}

// ------------------------------- Crosshair --------------------------------

#[derive(Component)]
pub struct Crosshair;

fn spawn_crosshair(mut commands: Commands) {
    // Holographic reticle: 4 short phosphor bars around a central dot, with
    // a tiny gap in the middle so it never occludes what you aim at.
    // Matches the "HUD visor" aesthetic of sci-fi shooters.
    let cyan = Color::srgba(0.20, 1.0, 0.62, 0.96);
    let cyan_dim = Color::srgba(0.20, 1.0, 0.62, 0.50);
    commands
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    position_type: PositionType::Absolute,
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                ..default()
            },
            Crosshair,
        ))
        .with_children(|p| {
            // Top tick
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(2.0),
                    height: Val::Px(7.0),
                    position_type: PositionType::Absolute,
                    top: Val::Percent(50.0),
                    margin: UiRect {
                        top: Val::Px(-15.0),
                        ..default()
                    },
                    ..default()
                },
                background_color: BackgroundColor(cyan),
                ..default()
            });
            // Bottom tick
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(2.0),
                    height: Val::Px(7.0),
                    position_type: PositionType::Absolute,
                    top: Val::Percent(50.0),
                    margin: UiRect {
                        top: Val::Px(8.0),
                        ..default()
                    },
                    ..default()
                },
                background_color: BackgroundColor(cyan),
                ..default()
            });
            // Left tick
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(7.0),
                    height: Val::Px(2.0),
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    margin: UiRect {
                        left: Val::Px(-15.0),
                        ..default()
                    },
                    ..default()
                },
                background_color: BackgroundColor(cyan),
                ..default()
            });
            // Right tick
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(7.0),
                    height: Val::Px(2.0),
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    margin: UiRect {
                        left: Val::Px(8.0),
                        ..default()
                    },
                    ..default()
                },
                background_color: BackgroundColor(cyan),
                ..default()
            });
            // Center dot
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(2.0),
                    height: Val::Px(2.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                background_color: BackgroundColor(cyan_dim),
                ..default()
            });
        });
}

// --------------------------- Stats text (top-left) ------------------------

#[derive(Component)]
pub struct StatsText;

fn spawn_stats_text(mut commands: Commands) {
    commands.spawn((
        TextBundle::from_section(
            "",
            TextStyle {
                font_size: 15.0,
                color: Color::srgba(0.74, 1.0, 0.82, 0.98),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Px(12.0),
            left: Val::Px(14.0),
            padding: UiRect::all(Val::Px(10.0)),
            ..default()
        })
        .with_background_color(Color::srgba(0.0, 0.02, 0.015, 0.68)),
        StatsText,
    ));
}

fn update_stats_text(
    diagnostics: Res<DiagnosticsStore>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    governor: Res<StreamingGovernor>,
    player_q: Query<(&Transform, &Player)>,
    pause: Option<Res<crate::editor::SimPause>>,
    director: Option<Res<SimulationDirector>>,
    mut text_q: Query<&mut Text, With<StatsText>>,
) {
    let Ok(mut text) = text_q.get_single_mut() else {
        return;
    };
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);

    let (pos, flying) = if let Ok((tf, player)) = player_q.get_single() {
        (tf.translation, player.flying)
    } else {
        (Vec3::ZERO, true)
    };

    let biome = world.biome_at(to_i32_safe(pos.x), to_i32_safe(pos.z));

    let hour = settings.time_of_day as u32 % 24;
    let minute = ((settings.time_of_day.fract()) * 60.0) as u32;

    let weather = &settings.weather;
    let weather_line = format!(
        "WX {:?}  RAIN {:>3.0}%  SNOW {:>3.0}%  FOG {:>3.0}%  FX {:>3.0}%",
        weather.preset,
        weather.rain_intensity * 100.0,
        weather.snow_intensity * 100.0,
        weather.fog_density * 100.0,
        governor.weather_fx_scale * 100.0,
    );

    let sim_mode = if pause.map(|p| p.paused).unwrap_or(false) {
        "[|| EDIT]"
    } else {
        "[> PLAY]"
    };
    let director_line = director
        .as_deref()
        .map(|d| d.cockpit_line())
        .unwrap_or_else(|| "RANK -- no objective".into());

    // Write in-place into the existing String to avoid a 200-byte
    // allocation + drop every frame (~14 MB/s alloc churn at 60fps
    // over one hour). `write!` on String can only fail via OOM which
    // would already be fatal; ignore the Result.
    use std::fmt::Write as _;
    let buf = &mut text.sections[0].value;
    buf.clear();
    let _ = write!(
        buf,
        "NEUROCORE {sim_mode}  {} {} {}  FPS {fps:>3.0}/{:>3.0}  P {:>2.0}%  Q {:>2.0}%\nNAV  X {:>7.1}  Y {:>6.1}  Z {:>7.1}  // {:?}\nWORLD {hour:02}:{minute:02} {:?}  //  {}  //  FOV {:.0}\nBUDGET RD {}/{}  TERR {}/{}  MESH {}/{}  UP {}  SHADOW {}  {}\n{}\nAETHER  Shift+F10 island  Shift+F11 station  SKYWAY workflow\nOBJ  {}\nSKETCH LMB draw face  RMB cut  G push/pull  Tab tools  F1 deck  ESC pause",
        governor.profile.label(),
        governor.intent.label(),
        governor.quality.label(),
        settings.target_fps,
        governor.frame_pressure * 100.0,
        governor.queue_pressure * 100.0,
        pos.x, pos.y, pos.z,
        biome,
        settings.time_mode,
        if flying { "FLY" } else { "WALK" },
        settings.fov_deg,
        governor.active_render_distance(settings.render_distance),
        settings.render_distance,
        governor.chunks_per_frame,
        governor.max_in_flight_terrain,
        governor.meshes_per_frame,
        governor.max_in_flight_meshes,
        governor.mesh_applies_per_frame,
        governor.shadow_radius,
        governor.status,
        weather_line,
        director_line,
    );
}

fn draw_neon_combat_hud(
    mut contexts: EguiContexts,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    film: Option<Res<crate::film::FilmRuntime>>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    governor: Res<StreamingGovernor>,
    player_q: Query<(&Transform, &Player)>,
    director: Option<Res<SimulationDirector>>,
    brain: Option<Res<crate::bots::FriendlyWorldBrain>>,
    telemetry: Res<UnifiedTelemetry>,
    mining: Res<DestructionStats>,
    suit: Res<SuitVitals>,
) {
    if *state.get() != crate::menu::GameState::InGame {
        return;
    }
    if film.as_ref().map(|f| f.hide_hud).unwrap_or(false) {
        return;
    }
    if mode
        .as_deref()
        .map(|m| m.is_build() || m.is_ship())
        .unwrap_or(false)
    {
        return;
    }
    let Ok((player_tf, player)) = player_q.get_single() else {
        return;
    };

    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("neon_combat_hud"),
    ));

    let colors = settings.theme.semantic();
    let cyan = colors.info;
    let magenta = egui::Color32::from_rgb(255, 46, 220);
    let amber = colors.warning;
    let green = colors.success;
    let profile = settings.hud_profile;
    let hud_opacity = settings.hud_panel_opacity;

    let pos = player_tf.translation;
    let biome = world.biome_at(to_i32_safe(pos.x), to_i32_safe(pos.z));
    let objective = brain
        .as_deref()
        .map(|b| b.cockpit_line())
        .or_else(|| director.as_deref().map(|d| d.cockpit_line()))
        .unwrap_or_else(|| "COMPANIONS // awaiting your instructions".into());

    let left_h = match profile {
        HudProfile::Focused => 56.0,
        HudProfile::Guided => 104.0,
        HudProfile::Creator => 92.0,
    };
    let left_w = match profile {
        HudProfile::Focused => 276.0,
        HudProfile::Guided | HudProfile::Creator => 326.0,
    };
    let left = egui::Rect::from_min_size(
        screen.left_top() + egui::vec2(22.0, 28.0),
        egui::vec2(left_w, left_h),
    );
    crate::ui_kit::hud_panel(&painter, left, settings.theme, hud_opacity, cyan);
    hud_text(
        &painter,
        left.left_top() + egui::vec2(16.0, 12.0),
        match profile {
            HudProfile::Creator => "CREATOR STATUS",
            _ => "OBJECTIVE",
        },
        cyan,
        15.0,
    );
    hud_text(
        &painter,
        left.left_top() + egui::vec2(16.0, 36.0),
        &compact_hud_line(
            &objective,
            if profile == HudProfile::Focused {
                28
            } else {
                34
            },
        ),
        colors.text,
        12.0,
    );
    if profile != HudProfile::Focused {
        let biome_line = format!(
            "{:?}  //  score {:>4.0}  bots {}  build {}",
            biome,
            telemetry.mission_score(),
            brain.as_deref().map(|b| b.bot_count()).unwrap_or(0),
            telemetry.build_actions
        );
        hud_text(
            &painter,
            left.left_top() + egui::vec2(16.0, 70.0),
            &compact_hud_line(&biome_line, 38),
            if biome.is_neon_showcase() {
                green
            } else {
                amber
            },
            11.0,
        );
    }
    let bot_tip = brain.as_deref().map(|brain| brain.nearest_bot_line(pos));
    let tip = if let Some(bot_tip) = bot_tip.as_deref() {
        bot_tip
    } else if !settings.ship_skirmish_ai {
        "Shuttle-KI aus  |  Gefecht: E → Inventar → KI-Gefecht"
    } else if biome.is_neon_showcase() {
        "Neon-Biom  |  Bloom+Sat aktiv — Spitzen leuchten stärker"
    } else {
        "F5 Speichern  |  F1 Command Deck"
    };
    if profile != HudProfile::Focused {
        hud_text(
            &painter,
            left.left_top() + egui::vec2(16.0, 84.0),
            &compact_hud_line(tip, 48),
            colors.text_muted,
            10.0,
        );
    }

    let top = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, screen.top() + 42.0),
        egui::vec2(430.0, 54.0),
    );
    crate::ui_kit::hud_panel(&painter, top, settings.theme, hud_opacity * 0.72, cyan);
    painter.line_segment(
        [
            egui::pos2(top.left() + 22.0, top.center().y),
            egui::pos2(top.right() - 22.0, top.center().y),
        ],
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 230, 255, 100)),
    );
    for i in 0..17 {
        let x = top.left() + 28.0 + i as f32 * (top.width() - 56.0) / 16.0;
        let tall = i % 4 == 0;
        painter.line_segment(
            [
                egui::pos2(x, top.center().y - if tall { 14.0 } else { 7.0 }),
                egui::pos2(x, top.center().y + if tall { 14.0 } else { 7.0 }),
            ],
            egui::Stroke::new(
                1.0,
                if tall {
                    cyan
                } else {
                    egui::Color32::from_rgba_unmultiplied(0, 230, 255, 90)
                },
            ),
        );
    }
    let compass = ["N", "NE", "E", "SE", "S"];
    for (i, label) in compass.iter().enumerate() {
        let x = top.left() + 45.0 + i as f32 * (top.width() - 90.0) / 4.0;
        hud_text(
            &painter,
            egui::pos2(x - 8.0, top.top() + 4.0),
            label,
            cyan,
            12.0,
        );
    }
    let heading = ((player.yaw.to_degrees() % 360.0) + 360.0) % 360.0;
    hud_text(
        &painter,
        egui::pos2(top.center().x - 44.0, top.bottom() - 17.0),
        &format!("HDG {:03}", heading.round() as i32),
        amber,
        13.0,
    );

    // Combat suit bars (concept FPS readout).
    let bl = egui::Rect::from_min_size(
        egui::pos2(screen.left() + 20.0, screen.bottom() - 148.0),
        egui::vec2(236.0, 98.0),
    );
    crate::ui_kit::hud_panel(&painter, bl, settings.theme, hud_opacity, cyan);
    let sh_fill = (suit.shield / 60.0).clamp(0.0, 1.0);
    let hp_fill = (suit.health / 100.0).clamp(0.0, 1.0);
    let bw = bl.width() - 24.0;
    let bh = 8.0_f32;
    hud_text(
        &painter,
        bl.left_top() + egui::vec2(12.0, 8.0),
        &format!("Shield {:>3.0}", suit.shield),
        cyan,
        12.0,
    );
    let b_sh =
        egui::Rect::from_min_size(bl.left_top() + egui::vec2(12.0, 26.0), egui::vec2(bw, bh));
    painter.rect_filled(
        b_sh,
        egui::Rounding::same(3.0),
        egui::Color32::from_gray(26),
    );
    painter.rect_filled(
        b_sh.with_max_x(b_sh.left() + bw * sh_fill),
        egui::Rounding::same(3.0),
        egui::Color32::from_rgb(50, 160, 255),
    );
    hud_text(
        &painter,
        bl.left_top() + egui::vec2(12.0, 38.0),
        &format!("Health {:>3.0}", suit.health),
        egui::Color32::from_rgb(255, 90, 90),
        12.0,
    );
    let b_hp =
        egui::Rect::from_min_size(bl.left_top() + egui::vec2(12.0, 54.0), egui::vec2(bw, bh));
    painter.rect_filled(
        b_hp,
        egui::Rounding::same(3.0),
        egui::Color32::from_gray(26),
    );
    painter.rect_filled(
        b_hp.with_max_x(b_hp.left() + bw * hp_fill),
        egui::Rounding::same(3.0),
        egui::Color32::from_rgb(240, 60, 72),
    );
    let o2_fill = (suit.oxygen / 100.0).clamp(0.0, 1.0);
    hud_text(
        &painter,
        bl.left_top() + egui::vec2(12.0, 68.0),
        &format!("Oxygen {:>3.0}%", suit.oxygen),
        cyan,
        12.0,
    );
    let b_o2 = egui::Rect::from_min_size(
        bl.left_top() + egui::vec2(116.0, 72.0),
        egui::vec2(108.0, bh),
    );
    painter.rect_filled(
        b_o2,
        egui::Rounding::same(3.0),
        egui::Color32::from_gray(26),
    );
    painter.rect_filled(
        b_o2.with_max_x(b_o2.left() + 108.0 * o2_fill),
        egui::Rounding::same(3.0),
        cyan,
    );

    if profile == HudProfile::Creator {
        let right = egui::Rect::from_min_size(
            egui::pos2(screen.right() - 302.0, screen.top() + 28.0),
            egui::vec2(280.0, 198.0),
        );
        crate::ui_kit::hud_panel(&painter, right, settings.theme, hud_opacity, magenta);
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 10.0),
            "UNIFIED TELEMETRY",
            magenta,
            16.0,
        );
        let lum_c = egui::Color32::from_rgb(40, 210, 255);
        let mag_c = egui::Color32::from_rgb(255, 140, 40);
        let ird_c = egui::Color32::from_rgb(180, 60, 255);
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 36.0),
            &format!("◆ Luminite Crystal   {:>5}", mining.luminite_units),
            lum_c,
            13.0,
        );
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 54.0),
            &format!("◆ Magnetite Ore      {:>5}", mining.magnetite_units),
            mag_c,
            13.0,
        );
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 72.0),
            &format!("◆ Iridium Vein       {:>5}", mining.iridium_units),
            ird_c,
            13.0,
        );

        let bar_w = right.width() - 32.0;
        let bar_h = 7.0_f32;
        let drill_fill = (suit.laser_drill_charge / 100.0).clamp(0.0, 1.0);
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 94.0),
            &format!("Laser drill  {:>3.0}%", suit.laser_drill_charge),
            amber,
            12.0,
        );
        let b_drill = egui::Rect::from_min_size(
            right.left_top() + egui::vec2(16.0, 112.0),
            egui::vec2(bar_w, bar_h),
        );
        painter.rect_filled(
            b_drill,
            egui::Rounding::same(3.0),
            egui::Color32::from_gray(28),
        );
        painter.rect_filled(
            b_drill.with_max_x(b_drill.left() + bar_w * drill_fill),
            egui::Rounding::same(3.0),
            amber,
        );

        let o2_fill = (suit.oxygen / 100.0).clamp(0.0, 1.0);
        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 126.0),
            &format!("Oxygen       {:>3.0}%", suit.oxygen),
            cyan,
            12.0,
        );
        let b_o2 = egui::Rect::from_min_size(
            right.left_top() + egui::vec2(16.0, 144.0),
            egui::vec2(bar_w, bar_h),
        );
        painter.rect_filled(
            b_o2,
            egui::Rounding::same(3.0),
            egui::Color32::from_gray(28),
        );
        painter.rect_filled(
            b_o2.with_max_x(b_o2.left() + bar_w * o2_fill),
            egui::Rounding::same(3.0),
            cyan,
        );

        hud_text(
            &painter,
            right.left_top() + egui::vec2(16.0, 158.0),
            &format!(
                "XYZ {:>4.0},{:>4.0},{:>4.0}  {}  BOTS {:02}",
                pos.x,
                pos.y,
                pos.z,
                if player.flying { "FLT" } else { "GND" },
                brain.as_deref().map(|b| b.bot_count()).unwrap_or(0)
            ),
            egui::Color32::from_gray(200),
            11.0,
        );
    }

    if profile != HudProfile::Focused {
        let config = egui::Rect::from_min_size(
            egui::pos2(screen.right() - 304.0, screen.bottom() - 246.0),
            egui::vec2(282.0, 64.0),
        );
        crate::ui_kit::hud_panel(&painter, config, settings.theme, hud_opacity * 0.78, green);
        hud_text(
            &painter,
            config.left_top() + egui::vec2(14.0, 9.0),
            "LIQUID CORE CONFIG",
            green,
            13.0,
        );
        hud_text(
            &painter,
            config.left_top() + egui::vec2(14.0, 30.0),
            &format!(
                "{} {}  RD {}/{}",
                governor.profile.label(),
                governor.quality.label(),
                governor.active_render_distance(settings.render_distance),
                settings.render_distance
            ),
            colors.text,
            11.0,
        );
        hud_text(
            &painter,
            config.left_top() + egui::vec2(14.0, 46.0),
            &format!(
                "TERR {}/{}  MESH {}/{}  UP {}",
                governor.chunks_per_frame,
                governor.max_in_flight_terrain,
                governor.meshes_per_frame,
                governor.max_in_flight_meshes,
                governor.mesh_applies_per_frame
            ),
            colors.text_muted,
            10.0,
        );
    }

    draw_ground_minimap(
        &painter,
        screen,
        &world,
        to_i32_safe(pos.x),
        to_i32_safe(pos.z),
        player.yaw,
    );

    if let Some(mode) = mode.as_deref().filter(|m| m.transition_hint_t > 0.0) {
        hud_text(
            &painter,
            egui::pos2(screen.center().x - 155.0, screen.bottom() - 180.0),
            &mode.status,
            cyan,
            12.0,
        );
    }
}

#[derive(Clone, Copy)]
struct WorkflowStep {
    label: &'static str,
    key: &'static str,
    icon: Icon,
}

fn workflow_steps_for_profile(profile: HudProfile) -> Vec<WorkflowStep> {
    if profile == HudProfile::Focused {
        return Vec::new();
    }
    vec![
        WorkflowStep {
            label: "MOVE",
            key: "WASD",
            icon: Icon::Move,
        },
        WorkflowStep {
            label: "BUILD",
            key: "LMB/RMB",
            icon: Icon::ModeBuild,
        },
        WorkflowStep {
            label: "CITY",
            key: "6-9",
            icon: Icon::City,
        },
        WorkflowStep {
            label: "BOTS",
            key: "F1",
            icon: Icon::Wand,
        },
        WorkflowStep {
            label: "SAVE",
            key: "F5",
            icon: Icon::Save,
        },
    ]
}

fn active_workflow_label(mode: Option<&crate::mode::ModeContext>) -> &'static str {
    let Some(mode) = mode else {
        return "MOVE";
    };
    if let Some(tool) = mode.build_tool() {
        if matches!(
            tool,
            ToolbeltTool::CityRoad
                | ToolbeltTool::CityDistrict
                | ToolbeltTool::CityBuilding
                | ToolbeltTool::CityFacade
                | ToolbeltTool::SmartTower
        ) {
            "CITY"
        } else {
            "BUILD"
        }
    } else if mode.allows_weapons() {
        "MOVE"
    } else {
        "BOTS"
    }
}

fn draw_workflow_rail(
    mut contexts: EguiContexts,
    state: Res<State<crate::menu::GameState>>,
    settings: Res<WorldSettings>,
    mode: Option<Res<crate::mode::ModeContext>>,
) {
    if *state.get() != crate::menu::GameState::InGame {
        return;
    }
    let steps = workflow_steps_for_profile(settings.hud_profile);
    if steps.is_empty() {
        return;
    }
    let ctx = contexts.ctx_mut();
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("workflow_rail"),
    ));
    let colors = settings.theme.semantic();
    let mode_ref = mode.as_deref();
    let active = active_workflow_label(mode_ref);
    let rail_w = (steps.len() as f32 * 88.0 + 20.0).min(screen.width() - 44.0);
    let rail_y = if mode_ref.map(|m| m.is_build()).unwrap_or(false) {
        screen.bottom() - 124.0
    } else {
        screen.bottom() - 74.0
    };
    let rail = egui::Rect::from_center_size(
        egui::pos2(screen.center().x, rail_y),
        egui::vec2(rail_w, 48.0),
    );
    crate::ui_kit::hud_panel(
        &painter,
        rail,
        settings.theme,
        settings.hud_panel_opacity * 0.72,
        colors.info,
    );
    let slot_w = (rail.width() - 18.0) / steps.len() as f32;
    for (idx, step) in steps.iter().enumerate() {
        let left = rail.left() + 9.0 + idx as f32 * slot_w;
        let rect = egui::Rect::from_min_size(
            egui::pos2(left + 3.0, rail.top() + 7.0),
            egui::vec2((slot_w - 6.0).max(54.0), 34.0),
        );
        let is_active = step.label == active;
        let tint = if is_active {
            colors.warning
        } else {
            colors.info.linear_multiply(0.72)
        };
        painter.rect_filled(
            rect,
            egui::Rounding::same(7.0),
            if is_active {
                egui::Color32::from_rgba_unmultiplied(tint.r(), tint.g(), tint.b(), 48)
            } else {
                egui::Color32::from_rgba_unmultiplied(4, 18, 24, 118)
            },
        );
        painter.rect_stroke(
            rect,
            egui::Rounding::same(7.0),
            egui::Stroke::new(1.0, tint),
        );
        let icon_rect = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(7.0, 8.0),
            egui::vec2(18.0, 18.0),
        );
        paint_icon(&painter, icon_rect, step.icon, tint);
        hud_text(
            &painter,
            rect.left_top() + egui::vec2(31.0, 5.0),
            step.label,
            tint,
            10.5,
        );
        hud_text(
            &painter,
            rect.left_top() + egui::vec2(31.0, 19.0),
            step.key,
            colors.text_muted,
            9.5,
        );
    }
}

fn compact_hud_line(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_workflow_exposes_the_core_engine_loop() {
        let steps = workflow_steps_for_profile(HudProfile::Guided);
        let labels: Vec<&str> = steps.iter().map(|step| step.label).collect();
        assert_eq!(labels, vec!["MOVE", "BUILD", "CITY", "BOTS", "SAVE"]);
        assert!(steps.iter().any(|step| step.key == "LMB/RMB"));
        assert!(steps.iter().any(|step| step.key == "F1"));
    }

    #[test]
    fn focused_hud_hides_workflow_rail() {
        assert!(workflow_steps_for_profile(HudProfile::Focused).is_empty());
    }
}

fn hud_text(painter: &egui::Painter, pos: egui::Pos2, text: &str, color: egui::Color32, size: f32) {
    painter.text(
        pos,
        egui::Align2::LEFT_TOP,
        text,
        egui::FontId::monospace(size),
        color,
    );
}

fn biome_minimap_color(biome: Biome, surface_y: i32) -> egui::Color32 {
    let lift = ((surface_y as i32).saturating_sub(48) / 8).clamp(0, 14) as u8;
    match biome {
        Biome::CrystalSpires => egui::Color32::from_rgb(20, 140 + lift, 200),
        Biome::AlienReef => egui::Color32::from_rgb(120, 40, 180 + lift),
        Biome::VolcanicWaste => egui::Color32::from_rgb(180 + lift, 50, 30),
        Biome::GlacierShards => egui::Color32::from_rgb(180 + lift, 210, 240),
        Biome::Mesa | Biome::Desert => egui::Color32::from_rgb(200, 120 + lift, 60),
        Biome::Karst | Biome::Jungle | Biome::Forest => egui::Color32::from_rgb(30, 90 + lift, 45),
        Biome::Ocean | Biome::Beach => egui::Color32::from_rgb(25, 80 + lift, 140),
        Biome::Mountains | Biome::SnowyMountains => egui::Color32::from_rgb(90 + lift, 95, 110),
        _ => egui::Color32::from_rgb(50 + lift, 70 + lift, 45),
    }
}

/// Tactical raster minimap (concept: circular field scanner bottom-right).
fn draw_ground_minimap(
    painter: &egui::Painter,
    screen: egui::Rect,
    world: &VoxelWorld,
    px: i32,
    pz: i32,
    player_yaw: f32,
) {
    let center = egui::pos2(screen.right() - 92.0, screen.bottom() - 102.0);
    let r = 54.0_f32;
    let rim = egui::Color32::from_rgba_unmultiplied(0, 230, 255, 130);
    painter.circle_filled(
        center,
        r + 4.0,
        egui::Color32::from_rgba_unmultiplied(0, 4, 12, 200),
    );
    painter.circle_stroke(center, r + 4.0, egui::Stroke::new(1.0, rim));
    painter.circle_stroke(
        center,
        r,
        egui::Stroke::new(0.8, egui::Color32::from_rgba_unmultiplied(0, 230, 255, 70)),
    );

    let step = 12_i32;
    let cells: i32 = 6;
    let cell_px = (r * 2.0) / (cells as f32 * 2.0 + 1.0);
    for iz in -cells..=cells {
        for ix in -cells..=cells {
            let wx = px + ix * step;
            let wz = pz + iz * step;
            let biome = world.biome_at(wx, wz);
            let h = world.surface_height_at(wx, wz);
            let col = biome_minimap_color(biome, h);
            let fx = ix as f32 / (cells as f32 + 0.5);
            let fz = iz as f32 / (cells as f32 + 0.5);
            if fx * fx + fz * fz > 0.92 {
                continue;
            }
            let mx = center.x + fx * (r - 2.0);
            let my = center.y - fz * (r - 2.0);
            painter.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(mx, my),
                    egui::vec2(cell_px.max(3.2), cell_px.max(3.2)),
                ),
                egui::Rounding::same(1.0),
                col,
            );
        }
    }

    let a = -player_yaw - std::f32::consts::FRAC_PI_2;
    let tip = center + egui::vec2(a.cos(), a.sin()) * 16.0;
    painter.line_segment([center, tip], egui::Stroke::new(2.0, egui::Color32::WHITE));
    painter.circle_filled(center, 3.0, egui::Color32::WHITE);
    painter.circle_stroke(center, 3.0, egui::Stroke::new(1.0, rim));

    hud_text(
        painter,
        center + egui::vec2(-28.0, -r - 18.0),
        "TAC MAP",
        rim,
        10.0,
    );
}

// ------------------------------ Hint banner -------------------------------

#[derive(Component)]
pub struct HintBanner;

fn spawn_hint(mut commands: Commands) {
    commands.spawn((
        TextBundle::from_section(
            "LMB START -> ENDPOINT: BUILD  //  RMB: CUT  //  TAB: TOOLS",
            TextStyle {
                font_size: 16.0,
                color: Color::srgba(0.72, 1.0, 0.80, 0.98),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            bottom: Val::Px(100.0),
            left: Val::Auto,
            right: Val::Auto,
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_text_justify(JustifyText::Center)
        .with_background_color(Color::srgba(0.0, 0.02, 0.01, 0.46)),
        HintBanner,
    ));
}

fn update_hint(
    windows: Query<&Window, With<PrimaryWindow>>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    mut q: Query<&mut Visibility, With<HintBanner>>,
) {
    let Ok(mut vis) = q.get_single_mut() else {
        return;
    };
    if *state.get() != crate::menu::GameState::InGame
        || mode
            .as_deref()
            .map(|m| m.is_build() || m.is_ship())
            .unwrap_or(false)
    {
        *vis = Visibility::Hidden;
        return;
    }
    let Ok(window) = windows.get_single() else {
        return;
    };
    *vis = if crate::mode::cursor_is_captured(window) {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
}

// -------------------------------- Hotbar ----------------------------------

#[derive(Resource, Debug, Clone)]
pub struct HotbarState {
    pub slots: [HotbarBlock; 9],
    pub active: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct HotbarBlock {
    pub color: Color,
}

impl Default for HotbarState {
    fn default() -> Self {
        // Weapons-only hotbar — 9 slots, each keyed to the WeaponKind
        // with that index in `WeaponKind::ALL`. The slot colour mirrors
        // the weapon's accent tint so the gun silhouette, the muzzle
        // flash and the HUD chip all agree.
        use crate::weapons::WeaponKind;
        let mut slots = [HotbarBlock {
            color: Color::WHITE,
        }; 9];
        for (i, k) in WeaponKind::ALL.iter().enumerate() {
            slots[i] = HotbarBlock { color: k.color() };
        }
        Self { slots, active: 5 }
    }
}

#[derive(Component)]
pub struct HotbarRoot;

#[derive(Component)]
pub struct HotbarSlot(pub usize);

fn spawn_hotbar(mut commands: Commands, hotbar: Res<HotbarState>) {
    // Slim command-deck hotbar: stable slot sizes, high contrast and
    // compact labels that can be scanned without covering the world.
    commands
        .spawn((
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(18.0),
                    left: Val::Percent(50.0),
                    margin: UiRect {
                        left: Val::Px(-9.0 * 33.0),
                        ..default()
                    },
                    column_gap: Val::Px(7.0),
                    padding: UiRect::all(Val::Px(7.0)),
                    ..default()
                },
                background_color: BackgroundColor(Color::srgba(0.0, 0.02, 0.015, 0.72)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                ..default()
            },
            HotbarRoot,
        ))
        .with_children(|p| {
            for i in 0..9 {
                let slot = hotbar.slots[i];
                p.spawn((
                    NodeBundle {
                        style: Style {
                            width: Val::Px(54.0),
                            height: Val::Px(54.0),
                            border: UiRect::all(Val::Px(2.0)),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::Center,
                            padding: UiRect::all(Val::Px(3.0)),
                            ..default()
                        },
                        background_color: BackgroundColor(Color::srgba(0.0, 0.015, 0.012, 0.92)),
                        border_color: BorderColor(Color::srgba(0.20, 1.0, 0.62, 0.42)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    HotbarSlot(i),
                ))
                .with_children(|c| {
                    use crate::weapons::WeaponKind;
                    let kind = WeaponKind::ALL[i];
                    // Inner weapon chip: dark background with the
                    // accent colour as a glowing border strip.
                    c.spawn(NodeBundle {
                        style: Style {
                            width: Val::Percent(100.0),
                            height: Val::Percent(100.0),
                            flex_direction: FlexDirection::Column,
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::SpaceBetween,
                            padding: UiRect::all(Val::Px(3.0)),
                            ..default()
                        },
                        background_color: BackgroundColor(slot.color.with_alpha(0.22)),
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        ..default()
                    })
                    .with_children(|cc| {
                        cc.spawn(TextBundle::from_section(
                            format!("{}", i + 1),
                            TextStyle {
                                font_size: 11.0,
                                color: Color::srgba(0.80, 1.0, 0.86, 0.90),
                                ..default()
                            },
                        ));
                        cc.spawn(TextBundle::from_section(
                            kind.name(),
                            TextStyle {
                                font_size: 10.0,
                                color: slot.color,
                                ..default()
                            },
                        ));
                        cc.spawn(TextBundle::from_section(
                            "∞",
                            TextStyle {
                                font_size: 14.0,
                                color: Color::srgba(1.0, 0.86, 0.26, 0.95),
                                ..default()
                            },
                        ));
                    });
                });
            }
        });
}

fn hotbar_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut hotbar: ResMut<HotbarState>,
    active: Res<crate::weapons::ActiveWeapon>,
    toolbelt: Option<Res<crate::toolbelt::ToolbeltState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
) {
    // Number keys still update the visible hotbar selection; the
    // actual weapon swap is performed by `weapons::switch_weapon`,
    // so we also mirror the active weapon back into the hotbar
    // highlight in case the player switches through other means.
    let keys_list = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    let build_blocks_weapons = mode
        .as_deref()
        .map(|mode| !mode.allows_weapons())
        .unwrap_or_else(|| {
            toolbelt
                .as_deref()
                .map(|toolbelt| toolbelt.blocks_weapons())
                .unwrap_or(false)
        });
    if !build_blocks_weapons {
        for (i, k) in keys_list.iter().enumerate() {
            if keys.just_pressed(*k) {
                hotbar.active = i;
            }
        }
    }
    if let Some(idx) = crate::weapons::WeaponKind::ALL
        .iter()
        .position(|k| *k == active.kind)
    {
        if hotbar.active != idx {
            hotbar.active = idx;
        }
    }
}

fn hotbar_highlight(hotbar: Res<HotbarState>, mut slots: Query<(&HotbarSlot, &mut BorderColor)>) {
    if !hotbar.is_changed() {
        return;
    }
    for (slot, mut border) in slots.iter_mut() {
        // Active slot: amber neon. Idle: dim cyan outline — keeps with
        // the rest of the futuristic HUD palette.
        *border = if slot.0 == hotbar.active {
            BorderColor(Color::srgb(1.0, 0.82, 0.2))
        } else {
            BorderColor(Color::srgba(0.20, 1.0, 0.62, 0.42))
        };
    }
}

// ------------------------------- Scope overlay --------------------------------
//
// When the player aim-down-sights any weapon (right mouse), we dim the
// outer edges of the screen and ring the centre with a bright circle +
// mil-dot reticle so it reads as "looking through an optic". The
// overlay fades in/out with `ScopeState::progress`.

/// Root marker for the scope overlay — the whole stack of black bars
/// + reticle lives under one node so we can hide it in non-InGame
/// states and fade it as ADS ramps.
#[derive(Component)]
pub struct ScopeOverlay;

/// Marker for panels whose alpha tracks the ADS progress.
#[derive(Component)]
pub struct ScopePanel {
    /// Base alpha of the panel when fully scoped (progress == 1).
    pub base_alpha: f32,
    /// Base colour (RGB kept, alpha modulated by `progress * base_alpha`).
    pub color: Color,
    /// Which colour field to modulate: background fill vs. border ring.
    pub channel: ScopeChannel,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeChannel {
    Background,
    Border,
}

fn spawn_scope_overlay(mut commands: Commands) {
    let black = Color::srgba(0.0, 0.0, 0.0, 0.0);
    let ring_cyan = Color::srgba(0.6, 0.95, 1.0, 0.0);
    let reticle_cyan = Color::srgba(0.3, 1.0, 1.0, 0.0);

    commands
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                visibility: Visibility::Hidden,
                z_index: ZIndex::Global(50),
                ..default()
            },
            ScopeOverlay,
        ))
        .with_children(|p| {
            // Four black bars surround a central square viewport. On a
            // 16:9 screen, 20% side margins + 18% top/bottom margins
            // leaves a ~60%-tall central square that reads as the
            // scope's optical window.
            // Top
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        left: Val::Px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(20.0),
                        ..default()
                    },
                    background_color: BackgroundColor(black),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.92,
                    color: Color::srgba(0.0, 0.0, 0.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Bottom
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        left: Val::Px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(20.0),
                        ..default()
                    },
                    background_color: BackgroundColor(black),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.92,
                    color: Color::srgba(0.0, 0.0, 0.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Left
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        top: Val::Percent(20.0),
                        bottom: Val::Percent(20.0),
                        left: Val::Px(0.0),
                        width: Val::Percent(22.0),
                        ..default()
                    },
                    background_color: BackgroundColor(black),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.92,
                    color: Color::srgba(0.0, 0.0, 0.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Right
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        top: Val::Percent(20.0),
                        bottom: Val::Percent(20.0),
                        right: Val::Px(0.0),
                        width: Val::Percent(22.0),
                        ..default()
                    },
                    background_color: BackgroundColor(black),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.92,
                    color: Color::srgba(0.0, 0.0, 0.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Bright ring outline on the inner edge of the viewport
            // (square with rounded corners — reads as a circular lens).
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        top: Val::Percent(20.0),
                        bottom: Val::Percent(20.0),
                        left: Val::Percent(22.0),
                        right: Val::Percent(22.0),
                        border: UiRect::all(Val::Px(3.0)),
                        ..default()
                    },
                    background_color: BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    border_color: BorderColor(ring_cyan),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.85,
                    color: Color::srgba(0.6, 0.95, 1.0, 1.0),
                    channel: ScopeChannel::Border,
                },
            ));
            // Horizontal reticle crosshair line.
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        top: Val::Percent(50.0),
                        left: Val::Percent(25.0),
                        width: Val::Percent(50.0),
                        height: Val::Px(1.0),
                        margin: UiRect::top(Val::Px(-0.5)),
                        ..default()
                    },
                    background_color: BackgroundColor(reticle_cyan),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.75,
                    color: Color::srgba(0.3, 1.0, 1.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Vertical reticle crosshair line.
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(50.0),
                        top: Val::Percent(22.0),
                        height: Val::Percent(56.0),
                        width: Val::Px(1.0),
                        margin: UiRect::left(Val::Px(-0.5)),
                        ..default()
                    },
                    background_color: BackgroundColor(reticle_cyan),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.75,
                    color: Color::srgba(0.3, 1.0, 1.0, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Central aiming dot.
            p.spawn((
                NodeBundle {
                    style: Style {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(50.0),
                        top: Val::Percent(50.0),
                        width: Val::Px(4.0),
                        height: Val::Px(4.0),
                        margin: UiRect {
                            left: Val::Px(-2.0),
                            top: Val::Px(-2.0),
                            ..default()
                        },
                        ..default()
                    },
                    background_color: BackgroundColor(Color::srgba(1.0, 0.4, 0.4, 0.0)),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 1.0,
                    color: Color::srgba(1.0, 0.4, 0.4, 1.0),
                    channel: ScopeChannel::Background,
                },
            ));
            // Mil-dot ticks at 25%, 75% on the horizontal line.
            for (xp, sz) in [(30.0_f32, 6.0_f32), (40.0, 3.0), (60.0, 3.0), (70.0, 6.0)] {
                p.spawn((
                    NodeBundle {
                        style: Style {
                            position_type: PositionType::Absolute,
                            left: Val::Percent(xp),
                            top: Val::Percent(50.0),
                            width: Val::Px(1.5),
                            height: Val::Px(sz),
                            margin: UiRect {
                                left: Val::Px(-0.75),
                                top: Val::Px(-sz * 0.5),
                                ..default()
                            },
                            ..default()
                        },
                        background_color: BackgroundColor(reticle_cyan),
                        ..default()
                    },
                    ScopePanel {
                        base_alpha: 0.8,
                        color: Color::srgba(0.3, 1.0, 1.0, 1.0),
                        channel: ScopeChannel::Background,
                    },
                ));
            }
        });
}

fn update_scope_overlay(
    scope: Res<crate::weapons::ScopeState>,
    state: Res<State<crate::menu::GameState>>,
    mut root_q: Query<&mut Visibility, With<ScopeOverlay>>,
    mut panel_q: Query<
        (
            &ScopePanel,
            Option<&mut BackgroundColor>,
            Option<&mut BorderColor>,
        ),
        Without<ScopeOverlay>,
    >,
) {
    let in_game = matches!(state.get(), crate::menu::GameState::InGame);
    // Fade in a bit earlier than full ADS so the scope reads smoothly.
    let p = if in_game {
        scope.progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Ease progress so the overlay snaps in near the top of the ADS
    // curve — keeps the transition feeling deliberate.
    let eased = (p * p * (3.0 - 2.0 * p)).clamp(0.0, 1.0);

    if let Ok(mut vis) = root_q.get_single_mut() {
        *vis = if eased > 0.01 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }

    for (panel, bg, border) in panel_q.iter_mut() {
        let a = panel.base_alpha * eased;
        let lin = panel.color.to_linear();
        let c = Color::srgba(lin.red, lin.green, lin.blue, a);
        match panel.channel {
            ScopeChannel::Background => {
                if let Some(mut bg) = bg {
                    bg.0 = c;
                }
            }
            ScopeChannel::Border => {
                if let Some(mut bd) = border {
                    bd.0 = c;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------
// Combo / hit-feedback HUD
// ---------------------------------------------------------------------

#[derive(Component)]
pub struct ComboText;

fn spawn_combo_text(mut commands: Commands) {
    commands.spawn((
        TextBundle::from_section(
            "",
            TextStyle {
                font_size: 58.0,
                color: Color::srgba(1.0, 0.9, 0.2, 0.0),
                ..default()
            },
        )
        .with_text_justify(JustifyText::Center)
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Percent(42.0),
            left: Val::Percent(50.0),
            margin: UiRect {
                left: Val::Px(-140.0),
                ..default()
            },
            width: Val::Px(280.0),
            ..default()
        }),
        ComboText,
        Name::new("ComboText"),
    ));
}

fn update_combo_text(
    stats: Res<crate::weapons::DestructionStats>,
    feedback: Res<crate::weapons::HitFeedback>,
    mut q: Query<&mut Text, With<ComboText>>,
) {
    let Ok(mut text) = q.get_single_mut() else {
        return;
    };
    let section = &mut text.sections[0];
    use std::fmt::Write as _;
    section.value.clear();
    if stats.combo >= 3 {
        let _ = write!(section.value, "x{}  COMBO", stats.combo);
        // Alpha tracks combo_timer (2.5s decay). Colour shifts from
        // yellow to orange to red as the combo gets meatier.
        let a = (stats.combo_timer / 2.5).clamp(0.0, 1.0);
        let c = stats.combo.min(40) as f32 / 40.0;
        section.style.color = Color::srgba(1.0, 0.9 - c * 0.7, 0.2 - c * 0.2, a);
    } else if feedback.flash_t > 0.0 && feedback.last_hit_blocks > 0 {
        let _ = write!(section.value, "+{}", feedback.last_hit_blocks);
        let a = (feedback.flash_t / 0.25).clamp(0.0, 1.0);
        section.style.color = Color::srgba(0.9, 1.0, 0.6, a);
    } else {
        section.style.color = Color::srgba(1.0, 0.9, 0.2, 0.0);
    }
}

/// Pulse the crosshair white for a few frames after a hit.
fn flash_crosshair_on_hit(
    feedback: Res<crate::weapons::HitFeedback>,
    mut q: Query<&Children, With<Crosshair>>,
    mut bg_q: Query<&mut BackgroundColor>,
) {
    let Ok(children) = q.get_single_mut() else {
        return;
    };
    // Interpolate from cyan (rest) to white (flash).
    let t = (feedback.flash_t / 0.25).clamp(0.0, 1.0);
    let r = 0.0 + t * 1.0;
    let g = 0.95 + t * 0.05;
    let b = 1.0;
    let a = 0.95;
    let colour = Color::srgba(r, g, b, a);
    for &child in children.iter() {
        if let Ok(mut bg) = bg_q.get_mut(child) {
            bg.0 = colour;
        }
    }
}
