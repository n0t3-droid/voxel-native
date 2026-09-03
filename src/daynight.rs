//! Day/night cycle — moves a directional "sun" light around the player,
//! swings sky colour + fog between day and night, and drops intensity at
//! dawn/dusk. Port target: the `DayNightCycle` component from
//! `components/VoxelEngine.tsx`.

use bevy::pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap};
use bevy::prelude::*;

use crate::player::Player;
use crate::settings::{GraphicsMode, TimeMode, WorldSettings};
use crate::terrain::Biome;
use crate::world::VoxelWorld;

#[derive(Debug, Clone, Copy)]
pub struct BiomeArtProfile {
    pub fog_density_mul: f32,
    pub ambient_mul: f32,
    pub sky_saturation: f32,
    pub bloom_mul: f32,
    pub weather_fx_mul: f32,
    pub streaming_bonus: i32,
}

impl BiomeArtProfile {
    pub const fn for_biome(biome: Biome) -> Self {
        match biome {
            Biome::CrystalSpires => Self {
                // Deep purple/cyan haze + strong bloom — matches crystal-world key art.
                fog_density_mul: 1.06,
                ambient_mul: 1.08,
                sky_saturation: 1.42,
                bloom_mul: 1.42,
                weather_fx_mul: 0.58,
                streaming_bonus: -18,
            },
            Biome::AlienReef => Self {
                fog_density_mul: 1.18,
                ambient_mul: 1.12,
                sky_saturation: 1.36,
                bloom_mul: 1.36,
                weather_fx_mul: 1.18,
                streaming_bonus: -16,
            },
            Biome::VolcanicWaste => Self {
                fog_density_mul: 1.12,
                ambient_mul: 0.92,
                sky_saturation: 1.10,
                bloom_mul: 1.14,
                weather_fx_mul: 0.42,
                streaming_bonus: -8,
            },
            Biome::GlacierShards => Self {
                fog_density_mul: 1.06,
                ambient_mul: 0.98,
                sky_saturation: 1.08,
                bloom_mul: 1.10,
                weather_fx_mul: 1.18,
                streaming_bonus: -6,
            },
            // Banded canyon country is the frontier's default ground, so
            // its profile is what most of the world looks like: clear
            // enough for the long mesa vistas, saturated enough that the
            // violet and ochre strata stay vivid at distance.
            Biome::Mesa | Biome::Desert => Self {
                fog_density_mul: 0.92,
                ambient_mul: 1.02,
                sky_saturation: 1.22,
                bloom_mul: 1.18,
                weather_fx_mul: 0.35,
                streaming_bonus: -2,
            },
            // Even the green transitional country between the provinces
            // is on the same planet under the same nebula. A flat 1.0
            // baseline here would make every ridge crossing look like a
            // different game.
            _ => Self {
                fog_density_mul: 1.0,
                ambient_mul: 1.0,
                sky_saturation: 1.14,
                bloom_mul: 1.10,
                weather_fx_mul: 1.0,
                streaming_bonus: 0,
            },
        }
    }
}

#[derive(Resource, Debug, Clone, Copy)]
pub struct WorldIntelRuntime {
    pub biome: Biome,
    pub profile: BiomeArtProfile,
}

impl Default for WorldIntelRuntime {
    fn default() -> Self {
        let biome = Biome::Plains;
        Self {
            biome,
            profile: BiomeArtProfile::for_biome(biome),
        }
    }
}

pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        // Shadow-map size is picked once at startup from the loaded
        // settings. 2048 was crippling on integrated GPUs (Vega 8 on
        // Ryzen 5700G allocates from shared RAM, and a 3-cascade 2048²
        // depth atlas = ~48 MB + proportional depth-pass fill cost).
        //
        //   Fast     =>  512  (≈0.75 MB × 3 cascades, fits in cache)
        //   Balanced => 1024  (≈3 MB, good quality/perf balance)
        //   High     => 2048  (crisp shadows for dGPUs)
        //
        // Users can still override via settings.graphics at runtime;
        // the plugin responds by rebuilding the cascades in
        // `update_shadow_quality` on mode changes.
        app.insert_resource(DirectionalLightShadowMap { size: 1024 })
            .insert_resource(WorldIntelRuntime::default())
            .add_systems(Startup, (apply_startup_shadow_size, spawn_sun).chain())
            .add_systems(
                Update,
                (
                    advance_time,
                    update_world_intel_runtime,
                    update_sun,
                    update_shadow_quality,
                )
                    .chain(),
            );
    }
}

fn apply_startup_shadow_size(
    settings: Res<WorldSettings>,
    mut shadow: ResMut<DirectionalLightShadowMap>,
) {
    shadow.size = shadow_size_for(settings.graphics);
}

fn shadow_size_for(mode: GraphicsMode) -> usize {
    match mode {
        GraphicsMode::Fast => 512,
        GraphicsMode::Balanced => 1024,
        GraphicsMode::High => 2048,
    }
}

fn cascade_config_for(mode: GraphicsMode) -> bevy::pbr::CascadeShadowConfig {
    match mode {
        // Fast: 1 tight cascade, 64-block radius. Perfect for iGPU +
        // low render-distance play. Removes ~66% of shadow-pass work.
        GraphicsMode::Fast => CascadeShadowConfigBuilder {
            num_cascades: 1,
            minimum_distance: 0.5,
            maximum_distance: 64.0,
            first_cascade_far_bound: 64.0,
            overlap_proportion: 0.2,
        }
        .build(),
        GraphicsMode::Balanced => CascadeShadowConfigBuilder {
            num_cascades: 2,
            minimum_distance: 0.5,
            maximum_distance: 120.0,
            first_cascade_far_bound: 22.0,
            overlap_proportion: 0.2,
        }
        .build(),
        GraphicsMode::High => CascadeShadowConfigBuilder {
            num_cascades: 3,
            minimum_distance: 0.5,
            maximum_distance: 160.0,
            first_cascade_far_bound: 18.0,
            overlap_proportion: 0.2,
        }
        .build(),
    }
}

#[derive(Component)]
pub struct Sun;

fn spawn_sun(mut commands: Commands, settings: Res<WorldSettings>) {
    let cascades = cascade_config_for(settings.graphics);

    commands.spawn((
        DirectionalLightBundle {
            directional_light: DirectionalLight {
                illuminance: 10_000.0,
                // Shadows off in Fast mode — single biggest iGPU win.
                shadows_enabled: settings.graphics != GraphicsMode::Fast,
                ..default()
            },
            transform: Transform::from_xyz(50.0, 200.0, 50.0).looking_at(Vec3::ZERO, Vec3::Y),
            cascade_shadow_config: cascades,
            ..default()
        },
        Sun,
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.7, 0.8, 1.0),
        brightness: 200.0,
    });
}

/// Reactively apply graphics-mode changes (from the F3 editor) to the
/// sun's cascades, shadow toggle and shadow-map size without requiring
/// a restart. Zero-cost when the mode hasn't changed.
fn update_shadow_quality(
    settings: Res<WorldSettings>,
    mut shadow: ResMut<DirectionalLightShadowMap>,
    mut sun: Query<(&mut bevy::pbr::CascadeShadowConfig, &mut DirectionalLight), With<Sun>>,
    mut last: Local<Option<GraphicsMode>>,
) {
    if *last == Some(settings.graphics) {
        return;
    }
    *last = Some(settings.graphics);
    shadow.size = shadow_size_for(settings.graphics);
    if let Ok((mut cfg, mut light)) = sun.get_single_mut() {
        *cfg = cascade_config_for(settings.graphics);
        light.shadows_enabled = settings.graphics != GraphicsMode::Fast;
    }
}

fn advance_time(
    time: Res<Time>,
    mut settings: ResMut<WorldSettings>,
    pause: Option<Res<crate::editor::SimPause>>,
) {
    // Edit mode (F6) freezes the day/night cycle so screenshots and
    // precision building aren't ruined by shifting sun angles.
    if pause.map(|p| p.paused).unwrap_or(false) {
        return;
    }
    if settings.time_mode == TimeMode::Cycle {
        settings.time_of_day =
            (settings.time_of_day + settings.cycle_speed * time.delta_seconds() * 60.0) % 24.0;
    }
}

fn update_sun(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    mut clear_color: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), With<Sun>>,
    mut fog: Query<&mut FogSettings>,
) {
    let Ok((mut transform, mut light)) = sun.get_single_mut() else {
        return;
    };

    // hour in radians, noon = π/2
    let t = (settings.time_of_day / 24.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
    let sun_dir = Vec3::new(t.cos(), t.sin(), 0.3).normalize();

    // Directional lights in Bevy shine along their -Z. Orient so -Z == -sun_dir.
    let forward = -sun_dir;
    *transform = Transform::from_xyz(sun_dir.x * 400.0, sun_dir.y * 400.0, sun_dir.z * 400.0)
        .looking_to(forward, Vec3::Y);

    // Day factor 0..1 where 1 = high noon, 0 = deep night.
    let day = sun_dir.y.max(0.0);
    light.illuminance = 2_200.0 + day * 14_000.0;
    // Warm sun, cool moon — the cinematic directional tint that
    // gives grass its golden rim at dusk and a silvery wash at night.
    let warmth = ((sun_dir.y - 0.05).clamp(-0.3, 0.4) / 0.4).clamp(-1.0, 1.0);
    let sun_tint = Color::srgb(1.0, 0.82 + warmth * 0.12, 0.62 + warmth * 0.32);
    light.color = sun_tint;

    // Ambient gets a cool tint at night, warm at sunrise/sunset.
    let sunset = (1.0 - (sun_dir.y.abs()).clamp(0.0, 1.0)).powf(3.0);
    let day_color = Color::srgb(0.72, 0.85, 1.0).to_linear();
    let night_color = Color::srgb(0.14, 0.18, 0.38).to_linear();
    let sunset_color = Color::srgb(1.0, 0.48, 0.25).to_linear();

    let base = if day > 0.0 { day_color } else { night_color };
    let amb_lin = base.mix(&sunset_color, sunset * 0.40);
    ambient.color = Color::LinearRgba(amb_lin);
    // Much brighter ambient floor so night is still visible (was 100.0).
    ambient.brightness = (380.0 + day * 550.0) * intel.profile.ambient_mul;

    // Sky (clear colour) interpolates similarly — richer gradient from
    // deep indigo night → fiery horizon → deep cyan midday.
    let sky_day = Color::srgb(0.48, 0.74, 0.98).to_linear();
    let sky_night = Color::srgb(0.012, 0.022, 0.08).to_linear();
    let sky = sky_night.mix(&sky_day, day);
    let sky = sky.mix(&sunset_color, sunset * 0.32);
    let sat: f32 = intel.profile.sky_saturation;
    let sky = sky.mix(
        &Color::srgb(0.5, 0.5, 0.5).to_linear(),
        (1.0_f32 - sat).max(0.0),
    );
    // Extra wash for showcase biomes — reads closer to neon concept art.
    let sky = match intel.biome {
        Biome::CrystalSpires => {
            let void_v = Color::srgb(0.06, 0.02, 0.20).to_linear();
            let acc_c = Color::srgb(0.04, 0.26, 0.40).to_linear();
            sky.mix(&void_v, (1.0 - day) * 0.62 + 0.07)
                .mix(&acc_c, day * 0.24 + 0.05)
        }
        Biome::AlienReef => {
            let reef = Color::srgb(0.16, 0.04, 0.22).to_linear();
            sky.mix(&reef, (1.0 - day) * 0.48 + 0.11)
        }
        _ => sky,
    };
    clear_color.0 = Color::LinearRgba(sky);

    // Drive fog colour from the same sky interpolation so the horizon
    // haze always matches the actual sky. This is THE trick that hides
    // the chunk-streaming edge for free. Uses a slightly brighter tint
    // near the horizon for atmospheric scattering feel.
    if let Ok(mut fog_settings) = fog.get_single_mut() {
        let horizon = sky
            .mix(&Color::srgb(1.0, 1.0, 1.0).to_linear(), 0.15)
            .mix(&sunset_color, sunset * 0.25);
        fog_settings.color = Color::LinearRgba(sky);
        if let FogFalloff::ExponentialSquared { density } = &mut fog_settings.falloff {
            // Fog thins at clear noon for epic long-distance vistas of
            // alien spires and mountain ranges, thickens dramatically
            // at sunset/sunrise for fiery god-ray haze.
            let base_density = 0.00055;
            *density = base_density
                * (1.0 + sunset * 1.4 + (1.0 - day) * 0.25)
                * intel.profile.fog_density_mul;
        }
        // Directional light scattering — makes god-ray / atmospheric
        // tints at sunset and during the night. Much stronger sunset
        // inscatter so the horizon glows fiery orange.
        fog_settings.directional_light_color = Color::LinearRgba(horizon);
        fog_settings.directional_light_exponent = 18.0;
    }
}

fn update_world_intel_runtime(
    world: Res<VoxelWorld>,
    player_q: Query<&Transform, With<Player>>,
    mut intel: ResMut<WorldIntelRuntime>,
) {
    let Ok(player_tf) = player_q.get_single() else {
        return;
    };
    let wx = crate::chunk::to_i32_safe(player_tf.translation.x);
    let wz = crate::chunk::to_i32_safe(player_tf.translation.z);
    let biome = world.biome_at(wx, wz);
    if biome == intel.biome {
        return;
    }
    intel.biome = biome;
    intel.profile = BiomeArtProfile::for_biome(biome);
}
