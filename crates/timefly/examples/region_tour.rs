//! Runs the real game app with `--region`, waits for the region's terrain, saves a shaded and a
//! wireframe (`F2`) screenshot from the region camera, then exits. For visual review of the CDLOD
//! terrain (Task 7) and later the painterly look.
//!
//! Like `ui_tour`, the camera renders into an offscreen image of the window's size and that is
//! what gets saved: window-surface screenshots of a process started from an agent's shell come
//! back blank on macOS. The window itself stays empty while the tour runs.
//!
//! `cargo run -p timefly --example region_tour -- <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix]
//! [shaded|fly|time] [AGL_M]` (run through cargo, or set `BEVY_ASSET_ROOT=crates/timefly`, so the assets are
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
use timefly::diagnostics;
use timefly::fly_camera::{CAMERA_CEILING_M, CAMERA_CLEARANCE_M, FlyState, FlySystems};
use timefly::region::launch::launch_request;
use timefly::region::navigate::{NavPhase, Navigation, same_region};
use timefly::region::{
    ActiveRegion, CAMERA_START_ALTITUDE_M, RegionEntered, RegionRequested, RegionState,
};
use timefly::terrain::view::RegionCamera;
use timefly::terrain::{TerrainScene, WIREFRAME_KEY};
use timefly::time::{GlobeTime, OLDER_KEY};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    WaitRegion,
    Settle,
    /// The shaded shot is requested; a screenshot resolves frames later, so wait before F2.
    Shaded,
    Wire,
    /// The scripted flight (`fly`): diving, then cruising.
    Dive,
    Cruise,
    /// The low shot is requested; then `[` is pressed.
    Low,
    /// Waiting for the snapshot change to finish.
    Older,
    /// Recording frame deltas (`time`).
    Timing,
    Done,
}

/// What follows the start shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wire,
    Shaded,
    Fly,
    Time,
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
    since: f32,
    frames: u32,
    left_generating: bool,
    /// Frame deltas recorded in `time` mode, milliseconds.
    frame_ms: Vec<f32>,
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

/// Points the region camera at an offscreen image the size of the window when it appears
/// (except in `time` mode, which draws to the window as the game does).
fn render_offscreen(
    mut tour: ResMut<Tour>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<Entity, Added<RegionCamera>>,
    mut commands: Commands,
) {
    if tour.mode == Mode::Time {
        return;
    }
    let (w, h) = tour.size;
    for camera in &cameras {
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
            if tour.mode == Mode::Fly && tour.frames >= WIRE_FRAMES {
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

/// `time` mode: vsync off, as `--region-bench` sets it, so frame times show the real cost.
fn uncap_frames(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    for mut window in &mut windows {
        window.present_mode = PresentMode::AutoNoVsync;
    }
}

fn main() -> AppExit {
    // `--no-dunes` may come anywhere; the rest are positional.
    let (flags, args): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a == "--no-dunes");
    let no_dunes = !flags.is_empty();
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!(
            "usage: region_tour [--no-dunes] <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix] \
             [shaded|fly|time] [AGL_M] [LATER_S]"
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
    let mode = match args.get(4).map(String::as_str) {
        Some("shaded") => Mode::Shaded,
        Some("fly") => Mode::Fly,
        Some("time") => Mode::Time,
        _ => Mode::Wire,
    };
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
        region: Some(region),
        no_dunes,
        ..default()
    });
    let mut app = timefly::app(&session);
    if mode == Mode::Time {
        app.add_systems(Startup, uncap_frames).add_systems(
            Update,
            tour.after(FlySystems::Step).before(FlySystems::Apply),
        );
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
        since: 0.0,
        frames: 0,
        left_generating: false,
        frame_ms: Vec::new(),
    })
    .add_systems(Update, render_offscreen);
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
                since: 0.0,
                frames: 0,
                left_generating: false,
                frame_ms: Vec::new(),
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
            (59_001.0, 1000.0, None),
            (59_000.0, 1000.0, Some(CAMERA_CEILING_M)),
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
