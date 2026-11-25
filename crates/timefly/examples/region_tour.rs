//! Runs the real game app with `--region`, waits for the region's terrain, saves a shaded and a
//! wireframe (`F2`) screenshot from the region camera, then exits. For visual review of the CDLOD
//! terrain (Task 7) and later the painterly look.
//!
//! Like `ui_tour`, the camera renders into an offscreen image of the window's size and that is
//! what gets saved: window-surface screenshots of a process started from an agent's shell come
//! back blank on macOS. The window itself stays empty while the tour runs.
//!
//! `cargo run -p timefly --example region_tour -- <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix]
//! [shaded|high|fly|cruise|cloud|time|cloudtime] [AGL_M]` (run through cargo, or set `BEVY_ASSET_ROOT=crates/timefly`, so the assets are
//! found). `--no-dunes`, anywhere among the arguments, turns the dunes off as the game's flag
//! does. The region defaults to the Alps at the present (`46.5,10,0`) and the prefix to
//! `cdlod`, giving `<prefix>-shaded.png` and `<prefix>-wire.png`; a fifth argument `shaded`
//! skips the wireframe shot. Exits with an error if the region never appears. Set a fresh
//! `TIMEFLY_LOG_DIR` per run.
//!
//! A sixth argument `AGL_M` places the camera that many metres above `max(ground, 0)` at the
//! region's centre instead of [`CAMERA_START_ALTITUDE_M`]. Heights outside the flight clearance
//! and absolute ceiling at that centre cause the tour to exit with an error.
//!
//! A seventh argument `LATER_S` (with `shaded`) saves a second shot, `<prefix>-later.png`, that
//! many seconds after the first, for motion such as the swash (each screenshot resolves a few
//! frames after its request, so the gap between the captured frames is about `LATER_S`).
//!
//! `tools/dev/region-shots.sh` runs it over the Task 8 set (the bench regions and one region
//! per vegetation era).
//!
//! A fifth argument `fly` replaces the wireframe shot with a scripted flight through the real
//! controls (Task 9): after the start shot it holds `W` and `Q` (forward and down) for
//! [`DIVE_SECS`], then `W` alone for [`CRUISE_SECS`], and saves `<prefix>-low.png`; then it
//! presses `[` (the next older snapshot), waits for the region to regenerate around the
//! camera's crust position and the fade to finish, and saves `<prefix>-older.png`.
//!
//! A fifth argument `high` checks a region from high up (M3 Task 6; give a large `AGL_M`): after
//! the start shot (looking north at the start pitch) it sets the steepest view pitch
//! ([`MIN_PITCH_DEG`], the chase camera looking 45° down) and saves `<prefix>-down.png`.
//!
//! A fifth argument `cruise` checks the chase camera in sustained flight (M3 Task 3): after the
//! start shot it holds `W` for [`FLIGHT_SHOT_SECS`] and saves `<prefix>-cruise.png` while still
//! flying, then holds `Shift`+`W` (boost) as long and saves `<prefix>-boost.png`. Each shot logs
//! a `region tour chase view` line with how far the fly sits below the view's centre.
//!
//! A fifth argument `cloud` checks the transition cloud (M3 Task 5): after the start shot it
//! presses `]` (the next younger snapshot; start the region at an older one) and, as the cloud
//! gathers, holds its density at 0.5 for [`PIN_FRAMES`] and saves `<prefix>-cloud-half.png`,
//! then, once the cloud is opaque, holds it at 1.0 as long and saves `<prefix>-cloud-full.png`,
//! and after the change saves `<prefix>-cloud-after.png`. A fifth argument `cloudtime` is `time`
//! with the cloud held at density 0.5 throughout, so the two read the cloud pass's cost as a
//! difference.
//!
//! `--offscreen`, anywhere among the arguments, makes `time` and `cloudtime` render into the
//! offscreen image as the other modes do: a window the compositor is not showing (behind
//! others, on another Space) gets no main pass at all, only the shadow and compute passes, so
//! GPU costs read from a trace (Metal System Trace) are only real offscreen.
//!
//! A fifth argument `dive` checks the dive and the climb through the cloud (M3 Task 7). The game
//! starts on the globe (no launch region), the globe camera renders into the offscreen image
//! too, and the tour puts the fly over `LAT,LON` at `AGE_YEARS`, [`ABOVE_CLOUD_KM`] up, just
//! above the cloud: `<prefix>-globe-above.png`. Then it holds `Ctrl` (descend) through the cloud
//! top and the dive altitude, and saves `<prefix>-dive-cloud.png` in the white-out over the
//! globe and `<prefix>-dive-swap.png` from the swap frame, on the region's side. Once the fly
//! has emerged below the cloud base and the region has settled: `<prefix>-region-below.png`.
//! Then it holds `Space` (climb) up through the cloud base, saving `<prefix>-climb-cloud.png`
//! (the region in the white-out) and `<prefix>-climb-swap.png` (the globe's side of the swap),
//! and once the fly has climbed out over the globe, `<prefix>-globe-after.png`. A fifth argument
//! `divekey` climbs out with `Backspace` instead. Each step logs a `region tour dive` line.
//!
//! A fifth argument `lowclimb` checks the opaque cloud against near ground (M3 Task 7, fix
//! round 1): after the start shot (give a low `AGL_M`, facing a ridge) it presses `Backspace`,
//! and as the cloud gathers for the climb out holds its density at 0.5 for [`PIN_FRAMES`] and
//! saves `<prefix>-climb-05.png` (mid-gather: near ground veiled, never sharp in front of the
//! cloud), then at 0.995 (`<prefix>-climb-0995.png`) and at 1.0 (`<prefix>-climb-opaque.png`,
//! what the frame before the swap draws: everything but the fly covered). Then it lets the climb finish onto the globe.
//!
//! A fifth argument `time` measures frame times instead of taking shots (art pass, ruling R8: the
//! region bench never flies over a sand sea). The region camera draws to the window, vsync is off
//! (as in the benches), the camera is held `AGL_M` above the centre, and after the settle it
//! records [`TIME_SECS`] of frame deltas and logs one `region tour frame times` line (mean,
//! median, p95 and worst), then exits. Run the same pose over two scenes to read a cost as a
//! difference.

use std::path::PathBuf;

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{PresentMode, PrimaryWindow};
use timefly::args::{LaunchArgs, parse_region, parse_window_size};
use timefly::bench::frame_time_stats;
use timefly::cloud::CloudDensity;
use timefly::diagnostics;
use timefly::fly_camera::{
    CAMERA_CEILING_M, CAMERA_CLEARANCE_M, FlyState, FlySystems, MIN_PITCH_DEG,
};
use timefly::globe::{GlobeCamera, GlobeRenderStatus};
use timefly::globe_flight::{GlobeFlightState, GlobeLook};
use timefly::region::launch::launch_request;
use timefly::region::navigate::{LEAVE_KEY, World};
use timefly::region::navigate::{NavPhase, Navigation, NavigationSystems, same_region};
use timefly::region::{
    ActiveRegion, CAMERA_START_ALTITUDE_M, RegionEntered, RegionRequested, RegionState,
};
use timefly::terrain::view::RegionCamera;
use timefly::terrain::{TerrainScene, WIREFRAME_KEY};
use timefly::time::{GlobeTime, OLDER_KEY, YOUNGER_KEY};
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::altitudes::CLOUD_TOP_KM;
use timefly_core::flight::globe::GlobeFlight;

/// Seconds after the region appears before the first shot, so its pipelines compile and the
/// shadow cascades settle.
const SETTLE_SECS: f32 = 6.0;
/// Frames between a shot and the next change, and between toggling the wireframe and its shot.
const WIRE_FRAMES: u32 = 20;
/// Frames after the last shot before exiting.
const DONE_FRAMES: u32 = 240;
/// Seconds to wait for the region.
const GIVE_UP_SECS: f32 = 180.0;
/// The scripted flight: seconds diving forward and down, then cruising forward.
const DIVE_SECS: f32 = 4.5;
const CRUISE_SECS: f32 = 1.5;
/// Seconds after a snapshot change finishes before its shot (pipelines and shadows settle).
const AFTER_CHANGE_SECS: f32 = 4.0;
/// Seconds of frame deltas the `time` mode records after the settle.
const TIME_SECS: f32 = 30.0;
/// Seconds of steady flight (`cruise`) before each in-flight shot.
const FLIGHT_SHOT_SECS: f32 = 2.0;
/// Frames the `cloud` mode holds the cloud's density for each shot: the shot resolves within
/// (two or three frames), and the change goes on underneath, so the hold at full cover is still
/// to come when the half shot is done.
const PIN_FRAMES: u32 = 8;
/// The density `cloudtime` holds the cloud at.
const TIMED_CLOUD_DENSITY: f32 = 0.5;
/// The `dive` mode's first shot: the fly this far above the cloud top (km).
const ABOVE_CLOUD_KM: f64 = 300.0;
/// Frames after a swap before its shot is requested (each shot resolves a few frames later).
const SWAP_SHOT_FRAMES: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    WaitRegion,
    Settle,
    /// The shaded shot is requested; a screenshot resolves frames later, so wait before F2.
    Shaded,
    Wire,
    /// The steepest look is set (`high`); its shot follows once the chase camera has glided.
    Down,
    /// The scripted flight (`fly`): diving, then cruising.
    Dive,
    Cruise,
    /// The low shot is requested; then `[` is pressed.
    Low,
    /// Waiting for the snapshot change to finish.
    Older,
    /// Recording frame deltas (`time`).
    Timing,
    /// Sustained flight (`cruise`): plain, then boosted.
    Plain,
    Boosted,
    /// The cloud (`cloud`): gathering to half, held at half, closing, held opaque, clearing.
    CloudGather,
    CloudHalf,
    CloudClose,
    CloudFull,
    CloudClear,
    /// `lowclimb`: climbing out, the shots held below opaque and opaque; then to the globe.
    LowGather,
    LowAfter,
    Done,
}

/// What follows the start shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wire,
    Shaded,
    High,
    Fly,
    Cruise,
    Cloud,
    Time,
    /// `Backspace` from low, the cloud held just below opaque and opaque (`lowclimb`).
    LowClimb,
    /// The dive and the climb (`dive`: climbing out through the cloud base; `divekey`: with
    /// `Backspace`).
    Dive {
        backspace: bool,
    },
}

#[derive(Resource)]
struct Tour {
    out: PathBuf,
    prefix: String,
    size: (u32, u32),
    target: Option<Handle<Image>>,
    mode: Mode,
    requested: RegionRequested,
    /// Camera height above `max(ground, 0)` at the centre.
    agl_m: f32,
    /// Seconds after the shaded shot to save the `later` one (`shaded` mode only).
    later_s: Option<f32>,
    /// When the shaded shot was requested.
    shot_at: f32,
    step: Step,
    /// The snapshot change has started (`Older`).
    changing: bool,
    /// The current flight segment's shot is requested (`cruise`).
    shot_taken: bool,
    since: f32,
    frames: u32,
    left_generating: bool,
    /// Frame deltas recorded in `time` mode, milliseconds.
    frame_ms: Vec<f32>,
    /// The density the cloud is held at, if any (`cloud`, `cloudtime`).
    pin: Option<f32>,
    /// `time` renders into the offscreen image too (`--offscreen`).
    offscreen: bool,
}

fn shoot(commands: &mut Commands, tour: &Tour, name: &str) {
    let Some(target) = tour.target.clone() else {
        return;
    };
    let path = tour.out.join(format!("{}-{name}.png", tour.prefix));
    info!(path = %path.display(), "region tour screenshot");
    commands
        .spawn(Screenshot::image(target))
        .observe(save_to_disk(path));
}

/// A region or globe camera spawned this frame.
type NewWorldCamera = Or<(Added<RegionCamera>, Added<GlobeCamera>)>;

/// Points the region camera (and in `dive` mode the globe camera, which carries the UI) at an
/// offscreen image the size of the window when it appears (except in `time` mode, which draws
/// to the window as the game does).
fn render_offscreen(
    mut tour: ResMut<Tour>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<(Entity, Has<GlobeCamera>), NewWorldCamera>,
    mut commands: Commands,
) {
    if tour.mode == Mode::Time && !tour.offscreen {
        return;
    }
    let (w, h) = tour.size;
    let diving = matches!(tour.mode, Mode::Dive { .. });
    for camera in cameras.iter().filter(|(_, globe)| diving || !globe) {
        let camera = camera.0;
        let target = tour
            .target
            .get_or_insert_with(|| {
                images.add(Image::new_target_texture(
                    w,
                    h,
                    TextureFormat::Rgba8UnormSrgb,
                    None,
                ))
            })
            .clone();
        commands
            .entity(camera)
            .insert(RenderTarget::Image(target.into()));
    }
}

/// Logs where the scripted flight is, for the report.
fn log_flight(label: &str, fly: &FlyState, region: Option<&ActiveRegion>, time: &GlobeTime) {
    let ground = region.map_or(0.0, |r| {
        timefly_core::terrain::blended_height_m(
            &r.grid,
            f64::from(fly.pos_m.x),
            f64::from(fly.pos_m.z),
        )
    });
    info!(
        label,
        x_m = fly.pos_m.x,
        y_m = fly.pos_m.y,
        z_m = fly.pos_m.z,
        ground_m = ground,
        pitch_deg = fly.pitch.to_degrees(),
        years = time.years.0,
        snapshot_years = region.map(|r| r.grid.spec.snapshot.0),
        "region tour flight"
    );
}

/// Logs where the fly sits in the chase camera's view: degrees below the view's centre
/// (negative is above) and sideways.
fn log_chase_view(label: &str, camera: &Transform, fly: &FlyState) {
    let to = (fly.pos_m - camera.translation).normalize();
    let forward = camera.forward().as_vec3();
    let below_deg = (-to.dot(camera.up().as_vec3()))
        .atan2(to.dot(forward))
        .to_degrees();
    let side_deg = to.dot(camera.right().as_vec3()).asin().to_degrees();
    info!(
        label,
        below_deg,
        side_deg,
        distance_m = (fly.pos_m - camera.translation).length(),
        "region tour chase view"
    );
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn tour(
    mut tour: ResMut<Tour>,
    mut entered: MessageReader<RegionEntered>,
    scene: Option<Res<TerrainScene>>,
    state: Res<State<RegionState>>,
    real: Res<Time<Real>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    flight: (
        Res<Navigation>,
        ResMut<FlyState>,
        Option<Res<ActiveRegion>>,
        Res<GlobeTime>,
    ),
    cameras: Query<&Transform, With<RegionCamera>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let (nav, mut fly, region, time) = flight;
    let now = real.elapsed_secs();
    tour.frames += 1;
    if *state.get() == RegionState::Generating {
        tour.left_generating = true;
    }
    let timed_flight = tour.mode == Mode::Time;
    let density = nav.density;
    let on_course = *state.get() == RegionState::InRegion
        && nav.phase == NavPhase::Flying
        && region.as_ref().is_some_and(|r| {
            same_region(
                r.grid.spec.center,
                r.grid.spec.snapshot,
                tour.requested.era_dir,
                tour.requested.snapshot,
            )
        });
    if matches!(tour.step, Step::Settle | Step::Timing) {
        if timed_flight && !on_course {
            error!("region tour: left the selected region during the timing run");
            exit.write(AppExit::error());
            return;
        }
        if let Some(r) = region.as_ref() {
            let surface = timefly_core::terrain::blended_height_m(&r.grid, 0.0, 0.0).max(0.0);
            if !(CAMERA_CLEARANCE_M..=CAMERA_CEILING_M - surface).contains(&tour.agl_m) {
                error!(
                    requested_agl_m = tour.agl_m,
                    min_agl_m = CAMERA_CLEARANCE_M,
                    max_agl_m = CAMERA_CEILING_M - surface,
                    "region tour: requested height is outside the flight limits"
                );
                exit.write(AppExit::error());
                return;
            }
            fly.pos_m = Vec3::new(0.0, surface + tour.agl_m, 0.0);
        }
    }
    match tour.step {
        Step::WaitRegion => {
            let failed = tour.left_generating && *state.get() == RegionState::Globe;
            if failed || now > GIVE_UP_SECS {
                error!(failed, "region tour: the region never appeared");
                exit.write(AppExit::error());
                return;
            }
            let ready = if timed_flight {
                on_course
            } else {
                entered.read().last().is_some() || scene.is_some() && tour.target.is_some()
            };
            if ready {
                tour.step = Step::Settle;
                tour.since = now;
            }
        }
        Step::Settle => {
            if now - tour.since >= SETTLE_SECS && tour.mode == Mode::Time {
                info!(agl_m = tour.agl_m, "region tour: timing frames");
                tour.step = Step::Timing;
                tour.since = now;
                tour.frame_ms.clear();
            } else if now - tour.since >= SETTLE_SECS {
                shoot(&mut commands, &tour, "shaded");
                tour.step = Step::Shaded;
                tour.frames = 0;
                tour.shot_at = now;
            }
        }
        Step::Shaded => {
            if tour.mode == Mode::Cloud && tour.frames >= WIRE_FRAMES {
                if tour.frames == WIRE_FRAMES {
                    keys.press(YOUNGER_KEY);
                } else {
                    keys.release(YOUNGER_KEY);
                    info!("region tour: snapshot change requested");
                    tour.step = Step::CloudGather;
                    tour.since = now;
                    tour.frames = 0;
                }
            } else if tour.mode == Mode::LowClimb && tour.frames >= WIRE_FRAMES {
                if tour.frames == WIRE_FRAMES {
                    log_flight("low climb start", &fly, region.as_deref(), &time);
                    keys.press(LEAVE_KEY);
                } else {
                    keys.release(LEAVE_KEY);
                    tour.step = Step::LowGather;
                    tour.frames = 0;
                }
            } else if tour.mode == Mode::High && tour.frames >= WIRE_FRAMES {
                fly.pitch = MIN_PITCH_DEG.to_radians();
                tour.step = Step::Down;
                tour.frames = 0;
            } else if tour.mode == Mode::Cruise && tour.frames >= WIRE_FRAMES {
                tour.step = Step::Plain;
                tour.since = now;
                tour.shot_taken = false;
            } else if tour.mode == Mode::Fly && tour.frames >= WIRE_FRAMES {
                log_flight("start", &fly, region.as_deref(), &time);
                tour.step = Step::Dive;
                tour.since = now;
            } else if tour.mode == Mode::Shaded && tour.frames >= WIRE_FRAMES {
                match tour.later_s {
                    Some(later) if now - tour.shot_at < later => {}
                    Some(later) => {
                        info!(
                            seconds = now - tour.shot_at,
                            requested = later,
                            "region tour: later shot"
                        );
                        shoot(&mut commands, &tour, "later");
                        tour.later_s = None;
                        tour.frames = 0;
                    }
                    None => {
                        tour.step = Step::Done;
                        tour.frames = 0;
                    }
                }
            } else if tour.frames >= WIRE_FRAMES {
                // The real toggle: press F2 for one frame.
                keys.press(WIREFRAME_KEY);
                tour.step = Step::Wire;
                tour.frames = 0;
            }
        }
        Step::Down => {
            if tour.frames == 2 * WIRE_FRAMES {
                shoot(&mut commands, &tour, "down");
            } else if tour.frames > 3 * WIRE_FRAMES {
                tour.step = Step::Done;
                tour.frames = 0;
            }
        }
        Step::Wire => {
            keys.release(WIREFRAME_KEY);
            if tour.frames >= WIRE_FRAMES {
                shoot(&mut commands, &tour, "wire");
                tour.step = Step::Done;
                tour.frames = 0;
            }
        }
        Step::Dive => {
            keys.press(KeyCode::KeyW);
            keys.press(KeyCode::KeyQ);
            if now - tour.since >= DIVE_SECS {
                keys.release(KeyCode::KeyQ);
                log_flight("dived", &fly, region.as_deref(), &time);
                tour.step = Step::Cruise;
                tour.since = now;
            }
        }
        Step::Cruise => {
            keys.press(KeyCode::KeyW);
            if now - tour.since >= CRUISE_SECS {
                keys.release(KeyCode::KeyW);
                log_flight("low", &fly, region.as_deref(), &time);
                shoot(&mut commands, &tour, "low");
                tour.step = Step::Low;
                tour.frames = 0;
            }
        }
        Step::Plain | Step::Boosted => {
            let boosted = tour.step == Step::Boosted;
            keys.press(KeyCode::KeyW);
            if boosted {
                keys.press(KeyCode::ShiftLeft);
            }
            let name = if boosted { "boost" } else { "cruise" };
            if !tour.shot_taken && now - tour.since >= FLIGHT_SHOT_SECS {
                log_flight(name, &fly, region.as_deref(), &time);
                if let Ok(camera) = cameras.single() {
                    log_chase_view(name, camera, &fly);
                }
                shoot(&mut commands, &tour, name);
                tour.shot_taken = true;
                tour.frames = 0;
            } else if tour.shot_taken && tour.frames >= WIRE_FRAMES {
                // Still flying while the shot resolves; then the next segment.
                if boosted {
                    keys.release(KeyCode::KeyW);
                    keys.release(KeyCode::ShiftLeft);
                    tour.step = Step::Done;
                } else {
                    tour.step = Step::Boosted;
                    tour.since = now;
                    tour.shot_taken = false;
                }
                tour.frames = 0;
            }
        }
        Step::CloudGather | Step::CloudClose => {
            let (target, next, name) = if tour.step == Step::CloudGather {
                (0.5, Step::CloudHalf, "cloud-half")
            } else {
                (1.0, Step::CloudFull, "cloud-full")
            };
            if density >= target {
                info!(density, pinned = target, phase = ?nav.phase, "region tour cloud shot");
                tour.pin = Some(target);
                shoot(&mut commands, &tour, name);
                tour.step = next;
                tour.frames = 0;
            } else if now - tour.since > GIVE_UP_SECS
                || matches!(
                    nav.phase,
                    NavPhase::Flying | NavPhase::ChangingSnapshot { placed: true, .. }
                )
            {
                error!(phase = ?nav.phase, "region tour: the cloud never gathered");
                exit.write(AppExit::error());
            }
        }
        Step::CloudHalf | Step::CloudFull => {
            if tour.frames >= PIN_FRAMES {
                tour.pin = None;
                tour.frames = 0;
                tour.step = if tour.step == Step::CloudHalf {
                    Step::CloudClose
                } else {
                    tour.since = now;
                    Step::CloudClear
                };
            }
        }
        Step::CloudClear => {
            if nav.phase != NavPhase::Flying {
                tour.since = now;
            } else if now - tour.since >= AFTER_CHANGE_SECS {
                log_flight("after the cloud", &fly, region.as_deref(), &time);
                shoot(&mut commands, &tour, "cloud-after");
                tour.step = Step::Done;
                tour.frames = 0;
            }
            if *state.get() == RegionState::Globe {
                error!(phase = ?nav.phase, "region tour: the snapshot change did not finish");
                exit.write(AppExit::error());
            }
        }
        Step::LowGather => {
            if !matches!(nav.phase, NavPhase::Climbing { .. }) {
                error!(phase = ?nav.phase, "region tour: the climb ended before its shots");
                exit.write(AppExit::error());
            } else if tour.frames == 1 {
                tour.pin = Some(0.5);
                shoot(&mut commands, &tour, "climb-05");
            } else if tour.frames == 1 + PIN_FRAMES {
                tour.pin = Some(0.995);
                shoot(&mut commands, &tour, "climb-0995");
            } else if tour.frames == 1 + 2 * PIN_FRAMES {
                tour.pin = Some(1.0);
                shoot(&mut commands, &tour, "climb-opaque");
            } else if tour.frames == 1 + 3 * PIN_FRAMES {
                tour.pin = None;
                log_flight("low climb shots", &fly, region.as_deref(), &time);
                tour.step = Step::LowAfter;
                tour.frames = 0;
            }
        }
        Step::LowAfter => {
            if nav.phase == NavPhase::Globe {
                tour.step = Step::Done;
                tour.frames = 0;
            } else if tour.frames > 600 {
                error!(phase = ?nav.phase, "region tour: the climb never reached the globe");
                exit.write(AppExit::error());
            }
        }
        Step::Low => {
            if tour.frames == WIRE_FRAMES {
                keys.press(OLDER_KEY);
            } else if tour.frames > WIRE_FRAMES {
                keys.release(OLDER_KEY);
                tour.step = Step::Older;
                tour.since = now;
                tour.changing = false;
            }
        }
        Step::Older => {
            let changing = matches!(nav.phase, NavPhase::ChangingSnapshot { .. });
            if changing && !tour.changing {
                tour.changing = true;
                info!("region tour: snapshot change started");
            }
            if !tour.changing || nav.phase != NavPhase::Flying {
                tour.since = now;
            }
            if tour.changing
                && nav.phase == NavPhase::Flying
                && now - tour.since >= AFTER_CHANGE_SECS
            {
                log_flight("older", &fly, region.as_deref(), &time);
                shoot(&mut commands, &tour, "older");
                tour.step = Step::Done;
                tour.frames = 0;
            } else if *state.get() == RegionState::Globe || now > GIVE_UP_SECS * 2.0 {
                error!(phase = ?nav.phase, "region tour: the snapshot change did not finish");
                exit.write(AppExit::error());
            }
        }
        Step::Timing => {
            let ms = real.delta_secs() * 1000.0;
            tour.frame_ms.push(ms);
            if now - tour.since >= TIME_SECS {
                let s = frame_time_stats(&tour.frame_ms);
                let mean = tour.frame_ms.iter().sum::<f32>() / tour.frame_ms.len().max(1) as f32;
                info!(
                    prefix = %tour.prefix,
                    agl_m = tour.agl_m,
                    frames = s.frames,
                    mean_ms = mean,
                    median_ms = s.median_ms,
                    p95_ms = s.p95_ms,
                    worst_ms = s.worst_ms,
                    "region tour frame times"
                );
                println!(
                    "region tour frame times: {} at {} m: {} frames, mean {:.2} ms, median {:.2} \
                     ms, p95 {:.2} ms, worst {:.2} ms",
                    tour.prefix, tour.agl_m, s.frames, mean, s.median_ms, s.p95_ms, s.worst_ms
                );
                exit.write(AppExit::Success);
            }
        }
        Step::Done => {
            // Let the screenshots reach the disk: the readback and the PNG encode take more
            // than WIRE_FRAMES frames when frames are fast.
            if tour.frames >= DONE_FRAMES {
                exit.write(AppExit::Success);
            }
        }
    }
}

/// The `dive` mode's steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiveStep {
    WaitGlobe,
    Above,
    Descend,
    DiveCloud,
    DiveSwap,
    Below,
    Climb,
    ClimbCloud,
    ClimbSwap,
    After,
    Done,
}

/// The `dive` mode's state.
#[derive(Resource)]
struct DiveTour {
    region: timefly::args::RegionArg,
    backspace: bool,
    step: DiveStep,
    since: f32,
    frames: u32,
    shot: bool,
}

/// Logs where both flies are and the cloud, for the report.
fn log_dive(label: &str, nav: &Navigation, fly: &FlyState, flight: &GlobeFlight, time: &GlobeTime) {
    let (lat, lon) = timefly_core::geo::geo_to_latlon(timefly_core::geo::render_to_geo(
        timefly_core::geo::RenderDir(flight.dir),
    ));
    info!(
        label,
        phase = ?nav.phase,
        density = nav.density,
        globe_lat = lat,
        globe_lon = lon,
        globe_altitude_km = flight.altitude_km,
        region_x_m = fly.pos_m.x,
        region_y_m = fly.pos_m.y,
        region_z_m = fly.pos_m.z,
        region_yaw = fly.yaw,
        years = time.years.0,
        "region tour dive"
    );
}

/// The `dive` mode: the globe above the cloud, the dive, the region below it, the climb, and
/// the globe again, with shots on each side of each cloud.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn dive_tour(
    mut dive: ResMut<DiveTour>,
    tour: Res<Tour>,
    real: Res<Time<Real>>,
    status: Option<Res<GlobeRenderStatus>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    nav: Res<Navigation>,
    worlds: (
        Res<FlyState>,
        ResMut<GlobeFlightState>,
        ResMut<GlobeLook>,
        ResMut<GlobeTime>,
    ),
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let (fly, mut flight, mut look, mut time) = worlds;
    let now = real.elapsed_secs();
    dive.frames += 1;
    if now > GIVE_UP_SECS * 2.0 && dive.step != DiveStep::Done {
        error!(step = ?dive.step, phase = ?nav.phase, "region tour: the dive tour timed out");
        exit.write(AppExit::error());
        return;
    }
    let next = |dive: &mut DiveTour, step: DiveStep| {
        dive.step = step;
        dive.since = now;
        dive.frames = 0;
        dive.shot = false;
    };
    match dive.step {
        DiveStep::WaitGlobe => {
            if matches!(status.as_deref(), Some(GlobeRenderStatus::Failed(_))) {
                error!("region tour: the globe failed");
                exit.write(AppExit::error());
            } else if matches!(
                status.as_deref(),
                Some(GlobeRenderStatus::Interactive { .. })
            ) {
                next(&mut dive, DiveStep::Above);
            }
        }
        DiveStep::Above => {
            // Held over the place, just above the cloud, until the shot is taken.
            let r = dive.region;
            time.years = YearsBp(r.years);
            flight.0 = GlobeFlight {
                altitude_km: CLOUD_TOP_KM + ABOVE_CLOUD_KM,
                ..GlobeFlight::start(r.lat_deg, r.lon_deg)
            };
            look.offset_deg = 0.0;
            if !dive.shot && now - dive.since >= SETTLE_SECS {
                log_dive("globe above the cloud", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "globe-above");
                dive.shot = true;
                dive.frames = 0;
            } else if dive.shot && dive.frames >= WIRE_FRAMES {
                next(&mut dive, DiveStep::Descend);
            }
        }
        DiveStep::Descend => {
            keys.press(KeyCode::ControlLeft);
            if matches!(nav.phase, NavPhase::Diving { .. }) {
                keys.release(KeyCode::ControlLeft);
                log_dive("dive committed", &nav, &fly, &flight.0, &time);
                next(&mut dive, DiveStep::DiveCloud);
            }
        }
        DiveStep::DiveCloud => {
            if !dive.shot && nav.density == 1.0 && matches!(nav.phase, NavPhase::Diving { .. }) {
                log_dive("in the cloud over the globe", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "dive-cloud");
                dive.shot = true;
            }
            if matches!(
                nav.phase,
                NavPhase::Emerging {
                    world: World::Region,
                    ..
                }
            ) {
                next(&mut dive, DiveStep::DiveSwap);
            } else if matches!(nav.phase, NavPhase::Globe | NavPhase::Emerging { .. }) {
                error!(phase = ?nav.phase, "region tour: the dive did not reach the region");
                exit.write(AppExit::error());
            }
        }
        DiveStep::DiveSwap => {
            if !dive.shot && dive.frames >= SWAP_SHOT_FRAMES {
                log_dive("the dive's swap", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "dive-swap");
                dive.shot = true;
            }
            if nav.phase == NavPhase::Flying {
                next(&mut dive, DiveStep::Below);
            }
        }
        DiveStep::Below => {
            if !dive.shot && now - dive.since >= SETTLE_SECS {
                log_dive("region below the cloud", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "region-below");
                dive.shot = true;
                dive.frames = 0;
            } else if dive.shot && dive.frames >= WIRE_FRAMES {
                next(&mut dive, DiveStep::Climb);
            }
        }
        DiveStep::Climb => {
            if dive.backspace {
                if dive.frames == 1 {
                    keys.press(LEAVE_KEY);
                } else {
                    keys.release(LEAVE_KEY);
                }
            } else {
                keys.press(KeyCode::Space);
            }
            if matches!(nav.phase, NavPhase::Climbing { .. }) {
                keys.release(KeyCode::Space);
                log_dive("climb started", &nav, &fly, &flight.0, &time);
                next(&mut dive, DiveStep::ClimbCloud);
            }
        }
        DiveStep::ClimbCloud => {
            if !dive.shot && nav.density == 1.0 && matches!(nav.phase, NavPhase::Climbing { .. }) {
                log_dive("in the cloud over the region", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "climb-cloud");
                dive.shot = true;
            }
            if matches!(
                nav.phase,
                NavPhase::Emerging {
                    world: World::Globe,
                    ..
                }
            ) {
                next(&mut dive, DiveStep::ClimbSwap);
            }
        }
        DiveStep::ClimbSwap => {
            if !dive.shot && dive.frames >= SWAP_SHOT_FRAMES {
                log_dive("the climb's swap", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "climb-swap");
                dive.shot = true;
            }
            if nav.phase == NavPhase::Globe {
                next(&mut dive, DiveStep::After);
            }
        }
        DiveStep::After => {
            if !dive.shot && now - dive.since >= AFTER_CHANGE_SECS {
                log_dive("globe after the climb", &nav, &fly, &flight.0, &time);
                shoot(&mut commands, &tour, "globe-after");
                dive.shot = true;
                dive.frames = 0;
            } else if dive.shot && dive.frames >= WIRE_FRAMES {
                next(&mut dive, DiveStep::Done);
            }
        }
        DiveStep::Done => {
            if dive.frames >= DONE_FRAMES {
                exit.write(AppExit::Success);
            }
        }
    }
}

/// Holds the cloud's density at the tour's pin, after navigation has raised it this frame.
fn pin_cloud(tour: Res<Tour>, mut density: ResMut<CloudDensity>) {
    if let Some(pin) = tour.pin
        && density.0 != pin
    {
        density.0 = pin;
    }
}

/// `time` mode: vsync off, as `--region-bench` sets it, so frame times show the real cost.
fn uncap_frames(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    for mut window in &mut windows {
        window.present_mode = PresentMode::AutoNoVsync;
    }
}

fn main() -> AppExit {
    // `--no-dunes` and `--offscreen` may come anywhere; the rest are positional.
    let (flags, args): (Vec<String>, Vec<String>) = std::env::args()
        .skip(1)
        .partition(|a| a == "--no-dunes" || a == "--offscreen");
    let no_dunes = flags.iter().any(|f| f == "--no-dunes");
    let offscreen = flags.iter().any(|f| f == "--offscreen");
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!(
            "usage: region_tour [--no-dunes] [--offscreen] <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix] \
             [shaded|fly|cruise|cloud|time|cloudtime|dive|divekey|lowclimb] [AGL_M] [LATER_S]"
        );
        return AppExit::error();
    };
    let size = args
        .get(1)
        .and_then(|s| parse_window_size(s))
        .unwrap_or((1280, 720));
    let Some(region) = parse_region(args.get(2).map_or("46.5,10,0", String::as_str)) else {
        eprintln!("bad region {:?}: want LAT,LON,AGE_YEARS", args.get(2));
        return AppExit::error();
    };
    let prefix = args.get(3).cloned().unwrap_or_else(|| "cdlod".to_string());
    let timed_cloud = args.get(4).is_some_and(|m| m == "cloudtime");
    let mode = match args.get(4).map(String::as_str) {
        Some("shaded") => Mode::Shaded,
        Some("high") => Mode::High,
        Some("fly") => Mode::Fly,
        Some("cruise") => Mode::Cruise,
        Some("cloud") => Mode::Cloud,
        Some("time" | "cloudtime") => Mode::Time,
        Some("lowclimb") => Mode::LowClimb,
        Some("dive") => Mode::Dive { backspace: false },
        Some("divekey") => Mode::Dive { backspace: true },
        _ => Mode::Wire,
    };
    let diving = matches!(mode, Mode::Dive { .. });
    let agl_m = match args.get(5) {
        None => CAMERA_START_ALTITUDE_M,
        Some(s) => match s.parse::<f32>() {
            Ok(v) if v.is_finite() && v > 0.0 => v,
            _ => {
                eprintln!("bad AGL_M {s:?}: want a positive number of metres");
                return AppExit::error();
            }
        },
    };
    let later_s = match args.get(6) {
        None => None,
        Some(s) => match s.parse::<f32>() {
            Ok(v) if v.is_finite() && v > 0.0 => Some(v),
            _ => {
                eprintln!("bad LATER_S {s:?}: want a positive number of seconds");
                return AppExit::error();
            }
        },
    };
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {e}", out.display());
        return AppExit::error();
    }
    let session = diagnostics::start(LaunchArgs {
        window: Some(size),
        // The dive starts on the globe.
        region: (!diving).then_some(region),
        no_dunes,
        ..default()
    });
    let mut app = timefly::app(&session);
    if mode == Mode::Time {
        app.add_systems(Startup, uncap_frames).add_systems(
            Update,
            tour.after(FlySystems::Step).before(FlySystems::Apply),
        );
    } else if let Mode::Dive { backspace } = mode {
        app.insert_resource(DiveTour {
            region,
            backspace,
            step: DiveStep::WaitGlobe,
            since: 0.0,
            frames: 0,
            shot: false,
        })
        .add_systems(PreUpdate, dive_tour.after(bevy::input::InputSystems));
    } else {
        app.add_systems(PreUpdate, tour.after(bevy::input::InputSystems));
    }
    app.insert_resource(Tour {
        out,
        prefix,
        size,
        target: None,
        mode,
        requested: launch_request(&region),
        agl_m,
        later_s,
        shot_at: 0.0,
        step: Step::WaitRegion,
        changing: false,
        shot_taken: false,
        since: 0.0,
        frames: 0,
        left_generating: false,
        frame_ms: Vec::new(),
        pin: timed_cloud.then_some(TIMED_CLOUD_DENSITY),
        offscreen,
    })
    .add_systems(Update, render_offscreen)
    .add_systems(Update, pin_cloud.after(NavigationSystems));
    let exit = app.run();
    session.finish(&exit);
    exit
}

#[cfg(test)]
mod tests {
    use super::*;
    use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};

    fn timing_app(agl_m: f32, ground: f32) -> App {
        let request = launch_request(&parse_region("0,0,0").unwrap());
        let spec = RegionSpec::new(request.era_dir, request.snapshot, 0);
        let field = Field {
            res: 2,
            size_m: 1000.0,
            elevation_m: vec![ground; 4],
            surface: vec![[0; 4]; 4],
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<RegionEntered>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<GlobeTime>()
            .init_resource::<FlyState>()
            .insert_resource(Navigation {
                phase: NavPhase::Flying,
                ..default()
            })
            .insert_resource(State::new(RegionState::InRegion))
            .insert_resource(ActiveRegion {
                frame: spec.frame(),
                grid: RegionGrid {
                    spec,
                    inner: field.clone(),
                    apron: field,
                }
                .into(),
            })
            .insert_resource(Tour {
                out: PathBuf::new(),
                prefix: "test".into(),
                size: (800, 600),
                target: None,
                mode: Mode::Time,
                requested: request,
                agl_m,
                later_s: None,
                shot_at: 0.0,
                step: Step::Timing,
                changing: false,
                shot_taken: false,
                since: 0.0,
                frames: 0,
                left_generating: false,
                frame_ms: Vec::new(),
                pin: None,
                offscreen: false,
            })
            .add_systems(Update, tour);
        app
    }

    #[test]
    fn interrupted_timing_exits_before_recording_another_sample() {
        let interruptions: [fn(&mut App); 5] = [
            |app| app.world_mut().resource_mut::<Navigation>().phase = NavPhase::Globe,
            |app| {
                app.world_mut().remove_resource::<ActiveRegion>();
            },
            |app| {
                app.insert_resource(State::new(RegionState::Generating));
            },
            |app| {
                let mut region = app.world_mut().resource_mut::<ActiveRegion>();
                std::sync::Arc::make_mut(&mut region.grid).spec.center =
                    launch_request(&parse_region("10,20,0").unwrap()).era_dir;
            },
            |app| {
                let mut region = app.world_mut().resource_mut::<ActiveRegion>();
                std::sync::Arc::make_mut(&mut region.grid).spec.snapshot =
                    launch_request(&parse_region("0,0,1000").unwrap()).snapshot;
            },
        ];
        for interrupt in interruptions {
            let mut app = timing_app(400.0, 0.0);
            app.update();
            assert_eq!(app.world().resource::<Tour>().frame_ms.len(), 1);
            interrupt(&mut app);
            app.update();
            assert_eq!(app.should_exit(), Some(AppExit::error()));
            assert_eq!(app.world().resource::<Tour>().frame_ms.len(), 1);
        }
    }

    #[test]
    fn requested_height_must_be_renderable_at_the_region_centre() {
        for (agl, ground, expected_y) in [
            (50.0, 0.0, None),
            (299_001.0, 1000.0, None),
            (299_000.0, 1000.0, Some(CAMERA_CEILING_M)),
            (CAMERA_CLEARANCE_M, -500.0, Some(CAMERA_CLEARANCE_M)),
        ] {
            let mut app = timing_app(agl, ground);
            app.update();
            if let Some(y) = expected_y {
                assert_eq!(app.should_exit(), None);
                assert_eq!(app.world().resource::<FlyState>().pos_m.y, y);
            } else {
                assert_eq!(app.should_exit(), Some(AppExit::error()));
                assert!(app.world().resource::<Tour>().frame_ms.is_empty());
            }
        }
    }
}
