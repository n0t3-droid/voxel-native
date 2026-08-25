// Render-only, two-mode Far Hydro optics.
//
// The CPU copies the first two records from Near water's live uniform. UVs
// carry the same absolute 4096 m wrapped X/Z phase, plus an out-of-range U
// marker for lava. No fluid geometry, occupancy, simulation, or save state is
// inferred in this shader.

#import bevy_pbr::{
    forward_io::{FragmentOutput, VertexOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

struct FarFieldFluidOptics {
    // xy integer 4096 m lattice vector q; z amplitude [m]; w wavelength [m].
    wave_0: vec4<f32>,
    wave_1: vec4<f32>,
    // Exact live copy of Near's four CPU-integrated modulo phases. Far uses
    // x/y with the two copied longest modes and ignores z/w deterministically.
    temporal_phase: vec4<f32>,
    // x weather energy [0,1], y/z calm/storm perceptual roughness,
    // w maximum bounded foam tint share.
    optics: vec4<f32>,
    shallow_color_linear: vec4<f32>,
    deep_color_linear: vec4<f32>,
};

@group(2) @binding(100)
var<uniform> water: FarFieldFluidOptics;

const TAU: f32 = 6.283185307179586;
const WATER_PHASE_PERIOD_METRES: f32 = 4096.0;
const WATER_UV_TO_METRES: f32 = 8.0;
const LAVA_UV_MARKER: f32 = 8192.0;
const LAVA_UV_THRESHOLD: f32 = 4096.0;
const WATER_BEVY_REFLECTANCE_PARAMETER: f32 = 0.357;

fn spectral_slope(
    position_metres: vec2<f32>,
    temporal_phase: f32,
    wave: vec4<f32>,
    phase_offset: f32,
) -> vec2<f32> {
    let wave_vector = wave.xy * (TAU / WATER_PHASE_PERIOD_METRES);
    let phase = dot(wave_vector, position_metres) + temporal_phase + phase_offset;
    return wave_vector * (wave.z * cos(phase));
}

fn spectral_height(
    position_metres: vec2<f32>,
    temporal_phase: f32,
    wave: vec4<f32>,
    phase_offset: f32,
) -> f32 {
    let wave_vector = wave.xy * (TAU / WATER_PHASE_PERIOD_METRES);
    let phase = dot(wave_vector, position_metres) + temporal_phase + phase_offset;
    return wave.z * sin(phase);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let is_lava = in.uv.x >= LAVA_UV_THRESHOLD;
    let phase_u = select(in.uv.x, in.uv.x - LAVA_UV_MARKER, is_lava);

    if !is_lava {
        // Far uses the exact two longest Near modes, the same phase offsets,
        // the same CPU-integrated phases, and the same (X,Z) restoration.
        let position_metres = vec2<f32>(in.uv.y, phase_u) * WATER_UV_TO_METRES;
        let slope = spectral_slope(position_metres, water.temporal_phase.x, water.wave_0, 0.31)
            + spectral_slope(position_metres, water.temporal_phase.y, water.wave_1, 1.73);
        let wave_normal = normalize(vec3<f32>(-slope.x, 1.0, -slope.y));
        pbr_input.N = wave_normal;
        pbr_input.world_normal = wave_normal;

        let slope_energy = clamp(length(slope) / 0.46, 0.0, 1.0);
        let weather_energy = clamp(water.optics.x, 0.0, 1.0);
        pbr_input.material.perceptual_roughness = clamp(
            mix(water.optics.y, water.optics.z, weather_energy)
                + slope_energy * 0.035,
            0.089,
            1.0,
        );
        pbr_input.material.reflectance = WATER_BEVY_REFLECTANCE_PARAMETER;
        pbr_input.material.metallic = 0.0;

        let height = spectral_height(position_metres, water.temporal_phase.x, water.wave_0, 0.31)
            + spectral_height(position_metres, water.temporal_phase.y, water.wave_1, 1.73);
        let view_path = 1.0 - clamp(abs(dot(pbr_input.N, pbr_input.V)), 0.0, 1.0);
        let absorption_mix = clamp(0.18 + view_path * 0.62 - height * 0.24, 0.0, 1.0);
        var water_color = mix(
            water.shallow_color_linear.rgb,
            water.deep_color_linear.rgb,
            absorption_mix,
        );
        let foam = smoothstep(0.66, 0.98, slope_energy)
            * weather_energy
            * clamp(water.optics.w, 0.0, 0.10);
        water_color = mix(water_color, vec3<f32>(0.78, 0.86, 0.88), foam);
        pbr_input.material.base_color = vec4<f32>(water_color, 1.0);
    } else {
        // Lava keeps its categorical vertex albedo and the scalar material's
        // stable response. The marker is presentation metadata, never colour.
        pbr_input.material.base_color.a = 1.0;
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
