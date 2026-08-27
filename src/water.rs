//! Bounded, render-only water optics.
//!
//! The authoritative voxel water remains flat, editable and collision-free.
//! This module changes only the shaded surface normal and optical response. A
//! four-band directional spectrum uses the deep-water dispersion relation
//! `omega = sqrt(g * k)`. CPU-side modulo-`2*pi` phase integration avoids the
//! renderer global's hourly time wrap while keeping weather and animation
//! updates finite, deterministic and constant-work.

use bevy::asset::load_internal_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, Shader, ShaderRef, ShaderType};

use crate::blocks::{BlockType, MaterialId};
use crate::settings::{WeatherSettings, WorldSettings};

pub const WATER_OPTICS_SHADER_HANDLE: Handle<Shader> =
    Handle::weak_from_u128(0x7a9f_2026_0825_51d7_ba11_0042_a9aa_0001);

/// Standard gravity, exact conventional value [m/s^2], CGPM 1901.
pub const STANDARD_GRAVITY_METRES_PER_SECOND_SQUARED: f32 = 9.806_65;
/// Representative visible-light refractive index of liquid water
/// [dimensionless]. The value varies slightly with wavelength and temperature.
pub const WATER_REFRACTIVE_INDEX: f32 = 1.333;
/// Bevy maps its reflectance parameter to dielectric F0 as `0.16 * r^2`.
/// This value therefore represents the air/water interface's ~2.04% F0.
pub const WATER_BEVY_REFLECTANCE_PARAMETER: f32 = 0.357;

const MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS: f32 = 12.0;
const STORM_REFERENCE_WIND_MAGNITUDE_SQUARED: f32 = 52.0;
const MAX_RESPONSE_DELTA_SECONDS: f32 = 0.100;
const RESPONSE_RATE_PER_SECOND: f32 = 3.2;
const DIRECTION_EPSILON_SQUARED: f32 = 1.0e-8;
const RESPONSE_SNAP_EPSILON: f32 = 1.0e-5;
const DEFAULT_WATER_WIND_DIRECTION: Vec2 = Vec2::new(0.843_661_5, 0.536_875_5);
const WATER_LATTICE_VECTORS: [IVec2; 4] = [
    IVec2::new(240, 128),
    IVec2::new(384, -288),
    IVec2::new(-576, 768),
    IVec2::new(560, -1_920),
];

/// The analytic normal can never exceed this conservative slope magnitude.
/// It corresponds to a tilt below 25 degrees and prevents storm highlights
/// from turning a calm inland river into a jagged ocean.
pub const MAX_WATER_NORMAL_SLOPE: f32 = 0.46;
/// Exact world-space phase period shared by the mesher and shader [m].
pub const WATER_PHASE_PERIOD_METRES: f32 = 4_096.0;

pub type WaterSurfaceMaterial = ExtendedMaterial<StandardMaterial, WaterOptics>;

pub struct WaterOpticsPlugin;

impl Plugin for WaterOpticsPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            WATER_OPTICS_SHADER_HANDLE,
            "../assets/shaders/water_optics.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(MaterialPlugin::<WaterSurfaceMaterial>::default())
            .init_resource::<WaterSurfaceLibrary>()
            .init_resource::<WaterWeatherResponse>()
            .add_systems(PostUpdate, update_weather_coherent_water_optics);
    }
}

/// One stable process-wide handle; all near-field water buckets share it.
/// This adds one material asset, but no geometry, simulation entity or draw
/// call beyond the water buckets that already existed.
#[derive(Resource)]
pub struct WaterSurfaceLibrary {
    handle: Handle<WaterSurfaceMaterial>,
}

impl FromWorld for WaterSurfaceLibrary {
    fn from_world(world: &mut World) -> Self {
        let material = WaterSurfaceMaterial {
            base: StandardMaterial {
                base_color: Color::WHITE,
                perceptual_roughness: 0.16,
                reflectance: WATER_BEVY_REFLECTANCE_PARAMETER,
                metallic: 0.0,
                ior: WATER_REFRACTIVE_INDEX,
                // Opaque depth remains deterministic under the project's
                // global Msaa::Off policy. The shader models surface optics,
                // not unsupported order-dependent scene refraction.
                alpha_mode: AlphaMode::Opaque,
                cull_mode: None,
                double_sided: true,
                ..default()
            },
            extension: WaterOptics {
                parameters: water_parameters(WaterWeatherResponse::default()),
            },
        };
        let handle = world
            .resource_mut::<Assets<WaterSurfaceMaterial>>()
            .add(material);
        Self { handle }
    }
}

impl WaterSurfaceLibrary {
    pub fn handle_for(&self, material: MaterialId) -> Option<Handle<WaterSurfaceMaterial>> {
        (material == BlockType::Water as MaterialId).then(|| self.handle.clone())
    }

    /// Read-only bridge for bounded far-water presentation. Far rendering may
    /// copy this uniform, but cannot mutate or replace Near's stable handle.
    pub fn current_parameters(
        &self,
        materials: &Assets<WaterSurfaceMaterial>,
    ) -> Option<WaterOpticsUniform> {
        materials
            .get(&self.handle)
            .map(|material| material.extension.parameters)
    }
}

/// Four `(integer lattice vector q.x/q.z, amplitude metres, wavelength metres)`
/// bands plus optical controls and linear-light colors. The shader constructs
/// `kappa=(2*pi/P)q`; integer `q` makes the phase exactly periodic over the
/// mesher's `P=4096 m` world-coordinate reduction on both horizontal axes.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct WaterOpticsUniform {
    pub wave_0: Vec4,
    pub wave_1: Vec4,
    pub wave_2: Vec4,
    pub wave_3: Vec4,
    /// Per-mode temporal phase in `[0, 2*pi)` [rad]. CPU integration makes
    /// this independent of Bevy's wrapped renderer-global time.
    pub temporal_phase: Vec4,
    /// x = normalized weather energy; y/z = calm/storm roughness;
    /// w = maximum bounded foam tint share.
    pub optics: Vec4,
    pub shallow_color_linear: Vec4,
    pub deep_color_linear: Vec4,
}

const _: () = assert!(std::mem::size_of::<WaterOpticsUniform>() == 128);

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct WaterOptics {
    #[uniform(100)]
    pub parameters: WaterOpticsUniform,
}

impl MaterialExtension for WaterOptics {
    fn fragment_shader() -> ShaderRef {
        WATER_OPTICS_SHADER_HANDLE.clone().into()
    }
}

#[derive(Resource, Clone, Copy, Debug)]
struct WaterWeatherResponse {
    direction: Vec2,
    strength: f32,
    phase_radians: [f64; 4],
}

impl Default for WaterWeatherResponse {
    fn default() -> Self {
        Self {
            direction: DEFAULT_WATER_WIND_DIRECTION,
            strength: 0.0,
            phase_radians: [0.0; 4],
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WaterWeatherTarget {
    direction: Vec2,
    strength: f32,
}

impl WaterWeatherResponse {
    fn advance(&mut self, target: WaterWeatherTarget, raw_delta_seconds: f32) -> bool {
        let alpha = response_alpha(raw_delta_seconds);
        if alpha == 0.0 {
            return false;
        }
        let previous = *self;
        let strength_delta = target.strength - self.strength;
        self.strength = if strength_delta.abs() <= RESPONSE_SNAP_EPSILON {
            target.strength
        } else {
            (self.strength + strength_delta * alpha).clamp(0.0, 1.0)
        };
        if target.strength > RESPONSE_SNAP_EPSILON {
            self.direction = smooth_unit_direction(self.direction, target.direction, alpha);
        }
        self.strength != previous.strength || self.direction != previous.direction
    }

    fn advance_temporal_phase(&mut self, raw_delta_seconds: f32) -> bool {
        if !raw_delta_seconds.is_finite() || raw_delta_seconds <= 0.0 {
            return false;
        }
        // Animation time is not a smoothing response: consuming the complete
        // finite frame delta preserves omega*t semantics through hitches. The
        // f64 accumulator and modulo keep even very large valid deltas finite.
        let delta_seconds = f64::from(raw_delta_seconds);
        for (phase, lattice) in self.phase_radians.iter_mut().zip(WATER_LATTICE_VECTORS) {
            let omega = water_angular_frequency_radians_per_second(lattice);
            *phase = (*phase - omega * delta_seconds).rem_euclid(std::f64::consts::TAU);
        }
        true
    }

    fn temporal_phase_vec4(self) -> Vec4 {
        Vec4::from_array(self.phase_radians.map(|phase| phase as f32))
    }
}

fn water_angular_frequency_radians_per_second(lattice: IVec2) -> f64 {
    let q = lattice.as_dvec2().length();
    let wave_number = std::f64::consts::TAU * q / f64::from(WATER_PHASE_PERIOD_METRES);
    (f64::from(STANDARD_GRAVITY_METRES_PER_SECOND_SQUARED) * wave_number).sqrt()
}

fn response_alpha(raw_delta_seconds: f32) -> f32 {
    if !raw_delta_seconds.is_finite() || raw_delta_seconds <= 0.0 {
        return 0.0;
    }
    let delta_seconds = raw_delta_seconds.min(MAX_RESPONSE_DELTA_SECONDS);
    1.0 - (-RESPONSE_RATE_PER_SECOND * delta_seconds).exp()
}

fn normalized_or_default(value: Vec2) -> Vec2 {
    if !value.is_finite() || value.length_squared() <= DIRECTION_EPSILON_SQUARED {
        DEFAULT_WATER_WIND_DIRECTION
    } else {
        value.normalize()
    }
}

fn smooth_unit_direction(current: Vec2, target: Vec2, alpha: f32) -> Vec2 {
    let current = normalized_or_default(current);
    let target = normalized_or_default(target);
    let signed_angle = current
        .perp_dot(target)
        .atan2(current.dot(target).clamp(-1.0, 1.0));
    let (sin_angle, cos_angle) = (signed_angle * alpha.clamp(0.0, 1.0)).sin_cos();
    normalized_or_default(Vec2::new(
        current.x * cos_angle - current.y * sin_angle,
        current.x * sin_angle + current.y * cos_angle,
    ))
}

fn water_weather_target(weather: &WeatherSettings) -> WaterWeatherTarget {
    let raw = Vec2::new(weather.wind_x, weather.wind_z);
    if !raw.is_finite() {
        return WaterWeatherTarget {
            direction: DEFAULT_WATER_WIND_DIRECTION,
            strength: 0.0,
        };
    }
    let bounded = raw.clamp(
        Vec2::splat(-MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS),
        Vec2::splat(MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS),
    );
    let magnitude_squared = bounded.length_squared();
    if magnitude_squared <= DIRECTION_EPSILON_SQUARED {
        return WaterWeatherTarget {
            direction: DEFAULT_WATER_WIND_DIRECTION,
            strength: 0.0,
        };
    }
    WaterWeatherTarget {
        direction: normalized_or_default(bounded),
        strength: (magnitude_squared / STORM_REFERENCE_WIND_MAGNITUDE_SQUARED)
            .sqrt()
            .clamp(0.0, 1.0),
    }
}

fn linear_color(red: f32, green: f32, blue: f32) -> Vec4 {
    let linear = Color::srgb(red, green, blue).to_linear();
    Vec4::new(linear.red, linear.green, linear.blue, 1.0)
}

fn water_parameters(response: WaterWeatherResponse) -> WaterOpticsUniform {
    let strength = response.strength.clamp(0.0, 1.0);
    let energy = strength * strength * (3.0 - 2.0 * strength);
    let direction = normalized_or_default(response.direction);
    let amplitude_scale = 0.24 + 0.76 * energy;
    let wave = |lattice: IVec2, maximum_amplitude: f32| {
        // q stays fixed so a changing wind cannot phase-pop the surface.
        // Weather direction instead redistributes a bounded 18% of energy
        // toward aligned modes while keeping the exact lattice continuous.
        let q = lattice;
        let alignment = normalized_or_default(q.as_vec2()).dot(direction).abs();
        let directional_energy = 0.82 + 0.18 * alignment * alignment;
        let wavelength = WATER_PHASE_PERIOD_METRES / q.as_vec2().length();
        let angular_frequency = water_angular_frequency_radians_per_second(q);
        debug_assert!(angular_frequency.is_finite() && angular_frequency > 0.0);
        Vec4::new(
            q.x as f32,
            q.y as f32,
            maximum_amplitude * amplitude_scale * directional_energy,
            wavelength,
        )
    };
    let parameters = WaterOpticsUniform {
        // Pythagorean lattice vectors retain exact integer norms and yield
        // wavelengths of about 15.06, 8.53, 4.27 and 2.05 metres.
        wave_0: wave(WATER_LATTICE_VECTORS[0], 0.30),
        wave_1: wave(WATER_LATTICE_VECTORS[1], 0.11),
        wave_2: wave(WATER_LATTICE_VECTORS[2], 0.040),
        wave_3: wave(WATER_LATTICE_VECTORS[3], 0.014),
        temporal_phase: response.temporal_phase_vec4(),
        optics: Vec4::new(strength, 0.11, 0.27, 0.10),
        // Artistic absorption endpoints in linear sRGB. They are intentionally
        // presentation parameters, not claims about dissolved constituents.
        shallow_color_linear: linear_color(0.035, 0.27, 0.34),
        deep_color_linear: linear_color(0.006, 0.050, 0.095),
    };
    debug_assert!(maximum_spectrum_slope(parameters) <= MAX_WATER_NORMAL_SLOPE);
    parameters
}

fn maximum_spectrum_slope(parameters: WaterOpticsUniform) -> f32 {
    [
        parameters.wave_0,
        parameters.wave_1,
        parameters.wave_2,
        parameters.wave_3,
    ]
    .into_iter()
    .map(|wave| {
        wave.z.abs() * std::f32::consts::TAU * wave.xy().length() / WATER_PHASE_PERIOD_METRES
    })
    .sum()
}

fn update_weather_coherent_water_optics(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    library: Res<WaterSurfaceLibrary>,
    mut materials: ResMut<Assets<WaterSurfaceMaterial>>,
    mut response: ResMut<WaterWeatherResponse>,
) {
    let target = water_weather_target(&settings.weather);
    let weather_changed = response.advance(target, time.delta_seconds());
    let phase_changed = response.advance_temporal_phase(time.delta_seconds());
    if !weather_changed && !phase_changed {
        return;
    }
    if let Some(material) = materials.get_mut(&library.handle) {
        material.extension.parameters = water_parameters(*response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weather(wind_x: f32, wind_z: f32) -> WeatherSettings {
        WeatherSettings {
            wind_x,
            wind_z,
            ..default()
        }
    }

    #[test]
    fn water_reflectance_parameter_matches_air_water_fresnel_f0() {
        let ratio = (WATER_REFRACTIVE_INDEX - 1.0) / (WATER_REFRACTIVE_INDEX + 1.0);
        let physical_f0 = ratio * ratio;
        let bevy_f0 = 0.16 * WATER_BEVY_REFLECTANCE_PARAMETER * WATER_BEVY_REFLECTANCE_PARAMETER;
        assert!((physical_f0 - bevy_f0).abs() < 0.0001);
    }

    #[test]
    fn spectrum_is_finite_normalized_periodic_and_slope_bounded() {
        for strength in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let parameters = water_parameters(WaterWeatherResponse {
                direction: Vec2::new(-0.31, 0.95),
                strength,
                ..default()
            });
            for wave in [
                parameters.wave_0,
                parameters.wave_1,
                parameters.wave_2,
                parameters.wave_3,
            ] {
                assert!(wave.is_finite());
                assert!(wave.x.fract() == 0.0);
                assert!(wave.y.fract() == 0.0);
                assert!(wave.xy().length() > 0.0);
                assert!(wave.z >= 0.0);
                assert!(wave.w > 0.0);
                let derived_wavelength = WATER_PHASE_PERIOD_METRES / wave.xy().length();
                assert!((wave.w - derived_wavelength).abs() < 1.0e-5);
                for axis in [wave.x, wave.y] {
                    let turns = axis;
                    assert_eq!(turns.fract(), 0.0);
                }
            }
            assert!(maximum_spectrum_slope(parameters) <= MAX_WATER_NORMAL_SLOPE);
        }
    }

    #[test]
    fn invalid_and_extreme_weather_fail_closed_to_finite_targets() {
        for input in [
            weather(f32::NAN, 1.0),
            weather(f32::INFINITY, 0.0),
            weather(f32::MAX, -f32::MAX),
            weather(0.0, 0.0),
        ] {
            let target = water_weather_target(&input);
            assert!(target.direction.is_finite());
            assert!((target.direction.length() - 1.0).abs() < 1.0e-5);
            assert!(target.strength.is_finite());
            assert!((0.0..=1.0).contains(&target.strength));
        }
    }

    #[test]
    fn response_is_timestep_bounded_and_keeps_unit_direction() {
        let target = water_weather_target(&weather(6.0, 4.0));
        let mut response = WaterWeatherResponse::default();
        assert!(!response.advance(target, f32::NAN));
        assert!(!response.advance(target, -1.0));
        assert!(response.advance(target, 10_000.0));
        assert!((response.direction.length() - 1.0).abs() < 1.0e-5);
        assert!((0.0..=1.0).contains(&response.strength));
    }

    #[test]
    fn temporal_phase_is_bounded_continuous_and_independent_of_renderer_time_wrap() {
        let mut response = WaterWeatherResponse::default();
        for _ in 0..40_000 {
            assert!(response.advance_temporal_phase(0.1));
        }
        let phase_before = response.temporal_phase_vec4();
        assert!(phase_before
            .to_array()
            .into_iter()
            .all(|phase| (0.0..std::f32::consts::TAU).contains(&phase)));

        let previous = response.phase_radians;
        let short_step = 0.016_f32;
        assert!(response.advance_temporal_phase(short_step));
        for ((before, after), lattice) in previous
            .into_iter()
            .zip(response.phase_radians)
            .zip(WATER_LATTICE_VECTORS)
        {
            let expected_delta =
                water_angular_frequency_radians_per_second(lattice) * f64::from(short_step);
            let observed_delta = (before - after).rem_euclid(std::f64::consts::TAU);
            assert!((observed_delta - expected_delta).abs() < 1.0e-9);
        }

        let mut one_hitch = WaterWeatherResponse::default();
        let mut segmented = WaterWeatherResponse::default();
        assert!(one_hitch.advance_temporal_phase(0.75));
        for _ in 0..3 {
            assert!(segmented.advance_temporal_phase(0.25));
        }
        for (single, split) in one_hitch
            .phase_radians
            .into_iter()
            .zip(segmented.phase_radians)
        {
            assert!((single - split).abs() < 1.0e-12);
        }
    }

    #[test]
    fn shader_contract_contains_dispersion_world_uv_and_pbr_paths() {
        let shader = include_str!("../assets/shaders/water_optics.wgsl");
        for required in [
            "water.temporal_phase.x",
            "side_triangle",
            "side_cue",
            "in.uv.yx * WATER_UV_TO_METRES",
            "pbr_input.N =",
            "apply_pbr_lighting",
            "main_pass_post_lighting_processing",
        ] {
            assert!(
                shader.contains(required),
                "missing shader contract: {required}"
            );
        }
        assert!(!shader.contains("globals.time"));
        assert_eq!(shader.matches("normalize(").count(), 2);
        assert_eq!(shader.matches("spectral_slope(position_metres").count(), 4);
        assert_eq!(shader.matches("spectral_height(position_metres").count(), 4);
        assert_eq!(shader.matches("wave.z * cos(phase)").count(), 1);
        assert_eq!(shader.matches("wave.z * sin(phase)").count(), 1);
        assert_eq!(shader.matches("textureSample").count(), 0);
        assert_eq!(std::mem::size_of::<WaterOpticsUniform>(), 128);
    }
}
