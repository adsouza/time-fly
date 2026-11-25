//! The navigation decision (M3 design §6, §7 and §9): [`step_nav`], a pure function of one
//! frame's [`NavInputs`] that returns the next [`NavPhase`] and the [`NavAction`]s for the shell.
//!
//! - **The dive** (§7.1). Flying down over the globe, the cloud gathers below
//!   [`CLOUD_TOP_KM`] ([`globe_density`]). Below [`DIVE_ALTITUDE_KM`] the dive commits once: the
//!   time snaps to the nearest snapshot and the region under the fly is requested
//!   ([`NavPhase::Diving`]). Inside the cloud the fly keeps descending, easing toward
//!   [`GLOBE_MIN_ALTITUDE_KM`] ([`dive_drift_mps`]). When the region is active and the cloud is
//!   exactly opaque, the views swap: the fly appears at the region's cloud base over the dive
//!   point with its heading carried ([`NavAction::PlaceInRegion`]), then
//!   [`NavPhase::Emerging`] eases it down to [`REGION_EMERGE_TO_M`] over [`EMERGE_S`] as the
//!   cloud thins. Climbing back above the cloud top before the swap cancels the dive, as does
//!   flying off the dive point above the opaque cloud ([`DIVE_LEAVE_KM`]); inside it the fly's
//!   horizontal flight is damped to a drift ([`NavPhase::damps_dive_drift`]).
//! - **The climb** (§7.2). Climbing to the cloud base ([`CLIMB_OUT_M`], ruling P3) or pressing
//!   `Backspace` gathers the cloud over [`CLOUD_GATHER_S`] ([`NavPhase::Climbing`]). After a
//!   frame at exactly full density the views swap: `RegionLeft`, and the fly over the globe at the dive altitude
//!   above its place in the region, its heading carried ([`NavAction::PlaceOnGlobe`]). Then
//!   [`NavPhase::Emerging`] climbs it to the cloud top over [`EMERGE_S`].
//! - **Snapshot changes** inside a region keep M2's rules behind a [`Fade`] through the cloud; a
//!   change whose crust does not exist climbs out (§9, M2 §6.2).
//! - **A region lost** from under the fly (a generation error, or the region gone while it is
//!   shown) comes out over the globe above the fly's place in it, at the dive altitude, as a
//!   climb does ([`NavAction::PlaceOnGlobe`] from [`NavInputs::region_fly`]).
//! - **The hop** (§8). Flying out within [`preload_distance_m`](timefly_core::flight::hop::preload_distance_m) of the inner field's edge
//!   ([`should_preload`]) requests the neighbour past it ([`neighbour_centre`],
//!   [`NavAction::Preload`]; [`NavPhase::Hopping`]). Within [`LIFT_LEAD_M`] of the edge the lift
//!   gathers: the cloud bank closes and the fly rises to [`HOP_ALTITUDE_M`] (a fly above that
//!   stays at its height), both full at the edge and both following the fly's distance from it,
//!   so turning back thins them again; the vertical controls are the lift's, and at full cover
//!   the fly slows to a drift ([`HOLD_DRIFT`]) until the neighbour is ready. After a frame drawn
//!   opaque, with the neighbour ready, the views swap ([`NavAction::SwapToNeighbour`]: the fly
//!   carried into the neighbour by `carry_pose`, its height above the ground kept). Then the
//!   cloud clears over [`CLOUD_CLEAR_S`] and the fly eases back to its height above the ground
//!   before the lift ([`NavAction::ReturnToAgl`]), unless the player climbs or descends.
//!   Turning back before the swap clears the cloud and then cancels the preload; turning back
//!   from the hold, past the edge, cancels it at once and brings the fly in to the edge under
//!   the opaque cloud ([`NavAction::PlaceInside`]). A neighbour that fails, or a hold longer
//!   than [`HOLD_MAX_S`], turns the fly round [`TURN_BACK_INSIDE_M`] inside the edge under the
//!   opaque cloud ([`NavAction::TurnBack`], Review Focus 4). A slider change before the lift
//!   ends the hop; during it, it waits.
//! - **The cloud** is the largest of the altitude band of the world on screen
//!   ([`globe_density`] or [`region_density`]) and the phase's own fade
//!   ([`NavPhase::cloud_density`]). Every swap happens on a frame at exactly 1, after a frame
//!   at exactly 1.
//! - **The slider** holds during [`NavPhase::holds_time`] phases; the shell queues the change and
//!   applies it once the transition ends ([`hold_slider`]).

use bevy::math::{DVec2, DVec3, Vec2};
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::altitudes::{
    CLOUD_CLEAR_S, CLOUD_GATHER_S, CLOUD_TOP_KM, DIVE_ALTITUDE_KM, EMERGE_S, GLOBE_MAX_ALTITUDE_KM,
    HOP_ALTITUDE_M, LIFT_LEAD_M, NEIGHBOUR_PAST_EDGE_M, REGION_CLOUD_BASE_M, REGION_EMERGE_TO_M,
};
use timefly_core::flight::cloud::{globe_density, region_density};
use timefly_core::flight::globe::{GLOBE_MIN_ALTITUDE_KM, heading_to_region_yaw};
use timefly_core::flight::hop::{
    EDGE_M, carry_pose, into_field, neighbour_centre, preload_distance_m, should_preload,
};
use timefly_core::geo::RenderDir;
use timefly_core::worldgen::frame::RegionFrame;

use crate::clock::LOAD_CLOCK_MAX_DELTA_S;
use crate::fly_camera::fly_speed;
use crate::region::{RegionRequested, RegionState, RequestPurpose};
use crate::time::nearest_snapshot;

/// The fade across a snapshot change inside a region (design §6.2: about a second).
pub const SNAPSHOT_FADE_S: f32 = 1.0;
/// A request the region plugin never starts on (it ignores requests it cannot serve, with a
/// `warn`) ends the dive after this long, on the `LoadClock`: a hidden tab or paused page does
/// not cancel it.
pub const START_TIMEOUT_S: f64 = 2.0;
/// The climb out of a region triggers at this height above sea level (ruling P3): 1 km under the
/// cloud base, which the eased ceiling always reaches.
pub const CLIMB_OUT_M: f32 = (REGION_CLOUD_BASE_M - 1_000.0) as f32;
/// The time constant (s) of the descent inside the cloud while a dive waits for its region
/// ([`dive_drift_mps`]) *(call)*: it eases toward [`GLOBE_MIN_ALTITUDE_KM`], so the fly keeps
/// descending and slows to a drift.
pub const DIVE_DRIFT_S: f64 = 1.5;

/// A dive is called off once the fly, out of the opaque cloud (above [`DIVE_ALTITUDE_KM`]), is
/// this far from its target along the ground (km) *(call)*: the swap puts the fly over the
/// target, so a fly that flew off it would land in a region far from where it was; a descent
/// then commits a new dive where it is. Out of the opaque cloud any flight covers this within
/// a frame (the globe's speed there is about 1,000 km/s), and a climb straight up none of it.
pub const DIVE_LEAVE_KM: f64 = 10.0;

/// Progress within this of the end counts as reaching it (frame times add up in `f32`).
const FADE_EPSILON: f32 = 1e-4;

/// A fade through the cloud: closing over the first half, clearing over the second.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Fade {
    pub elapsed_s: f32,
    pub duration_s: f32,
}

impl Fade {
    pub fn new(duration_s: f32) -> Fade {
        Fade {
            elapsed_s: 0.0,
            duration_s,
        }
    }

    /// The fade `dt_s` later. With `hold`, it stops at exactly the midpoint (fully covered) until
    /// a later advance without it; a fade already past the midpoint never holds.
    pub fn advance(self, dt_s: f32, hold: bool) -> Fade {
        let mid = self.duration_s * 0.5;
        let limit = if hold && self.elapsed_s <= mid {
            mid
        } else {
            self.duration_s
        };
        Fade {
            elapsed_s: (self.elapsed_s + dt_s.max(0.0)).min(limit),
            ..self
        }
    }

    /// Progress in [0, 1].
    pub fn progress(self) -> f32 {
        if self.duration_s <= 0.0 {
            return 1.0;
        }
        (self.elapsed_s / self.duration_s).clamp(0.0, 1.0)
    }

    /// The cloud's density: [`fade_alpha`] of the progress.
    pub fn alpha(self) -> f32 {
        fade_alpha(self.progress())
    }

    /// Whether the views may swap now: the cloud is exactly opaque (the midpoint, where a held
    /// fade stops). Within rounding of the midpoint is not enough (Task 5 review: a swap at
    /// progress 0.5 − 1e-4 drew density 0.99999988).
    pub fn opaque(self) -> bool {
        self.alpha() == 1.0
    }

    pub fn done(self) -> bool {
        self.progress() >= 1.0 - FADE_EPSILON
    }
}

/// The cloud's density at fade progress `t`: 0 at both ends, 1 at the midpoint, eased
/// (smoothstep) both ways.
pub fn fade_alpha(t: f32) -> f32 {
    if !(0.0..=1.0).contains(&t) {
        return 0.0;
    }
    let tri = 1.0 - (2.0 * t - 1.0).abs();
    tri * tri * (3.0 - 2.0 * tri)
}

/// Smoothstep of `t` clamped to [0, 1]: exactly 0 at 0 and exactly 1 at 1.
fn ease01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The cloud's own density while gathering for a climb, `elapsed_s` in: 0 to exactly 1 at
/// [`CLOUD_GATHER_S`].
pub fn gather_density(elapsed_s: f32) -> f32 {
    ease01(elapsed_s / CLOUD_GATHER_S as f32)
}

/// How far into the gather ([`gather_density`]) the cloud is already `density`: a climb starts
/// there, so the cloud never thins on the way to the swap (fix round 1: from a snapshot change
/// held at full cover it swaps on the next frame).
pub fn gather_elapsed_for(density: f32) -> f32 {
    let gather = CLOUD_GATHER_S as f32;
    if density >= 1.0 {
        return gather;
    }
    if density <= 0.0 || density.is_nan() {
        return 0.0;
    }
    // The inverse of smoothstep, then up to the first time whose density is not below it.
    let t = 0.5 - ((1.0 - 2.0 * density).asin() / 3.0).sin();
    let mut elapsed = (t * gather).clamp(0.0, gather);
    while gather_density(elapsed) < density && elapsed < gather {
        elapsed = elapsed.next_up();
    }
    elapsed
}

/// The cloud's own density while emerging, `elapsed_s` in: exactly 1 at the swap, 0 at
/// [`EMERGE_S`].
pub fn emerge_density(elapsed_s: f32) -> f32 {
    1.0 - ease01(elapsed_s / EMERGE_S as f32)
}

/// The height (m above sea level) the fly emerging into a region follows, `elapsed_s` after the
/// swap: from [`REGION_CLOUD_BASE_M`] down to [`REGION_EMERGE_TO_M`] over [`EMERGE_S`], eased.
pub fn emerge_height_m(elapsed_s: f32) -> f32 {
    let (from, to) = (REGION_CLOUD_BASE_M as f32, REGION_EMERGE_TO_M as f32);
    from + (to - from) * ease01(elapsed_s / EMERGE_S as f32)
}

/// The altitude (km) the fly emerging over the globe follows, `elapsed_s` after the swap: from
/// [`DIVE_ALTITUDE_KM`] up to [`CLOUD_TOP_KM`] over [`EMERGE_S`], eased.
pub fn emerge_altitude_km(elapsed_s: f32) -> f64 {
    DIVE_ALTITUDE_KM
        + (CLOUD_TOP_KM - DIVE_ALTITUDE_KM) * f64::from(ease01(elapsed_s / EMERGE_S as f32))
}

/// The vertical speed (m/s, negative descends) of the fly inside the cloud while a dive waits:
/// an exponential ease toward [`GLOBE_MIN_ALTITUDE_KM`] with time constant [`DIVE_DRIFT_S`].
pub fn dive_drift_mps(altitude_km: f64) -> f32 {
    (-(altitude_km - GLOBE_MIN_ALTITUDE_KM).max(0.0) * 1000.0 / DIVE_DRIFT_S) as f32
}

/// The world a fly emerges into.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum World {
    Globe,
    Region,
}

/// Where the player is in the dive → fly → climb cycle.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum NavPhase {
    /// Flying on the globe.
    Globe,
    /// The dive committed to the region at `target` (the ground under the fly), `snapshot`: the
    /// globe shows, inside the cloud, until the region is ready. `since_s` is on the
    /// `LoadClock`; `started` once the region plugin has left `RegionState::Globe` for it.
    Diving {
        target: RenderDir,
        snapshot: YearsBp,
        since_s: f64,
        started: bool,
    },
    /// Just swapped into `world`, `elapsed_s` ago: the cloud thins while the fly eases out of
    /// it (down in a region, up over the globe).
    Emerging { world: World, elapsed_s: f32 },
    /// The fly camera has the controls.
    Flying,
    /// A snapshot change: fading out, held at full cover until the region centred on
    /// `requested` at `to` is generated and active, then fading in with the camera `agl_m`
    /// above the new ground. `generated` once `RegionGenerated` arrived for the current
    /// request; `placed` once the camera (and, released in the same frame, the terrain) moved
    /// to the new region.
    ChangingSnapshot {
        fade: Fade,
        to: YearsBp,
        requested: RenderDir,
        agl_m: f32,
        generated: bool,
        placed: bool,
    },
    /// Climbing out of the region centred on `centre` (none without a region): the cloud
    /// gathers for `elapsed_s` of [`CLOUD_GATHER_S`], then the views swap to the globe.
    Climbing {
        elapsed_s: f32,
        centre: Option<RenderDir>,
    },
    /// A hop to the neighbour centred on `centre` (requested; the fly flies on). `lift`, from 0
    /// to 1, is how far the lift and the cloud bank have gathered; `lift_from_agl_m` is the fly's
    /// height above the ground before the lift, which it returns to. `failed` once the
    /// neighbour failed: at full cover the fly turns back instead of swapping. The hop ends
    /// under the opaque cloud, `swapped` into the neighbour or `turned_back` inside the edge;
    /// then for `elapsed_s` of [`CLOUD_CLEAR_S`] the cloud clears while the fly returns to its
    /// height. `edge` is the edge of the region it crosses, worked out once per neighbour.
    /// Before the end, `elapsed_s` is the time held at full cover, across retargets.
    ///
    /// Its invariants (fix round 4; `a_hop_keeps_its_invariants_whatever_the_player_does`
    /// checks them on random play):
    /// - **I1.** The fly is outside the inner field only in the hold (at full cover, the one
    ///   place [`FlyControls::confined`] is off), drawn at density exactly 1.
    /// - **I2.** The white-out is bounded whatever the player and the neighbour job do: the hold
    ///   lasts at most [`HOLD_MAX_S`], and the end starts clearing on its first frame and lasts
    ///   at most [`HOP_END_MAX_S`], whatever the ground does to the return.
    /// - **I3.** A swap happens on a frame drawn opaque after one drawn opaque, to the neighbour
    ///   asked for and ready, while the fly still flies out of `edge`; turning back from the
    ///   hold drops the neighbour, so that hop never swaps.
    /// - **I4.** A failure ([`NavAction::TurnBack`], facing in) or a turn back from the hold
    ///   ([`NavAction::PlaceInside`], moving in) puts the fly inside the field.
    ///
    /// The hold ends in one place in [`step_nav`], and every ending puts the fly inside the
    /// field on that opaque frame (the swap's carry lands well inside the neighbour); the end
    /// is then confined like flying, so I1 holds by construction.
    Hopping {
        centre: RenderDir,
        edge: HopEdge,
        lift_from_agl_m: f32,
        swapped: bool,
        elapsed_s: f32,
        lift: f32,
        failed: bool,
        turned_back: bool,
    },
}

/// How the fly camera's controls apply in a phase ([`NavPhase::fly_controls`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FlyControls {
    /// The share of the horizontal speed the controls give.
    pub horizontal: f32,
    /// Whether climbing and descending are the player's.
    pub vertical: bool,
    /// Whether the fly is kept inside the inner field ([`timefly_core::flight::hop::confine`]):
    /// everywhere but a hop's hold at full cover, so the fly is past the edge only under an
    /// opaque cloud: it gets out only from the hold, a hop keeps its cloud opaque while the fly
    /// is out, and every way the hold ends puts it back inside (I1 on [`NavPhase::Hopping`]).
    pub confined: bool,
}

/// The expected generation time (s) of a neighbour on this platform, for
/// [`preload_distance_m`](timefly_core::flight::hop::preload_distance_m).
#[cfg(target_arch = "wasm32")]
pub const EXPECTED_GEN_S: f32 = timefly_core::flight::altitudes::EXPECTED_GEN_S_WEB as f32;
/// The expected generation time (s) of a neighbour on this platform, for
/// [`preload_distance_m`](timefly_core::flight::hop::preload_distance_m).
#[cfg(not(target_arch = "wasm32"))]
pub const EXPECTED_GEN_S: f32 = timefly_core::flight::altitudes::EXPECTED_GEN_S_NATIVE as f32;
/// The edge of the region a hop crosses: `axis` 0 for `x`, 1 for `z`, and `side` ±1.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HopEdge {
    pub axis: usize,
    pub side: f32,
}

impl HopEdge {
    /// The edge a hop to the neighbour centred on `centre` crosses, from the region centred on
    /// `region`: the axis the neighbour lies farthest along, on its side.
    pub fn toward(region: RenderDir, centre: RenderDir) -> HopEdge {
        let (x, z) = RegionFrame { center: region }
            .dir_to_local(centre)
            .unwrap_or((0.0, 0.0));
        let axis = usize::from(z.abs() > x.abs());
        let c = if axis == 0 { x } else { z };
        HopEdge {
            axis,
            side: if c < 0.0 { -1.0 } else { 1.0 },
        }
    }

    /// The yaw (about `+y` from north, positive west) facing into the field across this edge,
    /// along its axis: west (+π/2) from the east edge, east from the west, north (0) from the
    /// south (`+z`) and south (π) from the north.
    pub fn inward_yaw(self) -> f32 {
        // Facing `d` is `atan2(−d.x, −d.z)`; inward is `−side` along the axis.
        if self.axis == 0 {
            self.side * std::f32::consts::FRAC_PI_2
        } else if self.side > 0.0 {
            0.0
        } else {
            std::f32::consts::PI
        }
    }

    /// How far out along this edge's axis `v` points (a position: how far out; a velocity: how
    /// fast out of the edge).
    pub fn out(self, v: Vec2) -> f32 {
        self.side * v[self.axis]
    }
}

/// Where a failed neighbour puts the fly at `pos_m` (Review Focus 4): [`TURN_BACK_INSIDE_M`]
/// inside `edge`, and as far inside any other edge it is out past (a corner retarget from the
/// hold); otherwise where it is.
fn turn_back_pos(edge: HopEdge, pos_m: Vec2) -> Vec2 {
    let inside = EDGE_M - TURN_BACK_INSIDE_M;
    let mut p = pos_m;
    for a in 0..2 {
        if a == edge.axis {
            p[a] = edge.side * inside;
        } else if p[a].abs() > EDGE_M {
            p[a] = p[a].signum() * inside;
        }
    }
    p
}

/// A failed neighbour turns the fly back this far inside the edge (Review Focus 4).
pub const TURN_BACK_INSIDE_M: f32 = 10_000.0;
/// While a hop holds in the cloud, the fly's horizontal speed is this share of its own *(call:
/// the dive's drift, Task 7)*: a gentle drift (design §6), so a long wait never carries it far
/// past the edge.
pub const HOLD_DRIFT: f32 = 0.05;
/// The longest a hop holds at full cover, across retargets, before it counts as failed and
/// turns the fly back *(call: twice the region job's load timeout, a safety net that a stalled
/// load reaches first; counted on the same clamped clock)*: whatever the job and the player do,
/// the white-out ends (fix round 4).
pub const HOLD_MAX_S: f32 = 2.0 * crate::region::LOAD_TIMEOUT_S as f32;
/// A hop's return to the fly's height above the ground is done within this.
const RETURNED_M: f32 = 1.0;
/// The longest a hop's end lasts (s): the clear ([`CLOUD_CLEAR_S`]) plus 12 s for the return to
/// the fly's height *(call)*. The slowest return over flat ground, from [`HOP_ALTITUDE_M`] down to
/// the fly's clearance over the ground at the M2 speed for its height, takes about 10 s. Over
/// steep ground under boosted flight the return may never get within [`RETURNED_M`], and the
/// end holds the slider and confines the fly (the next edge cannot hop) until it ends, so it
/// ends here whatever the return does (the final review).
pub const HOP_END_MAX_S: f32 = CLOUD_CLEAR_S as f32 + 12.0;

impl NavPhase {
    /// Whether the region camera, rather than the globe camera, should be showing.
    pub fn region_shown(&self) -> bool {
        match *self {
            NavPhase::Globe | NavPhase::Diving { .. } => false,
            NavPhase::Emerging { world, .. } => world == World::Region,
            NavPhase::Flying
            | NavPhase::ChangingSnapshot { .. }
            | NavPhase::Climbing { .. }
            | NavPhase::Hopping { .. } => true,
        }
    }

    /// The cloud's density ([`crate::cloud::CloudDensity`]): the largest of the altitude band
    /// of the world on screen and the phase's own fade.
    pub fn cloud_density(&self, i: &NavInputs) -> f32 {
        let band = if self.region_shown() {
            region_density(i.region_height_m)
        } else {
            globe_density(i.globe_altitude_km)
        };
        let own = match *self {
            NavPhase::Emerging { elapsed_s, .. } => emerge_density(elapsed_s),
            NavPhase::Climbing { elapsed_s, .. } => gather_density(elapsed_s),
            NavPhase::ChangingSnapshot { fade, .. } => fade.alpha(),
            NavPhase::Hopping {
                lift,
                swapped,
                turned_back,
                elapsed_s,
                ..
            } => {
                if swapped || turned_back {
                    1.0 - ease01(elapsed_s / CLOUD_CLEAR_S as f32)
                } else {
                    ease01(lift)
                }
            }
            NavPhase::Globe | NavPhase::Diving { .. } | NavPhase::Flying => 0.0,
        };
        band.max(own)
    }

    /// Whether the cloud fully covers the view (a dive waiting in the cloud, a snapshot change
    /// held until its region is ready): a time-boxed generation may then take a larger slice of
    /// the frame (`crate::region::gen_budget`).
    pub fn covers_view(&self, i: &NavInputs) -> bool {
        self.cloud_density(i) >= 1.0 - 1e-3
    }

    /// Whether a region generating now may take the large web budget
    /// (`crate::region::gen_budget`): no region is on screen to keep smooth. That is a first
    /// entry (from the globe, where the launch region and the bench enter, or while diving, with
    /// the globe in view and nothing else heavy running) or a view the cloud covers. While a
    /// region is flown or shows through the cloud, the small budget keeps its frames smooth.
    pub fn allows_large_gen_budget(&self, i: &NavInputs) -> bool {
        matches!(self, NavPhase::Globe | NavPhase::Diving { .. }) || self.covers_view(i)
    }

    /// Whether the slider waits (design §9): during the dive, the climb and the emergence, a
    /// time change is queued and applied once the transition ends ([`hold_slider`]).
    ///
    /// A hop holds it too; a change before its lift ends the hop ([`step_nav`]), so the slider
    /// waits only while the cloud bank is up.
    pub fn holds_time(&self) -> bool {
        matches!(
            self,
            NavPhase::Diving { .. }
                | NavPhase::Emerging { .. }
                | NavPhase::Climbing { .. }
                | NavPhase::Hopping { .. }
        )
    }

    /// Whether the globe's horizontal flight is damped to a drift
    /// (`crate::globe_flight::dive_horizontal_scale`) at `altitude_km`: while a dive waits in
    /// the opaque cloud (at or below [`DIVE_ALTITUDE_KM`]), where the swap can come, so the fly
    /// holds over the dive point (Task 7). Climbed back above it the fly flies freely, and
    /// flying off the point calls the dive off ([`DIVE_LEAVE_KM`]).
    pub fn damps_dive_drift(&self, altitude_km: f64) -> bool {
        matches!(self, NavPhase::Diving { .. }) && globe_density(altitude_km) == 1.0
    }

    /// How long a hop has held at full cover waiting for its neighbour (its `elapsed_s` while
    /// lifted fully, before its end), or `None` outside a hop's hold: the shell shows
    /// "Descending..." for it as for a region generating (design §6 "Holding"), since a
    /// neighbour's job never enters `RegionState::Generating`.
    pub fn hop_hold_s(&self) -> Option<f32> {
        match *self {
            NavPhase::Hopping {
                lift,
                swapped: false,
                turned_back: false,
                elapsed_s,
                ..
            } if lift >= 1.0 => Some(elapsed_s),
            _ => None,
        }
    }

    /// Whether the player is flying the region with no transition on screen: flying, or on a hop
    /// whose neighbour is still only preloading (its lift has not begun), which leaves the region
    /// and the view as they are.
    pub fn is_flying(&self) -> bool {
        match *self {
            NavPhase::Flying => true,
            NavPhase::Hopping {
                lift,
                swapped: false,
                turned_back: false,
                ..
            } => lift == 0.0,
            _ => false,
        }
    }

    /// How the fly camera's controls apply, or `None` where they do not (navigation moves the
    /// fly): all of them while flying and around a hop's lift; during the lift the height is
    /// the lift's, and at full cover before the swap the fly slows to [`HOLD_DRIFT`].
    pub fn fly_controls(&self) -> Option<FlyControls> {
        let full = FlyControls {
            horizontal: 1.0,
            vertical: true,
            confined: true,
        };
        match *self {
            NavPhase::Flying => Some(full),
            NavPhase::Hopping {
                lift,
                swapped: false,
                turned_back: false,
                ..
            } if lift > 0.0 => {
                let held = lift >= 1.0;
                Some(FlyControls {
                    horizontal: if held { HOLD_DRIFT } else { 1.0 },
                    vertical: false,
                    confined: !held,
                })
            }
            NavPhase::Hopping { .. } => Some(full),
            _ => None,
        }
    }
}

/// A time change seen while flying: the new snapshot, where the camera's crust lies then, and
/// the camera's height above the drawn surface (`blended_surface_m`: the ground or the water).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TimeChange {
    pub to: YearsBp,
    pub carried: Option<RenderDir>,
    pub agl_m: f32,
}

/// Everything [`step_nav`] decides from, gathered once per frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NavInputs {
    pub now_s: f64,
    /// The `LoadClock`: running time, which [`START_TIMEOUT_S`] is measured on.
    pub load_s: f64,
    pub dt_s: f32,
    pub region_state: RegionState,
    pub time: YearsBp,
    /// `Backspace`: climb out of the region (or out of a dive).
    pub climb_key: bool,
    /// `RegionEntered` arrived.
    pub entered: bool,
    /// The spec (centre, snapshot) of a `RegionGenerated` that arrived: the job finished a
    /// region, whichever event (entered or snapshot changed) it also sent.
    pub generated: Option<(RenderDir, YearsBp)>,
    /// The active region's centre and snapshot.
    pub region_centre: Option<RenderDir>,
    pub region_snapshot: Option<YearsBp>,
    /// A time change to act on (gathered only while flying or waiting for a new snapshot).
    pub time_change: Option<TimeChange>,
    /// The globe's fly: altitude, direction and heading (render frame).
    pub globe_altitude_km: f64,
    pub globe_dir: RenderDir,
    pub globe_heading: DVec3,
    /// The region's fly: its height above sea level, and (with a region) where it is over the
    /// globe and its heading there ([`fly_on_globe`]).
    pub region_height_m: f32,
    pub region_fly: Option<(RenderDir, DVec3)>,
    /// The cloud density navigation raised last frame: what the frame before this one drew.
    pub shown_density: f32,
    /// The height of the drawn surface (the ground or the water over it) under the region's fly
    /// in the terrain drawn.
    pub region_ground_m: f32,
    /// The region's fly: where it is, `(x, z)` in local metres, and how fast it moved there
    /// over the last frame.
    pub region_pos_m: Vec2,
    pub region_vel_mps: Vec2,
    /// The neighbour ready in `PendingRegion`: its centre and snapshot.
    pub neighbour: Option<(RenderDir, YearsBp)>,
    /// `RegionPreloadFailed` arrived.
    pub preload_failed: bool,
    /// Climb or descend is held.
    pub vertical_held: bool,
    /// A slider change waits for the transition to end.
    pub time_queued: bool,
}

/// What [`step_nav`] asks the shell to do.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum NavAction {
    /// Set the globe time (the snap to a snapshot as the dive commits).
    SnapTime(YearsBp),
    Request(RegionRequested),
    /// Write `RegionLeft` at the swap back to the globe.
    Leave,
    /// Drop the dive's request (`RegionLeft` cancels the generation or removes the region).
    CancelRequest,
    /// Put the fly camera at the region's start pose (a region entered without a dive: the
    /// launch argument, the bench).
    PlaceAtStart,
    /// Put the fly camera over the new region's centre, `agl_m` above its ground.
    PlaceAfterChange {
        agl_m: f32,
    },
    /// Put the region's fly over the centre at `height_m` above sea level, heading `yaw`, at the
    /// start pitch, with the chase camera's smoothing reset.
    PlaceInRegion {
        yaw: f32,
        height_m: f32,
    },
    /// Put the globe's fly over `dir` at `altitude_km`, heading along `heading`, with the view's
    /// drag offset and the chase smoothing reset.
    PlaceOnGlobe {
        dir: RenderDir,
        heading: DVec3,
        altitude_km: f64,
    },
    /// Move the fly on screen vertically at this speed (m/s) this frame: the descent inside the
    /// cloud, the emergence, the climb.
    EaseFly {
        vertical_mps: f32,
    },
    /// Request the neighbour centred here, at the region's snapshot (a newer request replaces
    /// an older one).
    Preload(RenderDir),
    /// Drop the neighbour (`RegionPreloadCancelled`): its job, or a ready one.
    CancelPreload,
    /// Put the region's fly at `height_m` above sea level: the lift.
    LiftTo {
        height_m: f32,
    },
    /// Promote the ready neighbour (`RegionSwapRequested`), carry the fly into it at the same
    /// height above the ground, and reset the chase camera's smoothing.
    SwapToNeighbour,
    /// Ease the region's fly toward `agl_m` above the ground, at the M2 speed for its height.
    ReturnToAgl {
        agl_m: f32,
    },
    /// Turn the region's fly back into the field: put it at `pos_m` (`x`, `z`) facing `yaw`
    /// (inward along the hop's axis, whatever way it faced), with the chase camera's smoothing
    /// reset.
    TurnBack {
        pos_m: Vec2,
        yaw: f32,
    },
    /// Bring the region's fly back inside the field at `pos_m` (`x`, `z`), facing as it does,
    /// with the chase camera's smoothing reset: a turn back out of a hop's hold, under the opaque
    /// cloud (fix round 4).
    PlaceInside {
        pos_m: Vec2,
    },
}

/// Whether the active region is the one centred on `requested` at `to` (centres within about
/// 10 m: the region plugin normalizes the request's direction).
fn region_is(i: &NavInputs, requested: RenderDir, to: YearsBp) -> bool {
    match (i.region_centre, i.region_snapshot) {
        (Some(centre), Some(snapshot)) => same_region(centre, snapshot, requested, to),
        _ => false,
    }
}

/// Whether two region locations and snapshot ages identify the same flight destination.
pub fn same_region(
    centre: RenderDir,
    snapshot: YearsBp,
    requested: RenderDir,
    to: YearsBp,
) -> bool {
    snapshot == to && centre.0.normalize().dot(requested.0.normalize()) > 1.0 - 1e-12
}

/// Where the region's fly is over the globe and its heading there: the era-frame direction of
/// its local position (`frame.local_to_dir`), and the tangent there toward a point 1 km ahead
/// along its `yaw` (0 faces north, positive turns west).
pub fn fly_on_globe(frame: &RegionFrame, x_m: f64, z_m: f64, yaw: f32) -> (RenderDir, DVec3) {
    const AHEAD_M: f64 = 1_000.0;
    let basis = frame.basis();
    let dir = basis.local_to_dir(x_m, z_m);
    let (s, c) = f64::from(yaw).sin_cos();
    // Bevy's yaw about +y turns −z (north) toward −x (west).
    let ahead = basis.local_to_dir(x_m - s * AHEAD_M, z_m - c * AHEAD_M);
    let d = dir.0;
    let tangent = ahead.0 - d * d.dot(ahead.0);
    (dir, tangent.normalize())
}

/// The neighbour to request for the fly in the region, if it should be preloading: never the
/// region itself (Task 8 note b; the region job does not check), and only across an edge the fly
/// is near and flying out of. The edge a slanting heading reaches first can lie across the
/// field from the one [`should_preload`] saw (fix round 4: a fly past the north edge, turning
/// in with a little drift east, retargeted to the south neighbour 300 km away and swapped there).
fn neighbour_to_preload(i: &NavInputs) -> Option<RenderDir> {
    let (Some(region), Some(snapshot)) = (i.region_centre, i.region_snapshot) else {
        return None;
    };
    let (p, v) = (i.region_pos_m, i.region_vel_mps);
    if i.region_state != RegionState::InRegion || !should_preload(p, v, v.length(), EXPECTED_GEN_S)
    {
        return None;
    }
    let centre = neighbour_centre(&RegionFrame { center: region }, p, v);
    let edge = HopEdge::toward(region, centre);
    let near = EDGE_M - edge.out(p) <= preload_distance_m(v.length(), EXPECTED_GEN_S);
    (near && edge.out(v) > 0.0 && !same_region(centre, snapshot, region, snapshot))
        .then_some(centre)
}

/// Where a swap to the neighbour centred on `centre` would put the fly at `pos_m` in the region
/// centred on `region` (the shell's `carry_pose`): `(x, z)` in the neighbour's local metres.
fn carried_m(region: RenderDir, centre: RenderDir, pos_m: Vec2) -> DVec2 {
    let p = DVec3::new(f64::from(pos_m.x), 0.0, f64::from(pos_m.y));
    let (q, _) = carry_pose(
        &RegionFrame { center: region },
        &RegionFrame { center: centre },
        p,
        0.0,
    );
    DVec2::new(q.x, q.z)
}

/// The neighbour across `edge` of the region centred on `region`, level with the fly at `pos_m`:
/// [`NEIGHBOUR_PAST_EDGE_M`] past the edge, as [`neighbour_centre`] places it.
fn neighbour_level(region: RenderDir, edge: HopEdge, pos_m: Vec2) -> RenderDir {
    let mut c = pos_m.as_dvec2();
    c[edge.axis] = f64::from(edge.side) * (f64::from(EDGE_M) + NEIGHBOUR_PAST_EDGE_M);
    RegionFrame { center: region }.local_to_dir(c.x, c.y)
}

/// A hop starting now, to `centre` past `edge`.
fn hop_to(centre: RenderDir, edge: HopEdge, i: &NavInputs) -> NavPhase {
    NavPhase::Hopping {
        centre,
        edge,
        lift_from_agl_m: i.region_height_m - i.region_ground_m,
        swapped: false,
        elapsed_s: 0.0,
        lift: 0.0,
        failed: false,
        turned_back: false,
    }
}

/// The lift's target for a fly `distance_m` short of the hop's edge (past it, negative): 0 at
/// [`LIFT_LEAD_M`] and beyond, 1 at the edge and past it.
fn lift_target(distance_m: f32) -> f32 {
    let lead = LIFT_LEAD_M as f32;
    ((lead - distance_m) / lead).clamp(0.0, 1.0)
}

/// `lift` moved toward `target` over `dt_s`: gathering at most over [`CLOUD_GATHER_S`], clearing
/// at most over [`CLOUD_CLEAR_S`], so the cloud never pops when the hop starts near the edge.
fn lift_toward(lift: f32, target: f32, dt_s: f32) -> f32 {
    let dt = dt_s.max(0.0);
    if target > lift {
        (lift + dt / CLOUD_GATHER_S as f32).min(target)
    } else {
        (lift - dt / CLOUD_CLEAR_S as f32).max(target)
    }
}

/// The fly's height at lift `lift` from `base` (its height above the ground before the lift,
/// over the ground now): up to [`HOP_ALTITUDE_M`] by full cover, never down.
fn lift_height_m(base: f32, lift: f32) -> f32 {
    let top = base.max(HOP_ALTITUDE_M as f32);
    let e = ease01(lift);
    if e >= 1.0 {
        top
    } else {
        base + (top - base) * e
    }
}

/// The region `yaw` for a globe `heading` at the centre of the region centred on `centre`.
pub fn region_yaw(centre: RenderDir, heading: DVec3) -> f32 {
    let basis = RegionFrame { center: centre }.basis();
    heading_to_region_yaw(basis.center, heading, basis.north, basis.east)
}

/// The fly's vertical speed while emerging into `world`, `elapsed_s` into the emergence (this
/// frame's end), so it follows [`emerge_height_m`] down or [`emerge_altitude_km`] up. It never
/// moves the fly the other way: a fly already below the profile in a region (a launch region's
/// start pose) or above it over the globe stays where it is.
fn emerge_speed_mps(world: World, elapsed_s: f32, i: &NavInputs) -> f32 {
    if i.dt_s <= 0.0 {
        return 0.0;
    }
    match world {
        World::Region => {
            let h = i.region_height_m;
            (emerge_height_m(elapsed_s).min(h) - h) / i.dt_s
        }
        World::Globe => {
            let a = i.globe_altitude_km;
            let to = emerge_altitude_km(elapsed_s).clamp(a, GLOBE_MAX_ALTITUDE_KM.max(a));
            ((to - a) * 1000.0 / f64::from(i.dt_s)) as f32
        }
    }
}

/// The next phase and the actions to apply, from this frame's inputs.
pub fn step_nav(phase: NavPhase, i: &NavInputs) -> (NavPhase, Vec<NavAction>) {
    let mut actions = Vec::new();
    // The gather starts where the cloud already is: it never thins on the way out.
    let climb = |i: &NavInputs| NavPhase::Climbing {
        elapsed_s: gather_elapsed_for(i.shown_density),
        centre: i.region_centre,
    };
    // Over the globe, out of the cloud: where a dive that fails leaves the fly.
    let out_over_globe = NavPhase::Emerging {
        world: World::Globe,
        elapsed_s: 0.0,
    };
    // A region lost from under the fly (its generation failed, or it is gone) leaves it over
    // the globe above its place in the region at the dive altitude, as a climb does, not over
    // the dive point, which may be a continent of hops away; not knowing the place, where the
    // globe's fly is.
    let lost_region = |actions: &mut Vec<NavAction>| {
        if let Some((dir, heading)) = i.region_fly {
            actions.push(NavAction::PlaceOnGlobe {
                dir,
                heading,
                altitude_km: DIVE_ALTITUDE_KM,
            });
        }
        out_over_globe
    };
    let next = match phase {
        NavPhase::Globe => {
            if i.entered {
                // A region entered without a dive: the launch argument or the bench.
                actions.push(NavAction::PlaceAtStart);
                NavPhase::Emerging {
                    world: World::Region,
                    elapsed_s: 0.0,
                }
            } else if i.region_state == RegionState::Globe && i.globe_altitude_km < DIVE_ALTITUDE_KM
            {
                let snapshot = nearest_snapshot(i.time);
                if snapshot != i.time {
                    actions.push(NavAction::SnapTime(snapshot));
                }
                let target = RenderDir(i.globe_dir.0.normalize());
                actions.push(NavAction::Request(RegionRequested {
                    era_dir: target,
                    snapshot,
                    purpose: RequestPurpose::Enter,
                }));
                NavPhase::Diving {
                    target,
                    snapshot,
                    since_s: i.load_s,
                    started: false,
                }
            } else {
                NavPhase::Globe
            }
        }
        NavPhase::Diving {
            target,
            snapshot,
            since_s,
            started,
        } => {
            let started = started || i.region_state != RegionState::Globe;
            let ready = i.region_state == RegionState::InRegion && region_is(i, target, snapshot);
            let failed = (started && i.region_state == RegionState::Globe)
                || (!started && i.load_s - since_s > START_TIMEOUT_S);
            if i.climb_key {
                actions.push(NavAction::CancelRequest);
                out_over_globe
            } else if i.globe_altitude_km > CLOUD_TOP_KM {
                // Climbed back out of the cloud before the swap: the dive is off.
                actions.push(NavAction::CancelRequest);
                NavPhase::Globe
            } else if globe_density(i.globe_altitude_km) < 1.0
                && i.globe_dir
                    .0
                    .normalize()
                    .angle_between(target.0.normalize())
                    * crate::globe::EARTH_RADIUS_KM
                    > DIVE_LEAVE_KM
            {
                // Flown off the dive point out of the opaque cloud: the dive is off too, so a
                // descent dives where the fly is, not into the old target.
                actions.push(NavAction::CancelRequest);
                NavPhase::Globe
            } else if ready && i.shown_density == 1.0 && globe_density(i.globe_altitude_km) == 1.0 {
                // After a frame drawn opaque (a boosted descent can cross the dive altitude in
                // one frame), on a frame that is.
                actions.push(NavAction::PlaceInRegion {
                    yaw: region_yaw(i.region_centre.unwrap_or(target), i.globe_heading),
                    height_m: REGION_CLOUD_BASE_M as f32,
                });
                NavPhase::Emerging {
                    world: World::Region,
                    elapsed_s: 0.0,
                }
            } else if failed {
                // The generation failed (the region plugin logged why) or never started.
                if !started {
                    actions.push(NavAction::CancelRequest);
                }
                out_over_globe
            } else {
                actions.push(NavAction::EaseFly {
                    vertical_mps: dive_drift_mps(i.globe_altitude_km),
                });
                NavPhase::Diving {
                    target,
                    snapshot,
                    since_s,
                    started,
                }
            }
        }
        NavPhase::Emerging { world, elapsed_s } => {
            let elapsed_s = (elapsed_s + i.dt_s.max(0.0)).min(EMERGE_S as f32);
            if world == World::Region && i.region_state == RegionState::Globe {
                lost_region(&mut actions)
            } else if world == World::Region && i.climb_key {
                climb(i)
            } else {
                let vertical_mps = emerge_speed_mps(world, elapsed_s, i);
                if vertical_mps != 0.0 {
                    actions.push(NavAction::EaseFly { vertical_mps });
                }
                match world {
                    _ if elapsed_s < EMERGE_S as f32 => NavPhase::Emerging { world, elapsed_s },
                    World::Region => NavPhase::Flying,
                    World::Globe => NavPhase::Globe,
                }
            }
        }
        NavPhase::Flying => {
            if i.region_state == RegionState::Globe {
                lost_region(&mut actions)
            } else if i.climb_key || i.region_height_m >= CLIMB_OUT_M {
                climb(i)
            } else if let Some(change) = i.time_change {
                match change.carried {
                    Some(era_dir) => {
                        actions.push(NavAction::Request(RegionRequested {
                            era_dir,
                            snapshot: change.to,
                            purpose: RequestPurpose::Snapshot,
                        }));
                        NavPhase::ChangingSnapshot {
                            fade: Fade::new(SNAPSHOT_FADE_S),
                            to: change.to,
                            requested: era_dir,
                            agl_m: change.agl_m,
                            generated: false,
                            placed: false,
                        }
                    }
                    None => climb(i),
                }
            } else if let Some(centre) = neighbour_to_preload(i)
                && let Some(region) = i.region_centre
            {
                actions.push(NavAction::Preload(centre));
                hop_to(centre, HopEdge::toward(region, centre), i)
            } else {
                NavPhase::Flying
            }
        }
        NavPhase::ChangingSnapshot {
            fade,
            mut to,
            mut requested,
            agl_m,
            generated,
            placed,
        } => {
            if i.region_state == RegionState::Globe {
                // The generation failed; the region plugin removed the region.
                return (lost_region(&mut actions), actions);
            }
            if i.climb_key {
                return (climb(i), actions);
            }
            let mut generated = generated
                || i.generated
                    .is_some_and(|(centre, snapshot)| same_region(centre, snapshot, requested, to));
            if !placed
                && !region_is(i, requested, to)
                && let Some(change) = i.time_change
            {
                // The time moved again before the region was ready: carry from the region
                // still shown, which supersedes the request in flight.
                match change.carried {
                    Some(era_dir) => {
                        to = change.to;
                        requested = era_dir;
                        generated = i.generated.is_some_and(|(centre, snapshot)| {
                            same_region(centre, snapshot, requested, to)
                        });
                        actions.push(NavAction::Request(RegionRequested {
                            era_dir,
                            snapshot: to,
                            purpose: RequestPurpose::Snapshot,
                        }));
                    }
                    None => return (climb(i), actions),
                }
            }
            // Ready once the job has generated the region asked for and it is the active one.
            // `RegionGenerated` comes with every finished region: a request back at the
            // snapshot shown ends in `RegionEntered`, not `RegionSnapshotChanged` (Task 5's
            // semantics, kept), so neither of those alone will do.
            let ready = generated && region_is(i, requested, to);
            // Held at full cover until the camera moves. It moves once ready, after a frame
            // already drawn exactly opaque, so the cloud is opaque on both sides of the swap.
            let opaque = fade.opaque();
            let fade = fade.advance(i.dt_s, !placed);
            let placed = placed || {
                let swap = ready && opaque;
                if swap {
                    actions.push(NavAction::PlaceAfterChange { agl_m });
                }
                swap
            };
            if placed && fade.done() {
                NavPhase::Flying
            } else {
                NavPhase::ChangingSnapshot {
                    fade,
                    to,
                    requested,
                    agl_m,
                    generated,
                    placed,
                }
            }
        }
        NavPhase::Climbing { elapsed_s, centre } => {
            // The swap follows a frame already drawn exactly opaque.
            let opaque = gather_density(elapsed_s) == 1.0;
            let elapsed_s = (elapsed_s + i.dt_s.max(0.0)).min(CLOUD_GATHER_S as f32);
            if i.region_state == RegionState::Globe {
                lost_region(&mut actions)
            } else if opaque {
                actions.push(NavAction::Leave);
                // Over the fly's place; without one, over the region's centre heading north;
                // without a region either, the globe's fly stays where it is.
                let centred = centre.map(|c| (c, RegionFrame { center: c }.north_east().0));
                if let Some((dir, heading)) = i.region_fly.or(centred) {
                    actions.push(NavAction::PlaceOnGlobe {
                        dir,
                        heading,
                        altitude_km: DIVE_ALTITUDE_KM,
                    });
                }
                out_over_globe
            } else {
                // At the M2 speed for its height above the ground drawn.
                actions.push(NavAction::EaseFly {
                    vertical_mps: fly_speed(i.region_height_m - i.region_ground_m),
                });
                NavPhase::Climbing { elapsed_s, centre }
            }
        }
        NavPhase::Hopping {
            centre,
            edge,
            lift_from_agl_m,
            swapped,
            elapsed_s,
            lift,
            failed,
            turned_back,
        } => {
            if i.region_state == RegionState::Globe {
                return (lost_region(&mut actions), actions);
            }
            if i.climb_key || i.region_height_m >= CLIMB_OUT_M {
                // Out to the globe, from the cloud already there. Leaving drops the neighbour
                // too, but only at the climb's swap: stop its job now (minor 6).
                if !(swapped || turned_back) {
                    actions.push(NavAction::CancelPreload);
                }
                return (climb(i), actions);
            }
            let agl = i.region_height_m - i.region_ground_m;
            if swapped || turned_back {
                return (step_hop_end(phase, agl, i, &mut actions), actions);
            }
            let Some(region) = i.region_centre else {
                return (lost_region(&mut actions), actions);
            };
            if lift == 0.0 && i.time_queued {
                // A slider change before the lift: the hop is off, the change applies in flight
                // and drops the neighbour (built for the old snapshot) with its request.
                actions.push(NavAction::CancelPreload);
                return (NavPhase::Flying, actions);
            }
            let mut centre = centre;
            // A job taking over for an enter or snapshot change drops the neighbour.
            let mut failed = failed || i.preload_failed || i.region_state != RegionState::InRegion;
            let mut edge = edge;
            // Flying no longer out of the hop's edge, near another: retarget.
            if edge.out(i.region_vel_mps) <= 0.0
                && let Some(c) = neighbour_to_preload(i)
                && HopEdge::toward(region, c) != edge
            {
                actions.push(NavAction::Preload(c));
                centre = c;
                failed = false;
                edge = HopEdge::toward(region, c);
            }
            // The neighbour must carry the fly inside its field (I1). One asked for along an
            // older heading lies level with where that heading crossed the edge; a fly that
            // turned since (sliding along another edge to a corner, say) can come out more than
            // a field's half from it along the edge, so ask again, level with the fly.
            let along = carried_m(region, centre, i.region_pos_m)[1 - edge.axis];
            if along.abs() > f64::from(EDGE_M) {
                let c = neighbour_level(region, edge, i.region_pos_m);
                actions.push(NavAction::Preload(c));
                centre = c;
                failed = false;
            }
            let out_m = edge.out(i.region_pos_m);
            let outward = edge.out(i.region_vel_mps);
            // Past an edge (the hold drifts out, and turning back or retargeting starts there),
            // the cloud stays opaque: the fly is outside the field only under full cover (I1).
            let outside = i.region_pos_m.abs().max_element() > EDGE_M;
            let target = if outside {
                1.0
            } else if outward < 0.0 {
                0.0
            } else {
                lift_target(EDGE_M - out_m)
            };
            let next_lift = lift_toward(lift, target, i.dt_s);
            // The height above the ground the lift starts from follows the player until it does.
            let lift_from_agl_m = if lift == 0.0 && next_lift == 0.0 {
                agl
            } else {
                lift_from_agl_m
            };
            if next_lift == 0.0 && outward < 0.0 {
                // Turned back, and the cloud has cleared: the hop is off, the region kept.
                if lift > 0.0 {
                    actions.push(NavAction::LiftTo {
                        height_m: i.region_ground_m + lift_from_agl_m,
                    });
                }
                actions.push(NavAction::CancelPreload);
                return (NavPhase::Flying, actions);
            }
            if lift > 0.0 || next_lift > 0.0 {
                actions.push(NavAction::LiftTo {
                    height_m: lift_height_m(i.region_ground_m + lift_from_agl_m, next_lift),
                });
            }
            // Time at full cover, across retargets: past the budget the hold counts as failed
            // (I2, whatever the job and the player do). Counted as the `LoadClock` counts the
            // job's own timeouts, so a stalled frame (a hidden tab) is one short frame here too.
            let elapsed_s = if next_lift >= 1.0 {
                elapsed_s + i.dt_s.clamp(0.0, LOAD_CLOCK_MAX_DELTA_S as f32)
            } else {
                elapsed_s
            };
            let failed = failed || elapsed_s >= HOLD_MAX_S;
            let opaque = ease01(next_lift) == 1.0 && i.shown_density == 1.0;
            // Ready, and near enough the edge to land inside the neighbour's field (a fly held
            // out past another edge may still be short of this one).
            let ready = i.region_state == RegionState::InRegion
                && i.neighbour.is_some_and(|(c, snapshot)| {
                    i.region_snapshot
                        .is_some_and(|shown| same_region(c, snapshot, centre, shown))
                })
                && carried_m(region, centre, i.region_pos_m)
                    .abs()
                    .max_element()
                    <= f64::from(EDGE_M);
            let ended = |swapped: bool, turned_back: bool| NavPhase::Hopping {
                centre,
                edge,
                lift_from_agl_m,
                swapped,
                elapsed_s: 0.0,
                lift: next_lift,
                failed,
                turned_back,
            };
            // The hold ends here, and only here: on a frame drawn opaque after one drawn opaque,
            // with the fly put back inside the field on that frame (I1, I4), so the end that
            // follows is always inside, confined, and clears at once (I2).
            if opaque && outside && outward < 0.0 {
                // Turned back out of the hold (fix rounds 3 and 4): the hop is off, so no swap
                // can follow and a ready neighbour is dropped (I3). The fly is brought in to the
                // edge, unseen, moving and facing as the player has it.
                actions.push(NavAction::CancelPreload);
                actions.push(NavAction::PlaceInside {
                    pos_m: into_field(i.region_pos_m),
                });
                ended(false, true)
            } else if opaque && failed {
                // Review Focus 4: round, inside the edge, under the opaque cloud.
                actions.push(NavAction::CancelPreload);
                actions.push(NavAction::TurnBack {
                    pos_m: turn_back_pos(edge, i.region_pos_m),
                    yaw: edge.inward_yaw(),
                });
                ended(false, true)
            } else if opaque && ready {
                // After a frame drawn opaque, on a frame that is.
                actions.push(NavAction::SwapToNeighbour);
                ended(true, false)
            } else {
                NavPhase::Hopping {
                    centre,
                    edge,
                    lift_from_agl_m,
                    swapped,
                    elapsed_s,
                    lift: next_lift,
                    failed,
                    turned_back,
                }
            }
        }
    };
    (next, actions)
}

/// A hop that has ended under the cloud (`swapped`, or turned back): the cloud clears over
/// [`CLOUD_CLEAR_S`] while the fly, `agl_m` above the ground, returns to `lift_from_agl_m`
/// (climbing or descending makes the player's height the one to keep); then flying, once
/// returned or after [`HOP_END_MAX_S`] in all.
fn step_hop_end(
    hop: NavPhase,
    agl_m: f32,
    i: &NavInputs,
    actions: &mut Vec<NavAction>,
) -> NavPhase {
    let NavPhase::Hopping {
        centre,
        edge,
        lift_from_agl_m,
        swapped,
        elapsed_s,
        ..
    } = hop
    else {
        return hop;
    };
    // Every ending put the fly inside the field, where the end's controls keep it: the clear
    // starts at once and nothing holds it (fix round 4).
    let elapsed_s = (elapsed_s + i.dt_s.max(0.0)).min(HOP_END_MAX_S);
    let lift_from_agl_m = if i.vertical_held {
        agl_m
    } else {
        lift_from_agl_m
    };
    let returned = (agl_m - lift_from_agl_m).abs() <= RETURNED_M;
    if !returned {
        actions.push(NavAction::ReturnToAgl {
            agl_m: lift_from_agl_m,
        });
    }
    let cleared = elapsed_s >= CLOUD_CLEAR_S as f32;
    if cleared && (returned || elapsed_s >= HOP_END_MAX_S) {
        NavPhase::Flying
    } else {
        NavPhase::Hopping {
            centre,
            edge,
            lift_from_agl_m,
            swapped,
            elapsed_s,
            lift: 1.0,
            failed: !swapped,
            turned_back: !swapped,
        }
    }
}

/// The slider's hold through a transition (design §9): the time shown, the time a transition
/// holds and a change waiting for its end.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SliderHold {
    pub held: Option<YearsBp>,
    pub queued: Option<YearsBp>,
}

/// The time to show and the hold after it: during a transition (`holding`) the first time seen
/// is held, and a time that moves off it is queued and put back; once the transition ends, a
/// queued time is applied, once.
pub fn hold_slider(holding: bool, time: YearsBp, hold: SliderHold) -> (YearsBp, SliderHold) {
    if holding {
        let held = hold.held.unwrap_or(time);
        let queued = if time == held {
            hold.queued
        } else {
            Some(time)
        };
        (
            held,
            SliderHold {
                held: Some(held),
                queued,
            },
        )
    } else {
        (hold.queued.unwrap_or(time), SliderHold::default())
    }
}
