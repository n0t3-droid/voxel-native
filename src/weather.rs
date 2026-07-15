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

use crate::daynight::{DayNightSet, WorldIntelRuntime};
use crate::neurocore::{RuntimeBudget, RuntimeProfile};
use crate::player::Player;
use crate::settings::{WeatherPreset, WeatherSettings, WorldSettings};

/// Radius (world units) of the particle ring around the player.
const PARTICLE_RADIUS: f32 = 36.0;
const PARTICLE_HEIGHT: f32 = 40.0;
const RAIN_POOL: usize = 900;
const SNOW_POOL: usize = 600;
const CHUNK_WORLD_SIZE: f32 = 16.0;
const FOG_TRANSITION_HALF_LIFE_SECONDS: f32 = 0.35;

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

#[derive(Debug, Clone, Copy, PartialEq)]
struct FogRecipe {
    weather_density: f32,
    start: f32,
    end: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct FogQuality {
    clear_start_factor: f32,
    clear_end_factor: f32,
    clear_min_start: f32,
    clear_min_end: f32,
    clear_max_start: f32,
    clear_max_end: f32,
    weather_min_start: f32,
    weather_min_end: f32,
    density_gamma: f32,
}

fn fog_quality(profile: RuntimeProfile) -> FogQuality {
    match profile {
        RuntimeProfile::LowSpec => FogQuality {
            clear_start_factor: 1.85,
            clear_end_factor: 3.35,
            clear_min_start: 800.0,
            clear_min_end: 1_800.0,
            clear_max_start: 3_600.0,
            clear_max_end: 5_600.0,
            weather_min_start: 220.0,
            weather_min_end: 620.0,
            density_gamma: 1.30,
        },
        RuntimeProfile::Cinematic => FogQuality {
            clear_start_factor: 2.70,
            clear_end_factor: 5.80,
            clear_min_start: 1_600.0,
            clear_min_end: 4_200.0,
            clear_max_start: 6_000.0,
            clear_max_end: 9_500.0,
            weather_min_start: 320.0,
            weather_min_end: 960.0,
            density_gamma: 1.60,
        },
        RuntimeProfile::Auto | RuntimeProfile::Balanced | RuntimeProfile::Benchmark => FogQuality {
            clear_start_factor: 2.25,
            clear_end_factor: 4.60,
            clear_min_start: 1_200.0,
            clear_min_end: 2_800.0,
            clear_max_start: 4_600.0,
            clear_max_end: 7_200.0,
            weather_min_start: 260.0,
            weather_min_end: 760.0,
            density_gamma: 1.45,
        },
    }
}

fn fog_recipe(
    weather_fog_density: f32,
    render_distance_chunks: i32,
    weather_fx_scale: f32,
    profile_weather_fx_mul: f32,
    profile_fog_density_mul: f32,
    runtime_profile: RuntimeProfile,
) -> FogRecipe {
    let raw_weather_density =
        (weather_fog_density * weather_fx_scale * profile_weather_fx_mul * profile_fog_density_mul)
            .clamp(0.0, 1.0);
    let quality = fog_quality(runtime_profile);
    let weather_density = raw_weather_density.powf(quality.density_gamma);
    let rd_blocks = (render_distance_chunks.max(8) as f32) * CHUNK_WORLD_SIZE;

    let clear_end =
        (rd_blocks * quality.clear_end_factor).clamp(quality.clear_min_end, quality.clear_max_end);
    let clear_start = (rd_blocks * quality.clear_start_factor)
        .clamp(quality.clear_min_start, quality.clear_max_start)
        .min(clear_end - 640.0);
    if weather_density <= 0.001 {
        return FogRecipe {
            weather_density,
            start: clear_start,
            end: clear_end,
        };
    }

    let end_factor = 1.02 + (1.0 - weather_density) * 1.55;
    let weather_end = (rd_blocks * end_factor).clamp(quality.weather_min_end, clear_end * 0.86);
    let min_gap = (weather_end * 0.32).clamp(180.0, 520.0);
    let weather_start = (weather_end * (0.56 + (1.0 - weather_density) * 0.18)).clamp(
        quality.weather_min_start,
        (weather_end - min_gap).max(quality.weather_min_start),
    );
    let transition = ((weather_density - 0.001) / 0.22).clamp(0.0, 1.0);
    let transition = transition * transition * (3.0 - 2.0 * transition);
    let end = clear_end + (weather_end - clear_end) * transition;
    let start = (clear_start + (weather_start - clear_start) * transition).min(end - 48.0);
    FogRecipe {
        weather_density,
        start,
        end,
    }
}

fn fog_color_for_sky(clear: Color, weather_density: f32) -> Color {
    let sky = clear.to_linear();
    let luminance = sky.red * 0.2126 + sky.green * 0.7152 + sky.blue * 0.0722;
    let neutral_haze = LinearRgba::rgb(luminance * 0.90, luminance * 0.96, luminance * 1.02);
    let desaturation = 0.05 + weather_density.clamp(0.0, 1.0) * 0.18;
    Color::LinearRgba(sky.mix(&neutral_haze, desaturation))
}

fn smooth_fog_recipe(current: FogRecipe, target: FogRecipe, delta_seconds: f32) -> FogRecipe {
    let delta_seconds = if delta_seconds.is_finite() {
        delta_seconds.max(0.0)
    } else {
        FOG_TRANSITION_HALF_LIFE_SECONDS
    };
    let blend =
        1.0 - 2.0_f32.powf(-delta_seconds / FOG_TRANSITION_HALF_LIFE_SECONDS.max(f32::EPSILON));
    FogRecipe {
        weather_density: current.weather_density
            + (target.weather_density - current.weather_density) * blend,
        start: current.start + (target.start - current.start) * blend,
        end: current.end + (target.end - current.end) * blend,
    }
}

fn effective_weather_fog_density(preset: WeatherPreset, fog_density: f32) -> f32 {
    // New clean worlds intentionally carry a tiny atmosphere value for
    // long-distance blending. Treat that as clear-sky haze, not as an
    // active fog preset that shortens the visible world.
    if matches!(preset, WeatherPreset::Clear) && fog_density <= 0.08 {
        0.0
    } else {
        fog_density
    }
}

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_weather).add_systems(
            Update,
            (
                apply_fog.after(DayNightSet::Lighting),
                update_rain,
                update_snow,
                update_particle_visibility,
            ),
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
    time: Res<Time>,
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    intel: Res<WorldIntelRuntime>,
    mut fog_q: Query<&mut FogSettings, With<Camera3d>>,
    clear: Res<ClearColor>,
    mut previous: Local<Option<FogRecipe>>,
) {
    let Ok(mut fog) = fog_q.get_single_mut() else {
        return;
    };
    let target = fog_recipe(
        effective_weather_fog_density(settings.weather.preset, settings.weather.fog_density),
        budget.render_distance,
        budget.weather_fx_scale,
        intel.profile.weather_fx_mul,
        intel.profile.fog_density_mul,
        budget.profile,
    );
    let recipe = previous
        .map(|current| smooth_fog_recipe(current, target, time.delta_seconds()))
        .unwrap_or(target);
    *previous = Some(recipe);
    fog.color = fog_color_for_sky(clear.0, recipe.weather_density);
    fog.falloff = FogFalloff::Linear {
        start: recipe.start,
        end: recipe.end,
    };
}

fn update_rain(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    budget: Res<RuntimeBudget>,
    intel: Res<WorldIntelRuntime>,
    player_q: Query<&Transform, (With<Player>, Without<RainDrop>)>,
    mut drops: Query<(&mut Transform, &RainDrop), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let w: &WeatherSettings = &settings.weather;
    let intensity =
        (w.rain_intensity * budget.weather_fx_scale * intel.profile.weather_fx_mul).clamp(0.0, 1.0);
    if intensity < 0.01 {
        return;
    }
    let dt = time.delta_seconds();
    let wind = Vec3::new(w.wind_x, 0.0, w.wind_z);

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
    budget: Res<RuntimeBudget>,
    intel: Res<WorldIntelRuntime>,
    player_q: Query<&Transform, (With<Player>, Without<SnowFlake>)>,
    mut flakes: Query<(&mut Transform, &mut SnowFlake), Without<Player>>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let w = &settings.weather;
    let intensity =
        (w.snow_intensity * budget.weather_fx_scale * intel.profile.weather_fx_mul).clamp(0.0, 1.0);
    if intensity < 0.01 {
        return;
    }
    let dt = time.delta_seconds();
    let wind = Vec3::new(w.wind_x, 0.0, w.wind_z);

    for (mut tf, mut flake) in flakes.iter_mut() {
        // Wrap phase modulo TAU so 10h+ sessions don't lose f32
        // precision in the sin/cos (at phase=72000 the LSB is ~0.02,
        // which would make snowflakes visibly stutter).
        flake.sway_phase = (flake.sway_phase + dt * 2.0) % std::f32::consts::TAU;
        let sway = Vec3::new(
            flake.sway_phase.sin() * 0.6,
            0.0,
            flake.sway_phase.cos() * 0.6,
        );
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
    budget: Res<RuntimeBudget>,
    intel: Res<WorldIntelRuntime>,
    mut rain_q: Query<&mut Visibility, (With<RainDrop>, Without<SnowFlake>)>,
    mut snow_q: Query<&mut Visibility, (With<SnowFlake>, Without<RainDrop>)>,
) {
    if !settings.is_changed() && !budget.is_changed() {
        return;
    }
    let fx = (budget.weather_fx_scale * intel.profile.weather_fx_mul).clamp(0.0, 1.0);
    let rain_active =
        (settings.weather.rain_intensity.clamp(0.0, 1.0) * fx * RAIN_POOL as f32) as usize;
    for (i, mut vis) in rain_q.iter_mut().enumerate() {
        *vis = if i < rain_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let snow_active =
        (settings.weather.snow_intensity.clamp(0.0, 1.0) * fx * SNOW_POOL as f32) as usize;
    for (i, mut vis) in snow_q.iter_mut().enumerate() {
        *vis = if i < snow_active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_weather_keeps_a_soft_streaming_haze() {
        let recipe = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);

        assert!(recipe.end < 10_000.0);
        assert!(
            recipe.start >= 500.0,
            "clear weather should not hide nearby mountains behind early fog"
        );
        assert!(
            recipe.end >= 1000.0,
            "clear weather should leave long-distance terrain readable"
        );
        assert!(recipe.end > recipe.start);
    }

    #[test]
    fn clear_weather_fog_stays_beyond_normal_chunk_edge() {
        let recipe = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);

        assert!(
            recipe.start >= 640.0,
            "clear weather fog should not begin before a 40-chunk world edge"
        );
        assert!(
            recipe.end >= 1600.0,
            "clear weather should avoid a short white linear-fog veil"
        );
    }

    #[test]
    fn clear_weather_keeps_zen_world_horizon_readable() {
        let recipe = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);

        assert!(
            recipe.start >= 1200.0,
            "clear weather should not put the blue fog wall right behind nearby mountains"
        );
        assert!(
            recipe.end >= 2200.0,
            "clear weather should leave large scenic terrain readable before fading out"
        );
    }

    #[test]
    fn weather_fog_shortens_the_distance_veil() {
        let clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let foggy = fog_recipe(0.8, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);

        assert!(foggy.weather_density > clear.weather_density);
        assert!(foggy.end < clear.end);
        assert!(foggy.start < clear.start);
        assert!(
            foggy.end >= 420.0,
            "heavy fog should still be playable at max chunk distance"
        );
        assert!(
            foggy.start >= 130.0,
            "heavy fog should not wash out everything right in front of the player"
        );
    }

    #[test]
    fn every_weather_density_is_valid_on_low_render_distances() {
        for render_distance in [4, 8, 14] {
            let mut previous_end = f32::INFINITY;
            for step in 0..=10 {
                let density = step as f32 / 10.0;
                let recipe = fog_recipe(
                    density,
                    render_distance,
                    1.0,
                    1.0,
                    1.0,
                    RuntimeProfile::Balanced,
                );
                assert!(recipe.start.is_finite() && recipe.end.is_finite());
                assert!(recipe.start >= 0.0);
                assert!(recipe.end > recipe.start);
                assert!(recipe.end <= previous_end + 1.0e-3);
                previous_end = recipe.end;
            }
        }
    }

    #[test]
    fn light_weather_blends_into_clear_haze_without_a_distance_jump() {
        let clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let light = fog_recipe(0.01, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);

        assert!(light.end < clear.end);
        assert!(light.end > clear.end * 0.9);
        assert!(light.start > clear.start * 0.9);
    }

    #[test]
    fn fog_transition_half_life_reaches_the_recipe_midpoint_without_overshoot() {
        let clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let foggy = fog_recipe(0.8, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let midpoint = smooth_fog_recipe(clear, foggy, FOG_TRANSITION_HALF_LIFE_SECONDS);

        assert!(
            (midpoint.weather_density - (clear.weather_density + foggy.weather_density) * 0.5)
                .abs()
                < 1.0e-5
        );
        assert!((midpoint.start - (clear.start + foggy.start) * 0.5).abs() < 1.0e-3);
        assert!((midpoint.end - (clear.end + foggy.end) * 0.5).abs() < 1.0e-3);
        assert!(midpoint.start > foggy.start && midpoint.start < clear.start);
        assert!(midpoint.end > foggy.end && midpoint.end < clear.end);
    }

    #[test]
    fn quality_profiles_provide_ordered_clear_and_weather_visibility() {
        let low_clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::LowSpec);
        let balanced_clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let cinematic_clear = fog_recipe(0.0, 40, 1.0, 1.0, 1.0, RuntimeProfile::Cinematic);

        assert!(low_clear.start < balanced_clear.start);
        assert!(balanced_clear.start < cinematic_clear.start);
        assert!(low_clear.end < balanced_clear.end);
        assert!(balanced_clear.end < cinematic_clear.end);

        let low_fog = fog_recipe(0.8, 40, 1.0, 1.0, 1.0, RuntimeProfile::LowSpec);
        let balanced_fog = fog_recipe(0.8, 40, 1.0, 1.0, 1.0, RuntimeProfile::Balanced);
        let cinematic_fog = fog_recipe(0.8, 40, 1.0, 1.0, 1.0, RuntimeProfile::Cinematic);
        assert!(low_fog.end < balanced_fog.end);
        assert!(balanced_fog.end < cinematic_fog.end);
    }

    #[test]
    fn fog_color_reduces_the_saturated_blue_wall() {
        let sky = Color::srgb(0.38, 0.64, 0.94);
        let sky_linear = sky.to_linear();
        let fog_linear = fog_color_for_sky(sky, 0.0).to_linear();

        assert!(fog_linear.blue - fog_linear.red < sky_linear.blue - sky_linear.red);
        assert!(fog_linear.red > sky_linear.red);
    }

    #[test]
    fn clear_world_default_haze_does_not_activate_weather_fog() {
        assert_eq!(
            effective_weather_fog_density(WeatherPreset::Clear, 0.06),
            0.0
        );
        assert_eq!(
            effective_weather_fog_density(WeatherPreset::Fog, 0.06),
            0.06
        );
        assert_eq!(
            effective_weather_fog_density(WeatherPreset::Custom, 0.06),
            0.06
        );
        assert_eq!(
            effective_weather_fog_density(WeatherPreset::Clear, 0.25),
            0.25
        );
    }
}
