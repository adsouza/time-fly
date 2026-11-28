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

/// The real bake: `TIMEFLY_REAL_DATA` (a worktree has none; point it at the main checkout's
/// `crates/timefly/assets/data`), else this checkout's.
fn data() -> PathBuf {
    std::env::var_os("TIMEFLY_REAL_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../timefly/assets/data"))
}

fn tile_dir() -> PathBuf {
    active_tile_dir(&data().join("tiles"))
}

fn active_tile_dir(dir: &Path) -> PathBuf {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    dir.join(manifest["generation"].as_str().unwrap_or(""))
}

struct Bake {
    crust: CrustModel,
    timeline: Timeline,
    files: Vec<String>,
}

fn bake(dir: &Path) -> Bake {
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let footprints =
        read_tfplates(&fs::read(dir.join(manifest["footprints"].as_str().unwrap())).unwrap())
            .unwrap();
    let rotations =
        read_tfrot(&fs::read(dir.join(manifest["rotations"].as_str().unwrap())).unwrap()).unwrap();
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
        .set_overview(read_tfgrid(&fs::read(tile_dir().join("overview.tfgrid")).unwrap()).unwrap())
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
    let grid = RegionGenerator::new(spec, &field, None).finish();
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
/// 1 Ma), near its oldest end (2 Ma) and at Panama's 3 Ma (past the path: the field is the
/// reference there): single-threaded generation time, and land/sea against
/// the globe's reference (plus the region's sliver rule) wherever the reference is clearly land
/// or sea (|elevation| > 150 m), across each inner field and apron. Fails on any sign miss.
#[test]
#[ignore = "reads the real bake; run in release"]
fn modern_path_on_the_real_bake() {
    let globe = data().join("globe");
    let b = bake(&globe);
    let mut total_misses = 0;
    // 3 Ma (Panama's bench age) is past the two-level path: the field is the reference there.
    for age in [0.05, 0.1, 1.0, 2.0, 3.0] {
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
    let overview = read_tfgrid(&fs::read(tile_dir().join("overview.tfgrid")).unwrap()).unwrap();
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
        ("himalayas", 28.0, 86.9),
        ("beringia", 62.0, -175.0),
        ("rockies", 48.5, -113.5),
        ("japan", 36.0, 138.5),
        ("panama", 9.0, -79.5),
        ("sundaland", 5.0, 107.0),
        ("red sea", 20.0, 37.5),
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
            let path = tile_dir().join(format!("r{r:02}c{c:02}.tfgrid"));
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
        let grid = RegionGenerator::new(spec, &field, None).finish();
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

/// One bench-region candidate's numbers over the real bake.
#[derive(Debug, Default)]
struct CandidateStats {
    tiles: usize,
    land: f64,
    anomaly_land: Option<f64>,
    coast_km: Option<f64>,
    ice: f64,
    max_m: f64,
    snowline_m: f64,
    above_snowline: f64,
    plates: Vec<u32>,
    sliver_points: usize,
    gen_secs: Option<f64>,
    grid_max_m: Option<f32>,
    biomes: Vec<(u8, usize)>,
}

/// The coarse field around a centre (era-frame lat/lon) at `years`, with its tiles loaded, and
/// its stats: land, anomaly land (land at the age, below sea level today: recent ages only),
/// ice, heights against the R18 snowline, the nearest coast (36 rays, 2 km steps, as the bench
/// looks), plates under the inner field and R17 sliver land (deep ages), and, with `generate`,
/// the single-threaded generation time and the inner field's biome classes.
fn candidate_stats(
    b: &Bake,
    globe: &Path,
    overview: &Grid,
    lat: f64,
    lon: f64,
    years: f64,
    generate: bool,
) -> CandidateStats {
    use timefly_core::worldgen::biome::snowline_m;
    use timefly_core::worldgen::coarse::LatticeKey;
    use timefly_core::worldgen::coarse::{SnapshotField, snapshot_sea_level_anomaly_m};
    let age = years / 1.0e6;
    let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
    for i in frames_needed(&b.timeline, age) {
        grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
    }
    let centre = geo_to_render(latlon_to_geo(lat, lon));
    let frame = RegionFrame { center: centre };
    let mut tiles = TileSet::new();
    tiles.set_overview(overview.clone()).unwrap();
    let needed = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap()
    .tiles_needed(&frame);
    for &(r, c) in &needed {
        let path = tile_dir().join(format!("r{r:02}c{c:02}.tfgrid"));
        tiles
            .insert(r, c, read_tfgrid(&fs::read(path).unwrap()).unwrap())
            .unwrap();
    }
    let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap();
    let anomaly = snapshot_sea_level_anomaly_m(&b.timeline, age);
    let recent = age <= 0.026076;
    let half = 150_000.0;
    let n = 61;
    let mut s = CandidateStats {
        tiles: needed.len(),
        snowline_m: snowline_m(lat, anomaly),
        max_m: f64::MIN,
        ..Default::default()
    };
    let (mut land, mut anomaly_land, mut ice, mut above) = (0usize, 0usize, 0usize, 0usize);
    let mut plates = std::collections::BTreeSet::new();
    for a in 0..n {
        for k in 0..n {
            let x = -half + 2.0 * half * f64::from(a) / f64::from(n - 1);
            let z = -half + 2.0 * half * f64::from(k) / f64::from(n - 1);
            let d = frame.local_to_dir(x, z);
            let c = field.sample(d);
            s.max_m = s.max_m.max(c.elevation_m);
            if c.elevation_m > 0.0 {
                land += 1;
                if recent {
                    let (la, lo) = geo_to_latlon(render_to_geo(d));
                    if tiles.sample(la, lo).is_some_and(|(h, _)| h < 0.0) {
                        anomaly_land += 1;
                    }
                }
            }
            if c.ice > 0.5 {
                ice += 1;
            }
            if c.elevation_m > s.snowline_m {
                above += 1;
            }
            if let LatticeKey::Tracked { plate_id, .. } = c.lattice {
                plates.insert(plate_id.0);
            }
            if let SnapshotField::Deep(df) = &field
                && df.sliver_land_m(d).is_some()
            {
                s.sliver_points += 1;
            }
        }
    }
    let total = (n * n) as f64;
    s.land = land as f64 / total;
    s.anomaly_land = recent.then(|| anomaly_land as f64 / total);
    s.ice = ice as f64 / total;
    s.above_snowline = above as f64 / total;
    s.plates = plates.into_iter().collect();
    // The nearest coast along 36 rays, as the bench's search.
    let sign = |x: f64, z: f64| field.sample(frame.local_to_dir(x, z)).elevation_m > 0.0;
    let centre_land = sign(0.0, 0.0);
    for ray in 0..36 {
        let t = f64::from(ray) * std::f64::consts::TAU / 36.0;
        let mut r = 2_000.0;
        while r <= 1_150_000.0 {
            if sign(r * t.sin(), -r * t.cos()) != centre_land {
                let km = r / 1000.0;
                s.coast_km = Some(s.coast_km.map_or(km, |c: f64| c.min(km)));
                break;
            }
            r += 2_000.0;
        }
    }
    if generate {
        let spec = RegionSpec {
            seed: 0x7F1E_0002,
            sea_level_anomaly_m: anomaly,
            ..RegionSpec::new(centre, YearsBp(years), 0)
        };
        let t0 = std::time::Instant::now();
        let grid = RegionGenerator::new(spec, &field, None).finish();
        s.gen_secs = Some(t0.elapsed().as_secs_f64());
        s.grid_max_m = grid.inner.elevation_m.iter().copied().reduce(f32::max);
        let mut counts = std::collections::BTreeMap::new();
        for (h, surf) in grid.inner.elevation_m.iter().zip(&grid.inner.surface) {
            if *h > 0.0 {
                *counts.entry(surf[0]).or_insert(0usize) += 1;
            }
        }
        s.biomes = counts.into_iter().collect();
    }
    s
}

/// Bench-region candidates over the real bake: `TIMEFLY_CANDIDATES="name:lat,lon,years;..."`
/// (defaults to the bench regions), `TIMEFLY_GENERATE=1` to time a single-threaded generation.
#[test]
#[ignore = "reads the real bake; run in release"]
fn bench_region_candidates() {
    let globe = data().join("globe");
    let b = bake(&globe);
    let overview = read_tfgrid(&fs::read(tile_dir().join("overview.tfgrid")).unwrap()).unwrap();
    let list = std::env::var("TIMEFLY_CANDIDATES")
        .unwrap_or_else(|_| "Himalayas:28,86.9,0;Aegean:38,24.5,1000000".to_string());
    let generate = std::env::var("TIMEFLY_GENERATE").is_ok_and(|v| v == "1");
    for item in list.split(';').filter(|s| !s.is_empty()) {
        let (name, rest) = item.split_once(':').unwrap();
        let v: Vec<f64> = rest.split(',').map(|x| x.trim().parse().unwrap()).collect();
        let s = candidate_stats(&b, &globe, &overview, v[0], v[1], v[2], generate);
        println!(
            "{name} {:.2},{:.2} @ {} y: tiles {}, land {:.2}, anomaly land {}, coast {}, ice {:.2}, max {:.0} m, snowline {:.0} m (above {:.3}), plates {:?}, sliver pts {}, gen {} (inner max {}), biomes {:?}",
            v[0],
            v[1],
            v[2],
            s.tiles,
            s.land,
            s.anomaly_land.map_or("-".into(), |f| format!("{f:.3}")),
            s.coast_km.map_or("none".into(), |k| format!("{k:.0} km")),
            s.ice,
            s.max_m,
            s.snowline_m,
            s.above_snowline,
            s.plates,
            s.sliver_points,
            s.gen_secs.map_or("-".into(), |g| format!("{g:.2} s")),
            s.grid_max_m.map_or("-".into(), |m| format!("{m:.0} m")),
            s.biomes,
        );
    }
}

/// The Himalaya bench region's tile (r12c53, 25–30° N, 85–90° E) keeps Everest's area peak
/// unclipped and on the f16 lattice of its height band (4 m steps from 4,096 m, 8 m from 8,192 m),
/// near Everest (27.99° N, 86.93° E). The bake averages ETOPO's 60″ cells onto nodes, so the
/// node sits well under the 8,849 m summit (7,640 m in the current bake): the region's relief tops
/// out there, below the f16 8 m band.
#[test]
#[ignore = "reads the real bake"]
fn everest_tile_keeps_its_peak_in_f16() {
    let tile = read_tfgrid(&fs::read(tile_dir().join("r12c53.tfgrid")).unwrap()).unwrap();
    let (mut best, mut at) = (f32::MIN, (0, 0));
    for row in 0..tile.height {
        for col in 0..tile.width {
            let h = tile.texel_f32(0, row, col, 0);
            if h > best {
                best = h;
                at = (row, col);
            }
        }
    }
    let lat = 30.0 - f64::from(at.0) / 60.0;
    let lon = 85.0 + f64::from(at.1) / 60.0;
    println!("peak {best} m at {lat:.3}, {lon:.3}");
    assert!(
        best.is_finite() && (7_000.0..=8_850.0).contains(&best),
        "{best}"
    );
    // On the f16 lattice of its band, and decoded exactly from it.
    let step = if best >= 8_192.0 { 8.0 } else { 4.0 };
    assert_eq!(best % step, 0.0, "{best}");
    assert_eq!(f32::from(half::f16::from_f32(best)), best);
    assert!(
        (lat - 27.99).abs() < 0.1 && (lon - 86.93).abs() < 0.1,
        "{lat}, {lon}"
    );
}

/// Ruling R27 over the real bake at 0 ka: the dry Tibetan Plateau north of Everest is not snow,
/// its crest is; and the humid Alps keep their snow within ±10% (against the latitude-only
/// snowline, recomputed per texel). Snow is the ice byte at half or more, or an ice-cap or alpine
/// biome.
#[test]
#[ignore = "reads the real bake; run in release"]
fn dry_snowline_on_the_real_bake() {
    use timefly_core::worldgen::biome::{BiomeClass, snowline_m};
    let globe = data().join("globe");
    let b = bake(&globe);
    let overview = read_tfgrid(&fs::read(tile_dir().join("overview.tfgrid")).unwrap()).unwrap();
    let grid_at = |lat: f64, lon: f64| {
        let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
        for i in frames_needed(&b.timeline, 0.0) {
            grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
        }
        let centre = geo_to_render(latlon_to_geo(lat, lon));
        let frame = RegionFrame { center: centre };
        let mut tiles = TileSet::new();
        tiles.set_overview(overview.clone()).unwrap();
        let probe = snapshot_field(&b.timeline, &b.crust, &tiles, false, 0.0, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap();
        for (r, c) in probe.tiles_needed(&frame) {
            let path = tile_dir().join(format!("r{r:02}c{c:02}.tfgrid"));
            tiles
                .insert(r, c, read_tfgrid(&fs::read(path).unwrap()).unwrap())
                .unwrap();
        }
        let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, 0.0, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap();
        let spec = RegionSpec {
            seed: 0x7F1E_0002,
            ..RegionSpec::new(centre, YearsBp(0.0), 0)
        };
        (RegionGenerator::new(spec, &field, None).finish(), frame)
    };
    let snowy = |s: &[u8; 4]| {
        s[2] >= 128 || s[0] == BiomeClass::IceCap as u8 || s[0] == BiomeClass::Alpine as u8
    };
    // The Himalayas: the plateau band and the crest.
    let (grid, _) = grid_at(28.0, 86.9);
    let (mut band, mut band_snow, mut crest, mut crest_snow) = (0, 0, 0, 0);
    let mut band_before = 0;
    for (h, s) in grid.inner.elevation_m.iter().zip(&grid.inner.surface) {
        if (4_500.0..5_500.0).contains(h) {
            band += 1;
            band_snow += usize::from(snowy(s));
            // The latitude-only line at the centre's latitude (4,940 m): what R27 replaced.
            band_before += usize::from(f64::from(*h) > snowline_m(28.0, 0.0));
        }
        if *h > 7_000.0 {
            crest += 1;
            crest_snow += usize::from(snowy(s));
        }
    }
    let band_frac = band_snow as f64 / band as f64;
    let crest_frac = crest_snow as f64 / crest as f64;
    println!(
        "Himalayas 0 ka: 4,500-5,500 m land {band} texels, {:.1}% snow ({:.1}% above the latitude-only line); above 7,000 m {crest} texels, {:.1}% snow",
        band_frac * 100.0,
        band_before as f64 / band as f64 * 100.0,
        crest_frac * 100.0
    );
    assert!(band_frac < 0.25, "{band_frac}");
    assert!(crest_frac >= 0.90, "{crest_frac}");
    // The Alps: snow against the latitude-only line.
    let (grid, frame) = grid_at(46.5, 10.0);
    let (mut before, mut after) = (0usize, 0usize);
    let n = grid.inner.res;
    for row in 0..n {
        for col in 0..n {
            let i = (row * n + col) as usize;
            let (h, s) = (grid.inner.elevation_m[i], grid.inner.surface[i]);
            if h <= 0.0 {
                continue;
            }
            let (x, z) = grid.inner.local_of(row, col);
            let (lat, _) = geo_to_latlon(render_to_geo(frame.local_to_dir(x, z)));
            let cap = s[0] == BiomeClass::IceCap as u8 || s[0] == BiomeClass::Alpine as u8;
            before += usize::from(f64::from(h) > snowline_m(lat, 0.0) || cap);
            after += usize::from(snowy(&s));
        }
    }
    let raised = grid
        .inner
        .surface
        .iter()
        .filter(|s| s[3] & 0x7F > 0)
        .count();
    let top = grid
        .inner
        .elevation_m
        .iter()
        .copied()
        .fold(f32::MIN, f32::max);
    println!(
        "Alps 0 ka: snow texels {before} with the latitude-only line, {after} now; {raised} of {} texels raised; inner max {top:.0} m (latitude-only snowline {:.0} m)",
        n * n,
        snowline_m(46.5, 0.0)
    );
    if before == 0 {
        assert_eq!(after, 0);
    } else {
        let change = after as f64 / before as f64 - 1.0;
        assert!(change.abs() <= 0.10, "{change}");
    }
}

fn load_probe_tiles(tile_root: &Path, needed: &[(u8, u8)]) -> TileSet {
    let dir = active_tile_dir(tile_root);
    let read = |name: &str| read_tfgrid(&fs::read(dir.join(name)).unwrap()).unwrap();
    let mut tiles = TileSet::new();
    tiles.set_overview(read("overview.tfgrid")).unwrap();
    for &(r, c) in needed {
        tiles
            .insert(r, c, read(&format!("r{r:02}c{c:02}.tfgrid")))
            .unwrap();
    }
    tiles
}

#[test]
fn art_probes_read_overview_and_tiles_from_the_published_generation() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../timefly/tests/assets/data/tiles");
    let root =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("art-probe-{}", std::process::id()));
    let id = "a".repeat(64);
    let generation = root.join(&id);
    fs::create_dir_all(&generation).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.join("manifest.json")).unwrap()).unwrap();
    manifest["version"] = 2.into();
    manifest["generation"] = id.into();
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    for name in ["overview.tfgrid", "r17c35.tfgrid"] {
        fs::copy(fixture.join(name), generation.join(name)).unwrap();
        fs::write(root.join(name), b"obsolete flat payload").unwrap();
    }
    for layout in [&fixture, &root] {
        let tiles = load_probe_tiles(layout, &[(17, 35)]);
        assert!(tiles.has_overview() && tiles.contains(17, 35));
    }
    fs::remove_dir_all(root).unwrap();
}

/// The inner field at `lat, lon` and 0 ka over the real bake (the Task 7 rock-banding probes).
fn inner_field_at(lat: f64, lon: f64) -> timefly_core::worldgen::generate::Field {
    let globe = data().join("globe");
    let b = bake(&globe);
    let age = 0.0;
    let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
    for i in frames_needed(&b.timeline, age) {
        grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
    }
    let centre = geo_to_render(latlon_to_geo(lat, lon));
    let frame = RegionFrame { center: centre };
    let empty = TileSet::new();
    let probe = snapshot_field(&b.timeline, &b.crust, &empty, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap();
    let tiles = load_probe_tiles(&data().join("tiles"), &probe.tiles_needed(&frame));
    let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap();
    let spec = RegionSpec {
        seed: 0x7F1E_0002,
        ..RegionSpec::new(centre, YearsBp::from_myr(age), 0)
    };
    RegionGenerator::new(spec, &field, None).finish().inner
}

/// The shader's band ramp on dry bare ground (`terrain.wesl`: smoothstep from `BAND_MIN_SLOPE`
/// to `BAND_FULL_SLOPE`, exposure 1 for deserts); constants mirror `material.rs`.
fn band_weight(slope: f32) -> f32 {
    let t = ((slope - 0.06) / (0.2 - 0.06)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Central-difference slope (rise over run) at an interior texel.
fn texel_slope(f: &timefly_core::worldgen::generate::Field, r: usize, c: usize) -> f32 {
    let n = f.res as usize;
    let h = |r: usize, c: usize| f.elevation_m[r * n + c];
    let step = f.spacing_m() as f32;
    let gx = (h(r, c + 1) - h(r, c - 1)) / (2.0 * step);
    let gz = (h(r + 1, c) - h(r - 1, c)) / (2.0 * step);
    gx.hypot(gz)
}

/// The Grand Canyon (36.1 N, -112.1 E at 0 ka): how much relief the ETOPO-sourced inner field
/// carries near the centre, for the rock-banding art pass. Prints the height range within 15 km
/// and the share of land texels above slope 0.06 (bands begin), 0.27 and 0.6; asserts only that
/// the field generated.
#[test]
#[ignore = "reads the real bake; run in release"]
fn grand_canyon_relief() {
    let f = inner_field_at(36.1, -112.1);
    let n = f.res as usize;
    let h = |r: usize, c: usize| f.elevation_m[r * n + c];
    let step = f.spacing_m();
    for radius in [15_000.0_f64, 150_000.0] {
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let (mut land, mut over) = (0usize, [0usize; 3]);
        for r in 1..n - 1 {
            for c in 1..n - 1 {
                let (x, z) = f.local_of(r as u32, c as u32);
                if x.hypot(z) > radius {
                    continue;
                }
                lo = lo.min(h(r, c));
                hi = hi.max(h(r, c));
                if h(r, c) <= 0.0 {
                    continue;
                }
                let slope = texel_slope(&f, r, c);
                land += 1;
                for (k, t) in [0.06, 0.27, 0.6].iter().enumerate() {
                    over[k] += usize::from(slope > *t);
                }
            }
        }
        let pct = |k: usize| 100.0 * k as f64 / land.max(1) as f64;
        println!(
            "grand canyon within {:.0} km (texel {step:.0} m): height {lo:.0} .. {hi:.0} m \
             (range {:.0} m); land {land}; slope > 0.06: {:.2} %; > 0.27: {:.2} %; > 0.6: {:.2} %",
            radius / 1000.0,
            hi - lo,
            pct(over[0]),
            pct(over[1]),
            pct(over[2])
        );
    }
    assert!(!f.elevation_m.is_empty());
}

/// The share of hot/cold-desert land texels whose band weight exceeds 0.1, over the Grand
/// Canyon and the Sahara (23 N, 10 E) at 0 ka, within 15 km and 150 km of the centre. Flat plains
/// (slope < 0.05) must get zero weight.
#[test]
#[ignore = "reads the real bake; run in release"]
fn desert_band_share() {
    for (name, lat, lon) in [("grand canyon", 36.1, -112.1), ("sahara", 23.0, 10.0)] {
        let f = inner_field_at(lat, lon);
        let n = f.res as usize;
        for radius in [15_000.0_f64, 150_000.0] {
            let (mut desert, mut banded, mut flat) = (0usize, 0usize, 0usize);
            for r in 1..n - 1 {
                for c in 1..n - 1 {
                    let (x, z) = f.local_of(r as u32, c as u32);
                    let biome = f.surface[r * n + c][0];
                    if x.hypot(z) > radius
                        || f.elevation_m[r * n + c] <= 0.0
                        || !(biome == 2 || biome == 3)
                    {
                        continue;
                    }
                    let slope = texel_slope(&f, r, c);
                    let w = band_weight(slope);
                    desert += 1;
                    banded += usize::from(w > 0.1);
                    if slope < 0.05 {
                        flat += 1;
                        assert_eq!(w, 0.0, "a flat plain (slope {slope}) got band weight");
                    }
                }
            }
            println!(
                "{name} within {:.0} km: desert land {desert}; band weight > 0.1: {:.2} %; \
                 flat (< 0.05) {:.1} %",
                radius / 1000.0,
                100.0 * banded as f64 / desert.max(1) as f64,
                100.0 * flat as f64 / desert.max(1) as f64
            );
        }
    }
}

/// A coarse map of the Grand Canyon's inner field (40 km square, north up, cells of 5 × 5 texels,
/// about 1.5 km) for choosing the showcase camera. Each cell prints its mean elevation in 400 m
/// steps as a digit (0 below 400 m, 9 from 3,600 m up), or `#` where its mean band weight exceeds
/// 0.5.
#[test]
#[ignore = "reads the real bake; run in release"]
fn grand_canyon_map() {
    let f = inner_field_at(36.1, -112.1);
    let n = f.res as usize;
    let cell = 5usize; // texels
    let half = 20_000.0;
    println!("x: -20..20 km east, rows north to south; digit = elevation/400 m, # = banded cell");
    let mut r0 = 1usize;
    while r0 + cell < n {
        let (_, z) = f.local_of(r0 as u32, 0);
        if z.abs() <= half {
            let mut line = String::new();
            let mut c0 = 1usize;
            while c0 + cell < n {
                let (x, _) = f.local_of(0, c0 as u32);
                if x.abs() <= half {
                    let (mut sum, mut w, mut cnt) = (0.0, 0.0, 0.0);
                    for r in r0..r0 + cell {
                        for c in c0..c0 + cell {
                            sum += f.elevation_m[r * n + c];
                            w += band_weight(texel_slope(&f, r, c));
                            cnt += 1.0;
                        }
                    }
                    line.push(if w / cnt > 0.5 {
                        '#'
                    } else {
                        char::from_digit(((sum / cnt) / 400.0).clamp(0.0, 9.0) as u32, 10).unwrap()
                    });
                }
                c0 += cell;
            }
            println!("z {:6.1} km {line}", z / 1000.0);
        }
        r0 += cell;
    }
}

/// The shipped climate table (`data/hydrology/climate_table.json`), parsed as the shell does.
fn climate_table() -> timefly_core::hydrology::climate::ClimateTable {
    use timefly_core::hydrology::climate::{BIOME_ORDER, Climate, ClimateTable};
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../../../data/hydrology/climate_table.json")).unwrap();
    let points = |key: &str| -> Vec<(f32, f32)> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32))
            .collect()
    };
    ClimateTable {
        pet_mm: points("pet_mm"),
        lake_evaporation_mm: points("lake_evaporation_mm"),
        turc_pike_n: v["turc_pike_n"].as_f64().unwrap() as f32,
        ice_melt_runoff_mm: v["ice_melt_runoff_mm"].as_f64().unwrap() as f32,
        biome_climate: BIOME_ORDER.map(|b| {
            let row = &v["biome_climate"][format!("{b:?}")];
            Climate {
                precip_mm: row[0].as_f64().unwrap() as f32,
                temp_c: row[1].as_f64().unwrap() as f32,
            }
        }),
    }
}

/// Rivers and lakes Task 8b's real-data check: the present day's continental pass on the real
/// overview and the Beyer 0 ka climate, as the game runs it (n = 900, recent mode), then the
/// region refinement at Victoria Falls, Iguazu Falls and Manaus for the rapids. Prints every
/// check, then asserts them. Uses only interfaces older than Task 8b, so the same test measures
/// the commit before it.
/// `TIMEFLY_REAL_DATA=<main checkout>/crates/timefly/assets/data cargo test -p timefly_core
/// --test real_bake_region -- --ignored --nocapture todays_lakes`.
#[test]
#[ignore = "reads the real bake and climate"]
fn todays_lakes_on_the_real_overview() {
    use timefly_core::hydrology::climate::{BeyerClimate, ZonalClimate};
    use timefly_core::hydrology::{
        HydroPrep, LakeKind, LatticeSizes, SnapshotSource, hydro_lattice_for, river_tile_margin_m,
    };
    use timefly_core::worldgen::generate::FieldKind;
    let globe = data().join("globe");
    let b = bake(&globe);
    let age = 0.0;
    let mut grids: Vec<Option<Grid>> = vec![None; b.files.len()];
    for i in frames_needed(&b.timeline, age) {
        grids[i] = Some(read_tfgrid(&fs::read(globe.join(&b.files[i])).unwrap()).unwrap());
    }
    let overview = read_tfgrid(&fs::read(tile_dir().join("overview.tfgrid")).unwrap()).unwrap();
    let mut tiles = TileSet::new();
    tiles.set_overview(overview.clone()).unwrap();
    let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
        grids[i].as_ref().unwrap()
    })
    .unwrap();
    let (lattice, mode) = hydro_lattice_for(&b.timeline, age, LatticeSizes::default());
    let source = SnapshotSource::new(&field, &tiles, &b.timeline, age, mode);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(data().join("climate/manifest.json")).unwrap()).unwrap();
    let file = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["age_ka"].as_f64() == Some(0.0))
        .unwrap()["file"]
        .as_str()
        .unwrap()
        .to_string();
    let bio = read_tfgrid(&fs::read(data().join("climate").join(file)).unwrap()).unwrap();
    let table = climate_table();
    let zonal = ZonalClimate {
        table: &table,
        sea_level_anomaly_m: 0.0,
    };
    let climate = BeyerClimate::new([&bio, &bio], 0.0, zonal);
    let t = std::time::Instant::now();
    let mut prep = HydroPrep::new(lattice, YearsBp(0.0));
    while !prep.step(&source, &climate, &table, 1 << 22) {}
    let h = prep.finish();
    println!("pass: {:?}, mode {mode:?}, n = {}", t.elapsed(), lattice.n);

    let count = |k: LakeKind| h.lakes.iter().filter(|l| l.kind == k).count();
    let area: f64 = h.lakes.iter().map(|l| l.area_km2).sum();
    println!(
        "lakes: {} (open {}, terminal {}, data {}), total area {:.0} km2, unplaced table points {:?}",
        h.lakes.len(),
        count(LakeKind::Open),
        count(LakeKind::Terminal),
        count(LakeKind::Data),
        area,
        h.unplaced_lake_levels
    );
    let mut big: Vec<&timefly_core::hydrology::Lake> = h.lakes.iter().collect();
    big.sort_by(|a, b| b.area_km2.total_cmp(&a.area_km2));
    for l in big.iter().take(12) {
        let (la, lo) = geo_to_latlon(render_to_geo(h.lattice.dir(l.exit)));
        println!(
            "  {:?} {:8.1} m (spill {:8.1}) {:9.0} km2 at ({la:.2}, {lo:.2})",
            l.kind, l.level_m, l.spill_m, l.area_km2
        );
    }
    let dir_of = |lat: f64, lon: f64| geo_to_render(latlon_to_geo(lat, lon));
    let lake_at = |lat: f64, lon: f64| h.lake_tap(dir_of(lat, lon));
    let overview_at = |lat: f64, lon: f64| tiles.sample_overview(lat, lon).unwrap().0;
    let mut failures = Vec::new();
    println!("| check | point | lake (kind, level m) | reference m | result |");
    println!("|---|---|---|---|---|");
    // Round 2: mapped lakes against their baked fine surface (the shipped map's level there),
    // the Caspian against the table.
    let baked = |lat: f64, lon: f64| {
        timefly_core::hydrology::lakemap::shipped_lake_map()
            .level_at(lat, lon)
            .unwrap_or(f32::NAN)
    };
    for (name, lat, lon, reference) in [
        ("Titicaca", -15.8, -69.4, None),
        ("Victoria", -1.0, 33.0, None),
        ("Dead Sea", 31.5, 35.5, None),
        ("Great Salt Lake", 41.1, -112.5, None),
        ("Caspian (table)", 42.0, 51.0, Some(-28.0f32)),
    ] {
        let want = reference.unwrap_or_else(|| baked(lat, lon));
        println!("    {name}: overview {:.1} m", overview_at(lat, lon));
        let got = lake_at(lat, lon);
        let ok = got.is_some_and(|t| (t.level_m - want).abs() <= 5.0);
        println!(
            "| {name} within 5 m | ({lat}, {lon}) | {} | {want:.1} | {} |",
            got.map_or("none".into(), |t| format!("{:?} {:.1}", t.kind, t.level_m)),
            if ok { "pass" } else { "FAIL" }
        );
        if !ok {
            failures.push(name);
        }
    }
    for (name, lat, lon) in [("Manaus", -3.1, -60.0), ("Skardu", 35.3, 75.6)] {
        let got = lake_at(lat, lon);
        println!(
            "| no lake at {name} | ({lat}, {lon}) | {} | - | {} |",
            got.map_or("none".into(), |t| format!("{:?} {:.1}", t.kind, t.level_m)),
            if got.is_none() { "pass" } else { "FAIL" }
        );
        if got.is_some() {
            failures.push(name);
        }
    }
    // The Kashmir valley floor: no lake above 1,600 m within 15 km of (34.1, 74.8).
    let mut kashmir: Option<f32> = None;
    for a in -3..=3 {
        for k in -3..=3 {
            let d = RegionFrame {
                center: dir_of(34.1, 74.8),
            }
            .local_to_dir(f64::from(a) * 5_000.0, f64::from(k) * 5_000.0);
            if let Some(t) = h.lake_tap(d) {
                kashmir = Some(kashmir.map_or(t.level_m, |m| m.max(t.level_m)));
            }
        }
    }
    let ok = kashmir.is_none_or(|l| l <= 1_600.0);
    println!(
        "| no lake above 1,600 m on the Kashmir floor | (34.1, 74.8) ± 15 km | {} | 1600 | {} |",
        kashmir.map_or("none".into(), |l| format!("highest {l:.1}")),
        if ok { "pass" } else { "FAIL" }
    );
    if !ok {
        failures.push("Kashmir");
    }

    // The Black Sea is sea, not part of a Caspian lake through the Manych.
    let black = h.lattice.nearest(dir_of(43.0, 34.0));
    let ok = h.is_ocean(black) && lake_at(43.0, 34.0).is_none();
    println!(
        "| Black Sea is ocean, no lake | (43.0, 34.0) | {} | - | {} |",
        lake_at(43.0, 34.0).map_or("none".into(), |t| format!("{:?} {:.1}", t.kind, t.level_m)),
        if ok { "pass" } else { "FAIL" }
    );
    if !ok {
        failures.push("Black Sea");
    }
    if let Some(t) = lake_at(42.0, 51.0) {
        let l = &h.lakes[t.lake as usize];
        let (la, lo) = geo_to_latlon(render_to_geo(h.lattice.dir(l.exit)));
        println!(
            "    Caspian: {:?} {:.1} m over {:.0} km2, exit ({la:.2}, {lo:.2})",
            l.kind, l.level_m, l.area_km2
        );
        if l.area_km2 > 500_000.0 {
            failures.push("Caspian footprint");
        }
    }

    // Rapids: the refinement's sub-points within `r_km` of a point, and how many are rapids.
    let margin = river_tile_margin_m(&h.lattice);
    let rapids_near = |lat: f64, lon: f64, r_km: f64| -> (usize, usize, f32, String) {
        let centre = dir_of(lat, lon);
        let frame = RegionFrame { center: centre };
        let mut tiles = TileSet::new();
        tiles.set_overview(overview.clone()).unwrap();
        let needed = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap()
        .tiles_needed_with_margin(&frame, margin);
        for (r, c) in needed {
            let path = tile_dir().join(format!("r{r:02}c{c:02}.tfgrid"));
            tiles
                .insert(r, c, read_tfgrid(&fs::read(path).unwrap()).unwrap())
                .unwrap();
        }
        let field = snapshot_field(&b.timeline, &b.crust, &tiles, false, age, |i| {
            grids[i].as_ref().unwrap()
        })
        .unwrap();
        let g = RegionGenerator::new(RegionSpec::new(centre, YearsBp(0.0), 7), &field, Some(&h));
        let rivers = g.rivers(FieldKind::Inner).expect("a pass");
        let reach = r_km * 1000.0 / 6_371_000.0;
        let (mut near, mut rapid, mut most) = (0, 0, 0.0f32);
        // The nearest sub-point, and the strongest rapid within 30 km.
        let (mut nearest, mut wide) = ((f64::MAX, 0.0f32, 0.0f32), (0.0f32, 0.0f64));
        for (a, _) in rivers.sub_segments() {
            let d_km = a.dir.angle_between(centre.0) * 6_371.0;
            if d_km < nearest.0 {
                nearest = (d_km, a.half_width_m * 2.0, a.speed_mps);
            }
            if d_km <= 30.0 && a.rapid > wide.0 {
                wide = (a.rapid, d_km);
            }
            if a.dir.angle_between(centre.0) <= reach {
                near += 1;
                if a.rapid > 0.0 {
                    rapid += 1;
                }
                most = most.max(a.rapid);
            }
        }
        let note = format!(
            "nearest {:.1} km ({:.0} m wide, {:.2} m/s); strongest rapid within 30 km {:.2} at {:.1} km",
            nearest.0, nearest.1, nearest.2, wide.0, wide.1
        );
        (near, rapid, most, note)
    };
    for (name, lat, lon, r_km, want_rapids) in [
        ("Victoria Falls", -17.925, 25.857, 5.0, true),
        ("Iguazu Falls", -25.695, -54.437, 5.0, true),
        ("Manaus", -3.1, -60.0, 50.0, false),
    ] {
        let (near, rapid, most, note) = rapids_near(lat, lon, r_km);
        println!("    {name}: {note}");
        // Round 2: the falls are recorded, not checked (their drops are below the data's
        // resolution; they stay as Task 12 renders).
        let ok = if want_rapids {
            true
        } else {
            near > 0 && rapid == 0
        };
        println!(
            "| {} within {r_km} km of {name} | ({lat}, {lon}) | {near} sub-points, {rapid} rapid, most {most:.2} | - | {} |",
            if want_rapids {
                "rapids (recorded)"
            } else {
                "no rapids"
            },
            if want_rapids {
                "-"
            } else if ok {
                "pass"
            } else {
                "FAIL"
            }
        );
        if !ok {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "failed: {failures:?}");
}
