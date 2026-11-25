//! Diving into regions, climbing out of them, and snapshot changes inside one: zoom-and-swap
//! (M3 design §6, §7 and §9; M2 design §6.2; parent spec §5.2, §6.2 and §11).
//!
//! The decision is [`phase`]'s pure [`step_nav`]; this module is the shell. The systems gather →
//! decide → apply:
//!
//! - **Gather** ([`navigate`]): the region state and events, the globe's and the region's fly
//!   (the region's carried onto the globe by [`fly_on_globe`]), `Backspace`, and a time change
//!   while flying ([`carry_crust_position`] carries the camera's crust to the new era, ranked as
//!   the globe draws it, [`SurfaceRanker`]).
//! - **Decide** with [`step_nav`].
//! - **Apply** the [`NavAction`]s to [`FlyState`] and [`GlobeFlightState`] (the placements
//!   reset the chase cameras' smoothing and the globe's drag offset, so neither drifts after a
//!   swap), write `RegionRequested` and `RegionLeft`, and raise the cloud
//!   ([`NavPhase::cloud_density`], from the state after the actions).
//! - **The slider** waits during a transition ([`hold_slider`]): a change is queued in
//!   [`Navigation::slider`] and the time put back, then applied once on return to flying or the
//!   globe.
//! - **The hop** ([`NavPhase::Hopping`]): the fly's velocity is measured from its motion between
//!   frames (a jump faster than it can fly, [`PLACEMENT_MPS`], is a placement and counts as
//!   none). The neighbour is requested with `RegionRequested { purpose: Neighbour }` and
//!   dropped with `RegionPreloadCancelled`; the swap writes `RegionSwapRequested` and carries the
//!   fly in the same frame, and the region swap ([`RegionSwapSystems`]) runs right after
//!   navigation, before the terrain scene and the fly camera, so that frame draws the new
//!   region with the fly in it, under the opaque cloud.
//! - **"Descending..."** shows only after a region has been generating for
//!   [`CAPTION_DELAY_S`].
//! - **"terrain detail unavailable here"** ([`FALLBACK_NOTICE_TEXT`]) shows for
//!   [`FALLBACK_NOTICE_S`] after a region appears (`RegionEntered` or `RegionSnapshotChanged`)
//!   whose `RegionGenerated` says it fell back (a tile it needs is missing or corrupt, so part of
//!   it uses globe-resolution data; the job logged a `warn`).
//!
//! **Only one world draws at a time**: `TerrainSettings::visible` follows
//! [`NavPhase::region_shown`], and the cameras switch (`crate::terrain::RegionViewSystems`)
//! before either world writes the fly's pose, so on a swap frame the pose and the active camera
//! belong to the same world.

use std::sync::Arc;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::altitudes::GLOBE_MAX_ALTITUDE_KM;
use timefly_core::flight::globe::{GLOBE_MIN_ALTITUDE_KM, GlobeFlight};
use timefly_core::flight::hop::{carry_pose, velocity_at_edges};
use timefly_core::formats::Grid;
use timefly_core::geo::RenderDir;
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
    ActiveRegion, LargeGenBudget, PendingRegion, RegionEntered, RegionGenerated, RegionLeft,
    RegionPreloadCancelled, RegionPreloadFailed, RegionRequested, RegionSnapshotChanged,
    RegionState, RegionSwapRequested, RegionSwapSystems, RegionSystems, RequestPurpose,
};
use crate::clock::LoadClock;
use crate::cloud::{CloudDensity, add_cloud_density};
use crate::data::GlobeData;
use crate::fly_camera::{
    BOOST, CAMERA_CEILING_M, CAMERA_CLEARANCE_M, FlyChase, FlyState, FlySystems, MAX_SPEED_MPS,
    fly_speed, keys_to_input,
};
use crate::globe_flight::{GlobeChase, GlobeFlightState, GlobeFlightSystems, GlobeLook};
use crate::terrain::{TerrainScene, TerrainSceneSystems, TerrainSettings};
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged};
use crate::ui::TEXT_COLOR;

mod phase;

pub use phase::{
    CLIMB_OUT_M, DIVE_DRIFT_S, EXPECTED_GEN_S, Fade, FlyControls, HOLD_DRIFT, HOLD_MAX_S, HopEdge,
    NavAction, NavInputs, NavPhase, SNAPSHOT_FADE_S, START_TIMEOUT_S, SliderHold,
    TURN_BACK_INSIDE_M, TimeChange, World, dive_drift_mps, emerge_altitude_km, emerge_density,
    emerge_height_m, fade_alpha, fly_on_globe, gather_density, gather_elapsed_for, hold_slider,
    region_yaw, same_region, step_nav,
};

/// "Descending..." shows only once a region has been generating this long (design §6.1).
pub const CAPTION_DELAY_S: f64 = 1.0;
/// The caption's text (the built-in font has no ellipsis glyph).
pub const CAPTION_TEXT: &str = "Descending...";
/// The notice for a region that fell back to globe-resolution data (Review Focus 2).
pub const FALLBACK_NOTICE_TEXT: &str = "terrain detail unavailable here";
/// How long the fallback notice shows after the region appears.
pub const FALLBACK_NOTICE_S: f64 = 3.0;
/// The region's fly never flies faster than this (twice its boosted cap): a faster jump between
/// frames is a placement (a swap, a scripted pose), not flight, and gives no velocity.
pub const PLACEMENT_MPS: f32 = 2.0 * BOOST * MAX_SPEED_MPS;

/// The ICE-6G_C frames, the recent globe's geographic frame, end at 26 ka BP (1950), 26,076
/// years before the game's present. Up to that age a direction is geographic: crust positions
/// use the present-day plate orientation, with validity still at the age (parent spec §5.2).
/// Past it they use the era's rotation. (The short bridge above it, to 30 ka, crossfades two
/// frames that differ by under 2 km of plate motion.)
pub const GEOGRAPHIC_FRAME_MAX_YEARS: f64 = 26_076.0;

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
/// region is up (`region_up`: shown and not climbing out).
pub fn fallback_notice_shown(since_s: Option<f64>, now_s: f64, region_up: bool) -> bool {
    region_up && since_s.is_some_and(|since| (0.0..FALLBACK_NOTICE_S).contains(&(now_s - since)))
}

// ---------------------------------------------------------------------------------------------
// The shell

/// The navigation state: the phase, when the current generation started (for the caption),
/// when the fallback notice started, the slider's hold, and the cloud density navigation raised
/// last.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct Navigation {
    pub phase: NavPhase,
    pub generating_since: Option<f64>,
    pub fallback_since: Option<f64>,
    /// The time a transition holds and a slider change waiting for its end (design §9).
    pub slider: SliderHold,
    /// The cloud's density from navigation in the last frame it ran.
    pub density: f32,
    /// The region's fly's `(x, z)` when navigation last ran, after its placements: its motion
    /// since gives its velocity.
    pub last_fly_m: Option<Vec2>,
}

impl Navigation {
    /// A slider change waiting for the transition to end.
    pub fn queued_time(&self) -> Option<YearsBp> {
        self.slider.queued
    }
}

impl Default for Navigation {
    fn default() -> Self {
        Navigation {
            phase: NavPhase::Globe,
            generating_since: None,
            fallback_since: None,
            slider: SliderHold::default(),
            density: 0.0,
            last_fly_m: None,
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

/// `Backspace` climbs out of a region (or out of a dive).
pub const LEAVE_KEY: KeyCode = KeyCode::Backspace;

pub struct RegionNavigationPlugin;

impl Plugin for RegionNavigationPlugin {
    fn build(&self, app: &mut App) {
        crate::clock::add_load_clock(app);
        add_cloud_density(app);
        app.init_resource::<Navigation>()
            .init_resource::<TerrainSettings>()
            .init_resource::<GlobeFlightState>()
            .init_resource::<GlobeLook>()
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
            )
            // A hop's swap lands in the frame navigation asks for it: before the terrain scene
            // and either fly moves.
            .configure_sets(
                Update,
                RegionSwapSystems
                    .after(NavigationSystems)
                    .before(TerrainSceneSystems)
                    .before(FlySystems::Step)
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

/// The two flies and the chase state their placements reset.
#[derive(SystemParam)]
struct Flies<'w> {
    fly: ResMut<'w, FlyState>,
    flight: ResMut<'w, GlobeFlightState>,
    look: ResMut<'w, GlobeLook>,
    fly_chase: Option<ResMut<'w, FlyChase>>,
    globe_chase: Option<ResMut<'w, GlobeChase>>,
}

/// What navigation writes.
#[derive(SystemParam)]
struct NavWriters<'w> {
    requests: MessageWriter<'w, RegionRequested>,
    left: MessageWriter<'w, RegionLeft>,
    cancels: MessageWriter<'w, RegionPreloadCancelled>,
    swaps: MessageWriter<'w, RegionSwapRequested>,
    changes: MessageWriter<'w, TimeChanged>,
    terrain: ResMut<'w, TerrainSettings>,
    large_budget: Option<ResMut<'w, LargeGenBudget>>,
    cloud: ResMut<'w, CloudDensity>,
}

/// What navigation reads of the region and the globe.
#[derive(SystemParam)]
struct NavWorld<'w> {
    region_state: Res<'w, State<RegionState>>,
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    globe: Option<Res<'w, GlobeData>>,
    region: Option<Res<'w, ActiveRegion>>,
    frames: Option<Res<'w, FrameCache>>,
    /// The terrain on screen: during a snapshot change (or a climb out of one) it can still be
    /// the old region after `ActiveRegion` became the new one, and the fly's local position is
    /// in its frame.
    scene: Option<Res<'w, TerrainScene>>,
    /// A neighbour ready for a hop.
    pending: Option<Res<'w, PendingRegion>>,
}

/// Sets the globe time to `to`, with a [`TimeChanged`] of `cause`, if it differs.
fn set_time(
    time: &mut GlobeTime,
    to: YearsBp,
    cause: TimeChangeCause,
    changes: &mut MessageWriter<TimeChanged>,
) {
    if time.years != to {
        let from = time.years;
        time.years = to;
        changes.write(TimeChanged { from, to, cause });
    }
}

/// Gathers input, events and the region; decides with [`step_nav`]; applies the actions and
/// the phase's view (which camera, the cloud, the caption).
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn navigate(
    mut nav: ResMut<Navigation>,
    real: Res<Time<Real>>,
    clock: Res<LoadClock>,
    world: NavWorld,
    mut events: (
        MessageReader<RegionEntered>,
        MessageReader<RegionGenerated>,
        MessageReader<RegionSnapshotChanged>,
        MessageReader<RegionPreloadFailed>,
    ),
    mut time: ResMut<GlobeTime>,
    mut flies: Flies,
    mut out: NavWriters,
    mut ui: NavUi,
) {
    let now_s = real.elapsed_secs_f64();
    let dt_s = real.delta_secs();
    let state = *world.region_state.get();
    let region = world.region.as_deref();
    // The terrain drawn, which the fly's local position is in.
    let shown_grid = world
        .scene
        .as_deref()
        .map(|s| &s.grid)
        .or(region.map(|r| &r.grid));
    let shown_frame = shown_grid.map(|g| g.spec.frame());
    let p = flies.fly.pos_m;
    let ground_m = shown_grid.map_or(0.0, |g| {
        blended_height_m(g, f64::from(p.x), f64::from(p.z)).max(0.0)
    });

    // The slider waits during a transition: a change since last frame is queued and put back.
    let (held, slider) = hold_slider(nav.phase.holds_time(), time.years, nav.slider);
    set_time(&mut time, held, TimeChangeCause::Held, &mut out.changes);
    nav.slider = slider;

    // Gather.
    let entered = events.0.read().count() > 0;
    let generated = events.1.read().last().copied();
    let changed = events.2.read().count() > 0;
    let flight = flies.flight.0;
    let pending = world.pending.as_deref().and_then(|p| p.0.as_ref());
    let xz = Vec2::new(p.x, p.z);
    let measured = match nav.last_fly_m {
        Some(last) if dt_s > 0.0 => Some((xz - last) / dt_s),
        _ => None,
    }
    .filter(|v| v.length() <= PLACEMENT_MPS)
    .unwrap_or(Vec2::ZERO);
    // Held at the field's edge (`FlyControls::confined`), the fly does not move out of it: what
    // the player asks for counts there on that axis, so pressing out of the edge, straight or at
    // an angle (sliding along it), still starts a hop.
    let wish = world.keys.as_deref().map_or(Vec2::ZERO, |k| {
        wish_mps(&keys_to_input(k), flies.fly.yaw, p.y - ground_m)
    });
    let velocity = velocity_at_edges(xz, measured, wish);
    let inputs = NavInputs {
        now_s,
        load_s: clock.elapsed_s,
        dt_s,
        region_state: state,
        time: time.years,
        climb_key: world
            .keys
            .as_ref()
            .is_some_and(|k| k.just_pressed(LEAVE_KEY)),
        entered,
        generated: generated.map(|g| (g.spec.center, g.spec.snapshot)),
        region_centre: region.map(|r| r.frame.center),
        region_snapshot: region.map(|r| r.grid.spec.snapshot),
        time_change: gather_time_change(
            &nav.phase,
            time.years,
            region,
            &flies.fly,
            world.globe.as_deref(),
            world.frames.as_deref(),
        ),
        globe_altitude_km: flight.altitude_km,
        globe_dir: RenderDir(flight.dir),
        globe_heading: flight.heading,
        region_height_m: flies.fly.pos_m.y,
        shown_density: nav.density,
        region_ground_m: ground_m,
        region_pos_m: xz,
        region_vel_mps: velocity,
        neighbour: pending.map(|r| (r.frame.center, r.grid.spec.snapshot)),
        preload_failed: events.3.read().count() > 0,
        vertical_held: world
            .keys
            .as_deref()
            .is_some_and(|k| keys_to_input(k).move_local.y != 0.0),
        time_queued: nav.slider.queued.is_some(),
        region_fly: shown_frame.map(|frame| {
            fly_on_globe(
                &frame,
                f64::from(flies.fly.pos_m.x),
                f64::from(flies.fly.pos_m.z),
                flies.fly.yaw,
            )
        }),
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
    let ease = Ease {
        region_shown: phase.region_shown(),
        dt_s,
        ground_m,
    };
    for action in actions {
        apply_action(
            action, ease, region, pending, &mut time, &mut flies, &mut out,
        );
    }
    if std::mem::discriminant(&phase) != std::mem::discriminant(&before) {
        info!(from = ?before, to = ?phase, "navigation phase");
    }
    nav.phase = phase;
    // Entering a transition holds the time it starts at; leaving one applies a queued change.
    let (shown_time, slider) = hold_slider(phase.holds_time(), time.years, nav.slider);
    set_time(
        &mut time,
        shown_time,
        TimeChangeCause::Held,
        &mut out.changes,
    );
    nav.slider = slider;

    let shown = phase.region_shown();
    if out.terrain.visible != shown {
        out.terrain.visible = shown;
    }
    // A snapshot change holds the new terrain back until the camera moves; a climb never shows
    // it (a change left mid-way finishes behind the climb), so only one world swap happens,
    // under the opaque cloud.
    let hold = matches!(
        phase,
        NavPhase::ChangingSnapshot { placed: false, .. } | NavPhase::Climbing { .. }
    );
    if out.terrain.hold_swap != hold {
        out.terrain.hold_swap = hold;
    }
    // The cloud from the state after the actions: a swap frame is judged in the new world.
    let after = NavInputs {
        globe_altitude_km: flies.flight.0.altitude_km,
        region_height_m: flies.fly.pos_m.y,
        ..inputs
    };
    let density = phase.cloud_density(&after);
    out.cloud.raise(density);
    let fly_xz = Vec2::new(flies.fly.pos_m.x, flies.fly.pos_m.z);
    if nav.last_fly_m != Some(fly_xz) {
        nav.last_fly_m = Some(fly_xz);
    }
    if nav.density != density {
        nav.density = density;
    }
    let large = phase.allows_large_gen_budget(&after);
    if let Some(mut budget) = out.large_budget.take()
        && budget.0 != large
    {
        budget.0 = large;
    }
    let climbing = matches!(phase, NavPhase::Climbing { .. });
    let show_caption = !climbing && caption_shown(nav.generating_since.map(|since| now_s - since));
    for mut node in &mut ui.caption {
        show_node(&mut node, show_caption);
    }
    let show_notice = fallback_notice_shown(nav.fallback_since, now_s, shown && !climbing);
    for mut node in &mut ui.notice {
        show_node(&mut node, show_notice);
    }
}

/// What [`NavAction::EaseFly`] moves: the world on screen after this frame's decision, over the
/// frame time, above the ground drawn under the region's fly.
#[derive(Copy, Clone, Debug)]
struct Ease {
    region_shown: bool,
    dt_s: f32,
    ground_m: f32,
}

/// Applies one action. `pending` is the neighbour ready for a hop's swap.
#[instrument(level = "debug", skip_all, fields(?action))]
fn apply_action(
    action: NavAction,
    ease: Ease,
    region: Option<&ActiveRegion>,
    pending: Option<&ActiveRegion>,
    time: &mut GlobeTime,
    flies: &mut Flies,
    out: &mut NavWriters,
) {
    match action {
        NavAction::SnapTime(to) => {
            set_time(time, to, TimeChangeCause::Snap, &mut out.changes);
        }
        NavAction::Request(request) => {
            out.requests.write(request);
        }
        NavAction::Leave | NavAction::CancelRequest => {
            out.left.write(RegionLeft);
        }
        NavAction::PlaceAtStart => {
            if let Some(region) = region {
                *flies.fly = FlyState::start(&region.grid);
            }
            reset_fly_chase(flies);
        }
        NavAction::PlaceAfterChange { agl_m } => {
            if let Some(region) = region {
                let ground = blended_height_m(&region.grid, 0.0, 0.0).max(0.0);
                let y = (ground + agl_m)
                    .min(CAMERA_CEILING_M)
                    .max(ground + CAMERA_CLEARANCE_M);
                flies.fly.pos_m = Vec3::new(0.0, y, 0.0);
            }
        }
        NavAction::PlaceInRegion { yaw, height_m } => {
            *flies.fly = FlyState {
                pos_m: Vec3::new(0.0, height_m, 0.0),
                yaw,
                ..default()
            };
            reset_fly_chase(flies);
        }
        NavAction::PlaceOnGlobe {
            dir,
            heading,
            altitude_km,
        } => {
            let dir = dir.0.normalize();
            let tangent = heading - dir * dir.dot(heading);
            let heading = tangent
                .try_normalize()
                .unwrap_or_else(|| GlobeFlight::start(0.0, 0.0).heading);
            flies.flight.0 = GlobeFlight {
                dir,
                heading,
                altitude_km,
            };
            if flies.look.offset_deg != 0.0 {
                flies.look.offset_deg = 0.0;
            }
            if let Some(chase) = flies.globe_chase.as_mut() {
                chase.reset();
            }
        }
        NavAction::EaseFly { vertical_mps } => {
            let dy = vertical_mps * ease.dt_s;
            if ease.region_shown {
                let p = flies.fly.pos_m;
                let y = (p.y + dy)
                    .min(CAMERA_CEILING_M)
                    .max(ease.ground_m + CAMERA_CLEARANCE_M);
                if y != p.y {
                    flies.fly.pos_m.y = y;
                }
            } else {
                let a = flies.flight.0.altitude_km;
                let to = (a + f64::from(dy) / 1000.0)
                    .clamp(GLOBE_MIN_ALTITUDE_KM, GLOBE_MAX_ALTITUDE_KM.max(a));
                if to != a {
                    flies.flight.0.altitude_km = to;
                }
            }
        }
        NavAction::Preload(era_dir) => {
            if let Some(region) = region {
                out.requests.write(RegionRequested {
                    era_dir,
                    snapshot: region.grid.spec.snapshot,
                    purpose: RequestPurpose::Neighbour,
                });
            }
        }
        NavAction::CancelPreload => {
            out.cancels.write(RegionPreloadCancelled);
        }
        NavAction::LiftTo { height_m } => {
            let y = height_m
                .min(CAMERA_CEILING_M)
                .max(ease.ground_m + CAMERA_CLEARANCE_M);
            if y != flies.fly.pos_m.y {
                flies.fly.pos_m.y = y;
            }
        }
        NavAction::SwapToNeighbour => {
            let (Some(old), Some(new)) = (region, pending) else {
                warn!("a hop's swap with no region or no neighbour ready; ignored");
                return;
            };
            let p = flies.fly.pos_m;
            let (q, yaw) = carry_pose(&old.frame, &new.frame, p.as_dvec3(), flies.fly.yaw);
            let ground = blended_height_m(&new.grid, q.x, q.z).max(0.0);
            let y = (ground + (p.y - ease.ground_m))
                .min(CAMERA_CEILING_M)
                .max(ground + CAMERA_CLEARANCE_M);
            flies.fly.pos_m = Vec3::new(q.x as f32, y, q.z as f32);
            flies.fly.yaw = yaw;
            reset_fly_chase(flies);
            out.swaps.write(RegionSwapRequested);
            info!(
                from = ?old.grid.spec.center,
                to = ?new.grid.spec.center,
                x_m = q.x,
                z_m = q.z,
                "hop: swapped to the neighbour"
            );
        }
        NavAction::ReturnToAgl { agl_m } => {
            let y = flies.fly.pos_m.y;
            let agl = y - ease.ground_m;
            let step = fly_speed(agl) * ease.dt_s;
            let to = (y + (agl_m - agl).clamp(-step, step))
                .min(CAMERA_CEILING_M)
                .max(ease.ground_m + CAMERA_CLEARANCE_M);
            if to != y {
                flies.fly.pos_m.y = to;
            }
        }
        NavAction::TurnBack { pos_m, yaw } => {
            let fly = &mut flies.fly;
            fly.pos_m.x = pos_m.x;
            fly.pos_m.z = pos_m.y;
            // Facing into the field along the hop's axis (not `yaw + π`: a fly already facing in,
            // or along the edge, would face out).
            fly.yaw = yaw;
            if let Some(region) = region {
                let ground =
                    blended_height_m(&region.grid, f64::from(pos_m.x), f64::from(pos_m.y)).max(0.0);
                fly.pos_m.y = fly.pos_m.y.max(ground + CAMERA_CLEARANCE_M);
            }
            reset_fly_chase(flies);
            info!(
                x_m = pos_m.x,
                z_m = pos_m.y,
                "hop: the neighbour failed; turned back"
            );
        }
        NavAction::PlaceInside { pos_m } => {
            let fly = &mut flies.fly;
            fly.pos_m.x = pos_m.x;
            fly.pos_m.z = pos_m.y;
            if let Some(region) = region {
                let ground =
                    blended_height_m(&region.grid, f64::from(pos_m.x), f64::from(pos_m.y)).max(0.0);
                fly.pos_m.y = fly.pos_m.y.max(ground + CAMERA_CLEARANCE_M);
            }
            reset_fly_chase(flies);
            info!(
                x_m = pos_m.x,
                z_m = pos_m.y,
                "hop: turned back in the cloud; the hop is off"
            );
        }
    }
}

/// The horizontal velocity (m/s, `(x, z)`) the held keys ask of the region's fly heading `yaw`,
/// `agl_m` above the ground: as [`crate::fly_camera::fly_step`] would move it, unheld.
fn wish_mps(keys: &crate::fly_camera::KeyInput, yaw: f32, agl_m: f32) -> Vec2 {
    let m = keys.move_local;
    let world = Quat::from_rotation_y(yaw) * Vec3::new(m.x, 0.0, m.z);
    let dir = Vec2::new(world.x, world.z);
    let dir = if dir.length_squared() > 1.0 {
        dir.normalize()
    } else {
        dir
    };
    let boost = if keys.boost { BOOST } else { 1.0 };
    dir * fly_speed(agl_m) * boost
}

/// Resets the region chase camera's smoothing, so it starts at the new pose.
fn reset_fly_chase(flies: &mut Flies) {
    if let Some(chase) = flies.fly_chase.as_mut() {
        chase.reset();
    }
}

#[cfg(test)]
mod tests;
