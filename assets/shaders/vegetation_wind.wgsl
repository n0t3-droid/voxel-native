// Render-only, world-coherent foliage wind for voxel chunk meshes.
// Slow crown sway and faster leaf flutter are separate bands so the motion
// does not read as a single synchronized sine wave. No physics state is read
// or written by this shader.

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    mesh_functions,
    prepass_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}
#else
#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}
#endif

struct VegetationWind {
    // xy direction [normalized], z macro amplitude [voxel],
    // w macro angular frequency [rad/s].
    direction_macro: vec4<f32>,
    // x flutter amplitude [voxel], y flutter angular frequency [rad/s],
    // z phase gradient [rad/voxel], w cross-wind share [0, 1].
    flutter_phase: vec4<f32>,
    // x/y/z/w modulo-2*pi gust, macro, primary-flutter and secondary-flutter
    // phase [rad], integrated on the CPU without renderer-time wrapping.
    temporal_phase: vec4<f32>,
};

@group(2) @binding(100)
var<uniform> vegetation_wind: VegetationWind;

struct FoliageDeformation {
    // Horizontal X/Z displacement in world-space voxel units.
    offset: vec2<f32>,
    // Analytic world-space partial derivatives of `offset` with respect to
    // input X, Y and Z. Keeping these beside the displacement prevents the
    // lighting normal from drifting away from the animated surface.
    derivative_x: vec2<f32>,
    derivative_y: vec2<f32>,
    derivative_z: vec2<f32>,
};

fn deform_foliage(world_position: vec3<f32>) -> FoliageDeformation {
    let direction = normalize(vegetation_wind.direction_macro.xy);
    let cross_direction = vec2<f32>(-direction.y, direction.x);
    let spatial_phase_gradient = vec2<f32>(0.73, 1.11)
        * vegetation_wind.flutter_phase.z;
    let spatial_phase = dot(world_position.xz, spatial_phase_gradient);

    // A slow secondary wave modulates the primary response into coherent
    // gust packets. Its envelope never reaches zero, avoiding frozen crowns.
    let gust_argument = vegetation_wind.temporal_phase.x
        + spatial_phase * 0.31
        + 1.7;
    let gust_wave = sin(gust_argument);
    let gust_envelope = 0.72 + 0.28 * (gust_wave * 0.5 + 0.5);
    let gust_envelope_derivative = 0.28 * 0.5 * 0.31 * cos(gust_argument);
    let macro_argument = vegetation_wind.temporal_phase.y + spatial_phase;
    let macro_wave = sin(macro_argument);
    let macro_motion = vegetation_wind.direction_macro.z
        * macro_wave
        * gust_envelope;
    let macro_spatial_derivative = vegetation_wind.direction_macro.z
        * (cos(macro_argument) * gust_envelope
            + macro_wave * gust_envelope_derivative);

    // Two incommensurate higher-frequency waves approximate the broad-band
    // leaf motion used by 1/f-inspired real-time vegetation models.
    let flutter_primary_argument = vegetation_wind.temporal_phase.z
        + spatial_phase * 3.17;
    let flutter_secondary_argument = vegetation_wind.temporal_phase.w
        - spatial_phase * 2.30
        + world_position.y * 0.41;
    let flutter_primary = sin(flutter_primary_argument);
    let flutter_secondary = 0.5 * sin(flutter_secondary_argument);
    let flutter_secondary_cos = cos(flutter_secondary_argument);
    let flutter_motion = vegetation_wind.flutter_phase.x
        * (flutter_primary + flutter_secondary);
    let flutter_spatial_derivative = vegetation_wind.flutter_phase.x
        * (3.17 * cos(flutter_primary_argument)
            - 0.5 * 2.30 * flutter_secondary_cos);
    let flutter_height_derivative = vegetation_wind.flutter_phase.x
        * 0.5 * 0.41 * flutter_secondary_cos;

    // Height and position break up large greedy quads without disconnecting
    // the voxel topology or moving their collision representation.
    let local_argument = world_position.y * 0.83 + spatial_phase * 0.61;
    let local_variation = 0.78 + 0.22 * sin(local_argument);
    let local_cos = cos(local_argument);
    let local_spatial_derivative = 0.22 * 0.61 * local_cos;
    let local_height_derivative = 0.22 * 0.83 * local_cos;
    let cross_share = clamp(vegetation_wind.flutter_phase.w, 0.0, 1.0);
    let motion = (
        direction * (macro_motion + flutter_motion * (1.0 - cross_share))
        + cross_direction * flutter_motion * cross_share
    );
    let motion_spatial_derivative = direction * (
        macro_spatial_derivative
        + flutter_spatial_derivative * (1.0 - cross_share)
    ) + cross_direction * flutter_spatial_derivative * cross_share;
    let motion_height_derivative = direction
        * flutter_height_derivative
        * (1.0 - cross_share)
        + cross_direction * flutter_height_derivative * cross_share;
    let offset_spatial_derivative = local_spatial_derivative * motion
        + local_variation * motion_spatial_derivative;
    let offset_height_derivative = local_height_derivative * motion
        + local_variation * motion_height_derivative;

    return FoliageDeformation(
        local_variation * motion,
        offset_spatial_derivative * spatial_phase_gradient.x,
        offset_height_derivative,
        offset_spatial_derivative * spatial_phase_gradient.y,
    );
}

fn displaced_world_normal(
    world_normal: vec3<f32>,
    deformation: FoliageDeformation,
) -> vec3<f32> {
    // For p' = (x + u, y, z + v), transform the original normal with the
    // cofactor of the analytic deformation Jacobian. Cofactor and inverse-
    // transpose differ only by the positive determinant, which normalization
    // removes. The Rust-side derivative budget proves the X/Z Jacobian stays
    // far from singular for every authored weather response.
    let a = 1.0 + deformation.derivative_x.x;
    let b = deformation.derivative_y.x;
    let c = deformation.derivative_z.x;
    let d = deformation.derivative_x.y;
    let e = deformation.derivative_y.y;
    let f = 1.0 + deformation.derivative_z.y;
    let determinant = a * f - c * d;
    let corrected = vec3<f32>(
        f * world_normal.x - d * world_normal.z,
        (c * e - b * f) * world_normal.x
            + determinant * world_normal.y
            + (b * d - a * e) * world_normal.z,
        -c * world_normal.x + a * world_normal.z,
    );
    let corrected_length_squared = dot(corrected, corrected);
    // NaN fails the positive comparison; infinity and extreme finite values
    // trip the upper guard. Both fall back to the geometric world normal.
    if (!(corrected_length_squared > 1e-12) || corrected_length_squared > 1e20) {
        return normalize(world_normal);
    }
    return corrected * inverseSqrt(corrected_length_squared);
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );
    let deformation = deform_foliage(world_position.xyz);
    world_position.x += deformation.offset.x;
    world_position.z += deformation.offset.y;

    out.world_position = world_position;
    out.position = position_world_to_clip(world_position.xyz);

#ifdef DEPTH_CLAMP_ORTHO
    out.clip_position_unclamped = out.position;
    out.position.z = min(out.position.z, 1.0);
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef PREPASS_PIPELINE
#ifdef NORMAL_PREPASS_OR_DEFERRED_PREPASS
    let base_world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
    out.world_normal = displaced_world_normal(base_world_normal, deformation);
#endif
#else
#ifdef VERTEX_NORMALS
    let base_world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
    out.world_normal = displaced_world_normal(base_world_normal, deformation);
#endif
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef MOTION_VECTOR_PREPASS
    // The visual wind has no gameplay transform history. Using the displaced
    // current position is a stable fallback if a motion-vector prepass is
    // enabled later and avoids inventing physical velocity.
    out.previous_world_position = world_position;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif

#ifndef PREPASS_PIPELINE
#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex.instance_index,
        world_from_local[3],
    );
#endif
#endif

    return out;
}
