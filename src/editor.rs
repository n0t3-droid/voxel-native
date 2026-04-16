//! In-game editor / debug panel (F3 to toggle). Uses `bevy_egui` for a
//! floating window where you can tweak render distance, FOV, time, weather,
//! and regenerate the world with a new seed.
//!
//! Port target: `components/TerrainEditor.tsx` + `components/WeatherEditor.tsx`.

use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPlugin};

use crate::player::Player;
use crate::settings::{GraphicsMode, TimeMode, WeatherPreset, WorldSettings};
use crate::world::{ChunkStreamer, VoxelWorld};

#[derive(Resource, Default)]
pub struct EditorState {
    pub open: bool,
    pub pending_seed: Option<u32>,
    pub regen_requested: bool,
}

pub struct EditorPlugin;

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin)
            .insert_resource(EditorState::default())
            .add_systems(Update, (toggle_editor, draw_editor, handle_regen).chain());
    }
}

fn toggle_editor(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<EditorState>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
) {
    if keys.just_pressed(KeyCode::F3) {
        state.open = !state.open;
        // When the editor opens, release the mouse so the user can click
        // sliders. When it closes, leave capture to their next click.
        if let Ok(mut window) = windows.get_single_mut() {
            if state.open {
                window.cursor.grab_mode = CursorGrabMode::None;
                window.cursor.visible = true;
            }
        }
    }
}

fn draw_editor(
    mut contexts: EguiContexts,
    mut state: ResMut<EditorState>,
    mut settings: ResMut<WorldSettings>,
    mut player_q: Query<(&mut Transform, &mut Player)>,
) {
    if !state.open {
        return;
    }
    let ctx = contexts.ctx_mut();

    egui::Window::new("Voxel-Native Editor  (F3)")
        .default_pos(egui::pos2(20.0, 60.0))
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.collapsing("Welt", |ui| {
                let mut seed_text = settings.seed.to_string();
                ui.horizontal(|ui| {
                    ui.label("Seed");
                    if ui.text_edit_singleline(&mut seed_text).changed() {
                        if let Ok(n) = seed_text.parse::<u32>() {
                            state.pending_seed = Some(n);
                        }
                    }
                    if ui.button("Neu generieren").clicked() {
                        if let Some(s) = state.pending_seed.take() {
                            settings.seed = s;
                        }
                        state.regen_requested = true;
                    }
                });
                ui.add(
                    egui::Slider::new(&mut settings.render_distance, 2..=20)
                        .text("Render-Distanz (Chunks)"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.vertical_chunks, 4..=16)
                        .text("Vertikale Chunks"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.chunks_per_frame, 1..=20)
                        .text("Chunks / Frame"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.meshes_per_frame, 1..=20)
                        .text("Meshes / Frame"),
                );
            });

            ui.collapsing("Grafik", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Preset:");
                    ui.radio_value(&mut settings.graphics, GraphicsMode::Fast, "Fast");
                    ui.radio_value(&mut settings.graphics, GraphicsMode::Balanced, "Balanced");
                    ui.radio_value(&mut settings.graphics, GraphicsMode::High, "High");
                });
                ui.add(egui::Slider::new(&mut settings.fov_deg, 50.0..=110.0).text("FOV (°)"));
            });

            ui.collapsing("Zeit & Himmel", |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut settings.time_mode, TimeMode::Cycle, "Zyklus");
                    ui.radio_value(&mut settings.time_mode, TimeMode::Fixed, "Fest");
                });
                ui.add(
                    egui::Slider::new(&mut settings.time_of_day, 0.0..=24.0)
                        .text("Uhrzeit")
                        .fixed_decimals(2),
                );
                ui.add(
                    egui::Slider::new(&mut settings.cycle_speed, 0.0..=1.0)
                        .text("Zyklus-Tempo (min/s)"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Morgen (6:00)").clicked() {
                        settings.time_of_day = 6.0;
                    }
                    if ui.button("Mittag (12:00)").clicked() {
                        settings.time_of_day = 12.0;
                    }
                    if ui.button("Sonnenuntergang (19:00)").clicked() {
                        settings.time_of_day = 19.0;
                    }
                    if ui.button("Nacht (23:00)").clicked() {
                        settings.time_of_day = 23.0;
                    }
                });
            });

            ui.collapsing("Wetter", |ui| {
                let mut preset = settings.weather.preset;
                ui.horizontal_wrapped(|ui| {
                    for p in [
                        WeatherPreset::Clear,
                        WeatherPreset::LightRain,
                        WeatherPreset::Storm,
                        WeatherPreset::Snow,
                        WeatherPreset::Fog,
                        WeatherPreset::Custom,
                    ] {
                        if ui.radio_value(&mut preset, p, format!("{p:?}")).clicked() {
                            settings.weather.apply_preset(p);
                        }
                    }
                });
                ui.add(
                    egui::Slider::new(&mut settings.weather.rain_intensity, 0.0..=1.0)
                        .text("Regen"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.weather.snow_intensity, 0.0..=1.0)
                        .text("Schnee"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.weather.fog_density, 0.0..=1.0)
                        .text("Nebel"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.weather.wind_x, -10.0..=10.0).text("Wind X"),
                );
                ui.add(
                    egui::Slider::new(&mut settings.weather.wind_z, -10.0..=10.0).text("Wind Z"),
                );
            });

            ui.collapsing("Spieler", |ui| {
                if let Ok((mut tf, mut player)) = player_q.get_single_mut() {
                    ui.label(format!(
                        "Position: {:.1}, {:.1}, {:.1}",
                        tf.translation.x, tf.translation.y, tf.translation.z
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Teleport → Origin").clicked() {
                            tf.translation = Vec3::new(0.0, 120.0, 0.0);
                            player.velocity = Vec3::ZERO;
                        }
                        if ui.button("Teleport → y=200").clicked() {
                            tf.translation.y = 200.0;
                            player.velocity = Vec3::ZERO;
                        }
                    });
                    ui.checkbox(&mut player.flying, "Fliegen (F)");
                    ui.add(egui::Slider::new(&mut player.walk_speed, 1.0..=20.0).text("Gehtempo"));
                    ui.add(egui::Slider::new(&mut player.fly_speed, 4.0..=80.0).text("Flugtempo"));
                    ui.add(
                        egui::Slider::new(&mut player.sensitivity, 0.0005..=0.01)
                            .text("Maussensitivität"),
                    );
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Speichern (F5)").clicked() {
                    settings.save();
                }
                if ui.button("Schließen").clicked() {
                    state.open = false;
                }
            });
            ui.label("Tipp: Esc gibt die Maus frei, Klick fängt sie wieder.");
        });
}

/// If the user pressed "Neu generieren", drop all chunks so the streamer
/// regenerates them with the new seed.
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
