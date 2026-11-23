//! Region generation over the real bake (`crates/timefly/assets/data/`), for diagnosis. Ignored:
//! the bake is git-ignored and large. Run in release:
//! `cargo test --release -p timefly_core --test real_bake_region -- --ignored --nocapture`.

use std::fs;
use std::path::{Path, PathBuf};

use timefly_core::cube::bilinear_taps;
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::{Grid, read_tfgrid, read_tfplates, read_tfrot};
use timefly_core::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use timefly_core::geodynamics::{CrustModel, PlateIndex};
use timefly_core::globe::{
    DeepSampling, FrameSample, PlateRotations, RenderedSurface, plate_rotations, rendered_surface,
};
use timefly_core::surface::{FrameMeta, FrameSelection, FrameSource, Timeline};
use timefly_core::worldgen::coarse::{CoarseField, frames_needed, snapshot_field};
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{RegionGenerator, RegionSpec};
use timefly_core::worldgen::tiles::TileSet;

fn data() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../timefly/assets/data")
}

struct Bake {
    crust: CrustModel,
    timeline: Timeline,
    files: Vec<String>,
}

fn bake(dir: &Path) -> Bake {
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let footprints = read_tfplates(&fs::read(dir.join("plates.tfplates")).unwrap()).unwrap();
    let rotations = read_tfrot(&fs::read(dir.join("rotations.tfrot")).unwrap()).unwrap();
    let crust = CrustModel::new(rotations, footprints).unwrap();
    let frames = manifest["frames"].as_array().unwrap();
    let mut files = Vec::new();
    let metas = frames
        .iter()
        .enumerate()
        .map(|(i, f)| {
            files.push(f["file"].as_str().unwrap().to_string());
            FrameMeta {
                age_myr: f["age_myr"].as_f64().unwrap(),
                source: match f["source"].as_str().unwrap() {
                    "ice6g" => FrameSource::Ice6g,
                    _ => FrameSource::PaleoDem {
                        source_rotation_age_myr: f["source_rotation_age_myr"].as_f64().unwrap(),
                    },
                },
                image_slot: i as u32,
            }
        })
        .collect();
    let curve = manifest["sea_level_curve"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| (k[0].as_f64().unwrap(), k[1].as_f64().unwrap()))
        .collect();
    Bake {
        crust,
        timeline: Timeline::new(metas, curve).unwrap(),
        files,
    }
}

fn cube_rg(grid: &Grid, d: RenderDir) -> FrameSample {
    let (mut e, mut c) = (0.0, 0.0);
    for ((face, col, row), w) in bilinear_taps(d, grid.height) {
        e += w * f64::from(grid.texel_f32(face as usize, row, col, 0));
        c += w * f64::from(grid.texel_f32(face as usize, row, col, 1));
    }
    FrameSample {
        elevation_m: e,
        covered: c > 0.5,
    }
}

fn label(s: &RenderedSurface) -> String {
    match s {
        RenderedSurface::Land {
            plate_id,
            elevation_m,
        } => format!("land(plate {}, {elevation_m:.0} m)", plate_id.0),
        RenderedSurface::GapLand { elevation_m } => format!("gap land({elevation_m:.0} m)"),
        RenderedSurface::Ocean => "ocean shell".to_string(),
    }
}

/// The Task 8 shot `era-10ma-modern` (35°N 36°E at 10 Ma): finds thin channels of water in dry
/// land and prints a transect across one with the coarse path behind each sample.
#[test]
#[ignore = "reads the real bake; run in release"]
fn ten_ma_channels() {
    let globe = data().join("globe");
    let b = bake(&globe);
    let age = 10.0;
    let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
    for i in frames_needed(&b.timeline, age) {
        grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
    }
    let mut tiles = TileSet::new();
    tiles
        .set_overview(
            read_tfgrid(&fs::read(data().join("tiles/overview.tfgrid")).unwrap()).unwrap(),
        )
        .unwrap();
    let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap();
    let centre = geo_to_render(latlon_to_geo(35.0, 36.0));
    let spec = RegionSpec {
        seed: 0x7F1E_0002,
        ..RegionSpec::new(centre, YearsBp::from_myr(age), 0)
    };
    let grid = RegionGenerator::new(spec, &field).finish();
    // The globe's reference at the same directions, with the same frames.
    let deep = match b.timeline.select_frames(age) {
        FrameSelection::Deep(d) => d,
        other => panic!("{other:?}"),
    };
    let sampling = DeepSampling::from_bracket(&b.timeline, age, &deep);
    let plates = b.crust.rotations().plate_ids.len() as u32;
    let rotations: Vec<PlateRotations> = (0..plates)
        .map(|p| plate_rotations(b.crust.rotations(), PlateIndex(p), &sampling))
        .collect();
    let frames = sampling.endpoints.map(|e| grids[e.frame].as_ref().unwrap());
    let reference = |d: RenderDir| {
        rendered_surface(&b.crust, d, &sampling, &rotations, |e, dd| {
            cube_rg(frames[e], dd)
        })
    };
    let frame = RegionFrame { center: centre };
    // Height steps between neighbouring texels, and the plates either side.
    for (name, f) in [("inner", &grid.inner), ("apron", &grid.apron)] {
        let n = f.res as usize;
        let h = |r: usize, c: usize| f.elevation_m[r * n + c];
        let kind = |r: usize, c: usize| {
            let (x, z) = f.local_of(r as u32, c as u32);
            field
                .sample_overview(frame.local_to_dir(x, z))
                .lattice
                .kind()
        };
        let (mut steps, mut across_plates) = (0, 0);
        let mut first = None;
        for r in 0..n {
            for c in 0..n - 1 {
                let jump = (h(r, c + 1) - h(r, c)).abs();
                if jump > 60.0 {
                    steps += 1;
                    if kind(r, c) != kind(r, c + 1) {
                        across_plates += 1;
                        first.get_or_insert((r, c, jump, kind(r, c), kind(r, c + 1)));
                    }
                }
            }
        }
        println!(
            "{name}: {steps} east-west steps > 60 m, {across_plates} where the plate changes; first {first:?}"
        );
        if let Some((r, c, ..)) = first {
            for dc in -3i64..=4 {
                let cc = (c as i64 + dc) as usize;
                let (x, z) = f.local_of(r as u32, cc as u32);
                let d = frame.local_to_dir(x, z);
                let (lat, lon) = geo_to_latlon(render_to_geo(d));
                let coarse = field.sample_overview(d);
                println!(
                    "    col {cc:4} ({lat:7.3}, {lon:7.3}): h {:8.1}  coarse {:8.1} {:?}  globe {}",
                    h(r, cc),
                    coarse.elevation_m,
                    coarse.lattice.kind(),
                    label(&reference(d))
                );
            }
        }
    }
    for (name, f) in [("inner", &grid.inner), ("apron", &grid.apron)] {
        let n = f.res as usize;
        let h = |r: usize, c: usize| f.elevation_m[r * n + c];
        // A channel texel: water with dry land (> 50 m) a few texels away on both sides, along
        // a row or a column.
        let mut channels = Vec::new();
        for r in 4..n - 4 {
            for c in 4..n - 4 {
                if h(r, c) >= 0.0 {
                    continue;
                }
                let across_cols = h(r, c - 4) > 50.0 && h(r, c + 4) > 50.0;
                let across_rows = h(r - 4, c) > 50.0 && h(r + 4, c) > 50.0;
                if across_cols || across_rows {
                    channels.push((r, c));
                }
            }
        }
        println!("{name}: {} channel texels", channels.len());
        let Some(&(r, c)) = channels.get(channels.len() / 2) else {
            continue;
        };
        println!("{name}: transect along row {r} across column {c}");
        for dc in -6i64..=6 {
            let cc = (c as i64 + dc) as usize;
            let (x, z) = f.local_of(r as u32, cc as u32);
            let d = frame.local_to_dir(x, z);
            let (lat, lon) = geo_to_latlon(render_to_geo(d));
            let coarse = field.sample_overview(d);
            println!(
                "  col {cc:4} ({lat:7.3}, {lon:7.3}): h {:8.1}  field {:8.1} {:?}  globe {}",
                h(r, cc),
                coarse.elevation_m,
                coarse.lattice.kind(),
                label(&reference(d))
            );
        }
    }
}

/// Global survey: how much of the globe at an age is ocean shell showing the covered-land
/// fallback (both endpoints land-valued, covered), and how far such points are from plate land.
#[test]
#[ignore = "reads the real bake; run in release"]
fn covered_land_fallback_survey() {
    let globe = data().join("globe");
    let b = bake(&globe);
    for age in [4.0, 10.0, 66.0, 150.0, 250.0, 400.0] {
        let deep = match b.timeline.select_frames(age) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let sampling = DeepSampling::from_bracket(&b.timeline, age, &deep);
        let grids = sampling
            .endpoints
            .map(|e| read_tfgrid(&fs::read(globe.join(&b.files[e.frame])).unwrap()).unwrap());
        let plates = b.crust.rotations().plate_ids.len() as u32;
        let rotations: Vec<PlateRotations> = (0..plates)
            .map(|p| plate_rotations(b.crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let rs = |d: RenderDir| {
            rendered_surface(&b.crust, d, &sampling, &rotations, |e, dd| {
                cube_rg(&grids[e], dd)
            })
        };
        let anomalies = sampling.anomalies_m();
        let (mut land, mut water, mut fallback, mut gap_land) = (0usize, 0usize, 0usize, 0usize);
        let mut near = [0usize; 4]; // fallback points with plate land within 10/25/50/100 km
        let step = 0.5;
        let mut lat: f64 = -89.75;
        while lat < 90.0 {
            let mut lon = -180.0;
            let w = lat.to_radians().cos();
            while lon < 180.0 {
                let d = geo_to_render(latlon_to_geo(lat, lon));
                match rs(d) {
                    RenderedSurface::Land { .. } => land += 1,
                    RenderedSurface::GapLand { .. } => gap_land += 1,
                    RenderedSurface::Ocean => {
                        let s = [0, 1].map(|e| cube_rg(&grids[e], d));
                        let landish = (0..2).all(|e| s[e].elevation_m - anomalies[e] > 0.0);
                        if landish && s.iter().all(|x| x.covered) {
                            fallback += 1;
                            let frame = RegionFrame { center: d };
                            for (k, r) in [10e3, 25e3, 50e3, 100e3].iter().enumerate() {
                                let hit = (0..16).any(|a| {
                                    let t = f64::from(a) * std::f64::consts::TAU / 16.0;
                                    let p = frame.local_to_dir(r * t.cos(), r * t.sin());
                                    matches!(rs(p), RenderedSurface::Land { .. })
                                });
                                if hit {
                                    near[k] += 1;
                                    break;
                                }
                            }
                        } else {
                            water += 1;
                        }
                    }
                }
                lon += step / w.max(0.05);
            }
            lat += step;
        }
        let total = (land + water + fallback + gap_land) as f64;
        println!(
            "{age:5} Ma: land {:.1}%, gap land {:.2}%, water {:.1}%, covered-land fallback {:.2}% \
             ({fallback} pts; plate land within 10 km {}, 25 km {}, 50 km {}, 100 km {})",
            100.0 * land as f64 / total,
            100.0 * gap_land as f64 / total,
            100.0 * water as f64 / total,
            100.0 * fallback as f64 / total,
            near[0],
            near[0] + near[1],
            near[0] + near[1] + near[2],
            near.iter().sum::<usize>(),
        );
    }
}

/// The two-level deep path over the real bake at every standard snapshot on it (50 ka, 100 ka,
/// 1 Ma) and near its oldest end (2 Ma): single-threaded generation time, and land/sea against
/// the globe's reference (plus the region's sliver rule) wherever the reference is clearly land
/// or sea (|elevation| > 150 m), across each inner field and apron. Fails on any sign miss.
#[test]
#[ignore = "reads the real bake; run in release"]
fn modern_path_on_the_real_bake() {
    let globe = data().join("globe");
    let b = bake(&globe);
    let mut total_misses = 0;
    for age in [0.05, 0.1, 1.0, 2.0] {
        total_misses += modern_path_at(&b, &globe, age);
    }
    assert_eq!(total_misses, 0, "sign misses where |reference| > 150 m");
}

/// [`modern_path_on_the_real_bake`] at one age; returns its sign misses.
fn modern_path_at(b: &Bake, globe: &Path, age: f64) -> usize {
    use timefly_core::globe::shell_surface_m;
    use timefly_core::worldgen::coarse::SnapshotField;
    let mut total = 0;
    let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
    for i in frames_needed(&b.timeline, age) {
        grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
    }
    let overview = read_tfgrid(&fs::read(data().join("tiles/overview.tfgrid")).unwrap()).unwrap();
    let deep = match b.timeline.select_frames(age) {
        FrameSelection::Deep(d) => d,
        other => panic!("{other:?}"),
    };
    let sampling = DeepSampling::from_bracket(&b.timeline, age, &deep);
    let plates = b.crust.rotations().plate_ids.len() as u32;
    let rotations: Vec<PlateRotations> = (0..plates)
        .map(|p| plate_rotations(b.crust.rotations(), PlateIndex(p), &sampling))
        .collect();
    let bridge = b.timeline.bridge_frame_index();
    for (name, lat, lon) in [
        ("east mediterranean", 35.0, 36.0),
        ("alps", 46.5, 10.0),
        ("scandinavia", 63.0, 15.0),
        ("aegean", 38.5, 25.0),
    ] {
        let centre = geo_to_render(latlon_to_geo(lat, lon));
        let frame = RegionFrame { center: centre };
        let mut tiles = TileSet::new();
        tiles.set_overview(overview.clone()).unwrap();
        let probe = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap();
        for (r, c) in probe.tiles_needed(&frame) {
            let path = data().join(format!("tiles/r{r:02}c{c:02}.tfgrid"));
            tiles
                .insert(r, c, read_tfgrid(&fs::read(path).unwrap()).unwrap())
                .unwrap();
        }
        let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap();
        let SnapshotField::Deep(df) = &field else {
            panic!("{age} Ma is a deep bracket")
        };
        let spec = RegionSpec {
            seed: 0x7F1E_0002,
            ..RegionSpec::new(centre, YearsBp::from_myr(age), 0)
        };
        let t0 = std::time::Instant::now();
        let grid = RegionGenerator::new(spec, &field).finish();
        let secs = t0.elapsed().as_secs_f64();
        let sample = |e: usize, d: RenderDir| {
            let mut s = cube_rg(grids[sampling.endpoints[e].frame].as_ref().unwrap(), d);
            if sampling.endpoints[e].frame == bridge {
                let (la, lo) = geo_to_latlon(render_to_geo(d));
                if let Some((h, _)) = tiles.sample_overview(la, lo) {
                    s.elevation_m = f64::from(h);
                }
            }
            s
        };
        let (mut checked, mut misses) = (0, Vec::new());
        let half = 1_200_000.0;
        for a in 0..=120 {
            for k in 0..=120 {
                let x = -half + 2.0 * half * f64::from(a) / 120.0;
                let z = -half + 2.0 * half * f64::from(k) / 120.0;
                let d = frame.local_to_dir(x, z);
                let rs = rendered_surface(&b.crust, d, &sampling, &rotations, sample);
                let reference = match rs {
                    RenderedSurface::Land { elevation_m, .. }
                    | RenderedSurface::GapLand { elevation_m } => elevation_m,
                    RenderedSurface::Ocean => df.sliver_land_m(d).unwrap_or_else(|| {
                        shell_surface_m(&sampling, [sample(0, d), sample(1, d)]).min(0.0)
                    }),
                };
                if reference.abs() <= 150.0 {
                    continue;
                }
                checked += 1;
                let region = field.sample_overview(d).elevation_m;
                if (region > 0.0) != (reference > 0.0) {
                    misses.push((x / 1000.0, z / 1000.0, reference, region, label(&rs)));
                }
            }
        }
        let low = |f: &timefly_core::worldgen::generate::Field| {
            f.elevation_m.iter().copied().fold(f32::MAX, f32::min)
        };
        total += misses.len();
        println!(
            "{age} Ma {name}: {:.2} s single-threaded, {} tiles; {checked} off-node checks, {} \
             sign misses; inner min {:.0} m",
            secs,
            tiles.len(),
            misses.len(),
            low(&grid.inner)
        );
        for m in misses.iter().take(5) {
            println!(
                "    miss at ({:.0}, {:.0}) km: reference {:.0}, region {:.0}, {}",
                m.0, m.1, m.2, m.3, m.4
            );
        }
    }
    total
}
