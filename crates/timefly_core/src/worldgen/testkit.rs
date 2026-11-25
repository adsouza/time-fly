//! Synthetic fixtures for the worldgen tests: a two-plate crust model (M1's `globe.rs` rift),
//! a Milestone-2-shaped timeline, cube frames and tiles.

use bevy_math::{DVec3, Quat};

use crate::cube::{CubeFace, texel_center_dir};
use crate::formats::{DType, Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use crate::geodynamics::{
    CrustModel, FootprintEntry, PlateFootprints, PlateId, PlateIndex, RotationTable,
};
use crate::hydrology::{CubeLattice, Down, EraHydrology, HydroGraph, Lake, RiverGraph};
use crate::surface::{FrameMeta, FrameSource, Timeline};
use crate::worldgen::coarse::{CoarseField, CoarseSample, LatticeKey};
use crate::worldgen::frame::RegionFrame;
use crate::worldgen::tiles::{TileSet, tests::tile_from};

/// A synthetic coarse field: elevation from a function of the direction, geographic keys.
pub struct FnField<G: Fn(RenderDir) -> f64 + Sync> {
    pub f: G,
    pub spacing_m: f64,
    pub koppen: Option<u8>,
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

/// One river of a synthetic graph: lattice nodes in flow order, each with its discharge and pass
/// elevation; the last node drains to `end` ([`Down::River`] names a *lattice* node of another
/// chain).
#[derive(Clone, Debug)]
pub struct RiverChain {
    pub nodes: Vec<u32>,
    pub q: Vec<f32>,
    pub bed: Vec<f32>,
    pub end: Down,
}

/// An era's hydrology holding exactly `chains` (beds become the running minimum downstream, as
/// the pass makes them), no ocean bits and no lake footprints.
pub fn river_hydro(lattice: CubeLattice, chains: &[RiverChain], lakes: Vec<Lake>) -> EraHydrology {
    let mut info: std::collections::BTreeMap<u32, (Down, f32, f32)> = Default::default();
    for c in chains {
        for (k, &v) in c.nodes.iter().enumerate() {
            let down = match c.nodes.get(k + 1) {
                Some(&next) => Down::River(next),
                None => c.end,
            };
            assert!(
                info.insert(v, (down, c.q[k], c.bed[k])).is_none(),
                "node {v} is in two chains"
            );
        }
    }
    let nodes: Vec<u32> = info.keys().copied().collect();
    for (&v, &(down, _, _)) in &info {
        let end = match down {
            Down::River(j) | Down::Ocean(j) | Down::Lake(_, j) | Down::Ice(j) => j,
        };
        assert!(
            lattice.neighbours(v).contains(&Some(end)),
            "{v} -> {end} is not a lattice step"
        );
    }
    let rivers = RiverGraph::build(
        &lattice,
        nodes,
        |v| info[&v].0,
        |v| (info[&v].1, 1000.0, info[&v].2),
        &lakes,
    );
    EraHydrology::with_rivers(lattice, lakes, rivers)
}

/// The nodes reached from `start` by `count` steps in neighbour direction `step` (0 = E, 2 = N,
/// [`crate::hydrology::NEIGHBOUR_STEPS`]), `start` first.
pub fn lattice_walk(lattice: &CubeLattice, start: u32, step: usize, count: u32) -> Vec<u32> {
    let mut out = vec![start];
    for _ in 0..count {
        let last = *out.last().expect("start is in");
        out.push(lattice.neighbours(last)[step].expect("an interior step"));
    }
    out
}

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

/// Constant tiles over everything the inner field around `centre` reads (no overview).
pub fn flat_tiles(centre: RenderDir, elevation_m: f32, koppen: u8) -> TileSet {
    flat_tiles_around(&[centre], elevation_m, koppen)
}

/// Constant tiles for the inner fields around each centre (no overview).
pub fn flat_tiles_around(centres: &[RenderDir], elevation_m: f32, koppen: u8) -> TileSet {
    let mut set = TileSet::new();
    for &centre in centres {
        for (r, c) in TileSet::needed_for(&RegionFrame { center: centre }) {
            if !set.contains(r, c) {
                set.insert(r, c, tile_from(r, c, |_, _| (elevation_m, koppen)))
                    .unwrap();
            }
        }
    }
    set
}

/// One constant tile around a latitude and longitude.
pub fn single_tile(lat: f64, lon: f64, elevation_m: f32, koppen: u8) -> TileSet {
    let (r, c) = crate::worldgen::tiles::tile_of(lat, lon);
    let mut set = TileSet::new();
    set.insert(r, c, tile_from(r, c, |_, _| (elevation_m, koppen)))
        .unwrap();
    set
}

/// Timeline-indexed frames: ICE-6G placeholders, then PaleoDEMs from `paleo(frame)`.
pub fn frames(paleo: impl Fn(usize, RenderDir) -> (f32, bool)) -> Vec<Grid> {
    (0..7)
        .map(|i| {
            if i < 4 {
                ice_frame(0.0, 0.0)
            } else {
                paleo_frame(|d| paleo(i, d))
            }
        })
        .collect()
}

/// A constant global overview at 2 nodes per degree (the bake's is 12; the shape rule is the
/// same and this one builds fast).
pub fn flat_overview(elevation_m: f32, koppen: u8) -> Grid {
    crate::worldgen::tiles::tests::overview_from(2, |_, _| (elevation_m, koppen))
}

/// Two plates meeting along the 0° meridian of their fixed frames on a cube-16 footprint grid
/// (its cell edges run exactly along it on the +Z face): plate 0 (id 202) over `x >= 0`, plate 1
/// (id 101) over `x < 0`. By 100 Ma plate 1 has turned `gap_deg` west about the pole relative to
/// plate 0, and both have turned 45° about render +Z (lat 0, lon 0). That leaves a straight
/// crack `gap_deg` wide where no plate is valid, running diagonally through lat 0, lon 0 in the
/// era frame: the quantisation sliver of the real bake, in miniature. `crack_dir` gives the era
/// direction at a plate-0-fixed latitude and longitude.
pub fn sliver(gap_deg: f32) -> CrustModel {
    let twist = Quat::from_rotation_z(45f32.to_radians());
    let at = |t: f32| Some(Quat::IDENTITY.slerp(twist, t));
    let plates = vec![
        (202, vec![Some(Quat::IDENTITY), at(0.0447), at(1.0)]),
        (
            101,
            vec![
                Some(Quat::IDENTITY),
                Some(at(0.0447).unwrap() * about_y(-gap_deg * 0.0447)),
                Some(twist * about_y(-gap_deg)),
            ],
        ),
    ];
    let t = table(&[0.0, 4.47, 100.0], &plates);
    let f = footprints(16, |d| {
        if d.x >= 0.0 {
            vec![(0, 0.0, 540.0)]
        } else {
            vec![(1, 0.0, 540.0)]
        }
    });
    CrustModel::new(t, f).unwrap()
}

/// The era direction (at 100 Ma) of a point given in [`sliver`]'s plate-0-fixed frame.
pub fn crack_dir(lat: f64, lon: f64) -> RenderDir {
    let twist = bevy_math::DQuat::from_rotation_z(45f64.to_radians());
    RenderDir(twist * dir(lat, lon).0)
}
