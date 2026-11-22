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

use bevy::math::Quat;
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
use timefly::globe::mesh::build_plate_pieces;

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
    let dir = globe_dir();
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
    assert!(
        rank_order.windows(2).all(|w| w[0] <= w[1]),
        "stable draw order"
    );
    let p101 = t.plate_index(101).0;
    let p202 = t.plate_index(202).0;
    assert!(plates.contains(&p101) && plates.contains(&p202));
    assert_eq!(plates[0], p101, "the lower source ID draws first");
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

/// Renders the tiny bake headless through the real shaders at 540 Ma and checks the gap and the
/// overlap in the image. Needs a GPU adapter, so it is ignored by default.
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn tiny_bake_renders_gap_and_overlap_through_the_shader() {
    use timefly::globe::capture::{CaptureRequest, capture_globe};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets");
    let shot = capture_globe(&CaptureRequest {
        asset_root: root,
        age_years: 540.0e6,
        size: (512, 512),
        look_at_lat_lon: (0.0, -5.0),
        distance_km: 3.0 * timefly_core::globe::EARTH_RADIUS_KM,
    })
    .expect("capture");
    let land = |lat, lon| shot.classify(dir(lat, lon).0.as_vec3());
    use timefly::globe::capture::Pixel;
    assert_eq!(
        land(0.0, -45.0),
        Pixel::Water,
        "the rift gap shows the ocean shell"
    );
    assert_eq!(land(0.0, 15.0), Pixel::Land, "the overlap shows land");
    assert_eq!(land(0.0, 40.0), Pixel::Land, "plate 202 shows land");
    assert_eq!(land(0.0, 5.0), Pixel::Land, "plate 101 moved east");
    assert_eq!(land(45.0, -5.0), Pixel::Water, "open ocean");
}
