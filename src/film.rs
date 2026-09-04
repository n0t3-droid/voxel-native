//! HUD-off cinematic film recorder (`--film` / `VOXEL_NATIVE_FILM=1`).
//!
//! Drives a deterministic hero-shot camera through Aether Frontier beats
//! that match the goal painting: island grass close-up, combat pad
//! silhouettes, ringed-planet framing, tunnel portal + docked fighter,
//! and skyway rail crews. Spawns film-only fill + rim lights so dark
//! voxels stay readable under ACES.

use bevy::app::AppExit;
use bevy::core_pipeline::bloom::BloomSettings;
use bevy::pbr::AmbientLight;
use bevy::prelude::*;
use bevy::render::view::screenshot::ScreenshotManager;
use bevy::window::PrimaryWindow;

use crate::frontier::{
    find_nearest_island, IslandSpec, ISLAND_CLOSEUP_DISTANCE_M, STATION_HEADROOM,
};
use crate::menu::{GameState, PendingWorldLoad};
use crate::player::Player;
use crate::settings::{ActiveWorld, TimeMode, WorldMeta, WorldSettings};
use crate::world::VoxelWorld;

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
    next_capture_at: f32,
    capture_interval: f32,
    lights_spawned: bool,
    island: Option<IslandSpec>,
    #[cfg(not(target_arch = "wasm32"))]
    out_dir: PathBuf,
    captures: Vec<String>,
}

#[derive(Component)]
struct FilmFillLight;

#[derive(Component)]
struct FilmRimLight;

impl FilmRuntime {
    fn from_env() -> Self {
        let enabled = film_enabled();
        let duration = env_f32("VOXEL_NATIVE_FILM_SECONDS")
            .unwrap_or(48.0)
            .clamp(12.0, 600.0);
        let capture_interval = env_f32("VOXEL_NATIVE_FILM_INTERVAL")
            .unwrap_or(6.0)
            .clamp(1.5, 60.0);
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
            next_capture_at: 2.5,
            capture_interval,
            lights_spawned: false,
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
) {
    if !film.enabled || film.started || *state.get() != GameState::MainMenu {
        return;
    }
    let seed = std::env::var("VOXEL_NATIVE_FILM_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12345);
    let hour = env_f32("VOXEL_NATIVE_FILM_HOUR")
        .unwrap_or(18.5)
        .clamp(0.0, 24.0);
    let mut meta = WorldMeta::new("aether_film".into(), seed);
    meta.time_mode = TimeMode::Fixed;
    meta.time_of_day = hour;
    settings.seed = seed;
    settings.time_mode = meta.time_mode;
    settings.time_of_day = meta.time_of_day;
    commands.insert_resource(ActiveWorld { meta });
    pending.0 = true;
    next.set(GameState::InGame);
    film.started = true;
    info!(
        "FILM: aether recorder started (duration {:.1}s, interval {:.1}s, seed {seed})",
        film.duration, film.capture_interval
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
                color: Color::srgb(0.72, 0.84, 1.0),
                intensity: 85_000.0,
                range: 90.0,
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
                color: Color::srgb(1.0, 0.62, 0.38),
                intensity: 55_000.0,
                range: 70.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(-14.0, 10.0, 16.0),
            ..default()
        },
        FilmRimLight,
        Name::new("FilmRimLight"),
    ));

    ambient.brightness = ambient.brightness.max(620.0);
    ambient.color = Color::srgb(0.55, 0.62, 0.78);
    if let Ok(mut bloom) = bloom_q.get_single_mut() {
        bloom.intensity = bloom.intensity.max(0.16);
        bloom.prefilter_settings.threshold = 0.55;
    }
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
        at: 0.08,
    },
    FilmShot {
        name: "island_keel_crystals",
        at: 0.22,
    },
    FilmShot {
        name: "combat_pad_silhouettes",
        at: 0.38,
    },
    FilmShot {
        name: "tunnel_portal_fighter",
        at: 0.52,
    },
    FilmShot {
        name: "ringed_planet_hero",
        at: 0.68,
    },
    FilmShot {
        name: "skyway_rail_crew",
        at: 0.84,
    },
];

fn film_drive_camera(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    mut film: ResMut<FilmRuntime>,
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

    if film.island.is_none() {
        film.island = find_nearest_island(
            world.generator.seed,
            0,
            0,
            8000,
            |x, z| world.generator.surface_height_at(x, z),
            |x, z| world.generator.biome_at(x, z),
        );
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
    if let Ok(mut fill_tf) = fill_q.get_single_mut() {
        fill_tf.translation = pos + right * 10.0 + up * 6.0 - forward * 4.0;
    }
    if let Ok(mut rim_tf) = rim_q.get_single_mut() {
        rim_tf.translation = pos - right * 12.0 + up * 3.0 + forward * 8.0;
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

    // Blend across hero beats.
    if t < 0.18 {
        // Island grass close-up: 15–25 m off the deck looking down.
        let dist = ISLAND_CLOSEUP_DISTANCE_M;
        let pos = deck + Vec3::new(8.0, dist * 0.55, 14.0);
        let look = deck + Vec3::new(0.0, 0.5, 0.0);
        return (pos, look);
    }
    if t < 0.32 {
        // Under-keel crystal pass.
        let pos = deck + Vec3::new(-12.0, -8.0, 10.0);
        let look = deck + Vec3::new(0.0, -6.0, 0.0);
        return (pos, look);
    }
    if t < 0.48 {
        // Combat pad — readable marine vs alien.
        let pos = station + Vec3::new(-9.0, 5.5, 11.0);
        let look = station + Vec3::new(0.0, 2.0, 2.0);
        return (pos, look);
    }
    if t < 0.62 {
        // Tunnel portal + docked fighter.
        let pos = station + Vec3::new(10.0, 4.0, -6.0);
        let look = station + Vec3::new(4.0, 2.0, -2.0);
        return (pos, look);
    }
    if t < 0.78 {
        // Ringed planet hero: lift and look toward the sky landmark.
        let pos = deck + Vec3::new(-20.0, 28.0, 35.0);
        let look = pos + Vec3::new(0.55, 0.45, -0.52).normalize() * 40.0;
        return (pos, look);
    }
    // Skyway rail crew — find a span neighbour or orbit the deck edge.
    let pos = deck + Vec3::new(island.radius_x as f32 + 18.0, 6.0, 4.0);
    let surface = world.surface_height_at(island.cx + island.radius_x + 20, island.cz) as f32;
    let look = Vec3::new(
        island.cx as f32 + island.radius_x as f32 + 8.0,
        (island.deck_y as f32 + 2.0).max(surface + 4.0),
        island.cz as f32,
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
    if film.elapsed < film.next_capture_at {
        return;
    }
    film.next_capture_at = film.elapsed + film.capture_interval;
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
            "duration={:.2}\nshots={}\ncaptures={}\nisland={:?}\ncloseup_distance_m={}\nstation_headroom={}\nhide_hud={}\nfiles:\n{}\n",
            film.duration,
            SHOTS.len(),
            film.captures.len(),
            film.island,
            ISLAND_CLOSEUP_DISTANCE_M,
            STATION_HEADROOM,
            film.hide_hud,
            film.captures.join("\n")
        );
        let _ = std::fs::write(&report, body);
        info!("FILM: finished → {}", report.display());
    }
    exit.send(AppExit::Success);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn film_shots_cover_painting_beats() {
        assert!(SHOTS.len() >= 6);
        assert!((ISLAND_CLOSEUP_DISTANCE_M - 20.0).abs() < 1e-3);
        let names: Vec<_> = SHOTS.iter().map(|s| s.name).collect();
        assert!(names.iter().any(|n| n.contains("island")));
        assert!(names.iter().any(|n| n.contains("combat")));
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
