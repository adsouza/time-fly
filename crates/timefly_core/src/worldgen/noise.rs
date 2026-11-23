//! Lattice gradient noise, written twice: here in Rust and in `assets/shaders/noise.wesl`.
//!
//! `hash3` and `gradient_noise3` use only operations WGSL has (`u32` wrapping arithmetic, xor,
//! shifts, `f32` arithmetic), so the two copies agree to float rounding. The golden table in
//! `tests/fixtures/noise_golden.json` holds them together: this file's tests check the Rust copy
//! against it and `crates/timefly/tests/region_gpu.rs` checks the WESL copy.
//! `lattice_split`, `fbm` and `ridged` run only on the CPU, in f64 where it matters.

use bevy_math::DVec3;

/// Twelve edge-midpoint gradients of the cube (Perlin's improved set). Identical in the WESL copy.
const GRADIENTS: [[f32; 3]; 12] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
];

/// Scales raw gradient noise. The raw value cannot exceed about 1.03 (even with the worst gradient
/// at every corner) and a dense scan reaches 0.99, so results lie within +-0.88 and reach about
/// +-0.84. `noise_peak_is_pinned` holds both numbers.
const NOISE_SCALE: f32 = 0.85;

/// PCG3D-style integer hash of a lattice cell and a seed. Wrapping `u32` operations only.
pub fn hash3(cell: [i32; 3], seed: u32) -> u32 {
    let mut x = (cell[0] as u32) ^ seed;
    let mut y = (cell[1] as u32) ^ seed.wrapping_mul(747_796_405);
    let mut z = (cell[2] as u32) ^ seed.wrapping_mul(2_891_336_453);
    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    y = y.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    z = z.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    x = x.wrapping_add(y.wrapping_mul(z));
    y = y.wrapping_add(z.wrapping_mul(x));
    z = z.wrapping_add(x.wrapping_mul(y));
    x ^= x >> 16;
    y ^= y >> 16;
    z ^= z >> 16;
    x = x.wrapping_add(y.wrapping_mul(z));
    y = y.wrapping_add(z.wrapping_mul(x));
    z = z.wrapping_add(x.wrapping_mul(y));
    x ^ y ^ z
}

/// Quintic fade `6t^5 - 15t^4 + 10t^3`.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Gradient noise at `frac` inside `cell`, within +-0.88 (peaks reach about 0.84). `frac` is each component in [0, 1].
pub fn gradient_noise3(cell: [i32; 3], frac: [f32; 3], seed: u32) -> f32 {
    // The dot product of the corner's gradient with the offset from that corner to the point.
    let corner = |dx: i32, dy: i32, dz: i32| -> f32 {
        // Wrapping, like WGSL's i32 addition, so cells at the i32 extremes agree on both sides.
        let h = hash3(
            [
                cell[0].wrapping_add(dx),
                cell[1].wrapping_add(dy),
                cell[2].wrapping_add(dz),
            ],
            seed,
        );
        let g = GRADIENTS[((h >> 16) % 12) as usize];
        g[0] * (frac[0] - dx as f32) + g[1] * (frac[1] - dy as f32) + g[2] * (frac[2] - dz as f32)
    };
    let (u, v, w) = (fade(frac[0]), fade(frac[1]), fade(frac[2]));
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u);
    let y0 = lerp(x00, x10, v);
    let y1 = lerp(x01, x11, v);
    lerp(y0, y1, w) * NOISE_SCALE
}

/// Splits a point into a lattice cell and the fraction within it. The floor is taken in f64 so
/// planet-scale coordinates keep their precision; only the fraction drops to f32.
pub fn lattice_split(p_m: DVec3, cell_m: f64) -> ([i32; 3], [f32; 3]) {
    let scaled = p_m / cell_m;
    let floor = scaled.floor();
    let frac = scaled - floor;
    (
        [floor.x as i32, floor.y as i32, floor.z as i32],
        [frac.x as f32, frac.y as f32, frac.z as f32],
    )
}

/// Seed for octave `i`, so octaves are uncorrelated.
fn octave_seed(seed: u32, i: u32) -> u32 {
    seed.wrapping_add(i.wrapping_mul(0x9E37_79B9))
}

/// Fractal Brownian motion: `octaves` gradient-noise layers, each `lacunarity` times finer and
/// `gain` times weaker than the last, normalised by the total amplitude to within +-0.88.
pub fn fbm(
    p_m: DVec3,
    base_cell_m: f64,
    octaves: u32,
    lacunarity: f64,
    gain: f32,
    seed: u32,
) -> f32 {
    let (mut sum, mut total, mut amp, mut cell_m) = (0.0f32, 0.0f32, 1.0f32, base_cell_m);
    for i in 0..octaves {
        let (cell, frac) = lattice_split(p_m, cell_m);
        sum += amp * gradient_noise3(cell, frac, octave_seed(seed, i));
        total += amp;
        amp *= gain;
        cell_m /= lacunarity;
    }
    if total > 0.0 { sum / total } else { 0.0 }
}

/// Ridged multifractal (lacunarity 2, gain 0.5): sharp crests where the noise crosses zero,
/// in [0.0144, 1]: each octave contributes `(1 - |n|)^2` with |n| <= 0.88.
pub fn ridged(p_m: DVec3, base_cell_m: f64, octaves: u32, seed: u32) -> f32 {
    let (mut sum, mut total, mut amp, mut cell_m) = (0.0f32, 0.0f32, 1.0f32, base_cell_m);
    for i in 0..octaves {
        let (cell, frac) = lattice_split(p_m, cell_m);
        let ridge = 1.0 - gradient_noise3(cell, frac, octave_seed(seed, i)).abs();
        sum += amp * ridge * ridge;
        total += amp;
        amp *= 0.5;
        cell_m *= 0.5;
    }
    if total > 0.0 { sum / total } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const NOISE_PEAK_BOUND: f32 = 0.88;
    const RIDGED_FLOOR: f32 = 0.0144;
    const GOLDEN: &str = include_str!("../../tests/fixtures/noise_golden.json");

    fn frac3() -> impl Strategy<Value = [f32; 3]> {
        prop::array::uniform3(0.0f32..=1.0)
    }

    proptest! {
        #[test]
        fn gradient_noise_is_bounded_and_continuous(
            cell in prop::array::uniform3(prop_oneof![-100_000i32..100_000, Just(i32::MAX), Just(i32::MIN)]),
            f in frac3(),
            seed in any::<u32>(),
            axis in 0usize..3,
            other in prop::array::uniform2(0.0f32..=1.0),
        ) {
            prop_assert!(gradient_noise3(cell, f, seed).abs() <= NOISE_PEAK_BOUND);
            // The face x_axis = 1 of `cell` is the face x_axis = 0 of its neighbour.
            let (i, j) = match axis { 0 => (1, 2), 1 => (0, 2), _ => (0, 1) };
            let mut a = [0.0f32; 3];
            a[i] = other[0];
            a[j] = other[1];
            let mut b = a;
            a[axis] = 1.0;
            b[axis] = 0.0;
            let mut next = cell;
            next[axis] = next[axis].wrapping_add(1);
            let d = (gradient_noise3(cell, a, seed) - gradient_noise3(next, b, seed)).abs();
            prop_assert!(d <= 1e-6, "face mismatch {d}");
        }

        #[test]
        fn lattice_split_round_trips_at_planet_scale(
            p in prop::array::uniform3(-6.4e6f64..6.4e6),
            cell_m in 10.0f64..2_000.0,
        ) {
            let (cell, frac) = lattice_split(DVec3::from_array(p), cell_m);
            for k in 0..3 {
                prop_assert!((0.0..=1.0).contains(&frac[k]));
                let back = f64::from(cell[k]) * cell_m + f64::from(frac[k]) * cell_m;
                prop_assert!((back - p[k]).abs() < 1e-3, "axis {k}: {back} vs {}", p[k]);
            }
        }

        #[test]
        fn fbm_and_ridged_stay_in_range(
            p in prop::array::uniform3(-1.0e6f64..1.0e6),
            octaves in 1u32..8,
            seed in any::<u32>(),
        ) {
            let p = DVec3::from_array(p);
            prop_assert!(fbm(p, 5_000.0, octaves, 2.0, 0.5, seed).abs() <= NOISE_PEAK_BOUND);
            let r = ridged(p, 5_000.0, octaves, seed);
            prop_assert!((RIDGED_FLOOR - 1e-6..=1.0).contains(&r), "{r}");
        }
    }

    #[test]
    fn hash_is_deterministic_and_seed_sensitive() {
        let c = [3, -7, 11];
        assert_eq!(hash3(c, 5), hash3(c, 5));
        assert_ne!(hash3(c, 5), hash3(c, 6));
        assert_ne!(hash3(c, 5), hash3([4, -7, 11], 5));
        assert_ne!(hash3(c, 5), hash3([3, -7, 12], 5));
    }

    /// Peak |noise| over a dense sample of many cells and fractions.
    fn dense_peak() -> f32 {
        let mut peak = 0.0f32;
        for i in 0..2000u32 {
            for k in 0..4096u32 {
                let f = [
                    (k % 16) as f32 / 15.0,
                    ((k / 16) % 16) as f32 / 15.0,
                    (k / 256) as f32 / 15.0,
                ];
                peak = peak.max(gradient_noise3([i as i32, 7, -3], f, i).abs());
            }
        }
        peak
    }

    #[test]
    fn noise_peak_is_pinned() {
        let peak = dense_peak();
        assert!(
            peak <= NOISE_PEAK_BOUND,
            "peak {peak} above the documented bound"
        );
        assert!(peak >= 0.80, "peak {peak}: noise lost range");
    }

    fn golden_rows() -> Vec<serde_json::Value> {
        let v: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
        v["rows"].as_array().unwrap().clone()
    }

    fn row_inputs(r: &serde_json::Value) -> ([i32; 3], [f32; 3], u32) {
        let ints = |k: &str| -> Vec<i64> {
            r[k].as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect()
        };
        let cell = ints("cell");
        let frac: Vec<f32> = r["frac"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap() as f32)
            .collect();
        (
            [cell[0] as i32, cell[1] as i32, cell[2] as i32],
            [frac[0], frac[1], frac[2]],
            r["seed"].as_u64().unwrap() as u32,
        )
    }

    #[test]
    fn matches_golden_table() {
        let rows = golden_rows();
        assert_eq!(rows.len(), 256);
        for r in &rows {
            let (cell, frac, seed) = row_inputs(r);
            let want = r["value"].as_f64().unwrap() as f32;
            let got = gradient_noise3(cell, frac, seed);
            assert!(
                (got - want).abs() <= 1e-6,
                "{cell:?} {frac:?} {seed}: {got} vs {want}"
            );
        }
    }

    /// Regenerates `tests/fixtures/noise_golden.json` from this implementation. Run with
    /// `cargo test -p timefly_core write_noise_golden -- --ignored`.
    #[test]
    #[ignore = "regenerates the golden fixture"]
    fn write_noise_golden() {
        let mut state = 0x2545_F491u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        let mut rows = Vec::new();
        for _ in 0..256 {
            // Mostly +-1e6 (planet-scale cells are about 6.4e5 at 10 m), plus rows with
            // negative, zero and i32-extreme words so the high bits of the hash input vary.
            let n = rows.len();
            let cell = [0, 0, 0].map(|_| match n {
                0..=159 => (next() >> 8) as i32 % 2_000_001 - 1_000_000,
                160..=199 => (next() >> 8) as i32 % 1_280_001 - 640_000,
                _ => [i32::MIN, i32::MAX, -1, 0, i32::MIN + 1, i32::MAX - 1]
                    [(next() >> 8) as usize % 6],
            });
            let frac = [0, 0, 0].map(|_| (next() >> 8) as f32 / (1u32 << 24) as f32);
            let seed = next();
            let value = gradient_noise3(cell, frac, seed);
            rows.push(serde_json::json!({
                "cell": cell, "frac": frac, "seed": seed, "value": value
            }));
        }
        let text = serde_json::to_string_pretty(&serde_json::json!({ "rows": rows })).unwrap();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/noise_golden.json"
        );
        std::fs::write(path, text + "\n").unwrap();
    }
}
