//! HUD-off cinematic film recorder (`--film` / `VOXEL_NATIVE_FILM=1`).
//!
//! Drives a deterministic hero-shot camera through Aether Frontier beats
//! that match the goal painting. Each beat **holds** a fixed pose long
//! enough for lavapipe to mesh + blit before the screenshot fires, so
//! labels match pixels.

use bevy::app::AppExit;
use bevy::core_pipeline::bloom::BloomSettings;
use bevy::pbr::AmbientLight;
use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::PrimaryWindow;

use crate::daynight::Sun;
use crate::frontier::{
    find_nearest_island, IslandSpec, ISLAND_CLOSEUP_DISTANCE_M, STATION_HEADROOM,
};
use crate::menu::{GameState, PendingWorldLoad};
use crate::mode::{ActiveMode, ModeContext};
use crate::player::Player;
use crate::settings::{ActiveWorld, TimeMode, WorldMeta, WorldSettings};
use crate::ships::{spawn_aether_film_shuttle, ShipFxCache};
use crate::toolbelt::ToolbeltState;
use crate::world::{ChunkStreamer, VoxelWorld};

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

pub struct FilmPlugin;

impl Plugin for FilmPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FilmRuntime::from_env()).add_systems(
            Update,
            (
                film_enter_game,
                film_spawn_lights.run_if(in_state(GameState::InGame)),
                film_spawn_shuttle.run_if(in_state(GameState::InGame)),
                film_drive_camera.run_if(in_state(GameState::InGame)),
                film_capture.run_if(in_state(GameState::InGame)),
                film_finish.run_if(in_state(GameState::InGame)),
            )
                .chain(),
        );
    }
}

/// Resource other systems (HUD / player) can read to hide chrome / lock look.
#[derive(Resource, Debug, Clone)]
pub struct FilmRuntime {
    pub enabled: bool,
    pub hide_hud: bool,
    started: bool,
    finished: bool,
    elapsed: f32,
    /// Seconds to linger on each beat before capturing.
    settle_secs: f32,
    /// Seconds to keep holding after the capture is queued (blit lag).
    hold_after_secs: f32,
    shot_index: usize,
    shot_entered_at: f32,
    capture_queued_at: Option<f32>,
    last_captured_shot: i32,
    lights_spawned: bool,
    shuttle_spawned: bool,
    ready_to_roll: bool,
    island: Option<IslandSpec>,
    #[cfg(not(target_arch = "wasm32"))]
    out_dir: PathBuf,
    captures: Vec<String>,
}

#[derive(Component)]
struct FilmFillLight;

#[derive(Component)]
struct FilmRimLight;

#[derive(Component)]
struct FilmShuttleMarker;

impl FilmRuntime {
    fn from_env() -> Self {
        let enabled = film_enabled();
        let settle_secs = env_f32("VOXEL_NATIVE_FILM_SETTLE")
            .unwrap_or(3.5)
            .clamp(1.0, 20.0);
        let hold_after_secs = env_f32("VOXEL_NATIVE_FILM_HOLD")
            .unwrap_or(2.0)
            .clamp(0.5, 10.0);
        #[cfg(not(target_arch = "wasm32"))]
        let out_dir = {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let dir = PathBuf::from(format!("video_captures/aether_film_{stamp}"));
            let _ = std::fs::create_dir_all(&dir);
            dir
        };
        Self {
            enabled,
            hide_hud: enabled,
            started: false,
            finished: false,
            elapsed: 0.0,
            settle_secs,
            hold_after_secs,
            shot_index: 0,
            shot_entered_at: 0.0,
            capture_queued_at: None,
            last_captured_shot: -1,
            lights_spawned: false,
            shuttle_spawned: false,
            ready_to_roll: false,
            island: None,
            #[cfg(not(target_arch = "wasm32"))]
            out_dir,
            captures: Vec::new(),
        }
    }

    fn duration_estimate(&self) -> f32 {
        // Warmup + per-shot settle/hold + small tail.
        14.0 + SHOTS.len() as f32 * (self.settle_secs + self.hold_after_secs + 0.4) + 2.0
    }
}

fn film_enabled() -> bool {
    std::env::args().any(|a| a == "--film")
        || env_flag("VOXEL_NATIVE_FILM")
        || env_flag("VOXEL_NATIVE_AETHER_FILM")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn film_enter_game(
    mut commands: Commands,
    mut film: ResMut<FilmRuntime>,
    state: Res<State<GameState>>,
    mut next: ResMut<NextState<GameState>>,
    mut pending: ResMut<PendingWorldLoad>,
    mut settings: ResMut<WorldSettings>,
    mut mode: ResMut<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
) {
    if !film.enabled || film.started || *state.get() != GameState::MainMenu {
        return;
    }
    let seed = std::env::var("VOXEL_NATIVE_FILM_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12345);
    // Mid-afternoon (~15.5 h): high solar elevation for grass / hull
    // silhouettes (CIE daylight cool fill still applied via film lights).
    let hour = env_f32("VOXEL_NATIVE_FILM_HOUR")
        .unwrap_or(15.5)
        .clamp(0.0, 24.0);
    let mut meta = WorldMeta::new("aether_film".into(), seed);
    meta.time_mode = TimeMode::Fixed;
    meta.time_of_day = hour;
    settings.seed = seed;
    settings.time_mode = meta.time_mode;
    settings.time_of_day = meta.time_of_day;
    settings.companion_ui.show_companion_dock = false;
    mode.set(ActiveMode::Combat, "Film recorder: HUD-off combat framing.");
    toolbelt.live = false;
    toolbelt.palette_open = false;
    commands.insert_resource(ActiveWorld { meta });
    pending.0 = true;
    next.set(GameState::InGame);
    film.started = true;
    info!(
        "FILM: aether recorder started (seed {seed}, hour {hour}, settle {:.1}s, shots {})",
        film.settle_secs,
        SHOTS.len()
    );
}

fn film_spawn_lights(
    mut commands: Commands,
    mut film: ResMut<FilmRuntime>,
    mut ambient: ResMut<AmbientLight>,
    mut bloom_q: Query<&mut BloomSettings, With<Player>>,
) {
    if !film.enabled || film.finished || film.lights_spawned {
        return;
    }
    film.lights_spawned = true;

    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(0.78, 0.88, 1.0),
                intensity: 420_000.0,
                range: 160.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(12.0, 18.0, -8.0),
            ..default()
        },
        FilmFillLight,
        Name::new("FilmFillLight"),
    ));
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(1.0, 0.68, 0.42),
                intensity: 280_000.0,
                range: 140.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(-14.0, 10.0, 16.0),
            ..default()
        },
        FilmRimLight,
        Name::new("FilmRimLight"),
    ));
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(0.95, 0.92, 0.85),
                intensity: 520_000.0,
                range: 180.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, 40.0, 0.0),
            ..default()
        },
        FilmFillLight,
        Name::new("FilmKeyLight"),
    ));

    ambient.brightness = ambient.brightness.max(1_450.0);
    ambient.color = Color::srgb(0.72, 0.78, 0.88);
    if let Ok(mut bloom) = bloom_q.get_single_mut() {
        bloom.intensity = bloom.intensity.max(0.22);
        bloom.prefilter_settings.threshold = 0.38;
    }
}

fn film_spawn_shuttle(
    mut commands: Commands,
    mut film: ResMut<FilmRuntime>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut fx: ResMut<ShipFxCache>,
) {
    if !film.enabled || film.finished || film.shuttle_spawned {
        return;
    }
    let Some(island) = film.island else {
        return;
    };
    film.shuttle_spawned = true;
    let pos = Vec3::new(
        island.cx as f32 + 24.0,
        island.deck_y as f32 + 6.0,
        island.cz as f32 + 4.0,
    );
    // Nose toward −X so wakes stream past a rear-quarter camera.
    let yaw = std::f32::consts::FRAC_PI_2;
    let entity = spawn_aether_film_shuttle(
        &mut commands,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut fx,
        pos,
        yaw,
    );
    commands.entity(entity).insert(FilmShuttleMarker);
    info!("FILM: spawned hero shuttle at {pos:?}");
}

#[derive(Clone, Copy)]
struct FilmShot {
    name: &'static str,
}

const SHOTS: &[FilmShot] = &[
    FilmShot {
        name: "island_grass_closeup",
    },
    FilmShot {
        name: "island_keel_crystals",
    },
    FilmShot {
        name: "combat_pad_silhouettes",
    },
    FilmShot {
        name: "pad_rail_crew",
    },
    FilmShot {
        name: "tunnel_portal_fighter",
    },
    FilmShot {
        name: "shuttle_cyan_plumes",
    },
    FilmShot {
        name: "ringed_planet_hero",
    },
    FilmShot {
        name: "skyway_rail_crew",
    },
];

fn film_drive_camera(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    mut film: ResMut<FilmRuntime>,
    mut ambient: ResMut<AmbientLight>,
    mut mode: ResMut<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut sun_q: Query<&mut DirectionalLight, With<Sun>>,
    mut query: Query<(&mut Transform, &mut Player, &mut Projection)>,
    mut fill_q: Query<
        &mut Transform,
        (With<FilmFillLight>, Without<Player>, Without<FilmRimLight>),
    >,
    mut rim_q: Query<&mut Transform, (With<FilmRimLight>, Without<Player>, Without<FilmFillLight>)>,
) {
    if !film.enabled || film.finished {
        return;
    }
    let Ok((mut transform, mut player, mut projection)) = query.get_single_mut() else {
        return;
    };
    let dt = time.delta_seconds().min(1.0);
    film.elapsed += dt;

    // Tighter hero FOV so pad figures and grass fill the frame.
    if let Projection::Perspective(ref mut persp) = *projection {
        let target: f32 = if film.shot_index == 6 { 42.0 } else { 52.0 };
        persp.fov = target.to_radians();
    }

    ambient.brightness = ambient.brightness.max(1_450.0);
    ambient.color = Color::srgb(0.72, 0.78, 0.88);
    if let Ok(mut sun) = sun_q.get_single_mut() {
        sun.illuminance = sun.illuminance.max(22_000.0);
    }
    if !matches!(mode.mode, ActiveMode::Combat) {
        mode.set(ActiveMode::Combat, "Film recorder: HUD-off combat framing.");
        toolbelt.live = false;
        toolbelt.palette_open = false;
    }

    if film.island.is_none() {
        let mut best = None;
        for origin in [(0, 0), (500, 0), (0, 500), (-500, 0), (0, -500), (900, 900)] {
            if let Some(spec) = find_nearest_island(
                world.generator.seed,
                origin.0,
                origin.1,
                6000,
                |x, z| world.generator.surface_height_at(x, z),
                |x, z| world.generator.biome_at(x, z),
            ) {
                if spec.has_station {
                    best = Some(spec);
                    break;
                }
                if best.is_none() {
                    best = Some(spec);
                }
            }
        }
        if let Some(spec) = best {
            // Jump to the island immediately so chunk streaming targets it.
            let warm = Vec3::new(
                spec.cx as f32 + 0.5,
                spec.deck_y as f32 + 12.0,
                spec.cz as f32 + 0.5,
            );
            transform.translation = warm;
            player.velocity = Vec3::ZERO;
            player.flying = true;
            player.placed_on_surface = true;
            film.island = Some(spec);
            film.shot_entered_at = film.elapsed;
            info!(
                "FILM: locked station island ({}, {}) deck_y={}",
                spec.cx, spec.cz, spec.deck_y
            );
        }
    }
    let Some(island) = film.island else {
        return;
    };

    // Warmup: wait for chunks around the island before rolling shots.
    if !film.ready_to_roll {
        let pending = streamer.pending_terrain.len() + streamer.pending_meshes.len();
        let loaded = world.chunks.len();
        let warm_pos = Vec3::new(
            island.cx as f32 + 0.5,
            island.deck_y as f32 + 12.0,
            island.cz as f32 + 0.5,
        );
        apply_camera(
            &mut transform,
            &mut player,
            warm_pos,
            warm_pos + Vec3::new(4.0, -2.0, 6.0),
        );
        follow_lights(&mut fill_q, &mut rim_q, transform.translation, &transform);
        if film.elapsed >= 12.0 || (loaded >= 28 && pending < 60 && film.elapsed >= 6.0) {
            film.ready_to_roll = true;
            film.shot_index = 0;
            film.shot_entered_at = film.elapsed;
            film.capture_queued_at = None;
            info!(
                "FILM: rolling (loaded={loaded}, pending={pending}, t={:.1})",
                film.elapsed
            );
        }
        return;
    }

    // Advance beat after settle + capture + post-hold.
    if let Some(queued_at) = film.capture_queued_at {
        if film.elapsed >= queued_at + film.hold_after_secs {
            if film.shot_index + 1 >= SHOTS.len() {
                // Stay on last pose until finish timer.
            } else {
                film.shot_index += 1;
                film.shot_entered_at = film.elapsed;
                film.capture_queued_at = None;
                info!(
                    "FILM: advance → {} ({}/{})",
                    SHOTS[film.shot_index].name,
                    film.shot_index + 1,
                    SHOTS.len()
                );
            }
        }
    }

    let (pos, look) = shot_pose(film.shot_index, island, &world);
    apply_camera(&mut transform, &mut player, pos, look);
    follow_lights(&mut fill_q, &mut rim_q, pos, &transform);
}

fn apply_camera(transform: &mut Transform, player: &mut Player, pos: Vec3, look: Vec3) {
    *transform = Transform::from_translation(pos).looking_at(look, Vec3::Y);
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    let forward = *transform.forward();
    player.yaw = (-forward.x).atan2(-forward.z);
    player.pitch = forward.y.asin().clamp(-1.25, 1.05);
}

fn follow_lights(
    fill_q: &mut Query<
        &mut Transform,
        (With<FilmFillLight>, Without<Player>, Without<FilmRimLight>),
    >,
    rim_q: &mut Query<
        &mut Transform,
        (With<FilmRimLight>, Without<Player>, Without<FilmFillLight>),
    >,
    pos: Vec3,
    cam: &Transform,
) {
    let right = cam.right();
    let up = cam.up();
    let forward = cam.forward();
    for mut fill_tf in fill_q.iter_mut() {
        fill_tf.translation = pos + right * 4.0 + up * 6.0 - forward * 1.0;
    }
    if let Ok(mut rim_tf) = rim_q.get_single_mut() {
        rim_tf.translation = pos - right * 5.0 + up * 2.0 + forward * 3.0;
    }
}

fn shot_pose(index: usize, island: IslandSpec, world: &VoxelWorld) -> (Vec3, Vec3) {
    let deck = Vec3::new(
        island.cx as f32 + 0.5,
        island.deck_y as f32 + 1.0,
        island.cz as f32 + 0.5,
    );
    let station = deck;

    match index {
        0 => {
            // Grass lawn just outside the ±5 pad, inside the island body.
            let lawn = deck + Vec3::new(7.0, 0.0, 0.0);
            let pos = lawn + Vec3::new(3.0, 7.5, 6.0);
            let look = lawn + Vec3::new(0.0, 1.5, 0.0);
            (pos, look)
        }
        1 => {
            // Under-keel hanging crystals — stand off so spikes fill mid-frame.
            let pos = deck + Vec3::new(-7.0, -4.0, 9.0);
            let look = deck + Vec3::new(0.0, -4.5, 0.0);
            (pos, look)
        }
        2 => {
            // Combat pad — elevated three-quarter on marine vs alien (~10 m).
            let pos = station + Vec3::new(-8.0, 6.0, 11.0);
            let look = station + Vec3::new(-0.5, 3.2, 2.2);
            (pos, look)
        }
        3 => {
            // Pad rail-crew pair at (−4, ·, −2/−3).
            let pos = station + Vec3::new(-9.0, 4.0, 3.0);
            let look = station + Vec3::new(-4.0, 2.6, -2.5);
            (pos, look)
        }
        4 => {
            // Tunnel portal (−Z) + fighter plume (+X) from a clear stand-off.
            let pos = station + Vec3::new(11.0, 5.5, -9.0);
            let look = station + Vec3::new(6.5, 2.4, -1.0);
            (pos, look)
        }
        5 => {
            // Hero shuttle rear-quarter so cyan wakes read.
            let shuttle = Vec3::new(
                island.cx as f32 + 24.0,
                island.deck_y as f32 + 6.0,
                island.cz as f32 + 4.0,
            );
            let pos = shuttle + Vec3::new(8.0, 3.0, 7.0);
            let look = shuttle + Vec3::new(-3.0, 0.4, 0.0);
            (pos, look)
        }
        6 => {
            // Ringed planet — match sky.rs planet_dir so the giant fills frame.
            let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
            let pos = deck + Vec3::new(-6.0, 14.0, 12.0);
            let look = pos + planet_dir * 120.0;
            (pos, look)
        }
        _ => {
            // Skyway rail — rim of the island toward a span.
            let pos = deck + Vec3::new(island.radius_x as f32 + 8.0, 5.0, 4.0);
            let surface =
                world.surface_height_at(island.cx + island.radius_x + 16, island.cz) as f32;
            let look = Vec3::new(
                island.cx as f32 + island.radius_x as f32 + 5.0,
                (island.deck_y as f32 + 2.5).max(surface + 3.0),
                island.cz as f32 + 1.0,
            );
            (pos, look)
        }
    }
}

fn film_capture(
    mut film: ResMut<FilmRuntime>,
    main_window: Query<Entity, With<PrimaryWindow>>,
    mut screenshots: ResMut<ScreenshotManager>,
) {
    if !film.enabled || film.finished || !film.ready_to_roll {
        return;
    }
    if film.island.is_none() {
        return;
    }
    if film.capture_queued_at.is_some() {
        return;
    }
    let shot_i = film.shot_index as i32;
    if shot_i <= film.last_captured_shot {
        return;
    }
    if film.elapsed < film.shot_entered_at + film.settle_secs {
        return;
    }
    let Ok(window) = main_window.get_single() else {
        return;
    };
    let shot = SHOTS
        .get(film.shot_index)
        .map(|s| s.name)
        .unwrap_or("aether");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let path = film
            .out_dir
            .join(format!("shot_{:02}_{shot}.png", film.captures.len()));
        if screenshots.save_screenshot_to_disk(window, &path).is_ok() {
            info!("FILM: queued capture {}", path.display());
            film.captures.push(path.display().to_string());
            film.last_captured_shot = shot_i;
            film.capture_queued_at = Some(film.elapsed);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (window, &mut screenshots, shot);
        film.last_captured_shot = shot_i;
        film.capture_queued_at = Some(film.elapsed);
    }
}

fn film_finish(mut film: ResMut<FilmRuntime>, mut exit: EventWriter<AppExit>) {
    if !film.enabled || film.finished {
        return;
    }
    if !film.ready_to_roll {
        return;
    }
    // Done when every shot captured and the last hold finished.
    let all_captured = film.last_captured_shot + 1 >= SHOTS.len() as i32;
    let last_hold_done = film
        .capture_queued_at
        .map(|t| film.elapsed >= t + film.hold_after_secs)
        .unwrap_or(false);
    if !(all_captured && last_hold_done) {
        // Safety valve so a stuck blit cannot hang forever.
        if film.elapsed < film.duration_estimate() + 20.0 {
            return;
        }
    }
    film.finished = true;
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Mirror captures into artifacts once files exist on disk.
        for (i, src) in film.captures.iter().enumerate() {
            let name = SHOTS.get(i).map(|s| s.name).unwrap_or("aether");
            let art = PathBuf::from("/opt/cursor/artifacts")
                .join(format!("aether_hold_{i:02}_{name}.png"));
            let _ = std::fs::copy(src, &art);
        }
        let report = film.out_dir.join("film_report.txt");
        let body = format!(
            "duration={:.2}\nshots={}\ncaptures={}\nisland={:?}\ncloseup_distance_m={}\nstation_headroom={}\nhide_hud={}\nshuttle_spawned={}\nsettle_secs={}\nhold_after_secs={}\nfiles:\n{}\n",
            film.elapsed,
            SHOTS.len(),
            film.captures.len(),
            film.island,
            ISLAND_CLOSEUP_DISTANCE_M,
            STATION_HEADROOM,
            film.hide_hud,
            film.shuttle_spawned,
            film.settle_secs,
            film.hold_after_secs,
            film.captures.join("\n")
        );
        let _ = std::fs::write(&report, &body);
        let _ = std::fs::write("/opt/cursor/artifacts/film_report.txt", &body);
        info!("FILM: finished → {}", report.display());
    }
    exit.send(AppExit::Success);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn film_shots_cover_painting_beats() {
        assert!(SHOTS.len() >= 8);
        assert!((ISLAND_CLOSEUP_DISTANCE_M - 14.0).abs() < 1e-3);
        let names: Vec<_> = SHOTS.iter().map(|s| s.name).collect();
        assert!(names
            .iter()
            .any(|n| n.contains("island") && n.contains("grass")));
        assert!(names.iter().any(|n| n.contains("combat")));
        assert!(names.iter().any(|n| n.contains("shuttle")));
        assert!(names
            .iter()
            .any(|n| n.contains("portal") || n.contains("fighter")));
        assert!(names.iter().any(|n| n.contains("planet")));
        assert!(names.iter().any(|n| n.contains("rail")));
    }

    #[test]
    fn film_enabled_parses_flag_helpers() {
        assert!(!env_flag("VOXEL_NATIVE_FILM_UNSET_SENTINEL_ZZZ"));
    }
}
