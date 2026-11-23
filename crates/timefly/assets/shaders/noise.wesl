// Lattice gradient noise, mirrored one-to-one by timefly_core::worldgen::noise (spec §8.3).
// Plain WGSL with no imports, so Bevy shaders can `import` it as a module and the golden test
// (tests/region_gpu.rs) can compile it on its own. Keep the table, hash and fade identical to
// the Rust copy; tests/fixtures/noise_golden.json holds them together.

const NOISE_SCALE: f32 = 0.85;

const GRADIENTS = array<vec3<f32>, 12>(
    vec3<f32>(1.0, 1.0, 0.0),
    vec3<f32>(-1.0, 1.0, 0.0),
    vec3<f32>(1.0, -1.0, 0.0),
    vec3<f32>(-1.0, -1.0, 0.0),
    vec3<f32>(1.0, 0.0, 1.0),
    vec3<f32>(-1.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, -1.0),
    vec3<f32>(-1.0, 0.0, -1.0),
    vec3<f32>(0.0, 1.0, 1.0),
    vec3<f32>(0.0, -1.0, 1.0),
    vec3<f32>(0.0, 1.0, -1.0),
    vec3<f32>(0.0, -1.0, -1.0),
);

// PCG3D-style integer hash; wrapping u32 arithmetic only.
fn hash3(cell: vec3<i32>, seed: u32) -> u32 {
    var x = bitcast<u32>(cell.x) ^ seed;
    var y = bitcast<u32>(cell.y) ^ (seed * 747796405u);
    var z = bitcast<u32>(cell.z) ^ (seed * 2891336453u);
    x = x * 1664525u + 1013904223u;
    y = y * 1664525u + 1013904223u;
    z = z * 1664525u + 1013904223u;
    x = x + y * z;
    y = y + z * x;
    z = z + x * y;
    x = x ^ (x >> 16u);
    y = y ^ (y >> 16u);
    z = z ^ (z >> 16u);
    x = x + y * z;
    y = y + z * x;
    z = z + x * y;
    return x ^ y ^ z;
}

fn fade(t: f32) -> f32 {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    return a + (b - a) * t;
}

// Dot of the corner's gradient with the offset from that corner to the point.
fn corner(cell: vec3<i32>, frac: vec3<f32>, d: vec3<i32>, seed: u32) -> f32 {
    let h = hash3(cell + d, seed);
    let g = GRADIENTS[(h >> 16u) % 12u];
    return dot(g, frac - vec3<f32>(d));
}

// Gradient noise at `frac` inside `cell`, within +-0.88 (peaks reach about 0.84), as pinned by
// the Rust tests. i32 addition of neighbour offsets wraps, matching the Rust copy.
fn gradient_noise3(cell: vec3<i32>, frac: vec3<f32>, seed: u32) -> f32 {
    let u = fade(frac.x);
    let v = fade(frac.y);
    let w = fade(frac.z);
    let x00 = lerp(corner(cell, frac, vec3<i32>(0, 0, 0), seed), corner(cell, frac, vec3<i32>(1, 0, 0), seed), u);
    let x10 = lerp(corner(cell, frac, vec3<i32>(0, 1, 0), seed), corner(cell, frac, vec3<i32>(1, 1, 0), seed), u);
    let x01 = lerp(corner(cell, frac, vec3<i32>(0, 0, 1), seed), corner(cell, frac, vec3<i32>(1, 0, 1), seed), u);
    let x11 = lerp(corner(cell, frac, vec3<i32>(0, 1, 1), seed), corner(cell, frac, vec3<i32>(1, 1, 1), seed), u);
    let y0 = lerp(x00, x10, v);
    let y1 = lerp(x01, x11, v);
    return lerp(y0, y1, w) * NOISE_SCALE;
}
