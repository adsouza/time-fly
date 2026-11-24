// The sea's waves (art pass §5.4): the swell's directional components and the swash at the
// shore. Plain WGSL with no imports, registered as `package::waves`, so water.wesl imports it and
// terrain.wesl can import the swash with no WaterLook uniform (ruling R1).
//
// Mirrored by water.rs: SWASH_REACH_M, SWASH_MAX_AMPLITUDE_M and SWASH_PROBE_M (a Rust test
// reads them from this file), `swash_amplitude_m`, `swash_reach_factor`, `swell_spacing_weight`,
// and the wave vectors `water_waves` builds for `wave_phase`.

const TAU: f32 = 6.2831855;
// How far up and down the beach the swash's edge travels (m) on a gentle beach.
const SWASH_REACH_M: f32 = 25.0;
// The swash's largest rise and fall of the water level (m).
const SWASH_MAX_AMPLITUDE_M: f32 = 0.5;
// How far (m) the swash looks along the floor's slope for the shore.
const SWASH_PROBE_M: f32 = 40.0;

// `swash_amplitude_m`: the reach times the floor's slope, capped, so the edge travels the same
// distance on any gentle beach and a near-flat shore does not flood hundreds of metres.
fn swash_amplitude_m(floor_slope: f32) -> f32 {
    return min(SWASH_REACH_M * floor_slope, SWASH_MAX_AMPLITUDE_M);
}

// The swash's water level (m) above the resting sea: zero on average, so at rest the shoreline
// is the height field's zero contour. `phase` (radians) varies along the coast.
fn swash_level_m(floor_slope: f32, time_s: f32, period_s: f32, phase: f32) -> f32 {
    return swash_amplitude_m(floor_slope) * sin(TAU * fract(time_s / period_s) + phase);
}

// `swash_reach_factor`: how much of the swash level applies at a point `depth_m` below the
// resting sea (negative on land), given the depth found SWASH_PROBE_M along the floor's slope
// toward the shore (downhill from land, uphill from the sea): all of it once the probe is past
// the shore by half the amplitude, none if it is not past it at all. So the edge stays within
// about SWASH_PROBE_M of the resting shoreline even where the floor flattens near the sea.
fn swash_reach_factor(depth_m: f32, probe_depth_m: f32, amplitude_m: f32) -> f32 {
    let crossed = select(probe_depth_m, -probe_depth_m, depth_m > 0.0);
    return smoothstep(0.0, max(0.5 * amplitude_m, 1e-4), crossed);
}

// A directional wave's phase (radians) at region-local xz and time. `wave` is (dir.x, dir.z,
// wavelength m, period s): crests travel along dir at the wavelength over the period. Both terms
// are taken modulo one cycle first, so f32 keeps its precision across the region and the phase
// is continuous where the time wraps (every period divides the wrap).
fn wave_phase(wave: vec4<f32>, xz: vec2<f32>, time_s: f32) -> f32 {
    return TAU * (fract(dot(wave.xy, xz) / wave.z) - fract(time_s / wave.w));
}

// `swell_spacing_weight`: all of a displaced component while the mesh has at least
// `min_vertices` vertices per wavelength, none with three or fewer.
fn swell_spacing_weight(spacing_m: f32, wavelength_m: f32, min_vertices: f32) -> f32 {
    return 1.0 - smoothstep(wavelength_m / min_vertices, wavelength_m / 3.0, spacing_m);
}
