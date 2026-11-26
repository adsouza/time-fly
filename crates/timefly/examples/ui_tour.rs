//! Runs the real game app and saves screenshots of the globe and its UI at review ages
//! (loading overlay, present, 21 ka, mid-bridge 28 ka, 250 Ma, 540 Ma, credits, idle fade),
//! and of the dragonfly over the globe at its top altitude and near the dive altitude (present
//! and 250 Ma, M3 Task 4), then exits. For visual review.
//!
//! The globe camera (and so the UI, which draws on it) renders into an offscreen image of the
//! window's size, which is what gets saved: window-surface screenshots of a process started
//! from an agent's shell came back blank on macOS (even for a bare Bevy app), so the tour does
//! not depend on them. The window itself stays empty while the tour runs.
//!
//! `cargo run --release -p timefly --example ui_tour -- <out_dir> [WxH] [time]` (run through
//! cargo, or set `BEVY_ASSET_ROOT=crates/timefly`, so the assets are found). Exits with an error
//! if the globe never becomes interactive.
//!
//! A third argument `time` measures frame times instead of taking shots: vsync off (as in the
//! benches), the fly held at the top altitude over Europe at the present, [`TIME_SETTLE_SECS`]
//! to settle, then [`TIME_SECS`] of frame deltas, logged as one `ui tour frame times` line
//! (mean, median, p95, worst) before it exits.

use std::path::PathBuf;

use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::{PresentMode, PrimaryWindow};
use timefly::args::{LaunchArgs, parse_window_size};
use timefly::bench::frame_time_stats;
use timefly::diagnostics::{self, FirstInteractiveFrame};
use timefly::globe::{GlobeCamera, GlobeRenderStatus};
use timefly::globe_flight::{GlobeFlightState, GlobeLook};
use timefly::time::GlobeTime;
use timefly::ui::credits::CreditsOpen;
use timefly::ui::slider::UiIdle;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::altitudes::GLOBE_MAX_ALTITUDE_KM;
use timefly_core::flight::globe::GlobeFlight;

/// One screenshot: name, age, the fly (lat, lon, altitude km, heading north), the view drag's
/// offset (degrees, positive looks further down), credits open, keep UI awake.
struct Shot {
    name: &'static str,
    years: f64,
    view: (f64, f64, f64),
    look_deg: f64,
    credits: bool,
    awake: bool,
}

/// The top of globe flight (M1's starting view, with the fly).
const TOP: f64 = GLOBE_MAX_ALTITUDE_KM;
/// Closer in, for the bridge shots.
const NEAR: f64 = 5_000.0;
/// Just above the dive altitude (Task 4's facet and shell check).
const DIVE: f64 = 1_250.0;

const SHOTS: &[Shot] = &[
    Shot {
        name: "present",
        years: 0.0,
        view: (30.0, 10.0, TOP),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "21ka-bering",
        years: 21.0e3,
        view: (62.0, -170.0, NEAR),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "28ka-mid-bridge",
        years: 28.0e3,
        view: (62.0, -170.0, NEAR),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "250ma-pangaea",
        years: 250.0e6,
        view: (0.0, 0.0, TOP),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "540ma",
        years: 540.0e6,
        view: (-30.0, 0.0, TOP),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "credits",
        years: 0.0,
        view: (30.0, 10.0, TOP),
        look_deg: 0.0,
        credits: true,
        awake: true,
    },
    Shot {
        name: "66ma-idle-faded",
        years: 66.0e6,
        view: (20.0, 100.0, TOP),
        look_deg: 0.0,
        credits: false,
        awake: false,
    },
    Shot {
        name: "fly-dive-present",
        years: 0.0,
        view: (30.0, 10.0, DIVE),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "fly-dive-present-look-down",
        years: 0.0,
        view: (30.0, 10.0, DIVE),
        look_deg: 30.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "fly-dive-250ma",
        years: 250.0e6,
        view: (0.0, 0.0, DIVE),
        look_deg: 0.0,
        credits: false,
        awake: true,
    },
    Shot {
        name: "fly-dive-250ma-look-down",
        years: 250.0e6,
        view: (0.0, 0.0, DIVE),
        look_deg: 30.0,
        credits: false,
        awake: true,
    },
];

/// Frames to let a change settle before the capture (the chase camera glides over about a
/// second), and seconds of idle for the faded shot.
const SETTLE_FRAMES: u32 = 90;
const IDLE_SECS: f32 = 4.0;
/// Seconds to wait for the first interactive frame.
const GIVE_UP_SECS: f32 = 90.0;
/// `time` mode: seconds after the first interactive frame before recording, and seconds
/// recorded.
const TIME_SETTLE_SECS: f32 = 5.0;
const TIME_SECS: f32 = 10.0;

#[derive(Resource)]
struct Tour {
    out: PathBuf,
    size: (u32, u32),
    target: Option<Handle<Image>>,
    interactive: bool,
    frame: u32,
    loading_shot: bool,
    index: usize,
    step_frame: u32,
    step_started: f32,
    /// `time` mode: frame deltas (ms) recorded so far, once settled.
    timing: Option<Vec<f32>>,
}

/// Points the globe camera at an offscreen image the size of the window.
fn render_offscreen(
    mut tour: ResMut<Tour>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<Entity, With<GlobeCamera>>,
    mut commands: Commands,
) {
    let (w, h) = tour.size;
    let target = images.add(Image::new_target_texture(
        w,
        h,
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    for camera in &cameras {
        commands
            .entity(camera)
            .insert(RenderTarget::Image(target.clone().into()));
    }
    tour.target = Some(target);
}

fn shoot(commands: &mut Commands, tour: &Tour, name: &str) {
    let Some(target) = tour.target.clone() else {
        return;
    };
    let path = tour.out.join(format!("{name}.png"));
    info!(path = %path.display(), "ui tour screenshot");
    commands
        .spawn(Screenshot::image(target))
        .observe(save_to_disk(path));
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn tour(
    mut tour: ResMut<Tour>,
    mut first: MessageReader<FirstInteractiveFrame>,
    real: Res<Time<Real>>,
    mut commands: Commands,
    mut time: ResMut<GlobeTime>,
    mut flight: ResMut<GlobeFlightState>,
    mut look: ResMut<GlobeLook>,
    mut credits: ResMut<CreditsOpen>,
    mut idle: ResMut<UiIdle>,
    status: Res<GlobeRenderStatus>,
    mut exit: MessageWriter<AppExit>,
) {
    tour.frame += 1;
    let now = real.elapsed_secs();
    // Give up (after the loading shot has been saved) if the globe cannot render.
    let failed = matches!(*status, GlobeRenderStatus::Failed(_));
    if !tour.interactive && tour.frame > 30 && (failed || now > GIVE_UP_SECS) {
        error!(status = ?*status, "ui tour: the globe never became interactive");
        exit.write(AppExit::error());
        return;
    }
    // The loading overlay, once the UI has been laid out but before the globe is ready.
    if !tour.loading_shot && tour.frame >= 5 && !tour.interactive {
        tour.loading_shot = true;
        shoot(&mut commands, &tour, "loading");
    }
    if first.read().last().is_some() {
        tour.interactive = true;
        tour.step_frame = tour.frame;
        tour.step_started = now;
    }
    if !tour.interactive {
        return;
    }
    let since = now - tour.step_started;
    if let Some(frame_ms) = tour.timing.as_mut() {
        let wanted = GlobeFlight::start(30.0, 10.0);
        if flight.0 != wanted {
            flight.0 = wanted;
        }
        idle.idle_secs = 0.0;
        if since < TIME_SETTLE_SECS {
            return;
        }
        frame_ms.push(real.delta_secs() * 1000.0);
        if since >= TIME_SETTLE_SECS + TIME_SECS {
            let s = frame_time_stats(frame_ms);
            let mean = frame_ms.iter().sum::<f32>() / frame_ms.len().max(1) as f32;
            println!(
                "ui tour frame times: top altitude, present: {} frames, mean {mean:.2} ms \
                 ({:.0} fps), median {:.2} ms, p95 {:.2} ms, worst {:.2} ms",
                s.frames,
                1000.0 / mean,
                s.median_ms,
                s.p95_ms,
                s.worst_ms
            );
            exit.write(AppExit::Success);
        }
        return;
    }
    let Some(shot) = SHOTS.get(tour.index) else {
        if tour.frame > tour.step_frame + SETTLE_FRAMES {
            exit.write(AppExit::Success);
        }
        return;
    };
    let (lat, lon, altitude_km) = shot.view;
    let wanted = GlobeFlight {
        altitude_km,
        ..GlobeFlight::start(lat, lon)
    };
    if flight.0 != wanted {
        flight.0 = wanted;
    }
    if look.offset_deg != shot.look_deg {
        look.offset_deg = shot.look_deg;
    }
    if time.years.0 != shot.years {
        time.years = YearsBp(shot.years);
    }
    if credits.0 != shot.credits {
        credits.0 = shot.credits;
    }
    if shot.awake {
        idle.idle_secs = 0.0;
    }
    let settled = tour.frame >= tour.step_frame + SETTLE_FRAMES
        && (shot.awake || now - tour.step_started >= IDLE_SECS);
    if settled {
        shoot(&mut commands, &tour, shot.name);
        tour.index += 1;
        tour.step_frame = tour.frame;
        tour.step_started = now;
    }
}

/// `time` mode: vsync off, as the benches set it, so frame times show the real cost.
fn uncap_frames(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    for mut window in &mut windows {
        window.present_mode = PresentMode::AutoNoVsync;
    }
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(out) = args.first().map(PathBuf::from) else {
        eprintln!("usage: ui_tour <out_dir> [WxH]");
        return AppExit::error();
    };
    let size = args
        .get(1)
        .and_then(|s| parse_window_size(s))
        .unwrap_or((1280, 720));
    let timing = args.get(2).is_some_and(|a| a == "time");
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {e}", out.display());
        return AppExit::error();
    }
    let session = diagnostics::start(LaunchArgs {
        window: Some(size),
        ..default()
    });
    let mut app = timefly::app(&session);
    app.insert_resource(Tour {
        out,
        size,
        target: None,
        interactive: false,
        frame: 0,
        loading_shot: false,
        index: 0,
        step_frame: 0,
        step_started: 0.0,
        timing: timing.then(Vec::new),
    })
    .add_systems(PostStartup, render_offscreen)
    .add_systems(PreUpdate, tour);
    if timing {
        app.add_systems(Startup, uncap_frames);
    }
    let exit = app.run();
    session.finish(&exit);
    exit
}
