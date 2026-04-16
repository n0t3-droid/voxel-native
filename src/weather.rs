//! Weather system — rain, snow, fog, wind. Port target: `RainField`,
//! `WeatherPresets` and the fog/sky blending from `components/VoxelEngine.tsx`.
//!
//! Strategy: spawn a fixed-size pool of particle entities that orbit around
//! the player (anchor), fall under gravity + wind, and respawn at the top
//! once they drop below a threshold. Intensity scales how many of the pool
//! are visible and how fast they fall.

use bevy::pbr::{FogFalloff, FogSettings};
use bevy::prelude::*;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

use crate::player::Player;
use crate::settings::{WeatherSettings, WorldSettings};

/// Radius (world units) of the particle ring around the player.
const PARTICLE_RADIUS: f32 = 36.0;
const PARTICLE_HEIGHT: f32 = 40.0;
const RAIN_POOL: usize = 900;
const SNOW_POOL: usize = 600;

#[derive(Component)]
pub struct RainDrop {
    pub fall_speed: f32,
}

#[derive(Component)]
pub struct SnowFlake {
    pub fall_speed: f32,
    pub sway_phase: f32,
}

pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_weather).add_systems(
            Update,
            (apply_fog, update_rain, update_snow, update_particle_visibility),
        );
    }
}

fn setup_weather(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Rain = tall thin cuboid (0.06 × 0.55 × 0.06), light blue unlit.
    let rain_mesh = meshes.add(Cuboid::new(0.06, 0.55, 0.06));
    let rain_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.70, 0.85, 1.0, 0.75),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    // Snow = small cube, emissive so it glows a bit at night.
    let snow_mesh = meshes.add(Cuboid::new(0.10, 0.10, 0.10));
    let snow_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 1.0, 1.0, 0.9),
        emissive: LinearRgba::rgb(0.6, 0.7, 0.9),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    let mut rng = ChaCha8Rng::seed_from_u64(0xA11CE_BEEF);
    for _ in 0..RAIN_POOL {
        let x: f32 = rng.gen_range(-PARTICLE_RADIUS..PARTICLE_RADIUS);
        let z: f32 = rng.gen_range(-PARTICLE_RADIUS..PARTICLE_RADIUS);
        let y: f32 = rng.gen_range(0.0..PARTICLE_HEIGHT);
        commands.spawn((
            PbrBundle {
                mesh: rain_mesh.clone(),
                material: rain_mat.clone(),
                transform: Transform::from_xyz(x, y, z),
                visibility: Visibility::Hidden,
                ..default()
            },
            RainDrop {
                fall_speed: rng.gen_range(18.0..30.0),
            },
        ));
    }
    for _ in 0..SNOW_POOL {
        let x: f32 = rng.gen_range(-PARTICLE_RADIUS..PARTICLE_RADIUS);
        let z: f32 = rng.gen_range(-PARTICLE_RADIUS..PARTICLE_RADIUS);
        let y: f32 = rng.gen_range(0.0..PARTICLE_HEIGHT);
        commands.spawn((
            PbrBundle {
                mesh: snow_mesh.clone(),
                material: snow_mat.clone(),
                transform: Transform::from_xyz(x, y, z),
                visibility: Visibility::Hidden,
                ..default()
            },
            SnowFlake {
                fall_speed: rng.gen_range(1.5..3.0),
                sway_phase: rng.gen_range(0.0..std::f32::consts::TAU),
            },
        ));
    }
}

fn apply_fog(
    settings: Res<WorldSettings>,
    mut fog_q: Query<&mut FogSettings, With<Camera3d>>,
    clear: Res<ClearColor>,
) {
    let Ok(mut fog) = fog_q.get_single_mut() else {
        return;
    };
    let density = settings.weather.fog_density.clamp(0.0, 1.0);
    if density <= 0.001 {
        fog.color = clear.0.with_alpha(0.0);
        fog.falloff = FogFalloff::Linear {
            start: 10_000.0,
            end: 10_000.0,
        };
    } else {
        fog.color = clear.0;
        // Stronger fog = much shorter visible range.
        let start = 10.0 + (1.0 - density) * 80.0;
        let end = 60.0 + (1.0 - density) * 220.0;
        fog.falloff = FogFalloff::Linear { start, end };
    }
}

fn update_rain(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    player_q: Query<&Transform, (With<Player>, Without<RainDrop>)>,
    mut drops: Query<(&mut Transform, &RainDrop), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let w: &WeatherSettings = &settings.weather;
    if w.rain_intensity < 0.01 {
        return;
    }
    let dt = time.delta_seconds();
    let wind = Vec3::new(w.wind_x, 0.0, w.wind_z);
    let intensity = w.rain_intensity.clamp(0.0, 1.0);

    for (mut tf, drop) in drops.iter_mut() {
        tf.translation += Vec3::new(0.0, -drop.fall_speed * (0.6 + intensity), 0.0) * dt;
        tf.translation += wind * dt * 0.25;
        // Recycle when too far below player.
        let rel = tf.translation - player_tf.translation;
        if rel.y < -6.0 || rel.x.abs() > PARTICLE_RADIUS || rel.z.abs() > PARTICLE_RADIUS {
            let angle = rel.x.atan2(rel.z) + 0.37;
            tf.translation.x = player_tf.translation.x + angle.sin() * PARTICLE_RADIUS * 0.9;
            tf.translation.z = player_tf.translation.z + angle.cos() * PARTICLE_RADIUS * 0.9;
            tf.translation.y = player_tf.translation.y + PARTICLE_HEIGHT * 0.8;
        }
    }
}

fn update_snow(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    player_q: Query<&Transform, (With<Player>, Without<SnowFlake>)>,
    mut flakes: Query<(&mut Transform, &mut SnowFlake), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let w = &settings.weather;
    if w.snow_intensity < 0.01 {
        return;
    }
    let dt = time.delta_seconds();
    let wind = Vec3::new(w.wind_x, 0.0, w.wind_z);
    let intensity = w.snow_intensity.clamp(0.0, 1.0);

    for (mut tf, mut flake) in flakes.iter_mut() {
        flake.sway_phase += dt * 2.0;
        let sway = Vec3::new(flake.sway_phase.sin() * 0.6, 0.0, flake.sway_phase.cos() * 0.6);
        tf.translation +=
            (Vec3::new(0.0, -flake.fall_speed * (0.4 + intensity), 0.0) + sway + wind * 0.2) * dt;
        let rel = tf.translation - player_tf.translation;
        if rel.y < -6.0 || rel.x.abs() > PARTICLE_RADIUS || rel.z.abs() > PARTICLE_RADIUS {
            let angle = rel.x.atan2(rel.z) + 1.37;
            tf.translation.x = player_tf.translation.x + angle.sin() * PARTICLE_RADIUS * 0.9;
            tf.translation.z = player_tf.translation.z + angle.cos() * PARTICLE_RADIUS * 0.9;
            tf.translation.y = player_tf.translation.y + PARTICLE_HEIGHT * 0.8;
        }
    }
}

/// Hide/show particles based on how much of the pool the current intensity wants.
fn update_particle_visibility(
    settings: Res<WorldSettings>,
    mut rain_q: Query<&mut Visibility, (With<RainDrop>, Without<SnowFlake>)>,
    mut snow_q: Query<&mut Visibility, (With<SnowFlake>, Without<RainDrop>)>,
) {
    if !settings.is_changed() {
        return;
    }
    let rain_active = (settings.weather.rain_intensity.clamp(0.0, 1.0) * RAIN_POOL as f32) as usize;
    for (i, mut vis) in rain_q.iter_mut().enumerate() {
        *vis = if i < rain_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let snow_active = (settings.weather.snow_intensity.clamp(0.0, 1.0) * SNOW_POOL as f32) as usize;
    for (i, mut vis) in snow_q.iter_mut().enumerate() {
        *vis = if i < snow_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}
