//! Day/night cycle — rotates world-space sun and moon light directions,
//! swings sky colour + fog between day and night, and drops intensity at
//! dawn/dusk. Port target: the `DayNightCycle` component from
//! `components/VoxelEngine.tsx`.

use bevy::pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap};
use bevy::prelude::*;

use crate::neurocore::RuntimeProfile;
use crate::player::Player;
use crate::settings::{GraphicsMode, TimeMode, WorldProfile, WorldSettings};
use crate::terrain::Biome;
use crate::world::VoxelWorld;

/// Relative-luminance coefficients for linear-light sRGB / BT.709 primaries.
/// Source: W3C CSS Color 4 and WCAG 2.2, derived from IEC 61966-2-1.
/// Dimensionless; coefficients sum to one within f32 precision.
const LINEAR_SRGB_LUMINANCE: Vec3 = Vec3::new(0.2126, 0.7152, 0.0722);

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
            Biome::Mesa | Biome::Desert => Self {
                fog_density_mul: 0.95,
                ambient_mul: 0.96,
                sky_saturation: 1.04,
                bloom_mul: 1.02,
                weather_fx_mul: 0.35,
                streaming_bonus: -2,
            },
            _ => Self {
                fog_density_mul: 1.0,
                ambient_mul: 1.0,
                sky_saturation: 1.0,
                bloom_mul: 1.0,
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

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DayNightSet {
    Lighting,
}

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
            .add_systems(
                Startup,
                (apply_startup_shadow_size, spawn_celestial_lights).chain(),
            )
            .add_systems(
                Update,
                (
                    advance_time,
                    update_world_intel_runtime,
                    update_sun.in_set(DayNightSet::Lighting),
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

fn terrain_directional_shadows_enabled(_mode: GraphicsMode) -> bool {
    // Terrain chunks are streamed and shadow-caster culled independently.
    // Until cascades are chunk-stable, directional terrain shadows produce
    // hard rectangular bands across sand/grass. Voxel face lighting, AO and
    // fog keep depth readable without the broken shadow plane.
    false
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

#[derive(Component)]
struct MoonKey;

fn spawn_celestial_lights(mut commands: Commands, settings: Res<WorldSettings>) {
    let cascades = cascade_config_for(settings.graphics);

    commands.spawn((
        DirectionalLightBundle {
            directional_light: DirectionalLight {
                illuminance: 10_000.0,
                shadows_enabled: terrain_directional_shadows_enabled(settings.graphics),
                ..default()
            },
            // Directional lights are translation-invariant. Keeping them at
            // the world origin prevents camera/player motion from leaking
            // into celestial transforms or shadow stabilization.
            transform: directional_light_transform(sun_direction_for_time(settings.time_of_day)),
            cascade_shadow_config: cascades,
            ..default()
        },
        bevy::pbr::VolumetricLight,
        Sun,
        Name::new("Sun.KeyLight"),
    ));

    commands.spawn((
        DirectionalLightBundle {
            directional_light: DirectionalLight {
                illuminance: 0.0,
                color: Color::srgb(0.58, 0.68, 0.92),
                shadows_enabled: false,
                ..default()
            },
            transform: directional_light_transform(moon_direction_for_time(settings.time_of_day)),
            ..default()
        },
        MoonKey,
        Name::new("Moon.KeyLight"),
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
        light.shadows_enabled = terrain_directional_shadows_enabled(settings.graphics);
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
        settings.time_of_day = time_of_day_after_delta(
            settings.time_of_day,
            settings.cycle_speed,
            time.delta_seconds(),
        );
    }
}

fn time_of_day_after_delta(
    current_hour: f32,
    speed_minutes_per_second: f32,
    delta_seconds: f32,
) -> f32 {
    let elapsed_hours = speed_minutes_per_second.max(0.0) * delta_seconds.max(0.0) / 60.0;
    (current_hour + elapsed_hours).rem_euclid(24.0)
}

fn celestial_phase(time_of_day: f32) -> f32 {
    (time_of_day.rem_euclid(24.0) / 24.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2
}

/// Unit direction from the world origin toward the visible sun.
///
/// Keep every solar consumer on this function: the directional light,
/// the reachable Helios body, atmosphere effects, and future orbital UI.
/// A single source of truth prevents the old failure where the bright sky
/// object and the shadows moved on different trajectories.
pub(crate) fn sun_direction_for_time(time_of_day: f32) -> Vec3 {
    const SUN_PATH_TILT_RAD: f32 = 17.0 * std::f32::consts::PI / 180.0;
    let phase = celestial_phase(time_of_day);
    let equatorial_direction = Vec3::new(phase.cos(), phase.sin(), 0.0);
    (Quat::from_rotation_x(SUN_PATH_TILT_RAD) * equatorial_direction).normalize()
}

/// Unit direction from the world origin toward the visible moon.
///
/// The fixed phase offset makes this a stable fictional orbit rather than an
/// Earth ephemeris. Its inclined great-circle path is shared by the visible
/// body and the moon key light, so neither depends on the camera position.
pub(crate) fn moon_direction_for_time(time_of_day: f32) -> Vec3 {
    const MOON_PHASE_OFFSET_RAD: f32 = 18.0 * std::f32::consts::PI / 180.0;
    const MOON_PATH_TILT_RAD: f32 = -6.0 * std::f32::consts::PI / 180.0;
    let phase = celestial_phase(time_of_day) + std::f32::consts::PI + MOON_PHASE_OFFSET_RAD;
    let orbital_direction = Vec3::new(phase.cos(), phase.sin(), 0.0);
    (Quat::from_rotation_x(MOON_PATH_TILT_RAD) * orbital_direction).normalize()
}

fn directional_light_transform(direction_to_source: Vec3) -> Transform {
    let direction_to_source = if direction_to_source.length_squared() > 1.0e-8 {
        direction_to_source.normalize()
    } else {
        Vec3::Y
    };
    // Bevy directional lights emit along local -Z. `from_rotation_arc`
    // avoids the unstable up-vector branch of `looking_to` near zenith.
    Transform::from_rotation(Quat::from_rotation_arc(Vec3::NEG_Z, -direction_to_source))
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SolarBlend {
    daylight: f32,
    twilight: f32,
    night: f32,
}

impl SolarBlend {
    fn for_elevation_sine(elevation_sine: f32) -> Self {
        // Civil twilight ends when the solar centre is 6 degrees below the
        // geometric horizon. NOAA Solar Calculator dawn/dusk definition.
        const CIVIL_TWILIGHT_SINE: f32 = -0.104_528_464; // sin(-6 degrees)

        let civil_light = smoothstep(CIVIL_TWILIGHT_SINE, 0.035, elevation_sine);
        let daylight = smoothstep(-0.01, 0.22, elevation_sine);
        Self {
            daylight,
            twilight: (civil_light - daylight).max(0.0),
            night: 1.0 - civil_light,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct LightingQuality {
    moon_illuminance: f32,
    twilight_illuminance: f32,
    noon_illuminance: f32,
    night_ambient: f32,
    twilight_ambient: f32,
    noon_ambient: f32,
    fog_scatter_exponent: f32,
}

/// Profile-level lighting separation, expressed as display-referred sRGB and
/// converted exactly once before linear-light mixing. Natural preserves the
/// established palette; Astral uses a warmer key and cooler fill so voxel
/// faces gain shape without depending on expensive dynamic shadow maps.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WorldLightingPalette {
    moon_key_srgb: Vec3,
    twilight_key_srgb: Vec3,
    daylight_key_srgb: Vec3,
    night_fill_srgb: Vec3,
    twilight_fill_srgb: Vec3,
    daylight_fill_srgb: Vec3,
    ambient_brightness_scale: f32,
}

fn world_lighting_palette(profile: WorldProfile) -> WorldLightingPalette {
    match profile {
        WorldProfile::Natural => WorldLightingPalette {
            moon_key_srgb: Vec3::new(0.58, 0.68, 0.92),
            twilight_key_srgb: Vec3::new(1.0, 0.58, 0.34),
            daylight_key_srgb: Vec3::new(1.0, 0.93, 0.82),
            night_fill_srgb: Vec3::new(0.31, 0.36, 0.52),
            twilight_fill_srgb: Vec3::new(0.70, 0.47, 0.42),
            daylight_fill_srgb: Vec3::new(0.72, 0.85, 1.0),
            ambient_brightness_scale: 1.0,
        },
        WorldProfile::AstralFrontier => WorldLightingPalette {
            moon_key_srgb: Vec3::new(0.46, 0.62, 1.0),
            twilight_key_srgb: Vec3::new(1.0, 0.48, 0.30),
            daylight_key_srgb: Vec3::new(1.0, 0.85, 0.70),
            night_fill_srgb: Vec3::new(0.22, 0.26, 0.50),
            twilight_fill_srgb: Vec3::new(0.58, 0.35, 0.52),
            daylight_fill_srgb: Vec3::new(0.48, 0.68, 1.0),
            // QA showed bright but nearly directionless terrain. Keeping the
            // fill at 96% of the Natural exposure restores a warm-key/cool-
            // fill ratio while remaining above the readability floor.
            ambient_brightness_scale: 0.96,
        },
    }
}

fn lighting_quality(profile: RuntimeProfile, graphics: GraphicsMode) -> LightingQuality {
    let tier = match profile {
        RuntimeProfile::LowSpec => GraphicsMode::Fast,
        RuntimeProfile::Balanced => GraphicsMode::Balanced,
        RuntimeProfile::Cinematic => GraphicsMode::High,
        RuntimeProfile::Auto | RuntimeProfile::Benchmark => graphics,
    };

    match tier {
        GraphicsMode::Fast => LightingQuality {
            // These are exposure-compensated gameplay lux, not physical
            // moonlight. The roughly 12:1 day/night key ratio preserves
            // terrain readability while restoring visible day/night contrast.
            moon_illuminance: 1_800.0,
            twilight_illuminance: 9_200.0,
            noon_illuminance: 22_000.0,
            night_ambient: 1_020.0,
            twilight_ambient: 1_820.0,
            noon_ambient: 2_620.0,
            fog_scatter_exponent: 56.0,
        },
        GraphicsMode::Balanced => LightingQuality {
            moon_illuminance: 2_200.0,
            twilight_illuminance: 10_600.0,
            noon_illuminance: 25_500.0,
            night_ambient: 1_160.0,
            twilight_ambient: 2_060.0,
            noon_ambient: 3_020.0,
            fog_scatter_exponent: 40.0,
        },
        GraphicsMode::High => LightingQuality {
            moon_illuminance: 2_600.0,
            twilight_illuminance: 12_000.0,
            noon_illuminance: 29_000.0,
            night_ambient: 1_260.0,
            twilight_ambient: 2_280.0,
            noon_ambient: 3_420.0,
            fog_scatter_exponent: 30.0,
        },
    }
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn update_sun(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    mut clear_color: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), (With<Sun>, Without<MoonKey>)>,
    mut moon: Query<(&mut Transform, &mut DirectionalLight), (With<MoonKey>, Without<Sun>)>,
    mut fog: Query<&mut FogSettings>,
) {
    let Ok((mut sun_transform, mut sun_light)) = sun.get_single_mut() else {
        return;
    };
    let Ok((mut moon_transform, mut moon_light)) = moon.get_single_mut() else {
        return;
    };

    let sun_dir = sun_direction_for_time(settings.time_of_day);
    let moon_dir = moon_direction_for_time(settings.time_of_day);
    *sun_transform = directional_light_transform(sun_dir);
    *moon_transform = directional_light_transform(moon_dir);

    // Day factor 0..1 where 1 = high noon, 0 = deep night.
    let solar = SolarBlend::for_elevation_sine(sun_dir.y);
    let quality = lighting_quality(settings.runtime_profile, settings.graphics);
    let world_profile = settings.effective_world_profile();
    let palette = world_lighting_palette(world_profile);
    sun_light.illuminance = sun_illuminance_for_conditions(sun_dir.y, solar, quality);
    moon_light.illuminance = moon_illuminance_for_conditions(solar, quality);
    // Warm sun, cool moon — the cinematic directional tint that
    // gives grass its golden rim at dusk and a silvery wash at night.
    let moon_key = Color::srgb(
        palette.moon_key_srgb.x,
        palette.moon_key_srgb.y,
        palette.moon_key_srgb.z,
    )
    .to_linear();
    let twilight_key = Color::srgb(
        palette.twilight_key_srgb.x,
        palette.twilight_key_srgb.y,
        palette.twilight_key_srgb.z,
    )
    .to_linear();
    let daylight_key = Color::srgb(
        palette.daylight_key_srgb.x,
        palette.daylight_key_srgb.y,
        palette.daylight_key_srgb.z,
    )
    .to_linear();
    let sun_color = twilight_key.mix(&daylight_key, smoothstep(0.0, 0.45, sun_dir.y.max(0.0)));
    sun_light.color = Color::LinearRgba(sun_color);
    moon_light.color = Color::LinearRgba(moon_key);

    // Ambient gets a cool tint at night, warm at sunrise/sunset.
    let day_color = Color::srgb(
        palette.daylight_fill_srgb.x,
        palette.daylight_fill_srgb.y,
        palette.daylight_fill_srgb.z,
    )
    .to_linear();
    let night_color = Color::srgb(
        palette.night_fill_srgb.x,
        palette.night_fill_srgb.y,
        palette.night_fill_srgb.z,
    )
    .to_linear();
    let twilight_color = Color::srgb(
        palette.twilight_fill_srgb.x,
        palette.twilight_fill_srgb.y,
        palette.twilight_fill_srgb.z,
    )
    .to_linear();
    let amb_lin = weighted_linear_color(night_color, twilight_color, day_color, solar);
    ambient.color = Color::LinearRgba(amb_lin);
    ambient.brightness =
        ambient_brightness_for_conditions(sun_dir.y, solar, quality, intel.profile.ambient_mul)
            * palette.ambient_brightness_scale;

    // Sky (clear colour) interpolates similarly — grounded blue day,
    // readable twilight, and deep indigo night without a milky clear fog.
    let sky_rgb = sky_linear_rgb_for_profile_conditions(
        solar,
        intel.profile.sky_saturation,
        intel.biome,
        world_profile,
    );
    let sky = LinearRgba::rgb(sky_rgb.x, sky_rgb.y, sky_rgb.z);
    clear_color.0 = Color::LinearRgba(sky);

    // Drive fog colour from the same sky interpolation so the horizon
    // haze always matches the actual sky. This is THE trick that hides
    // the chunk-streaming edge for free. Uses a slightly brighter tint
    // near the horizon for atmospheric scattering feel.
    if let Ok(mut fog_settings) = fog.get_single_mut() {
        let (profile_haze, haze_mix) = if world_profile == WorldProfile::AstralFrontier {
            (Color::srgb(0.46, 0.58, 0.82).to_linear(), 0.08)
        } else {
            (Color::srgb(0.78, 0.82, 0.86).to_linear(), 0.02)
        };
        let horizon = sky
            .mix(&profile_haze, haze_mix)
            .mix(&twilight_key, solar.twilight * 0.12);
        fog_settings.color = Color::LinearRgba(sky.mix(&profile_haze, haze_mix * 0.55));
        // Directional light scattering — makes god-ray / atmospheric
        // tints at sunset and during the night. Much stronger sunset
        // inscatter so the horizon glows fiery orange.
        fog_settings.directional_light_color = Color::LinearRgba(horizon);
        fog_settings.directional_light_exponent = quality.fog_scatter_exponent;
    }
}

fn weighted_linear_color(
    night: LinearRgba,
    twilight: LinearRgba,
    daylight: LinearRgba,
    solar: SolarBlend,
) -> LinearRgba {
    night * solar.night + twilight * solar.twilight + daylight * solar.daylight
}

fn sun_illuminance_for_conditions(
    elevation_sine: f32,
    solar: SolarBlend,
    quality: LightingQuality,
) -> f32 {
    let height = elevation_sine.max(0.0).sqrt();
    let daylight = quality.twilight_illuminance
        + (quality.noon_illuminance - quality.twilight_illuminance) * height;
    quality.twilight_illuminance * solar.twilight + daylight * solar.daylight
}

fn moon_illuminance_for_conditions(solar: SolarBlend, quality: LightingQuality) -> f32 {
    quality.moon_illuminance * smoothstep(0.0, 0.92, solar.night)
}

fn ambient_brightness_for_conditions(
    elevation_sine: f32,
    solar: SolarBlend,
    quality: LightingQuality,
    biome_ambient_mul: f32,
) -> f32 {
    let height = elevation_sine.max(0.0).sqrt();
    let daylight =
        quality.twilight_ambient + (quality.noon_ambient - quality.twilight_ambient) * height;
    let base = quality.night_ambient * solar.night
        + quality.twilight_ambient * solar.twilight
        + daylight * solar.daylight;
    base * biome_ambient_mul.clamp(0.90, 1.12)
}

fn lerp_vec3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

fn srgb_vec3_to_linear_rgb(srgb: Vec3) -> Vec3 {
    // Bevy implements the IEC 61966-2-1 piecewise sRGB transfer function.
    // Author-facing palette triples are decoded exactly once; all following
    // interpolation and luminance operations stay in linear-light RGB.
    let linear = Color::srgb(srgb.x, srgb.y, srgb.z).to_linear();
    Vec3::new(linear.red, linear.green, linear.blue)
}

fn adjust_linear_saturation(rgb: Vec3, saturation: f32) -> Vec3 {
    let luminance = rgb.dot(LINEAR_SRGB_LUMINANCE);
    let saturation = if saturation.is_finite() {
        saturation.clamp(0.72, 1.48)
    } else {
        1.0
    };
    (Vec3::splat(luminance) + (rgb - Vec3::splat(luminance)) * saturation)
        .clamp(Vec3::ZERO, Vec3::ONE)
}

fn sky_linear_rgb_for_conditions(solar: SolarBlend, sky_saturation: f32, biome: Biome) -> Vec3 {
    let sky_day = srgb_vec3_to_linear_rgb(Vec3::new(0.38, 0.64, 0.94));
    let sky_twilight = srgb_vec3_to_linear_rgb(Vec3::new(0.38, 0.28, 0.40));
    let sky_night = srgb_vec3_to_linear_rgb(Vec3::new(0.095, 0.125, 0.22));

    // SolarBlend weights are normalized. Mixing decoded endpoints therefore
    // models additive radiance instead of averaging gamma-encoded display
    // values, which previously made the civil-twilight interval too dark.
    let sky = sky_night * solar.night + sky_twilight * solar.twilight + sky_day * solar.daylight;
    let sky = adjust_linear_saturation(sky, sky_saturation);

    match biome {
        Biome::CrystalSpires => {
            let void_v = srgb_vec3_to_linear_rgb(Vec3::new(0.06, 0.02, 0.20));
            let acc_c = srgb_vec3_to_linear_rgb(Vec3::new(0.04, 0.26, 0.40));
            lerp_vec3(
                lerp_vec3(sky, void_v, solar.night * 0.62 + 0.07),
                acc_c,
                solar.daylight * 0.24 + 0.05,
            )
        }
        Biome::AlienReef => {
            let reef = srgb_vec3_to_linear_rgb(Vec3::new(0.16, 0.04, 0.22));
            lerp_vec3(sky, reef, solar.night * 0.48 + 0.11)
        }
        _ => sky,
    }
}

fn sky_linear_rgb_for_profile_conditions(
    solar: SolarBlend,
    sky_saturation: f32,
    biome: Biome,
    world_profile: WorldProfile,
) -> Vec3 {
    let natural = sky_linear_rgb_for_conditions(solar, sky_saturation, biome);
    if world_profile == WorldProfile::Natural {
        return natural;
    }

    // This is the low-frequency atmosphere underneath the textured nebula,
    // not the nebula itself. A darker indigo/cobalt foundation gives its
    // cyan and rose filaments room to read while retaining a bright daytime
    // horizon and smooth day/night exposure.
    let astral_day = srgb_vec3_to_linear_rgb(Vec3::new(0.24, 0.47, 0.80));
    let astral_twilight = srgb_vec3_to_linear_rgb(Vec3::new(0.37, 0.19, 0.45));
    let astral_night = srgb_vec3_to_linear_rgb(Vec3::new(0.045, 0.055, 0.16));
    let astral =
        astral_night * solar.night + astral_twilight * solar.twilight + astral_day * solar.daylight;
    lerp_vec3(natural, astral, 0.52)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terrain_directional_shadows_stay_off_to_avoid_chunk_shadow_bands() {
        assert!(!terrain_directional_shadows_enabled(GraphicsMode::Fast));
        assert!(!terrain_directional_shadows_enabled(GraphicsMode::Balanced));
        assert!(!terrain_directional_shadows_enabled(GraphicsMode::High));
    }

    #[test]
    fn night_ambient_floor_keeps_world_readable() {
        let solar = SolarBlend::for_elevation_sine(-1.0);
        let quality = lighting_quality(RuntimeProfile::Balanced, GraphicsMode::Balanced);
        assert!(
            ambient_brightness_for_conditions(-1.0, solar, quality, 1.0) >= 1_100.0,
            "night ambient must keep terrain/trees readable instead of black silhouettes"
        );
        let sky = sky_linear_rgb_for_conditions(solar, 1.0, Biome::Plains);
        let authored_night = srgb_vec3_to_linear_rgb(Vec3::new(0.095, 0.125, 0.22));
        assert!(sky.min_element() >= authored_night.min_element() - 1.0e-6);
    }

    #[test]
    fn civil_twilight_is_smooth_and_keeps_directional_form() {
        let before = SolarBlend::for_elevation_sine(-0.001);
        let horizon = SolarBlend::for_elevation_sine(0.0);
        let after = SolarBlend::for_elevation_sine(0.001);
        let quality = lighting_quality(RuntimeProfile::Balanced, GraphicsMode::Balanced);

        assert!(horizon.twilight > 0.75);
        assert!((before.twilight - after.twilight).abs() < 0.04);
        assert!(
            sun_illuminance_for_conditions(0.0, horizon, quality)
                + moon_illuminance_for_conditions(horizon, quality)
                >= 7_500.0,
            "low sun should still give visible form and not collapse to black"
        );
    }

    #[test]
    fn normal_day_sky_is_blue_not_whitewashed() {
        let solar = SolarBlend::for_elevation_sine(1.0);
        let sky = sky_linear_rgb_for_conditions(solar, 1.0, Biome::Plains);

        assert!(
            sky.z > sky.y && sky.y > sky.x,
            "clear day sky should stay visibly blue"
        );
        assert!(
            sky.x <= srgb_vec3_to_linear_rgb(Vec3::splat(0.46)).x,
            "clear day red channel should stay low enough to avoid milky fog"
        );
    }

    #[test]
    fn astral_palette_adds_warm_key_cool_fill_without_changing_natural() {
        let natural = world_lighting_palette(WorldProfile::Natural);
        assert_eq!(natural.daylight_key_srgb, Vec3::new(1.0, 0.93, 0.82));
        assert_eq!(natural.daylight_fill_srgb, Vec3::new(0.72, 0.85, 1.0));
        assert_eq!(natural.ambient_brightness_scale, 1.0);

        let astral = world_lighting_palette(WorldProfile::AstralFrontier);
        assert!(astral.daylight_key_srgb.x > astral.daylight_key_srgb.z);
        assert!(astral.daylight_fill_srgb.z > astral.daylight_fill_srgb.x);
        assert!((0.90..1.0).contains(&astral.ambient_brightness_scale));
    }

    #[test]
    fn astral_clear_sky_is_profile_scoped_and_leaves_room_for_nebulae() {
        let solar = SolarBlend::for_elevation_sine(1.0);
        let legacy = sky_linear_rgb_for_conditions(solar, 1.0, Biome::Plains);
        let natural =
            sky_linear_rgb_for_profile_conditions(solar, 1.0, Biome::Plains, WorldProfile::Natural);
        let astral = sky_linear_rgb_for_profile_conditions(
            solar,
            1.0,
            Biome::Plains,
            WorldProfile::AstralFrontier,
        );
        assert_eq!(natural, legacy);
        assert!(astral.z > astral.y && astral.y > astral.x);
        assert!(astral.max_element() < natural.max_element());
    }

    #[test]
    fn evening_sky_keeps_readable_brightness_floor() {
        let sky =
            sky_linear_rgb_for_conditions(SolarBlend::for_elevation_sine(0.0), 1.0, Biome::Plains);
        let luminance = sky.dot(LINEAR_SRGB_LUMINANCE);

        assert!(
            luminance >= 0.06,
            "evening sky should not collapse to an overly dark horizon"
        );
    }

    #[test]
    fn sky_palette_interpolates_in_linear_light_and_preserves_endpoints() {
        let night = sky_linear_rgb_for_conditions(
            SolarBlend {
                daylight: 0.0,
                twilight: 0.0,
                night: 1.0,
            },
            1.0,
            Biome::Plains,
        );
        let day = sky_linear_rgb_for_conditions(
            SolarBlend {
                daylight: 1.0,
                twilight: 0.0,
                night: 0.0,
            },
            1.0,
            Biome::Plains,
        );
        assert!(night.distance(srgb_vec3_to_linear_rgb(Vec3::new(0.095, 0.125, 0.22))) < 1.0e-6);
        assert!(day.distance(srgb_vec3_to_linear_rgb(Vec3::new(0.38, 0.64, 0.94))) < 1.0e-6);

        let midpoint = sky_linear_rgb_for_conditions(
            SolarBlend {
                daylight: 0.5,
                twilight: 0.0,
                night: 0.5,
            },
            1.0,
            Biome::Plains,
        );
        assert!(midpoint.distance((night + day) * 0.5) < 1.0e-6);
    }

    #[test]
    fn biome_saturation_changes_chroma_without_moving_linear_luminance() {
        let input = srgb_vec3_to_linear_rgb(Vec3::new(0.28, 0.52, 0.78));
        let neutral = adjust_linear_saturation(input, 1.0);
        let vivid = adjust_linear_saturation(input, 1.35);
        let neutral_luminance = neutral.dot(LINEAR_SRGB_LUMINANCE);
        let vivid_luminance = vivid.dot(LINEAR_SRGB_LUMINANCE);
        let neutral_chroma = neutral.max_element() - neutral.min_element();
        let vivid_chroma = vivid.max_element() - vivid.min_element();

        assert!((neutral_luminance - vivid_luminance).abs() < 1.0e-6);
        assert!(vivid_chroma > neutral_chroma);
    }

    #[test]
    fn every_route_sky_output_is_finite_and_bounded() {
        let biomes = [
            Biome::Plains,
            Biome::CrystalSpires,
            Biome::AlienReef,
            Biome::VolcanicWaste,
        ];
        for profile in [WorldProfile::Natural, WorldProfile::AstralFrontier] {
            for biome in biomes {
                for elevation in [-1.0, -0.104_528_464, -0.01, 0.0, 0.22, 1.0] {
                    for saturation in [f32::NAN, -10.0, 0.72, 1.0, 1.48, 10.0] {
                        let sky = sky_linear_rgb_for_profile_conditions(
                            SolarBlend::for_elevation_sine(elevation),
                            saturation,
                            biome,
                            profile,
                        );
                        assert!(sky.is_finite());
                        assert!(sky.min_element() >= 0.0);
                        assert!(sky.max_element() <= 1.0);
                    }
                }
            }
        }
    }

    #[test]
    fn solar_weights_are_normalized_across_day_twilight_and_night() {
        for elevation in [-1.0, -0.104_528_464, -0.03, 0.0, 0.1, 0.4, 1.0] {
            let solar = SolarBlend::for_elevation_sine(elevation);
            let sum = solar.daylight + solar.twilight + solar.night;
            assert!((sum - 1.0).abs() < 1.0e-5);
            assert!(solar.daylight >= 0.0 && solar.twilight >= 0.0 && solar.night >= 0.0);
        }
    }

    #[test]
    fn quality_profiles_scale_light_without_sacrificing_low_spec_readability() {
        let solar = SolarBlend::for_elevation_sine(-1.0);
        let low = lighting_quality(RuntimeProfile::LowSpec, GraphicsMode::High);
        let balanced = lighting_quality(RuntimeProfile::Balanced, GraphicsMode::Balanced);
        let cinematic = lighting_quality(RuntimeProfile::Cinematic, GraphicsMode::Fast);

        assert!(low.night_ambient >= 1_000.0);
        assert!(low.night_ambient < balanced.night_ambient);
        assert!(balanced.night_ambient < cinematic.night_ambient);
        assert!(
            moon_illuminance_for_conditions(solar, low)
                < moon_illuminance_for_conditions(solar, cinematic)
        );
        let noon = SolarBlend::for_elevation_sine(1.0);
        assert!(
            sun_illuminance_for_conditions(1.0, noon, low)
                < sun_illuminance_for_conditions(1.0, noon, cinematic)
        );
        assert!(low.fog_scatter_exponent > cinematic.fog_scatter_exponent);
    }

    #[test]
    fn noon_skylight_fill_is_strong_enough_for_voxel_cliff_readability() {
        let solar = SolarBlend::for_elevation_sine(1.0);
        for quality in [
            lighting_quality(RuntimeProfile::LowSpec, GraphicsMode::Fast),
            lighting_quality(RuntimeProfile::Balanced, GraphicsMode::Balanced),
            lighting_quality(RuntimeProfile::Cinematic, GraphicsMode::High),
        ] {
            let key = sun_illuminance_for_conditions(1.0, solar, quality);
            let fill = ambient_brightness_for_conditions(1.0, solar, quality, 1.0);
            assert!(
                fill >= key * 0.10,
                "sky fill {fill:.0} must keep unlit voxel faces above ink-black against key {key:.0}"
            );
            assert!(
                fill < key * 0.20,
                "key direction must still shape the scene"
            );
        }
    }

    #[test]
    fn shared_sun_direction_is_normalized_and_wraps_at_midnight() {
        let before = sun_direction_for_time(0.0);
        let after = sun_direction_for_time(24.0);

        assert!((before.length() - 1.0).abs() < 1.0e-5);
        assert!(before.distance(after) < 1.0e-5);
    }

    #[test]
    fn shared_sun_direction_places_noon_above_the_horizon() {
        assert!(sun_direction_for_time(12.0).y > 0.9);
        assert!(sun_direction_for_time(0.0).y < -0.9);
    }

    #[test]
    fn shared_moon_direction_is_world_fixed_and_opposes_the_sun_at_night() {
        let moon = moon_direction_for_time(0.0);
        let wrapped = moon_direction_for_time(24.0);
        let midnight_sun = sun_direction_for_time(0.0);

        assert!((moon.length() - 1.0).abs() < 1.0e-5);
        assert!(moon.distance(wrapped) < 1.0e-5);
        assert!(moon.y > 0.9);
        assert!(moon.dot(midnight_sun) < -0.8);
    }

    #[test]
    fn cycle_speed_uses_minutes_per_second() {
        let after_one_second = time_of_day_after_delta(12.0, 1.0, 1.0);
        assert!((after_one_second - (12.0 + 1.0 / 60.0)).abs() < 1.0e-6);

        let wrapped = time_of_day_after_delta(23.99, 1.0, 1.0);
        assert!((wrapped - 0.006_666_2).abs() < 1.0e-5);
    }

    #[test]
    fn directional_light_rotation_is_stable_and_translation_independent() {
        for direction in [
            sun_direction_for_time(0.0),
            sun_direction_for_time(6.0),
            sun_direction_for_time(12.0),
            moon_direction_for_time(0.0),
        ] {
            let transform = directional_light_transform(direction);
            let emitted_forward = transform.rotation * Vec3::NEG_Z;
            assert_eq!(transform.translation, Vec3::ZERO);
            assert!(emitted_forward.dot(-direction) > 0.9999);
            assert!(transform.rotation.is_finite());
        }
    }
}
