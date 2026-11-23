use std::sync::Arc;
use std::time::Duration;

use bevy::asset::AssetPlugin;
use bevy::math::DVec3;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly_core::cube::{CubeFace, texel_center_dir};
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::geodynamics::{
    Candidate, FootprintEntry, PlateFootprints, PlateId, PlateIndex, RotationTable,
};
use timefly_core::surface::{FrameMeta, FrameSource, Timeline};
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

use super::*;
use crate::data::BakeRevision;
use crate::diagnostics::aspects::DiagnosticsAppExt;
use crate::fly_camera::CAMERA_CLEARANCE_M;
use crate::region::{RegionGenerated, RegionPlugin, RegionSnapshotChanged};
use crate::time::TimePlugin;
use timefly_core::terrain::blended_height_m;

// ---------------------------------------------------------------------------------------------
// Fixtures

fn about_y(deg: f64) -> bevy::math::DQuat {
    bevy::math::DQuat::from_rotation_y(deg.to_radians())
}

fn rot_y(deg: f32) -> [f32; 4] {
    let q = Quat::from_rotation_y(deg.to_radians());
    let q = if q.w < 0.0 { -q } else { q };
    q.to_array()
}

/// M1's two-plate rift: plate 202 fixed over `z > 0.3`; plate 101 over `x < −0.3` at present,
/// turning +4° about +Y by 4.47 Ma and +90° by 100 Ma (held to 300 Ma). Plate 101's footprint
/// ends at 200 Ma, so its crust is newly invalid at 250 Ma.
fn crust() -> CrustModel {
    let times = [0.0, 4.47, 100.0, 300.0];
    let plate_101 = [rot_y(0.0), rot_y(4.0), rot_y(90.0), rot_y(90.0)];
    let mut quats = Vec::new();
    for q in plate_101 {
        quats.push(rot_y(0.0)); // plate 202
        quats.push(q);
    }
    let rotations = RotationTable {
        plate_ids: vec![PlateId(202), PlateId(101)],
        times_myr: times.to_vec(),
        quats,
        valid: vec![true; times.len() * 2],
    };
    let n = 16;
    let mut offsets = vec![0];
    let mut entries = Vec::new();
    for face in CubeFace::ALL {
        for row in 0..n {
            for col in 0..n {
                let d = texel_center_dir(face, col, row, n).0;
                if d.z > 0.3 {
                    entries.push(FootprintEntry {
                        plate_index: PlateIndex(0),
                        younger_myr: 0.0,
                        older_myr: 540.0,
                    });
                }
                if d.x < -0.3 {
                    entries.push(FootprintEntry {
                        plate_index: PlateIndex(1),
                        younger_myr: 0.0,
                        older_myr: 200.0,
                    });
                }
                offsets.push(entries.len() as u32);
            }
        }
    }
    let footprints = PlateFootprints {
        cube_size: n,
        offsets,
        entries,
    };
    CrustModel::new(rotations, footprints).unwrap()
}

/// On plate 101 at present.
const ON_PLATE: DVec3 = DVec3::new(-1.0, 0.0, 0.0);

fn close(a: RenderDir, b: DVec3) -> bool {
    a.0.distance(b) < 1e-6
}

// ---------------------------------------------------------------------------------------------
// Pure functions

#[test]
fn double_click_window() {
    let first = Some((10.0, Vec2::new(100.0, 100.0)));
    assert!(is_double_click(first, 10.34, Vec2::new(105.0, 100.0)));
    assert!(
        is_double_click(first, 10.34, Vec2::new(103.0, 104.0)),
        "5 px diagonally"
    );
    assert!(
        !is_double_click(first, 10.36, Vec2::new(105.0, 100.0)),
        "too slow"
    );
    assert!(
        !is_double_click(first, 10.34, Vec2::new(107.0, 100.0)),
        "too far"
    );
    assert!(!is_double_click(None, 10.0, Vec2::new(100.0, 100.0)));
    // The edges count.
    assert!(is_double_click(
        first,
        10.0 + DOUBLE_CLICK_S,
        Vec2::new(106.0, 100.0)
    ));
}

#[test]
fn globe_hit_returns_the_front_surface() {
    let r = EARTH_RADIUS_KM as f32;
    // From +z, straight at the centre: the near side, +z.
    let ray = Ray3d::new(Vec3::new(0.0, 0.0, 3.0 * r), Dir3::NEG_Z);
    let hit = globe_hit(ray).unwrap();
    assert!(close(hit, DVec3::Z), "{hit:?}");
    // Off-centre: the near intersection, not the far one.
    let ray = Ray3d::new(Vec3::new(0.5 * r, 0.0, 3.0 * r), Dir3::NEG_Z);
    let hit = globe_hit(ray).unwrap();
    let want = DVec3::new(0.5, 0.0, 0.75f64.sqrt());
    assert!(hit.0.distance(want) < 1e-6, "{hit:?}");
    assert!((hit.0.length() - 1.0).abs() < 1e-12);
    // Misses: beside the globe, and pointing away from it.
    assert!(globe_hit(Ray3d::new(Vec3::new(1.01 * r, 0.0, 3.0 * r), Dir3::NEG_Z)).is_none());
    assert!(globe_hit(Ray3d::new(Vec3::new(0.0, 0.0, 3.0 * r), Dir3::Z)).is_none());
}

#[test]
fn snapshot_change_keeps_crust_position() {
    let crust = crust();
    // Present (geographic) → 100 Ma: plate 101 has turned +90° about +Y.
    let to_100 = carry_crust_position(
        &crust,
        RenderDir(ON_PLATE),
        YearsBp(0.0),
        YearsBp(100e6),
        |_| None,
    );
    assert!(
        close(to_100.unwrap(), about_y(90.0) * ON_PLATE),
        "{to_100:?}"
    );
    assert!(close(to_100.unwrap(), DVec3::Z));
    // Deep → deep: from where the crust is at 4.47 Ma to where it is at 100 Ma.
    let at_447 = about_y(4.0) * ON_PLATE;
    let deep = carry_crust_position(
        &crust,
        RenderDir(at_447),
        YearsBp(4.47e6),
        YearsBp(100e6),
        |_| None,
    );
    assert!(close(deep.unwrap(), about_y(90.0) * ON_PLATE), "{deep:?}");
    // And back again.
    let back = carry_crust_position(
        &crust,
        deep.unwrap(),
        YearsBp(100e6),
        YearsBp(4.47e6),
        |_| None,
    );
    assert!(close(back.unwrap(), at_447), "{back:?}");
    // Between recent snapshots the frame is geographic: the place stays put.
    let recent = carry_crust_position(
        &crust,
        RenderDir(ON_PLATE),
        YearsBp(0.0),
        YearsBp(21e3),
        |_| None,
    );
    assert!(close(recent.unwrap(), ON_PLATE), "{recent:?}");
    // A point off the plate's centre keeps its offset as the plate turns.
    let off = geo_to_render(latlon_to_geo(20.0, -85.0)).0;
    let moved = carry_crust_position(&crust, RenderDir(off), YearsBp(0.0), YearsBp(100e6), |_| {
        None
    });
    assert!(close(moved.unwrap(), about_y(90.0) * off), "{moved:?}");
}

#[test]
fn missing_crust_carries_nowhere() {
    let crust = crust();
    // Newly invalid: plate 101's footprint ends at 200 Ma.
    assert_eq!(
        carry_crust_position(
            &crust,
            RenderDir(ON_PLATE),
            YearsBp(0.0),
            YearsBp(250e6),
            |_| None
        ),
        None
    );
    // Era-fixed: no plate covers the north pole.
    assert_eq!(
        carry_crust_position(
            &crust,
            RenderDir(DVec3::Y),
            YearsBp(0.0),
            YearsBp(100e6),
            |_| None
        ),
        None
    );
}

#[test]
fn fade_progress() {
    // The opacity rises over the first half, peaks at the midpoint and falls back, symmetric.
    assert_eq!(fade_alpha(0.0), 0.0);
    assert_eq!(fade_alpha(0.5), 1.0);
    assert_eq!(fade_alpha(1.0), 0.0);
    let mut last = 0.0;
    for i in 1..=50 {
        let t = i as f32 / 100.0;
        let a = fade_alpha(t);
        assert!(a > last, "rises at {t}");
        assert!((a - fade_alpha(1.0 - t)).abs() < 1e-6, "symmetric at {t}");
        last = a;
    }
    assert_eq!(fade_alpha(-1.0), 0.0);
    assert_eq!(fade_alpha(2.0), 0.0);

    // A fade swaps at its midpoint and is done at its duration.
    let f = Fade::new(ENTER_FADE_S);
    assert!(!f.swapped() && !f.done() && f.alpha() == 0.0);
    let half = f.advance(0.4, false);
    assert!(half.swapped() && !half.done());
    assert!((half.alpha() - 1.0).abs() < 1e-6);
    let end = half.advance(0.4, false);
    assert!(end.done() && end.alpha() == 0.0);
    assert_eq!(end.advance(1.0, false).progress(), 1.0);
    // Holding stops it at the midpoint, fully covered, however long it waits.
    let mut held = Fade::new(SNAPSHOT_FADE_S);
    for _ in 0..100 {
        held = held.advance(0.1, true);
    }
    assert!(held.swapped() && !held.done());
    assert!((held.alpha() - 1.0).abs() < 1e-6);
    assert!(held.advance(0.5, false).done());
    // Past the midpoint it never holds.
    let late = Fade::new(1.0).advance(0.7, false).advance(0.1, true);
    assert!((late.progress() - 0.8).abs() < 1e-6);
}

#[test]
fn caption_waits_a_second() {
    assert!(!caption_shown(None));
    assert!(!caption_shown(Some(0.0)));
    assert!(!caption_shown(Some(0.99)));
    assert!(caption_shown(Some(1.01)));
}

#[test]
fn only_a_fade_at_its_midpoint_covers_the_view() {
    let changing = |fade: Fade| NavPhase::ChangingSnapshot {
        fade,
        to: YearsBp(1e3),
        requested: RenderDir(DVec3::Z),
        agl_m: 100.0,
        generated: false,
        placed: false,
    };
    // Held at the midpoint while the new region generates: covered.
    let held = Fade::new(SNAPSHOT_FADE_S).advance(2.0, true);
    assert!(changing(held).covers_view());
    // On the way down or up again, flying, on the globe or descending: not covered.
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.3, true)).covers_view());
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.8, false)).covers_view());
    assert!(!NavPhase::Flying.covers_view());
    assert!(!NavPhase::Globe.covers_view());
    assert!(
        !NavPhase::Descending {
            target: RenderDir(DVec3::Z),
            since_s: 0.0,
            started: true,
        }
        .covers_view()
    );
}

#[test]
fn generation_takes_the_large_budget_only_while_no_region_is_on_screen() {
    let changing = |fade: Fade| NavPhase::ChangingSnapshot {
        fade,
        to: YearsBp(1e3),
        requested: RenderDir(DVec3::Z),
        agl_m: 100.0,
        generated: false,
        placed: false,
    };
    // First entries: from the globe (a launch region or the bench) and while descending.
    assert!(NavPhase::Globe.allows_large_gen_budget());
    assert!(
        NavPhase::Descending {
            target: RenderDir(DVec3::Z),
            since_s: 0.0,
            started: true,
        }
        .allows_large_gen_budget()
    );
    // A snapshot change held under the fade overlay.
    let held = Fade::new(SNAPSHOT_FADE_S).advance(2.0, true);
    assert!(changing(held).allows_large_gen_budget());
    // Flying, or a fade with the region still showing through: the small budget.
    assert!(!NavPhase::Flying.allows_large_gen_budget());
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.3, true)).allows_large_gen_budget());
    let entering = NavPhase::Entering {
        fade: Fade::new(ENTER_FADE_S).advance(0.6, false),
        placed: true,
    };
    assert!(!entering.allows_large_gen_budget());
}

#[test]
fn the_fallback_notice_shows_for_three_seconds_after_a_fallback_region() {
    // A region that fell back starts it; one that did not clears it; no region keeps it.
    assert_eq!(fallback_since(None, Some(true), 5.0), Some(5.0));
    assert_eq!(fallback_since(Some(5.0), None, 6.0), Some(5.0));
    assert_eq!(fallback_since(Some(5.0), Some(true), 7.0), Some(7.0));
    assert_eq!(fallback_since(Some(5.0), Some(false), 7.0), None);
    assert_eq!(fallback_since(None, Some(false), 7.0), None);
    assert_eq!(fallback_since(None, None, 7.0), None);
    // Shown from the start for FALLBACK_NOTICE_S, while the region is up.
    assert!(!fallback_notice_shown(None, 5.0, true));
    assert!(fallback_notice_shown(Some(5.0), 5.0, true));
    assert!(fallback_notice_shown(
        Some(5.0),
        5.0 + FALLBACK_NOTICE_S - 0.01,
        true
    ));
    assert!(!fallback_notice_shown(
        Some(5.0),
        5.0 + FALLBACK_NOTICE_S,
        true
    ));
    assert!(!fallback_notice_shown(Some(5.0), 6.0, false));
    assert!((2.5..=3.5).contains(&FALLBACK_NOTICE_S));
    assert_eq!(FALLBACK_NOTICE_TEXT, "terrain detail unavailable here");
}

fn inputs() -> NavInputs {
    NavInputs {
        now_s: 10.0,
        dt_s: 0.1,
        region_state: RegionState::Globe,
        time: YearsBp(30_000.0),
        double_click: None,
        backspace: false,
        entered: false,
        generated: None,
        region_centre: None,
        region_snapshot: None,
        time_change: None,
    }
}

#[test]
fn a_double_click_snaps_then_requests() {
    let at = RenderDir(DVec3::Z);
    let (phase, actions) = step_nav(
        NavPhase::Globe,
        &NavInputs {
            double_click: Some(at),
            ..inputs()
        },
    );
    assert_eq!(
        actions,
        vec![
            NavAction::SnapTime(YearsBp(21_000.0)),
            NavAction::Request(RegionRequested {
                era_dir: at,
                snapshot: YearsBp(21_000.0),
            }),
        ]
    );
    assert!(matches!(phase, NavPhase::Descending { started: false, .. }));
    // Already on a snapshot: no snap, just the request.
    let (_, actions) = step_nav(
        NavPhase::Globe,
        &NavInputs {
            double_click: Some(at),
            time: YearsBp(66e6),
            ..inputs()
        },
    );
    assert_eq!(actions.len(), 1);
    // A request that never starts gives up; one that fails returns to the globe.
    let descending = NavPhase::Descending {
        target: at,
        since_s: 10.0,
        started: false,
    };
    let (still, _) = step_nav(descending, &inputs());
    assert_eq!(still, descending);
    let late = NavInputs {
        now_s: 10.0 + START_TIMEOUT_S + 0.1,
        ..inputs()
    };
    assert_eq!(step_nav(descending, &late).0, NavPhase::Globe);
    let generating = NavInputs {
        region_state: RegionState::Generating,
        ..inputs()
    };
    let (started, _) = step_nav(descending, &generating);
    assert!(matches!(
        started,
        NavPhase::Descending { started: true, .. }
    ));
    assert_eq!(step_nav(started, &inputs()).0, NavPhase::Globe, "failed");
    // Backspace while descending cancels.
    let (cancelled, actions) = step_nav(
        started,
        &NavInputs {
            backspace: true,
            ..generating
        },
    );
    assert_eq!(cancelled, NavPhase::Globe);
    assert_eq!(actions, vec![NavAction::Leave]);
}

#[test]
fn entering_fades_to_the_start_pose() {
    let in_region = NavInputs {
        region_state: RegionState::InRegion,
        region_centre: Some(RenderDir(DVec3::Z)),
        ..inputs()
    };
    let (mut phase, _) = step_nav(
        NavPhase::Globe,
        &NavInputs {
            entered: true,
            ..in_region
        },
    );
    assert!(!phase.region_shown());
    let mut placed_at = None;
    for frame in 1..=8 {
        let (next, actions) = step_nav(phase, &in_region);
        if actions.contains(&NavAction::PlaceAtStart) {
            assert!(placed_at.is_none());
            placed_at = Some(frame);
        }
        phase = next;
    }
    // 0.8 s at 0.1 s a frame: the swap at the midpoint, flying at the end.
    assert_eq!(placed_at, Some(4));
    assert_eq!(phase, NavPhase::Flying);
    assert!(phase.region_shown());
    assert_eq!(phase.overlay_alpha(), 0.0);
}

#[test]
fn snapshot_change_holds_until_the_region_is_ready() {
    let centre = RenderDir(DVec3::Z);
    let flying = NavInputs {
        region_state: RegionState::InRegion,
        region_centre: Some(centre),
        region_snapshot: Some(YearsBp(0.0)),
        time: YearsBp(100e6),
        ..inputs()
    };
    let carried = RenderDir(DVec3::X);
    let change = TimeChange {
        to: YearsBp(100e6),
        carried: Some(carried),
        agl_m: 1_234.0,
    };
    let (mut phase, actions) = step_nav(
        NavPhase::Flying,
        &NavInputs {
            time_change: Some(change),
            ..flying
        },
    );
    assert_eq!(
        actions,
        vec![NavAction::Request(RegionRequested {
            era_dir: carried,
            snapshot: YearsBp(100e6),
        })]
    );
    let generating = NavInputs {
        region_state: RegionState::Generating,
        ..flying
    };
    for _ in 0..30 {
        let (next, actions) = step_nav(phase, &generating);
        assert!(actions.is_empty(), "{actions:?}");
        phase = next;
    }
    assert!(phase.region_shown());
    assert!((phase.overlay_alpha() - 1.0).abs() < 1e-6, "held covered");
    // Back in `InRegion` with the old region still active: not ready.
    let (still, actions) = step_nav(phase, &flying);
    assert!(actions.is_empty(), "{actions:?}");
    phase = still;
    // The requested region is active but its `RegionGenerated` has not been seen (an equal
    // region left from before): still not ready.
    let ready = NavInputs {
        region_centre: Some(carried),
        region_snapshot: Some(YearsBp(100e6)),
        ..flying
    };
    let (still, actions) = step_nav(phase, &ready);
    assert!(actions.is_empty(), "{actions:?}");
    phase = still;
    // Generated and active: the camera moves, under full cover.
    let (next, actions) = step_nav(
        phase,
        &NavInputs {
            generated: Some((carried, YearsBp(100e6))),
            ..ready
        },
    );
    assert_eq!(
        actions,
        vec![NavAction::PlaceAfterChange { agl_m: 1_234.0 }]
    );
    phase = next;
    for _ in 0..5 {
        phase = step_nav(phase, &ready).0;
    }
    assert_eq!(phase, NavPhase::Flying);
}

/// Ruling R19: at an overlap the carry follows the plate the globe draws. Plates 202 and 101
/// overlap where `x < −0.3` and `z > 0.3`; the lowest ID (101) turns 90° by 100 Ma, but here
/// 202 (fixed) has the higher land, so the camera stays with 202.
#[test]
fn carry_follows_the_drawn_plate_at_an_overlap() {
    let crust = crust();
    let at = RenderDir(DVec3::new(-0.7, 0.0, 0.7).normalize());
    let (from, to) = (YearsBp(4.47e6), YearsBp(100e6));
    // Both plates are candidates there at 4.47 Ma.
    let mut candidates = Vec::new();
    crust.resolve(at, from.myr(), |c| {
        candidates.push(c.plate_id);
        None
    });
    assert_eq!(candidates, vec![PlateId(101), PlateId(202)]);
    // The lowest ID without ranking: plate 101's turn.
    let lowest = carry_crust_position(&crust, at, from, to, |_| None).unwrap();
    let on_101 = about_y(90.0) * (about_y(-4.0) * at.0);
    assert!(close(lowest, on_101), "{lowest:?}");
    // Higher land on 202: the camera stays where it is (202 is fixed).
    let land = |c: &Candidate| Some(if c.plate_id == PlateId(202) { 2.0 } else { 1.0 });
    let drawn = carry_crust_position(&crust, at, from, to, land).unwrap();
    assert!(close(drawn, at.0), "{drawn:?}");
    // And higher land on 101 goes with 101.
    let land = |c: &Candidate| Some(if c.plate_id == PlateId(101) { 2.0 } else { 1.0 });
    assert!(close(
        carry_crust_position(&crust, at, from, to, land).unwrap(),
        on_101
    ));
}

/// The shell's ranker reads the snapshot's cube frames as the renderer does: on the synthetic
/// crust with a frame bracket where plate 202's frame land is higher at the overlap, it ranks
/// 202 above 101; with no frames at hand, or in the geographic frame, there is no ranker.
#[test]
fn surface_ranker_ranks_like_the_renderer() {
    use timefly_core::formats::{DType, Grid, GridKind};
    use timefly_core::geodynamics::Candidate;
    let crust = crust();
    let timeline = timeline();
    // One constant frame per image slot: elevation 1,000 m (2,000 m in slot 3, 100 Ma) and full
    // coverage, two f16 channels on a cube of 8.
    let frame = |elevation: f32| {
        let n = 8u32;
        let mut data = Vec::new();
        for _ in 0..(6 * n * n) {
            for v in [elevation, 1.0] {
                data.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
            }
        }
        Arc::new(Grid {
            kind: GridKind::Cube,
            dtype: DType::F16,
            channels: 2,
            width: n,
            height: n,
            data,
        })
    };
    assert!(SurfaceRanker::new(&timeline, &crust, YearsBp(0.0), |_| Some(frame(0.0))).is_none());
    assert!(SurfaceRanker::new(&timeline, &crust, YearsBp(100e6), |_| None).is_none());
    let ranker =
        SurfaceRanker::new(&timeline, &crust, YearsBp(100e6), |_| Some(frame(1_000.0))).unwrap();
    let at = RenderDir(DVec3::new(-0.7, 0.0, 0.7).normalize());
    let mut ranks = Vec::new();
    crust.resolve(at, 100.0, |c: &Candidate| {
        let r = ranker.rank(&crust, c, at);
        ranks.push((c.plate_id, r));
        r
    });
    // Both plates are land at 1,000 m in the frame, ranked by the displaced surface.
    assert!(
        ranks.iter().all(|(_, r)| r.is_some_and(|r| r > 0.0)),
        "{ranks:?}"
    );
}

#[test]
fn missing_crust_or_backspace_leaves() {
    let centre = RenderDir(DVec3::Z);
    let flying = NavInputs {
        region_state: RegionState::InRegion,
        region_centre: Some(centre),
        ..inputs()
    };
    for trigger in [
        NavInputs {
            time_change: Some(TimeChange {
                to: YearsBp(250e6),
                carried: None,
                agl_m: 500.0,
            }),
            ..flying
        },
        NavInputs {
            backspace: true,
            ..flying
        },
    ] {
        let (mut phase, actions) = step_nav(NavPhase::Flying, &trigger);
        assert!(actions.is_empty());
        assert!(matches!(phase, NavPhase::Leaving { left: false, .. }));
        let mut all = Vec::new();
        for _ in 0..8 {
            let (next, actions) = step_nav(phase, &flying);
            all.extend(actions);
            phase = next;
        }
        assert_eq!(all, vec![NavAction::Leave, NavAction::CentreGlobe(centre)]);
        assert_eq!(phase, NavPhase::Globe);
    }
}

// ---------------------------------------------------------------------------------------------
// Headless apps

const STEP: Duration = Duration::from_millis(100);

#[derive(Resource, Default)]
struct Seen {
    requests: Vec<RegionRequested>,
    left: usize,
}

fn record(
    mut seen: ResMut<Seen>,
    mut requests: MessageReader<RegionRequested>,
    mut left: MessageReader<RegionLeft>,
) {
    seen.requests.extend(requests.read().copied());
    seen.left += left.read().count();
}

fn timeline() -> Timeline {
    // ICE-6G to 26 ka, the modern-terrain bridge frame at 30 ka, a PaleoDEM at 100 Ma.
    let meta = |age_myr: f64, image_slot| FrameMeta {
        age_myr,
        source: match age_myr {
            a if a < 0.03 => FrameSource::Ice6g,
            a if a < 1.0 => FrameSource::PaleoDem {
                source_rotation_age_myr: 0.0,
            },
            a => FrameSource::PaleoDem {
                source_rotation_age_myr: a,
            },
        },
        image_slot,
    };
    Timeline::new(
        vec![
            meta(0.0, 0),
            meta(0.026076, 1),
            meta(0.03, 2),
            meta(100.0, 3),
        ],
        vec![(0.0, 0.0)],
    )
    .unwrap()
}

fn globe_data() -> GlobeData {
    GlobeData {
        revision: BakeRevision {
            manifest_version: 1,
            manifest_sha256: String::new(),
        },
        cube_size: 16,
        timeline: Arc::new(timeline()),
        images: Vec::new(),
        frame_paths: Vec::new(),
        crust: Arc::new(crust()),
        attribution: Vec::new(),
    }
}

fn flat(res: u32, size_m: f64, h: f32) -> Field {
    Field {
        res,
        size_m,
        surface: vec![[0, 0, 0, 0]; (res * res) as usize],
        elevation_m: vec![h; (res * res) as usize],
    }
}

fn grid(center: RenderDir, snapshot: YearsBp, ground_m: f32) -> Arc<RegionGrid> {
    Arc::new(RegionGrid {
        spec: RegionSpec {
            inner_res: 9,
            apron_res: 9,
            ..RegionSpec::new(center, snapshot, 1)
        },
        inner: flat(9, REGION_SIZE_M, ground_m),
        apron: flat(9, APRON_SIZE_M, ground_m),
    })
}

/// A headless app with navigation and the globe data (the synthetic crust). With
/// `region_plugin`, the real region plugin handles `RegionLeft`; without it, requests are only
/// recorded.
fn nav_app(region_plugin: bool) -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, StatesPlugin));
    if region_plugin {
        app.add_plugins((AssetPlugin::default(), RegionPlugin));
    } else {
        app.add_logged_state::<RegionState>()
            .add_domain_event::<RegionRequested>()
            .add_domain_event::<RegionEntered>()
            .add_domain_event::<RegionLeft>()
            .add_domain_event::<RegionSnapshotChanged>()
            .add_domain_event::<RegionGenerated>();
    }
    app.init_resource::<ButtonInput<KeyCode>>()
        .add_plugins((TimePlugin, RegionNavigationPlugin))
        .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
        .insert_resource(globe_data())
        .init_resource::<Seen>()
        .add_systems(Last, record);
    app.update();
    app
}

fn phase(app: &App) -> NavPhase {
    app.world().resource::<Navigation>().phase
}

fn region_state(app: &App) -> RegionState {
    *app.world().resource::<State<RegionState>>().get()
}

/// What the region plugin does when a region is ready: insert it, send `RegionEntered` and
/// enter `InRegion`. Then runs until the player is flying.
fn enter(app: &mut App, grid: &Arc<RegionGrid>) {
    app.insert_resource(ActiveRegion {
        grid: grid.clone(),
        frame: RegionFrame {
            center: grid.spec.center,
        },
    });
    app.world_mut()
        .resource_mut::<NextState<RegionState>>()
        .set(RegionState::InRegion);
    app.world_mut()
        .write_message(RegionEntered { spec: grid.spec });
    let mut shown_before_swap = false;
    for _ in 0..20 {
        app.update();
        let nav = phase(app);
        let visible = app.world().resource::<TerrainSettings>().visible;
        if let NavPhase::Entering { placed: false, .. } = nav {
            shown_before_swap |= visible;
        }
        if nav == NavPhase::Flying {
            assert!(visible, "the region shows once flying");
            assert!(!shown_before_swap, "the globe shows until the midpoint");
            return;
        }
    }
    panic!("never flying: {:?}", phase(app));
}

fn press(app: &mut App, key: KeyCode) {
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(key);
    app.update();
    let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
    keys.release(key);
    keys.clear();
}

fn set_time(app: &mut App, years: f64) {
    app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(years);
}

#[test]
fn snapshot_change_to_missing_crust_returns_to_globe() {
    let mut app = nav_app(true);
    let centre = RenderDir(ON_PLATE);
    enter(&mut app, &grid(centre, YearsBp(0.0), 500.0));
    assert_eq!(region_state(&app), RegionState::InRegion);
    let start = *app.world().resource::<FlyState>();
    assert_eq!(start.pos_m, Vec3::new(0.0, 500.0 + 8_000.0, 0.0));

    // 250 Ma: plate 101's crust under the camera no longer exists.
    set_time(&mut app, 250e6);
    app.update();
    assert!(
        matches!(phase(&app), NavPhase::Leaving { .. }),
        "{:?}",
        phase(&app)
    );
    for _ in 0..12 {
        app.update();
    }
    assert_eq!(region_state(&app), RegionState::Globe);
    assert_eq!(phase(&app), NavPhase::Globe);
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.left, 1, "RegionLeft once");
    assert!(
        seen.requests.is_empty(),
        "no region at 250 Ma: {:?}",
        seen.requests
    );
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(!app.world().resource::<TerrainSettings>().visible);
    // The globe is centred on the region, close in, at the new time.
    let orbit = *app.world().resource::<OrbitState>();
    assert!(
        (orbit.direction().as_dvec3() - ON_PLATE).length() < 1e-5,
        "{orbit:?}"
    );
    assert_eq!(orbit.distance_km, NEAR_ORBIT_KM);
    assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(250e6));
}

#[test]
fn snapshot_change_regenerates_around_the_carried_position() {
    let mut app = nav_app(false);
    let centre = RenderDir(ON_PLATE);
    let first = grid(centre, YearsBp(0.0), 500.0);
    enter(&mut app, &first);
    // The camera flies 30 km east and drops to 2,000 m above the 500 m ground.
    let camera = Vec3::new(30_000.0, 2_500.0, 0.0);
    app.world_mut().resource_mut::<FlyState>().pos_m = camera;

    set_time(&mut app, 100e6);
    app.update();
    let requests = app.world().resource::<Seen>().requests.clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    let camera_dir = RegionFrame { center: centre }.local_to_dir(30_000.0, 0.0);
    let want = about_y(90.0) * camera_dir.0;
    assert!(close(requests[0].era_dir, want), "{requests:?}");
    assert_eq!(requests[0].snapshot, YearsBp(100e6));
    assert!(matches!(phase(&app), NavPhase::ChangingSnapshot { .. }));

    // The region plugin generates for a while (the fade holds covered), then replaces it.
    app.world_mut()
        .resource_mut::<NextState<RegionState>>()
        .set(RegionState::Generating);
    for _ in 0..15 {
        app.update();
    }
    let alpha = phase(&app).overlay_alpha();
    assert!((alpha - 1.0).abs() < 1e-6, "{alpha}");
    let second = grid(requests[0].era_dir, YearsBp(100e6), 1_100.0);
    app.insert_resource(ActiveRegion {
        grid: second.clone(),
        frame: RegionFrame {
            center: second.spec.center,
        },
    });
    app.world_mut()
        .resource_mut::<NextState<RegionState>>()
        .set(RegionState::InRegion);
    app.world_mut().write_message(RegionGenerated {
        spec: second.spec,
        millis: 1,
        tiles_fetched: 0,
        bytes_fetched: 0,
        fallback: false,
    });
    app.world_mut().write_message(RegionSnapshotChanged {
        from: YearsBp(0.0),
        to: YearsBp(100e6),
        spec: second.spec,
    });
    for _ in 0..8 {
        app.update();
    }
    assert_eq!(phase(&app), NavPhase::Flying);
    // Over the new centre, the same 2,000 m above the (new) ground, the same heading.
    let fly = *app.world().resource::<FlyState>();
    assert_eq!(fly.pos_m, Vec3::new(0.0, 1_100.0 + 2_000.0, 0.0));
    assert_eq!(fly.pitch, FlyState::default().pitch);
    assert_eq!(app.world().resource::<Seen>().requests.len(), 1);
}

#[test]
fn backspace_leaves_to_the_globe_centred_on_the_region() {
    let mut app = nav_app(true);
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    enter(&mut app, &grid(centre, YearsBp(0.0), 0.0));
    press(&mut app, LEAVE_KEY);
    assert!(matches!(phase(&app), NavPhase::Leaving { .. }));
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(region_state(&app), RegionState::Globe);
    assert_eq!(phase(&app), NavPhase::Globe);
    assert_eq!(app.world().resource::<Seen>().left, 1);
    let orbit = *app.world().resource::<OrbitState>();
    assert!(
        (orbit.direction().as_dvec3() - centre.0).length() < 1e-5,
        "{orbit:?}"
    );
}

#[test]
fn the_overlay_follows_the_fade() {
    let mut app = nav_app(false);
    let alpha = |app: &mut App| {
        let world = app.world_mut();
        let (node, color) = world
            .query_filtered::<(&Node, &BackgroundColor), With<FadeOverlay>>()
            .single(world)
            .unwrap();
        (node.display, color.0.alpha())
    };
    assert_eq!(alpha(&mut app), (Display::None, 0.0));
    let g = grid(RenderDir(ON_PLATE), YearsBp(0.0), 0.0);
    app.insert_resource(ActiveRegion {
        grid: g.clone(),
        frame: RegionFrame {
            center: g.spec.center,
        },
    });
    app.world_mut()
        .resource_mut::<NextState<RegionState>>()
        .set(RegionState::InRegion);
    app.world_mut()
        .write_message(RegionEntered { spec: g.spec });
    let mut peak: f32 = 0.0;
    for _ in 0..10 {
        app.update();
        let (display, a) = alpha(&mut app);
        assert_eq!(display == Display::Flex, a > 0.0);
        peak = peak.max(a);
    }
    assert!(peak > 0.99, "{peak}");
    assert_eq!(alpha(&mut app), (Display::None, 0.0));
}

// ---------------------------------------------------------------------------------------------
// The camera against the terrain being shown, through a snapshot change

/// A stand-in for the region job: on a request it enters `Generating`, and `after` updates later
/// inserts the region through `Commands` and sends `RegionSnapshotChanged`, as `poll_generation`
/// does, from inside [`RegionSystems`].
#[derive(Resource)]
struct FakeJob {
    after: u32,
    ground_m: f32,
    pending: Option<(RegionRequested, u32)>,
}

fn fake_job(
    mut job: ResMut<FakeJob>,
    mut requests: MessageReader<RegionRequested>,
    active: Option<Res<ActiveRegion>>,
    mut commands: Commands,
    mut changed: MessageWriter<RegionSnapshotChanged>,
    mut generated: MessageWriter<RegionGenerated>,
    mut next: ResMut<NextState<RegionState>>,
) {
    if let Some(&r) = requests.read().last() {
        job.pending = Some((r, job.after));
        next.set(RegionState::Generating);
    }
    let ground = job.ground_m;
    let Some((r, left)) = job.pending.as_mut() else {
        return;
    };
    if *left > 0 {
        *left -= 1;
        return;
    }
    let g = grid(r.era_dir, r.snapshot, ground);
    commands.insert_resource(ActiveRegion {
        grid: g.clone(),
        frame: RegionFrame {
            center: g.spec.center,
        },
    });
    generated.write(RegionGenerated {
        spec: g.spec,
        millis: 1,
        tiles_fetched: 0,
        bytes_fetched: 0,
        fallback: false,
    });
    changed.write(RegionSnapshotChanged {
        from: active.map_or(YearsBp(0.0), |a| a.grid.spec.snapshot),
        to: r.snapshot,
        spec: g.spec,
    });
    next.set(RegionState::InRegion);
    job.pending = None;
}

/// Frames where the region camera sat below the clearance over the terrain on screen.
#[derive(Resource, Default)]
struct Clearance {
    checked: u32,
    violations: Vec<String>,
}

fn check_clearance(
    scene: Option<Res<crate::terrain::TerrainScene>>,
    cameras: Query<&Transform>,
    nav: Res<Navigation>,
    mut seen: ResMut<Clearance>,
) {
    let Some(scene) = scene else { return };
    let Ok(t) = cameras.get(scene.camera) else {
        return;
    };
    let p = t.translation;
    let ground = blended_height_m(&scene.grid, f64::from(p.x), f64::from(p.z)).max(0.0);
    seen.checked += 1;
    if p.y < ground + CAMERA_CLEARANCE_M - 1e-3 {
        seen.violations.push(format!(
            "camera {p} under {} over the {} m grid ({:?})",
            ground + CAMERA_CLEARANCE_M,
            scene.grid.spec.snapshot.0,
            nav.phase
        ));
    }
}

/// Fix round 1, important 2: the new region's ground is 1.9 km higher (Scandinavia onto the
/// ice). Whether it is ready before the fade's midpoint or long after, the camera is never
/// below the clearance over the terrain being drawn, on any frame.
#[test]
fn the_camera_clears_the_terrain_shown_through_a_snapshot_change() {
    for after in [0, 1, 3, 12] {
        let mut app = nav_app(false);
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<bevy::render::storage::ShaderBuffer>()
            .init_asset::<crate::terrain::material::TerrainMaterial>()
            .add_plugins(crate::fly_camera::FlyCameraPlugin)
            .insert_resource(FakeJob {
                after,
                ground_m: 288.0 + 1_862.0,
                pending: None,
            })
            .init_resource::<Clearance>()
            .add_systems(Update, fake_job.in_set(crate::region::RegionSystems))
            .add_systems(Last, check_clearance);
        crate::terrain::add_terrain_systems(&mut app);
        app.world_mut().spawn((
            crate::globe::GlobeCamera,
            Camera::default(),
            IsDefaultUiCamera,
        ));
        enter(&mut app, &grid(RenderDir(ON_PLATE), YearsBp(0.0), 288.0));
        // 274 m above the ground, 5 km north.
        app.world_mut().resource_mut::<FlyState>().pos_m = Vec3::new(0.0, 562.0, -5_000.0);
        app.update();
        set_time(&mut app, 100e6);
        for _ in 0..40 {
            app.update();
        }
        assert_eq!(phase(&app), NavPhase::Flying, "after {after}");
        let fly = *app.world().resource::<FlyState>();
        assert_eq!(
            fly.pos_m,
            Vec3::new(0.0, 2_150.0 + 274.0, 0.0),
            "after {after}"
        );
        let seen = app.world().resource::<Clearance>();
        assert!(seen.checked > 40, "after {after}: {}", seen.checked);
        assert!(
            seen.violations.is_empty(),
            "after {after}: {:#?}",
            seen.violations
        );
    }
}
