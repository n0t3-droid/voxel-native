//! Day/night cycle — moves a directional "sun" light around the player,
//! swings sky colour + fog between day and night, and drops intensity at
//! dawn/dusk. Port target: the `DayNightCycle` component from
//! `components/VoxelEngine.tsx`.

use bevy::pbr::{CascadeShadowConfigBuilder, DirectionalLightShadowMap, FogFalloff};
use bevy::prelude::*;

use crate::blocks::BlockType;
use crate::player::Player;
use crate::settings::{GraphicsMode, TimeMode, WorldSettings};
use crate::terrain::Biome;
use crate::textures::MaterialLibrary;
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
                    sync_postcard_bounce_lights,
                    snap_postcard_bounce_to_ground,
                    night_terrain_emissive_floor,
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

/// Shadowless bounce fill. Cheap on Fast (no cascades) and is what
/// keeps mesa banding readable at night without a second noon key.
#[derive(Component)]
pub struct FillLight;

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

    commands.spawn((
        DirectionalLightBundle {
            directional_light: DirectionalLight {
                illuminance: 0.0,
                shadows_enabled: false,
                color: Color::srgb(0.55, 0.62, 0.88),
                ..default()
            },
            transform: Transform::from_xyz(-80.0, 240.0, -40.0).looking_at(Vec3::ZERO, Vec3::Y),
            ..default()
        },
        FillLight,
    ));

    commands.insert_resource(AmbientLight {
        color: Color::srgb(0.7, 0.8, 1.0),
        brightness: 200.0,
    });
}

/// Unshadowed point fill parked on the spawn postcard. Lights nearby
/// mesa faces from crystals / plasma so night banding reads without a
/// second noon key. Fast GraphicsMode never spawns these.
#[derive(Component)]
struct PostcardBounceLight {
    base_intensity: f32,
    wx: i32,
    wz: i32,
    y_offset: f32,
}

/// wx, wz, y_offset above surface, r, g, b, lumens, range.
const POSTCARD_BOUNCE: [(i32, i32, f32, f32, f32, f32, f32, f32); 10] = [
    (72, -96, 8.0, 0.32, 0.82, 1.0, 420_000.0, 36.0),
    (80, -90, 5.0, 0.28, 0.78, 1.0, 260_000.0, 22.0),
    (142, -78, 8.0, 0.95, 0.28, 0.82, 360_000.0, 32.0),
    (90, -72, 3.0, 0.18, 0.78, 1.0, 340_000.0, 28.0),
    (90, -66, 4.0, 0.18, 0.78, 1.0, 220_000.0, 20.0),
    (90, -78, 4.0, 1.0, 0.42, 0.12, 260_000.0, 22.0),
    (120, -72, 3.0, 0.18, 0.78, 1.0, 280_000.0, 24.0),
    (150, -72, 3.0, 0.18, 0.78, 1.0, 300_000.0, 28.0),
    (108, -132, 6.0, 1.0, 0.62, 0.32, 180_000.0, 20.0),
    (96, -140, 5.0, 1.0, 0.55, 0.22, 170_000.0, 20.0),
];

fn night_bounce_dir(key_dir: Vec3, night: bool) -> Vec3 {
    if night {
        // Side skimming so vertical mesa faces get N·L. High-Y fill
        // only lit +Y tops and left the postcard cliffs crushed.
        Vec3::new(-key_dir.x * 0.95, 0.32, -key_dir.z * 1.15).normalize()
    } else {
        Vec3::new(-key_dir.x * 0.35, 0.82, -key_dir.z * 0.55).normalize()
    }
}

fn sync_postcard_bounce_lights(
    mut commands: Commands,
    settings: Res<WorldSettings>,
    existing: Query<Entity, With<PostcardBounceLight>>,
) {
    if settings.graphics == GraphicsMode::Fast {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
        return;
    }
    if !existing.is_empty() {
        return;
    }
    for &(wx, wz, y_offset, r, g, b, lumens, range) in &POSTCARD_BOUNCE {
        commands.spawn((
            PointLightBundle {
                point_light: PointLight {
                    color: Color::srgb(r, g, b),
                    intensity: 0.0,
                    range,
                    shadows_enabled: false,
                    ..default()
                },
                transform: Transform::from_xyz(wx as f32 + 0.5, 80.0 + y_offset, wz as f32 + 0.5),
                ..default()
            },
            PostcardBounceLight {
                base_intensity: lumens,
                wx,
                wz,
                y_offset,
            },
        ));
    }
}

fn snap_postcard_bounce_to_ground(
    world: Option<Res<VoxelWorld>>,
    mut q: Query<(&PostcardBounceLight, &mut Transform)>,
) {
    let Some(world) = world else {
        return;
    };
    for (spec, mut tf) in &mut q {
        let h = world.surface_height_at(spec.wx, spec.wz) as f32;
        tf.translation.x = spec.wx as f32 + 0.5;
        tf.translation.z = spec.wz as f32 + 0.5;
        tf.translation.y = h + spec.y_offset;
    }
}

/// Tiny night emissive floor on mesa stone so banding hues survive ACES
/// without a second directional key. Fast stays at zero. Crystals are
/// not in this list — they already sit well above the bloom threshold.
fn night_terrain_emissive_floor(
    settings: Res<WorldSettings>,
    lib: Option<Res<MaterialLibrary>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut last: Local<Option<u8>>,
) {
    let Some(lib) = lib else {
        return;
    };
    let sun = sun_direction(settings.time_of_day);
    let night_amt = (1.0 - day_factor(sun)).powf(1.55);
    let band = if night_amt > 0.55 {
        0u8
    } else if night_amt > 0.18 {
        1
    } else {
        2
    };
    if *last == Some(band) {
        return;
    }
    *last = Some(band);
    let floor = if settings.graphics == GraphicsMode::Fast {
        0.0
    } else {
        night_amt * 0.050
    };
    let e = LinearRgba::rgb(floor * 1.22, floor * 0.52, floor * 0.34);
    for block in [
        BlockType::RedStone,
        BlockType::MesaClay,
        BlockType::AmberStone,
        BlockType::VioletStone,
        BlockType::RedSand,
        BlockType::Basalt,
    ] {
        if let Some(handle) = lib.handle_for(block as u16) {
            if let Some(mat) = materials.get_mut(&handle) {
                mat.emissive = e;
            }
        }
    }
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

/// Cinematic solar geometry shared with `sky.rs`.
///
/// A naive 24 h mapping (`hour/24 * τ − π/2`) puts sunset at 18:00 with
/// `sun_dir.y == 0`. Lambert shading on walkable +Y faces then goes to
/// black and `day = y.max(0)` trips the night colour branch — the
/// silhouette-dusk bug. The key art is golden hour, so we linger the
/// sun above the horizon through 18:00 (sunset ~19:15) and keep a
/// shallow +Z lean so the disc never sits in a perfect cardinal plane.
pub fn sun_direction(time_of_day: f32) -> Vec3 {
    let hour = time_of_day.rem_euclid(24.0);
    // Civil-ish day length: sunrise ~05:45, sunset ~19:15.
    // Hour 17 sits ~25–30° up (late afternoon); hour 18 is ~12–16°
    // (true golden hour). Night at 21.5 has the sun well below.
    const SUNRISE: f32 = 5.75;
    const SUNSET: f32 = 19.25;
    let solar = (hour - SUNRISE) / (SUNSET - SUNRISE);
    let elev = solar * std::f32::consts::PI;
    Vec3::new(elev.cos(), elev.sin(), 0.3).normalize()
}

/// 1 at noon, still clearly "day" through golden hour, 0 at true night.
/// Using raw `sun_dir.y.max(0)` treated the horizon as night.
pub fn day_factor(sun_dir: Vec3) -> f32 {
    ((sun_dir.y + 0.18) / 0.90).clamp(0.0, 1.0)
}

/// Bell curve peaking near hour 17 (sun_dir.y ≈ 0.48). Pass-2 peaked
/// too close to true sunset, so the 17:00 postcard stayed a cool
/// afternoon instead of a golden rim. Hour 11 (y ≈ 0.95) is still 0.
pub fn sunset_factor(sun_dir: Vec3) -> f32 {
    let peak = 0.46;
    let width = 0.44;
    let t = ((sun_dir.y - peak) / width).abs();
    (1.0 - t.min(1.0)).powf(1.25)
}

fn update_sun(
    settings: Res<WorldSettings>,
    intel: Res<WorldIntelRuntime>,
    mut clear_color: ResMut<ClearColor>,
    mut ambient: ResMut<AmbientLight>,
    mut sun: Query<(&mut Transform, &mut DirectionalLight), With<Sun>>,
    mut fill: Query<(&mut Transform, &mut DirectionalLight), (With<FillLight>, Without<Sun>)>,
    mut bounce_lights: Query<(&PostcardBounceLight, &mut PointLight)>,
    mut fog: Query<&mut FogSettings>,
) {
    let Ok((mut transform, mut light)) = sun.get_single_mut() else {
        return;
    };

    let sun_dir = sun_direction(settings.time_of_day);
    let day = day_factor(sun_dir);
    let sunset = sunset_factor(sun_dir);

    // Keep a shallow key so ground N·L never hits zero during dusk.
    // Once the sun is truly down this becomes a cool moonlight from
    // the same azimuth, high enough to light walkable +Y faces so
    // mesas stay readable instead of pure silhouette.
    let mut key_dir = sun_dir;
    if key_dir.y < 0.12 {
        // Night key: skim vertical cliff faces (y ~ 0.36) rather than
        // a high moon that only paints +Y tops.
        key_dir.y = if sun_dir.y < -0.12 { 0.36 } else { 0.16 };
        key_dir = key_dir.normalize();
    }

    // Directional lights in Bevy shine along their -Z.
    let forward = -key_dir;
    *transform = Transform::from_xyz(key_dir.x * 400.0, key_dir.y * 400.0, key_dir.z * 400.0)
        .looking_to(forward, Vec3::Y);

    if sun_dir.y < -0.12 {
        // Night key: high enough to paint +Y strata (banding), still
        // well below crystal/lava HDR. Cool-but-not-icy so red mesa
        // albedo doesn't go grey under moonlight.
        light.illuminance = 5_600.0 + (1.0 - day) * 1_200.0;
        light.color = Color::srgb(0.82, 0.78, 0.92);
    } else {
        // Warm key. Sunset adds extra illuminance so low elevation
        // still rims the mesas instead of silhouetting them.
        light.illuminance = 3_200.0 + day.powf(0.85) * 13_000.0 + sunset * 5_200.0;
        light.color = Color::srgb(
            1.0,
            0.78 + day * 0.16 - sunset * 0.22,
            0.50 + day * 0.40 - sunset * 0.38,
        );
    }

    // Opposite-azimuth bounce. No shadows, so Fast pays one extra
    // unshadowed directional. Night is the whole point; noon stays
    // near zero so the look does not flatten. Night bounce comes in
    // from the side so mesa strata (vertical faces) actually read.
    if let Ok((mut fill_tf, mut fill_light)) = fill.get_single_mut() {
        let night_side = sun_dir.y < -0.12;
        let bounce = night_bounce_dir(key_dir, night_side);
        let fill_forward = -bounce;
        *fill_tf = Transform::from_xyz(bounce.x * 280.0, bounce.y * 280.0, bounce.z * 280.0)
            .looking_to(fill_forward, Vec3::Y);
        let night_amt = (1.0 - day).powf(1.35);
        fill_light.illuminance = 120.0 + sunset * 480.0 + night_amt * 4_200.0;
        fill_light.color = if sun_dir.y < -0.12 {
            Color::srgb(0.62, 0.66, 0.92)
        } else {
            Color::srgb(1.0, 0.70, 0.38)
        };
    }

    let night_amt = (1.0 - day).powf(1.35);
    let dusk_glow = sunset * 0.32;
    for (spec, mut point) in &mut bounce_lights {
        point.intensity = spec.base_intensity * (night_amt + dusk_glow);
    }

    // Ambient: warm fill through dusk so shadowed canyon floors stay
    // readable; a lifted cool floor at night so ground never crushes.
    // Keep dusk ambient well below the key so long shadows survive.
    // Night isotropic is modest now that FillLight carries the bounce.
    let day_color = Color::srgb(0.80, 0.88, 1.0).to_linear();
    let night_color = Color::srgb(0.38, 0.36, 0.52).to_linear();
    let sunset_color = Color::srgb(1.0, 0.52, 0.26).to_linear();
    let golden_fill = Color::srgb(1.0, 0.74, 0.44).to_linear();
    let night_amt = (1.0 - day).powf(1.55);
    let amb_lin = day_color
        .mix(&night_color, night_amt)
        .mix(&sunset_color, sunset * 0.50)
        .mix(&golden_fill, sunset * 0.38);
    ambient.color = Color::LinearRgba(amb_lin);
    ambient.brightness =
        (1_560.0 + day * 140.0 + sunset * 280.0 + night_amt * 1_720.0) * intel.profile.ambient_mul;

    // Sky (clear colour). Dusk zenith goes violet; the golden rim is
    // owned by `sky.rs` so we do not dye the whole dome orange.
    let sky_day = Color::srgb(0.42, 0.68, 0.96).to_linear();
    let sky_night = Color::srgb(0.018, 0.020, 0.09).to_linear();
    let sky_violet = Color::srgb(0.12, 0.04, 0.26).to_linear();
    let sky = sky_night
        .mix(&sky_day, day * (1.0 - sunset * 0.62))
        .mix(&sky_violet, (1.0 - day) * 0.50 + sunset * 0.38)
        .mix(&sunset_color, sunset * 0.06);
    let sat: f32 = intel.profile.sky_saturation;
    let sky = sky.mix(
        &Color::srgb(0.5, 0.5, 0.5).to_linear(),
        (1.0_f32 - sat).max(0.0),
    );
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

    // Atmospheric fog. Weather's Linear fog is skipped on Clear days
    // so this ExponentialSquared aerial perspective actually lands.
    // Midday is thinned (no milky bleach). Dusk keeps warm inscatter
    // without a density wall on walkable ground. Night fog is a lifted
    // fill, never a black cut-out.
    if let Ok(mut fog_settings) = fog.get_single_mut() {
        let horizon_day = Color::srgb(0.58, 0.72, 0.90).to_linear();
        let horizon_dusk = Color::srgb(1.0, 0.42, 0.12).to_linear();
        let horizon_night = Color::srgb(0.14, 0.10, 0.24).to_linear();
        let horizon = horizon_night
            .mix(&horizon_day, day * (1.0 - sunset))
            .mix(&horizon_dusk, sunset);
        let mut fog_fill = sky.mix(&horizon, 0.18).mix(&golden_fill, sunset * 0.40);
        fog_fill.alpha = (0.12 + sunset * 0.10 + (1.0 - day) * 0.06 - day.powf(1.8) * 0.04)
            .clamp(0.07, 0.26);
        fog_settings.color = Color::LinearRgba(fog_fill);
        fog_settings.falloff = FogFalloff::ExponentialSquared {
            density: 0.00009
                * (1.0 + sunset * 0.22 + (1.0 - day) * 0.08 - day.powf(1.8) * 0.62)
                * intel.profile.fog_density_mul,
        };
        let mut sun_scatter = horizon;
        sun_scatter.alpha = 0.12 + sunset * 0.28;
        fog_settings.directional_light_color = Color::LinearRgba(sun_scatter);
        fog_settings.directional_light_exponent = 14.0;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_hour_keeps_the_sun_above_the_horizon() {
        // Hour 18 used to map to sun_dir.y ≈ 0 → night branch + black terrain.
        let dusk = sun_direction(18.0);
        assert!(
            dusk.y > 0.10,
            "hour 18 sun elevation is {y:.3}; terrain would silhouette",
            y = dusk.y
        );
        assert!(
            day_factor(dusk) > 0.30,
            "hour 18 day factor is {}; dusk would fall through to night",
            day_factor(dusk)
        );
        assert!(
            sunset_factor(dusk) > 0.45,
            "hour 18 is not in the golden-hour band (sunset factor {})",
            sunset_factor(dusk)
        );
    }

    #[test]
    fn late_afternoon_is_warm_sunlit_dusk_not_night() {
        let hour = sun_direction(17.0);
        assert!(hour.y > 0.20, "hour 17 sun is too low ({:.3})", hour.y);
        assert!(day_factor(hour) > 0.45);
        assert!(
            sunset_factor(hour) > 0.55,
            "hour 17 must sit in the golden-hour peak (sunset factor {})",
            sunset_factor(hour)
        );
    }

    #[test]
    fn midday_sun_is_high_and_night_sun_is_down() {
        let noon = sun_direction(11.0);
        assert!(noon.y > 0.70, "hour 11 should be near zenith, got {:.3}", noon.y);
        assert!(day_factor(noon) > 0.90);
        assert!(
            sunset_factor(noon) < 0.08,
            "hour 11 must not pick up the golden rim (sunset factor {})",
            sunset_factor(noon)
        );

        let night = sun_direction(21.5);
        assert!(night.y < -0.20, "hour 21.5 should be true night, y={:.3}", night.y);
        assert!(day_factor(night) < 0.12);
        assert!(sunset_factor(night) < 0.20);
    }

    #[test]
    fn night_and_dusk_keep_a_walkable_fill_without_flattening_to_noon() {
        // Moonlight key must hit +Y faces (y well above 0) while dusk
        // still has a much stronger directional key than night.
        let night = sun_direction(21.5);
        let dusk = sun_direction(17.0);
        let noon = sun_direction(11.0);
        assert!(day_factor(dusk) > 0.45 && day_factor(dusk) < 0.90);
        assert!(day_factor(noon) > day_factor(dusk) + 0.15);
        assert!(night.y < 0.0);
        assert!(sunset_factor(dusk) > sunset_factor(noon));
    }

    #[test]
    fn night_bounce_skims_vertical_faces_instead_of_only_tops() {
        let key = sun_direction(21.5);
        let mut key_dir = key;
        key_dir.y = 0.36;
        let key_dir = key_dir.normalize();
        let night = night_bounce_dir(key_dir, true);
        let day = night_bounce_dir(key_dir, false);
        assert!(
            night.y < 0.50,
            "night bounce still comes from overhead (y={:.3})",
            night.y
        );
        assert!(
            day.y > 0.65,
            "day bounce should stay a high fill, y={:.3}",
            day.y
        );
        let cliff = Vec3::new(0.0, 0.0, -1.0);
        assert!(
            night.dot(cliff).abs() > day.dot(cliff).abs(),
            "night bounce should hit Z-facing mesa walls harder than the day fill"
        );
    }

    #[test]
    fn postcard_bounce_stays_off_fast_and_local() {
        assert_eq!(POSTCARD_BOUNCE.len(), 10);
        for &(wx, wz, _y, _r, _g, _b, lumens, range) in &POSTCARD_BOUNCE {
            assert!(
                crate::frontier::in_hero_postcard(wx, wz),
                "bounce light at {wx},{wz} left the postcard AABB"
            );
            assert!(range <= 40.0, "bounce range {range} would light the whole mesa");
            assert!(lumens <= 450_000.0, "bounce lumens {lumens} would flatten night");
        }
    }
}
