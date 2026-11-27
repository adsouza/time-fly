use super::*;
use crate::cube::{CubeFace, texel_center_dir};
use crate::formats::Grid;
use crate::geodynamics::{CrustModel, PlateIndex};
use crate::globe::{PlateRotations, plate_rotations, rendered_surface};
use crate::surface::FrameSelection;
use crate::worldgen::coarse::{DeepField, RecentField, SnapshotField, snapshot_field};
use crate::worldgen::testkit::*;
use crate::worldgen::tiles::TileSet;
use bevy_math::DVec3;
use proptest::prelude::*;

fn spec_at(centre: RenderDir, ma: f64, inner_res: u32, apron_res: u32) -> RegionSpec {
    RegionSpec {
        center: centre,
        snapshot: YearsBp::from_myr(ma),
        seed: 7,
        inner_res,
        apron_res,
        sea_level_anomaly_m: 0.0,
    }
}

fn generate<F: CoarseField>(spec: RegionSpec, field: &F) -> RegionGrid {
    RegionGenerator::new(spec, field, None).finish()
}

/// A grid as comparable bits: (inner heights, inner surface, apron heights, apron surface).
type GridBits = (Vec<u32>, Vec<[u8; 4]>, Vec<u32>, Vec<[u8; 4]>);

fn bits(grid: &RegionGrid) -> GridBits {
    let b = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    (
        b(&grid.inner.elevation_m),
        grid.inner.surface.clone(),
        b(&grid.apron.elevation_m),
        grid.apron.surface.clone(),
    )
}

/// Rolling terrain from sea floor to mountains, so every branch of the generator runs.
fn hilly(d: RenderDir) -> f64 {
    let p = d.0 * 40.0;
    2200.0 * (p.x.sin() * p.y.cos() + 0.4 * p.z.sin()) - 300.0
}

fn assert_finite(grid: &RegionGrid, label: &str) {
    for f in [&grid.inner, &grid.apron] {
        assert!(
            f.elevation_m.iter().all(|h| h.is_finite()),
            "{label}: non-finite height"
        );
        assert!(
            f.surface.iter().all(|s| s[0] < 12 && s[1] < 6),
            "{label}: bad surface code"
        );
        assert_eq!(f.elevation_m.len(), (f.res * f.res) as usize);
    }
}

#[test]
fn coastline_clamp_rule() {
    assert_eq!(coastline_clamp(-50.0, [10.0, 20.0, 5.0, 1.0]), 1.0);
    assert_eq!(coastline_clamp(300.0, [10.0, 20.0, 5.0, 1.0]), 300.0);
    assert_eq!(coastline_clamp(50.0, [-10.0, -20.0, -5.0, -1.0]), -1.0);
    assert_eq!(coastline_clamp(-300.0, [-10.0, -20.0, -5.0, -1.0]), -300.0);
    assert_eq!(coastline_clamp(50.0, [-10.0, 20.0, -5.0, -1.0]), 50.0);
    assert_eq!(
        coastline_clamp(-50.0, [10.0, 0.0, 5.0, 1.0]),
        -50.0,
        "0 is not land"
    );
}

proptest! {
    #[test]
    fn coastline_clamp_keeps_the_corners_side(
        h in -9000.0f64..9000.0,
        corners in prop::array::uniform4(-9000.0f64..9000.0),
    ) {
        let out = coastline_clamp(h, corners);
        if corners.iter().all(|&c| c > 0.0) {
            prop_assert!(out >= 1.0 && out >= h);
        } else if corners.iter().all(|&c| c < 0.0) {
            prop_assert!(out <= -1.0 && out <= h);
        } else {
            prop_assert_eq!(out, h);
        }
        // Only ever moves h towards the corners' side, and only as far as ±1 m.
        prop_assert!(out == h || out.abs() == 1.0);
    }
}

#[test]
fn detail_never_lifts_open_ocean() {
    // Deep-time spacing gives the most octaves and the biggest amplitude.
    for depth in [-2.0, -60.0, -4000.0] {
        let field = FnField {
            f: |d: RenderDir| depth - 300.0 * (d.0.x * 50.0).sin().abs(),
            spacing_m: 111_000.0,
            koppen: None,
        };
        let grid = generate(spec_at(dir(-20.0, -140.0), 100.0, 65, 33), &field);
        for f in [&grid.inner, &grid.apron] {
            let top = f.elevation_m.iter().cloned().fold(f32::MIN, f32::max);
            assert!(top <= -1.0, "depth {depth}: {top}");
        }
    }
}

#[test]
fn determinism() {
    let field = FnField {
        f: hilly,
        spacing_m: 111_000.0,
        koppen: None,
    };
    let spec = spec_at(dir(12.0, 34.0), 66.0, 33, 17);
    assert_eq!(bits(&generate(spec, &field)), bits(&generate(spec, &field)));
    let other = RegionSpec { seed: 8, ..spec };
    assert_ne!(
        bits(&generate(spec, &field)).0,
        bits(&generate(other, &field)).0,
        "the seed changes the detail"
    );
}

#[test]
fn chunking_is_invisible() {
    let field = FnField {
        f: hilly,
        spacing_m: 111_000.0,
        koppen: None,
    };
    let spec = spec_at(dir(-40.0, 100.0), 150.0, 33, 17);
    let whole = bits(&generate(spec, &field));
    for chunk in [1, 7, 64] {
        let mut g = RegionGenerator::new(spec, &field, None);
        assert_eq!(g.rows_remaining(), 33 + 17);
        let mut calls = 0;
        while g.rows_remaining() > 0 {
            g.generate_rows(chunk);
            calls += 1;
        }
        assert_eq!(calls, 50u32.div_ceil(chunk));
        assert_eq!(bits(&g.finish()), whole, "chunks of {chunk}");
    }
    // Blocks produced out of order (as parallel tasks would) and assembled in order.
    let g = RegionGenerator::new(spec, &field, None);
    let late = g.rows(FieldKind::Inner, 20..33);
    let early = g.rows(FieldKind::Inner, 0..20);
    let apron = vec![
        g.rows(FieldKind::Apron, 0..5),
        g.rows(FieldKind::Apron, 5..17),
    ];
    assert_eq!(bits(&g.assemble(vec![early, late], apron)), whole);
}

#[test]
fn generator_is_sync_and_send() {
    fn sync<T: Sync>() {}
    fn send<T: Send>() {}
    type Deep<'a> = RegionGenerator<'a, DeepField<'a>>;
    type Snap<'a> = RegionGenerator<'a, SnapshotField<'a>>;
    sync::<Deep<'static>>();
    sync::<Snap<'static>>();
    send::<Snap<'static>>();
}

#[test]
fn overlap_agreement() {
    let a = dir(30.0, 10.0);
    // About 100 km east.
    let b = RegionFrame { center: a }.local_to_dir(100_000.0, 0.0);
    let probe = RegionFrame { center: a }.local_to_dir(50_000.0, -20_000.0);
    // Recent: everything, biome included, is a function of the direction.
    let mut tiles = flat_tiles_around(&[a, b], 350.0, 26);
    tiles.set_overview(flat_overview(340.0, 26)).unwrap();
    let (era, present) = (ice_frame(380.0, 0.2), ice_frame(350.0, 0.0));
    let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
    let ga = RegionGenerator::new(spec_at(a, 0.021, 33, 17), &recent, None);
    let gb = RegionGenerator::new(spec_at(b, 0.021, 33, 17), &recent, None);
    assert_eq!(ga.sample_point(probe), gb.sample_point(probe));
    // Apron texels too (from the overview), even far apart and with different tiles loaded.
    let only_a = {
        let mut t = flat_tiles(a, 350.0, 26);
        t.set_overview(flat_overview(340.0, 26)).unwrap();
        t
    };
    let recent_a = RecentField::new(&only_a, [&era, &era], 0.0, &present, false).unwrap();
    let ga = RegionGenerator::new(spec_at(a, 0.021, 33, 17), &recent_a, None);
    let far = RegionFrame { center: a }.local_to_dir(900_000.0, 300_000.0);
    assert_eq!(
        ga.sample_point_in(FieldKind::Apron, far),
        gb.sample_point_in(FieldKind::Apron, far)
    );
    // Deep: heights and every byte but the biome (the zonal rule's coast distance comes from
    // each region's own apron window).
    let field = FnField {
        f: hilly,
        spacing_m: 111_000.0,
        koppen: None,
    };
    let ga = RegionGenerator::new(spec_at(a, 100.0, 33, 17), &field, None);
    let gb = RegionGenerator::new(spec_at(b, 100.0, 33, 17), &field, None);
    for k in 0..20 {
        let p = RegionFrame { center: a }.local_to_dir(f64::from(k) * 6_000.0, -30_000.0);
        let (ha, sa) = ga.sample_point(p);
        let (hb, sb) = gb.sample_point(p);
        assert_eq!(ha.to_bits(), hb.to_bits(), "probe {k}");
        assert_eq!(sa[1..], sb[1..]);
    }
    // Deep and ETOPO-dominant (the two-level path at 1 Ma), with every tile loaded: heights and
    // all bytes but the biome agree, inner and apron.
    {
        let crust = rift(0);
        let frames = rift_frames(|d| (hilly(d) as f32, true));
        let mut tiles = TileSet::new();
        for r in 15..21 {
            for c in 12..24 {
                tiles
                    .insert(
                        r,
                        c,
                        crate::worldgen::tiles::tests::tile_from(r, c, |lat, lon| {
                            (
                                (200.0 + 150.0 * (lat * 3.0).sin() * (lon * 2.0).cos()) as f32,
                                3,
                            )
                        }),
                    )
                    .unwrap();
            }
        }
        tiles.set_overview(flat_overview(250.0, 3)).unwrap();
        let field = rift_deep(&crust, &frames, &tiles, 1.0);
        let c = dir(0.0, -88.0);
        let d = RegionFrame { center: c }.local_to_dir(100_000.0, 0.0);
        let ga = RegionGenerator::new(spec_at(c, 1.0, 33, 17), &field, None);
        let gb = RegionGenerator::new(spec_at(d, 1.0, 33, 17), &field, None);
        for k in 0..20 {
            let p = RegionFrame { center: c }.local_to_dir(f64::from(k) * 6_000.0, 30_000.0);
            for kind in [FieldKind::Inner, FieldKind::Apron] {
                let (ha, sa) = ga.sample_point_in(kind, p);
                let (hb, sb) = gb.sample_point_in(kind, p);
                assert_eq!(ha.to_bits(), hb.to_bits(), "1 Ma {kind:?} probe {k}");
                assert_same_surface_but_dryness(sa, sb);
            }
        }
    }
    // Grid texels are exactly sample_point at their own directions.
    let grid = generate(spec_at(a, 100.0, 33, 17), &field);
    let frame = RegionFrame { center: a };
    for row in 0..33 {
        for col in 0..33 {
            let (x, z) = grid.inner.local_of(row, col);
            let i = (row * 33 + col) as usize;
            let (h, s) = ga.sample_point(frame.local_to_dir(x, z));
            assert_eq!(grid.inner.elevation_m[i].to_bits(), h.to_bits());
            assert_eq!(grid.inner.surface[i], s);
        }
    }
}

/// The rift at 100 Ma, frames of 500 m land everywhere, all covered: land exactly where plates
/// are, the ocean shell in the gap.
fn rift_deep<'a>(
    crust: &'a CrustModel,
    frames: &'a [Grid],
    tiles: &'a TileSet,
    age: f64,
) -> SnapshotField<'a> {
    snapshot_field(&timeline(), crust, tiles, false, age, |i| &frames[i]).unwrap()
}

fn rift_frames(paleo: impl Fn(RenderDir) -> (f32, bool)) -> Vec<Grid> {
    (0..7)
        .map(|i| {
            if i < 4 {
                ice_frame(0.0, 0.0)
            } else {
                paleo_frame(&paleo)
            }
        })
        .collect()
}

#[test]
fn deep_matches_globe() {
    let crust = rift(0);
    let frames = rift_frames(|_| (500.0, true));
    let tiles = TileSet::new();
    let field = rift_deep(&crust, &frames, &tiles, 100.0);
    let SnapshotField::Deep(deep) = &field else {
        panic!()
    };
    let sampling = *deep.sampling();
    let rotations: Vec<PlateRotations> = (0..2)
        .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
        .collect();
    // The globe's land, plus the thin era-fixed cracks within 50 km of plate land where a
    // region shows the frames' own land instead of the shell's crack fill (`sliver_land_m`).
    let slivers = std::cell::Cell::new(0);
    let globe_land = |d: RenderDir| {
        let land = rendered_surface(&crust, d, &sampling, &rotations, |_, _| {
            crate::globe::FrameSample {
                elevation_m: 500.0,
                covered: true,
            }
        })
        .is_land();
        if !land && deep.sliver_land_m(d).is_some() {
            slivers.set(slivers.get() + 1);
            return true;
        }
        land
    };
    // Straddling plate 202's western edge (render z ~ 0.3).
    let spec = spec_at(dir(0.0, 72.0), 100.0, 33, 33);
    let g = RegionGenerator::new(spec, &field, None);
    // Coarse truth: every precomputed node has the globe's land/sea sign.
    let mut nodes = 0;
    for lattice in &g.lattices {
        for (f, block) in lattice.blocks.iter().enumerate() {
            let Some(b) = block else { continue };
            for r in 0..b.h {
                for c in 0..b.w {
                    let node = b.nodes[(r * b.w + c) as usize];
                    assert!(lattice.kinds.len() <= 4);
                    let d = texel_center_dir(CubeFace::ALL[f], b.c0 + c, b.r0 + r, lattice.n);
                    assert_eq!(node.elevation_m > 0.0, globe_land(d), "node {d:?}");
                    nodes += 1;
                }
            }
        }
    }
    assert!(nodes > 100);
    // Texels: wherever the four corners agree, the generated sign is the globe's.
    let grid = g.finish();
    let frame = spec.frame();
    let g = RegionGenerator::new(spec, &field, None);
    let (mut agreed, mut land, mut sea) = (0, 0, 0);
    for (kind, f) in [
        (FieldKind::Inner, &grid.inner),
        (FieldKind::Apron, &grid.apron),
    ] {
        for row in 0..f.res {
            for col in 0..f.res {
                let (x, z) = f.local_of(row, col);
                let d = frame.local_to_dir(x, z);
                let (_, corners) = g.coarse_and_corners(kind, d);
                let h = f.elevation_m[(row * f.res + col) as usize];
                if corners.iter().all(|&c| c > 0.0) || corners.iter().all(|&c| c < 0.0) {
                    agreed += 1;
                    assert_eq!(h > 0.0, corners[0] > 0.0, "{kind:?} ({row},{col})");
                    assert_eq!(h > 0.0, globe_land(d), "{kind:?} ({row},{col}) h {h}");
                }
                if h > 0.0 {
                    land += 1;
                } else {
                    sea += 1;
                }
            }
        }
    }
    assert!(
        agreed > 1000 && land > 100 && sea > 100,
        "{agreed} {land} {sea}"
    );
    // The fixture's gap edge is such a crack along its first 50 km.
    assert!(slivers.get() > 0);
}

#[test]
fn thin_era_fixed_cracks_stay_land() {
    // The 10 Ma Task 8 shot's straight channels: lattice nodes landing in a thin crack between
    // plates read the globe's ocean shell, whose covered land is the -200 m crack fill, so rows
    // of texels dipped below sea level across dry land.
    let frames = rift_frames(|_| (400.0, true));
    let tiles = TileSet::new();
    for (gap_deg, label) in [(0.4f32, "thin"), (4.0, "wide")] {
        let crust = sliver(gap_deg);
        let field = rift_deep(&crust, &frames, &tiles, 100.0);
        let SnapshotField::Deep(deep) = &field else {
            panic!()
        };
        let sampling = *deep.sampling();
        let rotations: Vec<PlateRotations> = (0..2)
            .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let mid = crack_dir(0.0, -f64::from(gap_deg) / 2.0);
        let globe = rendered_surface(&crust, mid, &sampling, &rotations, |_, _| {
            crate::globe::FrameSample {
                elevation_m: 400.0,
                covered: true,
            }
        });
        assert_eq!(
            globe,
            crate::globe::RenderedSurface::Ocean,
            "{label}: the globe's crack"
        );
        let grid = generate(spec_at(mid, 100.0, 65, 33), &field);
        let lowest = |f: &Field| f.elevation_m.iter().copied().fold(f32::MAX, f32::min);
        let g = RegionGenerator::new(spec_at(mid, 100.0, 65, 33), &field, None);
        if label == "thin" {
            // 44 km wide: every crack point is within 50 km of plate land.
            assert_eq!(deep.sliver_land_m(mid), Some(400.0));
            assert!(
                lowest(&grid.inner) > 0.0,
                "inner dips to {}",
                lowest(&grid.inner)
            );
            assert!(
                lowest(&grid.apron) > 0.0,
                "apron dips to {}",
                lowest(&grid.apron)
            );
        } else {
            // 445 km wide: its middle is a real gap and keeps the globe's water.
            assert_eq!(deep.sliver_land_m(mid), None);
            assert!(g.sample_point(mid).0 < 0.0);
            let edge = crack_dir(0.0, -0.1);
            assert!(deep.sliver_land_m(edge).is_some(), "the gap's first 50 km");
        }
    }
}

#[test]
fn detail_has_no_step_at_plate_boundaries() {
    // Two plates meeting with no gap, over flat 400 m land: only the detail's seed changes
    // across the boundary. Each key's detail is independent noise, so without blending the
    // boundary was a step of hundreds of metres along a straight footprint edge: the dark lines
    // of the 10 Ma Task 8 shot.
    let crust = sliver(0.0);
    let frames = rift_frames(|_| (400.0, true));
    let tiles = TileSet::new();
    let field = rift_deep(&crust, &frames, &tiles, 100.0);
    let grid = generate(spec_at(crack_dir(0.0, 0.0), 100.0, 257, 9), &field);
    let f = &grid.inner;
    let n = f.res as usize;
    let mut steps: Vec<f32> = Vec::new();
    for r in 0..n {
        for c in 0..n - 1 {
            steps.push((f.elevation_m[r * n + c + 1] - f.elevation_m[r * n + c]).abs());
        }
    }
    for r in 0..n - 1 {
        for c in 0..n {
            steps.push((f.elevation_m[(r + 1) * n + c] - f.elevation_m[r * n + c]).abs());
        }
    }
    steps.sort_by(f32::total_cmp);
    let p99 = steps[steps.len() * 99 / 100];
    let worst = *steps.last().unwrap();
    println!("neighbour steps: p99 {p99:.1} m, worst {worst:.1} m");
    assert!(
        worst < 3.0 * p99,
        "a seam: worst step {worst} m vs p99 {p99} m"
    );
}

#[test]
fn recent_adds_ice_delta() {
    let centre = dir(62.0, -45.0);
    let mut tiles = flat_tiles(centre, 100.0, 29);
    tiles.set_overview(flat_overview(100.0, 29)).unwrap();
    let (era, present) = (ice_frame(2100.0, 1.0), ice_frame(100.0, 0.0));
    let field = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
    let grid = generate(spec_at(centre, 0.021, 33, 17), &field);
    for (h, s) in grid.inner.elevation_m.iter().zip(&grid.inner.surface) {
        assert!((h - 2100.0).abs() < 150.0, "{h}");
        assert_eq!(s[2], 255, "ice code");
    }
    // A -100 m shelf with the delta +120: +20 m land, clamped to stay land.
    let mut tiles = flat_tiles(centre, -100.0, 0);
    tiles.set_overview(flat_overview(-100.0, 0)).unwrap();
    let (era, present) = (ice_frame(20.0, 0.0), ice_frame(-100.0, 0.0));
    let field = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
    let grid = generate(spec_at(centre, 0.021, 33, 17), &field);
    for (h, s) in grid.inner.elevation_m.iter().zip(&grid.inner.surface) {
        assert!(*h >= 1.0 && (h - 20.0).abs() < 10.0, "{h}");
        assert_eq!(
            s[3] & SURFACE_SHORE,
            SURFACE_SHORE * u8::from((*h as f64) <= SHORE_BAND_M)
        );
    }
}

#[test]
fn glacial_eras_lower_the_snowline() {
    // Ruling R18: 3,200 m ground at the Alps' latitude is below today's 3,645 m snowline and
    // above the Last Glacial Maximum's 2,645 m one (sea 120 m lower). A sea 220 km out keeps
    // the dryness raise (ruling R27) at zero: near a coast, humid class.
    let centre = dir(46.5, 10.0);
    let field = FnField {
        f: |d: RenderDir| {
            if d.0.dot(centre.0) > (2.0f64).to_radians().cos() {
                3200.0
            } else {
                -1000.0
            }
        },
        spacing_m: 1000.0,
        koppen: Some(26), // Dfb: temperate broadleaf, so only the snowline decides the ice
    };
    let today = generate(spec_at(centre, 0.021, 33, 17), &field);
    assert!(
        today.inner.surface.iter().all(|s| s[2] == 0),
        "no ice today"
    );
    let lgm = RegionSpec {
        sea_level_anomaly_m: -120.0,
        ..spec_at(centre, 0.021, 33, 17)
    };
    let glacial = generate(lgm, &field);
    assert!(
        glacial.inner.surface.iter().all(|s| s[2] == 255),
        "snow at the LGM"
    );
    // The biome is the data's either way; only the ice byte changes.
    assert!(
        glacial
            .inner
            .surface
            .iter()
            .all(|s| s[0] == BiomeClass::TemperateBroadleaf as u8)
    );
    // The zonal rule (no Köppen class) turns the ground alpine under the lowered snowline.
    let zonal = FnField {
        koppen: None,
        ..field
    };
    let glacial = generate(lgm, &zonal);
    assert!(
        glacial
            .inner
            .surface
            .iter()
            .all(|s| s[0] == BiomeClass::Alpine as u8)
    );
}

#[test]
fn regions_at_awkward_places_are_finite() {
    let crust = rift(0);
    let frames = rift_frames(|d| (hilly(d) as f32, d.0.x > -0.6));
    let tiles = TileSet::new();
    let deep = rift_deep(&crust, &frames, &tiles, 100.0);
    let places = [
        ("antimeridian east", dir(0.0, 179.9)),
        ("antimeridian west", dir(0.0, -179.9)),
        ("near the north pole", dir(89.5, 10.0)),
        ("near the south pole", dir(-89.5, -170.0)),
        ("north pole", dir(90.0, 0.0)),
        ("open ocean (rift gap)", dir(0.0, -90.0)),
        ("plate boundary", dir(-5.0, 72.0)),
        ("cube corner", RenderDir(DVec3::ONE.normalize())),
    ];
    for (label, centre) in places {
        assert_finite(&generate(spec_at(centre, 100.0, 33, 17), &deep), label);
    }
    // Recent eras across the antimeridian (with tiles) and at a pole (no tiles: cube fallback).
    let centre = dir(-12.0, 179.9);
    let mut tiles = flat_tiles(centre, 40.0, 1);
    tiles.set_overview(flat_overview(40.0, 1)).unwrap();
    let (era, present) = (ice_frame(80.0, 0.0), ice_frame(40.0, 0.0));
    let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
    let grid = generate(spec_at(centre, 0.021, 33, 17), &recent);
    assert_finite(&grid, "recent antimeridian");
    assert!(grid.inner.elevation_m.iter().all(|&h| h >= 1.0), "all land");
    let empty = TileSet::new();
    let polar = RecentField::new(&empty, [&era, &era], 0.0, &present, true).unwrap();
    assert_finite(
        &generate(spec_at(dir(89.5, 0.0), 0.021, 33, 17), &polar),
        "recent pole",
    );
}

#[test]
fn vegetation_follows_age() {
    let field = FnField {
        f: hilly,
        spacing_m: 111_000.0,
        koppen: None,
    };
    for ma in [0.0, 25.0, 200.0, 350.0, 420.0, 500.0] {
        let spec = spec_at(dir(5.0, 5.0), ma, 17, 9);
        let era = vegetation_era(spec.snapshot) as u8;
        let grid = generate(spec, &field);
        for f in [&grid.inner, &grid.apron] {
            assert!(f.surface.iter().all(|s| s[1] == era), "{ma} Ma");
        }
        // Grasslands only in the last 20 Myr.
        if ma > 20.0 {
            let grass = [BiomeClass::Steppe as u8, BiomeClass::MonsoonSavanna as u8];
            assert!(grid.inner.surface.iter().all(|s| !grass.contains(&s[0])));
        }
    }
}

#[test]
fn mountains_have_more_detail_than_plains() {
    let rms_and_mean = |elevation: f64| {
        let field = FnField {
            f: move |_| elevation,
            spacing_m: 111_000.0,
            koppen: None,
        };
        let grid = generate(spec_at(dir(40.0, -100.0), 100.0, 65, 9), &field);
        let n = grid.inner.elevation_m.len() as f64;
        let d: Vec<f64> = grid
            .inner
            .elevation_m
            .iter()
            .map(|&h| f64::from(h) - elevation)
            .collect();
        let mean = d.iter().sum::<f64>() / n;
        let rms = (d.iter().map(|x| x * x).sum::<f64>() / n).sqrt();
        (rms, mean)
    };
    let (plains, _) = rms_and_mean(150.0);
    let (mountains, mean) = rms_and_mean(2500.0);
    assert!(mountains > 3.0 * plains, "{mountains} vs {plains}");
    assert!(plains > 1.0, "plains get some detail: {plains}");
    // Ridged detail is centred, so it does not shift the coarse trend much.
    assert!(mean.abs() < 0.5 * mountains, "mean {mean} rms {mountains}");
}

#[test]
fn octaves_stop_at_the_source_spacing() {
    // Recent data at 1.85 km: the apron (4.7 km texels) adds no detail at all.
    let field = FnField {
        f: |_| 400.0,
        spacing_m: 1852.0,
        koppen: Some(26),
    };
    let g = RegionGenerator::new(spec_at(dir(45.0, 7.0), 0.0, 1025, 513), &field, None);
    assert_eq!(
        g.layers[0].octaves, 2,
        "1852 m down to 586 m: cells 1852 and 926 m"
    );
    assert_eq!(g.layers[1].octaves, 0);
    let apron = g.rows(FieldKind::Apron, 250..252);
    assert!(apron.elevation_m.iter().all(|&h| h == 400.0));
    let inner = g.rows(FieldKind::Inner, 500..501);
    assert!(inner.elevation_m.iter().any(|&h| h != 400.0));
}

#[test]
fn coast_distance_grows_inland() {
    // Land east of the centre's meridian.
    let centre = dir(0.0, 30.0);
    let field = FnField {
        f: |d: RenderDir| if latlon(d).1 > 30.0 { 300.0 } else { -300.0 },
        spacing_m: 111_000.0,
        koppen: None,
    };
    let g = RegionGenerator::new(spec_at(centre, 100.0, 33, 129), &field, None);
    let frame = RegionFrame { center: centre };
    for km in [200.0, 500.0, 900.0] {
        let d = g.coast_km_at(frame.local_to_dir(km * 1000.0, 0.0));
        assert!((f64::from(d) - km).abs() < 40.0, "{km}: {d}");
    }
    assert_eq!(g.coast_km_at(frame.local_to_dir(-300_000.0, 0.0)), 0.0);
    assert_eq!(
        g.coast_km_at(frame.local_to_dir(1_500_000.0, 0.0)),
        COAST_UNKNOWN_KM
    );
}

#[test]
fn height_at_is_bilinear() {
    let field = Field::dry(
        3,
        200.0,
        vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0],
        vec![[0; 4]; 9],
    );
    assert_eq!(field.height_at(-100.0, -100.0), Some(0.0));
    assert_eq!(field.height_at(100.0, 100.0), Some(80.0));
    assert_eq!(field.height_at(-50.0, 0.0), Some(35.0));
    assert_eq!(field.height_at(101.0, 0.0), None);
    assert_eq!(field.local_of(2, 1), (0.0, 100.0));
}

#[test]
fn snapshot_selection_drives_the_field() {
    // 50 ka is a deep bracket between the bridge frame and 4.47 Ma (ruling R5).
    let s = timeline();
    assert!(matches!(s.select_frames(0.05), FrameSelection::Deep(_)));
    assert!(matches!(s.select_frames(0.021), FrameSelection::Recent(_)));
    assert!(matches!(
        s.select_frames(0.028),
        FrameSelection::Bridge { .. }
    ));
}

#[test]
fn prep_in_steps_matches_new() {
    // One lattice shared by both fields (deep spacing) and two lattices (recent spacing).
    for spacing_m in [111_000.0, 1852.0] {
        let field = FnField {
            f: hilly,
            spacing_m,
            koppen: None,
        };
        let spec = spec_at(dir(-33.0, 151.0), 100.0, 33, 17);
        let whole = bits(&RegionGenerator::new(spec, &field, None).finish());
        for max_nodes in [1, 97, u32::MAX] {
            let mut prep = RegionPrep::new(spec, &field, None);
            let mut steps = 0u64;
            loop {
                let before = prep.units_done();
                let done = prep.step(max_nodes);
                steps += 1;
                // Every step stays within its budget (the final transform counts no units).
                assert!(prep.units_done() - before <= u64::from(max_nodes));
                if done {
                    break;
                }
            }
            assert!(prep.step(max_nodes), "stays done");
            if max_nodes == 1 {
                assert!(steps > u64::from(lattice::PLAN_UNITS), "{steps} steps");
            }
            assert_eq!(
                bits(&prep.finish().finish()),
                whole,
                "{spacing_m} m, steps of {max_nodes}"
            );
        }
        // finish() completes a part-built prep.
        let mut prep = RegionPrep::new(spec, &field, None);
        prep.step(500);
        assert_eq!(bits(&prep.finish().finish()), whole);
    }
}

#[test]
fn apron_reads_the_overview_not_tiles() {
    // Tiles say 100 m, the overview 300 m, the ICE-6G frames 50 m with no change since: the
    // inner field reads tiles, the apron only the overview (at 1852 m spacing the apron adds no
    // detail, so its texels are the overview exactly).
    let centre = dir(-25.0, 135.0);
    let mut tiles = flat_tiles(centre, 100.0, 4);
    let (era, present) = (ice_frame(50.0, 0.0), ice_frame(50.0, 0.0));
    let spec = spec_at(centre, 0.021, 33, 17);
    {
        let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        // Without an overview the apron falls back to the cube frames, never to tiles.
        let grid = generate(spec, &recent);
        assert!(grid.apron.elevation_m.iter().all(|&h| h == 50.0));
        assert!(
            grid.inner
                .elevation_m
                .iter()
                .all(|&h| (h - 100.0).abs() < 10.0)
        );
    }
    tiles.set_overview(flat_overview(300.0, 4)).unwrap();
    let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
    let grid = generate(spec, &recent);
    assert!(grid.apron.elevation_m.iter().all(|&h| h == 300.0));
    assert!(grid.inner.elevation_m.iter().all(|&h| h == 100.0));
}

#[test]
fn deep_tiles_needed_cover_every_inner_read() {
    // At 1 Ma plate 101 has turned 0.9 degrees, so its present-day reads sit about 100 km west
    // of the era square: centred at lon -88, the carried square crosses into the tile column
    // west of -90 that the era square does not touch.
    let crust = rift(0);
    let frames = rift_frames(|_| (500.0, true));
    let centre = dir(0.0, -88.0);
    let frame = RegionFrame { center: centre };
    let overview = || flat_overview(300.0, 0);
    let probe = TileSet::new();
    let field = rift_deep(&crust, &frames, &probe, 1.0);
    let needed = field.tiles_needed(&frame);
    let era_square = TileSet::needed_for(&frame);
    assert!(era_square.iter().all(|t| needed.contains(t)));
    assert!(
        needed.len() > era_square.len(),
        "{needed:?} vs {era_square:?}"
    );
    // Tiles at 100 m, the overview at 300 m: any inner read that misses a tile shows.
    let with = |tiles: &[(u8, u8)]| {
        let mut set = TileSet::new();
        for &(r, c) in tiles {
            set.insert(
                r,
                c,
                crate::worldgen::tiles::tests::tile_from(r, c, |_, _| (100.0, 1)),
            )
            .unwrap();
        }
        set.set_overview(overview()).unwrap();
        set
    };
    let exact = with(&needed);
    assert!(exact.missing(&needed).is_empty(), "fallback stays false");
    let mut everything: Vec<(u8, u8)> = Vec::new();
    for r in 15..21 {
        for c in 12..24 {
            everything.push((r, c));
        }
    }
    let all = with(&everything);
    let era_only = with(&era_square);
    assert!(
        !era_only.missing(&needed).is_empty(),
        "the era square alone reports fallback"
    );
    let spec = spec_at(centre, 1.0, 33, 17);
    let grid = |tiles: &TileSet| {
        let field = rift_deep(&crust, &frames, tiles, 1.0);
        assert!(
            field.tiles_needed(&frame) == needed,
            "independent of the loaded tiles"
        );
        bits(&RegionGenerator::new(spec, &field, None).finish())
    };
    let (g_exact, g_all, g_era) = (grid(&exact), grid(&all), grid(&era_only));
    assert_eq!(
        g_exact, g_all,
        "exactly the needed tiles read like every tile"
    );
    assert_ne!(
        g_era.0, g_all.0,
        "the era square alone misses carried reads"
    );
    assert_eq!(g_era.2, g_all.2, "the apron reads only the overview");
}

#[test]
fn tiles_needed_covers_every_river_read() {
    use crate::hydrology::params::{INCISION_WINDOW_M, RIVER_MIN_DISCHARGE_M3S, SUBPOINT_M};
    use crate::hydrology::{CubeLattice, Down, river_tile_margin_m};
    use std::sync::Mutex;
    // The 1 Ma deep field of `deep_tiles_needed_cover_every_inner_read`: plate 101's present-day
    // reads sit 0.9 degrees west of the era frame. Centred at lon -87.3, the carried inner
    // square plus 5 km stays east of the tile edge at -90, but the reads of a river's upstream
    // window west of the square cross it.
    let crust = rift(0);
    let frames = rift_frames(|_| (500.0, true));
    let centre = dir(0.0, -87.3);
    let frame = RegionFrame { center: centre };
    let lattice = CubeLattice { n: 900 };
    let margin = river_tile_margin_m(&lattice);
    let probe = TileSet::new();
    let needed = rift_deep(&crust, &frames, &probe, 1.0).tiles_needed_with_margin(&frame, margin);
    // A 150,000-250,000 m³/s river flowing east through the region from 420 km west of its
    // centre.
    let mut nodes: Vec<u32> = Vec::new();
    for k in 0..=840 {
        let x = -420_000.0 + 1000.0 * f64::from(k);
        let v = lattice.nearest(frame.local_to_dir(x, 10_000.0));
        if nodes.last() != Some(&v) {
            nodes.push(v);
        }
    }
    let mouth = nodes.pop().expect("nodes");
    let len = nodes.len();
    let hydro = river_hydro(
        lattice,
        &[RiverChain {
            q: (0..len).map(|k| 150_000.0 + 200.0 * k as f32).collect(),
            bed: (0..len).map(|k| 400.0 - 4.0 * k as f32).collect(),
            nodes,
            end: Down::Ocean(mouth),
        }],
        vec![],
    );
    // Tiles at 100 m, the overview at 300 m: a read that misses a tile shows.
    let with = |tiles: &[(u8, u8)]| {
        let mut set = TileSet::new();
        for &(r, c) in tiles {
            set.insert(
                r,
                c,
                crate::worldgen::tiles::tests::tile_from(r, c, |_, _| (100.0, 1)),
            )
            .unwrap();
        }
        set.set_overview(flat_overview(300.0, 0)).unwrap();
        set
    };
    let exact = with(&needed);
    let field = rift_deep(&crust, &frames, &exact, 1.0);
    let reads: Mutex<Vec<RenderDir>> = Mutex::new(Vec::new());
    let floor_at = |d: RenderDir| {
        reads.lock().unwrap().push(d);
        field.sample(d).elevation_m
    };
    let key_at = |d: RenderDir| (d.0, 1);
    let mut builder = super::rivers::RiverBuilder::new(
        &hydro,
        &frame,
        REGION_SIZE_M / 2.0,
        SUBPOINT_M,
        RIVER_MIN_DISCHARGE_M3S as f32,
    );
    while !builder.step(&floor_at, &key_at, 10_000) {}
    let rivers = builder.finish();
    assert!(!rivers.subpoints().is_empty());
    let reads = reads.into_inner().unwrap();
    assert!(reads.len() > 10_000, "{} reads", reads.len());
    // Exactly the needed tiles read like every tile (the M2 pattern).
    let mut everything: Vec<(u8, u8)> = Vec::new();
    for r in 15..21 {
        for c in 12..24 {
            everything.push((r, c));
        }
    }
    let all_tiles = with(&everything);
    let all = rift_deep(&crust, &frames, &all_tiles, 1.0);
    let misses = |field: &SnapshotField| {
        reads
            .iter()
            .filter(|&&d| field.sample(d).elevation_m != all.sample(d).elevation_m)
            .count()
    };
    assert_eq!(misses(&field), 0, "every river read lies in a needed tile");
    // The hazards: the inner field's own tiles miss reads, and the reads reach past Task 3's
    // margin (1.9 spacings plus the incision window).
    let inner_tiles = rift_deep(&crust, &frames, &probe, 1.0).tiles_needed(&frame);
    let inner = with(&inner_tiles);
    assert!(misses(&rift_deep(&crust, &frames, &inner, 1.0)) > 0);
    let basis = frame.basis();
    let beyond = reads
        .iter()
        .map(|&d| {
            let (x, z) = basis.dir_to_local(d).unwrap();
            x.abs().max(z.abs()) - REGION_SIZE_M / 2.0
        })
        .fold(f64::MIN, f64::max);
    let old = 1.9 * lattice.mean_spacing_m() + INCISION_WINDOW_M;
    assert!(
        beyond > old && beyond < margin,
        "{beyond} vs {old}, {margin}"
    );
}

/// Steps a prep through with `max_nodes` per step; returns (slowest step before the last,
/// the last step (the coast-distance transform), number of steps), in seconds.
fn prep_profile<F: CoarseField>(spec: RegionSpec, field: &F, max_nodes: u32) -> (f64, f64, u64) {
    let mut prep = RegionPrep::new(spec, field, None);
    let (mut worst, mut steps) = (0.0f64, 0u64);
    loop {
        let t = std::time::Instant::now();
        let done = prep.step(max_nodes);
        let dt = t.elapsed().as_secs_f64();
        steps += 1;
        if done {
            return (worst, dt, steps);
        }
        worst = worst.max(dt);
    }
}

/// Profiles step sizes, each on a field fresh from `make` (so field-side caches start cold).
fn print_profile<F: CoarseField>(label: &str, spec: RegionSpec, make: impl Fn() -> F) {
    let field = make();
    let t = std::time::Instant::now();
    let _ = RegionPrep::new(spec, &field, None);
    println!(
        "  {label}: RegionPrep::new {:.3} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    for max_nodes in [256, 1024, 4096] {
        let field = make();
        let (worst, last, steps) = prep_profile(spec, &field, max_nodes);
        println!(
            "  {label}: step({max_nodes}): {steps} steps, slowest {:.2} ms, final transform {:.2} ms",
            worst * 1e3,
            last * 1e3
        );
    }
}

/// Single-threaded generation of a full deep region on a 150-plate synthetic bake. Run with
/// `cargo test --release -p timefly_core generation_timing -- --ignored --nocapture`.
#[test]
#[ignore = "timing; run in release"]
fn generation_timing() {
    let crust = rift(148);
    let frames = rift_frames(|d| (hilly(d) as f32, true));
    let tiles = TileSet::new();
    let field = rift_deep(&crust, &frames, &tiles, 100.0);
    let spec = RegionSpec::new(dir(10.0, 60.0), YearsBp::from_myr(100.0), 1);
    let t0 = std::time::Instant::now();
    let g = RegionGenerator::new(spec, &field, None);
    let t_new = t0.elapsed();
    let grid = g.finish();
    let t_all = t0.elapsed();
    assert_finite(&grid, "timing");
    println!(
        "deep region {}^2 + {}^2: new {:.3} s, total {:.3} s (single thread)",
        grid.inner.res,
        grid.apron.res,
        t_new.as_secs_f64(),
        t_all.as_secs_f64()
    );
    print_profile("deep", spec, || rift_deep(&crust, &frames, &tiles, 100.0));
    // Deep, ETOPO-dominant (1 Ma: 78% modern terrain, 1,852 m spacing), mid-latitude and at a
    // cube corner, on the 150-plate model.
    for (label, centre) in [
        ("deep 1 Ma mid-latitude", dir(46.0, 8.0)),
        ("deep 1 Ma cube corner", RenderDir(DVec3::ONE.normalize())),
    ] {
        let mut tiles = flat_tiles(centre, 700.0, 26);
        tiles
            .set_overview(crate::worldgen::tiles::tests::overview_from(12, |_, _| {
                (700.0, 26)
            }))
            .unwrap();
        let field = rift_deep(&crust, &frames, &tiles, 1.0);
        assert_eq!(field.source_spacing_m(), 1852.0);
        let spec = RegionSpec::new(centre, YearsBp::from_myr(1.0), 1);
        let t0 = std::time::Instant::now();
        let g = RegionGenerator::new(spec, &field, None);
        let t_new = t0.elapsed();
        let grid = g.finish();
        println!(
            "{label}: new {:.3} s, total {:.3} s (single thread)",
            t_new.as_secs_f64(),
            t0.elapsed().as_secs_f64()
        );
        assert_finite(&grid, label);
        print_profile(label, spec, || rift_deep(&crust, &frames, &tiles, 1.0));
    }
    // Recent regions with tiles: mid-latitude, and at a cube corner, where lattice nodes are
    // densest (the worst case for `new`).
    let (era, present) = (ice_frame(1500.0, 0.5), ice_frame(1200.0, 0.0));
    for (label, centre) in [
        ("mid-latitude", dir(46.0, 8.0)),
        ("cube corner", RenderDir(DVec3::ONE.normalize())),
    ] {
        let mut tiles = flat_tiles(centre, 1200.0, 26);
        // The bake's 5' overview (2161 x 4321).
        tiles
            .set_overview(crate::worldgen::tiles::tests::overview_from(12, |_, _| {
                (1200.0, 26)
            }))
            .unwrap();
        let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        let t0 = std::time::Instant::now();
        let g = RegionGenerator::new(RegionSpec::new(centre, YearsBp(21_000.0), 1), &recent, None);
        let t_new = t0.elapsed();
        let nodes: usize = g
            .lattices
            .iter()
            .flat_map(|l| l.blocks.iter().flatten())
            .map(|b| b.nodes.len())
            .sum();
        let grid = g.finish();
        println!(
            "recent region ({label}, {} tiles, {nodes} nodes): new {:.3} s, total {:.3} s \
             (single thread)",
            tiles.len(),
            t_new.as_secs_f64(),
            t0.elapsed().as_secs_f64()
        );
        assert_finite(&grid, "timing recent");
        print_profile(label, RegionSpec::new(centre, YearsBp(21_000.0), 1), || {
            recent
        });
    }
}

#[test]
fn the_snowline_raise_round_trips_through_the_surface_byte() {
    for raise in [0.0, 3.0, 500.0, 777.7, 1000.0] {
        let byte = encode_snowline_raise(raise);
        assert_eq!(byte & SURFACE_SHORE, 0);
        let back = decode_snowline_raise(byte | SURFACE_SHORE);
        assert!(
            (back - raise).abs() <= MAX_SNOWLINE_RAISE_M / 254.0 + 1e-9,
            "{raise} -> {back}"
        );
    }
    assert_eq!(encode_snowline_raise(-5.0), 0);
    assert_eq!(encode_snowline_raise(5000.0), SURFACE_RAISE_STEPS);
    assert_eq!(encode_snowline_raise(f64::NAN), 0);
}

/// Two regions' surface codes at one point agree except where they read the coast distance
/// from their own aprons (rulings R9, R27): the biome byte, and the snowline's dryness raise,
/// with the snow (ice byte) it moves. Where the raise agrees, the ice must agree too.
fn assert_same_surface_but_dryness(a: [u8; 4], b: [u8; 4]) {
    assert_eq!(a[1], b[1], "era");
    assert_eq!(a[3] & SURFACE_SHORE, b[3] & SURFACE_SHORE, "shore");
    if a[3] & SURFACE_RAISE_STEPS == b[3] & SURFACE_RAISE_STEPS {
        assert_eq!(a[2], b[2], "ice with the same raise");
    }
}

/// Lattice nodes live in allocations of one size whatever the region, so the allocator reuses a
/// freed lattice's memory for the next region's: macOS keeps freed blocks resident and hands them
/// back only for a request of the same size, so lattices of varying sizes each added their own
/// 17-31 MiB to the footprint (M3 hop memory, `docs/testing/m3/performance.md`).
#[test]
fn lattice_nodes_live_in_allocations_of_one_size() {
    let chunk = lattice::NODE_CHUNK * std::mem::size_of::<lattice::PackedNode>();
    assert_eq!(chunk, 512 << 10);
    // Coarser and finer sources: lattices of different sizes.
    for spacing_m in [111_000.0, 9_000.0] {
        let field = FnField {
            f: |d: RenderDir| 800.0 * (d.0.x * 40.0).sin() + 200.0,
            spacing_m,
            koppen: None,
        };
        let g = RegionGenerator::new(spec_at(dir(10.0, 30.0), 0.0, 33, 17), &field, None);
        let sizes = g.node_allocation_bytes();
        assert!(!sizes.is_empty());
        assert!(
            sizes.iter().all(|&s| s == chunk),
            "{spacing_m} m: node allocations {sizes:?}, want all {chunk}"
        );
    }
    // A store over several chunks reads back every node in order.
    let mut store = lattice::NodeStore::default();
    let n = 2 * lattice::NODE_CHUNK + 5;
    for i in 0..n {
        store.push(lattice::PackedNode::test_node(i as f32));
    }
    assert_eq!(store.len(), n);
    assert_eq!(store.allocation_bytes().collect::<Vec<_>>(), vec![chunk; 3]);
    for i in [0, 1, lattice::NODE_CHUNK - 1, lattice::NODE_CHUNK, n - 1] {
        assert_eq!(store.get(i).map(|p| p.elevation_m), Some(i as f32));
        assert_eq!(store[i].elevation_m, i as f32);
    }
    assert!(store.get(n).is_none());
}

// ---- Rivers and lakes (Task 6): carved terrain, water fields and boulders. ----

mod water {
    use super::*;
    use crate::hydrology::climate::ZonalClimate;
    use crate::hydrology::climate::tests::shipped_table;
    use crate::hydrology::params::{NO_WATER_DROP_M, RIVER_MIN_DISCHARGE_M3S, SUBPOINT_M};
    use crate::hydrology::{
        CubeLattice, Down, EraHydrology, HydroGraph, HydroPrep, NodeSample, NodeSource,
    };
    use crate::terrain::blended_water;
    use crate::worldgen::frame::EARTH_RADIUS_M;
    use crate::worldgen::generate::rivers::{RegionRivers, chord_point};
    use crate::worldgen::generate::water::decode_river;

    /// FNV-1a (64-bit) over the M3 fields' bytes: each field's heights (f32, little-endian)
    /// then its surface bytes, inner field first.
    fn m3_hash(grid: &RegionGrid) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: u8| {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for f in [&grid.inner, &grid.apron] {
            for x in &f.elevation_m {
                x.to_le_bytes().into_iter().for_each(&mut eat);
            }
            for s in &f.surface {
                s.iter().copied().for_each(&mut eat);
            }
        }
        h
    }

    /// [`m3_hash`] of each fixture of `overlap_agreement` and `prep_in_steps_matches_new`, from
    /// the generator at 273e20b (before any rivers code; 65259b5 gave the same values).
    const M3_GOLDEN: [(&str, u64); 8] = [
        ("prep deep", 0xcf7a_d1e5_75c8_6cc5),
        ("prep recent", 0x466a_35e7_a875_f082),
        ("overlap recent a", 0xeef6_b23a_3b2e_c482),
        ("overlap recent b", 0xeef6_b23a_3b2e_c482),
        ("overlap deep a", 0x3fac_4f39_6d56_a2f9),
        ("overlap deep b", 0xaf69_2e55_0455_ce33),
        ("overlap 1 Ma", 0x484c_04de_cc0b_1670),
        ("1 Ma 129/65", 0x3622_e244_e4c5_8987),
    ];

    /// A region without a pass, and per texel (inner, then apron) whether a coastline corner is
    /// below sea level.
    fn m3_region<F: CoarseField>(spec: RegionSpec, field: &F) -> (RegionGrid, Vec<bool>) {
        let g = RegionGenerator::new(spec, field, None);
        let frame = spec.frame();
        let mut below = Vec::new();
        for kind in [FieldKind::Inner, FieldKind::Apron] {
            let res = g.rows_in(kind);
            let size = if kind == FieldKind::Inner {
                REGION_SIZE_M
            } else {
                APRON_SIZE_M
            };
            for row in 0..res {
                for col in 0..res {
                    let (x, z) = local_of(res, size, row, col);
                    let (_, c) = g.coarse_and_corners(kind, frame.local_to_dir(x, z));
                    below.push(c.iter().any(|&v| v < 0.0));
                }
            }
        }
        (g.finish(), below)
    }

    /// The fixtures of `overlap_agreement` and `prep_in_steps_matches_new`.
    fn m3_golden_regions() -> Vec<(&'static str, RegionGrid, Vec<bool>)> {
        let mut out = Vec::new();
        for (label, spacing_m) in [("prep deep", 111_000.0), ("prep recent", 1852.0)] {
            let field = FnField {
                f: hilly,
                spacing_m,
                koppen: None,
            };
            let (g, b) = m3_region(spec_at(dir(-33.0, 151.0), 100.0, 33, 17), &field);
            out.push((label, g, b));
        }
        let a = dir(30.0, 10.0);
        let b = RegionFrame { center: a }.local_to_dir(100_000.0, 0.0);
        let mut tiles = flat_tiles_around(&[a, b], 350.0, 26);
        tiles.set_overview(flat_overview(340.0, 26)).unwrap();
        let (era, present) = (ice_frame(380.0, 0.2), ice_frame(350.0, 0.0));
        let recent = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        for (label, c) in [("overlap recent a", a), ("overlap recent b", b)] {
            let (g, bl) = m3_region(spec_at(c, 0.021, 33, 17), &recent);
            out.push((label, g, bl));
        }
        let field = FnField {
            f: hilly,
            spacing_m: 111_000.0,
            koppen: None,
        };
        for (label, c) in [("overlap deep a", a), ("overlap deep b", b)] {
            let (g, bl) = m3_region(spec_at(c, 100.0, 33, 17), &field);
            out.push((label, g, bl));
        }
        let crust = rift(0);
        let frames = rift_frames(|d| (hilly(d) as f32, true));
        let mut tiles = TileSet::new();
        for r in 15..21 {
            for c in 12..24 {
                tiles
                    .insert(
                        r,
                        c,
                        crate::worldgen::tiles::tests::tile_from(r, c, |lat, lon| {
                            (
                                (200.0 + 150.0 * (lat * 3.0).sin() * (lon * 2.0).cos()) as f32,
                                3,
                            )
                        }),
                    )
                    .unwrap();
            }
        }
        tiles.set_overview(flat_overview(250.0, 3)).unwrap();
        let field = rift_deep(&crust, &frames, &tiles, 1.0);
        let c = dir(0.0, -88.0);
        let (g, bl) = m3_region(spec_at(c, 1.0, 33, 17), &field);
        out.push(("overlap 1 Ma", g, bl));
        let (g, bl) = m3_region(spec_at(c, 1.0, 129, 65), &field);
        out.push(("1 Ma 129/65", g, bl));
        out
    }

    #[test]
    fn without_a_pass_the_terrain_is_m3s() {
        let (mut sea, mut dry) = (0, 0);
        for ((label, grid, below), (golden_label, golden)) in
            m3_golden_regions().into_iter().zip(M3_GOLDEN)
        {
            assert_eq!(label, golden_label);
            assert_eq!(
                m3_hash(&grid),
                golden,
                "{label}: the heights or surface moved"
            );
            assert!(grid.boulders.is_empty());
            let texels = grid.inner.elevation_m.iter().chain(&grid.apron.elevation_m);
            let levels = grid
                .inner
                .water_level_m
                .iter()
                .chain(&grid.apron.water_level_m);
            let rivers = grid.inner.river.iter().chain(&grid.apron.river);
            for (((&h, &level), river), below) in texels.zip(levels).zip(rivers).zip(below) {
                assert_eq!(*river, RIVER_NONE, "{label}");
                if below {
                    assert_eq!(level.to_bits(), 0f32.to_bits(), "{label}: sea level");
                    sea += 1;
                } else {
                    assert_eq!(level, h - NO_WATER_DROP_M as f32, "{label}: dry");
                    dry += 1;
                }
            }
        }
        assert!(sea > 1000 && dry > 1000, "both cases: {sea} sea, {dry} dry");
    }

    // ---- Synthetic eras. ----

    /// A pass's elevation from a function of the direction.
    struct PassTerrain<G: Fn(RenderDir) -> f64 + Sync>(G);

    impl<G: Fn(RenderDir) -> f64 + Sync> NodeSource for PassTerrain<G> {
        fn node(&self, dir: RenderDir) -> NodeSample {
            NodeSample {
                elevation_m: (self.0)(dir) as f32,
                ice: 0.0,
            }
        }

        fn data_lakes(&self) -> bool {
            false
        }
    }

    /// The continental pass at present-day climate over `f` on a lattice of size `n`.
    fn pass_over(n: u32, f: impl Fn(RenderDir) -> f64 + Sync) -> EraHydrology {
        let table = shipped_table();
        let climate = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        let mut prep = HydroPrep::with_lake_levels(CubeLattice { n }, YearsBp(0.0), &[]);
        let source = PassTerrain(f);
        while !prep.step(&source, &climate, &table, 1 << 20) {}
        prep.finish()
    }

    /// A wet continent around `centre` (Task 5's fixture, on any axis): a 1,800 m cone 0.45 rad
    /// in radius with ridges, ocean beyond.
    fn continent(centre: DVec3, d: RenderDir) -> f64 {
        let p = d.0.normalize();
        let r = p.dot(centre).clamp(-1.0, 1.0).acos();
        let (north, east) = RegionFrame {
            center: RenderDir(centre),
        }
        .north_east();
        let theta = p.dot(north).atan2(p.dot(east));
        1800.0 * (1.0 - r / 0.45)
            + 250.0 * (7.0 * theta).sin() * (r * 30.0).cos()
            + 120.0 * (p.dot(north) * 40.0).sin()
    }

    /// The river node direction with the most rivers of at least `min_q` within 150 km.
    fn densest_rivers(hydro: &EraHydrology, min_q: f32) -> RenderDir {
        let mut near = Vec::new();
        let nodes = hydro.rivers.nodes();
        let best = nodes
            .iter()
            .step_by(7)
            .filter(|n| n.discharge_m3s >= min_q)
            .map(|n| {
                hydro.rivers.near(
                    &hydro.lattice,
                    hydro.lattice.dir(n.node),
                    150_000.0,
                    &mut near,
                );
                let count = near
                    .iter()
                    .filter(|&&i| nodes[i as usize].discharge_m3s >= min_q)
                    .count();
                (count, n.node)
            })
            .max()
            .expect("rivers");
        hydro.lattice.dir(best.1)
    }

    fn texel_bits(t: &Texel) -> (u32, u32, [u16; 4]) {
        (t.h.to_bits(), t.water_level_m.to_bits(), t.river)
    }

    /// Asserts two regions give the same texels at `probes`; returns how many show a river
    /// and how many were carved (differ from the same region without a pass).
    fn assert_texels_agree<F: CoarseField>(
        ga: &RegionGenerator<F>,
        gb: &RegionGenerator<F>,
        bare: &RegionGenerator<F>,
        probes: &[(FieldKind, RenderDir)],
    ) -> (usize, usize) {
        let (mut rivers, mut carved) = (0, 0);
        for (i, &(kind, p)) in probes.iter().enumerate() {
            let (a, b) = (ga.sample_texel_in(kind, p), gb.sample_texel_in(kind, p));
            assert_eq!(texel_bits(&a), texel_bits(&b), "{kind:?} probe {i}");
            assert_same_surface_but_dryness(a.surface, b.surface);
            rivers += usize::from(a.river != RIVER_NONE);
            carved += usize::from(bare.sample_texel_in(kind, p).h != a.h);
        }
        (rivers, carved)
    }

    /// Probes in both regions' squares (of `kind`): every refined sub-point of `ga`'s rivers
    /// that lies in both, nudged off the centreline, then a grid; `count` in all.
    fn overlap_probes<F: CoarseField>(
        ga: &RegionGenerator<F>,
        gb: &RegionGenerator<F>,
        kind: FieldKind,
        count: usize,
    ) -> Vec<(FieldKind, RenderDir)> {
        let half = if kind == FieldKind::Inner {
            REGION_SIZE_M / 2.0
        } else {
            APRON_SIZE_M / 2.0
        } * 0.98;
        let (fa, fb) = (ga.spec().frame(), gb.spec().frame());
        let inside = |d: RenderDir| {
            [fa, fb].iter().all(|f| {
                f.dir_to_local(d)
                    .is_some_and(|(x, z)| x.abs() < half && z.abs() < half)
            })
        };
        let mut out: Vec<(FieldKind, RenderDir)> = Vec::new();
        let rivers = ga.rivers(kind).expect("a pass");
        let subs: Vec<DVec3> = rivers
            .subpoints()
            .iter()
            .map(|s| s.dir)
            .filter(|&d| inside(RenderDir(d)))
            .collect();
        let step = (subs.len() / (count / 2)).max(1);
        for (k, d) in subs.iter().step_by(step).enumerate() {
            let (north, east) = RegionFrame {
                center: RenderDir(*d),
            }
            .north_east();
            let off = (k % 7) as f64 * 3.0 / EARTH_RADIUS_M;
            out.push((kind, RenderDir((*d + (east + north) * off).normalize())));
        }
        let mut k = 0u32;
        while out.len() < count && k < 100_000 {
            // A Weyl sequence over A's square.
            let (u, v) = (
                (f64::from(k) * 0.618_033_988_75).fract(),
                (f64::from(k) * 0.754_877_666_25).fract(),
            );
            let d = fa.local_to_dir((2.0 * u - 1.0) * half, (2.0 * v - 1.0) * half);
            if inside(d) {
                out.push((kind, d));
            }
            k += 1;
        }
        out
    }

    /// Two regions `dist_m` apart either side of `mid`, at the standard resolutions, with the
    /// pass: 200 inner and 200 apron probes agree by bits, rivers included.
    fn assert_regions_agree<F: CoarseField>(
        hydro: &EraHydrology,
        field: &F,
        a: RenderDir,
        b: RenderDir,
    ) -> (usize, usize, usize) {
        let spec = |c| spec_at(c, 0.0, 1025, 513);
        let ga = RegionGenerator::new(spec(a), field, Some(hydro));
        let gb = RegionGenerator::new(spec(b), field, Some(hydro));
        let bare = RegionGenerator::new(spec(a), field, None);
        let mut probes = overlap_probes(&ga, &gb, FieldKind::Inner, 200);
        let inner_probes = probes.len();
        probes.extend(overlap_probes(&ga, &gb, FieldKind::Apron, 200));
        assert_eq!(probes.len(), 400);
        let (inner_rivers, inner_carved) =
            assert_texels_agree(&ga, &gb, &bare, &probes[..inner_probes]);
        let (apron_rivers, _) = assert_texels_agree(&ga, &gb, &bare, &probes[inner_probes..]);
        // The two regions refine different sets: the agreement is not a shared cache.
        assert_ne!(
            ga.rivers(FieldKind::Inner).unwrap().subpoints().len(),
            gb.rivers(FieldKind::Inner).unwrap().subpoints().len()
        );
        (inner_rivers, inner_carved, apron_rivers)
    }

    #[test]
    fn rivers_agree_bit_for_bit_where_regions_overlap() {
        let c0 = DVec3::X;
        let hydro = pass_over(419, move |d| continent(c0, d));
        let field = FnField {
            f: move |d: RenderDir| continent(c0, d),
            spacing_m: 1852.0,
            koppen: None,
        };
        let a = densest_rivers(&hydro, RIVER_MIN_DISCHARGE_M3S as f32);
        let b = RegionFrame { center: a }.local_to_dir(100_000.0, 0.0);
        let (inner_rivers, carved, apron_rivers) = assert_regions_agree(&hydro, &field, a, b);
        // The hazard: rivers in the overlap, inner and apron, and the terrain carved by them.
        assert!(inner_rivers > 60, "{inner_rivers} inner river probes");
        assert!(carved > 60, "{carved} carved probes");
        assert!(apron_rivers > 20, "{apron_rivers} apron river probes");
    }

    /// Rivers along great circles `local z = z_i` of `frame`, west to east over ±150 km: chains
    /// of the lattice's nearest nodes (consecutive nodes are lattice steps), discharge `q`, on a
    /// bed falling `slope` eastward from 5,000 m at the centre (gentle: 1e-4), each in a V
    /// valley. Returns the pass and the terrain.
    fn rivers_along(
        lattice: CubeLattice,
        frame: RegionFrame,
        lines_z: &[f64],
        q: f32,
        slope: f64,
    ) -> (EraHydrology, impl Fn(RenderDir) -> f64 + Sync + Copy) {
        let basis = frame.basis();
        let bed = move |x: f64| 5000.0 - slope * x;
        let chains: Vec<RiverChain> = lines_z
            .iter()
            .map(|&z| {
                let mut nodes: Vec<u32> = Vec::new();
                for k in 0..=300 {
                    let v =
                        lattice.nearest(basis.local_to_dir(-150_000.0 + 1000.0 * f64::from(k), z));
                    if nodes.last() != Some(&v) {
                        nodes.push(v);
                    }
                }
                let mouth = nodes.pop().expect("nodes");
                RiverChain {
                    q: vec![q; nodes.len()],
                    bed: nodes
                        .iter()
                        .map(|&v| {
                            let (x, _) = basis.dir_to_local(lattice.dir(v)).expect("near");
                            bed(x) as f32
                        })
                        .collect(),
                    nodes,
                    end: Down::Ocean(mouth),
                }
            })
            .collect();
        let hydro = river_hydro(lattice, &chains, vec![]);
        let (zs, n) = ([lines_z[0], *lines_z.last().expect("lines")], lines_z.len());
        let floor = move |d: RenderDir| {
            let (x, z) = basis.dir_to_local(d).unwrap_or((0.0, 0.0));
            let dist = if n == 1 {
                (z - zs[0]).abs()
            } else {
                (z - zs[0]).abs().min((z - zs[1]).abs())
            };
            bed(x) + 5.0 + 0.05 * dist
        };
        (hydro, floor)
    }

    /// Review Focus 3: two regions straddling the cube corner at 35.26° N, 45° E agree, rivers
    /// included, and the rivers in the overlap cross the face seams.
    #[test]
    fn rivers_agree_bit_for_bit_at_a_cube_corner() {
        let corner = dir(35.26439, 45.0);
        assert!(corner.0.abs_diff_eq(DVec3::ONE.normalize(), 1e-6));
        let frame = RegionFrame { center: corner };
        // Two meandering rivers (w = 274 m) on the recent lattice: one 8 km south of the corner,
        // crossing the PosZ/PosX edge; one 20 km north, crossing into PosY and out.
        let (hydro, floor) = rivers_along(
            CubeLattice { n: 900 },
            frame,
            &[8_000.0, -20_000.0],
            3000.0,
            1e-4,
        );
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        // South-west and south-east of the corner: either side of the PosX/PosZ edge, which
        // runs due south from it.
        let (a, b) = (
            frame.local_to_dir(-50_000.0, 30_000.0),
            frame.local_to_dir(50_000.0, 30_000.0),
        );
        // The cube face: the axis and sign of the largest component.
        let face = |d: RenderDir| {
            let v = d.0.to_array();
            let k = (0..3)
                .max_by(|&i, &j| v[i].abs().total_cmp(&v[j].abs()))
                .expect("three axes");
            (k, v[k] > 0.0)
        };
        assert_ne!(face(a), face(b), "the centres lie on different faces");
        let (inner_rivers, carved, apron_rivers) = assert_regions_agree(&hydro, &field, a, b);
        assert!(inner_rivers > 60, "{inner_rivers} inner river probes");
        assert!(carved > 60, "{carved} carved probes");
        assert!(apron_rivers > 20, "{apron_rivers} apron river probes");
        // The refined rivers near the corner lie on all three faces, and meander.
        let ga = RegionGenerator::new(spec_at(a, 0.0, 1025, 513), &field, Some(&hydro));
        let near: Vec<DVec3> = ga
            .rivers(FieldKind::Inner)
            .unwrap()
            .subpoints()
            .iter()
            .map(|s| s.dir)
            .filter(|d| d.angle_between(corner.0) * EARTH_RADIUS_M < 100_000.0)
            .collect();
        let faces: std::collections::BTreeSet<(usize, bool)> =
            near.iter().map(|&d| face(RenderDir(d))).collect();
        assert_eq!(faces.len(), 3, "faces {faces:?}");
        let off_line = near
            .iter()
            .filter_map(|&d| frame.dir_to_local(RenderDir(d)))
            .filter(|&(_, z)| (z - 8_000.0).abs().min((z + 20_000.0).abs()) > 100.0)
            .count();
        assert!(
            off_line > 50,
            "{off_line} sub-points meander off the valley axes"
        );
    }

    /// The cube-corner agreement extended to boulders and their foam sites (Task 11c): two
    /// regions either side of the PosX/PosZ edge, a rapid (a 3% bed) crossing it 8 km south of
    /// the corner, hold the same boulders and foam sites, bit for bit, where both squares hold
    /// them.
    #[test]
    fn boulders_and_foam_sites_agree_at_a_cube_corner() {
        let corner = dir(35.26439, 45.0);
        let frame = RegionFrame { center: corner };
        let (hydro, floor) = rivers_along(CubeLattice { n: 900 }, frame, &[8_000.0], 150.0, 0.03);
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let (a, b) = (
            frame.local_to_dir(-50_000.0, 30_000.0),
            frame.local_to_dir(50_000.0, 30_000.0),
        );
        let ga = RegionGenerator::new(spec_at(a, 0.0, 1025, 33), &field, Some(&hydro));
        let gb = RegionGenerator::new(spec_at(b, 0.0, 1025, 33), &field, Some(&hydro));
        let half = REGION_SIZE_M / 2.0;
        let in_square = |g: &RegionGenerator<_>, d: DVec3| {
            g.spec()
                .frame()
                .dir_to_local(RenderDir(d))
                .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half)
        };
        type Key = (u32, [u64; 3]);
        let key = |d: DVec3, seed: u32| -> Key { (seed, d.to_array().map(f64::to_bits)) };
        let bits = |b: &Boulder| {
            (
                [
                    b.base_m,
                    b.radius_m,
                    b.wet,
                    b.level_m,
                    b.flow_mps[0],
                    b.flow_mps[1],
                ]
                .map(f32::to_bits),
                b.seed,
            )
        };
        let shared = |g: &RegionGenerator<_>, other: &RegionGenerator<_>| {
            let mut v: Vec<Boulder> = g
                .boulders()
                .iter()
                .copied()
                .filter(|bo| in_square(other, bo.dir))
                .collect();
            v.sort_by_key(|b| key(b.dir, b.seed));
            v
        };
        let (sa, sb) = (shared(&ga, &gb), shared(&gb, &ga));
        assert!(sa.len() > 300, "{} shared boulders", sa.len());
        assert_eq!(sa.len(), sb.len());
        for (x, y) in sa.iter().zip(&sb) {
            assert_eq!(key(x.dir, x.seed), key(y.dir, y.seed));
            assert_eq!(bits(x), bits(y));
        }
        let (fa, fb): (Vec<FoamSite>, Vec<FoamSite>) =
            (foam_sites(&sa).collect(), foam_sites(&sb).collect());
        assert!(fa.len() > 100, "{} shared foam sites", fa.len());
        assert_eq!(fa, fb);
        // The shared boulders lie on both faces of the edge, beside the corner.
        let face = |d: DVec3| {
            let v = d.to_array();
            (0..3)
                .max_by(|&i, &j| v[i].abs().total_cmp(&v[j].abs()))
                .expect("three axes")
        };
        let faces: std::collections::BTreeSet<usize> = sa.iter().map(|b| face(b.dir)).collect();
        assert!(faces.len() >= 2, "faces {faces:?}");
    }

    /// Fix round 1: rapids too dense for the cap are thinned by direction (a hash of each
    /// cluster), not cut at a region-dependent prefix. Two steep rivers 30 km apart carry about
    /// 28,800 boulders over a region's square before thinning, past `MAX_BOULDERS`; thinned,
    /// both regions keep every cluster they hold, and where their squares overlap (the eastern
    /// two thirds of the first one, where the old cap cut) they hold the same boulders, bit for
    /// bit.
    #[test]
    fn thinned_rapids_agree_where_regions_overlap() {
        use crate::hydrology::params::{BOULDERS_PER_SUBPOINT_MAX, MAX_BOULDERS};
        let frame = RegionFrame {
            center: dir(20.0, 30.0),
        };
        let (hydro, floor) = rivers_along(
            CubeLattice { n: 900 },
            frame,
            &[-15_000.0, 15_000.0],
            150.0,
            0.03,
        );
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let (a, b) = (frame.center, frame.local_to_dir(100_000.0, 0.0));
        let ga = RegionGenerator::new(spec_at(a, 0.0, 1025, 33), &field, Some(&hydro));
        let gb = RegionGenerator::new(spec_at(b, 0.0, 1025, 33), &field, Some(&hydro));
        let half = REGION_SIZE_M / 2.0;
        let in_square = |g: &RegionGenerator<_>, d: DVec3| {
            g.spec()
                .frame()
                .dir_to_local(RenderDir(d))
                .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half)
        };
        // The hazard: before thinning the first region's rapids hold more than the cap.
        let unthinned: f64 = ga
            .rivers(FieldKind::Inner)
            .unwrap()
            .sub_segments()
            .filter(|(s, e)| s.rapid > 0.0 && in_square(&ga, s.dir) && in_square(&ga, e.dir))
            .map(|(s, _)| (f64::from(s.rapid) * f64::from(BOULDERS_PER_SUBPOINT_MAX)).floor())
            .sum();
        assert!(
            unthinned > f64::from(MAX_BOULDERS),
            "{unthinned} boulders before thinning"
        );
        for g in [&ga, &gb] {
            assert_eq!(g.boulders_dropped(), 0, "the cap never fires");
            assert!(g.boulders().len() < MAX_BOULDERS as usize);
        }
        let key = |b: &Boulder| (b.seed, b.dir.to_array().map(f64::to_bits));
        let bits = |b: &Boulder| {
            [
                b.base_m,
                b.radius_m,
                b.wet,
                b.level_m,
                b.flow_mps[0],
                b.flow_mps[1],
            ]
            .map(f32::to_bits)
        };
        let shared = |g: &RegionGenerator<_>, other: &RegionGenerator<_>| {
            let mut v: Vec<Boulder> = g
                .boulders()
                .iter()
                .copied()
                .filter(|bo| in_square(other, bo.dir))
                .collect();
            v.sort_by_key(key);
            v
        };
        let (sa, sb) = (shared(&ga, &gb), shared(&gb, &ga));
        assert!(sa.len() > 3_000, "{} shared boulders", sa.len());
        assert_eq!(sa.len(), sb.len());
        for (x, y) in sa.iter().zip(&sb) {
            assert_eq!(key(x), key(y));
            assert_eq!(bits(x), bits(y));
        }
        // Rocks all along both rivers, out to the far end of the overlap, where a prefix cut
        // in sub-segment order would have left none.
        let east = |b: &Boulder| ga.spec().frame().dir_to_local(RenderDir(b.dir)).unwrap().0;
        let far = sa.iter().map(east).fold(f64::MIN, f64::max);
        assert!(far > 140_000.0, "the shared rocks reach x = {far} m");
        // Fix round 2: the regions' grids drop the rocks in their own blend band at the inner
        // field's edge, where the river fades out; elsewhere they keep the same rocks.
        let drawn = |g: &RegionGenerator<_>| {
            let grid = RegionGenerator::new(
                RegionSpec {
                    apron_res: 513,
                    ..*g.spec()
                },
                &field,
                Some(&hydro),
            )
            .finish();
            grid.boulders
                .iter()
                .map(&key)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let (ka, kb) = (drawn(&ga), drawn(&gb));
        let band = |g: &RegionGenerator<_>, b: &Boulder| {
            let (x, z) = g.spec().frame().dir_to_local(RenderDir(b.dir)).unwrap();
            x.abs().max(z.abs()) > half - 4.0 * APRON_SIZE_M / 512.0
        };
        let (mut both, mut in_a_band) = (0, 0);
        for b in &sa {
            if band(&ga, b) || band(&gb, b) {
                in_a_band += 1;
                continue;
            }
            assert!(
                ka.contains(&key(b)) && kb.contains(&key(b)),
                "{b:?} drawn by one region only"
            );
            both += 1;
        }
        assert!(
            both > 2_000,
            "{both} shared rocks outside the bands, {in_a_band} inside one"
        );
    }

    /// Thinning keeps or drops whole clusters: each kept cluster has all its members, and a
    /// cluster is kept exactly when its hash falls under `BOULDER_KEEP_FRACTION`.
    #[test]
    fn thinning_keeps_whole_clusters() {
        use crate::hydrology::params::{BOULDER_CLUSTER_MAX, BOULDER_KEEP_FRACTION};
        use crate::worldgen::generate::water::cluster_kept;
        let g = rapids(150.0, true);
        let clusters = clusters_of(&g);
        let full = rapid_subsegments(&g, 0.999..=1.0);
        let mut kept = std::collections::BTreeSet::new();
        for (c, boulders) in &clusters {
            assert!(
                cluster_kept(c.river, c.k, c.index),
                "only kept clusters are listed"
            );
            kept.insert((c.river, c.k, c.index));
            if full.contains_key(&(c.river, c.k)) {
                // 24 boulders: four clusters of six, each whole.
                assert_eq!(c.members.len(), BOULDER_CLUSTER_MAX as usize);
                assert_eq!(boulders.len(), c.members.len(), "the whole cluster is held");
            }
        }
        let mut listed = 0;
        for &(river, k) in full.keys() {
            for index in 0..4 {
                listed += 1;
                assert_eq!(
                    kept.contains(&(river, k, index)),
                    cluster_kept(river, k, index),
                    "({river}, {k}, {index})"
                );
            }
        }
        let share = full
            .keys()
            .map(|&(r, k)| (0..4).filter(|&c| kept.contains(&(r, k, c))).count())
            .sum::<usize>() as f64
            / f64::from(listed);
        assert!(
            (share - BOULDER_KEEP_FRACTION).abs() < 0.05,
            "kept {share} of the clusters, the fraction {BOULDER_KEEP_FRACTION}"
        );
    }

    /// The cap is a guard: past it, the region keeps the first boulders and counts the rest.
    #[test]
    fn the_boulder_cap_guard_counts_its_overflow() {
        use crate::worldgen::generate::water::cap_boulders;
        let g = rapids(150.0, true);
        let all = g.boulders().to_vec();
        assert!(all.len() > 100);
        let mut capped = all.clone();
        assert_eq!(cap_boulders(&mut capped, 100), all.len() as u32 - 100);
        assert_eq!(&capped[..], &all[..100]);
        let mut whole = all.clone();
        assert_eq!(cap_boulders(&mut whole, all.len()), 0);
        assert_eq!(whole, all);
    }

    /// Fix round 2: the region keeps a boulder only where the drawn water (the water shader's
    /// own quantities, `blended_water` over the region's texels) holds it, and draws its collar
    /// only where the drawn water holds the collar's footprint. Real widths: a narrow steep river
    /// (Q 20 m³/s, 22 m) and a 61 m one (Q 150), each on a 3% bed along a line oblique to the
    /// lattice, so the course kinks at every node; and the straight equator rapid. Non-vacuous:
    /// the generator's rocks near the banks stand outside the drawn water in each fixture.
    #[test]
    fn placed_boulders_and_collars_lie_in_the_drawn_water() {
        use crate::terrain::drawn::{collar_footprint, flow_local};
        let frame = RegionFrame {
            center: dir(20.0, 30.0),
        };
        let oblique = |q: f32| {
            let (hydro, floor) = rivers_along(
                CubeLattice { n: 900 },
                frame,
                &[-15_000.0, 37_000.0],
                q,
                0.03,
            );
            let field = Box::leak(Box::new(FnField {
                f: floor,
                spacing_m: 1852.0,
                koppen: Some(26),
            }));
            let hydro = Box::leak(Box::new(hydro));
            let c = frame.local_to_dir(20_000.0, 0.0);
            // The real apron resolution: the drawn water blends toward the apron over four of
            // its texels from the inner field's edge.
            let g: RegionGenerator<'static, _> =
                RegionGenerator::new(spec_at(c, 0.0, 1025, 513), &*field, Some(&*hydro));
            let unplaced: Vec<Boulder> = clusters_of(&g).into_iter().flat_map(|(_, b)| b).collect();
            (unplaced, g.finish())
        };
        let straight = || {
            let (hydro, floor) = equator_river(150.0, |a| 3600.0 - 0.024 * a, 0.05);
            let field = FnField {
                f: floor,
                spacing_m: 1852.0,
                koppen: Some(26),
            };
            let g = RegionGenerator::new(
                spec_at(LAT.dir(eq(405)), 0.0, 1025, 513),
                &field,
                Some(&hydro),
            );
            let unplaced: Vec<Boulder> = clusters_of(&g).into_iter().flat_map(|(_, b)| b).collect();
            (unplaced, g.finish())
        };
        for (name, (unplaced, grid), kinked) in [
            ("narrow, kinked", oblique(20.0), true),
            ("61 m, kinked", oblique(150.0), true),
            ("61 m, straight", straight(), false),
        ] {
            let basis = grid.spec.frame().basis();
            let local = |b: &Boulder| basis.dir_to_local(RenderDir(b.dir)).unwrap();
            let outside = |b: &Boulder| {
                let (x, z) = local(b);
                let w = blended_water(&grid, x, z);
                w.thickness_m <= 0.0 || w.ribbon_m < b.radius_m
            };
            // The hazard: the rocks as sized on the routing geometry, away from the blend band,
            // that the drawn water doesn't hold (where the course kinks between the 293 m texels).
            let interior = |b: &Boulder| {
                let (x, z) = local(b);
                crate::terrain::drawn::drawn_from_the_inner_field(&grid, x, z)
            };
            let hazard = unplaced
                .iter()
                .filter(|b| interior(b) && outside(b))
                .count();
            println!(
                "{name}: {} generated, {hazard} outside the drawn water, {} placed",
                unplaced.len(),
                grid.boulders.len()
            );
            assert!(unplaced.len() > 200, "{name}: {} boulders", unplaced.len());
            if kinked {
                assert!(
                    hazard > 10,
                    "{name}: only {hazard} rocks outside the drawn water"
                );
            }
            // Not too many lost: the blend band is a quarter of the square.
            assert!(
                grid.boulders.len() * 3 > unplaced.len(),
                "{name}: {} of {} kept",
                grid.boulders.len(),
                unplaced.len()
            );
            let mut collars = 0;
            for b in &grid.boulders {
                let (x, z) = local(b);
                let w = blended_water(&grid, x, z);
                assert!(
                    w.thickness_m > 0.0,
                    "{name}: a rock on dry ground: {b:?} {w:?}"
                );
                assert!(
                    w.ribbon_m >= b.radius_m,
                    "{name}: a rock {} m across a ribbon of {} m: {b:?}",
                    b.radius_m,
                    w.ribbon_m
                );
                if b.collar {
                    collars += 1;
                    assert!(b.foam_site().is_some());
                    let down = flow_local(&basis, b.dir, b.flow_mps);
                    for p in collar_footprint(x, z, b.radius_m, down) {
                        let w = blended_water(&grid, p.x, p.y);
                        assert!(
                            w.thickness_m > 0.0 && w.ribbon_m >= 0.0,
                            "{name}: a collar spills onto dry ground at {p}: {w:?}"
                        );
                    }
                }
            }
            assert!(collars > 20, "{name}: {collars} collars");
        }
    }

    /// The continent's pass and a dense region at `inner`/`apron` resolution.
    fn continent_region(
        inner: u32,
        apron: u32,
    ) -> (
        EraHydrology,
        RegionSpec,
        impl Fn(RenderDir) -> f64 + Sync + Copy,
    ) {
        let c0 = DVec3::X;
        let f = move |d: RenderDir| continent(c0, d);
        let hydro = pass_over(419, f);
        let centre = densest_rivers(&hydro, RIVER_MIN_DISCHARGE_M3S as f32);
        (hydro, spec_at(centre, 0.0, inner, apron), f)
    }

    #[test]
    fn grid_texels_equal_sample_texel_at_their_directions() {
        let (hydro, spec, f) = continent_region(129, 65);
        let field = FnField {
            f,
            spacing_m: 1852.0,
            koppen: None,
        };
        let g = RegionGenerator::new(spec, &field, Some(&hydro));
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let frame = spec.frame();
        let mut rivers = 0;
        for (kind, f) in [
            (FieldKind::Inner, &grid.inner),
            (FieldKind::Apron, &grid.apron),
        ] {
            for row in 0..f.res {
                for col in 0..f.res {
                    let (x, z) = f.local_of(row, col);
                    let i = (row * f.res + col) as usize;
                    let t = g.sample_texel_in(kind, frame.local_to_dir(x, z));
                    assert_eq!(f.elevation_m[i].to_bits(), t.h.to_bits());
                    assert_eq!(f.surface[i], t.surface);
                    assert_eq!(f.water_level_m[i].to_bits(), t.water_level_m.to_bits());
                    assert_eq!(f.river[i], t.river);
                    rivers += usize::from(t.river != RIVER_NONE);
                }
            }
        }
        assert!(rivers > 100, "{rivers} river texels");
        assert_eq!(
            grid.boulders,
            crate::terrain::drawn::region_boulders(&grid, g.boulders())
        );
    }

    /// Walks every refined centreline in the inner field (away from the apron blend)
    /// downstream at 50 m steps. The water rule evaluated at the centreline
    /// (`sample_texel_in`) never rises. The grid's bilinear read (`blended_water`) never rises
    /// by more than 1e-3 m, except within two texels of a real bend: a vertex where the
    /// centreline turns by more than 5° (meanders, confluences, kinks at nodes). There the
    /// texels on either side of the bend take the level of different legs (each texel holds the
    /// level of its nearest point), so the read can rise by up to the level's fall over a few
    /// texels; the bound there is `1e-3 + 4 × texel × slope`, `slope` the steeper of the two
    /// sub-segments' level slopes. Segment joins count as vertices (the down segment's first
    /// sub-segment is looked up); a river's end into the sea or a lake does not. Reads whose
    /// four texels include one whose water is not a river (at a mouth, the sea's level 0 beside
    /// the channel) mix two waters and are not bounded; they are counted.
    #[test]
    fn water_never_flows_uphill_along_a_refined_centreline() {
        // Task 5's tree in its own valleys: confluences and meanders.
        let (hydro, f) = valley_tree();
        let field = FnField {
            f,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(LAT.dir(eq(410)), 0.0, 1025, 513);
        let (straight, bends) = assert_never_uphill(spec, &field, &hydro);
        assert!(
            straight > 2000 && bends > 2000,
            "{straight} straight, {bends} bends"
        );
        // The pass's rivers on the wet continent, where the edge-minimum relaxation made
        // hairpins before fix round 1.
        let (hydro, spec, f) = continent_region(1025, 513);
        let field = FnField {
            f,
            spacing_m: 1852.0,
            koppen: None,
        };
        let (straight, bends) = assert_never_uphill(spec, &field, &hydro);
        assert!(straight > 20_000, "{straight} straight, {bends} bends");
    }

    /// [`water_never_flows_uphill_along_a_refined_centreline`]'s walk; returns the steps on
    /// straight reaches and near bends.
    fn assert_never_uphill<F: CoarseField>(
        spec: RegionSpec,
        field: &F,
        hydro: &EraHydrology,
    ) -> (usize, usize) {
        let g = RegionGenerator::new(spec, field, Some(hydro));
        let rivers: &RegionRivers = g.rivers(FieldKind::Inner).unwrap();
        let grid = RegionGenerator::new(spec, field, Some(hydro)).finish();
        let basis = spec.frame().basis();
        let texel = grid.inner.spacing_m();
        let inner_only = REGION_SIZE_M / 2.0 - 4.0 * grid.apron.spacing_m() - texel;
        // Sub-segments by segment, in flow order.
        let mut segments: Vec<Vec<(DVec3, DVec3, f32, f32)>> = Vec::new();
        let mut last_river = None;
        for (a, b) in rivers.sub_segments() {
            if last_river != Some(a.river) {
                segments.push(Vec::new());
                last_river = Some(a.river);
            }
            segments
                .last_mut()
                .expect("pushed")
                .push((a.dir, b.dir, a.level_m, b.level_m));
        }
        let metres = |p: DVec3, q: DVec3| p.angle_between(q) * EARTH_RADIUS_M;
        let turn = |p: DVec3, q: DVec3, r: DVec3| {
            let t0 = (q - p) - q * q.dot(q - p);
            let t1 = (r - q) - q * q.dot(r - q);
            t0.angle_between(t1).to_degrees()
        };
        let slope = |s: &(DVec3, DVec3, f32, f32)| f64::from(s.2 - s.3) / metres(s.0, s.1).max(1.0);
        // Each segment's first sub-segment, by its start's bits: a segment's last sub-segment
        // ends exactly there.
        let first: std::collections::HashMap<[u64; 3], (DVec3, DVec3, f32, f32)> = segments
            .iter()
            .map(|seg| (seg[0].0.to_array().map(f64::to_bits), seg[0]))
            .collect();
        let (mut point_steps, mut straight, mut bends) = (0usize, 0usize, 0usize);
        let (mut mixed_steps, mut last_mixed) = (0usize, false);
        let (mut worst_straight, mut worst_bend) = (f32::NEG_INFINITY, f64::NEG_INFINITY);
        for seg in &segments {
            let mut last: Option<(f32, f32)> = None;
            for (j, sub) in seg.iter().enumerate() {
                let (a, b) = (sub.0, sub.1);
                let n = (metres(a, b) / 50.0).ceil().max(1.0) as u32;
                // The turn at either end, and the steeper slope beside it: none at a river's
                // end, and none at a segment's start, which the upstream segment's walk saw.
                let start =
                    (j > 0).then(|| (turn(seg[j - 1].0, a, b), slope(&seg[j - 1]).max(slope(sub))));
                let next = seg
                    .get(j + 1)
                    .or_else(|| first.get(&b.to_array().map(f64::to_bits)));
                let end = next.map(|nx| (turn(a, b, nx.1), slope(sub).max(slope(nx))));
                for i in 0..n {
                    let p = chord_point(a, b, f64::from(i) / f64::from(n));
                    let Some((x, z)) = basis.dir_to_local(RenderDir(p)) else {
                        continue;
                    };
                    if x.abs() > inner_only || z.abs() > inner_only {
                        last = None;
                        continue;
                    }
                    let point = g
                        .sample_texel_in(FieldKind::Inner, RenderDir(p))
                        .water_level_m;
                    let read = blended_water(&grid, x, z).level_m;
                    // The four texels of the bilinear read.
                    let (u, v) = (
                        ((x + REGION_SIZE_M / 2.0) / texel).floor() as usize,
                        ((z + REGION_SIZE_M / 2.0) / texel).floor() as usize,
                    );
                    let res = grid.inner.res as usize;
                    let mixed = [(v, u), (v, u + 1), (v + 1, u), (v + 1, u + 1)]
                        .iter()
                        .any(|&(r, c)| grid.inner.river[r * res + c] == RIVER_NONE);
                    let near = |v: DVec3, bend: Option<(f64, f64)>| {
                        let (deg, s) = bend?;
                        (deg > 5.0 && metres(p, v) < 2.0 * texel).then_some(s)
                    };
                    let bend_slope = [near(a, start), near(b, end)]
                        .into_iter()
                        .flatten()
                        .fold(-1.0f64, f64::max);
                    if let Some((point_before, read_before)) = last {
                        assert!(
                            point <= point_before,
                            "the rule rises: {point_before} -> {point} at ({x:.0}, {z:.0})"
                        );
                        point_steps += 1;
                        let rise = read - read_before;
                        if mixed || last_mixed {
                            mixed_steps += 1;
                        } else if bend_slope < 0.0 {
                            worst_straight = worst_straight.max(rise);
                            assert!(rise <= 1e-3, "{read_before} -> {read} at ({x:.0}, {z:.0})");
                            straight += 1;
                        } else {
                            let bound = 1e-3 + 4.0 * texel * bend_slope.min(1.0);
                            worst_bend = worst_bend.max(f64::from(rise) / bound);
                            assert!(
                                f64::from(rise) <= bound,
                                "{read_before} -> {read} at ({x:.0}, {z:.0}), bound {bound}"
                            );
                            bends += 1;
                        }
                    }
                    last = Some((point, read));
                    last_mixed = mixed;
                }
            }
        }
        println!(
            "{point_steps} steps: {straight} straight (worst rise {worst_straight} m), {bends} near \
             bends (worst {worst_bend:.2} of the bound), {mixed_steps} beside standing water"
        );
        (straight, bends)
    }

    // ---- A sub-texel river: a hand-made chain on Task 5's n = 819 equator row. ----

    const N: u32 = 819;
    const ROW: u32 = 409;
    const LAT: CubeLattice = CubeLattice { n: N };

    fn eq(col: u32) -> u32 {
        LAT.node(CubeFace::PosX, col, ROW)
    }

    /// Metres east along the equator from the PosX face centre (east is render −Z there).
    fn along_m(d: DVec3) -> f64 {
        (-d.z).atan2(d.x) * EARTH_RADIUS_M
    }

    fn north_m(d: DVec3) -> f64 {
        d.y.clamp(-1.0, 1.0).asin() * EARTH_RADIUS_M
    }

    /// A river east along the equator row from column 380 to 440, of discharge `q`, on a bed
    /// `bed(along)`, in a V valley `wall` steep around it.
    fn equator_river(
        q: f32,
        bed: fn(f64) -> f64,
        wall: f64,
    ) -> (EraHydrology, impl Fn(RenderDir) -> f64 + Sync + Copy) {
        let cols: Vec<u32> = (380..=440).collect();
        let hydro = river_hydro(
            LAT,
            &[RiverChain {
                nodes: cols.iter().map(|&c| eq(c)).collect(),
                q: vec![q; cols.len()],
                bed: cols
                    .iter()
                    .map(|&c| bed(along_m(LAT.dir(eq(c)).0)) as f32)
                    .collect(),
                end: Down::Ocean(eq(441)),
            }],
            vec![],
        );
        let floor = move |d: RenderDir| bed(along_m(d.0)) + 5.0 + wall * north_m(d.0).abs();
        (hydro, floor)
    }

    fn gentle_bed(a: f64) -> f64 {
        800.0 - 1e-3 * a
    }

    /// Task 5's tree in valleys of its own: a meandering main river east along the equator
    /// row (columns 380-440, Q 2,000-3,300, slope 1e-4), a tributary joining at column 400
    /// from row 395 and one at column 420 from row 424 (40 m per node), each in a V valley,
    /// so the relaxation keeps every river on its valley axis.
    fn valley_tree() -> (EraHydrology, impl Fn(RenderDir) -> f64 + Sync + Copy) {
        let main_bed = |a: f64| 2000.0 - 1e-4 * (a - along_m(LAT.dir(eq(380)).0));
        let cols: Vec<u32> = (380..=440).collect();
        let main = RiverChain {
            nodes: cols.iter().map(|&c| eq(c)).collect(),
            q: cols
                .iter()
                .map(|&c| {
                    2000.0 + if c >= 400 { 900.0 } else { 0.0 } + if c >= 420 { 400.0 } else { 0.0 }
                })
                .collect(),
            bed: cols
                .iter()
                .map(|&c| main_bed(along_m(LAT.dir(eq(c)).0)) as f32)
                .collect(),
            end: Down::Ocean(eq(441)),
        };
        let per_m = 40.0 / LAT.mean_spacing_m();
        let trib = |col: u32, from_row: u32, step: usize, count: u32, q: f32| {
            let nodes = lattice_walk(&LAT, LAT.node(CubeFace::PosX, col, from_row), step, count);
            let at = main_bed(along_m(LAT.dir(eq(col)).0));
            RiverChain {
                q: vec![q; nodes.len()],
                bed: nodes
                    .iter()
                    .map(|&v| (at + per_m * north_m(LAT.dir(v).0).abs()) as f32)
                    .collect(),
                nodes,
                end: Down::River(eq(col)),
            }
        };
        let (ta, tb) = (trib(400, 395, 6, 13, 900.0), trib(420, 424, 2, 14, 400.0));
        let side = |c: &RiverChain| north_m(LAT.dir(c.nodes[0]).0).signum();
        let tribs = [
            (along_m(LAT.dir(eq(400)).0), side(&ta)),
            (along_m(LAT.dir(eq(420)).0), side(&tb)),
        ];
        let hydro = river_hydro(LAT, &[main, ta, tb], vec![]);
        let floor = move |d: RenderDir| {
            let (a, n) = (along_m(d.0), north_m(d.0));
            let mut f = main_bed(a) + 5.0 + 0.05 * n.abs();
            for (ac, s) in tribs {
                if n * s > 0.0 {
                    f = f.min(main_bed(ac) + per_m * n.abs() + 5.0 + 0.05 * (a - ac).abs());
                }
            }
            f
        };
        (hydro, floor)
    }

    #[test]
    fn a_sub_texel_river_is_drawn_by_its_ribbon() {
        let (hydro, floor) = equator_river(20.0, gentle_bed, 0.05);
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        // 586 m texels; the river runs 200 m north of a texel row, between rows.
        let centre = RegionFrame {
            center: LAT.dir(eq(410)),
        }
        .local_to_dir(0.0, 200.0);
        let spec = spec_at(centre, 0.0, 513, 513);
        let g = RegionGenerator::new(spec, &field, Some(&hydro));
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let rivers = g.rivers(FieldKind::Inner).unwrap();
        let w = 2.0 * rivers.subpoints()[0].half_width_m;
        assert!((w - 22.36).abs() < 0.01, "w {w}");
        let basis = spec.frame().basis();
        let texel = grid.inner.spacing_m();
        let (mut inside, mut checked) = (0, 0);
        for k in 0..40 {
            // Cross-sections along the river, at sub-point and in-between positions.
            let x0 = -100_000.0 + 5_123.0 * f64::from(k);
            for i in -120..=120 {
                let z = -200.0 + 0.5 * f64::from(i);
                let p = basis.local_to_dir(x0, z);
                // The exact distance to the centreline, from the point query.
                let exact = rivers
                    .texel(p, 1000.0, texel)
                    .hit
                    .expect("in the valley")
                    .offset_m
                    .abs();
                let s = blended_water(&grid, x0, z);
                checked += 1;
                if s.ribbon_m > 0.0 {
                    inside += 1;
                    assert!(
                        exact <= w / 2.0 + 0.25,
                        "ribbon {} at {exact} m from the centre",
                        s.ribbon_m
                    );
                    assert!(s.thickness_m > 0.0, "water over the channel: {s:?}");
                }
                if exact < w / 2.0 - 0.25 {
                    assert!(s.ribbon_m > 0.0, "drawn at {exact} m: {s:?}");
                }
            }
            // The hazard: the texels either side hold negative ribbons, so interpolating the
            // ribbon itself would draw nothing.
            let (u, v) = (
                ((x0 + REGION_SIZE_M / 2.0) / texel).floor() as usize,
                ((-200.0 + REGION_SIZE_M / 2.0) / texel).floor() as usize,
            );
            let n = grid.inner.res as usize;
            for (r, c) in [(v, u), (v, u + 1), (v + 1, u), (v + 1, u + 1)] {
                let [ribbon, ..] = decode_river(grid.inner.river[r * n + c]);
                assert!(ribbon < 0.0, "texel ({r}, {c}) ribbon {ribbon}");
            }
        }
        assert!(
            inside > 40 * 30 && checked == 40 * 241,
            "{inside} of {checked}"
        );
    }

    #[test]
    fn an_inland_basin_below_sea_level_is_not_sea() {
        // A -28 m bowl 40 km across in the dry subtropics (22° N) on a 500 m dome 1,900 km in
        // radius, over 1,100 km from the ocean: a terminal basin (a wet climate would fill it to
        // its rim, where the sea's level could not show anyway).
        let c0 = dir(22.0, 30.0).0;
        let bowl = RegionFrame {
            center: RenderDir(c0),
        }
        .local_to_dir(600_000.0, 0.0);
        let land = move |d: RenderDir| {
            let base = 500.0 * (1.0 - d.0.angle_between(c0) / 0.3);
            let r = d.0.angle_between(bowl.0) * EARTH_RADIUS_M;
            let t = ((r - 20_000.0) / 40_000.0).clamp(0.0, 1.0);
            -28.0 * (1.0 - t) + base * t
        };
        let hydro = pass_over(419, land);
        let field = FnField {
            f: land,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(bowl, 0.0, 129, 513);
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let bare = RegionGenerator::new(spec, &field, None).finish();
        let lakes: Vec<f32> = hydro.lakes.iter().map(|l| l.level_m).collect();
        let (mut below, mut lake, mut dry) = (0, 0, 0);
        for i in 0..grid.inner.elevation_m.len() {
            let level = grid.inner.water_level_m[i];
            let h = grid.inner.elevation_m[i];
            assert_ne!(level, 0.0, "texel {i} is sea at h {h}");
            assert!(
                lakes.contains(&level)
                    || level == h - NO_WATER_DROP_M as f32
                    || grid.inner.river[i] != RIVER_NONE,
                "texel {i}: level {level} at h {h}"
            );
            // The hazard: without the pass, these texels are sea.
            if bare.inner.water_level_m[i] == 0.0 {
                below += 1;
                lake += usize::from(lakes.contains(&level));
                dry += usize::from(level == h - NO_WATER_DROP_M as f32);
            }
        }
        println!("{below} texels under the bowl's corners: {lake} lake, {dry} dry");
        assert!(below > 50, "{below} texels under the bowl's corners");
        // The bowl is not filled to its rim, where the sea's level could not show anyway.
        assert!(
            lakes.iter().all(|&l| l < 0.0) || dry > 0,
            "the basin must be dry or below sea level: lakes {lakes:?}"
        );
    }

    #[test]
    fn the_sea_band_covers_every_shoreline() {
        // An east-west coast along the equator at the PosX face centre (land north), with
        // bays and headlands 2-5 km across reaching 8 km in, on the recent lattice (n = 900,
        // about 11 km here): much of the shoreline is water in cells whose four lattice taps
        // are land.
        let coast = RenderDir(DVec3::X);
        let lattice = CubeLattice { n: 900 };
        // The mean coast 3 km south of a row of lattice nodes, so bays reach past that row
        // into cells whose four taps are land.
        let row_m = north_m(lattice.dir(lattice.nearest(coast)).0);
        let shore = move |d: RenderDir| {
            let east = along_m(d.0);
            let wobble = 5_000.0 * (east / 900.0).sin() + 3_000.0 * (east / 430.0 + 1.0).sin();
            // Metres north of the coastline, lightly sloped.
            let north = north_m(d.0) - (row_m - 3_000.0) - wobble;
            if north > 0.0 {
                0.004 * north + 2.0
            } else {
                0.01 * north - 2.0
            }
        };
        let hydro = pass_over(lattice.n, shore);
        let field = FnField {
            f: shore,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(coast, 0.0, 513, 33);
        let g = RegionGenerator::new(spec, &field, Some(&hydro));
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let frame = spec.frame();
        let f = &grid.inner;
        let (mut mixed, mut sub_cell) = (0, 0);
        for row in 0..f.res {
            for col in 0..f.res {
                let (x, z) = f.local_of(row, col);
                let d = frame.local_to_dir(x, z);
                let (_, c) = g.coarse_and_corners(FieldKind::Inner, d);
                if c.iter().all(|&v| v < 0.0) || c.iter().all(|&v| v > 0.0) {
                    continue;
                }
                let i = (row * f.res + col) as usize;
                let (h, level) = (f.elevation_m[i], f.water_level_m[i]);
                mixed += 1;
                // Under sea level, water stands over the ground; above it, only a river or a
                // lake may (sign(level − h) == sign(−h) for the sea and dry land).
                if h < 0.0 {
                    assert!(level > h, "({row}, {col}): h {h}, level {level}");
                } else if f.river[i] == RIVER_NONE && level != 0.0 {
                    assert!(level < h, "({row}, {col}): h {h}, level {level}");
                } else if f.river[i] == RIVER_NONE {
                    assert_eq!(
                        (level - h).signum(),
                        (-h).signum(),
                        "({row}, {col}): h {h}, sea"
                    );
                }
                if h < 0.0 && !hydro.ocean_tap(d) {
                    sub_cell += 1;
                }
            }
        }
        // The hazard: shoreline water the lattice's four taps call land.
        assert!(
            mixed > 500 && sub_cell >= 10,
            "{mixed} mixed, {sub_cell} sub-cell"
        );
    }

    /// Review fix (round 2): on the deep lattice (n = 180, 51 km spacing) the pass's strait
    /// reach is one step, so a below-sea basin two steps from the ocean is inland. Texels in the
    /// cell between the coastal land row and the basin row must not get the sea from the ocean
    /// node beyond the coastal row.
    #[test]
    fn a_basin_two_deep_steps_inland_is_not_sea() {
        let lattice = CubeLattice { n: 180 };
        // An island in the dry band (22° N) on the PosX face, rows -1 to 3 and 41 columns: a
        // coastal land row, the basin row (17 columns at -300 m, two steps from the ocean),
        // a rim, and rows draining north to the ocean, so the basin's catchment is its own rim
        // and it stays a terminal (dry or low) basin.
        let centre = lattice.nearest(dir(22.0, 90.0));
        let (face, c_col, c_row) = lattice.face_col_row(centre);
        assert_eq!(face, CubeFace::PosX);
        let north_row = |r: u32| north_m(lattice.dir(lattice.node(face, c_col, r)).0);
        let step: i64 = if north_row(c_row + 1) > north_row(c_row) {
            1
        } else {
            -1
        };
        let k_of = |v: u32| {
            let (f, col, row) = lattice.face_col_row(v);
            (
                f,
                (i64::from(row) - i64::from(c_row)) * step,
                i64::from(col) - i64::from(c_col),
            )
        };
        let node_e = move |v: u32| -> f64 {
            match k_of(v) {
                (f, _, _) if f != CubeFace::PosX => -200.0,
                (_, k, c) if !(-1..=3).contains(&k) || c.abs() > 20 => -200.0,
                (_, -1, _) => 50.0,
                (_, 0, c) if c.abs() <= 8 => -300.0,
                (_, 0, _) | (_, 1, _) => 60.0,
                (_, 2, _) => 40.0,
                _ => 20.0,
            }
        };
        let land = move |d: RenderDir| {
            lattice
                .taps(d)
                .iter()
                .map(|&(v, w)| w * node_e(v))
                .sum::<f64>()
        };
        let hydro = pass_over(lattice.n, land);
        let basin = centre;
        let row_node = |k: i64| {
            lattice.node(
                face,
                c_col,
                u32::try_from(i64::from(c_row) + k * step).unwrap(),
            )
        };
        assert!(!hydro.is_ocean(basin), "the basin is inland");
        assert!(!hydro.is_ocean(row_node(-1)), "the coastal row is land");
        assert!(hydro.is_ocean(row_node(-2)), "the ocean row");
        // The basin's lake, if any, is below sea level: where the sea would outrank it.
        assert!(
            hydro.lakes.iter().all(|l| l.level_m < 0.0),
            "{:?}",
            hydro.lakes
        );
        let field = FnField {
            f: land,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        // Centred on the cell between the coastal row and the basin.
        let mid = RenderDir((lattice.dir(row_node(-1)).0 + lattice.dir(basin).0).normalize());
        let spec = spec_at(mid, 0.0, 129, 513);
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let bare = RegionGenerator::new(spec, &field, None).finish();
        let frame = spec.frame();
        let f = &grid.inner;
        let (mut inland_below, mut old_rule_sea) = (0, 0);
        for row in 0..f.res {
            for col in 0..f.res {
                let (x, z) = f.local_of(row, col);
                let d = frame.local_to_dir(x, z);
                if hydro.ocean_tap(d) {
                    continue;
                }
                let i = (row * f.res + col) as usize;
                if bare.inner.water_level_m[i] != 0.0 {
                    continue;
                }
                inland_below += 1;
                // The hazard: an ocean node among the taps' neighbours.
                let near = lattice.taps(d).iter().any(|&(v, _)| {
                    lattice
                        .neighbours(v)
                        .iter()
                        .flatten()
                        .any(|&u| hydro.is_ocean(u))
                });
                old_rule_sea += usize::from(near);
                assert_ne!(
                    f.water_level_m[i], 0.0,
                    "({row}, {col}) is sea over the basin"
                );
            }
        }
        assert!(
            inland_below > 50 && old_rule_sea > 50,
            "{inland_below} inland texels below sea level, {old_rule_sea} by the taps' neighbours"
        );
    }

    /// 2% beds: rapids along the whole river.
    fn steep_bed(a: f64) -> f64 {
        3000.0 - 0.02 * a
    }

    #[test]
    fn boulders_lie_in_rapids_and_agree_across_regions() {
        let (hydro, floor) = equator_river(150.0, steep_bed, 0.05);
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let a = LAT.dir(eq(405));
        let b = RegionFrame { center: a }.local_to_dir(100_000.0, 0.0);
        let ga = RegionGenerator::new(spec_at(a, 0.0, 1025, 33), &field, Some(&hydro));
        let gb = RegionGenerator::new(spec_at(b, 0.0, 1025, 33), &field, Some(&hydro));
        let half = REGION_SIZE_M / 2.0;
        for g in [&ga, &gb] {
            let rivers = g.rivers(FieldKind::Inner).unwrap();
            let texel = REGION_SIZE_M / 1024.0;
            assert!(g.boulders().len() > 500, "{} boulders", g.boulders().len());
            for bo in g.boulders() {
                let hit = rivers
                    .texel(RenderDir(bo.dir), 5000.0, texel)
                    .hit
                    .expect("in a valley");
                assert!(
                    hit.ribbon_m >= bo.radius_m - 1e-3,
                    "in the channel: {hit:?} {bo:?}"
                );
                let rapid = rivers.subpoints().iter().any(|s| {
                    s.rapid > 0.0
                        && s.dir.angle_between(bo.dir) * EARTH_RADIUS_M
                            <= SUBPOINT_M + f64::from(s.half_width_m)
                });
                assert!(rapid, "{bo:?} near a rapid");
                let t = g.sample_texel_in(FieldKind::Inner, RenderDir(bo.dir));
                assert_eq!(bo.base_m.to_bits(), t.h.to_bits(), "on the channel bed");
                assert!((0.0..=1.0).contains(&bo.wet));
                assert!(bo.base_m < t.water_level_m, "under the river: {bo:?} {t:?}");
            }
        }
        let in_square = |g: &RegionGenerator<_>, d: DVec3| {
            g.spec()
                .frame()
                .dir_to_local(RenderDir(d))
                .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half)
        };
        let key = |bo: &Boulder| (bo.seed, bo.dir.to_array().map(f64::to_bits));
        let shared = |g: &RegionGenerator<_>, other: &RegionGenerator<_>| {
            let mut v: Vec<Boulder> = g
                .boulders()
                .iter()
                .copied()
                .filter(|bo| in_square(other, bo.dir))
                .collect();
            v.sort_by_key(key);
            v
        };
        let (sa, sb) = (shared(&ga, &gb), shared(&gb, &ga));
        assert!(sa.len() > 300, "{} shared boulders", sa.len());
        assert_eq!(sa.len(), sb.len());
        for (x, y) in sa.iter().zip(&sb) {
            assert_eq!(
                x.dir.to_array().map(f64::to_bits),
                y.dir.to_array().map(f64::to_bits)
            );
            assert_eq!(
                (
                    [
                        x.base_m,
                        x.radius_m,
                        x.wet,
                        x.level_m,
                        x.flow_mps[0],
                        x.flow_mps[1]
                    ]
                    .map(f32::to_bits),
                    x.seed,
                ),
                (
                    [
                        y.base_m,
                        y.radius_m,
                        y.wet,
                        y.level_m,
                        y.flow_mps[0],
                        y.flow_mps[1]
                    ]
                    .map(f32::to_bits),
                    y.seed,
                )
            );
        }
        assert!(
            ga.boulders().len() > sa.len(),
            "the sets differ outside the overlap"
        );
    }

    /// The equator river of discharge `q` on a bed that makes it a full rapid (`full`, a 2.4%
    /// bed: `rapid` = 1) or a half one (1.3%: `rapid` ≈ 0.5).
    fn rapids(
        q: f32,
        full: bool,
    ) -> RegionGenerator<'static, FnField<impl Fn(RenderDir) -> f64 + Sync + Copy>> {
        let bed: fn(f64) -> f64 = if full {
            |a| 3600.0 - 0.024 * a
        } else {
            |a| 3000.0 - 0.013 * a
        };
        let (hydro, floor) = equator_river(q, bed, 0.05);
        let field = Box::leak(Box::new(FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        }));
        let hydro = Box::leak(Box::new(hydro));
        RegionGenerator::new(spec_at(LAT.dir(eq(405)), 0.0, 1025, 33), field, Some(hydro))
    }

    /// The generator's boulder clusters, each with its boulders inside the region as sized
    /// (before the drawn-water hold), rebuilt as the generator builds them (its centre's bed
    /// first); the held ones, in cluster order, are checked to be the generator's boulders.
    fn clusters_of<F: CoarseField>(
        g: &RegionGenerator<F>,
    ) -> Vec<(
        crate::worldgen::generate::water::BoulderCluster,
        Vec<Boulder>,
    )> {
        use crate::worldgen::generate::water::{
            BoulderBed, DrawnTexel, boulder_clusters, drawn_cells, hold_in_drawn_water,
            size_cluster,
        };
        let rivers = g.rivers(FieldKind::Inner).unwrap();
        let frame = g.spec().frame();
        let half = REGION_SIZE_M / 2.0;
        let texel_m = REGION_SIZE_M / f64::from(g.spec().inner_res - 1);
        let bed = |d: DVec3| {
            let t = g.sample_texel_in(FieldKind::Inner, RenderDir(d));
            BoulderBed {
                base_m: t.h,
                level_m: t.water_level_m,
                river: t.river,
            }
        };
        let inside = |b: &Boulder| {
            frame
                .dir_to_local(RenderDir(b.dir))
                .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half)
        };
        let mut held_all = Vec::new();
        let out: Vec<_> = boulder_clusters(rivers, &frame.basis(), half)
            .into_iter()
            .map(|c| {
                let centre = bed(c.centre());
                let spread = c.spread_m(centre.depth_m());
                let dirs = c.member_dirs(spread);
                let beds: Vec<BoulderBed> = dirs.iter().map(|&d| bed(d)).collect();
                assert_eq!(beds[0], centre);
                let sized = size_cluster(&c, spread, &dirs, &beds);
                let cells = drawn_cells(c.centre(), texel_m);
                let texels: Vec<[DrawnTexel; 4]> = cells
                    .iter()
                    .map(|cell| {
                        cell.corners.map(|d| {
                            let b = bed(d);
                            DrawnTexel {
                                h_m: b.base_m,
                                level_m: b.level_m,
                                river: b.river,
                            }
                        })
                    })
                    .collect();
                let held = hold_in_drawn_water(
                    *g.spec(),
                    c.centre(),
                    texel_m,
                    &cells,
                    &texels,
                    sized.clone(),
                );
                held_all.extend(held.into_iter().filter(inside));
                (c, sized.into_iter().filter(inside).collect())
            })
            .collect();
        assert_eq!(
            &held_all[..],
            g.boulders(),
            "the generator's boulders, in cluster order"
        );
        out
    }

    /// The sub-segments whose rapid lies in `range` and whose boulders all fall in the square
    /// (both ends 20 km inside it): `(river, k) → rapid`.
    fn rapid_subsegments<F: CoarseField>(
        g: &RegionGenerator<F>,
        range: std::ops::RangeInclusive<f32>,
    ) -> std::collections::BTreeMap<(u32, u32), f32> {
        let frame = g.spec().frame();
        let inside = |d: DVec3| {
            frame
                .dir_to_local(RenderDir(d))
                .is_some_and(|(x, z)| x.abs().max(z.abs()) < REGION_SIZE_M / 2.0 - 20_000.0)
        };
        g.rivers(FieldKind::Inner)
            .unwrap()
            .sub_segments()
            .filter(|(a, b)| range.contains(&a.rapid) && inside(a.dir) && inside(b.dir))
            .map(|(a, _)| ((a.river, a.k), a.rapid))
            .collect()
    }

    fn metres(a: DVec3, b: DVec3) -> f64 {
        a.angle_between(b) * EARTH_RADIUS_M
    }

    #[test]
    fn rapids_hold_clustered_boulders() {
        use crate::hydrology::params::{
            BOULDER_CLUSTER_MAX, BOULDER_CLUSTER_REACH, BOULDERS_PER_SUBPOINT_MAX,
        };
        use crate::worldgen::generate::water::cluster_kept;
        use std::collections::BTreeMap;
        for (full, range, counts) in [(true, 0.999..=1.0, 24..=24), (false, 0.4..=0.6, 9..=15)] {
            let g = rapids(150.0, full);
            let subs = rapid_subsegments(&g, range.clone());
            assert!(subs.len() > 50, "full {full}: {} sub-segments", subs.len());
            let mut by_sub: BTreeMap<(u32, u32), Vec<_>> = BTreeMap::new();
            for (c, boulders) in clusters_of(&g) {
                by_sub
                    .entry((c.river, c.k))
                    .or_default()
                    .push((c, boulders));
            }
            for (&(river, k), &rapid) in &subs {
                let clusters = by_sub.get(&(river, k)).map(Vec::as_slice).unwrap_or(&[]);
                let h0 = f64::from(crate::worldgen::noise::hash3(
                    [river as i32, k as i32, -1],
                    0x5BD1_E995,
                )) / 4_294_967_296.0;
                let n = (f64::from(rapid) * f64::from(BOULDERS_PER_SUBPOINT_MAX) + h0).floor();
                let n = n as usize;
                assert!(counts.contains(&n), "({river}, {k}): {n}");
                // Clusters of at most six, ceil(n / 6) of them before thinning, each around its
                // largest boulder; the thinning keeps whole ones (fix round 1).
                let max = BOULDER_CLUSTER_MAX as usize;
                let kept: Vec<usize> = (0..n.div_ceil(max))
                    .filter(|&c| cluster_kept(river, k, c as u32))
                    .map(|c| (n - c * max).min(max))
                    .collect();
                let held: usize = clusters.iter().map(|(_, b)| b.len()).sum();
                assert_eq!(
                    held,
                    kept.iter().sum::<usize>(),
                    "({river}, {k}) at rapid {rapid}"
                );
                assert_eq!(clusters.len(), kept.len(), "({river}, {k})");
                for (c, boulders) in clusters {
                    assert!(boulders.len() <= max && boulders.len() == c.members.len());
                    let centre = boulders[0];
                    assert_eq!(centre.dir, c.centre(), "the centre comes first");
                    let big = f64::from(centre.radius_m);
                    for b in boulders {
                        assert!(b.radius_m <= centre.radius_m, "the largest at the centre");
                        let d = metres(b.dir, centre.dir);
                        assert!(
                            d <= BOULDER_CLUSTER_REACH * big * (1.0 + 1e-6) + 1e-3,
                            "a member {d} m from its centre, radius {big} m"
                        );
                    }
                    if boulders.len() > 1 {
                        assert!(
                            boulders[1..]
                                .iter()
                                .any(|b| metres(b.dir, centre.dir) > big),
                            "members spread around the centre"
                        );
                    }
                }
            }
        }
    }

    /// At `rapid` = 1 a third of each cluster's rocks stand above the water, wherever the cap
    /// lets them (water up to `2 × cap − BOULDER_BREAK_M[1]` deep over the cluster's beds).
    #[test]
    fn a_third_of_a_full_rapids_rocks_break_the_surface() {
        use crate::hydrology::params::BOULDER_BREAK_M;
        // A mid river and a deep one. The detail noise under the channel makes the water at
        // the beds deeper than the channel's own depth (1.9 and 5.1 m), up to tens of metres.
        for q in [150.0, 2_000.0] {
            let g = rapids(q, true);
            let full = rapid_subsegments(&g, 0.999..=1.0);
            assert!(full.len() > 50, "q {q}: {} full sub-segments", full.len());
            let (mut checked, mut too_deep) = (0, 0);
            for (c, boulders) in clusters_of(&g) {
                if !full.contains_key(&(c.river, c.k)) {
                    continue;
                }
                let deepest = boulders
                    .iter()
                    .map(|b| f64::from(b.level_m - b.base_m))
                    .fold(0.0, f64::max);
                if deepest > 2.0 * c.cap_m - BOULDER_BREAK_M[1] {
                    too_deep += 1;
                    continue;
                }
                checked += 1;
                let breaking = boulders
                    .iter()
                    .filter(|b| {
                        let t = g.sample_texel_in(FieldKind::Inner, RenderDir(b.dir));
                        assert_eq!(b.level_m.to_bits(), t.water_level_m.to_bits());
                        b.top_m() > b.level_m
                    })
                    .count();
                assert!(
                    3 * breaking >= boulders.len(),
                    "q {q}: {breaking} of {} break the surface",
                    boulders.len()
                );
            }
            println!("q {q}: {checked} clusters checked, {too_deep} in water too deep");
            assert!(checked > 100, "{checked} clusters");
            assert!(too_deep * 10 < checked, "{too_deep} too deep of {checked}");
        }
    }

    #[test]
    fn clustered_boulders_stay_in_the_channel() {
        // A narrow river (22 m: clusters wider than the channel are clamped), a mid one and a
        // wide one, at a full and a half rapid.
        for (q, full) in [(20.0, true), (150.0, false), (150.0, true), (2_000.0, true)] {
            let g = rapids(q, full);
            let rivers = g.rivers(FieldKind::Inner).unwrap();
            let texel = REGION_SIZE_M / 1024.0;
            assert!(
                g.boulders().len() > 500,
                "q {q}: {} boulders",
                g.boulders().len()
            );
            for bo in g.boulders() {
                let hit = rivers
                    .texel(RenderDir(bo.dir), 5000.0, texel)
                    .hit
                    .expect("in a valley");
                assert!(
                    hit.ribbon_m >= bo.radius_m - 1e-3,
                    "q {q}: wholly in the channel: {hit:?} {bo:?}"
                );
                assert!(bo.radius_m <= hit.half_width_m / 2.0 + 1e-3, "{bo:?}");
                let t = g.sample_texel_in(FieldKind::Inner, RenderDir(bo.dir));
                assert_eq!(bo.base_m.to_bits(), t.h.to_bits(), "on the channel bed");
                assert!(bo.base_m < t.water_level_m, "under the river: {bo:?} {t:?}");
            }
        }
    }

    #[test]
    fn foam_sites_mark_rocks_at_the_surface() {
        use crate::hydrology::params::FOAM_SITE_DEPTH_M;
        let g = rapids(150.0, false);
        let sites: Vec<FoamSite> = foam_sites(g.boulders()).collect();
        let (mut deep, mut breaking) = (0, 0);
        let mut it = sites.iter();
        for b in g.boulders() {
            let at_surface = b.base_m < b.level_m && b.top_m() >= b.level_m - FOAM_SITE_DEPTH_M;
            deep += usize::from(!at_surface);
            if !at_surface {
                assert_eq!(b.foam_site(), None);
                continue;
            }
            breaking += usize::from(b.top_m() > b.level_m);
            let s = it.next().expect("a site per rock at the surface");
            assert_eq!(
                (s.dir, s.seed, s.radius_m, s.top_m),
                (b.dir, b.seed, b.radius_m, b.top_m())
            );
            // Its level and flow are the water's at its boulder.
            let t = g.sample_texel_in(FieldKind::Inner, RenderDir(b.dir));
            assert_eq!(s.level_m.to_bits(), t.water_level_m.to_bits());
            let [_, east, north, _] = decode_river(t.river);
            assert_eq!(s.flow_mps, [east, north]);
            // The river runs east along the equator, at rapids speed.
            let speed = east.hypot(north);
            assert!(speed >= 3.0, "{s:?}");
            assert!(east > 0.9 * speed, "downstream is east: {s:?}");
        }
        assert!(it.next().is_none());
        assert!(
            sites.len() > 200 && breaking > 100 && deep > 100,
            "{} sites, {breaking} breaking, {deep} deep",
            sites.len()
        );
    }

    /// Every stored quantity's bits, boulders included.
    type AllBits = (GridBits, Vec<u32>, Vec<[u16; 4]>, Vec<u32>, Vec<Boulder>);

    fn all_bits(grid: &RegionGrid) -> AllBits {
        let levels = grid
            .inner
            .water_level_m
            .iter()
            .chain(&grid.apron.water_level_m)
            .map(|x| x.to_bits())
            .collect();
        let rivers = grid
            .inner
            .river
            .iter()
            .chain(&grid.apron.river)
            .copied()
            .collect();
        let boulder_bits = grid
            .boulders
            .iter()
            .flat_map(|b| {
                [
                    b.base_m.to_bits(),
                    b.radius_m.to_bits(),
                    b.seed,
                    b.wet.to_bits(),
                    b.level_m.to_bits(),
                    b.flow_mps[0].to_bits(),
                    b.flow_mps[1].to_bits(),
                ]
            })
            .collect();
        (
            bits(grid),
            levels,
            rivers,
            boulder_bits,
            grid.boulders.clone(),
        )
    }

    #[test]
    fn prep_in_steps_matches_new_with_a_pass() {
        let (hydro, floor) = equator_river(150.0, steep_bed, 0.05);
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(LAT.dir(eq(405)), 0.0, 65, 17);
        let generator = RegionGenerator::new(spec, &field, Some(&hydro));
        // The generator's boulders (at 4.7 km texels the drawn water holds none of them).
        assert!(!generator.boulders().is_empty(), "boulders");
        let whole = all_bits(&generator.finish());
        assert!(whole.2.iter().any(|&r| r != RIVER_NONE), "river texels");
        for max_nodes in [1, 97, 4096, u32::MAX] {
            let mut prep = RegionPrep::new(spec, &field, Some(&hydro));
            let mut steps = 0u64;
            loop {
                let before = prep.units_done();
                let done = prep.step(max_nodes);
                steps += 1;
                assert!(prep.units_done() - before <= u64::from(max_nodes));
                if done {
                    break;
                }
            }
            assert!(prep.step(max_nodes), "stays done");
            assert_eq!(
                all_bits(&prep.finish().finish()),
                whole,
                "steps of {max_nodes} ({steps} steps)"
            );
        }
        let mut prep = RegionPrep::new(spec, &field, Some(&hydro));
        prep.step(5000);
        assert_eq!(all_bits(&prep.finish().finish()), whole);
    }

    #[test]
    fn rows_carry_the_water_fields_in_any_order() {
        let (hydro, floor) = equator_river(150.0, steep_bed, 0.05);
        let field = FnField {
            f: floor,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(LAT.dir(eq(405)), 0.0, 65, 17);
        let g = RegionGenerator::new(spec, &field, Some(&hydro));
        let whole = all_bits(&RegionGenerator::new(spec, &field, Some(&hydro)).finish());
        let late = g.rows(FieldKind::Inner, 40..65);
        let early = g.rows(FieldKind::Inner, 0..40);
        let apron = vec![
            g.rows(FieldKind::Apron, 0..9),
            g.rows(FieldKind::Apron, 9..17),
        ];
        assert_eq!(all_bits(&g.assemble(vec![early, late], apron)), whole);
    }

    /// Counts the field's samples, delegating everything.
    struct Counting<'f, F: CoarseField> {
        inner: &'f F,
        tiles: std::sync::atomic::AtomicU64,
        overview: std::sync::atomic::AtomicU64,
    }

    impl<'f, F: CoarseField> Counting<'f, F> {
        fn new(inner: &'f F) -> Self {
            Counting {
                inner,
                tiles: Default::default(),
                overview: Default::default(),
            }
        }

        fn counts(&self) -> (u64, u64) {
            use std::sync::atomic::Ordering::Relaxed;
            (self.tiles.load(Relaxed), self.overview.load(Relaxed))
        }
    }

    impl<F: CoarseField> CoarseField for Counting<'_, F> {
        fn sample(&self, d: RenderDir) -> crate::worldgen::coarse::CoarseSample {
            self.tiles
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.sample(d)
        }

        fn sample_overview(&self, d: RenderDir) -> crate::worldgen::coarse::CoarseSample {
            self.overview
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.sample_overview(d)
        }

        fn source_spacing_m(&self) -> f64 {
            self.inner.source_spacing_m()
        }

        fn paleolat(&self, d: RenderDir) -> f64 {
            self.inner.paleolat(d)
        }

        fn lattice_key(
            &self,
            d: RenderDir,
            nearest: crate::worldgen::coarse::KeyKind,
        ) -> crate::worldgen::coarse::LatticeKey {
            self.inner.lattice_key(d, nearest)
        }
    }

    /// Rows of both fields across `threads` scoped threads in 16-row blocks (as
    /// `generate_parallel` does on the task pool); seconds.
    fn rows_parallel<F: CoarseField>(g: &RegionGenerator<F>, threads: usize) -> f64 {
        let t = std::time::Instant::now();
        let mut blocks: Vec<(FieldKind, std::ops::Range<u32>)> = Vec::new();
        for kind in [FieldKind::Inner, FieldKind::Apron] {
            let rows = g.rows_in(kind);
            for b in 0..rows.div_ceil(16) {
                blocks.push((kind, b * 16..((b + 1) * 16).min(rows)));
            }
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((kind, range)) = blocks.get(i).cloned() else {
                            break;
                        };
                        std::hint::black_box(g.rows(kind, range));
                    }
                });
            }
        });
        t.elapsed().as_secs_f64()
    }

    /// The generation time and memory rivers add on Task 5's river-dense region (the wet
    /// continent at n = 900, the densest 300 km square). Run with
    /// `cargo test --release -p timefly_core --lib water_generation_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run in release"]
    fn water_generation_timing() {
        use crate::hydrology::params::HYDRO_RECENT_N;
        use crate::worldgen::generate::rivers::river_read_reach_m;
        use std::time::Instant;
        let c0 = DVec3::X;
        let f = move |d: RenderDir| {
            continent(c0, d) + 40.0 * (d.0.y * 3000.0).sin() * (d.0.z * 2100.0).cos()
        };
        let t = Instant::now();
        let hydro = pass_over(HYDRO_RECENT_N, f);
        println!(
            "pass {:.2} s, {} river nodes",
            t.elapsed().as_secs_f64(),
            hydro.rivers.nodes().len()
        );
        let centre = densest_rivers(&hydro, RIVER_MIN_DISCHARGE_M3S as f32);
        let base = FnField {
            f,
            spacing_m: 1852.0,
            koppen: Some(26),
        };
        let spec = spec_at(centre, 0.0, 1025, 513);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        for run in 0..2 {
            println!("--- run {run}");
            let mut rows_single = [0.0; 2];
            let mut rows_par = [0.0; 2];
            let mut prep_s = [0.0; 2];
            let mut samples = [(0u64, 0u64); 2];
            let mut bytes = [0usize; 2];
            for (k, hydro) in [None, Some(&hydro)].into_iter().enumerate() {
                let field = Counting::new(&base);
                let t = Instant::now();
                let g = RegionGenerator::new(spec, &field, hydro);
                prep_s[k] = t.elapsed().as_secs_f64();
                samples[k] = field.counts();
                let t = Instant::now();
                let inner = g.rows(FieldKind::Inner, 0..1025);
                let apron = g.rows(FieldKind::Apron, 0..513);
                rows_single[k] = t.elapsed().as_secs_f64();
                rows_par[k] = rows_parallel(&g, threads);
                let grid = g.assemble(vec![inner], vec![apron]);
                bytes[k] = grid.bytes();
                if let Some(h) = hydro {
                    let (ri, ra) = (
                        g.rivers(FieldKind::Inner).unwrap(),
                        g.rivers(FieldKind::Apron).unwrap(),
                    );
                    let river_texels = grid
                        .inner
                        .river
                        .iter()
                        .filter(|&&r| r != RIVER_NONE)
                        .count();
                    let apron_river = grid
                        .apron
                        .river
                        .iter()
                        .filter(|&&r| r != RIVER_NONE)
                        .count();
                    println!(
                        "rivers: inner {} sub-points ({:.1} MiB), apron {} sub-points ({:.1} MiB); \
                         {} boulders ({} dropped); river texels inner {river_texels}, apron {apron_river}; \
                         working bytes {:.1} MiB; pass {:.1} MiB",
                        ri.subpoints().len(),
                        ri.bytes() as f64 / 1048576.0,
                        ra.subpoints().len(),
                        ra.bytes() as f64 / 1048576.0,
                        grid.boulders.len(),
                        g.boulders_dropped(),
                        g.working_bytes() as f64 / 1048576.0,
                        h.bytes() as f64 / 1048576.0,
                    );
                }
            }
            println!(
                "prep: {:.1} ms -> {:.1} ms (+{:.1}); rows single-threaded {:.1} -> {:.1} ms (+{:.1}); \
                 rows on {threads} threads {:.1} -> {:.1} ms (+{:.1})",
                prep_s[0] * 1e3,
                prep_s[1] * 1e3,
                (prep_s[1] - prep_s[0]) * 1e3,
                rows_single[0] * 1e3,
                rows_single[1] * 1e3,
                (rows_single[1] - rows_single[0]) * 1e3,
                rows_par[0] * 1e3,
                rows_par[1] * 1e3,
                (rows_par[1] - rows_par[0]) * 1e3,
            );
            println!(
                "field samples (tiles, overview): {:?} -> {:?}: on demand +{} tile-level, +{} overview-level",
                samples[0],
                samples[1],
                samples[1].0 - samples[0].0,
                samples[1].1 - samples[0].1
            );
            println!(
                "RegionGrid::bytes {} -> {} ({:.2} -> {:.2} MiB)",
                bytes[0],
                bytes[1],
                bytes[0] as f64 / 1048576.0,
                bytes[1] as f64 / 1048576.0
            );
        }
        // The web: RegionPrep stepped at WEB_PREP_STEP (256) units; the slowest step with rivers.
        for budget in [64u32, 256, 1024] {
            for (label, h) in [("no pass", None), ("pass", Some(&hydro))] {
                let mut prep = RegionPrep::new(spec, &base, h);
                let (mut worst, mut steps, mut total) = (0.0f64, 0u32, 0.0f64);
                loop {
                    let t = Instant::now();
                    let done = prep.step(budget);
                    let dt = t.elapsed().as_secs_f64();
                    total += dt;
                    steps += 1;
                    if done {
                        break;
                    }
                    worst = worst.max(dt);
                }
                println!(
                    "step({budget}) {label}: {steps} steps, total {:.1} ms, slowest {:.2} ms (before the final transform)",
                    total * 1e3,
                    worst * 1e3
                );
            }
        }
        // Task 5's centreline geometry here: long sub-segments and sharp turns.
        {
            let g = RegionGenerator::new(spec, &base, Some(&hydro));
            let subs: Vec<(DVec3, DVec3, u32)> = g
                .rivers(FieldKind::Inner)
                .unwrap()
                .sub_segments()
                .map(|(a, b)| (a.dir, b.dir, a.river))
                .collect();
            let len = |a: DVec3, b: DVec3| a.angle_between(b) * EARTH_RADIUS_M;
            let long = subs
                .iter()
                .filter(|s| len(s.0, s.1) > 2.0 * SUBPOINT_M)
                .count();
            // Rivers at least 182 m wide meander (λ = 11 w ≥ 2 km).
            let meandering = g
                .rivers(FieldKind::Inner)
                .unwrap()
                .sub_segments()
                .filter(|(a, b)| len(a.dir, b.dir) > 2.0 * SUBPOINT_M && a.half_width_m >= 91.0)
                .count();
            // Where along their segments the long sub-segments sit.
            {
                let rr = g.rivers(FieldKind::Inner).unwrap();
                let mut max_k: std::collections::HashMap<u32, u32> = Default::default();
                for sp in rr.subpoints() {
                    let e = max_k.entry(sp.river).or_insert(0);
                    *e = (*e).max(sp.k);
                }
                let (mut near_start, mut near_end, mut middle, mut jumps) = (0, 0, 0, 0);
                let mut longest_by = [0.0f64; 3];
                for (a, b) in rr.sub_segments() {
                    if len(a.dir, b.dir) <= 2.0 * SUBPOINT_M {
                        continue;
                    }
                    let l = len(a.dir, b.dir);
                    if a.k <= 3 {
                        near_start += 1;
                        longest_by[0] = longest_by[0].max(l);
                    } else if a.k + 3 >= max_k[&a.river] {
                        near_end += 1;
                        longest_by[1] = longest_by[1].max(l);
                    } else {
                        middle += 1;
                        longest_by[2] = longest_by[2].max(l);
                    }
                    // A middle leg that crosses the chord: the minimum switched sides.
                    let n = &hydro.rivers.nodes()[a.river as usize];
                    let (c0, c1) = (
                        hydro.lattice.dir(n.node).0,
                        hydro
                            .lattice
                            .dir(match n.down {
                                crate::hydrology::Down::River(j) => {
                                    hydro.rivers.nodes()[j as usize].node
                                }
                                crate::hydrology::Down::Ocean(v)
                                | crate::hydrology::Down::Lake(_, v)
                                | crate::hydrology::Down::Ice(v)
                                | crate::hydrology::Down::Sink(v) => v,
                            })
                            .0,
                    );
                    let nrm = c0.cross(c1).normalize();
                    jumps += usize::from(a.dir.dot(nrm).signum() != b.dir.dot(nrm).signum());
                }
                println!(
                    "long legs: {near_start} within 3 sub-points of a start, {near_end} of an end, \
                     {middle} mid-segment (longest {:.0} / {:.0} / {:.0} m); {jumps} cross the \
                     coarse line",
                    longest_by[0], longest_by[1], longest_by[2]
                );
            }
            let over = subs
                .iter()
                .filter(|s| len(s.0, s.1) > 1.5 * SUBPOINT_M)
                .count();
            println!(
                "long sub-segments on meandering rivers: {meandering} of {long}; longer than 1.5 \
                 sub-points: {over}"
            );
            let longest = subs.iter().map(|s| len(s.0, s.1)).fold(0.0, f64::max);
            let mut hairpins = 0;
            for w in subs.windows(2) {
                if w[0].1 == w[1].0 {
                    let q = w[0].1;
                    let t0 = (q - w[0].0) - q * q.dot(q - w[0].0);
                    let t1 = (w[1].1 - q) - q * q.dot(w[1].1 - q);
                    hairpins += usize::from(t0.angle_between(t1).to_degrees() > 150.0);
                }
            }
            println!(
                "centrelines: {} sub-segments, {long} longer than 2 sub-points (longest {longest:.0} m), \
                 {hairpins} turns over 150 degrees within a segment",
                subs.len()
            );
        }
        // The gathers (one unit each, no terrain reads).
        for (label, half, sub, q) in [
            (
                "inner",
                REGION_SIZE_M / 2.0,
                SUBPOINT_M,
                RIVER_MIN_DISCHARGE_M3S,
            ),
            (
                "apron",
                APRON_SIZE_M / 2.0,
                SUBPOINT_M.max(APRON_SIZE_M / 1024.0),
                crate::hydrology::params::APRON_RIVER_MIN_DISCHARGE_M3S,
            ),
        ] {
            let t = Instant::now();
            let b =
                super::super::rivers::RiverBuilder::new(&hydro, &spec.frame(), half, sub, q as f32);
            println!("{label} gather {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
            drop(b);
        }
        // Widening the inner lattice's precomputed square by the river reads' reach.
        let g = RegionGenerator::new(spec, &base, None);
        let inner = &g.lattices[g.layers[0].lattice];
        let reach = river_read_reach_m(&hydro.lattice, SUBPOINT_M);
        let nodes_of = |half: f64| {
            let mut l =
                lattice::Lattice::new(inner.n, crate::worldgen::tiles::SourceLevel::Tiles, half);
            while !l.planned() {
                l.plan(&spec.frame().basis(), u32::MAX);
            }
            l.blocks
                .iter()
                .flatten()
                .map(|b| u64::from(b.w) * u64::from(b.h))
                .sum::<u64>()
        };
        let half = REGION_SIZE_M / 2.0
            + 1852.0 / 2.0
            + 2.0 * (1852.0f64 / 4.0).max(REGION_SIZE_M / 1024.0);
        let (now, wide) = (nodes_of(half), nodes_of(half + reach));
        println!(
            "inner lattice: half {:.0} m, {now} nodes ({:.1} MiB); widened by {reach:.0} m: {wide} nodes ({:.1} MiB), +{} nodes",
            half,
            now as f64 * 8.0 / 1048576.0,
            wide as f64 * 8.0 / 1048576.0,
            wide - now
        );
    }

    // ---- Lake walls (Task 8b). ----

    /// A lake on a tilted plane: footprint = the n = 300 lattice's nodes within `radius_m` of a
    /// point `offset` (east, north m) from the region's centre, at `rise_m` over the plane there.
    #[derive(Copy, Clone, Debug)]
    struct TiltedLake {
        offset: (f64, f64),
        radius_m: f64,
        rise_m: f64,
        slope: f64,
        bearing: f64,
        data: bool,
    }

    /// The region (inner and apron at 129 x 129) over a tilted lake: `(grid, lake level, the
    /// same region without the pass)`.
    fn tilted_lake_region(t: TiltedLake) -> (RegionGrid, f32, RegionGrid) {
        use crate::hydrology::{Lake, LakeKind};
        let centre = dir(12.0, 21.0);
        let frame = RegionFrame { center: centre };
        let (north, east) = frame.north_east();
        let (c, s) = (t.bearing.cos(), t.bearing.sin());
        let plane = move |d: RenderDir| {
            let p = d.0.normalize();
            let (x, z) = (p.dot(east) * EARTH_RADIUS_M, p.dot(north) * EARTH_RADIUS_M);
            900.0 + t.slope * (x * c + z * s)
        };
        let middle = frame.local_to_dir(t.offset.0, -t.offset.1);
        let level = (plane(middle) + t.rise_m) as f32;
        let lattice = CubeLattice { n: 300 };
        let nodes: Vec<(u32, u16)> = (0..lattice.len())
            .filter(|&v| lattice.dir(v).0.angle_between(middle.0) * EARTH_RADIUS_M < t.radius_m)
            .map(|v| (v, 0))
            .collect();
        assert!(!nodes.is_empty(), "{t:?}");
        let lake = Lake {
            level_m: level,
            spill_m: level,
            kind: if t.data {
                LakeKind::Data
            } else {
                LakeKind::Open
            },
            inflow_m3s: 100.0,
            evaporation_m3s: 10.0,
            outflow_m3s: 90.0,
            area_km2: 1000.0,
            exit: nodes[0].0,
            surplus_m3s: 0.0,
        };
        let hydro = river_hydro(lattice, &[], vec![lake]).with_lake_nodes(&nodes);
        let field = FnField {
            f: plane,
            spacing_m: 1852.0,
            koppen: Some(8),
        };
        let spec = spec_at(centre, 0.0, 129, 129);
        let grid = RegionGenerator::new(spec, &field, Some(&hydro)).finish();
        let bare = RegionGenerator::new(spec, &field, None).finish();
        (grid, level, bare)
    }

    /// Lake texels (water at `level`) with a 4-neighbour that is dry with ground below
    /// `level − 0.5 m`, over both fields, as `(field, row, col)`; and how many lake texels and
    /// texels whose ground the pass raised above the bare region's there are.
    fn lake_walls(
        grid: &RegionGrid,
        level: f32,
        bare: &RegionGrid,
    ) -> (Vec<(u8, u32, u32)>, usize, usize) {
        let (mut walls, mut lake, mut raised) = (Vec::new(), 0, 0);
        for (k, (f, b)) in [(&grid.inner, &bare.inner), (&grid.apron, &bare.apron)]
            .into_iter()
            .enumerate()
        {
            let n = f.res;
            let at = |r: u32, c: u32| (r * n + c) as usize;
            for r in 0..n {
                for c in 0..n {
                    let i = at(r, c);
                    raised += usize::from(f.elevation_m[i] > b.elevation_m[i] + 0.25);
                    if f.water_level_m[i] != level {
                        continue;
                    }
                    lake += 1;
                    let near = [
                        (r.wrapping_sub(1), c),
                        (r + 1, c),
                        (r, c.wrapping_sub(1)),
                        (r, c + 1),
                    ];
                    for (rr, cc) in near.into_iter().filter(|&(rr, cc)| rr < n && cc < n) {
                        let j = at(rr, cc);
                        let h = f.elevation_m[j];
                        let dry = f.water_level_m[j] == h - NO_WATER_DROP_M as f32;
                        if dry && h < level - 0.5 {
                            walls.push((k as u8, r, c));
                        }
                    }
                }
            }
        }
        (walls, lake, raised)
    }

    #[test]
    fn a_lake_edge_over_lower_ground_is_dammed() {
        // 3 m per km toward the east: the lake's east reach ends 100+ m above the ground.
        let t = TiltedLake {
            offset: (-20_000.0, 7_000.0),
            radius_m: 60_000.0,
            rise_m: 120.0,
            slope: 0.003,
            bearing: 0.2,
            data: false,
        };
        let (grid, level, bare) = tilted_lake_region(t);
        let (walls, lake, raised) = lake_walls(&grid, level, &bare);
        println!("{lake} lake texels, {raised} raised, {} walls", walls.len());
        assert!(lake > 500, "{lake} lake texels");
        // The hazard: ground below the level beyond the lake's reach, so the dam engages.
        assert!(raised > 20, "{raised} texels raised");
        assert!(
            walls.is_empty(),
            "walls at {:?}",
            &walls[..walls.len().min(10)]
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn lake_water_never_stands_beside_lower_dry_ground(
            ox in -60_000.0f64..60_000.0,
            oz in -60_000.0f64..60_000.0,
            radius_m in 36_000.0f64..110_000.0,
            rise_m in -40.0f64..300.0,
            slope in 0.0005f64..0.008,
            bearing in 0.0f64..std::f64::consts::TAU,
            data in any::<bool>(),
        ) {
            let t = TiltedLake { offset: (ox, oz), radius_m, rise_m, slope, bearing, data };
            let (grid, level, bare) = tilted_lake_region(t);
            let (walls, _, _) = lake_walls(&grid, level, &bare);
            prop_assert!(walls.is_empty(), "{:?}: {} walls, first {:?}", t, walls.len(),
                &walls[..walls.len().min(5)]);
        }
    }
}
