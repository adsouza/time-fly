//! Synthetic fixtures for the worldgen tests: a two-plate crust model (M1's `globe.rs` rift),
//! a Milestone-2-shaped timeline, cube frames and tiles.

use bevy_math::{DVec3, Quat};

use crate::cube::{CubeFace, texel_center_dir};
use crate::formats::{DType, Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use crate::geodynamics::{
    CrustModel, FootprintEntry, PlateFootprints, PlateId, PlateIndex, RotationTable,
};
use crate::surface::{FrameMeta, FrameSource, Timeline};
use crate::worldgen::frame::RegionFrame;
use crate::worldgen::tiles::{TileSet, tests::tile_from};

pub fn about_y(deg: f32) -> Quat {
    Quat::from_rotation_y(deg.to_radians())
}

pub fn dir(lat: f64, lon: f64) -> RenderDir {
    geo_to_render(latlon_to_geo(lat, lon))
}

pub fn latlon(d: RenderDir) -> (f64, f64) {
    geo_to_latlon(render_to_geo(d))
}

pub fn table(times: &[f64], plates: &[(u32, Vec<Option<Quat>>)]) -> RotationTable {
    let mut quats = Vec::new();
    let mut valid = Vec::new();
    for t in 0..times.len() {
        for (_, samples) in plates {
            let q = samples[t].map_or(Quat::IDENTITY, |q| if q.w < 0.0 { -q } else { q });
            quats.push(q.to_array());
            valid.push(samples[t].is_some());
        }
    }
    RotationTable {
        plate_ids: plates.iter().map(|(id, _)| PlateId(*id)).collect(),
        times_myr: times.to_vec(),
        quats,
        valid,
    }
}

pub fn footprints(n: u32, cell: impl Fn(DVec3) -> Vec<(u32, f64, f64)>) -> PlateFootprints {
    let mut offsets = vec![0];
    let mut entries = Vec::new();
    for face in CubeFace::ALL {
        for row in 0..n {
            for col in 0..n {
                for (i, y, o) in cell(texel_center_dir(face, col, row, n).0) {
                    entries.push(FootprintEntry {
                        plate_index: PlateIndex(i),
                        younger_myr: y,
                        older_myr: o,
                    });
                }
                offsets.push(entries.len() as u32);
            }
        }
    }
    PlateFootprints {
        cube_size: n,
        offsets,
        entries,
    }
}

/// M1's rift on a cube-16 footprint grid: plate 0 (id 202) fixed over `z > 0.3`; plate 1 (id 101)
/// over `x < -0.3` at present, turning +90° about +Y by 100 Ma, onto plate 0's side. `extra`
/// footprint-less plates (ids 1000+) add the per-plate cost of a real bake's 149 plates.
pub fn rift(extra: u32) -> CrustModel {
    let mut plates = vec![
        (202, vec![Some(Quat::IDENTITY); 3]),
        (
            101,
            vec![
                Some(Quat::IDENTITY),
                Some(about_y(4.0)),
                Some(about_y(90.0)),
            ],
        ),
    ];
    plates.extend((0..extra).map(|i| (1000 + i, vec![Some(Quat::IDENTITY); 3])));
    let t = table(&[0.0, 4.47, 100.0], &plates);
    let f = footprints(16, |d| {
        let mut e = Vec::new();
        if d.z > 0.3 {
            e.push((0, 0.0, 540.0));
        }
        if d.x < -0.3 {
            e.push((1, 0.0, 540.0));
        }
        e
    });
    CrustModel::new(t, f).unwrap()
}

/// The real bake's shape: ICE-6G at 0, 20.076, 21.076 and 26.076 ka; the modern-terrain bridge
/// frame at 30 ka; PaleoDEMs at 4.47 Ma and 100 Ma. The sea-level curve reaches -50 m at 30 ka
/// and -100 m at 100 ka.
pub fn timeline() -> Timeline {
    let frames = [
        (0.0, None),
        (0.020076, None),
        (0.021076, None),
        (0.026076, None),
        (0.03, Some(0.0)),
        (4.47, Some(4.47)),
        (100.0, Some(100.0)),
    ];
    let metas = frames
        .iter()
        .enumerate()
        .map(|(i, &(age_myr, source))| FrameMeta {
            age_myr,
            source: match source {
                None => FrameSource::Ice6g,
                Some(s) => FrameSource::PaleoDem {
                    source_rotation_age_myr: s,
                },
            },
            image_slot: i as u32,
        })
        .collect();
    Timeline::new(metas, vec![(0.0, 0.0), (0.03, -50.0), (0.1, -100.0)]).unwrap()
}

/// A cube grid with `channels` F16 channels, each a function of the texel-centre direction.
pub fn cube(n: u32, channels: u8, f: impl Fn(RenderDir) -> Vec<f32>) -> Grid {
    let mut data = Vec::new();
    for face in CubeFace::ALL {
        for row in 0..n {
            for col in 0..n {
                let v = f(texel_center_dir(face, col, row, n));
                assert_eq!(v.len(), channels as usize);
                for x in v {
                    data.extend_from_slice(&half::f16::from_f32(x).to_le_bytes());
                }
            }
        }
    }
    Grid {
        kind: GridKind::Cube,
        dtype: DType::F16,
        channels,
        width: n,
        height: n,
        data,
    }
}

/// An ICE-6G frame: signed topography, land fraction, ice fraction (and an unused alpha).
pub fn ice_frame(topo_m: f32, ice: f32) -> Grid {
    cube(8, 4, |_| {
        vec![topo_m, if topo_m > 0.0 { 1.0 } else { 0.0 }, ice, 0.0]
    })
}

/// A PaleoDEM frame: elevation and the plate-coverage mask.
pub fn paleo_frame(f: impl Fn(RenderDir) -> (f32, bool)) -> Grid {
    cube(16, 2, |d| {
        let (e, covered) = f(d);
        vec![e, if covered { 1.0 } else { 0.0 }]
    })
}

/// Constant tiles over everything a field of side `size_m` around `centre` reads.
pub fn flat_tiles(centre: RenderDir, size_m: f64, elevation_m: f32, koppen: u8) -> TileSet {
    let frame = RegionFrame { center: centre };
    let mut set = TileSet::new();
    for (r, c) in TileSet::needed_for(&frame, size_m) {
        set.insert(r, c, tile_from(r, c, |_, _| (elevation_m, koppen)))
            .unwrap();
    }
    set
}
