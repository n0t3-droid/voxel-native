//! On-screen HUD: crosshair, FPS/position/time/biome overlay, hotbar,
//! startup hint banner that fades once the cursor is captured.
//!
//! Port target: `components/Hotbar.tsx`, `components/InfoOverlay.tsx` and
//! the corner text in `components/VoxelEngine.tsx`.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use crate::player::Player;
use crate::settings::WorldSettings;
use crate::world::VoxelWorld;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin)
            .insert_resource(HotbarState::default())
            .add_systems(Startup, (spawn_crosshair, spawn_stats_text, spawn_hint, spawn_hotbar))
            .add_systems(
                Update,
                (
                    update_stats_text,
                    update_hint,
                    hotbar_input,
                    hotbar_highlight,
                ),
            );
    }
}

// ------------------------------- Crosshair --------------------------------

#[derive(Component)]
pub struct Crosshair;

fn spawn_crosshair(mut commands: Commands) {
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
            // horizontal bar
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(14.0),
                    height: Val::Px(2.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                background_color: BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.85)),
                ..default()
            });
            // vertical bar
            p.spawn(NodeBundle {
                style: Style {
                    width: Val::Px(2.0),
                    height: Val::Px(14.0),
                    position_type: PositionType::Absolute,
                    ..default()
                },
                background_color: BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.85)),
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
                font_size: 18.0,
                color: Color::srgba(1.0, 1.0, 1.0, 0.92),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(12.0),
            ..default()
        })
        .with_background_color(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        StatsText,
    ));
}

fn update_stats_text(
    diagnostics: Res<DiagnosticsStore>,
    settings: Res<WorldSettings>,
    world: Res<VoxelWorld>,
    player_q: Query<(&Transform, &Player)>,
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

    let biome = world.biome_at(pos.x as i32, pos.z as i32);

    let hour = settings.time_of_day as u32 % 24;
    let minute = ((settings.time_of_day.fract()) * 60.0) as u32;

    let weather = &settings.weather;
    let weather_line = format!(
        "Wetter {:?}  Regen {:>4.0}%  Schnee {:>4.0}%  Nebel {:>4.0}%",
        weather.preset,
        weather.rain_intensity * 100.0,
        weather.snow_intensity * 100.0,
        weather.fog_density * 100.0,
    );

    text.sections[0].value = format!(
        "FPS  {fps:>5.0}\nPos  {:>6.1} {:>6.1} {:>6.1}\nBiom {:?}\nZeit {hour:02}:{minute:02}  ({:?})\nMode {}  RD {}  FOV {:.0}\n{weather_line}\n[F3] Editor   [F] Fliegen   [F5] Speichern",
        pos.x, pos.y, pos.z,
        biome,
        settings.time_mode,
        if flying { "FLY" } else { "WALK" },
        settings.render_distance,
        settings.fov_deg,
    );
}

// ------------------------------ Hint banner -------------------------------

#[derive(Component)]
pub struct HintBanner;

fn spawn_hint(mut commands: Commands) {
    commands.spawn((
        TextBundle::from_section(
            "KLICK ins Fenster zum Starten  (Maus capturen)",
            TextStyle {
                font_size: 32.0,
                color: Color::srgb(1.0, 0.95, 0.6),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            bottom: Val::Px(120.0),
            left: Val::Auto,
            right: Val::Auto,
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_text_justify(JustifyText::Center)
        .with_background_color(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        HintBanner,
    ));
}

fn update_hint(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut q: Query<&mut Visibility, With<HintBanner>>,
) {
    let Ok(window) = windows.get_single() else {
        return;
    };
    let Ok(mut vis) = q.get_single_mut() else {
        return;
    };
    *vis = if window.cursor.grab_mode == CursorGrabMode::Locked {
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
    pub name: &'static str,
    pub color: Color,
}

impl Default for HotbarState {
    fn default() -> Self {
        use crate::blocks::BlockType::*;
        let pairs = [
            (Grass, "Grass"),
            (Dirt, "Dirt"),
            (Stone, "Stone"),
            (Sand, "Sand"),
            (Wood, "Wood"),
            (Leaves, "Leaves"),
            (Snow, "Snow"),
            (Gravel, "Gravel"),
            (Bedrock, "Bedrock"),
        ];
        let mut slots = [HotbarBlock {
            name: "",
            color: Color::WHITE,
        }; 9];
        for (i, (b, name)) in pairs.iter().enumerate() {
            let c = crate::blocks::voxel_color((*b).into());
            slots[i] = HotbarBlock {
                name,
                color: Color::srgb(c[0], c[1], c[2]),
            };
        }
        Self { slots, active: 0 }
    }
}

#[derive(Component)]
pub struct HotbarSlot(pub usize);

fn spawn_hotbar(mut commands: Commands, hotbar: Res<HotbarState>) {
    commands
        .spawn(NodeBundle {
            style: Style {
                position_type: PositionType::Absolute,
                bottom: Val::Px(18.0),
                left: Val::Percent(50.0),
                margin: UiRect {
                    left: Val::Px(-9.0 * 30.0),
                    ..default()
                },
                column_gap: Val::Px(6.0),
                ..default()
            },
            ..default()
        })
        .with_children(|p| {
            for i in 0..9 {
                let slot = hotbar.slots[i];
                p.spawn((
                    NodeBundle {
                        style: Style {
                            width: Val::Px(52.0),
                            height: Val::Px(52.0),
                            border: UiRect::all(Val::Px(2.0)),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::Center,
                            ..default()
                        },
                        background_color: BackgroundColor(slot.color.with_alpha(0.85)),
                        border_color: BorderColor(Color::srgba(0.0, 0.0, 0.0, 0.8)),
                        ..default()
                    },
                    HotbarSlot(i),
                ))
                .with_children(|c| {
                    c.spawn(TextBundle::from_section(
                        format!("{}", i + 1),
                        TextStyle {
                            font_size: 14.0,
                            color: Color::srgba(0.0, 0.0, 0.0, 0.8),
                            ..default()
                        },
                    ));
                });
            }
        });
}

fn hotbar_input(keys: Res<ButtonInput<KeyCode>>, mut hotbar: ResMut<HotbarState>) {
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
    for (i, k) in keys_list.iter().enumerate() {
        if keys.just_pressed(*k) {
            hotbar.active = i;
        }
    }
}

fn hotbar_highlight(
    hotbar: Res<HotbarState>,
    mut slots: Query<(&HotbarSlot, &mut BorderColor)>,
) {
    if !hotbar.is_changed() {
        return;
    }
    for (slot, mut border) in slots.iter_mut() {
        *border = if slot.0 == hotbar.active {
            BorderColor(Color::srgb(1.0, 0.9, 0.2))
        } else {
            BorderColor(Color::srgba(0.0, 0.0, 0.0, 0.8))
        };
    }
}
