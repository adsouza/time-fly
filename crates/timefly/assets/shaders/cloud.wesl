// The transition cloud (M3 design §6): one full-screen pass over the world and under the fly
// and the UI. Not a ray march: a few octaves of the shared gradient noise, each on a sphere
// around the eye at its own distance, so the layers parallax as the camera moves.
//
// - Depth: the clip-space triangle sits at the clip depth of the cloud's view depth (just past
//   the fly, `crate::cloud::fly_view_depth`). The pipeline depth-tests and never writes depth,
//   so the fly (nearer) draws over the cloud and everything behind it is covered. Opaque (from
//   OPAQUE_FROM), the cloud moves to the near plane and covers everything, ground nearer than the
//   fly included, but for a hole around the fly (`crate::cloud::fly_hole`), where it sits just
//   past the fly so the fly stays in front (`crate::cloud::cloud_covers` mirrors the rule).
// - Field: octave k samples the noise at (dir × distance_k + motion) × (cells_k / distance_k):
//   cells_k cells per radian of view, offset by the camera's accumulated motion and the wind (in
//   cloud depths), which moves nearer layers across the view faster.
// - Light: the two broad octaves again, a step toward the sun; where the wisps thin toward the
//   sun they are lit. That relief and the view's angle to the sun go through the painterly ramp
//   (smoothstep(-0.25, 0.65, ·), as on the globe), between a warm sunlit white and a cool
//   lavender shade leaning to the era's zenith, all tinted a little by the era's horizon.
// - Opacity: the density lowers the field's coverage threshold and adds a veil; at density 1
//   every pixel is opaque, whatever the noise says.
// - Tilt-shift: while state.z > 0, the top and bottom bands drop the fine octaves and soften.
// - Colour: display colour in both worlds, never tone-mapped (the globe camera does not tone
//   map; the region's materials tone-map themselves), so the cloud looks the same in each.

import bevy_pbr::render::mesh_view_bindings::view;
import package::noise::gradient_noise3;

struct CloudParams {
    // x: density (0 clear, 1 opaque); y: the cloud's view depth (world units); z: tilt-shift;
    // w: the view's aspect (width over height).
    state: vec4<f32>,
    // The fly's hole in the opaque cloud: xy its centre (NDC); z the radius (NDC height units, 0
    // for none); w the view depth just past the fly.
    fly: vec4<f32>,
    // xyz: the camera's accumulated motion and the wind, in cloud depths.
    motion: vec4<f32>,
    // xyz: world direction toward the active world's sun.
    sun: vec4<f32>,
    // rgb: the sun's linear colour.
    sun_color: vec4<f32>,
    // rgb: the era's sky overhead and at the horizon (linear).
    zenith: vec4<f32>,
    horizon: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> cloud: CloudParams;

const OCTAVES: u32 = 4u;
// `OPAQUE_FROM` in cloud.rs: from this density every pixel is opaque.
const OPAQUE_FROM: f32 = 0.999;
// The broad octaves that shape the wisps; the sun lights these.
const SHAPE_OCTAVES: u32 = 2u;
// Per octave: x: cells per radian of view; y: the layer's distance in cloud depths; z: weight
// (they sum to 1); w: noise seed.
const LAYERS = array<vec4<f32>, 4>(
    vec4<f32>(1.6, 4.0, 0.46, 11.0),
    vec4<f32>(3.4, 2.6, 0.27, 23.0),
    vec4<f32>(7.2, 1.7, 0.16, 37.0),
    vec4<f32>(14.5, 1.15, 0.11, 53.0),
);
// How far toward the sun the lighting looks (radians), and how strongly the difference lights.
const SUN_STEP: f32 = 0.08;
const RELIEF_GAIN: f32 = 7.0;
// The field's contrast around its mean.
const CONTRAST: f32 = 1.8;

struct CloudVertex {
    @location(0) position: vec3<f32>,
}

struct CloudOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

struct CloudFragment {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

// The clip depth of a point at `view_depth` along the view, whatever the projection.
fn clip_depth(view_depth: f32) -> f32 {
    let at = view.clip_from_view * vec4<f32>(0.0, 0.0, -view_depth, 1.0);
    return at.z / at.w;
}

@vertex
fn vertex(v: CloudVertex) -> CloudOut {
    var out: CloudOut;
    // The clip depth of a point at the cloud's view depth, whatever the projection.
    let at = view.clip_from_view * vec4<f32>(0.0, 0.0, -cloud.state.y, 1.0);
    out.clip = vec4<f32>(v.position.xy, at.z / at.w, 1.0);
    out.ndc = v.position.xy;
    return out;
}

fn srgb(r: f32, g: f32, b: f32) -> vec3<f32> {
    let c = vec3<f32>(r, g, b) / 255.0;
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Octave `k` of the field along the world view direction `dir`, in about +-0.88.
fn octave(dir: vec3<f32>, k: u32) -> f32 {
    let layer = LAYERS[k];
    let p = (dir * layer.y + cloud.motion.xyz) * (layer.x / layer.y);
    let base = floor(p);
    return gradient_noise3(vec3<i32>(base), p - base, u32(layer.w));
}

// The broad octaves alone: the wisps' shape.
fn shape(dir: vec3<f32>) -> f32 {
    var s = 0.0;
    for (var k = 0u; k < SHAPE_OCTAVES; k += 1u) {
        s += octave(dir, k) * LAYERS[k].z;
    }
    return s;
}

@fragment
fn fragment(in: CloudOut) -> CloudFragment {
    var out: CloudFragment;
    out.color = cloud_color(in);
    out.depth = in.clip.z;
    if cloud.state.x >= OPAQUE_FROM {
        // Opaque: the near plane (reverse-Z 1) covers everything but the fly's hole.
        let in_hole = cloud.fly.z > 0.0 && length((in.ndc - cloud.fly.xy) * vec2<f32>(cloud.state.w, 1.0)) <= cloud.fly.z;
        out.depth = select(1.0, clip_depth(cloud.fly.w), in_hole);
    }
    return out;
}

fn cloud_color(in: CloudOut) -> vec4<f32> {
    let density = cloud.state.x;
    if density <= 0.0 {
        // Clear: drawn only to warm the pipeline.
        return vec4<f32>(0.0);
    }
    // The view ray, rotated to the world without subtracting large world positions (the globe is
    // in kilometres thousands out).
    let v = view.view_from_clip * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize((view.world_from_view * vec4<f32>(v.xyz / v.w, 0.0)).xyz);

    // Tilt-shift: the top and bottom bands soften.
    let band = cloud.state.z * smoothstep(0.35, 0.95, abs(in.ndc.y));

    var field = 0.0;
    let broad = shape(dir);
    field += broad;
    for (var k = SHAPE_OCTAVES; k < OCTAVES; k += 1u) {
        field += octave(dir, k) * LAYERS[k].z * (1.0 - band);
    }

    // Light: lit where the wisps thin toward the sun, and brighter on the sun's side of the view.
    let sun = normalize(cloud.sun.xyz);
    let tangent = sun - dir * dot(dir, sun);
    let toward = normalize(dir + tangent * SUN_STEP);
    let relief = clamp((broad - shape(toward)) * RELIEF_GAIN, -1.0, 1.0);
    let facing = dot(dir, sun);
    let ndl = 0.7 * relief + 0.3 * facing + 0.15;
    let ramp = smoothstep(-0.25, 0.65, ndl);

    let horizon = cloud.horizon.rgb / max(max(cloud.horizon.r, cloud.horizon.g), max(cloud.horizon.b, 1e-4));
    let lit = mix(vec3<f32>(1.0, 0.99, 0.97), cloud.sun_color.rgb, 0.3) * mix(vec3<f32>(1.0), horizon, 0.15);
    let lavender = srgb(150.0, 140.0, 176.0);
    let shade = mix(lit * 0.66, mix(cloud.zenith.rgb, lavender, 0.5), 0.4);
    var color = mix(shade, lit, ramp);
    // Forward scatter: a warm glow toward the sun.
    color += cloud.sun_color.rgb * pow(max(facing, 0.0), 6.0) * 0.25;
    // The white-out: as the cloud closes the shade lifts toward the light; the wisps still move.
    color = mix(color, lit, smoothstep(0.75, 1.0, density) * 0.65);

    // Opacity: the density lowers the coverage threshold and thickens a veil; the view's edges
    // gather first, so the cloud closes in around the fly.
    let edge = smoothstep(0.4, 1.3, length(in.ndc)) * (1.0 - density) * 0.15;
    let w = clamp(0.5 + field * CONTRAST + edge, 0.0, 1.0);
    let threshold = mix(1.15, -0.1, density);
    let soft = mix(0.16, 0.4, band);
    let wisps = smoothstep(threshold - soft, threshold + soft, w);
    let veil = 0.65 * density * density;
    var alpha = 1.0 - (1.0 - wisps) * (1.0 - veil);
    alpha = max(alpha, step(0.999, density));
    return vec4<f32>(min(color, vec3<f32>(1.0)), alpha);
}
