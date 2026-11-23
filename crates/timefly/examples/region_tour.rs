//! Runs the real game app with `--region`, waits for the region's terrain, saves a shaded and a
//! wireframe (`F2`) screenshot from the region camera, then exits. For visual review of the CDLOD
//! terrain (Task 7) and later the painterly look.
//!
//! Like `ui_tour`, the camera renders into an offscreen image of the window's size and that is
//! what gets saved: window-surface screenshots of a process started from an agent's shell come
//! back blank on macOS. The window itself stays empty while the tour runs.
//!
//! `cargo run -p timefly --example region_tour -- <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix]
//! [shaded|fly]` (run through cargo, or set `BEVY_ASSET_ROOT=crates/timefly`, so the assets are
//! found). The region defaults to the Alps at the present (`46.5,10,0`) and the prefix to
//! `cdlod`, giving `<prefix>-shaded.png` and `<prefix>-wire.png`; a fifth argument `shaded`
//! skips the wireframe shot. Exits with an error if the region never appears. Set a fresh
//! `TIMEFLY_LOG_DIR` per run.
//!
//! `tools/dev/region-shots.sh` runs it over the Task 8 set (the bench regions and one region
//! per vegetation era).
//!
//! A fifth argument `fly` replaces the wireframe shot with a scripted flight through the real
//! controls (Task 9): after the start shot it holds `W` and `Q` (forward and down) for
//! [`DIVE_SECS`], then `W` alone for [`CRUISE_SECS`], and saves `<prefix>-low.png`; then it
//! presses `[` (the next older snapshot), waits for the region to regenerate around the
//! camera's crust position and the fade to finish, and saves `<prefix>-older.png`.

use std::path::PathBuf;

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use timefly::args::{LaunchArgs, parse_region, parse_window_size};
use timefly::diagnostics;
use timefly::fly_camera::FlyState;
use timefly::region::navigate::{NavPhase, Navigation};
use timefly::region::{ActiveRegion, RegionEntered, RegionState};
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
    Done,
}

/// What follows the start shot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wire,
    Shaded,
    Fly,
}

#[derive(Resource)]
struct Tour {
    out: PathBuf,
    prefix: String,
    size: (u32, u32),
    target: Option<Handle<Image>>,
    mode: Mode,
    step: Step,
    /// The snapshot change has started (`Older`).
    changing: bool,
    since: f32,
    frames: u32,
    left_generating: bool,
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

/// Points the region camera at an offscreen image the size of the window when it appears.
fn render_offscreen(
    mut tour: ResMut<Tour>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<Entity, Added<RegionCamera>>,
    mut commands: Commands,
) {
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
        Res<FlyState>,
        Option<Res<ActiveRegion>>,
        Res<GlobeTime>,
    ),
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    let (nav, fly, region, time) = flight;
    let now = real.elapsed_secs();
    tour.frames += 1;
    if *state.get() == RegionState::Generating {
        tour.left_generating = true;
    }
    match tour.step {
        Step::WaitRegion => {
            let failed = tour.left_generating && *state.get() == RegionState::Globe;
            if failed || now > GIVE_UP_SECS {
                error!(failed, "region tour: the region never appeared");
                exit.write(AppExit::error());
                return;
            }
            if entered.read().last().is_some() || scene.is_some() && tour.target.is_some() {
                tour.step = Step::Settle;
                tour.since = now;
            }
        }
        Step::Settle => {
            if now - tour.since >= SETTLE_SECS {
                shoot(&mut commands, &tour, "shaded");
                tour.step = Step::Shaded;
                tour.frames = 0;
            }
        }
        Step::Shaded => {
            if tour.mode == Mode::Fly && tour.frames >= WIRE_FRAMES {
                log_flight("start", &fly, region.as_deref(), &time);
                tour.step = Step::Dive;
                tour.since = now;
            } else if tour.mode == Mode::Shaded && tour.frames >= WIRE_FRAMES {
                tour.step = Step::Done;
                tour.frames = 0;
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
        Step::Done => {
            // Let the screenshots reach the disk: the readback and the PNG encode take more
            // than WIRE_FRAMES frames when frames are fast.
            if tour.frames >= DONE_FRAMES {
                exit.write(AppExit::Success);
            }
        }
    }
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!("usage: region_tour <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix] [shaded|fly]");
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
        _ => Mode::Wire,
    };
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {e}", out.display());
        return AppExit::error();
    }
    let session = diagnostics::start(LaunchArgs {
        window: Some(size),
        region: Some(region),
        ..default()
    });
    let mut app = timefly::app(&session);
    app.insert_resource(Tour {
        out,
        prefix,
        size,
        target: None,
        mode,
        step: Step::WaitRegion,
        changing: false,
        since: 0.0,
        frames: 0,
        left_generating: false,
    })
    .add_systems(Update, render_offscreen)
    .add_systems(PreUpdate, tour.after(bevy::input::InputSystems));
    let exit = app.run();
    session.finish(&exit);
    exit
}
