use super::*;
use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
use crate::hydrology::params::NO_WATER_DROP_M;
use crate::worldgen::generate::RegionSpec;
use crate::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};
use bevy_math::DVec3;
use proptest::prelude::*;

const CLEARANCE_M: f64 = 150.0;
const CEILING_M: f64 = 60_000.0;

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A coordinate inside the inner field, often on a patch boundary or the field's edge.
fn region_coord() -> impl Strategy<Value = f64> {
    let half = REGION_SIZE_M / 2.0;
    let finest = LodSettings::region().finest_patch_m;
    prop_oneof![
        6 => -half..=half,
        1 => (-32i32..=32).prop_map(move |k| f64::from(k) * finest),
        1 => Just(half),
        1 => Just(-half),
        1 => Just(0.0),
    ]
}

fn altitude() -> impl Strategy<Value = f64> {
    prop_oneof![
        6 => CLEARANCE_M..=CEILING_M,
        2 => CLEARANCE_M..=2_000.0,
        1 => Just(CLEARANCE_M),
        1 => Just(CEILING_M),
    ]
}

fn camera() -> impl Strategy<Value = DVec3> {
    (region_coord(), altitude(), region_coord()).prop_map(|(x, y, z)| DVec3::new(x, y, z))
}

fn f32_camera(c: DVec3) -> [f32; 3] {
    [c.x as f32, c.y as f32, c.z as f32]
}

fn quad_m(p: &Patch, s: &LodSettings) -> f64 {
    p.size_m / f64::from(s.patch_quads)
}

/// Morphed vertices of `p` along one of its edges, ordered along the edge, over the part of the
/// edge in `[lo, hi]` (metres along the edge axis), with consecutive duplicates removed.
/// `vertical` edges run along z at grid column `fixed`; horizontal ones along x at row `fixed`.
fn edge_points(
    p: &Patch,
    s: &LodSettings,
    cam: [f32; 3],
    vertical: bool,
    fixed: u32,
    lo: f64,
    hi: f64,
) -> Vec<(u32, u32)> {
    let q = quad_m(p, s);
    let start = if vertical {
        p.origin_m[1]
    } else {
        p.origin_m[0]
    };
    let first = ((lo - start) / q).round() as u32;
    let last = ((hi - start) / q).round() as u32;
    let mut out: Vec<(u32, u32)> = Vec::new();
    for k in first..=last {
        let idx = if vertical { [fixed, k] } else { [k, fixed] };
        let [x, z] = morph_vertex(idx, p, cam, s);
        let key = (x.to_bits(), z.to_bits());
        if out.last() != Some(&key) {
            out.push(key);
        }
    }
    out
}

/// Every pair of patches sharing an edge of positive length:
/// `(a, b, vertical, lo, hi)` with `a` west of (or north of) `b`.
fn shared_edges(patches: &[Patch]) -> Vec<(usize, usize, bool, f64, f64)> {
    let mut out = Vec::new();
    for (i, a) in patches.iter().enumerate() {
        for (j, b) in patches.iter().enumerate() {
            let (ax1, az1) = (a.origin_m[0] + a.size_m, a.origin_m[1] + a.size_m);
            let (bx1, bz1) = (b.origin_m[0] + b.size_m, b.origin_m[1] + b.size_m);
            if ax1 == b.origin_m[0] {
                let lo = a.origin_m[1].max(b.origin_m[1]);
                let hi = az1.min(bz1);
                if hi > lo {
                    out.push((i, j, true, lo, hi));
                }
            }
            if az1 == b.origin_m[1] {
                let lo = a.origin_m[0].max(b.origin_m[0]);
                let hi = ax1.min(bx1);
                if hi > lo {
                    out.push((i, j, false, lo, hi));
                }
            }
        }
    }
    out
}

fn vertex_distance_m(p: &Patch, s: &LodSettings, idx: [u32; 2], cam: DVec3) -> f64 {
    let q = quad_m(p, s);
    let x = p.origin_m[0] + f64::from(idx[0]) * q;
    let z = p.origin_m[1] + f64::from(idx[1]) * q;
    DVec3::new(x - cam.x, cam.y, z - cam.z).length()
}

// ---------------------------------------------------------------------------------------------
// Settings and selection
// ---------------------------------------------------------------------------------------------

#[test]
fn region_settings_match_the_plan() {
    let s = LodSettings::region();
    assert_eq!(s.patch_quads, PATCH_QUADS);
    assert_eq!(s.levels, 10);
    assert_eq!(s.morph_start, MORPH_START);
    assert_eq!(s.morph_end, MORPH_END);
    // 4,687.5 m (the plan's "4,688 m"), so ten levels tile the apron exactly.
    assert_eq!(s.finest_patch_m, 4_687.5);
    assert_eq!(s.finest_quad_m(), 146.484_375);
    assert_eq!(s.extent_m(), APRON_SIZE_M);
    assert!(s.base_range_m > 0.0);
}

#[test]
fn ranges_double_per_level() {
    let s = LodSettings::region();
    for lod in 0..s.levels {
        assert_eq!(s.range_m(lod), s.base_range_m * f64::from(1u32 << lod));
        assert_eq!(
            s.patch_size_m(lod),
            s.finest_patch_m * f64::from(1u32 << lod)
        );
        let [start, end] = s.morph_range_m(lod);
        assert!(start < end, "lod {lod}: {start} .. {end}");
        assert!(f64::from(end) < s.range_m(lod), "lod {lod} ends at {end}");
        if lod > 0 {
            assert!(
                f64::from(start) > s.range_m(lod - 1),
                "lod {lod} starts at {start}"
            );
        }
    }
}

#[test]
fn far_camera_selects_only_the_root() {
    let s = LodSettings::region();
    let p = select_patches(DVec3::new(0.0, 1.0e9, 0.0), &s);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].lod, s.levels - 1);
    assert_eq!(p[0].origin_m, [-APRON_SIZE_M / 2.0; 2]);
    assert_eq!(p[0].size_m, APRON_SIZE_M);
}

#[test]
fn ground_camera_gets_the_finest_lod_underfoot() {
    let s = LodSettings::region();
    let cam = DVec3::new(1_234.0, CLEARANCE_M, -5_678.0);
    let patches = select_patches(cam, &s);
    let under = patches
        .iter()
        .find(|p| {
            (p.origin_m[0]..=p.origin_m[0] + p.size_m).contains(&cam.x)
                && (p.origin_m[1]..=p.origin_m[1] + p.size_m).contains(&cam.z)
        })
        .expect("a patch under the camera");
    assert_eq!(under.lod, 0);
    assert_eq!(under.morph_range_m, s.morph_range_m(0));
}

fn assert_tiles_the_apron(patches: &[Patch], s: &LodSettings) {
    let half = APRON_SIZE_M / 2.0;
    let mut area = 0.0;
    for p in patches {
        assert!(p.lod < s.levels);
        assert_eq!(p.size_m, s.patch_size_m(p.lod));
        assert!(
            p.origin_m[0] >= -half && p.origin_m[0] + p.size_m <= half,
            "{p:?}"
        );
        assert!(
            p.origin_m[1] >= -half && p.origin_m[1] + p.size_m <= half,
            "{p:?}"
        );
        // Aligned to its own size, as a quadtree node is.
        for c in p.origin_m {
            let k = (c + half) / p.size_m;
            assert_eq!(k, k.round(), "{p:?} is off its grid");
        }
        area += p.size_m * p.size_m;
    }
    // Sizes are dyadic multiples of the finest patch, so the sum is exact.
    assert_eq!(area, APRON_SIZE_M * APRON_SIZE_M, "gaps or overlaps");
    for (i, a) in patches.iter().enumerate() {
        for b in &patches[i + 1..] {
            let ox = (a.origin_m[0] + a.size_m).min(b.origin_m[0] + b.size_m)
                - a.origin_m[0].max(b.origin_m[0]);
            let oz = (a.origin_m[1] + a.size_m).min(b.origin_m[1] + b.size_m)
                - a.origin_m[1].max(b.origin_m[1]);
            assert!(ox <= 0.0 || oz <= 0.0, "{a:?} overlaps {b:?}");
        }
    }
}

#[test]
fn patch_count_is_bounded() {
    let s = LodSettings::region();
    let half = REGION_SIZE_M / 2.0;
    let mut worst = (0, DVec3::ZERO);
    let steps = 24;
    for ix in 0..=steps {
        for iz in 0..=steps {
            for &y in &[CLEARANCE_M, 1_000.0, 8_000.0, 30_000.0, CEILING_M] {
                let x = -half + REGION_SIZE_M * f64::from(ix) / f64::from(steps);
                let z = -half + REGION_SIZE_M * f64::from(iz) / f64::from(steps);
                let cam = DVec3::new(x, y, z);
                let n = select_patches(cam, &s).len();
                if n > worst.0 {
                    worst = (n, cam);
                }
            }
        }
    }
    assert!(worst.0 <= 600, "{} patches at {:?}", worst.0, worst.1);
}

// ---------------------------------------------------------------------------------------------
// Morph
// ---------------------------------------------------------------------------------------------

#[test]
fn morph_is_continuous_across_rings() {
    let s = LodSettings::region();
    for lod in 0..s.levels {
        let range = s.range_m(lod);
        let morph = s.morph_range_m(lod);
        // Reaches 1 at the ring's outer range, and stays there.
        assert_eq!(morph_factor(range as f32, morph), 1.0, "lod {lod}");
        assert_eq!(morph_factor((range * 1.5) as f32, morph), 1.0, "lod {lod}");
        // Zero at the ring's inner range, where the next finer ring is fully morphed.
        let inner = if lod == 0 { 0.0 } else { s.range_m(lod - 1) };
        assert_eq!(morph_factor(inner as f32, morph), 0.0, "lod {lod}");
        // Across the boundary into the next ring the geometry matches: this ring is fully
        // morphed (it looks like the coarser LOD) and the coarser ring is not morphing yet.
        if lod + 1 < s.levels {
            assert_eq!(morph_factor(range as f32, s.morph_range_m(lod + 1)), 0.0);
        }
        // Continuous and monotone in between: no step larger than the slope allows.
        let span = f64::from(morph[1] - morph[0]);
        let n = 4_000;
        let mut prev = 0.0f32;
        for k in 0..=n {
            let d = inner + (range - inner) * f64::from(k) / f64::from(n);
            let f = morph_factor(d as f32, morph);
            assert!((0.0..=1.0).contains(&f));
            assert!(f >= prev, "lod {lod}: factor falls at {d}");
            let max_step = ((range - inner) / f64::from(n) / span) as f32 + 1e-4;
            assert!(f - prev <= max_step, "lod {lod}: jump at {d}");
            prev = f;
        }
    }
}

#[test]
fn unmorphed_vertices_sit_on_their_grid() {
    let s = LodSettings::region();
    // A camera far above: nothing below the root's range morphs, and only the root is selected.
    let far = DVec3::new(0.0, 1.0e9, 0.0);
    let cam = [0.0, 1.0e3, 0.0];
    let root = select_patches(far, &s)[0];
    let quad = quad_m(&root, &s);
    for idx in [[0, 0], [1, 0], [17, 31], [32, 32]] {
        let [x, z] = morph_vertex(idx, &root, cam, &s);
        assert_eq!(f64::from(x), root.origin_m[0] + f64::from(idx[0]) * quad);
        assert_eq!(f64::from(z), root.origin_m[1] + f64::from(idx[1]) * quad);
    }
}

#[test]
fn fully_morphed_odd_vertices_snap_to_the_even_neighbour() {
    let s = LodSettings::region();
    let lod = 2;
    let size = s.patch_size_m(lod);
    let patch = Patch {
        origin_m: [-size * 3.0, size * 2.0],
        size_m: size,
        lod,
        morph_range_m: s.morph_range_m(lod),
    };
    // A camera well beyond the morph end: factor 1 everywhere on the patch.
    let cam = [0.0, (s.range_m(lod) * 4.0) as f32, 0.0];
    for i in 0..=s.patch_quads {
        for j in [0, 1, 2, 31, 32] {
            let got = morph_vertex([i, j], &patch, cam, &s);
            let want = morph_vertex([i & !1, j & !1], &patch, cam, &s);
            assert_eq!(got, want, "index ({i}, {j})");
        }
    }
    // Even vertices never move.
    let [x, z] = morph_vertex([4, 6], &patch, cam, &s);
    let q = quad_m(&patch, &s);
    assert_eq!(f64::from(x), patch.origin_m[0] + 4.0 * q);
    assert_eq!(f64::from(z), patch.origin_m[1] + 6.0 * q);
}

#[test]
fn partial_morph_moves_odd_vertices_toward_the_even_neighbour() {
    let s = LodSettings::region();
    let lod = 0;
    let size = s.patch_size_m(lod);
    let patch = Patch {
        origin_m: [0.0, 0.0],
        size_m: size,
        lod,
        morph_range_m: s.morph_range_m(lod),
    };
    let [start, end] = patch.morph_range_m;
    // Camera straight above the patch origin at the middle of the morph range: vertices near
    // the origin are at about that distance.
    let cam = [0.0, (start + end) / 2.0, 0.0];
    let [x, z] = morph_vertex([1, 0], &patch, cam, &s);
    let q = quad_m(&patch, &s) as f32;
    assert_eq!(z, 0.0);
    assert!(x > 0.0 && x < q, "x = {x}");
    assert!((x - q * 0.5).abs() < q * 0.05, "x = {x}");
}

/// Every pair of selected patches sharing an edge differs by at most one LOD, and both give
/// the same morphed vertices along the edge, bit for bit.
fn check_shared_edges(cam: DVec3, s: &LodSettings) -> Result<(), TestCaseError> {
    let patches = select_patches(cam, s);
    let c = f32_camera(cam);
    let n = s.patch_quads;
    for (i, j, vertical, lo, hi) in shared_edges(&patches) {
        let (a, b) = (&patches[i], &patches[j]);
        prop_assert!(
            a.lod.abs_diff(b.lod) <= 1,
            "T-junction: {a:?} next to {b:?}"
        );
        let pa = edge_points(a, s, c, vertical, n, lo, hi);
        let pb = edge_points(b, s, c, vertical, 0, lo, hi);
        prop_assert_eq!(&pa, &pb, "crack between {:?} and {:?}", a, b);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn shared_edges_match_after_morph(cam in camera()) {
        check_shared_edges(cam, &LodSettings::region())?;
    }

    /// The region's sizes are dyadic, so even patch-relative f32 positions happen to round
    /// alike; with a size that is not, only computing from whole cells keeps edges exact.
    #[test]
    fn shared_edges_match_for_a_non_dyadic_patch_size(cam in camera()) {
        let finest_patch_m = 4_687.3;
        let s = LodSettings {
            finest_patch_m,
            base_range_m: 4.25 * finest_patch_m,
            ..LodSettings::region()
        };
        check_shared_edges(cam, &s)?;
    }

    #[test]
    fn selection_covers_the_apron_without_overlap(cam in camera()) {
        let s = LodSettings::region();
        assert_tiles_the_apron(&select_patches(cam, &s), &s);
    }

    /// Every selected vertex at or beyond its LOD's range is fully morphed, so it lies on the
    /// coarser LOD's grid where that LOD takes over.
    #[test]
    fn vertices_beyond_their_range_are_fully_morphed(cam in camera()) {
        let s = LodSettings::region();
        let c = f32_camera(cam);
        for p in select_patches(cam, &s) {
            let range = s.range_m(p.lod);
            for idx in [[0, 1], [1, 0], [1, 1], [31, 32], [32, 31], [15, 17]] {
                if vertex_distance_m(&p, &s, idx, cam) >= range {
                    let got = morph_vertex(idx, &p, c, &s);
                    let want = morph_vertex([idx[0] & !1, idx[1] & !1], &p, c, &s);
                    prop_assert_eq!(got, want, "{:?} at {:?}", p, idx);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Height blend and curvature
// ---------------------------------------------------------------------------------------------

fn inner_fn(x: f64, z: f64) -> f64 {
    1_000.0 + 300.0 * (x / 7_000.0).sin() * (z / 9_000.0).cos()
}

fn apron_fn(x: f64, z: f64) -> f64 {
    inner_fn(x, z) + 400.0 + 200.0 * (x / 50_000.0).sin()
}

fn field(res: u32, size_m: f64, f: fn(f64, f64) -> f64) -> Field {
    let n = (res * res) as usize;
    let mut elevation_m = Vec::with_capacity(n);
    let step = size_m / f64::from(res - 1);
    for row in 0..res {
        for col in 0..res {
            let x = -size_m / 2.0 + f64::from(col) * step;
            let z = -size_m / 2.0 + f64::from(row) * step;
            elevation_m.push(f(x, z) as f32);
        }
    }
    Field::dry(res, size_m, elevation_m, vec![[0; 4]; n])
}

fn test_grid() -> RegionGrid {
    RegionGrid {
        spec: RegionSpec::new(RenderDir(DVec3::Y), YearsBp(0.0), 1),
        inner: field(REGION_RES, REGION_SIZE_M, inner_fn),
        apron: field(APRON_RES, APRON_SIZE_M, apron_fn),
        boulders: Vec::new(),
    }
}

#[test]
fn blend_is_continuous() {
    let grid = test_grid();
    let e1 = REGION_SIZE_M / 2.0;
    let e0 = e1 - BLEND_APRON_TEXELS * grid.apron.spacing_m();
    assert_eq!(e0, 131_250.0);
    // Slopes: the fields' own (under 0.09 m/m) plus the blend's (|apron − inner| ≤ 700 m over a
    // smoothstep of max slope 1.5 / 18,750 m). A hard switch would jump about 400 m.
    let max_step = 0.25;
    // Lines `(x, z) = offset + t × direction` crossing the transition: along x at several z
    // (centre, near a corner, on the edge), westward, along z, and along the diagonal.
    let mut lines: Vec<([f64; 2], [f64; 2])> = Vec::new();
    for c in [0.0, 100_000.0, 140_000.0, -e1] {
        lines.push(([0.0, c], [1.0, 0.0]));
        lines.push(([0.0, c], [-1.0, 0.0]));
        lines.push(([c, 0.0], [0.0, 1.0]));
    }
    lines.push(([0.0, 0.0], [1.0, 1.0]));
    for (offset, dir) in lines {
        let at = |t: f64| (offset[0] + t * dir[0], offset[1] + t * dir[1]);
        let mut t = e0 - 2_000.0;
        let (x, z) = at(t);
        let mut prev = blended_height_m(&grid, x, z);
        while t < e1 + 2_000.0 {
            t += 1.0;
            let (x, z) = at(t);
            let h = blended_height_m(&grid, x, z);
            assert!(
                (h - prev).abs() <= max_step,
                "jump of {} m at ({x}, {z})",
                h - prev
            );
            prev = h;
        }
    }
}

#[test]
fn blend_is_the_inner_field_inside_and_the_apron_outside() {
    let grid = test_grid();
    for (x, z) in [
        (0.0, 0.0),
        (-131_250.0, 5.0),
        (12_345.6, -98_765.4),
        (131_250.0, 131_250.0),
    ] {
        assert_eq!(
            blended_height_m(&grid, x, z),
            grid.inner.height_at(x, z).unwrap(),
            "({x}, {z})"
        );
    }
    for (x, z) in [
        (150_000.0, 0.0),
        (-200_000.0, 3.0),
        (0.0, 1_199_999.0),
        (500_000.0, -800_000.0),
    ] {
        let want = grid.apron.height_at(x, z).unwrap();
        assert!(
            (blended_height_m(&grid, x, z) - want).abs() < 1e-3,
            "({x}, {z})"
        );
    }
    // Beyond the apron: its edge value.
    let edge = grid.apron.height_at(APRON_SIZE_M / 2.0, 0.0).unwrap();
    assert!((blended_height_m(&grid, 2.0e6, 0.0) - edge).abs() < 1e-3);
    // Halfway through the blend: between the two.
    let mid = (131_250.0 + 150_000.0) / 2.0;
    let (hi, ha) = (
        grid.inner.height_at(mid, 0.0).unwrap(),
        grid.apron.height_at(mid, 0.0).unwrap(),
    );
    let h = blended_height_m(&grid, mid, 0.0);
    assert!((h - (hi + ha) / 2.0).abs() < 1e-2, "{h} vs {hi} and {ha}");
}

/// `f` at each texel of a field shaped like `like`.
fn texels_of<T>(like: &Field, f: impl Fn(f64, f64) -> T) -> Vec<T> {
    (0..like.res)
        .flat_map(|row| (0..like.res).map(move |col| (row, col)))
        .map(|(row, col)| {
            let (x, z) = like.local_of(row, col);
            f(x, z)
        })
        .collect()
}

#[test]
fn blended_water_mirrors_blended_height() {
    use crate::worldgen::generate::water::{decode_river, encode_river};
    let mut grid = test_grid();
    // Water levels, flows and a sub-texel river (22 m wide, along x = 1,234.5 m in the inner
    // field: its texels hold the signed offset and ribbon, the apron's none).
    let level = |x: f64, z: f64| (40.0 + 30.0 * (x / 11_000.0).cos() + z / 9_000.0) as f32;
    let east = |x: f64, _z: f64| (1.5 * (x / 20_000.0).sin()) as f32;
    let north = |_x: f64, z: f64| (0.75 * (z / 30_000.0).cos()) as f32;
    let x0 = 1_234.5;
    for (f, river) in [(&mut grid.inner, true), (&mut grid.apron, false)] {
        f.water_level_m = texels_of(f, level);
        let offsets = texels_of(f, |x, _| (x - x0) as f32);
        let (es, ns) = (texels_of(f, east), texels_of(f, north));
        f.river = (0..offsets.len())
            .map(|i| {
                let o = if river { offsets[i] } else { 0.0 };
                let ribbon = if river { 11.0 - o.abs() } else { 1000.0 };
                encode_river(ribbon, es[i], ns[i], o)
            })
            .collect();
    }
    // A grid whose heights are each quantity: blended_water must match blended_height_m on it.
    let mirror = |pick: &dyn Fn(&Field, usize) -> f32| {
        let mut m = grid.clone();
        for f in [&mut m.inner, &mut m.apron] {
            f.elevation_m = (0..f.river.len()).map(|i| pick(f, i)).collect();
        }
        m
    };
    let levels = mirror(&|f, i| f.water_level_m[i]);
    let easts = mirror(&|f, i| decode_river(f.river[i])[1]);
    let norths = mirror(&|f, i| decode_river(f.river[i])[2]);
    let e1 = REGION_SIZE_M / 2.0;
    let mut points: Vec<(f64, f64)> = vec![(0.0, 0.0), (e1, e1), (-e1, 3.0), (2.0e6, -1.0e6)];
    for k in 0..400 {
        let t = f64::from(k);
        // Through the inner field, the blend (131-150 km) and the apron beyond.
        points.push((
            (t * 0.618_034).fract() * 3.2e5 - 1.6e5,
            (t * 0.754_878).fract() * 3.2e5 - 1.6e5,
        ));
        points.push((
            (t * 0.381_966).fract() * 2.6e6 - 1.3e6,
            (t * 0.569_840).fract() * 2.6e6 - 1.3e6,
        ));
    }
    for &(x, z) in &points {
        let w = blended_water(&grid, x, z);
        assert_eq!(
            w.level_m.to_bits(),
            blended_height_m(&levels, x, z).to_bits(),
            "({x}, {z})"
        );
        assert_eq!(w.thickness_m, w.level_m - blended_height_m(&grid, x, z));
        assert_eq!(
            w.flow[0].to_bits(),
            blended_height_m(&easts, x, z).to_bits()
        );
        assert_eq!(
            w.flow[1].to_bits(),
            blended_height_m(&norths, x, z).to_bits()
        );
        assert_eq!(
            blended_surface_m(&grid, x, z),
            blended_height_m(&grid, x, z).max(w.level_m)
        );
    }
    // The sub-texel river keeps its width between texels 293 m apart, where every texel's own
    // ribbon is negative: the ribbon is positive exactly within 11 m of the centreline.
    let texel = grid.inner.spacing_m();
    let mut inside = 0;
    for k in 0..2000 {
        let x = x0 - 40.0 + 0.04 * f64::from(k);
        let z = -50_000.0 + 7.3 * f64::from(k);
        let r = blended_water(&grid, x, z).ribbon_m;
        let exact = 11.0 - (x - x0).abs() as f32;
        assert!(
            (r - exact).abs() < 0.3,
            "({x}, {z}): ribbon {r}, exact {exact}"
        );
        inside += usize::from(r > 0.0);
    }
    assert!(inside > 500);
    let col = ((x0 + e1) / texel).floor() as usize;
    for c in [col, col + 1] {
        assert!(
            decode_river(grid.inner.river[512 * 1025 + c])[0] < -50.0,
            "texel ribbons"
        );
    }
}

/// Two rivers 1.5 km apart, each 22 m wide, converging side by side (ruling 2 of Task 8): the
/// texels west of the divide belong to the west river, on its left bank (offset `+`), those east
/// of it to the east river, on its right bank (offset `−`). Between the last texel of one and the
/// first of the other the interpolated offset passes through 0, where `hw − |o|` alone would
/// rebuild an 11 m ribbon over dry ground. No ribbon may appear between the channels, and each
/// river keeps its true width.
#[test]
fn converging_rivers_leave_no_ribbon_between_them() {
    use crate::worldgen::generate::water::encode_river;
    let mut grid = test_grid();
    let (west, east) = (1_234.5, 2_734.5);
    let divide = (west + east) / 2.0;
    let half_width = 11.0f32;
    let f = &mut grid.inner;
    f.water_level_m = texels_of(f, |_, _| 5_000.0);
    let offsets = texels_of(f, |x, _| {
        if x < divide {
            (x - west) as f32
        } else {
            (x - east) as f32
        }
    });
    f.river = offsets
        .iter()
        .map(|&o| encode_river(half_width - o.abs(), 1.0, 0.0, o))
        .collect();
    let mut between = 0;
    for k in 0..=6_000 {
        let x = west - 40.0 + f64::from(k) * (east - west + 80.0) / 6_000.0;
        let z = -20_000.0 + 3.7 * f64::from(k);
        let r = blended_water(&grid, x, z).ribbon_m;
        let d = (x - west).abs().min((x - east).abs()) as f32;
        let exact = half_width - d;
        if d > half_width + 1.0 {
            between += 1;
            assert!(
                r < 0.0,
                "({x}, {z}): a ribbon of {r} m {d} m from both rivers"
            );
        } else {
            assert!(
                (r - exact).abs() < 0.3,
                "({x}, {z}): ribbon {r}, exact {exact}"
            );
        }
    }
    assert!(between > 5_000);
    // The fixture creates the hazard: an unguarded blend of the two texels astride the divide
    // passes through a zero offset with an 11 m half-width.
    let texel = grid.inner.spacing_m();
    let col = ((divide + REGION_SIZE_M / 2.0) / texel).floor() as usize;
    let (a, b) = (offsets[512 * 1025 + col], offsets[512 * 1025 + col + 1]);
    assert!(a > 400.0 && b < -400.0, "{a}, {b}");
}

/// A lake at 100 m on a sloping shore (Task 8b): lake texels store the level, dry ones
/// `h − NO_WATER_DROP_M`. The wet/dry boundary must follow the ground's own `h = L` contour, as
/// the terrain mesh shows it, not the zero contour of a level dropping across the dry texels.
#[test]
fn a_lake_shore_follows_the_ground_contour() {
    const L: f32 = 100.0;
    let mut grid = test_grid();
    // About 3 m per texel, at an angle to the texel rows.
    let (c, s) = (0.31f64.cos(), 0.31f64.sin());
    let ground = move |x: f64, z: f64| 100.0 + 0.01 * (x * c + z * s);
    for f in [&mut grid.inner, &mut grid.apron] {
        f.elevation_m = texels_of(f, |x, z| ground(x, z) as f32);
        f.water_level_m = f
            .elevation_m
            .iter()
            .map(|&h| if h < L { L } else { h - NO_WATER_DROP_M as f32 })
            .collect();
    }
    // The hazard: the same grid read with the level blended over every tap.
    let mut levels = grid.clone();
    for f in [&mut levels.inner, &mut levels.apron] {
        f.elevation_m = f.water_level_m.clone();
    }
    let texel = grid.inner.spacing_m();
    let (mut worst, mut worst_plain, mut lines) = (0.0f64, 0.0f64, 0);
    for k in 0..40 {
        // Lines across the shore (along the slope), through the inner field.
        let t0 = -30_000.0 + 1_517.3 * f64::from(k);
        let (ox, oz) = (-s * t0, c * t0);
        let along = |u: f64| (ox + c * u, oz + s * u);
        let crossing = |f: &dyn Fn(f64, f64) -> f32| {
            let mut last = None;
            for j in 0..=4000 {
                let u = -2.0 * texel + 4.0 * texel * f64::from(j) / 4000.0;
                let (x, z) = along(u);
                let wet = f(x, z) > 0.0;
                if last == Some(true) && !wet {
                    return u;
                }
                last = Some(wet);
            }
            panic!("no shore on line {k}");
        };
        let shore = crossing(&|x, z| blended_water(&grid, x, z).thickness_m);
        let contour = crossing(&|x, z| L - blended_height_m(&grid, x, z));
        let plain =
            crossing(&|x, z| blended_height_m(&levels, x, z) - blended_height_m(&grid, x, z));
        worst = worst.max((shore - contour).abs() / texel);
        worst_plain = worst_plain.max((plain - contour).abs() / texel);
        lines += 1;
        // The fly's floor stands at the level over the water, on the ground beyond it.
        let (x, z) = along(contour - 0.3 * texel);
        assert!(
            (blended_surface_m(&grid, x, z) - L).abs() < 1e-3,
            "line {k}"
        );
    }
    println!("{lines} lines: shore within {worst:.4} texel; a plain blend {worst_plain:.3}");
    assert!(worst <= 0.05, "the shore is {worst} texel from h = L");
    assert!(
        worst_plain > 0.2,
        "the fixture creates the hazard: {worst_plain}"
    );
}

/// Rivers keep the plain blend (Task 8b, round 2): where a river's texels meet dry banks, the
/// level blends with the banks' `h − NO_WATER_DROP_M` as before, in `blended_water` and in the
/// fly's floor alike, before and after the river texels are freed.
#[test]
fn a_river_beside_dry_banks_blends_as_before() {
    use crate::worldgen::generate::water::encode_river;
    let mut grid = test_grid();
    let x0 = 1_234.5;
    let texel = grid.inner.spacing_m();
    let f = &mut grid.inner;
    let offsets = texels_of(f, |x, _| (x - x0) as f32);
    let half = 1.5 * texel as f32;
    let channel: Vec<bool> = offsets.iter().map(|o| o.abs() < half).collect();
    f.water_level_m = (0..offsets.len())
        .map(|i| {
            let h = f.elevation_m[i];
            if channel[i] {
                h + 2.0
            } else {
                h - NO_WATER_DROP_M as f32
            }
        })
        .collect();
    f.river = (0..offsets.len())
        .map(|i| {
            if channel[i] {
                encode_river(half - offsets[i].abs(), 1.0, 0.0, offsets[i])
            } else {
                crate::worldgen::generate::water::RIVER_NONE
            }
        })
        .collect();
    let mut levels = grid.clone();
    for f in [&mut levels.inner, &mut levels.apron] {
        f.elevation_m = f.water_level_m.clone();
    }
    let points: Vec<(f64, f64)> = (0..400)
        .map(|k| {
            let t = f64::from(k);
            (
                x0 - 3.0 * texel + 6.0 * texel * (t * 0.618_034).fract(),
                -60_000.0 + 297.3 * t,
            )
        })
        .collect();
    let mut banks = 0;
    let surfaces: Vec<f32> = points
        .iter()
        .map(|&(x, z)| {
            let plain = blended_height_m(&levels, x, z);
            let w = blended_water(&grid, x, z);
            assert_eq!(w.level_m.to_bits(), plain.to_bits(), "({x}, {z})");
            let floor = blended_surface_m(&grid, x, z);
            assert_eq!(
                floor,
                blended_height_m(&grid, x, z).max(plain),
                "({x}, {z})"
            );
            // The hazard: a cell with river and dry taps, where flat standing water would stand
            // metres above the plain blend.
            banks += usize::from(plain < blended_height_m(&grid, x, z) - 1.0 && w.ribbon_m > -half);
            floor
        })
        .collect();
    assert!(banks > 20, "{banks} points on a bank cell");
    grid.inner.free_rivers();
    grid.apron.free_rivers();
    for (&(x, z), &floor) in points.iter().zip(&surfaces) {
        assert_eq!(
            blended_surface_m(&grid, x, z),
            floor,
            "({x}, {z}) after freeing"
        );
    }
}

/// A perched river (the Verde north of Phoenix, bug shot 2026-11-27 16:28:29): the river's
/// footprint, the texels within its valley half-width, holds its level 80 m above their ground
/// (the lattice level stands above the fine valley floor), and the dry texels beyond store
/// `h − NO_WATER_DROP_M` and [`RIVER_NONE`](crate::worldgen::generate::water::RIVER_NONE). A
/// dry texel is outside the water: where a footprint texel meets one, no ribbon may appear, so
/// no water strip runs along the footprint's edge parallel to the river.
#[test]
fn a_perched_river_draws_no_water_at_its_footprints_edge() {
    use crate::worldgen::generate::water::{RIVER_NONE, encode_river};
    let mut grid = test_grid();
    let x0 = 1_234.5;
    let texel = grid.inner.spacing_m();
    let half_width = 11.0f32;
    let reach = 5.2 * texel as f32;
    let f = &mut grid.inner;
    let offsets = texels_of(f, |x, _| (x - x0) as f32);
    let footprint: Vec<bool> = offsets.iter().map(|o| o.abs() < reach).collect();
    f.water_level_m = (0..offsets.len())
        .map(|i| {
            let h = f.elevation_m[i];
            if footprint[i] {
                h + 80.0
            } else {
                h - NO_WATER_DROP_M as f32
            }
        })
        .collect();
    f.river = (0..offsets.len())
        .map(|i| {
            if footprint[i] {
                encode_river(half_width - offsets[i].abs(), 1.0, 0.0, offsets[i])
            } else {
                RIVER_NONE
            }
        })
        .collect();
    let (mut edge_cells, mut inside) = (0, 0);
    for k in 0..4_000 {
        let t = f64::from(k);
        let x = x0 - 7.0 * texel + 14.0 * texel * (t * 0.618_034).fract();
        let z = -60_000.0 + 29.3 * t;
        let w = blended_water(&grid, x, z);
        let d = (x - x0).abs() as f32;
        // The hazard: a cell between a footprint texel and a dry one, where the level still
        // stands over the ground.
        edge_cells += usize::from(
            d > reach - texel as f32 && d < reach + texel as f32 && w.thickness_m > 0.0,
        );
        if d > half_width + 1.0 {
            assert!(
                !(w.ribbon_m > 0.0 && w.thickness_m > 0.0),
                "({x}, {z}): water drawn {d} m from the river (ribbon {}, thickness {})",
                w.ribbon_m,
                w.thickness_m
            );
        } else if d < half_width - 1.0 {
            inside += 1;
            assert!(
                w.ribbon_m > 0.0,
                "({x}, {z}): the river itself, ribbon {}",
                w.ribbon_m
            );
        }
    }
    assert!(
        edge_cells > 50,
        "the fixture creates the hazard: {edge_cells}"
    );
    assert!(inside > 2, "points on the river: {inside}");
}

#[test]
fn flight_floor_follows_the_drawn_perched_river_before_and_after_upload() {
    use crate::worldgen::generate::water::encode_river;
    let mut grid = test_grid();
    let field = &mut grid.inner;
    field.water_level_m = field.elevation_m.iter().map(|h| h + 80.0).collect();
    field.river = texels_of(field, |x, _| {
        let offset = x.clamp(-1000.0, 1000.0) as f32;
        encode_river(11.0 - offset.abs(), 1.0, 0.0, offset)
    });
    for uploaded in [false, true] {
        if uploaded {
            grid.inner.free_rivers();
        }
        for (x, rise) in [(0.0, 80.0), (50.0, 0.0)] {
            let ground = blended_height_m(&grid, x, 0.0);
            assert!((blended_surface_m(&grid, x, 0.0) - ground - rise).abs() < 0.01);
        }
    }
}

/// The crossing guard's fallback (two rivers, or a sharp bend, in one cell) interpolates the
/// taps' ribbons; a dry tap among them is outside the water, not [`RIBBON_NONE_M`] of it.
#[test]
fn the_crossing_guard_keeps_dry_taps_dry() {
    use crate::hydrology::params::RIBBON_NONE_M;
    use crate::worldgen::generate::water::{RIVER_NONE, encode_river};
    let spacing = 293.0;
    let river = |o: f32| TapTexel::new(0.0, 80.0, encode_river(11.0 - o.abs(), 1.0, 0.0, o));
    let dry = TapTexel::new(0.0, -(NO_WATER_DROP_M as f32), RIVER_NONE);
    // Offsets of both signs 600 m apart: more than CROSSING_REACH_TEXELS texels.
    let taps = [river(-300.0), river(300.0), river(-300.0), dry];
    // The hazard: the dry tap's own ribbon is RIBBON_NONE_M.
    assert_eq!(dry.river[0], RIBBON_NONE_M as f32);
    for (fu, fv) in [(0.9f32, 0.9f32), (0.7, 0.8), (0.5, 0.95)] {
        let w = water_from_taps(&taps, fu, fv, spacing);
        assert!(
            !(w.ribbon_m > 0.0 && w.thickness_m > 0.0),
            "({fu}, {fv}): ribbon {} thickness {}",
            w.ribbon_m,
            w.thickness_m
        );
    }
}

/// Once a grid's river texels are on the GPU the shell frees them (`Field::free_rivers`): the
/// floor and water geometry survive, while the CPU flow vectors are released.
#[test]
fn a_grid_without_its_rivers_keeps_its_levels() {
    use crate::worldgen::generate::water::encode_river;
    let mut grid = test_grid();
    for f in [&mut grid.inner, &mut grid.apron] {
        let n = f.river.len();
        f.water_level_m = f.elevation_m.iter().map(|h| h + 2.0).collect();
        f.river = (0..n)
            .map(|i| encode_river(3.0, 1.0, -0.5, (i % 7) as f32))
            .collect();
    }
    let before = grid.bytes();
    let points = [
        (0.0, 0.0),
        (12_345.0, -67_890.0),
        (140_000.0, 1.0),
        (-9.0e5, 4.0e5),
    ];
    let kept: Vec<_> = points
        .iter()
        .map(|&(x, z)| (blended_surface_m(&grid, x, z), blended_water(&grid, x, z)))
        .collect();
    let freed = grid.inner.river.len() * 8 + grid.apron.river.len() * 8;
    grid.inner.free_rivers();
    grid.apron.free_rivers();
    assert!(grid.inner.river.is_empty() && grid.apron.river.is_empty());
    // The footprint and packed lookup survive the upload.
    let mask = 8 * (grid.inner.river_mask.len() + grid.apron.river_mask.len());
    let packed: usize = [&grid.inner, &grid.apron]
        .iter()
        .map(|f| 4 * (f.river_starts.capacity() + f.river_geometry.capacity()))
        .sum();
    assert_eq!(before - grid.bytes(), freed - mask - packed);
    for (&(x, z), (surface, water)) in points.iter().zip(&kept) {
        assert_eq!(blended_surface_m(&grid, x, z), *surface, "({x}, {z})");
        let w = blended_water(&grid, x, z);
        assert_eq!(
            (w.level_m, w.thickness_m),
            (water.level_m, water.thickness_m)
        );
        assert_eq!(w.flow, [0.0, 0.0], "({x}, {z})");
        assert_eq!(w.ribbon_m, water.ribbon_m);
    }
}

#[test]
fn curvature_drop_is_the_sagitta() {
    assert_eq!(curvature_drop_m(0.0), 0.0);
    assert_eq!(curvature_drop_m(-0.0), 0.0);
    let d = 100_000.0;
    assert_eq!(curvature_drop_m(d), d * d / (2.0 * 6_371_000.0));
    assert!((curvature_drop_m(d) - 784.806_153_6).abs() < 1e-6);
    assert_eq!(curvature_drop_m(-d), curvature_drop_m(d));
}

fn flat_grid(f: fn(f64, f64) -> f64) -> RegionGrid {
    RegionGrid {
        spec: RegionSpec::new(RenderDir(DVec3::Y), YearsBp(0.0), 1),
        inner: field(REGION_RES, REGION_SIZE_M, f),
        apron: field(APRON_RES, APRON_SIZE_M, f),
        boulders: Vec::new(),
    }
}

fn tilt_deg(n: [f32; 3]) -> f64 {
    f64::from(n[1]).clamp(-1.0, 1.0).acos().to_degrees()
}

#[test]
fn surface_normal_tilts_with_the_curvature_drop() {
    let r = 6_371_000.0;
    let flat = flat_grid(|_, _| 500.0);
    let camera = [12_000.0, -3_000.0];
    // Under the camera the flat field faces straight up.
    let n = surface_normal(&flat, camera, camera[0], camera[1]);
    assert!(
        n[0].abs() < 1e-6 && n[2].abs() < 1e-6 && (n[1] - 1.0).abs() < 1e-6,
        "{n:?}"
    );
    // Away from the camera it tilts away from it by atan(d / R): about 9 degrees at 1,000 km.
    for (dx, dz) in [
        (1_000_000.0, 0.0),
        (0.0, -1_000_000.0),
        (100_000.0, 60_000.0),
        (-40_000.0, 0.0),
    ] {
        let n = surface_normal(&flat, camera, camera[0] + dx, camera[1] + dz);
        let d: f64 = f64::hypot(dx, dz);
        let want = (d / r).atan().to_degrees();
        assert!(
            (tilt_deg(n) - want).abs() < 1e-3,
            "({dx}, {dz}): {} vs {want}",
            tilt_deg(n)
        );
        // Outward: the horizontal part points from the camera to the point.
        let out = (f64::from(n[0]) * dx + f64::from(n[2]) * dz) / d;
        assert!(out > 0.0, "({dx}, {dz}): {n:?}");
        let len = n.iter().map(|c| c * c).sum::<f32>();
        assert!((len - 1.0).abs() < 1e-5);
    }
    let far = surface_normal(&flat, [0.0, 0.0], 1_000_000.0, 0.0);
    assert!((tilt_deg(far) - 8.93).abs() < 0.01, "{}", tilt_deg(far));

    // A plane rising east at 10 %: the slope and the drop add, along x only.
    let ramp = flat_grid(|x, _| 0.1 * x);
    let at = surface_normal(&ramp, [0.0, 0.0], 0.0, 0.0);
    assert!(
        (at[0] / at[1] + 0.1).abs() < 1e-4 && at[2].abs() < 1e-6,
        "{at:?}"
    );
    let x = 60_000.0;
    let n = surface_normal(&ramp, [0.0, 0.0], x, 0.0);
    let want = -0.1 + x / r;
    assert!(
        (f64::from(n[0] / n[1]) - want).abs() < 1e-4,
        "{n:?} vs {want}"
    );
    // In the apron the step is an apron texel; the same plane gives the same slope.
    let n = surface_normal(&ramp, [0.0, 0.0], 400_000.0, 0.0);
    let want = -0.1 + 400_000.0 / r;
    assert!(
        (f64::from(n[0] / n[1]) - want).abs() < 1e-4,
        "{n:?} vs {want}"
    );
}
