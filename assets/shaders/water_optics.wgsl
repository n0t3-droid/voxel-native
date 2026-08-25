// Render-only, world-anchored near-water optics.
//
// Four fixed directional bands receive modulo-2*pi temporal phase integrated
// on the CPU from the deep-water dispersion relation omega = sqrt(g*k). The
// shader perturbs only the PBR normal and bounded absorption color: voxel
// topology, collision, fluid authority and saves remain unchanged.

#import bevy_pbr::{
    forward_io::{FragmentOutput, VertexOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}

struct WaterOptics {
    // xy integer lattice vector q, z amplitude [m], w derived wavelength [m].
    wave_0: vec4<f32>,
    wave_1: vec4<f32>,
    wave_2: vec4<f32>,
    wave_3: vec4<f32>,
    // Per-mode phase in [0,2*pi), integrated on the CPU without renderer-time
    // wrapping. Components correspond exactly to wave_0 through wave_3.
    temporal_phase: vec4<f32>,
    // x weather energy [0,1], y/z calm/storm perceptual roughness,
    // w maximum foam tint share.
    optics: vec4<f32>,
    shallow_color_linear: vec4<f32>,
    deep_color_linear: vec4<f32>,
};

@group(2) @binding(100)
var<uniform> water: WaterOptics;

const TAU: f32 = 6.283185307179586;
const WATER_PHASE_PERIOD_METRES: f32 = 4096.0;
// Near-water UVs are authored at 0.125 repeat/voxel by the mesher.
const WATER_UV_TO_METRES: f32 = 8.0;

fn spectral_slope(
    position_metres: vec2<f32>,
    temporal_phase: f32,
    wave: vec4<f32>,
    phase_offset: f32,
) -> vec2<f32> {
    let wave_vector = wave.xy * (TAU / WATER_PHASE_PERIOD_METRES);
    let phase = dot(wave_vector, position_metres) + temporal_phase + phase_offset;
    // dh/dx for h=A*sin(kappa.x - omega*t): A*kappa*cos(phase).
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
    // Bevy's PBR input supplies a normalized geometric normal.
    let original_normal = pbr_input.N;
    let top_weight = smoothstep(0.55, 0.96, abs(original_normal.y));

    if top_weight > 0.0 {
        // The integer-space mesher wraps world UVs at an exact 4096-voxel
        // period before f32 conversion. Swizzle restores X/Z for Y faces.
        let position_metres = in.uv.yx * WATER_UV_TO_METRES;
        let slope = spectral_slope(position_metres, water.temporal_phase.x, water.wave_0, 0.31)
            + spectral_slope(position_metres, water.temporal_phase.y, water.wave_1, 1.73)
            + spectral_slope(position_metres, water.temporal_phase.z, water.wave_2, 3.11)
            + spectral_slope(position_metres, water.temporal_phase.w, water.wave_3, 5.07);
        var wave_normal = normalize(vec3<f32>(-slope.x, 1.0, -slope.y));
        if original_normal.y < 0.0 {
            wave_normal = -wave_normal;
        }
        pbr_input.N = normalize(mix(original_normal, wave_normal, top_weight));
        pbr_input.world_normal = pbr_input.N;

        let slope_energy = clamp(length(slope) / 0.46, 0.0, 1.0);
        let weather_energy = clamp(water.optics.x, 0.0, 1.0);
        pbr_input.material.perceptual_roughness = clamp(
            mix(water.optics.y, water.optics.z, weather_energy)
                + slope_energy * 0.035,
            0.089,
            1.0,
        );

        let height = spectral_height(position_metres, water.temporal_phase.x, water.wave_0, 0.31)
            + spectral_height(position_metres, water.temporal_phase.y, water.wave_1, 1.73)
            + spectral_height(position_metres, water.temporal_phase.z, water.wave_2, 3.11)
            + spectral_height(position_metres, water.temporal_phase.w, water.wave_3, 5.07);
        let view_path = 1.0 - clamp(abs(dot(pbr_input.N, pbr_input.V)), 0.0, 1.0);
        let absorption_mix = clamp(0.18 + view_path * 0.62 - height * 0.24, 0.0, 1.0);
        var water_color = mix(
            water.shallow_color_linear.rgb,
            water.deep_color_linear.rgb,
            absorption_mix,
        );

        // A restrained storm-only crest cue; this is not claimed as a foam
        // transport simulation and is capped at the uniform's 10% share.
        let foam = smoothstep(0.66, 0.98, slope_energy)
            * weather_energy
            * clamp(water.optics.w, 0.0, 0.10);
        water_color = mix(water_color, vec3<f32>(0.78, 0.86, 0.88), foam);
        pbr_input.material.base_color = vec4<f32>(water_color, 1.0);
    } else {
        // A bounded, sample-free side cue prevents vertical voxel-water faces
        // from becoming a featureless solid panel. It shares wave_0's CPU
        // phase and therefore cannot inherit the renderer's hourly time wrap.
        let side_coordinate = dot(in.uv, vec2<f32>(0.73, 1.11));
        let side_triangle = 1.0 - abs(
            fract(side_coordinate * 0.19 + water.temporal_phase.x / TAU) * 2.0 - 1.0,
        );
        let side_cue = side_triangle * side_triangle * (3.0 - 2.0 * side_triangle);
        let side_color = mix(
            water.deep_color_linear.rgb * 0.56,
            water.shallow_color_linear.rgb * 0.50,
            0.18 + 0.32 * side_cue,
        );
        pbr_input.material.base_color = vec4<f32>(side_color, 1.0);
    }

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
