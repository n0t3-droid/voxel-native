// Render-only, world-coherent foliage wind for voxel chunk meshes.
// Slow crown sway and faster leaf flutter are separate bands so the motion
// does not read as a single synchronized sine wave. No physics state is read
// or written by this shader.

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    mesh_functions,
    mesh_view_bindings::globals,
    prepass_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}
#else
#import bevy_pbr::{
    mesh_functions,
    mesh_view_bindings::globals,
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
};

@group(2) @binding(100)
var<uniform> vegetation_wind: VegetationWind;

fn displace_foliage(world_position: vec3<f32>) -> vec2<f32> {
    let direction = normalize(vegetation_wind.direction_macro.xy);
    let cross_direction = vec2<f32>(-direction.y, direction.x);
    let spatial_phase = dot(world_position.xz, vec2<f32>(0.73, 1.11))
        * vegetation_wind.flutter_phase.z;
    let time_seconds = globals.time;

    // A slow secondary wave modulates the primary response into coherent
    // gust packets. Its envelope never reaches zero, avoiding frozen crowns.
    let gust_wave = sin(
        time_seconds * vegetation_wind.direction_macro.w * 0.37
        + spatial_phase * 0.31
        + 1.7,
    );
    let gust_envelope = 0.72 + 0.28 * (gust_wave * 0.5 + 0.5);
    let macro_wave = sin(
        time_seconds * vegetation_wind.direction_macro.w + spatial_phase,
    );
    let macro_motion = vegetation_wind.direction_macro.z
        * macro_wave
        * gust_envelope;

    // Two incommensurate higher-frequency waves approximate the broad-band
    // leaf motion used by 1/f-inspired real-time vegetation models.
    let flutter_primary = sin(
        time_seconds * vegetation_wind.flutter_phase.y + spatial_phase * 3.17,
    );
    let flutter_secondary = 0.5 * sin(
        time_seconds * vegetation_wind.flutter_phase.y * 1.71
        - spatial_phase * 2.30
        + world_position.y * 0.41,
    );
    let flutter_motion = vegetation_wind.flutter_phase.x
        * (flutter_primary + flutter_secondary);

    // Height and position break up large greedy quads without disconnecting
    // the voxel topology or moving their collision representation.
    let local_variation = 0.78 + 0.22 * sin(world_position.y * 0.83 + spatial_phase * 0.61);
    let cross_share = clamp(vegetation_wind.flutter_phase.w, 0.0, 1.0);
    return local_variation * (
        direction * (macro_motion + flutter_motion * (1.0 - cross_share))
        + cross_direction * flutter_motion * cross_share
    );
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    var world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );
    let wind_offset = displace_foliage(world_position.xyz);
    world_position.x += wind_offset.x;
    world_position.z += wind_offset.y;

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
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
#endif
#else
#ifdef VERTEX_NORMALS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index,
    );
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
