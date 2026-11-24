//! Entering and leaving regions, and snapshot changes inside one: the stand-in for zoom-and-swap
//! (M2 design §6.1–6.2, parent spec §5.2, §6.2 and §11).
//!
//! - **Entering.** A double-click on the globe ([`is_double_click`]) ray-casts to the direction
//!   under the cursor ([`globe_hit`]), snaps the time to the nearest snapshot
//!   ([`nearest_snapshot`]) and sends [`RegionRequested`]. While the region generates, the fly
//!   eases over the spot and down to [`NEAR_GLOBE_ALTITUDE_KM`] ([`ease_flight_toward`]; Task 7
//!   replaces this stand-in with the dive). When it is ready ([`RegionEntered`]), the view fades over
//!   [`ENTER_FADE_S`] to the fly camera at its start pose ([`FlyState::start`]).
//! - **Leaving.** `Backspace` fades back to the globe, centred on the region; [`RegionLeft`] is
//!   written at the fade's midpoint, once the view is back on the globe.
//! - **Snapshot changes.** When the time moves to another snapshot while flying (the slider
//!   snaps on release inside a region; `[` and `]` step snapshots), the camera's crust position
//!   is carried to the new era ([`carry_crust_position`]). The region regenerates around it,
//!   keeping the camera's height above ground, behind a [`SNAPSHOT_FADE_S`] fade that holds at
//!   its midpoint until the new region is ready. If that crust does not exist in the new era,
//!   the view returns to the globe instead.
//! - **"Descending..."** shows only after a region has been generating for
//!   [`CAPTION_DELAY_S`].
//! - **"terrain detail unavailable here"** ([`FALLBACK_NOTICE_TEXT`]) shows for
//!   [`FALLBACK_NOTICE_S`] after a region appears ([`RegionEntered`] or
//!   [`RegionSnapshotChanged`]) whose [`RegionGenerated`] says it fell back (a tile it needs is
//!   missing or corrupt, so part of it uses globe-resolution data; the job logged a `warn`).
//!
//! **The crossfade** goes through the transition cloud (M3 design §6, `crate::cloud`): a
//! [`Fade`]'s [`fade_alpha`] is the [`CloudDensity`] it raises, so the old view clouds over in
//! the first half, the views swap at the midpoint, on the frame the cloud is exactly opaque (a
//! fade stops there for that frame; only one camera is ever active, so the frame cost never
//! doubles), and the new view clears in the second half. The "Descending..." caption and the
//! fallback notice stay UI nodes over it.
//!
//! The systems gather → decide → apply: [`step_nav`] is the decision, a pure function of
//! [`NavInputs`] that returns the next [`NavPhase`] and the [`NavAction`]s to apply.

use std::sync::Arc;

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::MouseButton;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::Grid;
use timefly_core::geo::{RenderDir, geo_to_latlon, render_to_geo};
use timefly_core::geodynamics::{Candidate, CrustModel, PlateIndex, ResolvedPosition};
use timefly_core::globe::{
    DeepSampling, PlateRotations, candidate_land_m, cube_frame_sample, plate_rotations,
    ranking_surface_km,
};
use timefly_core::surface::{FrameSelection, Timeline};
use timefly_core::terrain::blended_height_m;
use tracing::instrument;

use super::frames::FrameCache;
use super::{
    ActiveRegion, LargeGenBudget, RegionEntered, RegionGenerated, RegionLeft, RegionRequested,
    RegionSnapshotChanged, RegionState, RegionSystems,
};
use crate::clock::LoadClock;
use crate::cloud::{CloudDensity, add_cloud_density};
use crate::data::GlobeData;
use crate::fly_camera::{CAMERA_CEILING_M, CAMERA_CLEARANCE_M, FlyState};
use crate::globe::{EARTH_RADIUS_KM, GlobeCamera};
use crate::globe_flight::{GlobeFlightState, GlobeFlightSystems, ease_flight_toward};
use crate::terrain::{TerrainSceneSystems, TerrainSettings};
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged, nearest_snapshot};
use crate::ui::{PointerOverUi, TEXT_COLOR};
use timefly_core::flight::altitudes::DIVE_ALTITUDE_KM;
use timefly_core::flight::globe::GlobeFlight;

/// The fade from the globe into a region, and back out (design §6.1: under a second).
pub const ENTER_FADE_S: f32 = 0.8;
/// The fade across a snapshot change inside a region (design §6.2: about a second).
pub const SNAPSHOT_FADE_S: f32 = 1.0;
/// Two presses within this many seconds...
pub const DOUBLE_CLICK_S: f64 = 0.35;
/// ...and this many logical pixels make a double-click.
pub const DOUBLE_CLICK_PX: f32 = 6.0;
/// "Descending..." shows only once a region has been generating this long (design §6.1).
pub const CAPTION_DELAY_S: f64 = 1.0;
/// The caption's text (the built-in font has no ellipsis glyph).
pub const CAPTION_TEXT: &str = "Descending...";
/// The notice for a region that fell back to globe-resolution data (Review Focus 2).
pub const FALLBACK_NOTICE_TEXT: &str = "terrain detail unavailable here";
/// How long the fallback notice shows after the region appears.
pub const FALLBACK_NOTICE_S: f64 = 3.0;
/// A request the region plugin never starts on (it ignores requests it cannot serve, with a
/// `warn`) stops the descent after this long, on the [`LoadClock`]: a hidden tab or paused page
/// does not cancel it.
pub const START_TIMEOUT_S: f64 = 2.0;
/// The globe altitude the fly eases down to while descending, and returns at when leaving: the
/// dive altitude, where Task 7's dive and climb swap worlds.
pub const NEAR_GLOBE_ALTITUDE_KM: f64 = DIVE_ALTITUDE_KM;

/// The ICE-6G_C frames, the recent globe's geographic frame, end at 26 ka BP (1950), 26,076
/// years before the game's present. Up to that age a direction is geographic: crust positions
/// use the present-day plate orientation, with validity still at the age (parent spec §5.2).
/// Past it they use the era's rotation. (The short bridge above it, to 30 ka, crossfades two
/// frames that differ by under 2 km of plate motion.)
pub const GEOGRAPHIC_FRAME_MAX_YEARS: f64 = 26_076.0;

/// Whether a press at `now_s` and `pos` (logical pixels) completes a double-click with the
/// previous press: within [`DOUBLE_CLICK_S`] and [`DOUBLE_CLICK_PX`].
pub fn is_double_click(prev: Option<(f64, Vec2)>, now_s: f64, pos: Vec2) -> bool {
    prev.is_some_and(|(then_s, at)| {
        let dt = now_s - then_s;
        (0.0..=DOUBLE_CLICK_S).contains(&dt) && at.distance(pos) <= DOUBLE_CLICK_PX
    })
}

/// The direction (render frame) of the first point where `ray` meets the globe, a sphere of
/// radius [`EARTH_RADIUS_KM`] at the origin in the globe's units (1 unit = 1 km). `None` when
/// the ray misses or the sphere is behind it.
pub fn globe_hit(ray: Ray3d) -> Option<RenderDir> {
    let origin = ray.origin.as_dvec3();
    let dir = ray.direction.as_vec3().as_dvec3().normalize();
    // |o + t d|² = R²  →  t² + 2 b t + c = 0
    let b = origin.dot(dir);
    let c = origin.length_squared() - EARTH_RADIUS_KM * EARTH_RADIUS_KM;
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let root = disc.sqrt();
    let t = [-b - root, -b + root].into_iter().find(|&t| t >= 0.0)?;
    let hit = origin + dir * t;
    (hit.length_squared() > 0.0).then(|| RenderDir(hit.normalize()))
}

/// Whether ages up to `age` read their directions in the geographic frame (see
/// [`GEOGRAPHIC_FRAME_MAX_YEARS`]).
fn geographic(age: YearsBp) -> bool {
    age.0 <= GEOGRAPHIC_FRAME_MAX_YEARS
}

/// Where the crust under `era_dir` at `from` lies at `to`: the era-frame direction to centre
/// the new region on, or `None` when the point is era-fixed at `from` (no valid plate) or that
/// crust is not valid at `to` (parent spec §5.2: the traveller returns to the globe).
///
/// `surface_for_candidate` ranks overlapping plates, as [`CrustModel::resolve`] takes it. Pass
/// the renderer's ranking at `from` ([`SurfaceRanker`]) so the camera is carried with the plate
/// drawn under it (ruling R19, a deliberate change from the plan's signature); `|_| None` falls
/// back to the lowest valid plate ID. Within the geographic frame (both ages up to
/// [`GEOGRAPHIC_FRAME_MAX_YEARS`]) the place stays put: the carry is the identity.
pub fn carry_crust_position(
    crust: &CrustModel,
    era_dir: RenderDir,
    from: YearsBp,
    to: YearsBp,
    surface_for_candidate: impl FnMut(&Candidate) -> Option<f64>,
) -> Option<RenderDir> {
    if geographic(from) && geographic(to) {
        return Some(RenderDir(era_dir.0.normalize()));
    }
    let resolved = if geographic(from) {
        crust.resolve_recent(era_dir, from.myr(), surface_for_candidate)
    } else {
        crust.resolve(era_dir, from.myr(), surface_for_candidate)
    };
    let ResolvedPosition::Tracked(pos) = resolved else {
        return None;
    };
    if geographic(to) {
        crust.forward_recent(pos, to.myr())
    } else {
        crust.forward(pos, to.myr())
    }
}

/// Progress within this of the midpoint or the end counts as reaching it (frame times add up
/// in `f32`).
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

    /// The fade `dt_s` later. With `hold`, it stops at the midpoint (fully covered) until a
    /// later advance without it; a fade already past the midpoint never holds.
    pub fn advance(self, dt_s: f32, hold: bool) -> Fade {
        let mid = self.duration_s * 0.5;
        // Held at the midpoint (or wherever within rounding of it the fade stopped).
        let limit = if hold && self.progress() <= 0.5 + FADE_EPSILON {
            mid.max(self.elapsed_s)
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

    /// Whether the views have swapped (the midpoint is reached).
    pub fn swapped(self) -> bool {
        self.progress() >= 0.5 - FADE_EPSILON
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

/// Whether "Descending..." shows, `generating_for_s` after the region started generating.
pub fn caption_shown(generating_for_s: Option<f64>) -> bool {
    generating_for_s.is_some_and(|s| s > CAPTION_DELAY_S)
}

/// When the fallback notice started, after a frame in which a region appeared
/// (`appeared = Some(fallback)`) or none did (`None`): a region that fell back restarts it at
/// `now_s`, one that did not clears it.
pub fn fallback_since(prev: Option<f64>, appeared: Option<bool>, now_s: f64) -> Option<f64> {
    match appeared {
        Some(true) => Some(now_s),
        Some(false) => None,
        None => prev,
    }
}

/// Whether the fallback notice shows at `now_s`, for a notice started at `since_s`, while the
/// region is up (`region_up`: not leaving, not back on the globe).
pub fn fallback_notice_shown(since_s: Option<f64>, now_s: f64, region_up: bool) -> bool {
    region_up && since_s.is_some_and(|since| (0.0..FALLBACK_NOTICE_S).contains(&(now_s - since)))
}

/// Where the player is in the enter → fly → leave cycle.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum NavPhase {
    Globe,
    /// A region was requested from the globe; the fly eases over `target`.
    /// `started` once the region plugin has left `RegionState::Globe` for it. `since_s` is on
    /// the [`LoadClock`].
    Descending {
        target: RenderDir,
        since_s: f64,
        started: bool,
    },
    /// Fading from the globe into the region; `placed` once the fly camera is at its start.
    Entering {
        fade: Fade,
        placed: bool,
    },
    /// The fly camera has the controls.
    Flying,
    /// A snapshot change: fading out, held at full cover until the region centred on
    /// `requested` at `to` is generated and active, then fading in with the camera `agl_m`
    /// above the new ground. `generated` once [`RegionGenerated`] arrived for the current
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
    /// Fading back to the globe, centred on `centre`; `left` once `RegionLeft` is written.
    Leaving {
        fade: Fade,
        centre: RenderDir,
        left: bool,
    },
}

impl NavPhase {
    /// Whether the region camera, rather than the globe camera, should be showing.
    pub fn region_shown(&self) -> bool {
        match *self {
            NavPhase::Globe | NavPhase::Descending { .. } => false,
            NavPhase::Entering { placed, .. } => placed,
            NavPhase::Flying | NavPhase::ChangingSnapshot { .. } => true,
            NavPhase::Leaving { left, .. } => !left,
        }
    }

    /// The cloud density the phase raises ([`CloudDensity`]).
    pub fn cloud_density(&self) -> f32 {
        match *self {
            NavPhase::Entering { fade, .. }
            | NavPhase::ChangingSnapshot { fade, .. }
            | NavPhase::Leaving { fade, .. } => fade.alpha(),
            NavPhase::Globe | NavPhase::Descending { .. } | NavPhase::Flying => 0.0,
        }
    }

    /// Whether the cloud fully covers the view (a fade at its midpoint, such as a snapshot
    /// change held until its region is ready): a time-boxed generation may then take a larger
    /// slice of the frame (`crate::region::gen_budget`).
    pub fn covers_view(&self) -> bool {
        self.cloud_density() >= 1.0 - 1e-3
    }

    /// Whether a region generating now may take the large web budget
    /// (`crate::region::gen_budget`): no region is on screen to keep smooth. That is a first
    /// entry (from the globe, where the launch region and the bench enter, or while descending,
    /// with the globe in view and nothing else heavy running) or a fade held at full cover. While
    /// a region is flown or shows through a fade, the small budget keeps its frames smooth.
    pub fn allows_large_gen_budget(&self) -> bool {
        matches!(self, NavPhase::Globe | NavPhase::Descending { .. }) || self.covers_view()
    }
}

/// A time change seen while flying: the new snapshot, where the camera's crust lies then, and
/// the camera's height above `max(ground, 0)`.
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
    /// The [`LoadClock`]: running time, which [`START_TIMEOUT_S`] is measured on.
    pub load_s: f64,
    pub dt_s: f32,
    pub region_state: RegionState,
    pub time: YearsBp,
    /// The globe direction under a double-click this frame.
    pub double_click: Option<RenderDir>,
    pub backspace: bool,
    /// [`RegionEntered`] arrived.
    pub entered: bool,
    /// The spec (centre, snapshot) of a [`RegionGenerated`] that arrived: the job finished a
    /// region, whichever event (entered or snapshot changed) it also sent.
    pub generated: Option<(RenderDir, YearsBp)>,
    /// The active region's centre and snapshot.
    pub region_centre: Option<RenderDir>,
    pub region_snapshot: Option<YearsBp>,
    /// A time change to act on (gathered only while flying or waiting for a new snapshot).
    pub time_change: Option<TimeChange>,
}

/// What [`step_nav`] asks the shell to do.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum NavAction {
    /// Set the globe time (a snap to a snapshot before entering).
    SnapTime(YearsBp),
    Request(RegionRequested),
    Leave,
    /// Put the fly camera at the region's start pose.
    PlaceAtStart,
    /// Put the fly camera over the new region's centre, `agl_m` above its ground.
    PlaceAfterChange {
        agl_m: f32,
    },
    /// Put the globe's fly over a direction at [`NEAR_GLOBE_ALTITUDE_KM`], heading north.
    CentreGlobe(RenderDir),
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

/// The next phase and the actions to apply, from this frame's inputs.
pub fn step_nav(phase: NavPhase, i: &NavInputs) -> (NavPhase, Vec<NavAction>) {
    let mut actions = Vec::new();
    let leave = |centre: Option<RenderDir>| NavPhase::Leaving {
        fade: Fade::new(ENTER_FADE_S),
        // Without a region (it cannot happen while flying) the globe stays where it is.
        centre: centre.unwrap_or(RenderDir(DVec3::ZERO)),
        left: false,
    };
    let next = match phase {
        NavPhase::Globe => {
            if i.entered {
                // A region entered without a double-click: the launch argument.
                NavPhase::Entering {
                    fade: Fade::new(ENTER_FADE_S),
                    placed: false,
                }
            } else if let Some(target) = i.double_click
                && i.region_state == RegionState::Globe
            {
                let snapshot = nearest_snapshot(i.time);
                if snapshot != i.time {
                    actions.push(NavAction::SnapTime(snapshot));
                }
                actions.push(NavAction::Request(RegionRequested {
                    era_dir: target,
                    snapshot,
                }));
                NavPhase::Descending {
                    target,
                    since_s: i.load_s,
                    started: false,
                }
            } else {
                NavPhase::Globe
            }
        }
        NavPhase::Descending {
            target,
            since_s,
            started,
        } => {
            let started = started || i.region_state != RegionState::Globe;
            if i.backspace {
                actions.push(NavAction::Leave);
                NavPhase::Globe
            } else if i.entered {
                NavPhase::Entering {
                    fade: Fade::new(ENTER_FADE_S),
                    placed: false,
                }
            } else if started && i.region_state == RegionState::Globe {
                // The generation failed (the region plugin logged why).
                NavPhase::Globe
            } else if !started && i.load_s - since_s > START_TIMEOUT_S {
                NavPhase::Globe
            } else {
                NavPhase::Descending {
                    target,
                    since_s,
                    started,
                }
            }
        }
        NavPhase::Entering { fade, placed } => {
            // Stops at the midpoint for the swap frame, so the views swap under an opaque cloud.
            let fade = fade.advance(i.dt_s, !placed);
            let placed = placed || {
                let swap = fade.swapped();
                if swap {
                    actions.push(NavAction::PlaceAtStart);
                }
                swap
            };
            if i.region_state == RegionState::Globe {
                NavPhase::Globe
            } else if fade.done() {
                NavPhase::Flying
            } else {
                NavPhase::Entering { fade, placed }
            }
        }
        NavPhase::Flying => {
            if i.region_state == RegionState::Globe {
                NavPhase::Globe
            } else if i.backspace {
                leave(i.region_centre)
            } else if let Some(change) = i.time_change {
                match change.carried {
                    Some(era_dir) => {
                        actions.push(NavAction::Request(RegionRequested {
                            era_dir,
                            snapshot: change.to,
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
                    None => leave(i.region_centre),
                }
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
                return (NavPhase::Globe, actions);
            }
            if i.backspace {
                return (leave(i.region_centre), actions);
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
                        }));
                    }
                    None => return (leave(i.region_centre), actions),
                }
            }
            // Ready once the job has generated the region asked for and it is the active one.
            // `RegionGenerated` comes with every finished region: a request back at the
            // snapshot shown ends in `RegionEntered`, not `RegionSnapshotChanged` (Task 5's
            // semantics, kept), so neither of those alone will do.
            let ready = generated && region_is(i, requested, to);
            // Held at full cover until the camera moves; it moves at the midpoint once ready.
            let fade = fade.advance(i.dt_s, !placed);
            let placed = placed || {
                let swap = ready && fade.swapped();
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
        NavPhase::Leaving { fade, centre, left } => {
            let fade = fade.advance(i.dt_s, !left);
            let left = left || {
                let swap = fade.swapped();
                if swap {
                    actions.push(NavAction::Leave);
                    if centre.0 != DVec3::ZERO {
                        actions.push(NavAction::CentreGlobe(centre));
                    }
                }
                swap
            };
            if fade.done() {
                NavPhase::Globe
            } else {
                NavPhase::Leaving { fade, centre, left }
            }
        }
    };
    (next, actions)
}

// ---------------------------------------------------------------------------------------------
// The shell

/// The navigation state: the phase, the last press (for double-clicks), when the current
/// generation started (for the caption) and when the fallback notice started.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Navigation {
    pub phase: NavPhase,
    pub last_press: Option<(f64, Vec2)>,
    pub generating_since: Option<f64>,
    pub fallback_since: Option<f64>,
}

impl Default for Navigation {
    fn default() -> Self {
        Navigation {
            phase: NavPhase::Globe,
            last_press: None,
            generating_since: None,
            fallback_since: None,
        }
    }
}

/// The navigation systems; the fly camera and the terrain scene order themselves after them.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NavigationSystems;

/// The "Descending..." caption.
#[derive(Component)]
pub struct DescendingCaption;

/// The "terrain detail unavailable here" notice.
#[derive(Component)]
pub struct FallbackNotice;

/// `Backspace` leaves a region.
pub const LEAVE_KEY: KeyCode = KeyCode::Backspace;

pub struct RegionNavigationPlugin;

impl Plugin for RegionNavigationPlugin {
    fn build(&self, app: &mut App) {
        crate::clock::add_load_clock(app);
        add_cloud_density(app);
        app.init_resource::<Navigation>()
            .init_resource::<TerrainSettings>()
            .init_resource::<GlobeFlightState>()
            .init_resource::<FlyState>()
            .add_systems(Startup, spawn_notices)
            .add_systems(
                Update,
                navigate
                    .in_set(NavigationSystems)
                    // After the region job, so a region it inserts (through commands) is seen
                    // in the same frame; before the terrain, so camera and terrain move together.
                    .after(RegionSystems)
                    .before(TerrainSceneSystems)
                    .before(GlobeFlightSystems::Step),
            );
    }
}

fn spawn_notices(mut commands: Commands) {
    commands
        .spawn((
            Name::new("Descending caption"),
            DescendingCaption,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: percent(40),
                justify_content: JustifyContent::Center,
                display: Display::None,
                ..default()
            },
        ))
        .with_child((
            Text::new(CAPTION_TEXT),
            TextFont::from_font_size(24.0),
            TextColor(TEXT_COLOR.with_alpha(0.85)),
            Pickable::IGNORE,
        ));
    // Short and out of the way: near the top, smaller and fainter than the caption.
    commands
        .spawn((
            Name::new("Fallback notice"),
            FallbackNotice,
            Pickable::IGNORE,
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: percent(8),
                justify_content: JustifyContent::Center,
                display: Display::None,
                ..default()
            },
        ))
        .with_child((
            Text::new(FALLBACK_NOTICE_TEXT),
            TextFont::from_font_size(16.0),
            TextColor(TEXT_COLOR.with_alpha(0.7)),
            Pickable::IGNORE,
        ));
}

/// The globe direction under the cursor, from the globe camera.
fn cursor_globe_dir(
    windows: &Query<&Window, With<PrimaryWindow>>,
    cameras: &Query<(&Camera, &GlobalTransform), With<GlobeCamera>>,
) -> Option<RenderDir> {
    let cursor = windows.single().ok()?.cursor_position()?;
    let (camera, transform) = cameras.single().ok()?;
    let ray = camera.viewport_to_world(transform, cursor).ok()?;
    globe_hit(ray)
}

/// Ranks [`CrustModel::resolve`] candidates at a deep snapshot exactly as the globe draws them
/// (ruling R19): the renderer's ranking ([`candidate_land_m`]) over that snapshot's PaleoDEM
/// bracket, read from its cube frames.
pub struct SurfaceRanker {
    sampling: DeepSampling,
    rotations: Vec<PlateRotations>,
    frames: [Arc<Grid>; 2],
}

impl SurfaceRanker {
    /// The ranker at `age`, or `None` in the geographic frame (no ranking: the carry is the
    /// identity there, or the geographic surface is common to every candidate) or when a frame
    /// is not at hand. `frame(slot)` gives a decoded frame file by image slot.
    pub fn new(
        timeline: &Timeline,
        crust: &CrustModel,
        age: YearsBp,
        mut frame: impl FnMut(u32) -> Option<Arc<Grid>>,
    ) -> Option<SurfaceRanker> {
        if geographic(age) {
            return None;
        }
        let bracket = match timeline.select_frames(age.myr()) {
            FrameSelection::Recent(_) => return None,
            FrameSelection::Deep(deep) | FrameSelection::Bridge { deep, .. } => deep,
        };
        let sampling = DeepSampling::from_bracket(timeline, age.myr(), &bracket);
        let slot = |e: usize| timeline.frames()[sampling.endpoints[e].frame].image_slot;
        let frames = [frame(slot(0))?, frame(slot(1))?];
        let table = crust.rotations();
        let rotations = (0..table.plate_ids.len() as u32)
            .map(|p| plate_rotations(table, PlateIndex(p), &sampling))
            .collect();
        Some(SurfaceRanker {
            sampling,
            rotations,
            frames,
        })
    }

    /// The ranking surface of `candidate` at `era_dir`: what the resolve callback returns.
    pub fn rank(
        &self,
        crust: &CrustModel,
        candidate: &Candidate,
        era_dir: RenderDir,
    ) -> Option<f64> {
        candidate_land_m(
            crust,
            candidate,
            era_dir,
            &self.sampling,
            &self.rotations,
            |e, d| cube_frame_sample(&self.frames[e], d).elevation_m,
        )
        .map(ranking_surface_km)
    }
}

/// The time change to act on, if the time differs from the snapshot the view expects.
fn gather_time_change(
    phase: &NavPhase,
    time: YearsBp,
    region: Option<&ActiveRegion>,
    fly: &FlyState,
    globe: Option<&GlobeData>,
    frames: Option<&FrameCache>,
) -> Option<TimeChange> {
    let region = region?;
    let snapshot = region.grid.spec.snapshot;
    let expected = match *phase {
        NavPhase::Flying => snapshot,
        // Only while the region shown is still the old one: the camera's position is in its
        // frame.
        NavPhase::ChangingSnapshot {
            to,
            requested,
            placed: false,
            ..
        } if !same_region(region.frame.center, snapshot, requested, to) => to,
        _ => return None,
    };
    if time == expected {
        return None;
    }
    let (x, z) = (f64::from(fly.pos_m.x), f64::from(fly.pos_m.z));
    let ground = blended_height_m(&region.grid, x, z).max(0.0);
    let camera_dir = region.frame.local_to_dir(x, z);
    let carried = globe.and_then(|g| {
        let ranker = SurfaceRanker::new(&g.timeline, &g.crust, snapshot, |slot| {
            frames.and_then(|f| f.peek(slot))
        });
        if ranker.is_none() && !geographic(snapshot) {
            debug!(
                snapshot_years = snapshot.0,
                "the snapshot's frames are not cached; overlaps carry with the lowest plate ID"
            );
        }
        carry_crust_position(&g.crust, camera_dir, snapshot, time, |c| {
            ranker
                .as_ref()
                .and_then(|r| r.rank(&g.crust, c, camera_dir))
        })
    });
    Some(TimeChange {
        to: time,
        carried,
        agl_m: fly.pos_m.y - ground,
    })
}

/// The pointer and keys navigation reads.
#[derive(SystemParam)]
struct NavInput<'w, 's> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    mouse: Option<Res<'w, ButtonInput<MouseButton>>>,
    over_ui: Option<Res<'w, PointerOverUi>>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    globe_cameras: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<GlobeCamera>>,
}

/// The caption and notice nodes.
#[derive(SystemParam)]
struct NavUi<'w, 's> {
    caption: Query<'w, 's, &'static mut Node, (With<DescendingCaption>, Without<FallbackNotice>)>,
    notice: Query<'w, 's, &'static mut Node, (With<FallbackNotice>, Without<DescendingCaption>)>,
}

/// Shows or hides a UI node.
fn show_node(node: &mut Mut<Node>, shown: bool) {
    let display = if shown { Display::Flex } else { Display::None };
    if node.display != display {
        node.display = display;
    }
}

/// Gathers input, events and the region; decides with [`step_nav`]; applies the actions and
/// the phase's view (which camera, the cloud, the caption, the descent's ease).
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn navigate(
    mut nav: ResMut<Navigation>,
    real: Res<Time<Real>>,
    clock: Res<LoadClock>,
    region_state: Res<State<RegionState>>,
    input: NavInput,
    globe: Option<Res<GlobeData>>,
    region: Option<Res<ActiveRegion>>,
    frames: Option<Res<FrameCache>>,
    mut events: (
        MessageReader<RegionEntered>,
        MessageReader<RegionGenerated>,
        MessageReader<RegionSnapshotChanged>,
    ),
    mut writers: (
        MessageWriter<RegionRequested>,
        MessageWriter<RegionLeft>,
        MessageWriter<TimeChanged>,
    ),
    mut time: ResMut<GlobeTime>,
    mut fly: ResMut<FlyState>,
    mut flight: ResMut<GlobeFlightState>,
    mut terrain: ResMut<TerrainSettings>,
    mut ui: NavUi,
    (large_budget, mut cloud): (Option<ResMut<LargeGenBudget>>, ResMut<CloudDensity>),
) {
    let now_s = real.elapsed_secs_f64();
    let dt_s = real.delta_secs();
    let state = *region_state.get();

    // Gather.
    let mut double_click = None;
    if nav.phase == NavPhase::Globe
        && let Some(mouse) = &input.mouse
        && mouse.just_pressed(MouseButton::Left)
        && !input.over_ui.as_ref().is_some_and(|o| o.0)
        && let Some(pos) = input
            .windows
            .single()
            .ok()
            .and_then(Window::cursor_position)
    {
        if is_double_click(nav.last_press, now_s, pos) {
            nav.last_press = None;
            double_click = cursor_globe_dir(&input.windows, &input.globe_cameras);
        } else {
            nav.last_press = Some((now_s, pos));
        }
    }
    let entered = events.0.read().count() > 0;
    let generated = events.1.read().last().copied();
    let changed = events.2.read().count() > 0;
    let inputs = NavInputs {
        now_s,
        load_s: clock.elapsed_s,
        dt_s,
        region_state: state,
        time: time.years,
        double_click,
        backspace: input
            .keys
            .as_ref()
            .is_some_and(|k| k.just_pressed(LEAVE_KEY)),
        entered,
        generated: generated.map(|g| (g.spec.center, g.spec.snapshot)),
        region_centre: region.as_ref().map(|r| r.frame.center),
        region_snapshot: region.as_ref().map(|r| r.grid.spec.snapshot),
        time_change: gather_time_change(
            &nav.phase,
            time.years,
            region.as_deref(),
            &fly,
            globe.as_deref(),
            frames.as_deref(),
        ),
    };
    nav.generating_since = match (state, nav.generating_since) {
        (RegionState::Generating, None) => Some(now_s),
        (RegionState::Generating, since) => since,
        _ => None,
    };
    let appeared = (entered || changed).then(|| generated.is_some_and(|g| g.fallback));
    nav.fallback_since = fallback_since(nav.fallback_since, appeared, now_s);

    // Decide.
    let before = nav.phase;
    let (phase, actions) = step_nav(before, &inputs);

    // Apply.
    let (requests, left, changes) = &mut writers;
    for action in actions {
        apply_action(
            action,
            region.as_deref(),
            &mut time,
            &mut fly,
            &mut flight,
            requests,
            left,
            changes,
        );
    }
    if std::mem::discriminant(&phase) != std::mem::discriminant(&before) {
        info!(from = ?before, to = ?phase, "navigation phase");
    }
    nav.phase = phase;
    if let NavPhase::Descending { target, .. } = phase {
        let next = ease_flight_toward(flight.0, target.0, NEAR_GLOBE_ALTITUDE_KM, f64::from(dt_s));
        if next != flight.0 {
            flight.0 = next;
        }
    }
    let shown = phase.region_shown();
    if terrain.visible != shown {
        terrain.visible = shown;
    }
    if let Some(mut large) = large_budget
        && large.0 != phase.allows_large_gen_budget()
    {
        large.0 = phase.allows_large_gen_budget();
    }
    let hold = matches!(phase, NavPhase::ChangingSnapshot { placed: false, .. });
    if terrain.hold_swap != hold {
        terrain.hold_swap = hold;
    }
    cloud.raise(phase.cloud_density());
    let leaving = matches!(phase, NavPhase::Leaving { .. });
    let show_caption = !leaving && caption_shown(nav.generating_since.map(|since| now_s - since));
    for mut node in &mut ui.caption {
        show_node(&mut node, show_caption);
    }
    let region_up = !leaving && phase != NavPhase::Globe;
    let show_notice = fallback_notice_shown(nav.fallback_since, now_s, region_up);
    for mut node in &mut ui.notice {
        show_node(&mut node, show_notice);
    }
}

#[expect(clippy::too_many_arguments, reason = "the shell's writers")]
#[instrument(level = "debug", skip_all, fields(?action))]
fn apply_action(
    action: NavAction,
    region: Option<&ActiveRegion>,
    time: &mut GlobeTime,
    fly: &mut FlyState,
    flight: &mut GlobeFlightState,
    requests: &mut MessageWriter<RegionRequested>,
    left: &mut MessageWriter<RegionLeft>,
    changes: &mut MessageWriter<TimeChanged>,
) {
    match action {
        NavAction::SnapTime(to) => {
            let from = time.years;
            time.years = to;
            changes.write(TimeChanged {
                from,
                to,
                cause: TimeChangeCause::Snap,
            });
        }
        NavAction::Request(request) => {
            requests.write(request);
        }
        NavAction::Leave => {
            left.write(RegionLeft);
        }
        NavAction::PlaceAtStart => {
            if let Some(region) = region {
                *fly = FlyState::start(&region.grid);
            }
        }
        NavAction::PlaceAfterChange { agl_m } => {
            if let Some(region) = region {
                let ground = blended_height_m(&region.grid, 0.0, 0.0).max(0.0);
                let y = (ground + agl_m)
                    .min(CAMERA_CEILING_M)
                    .max(ground + CAMERA_CLEARANCE_M);
                fly.pos_m = Vec3::new(0.0, y, 0.0);
            }
        }
        NavAction::CentreGlobe(dir) => {
            let (lat, lon) = geo_to_latlon(render_to_geo(dir));
            flight.0 = GlobeFlight {
                altitude_km: NEAR_GLOBE_ALTITUDE_KM,
                ..GlobeFlight::start(lat, lon)
            };
        }
    }
}

#[cfg(test)]
mod tests;
