//! Remembers the primary window's position and size across restarts (native only).
//!
//! A plain launch restores the last geometry from `window.json` in the game's state folder and
//! saves it again whenever the window settles after a move or resize. Launches that pick their
//! own window (`--window WxH`, the benchmarks, probes, data checks) neither restore nor save, so
//! a bench run never overwrites the player's window.
//!
//! The file is saved [`SAVE_DELAY_S`] after the last change, not at exit: on macOS, Cmd+Q
//! never returns from `App::run`, so code after it never runs. On Wayland the compositor
//! places windows and never reports a position, so only the size is remembered there.

use std::io;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy::window::{
    Monitor, PrimaryMonitor, PrimaryWindow, WindowCreated, WindowMode, WindowMoved, WindowPosition,
    WindowResized,
};
use serde::{Deserialize, Serialize};

use crate::args::LaunchArgs;
use crate::diagnostics::session::LogRootEnv;

/// The geometry file inside the state folder.
pub const WINDOW_FILE: &str = "window.json";
/// Seconds after the last move or resize before the geometry is saved.
pub const SAVE_DELAY_S: f64 = 0.5;
/// How much of a restored window (physical pixels, from its top-left corner) must lie on one
/// monitor; otherwise it is centred on the primary monitor instead.
pub const MIN_VISIBLE_PX: IVec2 = IVec2::new(100, 50);
/// Saved sizes outside 1..=this (physical pixels) are treated as corrupt.
const MAX_SIDE_PX: u32 = 16_384;

/// The primary window's outer position and inner size, in physical pixels.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    /// `None` where the platform never reports a position (Wayland).
    pub position: Option<[i32; 2]>,
    pub size: [u32; 2],
}

/// Whether this launch restores and saves the window: only when nothing on the command line
/// chooses the window (a `--from-shot` repro takes its shot's size) or runs an automated mode.
pub fn remembers_window(args: &LaunchArgs) -> bool {
    args.window.is_none()
        && !args.scrub_bench
        && !args.region_bench
        && !args.journey_bench
        && args.diagnostics_probe.is_none()
        && args.check_data.is_none()
        && args.hydro_check.is_none()
        && args.from_shot.is_none()
}

/// The game's state folder, beside the log folder's rules (`resolve_log_root`): macOS
/// `~/Library/Application Support/Time Fly`, Windows `%LOCALAPPDATA%\Time Fly`, Linux
/// `$XDG_STATE_HOME/time-fly` (or `~/.local/state/time-fly`). `TIMEFLY_LOG_DIR` overrides it
/// too, so tests and CI never touch the player's file.
pub fn resolve_state_dir(os: &str, env: &LogRootEnv) -> Option<PathBuf> {
    if let Some(dir) = &env.override_dir {
        return Some(dir.clone());
    }
    match os {
        "macos" => env.home.as_ref().map(|h| {
            h.join("Library")
                .join("Application Support")
                .join("Time Fly")
        }),
        "windows" => env.local_app_data.as_ref().map(|d| d.join("Time Fly")),
        "linux" | "freebsd" | "dragonfly" | "netbsd" | "openbsd" => env
            .xdg_state_home
            .clone()
            .filter(|p| p.is_absolute())
            .or_else(|| env.home.as_ref().map(|h| h.join(".local").join("state")))
            .map(|state| state.join("time-fly")),
        _ => None,
    }
}

/// This machine's geometry file, or `None` where there is no state folder.
pub fn window_file() -> Option<PathBuf> {
    let env = crate::diagnostics::session::store::log_root_env();
    resolve_state_dir(std::env::consts::OS, &env).map(|dir| dir.join(WINDOW_FILE))
}

/// The saved geometry, or `None` if there is none or it is unreadable or implausible.
pub fn load_geometry(path: &Path) -> Option<WindowGeometry> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!(error = %e, "could not read the saved window geometry");
            return None;
        }
    };
    let geometry: WindowGeometry = match serde_json::from_str(&text) {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "ignoring an unreadable saved window geometry");
            return None;
        }
    };
    let plausible = geometry
        .size
        .iter()
        .all(|side| (1..=MAX_SIDE_PX).contains(side));
    if !plausible {
        warn!(size = ?geometry.size, "ignoring an implausible saved window size");
        return None;
    }
    Some(geometry)
}

/// Writes the geometry atomically (a temporary file renamed over the old one), creating the
/// folder if needed.
pub fn save_geometry(path: &Path, geometry: &WindowGeometry) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(geometry).map_err(io::Error::other)?,
    )?;
    std::fs::rename(&tmp, path)
}

/// Whether at least [`MIN_VISIBLE_PX`] of the window's top-left corner lies on one of the
/// monitors (each `(top-left, size)` in physical pixels). A window without a position fits.
pub fn fits_monitors(geometry: &WindowGeometry, monitors: &[(IVec2, UVec2)]) -> bool {
    let Some([x, y]) = geometry.position else {
        return true;
    };
    let corner = IRect::from_corners(
        IVec2::new(x, y),
        IVec2::new(x, y) + MIN_VISIBLE_PX.min(geometry_size(geometry)),
    );
    monitors.iter().any(|&(at, size)| {
        let screen = IRect::from_corners(at, at + size.as_ivec2());
        corner.intersect(screen) == corner
    })
}

fn geometry_size(geometry: &WindowGeometry) -> IVec2 {
    UVec2::from(geometry.size).as_ivec2()
}

/// The geometry shrunk to fit the monitor if it is larger, and centred on it.
pub fn fit_to_monitor(geometry: &WindowGeometry, monitor: (IVec2, UVec2)) -> WindowGeometry {
    let (at, screen) = monitor;
    let size = UVec2::from(geometry.size).min(screen);
    let corner = at + (screen - size).as_ivec2() / 2;
    WindowGeometry {
        position: Some(corner.to_array()),
        size: size.to_array(),
    }
}

/// Applies geometry after the backend has set the window’s scale factor.
pub fn apply_geometry(window: &mut Window, geometry: &WindowGeometry) {
    window
        .resolution
        .set_physical_resolution(geometry.size[0], geometry.size[1]);
    if let Some(position) = geometry.position {
        window.position = WindowPosition::At(IVec2::from(position));
    }
}

/// The geometry of a window as it is now, or `None` when there is nothing worth saving: a
/// fullscreen or minimised (zero-sized) window.
fn current_geometry(window: &Window) -> Option<WindowGeometry> {
    let size = [
        window.resolution.physical_width(),
        window.resolution.physical_height(),
    ];
    if window.mode != WindowMode::Windowed || size.contains(&0) {
        return None;
    }
    let position = match window.position {
        WindowPosition::At(p) => Some(p.to_array()),
        _ => None,
    };
    Some(WindowGeometry { position, size })
}

/// Geometry to restore once winit has installed the monitor's scale factor.
#[derive(Resource)]
pub struct PendingWindowRestore(pub WindowGeometry);

fn restore_created_window(
    pending: Option<Res<PendingWindowRestore>>,
    mut created: MessageReader<WindowCreated>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut commands: Commands,
) {
    let Some(saved) = pending else { return };
    for event in created.read() {
        if let Ok(mut window) = windows.get_mut(event.window) {
            apply_geometry(&mut window, &saved.0);
            commands.remove_resource::<PendingWindowRestore>();
            break;
        }
    }
}

/// Saves the primary window's geometry to `path` once it settles, and recentres a restored
/// window that no monitor shows.
pub struct WindowMemoryPlugin {
    pub path: PathBuf,
}

/// Where [`WindowMemoryPlugin`] saves.
#[derive(Resource)]
struct WindowMemory {
    path: PathBuf,
    /// When the last move or resize was seen (real seconds), until it is saved.
    dirty_since: Option<f64>,
    /// The restored position has been checked against the monitors.
    checked: bool,
}

impl Plugin for WindowMemoryPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WindowMemory {
            path: self.path.clone(),
            dirty_since: None,
            checked: false,
        })
        .add_systems(
            Update,
            (restore_created_window, keep_on_screen, save_when_settled).chain(),
        );
    }
}

/// Once the monitors are known, recentres a restored window that none of them shows (its
/// monitor was unplugged, or the layout changed).
fn keep_on_screen(
    pending: Option<Res<PendingWindowRestore>>,
    mut memory: ResMut<WindowMemory>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    monitors: Query<(&Monitor, Has<PrimaryMonitor>)>,
) {
    if pending.is_some() || memory.checked || monitors.is_empty() {
        return;
    }
    memory.checked = true;
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let Some(geometry) = current_geometry(&window) else {
        return;
    };
    let screens: Vec<(IVec2, UVec2)> = monitors
        .iter()
        .map(|(m, _)| (m.physical_position, m.physical_size()))
        .collect();
    if fits_monitors(&geometry, &screens) {
        return;
    }
    let primary = monitors
        .iter()
        .find(|(_, primary)| *primary)
        .or_else(|| monitors.iter().next())
        .map(|(m, _)| (m.physical_position, m.physical_size()));
    if let Some(primary) = primary {
        let fitted = fit_to_monitor(&geometry, primary);
        info!(from = ?geometry, to = ?fitted, "the saved window position is off screen; recentring");
        apply_geometry(&mut window, &fitted);
    }
}

/// Saves the geometry [`SAVE_DELAY_S`] after the primary window last moved or resized.
fn save_when_settled(
    mut memory: ResMut<WindowMemory>,
    mut moved: MessageReader<WindowMoved>,
    mut resized: MessageReader<WindowResized>,
    windows: Query<(Entity, &Window), With<PrimaryWindow>>,
    time: Res<Time<Real>>,
) {
    let Ok((entity, window)) = windows.single() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let changed =
        moved.read().any(|m| m.window == entity) | resized.read().any(|r| r.window == entity);
    if changed {
        memory.dirty_since = Some(now);
    }
    let Some(since) = memory.dirty_since else {
        return;
    };
    if now - since < SAVE_DELAY_S {
        return;
    }
    memory.dirty_since = None;
    let Some(geometry) = current_geometry(window) else {
        return;
    };
    if let Err(e) = save_geometry(&memory.path, &geometry) {
        warn!(error = %e, "could not save the window geometry");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;
    use bevy::window::{MonitorSelection, WindowResolution};

    use super::*;

    fn temp_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "timefly-window-memory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn geometry(x: i32, y: i32, w: u32, h: u32) -> WindowGeometry {
        WindowGeometry {
            position: Some([x, y]),
            size: [w, h],
        }
    }

    #[test]
    fn only_plain_launches_remember_the_window() {
        assert!(remembers_window(&LaunchArgs::default()));
        let region = LaunchArgs {
            region: crate::args::parse_region("36.1,-112.1,0"),
            ..default()
        };
        assert!(remembers_window(&region), "--region is still play");
        for args in [
            LaunchArgs {
                window: Some((1280, 720)),
                ..default()
            },
            LaunchArgs {
                scrub_bench: true,
                ..default()
            },
            LaunchArgs {
                region_bench: true,
                ..default()
            },
            LaunchArgs {
                journey_bench: true,
                ..default()
            },
            LaunchArgs {
                diagnostics_probe: Some(crate::diagnostics::ProbeMode::Clean),
                ..default()
            },
            LaunchArgs {
                check_data: Some(crate::args::DataCheck::default()),
                ..default()
            },
            LaunchArgs {
                hydro_check: Some(crate::args::HydroCheck::default()),
                ..default()
            },
        ] {
            assert!(!remembers_window(&args), "{args:?}");
        }
    }

    #[test]
    fn state_dir_per_platform() {
        let env = LogRootEnv {
            override_dir: None,
            home: Some(PathBuf::from("/home/p")),
            xdg_state_home: Some(PathBuf::from("/xdg")),
            local_app_data: Some(PathBuf::from("C:/Users/p/AppData/Local")),
        };
        assert_eq!(
            resolve_state_dir("macos", &env),
            Some(PathBuf::from(
                "/home/p/Library/Application Support/Time Fly"
            ))
        );
        assert_eq!(
            resolve_state_dir("windows", &env),
            Some(PathBuf::from("C:/Users/p/AppData/Local/Time Fly"))
        );
        assert_eq!(
            resolve_state_dir("linux", &env),
            Some(PathBuf::from("/xdg/time-fly"))
        );
        let relative_xdg = LogRootEnv {
            xdg_state_home: Some(PathBuf::from("rel")),
            ..env.clone()
        };
        assert_eq!(
            resolve_state_dir("linux", &relative_xdg),
            Some(PathBuf::from("/home/p/.local/state/time-fly")),
            "a relative XDG_STATE_HOME is ignored, as the spec says"
        );
        let overridden = LogRootEnv {
            override_dir: Some(PathBuf::from("/tmp/logs")),
            ..env
        };
        assert_eq!(
            resolve_state_dir("macos", &overridden),
            Some(PathBuf::from("/tmp/logs"))
        );
        assert_eq!(resolve_state_dir("haiku", &LogRootEnv::default()), None);
    }

    #[test]
    fn geometry_round_trips_and_bad_files_are_ignored() {
        let dir = temp_dir();
        let path = dir.join("nested").join(WINDOW_FILE);
        assert_eq!(load_geometry(&path), None, "missing");
        let saved = geometry(-1200, 40, 1600, 900);
        save_geometry(&path, &saved).unwrap();
        assert_eq!(load_geometry(&path), Some(saved));
        let wayland = WindowGeometry {
            position: None,
            size: [1280, 720],
        };
        save_geometry(&path, &wayland).unwrap();
        assert_eq!(load_geometry(&path), Some(wayland));
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(WINDOW_FILE)]);
        for bad in [
            "not json",
            r#"{"position":[0,0],"size":[0,720]}"#,
            r#"{"position":[0,0],"size":[1280,99999]}"#,
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(load_geometry(&path), None, "{bad}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restored_windows_must_be_reachable() {
        let main = (IVec2::ZERO, UVec2::new(2560, 1440));
        let left = (IVec2::new(-1920, 0), UVec2::new(1920, 1080));
        let monitors = [main, left];
        assert!(fits_monitors(&geometry(100, 100, 1280, 720), &monitors));
        assert!(
            fits_monitors(&geometry(-1800, 200, 1280, 720), &monitors),
            "left monitor"
        );
        assert!(
            fits_monitors(&geometry(-640, 100, 1280, 720), &monitors),
            "spans both"
        );
        assert!(
            !fits_monitors(&geometry(3000, 100, 1280, 720), &monitors),
            "right of every monitor (an unplugged one)"
        );
        assert!(
            !fits_monitors(&geometry(2500, 100, 1280, 720), &monitors),
            "only 60 px visible"
        );
        assert!(
            !fits_monitors(&geometry(100, 1420, 1280, 720), &monitors),
            "title bar below the screen"
        );
        assert!(
            !fits_monitors(&geometry(100, 100, 1280, 720), &[]),
            "no monitors"
        );
        let wayland = WindowGeometry {
            position: None,
            size: [1280, 720],
        };
        assert!(fits_monitors(&wayland, &[]));
    }

    #[test]
    fn fitting_shrinks_and_centres() {
        let monitor = (IVec2::new(-1920, 0), UVec2::new(1920, 1080));
        assert_eq!(
            fit_to_monitor(&geometry(5000, 5000, 1280, 720), monitor),
            geometry(-1920 + 320, 180, 1280, 720)
        );
        assert_eq!(
            fit_to_monitor(&geometry(0, 0, 2560, 1440), monitor),
            geometry(-1920, 0, 1920, 1080)
        );
    }

    fn memory_app(path: &Path, window: Window) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                100,
            )))
            .add_message::<WindowCreated>()
            .add_message::<WindowMoved>()
            .add_message::<WindowResized>()
            .add_plugins(WindowMemoryPlugin {
                path: path.to_path_buf(),
            });
        app.world_mut().spawn((window, PrimaryWindow));
        app.update();
        app
    }

    #[test]
    fn saves_once_the_window_settles() {
        let dir = temp_dir();
        let path = dir.join(WINDOW_FILE);
        let mut app = memory_app(
            &path,
            Window {
                position: WindowPosition::At(IVec2::new(300, 200)),
                resolution: WindowResolution::new(1600, 900),
                ..default()
            },
        );
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        app.world_mut().write_message(WindowMoved {
            window,
            position: IVec2::new(300, 200),
        });
        app.update();
        assert_eq!(load_geometry(&path), None, "not before the delay");
        for _ in 0..(SAVE_DELAY_S / 0.1) as usize + 1 {
            app.update();
        }
        assert_eq!(load_geometry(&path), Some(geometry(300, 200, 1600, 900)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn relaunch_restores_physical_size_on_a_double_density_display() {
        let dir = temp_dir();
        let path = dir.join(WINDOW_FILE);
        let saved = geometry(60, 80, 1600, 1000);
        save_geometry(&path, &saved).unwrap();
        let mut window = Window::default();
        window
            .resolution
            .set_scale_factor_and_apply_to_physical_size(2.0);
        let mut app = memory_app(&path, window);
        app.insert_resource(PendingWindowRestore(load_geometry(&path).unwrap()));
        let entity = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        app.world_mut()
            .write_message(WindowCreated { window: entity });
        app.update();
        let window = app.world().get::<Window>(entity).unwrap();
        assert_eq!(window.width(), 800.0);
        assert_eq!(window.height(), 500.0);
        assert_eq!(current_geometry(window), Some(saved));
        assert!(!app.world().contains_resource::<PendingWindowRestore>());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fullscreen_is_not_saved() {
        let dir = temp_dir();
        let path = dir.join(WINDOW_FILE);
        let mut app = memory_app(
            &path,
            Window {
                position: WindowPosition::At(IVec2::new(0, 0)),
                mode: WindowMode::BorderlessFullscreen(MonitorSelection::Primary),
                ..default()
            },
        );
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        app.world_mut().write_message(WindowResized {
            window,
            width: 2560.0,
            height: 1440.0,
        });
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(load_geometry(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreachable_window_is_recentred() {
        let dir = temp_dir();
        let mut app = memory_app(
            &dir.join(WINDOW_FILE),
            Window {
                position: WindowPosition::At(IVec2::new(4000, 100)),
                resolution: WindowResolution::new(1280, 720),
                ..default()
            },
        );
        app.world_mut().spawn((
            Monitor {
                name: None,
                physical_height: 1080,
                physical_width: 1920,
                physical_position: IVec2::ZERO,
                refresh_rate_millihertz: None,
                scale_factor: 1.0,
                video_modes: Vec::new(),
            },
            PrimaryMonitor,
        ));
        app.update();
        let window = app
            .world_mut()
            .query_filtered::<&Window, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        assert_eq!(window.position, WindowPosition::At(IVec2::new(320, 180)));
    }
}
