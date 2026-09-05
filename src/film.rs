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

use crate::blocks::{BlockType, AIR};
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
                film_stage_combat_slab.run_if(in_state(GameState::InGame)),
                film_stamp_vista_archipelago.run_if(in_state(GameState::InGame)),
                film_spawn_silhouettes.run_if(in_state(GameState::InGame)),
                film_ensure_station_pad.run_if(in_state(GameState::InGame)),
                film_drive_camera.run_if(in_state(GameState::InGame)),
                film_toggle_helpers.run_if(in_state(GameState::InGame)),
                film_capture.run_if(in_state(GameState::InGame)),
                film_finish.run_if(in_state(GameState::InGame)),
            )
                .chain(),
        );
        // After daynight's Update ClearColor write so painting/planet nebula
        // isn't crushed by noon sky wash.
        app.add_systems(
            PostUpdate,
            film_override_sky_clear.run_if(in_state(GameState::InGame)),
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
    pub shot_index: usize,
    shot_entered_at: f32,
    capture_queued_at: Option<f32>,
    last_captured_shot: i32,
    lights_spawned: bool,
    shuttle_spawned: bool,
    silhouettes_spawned: bool,
    combat_slab_staged: bool,
    vista_stamped: bool,
    station_forced: bool,
    pub ready_to_roll: bool,
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
struct FilmUnderKeelLight;

#[derive(Component)]
struct FilmFigureKeyLight;

#[derive(Component)]
struct FilmSilhouette;

/// Bounce cards / keel shells — only visible on the deck+keel beat so they
/// don't white-out the wide painting_hero frame.
#[derive(Component)]
struct FilmKeelHelper;

/// Unlit plasma/lava mesh ribbons — visible on painting_hero + dual_rivers.
#[derive(Component)]
struct FilmRiverRibbon;

/// Turret bases / muzzles / beams — hidden on tunnel so rails aren't buried
/// under fat red fire-lane slabs.
#[derive(Component)]
struct FilmTurretFx;

/// Tunnel portal + cyan monorail — hidden on combat_pad so biped/alien read.
#[derive(Component)]
struct FilmTunnelFx;

#[derive(Component)]
struct FilmPlanetProxy;

/// Crystal tower grove — hidden on fighter_swarm so plumes aren't buried.
#[derive(Component)]
struct FilmCrystalFx;

/// Fighter swarm meshes — hidden on crystal_towers so spires read clean.
#[derive(Component)]
struct FilmFighterFx;

/// High open-sky fighters only (dedicated swarm beat — no low painting wing).
#[derive(Component)]
struct FilmFighterSky;

/// Waterfall sheets/cliff — hidden on crystal grove (cyan slabs flooded the frame).
#[derive(Component)]
struct FilmWaterfallFx;

/// Verdant lawn caps / cliff faces — hide on fighter so plumes aren't green soup.
#[derive(Component)]
struct FilmGrassFx;

/// Pad + painting combat silhouettes (marine/alien).
#[derive(Component)]
struct FilmCombatFx;

/// Painting-scale combat giants — hidden on dedicated pad beat (declutter).
#[derive(Component)]
struct FilmCombatVista;

/// Bright combat stage floor — covers residual dark pad/keel lattice.
#[derive(Component)]
struct FilmCombatStage;

/// Floating combat arena (shot 2 only) — never pollute painting_hero.
#[derive(Component)]
struct FilmCombatArena;

/// Mountain station mass mesh — always on for painting/station beats.
#[derive(Component)]
struct FilmStationFx;

/// Cyan skyway rails / decks — painting + skyway_shuttle beat.
#[derive(Component)]
struct FilmSkywayFx;

#[derive(Component)]
struct FilmShuttleMarker;

impl FilmRuntime {
    fn from_env() -> Self {
        let enabled = film_enabled();
        let settle_secs = env_f32("VOXEL_NATIVE_FILM_SETTLE")
            .unwrap_or(4.0)
            .clamp(1.0, 20.0);
        let hold_after_secs = env_f32("VOXEL_NATIVE_FILM_HOLD")
            .unwrap_or(6.0)
            .clamp(0.5, 30.0);
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
            silhouettes_spawned: false,
            combat_slab_staged: false,
            vista_stamped: false,
            station_forced: false,
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
                intensity: 680_000.0,
                range: 200.0,
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
                intensity: 360_000.0,
                range: 160.0,
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
                intensity: 780_000.0,
                range: 220.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, 40.0, 0.0),
            ..default()
        },
        FilmFillLight,
        Name::new("FilmKeyLight"),
    ));
    // Under-island fill so crystal keels and grass deck share one hero frame.
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(0.78, 0.98, 1.0),
                intensity: 22_000_000.0,
                range: 320.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, -20.0, 0.0),
            ..default()
        },
        FilmUnderKeelLight,
        Name::new("FilmUnderKeelLight"),
    ));
    // Second under fill offset toward camera side of island_deck_keel.
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(0.65, 1.0, 0.88),
                intensity: 16_000_000.0,
                range: 260.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, -20.0, 0.0),
            ..default()
        },
        FilmUnderKeelLight,
        Name::new("FilmUnderKeelLightB"),
    ));
    // Bounce fill toward rim crystals (third under-island key).
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(1.0, 0.95, 0.85),
                intensity: 14_000_000.0,
                range: 220.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, -20.0, 0.0),
            ..default()
        },
        FilmUnderKeelLight,
        Name::new("FilmUnderKeelLightC"),
    ));
    // Upward spot so downward-facing voxel keel faces receive direct light.
    commands.spawn((
        SpotLightBundle {
            spot_light: SpotLight {
                color: Color::srgb(0.85, 0.98, 1.0),
                intensity: 32_000_000.0,
                range: 160.0,
                outer_angle: 1.25,
                inner_angle: 0.65,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, -30.0, 0.0).looking_at(Vec3::Y * 10.0, Vec3::Z),
            ..default()
        },
        FilmUnderKeelLight,
        Name::new("FilmUnderKeelSpot"),
    ));
    // Hard side key so mesh silhouettes cast readable limb edges on lavapipe.
    commands.spawn((
        PointLightBundle {
            point_light: PointLight {
                color: Color::srgb(1.0, 0.98, 0.92),
                intensity: 2_400_000.0,
                range: 90.0,
                shadows_enabled: false,
                ..default()
            },
            transform: Transform::from_xyz(0.0, 8.0, 0.0),
            ..default()
        },
        FilmFigureKeyLight,
        Name::new("FilmFigureKeyLight"),
    ));

    ambient.brightness = ambient.brightness.max(2_050.0);
    ambient.color = Color::srgb(0.82, 0.88, 0.78);
    if let Ok(mut bloom) = bloom_q.get_single_mut() {
        bloom.intensity = 0.08;
        bloom.prefilter_settings.threshold = 0.55;
    }
}

fn film_ensure_station_pad(mut world: ResMut<VoxelWorld>, mut film: ResMut<FilmRuntime>) {
    if !film.enabled || film.finished || film.station_forced {
        return;
    }
    let Some(island) = film.island else {
        return;
    };
    // Wait a couple of seconds for natural streaming, then force-stamp
    // the pad into resident chunks so marine/crew voxels cannot be Air
    // just because a neighbouring 16³ column lagged behind.
    if film.elapsed < 5.0 {
        return;
    }
    if station_pad_streamed(&world, island) {
        film.station_forced = true;
        return;
    }
    let ox = island.cx;
    let oy = island.deck_y + 1;
    let oz = island.cz;
    let mut written = 0usize;
    crate::frontier::visit_orbital_station(ox, oy, oz, |x, y, z, block| {
        if world.edit_set_voxel(x, y, z, block.into()) {
            written += 1;
        }
    });
    film.station_forced = true;
    info!("FILM: force-stamped orbital station at ({ox},{oy},{oz}) wrote={written} voxels");
}

/// Clear grass/props and stamp a flat dark hull slab south of the station so
/// biped vs multi-leg mesh silhouettes always read without voxel clutter.
fn film_stage_combat_slab(mut world: ResMut<VoxelWorld>, mut film: ResMut<FilmRuntime>) {
    if !film.enabled || film.finished || film.combat_slab_staged {
        return;
    }
    let Some(island) = film.island else {
        return;
    };
    if film.elapsed < 3.5 {
        return;
    }
    film.combat_slab_staged = true;
    let ox = island.cx;
    let oy = island.deck_y;
    let oz = island.cz + 14;
    let mut cleared = 0usize;
    let mut stamped = 0usize;
    // Wide/tall clear + SOLID keel fill (no AIR caves → no black lattice).
    // Also scrub the camera corridor south of the pad (+Z).
    for dx in -18..=18 {
        for dz in -12..=28 {
            let x = ox + dx;
            let z = oz + dz;
            // Air column above pad — kill rails, pylons, station stubs, grass.
            for dy in 2..=28 {
                if world.edit_set_voxel(x, oy + dy, z, AIR) {
                    cleared += 1;
                }
            }
            // Solid bright pad stack — never ShipHullDark (inks as lattice).
            for dy in -6..=1 {
                let y = oy + dy;
                let block = if dy >= 0 {
                    BlockType::LuminiteCrystal
                } else if dy >= -2 {
                    BlockType::ShipHullAlloy
                } else {
                    BlockType::CrystalVerdant
                };
                if world.edit_set_voxel(x, y, z, block.into()) {
                    stamped += 1;
                }
            }
        }
    }
    // Rest of island keel: keep prior rim treatment but skip combat frustum
    // (already solid-filled above) so we don't re-carve AIR caves under the pad.
    let mut keel_lit = 0usize;
    let rx = island.radius_x;
    let rz = island.radius_z;
    for dx in -rx..=rx {
        for dz in -rz..=rz {
            // Skip combat pad columns — already solid bright fill.
            if dx.abs() <= 18 && (dz - 14).abs() <= 28 && dz >= 2 {
                continue;
            }
            let nx = dx as f32 / rx.max(1) as f32;
            let nz = dz as f32 / rz.max(1) as f32;
            let d2 = nx * nx + nz * nz;
            if d2 > 1.02 {
                continue;
            }
            let x = island.cx + dx;
            let z = island.cz + dz;
            let thickness = ((island.keel_depth as f32)
                * (0.45 + 0.55 * (1.0 - d2.sqrt()).max(0.0)))
            .round() as i32;
            let bottom = island.deck_y - thickness.max(5);
            let edge = d2.sqrt();
            for y in bottom..=(island.deck_y - 1).max(bottom) {
                let near_bottom = y <= bottom + 2;
                let near_rim = edge > 0.42;
                if y < island.deck_y - 4 {
                    // Fill with alloy instead of AIR — prevents black cave faces
                    // that read as lattice from elevated combat cams.
                    if world.edit_set_voxel(x, y, z, BlockType::ShipHullAlloy.into()) {
                        keel_lit += 1;
                    }
                    continue;
                }
                let block = if near_bottom && ((dx + dz + y) & 1) == 0 {
                    BlockType::LuminiteCrystal
                } else if near_bottom {
                    BlockType::CrystalVerdant
                } else if near_rim {
                    if ((dx + dz) & 1) == 0 {
                        BlockType::NeonCyan
                    } else {
                        BlockType::Crystal
                    }
                } else {
                    BlockType::ShipHullAlloy
                };
                if world.edit_set_voxel(x, y, z, block.into()) {
                    keel_lit += 1;
                }
            }
        }
    }
    info!(
        "FILM: combat slab at ({ox},{oy},{oz}) cleared={cleared} stamped={stamped} keel_lit={keel_lit}"
    );
    let mut scrubbed = 0usize;
    let scrub_r = island.radius_x.max(island.radius_z) + 8;
    for dx in -scrub_r..=scrub_r {
        for dz in -scrub_r..=scrub_r {
            let x = island.cx + dx;
            let z = island.cz + dz;
            for dy in 1..=(island.keel_depth + 4) {
                let y = island.deck_y - dy;
                let v = world.voxel_at(x, y, z);
                if matches!(
                    BlockType::from_voxel(v),
                    BlockType::CrystalMagenta | BlockType::NeonMagenta
                ) {
                    let repl = if (dx + dz + dy) & 1 == 0 {
                        BlockType::LuminiteCrystal
                    } else {
                        BlockType::CrystalVerdant
                    };
                    if world.edit_set_voxel(x, y, z, repl.into()) {
                        scrubbed += 1;
                    }
                }
            }
        }
    }
    if scrubbed > 0 {
        info!("FILM: scrubbed magenta keel voxels={scrubbed}");
    }
}

/// Stamp extra floating islands near the hero station so a wide painting
/// beat can stack archipelago + skyway + station in one lavapipe frame.
fn film_stamp_vista_archipelago(mut world: ResMut<VoxelWorld>, mut film: ResMut<FilmRuntime>) {
    if !film.enabled || film.finished || film.vista_stamped {
        return;
    }
    let Some(island) = film.island else {
        return;
    };
    if film.elapsed < 4.0 {
        return;
    }
    film.vista_stamped = true;
    let mut written = 0usize;
    // Dense archipelago filling the painting_hero frustum (cam SW looking NE).
    // Never stamp magenta crystal — thin magenta columns read as HUD streaks.
    let film_crystal = BlockType::CrystalVerdant;
    let satellites: &[(i32, i32, i32, i32, i32)] = &[
        (island.cx + 28, island.cz + 18, 16, 13, -1),
        (island.cx - 22, island.cz + 28, 15, 12, 1),
        (island.cx + 42, island.cz + 22, 14, 11, -1),
        (island.cx - 38, island.cz + 36, 14, 11, 2),
        (island.cx + 58, island.cz - 12, 13, 12, -3),
        (island.cx - 24, island.cz - 48, 15, 11, 1),
        (island.cx + 32, island.cz + 58, 13, 11, 0),
        (island.cx + 78, island.cz + 34, 12, 10, -2),
        (island.cx - 62, island.cz + 12, 13, 10, 1),
        (island.cx + 18, island.cz + 78, 12, 9, -1),
        (island.cx - 52, island.cz - 28, 11, 11, -2),
        (island.cx + 88, island.cz - 35, 10, 12, 0),
        (island.cx - 78, island.cz + 48, 11, 9, -3),
        (island.cx + 48, island.cz + 88, 11, 10, 2),
        (island.cx - 15, island.cz + 62, 13, 10, -1),
        (island.cx + 65, island.cz + 55, 10, 9, 1),
        (island.cx - 40, island.cz + 75, 11, 11, 0),
        (island.cx + 12, island.cz + 42, 12, 10, -2),
        (island.cx - 8, island.cz + 48, 11, 9, 1),
        (island.cx + 55, island.cz + 12, 10, 9, 0),
        (island.cx + 72, island.cz + 68, 9, 9, -1),
        (island.cx - 55, island.cz + 58, 10, 8, 2),
        (island.cx + 38, island.cz + 72, 10, 9, -2),
        (island.cx - 30, island.cz + 18, 11, 9, 0),
        (island.cx + 95, island.cz + 18, 9, 10, 1),
        // Extra near-frustum islands for painting_hero density.
        (island.cx + 22, island.cz + 32, 14, 11, 0),
        (island.cx - 18, island.cz + 40, 12, 10, -1),
        (island.cx + 36, island.cz + 48, 11, 10, 1),
        (island.cx + 8, island.cz + 55, 10, 9, -2),
        (island.cx + 50, island.cz + 38, 9, 9, 0),
        // One more near-frustum beat (finish5 density notch).
        (island.cx + 28, island.cz + 44, 12, 10, -1),
        (island.cx - 12, island.cz + 52, 11, 9, 1),
    ];
    let mut sat_centers = Vec::with_capacity(satellites.len());
    for &(ox, oz, rx, rz, lift) in satellites {
        let deck_y = island.deck_y + lift;
        written += stamp_film_vista_island(&mut world, ox, oz, deck_y, rx, rz, film_crystal);
        sat_centers.push((ox, oz, deck_y));
    }
    // Skyway web: station → many satellites + cross-links for station mass.
    let hub = (island.cx, island.cz, island.deck_y + 1);
    for &(tx, tz, ty) in &sat_centers[..sat_centers.len().min(16)] {
        written += stamp_film_skyway_stub(&mut world, hub.0, hub.1, hub.2, tx, tz, ty + 1);
    }
    for window in sat_centers.windows(2).take(14) {
        let (a, b) = (window[0], window[1]);
        written += stamp_film_skyway_stub(&mut world, a.0, a.1, a.2 + 1, b.0, b.1, b.2 + 1);
    }
    // Cross links every 3rd pair for denser skyway lattice.
    for i in (0..sat_centers.len().saturating_sub(3)).step_by(3) {
        let a = sat_centers[i];
        let b = sat_centers[i + 3];
        written += stamp_film_skyway_stub(&mut world, a.0, a.1, a.2 + 1, b.0, b.1, b.2 + 1);
    }
    // Station-mass towers on several satellites so the wide hero reads "base".
    for &(sx, sz, sy) in sat_centers.iter().take(8) {
        written += stamp_film_station_mass(&mut world, sx, sy + 1, sz);
    }
    // Hero mountain station on the main island (+X painting look).
    written += stamp_film_station_mass(
        &mut world,
        island.cx + 20,
        island.deck_y + 1,
        island.cz + 30,
    );
    // Dual plasma + lava ribbons on the terrain shelf below the archipelago.
    written += stamp_film_dual_rivers(&mut world, island);
    info!("FILM: vista archipelago stamped voxels={written}");
}

fn stamp_film_station_mass(world: &mut VoxelWorld, cx: i32, oy: i32, cz: i32) -> usize {
    let mut n = 0usize;
    // Mountain-scale station: stepped darkrock pyramid — alloy only at tip.
    for dy in 0i32..28 {
        let half = (14 - dy / 2).max(3);
        for dx in -half..=half {
            for dz in -half..=half {
                if dx.abs() == half && dz.abs() == half && dy < 8 {
                    continue;
                }
                let block = if dy >= 26 {
                    BlockType::NeonCyan
                } else if dx.abs() + dz.abs() <= 1 && dy >= 20 {
                    BlockType::EngineCore
                } else if dy >= 24 {
                    BlockType::ShipHullDark
                } else if dy >= 12 {
                    BlockType::ShipHullDark
                } else {
                    BlockType::Stone
                };
                if world.edit_set_voxel(cx + dx, oy + dy, cz + dz, block.into()) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Force dual-channel plasma (cyan) + lava (orange) filaments into the film
/// vista so painting_hero / dual_rivers beats don't depend on procedural luck.
fn stamp_film_dual_rivers(world: &mut VoxelWorld, island: IslandSpec) -> usize {
    let mut n = 0usize;
    // Shelf below the floating deck — visible from the wide painting cam.
    // Raise slightly so painting_hero looking under the near rim catches both.
    let base_y = (island.deck_y - 22).max(crate::terrain::WATER_LEVEL + 4);
    let ax = island.cx - 20;
    let az = island.cz + 35;
    for i in 0..110 {
        let t = i as f32 * 0.12;
        // Plasma filament (cyan) — meandering +X/+Z, 3-wide for bloom read.
        let px = ax + (i as f32 * 0.9 + t.sin() * 3.0).round() as i32;
        let pz = az + (i as f32 * 0.55 + (t * 1.3).cos() * 4.0).round() as i32;
        for dy in 0..4 {
            for ox in 0..3 {
                if world.edit_set_voxel(px + ox, base_y + dy, pz, BlockType::PlasmaFlow.into()) {
                    n += 1;
                }
            }
        }
        // Parallel lava ribbon — thicker/hotter so it survives lavapipe crush.
        let lx = px + 5 + (t * 0.7).cos().round() as i32;
        let lz = pz - 4 + (t * 0.9).sin().round() as i32;
        for dy in 0..4 {
            for oz in 0..3 {
                if world.edit_set_voxel(lx, base_y + dy, lz + oz, BlockType::Lava.into()) {
                    n += 1;
                }
            }
            // Amber neon core so the lava channel stays orange under bloom.
            if world.edit_set_voxel(lx + 1, base_y + dy, lz + 1, BlockType::NeonAmber.into()) {
                n += 1;
            }
        }
    }
    // Second dual pair crossing the vista for density (painting mid-ground).
    let bx = island.cx + 25;
    let bz = island.cz + 50;
    for i in 0..70 {
        let t = i as f32 * 0.15;
        let px = bx + (i as f32 * 0.7 - t.sin() * 2.5).round() as i32;
        let pz = bz + (i as f32 * 0.85).round() as i32;
        for dy in 0..3 {
            for ox in 0..2 {
                if world.edit_set_voxel(px + ox, base_y + dy, pz, BlockType::PlasmaFlow.into()) {
                    n += 1;
                }
                if world.edit_set_voxel(px + 4 + ox, base_y + dy, pz - 2, BlockType::Lava.into()) {
                    n += 1;
                }
            }
        }
    }
    // Third short pair under painting_hero look target (cx+22, cz+25 shelf).
    let cx = island.cx + 10;
    let cz = island.cz + 20;
    for i in 0..40 {
        let t = i as f32 * 0.18;
        let px = cx + (i as f32 * 0.85).round() as i32;
        let pz = cz + (t.sin() * 3.0).round() as i32;
        for dy in 0..3 {
            let _ = world.edit_set_voxel(px, base_y + dy + 2, pz, BlockType::PlasmaFlow.into());
            let _ = world.edit_set_voxel(px + 3, base_y + dy + 2, pz + 2, BlockType::Lava.into());
            n += 2;
        }
    }
    // Hero ribbon aimed at painting cam look (deck+24,-6,+28) — thick dual
    // channel hanging just under the near archipelago rim.
    let hx = island.cx + 18;
    let hz = island.cz + 40;
    let hy = (island.deck_y - 14).max(crate::terrain::WATER_LEVEL + 4);
    for i in 0..80 {
        let t = i as f32 * 0.11;
        let px = hx + (i as f32 * 0.75 + t.sin() * 2.0).round() as i32;
        let pz = hz + ((i as f32 * 0.35) + (t * 1.1).cos() * 3.0).round() as i32;
        for dy in 0..5 {
            for ox in 0..4 {
                if world.edit_set_voxel(px + ox, hy + dy, pz, BlockType::PlasmaFlow.into()) {
                    n += 1;
                }
            }
            for oz in 0..4 {
                if world.edit_set_voxel(px + 6, hy + dy, pz + oz, BlockType::Lava.into()) {
                    n += 1;
                }
            }
            if world.edit_set_voxel(px + 7, hy + dy, pz + 1, BlockType::NeonAmber.into()) {
                n += 1;
            }
        }
    }
    n
}

fn stamp_film_vista_island(
    world: &mut VoxelWorld,
    cx: i32,
    cz: i32,
    deck_y: i32,
    rx: i32,
    rz: i32,
    crystal: BlockType,
) -> usize {
    let mut n = 0usize;
    let keel = 9i32;
    // Film-safe keel accents: never magenta (reads as pink HUD streaks).
    let accent = match crystal {
        BlockType::CrystalMagenta | BlockType::NeonMagenta => BlockType::CrystalVerdant,
        other => other,
    };
    for dx in -rx..=rx {
        for dz in -rz..=rz {
            let nx = dx as f32 / rx.max(1) as f32;
            let nz = dz as f32 / rz.max(1) as f32;
            if nx * nx + nz * nz > 1.05 {
                continue;
            }
            let x = cx + dx;
            let z = cz + dz;
            let edge = (nx * nx + nz * nz).sqrt();
            let thickness = ((keel as f32) * (0.35 + 0.65 * (1.0 - edge).max(0.0))).round() as i32;
            // Clear air column above deck so grass isn't swallowed by terrain.
            for y in (deck_y + 1)..=(deck_y + 4) {
                let _ = world.edit_set_voxel(x, y, z, AIR);
            }
            if world.edit_set_voxel(x, deck_y, z, BlockType::Grass.into()) {
                n += 1;
            }
            for dy in 1..=thickness.max(2) {
                let y = deck_y - dy;
                let block = if dy == thickness {
                    if ((dx + dz) & 1) == 0 {
                        BlockType::LuminiteCrystal
                    } else {
                        accent
                    }
                } else if dy + 1 == thickness {
                    BlockType::Crystal
                } else if edge > 0.55 {
                    BlockType::ShipHullAlloy
                } else {
                    BlockType::Stone
                };
                if world.edit_set_voxel(x, y, z, block.into()) {
                    n += 1;
                }
            }
        }
    }
    n
}

fn stamp_film_skyway_stub(
    world: &mut VoxelWorld,
    ax: i32,
    az: i32,
    ay: i32,
    bx: i32,
    bz: i32,
    by: i32,
) -> usize {
    let mut n = 0usize;
    let steps = 36i32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (ax as f32 + (bx - ax) as f32 * t).round() as i32;
        let z = (az as f32 + (bz - az) as f32 * t).round() as i32;
        let y = (ay as f32 + (by - ay) as f32 * t).round() as i32;
        for ox in 0..2 {
            for oz in 0..2 {
                if world.edit_set_voxel(x + ox, y, z + oz, BlockType::SkywayDeck.into()) {
                    n += 1;
                }
                // Sparse neon trim — full NeonCyan web white-outs painting_hero.
                if (i + ox + oz) % 3 == 0 {
                    if world.edit_set_voxel(x + ox, y + 1, z + oz, BlockType::NeonCyan.into()) {
                        n += 1;
                    }
                }
            }
        }
    }
    n
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
    // Park real shuttle in painting frustum (proxy carries the plume read).
    let pos = Vec3::new(
        island.cx as f32 - 8.0,
        island.deck_y as f32 + 30.0,
        island.cz as f32 + 92.0,
    );
    // Nose toward −X so wakes stream +X toward a rear-quarter camera.
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
    commands
        .entity(entity)
        .insert((FilmShuttleMarker, FilmSkywayFx));
    info!("FILM: spawned hero shuttle at {pos:?}");
}

/// Film-only oversized box silhouettes so biped vs multi-leg reads on lavapipe
/// even when voxel figures crush into grass/station clutter.
fn film_spawn_silhouettes(
    mut commands: Commands,
    mut film: ResMut<FilmRuntime>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !film.enabled || film.finished || film.silhouettes_spawned {
        return;
    }
    let Some(island) = film.island else {
        return;
    };
    // Wait for the clean combat slab so feet land on dark hull, not grass.
    if !film.combat_slab_staged {
        return;
    }
    film.silhouettes_spawned = true;

    let deck = Vec3::new(
        island.cx as f32 + 0.5,
        island.deck_y as f32 + 1.0,
        island.cz as f32 + 0.5,
    );
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let marine_body = materials.add(sil_mat(
        Color::srgb(0.94, 0.97, 1.0),
        LinearRgba::rgb(0.18, 0.22, 0.28),
    ));
    let marine_dark = materials.add(sil_mat(Color::srgb(0.05, 0.06, 0.09), LinearRgba::BLACK));
    let marine_visor = materials.add(sil_mat(
        Color::srgb(0.05, 1.0, 1.0),
        LinearRgba::rgb(1.0, 5.5, 6.5),
    ));
    let alien_body = materials.add(sil_mat(
        Color::srgb(1.0, 0.78, 0.28),
        LinearRgba::rgb(0.55, 0.28, 0.04),
    ));
    let alien_leg = materials.add(sil_mat(
        Color::srgb(0.72, 0.42, 0.12),
        LinearRgba::rgb(0.12, 0.05, 0.01),
    ));
    let alien_crest = materials.add(sil_mat(
        Color::srgb(1.0, 0.35, 0.85),
        LinearRgba::rgb(4.0, 0.8, 3.5),
    ));
    let crew_body = materials.add(sil_mat(
        Color::srgb(0.55, 0.62, 0.72),
        LinearRgba::rgb(0.08, 0.10, 0.14),
    ));
    let crew_visor = materials.add(sil_mat(
        Color::srgb(0.05, 1.0, 1.0),
        LinearRgba::rgb(0.6, 4.0, 5.0),
    ));
    // Unlit underside plates REPLACE downward keel faces in screenshots.
    // Combat-slab keeps only top-2 solid layers under grass; lavapipe still
    // inks those bottoms (and hollow cave walls) black unless an opaque
    // unlit VOLUME fills the carved keel so no lit voxel face remains in view.
    // `deck` is at deck_y+1; kept solids end ~deck_y-2 → relative y ≈ -3.
    let keel = island.keel_depth as f32;
    let flush_y = -2.4_f32; // just under grass / kept solids
    let deep_y = -(keel.max(8.0) * 0.85);
    let crystal_plate = materials.add(sil_mat(
        Color::srgb(0.42, 0.88, 0.95),
        LinearRgba::rgb(1.0, 3.2, 3.8),
    ));
    let verdant_plate = materials.add(sil_mat(
        Color::srgb(0.32, 0.85, 0.48),
        LinearRgba::rgb(0.4, 2.8, 1.2),
    ));
    let alloy_plate = materials.add(sil_mat(
        Color::srgb(0.82, 0.78, 0.58),
        LinearRgba::rgb(1.1, 0.95, 0.55),
    ));
    let luminite_plate = materials.add(sil_mat(
        Color::srgb(0.48, 0.95, 0.92),
        LinearRgba::rgb(1.2, 3.5, 3.6),
    ));
    let rx = (island.radius_x as f32).max(18.0);
    let rz = (island.radius_z as f32).max(16.0);
    // Solid unlit keel volume — fills the hollow so cave walls never read ink.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: crystal_plate.clone(),
            transform: Transform::from_translation(deck + Vec3::new(0.0, -5.5, 1.0))
                .with_scale(Vec3::new(rx * 2.35, 11.0, rz * 2.35)),
            ..default()
        },
        FilmSilhouette,
        FilmKeelHelper,
        Name::new("FilmKeelUndersideDeck"),
    ));
    // Alloy under-cap for crystal→alloy depth on the rim profile.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: alloy_plate.clone(),
            transform: Transform::from_translation(deck + Vec3::new(0.0, deep_y - 0.5, 1.0))
                .with_scale(Vec3::new(rx * 1.85, 2.2, rz * 1.85)),
            ..default()
        },
        FilmSilhouette,
        FilmKeelHelper,
        Name::new("FilmKeelUndersideAlloy"),
    ));
    // Tile accent plates across the flush deck for crystal/alloy variety.
    let step = 4.5_f32;
    let mut plate_i = 0usize;
    let mut x = -rx;
    while x <= rx {
        let mut z = -rz;
        while z <= rz {
            let nx = x / rx;
            let nz = z / rz;
            if nx * nx + nz * nz <= 1.05 {
                let mat = match plate_i % 4 {
                    0 => &crystal_plate,
                    1 => &alloy_plate,
                    2 => &luminite_plate,
                    _ => &verdant_plate,
                };
                let edge = (nx * nx + nz * nz).sqrt();
                let dy = -edge * 0.35;
                commands.spawn((
                    PbrBundle {
                        mesh: cube.clone(),
                        material: mat.clone(),
                        transform: Transform::from_translation(
                            deck + Vec3::new(x, flush_y - 0.2 + dy, z + 1.0),
                        )
                        .with_scale(Vec3::new(
                            step * 1.08,
                            1.35,
                            step * 1.08,
                        )),
                        ..default()
                    },
                    FilmSilhouette,
                    FilmKeelHelper,
                    Name::new(format!("FilmKeelUnderside{plate_i}")),
                ));
                plate_i += 1;
            }
            z += step;
        }
        x += step;
    }
    // Camera-facing keel body slab under the near rim (deck_keel SE cam).
    let body = materials.add(sil_mat(
        Color::srgb(0.62, 0.88, 0.95),
        LinearRgba::rgb(1.2, 3.8, 4.5),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: body,
            transform: Transform::from_translation(deck + Vec3::new(6.0, flush_y - 3.5, 12.0))
                .with_scale(Vec3::new(rx * 1.15, keel.max(7.0) * 0.55, rz * 0.45))
                .with_rotation(Quat::from_rotation_x(-0.08)),
            ..default()
        },
        FilmSilhouette,
        FilmKeelHelper,
        Name::new("FilmKeelBodySlab"),
    ));
    // Vertical keel skirt — high painting cams see SIDE faces as black
    // silhouettes; unlit vertical panels give those faces readable color.
    let skirt = materials.add(sil_mat(
        Color::srgb(0.55, 0.90, 0.95),
        LinearRgba::rgb(1.0, 3.5, 4.0),
    ));
    for (ox, oz, sx, sz) in [
        (0.0_f32, rz * 0.95, rx * 1.7, 0.7),
        (0.0, -rz * 0.95, rx * 1.7, 0.7),
        (rx * 0.95, 0.0, 0.7, rz * 1.7),
        (-rx * 0.95, 0.0, 0.7, rz * 1.7),
    ] {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: skirt.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, -5.0, oz))
                    .with_scale(Vec3::new(sx, 8.5, sz)),
                ..default()
            },
            FilmSilhouette,
            FilmKeelHelper,
            Name::new("FilmKeelSkirt"),
        ));
    }

    // Cyan crystal lip along the near rim (camera side of deck_keel).
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: luminite_plate.clone(),
            transform: Transform::from_translation(deck + Vec3::new(2.0, flush_y + 0.6, rz * 0.85))
                .with_scale(Vec3::new(rx * 1.5, 1.6, 2.4)),
            ..default()
        },
        FilmSilhouette,
        FilmKeelHelper,
        Name::new("FilmKeelCyanLip"),
    ));

    // Satellite continuous underside volumes — every vista island offset so
    // painting_hero high cams don't leave ink-black neighbor keels.
    for (si, (sx, sz, srx, srz, lift)) in [
        (28.0_f32, 18.0, 16.0, 13.0, -1.0),
        (-22.0, 28.0, 15.0, 12.0, 1.0),
        (42.0, 22.0, 14.0, 11.0, -1.0),
        (-38.0, 36.0, 14.0, 11.0, 2.0),
        (58.0, -12.0, 13.0, 12.0, -3.0),
        (-24.0, -48.0, 15.0, 11.0, 1.0),
        (32.0, 58.0, 13.0, 11.0, 0.0),
        (78.0, 34.0, 12.0, 10.0, -2.0),
        (-62.0, 12.0, 13.0, 10.0, 1.0),
        (18.0, 78.0, 12.0, 9.0, -1.0),
        (-52.0, -28.0, 11.0, 11.0, -2.0),
        (88.0, -35.0, 10.0, 12.0, 0.0),
        (-78.0, 48.0, 11.0, 9.0, -3.0),
        (48.0, 88.0, 11.0, 10.0, 2.0),
        (-15.0, 62.0, 13.0, 10.0, -1.0),
        (65.0, 55.0, 10.0, 9.0, 1.0),
        (-40.0, 75.0, 11.0, 11.0, 0.0),
        (12.0, 42.0, 12.0, 10.0, -2.0),
        (-8.0, 48.0, 11.0, 9.0, 1.0),
        (55.0, 12.0, 10.0, 9.0, 0.0),
        (72.0, 68.0, 9.0, 9.0, -1.0),
        (-55.0, 58.0, 10.0, 8.0, 2.0),
        (38.0, 72.0, 10.0, 9.0, -2.0),
        (-30.0, 18.0, 11.0, 9.0, 0.0),
        (95.0, 18.0, 9.0, 10.0, 1.0),
        (22.0, 32.0, 14.0, 11.0, 0.0),
        (-18.0, 40.0, 12.0, 10.0, -1.0),
        (36.0, 48.0, 11.0, 10.0, 1.0),
        (8.0, 55.0, 10.0, 9.0, -2.0),
        (50.0, 38.0, 9.0, 9.0, 0.0),
        (28.0, 44.0, 12.0, 10.0, -1.0),
        (-12.0, 52.0, 11.0, 9.0, 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        // Skip keel volumes that overlap the dual-river shelf corridor so
        // plasma/lava ribbons aren't buried inside opaque sat boxes.
        // Shelf ≈ x∈[-8,50], z∈[52,72] relative to deck.
        let in_river_corridor = sx > -35.0 && sx < 55.0 && sz > 50.0 && sz < 85.0;
        if in_river_corridor {
            continue;
        }
        let sat_deck = deck + Vec3::new(sx, lift, sz);
        let mat = if si % 2 == 0 {
            luminite_plate.clone()
        } else {
            alloy_plate.clone()
        };
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: mat,
                transform: Transform::from_translation(sat_deck + Vec3::new(0.0, -4.8, 0.0))
                    .with_scale(Vec3::new(srx * 2.35, 9.0, srz * 2.35)),
                ..default()
            },
            FilmSilhouette,
            FilmKeelHelper,
            Name::new(format!("FilmSatKeelDeck{si}")),
        ));
        for (ox, oz, sxv, szv) in [
            (0.0_f32, srz * 0.98, srx * 1.9, 0.75),
            (0.0, -srz * 0.98, srx * 1.9, 0.75),
            (srx * 0.98, 0.0, 0.75, srz * 1.9),
            (-srx * 0.98, 0.0, 0.75, srz * 1.9),
        ] {
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: crystal_plate.clone(),
                    transform: Transform::from_translation(sat_deck + Vec3::new(ox, -4.5, oz))
                        .with_scale(Vec3::new(sxv, 9.5, szv)),
                    ..default()
                },
                FilmSilhouette,
                FilmKeelHelper,
                Name::new(format!("FilmSatKeelSkirt{si}")),
            ));
        }
    }

    // Film-only hanging crystal spikes (cyan/verdant only — no magenta/pink).
    let crystal_a = materials.add(sil_mat(
        Color::srgb(0.55, 1.0, 0.95),
        LinearRgba::rgb(1.6, 6.5, 6.0),
    ));
    let crystal_b = materials.add(sil_mat(
        Color::srgb(0.25, 0.95, 0.55),
        LinearRgba::rgb(0.8, 5.5, 2.2),
    ));
    let keel_y = deep_y - 1.5;
    for (i, (ox, oz, mat)) in [
        (-10.0_f32, 8.0, &crystal_a),
        (0.0, 12.0, &crystal_b),
        (11.0, 6.0, &crystal_a),
        (-6.0, -8.0, &crystal_b),
        (8.0, -6.0, &crystal_a),
        (3.0, 1.0, &crystal_b),
        (-2.0, 14.0, &crystal_a),
        (5.0, 16.0, &crystal_b),
        (14.0, 10.0, &crystal_a),
        (-12.0, 4.0, &crystal_b),
    ]
    .into_iter()
    .enumerate()
    {
        let h = 3.8 + (i as f32) * 0.35;
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, keel_y - h * 0.35, oz))
                    .with_scale(Vec3::new(0.85, h, 0.85))
                    .with_rotation(Quat::from_rotation_z(
                        0.18 * if i % 2 == 0 { 1.0 } else { -1.0 },
                    )),
                ..default()
            },
            FilmSilhouette,
            FilmKeelHelper,
            Name::new(format!("FilmKeelCrystal{i}")),
        ));
    }

    // Unlit dual-river ribbons for painting_hero / dual_rivers.
    // CRITICAL: keep them OUTSIDE keel-volume AABBs. Prior ribbons at
    // (ox≈16..90, oz≈30, y=-8) sat inside sat keel boxes and orange→0.
    // Painting cam ≈ deck+(-38,22,72) → look+(22,-10,36): place a clear
    // shelf band between cam and archipelago at high-Z / mid-X, below decks.
    let plasma_mat = materials.add(sil_mat(
        Color::srgb(0.05, 0.92, 1.0),
        LinearRgba::rgb(1.5, 9.0, 12.0),
    ));
    let lava_mat = materials.add(sil_mat(
        // Hot molten orange — keep G channel so ACES doesn't crush to brown/black.
        Color::srgb(1.0, 0.55, 0.04),
        LinearRgba::rgb(22.0, 7.0, 0.08),
    ));
    // Clear shelf for dedicated dual_rivers — parallel cyan + orange lanes.
    let river_y = 2.0_f32;
    for i in 0..22 {
        let t = i as f32;
        let ox = -8.0 + t * 3.2;
        let oz = 64.0 + (t * 0.4).sin() * 3.0;
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: plasma_mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, river_y, oz))
                    .with_scale(Vec3::new(10.0, 5.0, 5.5))
                    .with_rotation(Quat::from_rotation_y(0.35)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmPlasmaRibbon{i}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: lava_mat.clone(),
                transform: Transform::from_translation(
                    deck + Vec3::new(ox + 8.0, river_y + 0.4, oz - 8.0),
                )
                .with_scale(Vec3::new(12.0, 6.5, 6.5))
                .with_rotation(Quat::from_rotation_y(0.32)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmLavaRibbon{i}")),
        ));
    }
    // Painting lower-left dual lanes — beside grass crowns (z≈100), not under them.
    for (i, ox) in [-36.0_f32, -24.0, -12.0, 0.0, 12.0].into_iter().enumerate() {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: plasma_mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, 8.0, 102.0))
                    .with_scale(Vec3::new(16.0, 4.5, 6.0))
                    .with_rotation(Quat::from_rotation_y(0.5)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmPlasmaPaint{i}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: lava_mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox + 10.0, 7.0, 96.0))
                    .with_scale(Vec3::new(18.0, 5.5, 7.0))
                    .with_rotation(Quat::from_rotation_y(0.5)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmLavaPaint{i}")),
        ));
    }
    // Tall dual sheets for upward painting look (left of green crown).
    for (i, ox) in [-30.0_f32, -14.0, 2.0].into_iter().enumerate() {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: plasma_mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, 10.0, 88.0))
                    .with_scale(Vec3::new(6.0, 14.0, 18.0)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmPlasmaSheet{i}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: lava_mat.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox + 12.0, 9.0, 84.0))
                    .with_scale(Vec3::new(7.5, 16.0, 20.0)),
                ..default()
            },
            FilmSilhouette,
            FilmRiverRibbon,
            Name::new(format!("FilmLavaSheet{i}")),
        ));
    }

    // Combat figures on floating sky arena (no island keel/lattice in frustum).
    let combat_pad = deck + Vec3::new(8.0, 48.0, 130.0);
    // Deck pad for turrets / fire-lane (shot 3) — separate from floating arena.
    let pad = deck + Vec3::new(0.0, 0.0, 14.0);
    let stage_mat = materials.add(sil_mat(
        Color::srgb(0.82, 0.86, 0.92),
        LinearRgba::rgb(0.45, 0.5, 0.6),
    ));
    let stage_edge = materials.add(sil_mat(
        Color::srgb(0.25, 0.85, 0.95),
        LinearRgba::rgb(1.0, 4.0, 5.0),
    ));
    // Bright unlit stage — isolated from island voxels; shot 2 only.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: stage_mat,
            transform: Transform::from_translation(combat_pad + Vec3::new(0.5, -0.7, 1.0))
                .with_scale(Vec3::new(36.0, 1.6, 24.0)),
            ..default()
        },
        FilmSilhouette,
        FilmCombatFx,
        FilmCombatStage,
        FilmCombatArena,
        Name::new("FilmCombatStageFloor"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: stage_edge,
            transform: Transform::from_translation(combat_pad + Vec3::new(0.5, -0.1, 1.0))
                .with_scale(Vec3::new(37.0, 0.4, 25.0)),
            ..default()
        },
        FilmSilhouette,
        FilmCombatFx,
        FilmCombatStage,
        FilmCombatArena,
        Name::new("FilmCombatStageRim"),
    ));
    let arena_marine = spawn_film_marine(
        &mut commands,
        &cube,
        &marine_body,
        &marine_dark,
        &marine_visor,
        combat_pad + Vec3::new(-5.5, 0.0, 1.5),
        3.6,
    );
    commands.entity(arena_marine).insert(FilmCombatArena);
    let arena_alien = spawn_film_alien(
        &mut commands,
        &cube,
        &alien_body,
        &alien_leg,
        &alien_crest,
        combat_pad + Vec3::new(6.5, 0.0, 2.5),
        3.8,
    );
    commands.entity(arena_alien).insert(FilmCombatArena);
    // Painting-scale giants — low-left grass edge so mid-right mountain owns the mass.
    let vista_marine = spawn_film_marine(
        &mut commands,
        &cube,
        &marine_body,
        &marine_dark,
        &marine_visor,
        deck + Vec3::new(-28.0, 6.0, 108.0),
        6.5,
    );
    commands.entity(vista_marine).insert(FilmCombatVista);
    let vista_alien = spawn_film_alien(
        &mut commands,
        &cube,
        &alien_body,
        &alien_leg,
        &alien_crest,
        deck + Vec3::new(-16.0, 6.0, 112.0),
        7.0,
    );
    commands.entity(vista_alien).insert(FilmCombatVista);
    spawn_film_crew(
        &mut commands,
        &cube,
        &crew_body,
        &crew_visor,
        deck + Vec3::new(-6.0, 0.0, -10.0),
    );

    // Oversized mountain tunnel portal (−Z) — glowing cyan mouth into dark bore.
    let tunnel_rock = materials.add(sil_mat(
        Color::srgb(0.40, 0.34, 0.28),
        LinearRgba::rgb(0.05, 0.04, 0.03),
    ));
    let tunnel_cyan = materials.add(sil_mat(
        Color::srgb(0.25, 0.95, 1.0),
        LinearRgba::rgb(1.5, 7.0, 8.0),
    ));
    let tunnel_dark = materials.add(sil_mat(Color::srgb(0.04, 0.04, 0.06), LinearRgba::BLACK));
    let tunnel_glow = materials.add(sil_mat(
        Color::srgb(0.45, 1.0, 0.95),
        LinearRgba::rgb(2.5, 8.0, 7.5),
    ));
    spawn_film_tunnel_portal(
        &mut commands,
        &cube,
        &tunnel_rock,
        &tunnel_cyan,
        &tunnel_dark,
        &tunnel_glow,
        deck,
    );
    // Cyan monorail into the tunnel mouth — next painting gap after towers.
    let rail_metal = materials.add(sil_mat(
        Color::srgb(0.62, 0.66, 0.72),
        LinearRgba::rgb(0.25, 0.28, 0.35),
    ));
    spawn_film_tunnel_rails(
        &mut commands,
        &cube,
        &tunnel_cyan,
        &rail_metal,
        &tunnel_glow,
        deck,
    );

    // Pad turrets with muzzle flashes + tracers aimed at the alien.
    let alien_world = pad + Vec3::new(6.0, 4.0, 2.5);
    let turret_hull = materials.add(sil_mat(
        Color::srgb(0.55, 0.58, 0.62),
        LinearRgba::rgb(0.15, 0.16, 0.18),
    ));
    let turret_muzzle = materials.add(sil_mat(
        Color::srgb(1.0, 0.95, 0.20),
        LinearRgba::rgb(10.0, 8.0, 0.6),
    ));
    // Pure-R + deep-orange beams — orange G channel keeps warm body under ACES.
    let turret_tracer = materials.add(sil_mat(
        Color::srgb(1.0, 0.0, 0.0),
        LinearRgba::rgb(7.5, 0.05, 0.0),
    ));
    let turret_orange = materials.add(sil_mat(
        Color::srgb(1.0, 0.22, 0.0),
        LinearRgba::rgb(9.0, 1.6, 0.0),
    ));
    spawn_film_turrets_firing(
        &mut commands,
        &cube,
        &turret_hull,
        &turret_muzzle,
        &turret_tracer,
        &turret_orange,
        pad,
        alien_world,
    );

    // Cheap docked-fighter swarm (+X / painting frustum) with cyan plumes.
    let fighter_hull = materials.add(sil_mat(
        Color::srgb(0.78, 0.82, 0.88),
        LinearRgba::rgb(0.35, 0.40, 0.55),
    ));
    spawn_film_fighter_swarm(&mut commands, &cube, &fighter_hull, &tunnel_cyan, deck);

    // Crystal towers — next painting gap after fighter swarm readability.
    let tower_crystal = materials.add(sil_mat(
        Color::srgb(0.55, 1.0, 1.0),
        LinearRgba::rgb(2.5, 8.0, 9.0),
    ));
    let tower_verdant = materials.add(sil_mat(
        Color::srgb(0.40, 0.98, 0.85),
        LinearRgba::rgb(1.5, 6.0, 5.5),
    ));
    spawn_film_crystal_towers(&mut commands, &cube, &tower_crystal, &tower_verdant, deck);

    // Bright grass caps in the painting frustum — cliff tops must read green.
    let grass_mat = materials.add(sil_mat(
        Color::srgb(0.18, 0.92, 0.22),
        LinearRgba::rgb(1.2, 6.5, 1.0),
    ));
    let grass_dark = materials.add(sil_mat(
        Color::srgb(0.10, 0.48, 0.14),
        LinearRgba::rgb(0.3, 2.0, 0.35),
    ));
    spawn_film_grass_caps(&mut commands, &cube, &grass_mat, &grass_dark, deck);

    // Mountain station mass — mesh mountain for painting + dedicated station beat.
    // Dark rock mountain — wider base, shorter crown (installation, not white tower).
    // Dark rock mountain — darkrock dominates; alloy only as faint crown lip.
    // Carved darkrock (readable stone gray) — not void-black, not tan alloy.
    let station_dark = materials.add(sil_mat(
        Color::srgb(0.30, 0.28, 0.32),
        LinearRgba::rgb(0.12, 0.10, 0.14),
    ));
    let station_alloy = materials.add(sil_mat(
        Color::srgb(0.38, 0.34, 0.30),
        LinearRgba::rgb(0.18, 0.14, 0.10),
    ));
    let station_neon = materials.add(sil_mat(
        Color::srgb(0.25, 0.95, 0.90),
        LinearRgba::rgb(1.5, 6.5, 6.0),
    ));
    spawn_film_station_mountain(
        &mut commands,
        &cube,
        &station_dark,
        &station_alloy,
        &station_neon,
        deck,
    );

    // Skyway lattice + oversized shuttle proxy with cyan plumes.
    let skyway_deck = materials.add(sil_mat(
        Color::srgb(0.55, 0.58, 0.65),
        LinearRgba::rgb(0.4, 0.45, 0.55),
    ));
    let skyway_cyan = materials.add(sil_mat(
        Color::srgb(0.20, 0.95, 1.0),
        LinearRgba::rgb(2.0, 9.0, 10.0),
    ));
    let shuttle_hull = materials.add(sil_mat(
        Color::srgb(0.82, 0.86, 0.92),
        LinearRgba::rgb(0.5, 0.55, 0.7),
    ));
    spawn_film_skyway_and_shuttle_proxy(
        &mut commands,
        &cube,
        &skyway_deck,
        &skyway_cyan,
        &shuttle_hull,
        deck,
    );

    // Glowing cyan waterfall off +X rim into the plasma shelf.
    let fall_cyan = materials.add(sil_mat(
        Color::srgb(0.20, 0.95, 1.0),
        LinearRgba::rgb(2.0, 8.5, 10.0),
    ));
    let fall_deep = materials.add(sil_mat(
        Color::srgb(0.10, 0.55, 1.0),
        LinearRgba::rgb(0.8, 4.0, 9.0),
    ));
    let fall_mist = materials.add(sil_mat(
        Color::srgb(0.70, 0.98, 1.0),
        LinearRgba::rgb(3.5, 7.0, 8.0),
    ));
    spawn_film_waterfall(
        &mut commands,
        &cube,
        &fall_cyan,
        &fall_deep,
        &fall_mist,
        &plasma_mat,
        &grass_mat,
        deck,
    );

    // Film planet sphere+ring — painting backup; hidden on dedicated sky shot.
    let planet_sphere = meshes.add(Sphere::new(1.0).mesh().ico(3).expect("ico 3"));
    let planet_body = materials.add(sil_mat(
        Color::srgb(0.98, 0.55, 0.98),
        LinearRgba::rgb(9.0, 3.5, 11.0),
    ));
    let planet_ring = materials.add(sil_mat(
        Color::srgb(1.0, 0.92, 0.78),
        LinearRgba::rgb(10.0, 7.5, 5.0),
    ));
    let planet_dark = materials.add(sil_mat(
        Color::srgb(0.22, 0.06, 0.26),
        LinearRgba::rgb(0.5, 0.12, 0.6),
    ));
    spawn_film_planet_proxy(
        &mut commands,
        &planet_sphere,
        &cube,
        &planet_body,
        &planet_ring,
        &planet_dark,
        deck,
    );

    info!(
        "FILM: spawned mesh silhouettes + keel underside plates on combat slab ({}, {})",
        island.cx, island.cz
    );
}

fn sil_mat(base: Color, emissive: LinearRgba) -> StandardMaterial {
    StandardMaterial {
        base_color: base,
        emissive,
        // Unlit so lavapipe lighting cannot crush limb edges into pad clutter.
        unlit: true,
        alpha_mode: AlphaMode::Opaque,
        metallic: 0.0,
        perceptual_roughness: 1.0,
        reflectance: 0.0,
        ..default()
    }
}

fn spawn_film_marine(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    body: &Handle<StandardMaterial>,
    dark: &Handle<StandardMaterial>,
    visor: &Handle<StandardMaterial>,
    origin: Vec3,
    scale: f32,
) -> Entity {
    // Oversized so limbs survive mid-distance lavapipe framing.
    let root = commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(origin).with_scale(Vec3::splat(scale)),
                ..default()
            },
            FilmSilhouette,
            FilmCombatFx,
            Name::new("FilmMarineSilhouette"),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        // Legs — wide stance so biped reads (not a pedestal).
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: dark.clone(),
                transform: Transform::from_translation(Vec3::new(-0.55, 0.75, 0.0))
                    .with_scale(Vec3::new(0.38, 1.5, 0.42)),
                ..default()
            },
            Name::new("MarineLegL"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: dark.clone(),
                transform: Transform::from_translation(Vec3::new(0.55, 0.75, 0.0))
                    .with_scale(Vec3::new(0.38, 1.5, 0.42)),
                ..default()
            },
            Name::new("MarineLegR"),
        ));
        // Hip gap marker (dark) so legs don't fuse into one column.
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: dark.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.45, 0.0))
                    .with_scale(Vec3::new(0.95, 0.22, 0.4)),
                ..default()
            },
            Name::new("MarineHips"),
        ));
        // Torso
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 2.05, 0.0))
                    .with_scale(Vec3::new(0.95, 1.55, 0.55)),
                ..default()
            },
            Name::new("MarineTorso"),
        ));
        // Head
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 3.15, 0.05))
                    .with_scale(Vec3::new(0.55, 0.55, 0.55)),
                ..default()
            },
            Name::new("MarineHead"),
        ));
        // Visor strip
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: visor.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 3.15, 0.32))
                    .with_scale(Vec3::new(0.42, 0.16, 0.08)),
                ..default()
            },
            Name::new("MarineVisor"),
        ));
        // Rifle along +X
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: dark.clone(),
                transform: Transform::from_translation(Vec3::new(1.15, 2.15, 0.15))
                    .with_scale(Vec3::new(1.85, 0.16, 0.16)),
                ..default()
            },
            Name::new("MarineRifle"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: visor.clone(),
                transform: Transform::from_translation(Vec3::new(2.05, 2.15, 0.15))
                    .with_scale(Vec3::new(0.22, 0.14, 0.14)),
                ..default()
            },
            Name::new("MarineMuzzle"),
        ));
    });
    root
}

fn spawn_film_alien(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    body: &Handle<StandardMaterial>,
    leg: &Handle<StandardMaterial>,
    crest: &Handle<StandardMaterial>,
    origin: Vec3,
    scale: f32,
) -> Entity {
    let root = commands
        .spawn((
            SpatialBundle {
                // Face the marine (−X) so splayed legs read in the side-on two-shot.
                transform: Transform::from_translation(origin)
                    .with_rotation(Quat::from_rotation_y(std::f32::consts::PI))
                    .with_scale(Vec3::splat(scale)),
                ..default()
            },
            FilmSilhouette,
            FilmCombatFx,
            Name::new("FilmAlienSilhouette"),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        // Raised central body (wider than marine torso)
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.85, 0.0))
                    .with_scale(Vec3::new(1.55, 1.35, 1.35)),
                ..default()
            },
            Name::new("AlienBody"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: crest.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 2.85, 0.0))
                    .with_scale(Vec3::new(0.55, 0.45, 0.4)),
                ..default()
            },
            Name::new("AlienCrest"),
        ));
        // Six splayed legs — unmistakable multi-leg vs biped marine.
        let legs = [
            Vec3::new(-2.25, 0.55, -1.95),
            Vec3::new(2.25, 0.55, -1.95),
            Vec3::new(-2.55, 0.55, 0.05),
            Vec3::new(2.55, 0.55, 0.05),
            Vec3::new(-2.05, 0.55, 2.05),
            Vec3::new(2.05, 0.55, 2.05),
        ];
        for (i, offset) in legs.iter().enumerate() {
            let mid = Vec3::new(offset.x * 0.5, 1.35, offset.z * 0.5);
            p.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: leg.clone(),
                    transform: Transform::from_translation(*offset)
                        .with_scale(Vec3::new(0.32, 1.65, 0.32)),
                    ..default()
                },
                Name::new(format!("AlienLeg{i}")),
            ));
            p.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: leg.clone(),
                    transform: Transform::from_translation(mid)
                        .with_scale(Vec3::new(0.48, 0.55, 0.48)),
                    ..default()
                },
                Name::new(format!("AlienJoint{i}")),
            ));
        }
    });
    root
}

fn spawn_film_crew(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    body: &Handle<StandardMaterial>,
    visor: &Handle<StandardMaterial>,
    origin: Vec3,
) {
    let root = commands
        .spawn((
            SpatialBundle {
                transform: Transform::from_translation(origin).with_scale(Vec3::splat(1.25)),
                ..default()
            },
            FilmSilhouette,
            Name::new("FilmCrewSilhouette"),
        ))
        .id();
    commands.entity(root).with_children(|p| {
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(-0.28, 0.65, 0.0))
                    .with_scale(Vec3::new(0.32, 1.3, 0.36)),
                ..default()
            },
            Name::new("CrewLegL"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.28, 0.65, 0.0))
                    .with_scale(Vec3::new(0.32, 1.3, 0.36)),
                ..default()
            },
            Name::new("CrewLegR"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 1.75, 0.0))
                    .with_scale(Vec3::new(0.72, 1.25, 0.42)),
                ..default()
            },
            Name::new("CrewTorso"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: body.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 2.65, 0.0))
                    .with_scale(Vec3::new(0.42, 0.42, 0.42)),
                ..default()
            },
            Name::new("CrewHead"),
        ));
        p.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: visor.clone(),
                transform: Transform::from_translation(Vec3::new(0.0, 2.65, 0.24))
                    .with_scale(Vec3::new(0.32, 0.12, 0.06)),
                ..default()
            },
            Name::new("CrewVisor"),
        ));
    });
}

fn spawn_film_tunnel_portal(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    rock: &Handle<StandardMaterial>,
    cyan: &Handle<StandardMaterial>,
    dark: &Handle<StandardMaterial>,
    glow: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Mountain face on −Z of the station — oversized so the portal reads
    // in a dedicated hero frame (voxel portal alone is tiny).
    let mouth = deck + Vec3::new(0.0, 8.0, -18.0);
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: rock.clone(),
            transform: Transform::from_translation(mouth + Vec3::new(0.0, 4.0, -5.0))
                .with_scale(Vec3::new(36.0, 28.0, 16.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelMountain"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(mouth + Vec3::new(0.0, 2.0, -1.5))
                .with_scale(Vec3::new(12.0, 14.0, 10.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelBore"),
    ));
    // Thick cyan arch — must dominate the mouth silhouette.
    for (ox, oy, sx, sy) in [
        (-7.5_f32, 2.0, 2.4, 16.0),
        (7.5, 2.0, 2.4, 16.0),
        (0.0, 11.0, 18.0, 2.6),
        (0.0, -6.0, 18.0, 2.6),
    ] {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: cyan.clone(),
                transform: Transform::from_translation(mouth + Vec3::new(ox, oy, 2.5))
                    .with_scale(Vec3::new(sx, sy, 2.4)),
                ..default()
            },
            FilmSilhouette,
            FilmTunnelFx,
            Name::new("FilmTunnelArch"),
        ));
    }
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: glow.clone(),
            transform: Transform::from_translation(mouth + Vec3::new(0.0, 2.0, 1.0))
                .with_scale(Vec3::new(9.0, 11.0, 1.6)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelGlow"),
    ));

    // Painting-facing portal/holo arch — left of mountain so shot 8 reads the mouth.
    let paint_mouth = deck + Vec3::new(-42.0, 18.0, 78.0);
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: rock.clone(),
            transform: Transform::from_translation(paint_mouth)
                .with_scale(Vec3::new(22.0, 18.0, 8.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmPaintPortalRock"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(paint_mouth + Vec3::new(0.0, 0.0, 3.0))
                .with_scale(Vec3::new(12.0, 12.0, 4.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmPaintPortalBore"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(paint_mouth + Vec3::new(0.0, 0.0, 5.5))
                .with_scale(Vec3::new(10.0, 10.0, 1.2)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmPaintPortalArch"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: glow.clone(),
            transform: Transform::from_translation(paint_mouth + Vec3::new(0.0, 0.0, 6.5))
                .with_scale(Vec3::new(7.0, 8.0, 1.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmPaintPortalGlow"),
    ));
}

fn spawn_film_turrets_firing(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    hull: &Handle<StandardMaterial>,
    muzzle: &Handle<StandardMaterial>,
    tracer: &Handle<StandardMaterial>,
    orange: &Handle<StandardMaterial>,
    pad: Vec3,
    alien: Vec3,
) {
    for (i, origin) in [
        pad + Vec3::new(-9.0, 0.0, 8.0),
        pad + Vec3::new(9.5, 0.0, 7.5),
        pad + Vec3::new(-2.0, 0.0, 10.0),
    ]
    .into_iter()
    .enumerate()
    {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: hull.clone(),
                transform: Transform::from_translation(origin + Vec3::new(0.0, 1.4, 0.0))
                    .with_scale(Vec3::new(2.8, 2.8, 2.8)),
                ..default()
            },
            FilmSilhouette,
            FilmTurretFx,
            Name::new(format!("FilmTurretBase{i}")),
        ));
        let aim = alien + Vec3::new(0.0, 1.0, 0.0);
        let to_alien = (aim - (origin + Vec3::new(0.0, 3.2, 0.0))).normalize_or_zero();
        let barrel_mid = origin + Vec3::new(0.0, 3.2, 0.0) + to_alien * 3.0;
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: hull.clone(),
                transform: Transform::from_translation(barrel_mid)
                    .looking_at(aim, Vec3::Y)
                    .with_scale(Vec3::new(0.9, 0.9, 5.5)),
                ..default()
            },
            FilmSilhouette,
            FilmTurretFx,
            Name::new(format!("FilmTurretBarrel{i}")),
        ));
        let flash = origin + Vec3::new(0.0, 3.2, 0.0) + to_alien * 6.2;
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: muzzle.clone(),
                transform: Transform::from_translation(flash).with_scale(Vec3::new(5.5, 5.5, 5.5)),
                ..default()
            },
            FilmSilhouette,
            FilmTurretFx,
            Name::new(format!("FilmTurretMuzzle{i}")),
        ));
        // Fat red core + orange sheath so warm body survives ACES wash.
        let beam_mid = flash.lerp(aim, 0.55);
        let beam_len = flash.distance(aim).max(10.0);
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: tracer.clone(),
                transform: Transform::from_translation(beam_mid)
                    .looking_at(aim, Vec3::Y)
                    .with_scale(Vec3::new(4.2, 4.2, beam_len)),
                ..default()
            },
            FilmSilhouette,
            FilmTurretFx,
            Name::new(format!("FilmTurretBeam{i}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: orange.clone(),
                transform: Transform::from_translation(beam_mid)
                    .looking_at(aim, Vec3::Y)
                    .with_scale(Vec3::new(6.4, 6.4, beam_len * 0.92)),
                ..default()
            },
            FilmSilhouette,
            FilmTurretFx,
            Name::new(format!("FilmTurretBeamOrange{i}")),
        ));
        // Hero beam slab parked in the fire lane for the turrets shot —
        // guaranteed red/orange body even if thin tracers crush under ACES.
        if i == 0 {
            let hero_mid = flash.lerp(aim, 0.48);
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: orange.clone(),
                    transform: Transform::from_translation(hero_mid)
                        .looking_at(aim, Vec3::Y)
                        .with_scale(Vec3::new(7.5, 7.5, beam_len * 1.05)),
                    ..default()
                },
                FilmSilhouette,
                FilmTurretFx,
                Name::new("FilmTurretHeroBeam"),
            ));
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: tracer.clone(),
                    transform: Transform::from_translation(hero_mid)
                        .looking_at(aim, Vec3::Y)
                        .with_scale(Vec3::new(4.0, 4.0, beam_len * 1.05)),
                    ..default()
                },
                FilmSilhouette,
                FilmTurretFx,
                Name::new("FilmTurretHeroBeamCore"),
            ));
        }
        for s in 1..8 {
            let t = s as f32 / 8.0;
            let p = flash.lerp(aim, t);
            let mat = if s % 2 == 0 { orange } else { tracer };
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: mat.clone(),
                    transform: Transform::from_translation(p)
                        .with_scale(Vec3::splat(3.2 - t * 1.2)),
                    ..default()
                },
                FilmSilhouette,
                FilmTurretFx,
                Name::new(format!("FilmTurretSpark{i}_{s}")),
            ));
        }
    }
}

fn spawn_film_tunnel_rails(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    cyan: &Handle<StandardMaterial>,
    metal: &Handle<StandardMaterial>,
    glow: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Axis-aligned dual cyan rails (no looking_at) so lavapipe always shows
    // long Z-runs into the −Z tunnel mouth. Raised above deck grass lip.
    let y = 5.8_f32;
    let z0 = 18.0_f32;
    let z1 = -14.0_f32;
    let mid_z = (z0 + z1) * 0.5;
    let len = (z0 - z1).abs();
    // Approach plate.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: metal.clone(),
            transform: Transform::from_translation(deck + Vec3::new(0.0, y - 0.9, mid_z))
                .with_scale(Vec3::new(11.0, 0.6, len + 2.0)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelRailDeck"),
    ));
    for (ox, name) in [(-3.2_f32, "L"), (3.2, "R")] {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: cyan.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, y, mid_z))
                    .with_scale(Vec3::new(2.2, 1.5, len)),
                ..default()
            },
            FilmSilhouette,
            FilmTunnelFx,
            Name::new(format!("FilmTunnelRail{name}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: glow.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, y + 0.85, mid_z))
                    .with_scale(Vec3::new(1.0, 0.55, len * 0.98)),
                ..default()
            },
            FilmSilhouette,
            FilmTunnelFx,
            Name::new(format!("FilmTunnelRailGlow{name}")),
        ));
    }
    // Center strip.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: glow.clone(),
            transform: Transform::from_translation(deck + Vec3::new(0.0, y - 0.2, mid_z))
                .with_scale(Vec3::new(3.0, 0.4, len * 0.96)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelRailCenter"),
    ));
    // Discrete cyan sleepers — unmistakable track rhythm into the bore.
    let steps = 10;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let z = z0 + (z1 - z0) * t;
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: cyan.clone(),
                transform: Transform::from_translation(deck + Vec3::new(0.0, y + 0.3, z))
                    .with_scale(Vec3::new(8.5, 0.7, 1.4)),
                ..default()
            },
            FilmSilhouette,
            FilmTunnelFx,
            Name::new(format!("FilmTunnelRailSleeper{i}")),
        ));
        for ox in [-5.0_f32, 5.0] {
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: metal.clone(),
                    transform: Transform::from_translation(deck + Vec3::new(ox, y + 2.6, z))
                        .with_scale(Vec3::new(0.9, 5.2, 0.9)),
                    ..default()
                },
                FilmSilhouette,
                FilmTunnelFx,
                Name::new(format!("FilmTunnelRailPylon{i}")),
            ));
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: cyan.clone(),
                    transform: Transform::from_translation(deck + Vec3::new(ox, y + 5.4, z))
                        .with_scale(Vec3::new(1.5, 1.0, 1.5)),
                    ..default()
                },
                FilmSilhouette,
                FilmTunnelFx,
                Name::new(format!("FilmTunnelRailCap{i}")),
            ));
        }
    }
    // Mouth threshold — fat cyan lip into the bore.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(deck + Vec3::new(0.0, y + 0.5, z1 + 4.0))
                .with_scale(Vec3::new(12.0, 1.6, 2.8)),
            ..default()
        },
        FilmSilhouette,
        FilmTunnelFx,
        Name::new("FilmTunnelRailThreshold"),
    ));
}

fn spawn_film_fighter_swarm(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    hull: &Handle<StandardMaterial>,
    cyan: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // High open-sky V for dedicated; painting wing stays lower in frustum.
    for (i, (ox, oy, oz, yaw, scale, sky)) in [
        // Dedicated core — high altitude, clear of islands/tunnel
        (20.0_f32, 52.0, 18.0, -0.55, 1.15, true),
        (30.0, 54.0, 22.0, -0.45, 1.2, true),
        (40.0, 53.0, 20.0, -0.50, 1.15, true),
        (50.0, 56.0, 24.0, -0.40, 1.25, true),
        (25.0, 50.0, 28.0, -0.60, 1.15, true),
        (45.0, 57.0, 30.0, -0.35, 1.2, true),
        (35.0, 58.0, 16.0, -0.48, 1.3, true),
        (55.0, 55.0, 26.0, -0.42, 1.15, true),
        // Painting-hero wing — left-mid above crown / skyway.
        (-20.0_f32, 36.0, 100.0, -0.70, 1.55, false),
        (-8.0, 40.0, 108.0, -0.65, 1.6, false),
        (6.0, 38.0, 104.0, -0.55, 1.5, false),
        (-28.0, 34.0, 94.0, -0.75, 1.45, false),
        (16.0, 42.0, 98.0, -0.50, 1.5, false),
        (28.0, 44.0, 90.0, -0.45, 1.4, false),
    ]
    .into_iter()
    .enumerate()
    {
        let p = deck + Vec3::new(ox, oy, oz);
        let s = scale;
        let plume_dir = Quat::from_rotation_y(yaw) * Vec3::new(-1.0, 0.0, 0.0);
        let parts = [
            (
                hull.clone(),
                Transform::from_translation(p)
                    .with_scale(Vec3::new(8.0 * s, 2.4 * s, 3.5 * s))
                    .with_rotation(Quat::from_rotation_y(yaw)),
                format!("FilmFighter{i}"),
            ),
            (
                cyan.clone(),
                Transform::from_translation(p + plume_dir * (9.0 * s))
                    .with_scale(Vec3::new(18.0 * s, 2.6 * s, 2.4 * s))
                    .with_rotation(Quat::from_rotation_y(yaw)),
                format!("FilmFighterPlume{i}"),
            ),
            (
                hull.clone(),
                Transform::from_translation(p)
                    .with_scale(Vec3::new(3.2 * s, 0.7 * s, 8.0 * s))
                    .with_rotation(Quat::from_rotation_y(yaw)),
                format!("FilmFighterWing{i}"),
            ),
            (
                cyan.clone(),
                Transform::from_translation(
                    p - plume_dir * (5.0 * s) + Vec3::new(0.0, 0.8 * s, 0.0),
                )
                .with_scale(Vec3::new(1.8 * s, 1.4 * s, 1.8 * s)),
                format!("FilmFighterNose{i}"),
            ),
        ];
        for (mat, tf, name) in parts {
            let id = commands
                .spawn((
                    PbrBundle {
                        mesh: cube.clone(),
                        material: mat,
                        transform: tf,
                        ..default()
                    },
                    FilmSilhouette,
                    FilmFighterFx,
                    Name::new(name),
                ))
                .id();
            if sky {
                commands.entity(id).insert(FilmFighterSky);
            }
        }
    }
}

fn spawn_film_planet_proxy(
    commands: &mut Commands,
    sphere: &Handle<Mesh>,
    cube: &Handle<Mesh>,
    body: &Handle<StandardMaterial>,
    ring: &Handle<StandardMaterial>,
    dark: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Large sphere + tilted ring in painting upper frustum (sky path backup).
    let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
    let center = deck + planet_dir * 88.0 + Vec3::new(18.0, 55.0, -12.0);
    let tilt = Quat::from_rotation_x(0.95) * Quat::from_rotation_z(-0.22);
    commands.spawn((
        PbrBundle {
            mesh: sphere.clone(),
            material: body.clone(),
            transform: Transform::from_translation(center).with_scale(Vec3::splat(48.0)),
            ..default()
        },
        FilmSilhouette,
        FilmPlanetProxy,
        Name::new("FilmPlanetBody"),
    ));
    // Limb-darkening rim (slightly larger dark shell cue).
    commands.spawn((
        PbrBundle {
            mesh: sphere.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(center - planet_dir * 2.0)
                .with_scale(Vec3::splat(50.0)),
            ..default()
        },
        FilmSilhouette,
        FilmPlanetProxy,
        Name::new("FilmPlanetLimb"),
    ));
    // Outer ring + Cassini gap (inner dark annulus).
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: ring.clone(),
            transform: Transform::from_translation(center)
                .with_rotation(tilt)
                .with_scale(Vec3::new(118.0, 2.2, 118.0)),
            ..default()
        },
        FilmSilhouette,
        FilmPlanetProxy,
        Name::new("FilmPlanetRing"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(center)
                .with_rotation(tilt)
                .with_scale(Vec3::new(72.0, 2.8, 72.0)),
            ..default()
        },
        FilmSilhouette,
        FilmPlanetProxy,
        Name::new("FilmPlanetCassini"),
    ));
    // Outer bright band beyond Cassini.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: ring.clone(),
            transform: Transform::from_translation(center)
                .with_rotation(tilt)
                .with_scale(Vec3::new(120.0, 1.2, 120.0)),
            ..default()
        },
        FilmSilhouette,
        FilmPlanetProxy,
        Name::new("FilmPlanetRingOuter"),
    ));
}

fn spawn_film_waterfall(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    cyan: &Handle<StandardMaterial>,
    deep: &Handle<StandardMaterial>,
    mist: &Handle<StandardMaterial>,
    pool: &Handle<StandardMaterial>,
    grass: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // OPEN-AIR hero fall on far +X/+Z — clear of sat keels so dedicated cam
    // sees a vertical cyan sheet into a plasma pool (painting also catches it).
    let cliff = deck + Vec3::new(62.0, 4.0, 66.0);
    let lip = cliff + Vec3::new(0.0, 8.0, 2.0);
    let bottom = cliff + Vec3::new(2.0, -22.0, 6.0);
    let mid = lip.lerp(bottom, 0.48);
    let fall_h = lip.distance(bottom).max(28.0);
    // Rock face the water falls off.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: deep.clone(),
            transform: Transform::from_translation(cliff + Vec3::new(-4.0, 0.0, -2.0))
                .with_scale(Vec3::new(10.0, 28.0, 8.0)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallCliff"),
    ));
    // Verdant grass lip on cliff top.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: grass.clone(),
            transform: Transform::from_translation(lip + Vec3::new(-2.0, 0.5, -1.0))
                .with_scale(Vec3::new(14.0, 2.4, 9.0)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallGrassLip"),
    ));
    // Main vertical cyan sheet — fills dedicated frame.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(mid).with_scale(Vec3::new(12.0, fall_h, 4.5)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallSheet"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: deep.clone(),
            transform: Transform::from_translation(mid + Vec3::new(0.5, 0.0, 1.2))
                .with_scale(Vec3::new(6.0, fall_h * 0.98, 2.5)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallCore"),
    ));
    for (i, ox) in [(-6.5_f32), (6.5)].into_iter().enumerate() {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: cyan.clone(),
                transform: Transform::from_translation(mid + Vec3::new(ox, -1.0, 1.8))
                    .with_scale(Vec3::new(4.0, fall_h * 0.9, 3.0)),
                ..default()
            },
            FilmSilhouette,
            FilmWaterfallFx,
            Name::new(format!("FilmWaterfallRibbon{i}")),
        ));
    }
    // Splash pool.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: pool.clone(),
            transform: Transform::from_translation(bottom + Vec3::new(0.0, 1.5, 3.0))
                .with_scale(Vec3::new(20.0, 5.0, 14.0)),
            ..default()
        },
        FilmSilhouette,
        FilmRiverRibbon,
        Name::new("FilmWaterfallPool"),
    ));
    for i in 0..14 {
        let tt = i as f32 / 13.0;
        let p = lip.lerp(bottom, tt) + Vec3::new((i as f32 * 0.85).sin() * 2.5, 0.0, 1.0);
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: mist.clone(),
                transform: Transform::from_translation(p).with_scale(Vec3::splat(3.0 - tt * 0.9)),
                ..default()
            },
            FilmSilhouette,
            FilmWaterfallFx,
            Name::new(format!("FilmWaterfallMist{i}")),
        ));
    }
    // Painting-frustum secondary fall closer to cam / crown.
    let lip2 = deck + Vec3::new(48.0, 4.0, 88.0);
    let mid2 = lip2 + Vec3::new(4.0, -16.0, 6.0);
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(mid2).with_scale(Vec3::new(8.0, 24.0, 3.5)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallSheetB"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: grass.clone(),
            transform: Transform::from_translation(lip2).with_scale(Vec3::new(11.0, 2.2, 5.0)),
            ..default()
        },
        FilmSilhouette,
        FilmWaterfallFx,
        Name::new("FilmWaterfallGrassLipB"),
    ));
}

fn spawn_film_station_mountain(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    dark: &Handle<StandardMaterial>,
    alloy: &Handle<StandardMaterial>,
    neon: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Wide mountain installation mid-right — finish14 scale, darkrock-dominant.
    let base = deck + Vec3::new(34.0, 0.0, 70.0);
    for (i, (y, sx, sz, h)) in [
        (6.0_f32, 72.0, 58.0, 14.0),
        (18.0, 62.0, 50.0, 14.0),
        (30.0, 50.0, 40.0, 12.0),
        (42.0, 38.0, 30.0, 12.0),
        (52.0, 26.0, 22.0, 10.0),
        (60.0, 16.0, 14.0, 8.0),
    ]
    .into_iter()
    .enumerate()
    {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                // Darkrock through mid tiers — alloy only on tip lip.
                material: if i < 5 { dark.clone() } else { alloy.clone() },
                transform: Transform::from_translation(base + Vec3::new(0.0, y, 0.0))
                    .with_scale(Vec3::new(sx, h, sz)),
                ..default()
            },
            FilmSilhouette,
            FilmStationFx,
            Name::new(format!("FilmStationTier{i}")),
        ));
    }
    // Low neon rim lights — installation cue without tall white spires.
    for (i, ox) in [-8.0_f32, 0.0, 8.0].into_iter().enumerate() {
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: neon.clone(),
                transform: Transform::from_translation(base + Vec3::new(ox, 66.0, 2.0))
                    .with_scale(Vec3::new(3.5, 6.0, 3.5)),
                ..default()
            },
            FilmSilhouette,
            FilmStationFx,
            Name::new(format!("FilmStationRim{i}")),
        ));
    }
    // Broad buttress shoulders toward painting cam.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(base + Vec3::new(-28.0, 16.0, 24.0))
                .with_scale(Vec3::new(28.0, 34.0, 20.0)),
            ..default()
        },
        FilmSilhouette,
        FilmStationFx,
        Name::new("FilmStationButtress"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(base + Vec3::new(26.0, 12.0, 20.0))
                .with_scale(Vec3::new(24.0, 28.0, 18.0)),
            ..default()
        },
        FilmSilhouette,
        FilmStationFx,
        Name::new("FilmStationButtressB"),
    ));
    // Cliff face plate — dark carved mountain face toward cam.
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: dark.clone(),
            transform: Transform::from_translation(base + Vec3::new(-4.0, 24.0, 30.0))
                .with_scale(Vec3::new(36.0, 30.0, 10.0)),
            ..default()
        },
        FilmSilhouette,
        FilmStationFx,
        Name::new("FilmStationCliffFace"),
    ));
}

fn spawn_film_skyway_and_shuttle_proxy(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    deck_mat: &Handle<StandardMaterial>,
    cyan: &Handle<StandardMaterial>,
    hull: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Fat left-mid skyway spans — must read at painting_hero distance.
    for (i, (ax, az, bx, bz, y)) in [
        (-36.0_f32, 108.0, 18.0, 88.0, 22.0),
        (-28.0, 118.0, 26.0, 94.0, 28.0),
        (-20.0, 100.0, 30.0, 78.0, 34.0),
        (-40.0, 96.0, 8.0, 104.0, 18.0),
        (-12.0, 112.0, 38.0, 82.0, 40.0),
        (-32.0, 90.0, 14.0, 70.0, 30.0),
    ]
    .into_iter()
    .enumerate()
    {
        let a = deck + Vec3::new(ax, y, az);
        let b = deck + Vec3::new(bx, y + 2.0, bz);
        let mid = a.lerp(b, 0.5);
        let len = a.distance(b).max(10.0);
        let dir = (b - a).normalize_or_zero();
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: deck_mat.clone(),
                transform: Transform::from_translation(mid)
                    .looking_to(dir, Vec3::Y)
                    .with_scale(Vec3::new(9.0, 3.5, len)),
                ..default()
            },
            FilmSilhouette,
            FilmSkywayFx,
            Name::new(format!("FilmSkywayDeck{i}")),
        ));
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: cyan.clone(),
                transform: Transform::from_translation(mid + Vec3::Y * 2.4)
                    .looking_to(dir, Vec3::Y)
                    .with_scale(Vec3::new(3.2, 4.5, len * 0.98)),
                ..default()
            },
            FilmSilhouette,
            FilmSkywayFx,
            Name::new(format!("FilmSkywayRail{i}")),
        ));
    }
    // Oversized shuttle + fat cyan plumes in left-mid painting frustum.
    let shuttle = deck + Vec3::new(-8.0, 30.0, 92.0);
    let yaw = -0.55_f32;
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: hull.clone(),
            transform: Transform::from_translation(shuttle)
                .with_scale(Vec3::new(34.0, 10.0, 14.0))
                .with_rotation(Quat::from_rotation_y(yaw)),
            ..default()
        },
        FilmSilhouette,
        FilmSkywayFx,
        FilmShuttleMarker,
        Name::new("FilmShuttleProxyHull"),
    ));
    let plume_dir = Quat::from_rotation_y(yaw) * Vec3::new(-1.0, 0.0, 0.0);
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(shuttle + plume_dir * 28.0)
                .with_scale(Vec3::new(52.0, 8.0, 8.0))
                .with_rotation(Quat::from_rotation_y(yaw)),
            ..default()
        },
        FilmSilhouette,
        FilmSkywayFx,
        FilmShuttleMarker,
        Name::new("FilmShuttleProxyPlume"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: cyan.clone(),
            transform: Transform::from_translation(shuttle + plume_dir * 18.0 + Vec3::Y * 4.0)
                .with_scale(Vec3::new(40.0, 5.5, 5.5))
                .with_rotation(Quat::from_rotation_y(yaw)),
            ..default()
        },
        FilmSilhouette,
        FilmSkywayFx,
        FilmShuttleMarker,
        Name::new("FilmShuttleProxyPlumeB"),
    ));
    commands.spawn((
        PbrBundle {
            mesh: cube.clone(),
            material: hull.clone(),
            transform: Transform::from_translation(shuttle)
                .with_scale(Vec3::new(12.0, 3.0, 26.0))
                .with_rotation(Quat::from_rotation_y(yaw)),
            ..default()
        },
        FilmSilhouette,
        FilmSkywayFx,
        FilmShuttleMarker,
        Name::new("FilmShuttleProxyWing"),
    ));
}

fn spawn_film_grass_caps(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    grass: &Handle<StandardMaterial>,
    soil: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Near-cam verdant crowns — fill lower third; left lip + right island.
    for (i, (ox, oz, w, d, lift)) in [
        (-12.0_f32, 108.0, 52.0, 40.0, 6.0),
        (10.0, 104.0, 46.0, 36.0, 5.5),
        (-32.0, 100.0, 40.0, 32.0, 5.0),
        (8.0, 94.0, 42.0, 34.0, 6.0),
        (28.0, 98.0, 36.0, 28.0, 5.0),
        (-18.0, 90.0, 34.0, 28.0, 4.5),
        (10.0, 82.0, 30.0, 24.0, 4.0),
        (28.0, 86.0, 28.0, 22.0, 4.0),
        (-6.0, 76.0, 26.0, 20.0, 3.0),
        (40.0, 80.0, 24.0, 20.0, 3.5),
        // Extra lower-left verdant shelf so grass isn't only a right corner chip.
        (-40.0, 112.0, 36.0, 28.0, 5.5),
        (-22.0, 114.0, 30.0, 24.0, 6.0),
        (18.0, 110.0, 28.0, 22.0, 5.0),
    ]
    .into_iter()
    .enumerate()
    {
        let y0 = lift;
        // Soil undercroft.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: soil.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, y0, oz))
                    .with_scale(Vec3::new(w + 2.0, 4.0, d + 2.0)),
                ..default()
            },
            FilmSilhouette,
            FilmGrassFx,
            Name::new(format!("FilmGrassSoil{i}")),
        ));
        // Bright lawn lid (readable when cam is high enough).
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: grass.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, y0 + 3.0, oz))
                    .with_scale(Vec3::new(w, 3.2, d)),
                ..default()
            },
            FilmSilhouette,
            FilmGrassFx,
            Name::new(format!("FilmGrassCap{i}")),
        ));
        // Cliff face toward painting cam (+Z) so green reads even looking up.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: grass.clone(),
                transform: Transform::from_translation(
                    deck + Vec3::new(ox, y0 + 2.0, oz + d * 0.45),
                )
                .with_scale(Vec3::new(w * 0.9, 9.0, 3.5)),
                ..default()
            },
            FilmSilhouette,
            FilmGrassFx,
            Name::new(format!("FilmGrassCliff{i}")),
        ));
        // Side face (−X) for three-quarter readability.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: grass.clone(),
                transform: Transform::from_translation(
                    deck + Vec3::new(ox - w * 0.42, y0 + 2.0, oz),
                )
                .with_scale(Vec3::new(3.2, 8.5, d * 0.85)),
                ..default()
            },
            FilmSilhouette,
            FilmGrassFx,
            Name::new(format!("FilmGrassSide{i}")),
        ));
        // Rim tufts.
        for (j, (tx, tz)) in [
            (w * 0.32, d * 0.28),
            (-w * 0.28, d * 0.22),
            (w * 0.12, -d * 0.30),
            (-w * 0.22, -d * 0.18),
        ]
        .into_iter()
        .enumerate()
        {
            commands.spawn((
                PbrBundle {
                    mesh: cube.clone(),
                    material: grass.clone(),
                    transform: Transform::from_translation(
                        deck + Vec3::new(ox + tx, y0 + 6.5, oz + tz),
                    )
                    .with_scale(Vec3::new(3.0, 5.5, 3.0)),
                    ..default()
                },
                FilmSilhouette,
                FilmGrassFx,
                Name::new(format!("FilmGrassTuft{i}_{j}")),
            ));
        }
    }
}

fn spawn_film_crystal_towers(
    commands: &mut Commands,
    cube: &Handle<Mesh>,
    crystal: &Handle<StandardMaterial>,
    verdant: &Handle<StandardMaterial>,
    deck: Vec3,
) {
    // Tapered cyan spires (base→mid→shaft→tip) so dedicated + painting read as towers.
    let ice = verdant;
    let lift = 8.0; // float grove above pad clutter
    for (i, (ox, oz, h)) in [
        (18.0_f32, 18.0, 44.0),
        (28.0, 24.0, 58.0),
        (22.0, 32.0, 40.0),
        (36.0, 28.0, 64.0),
        (14.0, 40.0, 50.0),
        (42.0, 36.0, 54.0),
        (10.0, 28.0, 42.0),
        (48.0, 22.0, 46.0),
    ]
    .into_iter()
    .enumerate()
    {
        let lean = 0.08 * if i % 2 == 0 { 1.0 } else { -1.0 };
        let rot = Quat::from_rotation_z(lean);
        // Broad base plinth.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: crystal.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, lift + h * 0.12, oz))
                    .with_scale(Vec3::new(7.5, h * 0.24, 7.5))
                    .with_rotation(rot),
                ..default()
            },
            FilmSilhouette,
            FilmCrystalFx,
            Name::new(format!("FilmCrystalBase{i}")),
        ));
        // Mid facet block.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: crystal.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, lift + h * 0.38, oz))
                    .with_scale(Vec3::new(5.2, h * 0.32, 5.2))
                    .with_rotation(rot * Quat::from_rotation_y(0.35)),
                ..default()
            },
            FilmSilhouette,
            FilmCrystalFx,
            Name::new(format!("FilmCrystalMid{i}")),
        ));
        // Narrow upper shaft.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: ice.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, lift + h * 0.68, oz))
                    .with_scale(Vec3::new(3.2, h * 0.36, 3.2))
                    .with_rotation(rot),
                ..default()
            },
            FilmSilhouette,
            FilmCrystalFx,
            Name::new(format!("FilmCrystalShaft{i}")),
        ));
        // Faceted diamond tip.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: ice.clone(),
                transform: Transform::from_translation(deck + Vec3::new(ox, lift + h + 4.0, oz))
                    .with_scale(Vec3::new(6.5, 7.0, 6.5))
                    .with_rotation(Quat::from_rotation_y(0.55) * Quat::from_rotation_z(0.55)),
                ..default()
            },
            FilmSilhouette,
            FilmCrystalFx,
            Name::new(format!("FilmCrystalTip{i}")),
        ));
        // Angled secondary shard.
        commands.spawn((
            PbrBundle {
                mesh: cube.clone(),
                material: crystal.clone(),
                transform: Transform::from_translation(
                    deck + Vec3::new(ox + 4.0, lift + h * 0.50, oz - 2.5),
                )
                .with_scale(Vec3::new(2.2, h * 0.48, 2.2))
                .with_rotation(Quat::from_rotation_z(-lean * 2.2) * Quat::from_rotation_x(0.25)),
                ..default()
            },
            FilmSilhouette,
            FilmCrystalFx,
            Name::new(format!("FilmCrystalShard{i}")),
        ));
    }
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
        name: "island_deck_keel",
    },
    FilmShot {
        name: "combat_pad_silhouettes",
    },
    FilmShot {
        name: "turrets_firing",
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
        name: "painting_hero",
    },
    FilmShot {
        name: "dual_rivers",
    },
    FilmShot {
        name: "waterfall_cyan",
    },
    FilmShot {
        name: "fighter_swarm",
    },
    FilmShot {
        name: "crystal_towers",
    },
    FilmShot {
        name: "station_mass",
    },
    FilmShot {
        name: "skyway_shuttle",
    },
];

fn film_toggle_helpers(
    film: Res<FilmRuntime>,
    mut keel_helpers: Query<
        &mut Visibility,
        (
            With<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut river_ribbons: Query<
        &mut Visibility,
        (
            With<FilmRiverRibbon>,
            Without<FilmKeelHelper>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut turret_fx: Query<
        &mut Visibility,
        (
            With<FilmTurretFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut planet_proxy: Query<
        &mut Visibility,
        (
            With<FilmPlanetProxy>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut crystal_fx: Query<
        &mut Visibility,
        (
            With<FilmCrystalFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut fighter_fx: Query<
        (&mut Visibility, Option<&FilmFighterSky>),
        (
            With<FilmFighterFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut waterfall_fx: Query<
        &mut Visibility,
        (
            With<FilmWaterfallFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut grass_fx: Query<
        &mut Visibility,
        (
            With<FilmGrassFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut combat_fx: Query<
        (
            &mut Visibility,
            Option<&FilmCombatVista>,
            Option<&FilmCombatArena>,
        ),
        (
            With<FilmCombatFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut station_fx: Query<
        &mut Visibility,
        (
            With<FilmStationFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmSkywayFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut skyway_fx: Query<
        &mut Visibility,
        (
            With<FilmSkywayFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmTunnelFx>,
        ),
    >,
    mut tunnel_fx: Query<
        &mut Visibility,
        (
            With<FilmTunnelFx>,
            Without<FilmKeelHelper>,
            Without<FilmRiverRibbon>,
            Without<FilmTurretFx>,
            Without<FilmPlanetProxy>,
            Without<FilmCrystalFx>,
            Without<FilmFighterFx>,
            Without<FilmWaterfallFx>,
            Without<FilmGrassFx>,
            Without<FilmCombatFx>,
            Without<FilmStationFx>,
            Without<FilmSkywayFx>,
        ),
    >,
) {
    if !film.enabled || film.finished {
        return;
    }
    let show_keel = film.shot_index == 1;
    for mut vis in keel_helpers.iter_mut() {
        *vis = if show_keel {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let show_rivers = matches!(film.shot_index, 8 | 9 | 10);
    for mut vis in river_ribbons.iter_mut() {
        *vis = if show_rivers {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Turrets off combat_pad — biped vs alien must read without beam clutter.
    let show_turrets = matches!(film.shot_index, 3 | 8);
    for mut vis in turret_fx.iter_mut() {
        *vis = if show_turrets {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let show_proxy = film.shot_index == 8;
    for mut vis in planet_proxy.iter_mut() {
        *vis = if show_proxy {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let show_crystal = matches!(film.shot_index, 8 | 12);
    for mut vis in crystal_fx.iter_mut() {
        *vis = if show_crystal {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Fighters: painting wing + dedicated sky — NOT combat_pad.
    for (mut vis, sky) in fighter_fx.iter_mut() {
        let show = match film.shot_index {
            8 => true,
            11 => sky.is_some(),
            _ => false,
        };
        *vis = if show {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let show_waterfall = matches!(film.shot_index, 8 | 10);
    for mut vis in waterfall_fx.iter_mut() {
        *vis = if show_waterfall {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Grass off on dual_rivers + fighter + station so those heroes stay clean.
    let show_grass = matches!(film.shot_index, 0 | 8 | 10);
    for mut vis in grass_fx.iter_mut() {
        *vis = if show_grass {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Combat: arena = shot 2 only; vista giants = painting only (never steal mountain).
    for (mut vis, vista, arena) in combat_fx.iter_mut() {
        let show = match film.shot_index {
            2 => arena.is_some(),
            8 => vista.is_some(),
            _ => false,
        };
        *vis = if show {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Station mountain: painting + dedicated station beat.
    let show_station = matches!(film.shot_index, 8 | 13);
    for mut vis in station_fx.iter_mut() {
        *vis = if show_station {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Skyway spans + cyan-plume shuttle proxy: painting, shuttle perch, dedicated.
    let show_skyway = matches!(film.shot_index, 6 | 8 | 14);
    for mut vis in skyway_fx.iter_mut() {
        *vis = if show_skyway {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    // Tunnel portal + monorail: dedicated tunnel + painting; OFF combat_pad.
    let show_tunnel = matches!(film.shot_index, 5 | 8);
    for mut vis in tunnel_fx.iter_mut() {
        *vis = if show_tunnel {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn film_override_sky_clear(film: Res<FilmRuntime>, mut clear_color: ResMut<ClearColor>) {
    if !film.enabled || film.finished || !film.ready_to_roll {
        return;
    }
    // Must win against daynight::update_sun (Update) so nebula volume reads.
    match film.shot_index {
        7 | 8 => {
            clear_color.0 = Color::srgb(0.06, 0.03, 0.14);
        }
        9 | 10 => {
            clear_color.0 = Color::srgb(0.14, 0.10, 0.22);
        }
        _ => {}
    }
}

fn film_drive_camera(
    time: Res<Time>,
    world: Res<VoxelWorld>,
    streamer: Res<ChunkStreamer>,
    mut film: ResMut<FilmRuntime>,
    mut ambient: ResMut<AmbientLight>,
    mut clear_color: ResMut<ClearColor>,
    mut mode: ResMut<ModeContext>,
    mut toolbelt: ResMut<ToolbeltState>,
    mut sun_q: Query<&mut DirectionalLight, With<Sun>>,
    mut query: Query<(&mut Transform, &mut Player, &mut Projection)>,
    mut bloom_q: Query<&mut BloomSettings, With<Player>>,
    mut fill_q: Query<
        &mut Transform,
        (
            With<FilmFillLight>,
            Without<Player>,
            Without<FilmRimLight>,
            Without<FilmUnderKeelLight>,
            Without<FilmFigureKeyLight>,
        ),
    >,
    mut rim_q: Query<
        &mut Transform,
        (
            With<FilmRimLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmUnderKeelLight>,
            Without<FilmFigureKeyLight>,
        ),
    >,
    mut under_q: Query<
        &mut Transform,
        (
            With<FilmUnderKeelLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmRimLight>,
            Without<FilmFigureKeyLight>,
        ),
    >,
    mut figure_q: Query<
        &mut Transform,
        (
            With<FilmFigureKeyLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmRimLight>,
            Without<FilmUnderKeelLight>,
        ),
    >,
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
        let target: f32 = match film.shot_index {
            0 => 50.0,  // grass lawn stand-off
            1 => 54.0,  // deck + keel profile
            2 => 52.0,  // full-body combat two-shot (slightly wider after declutter)
            3 => 46.0,  // along-axis turret beams + yellow muzzles
            4 => 48.0,  // crew pair
            5 => 55.0,  // tunnel portal + cyan rails
            6 => 46.0,  // shuttle rear-quarter
            7 => 48.0,  // planet + rings fill
            8 => 54.0,  // painting: planet + green crown + crystals
            9 => 52.0,  // dual plasma + lava rivers
            10 => 48.0, // cyan waterfall cascade
            11 => 36.0, // fighter swarm — tight on open-sky V
            12 => 44.0, // crystal tower side elevation
            13 => 50.0, // station mountain stand-off
            14 => 48.0, // skyway span + cyan plume shuttle
            _ => 52.0,
        };
        persp.fov = target.to_radians();
    }
    if let Ok(mut bloom) = bloom_q.get_single_mut() {
        // Keep bloom low so non-emissive limbs / grass survive lavapipe.
        bloom.intensity = match film.shot_index {
            3 => 0.16, // muzzle flashes / tracers
            6 => 0.10, // shuttle wakes — cyan, not washed
            7 => 0.12,
            8 => 0.08,  // painting: tame skyway white-out so grass/rivers read
            9 => 0.14,  // dual rivers emissives
            10 => 0.16, // waterfall cyan emissives
            11 => 0.10, // fighter plumes
            12 => 0.14, // crystal emissives
            13 => 0.12, // station neon crown
            14 => 0.14, // skyway cyan plumes
            _ => 0.05,
        };
        bloom.prefilter_settings.threshold = match film.shot_index {
            3 => 0.50,
            6 => 0.62, // tame pink/white bloom around nozzles
            8 => 0.70,
            14 => 0.58,
            _ => 0.55,
        };
    }

    // Keel bounce / river ribbons toggled in film_toggle_helpers (param budget).

    // Darken ClearColor on planet/painting so additive nebula isn't crushed
    // by noon sky wash (daytime pad shots keep the normal daynight clear).
    // Final write also happens in PostUpdate — see film_override_sky_clear.
    if matches!(film.shot_index, 7 | 8) {
        clear_color.0 = Color::srgb(0.06, 0.03, 0.14);
        if let Ok(mut sun) = sun_q.get_single_mut() {
            sun.illuminance = 4_500.0; // tame noon wash so nebula chroma survives
        }
    } else if matches!(film.shot_index, 9 | 10) {
        clear_color.0 = Color::srgb(0.22, 0.20, 0.28);
    }

    // Extra ambient bounce on deck+keel / dual-river so undersides aren't crushed.
    ambient.brightness = match film.shot_index {
        1 => ambient.brightness.max(6_800.0),
        2 | 3 => ambient.brightness.max(3_800.0),
        9 | 10 => ambient.brightness.max(4_800.0),
        8 => ambient.brightness.max(2_200.0),
        _ => ambient.brightness.max(2_050.0),
    };
    ambient.color = match film.shot_index {
        1 => Color::srgb(0.78, 0.94, 1.0),
        9 => Color::srgb(0.9, 0.82, 0.7),
        8 => Color::srgb(0.7, 0.72, 0.85),
        _ => Color::srgb(0.82, 0.88, 0.78),
    };
    if let Ok(mut sun) = sun_q.get_single_mut() {
        if matches!(film.shot_index, 7 | 8) {
            sun.illuminance = 4_500.0;
        } else {
            sun.illuminance = sun.illuminance.max(28_000.0);
        }
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

    // Warmup: orbit the station pad so the streamer loads every chunk that
    // holds combat / crew voxels (±9 of centre spans multiple 16³ chunks).
    if !film.ready_to_roll {
        let pending = streamer.pending_terrain.len() + streamer.pending_meshes.len();
        let loaded = world.chunks.len();
        let orbit = [
            Vec3::new(0.0, 12.0, 0.0),
            Vec3::new(-14.0, 10.0, 10.0),
            Vec3::new(14.0, 10.0, 10.0),
            Vec3::new(0.0, 10.0, -14.0),
            Vec3::new(-10.0, 9.0, -10.0),
            Vec3::new(10.0, 9.0, -10.0),
        ];
        let oi = ((film.elapsed * 0.55) as usize) % orbit.len();
        let warm_pos = Vec3::new(
            island.cx as f32 + 0.5,
            island.deck_y as f32 + 1.0,
            island.cz as f32 + 0.5,
        ) + orbit[oi];
        let look = Vec3::new(
            island.cx as f32 + 0.5,
            island.deck_y as f32 + 3.0,
            island.cz as f32 + 0.5,
        );
        apply_camera(&mut transform, &mut player, warm_pos, look);
        follow_lights(&mut fill_q, &mut rim_q, transform.translation, &transform);
        park_island_lights(&mut under_q, &mut figure_q, island);

        let pad_ready = station_pad_streamed(&world, island);
        let time_ok = film.elapsed >= 8.0;
        let stream_ok = loaded >= 40 && pending < 80;
        if (pad_ready && time_ok && stream_ok) || film.elapsed >= 22.0 {
            film.ready_to_roll = true;
            film.shot_index = 0;
            film.shot_entered_at = film.elapsed;
            film.capture_queued_at = None;
            info!(
                "FILM: rolling (loaded={loaded}, pending={pending}, pad_ready={pad_ready}, t={:.1})",
                film.elapsed
            );
        }
        return;
    }

    // Advance beat only after the PNG has landed on disk (lavapipe blit
    // often lags several seconds past queue time) plus a short hold.
    if let Some(queued_at) = film.capture_queued_at {
        let last = film.captures.last().cloned();
        let file_ready = last
            .as_ref()
            .map(|p| {
                std::path::Path::new(p)
                    .metadata()
                    .map(|m| m.len() > 8_000)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        let waited = film.elapsed >= queued_at + film.hold_after_secs;
        if file_ready && waited {
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
    park_island_lights(&mut under_q, &mut figure_q, island);
}

fn station_pad_streamed(world: &VoxelWorld, island: IslandSpec) -> bool {
    use crate::blocks::AIR;
    let ox = island.cx;
    let oy = island.deck_y + 1;
    let oz = island.cz;
    // Mast core + pad figures must be resident (each can land in a
    // different 16³ chunk once the station spans ±9 of the origin).
    let mast = world.voxel_at(ox, oy, oz);
    let marine = world.voxel_at(ox - 3, oy + 1, oz + 2);
    let alien = world.voxel_at(ox + 2, oy + 2, oz + 2);
    let crew = world.voxel_at(ox - 4, oy + 1, oz - 2);
    mast != AIR && marine != AIR && alien != AIR && crew != AIR
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
        (
            With<FilmFillLight>,
            Without<Player>,
            Without<FilmRimLight>,
            Without<FilmUnderKeelLight>,
            Without<FilmFigureKeyLight>,
        ),
    >,
    rim_q: &mut Query<
        &mut Transform,
        (
            With<FilmRimLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmUnderKeelLight>,
            Without<FilmFigureKeyLight>,
        ),
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

fn park_island_lights(
    under_q: &mut Query<
        &mut Transform,
        (
            With<FilmUnderKeelLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmRimLight>,
            Without<FilmFigureKeyLight>,
        ),
    >,
    figure_q: &mut Query<
        &mut Transform,
        (
            With<FilmFigureKeyLight>,
            Without<Player>,
            Without<FilmFillLight>,
            Without<FilmRimLight>,
            Without<FilmUnderKeelLight>,
        ),
    >,
    island: IslandSpec,
) {
    let deck = Vec3::new(
        island.cx as f32 + 0.5,
        island.deck_y as f32 + 1.0,
        island.cz as f32 + 0.5,
    );
    let mut under_i = 0usize;
    for mut under in under_q.iter_mut() {
        match under_i {
            0 => {
                under.translation =
                    deck + Vec3::new(8.0, -(island.keel_depth as f32 * 0.95).max(9.0), 12.0);
            }
            1 => {
                under.translation = deck
                    + Vec3::new(
                        island.radius_x as f32 * 0.4,
                        -(island.keel_depth as f32 * 0.55).max(6.0),
                        island.radius_z as f32 * 0.45,
                    );
            }
            2 => {
                under.translation =
                    deck + Vec3::new(-10.0, -(island.keel_depth as f32 * 0.75).max(7.0), -6.0);
            }
            _ => {
                let t = deck + Vec3::new(4.0, -(island.keel_depth as f32 * 1.15).max(12.0), 8.0);
                *under = Transform::from_translation(t)
                    .looking_at(deck + Vec3::new(0.0, -2.0, 0.0), Vec3::Z);
            }
        }
        under_i += 1;
    }
    if let Ok(mut figure) = figure_q.get_single_mut() {
        // Side-lit combat pair on the cleared south slab.
        figure.translation = deck + Vec3::new(-10.0, 7.0, 18.0);
    }
}

fn shot_pose(index: usize, island: IslandSpec, _world: &VoxelWorld) -> (Vec3, Vec3) {
    let deck = Vec3::new(
        island.cx as f32 + 0.5,
        island.deck_y as f32 + 1.0,
        island.cz as f32 + 0.5,
    );
    let station = deck;

    match index {
        0 => {
            // Mega verdant crown near painting frustum.
            let lawn = deck + Vec3::new(-8.0, 10.0, 96.0);
            let pos = lawn + Vec3::new(-32.0, 22.0, 34.0);
            let look = lawn + Vec3::new(4.0, -1.0, -6.0);
            (pos, look)
        }
        1 => {
            // Three-quarter rim pulled back: grass lawn + cyan keel volume
            // without stuffing the lens into a neighboring black silhouette.
            let rx = island.radius_x as f32;
            let rz = island.radius_z as f32;
            let pos = deck + Vec3::new(rx + 20.0, 7.0, rz + 26.0);
            let look = deck + Vec3::new(-1.0, -2.5, 4.0);
            (pos, look)
        }
        2 => {
            // Floating sky arena — look outward (+Z) so island lattice stays behind cam.
            let look = deck + Vec3::new(8.5, 52.0, 132.0);
            let pos = look + Vec3::new(-16.0, 4.5, -20.0);
            (pos, look)
        }
        3 => {
            // Pull back along fire lane: clear deck clutter, yellow muzzles
            // near, fat orange/red beam body stretching to alien.
            let flash = station + Vec3::new(-7.0, 6.5, 26.0);
            let aim = station + Vec3::new(6.0, 5.0, 16.5);
            let axis = (aim - flash).normalize_or_zero();
            let side = axis.cross(Vec3::Y).normalize_or_zero();
            let pos = flash - axis * 14.0 + side * 5.5 + Vec3::Y * 5.0;
            let look = flash.lerp(aim, 0.55) + Vec3::Y * 0.5;
            (pos, look)
        }
        4 => {
            // Crew pair — pull back so both bipeds read as twin figures.
            let look = station + Vec3::new(-5.1, 2.4, -10.0);
            let pos = look + Vec3::new(-11.0, 3.0, 10.0);
            (pos, look)
        }
        5 => {
            // Along cyan rails into mouth (turret FX hidden on this beat).
            let pos = station + Vec3::new(0.0, 8.5, 28.0);
            let look = station + Vec3::new(0.0, 5.5, -10.0);
            (pos, look)
        }
        6 => {
            // Hero shuttle REAR-QUARTER on elevated left-mid painting perch.
            let shuttle = deck + Vec3::new(-8.0, 30.0, 92.0);
            let pos = shuttle + Vec3::new(18.0, 6.0, 16.0);
            let look = shuttle + Vec3::new(-8.0, 1.0, -2.0);
            (pos, look)
        }
        7 => {
            // Ringed planet hero — pure sky look (film proxy hidden this beat).
            let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
            let pos = deck + Vec3::new(-4.0, 18.0, 10.0);
            let look = pos + planet_dir * 200.0;
            (pos, look)
        }
        8 => {
            // Coherent hero: planet upper; mountain mid-right; grass+skyway+rivers stacked.
            let planet_dir = Vec3::new(0.55, 0.65, -0.52).normalize();
            let pos = deck + Vec3::new(-60.0, 38.0, 140.0);
            let station_mid = deck + Vec3::new(34.0, 28.0, 70.0);
            let green_crown = deck + Vec3::new(-16.0, 5.0, 108.0);
            let skyway_mid = deck + Vec3::new(-18.0, 26.0, 96.0);
            let rivers = deck + Vec3::new(-22.0, 8.0, 100.0);
            let planet = pos + planet_dir * 155.0;
            // Favor verdant lip a touch more; station stays mid-right darkrock.
            let ground = green_crown
                .lerp(skyway_mid, 0.18)
                .lerp(rivers, 0.16)
                .lerp(station_mid, 0.28);
            let look = ground.lerp(planet, 0.17);
            (pos, look)
        }
        9 => {
            // Dual plasma + lava — face parallel lanes; grass hidden this beat.
            let look = deck + Vec3::new(20.0, 4.0, 66.0);
            let pos = deck + Vec3::new(-38.0, 22.0, 100.0);
            (pos, look)
        }
        10 => {
            // Dedicated waterfall: face the open-air cyan cascade + cliff.
            let mid = deck + Vec3::new(64.0, -4.0, 70.0);
            let pos = deck + Vec3::new(92.0, 2.0, 96.0);
            let look = mid;
            (pos, look)
        }
        11 => {
            // Fighter swarm: closer horizontal look at high V (sky craft only).
            let form = deck + Vec3::new(40.0, 60.0, 18.0);
            let pos = form + Vec3::new(20.0, 3.0, 26.0);
            let look = form + Vec3::new(-12.0, 3.0, -1.0);
            (pos, look)
        }
        12 => {
            // Crystal towers: +X elevated side view (waterfall hidden this beat).
            let cluster = deck + Vec3::new(30.0, 42.0, 28.0);
            let pos = deck + Vec3::new(135.0, 48.0, 28.0);
            let look = cluster;
            (pos, look)
        }
        13 => {
            // Station mountain — stand off so full stepped mass + neon crown read.
            let station = deck + Vec3::new(34.0, 36.0, 70.0);
            let pos = deck + Vec3::new(-55.0, 78.0, 155.0);
            let look = station;
            (pos, look)
        }
        _ => {
            // Skyway + shuttle — rear-quarter of cyan-plume craft over spans.
            let shuttle = deck + Vec3::new(-8.0, 30.0, 92.0);
            let pos = shuttle + Vec3::new(32.0, 14.0, 34.0);
            let look = shuttle + Vec3::new(-6.0, 2.0, -8.0);
            (pos, look)
        }
    }
}

fn film_capture(
    mut film: ResMut<FilmRuntime>,
    world: Res<VoxelWorld>,
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
        if shot.contains("combat") {
            if let Some(island) = film.island {
                dump_pad_voxels(&world, island);
            }
        }
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
        let _ = (window, &mut screenshots, shot, &world);
        film.last_captured_shot = shot_i;
        film.capture_queued_at = Some(film.elapsed);
    }
}

fn dump_pad_voxels(world: &VoxelWorld, island: IslandSpec) {
    let ox = island.cx;
    let oy = island.deck_y + 1;
    let oz = island.cz;
    let mut lines = vec![format!(
        "pad_origin=({ox},{oy},{oz})\nprobe=marine(-3..0,1..5,2..3) alien(0..4,1..6,0..4)\n"
    )];
    for (label, dx, dy, dz) in [
        ("marine_leg", -3, 1, 2),
        ("marine_chest", -3, 3, 2),
        ("marine_beacon", -3, 5, 2),
        ("alien_body", 2, 3, 2),
        ("alien_crest", 2, 6, 2),
        ("alien_leg", 4, 1, 4),
        ("crew_a", -4, 3, -2),
        ("crew_b", -4, 3, -3),
        ("fighter_plume", 5, 2, 0),
    ] {
        let v = world.voxel_at(ox + dx, oy + dy, oz + dz);
        lines.push(format!(
            "{label}=({},{},{}) voxel={:?} block={:?}\n",
            ox + dx,
            oy + dy,
            oz + dz,
            v,
            crate::blocks::BlockType::from_voxel(v)
        ));
    }
    let body = lines.concat();
    let _ = std::fs::write("/opt/cursor/artifacts/film_pad_voxel_probe.txt", &body);
    info!("FILM: pad voxel probe\n{body}");
}

fn film_finish(mut film: ResMut<FilmRuntime>, mut exit: EventWriter<AppExit>) {
    if !film.enabled || film.finished {
        return;
    }
    if !film.ready_to_roll {
        return;
    }
    // Done when every shot captured, the PNG exists, and the last hold finished.
    let all_captured = film.last_captured_shot + 1 >= SHOTS.len() as i32;
    let last_file_ready = film
        .captures
        .last()
        .map(|p| {
            std::path::Path::new(p)
                .metadata()
                .map(|m| m.len() > 8_000)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let last_hold_done = film
        .capture_queued_at
        .map(|t| film.elapsed >= t + film.hold_after_secs)
        .unwrap_or(false);
    if !(all_captured && last_file_ready && last_hold_done) {
        // Safety valve so a stuck blit cannot hang forever.
        if film.elapsed < film.duration_estimate() + 40.0 {
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
        assert!(SHOTS.len() >= 10);
        assert!((ISLAND_CLOSEUP_DISTANCE_M - 14.0).abs() < 1e-3);
        let names: Vec<_> = SHOTS.iter().map(|s| s.name).collect();
        assert!(names
            .iter()
            .any(|n| n.contains("island") && n.contains("grass")));
        assert!(names
            .iter()
            .any(|n| n.contains("deck_keel") || n.contains("keel")));
        assert!(names.iter().any(|n| n.contains("combat")));
        assert!(names.iter().any(|n| n.contains("turret")));
        assert!(names.iter().any(|n| n.contains("shuttle")));
        assert!(names
            .iter()
            .any(|n| n.contains("fighter") && n.contains("swarm")));
        assert!(names
            .iter()
            .any(|n| n.contains("portal") || n.contains("tunnel") || n.contains("fighter")));
        assert!(names.iter().any(|n| n.contains("planet")));
        assert!(names.iter().any(|n| n.contains("painting")));
        assert!(names
            .iter()
            .any(|n| n.contains("dual") || n.contains("river") || n.contains("plasma")));
        assert!(names.iter().any(|n| n.contains("waterfall")));
        assert!(names.iter().any(|n| n.contains("crystal")));
        assert!(names.iter().any(|n| n.contains("rail")));
    }

    #[test]
    fn film_enabled_parses_flag_helpers() {
        assert!(!env_flag("VOXEL_NATIVE_FILM_UNSET_SENTINEL_ZZZ"));
    }
}
