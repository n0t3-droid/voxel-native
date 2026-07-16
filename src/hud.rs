//! Adaptive in-game HUD: mission, vitals, hotbar and combat feedback.
//!
//! Product layers are gated by the authoritative interaction mode. Diagnostics
//! remain opt-in and never share the editor or overlay surfaces.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts};

use crate::chunk::to_i32_safe;
use crate::director::SimulationDirector;
use crate::icons::{paint_icon, Icon};
use crate::neurocore::RuntimeProfile;
use crate::player::{Player, SuitVitals};
use crate::settings::{HudProfile, WorldSettings};
use crate::world::{StreamingGovernor, VoxelWorld};

pub struct HudPlugin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HudSurface {
    Hidden,
    BuildAim,
    Play,
}

fn hud_surface(in_game: bool, active_mode: Option<crate::mode::ActiveMode>) -> HudSurface {
    if !in_game {
        return HudSurface::Hidden;
    }
    match active_mode {
        None | Some(crate::mode::ActiveMode::Combat) => HudSurface::Play,
        Some(mode) if mode.is_build_live() => HudSurface::BuildAim,
        _ => HudSurface::Hidden,
    }
}

fn current_hud_surface(
    state: &State<crate::menu::GameState>,
    mode: Option<&crate::mode::ModeContext>,
) -> HudSurface {
    hud_surface(
        *state.get() == crate::menu::GameState::InGame,
        mode.map(|mode| mode.mode),
    )
}

fn uses_static_hud_motion(settings: &WorldSettings) -> bool {
    settings.reduce_motion || settings.runtime_profile == RuntimeProfile::LowSpec
}

fn product_hud_visible(surface: HudSurface, debug_visible: bool) -> bool {
    surface == HudSurface::Play && !debug_visible
}

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
                    draw_play_hud,
                    update_hint,
                    hotbar_input.run_if(in_state(crate::menu::GameState::InGame)),
                    refresh_hotbar_contents,
                    adapt_hotbar_layout,
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
    mode: Option<Res<crate::mode::ModeContext>>,
) {
    if current_hud_surface(&state, mode.as_deref()) != HudSurface::Play {
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
    let surface = current_hud_surface(&state, mode.as_deref());
    let stats_vis = if surface == HudSurface::Play && overlay.visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let crosshair_vis = if matches!(surface, HudSurface::Play | HudSurface::BuildAim) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    let hotbar_vis = if product_hud_visible(surface, overlay.visible) {
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

fn spawn_crosshair(mut commands: Commands, settings: Res<WorldSettings>) {
    let accent = settings.theme.color.primary();
    let reticle = bevy_theme_color(accent, 0.92);
    let reticle_dim = bevy_theme_color(accent, 0.48);
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
                visibility: Visibility::Hidden,
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
                background_color: BackgroundColor(reticle),
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
                background_color: BackgroundColor(reticle),
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
                background_color: BackgroundColor(reticle),
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
                background_color: BackgroundColor(reticle),
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
                background_color: BackgroundColor(reticle_dim),
                ..default()
            });
        });
}

// --------------------------- Stats text (top-left) ------------------------

#[derive(Component)]
pub struct StatsText;

fn spawn_stats_text(mut commands: Commands, settings: Res<WorldSettings>) {
    let colors = settings.theme.semantic();
    let mut bundle = TextBundle::from_section(
        "",
        TextStyle {
            font_size: 12.0,
            color: bevy_theme_color(colors.text, 0.96),
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
    .with_background_color(bevy_theme_color(settings.theme.panel_fill(0.76), 0.76));
    bundle.visibility = Visibility::Hidden;
    commands.spawn((bundle, StatsText));
}

fn update_stats_text(
    diagnostics: Res<DiagnosticsStore>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    governor: Res<StreamingGovernor>,
    player_q: Query<(&Transform, &Player)>,
    pause: Option<Res<crate::editor::SimPause>>,
    director: Option<Res<SimulationDirector>>,
    overlay: Res<DebugOverlay>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    mut text_q: Query<(&mut Text, &mut BackgroundColor), With<StatsText>>,
) {
    if !overlay.visible || current_hud_surface(&state, mode.as_deref()) != HudSurface::Play {
        return;
    }
    let Ok((mut text, mut background)) = text_q.get_single_mut() else {
        return;
    };
    let colors = settings.theme.semantic();
    text.sections[0].style.color = bevy_theme_color(colors.text, 0.96);
    background.0 = bevy_theme_color(settings.theme.panel_fill(0.76), 0.76);
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
        "SIM PAUSED"
    } else {
        "LIVE"
    };
    let director_line = director
        .as_deref()
        .map(|d| d.cockpit_line())
        .unwrap_or_else(|| "No active mission".into());
    let director_line = compact_hud_line(&director_line, 56);

    // Keep the opt-in diagnostic compact and update its existing buffer.
    use std::fmt::Write as _;
    let buf = &mut text.sections[0].value;
    buf.clear();
    let _ = write!(
        buf,
        "DEBUG  {sim_mode}\nFRAME  FPS {fps:>3.0}/{:>3.0}  PRESS {:>2.0}%  QUEUE {:>2.0}%  {} {} {}\nPOS    {:>7.1}  {:>6.1}  {:>7.1}  {:?}  {}\nWORLD  {hour:02}:{minute:02} {:?}  FOV {:.0}\nSTREAM RD {}/{}  TERR {}/{}  MESH {}/{}  UP {}  SHADOW {}  {}\n{}\nMISSION {}",
        settings.target_fps,
        governor.frame_pressure * 100.0,
        governor.queue_pressure * 100.0,
        governor.profile.label(),
        governor.intent.label(),
        governor.quality.label(),
        pos.x,
        pos.y,
        pos.z,
        biome,
        if flying { "FLY" } else { "WALK" },
        settings.time_mode,
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

#[derive(Debug, Clone, Copy)]
struct MissionReadout {
    label: &'static str,
    distance_m: f32,
    active_builds: usize,
}

#[derive(Debug, Clone, Copy)]
struct PlayHudLayout {
    mission: egui::Rect,
    vitals: egui::Rect,
}

fn active_mission(
    brain: Option<&crate::bots::FriendlyWorldBrain>,
    player_position: Vec3,
) -> Option<MissionReadout> {
    let brain = brain?;
    let active_builds = brain.active_project_count();
    if active_builds == 0 {
        return None;
    }
    let (label, destination) = brain.navigation_dest();
    let distance_m = destination.distance(player_position);
    Some(MissionReadout {
        label,
        distance_m: if distance_m.is_finite() {
            distance_m.max(0.0)
        } else {
            0.0
        },
        active_builds,
    })
}

fn play_hud_layout(screen: egui::Rect, mission_visible: bool) -> PlayHudLayout {
    let stacked = screen.width() < 980.0 || screen.height() < 320.0;
    let margin = if screen.width() < 720.0 || screen.height() < 400.0 {
        12.0
    } else {
        18.0
    };
    let available_width = (screen.width() - margin * 2.0).max(1.0);
    let mission_size = egui::vec2(available_width.min(360.0), 68.0);
    let vitals_size = egui::vec2(available_width.min(248.0), 84.0);
    let mission =
        egui::Rect::from_min_size(screen.left_top() + egui::vec2(margin, margin), mission_size);
    let vitals_y = if stacked {
        screen.top()
            + margin
            + if mission_visible {
                mission_size.y + 8.0
            } else {
                0.0
            }
    } else {
        (screen.bottom() - margin - vitals_size.y).max(screen.top() + margin)
    };
    let vitals =
        egui::Rect::from_min_size(egui::pos2(screen.left() + margin, vitals_y), vitals_size);
    PlayHudLayout { mission, vitals }
}

fn vitals_visible(profile: HudProfile, suit: &SuitVitals) -> bool {
    if profile != HudProfile::Focused {
        return true;
    }
    normalized_vital(suit.health, 100.0) < 0.75
        || normalized_vital(suit.shield, 60.0) < 0.50
        || normalized_vital(suit.oxygen, 100.0) < 0.50
}

fn draw_play_hud(
    mut contexts: EguiContexts,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    debug: Res<DebugOverlay>,
    settings: Res<WorldSettings>,
    player_q: Query<&Transform, With<Player>>,
    brain: Option<Res<crate::bots::FriendlyWorldBrain>>,
    suit: Res<SuitVitals>,
) {
    let surface = current_hud_surface(&state, mode.as_deref());
    if !product_hud_visible(surface, debug.visible) {
        return;
    }

    let ctx = contexts.ctx_mut();

    let player_position = player_q
        .get_single()
        .map(|transform| transform.translation)
        .unwrap_or(Vec3::ZERO);
    let mission = active_mission(brain.as_deref(), player_position);
    let show_vitals = vitals_visible(settings.hud_profile, &suit);
    if mission.is_none() && !show_vitals {
        return;
    }

    let screen = ctx.screen_rect();
    let layout = play_hud_layout(screen, mission.is_some());
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("play_hud"),
    ));

    if let Some(mission) = mission {
        draw_mission_panel(&painter, layout.mission, mission, &settings);
    }
    if show_vitals {
        draw_vitals_panel(&painter, layout.vitals, &suit, &settings);
    }
}

fn draw_mission_panel(
    painter: &egui::Painter,
    rect: egui::Rect,
    mission: MissionReadout,
    settings: &WorldSettings,
) {
    let colors = settings.theme.semantic();
    crate::ui_kit::hud_panel(
        painter,
        rect,
        settings.theme,
        settings.hud_panel_opacity * 0.84,
        colors.accent,
    );

    let icon_rect = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(14.0, 24.0),
        egui::vec2(18.0, 18.0),
    );
    paint_icon(painter, icon_rect, Icon::Bookmark, colors.accent);
    hud_text(
        painter,
        rect.left_top() + egui::vec2(44.0, 9.0),
        "ACTIVE MISSION",
        colors.text_muted,
        9.5,
    );

    let max_label_chars = (((rect.width() - 136.0) / 7.0).floor() as usize).max(8);
    hud_text(
        painter,
        rect.left_top() + egui::vec2(44.0, 26.0),
        &compact_hud_line(mission.label, max_label_chars),
        colors.text,
        13.5,
    );
    let distance = if mission.distance_m >= 1_000.0 {
        format!("{:.1} km", mission.distance_m / 1_000.0)
    } else {
        format!("{:.0} m", mission.distance_m)
    };
    hud_text_right(
        painter,
        rect.right_top() + egui::vec2(-14.0, 26.0),
        &distance,
        colors.accent,
        12.5,
    );
    let build_state = if mission.active_builds == 1 {
        "1 ACTIVE BUILD".to_owned()
    } else {
        format!("{} ACTIVE BUILDS", mission.active_builds)
    };
    hud_text(
        painter,
        rect.left_top() + egui::vec2(44.0, 48.0),
        &build_state,
        colors.text_muted,
        9.5,
    );
}

fn draw_vitals_panel(
    painter: &egui::Painter,
    rect: egui::Rect,
    suit: &SuitVitals,
    settings: &WorldSettings,
) {
    let colors = settings.theme.semantic();
    let health = normalized_vital(suit.health, 100.0);
    let shield = normalized_vital(suit.shield, 60.0);
    let oxygen = normalized_vital(suit.oxygen, 100.0);
    let weakest = health.min(shield).min(oxygen);
    let (status, accent) = if weakest <= 0.25 {
        ("CRITICAL", colors.danger)
    } else if weakest <= 0.50 {
        ("CAUTION", colors.warning)
    } else {
        ("NOMINAL", colors.success)
    };

    crate::ui_kit::hud_panel(
        painter,
        rect,
        settings.theme,
        settings.hud_panel_opacity * 0.88,
        accent,
    );
    let icon_rect = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(12.0, 9.0),
        egui::vec2(16.0, 16.0),
    );
    paint_icon(painter, icon_rect, Icon::Player, accent);
    hud_text(
        painter,
        rect.left_top() + egui::vec2(36.0, 8.0),
        "SUIT",
        colors.text,
        10.5,
    );
    hud_text_right(
        painter,
        rect.right_top() + egui::vec2(-12.0, 8.0),
        status,
        accent,
        9.5,
    );

    draw_vital_row(
        painter,
        rect,
        30.0,
        "HP",
        &format!("{:.0}", suit.health.max(0.0)),
        health,
        vital_tone(health, colors.success, colors),
        colors,
    );
    draw_vital_row(
        painter,
        rect,
        48.0,
        "SHD",
        &format!("{:.0}", suit.shield.max(0.0)),
        shield,
        vital_tone(shield, colors.info, colors),
        colors,
    );
    draw_vital_row(
        painter,
        rect,
        66.0,
        "O2",
        &format!("{:.0}%", suit.oxygen.max(0.0)),
        oxygen,
        vital_tone(oxygen, colors.accent, colors),
        colors,
    );
}

fn draw_vital_row(
    painter: &egui::Painter,
    panel: egui::Rect,
    y: f32,
    label: &str,
    value: &str,
    ratio: f32,
    tone: egui::Color32,
    colors: crate::theme::SemanticColors,
) {
    let origin = panel.left_top();
    hud_text(
        painter,
        origin + egui::vec2(12.0, y - 3.0),
        label,
        colors.text_muted,
        9.0,
    );
    let bar = egui::Rect::from_min_size(
        origin + egui::vec2(48.0, y),
        egui::vec2((panel.width() - 92.0).max(24.0), 5.0),
    );
    painter.rect_filled(bar, egui::Rounding::same(2.0), colors.surface);
    painter.rect_filled(
        bar.with_max_x(bar.left() + bar.width() * ratio),
        egui::Rounding::same(2.0),
        tone,
    );
    hud_text_right(
        painter,
        panel.right_top() + egui::vec2(-12.0, y - 4.0),
        value,
        colors.text,
        9.5,
    );
}

fn normalized_vital(value: f32, maximum: f32) -> f32 {
    if value.is_finite() && maximum.is_finite() && maximum > 0.0 {
        (value / maximum).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn vital_tone(
    ratio: f32,
    normal: egui::Color32,
    colors: crate::theme::SemanticColors,
) -> egui::Color32 {
    if ratio <= 0.25 {
        colors.danger
    } else if ratio <= 0.50 {
        colors.warning
    } else {
        normal
    }
}

fn hud_text_right(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    size: f32,
) {
    painter.text(
        pos,
        egui::Align2::RIGHT_TOP,
        text,
        egui::FontId::monospace(size),
        color,
    );
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
    use crate::toolbelt::ToolbeltTool;

    #[test]
    fn play_layers_are_exclusive_to_combat() {
        use crate::mode::ActiveMode;

        assert_eq!(
            hud_surface(true, Some(ActiveMode::Combat)),
            HudSurface::Play
        );
        assert_eq!(hud_surface(true, None), HudSurface::Play);
        assert!(product_hud_visible(HudSurface::Play, false));
        assert!(!product_hud_visible(HudSurface::Play, true));
        assert_eq!(
            hud_surface(
                true,
                Some(ActiveMode::BuildLive {
                    tool: ToolbeltTool::DrawRect,
                })
            ),
            HudSurface::BuildAim
        );
        for mode in [
            ActiveMode::BuildPicker {
                tool: ToolbeltTool::DrawRect,
            },
            ActiveMode::Editor {
                tab: crate::editor::EditorTab::World,
            },
            ActiveMode::Inventory,
            ActiveMode::CommandPalette,
            ActiveMode::Paused,
        ] {
            assert_eq!(hud_surface(true, Some(mode)), HudSurface::Hidden);
        }
        assert_eq!(
            hud_surface(false, Some(ActiveMode::Combat)),
            HudSurface::Hidden
        );
    }

    #[test]
    fn compact_layout_stacks_fixed_panels_inside_the_viewport() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 360.0));
        let layout = play_hud_layout(screen, true);

        assert!(screen.contains_rect(layout.mission));
        assert!(screen.contains_rect(layout.vitals));
        assert!(layout.mission.bottom() < layout.vitals.top());
    }

    #[test]
    fn hotbar_breakpoints_keep_stable_geometry_inside_compact_windows() {
        let compact = hotbar_metrics(640.0);
        let wide = hotbar_metrics(1280.0);

        assert!(compact.root_width <= 640.0 - 24.0);
        assert_eq!(compact.slot_size, 38.0);
        assert_eq!(wide.slot_size, 44.0);
        assert!(wide.root_width > compact.root_width);
    }

    #[test]
    fn focused_vitals_are_alert_driven() {
        let mut suit = SuitVitals::default();
        assert!(!vitals_visible(HudProfile::Focused, &suit));
        assert!(vitals_visible(HudProfile::Guided, &suit));

        suit.health = 70.0;
        assert!(vitals_visible(HudProfile::Focused, &suit));
    }

    #[test]
    fn low_spec_and_reduced_motion_use_static_feedback() {
        let mut settings = WorldSettings::default();
        assert!(!uses_static_hud_motion(&settings));

        settings.reduce_motion = true;
        assert!(uses_static_hud_motion(&settings));
        settings.reduce_motion = false;
        settings.runtime_profile = RuntimeProfile::LowSpec;
        assert!(uses_static_hud_motion(&settings));
        assert_eq!(scope_visual_amount(0.2, true, true), 1.0);
        assert_eq!(scope_visual_amount(0.8, false, true), 0.0);
        assert_eq!(feedback_alpha(0.1, 0.25, true), 1.0);
    }

    #[test]
    fn default_hotbar_keeps_typed_weapon_identity() {
        let hotbar = HotbarState::default();

        for (slot, kind) in hotbar.slots.iter().zip(crate::weapons::WeaponKind::ALL) {
            assert_eq!(slot.item, HotbarItem::Weapon(kind));
            assert_eq!(slot.label(), kind.name());
        }
    }

    #[test]
    fn creative_assignment_stores_the_selected_block_type() {
        let mut hotbar = HotbarState::default();

        assert!(hotbar.assign_block(2, crate::blocks::BlockType::ShojiLamp));
        assert_eq!(
            hotbar.slots[2].item,
            HotbarItem::Block(crate::blocks::BlockType::ShojiLamp)
        );
        assert_eq!(hotbar.slots[2].label(), "Lantern");
        assert_eq!(hotbar.active, 2);
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

fn bevy_theme_color(color: egui::Color32, alpha: f32) -> Color {
    let [red, green, blue, _] = color.to_srgba_unmultiplied();
    Color::srgba(
        red as f32 / 255.0,
        green as f32 / 255.0,
        blue as f32 / 255.0,
        alpha.clamp(0.0, 1.0),
    )
}

fn mix_theme_colors(from: egui::Color32, to: egui::Color32, amount: f32, alpha: f32) -> Color {
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let [from_r, from_g, from_b, _] = from.to_srgba_unmultiplied();
    let [to_r, to_g, to_b, _] = to.to_srgba_unmultiplied();
    let mix =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() / 255.0;
    Color::srgba(
        mix(from_r, to_r),
        mix(from_g, to_g),
        mix(from_b, to_b),
        alpha.clamp(0.0, 1.0),
    )
}

// ------------------------------ Hint banner -------------------------------

#[derive(Component)]
pub struct HintBanner;

fn spawn_hint(mut commands: Commands, settings: Res<WorldSettings>) {
    let colors = settings.theme.semantic();
    let mut bundle = TextBundle::from_section(
        "CLICK TO RESUME",
        TextStyle {
            font_size: 12.0,
            color: bevy_theme_color(colors.text, 0.96),
            ..default()
        },
    )
    .with_style(Style {
        position_type: PositionType::Absolute,
        bottom: Val::Px(86.0),
        left: Val::Percent(50.0),
        width: Val::Px(180.0),
        margin: UiRect {
            left: Val::Px(-90.0),
            ..default()
        },
        padding: UiRect {
            left: Val::Px(8.0),
            right: Val::Px(8.0),
            top: Val::Px(5.0),
            bottom: Val::Px(5.0),
        },
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    })
    .with_text_justify(JustifyText::Center)
    .with_background_color(bevy_theme_color(settings.theme.panel_fill(0.74), 0.74));
    bundle.visibility = Visibility::Hidden;
    commands.spawn((bundle, HintBanner));
}

fn update_hint(
    windows: Query<&Window, With<PrimaryWindow>>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    debug: Res<DebugOverlay>,
    settings: Res<WorldSettings>,
    mut q: Query<(&mut Visibility, &mut Text, &mut BackgroundColor), With<HintBanner>>,
) {
    let Ok((mut vis, mut text, mut background)) = q.get_single_mut() else {
        return;
    };
    if settings.is_changed() {
        let colors = settings.theme.semantic();
        text.sections[0].style.color = bevy_theme_color(colors.text, 0.96);
        background.0 = bevy_theme_color(settings.theme.panel_fill(0.74), 0.74);
    }
    let surface = current_hud_surface(&state, mode.as_deref());
    if !product_hud_visible(surface, debug.visible) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotbarItem {
    Weapon(crate::weapons::WeaponKind),
    Block(crate::blocks::BlockType),
}

#[derive(Resource, Debug, Clone)]
pub struct HotbarState {
    pub slots: [HotbarBlock; 9],
    pub active: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct HotbarBlock {
    pub item: HotbarItem,
    pub color: Color,
}

impl HotbarBlock {
    pub fn weapon(kind: crate::weapons::WeaponKind) -> Self {
        Self {
            item: HotbarItem::Weapon(kind),
            color: kind.color(),
        }
    }

    pub fn block(block: crate::blocks::BlockType) -> Self {
        let rgba = crate::blocks::voxel_color(block.into());
        Self {
            item: HotbarItem::Block(block),
            color: Color::srgb(rgba[0], rgba[1], rgba[2]),
        }
    }

    pub fn label(self) -> &'static str {
        match self.item {
            HotbarItem::Weapon(kind) => kind.name(),
            HotbarItem::Block(block) => crate::blocks::block_label(block),
        }
    }
}

impl HotbarState {
    pub fn assign_block(&mut self, index: usize, block: crate::blocks::BlockType) -> bool {
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        *slot = HotbarBlock::block(block);
        self.active = index;
        true
    }
}

impl Default for HotbarState {
    fn default() -> Self {
        // The default loadout is weapons-only — 9 slots keyed to WeaponKind
        // with that index in `WeaponKind::ALL`. The slot colour mirrors
        // the weapon's accent tint so the gun silhouette, the muzzle
        // flash and the HUD chip all agree.
        let slots = crate::weapons::WeaponKind::ALL.map(HotbarBlock::weapon);
        Self { slots, active: 5 }
    }
}

#[derive(Component)]
pub struct HotbarRoot;

#[derive(Component)]
pub struct HotbarSlot(pub usize);

#[derive(Component)]
struct HotbarSlotFill(pub usize);

#[derive(Component)]
struct HotbarSlotLabel(pub usize);

#[derive(Debug, Clone, Copy, PartialEq)]
struct HotbarMetrics {
    root_width: f32,
    slot_size: f32,
    gap: f32,
    padding: f32,
    bottom: f32,
}

fn hotbar_metrics(viewport_width: f32) -> HotbarMetrics {
    let (slot_size, gap, padding, bottom) = if viewport_width < 720.0 {
        (38.0, 3.0, 5.0, 10.0)
    } else {
        (44.0, 4.0, 6.0, 14.0)
    };
    HotbarMetrics {
        root_width: slot_size * 9.0 + gap * 8.0 + padding * 2.0,
        slot_size,
        gap,
        padding,
        bottom,
    }
}

fn spawn_hotbar(mut commands: Commands, hotbar: Res<HotbarState>, settings: Res<WorldSettings>) {
    let metrics = hotbar_metrics(1280.0);
    let colors = settings.theme.semantic();
    commands
        .spawn((
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(metrics.bottom),
                    left: Val::Percent(50.0),
                    width: Val::Px(metrics.root_width),
                    height: Val::Px(metrics.slot_size + metrics.padding * 2.0),
                    margin: UiRect {
                        left: Val::Px(-metrics.root_width * 0.5),
                        ..default()
                    },
                    column_gap: Val::Px(metrics.gap),
                    padding: UiRect::all(Val::Px(metrics.padding)),
                    border: UiRect::all(Val::Px(1.0)),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                background_color: BackgroundColor(bevy_theme_color(
                    settings.theme.panel_fill(0.74),
                    0.74,
                )),
                border_color: BorderColor(bevy_theme_color(colors.outline, 0.82)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                visibility: Visibility::Hidden,
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
                            width: Val::Px(metrics.slot_size),
                            height: Val::Px(metrics.slot_size),
                            border: UiRect::all(Val::Px(1.0)),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::Center,
                            padding: UiRect::all(Val::Px(2.0)),
                            ..default()
                        },
                        background_color: BackgroundColor(bevy_theme_color(
                            colors.surface_strong,
                            0.92,
                        )),
                        border_color: BorderColor(bevy_theme_color(
                            if i == hotbar.active {
                                colors.warning
                            } else {
                                colors.outline
                            },
                            if i == hotbar.active { 0.96 } else { 0.72 },
                        )),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        ..default()
                    },
                    HotbarSlot(i),
                ))
                .with_children(|c| {
                    // Inner item chip: dark background with the
                    // accent colour as a glowing border strip.
                    c.spawn((
                        NodeBundle {
                            style: Style {
                                width: Val::Percent(100.0),
                                height: Val::Percent(100.0),
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                justify_content: JustifyContent::SpaceBetween,
                                padding: UiRect::all(Val::Px(3.0)),
                                ..default()
                            },
                            background_color: BackgroundColor(slot.color.with_alpha(0.16)),
                            border_radius: BorderRadius::all(Val::Px(4.0)),
                            ..default()
                        },
                        HotbarSlotFill(i),
                    ))
                    .with_children(|cc| {
                        cc.spawn(TextBundle::from_section(
                            format!("{}", i + 1),
                            TextStyle {
                                font_size: 9.0,
                                color: bevy_theme_color(colors.text_muted, 0.92),
                                ..default()
                            },
                        ));
                        cc.spawn((
                            TextBundle::from_section(
                                compact_hud_line(slot.label(), 7),
                                TextStyle {
                                    font_size: 8.0,
                                    color: slot.color,
                                    ..default()
                                },
                            ),
                            HotbarSlotLabel(i),
                        ));
                        cc.spawn(TextBundle::from_section(
                            "INF",
                            TextStyle {
                                font_size: 8.0,
                                color: bevy_theme_color(colors.warning, 0.94),
                                ..default()
                            },
                        ));
                    });
                });
            }
        });
}

fn refresh_hotbar_contents(
    hotbar: Res<HotbarState>,
    mut fills: Query<(&HotbarSlotFill, &mut BackgroundColor)>,
    mut labels: Query<(&HotbarSlotLabel, &mut Text)>,
) {
    if !hotbar.is_changed() {
        return;
    }

    for (marker, mut background) in fills.iter_mut() {
        let Some(slot) = hotbar.slots.get(marker.0) else {
            continue;
        };
        background.0 = slot.color.with_alpha(0.16);
    }

    for (marker, mut text) in labels.iter_mut() {
        let Some(slot) = hotbar.slots.get(marker.0) else {
            continue;
        };
        let Some(section) = text.sections.first_mut() else {
            continue;
        };
        section.value = compact_hud_line(slot.label(), 7);
        section.style.color = slot.color;
    }
}

fn adapt_hotbar_layout(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut root_q: Query<&mut Style, (With<HotbarRoot>, Without<HotbarSlot>)>,
    mut slot_q: Query<&mut Style, (With<HotbarSlot>, Without<HotbarRoot>)>,
) {
    let Ok(window) = windows.get_single() else {
        return;
    };
    let metrics = hotbar_metrics(window.width());
    if let Ok(mut style) = root_q.get_single_mut() {
        let desired_height = metrics.slot_size + metrics.padding * 2.0;
        if style.width != Val::Px(metrics.root_width)
            || style.height != Val::Px(desired_height)
            || style.bottom != Val::Px(metrics.bottom)
        {
            style.width = Val::Px(metrics.root_width);
            style.height = Val::Px(desired_height);
            style.bottom = Val::Px(metrics.bottom);
            style.margin.left = Val::Px(-metrics.root_width * 0.5);
            style.column_gap = Val::Px(metrics.gap);
            style.padding = UiRect::all(Val::Px(metrics.padding));
        }
    }
    for mut style in slot_q.iter_mut() {
        if style.width != Val::Px(metrics.slot_size) || style.height != Val::Px(metrics.slot_size) {
            style.width = Val::Px(metrics.slot_size);
            style.height = Val::Px(metrics.slot_size);
        }
    }
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
    if let Some(idx) = hotbar
        .slots
        .iter()
        .position(|slot| slot.item == HotbarItem::Weapon(active.kind))
    {
        if hotbar.active != idx {
            hotbar.active = idx;
        }
    }
}

fn hotbar_highlight(
    hotbar: Res<HotbarState>,
    settings: Res<WorldSettings>,
    mut root_q: Query<
        (&mut BackgroundColor, &mut BorderColor),
        (With<HotbarRoot>, Without<HotbarSlot>),
    >,
    mut slots: Query<
        (&HotbarSlot, &mut BackgroundColor, &mut BorderColor),
        (With<HotbarSlot>, Without<HotbarRoot>),
    >,
) {
    if !hotbar.is_changed() && !settings.is_changed() {
        return;
    }
    let colors = settings.theme.semantic();
    if let Ok((mut background, mut border)) = root_q.get_single_mut() {
        background.0 = bevy_theme_color(settings.theme.panel_fill(0.74), 0.74);
        border.0 = bevy_theme_color(colors.outline, 0.82);
    }
    for (slot, mut background, mut border) in slots.iter_mut() {
        background.0 = bevy_theme_color(colors.surface_strong, 0.92);
        // Selection and idle outlines follow the active semantic palette.
        *border = if slot.0 == hotbar.active {
            BorderColor(bevy_theme_color(colors.warning, 0.96))
        } else {
            BorderColor(bevy_theme_color(colors.outline, 0.72))
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

fn spawn_scope_overlay(mut commands: Commands, settings: Res<WorldSettings>) {
    let colors = settings.theme.semantic();
    let black = Color::srgba(0.0, 0.0, 0.0, 0.0);
    let ring = bevy_theme_color(colors.accent, 0.0);
    let ring_solid = bevy_theme_color(colors.accent, 1.0);
    let reticle = bevy_theme_color(colors.info, 0.0);
    let reticle_solid = bevy_theme_color(colors.info, 1.0);
    let danger = bevy_theme_color(colors.danger, 0.0);
    let danger_solid = bevy_theme_color(colors.danger, 1.0);

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
                    border_color: BorderColor(ring),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.85,
                    color: ring_solid,
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
                    background_color: BackgroundColor(reticle),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.75,
                    color: reticle_solid,
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
                    background_color: BackgroundColor(reticle),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 0.75,
                    color: reticle_solid,
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
                    background_color: BackgroundColor(danger),
                    border_radius: BorderRadius::all(Val::Percent(50.0)),
                    ..default()
                },
                ScopePanel {
                    base_alpha: 1.0,
                    color: danger_solid,
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
                        background_color: BackgroundColor(reticle),
                        ..default()
                    },
                    ScopePanel {
                        base_alpha: 0.8,
                        color: reticle_solid,
                        channel: ScopeChannel::Background,
                    },
                ));
            }
        });
}

fn scope_visual_amount(progress: f32, active: bool, static_motion: bool) -> f32 {
    if static_motion {
        return if active { 1.0 } else { 0.0 };
    }
    let progress = if progress.is_finite() {
        progress.clamp(0.0, 1.0)
    } else {
        0.0
    };
    progress * progress * (3.0 - 2.0 * progress)
}

fn update_scope_overlay(
    scope: Res<crate::weapons::ScopeState>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    settings: Res<WorldSettings>,
    mut last_amount: Local<Option<f32>>,
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
    let play = current_hud_surface(&state, mode.as_deref()) == HudSurface::Play;
    let amount = if play {
        scope_visual_amount(
            scope.progress,
            scope.active,
            uses_static_hud_motion(&settings),
        )
    } else {
        0.0
    };
    if last_amount.is_some_and(|last| (last - amount).abs() <= f32::EPSILON) {
        return;
    }
    *last_amount = Some(amount);

    if let Ok(mut vis) = root_q.get_single_mut() {
        *vis = if amount > 0.01 {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }

    for (panel, bg, border) in panel_q.iter_mut() {
        let a = panel.base_alpha * amount;
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

fn spawn_combo_text(mut commands: Commands, settings: Res<WorldSettings>) {
    let colors = settings.theme.semantic();
    commands.spawn((
        TextBundle::from_section(
            "",
            TextStyle {
                font_size: 24.0,
                color: bevy_theme_color(colors.warning, 0.0),
                ..default()
            },
        )
        .with_text_justify(JustifyText::Center)
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Percent(44.0),
            left: Val::Percent(50.0),
            margin: UiRect {
                left: Val::Px(-90.0),
                ..default()
            },
            width: Val::Px(180.0),
            ..default()
        }),
        ComboText,
        Name::new("ComboText"),
    ));
}

fn update_combo_text(
    stats: Res<crate::weapons::DestructionStats>,
    feedback: Res<crate::weapons::HitFeedback>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    debug: Res<DebugOverlay>,
    settings: Res<WorldSettings>,
    mut q: Query<&mut Text, With<ComboText>>,
) {
    let Ok(mut text) = q.get_single_mut() else {
        return;
    };
    let section = &mut text.sections[0];
    let colors = settings.theme.semantic();
    let surface = current_hud_surface(&state, mode.as_deref());
    let static_motion = uses_static_hud_motion(&settings);
    let (value, color) = if !product_hud_visible(surface, debug.visible) {
        (None, bevy_theme_color(colors.warning, 0.0))
    } else if stats.combo >= 3 {
        let alpha = feedback_alpha(stats.combo_timer, 2.5, static_motion);
        let intensity = stats.combo.min(40) as f32 / 40.0;
        (
            Some(format!("COMBO x{}", stats.combo)),
            mix_theme_colors(colors.warning, colors.danger, intensity, alpha),
        )
    } else if feedback.flash_t > 0.0 && feedback.last_hit_blocks > 0 {
        let alpha = feedback_alpha(feedback.flash_t, 0.25, static_motion);
        (
            Some(format!("+{}", feedback.last_hit_blocks)),
            bevy_theme_color(colors.success, alpha),
        )
    } else {
        (None, bevy_theme_color(colors.warning, 0.0))
    };
    if let Some(value) = value {
        if section.value != value {
            section.value = value;
        }
    } else if !section.value.is_empty() {
        section.value.clear();
    }
    if section.style.color != color {
        section.style.color = color;
    }
}

fn feedback_alpha(remaining: f32, duration: f32, static_motion: bool) -> f32 {
    if !remaining.is_finite() || remaining <= 0.0 {
        0.0
    } else if static_motion {
        1.0
    } else if duration.is_finite() && duration > 0.0 {
        (remaining / duration).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Briefly changes the reticle tone after a hit.
fn flash_crosshair_on_hit(
    feedback: Res<crate::weapons::HitFeedback>,
    state: Res<State<crate::menu::GameState>>,
    mode: Option<Res<crate::mode::ModeContext>>,
    settings: Res<WorldSettings>,
    mut last_colour: Local<Option<Color>>,
    q: Query<&Children, With<Crosshair>>,
    mut bg_q: Query<&mut BackgroundColor>,
) {
    let Ok(children) = q.get_single() else {
        return;
    };
    let play = current_hud_surface(&state, mode.as_deref()) == HudSurface::Play;
    let flash = if play {
        feedback_alpha(feedback.flash_t, 0.25, uses_static_hud_motion(&settings))
    } else {
        0.0
    };
    let colour = mix_theme_colors(
        settings.theme.color.primary(),
        egui::Color32::WHITE,
        flash,
        0.92,
    );
    if last_colour
        .as_ref()
        .is_some_and(|previous| *previous == colour)
    {
        return;
    }
    *last_colour = Some(colour);
    for &child in children.iter() {
        if let Ok(mut bg) = bg_q.get_mut(child) {
            bg.0 = colour;
        }
    }
}
