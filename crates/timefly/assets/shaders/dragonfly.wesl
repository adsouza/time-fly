// The dragonfly (M3 design §5.1): a vertex stage that flaps the wings, then Bevy's
// StandardMaterial PBR fragment (the scene's sun and ambient light; the material turns fog off).
//
// The wings flap about the body axis (the model's z) by sin(time × 2π × hz) × amplitude, mirrored
// between the right (x > 0) and left wings. At 25 Hz the beats alias at any frame rate, which
// reads as a blur rather than as individual strokes. `globals.time` is Bevy's virtual time.

import bevy_pbr::render::mesh_functions;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::mesh_view_bindings::globals;
import bevy_pbr::render::forward_io::VertexOutput;

const TAU: f32 = 6.283185307179586;

struct DragonflyLook {
    // x: flap frequency (Hz); y: flap amplitude (radians).
    flap: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<uniform> look: DragonflyLook;

struct DragonflyVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(5) color: vec4<f32>,
    // 0 on the body, 1 on a wing.
    @location(8) wing: f32,
}

// Rotates `v` about the z axis by `angle`.
fn about_z(v: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return vec3<f32>(v.x * c - v.y * s, v.x * s + v.y * c, v.z);
}

@vertex
fn vertex(v: DragonflyVertex) -> VertexOutput {
    // Positive angles raise the right wing; the left wing mirrors it, so both rise together.
    let side = select(-1.0, 1.0, v.position.x >= 0.0);
    let angle = sin(globals.time * TAU * look.flap.x) * look.flap.y * v.wing * side;
    let position = about_z(v.position, angle);
    let normal = about_z(v.normal, angle);

    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    out.world_position = mesh_functions::mesh_position_local_to_world(
        world_from_local,
        vec4<f32>(position, 1.0),
    );
    out.position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(normal, v.instance_index);
    @if(VERTEX_COLORS)
    out.color = v.color;
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = v.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        v.instance_index,
        world_from_local[3],
    );
    return out;
}
