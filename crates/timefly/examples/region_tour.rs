//! Runs the real game app with `--region`, waits for the region's terrain, saves a shaded and a
//! wireframe (`F2`) screenshot from the region camera, then exits. For visual review of the CDLOD
//! terrain (Task 7) and later the painterly look.
//!
//! Like `ui_tour`, the camera renders into an offscreen image of the window's size and that is
//! what gets saved: window-surface screenshots of a process started from an agent's shell come
//! back blank on macOS. The window itself stays empty while the tour runs.
//!
//! `cargo run -p timefly --example region_tour -- <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix]`
//! (run through cargo, or set `BEVY_ASSET_ROOT=crates/timefly`, so the assets are found). The
//! region defaults to the Alps at the present (`46.5,10,0`) and the prefix to `cdlod`, giving
//! `<prefix>-shaded.png` and `<prefix>-wire.png`. Exits with an error if the region never
//! appears. Set a fresh `TIMEFLY_LOG_DIR` per run.

use std::path::PathBuf;

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use timefly::args::{LaunchArgs, parse_region, parse_window_size};
use timefly::diagnostics;
use timefly::region::{RegionEntered, RegionState};
use timefly::terrain::view::RegionCamera;
use timefly::terrain::{TerrainScene, WIREFRAME_KEY};

/// Seconds after the region appears before the first shot, so its pipelines compile and the
/// shadow cascades settle.
const SETTLE_SECS: f32 = 6.0;
/// Frames between a shot and the next change, and between toggling the wireframe and its shot.
const WIRE_FRAMES: u32 = 20;
/// Seconds to wait for the region.
const GIVE_UP_SECS: f32 = 180.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    WaitRegion,
    Settle,
    /// The shaded shot is requested; a screenshot resolves frames later, so wait before F2.
    Shaded,
    Wire,
    Done,
}

#[derive(Resource)]
struct Tour {
    out: PathBuf,
    prefix: String,
    size: (u32, u32),
    target: Option<Handle<Image>>,
    step: Step,
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

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn tour(
    mut tour: ResMut<Tour>,
    mut entered: MessageReader<RegionEntered>,
    scene: Option<Res<TerrainScene>>,
    state: Res<State<RegionState>>,
    real: Res<Time<Real>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
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
            if tour.frames >= WIRE_FRAMES {
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
        Step::Done => {
            // Let the screenshots reach the disk.
            if tour.frames >= WIRE_FRAMES {
                exit.write(AppExit::Success);
            }
        }
    }
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!("usage: region_tour <out_dir> [WxH] [LAT,LON,AGE_YEARS] [prefix]");
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
        step: Step::WaitRegion,
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
