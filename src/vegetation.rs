//! Render-only vegetation motion.
//!
//! This module deliberately owns no forces, velocities, colliders, player, or
//! ship state. Wind here is a vertex displacement field: it can make foliage
//! feel alive without changing flight, projectiles, pathfinding, or voxel
//! authority. The two-frequency response follows the real-time animation
//! pattern of slow spring-like branch motion plus faster, lower-amplitude leaf
//! motion described by Muraoka et al. (2011), DOI 10.3756/artsci.10.140.

use bevy::asset::load_internal_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, Shader, ShaderRef, ShaderType};

use crate::blocks::{BlockType, MaterialId};
use crate::settings::{WeatherSettings, WorldSettings};
use crate::textures::MaterialLibrary;

pub const VEGETATION_WIND_SHADER_HANDLE: Handle<Shader> =
    Handle::weak_from_u128(0x7a9f_2026_0808_51d7_ba11_0042_ded0_7101);

/// Conservative culling expansion in voxel-world units. Every preset below
/// is checked against this bound so wind cannot disappear at chunk edges.
pub const MAX_VEGETATION_DISPLACEMENT_VOXELS: f32 = 0.35;

/// Exact maxima across the four foliage response presets. Weather can scale
/// these amplitudes down, but never above their authored values.
pub const MAX_VEGETATION_MACRO_AMPLITUDE_VOXELS: f32 = 0.19;
pub const MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS: f32 = 0.060;
pub const MAX_VEGETATION_ANIMATED_DISPLACEMENT_VOXELS: f32 =
    MAX_VEGETATION_MACRO_AMPLITUDE_VOXELS + MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS * 1.5;

/// A weather update touches this fixed set of existing material assets. It
/// creates no vegetation entities, geometry, draw calls, or per-frame lists.
pub const VEGETATION_WIND_MATERIAL_COUNT: usize = 4;
const WEATHER_DRIVEN_FOLIAGE: [BlockType; VEGETATION_WIND_MATERIAL_COUNT] = [
    BlockType::Leaves,
    BlockType::JungleLeaves,
    BlockType::BlossomLeaves,
    BlockType::SakuraPetals,
];

/// Runtime settings already clamp each horizontal wind component to this
/// engine-relative value in `WorldSettings::normalize_runtime_safety`.
const MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS: f32 = 12.0;
/// The built-in Storm preset is (6, 4), so 6^2 + 4^2 = 52 world-units squared.
/// It is the point at which authored foliage amplitudes reach 100%.
const STORM_REFERENCE_WIND_MAGNITUDE_SQUARED: f32 = 52.0;
/// A hitch must not turn one update into an instant weather-direction snap.
const MAX_WIND_RESPONSE_DELTA_SECONDS: f32 = 0.100;
/// First-order response rate [1/s]. The exponential form is invariant under
/// subdivision of a constant-target interval, unlike a frame-count lerp.
const WIND_RESPONSE_RATE_PER_SECOND: f32 = 4.0;
/// Clear air retains only 8% of authored leaf flutter: at most 0.0048 voxel.
/// This is visual micro-turbulence, not a physical force or collision motion.
const CALM_MICRO_FLUTTER_SHARE: f32 = 0.08;
const RESPONSE_SNAP_EPSILON: f32 = 1.0e-5;
const DIRECTION_EPSILON_SQUARED: f32 = 1.0e-8;
const DEFAULT_WIND_DIRECTION: Vec2 = Vec2::new(0.843_661_5, 0.536_875_5);

pub type VegetationMaterial = ExtendedMaterial<StandardMaterial, VegetationWind>;

pub struct VegetationPlugin;

impl Plugin for VegetationPlugin {
    fn build(&self, app: &mut App) {
        // Embed the shader in the executable. This keeps direct debug runs,
        // packaged builds, and release binaries identical instead of relying
        // on an external assets folder beside the executable.
        load_internal_asset!(
            app,
            VEGETATION_WIND_SHADER_HANDLE,
            "../assets/shaders/vegetation_wind.wgsl",
            Shader::from_wgsl
        );
        app.add_plugins(MaterialPlugin::<VegetationMaterial>::default())
            .init_resource::<WeatherWindResponse>()
            // PostUpdate keeps a same-frame material-library rebuild ahead of
            // the visual uniform refresh. No gameplay schedule reads this.
            .add_systems(PostUpdate, update_weather_coherent_vegetation_wind);
    }
}

/// Smoothed, render-only response to the active weather. Direction is always
/// normalized and strength stays in [0, 1]. There are deliberately no entity,
/// transform, collider, velocity, player, shuttle, or projectile references.
#[derive(Resource, Clone, Copy, Debug)]
struct WeatherWindResponse {
    direction: Vec2,
    strength: f32,
}

impl Default for WeatherWindResponse {
    fn default() -> Self {
        Self {
            direction: DEFAULT_WIND_DIRECTION,
            strength: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WeatherWindTarget {
    direction: Vec2,
    strength: f32,
}

impl WeatherWindResponse {
    /// Advances a bounded first-order response. Direction follows the shortest
    /// arc instead of linearly blending components, so even a 180-degree
    /// reversal stays normalized and deterministic.
    fn advance(&mut self, target: WeatherWindTarget, raw_delta_seconds: f32) -> bool {
        let alpha = smoothing_alpha(raw_delta_seconds);
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

        // Calm weather has no new heading. Keeping the last valid direction
        // prevents a visible direction reset in the tiny microflutter band.
        if target.strength > RESPONSE_SNAP_EPSILON {
            self.direction = smooth_unit_direction(self.direction, target.direction, alpha);
        }

        self.strength != previous.strength || self.direction != previous.direction
    }
}

fn smoothing_alpha(raw_delta_seconds: f32) -> f32 {
    if !raw_delta_seconds.is_finite() || raw_delta_seconds <= 0.0 {
        return 0.0;
    }
    let delta_seconds = raw_delta_seconds.min(MAX_WIND_RESPONSE_DELTA_SECONDS);
    1.0 - (-WIND_RESPONSE_RATE_PER_SECOND * delta_seconds).exp()
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

fn normalized_or_default(value: Vec2) -> Vec2 {
    if !value.is_finite() || value.length_squared() <= DIRECTION_EPSILON_SQUARED {
        DEFAULT_WIND_DIRECTION
    } else {
        value.normalize()
    }
}

fn weather_wind_target(weather: &WeatherSettings) -> WeatherWindTarget {
    let raw = Vec2::new(weather.wind_x, weather.wind_z);
    if !raw.is_finite() {
        return WeatherWindTarget {
            direction: DEFAULT_WIND_DIRECTION,
            strength: 0.0,
        };
    }

    // Clamp before taking the length so even finite f32 extremes cannot
    // overflow. Invalid inputs fail closed to calm above.
    let bounded = raw.clamp(
        Vec2::splat(-MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS),
        Vec2::splat(MAX_WEATHER_WIND_COMPONENT_WORLD_UNITS),
    );
    let magnitude_squared = bounded.length_squared();
    if magnitude_squared <= DIRECTION_EPSILON_SQUARED {
        return WeatherWindTarget {
            direction: DEFAULT_WIND_DIRECTION,
            strength: 0.0,
        };
    }

    WeatherWindTarget {
        direction: normalized_or_default(bounded),
        strength: (magnitude_squared / STORM_REFERENCE_WIND_MAGNITUDE_SQUARED)
            .sqrt()
            .clamp(0.0, 1.0),
    }
}

fn smoothstep_unit(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn weather_parameters_for_block(
    block: BlockType,
    response: WeatherWindResponse,
) -> Option<VegetationWindUniform> {
    let mut parameters = VegetationWind::for_block(block)?.parameters;
    let strength = response.strength.clamp(0.0, 1.0);
    let amplitude_response = smoothstep_unit(strength);
    let direction = normalized_or_default(response.direction);

    parameters.direction_macro.x = direction.x;
    parameters.direction_macro.y = direction.y;
    parameters.direction_macro.z *= amplitude_response;
    parameters.direction_macro.w *= 0.72 + 0.28 * strength;
    parameters.flutter_phase.x *=
        CALM_MICRO_FLUTTER_SHARE + (1.0 - CALM_MICRO_FLUTTER_SHARE) * amplitude_response;
    parameters.flutter_phase.y *= 0.78 + 0.22 * strength;
    Some(parameters)
}

fn update_weather_coherent_vegetation_wind(
    time: Res<Time>,
    settings: Res<WorldSettings>,
    material_library: Res<MaterialLibrary>,
    mut materials: ResMut<Assets<VegetationMaterial>>,
    mut response: ResMut<WeatherWindResponse>,
) {
    let target = weather_wind_target(&settings.weather);
    let response_changed = response.advance(target, time.delta_seconds());
    if !response_changed && !material_library.is_changed() {
        return;
    }

    // Constant work: at most four BTreeMap lookups and four existing uniform
    // writes. No collection is built and no asset/entity is created here.
    for block in WEATHER_DRIVEN_FOLIAGE {
        let material_id = block as MaterialId;
        let Some(handle) = material_library.vegetation_handles.get(&material_id) else {
            continue;
        };
        let Some(material) = materials.get_mut(handle) else {
            continue;
        };
        let Some(parameters) = weather_parameters_for_block(block, *response) else {
            continue;
        };
        material.extension.parameters = parameters;
        debug_assert!(
            material.extension.conservative_displacement_voxels()
                <= MAX_VEGETATION_DISPLACEMENT_VOXELS
        );
    }
}

/// Shader parameters with units encoded in their field names.
///
/// One voxel is the engine's world-space length unit. Frequencies are angular
/// frequencies in radians per second, and the spatial phase is radians per
/// voxel. Values are visual-response presets, not a force model: the voxel art
/// scale does not assert a one-voxel-to-one-metre conversion.
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct VegetationWindUniform {
    /// x/y = normalized horizontal X/Z direction; z = macro amplitude [voxel];
    /// w = macro angular frequency [rad/s].
    pub direction_macro: Vec4,
    /// x = flutter amplitude [voxel]; y = flutter angular frequency [rad/s];
    /// z = spatial phase gradient [rad/voxel]; w = cross-wind share [0, 1].
    pub flutter_phase: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct VegetationWind {
    // StandardMaterial occupies the low binding slots. Extension bindings
    // intentionally start at 100, matching Bevy's MaterialExtension contract.
    #[uniform(100)]
    pub parameters: VegetationWindUniform,
}

impl MaterialExtension for VegetationWind {
    fn vertex_shader() -> ShaderRef {
        VEGETATION_WIND_SHADER_HANDLE.clone().into()
    }

    fn prepass_vertex_shader() -> ShaderRef {
        VEGETATION_WIND_SHADER_HANDLE.clone().into()
    }

    fn deferred_vertex_shader() -> ShaderRef {
        VEGETATION_WIND_SHADER_HANDLE.clone().into()
    }
}

impl VegetationWind {
    pub fn for_block(block: BlockType) -> Option<Self> {
        let (
            macro_amplitude_voxels,
            macro_omega_rad_per_s,
            flutter_amplitude_voxels,
            flutter_omega_rad_per_s,
        ) = match block {
            // Broad leaves: moderate crown motion with restrained flutter.
            BlockType::Leaves => (0.13, 0.85, 0.035, 5.7),
            // Dense jungle crowns have more apparent mass and respond more slowly.
            BlockType::JungleLeaves => (0.10, 0.70, 0.028, 5.1),
            // Blossom clusters read best with a slightly lighter response.
            BlockType::BlossomLeaves => (0.16, 0.95, 0.050, 6.4),
            // Petal masses are the lightest existing vegetation material.
            BlockType::SakuraPetals => (0.19, 1.05, 0.060, 7.2),
            _ => return None,
        };

        // A normalized, non-axis-aligned direction avoids synchronized motion
        // that makes a voxel forest look like a sliding grid.
        let direction = DEFAULT_WIND_DIRECTION;
        let wind = Self {
            parameters: VegetationWindUniform {
                direction_macro: Vec4::new(
                    direction.x,
                    direction.y,
                    macro_amplitude_voxels,
                    macro_omega_rad_per_s,
                ),
                flutter_phase: Vec4::new(
                    flutter_amplitude_voxels,
                    flutter_omega_rad_per_s,
                    0.17, // radians of phase shift per voxel
                    0.62, // dimensionless cross-wind share
                ),
            },
        };
        debug_assert!(
            wind.conservative_displacement_voxels() <= MAX_VEGETATION_DISPLACEMENT_VOXELS
        );
        debug_assert!(
            wind.conservative_displacement_voxels() <= MAX_VEGETATION_ANIMATED_DISPLACEMENT_VOXELS
        );
        debug_assert!(
            wind.parameters.direction_macro.z.abs() <= MAX_VEGETATION_MACRO_AMPLITUDE_VOXELS
        );
        debug_assert!(
            wind.parameters.flutter_phase.x.abs() <= MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS
        );
        Some(wind)
    }

    fn conservative_displacement_voxels(&self) -> f32 {
        let macro_amplitude = self.parameters.direction_macro.z.abs();
        let flutter_amplitude = self.parameters.flutter_phase.x.abs();
        // The shader sums one full and one half-amplitude flutter wave.
        macro_amplitude + flutter_amplitude * 1.5
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
    fn only_visual_foliage_materials_receive_wind() {
        for block in [
            BlockType::Leaves,
            BlockType::JungleLeaves,
            BlockType::BlossomLeaves,
            BlockType::SakuraPetals,
        ] {
            assert!(VegetationWind::for_block(block).is_some(), "{block:?}");
        }
        for block in [
            BlockType::Wood,
            BlockType::Grass,
            BlockType::Bamboo,
            BlockType::Stone,
            BlockType::ShipHullAlloy,
            BlockType::CockpitGlass,
        ] {
            assert!(VegetationWind::for_block(block).is_none(), "{block:?}");
        }
    }

    #[test]
    fn every_wind_preset_is_normalized_positive_and_inside_culling_budget() {
        for block in WEATHER_DRIVEN_FOLIAGE {
            let wind = VegetationWind::for_block(block).expect("foliage preset");
            let direction = wind.parameters.direction_macro.xy();
            assert!((direction.length() - 1.0).abs() < 1e-5);
            assert!(wind.parameters.direction_macro.w > 0.0);
            assert!(wind.parameters.flutter_phase.y > wind.parameters.direction_macro.w);
            assert!(wind.conservative_displacement_voxels() <= MAX_VEGETATION_DISPLACEMENT_VOXELS);
        }
    }

    #[test]
    fn species_keep_distinct_macro_sway_and_micro_flutter_signatures() {
        let mut signatures = std::collections::BTreeSet::new();
        for block in WEATHER_DRIVEN_FOLIAGE {
            let parameters = weather_parameters_for_block(
                block,
                WeatherWindResponse {
                    direction: Vec2::X,
                    strength: 1.0,
                },
            )
            .unwrap();
            signatures.insert((
                parameters.direction_macro.z.to_bits(),
                parameters.direction_macro.w.to_bits(),
                parameters.flutter_phase.x.to_bits(),
                parameters.flutter_phase.y.to_bits(),
            ));
        }
        assert_eq!(signatures.len(), VEGETATION_WIND_MATERIAL_COUNT);
    }

    #[test]
    fn invalid_and_extreme_weather_are_sanitized_fail_closed_or_capped() {
        for invalid in [
            weather(f32::NAN, 1.0),
            weather(1.0, f32::NAN),
            weather(f32::INFINITY, 1.0),
            weather(1.0, f32::NEG_INFINITY),
        ] {
            let target = weather_wind_target(&invalid);
            assert_eq!(target.strength, 0.0);
            assert_eq!(target.direction, DEFAULT_WIND_DIRECTION);
        }

        let positive = weather_wind_target(&weather(f32::MAX, f32::MAX));
        let negative = weather_wind_target(&weather(-f32::MAX, -f32::MAX));
        assert_eq!(positive.strength, 1.0);
        assert_eq!(negative.strength, 1.0);
        assert!((positive.direction - Vec2::splat(1.0).normalize()).length() < 1.0e-6);
        assert!((negative.direction + Vec2::splat(1.0).normalize()).length() < 1.0e-6);
    }

    #[test]
    fn calm_light_and_storm_weather_scale_without_exceeding_authored_motion() {
        let calm = weather_wind_target(&weather(0.0, 0.0));
        let light = weather_wind_target(&weather(2.0, 1.0));
        let storm = weather_wind_target(&weather(6.0, 4.0));
        assert_eq!(calm.strength, 0.0);
        assert!(light.strength > calm.strength);
        assert!(light.strength < storm.strength);
        assert!((storm.strength - 1.0).abs() < 1.0e-6);

        let calm_parameters = weather_parameters_for_block(
            BlockType::SakuraPetals,
            WeatherWindResponse {
                direction: DEFAULT_WIND_DIRECTION,
                strength: calm.strength,
            },
        )
        .unwrap();
        let storm_parameters = weather_parameters_for_block(
            BlockType::SakuraPetals,
            WeatherWindResponse {
                direction: storm.direction,
                strength: storm.strength,
            },
        )
        .unwrap();
        assert_eq!(calm_parameters.direction_macro.z, 0.0);
        assert!(
            (calm_parameters.flutter_phase.x
                - MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS * CALM_MICRO_FLUTTER_SHARE)
                .abs()
                < 1.0e-6
        );
        assert!(
            (storm_parameters.direction_macro.z - MAX_VEGETATION_MACRO_AMPLITUDE_VOXELS).abs()
                < 1.0e-6
        );
        assert!(
            (storm_parameters.flutter_phase.x - MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS).abs()
                < 1.0e-6
        );
    }

    #[test]
    fn target_and_smoothed_directions_are_normalized() {
        for (x, z) in [(1.0, 0.0), (0.0, -4.0), (3.0, 7.0), (-12.0, 12.0)] {
            let target = weather_wind_target(&weather(x, z));
            assert!((target.direction.length() - 1.0).abs() < 1.0e-6);
        }

        let mut response = WeatherWindResponse::default();
        let target = weather_wind_target(&weather(-6.0, -4.0));
        let mut previous_remaining_angle = response
            .direction
            .perp_dot(target.direction)
            .atan2(response.direction.dot(target.direction))
            .abs();
        for _ in 0..20 {
            response.advance(target, 1.0 / 60.0);
            assert!((response.direction.length() - 1.0).abs() < 1.0e-6);
            let remaining_angle = response
                .direction
                .perp_dot(target.direction)
                .atan2(response.direction.dot(target.direction))
                .abs();
            assert!(remaining_angle < previous_remaining_angle);
            assert!(remaining_angle > 0.0);
            previous_remaining_angle = remaining_angle;
        }
        assert_ne!(response.direction, target.direction);
    }

    #[test]
    fn smoothing_is_monotonic_and_large_delta_is_clamped() {
        let target = weather_wind_target(&weather(6.0, 4.0));
        let mut response = WeatherWindResponse::default();
        let mut previous_strength = response.strength;
        for _ in 0..120 {
            assert!(response.advance(target, 1.0 / 60.0));
            assert!(response.strength > previous_strength);
            assert!(response.strength < target.strength);
            previous_strength = response.strength;
        }

        let mut clamped = WeatherWindResponse::default();
        let mut exact_max = WeatherWindResponse::default();
        clamped.advance(target, 10_000.0);
        exact_max.advance(target, MAX_WIND_RESPONSE_DELTA_SECONDS);
        assert!((clamped.strength - exact_max.strength).abs() < f32::EPSILON);
        assert!((clamped.direction - exact_max.direction).length() < f32::EPSILON);

        let unchanged = response;
        assert!(!response.advance(target, f32::NAN));
        assert_eq!(response.strength, unchanged.strength);
        assert_eq!(response.direction, unchanged.direction);
    }

    #[test]
    fn exact_amplitude_and_displacement_caps_hold_for_every_weather_strength() {
        let mut maximum_observed = 0.0_f32;
        for step in 0..=100 {
            let response = WeatherWindResponse {
                direction: Vec2::X,
                strength: step as f32 / 100.0,
            };
            for block in WEATHER_DRIVEN_FOLIAGE {
                let parameters = weather_parameters_for_block(block, response).unwrap();
                let wind = VegetationWind { parameters };
                assert!(
                    parameters.direction_macro.z.abs() <= MAX_VEGETATION_MACRO_AMPLITUDE_VOXELS
                );
                assert!(
                    parameters.flutter_phase.x.abs() <= MAX_VEGETATION_FLUTTER_AMPLITUDE_VOXELS
                );
                maximum_observed = maximum_observed.max(wind.conservative_displacement_voxels());
                assert!(
                    wind.conservative_displacement_voxels() <= MAX_VEGETATION_DISPLACEMENT_VOXELS
                );
            }
        }
        assert!((maximum_observed - MAX_VEGETATION_ANIMATED_DISPLACEMENT_VOXELS).abs() < 1.0e-6);
        assert!(MAX_VEGETATION_ANIMATED_DISPLACEMENT_VOXELS < MAX_VEGETATION_DISPLACEMENT_VOXELS);
    }

    #[test]
    fn weather_update_work_set_is_exactly_four_existing_foliage_materials() {
        assert_eq!(WEATHER_DRIVEN_FOLIAGE.len(), VEGETATION_WIND_MATERIAL_COUNT);
        let mut unique_ids = std::collections::BTreeSet::new();
        for block in WEATHER_DRIVEN_FOLIAGE {
            assert!(VegetationWind::for_block(block).is_some());
            unique_ids.insert(block as MaterialId);
        }
        assert_eq!(unique_ids.len(), VEGETATION_WIND_MATERIAL_COUNT);
    }

    #[test]
    fn weather_update_mutates_only_uniforms_and_preserves_counts() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.insert_resource(WorldSettings::default());
        app.init_resource::<WeatherWindResponse>();

        let mut library = MaterialLibrary::default();
        let mut materials = Assets::<VegetationMaterial>::default();
        for block in WEATHER_DRIVEN_FOLIAGE {
            let handle = materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    base_color: Color::srgb(0.12, 0.34, 0.56),
                    perceptual_roughness: 0.73,
                    reflectance: 0.11,
                    double_sided: true,
                    ..default()
                },
                extension: VegetationWind::for_block(block).unwrap(),
            });
            library
                .vegetation_handles
                .insert(block as MaterialId, handle);
        }
        app.insert_resource(library);
        app.insert_resource(materials);
        app.add_systems(Update, update_weather_coherent_vegetation_wind);

        let entity_count_before = app.world().entities().len();
        let asset_count_before = app.world().resource::<Assets<VegetationMaterial>>().len();
        app.update();
        assert_eq!(app.world().entities().len(), entity_count_before);
        assert_eq!(
            app.world().resource::<Assets<VegetationMaterial>>().len(),
            asset_count_before
        );

        let library = app.world().resource::<MaterialLibrary>();
        let materials = app.world().resource::<Assets<VegetationMaterial>>();
        for block in WEATHER_DRIVEN_FOLIAGE {
            let handle = library
                .vegetation_handles
                .get(&(block as MaterialId))
                .unwrap();
            let material = materials.get(handle).unwrap();
            assert_eq!(material.base.base_color, Color::srgb(0.12, 0.34, 0.56));
            assert_eq!(material.base.perceptual_roughness, 0.73);
            assert_eq!(material.base.reflectance, 0.11);
            assert!(material.base.double_sided);
        }
    }

    #[test]
    fn weather_update_after_rebuild_reaches_the_pre_remesh_handle() {
        let mut library = MaterialLibrary::default();
        let mut standard_materials = Assets::<StandardMaterial>::default();
        let mut vegetation_materials = Assets::<VegetationMaterial>::default();
        let mut images = Assets::<Image>::default();
        library.rebuild_without_custom_for_test(
            &mut standard_materials,
            &mut vegetation_materials,
            &mut images,
            4,
        );
        let leaves_id = BlockType::Leaves as MaterialId;
        let pre_remesh_handle = library.vegetation_handles.get(&leaves_id).unwrap().clone();

        library.rebuild_without_custom_for_test(
            &mut standard_materials,
            &mut vegetation_materials,
            &mut images,
            4,
        );
        assert_eq!(
            library.vegetation_handles.get(&leaves_id).unwrap().id(),
            pre_remesh_handle.id(),
            "bounded remesh must not split vegetation asset identity"
        );

        let mut settings = WorldSettings::default();
        settings.weather = weather(6.0, 4.0);
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_millis(100));
        let material_count = vegetation_materials.len();

        let mut app = App::new();
        app.insert_resource(time);
        app.insert_resource(settings);
        app.insert_resource(WeatherWindResponse::default());
        app.insert_resource(library);
        app.insert_resource(vegetation_materials);
        app.add_systems(Update, update_weather_coherent_vegetation_wind);
        app.update();

        let library = app.world().resource::<MaterialLibrary>();
        assert_eq!(
            library.vegetation_handles.get(&leaves_id).unwrap().id(),
            pre_remesh_handle.id()
        );
        let materials = app.world().resource::<Assets<VegetationMaterial>>();
        assert_eq!(materials.len(), material_count);
        let updated = materials
            .get(&pre_remesh_handle)
            .expect("entity's pre-remesh handle must see the weather update");
        let authored = VegetationWind::for_block(BlockType::Leaves)
            .unwrap()
            .parameters
            .direction_macro
            .z;
        assert!(updated.extension.parameters.direction_macro.z > 0.0);
        assert!(updated.extension.parameters.direction_macro.z < authored);
        assert_ne!(
            updated.extension.parameters.direction_macro.xy(),
            DEFAULT_WIND_DIRECTION
        );
    }
}
