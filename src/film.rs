//! HUD-off cinematic film recorder (`--film` / `VOXEL_NATIVE_FILM=1`).
//!
//! Drives a deterministic hero-shot camera through Aether Frontier beats
//! that match the goal painting: island grass close-up, combat pad
//! silhouettes, ringed-planet framing, tunnel portal + docked fighter,
//! hero shuttle with cyan plumes, and skyway / pad rail crews. Spawns
//! film-only fill + rim lights so dark voxels stay readable under ACES.

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

/// Resource other systems (HUD) can read to hide chrome during film.
#[derive(Resource, Debug, Clone)]
pub struct FilmRuntime {
    pub enabled: bool,
    pub hide_hud: bool,
    started: bool,
    finished: bool,
    elapsed: f32,
    duration: f32,
    shot_index: usize,
    /// Last shot index that already produced a capture (−1 = none).
    last_captured_shot: i32,
    lights_spawned: bool,
    shuttle_spawned: bool,
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
        // Long enough for one capture per painting beat after chunks mesh.
        let duration = env_f32("VOXEL_NATIVE_FILM_SECONDS")
            .unwrap_or(56.0)
            .clamp(20.0, 600.0);
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
            duration,
            shot_index: 0,
            last_captured_shot: -1,
            lights_spawned: false,
            shuttle_spawned: false,
            island: None,
            #[cfg(not(target_arch = "wasm32"))]
            out_dir,
            captures: Vec::new(),
        }
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
    // silhouettes without crushing the nebula (CIE daylight D65-ish cool
    // fill still applied via film lights).
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
    // Combat mode hides the Build Studio egui dock so film frames stay clean.
    mode.set(ActiveMode::Combat, "Film recorder: HUD-off combat framing.");
    toolbelt.live = false;
    toolbelt.palette_open = false;
    commands.insert_resource(ActiveWorld { meta });
    pending.0 = true;
    next.set(GameState::InGame);
    film.started = true;
    info!(
        "FILM: aether recorder started (duration {:.1}s, seed {seed}, hour {hour})",
        film.duration
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

    // Soft fill from camera-right so grass and dark hull voxels lift out
    // of crushed blacks without bleaching the nebula.
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
    // Warm rim opposite the fill — edge-lights soldiers/monsters/ships.
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
    // Key fill above the deck — keeps grass readable at 12–18 m.
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
    // Park the hero shuttle off the +X pad rim, nose toward −X so cyan
    // wakes stream toward the camera on the shuttle beat.
    let pos = Vec3::new(
        island.cx as f32 + 16.0,
        island.deck_y as f32 + 5.5,
        island.cz as f32 - 2.0,
    );
    let yaw = std::f32::consts::PI * 0.92;
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
    /// Normalised time within the film [0, 1].
    at: f32,
}

const SHOTS: &[FilmShot] = &[
    FilmShot {
        name: "island_grass_closeup",
        at: 0.10,
    },
    FilmShot {
        name: "island_keel_crystals",
        at: 0.22,
    },
    FilmShot {
        name: "combat_pad_silhouettes",
        at: 0.34,
    },
    FilmShot {
        name: "pad_rail_crew",
        at: 0.46,
    },
    FilmShot {
        name: "tunnel_portal_fighter",
        at: 0.58,
    },
    FilmShot {
        name: "shuttle_cyan_plumes",
        at: 0.70,
    },
    FilmShot {
        name: "ringed_planet_hero",
        at: 0.82,
    },
    FilmShot {
        name: "skyway_rail_crew",
        at: 0.92,
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
    mut query: Query<(&mut Transform, &mut Player)>,
    mut fill_q: Query<
        &mut Transform,
        (With<FilmFillLight>, Without<Player>, Without<FilmRimLight>),
    >,
    mut rim_q: Query<&mut Transform, (With<FilmRimLight>, Without<Player>, Without<FilmFillLight>)>,
) {
    if !film.enabled || film.finished {
        return;
    }
    let Ok((mut transform, mut player)) = query.get_single_mut() else {
        return;
    };
    let dt = time.delta_seconds().min(1.0);
    film.elapsed += dt;

    // Day/night rewrites ambient + sun each frame — reassert film floors
    // so dark voxels do not crush under ACES / lavapipe.
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
        // Prefer a station island so combat / portal / fighter beats land.
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
        film.island = best;
    }
    let Some(island) = film.island else {
        return;
    };

    let t = (film.elapsed / film.duration).clamp(0.0, 1.0);
    let (pos, look) = shot_camera(t, island, &world);

    transform.translation = pos;
    player.velocity = Vec3::ZERO;
    player.flying = true;
    player.placed_on_surface = true;
    let dir = (look - pos).normalize_or_zero();
    if dir.length_squared() > 0.0 {
        player.yaw = (-dir.x).atan2(-dir.z);
        player.pitch = dir.y.asin().clamp(-1.25, 1.05);
        transform.rotation = Quat::from_axis_angle(Vec3::Y, player.yaw)
            * Quat::from_axis_angle(Vec3::X, player.pitch);
    }

    // Parent fill/rim to the camera so every shot stays lit.
    let right = transform.right();
    let up = transform.up();
    let forward = transform.forward();
    for mut fill_tf in fill_q.iter_mut() {
        fill_tf.translation = pos + right * 6.0 + up * 10.0 - forward * 1.0;
    }
    if let Ok(mut rim_tf) = rim_q.get_single_mut() {
        rim_tf.translation = pos - right * 8.0 + up * 3.0 + forward * 5.0;
    }

    // Soft stream gate: give the hero island a few seconds to mesh, but
    // never starve the whole film on slow software adapters.
    let pending = streamer.pending_terrain.len() + streamer.pending_meshes.len();
    let loaded = world.chunks.len();
    if film.elapsed < 16.0 && film.captures.is_empty() && (pending > 80 || loaded < 20) {
        // Hold the first shot beat until chunks catch up.
        return;
    }

    let shot_i = SHOTS.iter().rposition(|s| t + 1e-3 >= s.at).unwrap_or(0);
    film.shot_index = shot_i;
}

fn shot_camera(t: f32, island: IslandSpec, world: &VoxelWorld) -> (Vec3, Vec3) {
    let deck = Vec3::new(
        island.cx as f32 + 0.5,
        island.deck_y as f32 + 1.0,
        island.cz as f32 + 0.5,
    );
    let station = deck + Vec3::Y * 1.0;

    if t < 0.18 {
        // Island grass close-up: ~14 m off the deck looking down at tufts.
        let dist = ISLAND_CLOSEUP_DISTANCE_M;
        let pos = deck + Vec3::new(5.0, dist * 0.42, 9.0);
        let look = deck + Vec3::new(1.0, 1.2, -1.0);
        return (pos, look);
    }
    if t < 0.30 {
        // Under-keel crystal pass — close enough that hanging spikes fill frame.
        let pos = deck + Vec3::new(-8.0, -5.0, 7.0);
        let look = deck + Vec3::new(0.0, -4.0, 0.0);
        return (pos, look);
    }
    if t < 0.42 {
        // Combat pad — marine vs multi-leg alien, ~8 m framing.
        let pos = station + Vec3::new(-6.0, 3.8, 7.5);
        let look = station + Vec3::new(-0.5, 2.4, 2.0);
        return (pos, look);
    }
    if t < 0.54 {
        // Pad rail-crew pair on the −X rim.
        let pos = station + Vec3::new(-7.5, 3.2, -0.5);
        let look = station + Vec3::new(-4.0, 2.2, -2.4);
        return (pos, look);
    }
    if t < 0.66 {
        // Tunnel portal + docked fighter with cyan plume.
        let pos = station + Vec3::new(7.5, 3.5, -4.0);
        let look = station + Vec3::new(5.5, 2.2, -1.0);
        return (pos, look);
    }
    if t < 0.78 {
        // Hero shuttle with cyan energy wakes.
        let shuttle = Vec3::new(
            island.cx as f32 + 16.0,
            island.deck_y as f32 + 5.5,
            island.cz as f32 - 2.0,
        );
        let pos = shuttle + Vec3::new(-7.0, 2.5, 6.5);
        let look = shuttle + Vec3::new(-1.5, 0.2, 0.0);
        return (pos, look);
    }
    if t < 0.90 {
        // Ringed planet hero: lift and catch the thick ring silhouette.
        let pos = deck + Vec3::new(-18.0, 22.0, 28.0);
        let look = pos + Vec3::new(0.35, 0.55, -0.60).normalize() * 50.0;
        return (pos, look);
    }
    // Skyway rail crew — orbit the deck edge toward a span neighbour.
    let pos = deck + Vec3::new(island.radius_x as f32 + 10.0, 5.0, 3.0);
    let surface = world.surface_height_at(island.cx + island.radius_x + 20, island.cz) as f32;
    let look = Vec3::new(
        island.cx as f32 + island.radius_x as f32 + 6.0,
        (island.deck_y as f32 + 2.5).max(surface + 4.0),
        island.cz as f32 + 1.0,
    );
    (pos, look)
}

fn film_capture(
    mut film: ResMut<FilmRuntime>,
    main_window: Query<Entity, With<PrimaryWindow>>,
    mut screenshots: ResMut<ScreenshotManager>,
) {
    if !film.enabled || film.finished {
        return;
    }
    if film.island.is_none() {
        return;
    }
    let shot_i = film.shot_index as i32;
    if shot_i <= film.last_captured_shot {
        return;
    }
    // Require a brief settle on the new beat before grabbing.
    let t = (film.elapsed / film.duration).clamp(0.0, 1.0);
    let beat_at = SHOTS.get(film.shot_index).map(|s| s.at).unwrap_or(0.0);
    if t < beat_at + 0.012 {
        return;
    }
    film.last_captured_shot = shot_i;
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
            info!("FILM: captured {}", path.display());
            film.captures.push(path.display().to_string());
            // Mirror into artifacts for the overnight review pack.
            let art = PathBuf::from("/opt/cursor/artifacts").join(format!(
                "aether_film_{:02}_{shot}.png",
                film.captures.len() - 1
            ));
            let _ = std::fs::copy(&path, &art);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (window, &mut screenshots, shot);
    }
}

fn film_finish(mut film: ResMut<FilmRuntime>, mut exit: EventWriter<AppExit>) {
    if !film.enabled || film.finished {
        return;
    }
    if film.elapsed < film.duration {
        return;
    }
    film.finished = true;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let report = film.out_dir.join("film_report.txt");
        let body = format!(
            "duration={:.2}\nshots={}\ncaptures={}\nisland={:?}\ncloseup_distance_m={}\nstation_headroom={}\nhide_hud={}\nshuttle_spawned={}\nfiles:\n{}\n",
            film.duration,
            SHOTS.len(),
            film.captures.len(),
            film.island,
            ISLAND_CLOSEUP_DISTANCE_M,
            STATION_HEADROOM,
            film.hide_hud,
            film.shuttle_spawned,
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
        let mut prev = -1.0;
        for s in SHOTS {
            assert!(s.at > prev);
            prev = s.at;
        }
    }

    #[test]
    fn film_enabled_parses_flag_helpers() {
        // env helpers stay deterministic without requiring process args.
        assert!(!env_flag("VOXEL_NATIVE_FILM_UNSET_SENTINEL_ZZZ"));
    }
}
