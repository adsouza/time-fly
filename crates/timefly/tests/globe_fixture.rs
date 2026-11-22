//! The two-plate render fixture: the committed tiny bake (`tests/assets/data/globe`). Plate 101
//! (lat −30..30, lon −60..−10 today) turns 40° east about the pole by 540 Ma and plate 202
//! (lon 0..50) stays put, so at 540 Ma they overlap over lon 0..30 and leave an ocean gap over
//! lon −60..−20 behind plate 101.
//!
//! The CPU tests run the plate pieces and the CPU reference of the shader rules over the bake.
//! The ignored GPU test renders the same fixture through the real shader path, headless:
//! `cargo test -p timefly --test globe_fixture -- --ignored`.

use std::fs;
use std::path::PathBuf;

use bevy::math::{Quat, Vec3};
use timefly_core::cube::dir_to_texel;
use timefly_core::formats::{Grid, read_tfgrid, read_tfplates, read_tfrot};
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};
use timefly_core::geodynamics::{CrustModel, PlateId, PlateIndex};
use timefly_core::globe::{
    DeepSampling, PlateRotations, RenderedSurface, plate_rotations, rendered_surface,
    source_id_ranks,
};
use timefly_core::surface::{FrameSelection, Timeline};

use timefly::globe::material::{piece_world_dir, plate_gpu_data};
use timefly::globe::mesh::{PIECE_DEPTH_COMPARE, build_plate_pieces, later_draw_wins_ties};

fn globe_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/data/globe")
}

struct Tiny {
    crust: CrustModel,
    timeline: Timeline,
    /// Frame grids by frame index (CPU copies; the app keeps only GPU images).
    grids: Vec<Grid>,
}

fn tiny() -> Tiny {
    tiny_at(&globe_dir())
}

fn tiny_at(dir: &std::path::Path) -> Tiny {
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let footprints = read_tfplates(&fs::read(dir.join("plates.tfplates")).unwrap()).unwrap();
    let rotations = read_tfrot(&fs::read(dir.join("rotations.tfrot")).unwrap()).unwrap();
    let crust = CrustModel::new(rotations, footprints).unwrap();
    let frames = manifest["frames"].as_array().unwrap();
    let metas = frames
        .iter()
        .enumerate()
        .map(|(i, f)| timefly_core::surface::FrameMeta {
            age_myr: f["age_myr"].as_f64().unwrap(),
            source: match f["source"].as_str().unwrap() {
                "ice6g" => timefly_core::surface::FrameSource::Ice6g,
                _ => timefly_core::surface::FrameSource::PaleoDem {
                    source_rotation_age_myr: f["source_rotation_age_myr"].as_f64().unwrap(),
                },
            },
            image_slot: i as u32,
        })
        .collect();
    let curve = manifest["sea_level_curve"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| (k[0].as_f64().unwrap(), k[1].as_f64().unwrap()))
        .collect();
    let timeline = Timeline::new(metas, curve).unwrap();
    let grids = frames
        .iter()
        .map(|f| read_tfgrid(&fs::read(dir.join(f["file"].as_str().unwrap())).unwrap()).unwrap())
        .collect();
    Tiny {
        crust,
        timeline,
        grids,
    }
}

fn dir(lat: f64, lon: f64) -> RenderDir {
    geo_to_render(latlon_to_geo(lat, lon))
}

/// Nearest-texel elevation (channel R) of a frame grid.
fn sample(grid: &Grid, d: RenderDir) -> f64 {
    let (face, col, row) = dir_to_texel(d, grid.height);
    f64::from(grid.texel_f32(face as usize, row, col, 0))
}

impl Tiny {
    fn sampling(&self, age_myr: f64) -> DeepSampling {
        match self.timeline.select_frames(age_myr) {
            FrameSelection::Deep(d) => DeepSampling::from_bracket(&self.timeline, age_myr, &d),
            other => panic!("expected a deep selection at {age_myr}, got {other:?}"),
        }
    }

    fn surface(&self, age_myr: f64, d: RenderDir) -> RenderedSurface {
        let sampling = self.sampling(age_myr);
        let plates = self.crust.rotations().plate_ids.len() as u32;
        let rotations: Vec<PlateRotations> = (0..plates)
            .map(|p| plate_rotations(self.crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let ranks = source_id_ranks(self.crust.rotations());
        let frames = sampling.endpoints.map(|e| e.frame);
        rendered_surface(&self.crust, d, &sampling, &rotations, &ranks, |e, d| {
            sample(&self.grids[frames[e]], d)
        })
    }

    fn plate_index(&self, id: u32) -> PlateIndex {
        self.crust.rotations().index_of(PlateId(id)).unwrap()
    }
}

fn land_of(s: RenderedSurface) -> Option<u32> {
    match s {
        RenderedSurface::Land { plate_id, .. } => Some(plate_id.0),
        RenderedSurface::Ocean => None,
    }
}

#[test]
fn tiny_bake_builds_one_plate_per_triangle_in_source_id_order() {
    let t = tiny();
    let ranks = source_id_ranks(t.crust.rotations());
    let pieces = build_plate_pieces(t.crust.footprints(), &ranks, 2);
    assert!(pieces.triangle_count() > 0);
    let plates: Vec<u32> = (0..pieces.triangle_count())
        .map(|i| pieces.triangle_plate(i))
        .collect();
    let rank_order: Vec<u32> = plates.iter().map(|&p| ranks[p as usize]).collect();
    // A later primitive wins exact depth ties, so ranks must not increase along the draw.
    assert!(later_draw_wins_ties(PIECE_DEPTH_COMPARE));
    assert!(
        rank_order.windows(2).all(|w| w[0] >= w[1]),
        "stable draw order, lowest source ID last"
    );
    let p101 = t.plate_index(101).0;
    let p202 = t.plate_index(202).0;
    assert!(plates.contains(&p101) && plates.contains(&p202));
    assert_eq!(
        *plates.last().unwrap(),
        p101,
        "the lower source ID draws last"
    );
}

#[test]
fn present_day_plates_sit_on_their_source_coasts() {
    let t = tiny();
    assert_eq!(land_of(t.surface(0.03, dir(0.0, -35.0))), Some(101));
    assert_eq!(land_of(t.surface(0.03, dir(0.0, 25.0))), Some(202));
    assert_eq!(land_of(t.surface(0.03, dir(0.0, 120.0))), None);
}

#[test]
fn rift_moves_plates_rigidly_fills_the_gap_and_resolves_the_overlap() {
    let t = tiny();
    // Plate 101 moved 40 deg east; plate 202 stayed.
    assert_eq!(
        land_of(t.surface(540.0, dir(0.0, 5.0))),
        Some(101),
        "101 now covers lon 5"
    );
    assert_eq!(
        land_of(t.surface(540.0, dir(0.0, 40.0))),
        Some(202),
        "202 still alone at lon 40"
    );
    // Behind plate 101 the ocean shell shows.
    assert_eq!(
        land_of(t.surface(540.0, dir(0.0, -45.0))),
        None,
        "the rift gap is ocean"
    );
    // The overlap (lon 0..30): equal source land on both; the lower source ID wins the tie.
    assert_eq!(land_of(t.surface(540.0, dir(0.0, 15.0))), Some(101));

    // The pieces themselves: plate 101's vertices turn 40 deg about the pole, 202's do not.
    let sampling = t.sampling(540.0);
    let gpu = plate_gpu_data(&t.crust, &sampling);
    let p = dir(10.0, -35.0).0.as_vec3();
    let moved = piece_world_dir(&gpu[t.plate_index(101).0 as usize], p);
    let expected = Quat::from_rotation_y(40f32.to_radians()) * p;
    assert!((moved - expected).length() < 1e-3, "{moved} vs {expected}");
    let still = piece_world_dir(&gpu[t.plate_index(202).0 as usize], p);
    assert!((still - p).length() < 1e-5);
}

/// Writes a `.tfplates` (the format `read_tfplates` reads).
fn write_tfplates(path: &std::path::Path, fp: &timefly_core::geodynamics::PlateFootprints) {
    let mut raw = Vec::new();
    for o in &fp.offsets {
        raw.extend_from_slice(&o.to_le_bytes());
    }
    for e in &fp.entries {
        raw.extend_from_slice(&e.plate_index.0.to_le_bytes());
        raw.extend_from_slice(&e.younger_myr.to_le_bytes());
        raw.extend_from_slice(&e.older_myr.to_le_bytes());
    }
    let z =
        ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
    let mut out = b"TFPLAT01".to_vec();
    out.extend_from_slice(&fp.cube_size.to_le_bytes());
    out.extend_from_slice(&(fp.entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&(z.len() as u32).to_le_bytes());
    out.extend_from_slice(&z);
    fs::write(path, out).unwrap();
}

/// A copy of the tiny bake whose plate 101 cells north of the equator expire at 250 Ma: at
/// 200 Ma they draw but only their younger endpoint (0 Ma) is available; at 300 Ma they are hidden.
fn tiny_with_expiry() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("globe_fixture-expiry-{}", std::process::id()));
    let globe = root.join("data/globe");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(globe.join("frames")).unwrap();
    for entry in fs::read_dir(globe_dir().join("frames")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), globe.join("frames").join(entry.file_name())).unwrap();
    }
    for f in ["manifest.json", "rotations.tfrot"] {
        fs::copy(globe_dir().join(f), globe.join(f)).unwrap();
    }
    let t = tiny();
    let p101 = t.plate_index(101);
    let mut fp = t.crust.footprints().clone();
    let n = fp.cube_size;
    for cell in 0..fp.offsets.len() - 1 {
        let face = timefly_core::cube::CubeFace::ALL[cell / (n * n) as usize];
        let within = (cell % (n * n) as usize) as u32;
        let centre = timefly_core::cube::texel_center_dir(face, within % n, within / n, n);
        if centre.0.y > 0.0 {
            for e in &mut fp.entries[fp.offsets[cell] as usize..fp.offsets[cell + 1] as usize] {
                if e.plate_index == p101 {
                    e.older_myr = 250.0;
                }
            }
        }
    }
    write_tfplates(&globe.join("plates.tfplates"), &fp);
    root
}

/// Lat/lon grid directions (render frame) over the fixture's plates.
fn grid_dirs(step_deg: f64) -> Vec<Vec3> {
    let mut out = Vec::new();
    let mut lat = -56.0;
    while lat <= 56.0 {
        let mut lon = -80.0;
        while lon <= 70.0 {
            out.push(dir(lat, lon).0.as_vec3());
            lon += step_deg;
        }
        lat += step_deg;
    }
    out
}

/// Renders the fixture (with expiring cells) headless through the real shaders and compares the
/// shader's result with the CPU reference, direction by direction: in the verification view
/// (winning plate or ocean) at four ages covering the sea-level bridge frame, distinct blended
/// endpoints with per-endpoint availability, expired-cell hiding and the overlap tie; and in the
/// painterly view (land/water via `classify`) at two ages. Needs a GPU adapter.
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn shader_matches_the_cpu_reference_on_the_two_plate_fixture() {
    use timefly::globe::capture::{
        CaptureSession, ParityStats, compare_mask_with_reference, compare_with_reference,
    };
    let root = tiny_with_expiry();
    let t = tiny_at(&root.join("data/globe"));
    let p101 = t.plate_index(101).0;
    let p202 = t.plate_index(202).0;
    let cpu_plate = |age: f64| {
        let t = &t;
        move |d: Vec3| match t.surface(age, RenderDir(d.as_dvec3().normalize())) {
            RenderedSurface::Land { plate_id, .. } => {
                Some(t.crust.rotations().index_of(plate_id).unwrap().0)
            }
            RenderedSurface::Ocean => None,
        }
    };
    // One footprint cell at cube size 8.
    let tolerance = (90.0f32 / 8.0).to_radians();
    let max_angle = 60f32.to_radians();
    let dirs = grid_dirs(2.0);
    let distance = 3.0 * timefly_core::globe::EARTH_RADIUS_KM;

    // The cases the ages are meant to exercise hold in the CPU reference.
    let north101 = dir(20.0, -35.0).0.as_vec3();
    assert_eq!(cpu_plate(0.03)(north101), Some(p101));
    assert_eq!(
        cpu_plate(300.0)(dir(20.0, -10.0).0.as_vec3()),
        None,
        "expired, hidden"
    );
    assert_eq!(cpu_plate(300.0)(dir(-20.0, -10.0).0.as_vec3()), Some(p101));
    assert_eq!(
        cpu_plate(540.0)(dir(-10.0, 15.0).0.as_vec3()),
        Some(p101),
        "overlap tie"
    );

    let mut session = CaptureSession::new(&root, (480, 480), true).expect("session");
    let mut total = ParityStats::default();
    for age_myr in [0.03, 200.0, 300.0, 540.0] {
        let shot = session
            .shoot(age_myr * 1e6, (0.0, -10.0), distance)
            .expect("capture");
        let stats = compare_with_reference(
            &shot,
            dirs.iter().copied(),
            cpu_plate(age_myr),
            tolerance,
            max_angle,
        );
        println!(
            "{age_myr} Ma: compared {}, plate agree {}, near boundary {}, unreadable {}, mismatches {:?}",
            stats.compared,
            stats.plate_agree,
            stats.near_boundary,
            stats.unreadable,
            stats.mismatches
        );
        if let Ok(out) = std::env::var("GLOBE_FIXTURE_SHOTS") {
            shot.save_png(&PathBuf::from(out).join(format!("ids-{age_myr}.png")))
                .unwrap();
            for (d, _, _) in &stats.mismatches {
                let around: Vec<_> = (-2..=2)
                    .map(|k| shot.pixel_at(*d + Vec3::new(0.004 * k as f32, 0.0, 0.0)))
                    .collect();
                println!("mismatch {d}: along x {around:?}, p101 {p101} p202 {p202}");
            }
        }
        assert!(
            stats.compared > 150,
            "{age_myr} Ma: too few comparable directions"
        );
        assert!(
            stats.mismatches.is_empty(),
            "{age_myr} Ma: {:?}",
            stats.mismatches
        );
        // Both plates and the ocean were actually compared.
        let seen = |want: Option<u32>| {
            dirs.iter().any(|&d| {
                d.angle_between(shot.view_dir()) < max_angle && cpu_plate(age_myr)(d) == want
            })
        };
        assert!(seen(Some(p101)) && seen(None), "{age_myr} Ma");
        if age_myr != 300.0 {
            assert!(seen(Some(p202)), "{age_myr} Ma");
        }
        total.add(&stats);
    }
    println!(
        "verification view total: {} compared, {} agree",
        total.compared, total.plate_agree
    );
    drop(session);

    let mut session = CaptureSession::new(&root, (480, 480), false).expect("session");
    for age_myr in [200.0, 540.0] {
        let shot = session
            .shoot(age_myr * 1e6, (0.0, -10.0), distance)
            .expect("capture");
        let cpu = cpu_plate(age_myr);
        let stats = compare_mask_with_reference(
            &shot,
            dirs.iter().copied(),
            |d| cpu(d).is_some(),
            tolerance,
            // The atmosphere rim tints water toward the limb, so colours are only read
            // head-on.
            45f32.to_radians(),
        );
        println!(
            "painterly {age_myr} Ma: compared {}, mask agree {}, mismatches {:?}",
            stats.compared, stats.mask_agree, stats.mismatches
        );
        assert!(stats.compared > 150);
        assert!(
            stats.mismatches.is_empty(),
            "{age_myr} Ma: {:?}",
            stats.mismatches
        );
    }
}
