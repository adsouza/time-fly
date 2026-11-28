use std::sync::Arc;
use std::time::Duration;

use bevy::asset::AssetPlugin;
use bevy::math::DVec3;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly_core::cube::{CubeFace, texel_center_dir};
use timefly_core::flight::altitudes::{
    CLOUD_CLEAR_S, CLOUD_GATHER_S, CLOUD_TOP_KM, DIVE_ALTITUDE_KM, EMERGE_S, GLOBE_MAX_ALTITUDE_KM,
    REGION_CLOUD_BASE_M, REGION_EMERGE_TO_M,
};
use timefly_core::flight::globe::{GlobeFlight, heading_to_region_yaw};
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::geodynamics::{
    Candidate, FootprintEntry, PlateFootprints, PlateId, PlateIndex, RotationTable,
};
use timefly_core::surface::{FrameMeta, FrameSource, Timeline};
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

use super::*;
use crate::cloud::CloudDensity;
use crate::data::BakeRevision;
use crate::diagnostics::aspects::DiagnosticsAppExt;
use crate::fly_camera::{CAMERA_CEILING_M, CAMERA_CLEARANCE_M, fly_speed};
use crate::globe::EARTH_RADIUS_KM;
use crate::region::{RegionGenerated, RegionPlugin, RegionSnapshotChanged, RequestPurpose};
use crate::time::TimePlugin;
use timefly_core::terrain::blended_surface_m;

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

    // A fade is opaque exactly at its midpoint and done at its duration.
    let f = Fade::new(0.8);
    assert!(!f.opaque() && !f.done() && f.alpha() == 0.0);
    let half = f.advance(0.4, false);
    assert!(half.opaque() && !half.done());
    let end = half.advance(0.4, false);
    assert!(end.done() && end.alpha() == 0.0);
    assert_eq!(end.advance(1.0, false).progress(), 1.0);
    // Holding stops it at the midpoint, fully covered, however long it waits.
    let mut held = Fade::new(SNAPSHOT_FADE_S);
    for _ in 0..100 {
        held = held.advance(0.1, true);
    }
    assert!(held.opaque() && !held.done());
    assert!(held.advance(0.5, false).done());
    // Past the midpoint it never holds.
    let late = Fade::new(1.0).advance(0.7, false).advance(0.1, true);
    assert!((late.progress() - 0.8).abs() < 1e-6);
}

#[test]
fn caption_waits_a_second() {
    assert!(!caption_shown(None, None));
    assert!(!caption_shown(Some(0.0), None));
    assert!(!caption_shown(Some(0.99), None));
    assert!(caption_shown(Some(1.01), None));
    // A hop's hold at full cover shows it too (spec §6 "Holding"; the final review): a
    // neighbour's job never enters `RegionState::Generating`.
    assert!(!caption_shown(None, Some(0.99)));
    assert!(caption_shown(None, Some(1.01)));
    assert!(caption_shown(Some(0.2), Some(1.01)));
}

/// A region lost from under the fly (a snapshot change's generation error, the region gone
/// while flying, emerging, climbing or hopping) comes out over the globe above the fly's place
/// in it, as a climb does, not over the dive point a continent of hops away (the final review).
#[test]
fn a_lost_region_comes_out_over_the_flys_place() {
    let centre = RenderDir(geo_to_render(latlon_to_geo(46.5, 10.0)).0.normalize());
    let place = RenderDir(geo_to_render(latlon_to_geo(47.0, 14.0)).0.normalize());
    let heading = DVec3::X;
    let phases = [
        NavPhase::Flying,
        NavPhase::Emerging {
            world: World::Region,
            elapsed_s: 0.5,
        },
        NavPhase::ChangingSnapshot {
            fade: Fade::new(SNAPSHOT_FADE_S).advance(0.5, true),
            to: YearsBp(1e6),
            requested: centre,
            agl_m: 1_000.0,
            generated: false,
            placed: false,
        },
        NavPhase::Climbing {
            elapsed_s: 0.1,
            centre: Some(centre),
        },
        NavPhase::Hopping {
            centre,
            edge: HopEdge { axis: 0, side: 1.0 },
            lift_from_agl_m: 1_000.0,
            swapped: false,
            elapsed_s: 3.0,
            lift: 1.0,
            failed: false,
            turned_back: false,
        },
    ];
    let lost = NavInputs {
        region_state: RegionState::Globe,
        region_fly: Some((place, heading)),
        globe_dir: centre,
        ..inputs()
    };
    let over_globe = NavPhase::Emerging {
        world: World::Globe,
        elapsed_s: 0.0,
    };
    let placed = NavAction::PlaceOnGlobe {
        dir: place,
        heading,
        altitude_km: DIVE_ALTITUDE_KM,
    };
    for phase in phases {
        let (next, actions) = step_nav(phase, &lost);
        assert_eq!(next, over_globe, "{phase:?}");
        assert!(actions.contains(&placed), "{phase:?}: {actions:?}");
        // Not knowing the fly's place, the globe's fly stays where it is.
        let unknown = NavInputs {
            region_fly: None,
            ..lost
        };
        let (_, actions) = step_nav(phase, &unknown);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, NavAction::PlaceOnGlobe { .. })),
            "{phase:?}: {actions:?}"
        );
    }
    // A hop whose region is gone from the inputs (no centre) comes out the same way.
    let hop = phases[4];
    let gone = NavInputs {
        region_state: RegionState::InRegion,
        region_centre: None,
        ..lost
    };
    let (next, actions) = step_nav(hop, &gone);
    assert_eq!(next, over_globe);
    assert!(actions.contains(&placed), "{actions:?}");
    // A dive that fails never left the globe: its fly is already over the place.
    let diving = NavPhase::Diving {
        target: centre,
        snapshot: YearsBp(0.0),
        since_s: 0.0,
        started: true,
    };
    let (next, actions) = step_nav(
        diving,
        &NavInputs {
            globe_altitude_km: DIVE_ALTITUDE_KM - 100.0,
            ..lost
        },
    );
    assert_eq!(next, over_globe);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, NavAction::PlaceOnGlobe { .. })),
        "{actions:?}"
    );
}

#[test]
fn only_an_opaque_cloud_covers_the_view() {
    let changing = |fade: Fade| NavPhase::ChangingSnapshot {
        fade,
        to: YearsBp(1e3),
        requested: RenderDir(DVec3::Z),
        agl_m: 100.0,
        generated: false,
        placed: false,
    };
    let i = inputs();
    // Held at the midpoint while the new region generates: covered.
    let held = Fade::new(SNAPSHOT_FADE_S).advance(2.0, true);
    assert!(changing(held).covers_view(&i));
    // On the way down or up again, flying, or on the globe above the cloud: not covered.
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.3, true)).covers_view(&i));
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.8, false)).covers_view(&i));
    assert!(!NavPhase::Flying.covers_view(&i));
    assert!(!NavPhase::Globe.covers_view(&i));
    // A dive waiting inside the cloud is covered; a fly at the region's cloud base too.
    let diving = NavPhase::Diving {
        target: RenderDir(DVec3::Z),
        snapshot: YearsBp(0.0),
        since_s: 0.0,
        started: true,
    };
    let in_cloud = NavInputs {
        globe_altitude_km: DIVE_ALTITUDE_KM - 100.0,
        ..i
    };
    assert!(diving.covers_view(&in_cloud));
    assert!(!diving.covers_view(&i));
    let at_base = NavInputs {
        region_height_m: REGION_CLOUD_BASE_M as f32,
        ..i
    };
    assert!(NavPhase::Flying.covers_view(&at_base));
}

/// The altitude bands' smoothstep tails never quite reach 0: a fly left at 2,499.9999999 km
/// after climbing out would keep the cloud's pass running for nothing. They stay below the
/// cloud's floor ([`crate::cloud::CLEAR_BELOW`], applied in its one sink), which draws nothing.
#[test]
fn a_cloud_too_faint_to_see_is_clear() {
    use timefly_core::flight::altitudes::REGION_CLOUD_BAND_M;
    use timefly_core::flight::cloud::{globe_density, region_density};
    let top = CLOUD_TOP_KM - 1e-7;
    assert!(globe_density(top) > 0.0);
    let i = NavInputs {
        globe_altitude_km: top,
        ..inputs()
    };
    let tail = NavPhase::Globe.cloud_density(&i);
    assert!(tail < crate::cloud::CLEAR_BELOW, "{tail}");
    let floor = (REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M) as f32 + 1.0;
    assert!(region_density(floor) > 0.0);
    let i = NavInputs {
        region_height_m: floor,
        ..inputs()
    };
    let tail = NavPhase::Flying.cloud_density(&i);
    assert!(tail < crate::cloud::CLEAR_BELOW, "{tail}");
    // Anything a display could show still draws.
    let i = NavInputs {
        globe_altitude_km: CLOUD_TOP_KM - 100.0,
        ..inputs()
    };
    let d = NavPhase::Globe.cloud_density(&i);
    assert!(
        d > 1.0 / 255.0 && d == globe_density(CLOUD_TOP_KM - 100.0),
        "{d}"
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
    let i = inputs();
    // First entries: from the globe (a launch region or the bench) and while diving.
    assert!(NavPhase::Globe.allows_large_gen_budget(&i));
    assert!(
        NavPhase::Diving {
            target: RenderDir(DVec3::Z),
            snapshot: YearsBp(0.0),
            since_s: 0.0,
            started: true,
        }
        .allows_large_gen_budget(&i)
    );
    // A snapshot change held under the opaque cloud.
    let held = Fade::new(SNAPSHOT_FADE_S).advance(2.0, true);
    assert!(changing(held).allows_large_gen_budget(&i));
    // Flying, or the region showing through the cloud: the small budget.
    assert!(!NavPhase::Flying.allows_large_gen_budget(&i));
    assert!(!changing(Fade::new(SNAPSHOT_FADE_S).advance(0.3, true)).allows_large_gen_budget(&i));
    let emerging = NavPhase::Emerging {
        world: World::Region,
        elapsed_s: 0.6,
    };
    assert!(!emerging.allows_large_gen_budget(&i));
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

thread_local! {
    /// The frames [`frame`]'s stand-in for the terrain scene still holds the cloud for; every
    /// test's inputs start from [`inputs`], which clears it.
    static SETTLING_FRAMES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn inputs() -> NavInputs {
    SETTLING_FRAMES.set(0);
    NavInputs {
        now_s: 10.0,
        load_s: 10.0,
        dt_s: 0.1,
        region_state: RegionState::Globe,
        time: YearsBp(30_000.0),
        climb_key: false,
        entered: false,
        generated: None,
        region_centre: None,
        region_snapshot: None,
        time_change: None,
        globe_altitude_km: GLOBE_MAX_ALTITUDE_KM,
        globe_dir: RenderDir(DVec3::Z),
        globe_heading: DVec3::Y,
        region_height_m: 8_000.0,
        region_fly: None,
        shown_density: 0.0,
        region_ground_m: 0.0,
        region_pos_m: Vec2::ZERO,
        region_vel_mps: Vec2::ZERO,
        neighbour: None,
        preload_failed: false,
        vertical_held: false,
        time_queued: false,
        fields_settling: false,
    }
}

/// What the shell does around [`step_nav`], for the pure tests: one frame decided, then the
/// placements and eases applied to the inputs' flies. Returns the next phase, the actions, and
/// the cloud density on screen before (last frame's) and after (this frame's) the decision.
fn frame(phase: NavPhase, i: &mut NavInputs) -> (NavPhase, Vec<NavAction>, f32, f32) {
    let before = phase.cloud_density(i);
    let (next, actions) = step_nav(phase, i);
    // As the terrain scene: a swap this frame (a hop's, a snapshot change's) leaves the next
    // `REPLACE_EXTRA_HELD_FRAMES` frames' fields incomplete (`TerrainScene::holds_cloud`).
    let swapped = actions.iter().any(|a| {
        matches!(
            a,
            NavAction::SwapToNeighbour | NavAction::PlaceAfterChange { .. }
        )
    });
    let left = if swapped {
        crate::terrain::REPLACE_EXTRA_HELD_FRAMES
    } else {
        SETTLING_FRAMES.get().saturating_sub(1)
    };
    SETTLING_FRAMES.set(left);
    i.fields_settling = left > 0;
    for action in &actions {
        match *action {
            NavAction::SnapTime(t) => i.time = t,
            NavAction::PlaceInRegion { height_m, .. } => i.region_height_m = height_m,
            NavAction::PlaceOnGlobe {
                dir,
                heading,
                altitude_km,
            } => {
                i.globe_dir = dir;
                i.globe_heading = heading;
                i.globe_altitude_km = altitude_km;
            }
            NavAction::EaseFly { vertical_mps } => {
                let dy = vertical_mps * i.dt_s;
                if next.region_shown() {
                    i.region_height_m = (i.region_height_m + dy).min(CAMERA_CEILING_M);
                } else {
                    i.globe_altitude_km = (i.globe_altitude_km + f64::from(dy) / 1000.0)
                        .max(timefly_core::flight::globe::GLOBE_MIN_ALTITUDE_KM);
                }
            }
            NavAction::LiftTo { height_m } => i.region_height_m = height_m,
            NavAction::ReturnToAgl { agl_m } => {
                // As the shell eases it: at the M2 speed for its height, never past the target.
                let agl = i.region_height_m - i.region_ground_m;
                let step = fly_speed(agl) * i.dt_s;
                i.region_height_m += (agl_m - agl).clamp(-step, step);
            }
            NavAction::TurnBack { pos_m, .. } | NavAction::PlaceInside { pos_m } => {
                i.region_pos_m = pos_m;
            }
            NavAction::SwapToNeighbour => {
                let (centre, _) = i.neighbour.take().expect("a swap with a neighbour ready");
                let old = RegionFrame {
                    center: i.region_centre.expect("a region to leave"),
                };
                let new = RegionFrame { center: centre };
                let p = DVec3::new(
                    f64::from(i.region_pos_m.x),
                    f64::from(i.region_height_m),
                    f64::from(i.region_pos_m.y),
                );
                let (q, _) = timefly_core::flight::hop::carry_pose(&old, &new, p, 0.0);
                i.region_pos_m = Vec2::new(q.x as f32, q.z as f32);
                i.region_centre = Some(centre);
            }
            _ => {}
        }
    }
    let after = next.cloud_density(i);
    i.shown_density = after;
    (next, actions, before, after)
}

/// The vertical ease among `actions`, if any.
fn ease(actions: &[NavAction]) -> Option<f32> {
    actions.iter().find_map(|a| match *a {
        NavAction::EaseFly { vertical_mps } => Some(vertical_mps),
        _ => None,
    })
}

/// Whether `a` swaps the world on screen (or moves the region under the camera).
fn is_swap(a: &NavAction) -> bool {
    matches!(
        a,
        NavAction::PlaceInRegion { .. }
            | NavAction::PlaceOnGlobe { .. }
            | NavAction::PlaceAfterChange { .. }
            | NavAction::PlaceAtStart
    )
}

/// Everest at the present, for the dives.
fn everest() -> RenderDir {
    RenderDir(geo_to_render(latlon_to_geo(28.0, 86.9)).0.normalize())
}

/// A dive committed to `target` at 21 ka, generating.
fn diving(target: RenderDir) -> NavPhase {
    NavPhase::Diving {
        target,
        snapshot: YearsBp(21_000.0),
        since_s: 10.0,
        started: true,
    }
}

/// The region at `target`, 21 ka, active.
fn ready(target: RenderDir, i: NavInputs) -> NavInputs {
    NavInputs {
        region_state: RegionState::InRegion,
        region_centre: Some(target),
        region_snapshot: Some(YearsBp(21_000.0)),
        ..i
    }
}

#[test]
fn descending_below_the_dive_altitude_commits_once() {
    let target = everest();
    let east = DVec3::Y.cross(target.0).normalize();
    let mut i = NavInputs {
        globe_dir: target,
        globe_heading: east,
        globe_altitude_km: DIVE_ALTITUDE_KM,
        ..inputs()
    };
    // At the dive altitude itself: not yet.
    assert_eq!(step_nav(NavPhase::Globe, &i), (NavPhase::Globe, vec![]));
    i.globe_altitude_km = DIVE_ALTITUDE_KM - 1.0;
    // Not while a region is still up (the climb's `RegionLeft` not yet handled).
    let up = NavInputs {
        region_state: RegionState::InRegion,
        ..i
    };
    assert_eq!(step_nav(NavPhase::Globe, &up), (NavPhase::Globe, vec![]));
    let (mut phase, actions) = step_nav(NavPhase::Globe, &i);
    let snapshot = YearsBp(21_000.0);
    assert_eq!(
        actions,
        vec![
            NavAction::SnapTime(snapshot),
            NavAction::Request(RegionRequested {
                era_dir: target,
                snapshot,
                purpose: RequestPurpose::Enter,
            }),
        ]
    );
    assert!(
        matches!(phase, NavPhase::Diving { target: t, snapshot: s, started: false, .. }
            if t == target && s == snapshot),
        "{phase:?}"
    );
    // Exactly once: the dive goes on below the dive altitude, generating, asking nothing more.
    i.time = snapshot;
    let mut asked = 0;
    for k in 0..50 {
        let state = if k < 5 {
            RegionState::Globe
        } else {
            RegionState::Generating
        };
        let (next, actions) = step_nav(
            phase,
            &NavInputs {
                region_state: state,
                globe_altitude_km: DIVE_ALTITUDE_KM - 1.0 - 5.0 * f64::from(k),
                load_s: 10.0 + 0.1 * f64::from(k),
                ..i
            },
        );
        asked += actions
            .iter()
            .filter(|a| matches!(a, NavAction::Request(_) | NavAction::SnapTime(_)))
            .count();
        phase = next;
    }
    assert_eq!(asked, 0);
    assert!(matches!(phase, NavPhase::Diving { started: true, .. }));
    // Already on a snapshot: no snap, just the request.
    let (_, actions) = step_nav(
        NavPhase::Globe,
        &NavInputs {
            time: YearsBp(66e6),
            ..i
        },
    );
    assert_eq!(actions.len(), 1);
}

/// A dive whose request never starts gives up on the load clock only; one whose generation
/// fails climbs back out of the cloud. Either way the fly is never left in the white-out.
#[test]
fn a_failed_dive_climbs_back_out() {
    let target = everest();
    let i = NavInputs {
        globe_altitude_km: DIVE_ALTITUDE_KM - 50.0,
        ..inputs()
    };
    let unstarted = NavPhase::Diving {
        target,
        snapshot: YearsBp(21_000.0),
        since_s: 10.0,
        started: false,
    };
    assert!(matches!(step_nav(unstarted, &i).0, NavPhase::Diving { .. }));
    // Only the load clock counts: wall-clock time alone (a stalled page) does not give up.
    let stalled = NavInputs { now_s: 70.0, ..i };
    assert!(matches!(
        step_nav(unstarted, &stalled).0,
        NavPhase::Diving { .. }
    ));
    let late = NavInputs {
        load_s: 10.0 + START_TIMEOUT_S + 0.1,
        ..i
    };
    let out = NavPhase::Emerging {
        world: World::Globe,
        elapsed_s: 0.0,
    };
    assert_eq!(
        step_nav(unstarted, &late),
        (out, vec![NavAction::CancelRequest])
    );
    // Started, then back on the globe: the generation failed.
    assert_eq!(step_nav(diving(target), &i).0, out);
    // The emergence climbs it to the cloud top and the cloud clears.
    let mut i = i;
    let mut phase = out;
    for _ in 0..40 {
        phase = frame(phase, &mut i).0;
    }
    assert_eq!(phase, NavPhase::Globe);
    assert!((i.globe_altitude_km - CLOUD_TOP_KM).abs() < 1e-6);
    assert_eq!(phase.cloud_density(&i), 0.0);
}

#[test]
fn waiting_holds_in_the_cloud() {
    let target = everest();
    let mut i = NavInputs {
        region_state: RegionState::Generating,
        globe_altitude_km: DIVE_ALTITUDE_KM - 1.0,
        ..inputs()
    };
    let mut phase = diving(target);
    let mut last = f32::NEG_INFINITY;
    let mut speed = 0.0;
    // Fifteen seconds without the region: held in the cloud, descending ever more gently.
    for k in 0..150 {
        let (next, actions, before, after) = frame(phase, &mut i);
        assert_eq!((before, after), (1.0, 1.0), "frame {k}");
        assert!(
            matches!(next, NavPhase::Diving { .. }),
            "frame {k}: {next:?}"
        );
        speed = ease(&actions).expect("the fly keeps descending");
        assert!(speed <= 0.0, "frame {k}: {speed}");
        assert!(speed >= last, "frame {k}: speeds up {last} -> {speed}");
        last = speed;
        phase = next;
        i.load_s += f64::from(i.dt_s);
        i.now_s += f64::from(i.dt_s);
    }
    // A drift: no more than 5 % of the M2 speed at that height (capped at MAX_SPEED_MPS).
    let height_m = (i.globe_altitude_km * 1000.0) as f32;
    assert!(
        speed.abs() <= 0.05 * fly_speed(height_m),
        "{speed} m/s at {height_m} m"
    );
    assert!(i.globe_altitude_km < DIVE_ALTITUDE_KM);
    assert!(i.globe_altitude_km > timefly_core::flight::globe::GLOBE_MIN_ALTITUDE_KM);
}

#[test]
fn entering_places_the_fly_below_the_cloud_base_with_its_heading() {
    let target = RenderDir(DVec3::new(0.3, 0.5, 0.8).normalize());
    let basis = RegionFrame { center: target }.basis();
    // Heading east over the dive point.
    let mut i = ready(
        target,
        NavInputs {
            entered: true,
            globe_dir: target,
            globe_heading: basis.east,
            globe_altitude_km: DIVE_ALTITUDE_KM - 50.0,
            time: YearsBp(21_000.0),
            shown_density: 1.0,
            ..inputs()
        },
    );
    let mut phase = NavPhase::Diving {
        target,
        snapshot: YearsBp(21_000.0),
        since_s: 10.0,
        started: true,
    };
    let (next, actions, before, after) = frame(phase, &mut i);
    let yaw = heading_to_region_yaw(target.0, basis.east, basis.north, basis.east);
    assert!(
        (yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-6,
        "east is a right turn: {yaw}"
    );
    assert_eq!(
        actions,
        vec![NavAction::PlaceInRegion {
            yaw,
            height_m: REGION_CLOUD_BASE_M as f32,
        }]
    );
    assert_eq!((before, after), (1.0, 1.0));
    assert_eq!(
        next,
        NavPhase::Emerging {
            world: World::Region,
            elapsed_s: 0.0
        }
    );
    assert!(next.region_shown());
    // Emerging: down to REGION_EMERGE_TO_M over EMERGE_S as the cloud thins, then flying. The
    // climb out does not trigger at the base it starts from (ruling P4).
    i.entered = false;
    phase = next;
    let (mut t, mut height, mut density) = (0.0f32, i.region_height_m, 1.0f32);
    while phase != NavPhase::Flying {
        let (next, _, _, after) = frame(phase, &mut i);
        assert!(
            matches!(next, NavPhase::Emerging { .. } | NavPhase::Flying),
            "{next:?}"
        );
        t += i.dt_s;
        assert!(i.region_height_m <= height, "climbed at {t} s");
        assert!(after <= density, "thickened at {t} s");
        (height, density) = (i.region_height_m, after);
        phase = next;
        assert!(t <= EMERGE_S as f32 + 0.5, "never flying");
    }
    assert!((t - EMERGE_S as f32).abs() < 1e-3, "{t} s");
    assert!(
        (i.region_height_m - REGION_EMERGE_TO_M as f32).abs() < 1.0,
        "{}",
        i.region_height_m
    );
    assert_eq!(density, 0.0);
}

#[test]
fn climbing_out_of_the_cloud_cancels_the_dive() {
    let target = everest();
    // The region is even ready, but the player climbs before the cloud is opaque again.
    // Over the dive point: the dive targets the ground under the fly.
    let mut i = ready(
        target,
        NavInputs {
            globe_altitude_km: DIVE_ALTITUDE_KM + 10.0,
            globe_dir: target,
            ..inputs()
        },
    );
    let mut phase = diving(target);
    let mut cancelled = false;
    for k in 0..100 {
        // The player's climb: faster than the in-cloud descent.
        i.globe_altitude_km += 200.0;
        let (next, actions, _, after) = frame(phase, &mut i);
        assert!(!actions.iter().any(is_swap), "frame {k}: {actions:?}");
        if actions.contains(&NavAction::CancelRequest) {
            assert!(i.globe_altitude_km > CLOUD_TOP_KM);
            assert_eq!(next, NavPhase::Globe);
            assert!(after < 1.0);
            cancelled = true;
            phase = next;
            break;
        }
        phase = next;
    }
    assert!(cancelled, "never cancelled: {phase:?}");
    // The cloud clears within CLOUD_CLEAR_S over the globe, and nothing is stuck at density 1.
    let frames = (CLOUD_CLEAR_S as f32 / i.dt_s).ceil() as usize;
    let mut density = 1.0;
    for _ in 0..frames {
        let (next, actions, _, after) = frame(phase, &mut i);
        assert!(actions.is_empty(), "{actions:?}");
        (phase, density) = (next, after);
    }
    assert_eq!(phase, NavPhase::Globe);
    assert_eq!(density, 0.0);
}

/// The final review: the dive's horizontal damping is for a fly waiting in the opaque cloud,
/// where the swap can come; it held a fly that had climbed back above the dive altitude at 5%
/// speed all the way to the cloud top. There it flies freely, and flying off the dive point
/// calls the dive off, so a later descent dives where the fly is, never into the old target.
#[test]
fn a_dive_damps_the_drift_only_in_the_opaque_cloud() {
    let target = everest();
    let d = diving(target);
    assert!(d.damps_dive_drift(DIVE_ALTITUDE_KM));
    assert!(d.damps_dive_drift(DIVE_ALTITUDE_KM - 300.0));
    assert!(!d.damps_dive_drift(DIVE_ALTITUDE_KM + 1.0));
    assert!(!d.damps_dive_drift(2_000.0));
    assert!(!NavPhase::Globe.damps_dive_drift(DIVE_ALTITUDE_KM - 300.0));
    // `off` km from the target along the ground, at `alt` km.
    let east = DVec3::Y.cross(target.0).normalize();
    let away = |off_km: f64, alt: f64| {
        let a = off_km / EARTH_RADIUS_KM;
        NavInputs {
            globe_altitude_km: alt,
            globe_dir: RenderDir(target.0 * a.cos() + east * a.sin()),
            region_state: RegionState::Generating,
            ..inputs()
        }
    };
    // Above the opaque cloud, flown off the dive point: the dive is off.
    let (next, actions) = step_nav(d, &away(DIVE_LEAVE_KM + 1.0, 1_600.0));
    assert_eq!(next, NavPhase::Globe);
    assert_eq!(actions, [NavAction::CancelRequest]);
    // Still over it (a climb straight up), or waiting in the opaque cloud: the dive goes on.
    let (next, _) = step_nav(d, &away(DIVE_LEAVE_KM - 1.0, 1_600.0));
    assert!(matches!(next, NavPhase::Diving { .. }), "{next:?}");
    let (next, _) = step_nav(d, &away(DIVE_LEAVE_KM + 1.0, DIVE_ALTITUDE_KM - 100.0));
    assert!(matches!(next, NavPhase::Diving { .. }), "{next:?}");
}

/// Climbs out from `phase` (already `Climbing`) and returns the swap frame's actions, after
/// checking the cloud is opaque on both sides of it; then emerges over the globe to the cloud
/// top and checks it ends there, clear.
fn climb_out(mut phase: NavPhase, i: &mut NavInputs) -> Vec<NavAction> {
    let gather = (CLOUD_GATHER_S as f32 / i.dt_s).ceil() as usize;
    let mut swap = None;
    for k in 0..gather + 2 {
        let (next, actions, before, after) = frame(phase, i);
        i.climb_key = false;
        assert!(
            after >= before,
            "frame {k}: the cloud thinned {before} -> {after}"
        );
        if actions.iter().any(is_swap) {
            assert_eq!((before, after), (1.0, 1.0), "frame {k}");
            assert_eq!(
                next,
                NavPhase::Emerging {
                    world: World::Globe,
                    elapsed_s: 0.0
                }
            );
            swap = Some(actions);
            phase = next;
            break;
        }
        assert!(
            matches!(next, NavPhase::Climbing { .. }),
            "frame {k}: {next:?}"
        );
        phase = next;
    }
    let swap = swap.expect("never swapped");
    let mut t = 0.0;
    while phase != NavPhase::Globe {
        phase = frame(phase, i).0;
        t += i.dt_s;
        assert!(t <= EMERGE_S as f32 + 0.5, "never on the globe: {phase:?}");
    }
    assert!((i.globe_altitude_km - CLOUD_TOP_KM).abs() < 1e-6);
    assert_eq!(phase.cloud_density(i), 0.0);
    swap
}

/// A fly 20 km east and 35 km north of the centre of a region at `centre`, `height_m` above
/// sea level, heading 0.7 rad west of north.
fn region_fly(centre: RenderDir, height_m: f32) -> NavInputs {
    let fly = fly_on_globe(&RegionFrame { center: centre }, 20_000.0, -35_000.0, 0.7);
    ready(
        centre,
        NavInputs {
            region_height_m: height_m,
            region_fly: Some(fly),
            // The globe's fly where the dive left it, inside the cloud.
            globe_altitude_km: 900.0,
            time: YearsBp(21_000.0),
            ..inputs()
        },
    )
}

#[test]
fn climbing_through_the_cloud_base_returns_to_the_globe() {
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    let mut i = region_fly(centre, CLIMB_OUT_M - 10.0);
    assert_eq!(step_nav(NavPhase::Flying, &i), (NavPhase::Flying, vec![]));
    i.region_height_m = CLIMB_OUT_M;
    let (phase, actions) = step_nav(NavPhase::Flying, &i);
    assert!(actions.is_empty());
    assert!(
        matches!(phase, NavPhase::Climbing { centre: Some(c), .. } if c == centre),
        "{phase:?}"
    );
    let (dir, heading) = i.region_fly.unwrap();
    assert_eq!(
        climb_out(phase, &mut i),
        vec![
            NavAction::Leave,
            NavAction::PlaceOnGlobe {
                dir,
                heading,
                altitude_km: DIVE_ALTITUDE_KM,
            },
        ]
    );
}

#[test]
fn backspace_climbs_out() {
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    for height_m in [500.0, 8_000.0, 120_000.0, CLIMB_OUT_M - 1.0] {
        let mut i = NavInputs {
            climb_key: true,
            ..region_fly(centre, height_m)
        };
        let (dir, heading) = i.region_fly.unwrap();
        let (phase, _) = step_nav(NavPhase::Flying, &i);
        assert!(matches!(phase, NavPhase::Climbing { .. }), "{height_m}");
        i.climb_key = false;
        assert_eq!(
            climb_out(phase, &mut i),
            vec![
                NavAction::Leave,
                NavAction::PlaceOnGlobe {
                    dir,
                    heading,
                    altitude_km: DIVE_ALTITUDE_KM,
                },
            ],
            "{height_m}"
        );
    }
    // While emerging into a region, and while a snapshot change waits, too.
    for phase in [
        NavPhase::Emerging {
            world: World::Region,
            elapsed_s: 0.5,
        },
        NavPhase::ChangingSnapshot {
            fade: Fade::new(SNAPSHOT_FADE_S),
            to: YearsBp(100e6),
            requested: RenderDir(DVec3::X),
            agl_m: 100.0,
            generated: false,
            placed: false,
        },
    ] {
        let mut i = NavInputs {
            climb_key: true,
            ..region_fly(centre, 250_000.0)
        };
        i.shown_density = phase.cloud_density(&i);
        let (dir, heading) = i.region_fly.unwrap();
        assert_eq!(
            climb_out(phase, &mut i),
            vec![
                NavAction::Leave,
                NavAction::PlaceOnGlobe {
                    dir,
                    heading,
                    altitude_km: DIVE_ALTITUDE_KM,
                },
            ],
            "{phase:?}"
        );
    }
}

/// Fix round 1, important 1: climbing out of a snapshot change held at full cover (on
/// `Backspace`, or the time moving on to a missing crust), or from a fly in the cloud base's
/// band, starts the gather where the cloud already is: it never thins on the way to the swap.
#[test]
fn climbing_out_never_thins_the_cloud() {
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    let changing = |fade: Fade| NavPhase::ChangingSnapshot {
        fade,
        to: YearsBp(100e6),
        requested: RenderDir(DVec3::X),
        agl_m: 500.0,
        generated: false,
        placed: false,
    };
    let held = changing(Fade::new(SNAPSHOT_FADE_S).advance(2.0, true));
    let closing = changing(Fade::new(SNAPSHOT_FADE_S).advance(0.3, true));
    let missing = TimeChange {
        to: YearsBp(250e6),
        carried: None,
        agl_m: 500.0,
    };
    for (what, phase, height, key, change) in [
        ("held, Backspace", held, 8_000.0, true, None),
        ("held, missing crust", held, 8_000.0, false, Some(missing)),
        ("closing, Backspace", closing, 8_000.0, true, None),
        (
            "in the band, Backspace",
            NavPhase::Flying,
            280_000.0,
            true,
            None,
        ),
    ] {
        let mut i = NavInputs {
            climb_key: key,
            time_change: change,
            region_snapshot: Some(YearsBp(0.0)),
            ..region_fly(centre, height)
        };
        i.shown_density = phase.cloud_density(&i);
        let (next, _, before, after) = frame(phase, &mut i);
        assert!(
            matches!(next, NavPhase::Climbing { .. }),
            "{what}: {next:?}"
        );
        assert!(after >= before, "{what}: {before} -> {after}");
        (i.climb_key, i.time_change) = (false, None);
        climb_out(next, &mut i);
    }
}

/// Fix round 2: the climb out rises at the M2 speed for the fly's height above the ground
/// drawn, so over a 4,400 m plateau it climbs as it would 500 m over the sea, not ten times
/// faster.
#[test]
fn the_climb_rises_by_the_height_above_the_ground() {
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    let climbing = NavPhase::Climbing {
        elapsed_s: 0.0,
        centre: Some(centre),
    };
    let speed = |ground_m: f32| {
        let i = NavInputs {
            region_height_m: ground_m + 500.0,
            region_ground_m: ground_m,
            ..region_fly(centre, 0.0)
        };
        ease(&step_nav(climbing, &i).1).expect("climbing")
    };
    assert_eq!(speed(0.0), fly_speed(500.0));
    assert_eq!(speed(4_400.0), speed(0.0));
}

/// Fix round 1, minor 1: the dive swaps only after a frame drawn opaque. A boosted descent that
/// crosses the dive altitude in one frame (from 1,264 km, drawn at 0.993) waits a frame.
#[test]
fn a_boosted_crossing_waits_for_a_frame_drawn_opaque() {
    let target = everest();
    let mut i = ready(
        target,
        NavInputs {
            globe_altitude_km: 1_264.0,
            ..inputs()
        },
    );
    let phase = diving(target);
    i.shown_density = phase.cloud_density(&i);
    assert!(
        (0.99..1.0).contains(&i.shown_density),
        "{}",
        i.shown_density
    );
    i.globe_altitude_km = 1_190.0;
    let (next, actions, _, _) = frame(phase, &mut i);
    assert!(!actions.iter().any(is_swap), "{actions:?}");
    assert!(matches!(next, NavPhase::Diving { .. }));
    let (next, actions, before, after) = frame(next, &mut i);
    assert!(
        matches!(actions[..], [NavAction::PlaceInRegion { .. }]),
        "{actions:?}"
    );
    assert_eq!((before, after), (1.0, 1.0));
    assert!(matches!(next, NavPhase::Emerging { .. }));
}

/// Fix round 1, minor 5: a climb with no region and no fly (it cannot happen while flying)
/// leaves the globe's fly where it is: no placement, nothing from a zero direction.
#[test]
fn a_climb_without_a_region_places_nothing() {
    let phase = NavPhase::Climbing {
        elapsed_s: CLOUD_GATHER_S as f32,
        centre: None,
    };
    let i = NavInputs {
        region_state: RegionState::InRegion,
        ..inputs()
    };
    let (next, actions) = step_nav(phase, &i);
    assert_eq!(actions, vec![NavAction::Leave]);
    assert!(matches!(next, NavPhase::Emerging { .. }));
    // With a centre but no fly: over the centre, heading north.
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    let (_, actions) = step_nav(
        NavPhase::Climbing {
            elapsed_s: CLOUD_GATHER_S as f32,
            centre: Some(centre),
        },
        &i,
    );
    let north = RegionFrame { center: centre }.north_east().0;
    assert_eq!(
        actions,
        vec![
            NavAction::Leave,
            NavAction::PlaceOnGlobe {
                dir: centre,
                heading: north,
                altitude_km: DIVE_ALTITUDE_KM,
            },
        ]
    );
}

#[test]
fn a_snapshot_with_no_crust_climbs_out() {
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    let mut i = NavInputs {
        time_change: Some(TimeChange {
            to: YearsBp(250e6),
            carried: None,
            agl_m: 500.0,
        }),
        ..region_fly(centre, 8_000.0)
    };
    let (phase, actions) = step_nav(NavPhase::Flying, &i);
    assert!(actions.is_empty(), "{actions:?}");
    assert!(matches!(phase, NavPhase::Climbing { .. }), "{phase:?}");
    i.time_change = None;
    let (dir, heading) = i.region_fly.unwrap();
    assert_eq!(
        climb_out(phase, &mut i),
        vec![
            NavAction::Leave,
            NavAction::PlaceOnGlobe {
                dir,
                heading,
                altitude_km: DIVE_ALTITUDE_KM,
            },
        ]
    );
    // And when the time moves again during a snapshot change, to a missing crust.
    let changing = NavPhase::ChangingSnapshot {
        fade: Fade::new(SNAPSHOT_FADE_S).advance(0.2, true),
        to: YearsBp(100e6),
        requested: RenderDir(DVec3::X),
        agl_m: 100.0,
        generated: false,
        placed: false,
    };
    let again = NavInputs {
        region_snapshot: Some(YearsBp(0.0)),
        time_change: Some(TimeChange {
            to: YearsBp(250e6),
            carried: None,
            agl_m: 100.0,
        }),
        ..region_fly(centre, 8_000.0)
    };
    assert!(matches!(
        step_nav(changing, &again).0,
        NavPhase::Climbing { .. }
    ));
}

#[test]
fn the_fly_is_carried_onto_the_globe_with_its_heading() {
    // At 0° N, 0° E: north is toward 1° N, east toward 1° E (independent of the frame's basis).
    let centre = RenderDir(geo_to_render(latlon_to_geo(0.0, 0.0)).0.normalize());
    let frame = RegionFrame { center: centre };
    let toward = |lat: f64, lon: f64| {
        let d = geo_to_render(latlon_to_geo(lat, lon)).0.normalize();
        (d - centre.0 * centre.0.dot(d)).normalize()
    };
    let (north, east) = (toward(1.0, 0.0), toward(0.0, 1.0));
    for (yaw, want) in [
        (0.0f32, north),
        (std::f32::consts::FRAC_PI_2, -east),
        (-std::f32::consts::FRAC_PI_2, east),
        (std::f32::consts::PI, -north),
    ] {
        let (dir, heading) = fly_on_globe(&frame, 0.0, 0.0, yaw);
        assert!(close(dir, centre.0), "{dir:?}");
        assert!(
            heading.dot(want) > 1.0 - 1e-9,
            "yaw {yaw}: {heading} vs {want}"
        );
        // And back: the dive's yaw from that heading.
        let turn = (region_yaw(centre, heading) - yaw).rem_euclid(std::f32::consts::TAU);
        assert!(turn.min(std::f32::consts::TAU - turn) < 1e-5, "yaw {yaw}");
    }
    // Off the centre: over its own place, the heading a unit tangent there that leads to the
    // place a kilometre ahead in the region.
    let (x, z, yaw) = (60_000.0, -110_000.0, 0.4f32);
    let (dir, heading) = fly_on_globe(&frame, x, z, yaw);
    assert!(close(dir, frame.local_to_dir(x, z).0));
    assert!((heading.length() - 1.0).abs() < 1e-12 && heading.dot(dir.0).abs() < 1e-12);
    let (s, c) = f64::from(yaw).sin_cos();
    let ahead = frame.local_to_dir(x - 1_000.0 * s, z - 1_000.0 * c).0;
    assert!(heading.dot((ahead - dir.0).normalize()) > 1.0 - 1e-6);
}

#[test]
fn no_swap_before_the_cloud_is_opaque() {
    // Every swap — into a region, onto the globe, to a new snapshot — happens on a frame where
    // the cloud is exactly opaque, and the frame before it was too: across frame times that do
    // and do not divide the transitions' durations.
    let target = everest();
    let mut swaps = 0;
    let mut check = |phase: NavPhase,
                     i: &mut NavInputs,
                     frames: usize,
                     script: &dyn Fn(usize, &mut NavInputs)| {
        let mut phase = phase;
        for k in 0..frames {
            script(k, i);
            let (next, actions, before, after) = frame(phase, i);
            if actions.iter().any(is_swap) {
                assert_eq!(
                    (before, after),
                    (1.0, 1.0),
                    "dt {}, frame {k}: {phase:?} -> {next:?}, {actions:?}",
                    i.dt_s
                );
                swaps += 1;
            }
            phase = next;
            i.load_s += f64::from(i.dt_s);
        }
        phase
    };
    for dt in [1.0 / 60.0, 1.0 / 144.0, 0.0123, 0.1, 0.37, 0.5] {
        // The dive: generating for a while, then ready while the player has climbed back above
        // the dive altitude (the cloud thinner), then descending into it again.
        let mut i = NavInputs {
            dt_s: dt,
            region_state: RegionState::Generating,
            globe_altitude_km: DIVE_ALTITUDE_KM - 1.0,
            globe_dir: target,
            time: YearsBp(21_000.0),
            ..inputs()
        };
        let end = check(diving(target), &mut i, 400, &|k, i| {
            if k == 20 {
                *i = ready(target, *i);
            }
            if (20..23).contains(&k) {
                i.globe_altitude_km = DIVE_ALTITUDE_KM + 300.0;
            }
        });
        assert_eq!(end, NavPhase::Flying, "dt {dt}");
        // The climb through the base and the climb on Backspace.
        let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
        let mut i = NavInputs {
            dt_s: dt,
            ..region_fly(centre, CLIMB_OUT_M)
        };
        assert_eq!(
            check(NavPhase::Flying, &mut i, 400, &|_, _| {}),
            NavPhase::Globe,
            "dt {dt}"
        );
        let mut i = NavInputs {
            dt_s: dt,
            ..region_fly(centre, 900.0)
        };
        assert_eq!(
            check(NavPhase::Flying, &mut i, 400, &|k, i| i.climb_key = k == 0),
            NavPhase::Globe,
            "dt {dt}"
        );
        // A snapshot change, its region ready before the midpoint or long after.
        for ready_at in [0usize, 3, 40] {
            let carried = RenderDir(DVec3::X);
            let mut i = NavInputs {
                dt_s: dt,
                region_state: RegionState::InRegion,
                region_centre: Some(centre),
                region_snapshot: Some(YearsBp(0.0)),
                ..inputs()
            };
            let changing = NavPhase::ChangingSnapshot {
                fade: Fade::new(SNAPSHOT_FADE_S),
                to: YearsBp(100e6),
                requested: carried,
                agl_m: 500.0,
                generated: false,
                placed: false,
            };
            let end = check(changing, &mut i, 400, &|k, i| {
                if k == ready_at {
                    i.region_centre = Some(carried);
                    i.region_snapshot = Some(YearsBp(100e6));
                    i.generated = Some((carried, YearsBp(100e6)));
                } else {
                    i.generated = None;
                }
            });
            assert_eq!(end, NavPhase::Flying, "dt {dt}, ready at {ready_at}");
        }
    }
    assert_eq!(swaps, 6 * 6, "every scenario swapped exactly once");
}

/// Task 5 review: a fade that stopped a hair short of its midpoint (within the old 1e-4 of
/// progress) drew the cloud at 0.99999988, and the views swapped there. Now only an exactly
/// opaque cloud swaps.
#[test]
fn a_fade_a_hair_short_of_its_midpoint_does_not_swap() {
    let carried = RenderDir(DVec3::X);
    let ready = NavInputs {
        region_state: RegionState::InRegion,
        region_centre: Some(carried),
        region_snapshot: Some(YearsBp(100e6)),
        generated: Some((carried, YearsBp(100e6))),
        dt_s: 1e-5,
        ..inputs()
    };
    let phase = NavPhase::ChangingSnapshot {
        fade: Fade {
            elapsed_s: 0.5 - 8e-5,
            duration_s: 1.0,
        },
        to: YearsBp(100e6),
        requested: carried,
        agl_m: 500.0,
        generated: true,
        placed: false,
    };
    let (next, actions) = step_nav(phase, &ready);
    let NavPhase::ChangingSnapshot { fade, placed, .. } = next else {
        panic!("{next:?}");
    };
    assert!(fade.alpha() < 1.0, "{}", fade.alpha());
    assert!(
        actions.is_empty() && !placed,
        "swapped at density {}",
        fade.alpha()
    );
    // The next frame reaches the midpoint exactly, drawn opaque; the one after swaps.
    let on = NavInputs { dt_s: 0.1, ..ready };
    let (next, actions) = step_nav(next, &on);
    let NavPhase::ChangingSnapshot { fade, .. } = next else {
        panic!("{next:?}");
    };
    assert!(actions.is_empty(), "{actions:?}");
    assert!(fade.opaque());
    let (next, actions) = step_nav(next, &on);
    let NavPhase::ChangingSnapshot { fade, placed, .. } = next else {
        panic!("{next:?}");
    };
    assert_eq!(actions, vec![NavAction::PlaceAfterChange { agl_m: 500.0 }]);
    assert!(placed && fade.opaque());
}

/// A region entered without a dive (the launch argument, the bench) starts at the start pose
/// with the cloud opaque, which clears as it would after a dive.
#[test]
fn a_launch_region_starts_at_the_start_pose_and_clears() {
    let mut i = NavInputs {
        entered: true,
        region_state: RegionState::InRegion,
        region_centre: Some(RenderDir(DVec3::Z)),
        ..inputs()
    };
    let (mut phase, actions, _, after) = frame(NavPhase::Globe, &mut i);
    assert_eq!(actions, vec![NavAction::PlaceAtStart]);
    assert_eq!(after, 1.0);
    assert!(phase.region_shown());
    i.entered = false;
    let height = i.region_height_m;
    for _ in 0..20 {
        phase = frame(phase, &mut i).0;
    }
    assert_eq!(phase, NavPhase::Flying);
    assert_eq!(i.region_height_m, height, "the start pose is kept");
    assert_eq!(phase.cloud_density(&i), 0.0);
}

#[test]
fn slider_changes_wait_for_the_transition() {
    // Only the dive, the climb and the emergence hold the slider.
    let centre = RenderDir(DVec3::Z);
    for (phase, holds) in [
        (NavPhase::Globe, false),
        (NavPhase::Flying, false),
        (diving(centre), true),
        (
            NavPhase::Emerging {
                world: World::Region,
                elapsed_s: 0.0,
            },
            true,
        ),
        (
            NavPhase::Emerging {
                world: World::Globe,
                elapsed_s: 1.0,
            },
            true,
        ),
        (
            NavPhase::Climbing {
                elapsed_s: 0.1,
                centre: Some(centre),
            },
            true,
        ),
        (
            NavPhase::ChangingSnapshot {
                fade: Fade::new(SNAPSHOT_FADE_S),
                to: YearsBp(1e3),
                requested: centre,
                agl_m: 1.0,
                generated: false,
                placed: false,
            },
            false,
        ),
    ] {
        assert_eq!(phase.holds_time(), holds, "{phase:?}");
    }
    let (start, a, b) = (YearsBp(21_000.0), YearsBp(66e6), YearsBp(100e6));
    // Entering a transition holds the time it starts at.
    let (t, hold) = hold_slider(true, start, SliderHold::default());
    assert_eq!((t, hold.held, hold.queued), (start, Some(start), None));
    // The slider moves: put back, and queued; it moves again: the latest waits.
    let (t, hold) = hold_slider(true, a, hold);
    assert_eq!((t, hold.queued), (start, Some(a)));
    let (t, hold) = hold_slider(true, b, hold);
    assert_eq!((t, hold.queued), (start, Some(b)));
    let (t, hold) = hold_slider(true, start, hold);
    assert_eq!((t, hold.queued), (start, Some(b)));
    // The transition ends: applied, once.
    let (t, hold) = hold_slider(false, start, hold);
    assert_eq!((t, hold), (b, SliderHold::default()));
    assert_eq!(hold_slider(false, t, hold), (b, SliderHold::default()));
    // Nothing moved: nothing applied.
    let (_, hold) = hold_slider(true, start, SliderHold::default());
    assert_eq!(hold_slider(false, start, hold).0, start);
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
            purpose: RequestPurpose::Snapshot,
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
    assert_eq!(phase.cloud_density(&generating), 1.0, "held covered");
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

// ---------------------------------------------------------------------------------------------
// Headless apps

const STEP: Duration = Duration::from_millis(100);

#[derive(Resource, Default)]
struct Seen {
    requests: Vec<RegionRequested>,
    left: usize,
    cancelled: usize,
    swaps: usize,
}

fn record(
    mut seen: ResMut<Seen>,
    mut requests: MessageReader<RegionRequested>,
    mut left: MessageReader<RegionLeft>,
    mut cancelled: MessageReader<crate::region::RegionPreloadCancelled>,
    mut swaps: MessageReader<crate::region::RegionSwapRequested>,
) {
    seen.requests.extend(requests.read().copied());
    seen.left += left.read().count();
    seen.cancelled += cancelled.read().count();
    seen.swaps += swaps.read().count();
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
    Field::dry(
        res,
        size_m,
        vec![h; (res * res) as usize],
        vec![[0, 0, 0, 0]; (res * res) as usize],
    )
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
        boulders: Vec::new(),
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
            .add_domain_event::<RegionGenerated>()
            .add_domain_event::<crate::region::RegionPreloadCancelled>()
            .add_domain_event::<crate::region::RegionSwapRequested>()
            .add_domain_event::<crate::region::RegionPreloadFailed>()
            .init_resource::<crate::region::PendingRegion>()
            .add_systems(
                Update,
                crate::region::job::swap_region.in_set(crate::region::RegionSwapSystems),
            );
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

/// What the region plugin does when a region is ready without a dive (a launch region): insert
/// it, send `RegionEntered` and enter `InRegion`. Then runs until the player is flying.
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
    app.world_mut().write_message(RegionEntered {
        spec: grid.spec,
        fallback: false,
    });
    for _ in 0..40 {
        app.update();
        let nav = phase(app);
        let visible = app.world().resource::<TerrainSettings>().visible;
        assert_eq!(visible, nav.region_shown(), "{nav:?}");
        if nav == NavPhase::Flying {
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

fn years(app: &App) -> YearsBp {
    app.world().resource::<GlobeTime>().years
}

/// Runs until `done`, at most `frames` updates.
fn run_until(app: &mut App, frames: usize, done: impl Fn(&App) -> bool) {
    for _ in 0..frames {
        if done(app) {
            return;
        }
        app.update();
    }
    assert!(done(app), "still {:?}", phase(app));
}

/// One frame stalled past [`START_TIMEOUT_S`] (a hidden tab, a host pausing
/// `requestAnimationFrame`) does not cancel a dive; running time without a start does, and the
/// fly climbs back out of the cloud.
#[test]
fn a_frame_stall_does_not_cancel_a_dive() {
    let mut app = nav_app(false);
    app.world_mut()
        .resource_mut::<GlobeFlightState>()
        .0
        .altitude_km = DIVE_ALTITUDE_KM - 100.0;
    let diving = NavPhase::Diving {
        target: RenderDir(ON_PLATE),
        snapshot: YearsBp(0.0),
        since_s: 0.0,
        started: false,
    };
    app.world_mut().resource_mut::<Navigation>().phase = diving;
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(40)));
    app.update();
    assert!(
        matches!(phase(&app), NavPhase::Diving { started: false, .. }),
        "a 40 s stall: {:?}",
        phase(&app)
    );
    app.insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
    let frames = (START_TIMEOUT_S / STEP.as_secs_f64()) as usize + 4;
    for _ in 0..frames {
        app.update();
    }
    assert!(
        matches!(
            phase(&app),
            NavPhase::Emerging {
                world: World::Globe,
                ..
            }
        ),
        "{START_TIMEOUT_S} s without a start: {:?}",
        phase(&app)
    );
    assert_eq!(
        app.world().resource::<Seen>().left,
        1,
        "the request dropped"
    );
    run_until(&mut app, 40, |a| phase(a) == NavPhase::Globe);
}

#[test]
fn snapshot_change_to_missing_crust_climbs_out() {
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
        matches!(phase(&app), NavPhase::Climbing { .. }),
        "{:?}",
        phase(&app)
    );
    run_until(&mut app, 40, |a| phase(a) == NavPhase::Globe);
    assert_eq!(region_state(&app), RegionState::Globe);
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.left, 1, "RegionLeft once");
    assert!(
        seen.requests.is_empty(),
        "no region at 250 Ma: {:?}",
        seen.requests
    );
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(!app.world().resource::<TerrainSettings>().visible);
    // Over the fly's place (the centre), climbed out to the cloud top, at the new time.
    let flight = app.world().resource::<GlobeFlightState>().0;
    assert!((flight.dir - ON_PLATE).length() < 1e-5, "{flight:?}");
    assert!(
        (flight.altitude_km - CLOUD_TOP_KM).abs() < 1.0,
        "{flight:?}"
    );
    assert_eq!(years(&app), YearsBp(250e6));
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
    let alpha = app.world().resource::<Navigation>().density;
    assert_eq!(alpha, 1.0);
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
        frames_fetched: 0,
        fallback: false,
        hydro_wait_ms: 0,
        hydrology: false,
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
fn backspace_climbs_out_over_the_fly() {
    let mut app = nav_app(true);
    let centre = RenderDir(DVec3::new(0.6, 0.0, 0.8));
    enter(&mut app, &grid(centre, YearsBp(0.0), 0.0));
    // 20 km east, 35 km north, turned 0.7 rad west of north.
    {
        let mut fly = app.world_mut().resource_mut::<FlyState>();
        fly.pos_m = Vec3::new(20_000.0, 8_000.0, -35_000.0);
        fly.yaw = 0.7;
    }
    let (dir, heading) = fly_on_globe(&RegionFrame { center: centre }, 20_000.0, -35_000.0, 0.7);
    press(&mut app, LEAVE_KEY);
    assert!(matches!(phase(&app), NavPhase::Climbing { .. }));
    run_until(&mut app, 40, |a| phase(a) == NavPhase::Globe);
    assert_eq!(region_state(&app), RegionState::Globe);
    assert_eq!(app.world().resource::<Seen>().left, 1);
    let flight = app.world().resource::<GlobeFlightState>().0;
    assert!((flight.dir - dir.0).length() < 1e-5, "{flight:?}");
    assert!(flight.heading.dot(heading) > 1.0 - 1e-6, "{flight:?}");
    assert!(
        (flight.altitude_km - CLOUD_TOP_KM).abs() < 1.0,
        "{flight:?}"
    );
}

/// A dive from the globe's fly into a region the stand-in job generates, with a slider change
/// in the dive and another while emerging: the time stays at the dive's snapshot throughout,
/// then the last change applies once, as a snapshot change around the fly.
#[test]
fn a_slider_change_during_a_dive_waits_for_flying() {
    let mut app = nav_app(false);
    app.insert_resource(FakeJob {
        after: 5,
        ground_m: 500.0,
        pending: None,
    })
    .add_systems(Update, fake_job.in_set(crate::region::RegionSystems));
    set_time(&mut app, 30_000.0);
    let north = DVec3::Y;
    app.world_mut().resource_mut::<GlobeFlightState>().0 = GlobeFlight {
        dir: ON_PLATE,
        heading: north,
        altitude_km: DIVE_ALTITUDE_KM - 1.0,
    };
    app.update();
    assert!(
        matches!(phase(&app), NavPhase::Diving { .. }),
        "{:?}",
        phase(&app)
    );
    let snapshot = YearsBp(21_000.0);
    assert_eq!(years(&app), snapshot);
    set_time(&mut app, 66e6);
    app.update();
    assert_eq!(years(&app), snapshot, "held in the dive");
    assert_eq!(
        app.world().resource::<Navigation>().queued_time(),
        Some(YearsBp(66e6))
    );
    run_until(&mut app, 20, |a| {
        matches!(phase(a), NavPhase::Emerging { .. })
    });
    set_time(&mut app, 100e6);
    let mut flying = false;
    for _ in 0..40 {
        app.update();
        if phase(&app).holds_time() {
            assert_eq!(years(&app), snapshot, "{:?}", phase(&app));
        } else {
            flying = true;
            break;
        }
    }
    assert!(flying, "{:?}", phase(&app));
    assert_eq!(phase(&app), NavPhase::Flying);
    assert_eq!(years(&app), YearsBp(100e6), "applied on return to flying");
    assert_eq!(app.world().resource::<Navigation>().queued_time(), None);
    app.update();
    assert!(matches!(phase(&app), NavPhase::ChangingSnapshot { .. }));
    let requests = app.world().resource::<Seen>().requests.clone();
    let snapshots: Vec<_> = requests.iter().map(|r| r.snapshot).collect();
    assert_eq!(snapshots, vec![snapshot, YearsBp(100e6)], "once");
}

/// A slider change while climbing out and another while emerging over the globe: the region's
/// snapshot holds until the globe, then the last applies, once.
#[test]
fn a_slider_change_during_a_climb_waits_for_the_globe() {
    let mut app = nav_app(true);
    enter(&mut app, &grid(RenderDir(ON_PLATE), YearsBp(0.0), 0.0));
    press(&mut app, LEAVE_KEY);
    assert!(matches!(phase(&app), NavPhase::Climbing { .. }));
    set_time(&mut app, 66e6);
    app.update();
    assert_eq!(years(&app), YearsBp(0.0), "held while climbing");
    run_until(&mut app, 20, |a| {
        matches!(
            phase(a),
            NavPhase::Emerging {
                world: World::Globe,
                ..
            }
        )
    });
    set_time(&mut app, 100e6);
    app.update();
    assert_eq!(years(&app), YearsBp(0.0), "held while emerging");
    run_until(&mut app, 40, |a| phase(a) == NavPhase::Globe);
    assert_eq!(years(&app), YearsBp(100e6));
    app.update();
    assert_eq!(years(&app), YearsBp(100e6));
    assert!(app.world().resource::<Seen>().requests.is_empty());
}

/// Design §6: a snapshot change happens behind the cloud, which is opaque (density exactly 1)
/// on the frame the views swap and clear again once flying; M2's haze overlay is gone. Whether
/// the region is ready before the midpoint or only long after.
#[test]
fn the_snapshot_change_uses_the_cloud() {
    for after in [0, 12] {
        let mut app = nav_app(false);
        app.insert_resource(FakeJob {
            after,
            ground_m: 900.0,
            pending: None,
        })
        .add_systems(Update, fake_job.in_set(crate::region::RegionSystems));
        enter(&mut app, &grid(RenderDir(ON_PLATE), YearsBp(0.0), 500.0));
        {
            let world = app.world_mut();
            let overlays = world
                .query::<&Name>()
                .iter(world)
                .filter(|n| n.as_str() == "Fade overlay")
                .count();
            assert_eq!(overlays, 0, "no haze overlay");
        }
        assert_eq!(app.world().resource::<CloudDensity>().0, 0.0);

        set_time(&mut app, 100e6);
        let placed = |p: NavPhase| matches!(p, NavPhase::ChangingSnapshot { placed: true, .. });
        let mut swap = None;
        let mut peak: f32 = 0.0;
        let mut flying_again = false;
        for frame in 0..60 {
            let before = phase(&app);
            app.update();
            let now = phase(&app);
            let density = app.world().resource::<CloudDensity>().0;
            peak = peak.max(density);
            if placed(now) && !placed(before) {
                swap = Some((frame, density));
            }
            if swap.is_some() && now == NavPhase::Flying {
                assert_eq!(density, 0.0, "after {after}: clear once flying");
                flying_again = true;
                break;
            }
        }
        let (frame, at_swap) = swap.unwrap_or_else(|| panic!("after {after}: never swapped"));
        assert_eq!(at_swap, 1.0, "after {after}: the swap at frame {frame}");
        assert_eq!(peak, 1.0, "after {after}");
        assert!(flying_again, "after {after}: {:?}", phase(&app));
    }
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
    // Neighbours are `FakeNeighbours`' (the region job keeps them apart too).
    if let Some(&r) = requests
        .read()
        .filter(|r| r.purpose != RequestPurpose::Neighbour)
        .last()
    {
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
        frames_fetched: 0,
        fallback: false,
        hydro_wait_ms: 0,
        hydrology: false,
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
    let ground = blended_surface_m(&scene.grid, f64::from(p.x), f64::from(p.z));
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

/// One frame's terrain fields as drawn: whether the heights and the water textures are the
/// scene grid's (read back from the images' first texel), and the cloud.
#[derive(Copy, Clone, Debug)]
struct FieldsDrawn {
    density: f32,
    pending: bool,
    heights_current: bool,
    water_current: bool,
    centre: RenderDir,
}

#[derive(Resource, Default)]
struct FieldFrames(Vec<FieldsDrawn>);

fn record_fields(
    scene: Option<Res<crate::terrain::TerrainScene>>,
    images: Res<Assets<Image>>,
    density: Res<CloudDensity>,
    mut frames: ResMut<FieldFrames>,
) {
    let Some(scene) = scene else { return };
    // `None` where no texture is bound (a staged replace's swap frame has no water).
    let first = |h: &Handle<Image>| {
        let data = images.get(h).and_then(|i| i.data.as_ref())?;
        Some(f32::from_le_bytes(data[..4].try_into().unwrap()))
    };
    let g = &scene.grid;
    frames.0.push(FieldsDrawn {
        density: density.0,
        pending: scene.fields_pending(),
        heights_current: first(&scene.inner_height) == Some(g.inner.elevation_m[0])
            && first(&scene.apron_height) == Some(g.apron.elevation_m[0]),
        water_current: first(&scene.inner_water) == Some(g.inner.water_level_m[0])
            && first(&scene.apron_water) == Some(g.apron.water_level_m[0]),
        centre: g.spec.center,
    });
}

/// The hop's memory (third stage), for any swap: every frame whose fields are incomplete (no
/// water bound) or mixed (another grid's heights) is drawn at density exactly 1. Per replace
/// there are `1 + REPLACE_EXTRA_HELD_FRAMES` of them in a row: the swap frame, with no water,
/// then the new water over the old heights. `replaces` is how many the frames hold.
fn assert_mixed_fields_only_under_the_cloud(frames: &[FieldsDrawn], replaces: usize) {
    use crate::terrain::REPLACE_EXTRA_HELD_FRAMES;
    let held = 1 + REPLACE_EXTRA_HELD_FRAMES as usize;
    let mixed: Vec<usize> = (0..frames.len())
        .filter(|&k| !(frames[k].heights_current && frames[k].water_current))
        .collect();
    for &k in &mixed {
        let f = frames[k];
        assert_eq!(f.density, 1.0, "frame {k} draws incomplete fields: {f:?}");
        assert!(f.pending && !f.heights_current, "frame {k}: {f:?}");
    }
    assert!(
        frames
            .iter()
            .all(|f| f.pending == !(f.heights_current && f.water_current))
    );
    assert_eq!(
        mixed.len(),
        replaces * held,
        "{held} staged frames per replace: {mixed:?}"
    );
    for run in mixed.chunks(held) {
        assert!(
            run.windows(2).all(|w| w[1] == w[0] + 1),
            "not in a row: {mixed:?}"
        );
        assert!(
            !frames[run[0]].water_current,
            "the swap frame binds no water"
        );
        assert!(frames[run[held - 1]].water_current, "then the new water");
        assert!(
            frames[run[held - 1] + 1].heights_current,
            "complete after {held} frames"
        );
    }
}

/// The hop's memory (third stage): a snapshot change's staged replace draws incomplete or mixed
/// fields only at density exactly 1, whether the region is ready early or late.
#[test]
fn a_snapshot_change_replaces_the_fields_under_the_opaque_cloud() {
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
                ground_m: 2_150.0,
                pending: None,
            })
            .init_resource::<FieldFrames>()
            .add_systems(Update, fake_job.in_set(crate::region::RegionSystems))
            .add_systems(Last, record_fields);
        crate::terrain::add_terrain_systems(&mut app);
        app.world_mut().spawn((
            crate::globe::GlobeCamera,
            Camera::default(),
            IsDefaultUiCamera,
        ));
        enter(&mut app, &grid(RenderDir(ON_PLATE), YearsBp(0.0), 288.0));
        app.world_mut().resource_mut::<FlyState>().pos_m = Vec3::new(0.0, 562.0, -5_000.0);
        app.update();
        set_time(&mut app, 100e6);
        for _ in 0..40 {
            app.update();
        }
        assert_eq!(phase(&app), NavPhase::Flying, "after {after}");
        let frames = &app.world().resource::<FieldFrames>().0;
        assert!(frames.last().unwrap().heights_current);
        assert_mixed_fields_only_under_the_cloud(frames, 1);
    }
}

// ---------------------------------------------------------------------------------------------
// The dive and the climb through both worlds' cameras

/// One frame as drawn: the phase, the cloud, the active camera and the fly.
#[derive(Clone, Debug)]
struct Drawn {
    phase: NavPhase,
    density: f32,
    region_camera: bool,
    eye: Transform,
    fov: f32,
    near: f32,
    pose: crate::creature::FlyPose,
    fly: FlyState,
    flight: GlobeFlight,
    look_offset_deg: f64,
    /// The centre of the region the terrain scene draws.
    scene_centre: Option<RenderDir>,
}

#[derive(Resource, Default)]
struct DrawnFrames(Vec<Drawn>);

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn record_drawn(
    nav: Res<Navigation>,
    density: Res<CloudDensity>,
    pose: Res<crate::creature::FlyPose>,
    fly: Res<FlyState>,
    flight: Res<GlobeFlightState>,
    look: Res<GlobeLook>,
    scene: Option<Res<crate::terrain::TerrainScene>>,
    cameras: Query<(
        &Camera,
        &Transform,
        Option<&Projection>,
        Has<crate::terrain::view::RegionCamera>,
    )>,
    mut frames: ResMut<DrawnFrames>,
) {
    let active: Vec<_> = cameras.iter().filter(|c| c.0.is_active).collect();
    assert_eq!(active.len(), 1, "exactly one world draws");
    let (_, eye, projection, region_camera) = active[0];
    let (fov, near) = match projection {
        Some(Projection::Perspective(p)) => (p.fov, p.near),
        _ => panic!("a perspective camera"),
    };
    frames.0.push(Drawn {
        phase: nav.phase,
        density: density.0,
        region_camera,
        eye: *eye,
        fov,
        near,
        pose: *pose,
        fly: *fly,
        flight: flight.0,
        look_offset_deg: look.offset_deg,
        scene_centre: scene.map(|s| s.grid.spec.center),
    });
}

/// Navigation, the stand-in job, the terrain scene's camera switch, the fly camera and globe
/// flight, with a globe camera: both worlds' cameras and the fly's pose, headless.
fn worlds_app() -> App {
    let mut app = nav_app(false);
    app.add_plugins(AssetPlugin::default())
        .init_asset::<Image>()
        .init_asset::<Mesh>()
        .init_asset::<bevy::render::storage::ShaderBuffer>()
        .init_asset::<crate::terrain::material::TerrainMaterial>()
        .init_resource::<ButtonInput<bevy::input::mouse::MouseButton>>()
        .add_plugins((
            crate::fly_camera::FlyCameraPlugin,
            crate::globe_flight::GlobeFlightPlugin,
        ))
        .insert_resource(FakeJob {
            after: 3,
            ground_m: 500.0,
            pending: None,
        })
        .init_resource::<DrawnFrames>()
        .add_systems(Update, fake_job.in_set(crate::region::RegionSystems))
        .add_systems(Last, record_drawn);
    crate::terrain::add_terrain_systems(&mut app);
    app.world_mut().spawn((
        crate::globe::GlobeCamera,
        Camera::default(),
        Projection::Perspective(PerspectiveProjection {
            aspect_ratio: 16.0 / 9.0,
            ..default()
        }),
        Transform::default(),
        IsDefaultUiCamera,
    ));
    app
}

/// The index of the first drawn frame in `phase` after `from`.
fn first(frames: &[Drawn], from: usize, phase: impl Fn(&NavPhase) -> bool) -> usize {
    from + frames[from..]
        .iter()
        .position(|f| phase(&f.phase))
        .unwrap_or_else(|| panic!("never: {:?}", frames.last().map(|f| f.phase)))
}

/// On every frame the fly's pose is the active camera's: its distance matches its drawn size,
/// so the cloud sits just past it ([`crate::cloud::fly_view_depth`]) rather than falling back.
fn assert_pose_follows_the_camera(frames: &[Drawn]) {
    for (k, f) in frames.iter().enumerate().skip(1) {
        let depth = crate::cloud::fly_view_depth(f.eye.translation, &f.pose, f.fov, f.near);
        let fallback = crate::cloud::FALLBACK_NEAR_PLANES * f.near;
        assert!(
            depth != fallback,
            "frame {k} ({:?}, region camera {}): the pose is the other world's",
            f.phase,
            f.region_camera
        );
    }
}

fn assert_transform_near(got: &Transform, want: &Transform, what: &str) {
    let scale = want.translation.length().max(1.0);
    assert!(
        got.translation.distance(want.translation) < 1e-5 * scale,
        "{what}: eye {} vs {}",
        got.translation,
        want.translation
    );
    assert!(
        got.rotation.dot(want.rotation).abs() > 1.0 - 1e-6,
        "{what}: {:?} vs {:?}",
        got.rotation,
        want.rotation
    );
}

/// Dives from the globe into the stand-in job's region and runs until flying; returns the
/// frame index of the swap.
fn dive(app: &mut App) -> usize {
    app.world_mut().resource_mut::<GlobeFlightState>().0 = GlobeFlight {
        dir: ON_PLATE,
        heading: DVec3::Y,
        altitude_km: DIVE_ALTITUDE_KM + 400.0,
    };
    // A stale region pose, which the chase camera glides toward while the region is held.
    *app.world_mut().resource_mut::<FlyState>() = FlyState {
        pos_m: Vec3::new(5_000.0, 9_000.0, -3_000.0),
        yaw: 1.0,
        ..default()
    };
    for _ in 0..5 {
        app.update();
    }
    app.world_mut()
        .resource_mut::<GlobeFlightState>()
        .0
        .altitude_km = DIVE_ALTITUDE_KM - 1.0;
    run_until(app, 60, |a| phase(a) == NavPhase::Flying);
    let frames = &app.world().resource::<DrawnFrames>().0;
    first(frames, 0, |p| {
        matches!(
            p,
            NavPhase::Emerging {
                world: World::Region,
                ..
            }
        )
    })
}

#[test]
fn the_dive_swaps_worlds_under_the_cloud() {
    let mut app = worlds_app();
    let swap = dive(&mut app);
    let frames = app.world().resource::<DrawnFrames>().0.clone();
    let (before, at) = (&frames[swap - 1], &frames[swap]);
    assert!(matches!(before.phase, NavPhase::Diving { .. }));
    // The cloud is opaque on both sides of the swap, and only then does the region draw.
    assert_eq!((before.density, at.density), (1.0, 1.0));
    assert!(!before.region_camera && at.region_camera);
    assert!(frames[..swap].iter().all(|f| !f.region_camera));
    assert!(frames[swap..].iter().all(|f| f.region_camera));
    assert_pose_follows_the_camera(&frames);
    // The fly at the cloud base, heading north as it did over the globe, at the start pitch;
    // the chase camera exactly at its wanted pose (no glide from the stale one).
    assert_eq!(
        at.fly.pos_m,
        Vec3::new(0.0, REGION_CLOUD_BASE_M as f32, 0.0)
    );
    assert!(at.fly.yaw.abs() < 1e-6, "{}", at.fly.yaw);
    assert_eq!(at.fly.pitch, FlyState::default().pitch);
    let want = crate::fly_camera::region_chase_pose(&at.fly, |_, _| 500.0);
    let want =
        Transform::from_translation(want.eye.as_vec3()).looking_at(want.target.as_vec3(), Vec3::Y);
    assert_transform_near(&at.eye, &want, "the region camera on the swap frame");
    // Then the cloud thins as the fly emerges.
    let flying = first(&frames, swap, |p| *p == NavPhase::Flying);
    assert!(
        frames[swap..flying]
            .windows(2)
            .all(|w| w[1].density <= w[0].density)
    );
    assert_eq!(frames[flying].density, 0.0);
}

#[test]
fn the_climb_swaps_worlds_under_the_cloud() {
    let mut app = worlds_app();
    dive(&mut app);
    // A stale drag on the globe view, and the fly elsewhere in the region, low.
    app.world_mut().resource_mut::<GlobeLook>().offset_deg = 20.0;
    {
        let mut fly = app.world_mut().resource_mut::<FlyState>();
        fly.pos_m = Vec3::new(20_000.0, 8_000.0, -35_000.0);
        fly.yaw = 0.7;
    }
    for _ in 0..5 {
        app.update();
    }
    let from = app.world().resource::<DrawnFrames>().0.len();
    press(&mut app, LEAVE_KEY);
    run_until(&mut app, 60, |a| phase(a) == NavPhase::Globe);
    let frames = app.world().resource::<DrawnFrames>().0.clone();
    let swap = first(&frames, from, |p| {
        matches!(
            p,
            NavPhase::Emerging {
                world: World::Globe,
                ..
            }
        )
    });
    let (before, at) = (&frames[swap - 1], &frames[swap]);
    assert!(matches!(before.phase, NavPhase::Climbing { .. }));
    assert_eq!((before.density, at.density), (1.0, 1.0));
    assert!(before.region_camera && !at.region_camera);
    assert!(frames[swap..].iter().all(|f| !f.region_camera));
    assert_pose_follows_the_camera(&frames);
    assert_eq!(
        app.world().resource::<Seen>().left,
        1,
        "RegionLeft at the swap"
    );
    // Over the fly's place at the dive altitude, heading as it did, the drag offset gone; the
    // globe camera exactly at its wanted pose, at the dive's 50° look (55° eye).
    let (dir, heading) = fly_on_globe(
        &RegionFrame {
            center: RenderDir(ON_PLATE),
        },
        20_000.0,
        -35_000.0,
        0.7,
    );
    let f = at.flight;
    assert!((f.dir - dir.0).length() < 1e-9, "{f:?}");
    assert!(f.heading.dot(heading) > 1.0 - 1e-9, "{f:?}");
    assert_eq!(f.altitude_km, DIVE_ALTITUDE_KM);
    assert_eq!(at.look_offset_deg, 0.0);
    let want = timefly_core::flight::chase::globe_chase_pose(&f, 0.0);
    let want = Transform::from_translation(want.eye.as_vec3())
        .looking_at(want.target.as_vec3(), f.up().as_vec3());
    assert_transform_near(&at.eye, &want, "the globe camera on the swap frame");
    let down = -at
        .eye
        .forward()
        .as_vec3()
        .as_dvec3()
        .dot(f.up())
        .asin()
        .to_degrees();
    let look = timefly_core::flight::chase::globe_look_pitch_deg(DIVE_ALTITUDE_KM);
    assert!((look - 50.0).abs() < 1e-9);
    assert!((down - look).abs() < 0.05, "{down}");
    // Then the cloud thins as the fly climbs out.
    let globe = first(&frames, swap, |p| *p == NavPhase::Globe);
    assert!(
        frames[swap..globe]
            .windows(2)
            .all(|w| w[1].density <= w[0].density)
    );
    assert_eq!(frames[globe].density, 0.0);
}

/// At full density the cloud covers only what lies beyond 1.2 × the eye-to-fly distance
/// (`crate::cloud::DEPTH_PAST_FLY`). Across both swaps' camera jumps (the globe chase at the
/// dive, look 50° / eye 55°; the region chase at the cloud base or the climb trigger, look 15°
/// / eye 20°) no ground is that near the eye, even under Everest everywhere and the globe's
/// relief exaggerated; nor over flat ground at any height a `Backspace` climb starts from.
#[test]
fn no_ground_is_nearer_than_the_cloud_across_the_swaps() {
    use crate::cloud::DEPTH_PAST_FLY;
    use crate::fly_camera::region_chase_pose;
    use timefly_core::flight::chase::globe_chase_pose;
    use timefly_core::flight::cloud::globe_exaggeration;
    let everest_m = 8_849.0;
    // The globe side: the eye to the highest (exaggerated) ground is at least its altitude
    // above it.
    let f = GlobeFlight {
        altitude_km: DIVE_ALTITUDE_KM,
        ..GlobeFlight::start(28.0, 86.9)
    };
    let pose = globe_chase_pose(&f, 0.0);
    let reach = DEPTH_PAST_FLY as f64 * (pose.eye - f.position_km()).length();
    let top = EARTH_RADIUS_KM + globe_exaggeration(DIVE_ALTITUDE_KM) * everest_m / 1000.0;
    assert!(
        pose.eye.length() - top > reach,
        "globe: ground {} km away, the cloud at {reach} km",
        pose.eye.length() - top
    );
    // The region side: at the base, at the climb trigger, and from the clearance up.
    let mut heights = vec![REGION_CLOUD_BASE_M as f32, CLIMB_OUT_M];
    heights.extend((0..200).map(|k| CAMERA_CLEARANCE_M + 1_500.0 * k as f32));
    for h in heights {
        let ground = if h >= 100_000.0 { everest_m } else { 0.0 };
        let fly = FlyState {
            pos_m: Vec3::new(
                0.0,
                ground as f32 + h.min(CAMERA_CEILING_M - ground as f32),
                0.0,
            ),
            ..default()
        };
        let pose = region_chase_pose(&fly, |_, _| ground);
        let reach = f64::from(DEPTH_PAST_FLY) * (pose.eye - fly.pos_m.as_dvec3()).length();
        // Flat ground at `ground`: the nearest point is straight below the eye.
        assert!(
            pose.eye.y - ground > reach,
            "{h} m: ground {} m below, the cloud at {reach} m",
            pose.eye.y - ground
        );
    }
}

/// Fix round 1, important 1: `Backspace` during a snapshot change, its region already
/// generated but not placed (`after` 1), or finishing during the climb's gather (3, 5). The
/// terrain never swaps to it, the cloud never thins until the swap to the globe, and the fly
/// lands over its place in the region it was flying in.
#[test]
fn a_snapshot_change_left_mid_way_never_swaps_the_terrain() {
    for after in [1u32, 3, 5] {
        let mut app = worlds_app();
        dive(&mut app);
        let old = app.world().resource::<ActiveRegion>().frame;
        {
            let mut fly = app.world_mut().resource_mut::<FlyState>();
            fly.pos_m = Vec3::new(20_000.0, 8_000.0, -35_000.0);
            fly.yaw = 0.7;
        }
        for _ in 0..3 {
            app.update();
        }
        app.world_mut().resource_mut::<FakeJob>().after = after;
        set_time(&mut app, 100e6);
        app.update();
        assert!(
            matches!(phase(&app), NavPhase::ChangingSnapshot { .. }),
            "{:?}",
            phase(&app)
        );
        app.update();
        app.update();
        let from = app.world().resource::<DrawnFrames>().0.len();
        press(&mut app, LEAVE_KEY);
        assert!(matches!(phase(&app), NavPhase::Climbing { .. }));
        run_until(&mut app, 60, |a| phase(a) == NavPhase::Globe);
        let frames = app.world().resource::<DrawnFrames>().0.clone();
        let swap = first(&frames, from, |p| {
            matches!(
                p,
                NavPhase::Emerging {
                    world: World::Globe,
                    ..
                }
            )
        });
        for (k, f) in frames.iter().enumerate().take(swap).skip(from - 1) {
            assert_eq!(
                f.scene_centre,
                Some(old.center),
                "after {after}, frame {k} ({:?}, density {}): the terrain swapped",
                f.phase,
                f.density
            );
        }
        for w in frames[from - 1..=swap].windows(2) {
            assert!(
                w[1].density >= w[0].density,
                "after {after}: the cloud thinned {} -> {} ({:?})",
                w[0].density,
                w[1].density,
                w[1].phase
            );
        }
        assert_eq!(frames[swap].density, 1.0);
        let (dir, heading) = fly_on_globe(&old, 20_000.0, -35_000.0, 0.7);
        let f = frames[swap].flight;
        assert!((f.dir - dir.0).length() < 1e-9, "after {after}: {f:?}");
        assert!(f.heading.dot(heading) > 1.0 - 1e-9, "after {after}: {f:?}");
    }
}

mod hop;
