//! The timeline aggregate: baked frames in age order, the bridge from the recent (ICE-6G_C) path
//! to the deep (PaleoDEM) path, and the sea-level curve applied to the modern-terrain bridge
//! sample.
//!
//! Selection rules (Task 7 of the Milestone 1 plan):
//! - Up to the last `ice6g` frame, blend the two bracketing ICE-6G_C frames (`Recent`).
//! - From the first `paleodem` frame (the bridge frame: present-day terrain, reconstruction age 0)
//!   on, blend the two bracketing PaleoDEMs (`Deep`). The bridge frame's signed elevation gets the
//!   sea-level anomaly at the selected age, as `elev − anomaly`, before blending; older PaleoDEMs
//!   keep their own sea level (zero anomaly). After the curve's oldest knot the anomaly tapers
//!   linearly to zero at the first older PaleoDEM.
//! - Between the two, hold the final recent frame and crossfade into the deep result
//!   (`Bridge`). Both bounds come from the frame list; at each bound the selection equals the
//!   adjacent path.

use std::fmt;

/// Where a frame's image came from, and so how it is sampled.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FrameSource {
    /// ICE-6G_C: signed topography, land and ice coverage, relative to that slice's sea level.
    Ice6g,
    /// A PaleoDEM in its published geographic frame, placed with rotations at
    /// `source_rotation_age_myr` (which may differ from the frame's timeline age).
    PaleoDem { source_rotation_age_myr: f64 },
}

/// One timeline entry. `image_slot` indexes the shell's image list; repeated files share a slot.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameMeta {
    pub age_myr: f64,
    pub source: FrameSource,
    pub image_slot: u32,
}

/// Two frame indices and the weight of the older one. Exact and clamped ages use one index for
/// both, with weight 0.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Bracket {
    pub younger: usize,
    pub older: usize,
    pub older_weight: f64,
}

/// A PaleoDEM bracket with the sea-level anomaly (m) for each endpoint, `[younger, older]`.
/// Only the modern-terrain bridge frame ever has a nonzero anomaly.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DeepBracket {
    pub frames: Bracket,
    pub sea_level_anomaly_m: [f64; 2],
}

impl DeepBracket {
    /// Blends two endpoint elevation samples: each endpoint's anomaly is applied first
    /// (`elev − anomaly`), then the two are mixed by the older weight.
    pub fn blend_elevation_m(&self, younger_elev_m: f64, older_elev_m: f64) -> f64 {
        let [a_young, a_old] = self.sea_level_anomaly_m;
        let young = effective_elevation_m(younger_elev_m, a_young);
        let old = effective_elevation_m(older_elev_m, a_old);
        young + (old - young) * self.frames.older_weight
    }
}

/// Which frames to show at an age.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FrameSelection {
    /// An ICE-6G_C bracket.
    Recent(Bracket),
    /// A PaleoDEM bracket.
    Deep(DeepBracket),
    /// Between the last ICE-6G_C frame and the bridge frame: the held final recent frame
    /// crossfaded into the deep result by `deep_weight`.
    Bridge {
        recent: usize,
        deep: DeepBracket,
        deep_weight: f64,
    },
}

impl FrameSelection {
    /// Every frame index this selection samples, without repeats.
    pub fn frame_indices(&self) -> Vec<usize> {
        let mut out = match *self {
            FrameSelection::Recent(b) => vec![b.younger, b.older],
            FrameSelection::Deep(d) => vec![d.frames.younger, d.frames.older],
            FrameSelection::Bridge { recent, deep, .. } => {
                vec![recent, deep.frames.younger, deep.frames.older]
            }
        };
        out.dedup();
        out
    }
}

/// Sea-level rule shared by CPU and GPU: `effective = elev − anomaly`, so a negative anomaly
/// (lower sea) lifts the terrain relative to the sea surface.
pub fn effective_elevation_m(elev_m: f64, anomaly_m: f64) -> f64 {
    elev_m - anomaly_m
}

/// The two nearest indices of ascending `ages_myr` around `age_myr`, and the weight of the older.
/// Ages at or beyond either end clamp to that end; NaN clamps to the youngest.
///
/// Panics if `ages_myr` is empty.
pub fn bracket(ages_myr: &[f64], age_myr: f64) -> Bracket {
    assert!(!ages_myr.is_empty(), "bracket needs at least one age");
    let last = ages_myr.len() - 1;
    let single = |i| Bracket {
        younger: i,
        older: i,
        older_weight: 0.0,
    };
    if age_myr.is_nan() || age_myr <= ages_myr[0] {
        return single(0);
    }
    if age_myr >= ages_myr[last] {
        return single(last);
    }
    let older = ages_myr.partition_point(|&a| a < age_myr); // first age >= age, in 1..=last
    if ages_myr[older] == age_myr {
        return single(older);
    }
    let younger = older - 1;
    let (a, b) = (ages_myr[younger], ages_myr[older]);
    Bracket {
        younger,
        older,
        older_weight: (age_myr - a) / (b - a),
    }
}

/// Why a [`Timeline`] could not be built.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineError {
    NoFrames,
    /// A frame age is NaN, infinite or negative.
    BadAge {
        index: usize,
    },
    /// A frame age is not strictly older than the one before it.
    AgesNotAscending {
        index: usize,
    },
    /// An `ice6g` frame follows a `paleodem` frame.
    SourceOrder {
        index: usize,
    },
    /// The list lacks an `ice6g` or a `paleodem` frame, so there is no bridge.
    NoBridge,
    /// A PaleoDEM source rotation age is NaN, infinite or negative.
    BadSourceRotationAge {
        index: usize,
    },
    /// The bridge frame is not present-day terrain (source rotation age 0).
    BridgeNotModern {
        source_rotation_age_myr: f64,
    },
    EmptySeaLevelCurve,
    /// A sea-level knot is not finite or not strictly older than the one before it.
    BadSeaLevelKnot {
        index: usize,
    },
    /// A frame or source rotation age lies outside the rotation table's time range.
    RotationCoverage {
        age_myr: f64,
    },
}

impl fmt::Display for TimelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimelineError::NoFrames => write!(f, "the timeline has no frames"),
            TimelineError::BadAge { index } => {
                write!(f, "frame {index}: age must be finite and non-negative")
            }
            TimelineError::AgesNotAscending { index } => {
                write!(f, "frame {index}: ages must be strictly ascending")
            }
            TimelineError::SourceOrder { index } => write!(
                f,
                "frame {index}: every ice6g frame must come before every paleodem frame"
            ),
            TimelineError::NoBridge => write!(
                f,
                "the timeline needs at least one ice6g and one paleodem frame for the bridge"
            ),
            TimelineError::BadSourceRotationAge { index } => write!(
                f,
                "frame {index}: source rotation age must be finite and non-negative"
            ),
            TimelineError::BridgeNotModern {
                source_rotation_age_myr,
            } => write!(
                f,
                "the bridge frame must be present-day terrain (source rotation age 0), \
                 got {source_rotation_age_myr} Myr"
            ),
            TimelineError::EmptySeaLevelCurve => write!(f, "the sea-level curve is empty"),
            TimelineError::BadSeaLevelKnot { index } => write!(
                f,
                "sea-level knot {index}: values must be finite and ages strictly ascending"
            ),
            TimelineError::RotationCoverage { age_myr } => write!(
                f,
                "age {age_myr} Myr is outside the rotation table's time range"
            ),
        }
    }
}

impl std::error::Error for TimelineError {}

/// Frames in ascending age with a bridge and a sea-level curve. Built only by [`Timeline::new`].
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    frames: Vec<FrameMeta>,
    ages_myr: Vec<f64>,
    /// `(age_myr, anomaly_m)` knots, ascending.
    sea_level: Vec<(f64, f64)>,
    /// Index of the last `ice6g` frame; the bridge frame is the next one.
    last_recent: usize,
}

impl Timeline {
    /// Validates and builds a timeline. `sea_level_curve` holds `(age_myr, anomaly_m)` knots.
    pub fn new(
        frames: Vec<FrameMeta>,
        sea_level_curve: Vec<(f64, f64)>,
    ) -> Result<Timeline, TimelineError> {
        if frames.is_empty() {
            return Err(TimelineError::NoFrames);
        }
        for (index, frame) in frames.iter().enumerate() {
            if !(frame.age_myr.is_finite() && frame.age_myr >= 0.0) {
                return Err(TimelineError::BadAge { index });
            }
            if index > 0 && frame.age_myr <= frames[index - 1].age_myr {
                return Err(TimelineError::AgesNotAscending { index });
            }
            if let FrameSource::PaleoDem {
                source_rotation_age_myr: s,
            } = frame.source
                && !(s.is_finite() && s >= 0.0)
            {
                return Err(TimelineError::BadSourceRotationAge { index });
            }
        }
        let first_deep = frames
            .iter()
            .position(|f| matches!(f.source, FrameSource::PaleoDem { .. }));
        if let Some(first_deep) = first_deep
            && let Some(offset) = frames[first_deep..]
                .iter()
                .position(|f| f.source == FrameSource::Ice6g)
        {
            return Err(TimelineError::SourceOrder {
                index: first_deep + offset,
            });
        }
        let first_deep = match first_deep {
            Some(i) if i > 0 => i,
            _ => return Err(TimelineError::NoBridge),
        };
        if let FrameSource::PaleoDem {
            source_rotation_age_myr,
        } = frames[first_deep].source
            && source_rotation_age_myr != 0.0
        {
            return Err(TimelineError::BridgeNotModern {
                source_rotation_age_myr,
            });
        }
        if sea_level_curve.is_empty() {
            return Err(TimelineError::EmptySeaLevelCurve);
        }
        for (index, &(age, anomaly)) in sea_level_curve.iter().enumerate() {
            let ascending = index == 0 || age > sea_level_curve[index - 1].0;
            if !(age.is_finite() && anomaly.is_finite() && ascending) {
                return Err(TimelineError::BadSeaLevelKnot { index });
            }
        }
        let ages_myr = frames.iter().map(|f| f.age_myr).collect();
        Ok(Timeline {
            frames,
            ages_myr,
            sea_level: sea_level_curve,
            last_recent: first_deep - 1,
        })
    }

    pub fn frames(&self) -> &[FrameMeta] {
        &self.frames
    }

    pub fn ages_myr(&self) -> &[f64] {
        &self.ages_myr
    }

    /// Index of the final recent (`ice6g`) frame.
    pub fn last_recent_index(&self) -> usize {
        self.last_recent
    }

    /// Index of the bridge frame: the first `paleodem` frame, present-day terrain.
    pub fn bridge_frame_index(&self) -> usize {
        self.last_recent + 1
    }

    /// The bridge's `(start, end)` ages: the last `ice6g` and the first `paleodem` frame.
    pub fn bridge_myr(&self) -> (f64, f64) {
        (
            self.ages_myr[self.last_recent],
            self.ages_myr[self.bridge_frame_index()],
        )
    }

    /// The sea-level anomaly (m) applied to the modern-terrain bridge sample at `age_myr`:
    /// linear between knots, the youngest knot held before the curve, and after the oldest
    /// knot a linear taper to zero at the first older PaleoDEM (held if there is none).
    pub fn sea_level_anomaly_m(&self, age_myr: f64) -> f64 {
        let knots = &self.sea_level;
        let (first_age, first_value) = knots[0];
        let (last_age, last_value) = knots[knots.len() - 1];
        if age_myr.is_nan() || age_myr <= first_age {
            return first_value;
        }
        if age_myr <= last_age {
            let hi = knots.partition_point(|&(a, _)| a < age_myr); // in 1..len
            let (a1, v1) = knots[hi];
            if a1 == age_myr {
                return v1;
            }
            let (a0, v0) = knots[hi - 1];
            return v0 + (v1 - v0) * (age_myr - a0) / (a1 - a0);
        }
        match self.ages_myr.get(self.bridge_frame_index() + 1) {
            Some(&taper_end) if age_myr >= taper_end => 0.0,
            Some(&taper_end) => last_value * (taper_end - age_myr) / (taper_end - last_age),
            None => last_value,
        }
    }

    /// The frames to show at `age_myr`. Pure; returns indices into [`Timeline::frames`].
    pub fn select_frames(&self, age_myr: f64) -> FrameSelection {
        let (start, end) = self.bridge_myr();
        if age_myr.is_nan() || age_myr <= start {
            return FrameSelection::Recent(bracket(&self.ages_myr[..=self.last_recent], age_myr));
        }
        let deep = self.deep_bracket(age_myr);
        if age_myr >= end {
            return FrameSelection::Deep(deep);
        }
        FrameSelection::Bridge {
            recent: self.last_recent,
            deep,
            deep_weight: (age_myr - start) / (end - start),
        }
    }

    /// The PaleoDEM bracket at `age_myr` (clamped to the bridge frame below it), with the
    /// bridge frame's sea-level anomaly.
    fn deep_bracket(&self, age_myr: f64) -> DeepBracket {
        let first = self.bridge_frame_index();
        let local = bracket(&self.ages_myr[first..], age_myr);
        let frames = Bracket {
            younger: local.younger + first,
            older: local.older + first,
            older_weight: local.older_weight,
        };
        let anomaly = |i: usize| {
            if i == first {
                self.sea_level_anomaly_m(age_myr)
            } else {
                0.0
            }
        };
        DeepBracket {
            frames,
            sea_level_anomaly_m: [anomaly(frames.younger), anomaly(frames.older)],
        }
    }

    /// Checks that every frame age and PaleoDEM source rotation age lies within the rotation
    /// table's sample times (ascending), so rotations exist across the whole timeline.
    pub fn check_rotation_coverage(&self, times_myr: &[f64]) -> Result<(), TimelineError> {
        let (Some(&first), Some(&last)) = (times_myr.first(), times_myr.last()) else {
            return Err(TimelineError::RotationCoverage {
                age_myr: self.ages_myr[0],
            });
        };
        let source_ages = self.frames.iter().filter_map(|f| match f.source {
            FrameSource::PaleoDem {
                source_rotation_age_myr,
            } => Some(source_rotation_age_myr),
            FrameSource::Ice6g => None,
        });
        match self
            .ages_myr
            .iter()
            .copied()
            .chain(source_ages)
            .find(|&a| !(first..=last).contains(&a))
        {
            Some(age_myr) => Err(TimelineError::RotationCoverage { age_myr }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ice(age_myr: f64, image_slot: u32) -> FrameMeta {
        FrameMeta {
            age_myr,
            source: FrameSource::Ice6g,
            image_slot,
        }
    }

    fn paleo(age_myr: f64, source_rotation_age_myr: f64, image_slot: u32) -> FrameMeta {
        FrameMeta {
            age_myr,
            source: FrameSource::PaleoDem {
                source_rotation_age_myr,
            },
            image_slot,
        }
    }

    fn single(i: usize) -> Bracket {
        Bracket {
            younger: i,
            older: i,
            older_weight: 0.0,
        }
    }

    /// The production shape: ICE-6G_C at 0 (reusing the youngest slice) and 0.000076 …
    /// 0.026076 Myr (indices 0..=27), the bridge PaleoDEM at 0.030 (28), then 4.47, 10.5, 540.
    fn production() -> Timeline {
        let mut frames = vec![ice(0.0, 0)];
        for k in 0..=26u32 {
            // Exact decimal ages, as the manifest's JSON gives them.
            frames.push(ice(f64::from(76 + 1000 * k) / 1e6, k));
        }
        frames.push(paleo(0.030, 0.0, 27));
        frames.push(paleo(4.47, 5.0, 28));
        frames.push(paleo(10.5, 10.0, 29));
        frames.push(paleo(540.0, 540.0, 30));
        let curve = vec![
            (0.000076, 8.96),
            (0.021076, -120.0),
            (0.030076, -80.0),
            (0.798076, -92.37),
        ];
        Timeline::new(frames, curve).unwrap()
    }

    /// The tiny bake fixture's frame list.
    fn tiny() -> Timeline {
        let frames = vec![
            ice(0.0, 0),
            ice(0.000076, 0),
            paleo(0.030, 0.0, 1),
            paleo(540.0, 540.0, 2),
        ];
        let curve = vec![(0.000076, 0.5), (0.001076, -1.5), (0.031076, -82.5)];
        Timeline::new(frames, curve).unwrap()
    }

    fn deep(sel: FrameSelection) -> DeepBracket {
        match sel {
            FrameSelection::Deep(d) => d,
            other => panic!("expected Deep, got {other:?}"),
        }
    }

    #[test]
    fn bracket_clamps_at_ends() {
        let ages = [0.0, 1.0, 5.0];
        for at in [-3.0, 0.0, f64::NEG_INFINITY, f64::NAN] {
            assert_eq!(bracket(&ages, at), single(0), "at {at}");
        }
        for at in [5.0, 7.0, f64::INFINITY] {
            assert_eq!(bracket(&ages, at), single(2), "at {at}");
        }
        assert_eq!(bracket(&[2.0], 1.0), single(0));
        assert_eq!(bracket(&[2.0], 3.0), single(0));
    }

    #[test]
    fn bracket_exact_age_uses_one_index() {
        assert_eq!(bracket(&[0.0, 1.0, 5.0], 1.0), single(1));
    }

    #[test]
    fn bracket_interpolates_interior() {
        let b = bracket(&[0.0, 1.0, 5.0], 2.0);
        assert_eq!((b.younger, b.older), (1, 2));
        assert!((b.older_weight - 0.25).abs() < 1e-12);
    }

    #[test]
    fn recent_selection_at_present_and_bridge_start() {
        let t = production();
        assert_eq!(t.bridge_myr(), (0.026076, 0.030));
        assert_eq!(t.select_frames(0.0), FrameSelection::Recent(single(0)));
        assert_eq!(
            t.select_frames(0.026076),
            FrameSelection::Recent(single(27))
        );
        match t.select_frames(0.0215) {
            FrameSelection::Recent(b) => {
                assert_eq!((b.younger, b.older), (22, 23));
                assert!(b.older_weight > 0.0 && b.older_weight < 1.0);
            }
            other => panic!("expected Recent, got {other:?}"),
        }

        let t = tiny();
        assert_eq!(t.bridge_myr(), (0.000076, 0.030));
        assert_eq!(t.select_frames(0.0), FrameSelection::Recent(single(0)));
        assert_eq!(t.select_frames(0.000076), FrameSelection::Recent(single(1)));
    }

    #[test]
    fn deep_selection_at_bridge_end_equals_deep_path() {
        let t = production();
        let d = deep(t.select_frames(0.030));
        assert_eq!(d.frames, single(28));
        let anomaly = t.sea_level_anomaly_m(0.030);
        assert_eq!(d.sea_level_anomaly_m, [anomaly, anomaly]);

        let t = tiny();
        let d = deep(t.select_frames(0.030));
        assert_eq!(d.frames, single(2));
        assert_eq!(deep(t.select_frames(540.0)).frames, single(3));
        assert_eq!(deep(t.select_frames(600.0)).frames, single(3));
    }

    #[test]
    fn bridge_weights_are_intermediate() {
        let t = production();
        let mut last = 0.0;
        for age in [0.0262, 0.027, 0.028, 0.029, 0.0299] {
            match t.select_frames(age) {
                FrameSelection::Bridge {
                    recent,
                    deep,
                    deep_weight,
                } => {
                    assert_eq!(recent, 27);
                    assert_eq!(deep.frames, single(28));
                    assert!(deep_weight > last && deep_weight < 1.0, "{deep_weight}");
                    last = deep_weight;
                }
                other => panic!("expected Bridge at {age}, got {other:?}"),
            }
        }
        match t.select_frames(0.028) {
            FrameSelection::Bridge { deep_weight, .. } => {
                assert!((deep_weight - (0.028 - 0.026076) / (0.030 - 0.026076)).abs() < 1e-12);
            }
            other => panic!("expected Bridge, got {other:?}"),
        }
    }

    #[test]
    fn sea_level_offset_applies_to_modern_terrain_only() {
        let t = production();
        // Between the bridge frame and 4.47 Ma: only the bridge endpoint carries an anomaly.
        let d = deep(t.select_frames(0.5));
        assert_eq!((d.frames.younger, d.frames.older), (28, 29));
        assert!(
            d.sea_level_anomaly_m[0] < -80.0,
            "{:?}",
            d.sea_level_anomaly_m
        );
        assert_eq!(d.sea_level_anomaly_m[1], 0.0);
        // Between two older PaleoDEMs: none.
        let d = deep(t.select_frames(7.0));
        assert_eq!((d.frames.younger, d.frames.older), (29, 30));
        assert_eq!(d.sea_level_anomaly_m, [0.0, 0.0]);
        // ICE frames carry their own sea level: the recent path has no anomaly at all, and the
        // bridge crossfade's held recent frame is not part of the deep bracket.
        assert!(matches!(
            t.select_frames(0.021076),
            FrameSelection::Recent(_)
        ));
    }

    #[test]
    fn sea_level_offset_exposes_shelf() {
        assert_eq!(effective_elevation_m(-100.0, -120.0), 20.0);
        let frames = vec![ice(0.0, 0), paleo(0.030, 0.0, 1), paleo(5.0, 5.0, 2)];
        let t = Timeline::new(frames, vec![(0.0, 0.0), (0.030, -120.0)]).unwrap();
        let d = deep(t.select_frames(0.030));
        assert_eq!(d.sea_level_anomaly_m, [-120.0, -120.0]);
        assert_eq!(d.blend_elevation_m(-100.0, -100.0), 20.0);
    }

    #[test]
    fn sea_level_tapers_continuously_to_zero() {
        let t = production();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(t.sea_level_anomaly_m(0.798076), -92.37));
        assert!(close(
            t.sea_level_anomaly_m((0.798076 + 4.47) / 2.0),
            -92.37 / 2.0
        ));
        assert_eq!(t.sea_level_anomaly_m(4.47), 0.0);
        assert_eq!(t.sea_level_anomaly_m(100.0), 0.0);
        // Continuous across the oldest knot and across the first older PaleoDEM, for fixed
        // endpoint samples blended through the deep bracket.
        let blended = |age: f64| {
            let d = deep(t.select_frames(age));
            let sample = |i: usize| if i == 28 { -50.0 } else { 300.0 };
            d.blend_elevation_m(sample(d.frames.younger), sample(d.frames.older))
        };
        for edge in [0.798076, 4.47] {
            let (below, at, above) = (blended(edge - 1e-9), blended(edge), blended(edge + 1e-9));
            assert!(close(below, at) && close(at, above), "{below} {at} {above}");
        }
        // Monotonic taper between the knot and the first older PaleoDEM.
        let mut prev = t.sea_level_anomaly_m(0.798076);
        for i in 1..=20 {
            let a = t.sea_level_anomaly_m(0.798076 + (4.47 - 0.798076) * f64::from(i) / 20.0);
            assert!(a >= prev - 1e-12, "{a} < {prev}");
            prev = a;
        }
        assert_eq!(prev, 0.0);
    }

    #[test]
    fn frame_indices_list_each_frame_once() {
        let t = production();
        assert_eq!(t.select_frames(0.0).frame_indices(), vec![0]);
        assert_eq!(t.select_frames(0.028).frame_indices(), vec![27, 28]);
        assert_eq!(t.select_frames(7.0).frame_indices(), vec![29, 30]);
    }

    #[test]
    fn timeline_rejects_invalid_lists() {
        let curve = || vec![(0.0, 0.0)];
        let err = |frames: Vec<FrameMeta>| Timeline::new(frames, curve()).unwrap_err();
        assert_eq!(err(vec![]), TimelineError::NoFrames);
        assert_eq!(
            err(vec![ice(0.0, 0), ice(0.0, 0), paleo(1.0, 0.0, 1)]),
            TimelineError::AgesNotAscending { index: 1 }
        );
        assert_eq!(
            err(vec![ice(f64::NAN, 0), paleo(1.0, 0.0, 1)]),
            TimelineError::BadAge { index: 0 }
        );
        assert_eq!(
            err(vec![ice(0.0, 0), paleo(1.0, 0.0, 1), ice(2.0, 2)]),
            TimelineError::SourceOrder { index: 2 }
        );
        assert_eq!(err(vec![ice(0.0, 0), ice(1.0, 1)]), TimelineError::NoBridge);
        assert_eq!(
            err(vec![paleo(0.0, 0.0, 0), paleo(1.0, 1.0, 1)]),
            TimelineError::NoBridge
        );
        assert_eq!(
            err(vec![
                ice(0.0, 0),
                paleo(1.0, 0.0, 1),
                paleo(2.0, f64::NAN, 2)
            ]),
            TimelineError::BadSourceRotationAge { index: 2 }
        );
        assert_eq!(
            err(vec![ice(0.0, 0), paleo(1.0, 3.0, 1)]),
            TimelineError::BridgeNotModern {
                source_rotation_age_myr: 3.0
            }
        );
        let frames = || vec![ice(0.0, 0), paleo(1.0, 0.0, 1)];
        assert_eq!(
            Timeline::new(frames(), vec![]).unwrap_err(),
            TimelineError::EmptySeaLevelCurve
        );
        assert_eq!(
            Timeline::new(frames(), vec![(0.1, 0.0), (0.1, 1.0)]).unwrap_err(),
            TimelineError::BadSeaLevelKnot { index: 1 }
        );
        assert_eq!(
            Timeline::new(frames(), vec![(0.1, f64::NAN)]).unwrap_err(),
            TimelineError::BadSeaLevelKnot { index: 0 }
        );
    }

    #[test]
    fn rotation_coverage_includes_source_ages() {
        let t = production();
        assert_eq!(t.check_rotation_coverage(&[0.0, 270.0, 540.0]), Ok(()));
        assert_eq!(
            t.check_rotation_coverage(&[0.0, 500.0]),
            Err(TimelineError::RotationCoverage { age_myr: 540.0 })
        );
        let frames = vec![ice(0.0, 0), paleo(0.030, 0.0, 1), paleo(4.47, 5.0, 2)];
        let t = Timeline::new(frames, vec![(0.0, 0.0)]).unwrap();
        assert_eq!(
            t.check_rotation_coverage(&[0.0, 4.5]),
            Err(TimelineError::RotationCoverage { age_myr: 5.0 })
        );
        assert!(t.check_rotation_coverage(&[]).is_err());
    }
}
