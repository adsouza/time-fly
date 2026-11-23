use super::*;
use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
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
    Field {
        res,
        size_m,
        elevation_m,
        surface: vec![[0; 4]; n],
    }
}

fn test_grid() -> RegionGrid {
    RegionGrid {
        spec: RegionSpec::new(RenderDir(DVec3::Y), YearsBp(0.0), 1),
        inner: field(REGION_RES, REGION_SIZE_M, inner_fn),
        apron: field(APRON_RES, APRON_SIZE_M, apron_fn),
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

#[test]
fn curvature_drop_is_the_sagitta() {
    assert_eq!(curvature_drop_m(0.0), 0.0);
    assert_eq!(curvature_drop_m(-0.0), 0.0);
    let d = 100_000.0;
    assert_eq!(curvature_drop_m(d), d * d / (2.0 * 6_371_000.0));
    assert!((curvature_drop_m(d) - 784.806_153_6).abs() < 1e-6);
    assert_eq!(curvature_drop_m(-d), curvature_drop_m(d));
}
