//! Globe sampling rules: how the plate renderer chooses rotations, endpoints and the visible
//! surface (spec §7.1). The shader (`assets/shaders/globe.wesl`) mirrors these functions, and the
//! CPU reference built from them ([`rendered_surface`]) is what the coastline diagnostic and the
//! fixture tests compare against.
//!
//! Rules, per plate piece and per sample:
//! - The piece draws only while its plate has a current-age rotation and its footprint cell
//!   accepts the current age ([`piece_visible`]).
//! - Each deep endpoint (a PaleoDEM frame) is sampled at `rotation_at(plate, source age) * p`,
//!   where `p` is the plate-fixed sample direction. An endpoint is available only with a valid
//!   source rotation and a footprint accepting `p` at that source age ([`endpoint_available`]).
//!   Both available: blend by the older weight. One: use it alone. None: the ocean shell shows.
//! - Sea level applies per endpoint as `elev − anomaly` before blending; only the modern-terrain
//!   bridge frame ever has a nonzero anomaly (the selection decides).
//! - A sample at or below sea level is not land, so the ocean shell shows through.
//! - Overlapping land: the highest displaced surface wins ([`ranking_surface_km`]); exact ties go
//!   to the lower source ID. There is no radial bias: the shader ranks every plate at a pixel at
//!   the same era direction, so exactly tied plates get bit-identical depths, and the draw order
//!   (lowest source ID last under a `GreaterEqual` depth test) breaks the tie.
//! - The ocean shell shows each endpoint frame's own land where that frame's plate-coverage mask
//!   says no plate covered it at the frame's source age (terrain-drift gaps); elsewhere it shows
//!   water, with land-valued samples clamped to a shallow fallback ([`shell_surface_m`]).

use bevy_math::Quat;

use crate::geo::RenderDir;
use crate::geodynamics::{CrustModel, PlateFootprints, PlateId, PlateIndex, RotationTable};
use crate::surface::{DeepBracket, FrameSource, Timeline, effective_elevation_m};

/// Mean Earth radius; 1 world unit = 1 km.
pub const EARTH_RADIUS_KM: f64 = 6371.0;
/// Vertical exaggeration of displaced terrain *(call)*.
pub const ELEVATION_EXAGGERATION: f64 = 20.0;
/// Ocean-shell depth used where a source grid has land that no plate piece covers (a coverage
/// gap or a source artefact): shallow, since such samples sit on or next to land.
pub const OCEAN_GAP_FALLBACK_M: f64 = -200.0;

/// The ocean shell's tint elevation for an effective source sample: water keeps its depth,
/// land-valued (or NaN) samples take [`OCEAN_GAP_FALLBACK_M`]. Mirrored by `shell_elevation`.
pub fn ocean_shell_elevation_m(effective_elevation_m: f64) -> f64 {
    if effective_elevation_m <= 0.0 {
        effective_elevation_m
    } else {
        OCEAN_GAP_FALLBACK_M
    }
}

/// One PaleoDEM endpoint of a deep bracket, as the plate renderer samples it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DeepEndpoint {
    /// Timeline frame index.
    pub frame: usize,
    /// The reconstruction age its source grid was published at.
    pub source_rotation_age_myr: f64,
    pub sea_level_anomaly_m: f64,
}

/// Everything the plate renderer needs to sample the deep path at one age.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DeepSampling {
    /// The age the pieces are placed at (their current rotation).
    pub age_myr: f64,
    /// `[younger, older]`.
    pub endpoints: [DeepEndpoint; 2],
    pub older_weight: f64,
}

/// The source reconstruction age of a frame; an ICE-6G_C frame is in present geography.
fn source_rotation_age_myr(timeline: &Timeline, frame: usize) -> f64 {
    match timeline.frames()[frame].source {
        FrameSource::PaleoDem {
            source_rotation_age_myr,
        } => source_rotation_age_myr,
        FrameSource::Ice6g => 0.0,
    }
}

impl DeepSampling {
    /// The deep sampling at `age_myr` for a deep bracket from [`Timeline::select_frames`].
    pub fn from_bracket(timeline: &Timeline, age_myr: f64, deep: &DeepBracket) -> DeepSampling {
        let endpoint = |frame: usize, anomaly: f64| DeepEndpoint {
            frame,
            source_rotation_age_myr: source_rotation_age_myr(timeline, frame),
            sea_level_anomaly_m: anomaly,
        };
        DeepSampling {
            age_myr,
            endpoints: [
                endpoint(deep.frames.younger, deep.sea_level_anomaly_m[0]),
                endpoint(deep.frames.older, deep.sea_level_anomaly_m[1]),
            ],
            older_weight: deep.frames.older_weight,
        }
    }

    /// One frame on its own, placed at `placement_age_myr`, with the given sea-level anomaly. The
    /// coastline diagnostic places a frame at its source age to compare with the source grid.
    pub fn single_frame(
        timeline: &Timeline,
        frame: usize,
        placement_age_myr: f64,
        sea_level_anomaly_m: f64,
    ) -> DeepSampling {
        let endpoint = DeepEndpoint {
            frame,
            source_rotation_age_myr: source_rotation_age_myr(timeline, frame),
            sea_level_anomaly_m,
        };
        DeepSampling {
            age_myr: placement_age_myr,
            endpoints: [endpoint, endpoint],
            older_weight: 0.0,
        }
    }

    pub fn source_ages_myr(&self) -> [f64; 2] {
        self.endpoints.map(|e| e.source_rotation_age_myr)
    }

    pub fn anomalies_m(&self) -> [f64; 2] {
        self.endpoints.map(|e| e.sea_level_anomaly_m)
    }
}

/// One plate's normalised rotations for a deep sampling: at the current age and at each
/// endpoint's source age. `None` marks a missing circuit or an age outside the table.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PlateRotations {
    pub current: Option<Quat>,
    pub sources: [Option<Quat>; 2],
}

/// Selects a plate's current and source-frame rotations. Interpolation is
/// [`RotationTable::rotation_at`]'s shortest-path slerp, so the bake's `w >= 0` sign
/// canonicalisation cannot send a rotation the long way round; the results are normalised
/// before anything is uploaded.
pub fn plate_rotations(
    rotations: &RotationTable,
    plate: PlateIndex,
    sampling: &DeepSampling,
) -> PlateRotations {
    let at = |age| rotations.rotation_at(plate, age).map(Quat::normalize);
    PlateRotations {
        current: at(sampling.age_myr),
        sources: sampling.source_ages_myr().map(at),
    }
}

/// Whether a piece draws: its plate has a current rotation and its cell accepts the current age.
pub fn piece_visible(current_rotation_valid: bool, footprint_accepts_current_age: bool) -> bool {
    current_rotation_valid && footprint_accepts_current_age
}

/// Whether a deep endpoint may be sampled: a valid source rotation and a footprint accepting the
/// sample at the endpoint's source age.
pub fn endpoint_available(source_rotation_valid: bool, footprint_accepts_source_age: bool) -> bool {
    source_rotation_valid && footprint_accepts_source_age
}

/// Endpoint weights `[younger, older]`: a blend when both are available, the available one alone,
/// and `None` (expose the ocean shell) when neither is.
pub fn endpoint_weights(available: [bool; 2], older_weight: f64) -> Option<[f64; 2]> {
    match available {
        [true, true] => Some([1.0 - older_weight, older_weight]),
        [true, false] => Some([1.0, 0.0]),
        [false, true] => Some([0.0, 1.0]),
        [false, false] => None,
    }
}

/// Blends endpoint elevations after applying each endpoint's sea-level anomaly. A zero-weight
/// endpoint is skipped, so its (possibly meaningless) sample cannot leak in as NaN.
pub fn blend_endpoints_m(weights: [f64; 2], elevation_m: [f64; 2], anomaly_m: [f64; 2]) -> f64 {
    (0..2)
        .filter(|&e| weights[e] != 0.0)
        .map(|e| weights[e] * effective_elevation_m(elevation_m[e], anomaly_m[e]))
        .sum()
}

/// The elevation if it is visible land (above sea level). NaN is not land.
pub fn visible_land_m(effective_elevation_m: f64) -> Option<f64> {
    (effective_elevation_m > 0.0).then_some(effective_elevation_m)
}

/// The radial displacement (km) of a sample: land only, exaggerated.
pub fn displacement_km(effective_elevation_m: f64) -> f64 {
    effective_elevation_m.max(0.0) * ELEVATION_EXAGGERATION / 1000.0
}

/// Each plate's tie rank: its position in ascending source-ID order, indexed by plate index.
pub fn source_id_ranks(rotations: &RotationTable) -> Vec<u32> {
    let mut order: Vec<usize> = (0..rotations.plate_ids.len()).collect();
    order.sort_by_key(|&i| rotations.plate_ids[i]);
    let mut ranks = vec![0; order.len()];
    for (rank, plate) in order.into_iter().enumerate() {
        ranks[plate] = rank as u32;
    }
    ranks
}

/// The surface the depth test ranks overlapping land by: the displaced height. Exact ties fall
/// to the lower source ID (in [`CrustModel::resolve`] and, on the GPU, by draw order).
pub fn ranking_surface_km(land_m: f64) -> f64 {
    displacement_km(land_m)
}

/// The rotation taking a plate's current era frame to an endpoint's source frame,
/// `source * current⁻¹`, computed in f64. Exactly the identity when the two are equal (a frame
/// shown at its own source age), so plates tied there sample bit-identical texture coordinates.
pub fn relative_rotation(source: Quat, current: Quat) -> Quat {
    if source == current {
        return Quat::IDENTITY;
    }
    let q = source.as_dquat().normalize() * current.as_dquat().normalize().conjugate();
    q.normalize().as_quat()
}

/// One frame sample in its published geographic frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameSample {
    pub elevation_m: f64,
    /// The frame's plate-coverage mask: some plate covered this texel at the frame's source age.
    pub covered: bool,
}

/// The ocean shell's elevation at an era direction from both endpoint frames' samples there:
/// per endpoint, uncovered texels keep their own (sea-level adjusted) elevation, so a terrain
/// gap shows the frame's land; covered texels go through [`ocean_shell_elevation_m`]. The two
/// are mixed by the older weight. Positive means gap land.
pub fn shell_surface_m(sampling: &DeepSampling, samples: [FrameSample; 2]) -> f64 {
    let anomalies = sampling.anomalies_m();
    let value = |e: usize| {
        let effective = effective_elevation_m(samples[e].elevation_m, anomalies[e]);
        if samples[e].covered {
            ocean_shell_elevation_m(effective)
        } else {
            effective
        }
    };
    let (a, b) = (value(0), value(1));
    a + (b - a) * sampling.older_weight
}

/// The plate renderer's visible land at plate-fixed direction `p` (the fragment rule), or `None`
/// where the ocean shell shows. `sample(endpoint, dir)` reads endpoint `0` (younger) or `1`
/// (older) at a direction in that endpoint's published frame.
pub fn plate_land_m(
    footprints: &PlateFootprints,
    plate: PlateIndex,
    rotations: &PlateRotations,
    p: RenderDir,
    sampling: &DeepSampling,
    sample: impl FnMut(usize, RenderDir) -> f64,
) -> Option<f64> {
    let dirs = rotations
        .sources
        .map(|q| q.map(|q| RenderDir((q.as_dquat() * p.0).normalize())));
    land_with_sample_dirs(footprints, plate, rotations, p, dirs, sampling, sample)
}

/// [`plate_land_m`] ranked as the depth test does: each endpoint is sampled at
/// `relative_rotation(source, current) * era_dir`, which is exactly `era_dir` for a frame shown
/// at its own source age, so plates tied there read bit-identical samples.
fn ranking_land_m(
    footprints: &PlateFootprints,
    plate: PlateIndex,
    rotations: &PlateRotations,
    p: RenderDir,
    era_dir: RenderDir,
    sampling: &DeepSampling,
    sample: impl FnMut(usize, RenderDir) -> f64,
) -> Option<f64> {
    let current = rotations.current?.normalize();
    let dirs = rotations.sources.map(|q| {
        q.map(|q| {
            let r = relative_rotation(q.normalize(), current);
            if r == Quat::IDENTITY {
                era_dir
            } else {
                RenderDir((r.as_dquat() * era_dir.0).normalize())
            }
        })
    });
    land_with_sample_dirs(footprints, plate, rotations, p, dirs, sampling, sample)
}

fn land_with_sample_dirs(
    footprints: &PlateFootprints,
    plate: PlateIndex,
    rotations: &PlateRotations,
    p: RenderDir,
    dirs: [Option<RenderDir>; 2],
    sampling: &DeepSampling,
    mut sample: impl FnMut(usize, RenderDir) -> f64,
) -> Option<f64> {
    let ages = sampling.source_ages_myr();
    let available = [0, 1].map(|e| {
        endpoint_available(
            rotations.sources[e].is_some(),
            footprints.contains(plate, p, ages[e]),
        )
    });
    let weights = endpoint_weights(available, sampling.older_weight)?;
    let elevation = [0, 1].map(|e| match dirs[e] {
        Some(d) if weights[e] != 0.0 => sample(e, d),
        _ => 0.0,
    });
    visible_land_m(blend_endpoints_m(
        weights,
        elevation,
        sampling.anomalies_m(),
    ))
}

/// What the plate renderer shows at an era-frame direction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum RenderedSurface {
    /// Land of the winning plate, with its effective elevation.
    Land { plate_id: PlateId, elevation_m: f64 },
    /// Frame land on the ocean shell, where no plate covered it at the frame's source age.
    GapLand { elevation_m: f64 },
    /// The ocean shell (a gap, an expired piece, or water on every candidate).
    Ocean,
}

impl RenderedSurface {
    pub fn is_land(&self) -> bool {
        !matches!(self, RenderedSurface::Ocean)
    }
}

/// The CPU reference of the plate renderer at an era-frame direction: every candidate plate is
/// tried through [`CrustModel::resolve`] with the depth test's ranking surface, so overlaps and
/// ties resolve exactly as the GPU does; without plate land, the ocean shell's rule applies.
/// `rotations` is indexed by plate index; `sample(endpoint, dir)` reads a frame in its
/// published frame.
pub fn rendered_surface(
    crust: &CrustModel,
    era_dir: RenderDir,
    sampling: &DeepSampling,
    rotations: &[PlateRotations],
    mut sample: impl FnMut(usize, RenderDir) -> FrameSample,
) -> RenderedSurface {
    let mut lands: Vec<(PlateId, f64)> = Vec::new();
    let era = RenderDir(era_dir.0.normalize());
    let resolved = crust.resolve(era_dir, sampling.age_myr, |c| {
        let index = c.plate_index.0 as usize;
        let land = ranking_land_m(
            crust.footprints(),
            c.plate_index,
            &rotations[index],
            c.plate_fixed_dir,
            era,
            sampling,
            |e, d| sample(e, d).elevation_m,
        )?;
        lands.push((c.plate_id, land));
        Some(ranking_surface_km(land))
    });
    let winner = match resolved {
        crate::geodynamics::ResolvedPosition::Tracked(pos) => {
            lands.iter().find(|(id, _)| *id == pos.plate_id).copied()
        }
        crate::geodynamics::ResolvedPosition::EraFixed { .. } => None,
    };
    if let Some((plate_id, elevation_m)) = winner {
        return RenderedSurface::Land {
            plate_id,
            elevation_m,
        };
    }
    let shell = shell_surface_m(sampling, [sample(0, era), sample(1, era)]);
    match visible_land_m(shell) {
        Some(elevation_m) => RenderedSurface::GapLand { elevation_m },
        None => RenderedSurface::Ocean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::{CubeFace, texel_center_dir};
    use crate::geo::{geo_to_render, latlon_to_geo};
    use crate::geodynamics::FootprintEntry;
    use crate::surface::{FrameMeta, FrameSelection};
    use bevy_math::{DVec3, Vec3};

    fn about_y(deg: f32) -> Quat {
        Quat::from_rotation_y(deg.to_radians())
    }

    /// The bake stores each sample with `w >= 0`.
    fn canonical(q: Quat) -> Quat {
        if q.w < 0.0 { -q } else { q }
    }

    fn table(times: &[f64], plates: &[(u32, Vec<Option<Quat>>)]) -> RotationTable {
        let mut quats = Vec::new();
        let mut valid = Vec::new();
        for t in 0..times.len() {
            for (_, samples) in plates {
                quats.push(samples[t].map_or(Quat::IDENTITY, canonical).to_array());
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

    fn timeline(frames: &[(f64, Option<f64>)]) -> Timeline {
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
        Timeline::new(metas, vec![(0.0, 0.0)]).unwrap()
    }

    fn footprints(n: u32, cell: impl Fn(DVec3) -> Vec<(u32, f64, f64)>) -> PlateFootprints {
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

    #[test]
    fn rotation_across_180_degrees_takes_the_short_way() {
        // about_y(190) has w < 0, so the bake stores its negation: q and -q neighbours.
        assert!(about_y(190.0).w < 0.0);
        let t = table(
            &[0.0, 10.0],
            &[(101, vec![Some(about_y(170.0)), Some(about_y(190.0))])],
        );
        assert!(Quat::from_array(t.quats[1]).w >= 0.0);
        let s = timeline(&[(0.0, None), (0.03, Some(0.0)), (10.0, Some(10.0))]);
        let deep = match s.select_frames(5.0) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let r = plate_rotations(
            &t,
            PlateIndex(0),
            &DeepSampling::from_bracket(&s, 5.0, &deep),
        );
        let mid = r.current.unwrap();
        assert!(
            (mid * Vec3::Z - Vec3::NEG_Z).length() < 1e-5,
            "{:?}",
            mid * Vec3::Z
        );
        assert!((mid.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn current_and_source_rotations_are_selected_separately() {
        // A PaleoDEM placed at 66 Ma on the timeline but reconstructed at 65 Ma.
        let t = table(
            &[0.0, 65.0, 66.0, 100.0],
            &[(
                101,
                vec![
                    Some(Quat::IDENTITY),
                    Some(about_y(10.0)),
                    Some(about_y(20.0)),
                    Some(about_y(60.0)),
                ],
            )],
        );
        let s = timeline(&[
            (0.0, None),
            (0.03, Some(0.0)),
            (66.0, Some(65.0)),
            (100.0, Some(100.0)),
        ]);
        let deep = match s.select_frames(66.0) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let sampling = DeepSampling::from_bracket(&s, 66.0, &deep);
        assert_eq!(sampling.source_ages_myr(), [65.0, 65.0]);
        let r = plate_rotations(&t, PlateIndex(0), &sampling);
        let angle = |q: Option<Quat>| q.unwrap().angle_between(Quat::IDENTITY).to_degrees();
        assert!((angle(r.current) - 20.0).abs() < 1e-3);
        assert!((angle(r.sources[0]) - 10.0).abs() < 1e-3);
        assert!((angle(r.sources[1]) - 10.0).abs() < 1e-3);

        // Between 66 and 100 Ma: current at 83 Ma, sources at 65 and 100 Ma.
        let deep = match s.select_frames(83.0) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let r = plate_rotations(
            &t,
            PlateIndex(0),
            &DeepSampling::from_bracket(&s, 83.0, &deep),
        );
        assert!((angle(r.current) - 40.0).abs() < 1e-3);
        assert!((angle(r.sources[0]) - 10.0).abs() < 1e-3);
        assert!((angle(r.sources[1]) - 60.0).abs() < 1e-3);
    }

    #[test]
    fn missing_circuits_are_reported_per_rotation() {
        let t = table(
            &[0.0, 50.0, 100.0],
            &[(101, vec![Some(Quat::IDENTITY), Some(Quat::IDENTITY), None])],
        );
        let s = timeline(&[(0.0, None), (0.03, Some(0.0)), (100.0, Some(100.0))]);
        let sampling = DeepSampling {
            age_myr: 40.0,
            endpoints: [
                DeepEndpoint {
                    frame: 1,
                    source_rotation_age_myr: 0.0,
                    sea_level_anomaly_m: 0.0,
                },
                DeepEndpoint {
                    frame: 2,
                    source_rotation_age_myr: 100.0,
                    sea_level_anomaly_m: 0.0,
                },
            ],
            older_weight: 0.4,
        };
        let _ = s;
        let r = plate_rotations(&t, PlateIndex(0), &sampling);
        assert!(r.current.is_some());
        assert!(r.sources[0].is_some());
        assert!(r.sources[1].is_none());
    }

    #[test]
    fn endpoint_rules() {
        assert!(endpoint_available(true, true));
        assert!(!endpoint_available(false, true));
        assert!(!endpoint_available(true, false));
        assert_eq!(endpoint_weights([true, true], 0.25), Some([0.75, 0.25]));
        assert_eq!(endpoint_weights([true, false], 0.25), Some([1.0, 0.0]));
        assert_eq!(endpoint_weights([false, true], 0.25), Some([0.0, 1.0]));
        assert_eq!(endpoint_weights([false, false], 0.25), None);
        assert!(piece_visible(true, true));
        assert!(!piece_visible(false, true));
        assert!(!piece_visible(true, false));
    }

    #[test]
    fn blend_applies_sea_level_per_endpoint_and_skips_unused_samples() {
        // A -120 m anomaly lifts a -100 m shelf to +20 m.
        assert_eq!(
            blend_endpoints_m([1.0, 0.0], [-100.0, f64::NAN], [-120.0, 0.0]),
            20.0
        );
        assert_eq!(
            blend_endpoints_m([0.5, 0.5], [100.0, 300.0], [0.0, 0.0]),
            200.0
        );
        assert_eq!(visible_land_m(20.0), Some(20.0));
        assert_eq!(visible_land_m(0.0), None);
        assert_eq!(visible_land_m(f64::NAN), None);
        assert_eq!(displacement_km(-50.0), 0.0);
        assert_eq!(displacement_km(1000.0), 20.0);
        assert_eq!(ocean_shell_elevation_m(-3000.0), -3000.0);
        assert_eq!(ocean_shell_elevation_m(0.0), 0.0);
        assert_eq!(ocean_shell_elevation_m(450.0), OCEAN_GAP_FALLBACK_M);
        assert_eq!(ocean_shell_elevation_m(f64::NAN), OCEAN_GAP_FALLBACK_M);
    }

    #[test]
    fn ties_rank_by_source_id() {
        let t = table(
            &[0.0],
            &[
                (500, vec![Some(Quat::IDENTITY)]),
                (100, vec![Some(Quat::IDENTITY)]),
                (300, vec![Some(Quat::IDENTITY)]),
            ],
        );
        let ranks = source_id_ranks(&t);
        assert_eq!(ranks, vec![2, 0, 1]);
        // A true tie-break: no radial bias at all, so a metre of land always wins.
        assert_eq!(ranking_surface_km(800.0), displacement_km(800.0));
        assert!(ranking_surface_km(801.0) > ranking_surface_km(800.0));
    }

    #[test]
    fn relative_rotation_is_exactly_identity_at_the_source_age() {
        let q = Quat::from_axis_angle(Vec3::new(0.3, 0.8, -0.2).normalize(), 2.1);
        assert_eq!(relative_rotation(q, q), Quat::IDENTITY);
        let r = relative_rotation(about_y(30.0), about_y(10.0));
        assert!((r * Vec3::Z - about_y(20.0) * Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn shell_shows_uncovered_frame_land_only() {
        let s = DeepSampling {
            age_myr: 10.0,
            endpoints: [
                DeepEndpoint {
                    frame: 0,
                    source_rotation_age_myr: 0.0,
                    sea_level_anomaly_m: -100.0,
                },
                DeepEndpoint {
                    frame: 1,
                    source_rotation_age_myr: 20.0,
                    sea_level_anomaly_m: 0.0,
                },
            ],
            older_weight: 0.25,
        };
        let f = |elevation_m, covered| FrameSample {
            elevation_m,
            covered,
        };
        // Uncovered land in both: crossfaded like elevation, with the bridge sea-level rule.
        assert_eq!(
            shell_surface_m(&s, [f(300.0, false), f(800.0, false)]),
            0.75 * 400.0 + 0.25 * 800.0
        );
        // Covered land is the ocean fallback.
        assert_eq!(
            shell_surface_m(&s, [f(300.0, true), f(800.0, true)]),
            OCEAN_GAP_FALLBACK_M
        );
        // Covered water keeps its depth (sea level applied).
        assert_eq!(
            shell_surface_m(&s, [f(-3000.0, true), f(-3000.0, true)]),
            0.75 * -2900.0 + 0.25 * -3000.0
        );
        // Gap land in one frame only fades in between frames.
        assert!(shell_surface_m(&s, [f(-4000.0, false), f(500.0, false)]) < 0.0);
    }

    #[test]
    fn a_lower_id_plate_with_lower_land_loses() {
        // At 100 Ma plates 101 and 202 overlap over +Z. 101 (lower ID) samples 200 m, 202 samples
        // 500 m: the higher land wins whatever the IDs, even by a few hundred metres or one.
        // 100 plates rank between them: a rank-scaled bias would lift 101 by kilometres.
        let (crust, s) = rift_with_ids_between(100);
        // Placed at 100 Ma, sampling the present-day frame: 101 reads its present position.
        let sampling = DeepSampling::single_frame(&s, 1, 100.0, 0.0);
        let rotations: Vec<PlateRotations> = (0..crust.rotations().plate_ids.len() as u32)
            .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let dir = RenderDir(DVec3::new(0.0, -0.2, 1.0).normalize());
        for (low, high) in [(300.0, 500.0), (499.0, 500.0)] {
            // Plate 202 is fixed, so its samples land at the era direction itself; 101's come
            // from its rotated source position.
            let r = rendered_surface(&crust, dir, &sampling, &rotations, |_, d| FrameSample {
                elevation_m: if (d.0 - dir.0).length() < 1e-9 {
                    high
                } else {
                    low
                },
                covered: true,
            });
            assert_eq!(
                r,
                RenderedSurface::Land {
                    plate_id: PlateId(202),
                    elevation_m: high
                },
                "{low} vs {high}"
            );
        }
    }

    /// Two plates on a cube-8 footprint grid: plate 0 (id 202) fixed over `z > 0.3`, plate 1
    /// (id 101) over `x < -0.3` at present, turning +90 deg about +Y by 100 Ma so its area moves
    /// from -X onto +Z, over plate 0, and leaves a gap behind. Plate 1's cells with `y > 0`
    /// expire at 50 Ma.
    fn rift() -> (CrustModel, Timeline) {
        rift_with_ids_between(0)
    }

    /// [`rift`] with `extra` footprint-less plates whose IDs rank between 101 and 202, so the two
    /// overlapping plates are far apart in source-ID rank (as in the real bake's 149 plates).
    fn rift_with_ids_between(extra: u32) -> (CrustModel, Timeline) {
        let mut plates = vec![
            (202, vec![Some(Quat::IDENTITY), Some(Quat::IDENTITY)]),
            (101, vec![Some(Quat::IDENTITY), Some(about_y(90.0))]),
        ];
        plates.extend((0..extra).map(|i| (102 + i, vec![Some(Quat::IDENTITY); 2])));
        let t = table(&[0.0, 100.0], &plates);
        let f = footprints(8, |d| {
            let mut e = Vec::new();
            if d.z > 0.3 {
                e.push((0, 0.0, 540.0));
            }
            if d.x < -0.3 {
                e.push((1, 0.0, if d.y > 0.0 { 50.0 } else { 540.0 }));
            }
            e
        });
        let s = timeline(&[(0.0, None), (0.03, Some(0.0)), (100.0, Some(100.0))]);
        (CrustModel::new(t, f).unwrap(), s)
    }

    fn surface_at(crust: &CrustModel, s: &Timeline, age: f64, dir: DVec3) -> RenderedSurface {
        let deep = match s.select_frames(age) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let sampling = DeepSampling::from_bracket(s, age, &deep);
        let rotations: Vec<PlateRotations> = (0..2)
            .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        // Land everywhere in every source frame (500 m), all of it covered by some plate.
        rendered_surface(
            crust,
            RenderDir(dir.normalize()),
            &sampling,
            &rotations,
            |_, _| FrameSample {
                elevation_m: 500.0,
                covered: true,
            },
        )
    }

    #[test]
    fn rift_overlap_and_gap_resolve_like_the_renderer() {
        let (crust, s) = rift();
        let land = |id| RenderedSurface::Land {
            plate_id: PlateId(id),
            elevation_m: 500.0,
        };
        // Present: each plate on its own side, nothing in between.
        assert_eq!(surface_at(&crust, &s, 0.03, DVec3::Z), land(202));
        assert_eq!(
            surface_at(&crust, &s, 0.03, DVec3::new(-1.0, -0.1, 0.0)),
            land(101)
        );
        assert_eq!(
            surface_at(&crust, &s, 0.03, DVec3::X),
            RenderedSurface::Ocean
        );
        // 100 Ma: plate 101 has turned onto plate 202's side; both cover +Z with equal land, and
        // the lower source ID wins the tie.
        assert_eq!(
            surface_at(&crust, &s, 100.0, DVec3::new(0.0, -0.2, 1.0)),
            land(101)
        );
        // The gap it left behind shows the ocean shell.
        assert_eq!(
            surface_at(&crust, &s, 100.0, -DVec3::X),
            RenderedSurface::Ocean
        );
        // Its expired northern cells are hidden: north of the overlap only plate 202 remains.
        assert_eq!(
            surface_at(&crust, &s, 100.0, DVec3::new(0.0, 0.2, 1.0)),
            land(202)
        );
        // Uncovered frame land in the gap shows on the ocean shell.
        let deep = match s.select_frames(100.0) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let sampling = DeepSampling::from_bracket(&s, 100.0, &deep);
        let rotations: Vec<PlateRotations> = (0..2)
            .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let gap = rendered_surface(
            &crust,
            RenderDir(-DVec3::X),
            &sampling,
            &rotations,
            |_, _| FrameSample {
                elevation_m: 500.0,
                covered: false,
            },
        );
        assert_eq!(gap, RenderedSurface::GapLand { elevation_m: 500.0 });
        // Rigid motion: plate 101's present-day -X sample now sits at +Z.
        let sampling = DeepSampling::single_frame(&s, 2, 100.0, 0.0);
        let q = plate_rotations(crust.rotations(), PlateIndex(1), &sampling)
            .current
            .unwrap();
        assert!((q * Vec3::NEG_X - Vec3::Z).length() < 1e-5);
    }

    #[test]
    fn endpoint_availability_follows_the_footprint_at_each_source_age() {
        // Plate 101's northern cells exist only until 50 Ma, so at 75 Ma (between the 0 Ma and
        // 100 Ma endpoints) they are hidden altogether, while at 40 Ma they draw and sample only
        // the younger endpoint.
        let (crust, s) = rift();
        let p = geo_to_render(latlon_to_geo(20.0, -90.0)); // render (-x, +y): plate 101, north
        let deep = match s.select_frames(40.0) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        let sampling = DeepSampling::from_bracket(&s, 40.0, &deep);
        let rot = plate_rotations(crust.rotations(), PlateIndex(1), &sampling);
        let land = plate_land_m(
            crust.footprints(),
            PlateIndex(1),
            &rot,
            p,
            &sampling,
            |e, _| {
                if e == 0 { 100.0 } else { 900.0 }
            },
        );
        assert_eq!(land, Some(100.0), "only the younger endpoint is available");
        let south = geo_to_render(latlon_to_geo(-20.0, -90.0));
        let land = plate_land_m(
            crust.footprints(),
            PlateIndex(1),
            &rot,
            south,
            &sampling,
            |e, _| {
                if e == 0 { 100.0 } else { 900.0 }
            },
        );
        let w = sampling.older_weight;
        assert!(
            (land.unwrap() - (100.0 + w * 800.0)).abs() < 1e-9,
            "{land:?}"
        );
    }
}
