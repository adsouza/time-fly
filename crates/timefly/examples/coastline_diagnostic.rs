//! Coastline diagnostic (Task 9 Step 5): compares the plate renderer's land/water mask, from the
//! CPU reference of the shader rules, with each source PaleoDEM's own mask in its published
//! geographic frame.
//!
//! `cargo run --release -p timefly --example coastline_diagnostic -- <globe dir> <out dir>`
//!
//! For each checked frame, two placements:
//! - `source`: placed and sampled at the frame's source reconstruction age, alone, with zero
//!   sea-level offset (the modern check uses the 30 ka bridge's present-day PaleoDEM).
//! - `slider`: the normal output at the frame's timeline age (frame selection, sea level and the
//!   current-age rotation the slider would use).
//!
//! Missing land (source land shown as ocean) and added land (source ocean shown as land) are
//! counted by area, ignoring cells within one source cell (1°) of a source coastline. Coverage is
//! the share of source land that any valid plate footprint covers. Each comparison is also written
//! as an equirectangular PNG: source mask, renderer mask, and differences.

use std::fs;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::math::DVec3;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use timefly::globe::capture::{CaptureSession, ParityStats, compare_with_reference};
use timefly_core::cube::{CubeFace, dir_to_texel, texel_center_dir};
use timefly_core::formats::{Grid, read_tfgrid, read_tfplates, read_tfrot};
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};
use timefly_core::geodynamics::{CrustModel, PlateIndex, ResolvedPosition};
use timefly_core::globe::{
    DeepSampling, EARTH_RADIUS_KM, PlateRotations, RenderedSurface, plate_rotations,
    rendered_surface, source_id_ranks,
};
use timefly_core::surface::{FrameMeta, FrameSelection, FrameSource, Timeline};

/// Timeline ages to check (Myr); each picks the nearest PaleoDEM frame.
const CHECK_AGES_MYR: [f64; 4] = [0.03, 66.0, 250.0, 540.0];
/// The shoreline tolerance: one 1° source cell.
const TOLERANCE_DEG: f64 = 1.0;
const MAP_WIDTH: u32 = 720;

struct Bake {
    crust: CrustModel,
    timeline: Timeline,
    files: Vec<String>,
    dir: PathBuf,
}

fn load(dir: &Path) -> Result<Bake, String> {
    let read = |name: &str| fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&read("manifest.json")?).map_err(|e| e.to_string())?;
    let footprints = read_tfplates(&read(manifest["footprints"].as_str().unwrap_or(""))?)
        .map_err(|e| e.to_string())?;
    let rotations = read_tfrot(&read(manifest["rotations"].as_str().unwrap_or(""))?)
        .map_err(|e| e.to_string())?;
    let crust = CrustModel::new(rotations, footprints).map_err(|e| e.to_string())?;
    let frames = manifest["frames"].as_array().ok_or("no frames")?;
    let mut metas = Vec::new();
    let mut files = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let source = match f["source"].as_str() {
            Some("ice6g") => FrameSource::Ice6g,
            _ => FrameSource::PaleoDem {
                source_rotation_age_myr: f["source_rotation_age_myr"].as_f64().ok_or("age")?,
            },
        };
        metas.push(FrameMeta {
            age_myr: f["age_myr"].as_f64().ok_or("age")?,
            source,
            image_slot: i as u32,
        });
        files.push(f["file"].as_str().ok_or("file")?.to_string());
    }
    let curve = manifest["sea_level_curve"]
        .as_array()
        .ok_or("curve")?
        .iter()
        .map(|k| (k[0].as_f64().unwrap_or(0.0), k[1].as_f64().unwrap_or(0.0)))
        .collect();
    let timeline = Timeline::new(metas, curve).map_err(|e| e.to_string())?;
    Ok(Bake {
        crust,
        timeline,
        files,
        dir: dir.to_path_buf(),
    })
}

fn grid(bake: &Bake, frame: usize) -> Grid {
    let bytes = fs::read(bake.dir.join(&bake.files[frame])).expect("frame file");
    read_tfgrid(&bytes).expect("frame grid")
}

/// Nearest-texel elevation (channel R).
fn sample(grid: &Grid, d: RenderDir) -> f64 {
    let (face, col, row) = dir_to_texel(d, grid.height);
    f64::from(grid.texel_f32(face as usize, row, col, 0))
}

fn tangent_frame(p: DVec3) -> (DVec3, DVec3) {
    let up = if p.y.abs() > 0.9 { DVec3::X } else { DVec3::Y };
    let t1 = up.cross(p).normalize();
    (t1, p.cross(t1))
}

/// Whether the source has the opposite class within the tolerance of `d`.
fn near_coast(grid: &Grid, d: DVec3, land: bool) -> bool {
    let (t1, t2) = tangent_frame(d);
    let step = TOLERANCE_DEG.to_radians();
    (0..16).any(|i| {
        let a = f64::from(i) * std::f64::consts::TAU / 16.0;
        [0.5, 1.0].iter().any(|&r| {
            let q = (d + (t1 * a.cos() + t2 * a.sin()) * step * r).normalize();
            (sample(grid, RenderDir(q)) > 0.0) != land
        })
    })
}

/// Solid angle of a cube texel (steradians).
fn texel_solid_angle(col: u32, row: u32, n: u32) -> f64 {
    let s = 2.0 * (f64::from(col) + 0.5) / f64::from(n) - 1.0;
    let t = 2.0 * (f64::from(row) + 0.5) / f64::from(n) - 1.0;
    let da = (2.0 / f64::from(n)).powi(2);
    da / (1.0 + s * s + t * t).powf(1.5)
}

struct Placement {
    sampling: DeepSampling,
    rotations: Vec<PlateRotations>,
    ranks: Vec<u32>,
}

impl Placement {
    fn new(bake: &Bake, sampling: DeepSampling) -> Placement {
        let plates = bake.crust.rotations().plate_ids.len() as u32;
        Placement {
            rotations: (0..plates)
                .map(|p| plate_rotations(bake.crust.rotations(), PlateIndex(p), &sampling))
                .collect(),
            ranks: source_id_ranks(bake.crust.rotations()),
            sampling,
        }
    }

    fn land(&self, bake: &Bake, grids: &[Grid; 2], d: RenderDir) -> bool {
        matches!(
            rendered_surface(
                &bake.crust,
                d,
                &self.sampling,
                &self.rotations,
                &self.ranks,
                |e, dir| sample(&grids[e], dir)
            ),
            RenderedSurface::Land { .. }
        )
    }

    /// Whether a plate with a current rotation owns the cell under `d` at some other age.
    fn owned_at_other_ages(&self, bake: &Bake, d: RenderDir) -> bool {
        let fp = bake.crust.footprints();
        self.rotations.iter().enumerate().any(|(plate, r)| {
            r.current.is_some_and(|q| {
                let p = RenderDir((q.as_dquat().inverse() * d.0).normalize());
                (0..=54).any(|k| fp.contains(PlateIndex(plate as u32), p, f64::from(k) * 10.0))
            })
        })
    }

    fn covered(&self, bake: &Bake, d: RenderDir) -> bool {
        matches!(
            bake.crust.resolve(d, self.sampling.age_myr, |_| None),
            ResolvedPosition::Tracked(_)
        )
    }
}

#[derive(Default, Debug)]
struct Stats {
    source_land_km2: f64,
    rendered_land_km2: f64,
    missing_km2: f64,
    added_km2: f64,
    missing_raw_km2: f64,
    added_raw_km2: f64,
    covered_land_km2: f64,
    /// Missing land (beyond tolerance) in cells that a plate with a current rotation owns at
    /// other ages only (an expired or not-yet-valid footprint interval).
    missing_expired_km2: f64,
}

fn compare(bake: &Bake, source: &Grid, grids: &[Grid; 2], placement: &Placement) -> Stats {
    let n = source.height;
    let r2 = EARTH_RADIUS_KM * EARTH_RADIUS_KM;
    let mut s = Stats::default();
    for face in CubeFace::ALL {
        for row in 0..n {
            for col in 0..n {
                let d = texel_center_dir(face, col, row, n);
                let area = texel_solid_angle(col, row, n) * r2;
                let src = sample(source, d) > 0.0;
                let out = placement.land(bake, grids, d);
                if src {
                    s.source_land_km2 += area;
                    if placement.covered(bake, d) {
                        s.covered_land_km2 += area;
                    }
                }
                if out {
                    s.rendered_land_km2 += area;
                }
                if src != out {
                    let beyond = !near_coast(source, d.0, src);
                    match (src, beyond) {
                        (true, true) => {
                            s.missing_km2 += area;
                            if placement.owned_at_other_ages(bake, d) {
                                s.missing_expired_km2 += area;
                            }
                        }
                        (false, true) => s.added_km2 += area,
                        _ => {}
                    }
                    if src {
                        s.missing_raw_km2 += area;
                    } else {
                        s.added_raw_km2 += area;
                    }
                }
            }
        }
    }
    s
}

/// Equirectangular map: source mask, renderer mask, differences (red missing, yellow added).
fn write_map(path: &Path, bake: &Bake, source: &Grid, grids: &[Grid; 2], placement: &Placement) {
    let (w, h) = (MAP_WIDTH, MAP_WIDTH / 2);
    let mut data = vec![0u8; (w * h * 3 * 4) as usize];
    let land_c = [118u8, 140, 80, 255];
    let water_c = [40u8, 80, 120, 255];
    for y in 0..h {
        for x in 0..w {
            let lat = 90.0 - (f64::from(y) + 0.5) * 180.0 / f64::from(h);
            let lon = -180.0 + (f64::from(x) + 0.5) * 360.0 / f64::from(w);
            let d = geo_to_render(latlon_to_geo(lat, lon));
            let src = sample(source, d) > 0.0;
            let out = placement.land(bake, grids, d);
            let diff = match (src, out) {
                (true, true) => [150u8, 150, 150, 255],
                (false, false) => [25u8, 30, 45, 255],
                (true, false) if !near_coast(source, d.0, true) => [230u8, 40, 40, 255],
                (false, true) if !near_coast(source, d.0, false) => [250u8, 210, 40, 255],
                (true, false) => [140u8, 70, 70, 255],
                (false, true) => [140u8, 130, 60, 255],
            };
            for (panel, color) in [
                if src { land_c } else { water_c },
                if out { land_c } else { water_c },
                diff,
            ]
            .into_iter()
            .enumerate()
            {
                let i = (((panel as u32 * h + y) * w + x) * 4) as usize;
                data[i..i + 4].copy_from_slice(&color);
            }
        }
    }
    let image = Image::new(
        Extent3d {
            width: w,
            height: h * 3,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    );
    image
        .try_into_dynamic()
        .expect("rgba image")
        .to_rgb8()
        .save(path)
        .expect("write map");
}

/// The camera views used for the shader comparison: four around the equator and the poles.
const VIEWS: [(f64, f64); 6] = [
    (0.0, 0.0),
    (0.0, 90.0),
    (0.0, 180.0),
    (0.0, -90.0),
    (89.0, 0.0),
    (-89.0, 0.0),
];
const PARITY_STEP_DEG: f64 = 1.5;

/// Renders the slider output through the real shaders (verification view) from six views and
/// compares each pixel's plate (or ocean) with the CPU reference. Returns the stats and the
/// area (km²) of mismatches beyond the tolerance.
fn shader_parity(
    session: &mut CaptureSession,
    bake: &Bake,
    age_myr: f64,
    grids: &[Grid; 2],
    placement: &Placement,
) -> Result<(ParityStats, f64), String> {
    let cpu = |d: bevy::math::Vec3| match rendered_surface(
        &bake.crust,
        RenderDir(d.as_dvec3().normalize()),
        &placement.sampling,
        &placement.rotations,
        &placement.ranks,
        |e, dir| sample(&grids[e], dir),
    ) {
        RenderedSurface::Land { plate_id, .. } => {
            bake.crust.rotations().index_of(plate_id).map(|i| i.0)
        }
        RenderedSurface::Ocean => None,
    };
    let views: Vec<bevy::math::Vec3> = VIEWS
        .iter()
        .map(|&(lat, lon)| geo_to_render(latlon_to_geo(lat, lon)).0.as_vec3())
        .collect();
    let mut total = ParityStats::default();
    let mut weights = Vec::new();
    for (v, &(lat, lon)) in views.iter().zip(&VIEWS) {
        let shot = session.shoot(age_myr * 1e6, (lat, lon), 2.6 * EARTH_RADIUS_KM)?;
        // Each grid direction is compared in the view that sees it most head-on.
        let mut dirs = Vec::new();
        let mut la = -90.0 + PARITY_STEP_DEG / 2.0;
        while la < 90.0 {
            let mut lo = -180.0 + PARITY_STEP_DEG / 2.0;
            while lo < 180.0 {
                let d = geo_to_render(latlon_to_geo(la, lo)).0.as_vec3();
                let best = views
                    .iter()
                    .map(|w| d.angle_between(*w))
                    .fold(f32::MAX, f32::min);
                if (d.angle_between(*v) - best).abs() < 1e-6 {
                    dirs.push(d);
                }
                lo += PARITY_STEP_DEG;
            }
            la += PARITY_STEP_DEG;
        }
        let stats = compare_with_reference(
            &shot,
            dirs,
            cpu,
            (TOLERANCE_DEG as f32).to_radians(),
            60f32.to_radians(),
        );
        total.add(&stats);
    }
    let cell = PARITY_STEP_DEG.to_radians().powi(2) * EARTH_RADIUS_KM * EARTH_RADIUS_KM;
    for (d, _, _) in &total.mismatches {
        let lat = (d.y as f64).clamp(-1.0, 1.0).asin();
        weights.push(cell * lat.cos());
    }
    Ok((total, weights.iter().sum()))
}

fn nearest_paleodem(t: &Timeline, age: f64) -> usize {
    let first = t.bridge_frame_index();
    (first..t.frames().len())
        .min_by(|&a, &b| {
            (t.frames()[a].age_myr - age)
                .abs()
                .total_cmp(&(t.frames()[b].age_myr - age).abs())
        })
        .unwrap_or(first)
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, out] = [0, 1].map(|i| PathBuf::from(args.get(i).cloned().unwrap_or_default()));
    if args.len() < 2 {
        return Err("usage: coastline_diagnostic <globe dir> <out dir>".into());
    }
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let bake = load(&dir)?;
    let asset_root = dir
        .parent()
        .and_then(Path::parent)
        .ok_or("the globe dir must be <asset root>/data/globe")?
        .to_path_buf();
    let mut session = CaptureSession::new(&asset_root, (768, 768), true)?;
    let mut parity_rows = Vec::new();
    println!(
        "| frame | timeline Ma | source Ma | placement | source land Mkm² | rendered land Mkm² | coverage | missing Mkm² (raw) | of which expired-footprint Mkm² | added Mkm² (raw) |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for age in CHECK_AGES_MYR {
        let frame = nearest_paleodem(&bake.timeline, age);
        let meta = bake.timeline.frames()[frame];
        let FrameSource::PaleoDem {
            source_rotation_age_myr: source_age,
        } = meta.source
        else {
            continue;
        };
        let source = grid(&bake, frame);
        let modes: [(&str, DeepSampling); 2] = [
            (
                "source",
                DeepSampling::single_frame(&bake.timeline, frame, source_age, 0.0),
            ),
            (
                "slider",
                match bake.timeline.select_frames(meta.age_myr) {
                    FrameSelection::Deep(d) => {
                        DeepSampling::from_bracket(&bake.timeline, meta.age_myr, &d)
                    }
                    _ => DeepSampling::single_frame(&bake.timeline, frame, meta.age_myr, 0.0),
                },
            ),
        ];
        for (name, sampling) in modes {
            let grids = sampling.endpoints.map(|e| grid(&bake, e.frame));
            let placement = Placement::new(&bake, sampling);
            let fp = bake.crust.footprints();
            let lacking: Vec<usize> = (0..placement.rotations.len())
                .filter(|&i| placement.rotations[i].current.is_none())
                .collect();
            let lacking_cells = fp
                .entries
                .iter()
                .filter(|e| lacking.contains(&(e.plate_index.0 as usize)))
                .count();
            eprintln!(
                "{} Ma {name}: {} plates without a rotation at {} Ma, owning {} footprint entries",
                meta.age_myr,
                lacking.len(),
                placement.sampling.age_myr,
                lacking_cells
            );
            let s = compare(&bake, &source, &grids, &placement);
            let m = |km2: f64| km2 / 1e6;
            println!(
                "| {frame} | {} | {source_age} | {name} | {:.2} | {:.2} | {:.1}% | {:.3} ({:.2}) | {:.3} | {:.3} ({:.2}) |",
                meta.age_myr,
                m(s.source_land_km2),
                m(s.rendered_land_km2),
                100.0 * s.covered_land_km2 / s.source_land_km2.max(1.0),
                m(s.missing_km2),
                m(s.missing_raw_km2),
                m(s.missing_expired_km2),
                m(s.added_km2),
                m(s.added_raw_km2),
            );
            let file = out.join(format!(
                "coast-{:03}ma-{name}.png",
                meta.age_myr.round() as u32
            ));
            write_map(&file, &bake, &source, &grids, &placement);
            if name == "slider" {
                let (p, area) =
                    shader_parity(&mut session, &bake, meta.age_myr, &grids, &placement)?;
                parity_rows.push(format!(
                    "| {} | {} | {:.2}% | {:.2}% | {} | {} | {:.3} |",
                    meta.age_myr,
                    p.compared,
                    100.0 * p.mask_agree as f64 / p.compared.max(1) as f64,
                    100.0 * p.plate_agree as f64 / p.compared.max(1) as f64,
                    p.near_boundary,
                    p.mismatches.len(),
                    area / 1e6,
                ));
            }
        }
    }
    println!();
    println!(
        "| shader vs CPU, timeline Ma | compared | mask agree | plate agree | near boundary (skipped) | mismatches | mismatch Mkm² |"
    );
    println!("|---|---|---|---|---|---|---|");
    for row in parity_rows {
        println!("{row}");
    }
    Ok(())
}
