// The dragonfly (M3 design §5.1): a vertex stage that flaps the wings, and a fragment stage that
// lights it through Bevy's StandardMaterial PBR path (the scene's sun and ambient light; the
// material turns fog off) and adds a subtle rim light, so the fly's outline reads over dark
// forest: the rim colour × strength × (1 − N·V)^power, on the body and the wings alike.
//
// The wings flap about the body axis (the model's z) by sin(2π × phase) × amplitude, mirrored
// between the right (x > 0) and left wings. The phase steps a fixed part of a cycle every frame
// (`look.flap.x`, 0.4: creature's `FLAP_CYCLES_PER_FRAME` and `flap_phase`), so the stroke sweeps
// from one side to the other each frame at any frame rate. A beat in time aliased instead: 25 Hz
// sampled at the frame rate read as about 1 Hz at 26 fps, 5 Hz at 30, and held still at 50.

import bevy_pbr::render::mesh_functions;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::mesh_view_bindings::globals;
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};

const TAU: f32 = 6.283185307179586;

struct DragonflyLook {
    // x: the flap's step (cycles per frame); y: flap amplitude (radians).
    flap: vec4<f32>,
    // rgb: rim colour (linear); w: rim strength.
    rim: vec4<f32>,
    // x: rim power.
    rim_shape: vec4<f32>,
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

// The flap's phase (cycles) on this frame. The frame count wraps every 1000 frames, a whole number
// of cycles at 0.4 a frame (creature's `FLAP_WRAP_FRAMES`), so `f32` keeps the phase exact.
fn flap_phase() -> f32 {
    return fract(f32(globals.frame_count % 1000u) * look.flap.x);
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
    let angle = sin(TAU * flap_phase()) * look.flap.y * v.wing * side;
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

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // N faces the viewer on both sides of a double-sided wing (Bevy flips it for back faces).
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    var color = apply_pbr_lighting(pbr_input);
    let facing = clamp(dot(pbr_input.N, pbr_input.V), 0.0, 1.0);
    let rim = pow(1.0 - facing, look.rim_shape.x) * look.rim.w;
    color = vec4<f32>(color.rgb + look.rim.rgb * rim, color.a);
    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, color);
    return out;
}
