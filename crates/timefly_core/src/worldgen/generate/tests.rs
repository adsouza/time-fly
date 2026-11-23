use super::*;
use crate::cube::{CubeFace, texel_center_dir};
use crate::formats::Grid;
use crate::geodynamics::{CrustModel, PlateIndex};
use crate::globe::{PlateRotations, plate_rotations, rendered_surface};
use crate::surface::FrameSelection;
use crate::worldgen::coarse::{CoarseSample, LatticeKey};
use crate::worldgen::coarse::{DeepField, RecentField, SnapshotField, snapshot_field};
use crate::worldgen::testkit::*;
use crate::worldgen::tiles::TileSet;
use bevy_math::DVec3;
use proptest::prelude::*;

/// A synthetic coarse field: elevation from a function of the direction, geographic keys.
struct FnField<G: Fn(RenderDir) -> f64 + Sync> {
    f: G,
    spacing_m: f64,
    koppen: Option<u8>,
}

impl<G: Fn(RenderDir) -> f64 + Sync> CoarseField for FnField<G> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        CoarseSample {
            elevation_m: (self.f)(era_dir),
            ice: 0.0,
            koppen: self.koppen,
            lattice: LatticeKey::Geographic(era_dir.0),
        }
    }

    fn source_spacing_m(&self) -> f64 {
        self.spacing_m
    }
}

fn spec_at(centre: RenderDir, ma: f64, inner_res: u32, apron_res: u32) -> RegionSpec {
    RegionSpec {
        center: centre,
        snapshot: YearsBp::from_myr(ma),
        seed: 7,
        inner_res,
        apron_res,
    }
}

fn generate<F: CoarseField>(spec: RegionSpec, field: &F) -> RegionGrid {
    RegionGenerator::new(spec, field).finish()
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
        let mut g = RegionGenerator::new(spec, &field);
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
    let g = RegionGenerator::new(spec, &field);
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
    let ga = RegionGenerator::new(spec_at(a, 0.021, 33, 17), &recent);
    let gb = RegionGenerator::new(spec_at(b, 0.021, 33, 17), &recent);
    assert_eq!(ga.sample_point(probe), gb.sample_point(probe));
    // Apron texels too (from the overview), even far apart and with different tiles loaded.
    let only_a = {
        let mut t = flat_tiles(a, 350.0, 26);
        t.set_overview(flat_overview(340.0, 26)).unwrap();
        t
    };
    let recent_a = RecentField::new(&only_a, [&era, &era], 0.0, &present, false).unwrap();
    let ga = RegionGenerator::new(spec_at(a, 0.021, 33, 17), &recent_a);
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
    let ga = RegionGenerator::new(spec_at(a, 100.0, 33, 17), &field);
    let gb = RegionGenerator::new(spec_at(b, 100.0, 33, 17), &field);
    for k in 0..20 {
        let p = RegionFrame { center: a }.local_to_dir(f64::from(k) * 6_000.0, -30_000.0);
        let (ha, sa) = ga.sample_point(p);
        let (hb, sb) = gb.sample_point(p);
        assert_eq!(ha.to_bits(), hb.to_bits(), "probe {k}");
        assert_eq!(sa[1..], sb[1..]);
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
    let globe_land = |d: RenderDir| {
        rendered_surface(&crust, d, &sampling, &rotations, |_, _| {
            crate::globe::FrameSample {
                elevation_m: 500.0,
                covered: true,
            }
        })
        .is_land()
    };
    // Straddling plate 202's western edge (render z ~ 0.3).
    let spec = spec_at(dir(0.0, 72.0), 100.0, 33, 33);
    let g = RegionGenerator::new(spec, &field);
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
    let g = RegionGenerator::new(spec, &field);
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
        assert_eq!(s[3], SURFACE_SHORE * u8::from((*h as f64) <= SHORE_BAND_M));
    }
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
    let g = RegionGenerator::new(spec_at(dir(45.0, 7.0), 0.0, 1025, 513), &field);
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
    let g = RegionGenerator::new(spec_at(centre, 100.0, 33, 129), &field);
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
    let field = Field {
        res: 3,
        size_m: 200.0,
        elevation_m: vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0],
        surface: vec![[0; 4]; 9],
    };
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
        let whole = bits(&RegionGenerator::new(spec, &field).finish());
        for max_nodes in [1, 97, u32::MAX] {
            let mut prep = RegionPrep::new(spec, &field);
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
        let mut prep = RegionPrep::new(spec, &field);
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

/// Steps a prep through with `max_nodes` per step; returns (slowest step before the last,
/// the last step (the coast-distance transform), number of steps), in seconds.
fn prep_profile<F: CoarseField>(spec: RegionSpec, field: &F, max_nodes: u32) -> (f64, f64, u64) {
    let mut prep = RegionPrep::new(spec, field);
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

fn print_profile<F: CoarseField>(label: &str, spec: RegionSpec, field: &F) {
    let t = std::time::Instant::now();
    let _ = RegionPrep::new(spec, field);
    println!(
        "  {label}: RegionPrep::new {:.3} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    for max_nodes in [256, 1024, 4096] {
        let (worst, last, steps) = prep_profile(spec, field, max_nodes);
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
    let g = RegionGenerator::new(spec, &field);
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
    print_profile("deep", spec, &field);
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
        let g = RegionGenerator::new(RegionSpec::new(centre, YearsBp(21_000.0), 1), &recent);
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
        print_profile(
            label,
            RegionSpec::new(centre, YearsBp(21_000.0), 1),
            &recent,
        );
    }
}
