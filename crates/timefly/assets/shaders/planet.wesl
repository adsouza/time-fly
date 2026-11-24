// The planet-fixed position of a region-local point, mirrored by
// radius_m * timefly_core::worldgen::frame::FrameBasis::local_to_dir.
// Plain WGSL with no imports, so terrain.wesl imports it as `package::planet` and the GPU parity
// test (tests/region_gpu.rs) compiles it on its own.
//
// Dunes and rock bands are keyed to this position, not to region-local metres, so the same place
// looks the same whichever region centre it is entered from.

// The point `xz` metres from the centre (+x east, -z north) on the sphere of radius `radius_m`,
// in the era frame's render axes (azimuthal equidistant: exact distance from the centre).
// `center`, `north` and `east` are the frame's unit vectors.
fn planet_position_m(
    xz: vec2<f32>,
    center: vec3<f32>,
    north: vec3<f32>,
    east: vec3<f32>,
    radius_m: f32,
) -> vec3<f32> {
    let offset = east * xz.x - north * xz.y;
    let dist = length(offset);
    if (dist == 0.0) {
        return center * radius_m;
    }
    let angle = dist / radius_m;
    let dir = normalize(center * cos(angle) + offset * (sin(angle) / dist));
    return dir * radius_m;
}
