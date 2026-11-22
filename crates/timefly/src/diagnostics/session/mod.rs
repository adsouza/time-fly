//! Session records: what each run of the game was, and how the previous one ended.
//!
//! The top half is pure (record types, the outcome truth table, naming, log-root resolution).
//! The `store` module below does the file I/O on native; the web keeps records in
//! `localStorage` (see `web`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The build that wrote a session, from `build.rs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    pub git_sha: String,
    pub profile: String,
    pub target: String,
    pub features: Vec<String>,
}

impl BuildInfo {
    /// The running build.
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_sha: env!("TIMEFLY_GIT_SHA").to_string(),
            profile: env!("TIMEFLY_BUILD_PROFILE").to_string(),
            target: env!("TIMEFLY_BUILD_TARGET").to_string(),
            features: env!("TIMEFLY_BUILD_FEATURES")
                .split(',')
                .filter(|f| !f.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    /// One line, e.g. `0.1.0 (af374fa, release, aarch64-apple-darwin; system-spans)`.
    pub fn summary(&self) -> String {
        format!(
            "{} ({}, {}, {}; {})",
            self.version,
            self.git_sha,
            self.profile,
            self.target,
            if self.features.is_empty() {
                "no features".to_string()
            } else {
                self.features.join(", ")
            }
        )
    }
}

/// The machine a session ran on. The adapter and backend are filled in once the renderer
/// reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub adapter: Option<String>,
    pub backend: Option<String>,
}

impl PlatformInfo {
    /// The running platform, before the renderer starts.
    pub fn current() -> Self {
        Self {
            os: if cfg!(target_arch = "wasm32") {
                "web".to_string()
            } else {
                std::env::consts::OS.to_string()
            },
            arch: std::env::consts::ARCH.to_string(),
            adapter: None,
            backend: None,
        }
    }

    /// One line, e.g. `macos aarch64 (Apple M2 Pro, Metal)`.
    pub fn summary(&self) -> String {
        let gpu: Vec<&str> = [self.adapter.as_deref(), self.backend.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if gpu.is_empty() {
            format!("{} {}", self.os, self.arch)
        } else {
            format!("{} {} ({})", self.os, self.arch, gpu.join(", "))
        }
    }
}

/// How a session stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Clean,
    Crashed { panicked: bool },
}

/// `session.json`: one per run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: String,
    pub started_utc: String,
    pub build: BuildInfo,
    pub platform: PlatformInfo,
    pub status: SessionStatus,
    pub reached_first_frame: bool,
    /// The player answered the crash notice about this session.
    pub acknowledged: bool,
}

/// What the previous session tells the next launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviousOutcome {
    /// No previous session, or a crash the player already acknowledged.
    Nothing,
    Clean,
    /// The previous session is still running (its lock is held).
    OtherInstanceRunning,
    Crashed {
        during_startup: bool,
        has_crash_report: bool,
    },
}

/// The crash-detection truth table (spec §20).
///
/// A session still marked `Running` whose lock is free has crashed or been killed; one whose
/// lock is held is another instance that is still running. A crash report always means a crash,
/// even if the record says `Clean` (defence in depth against a status written after a panic).
pub fn previous_outcome(
    record: Option<&SessionRecord>,
    lock_free: bool,
    has_crash_report: bool,
) -> PreviousOutcome {
    let Some(record) = record else {
        return PreviousOutcome::Nothing;
    };
    let crashed = PreviousOutcome::Crashed {
        during_startup: !record.reached_first_frame,
        has_crash_report,
    };
    match record.status {
        SessionStatus::Clean if !has_crash_report => PreviousOutcome::Clean,
        SessionStatus::Running if !lock_free => PreviousOutcome::OtherInstanceRunning,
        _ if record.acknowledged => PreviousOutcome::Nothing,
        SessionStatus::Clean | SessionStatus::Running | SessionStatus::Crashed { .. } => crashed,
    }
}

/// The exit code of a run that finished its job and reports a failed result (the scrub bench's
/// FAIL): non-zero for scripts, but a clean session, not a crash. Bevy's own error exits use 1.
pub const FAILED_RESULT_EXIT_CODE: u8 = 3;

/// The status a session ends with when the `App` returns.
///
/// Only a running session that exits successfully (or with [`FAILED_RESULT_EXIT_CODE`]) is
/// clean. A crash recorded earlier (a panic
/// that Bevy caught, for example on the render thread) is never downgraded, and an error exit
/// (render-thread panic, wgpu validation error, out of memory, lost device) is a crash without
/// a panic report, so the next launch shows the notice.
pub fn finish_status(current: SessionStatus, exit: &bevy::app::AppExit) -> SessionStatus {
    match (current, exit) {
        (SessionStatus::Crashed { panicked }, _) => SessionStatus::Crashed { panicked },
        (_, bevy::app::AppExit::Error(code)) if code.get() == FAILED_RESULT_EXIT_CODE => {
            SessionStatus::Clean
        }
        (_, bevy::app::AppExit::Error(_)) => SessionStatus::Crashed { panicked: false },
        (SessionStatus::Running | SessionStatus::Clean, bevy::app::AppExit::Success) => {
            SessionStatus::Clean
        }
    }
}

/// The status of a session the game ended on purpose before or without its `App` (no usable
/// GPU, a failed `--check-data`): clean whatever the exit code, because the player (or CI) was
/// told why. A crash recorded earlier is never downgraded.
pub fn stopped_status(current: SessionStatus) -> SessionStatus {
    match current {
        SessionStatus::Crashed { panicked } => SessionStatus::Crashed { panicked },
        SessionStatus::Running | SessionStatus::Clean => SessionStatus::Clean,
    }
}

/// Calendar UTC time from milliseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

impl UtcTime {
    pub fn from_unix_millis(millis: u64) -> Self {
        let secs = millis / 1000;
        let days = (secs / 86_400) as i64;
        let rem = secs % 86_400;
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: (rem % 3600 / 60) as u32,
            second: (rem % 60) as u32,
            millis: (millis % 1000) as u32,
        }
    }

    /// RFC 3339, e.g. `2026-11-22T09:33:00.123Z`.
    pub fn rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }

    /// Compact and sortable, for directory names: `20261122T093300.123Z`.
    pub fn compact(&self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millis
        )
    }
}

/// `<utc timestamp>_<short id>`. Names sort in start order.
pub fn session_dir_name(started: &UtcTime, id: &str) -> String {
    format!("{}_{id}", started.compact())
}

/// Whether `name` is a directory [`session_dir_name`] made. Nothing else in the log root is
/// ever read or pruned.
pub fn is_session_dir_name(name: &str) -> bool {
    // 20261122T093300.123Z_0123abcd
    const PATTERN: &[u8] = b"ddddddddTdddddd.dddZ_hhhhhhhh";
    name.len() == PATTERN.len()
        && name.bytes().zip(PATTERN).all(|(c, &p)| match p {
            b'd' => c.is_ascii_digit(),
            b'h' => c.is_ascii_digit() || (b'a'..=b'f').contains(&c),
            literal => c == literal,
        })
}

/// The newest session directory among `names`, ignoring other entries.
pub fn newest_session<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    names.into_iter().filter(|n| is_session_dir_name(n)).max()
}

/// The session directories to delete so that `keep` remain, oldest first. `current` is never
/// returned.
pub fn sessions_to_prune<'a>(
    names: impl IntoIterator<Item = &'a str>,
    keep: usize,
    current: &str,
) -> Vec<&'a str> {
    let mut sessions: Vec<&str> = names
        .into_iter()
        .filter(|n| is_session_dir_name(n) && *n != current)
        .collect();
    sessions.sort_unstable();
    // The current session takes one of the `keep` places.
    let keep_others = keep.saturating_sub(1);
    let excess = sessions.len().saturating_sub(keep_others);
    sessions.truncate(excess);
    sessions
}

/// Inputs for [`resolve_log_root`], gathered by the shell.
#[derive(Clone, Debug, Default)]
pub struct LogRootEnv {
    /// `TIMEFLY_LOG_DIR`, for tests and CI.
    pub override_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// `$XDG_STATE_HOME` (Linux).
    pub xdg_state_home: Option<PathBuf>,
    /// `%LOCALAPPDATA%` (Windows).
    pub local_app_data: Option<PathBuf>,
}

/// Where session directories live (spec §20). `os` is `std::env::consts::OS`.
pub fn resolve_log_root(os: &str, env: &LogRootEnv) -> Option<PathBuf> {
    if let Some(dir) = &env.override_dir {
        return Some(dir.clone());
    }
    match os {
        "macos" => env
            .home
            .as_ref()
            .map(|h| h.join("Library").join("Logs").join("Time Fly")),
        "windows" => env
            .local_app_data
            .as_ref()
            .map(|d| d.join("Time Fly").join("logs")),
        "linux" | "freebsd" | "dragonfly" | "netbsd" | "openbsd" => env
            .xdg_state_home
            .clone()
            .filter(|p| p.is_absolute())
            .or_else(|| env.home.as_ref().map(|h| h.join(".local").join("state")))
            .map(|state| state.join("time-fly").join("logs")),
        _ => None,
    }
}

/// Files inside a session directory.
pub const RECORD_FILE: &str = "session.json";
pub const LOCK_FILE: &str = "session.lock";
pub const LOG_FILE: &str = "session.log";
pub const CRASH_FILE: &str = "crash.txt";
/// Crash reports kept per session: the first panic's report is the one that matters, later
/// ones go to `crash-2.txt` and so on.
pub const MAX_CRASH_REPORTS: usize = 5;

/// `crash.txt` for the first report, `crash-<n>.txt` after that.
pub fn crash_file_name(n: usize) -> String {
    if n <= 1 {
        CRASH_FILE.to_string()
    } else {
        format!("crash-{n}.txt")
    }
}

/// The log that best explains a crashed session: the crash report if there is one.
pub fn crash_log_path(session_dir: &Path, has_crash_report: bool) -> PathBuf {
    session_dir.join(if has_crash_report {
        CRASH_FILE
    } else {
        LOG_FILE
    })
}

/// The newest session found in a log root, and what it means for this launch.
#[derive(Clone, Debug)]
pub struct PreviousSession {
    /// The session directory (native) or the session ID (web).
    pub dir: PathBuf,
    pub record: Option<SessionRecord>,
    pub has_crash_report: bool,
    pub outcome: PreviousOutcome,
}

/// Reads the newest session in `root`, tries its lock, and computes [`previous_outcome`].
#[cfg(not(target_arch = "wasm32"))]
pub fn inspect_newest_session(root: &Path) -> Option<PreviousSession> {
    let names = store::session_names(root);
    let dir = root.join(newest_session(names.iter().map(String::as_str))?);
    let record = store::read_record(&dir);
    let has_crash_report = dir.join(CRASH_FILE).is_file();
    let lock_free = store::lock_is_free(&dir);
    let outcome = previous_outcome(record.as_ref(), lock_free, has_crash_report);
    Some(PreviousSession {
        dir,
        record,
        has_crash_report,
        outcome,
    })
}

/// Session files on native.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod store;

/// Session records and logs in the browser's `localStorage`.
#[cfg(target_arch = "wasm32")]
pub(crate) mod web;

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        status: SessionStatus,
        reached_first_frame: bool,
        acknowledged: bool,
    ) -> SessionRecord {
        SessionRecord {
            id: "0123abcd".into(),
            started_utc: "2026-11-22T09:33:00.123Z".into(),
            build: BuildInfo {
                version: "0.1.0".into(),
                git_sha: "abc1234".into(),
                profile: "debug".into(),
                target: "aarch64-apple-darwin".into(),
                features: vec!["system-spans".into()],
            },
            platform: PlatformInfo {
                os: "macos".into(),
                arch: "aarch64".into(),
                adapter: None,
                backend: None,
            },
            status,
            reached_first_frame,
            acknowledged,
        }
    }

    use PreviousOutcome as O;
    use SessionStatus as S;

    #[test]
    fn outcome_without_a_record_is_nothing() {
        assert_eq!(previous_outcome(None, true, false), O::Nothing);
        assert_eq!(previous_outcome(None, false, true), O::Nothing);
    }

    #[test]
    fn outcome_clean() {
        assert_eq!(
            previous_outcome(Some(&record(S::Clean, true, false)), true, false),
            O::Clean
        );
    }

    #[test]
    fn outcome_running_with_free_lock_is_a_crash() {
        assert_eq!(
            previous_outcome(Some(&record(S::Running, true, false)), true, false),
            O::Crashed {
                during_startup: false,
                has_crash_report: false
            }
        );
    }

    #[test]
    fn outcome_running_with_held_lock_is_another_instance() {
        assert_eq!(
            previous_outcome(Some(&record(S::Running, true, false)), false, false),
            O::OtherInstanceRunning
        );
        assert_eq!(
            previous_outcome(Some(&record(S::Running, false, false)), false, false),
            O::OtherInstanceRunning
        );
    }

    #[test]
    fn outcome_crashed_with_and_without_report() {
        assert_eq!(
            previous_outcome(
                Some(&record(S::Crashed { panicked: true }, true, false)),
                true,
                true
            ),
            O::Crashed {
                during_startup: false,
                has_crash_report: true
            }
        );
        assert_eq!(
            previous_outcome(
                Some(&record(S::Crashed { panicked: true }, true, false)),
                true,
                false
            ),
            O::Crashed {
                during_startup: false,
                has_crash_report: false
            }
        );
    }

    #[test]
    fn outcome_before_vs_after_first_frame() {
        assert_eq!(
            previous_outcome(Some(&record(S::Running, false, false)), true, false),
            O::Crashed {
                during_startup: true,
                has_crash_report: false
            }
        );
        assert_eq!(
            previous_outcome(
                Some(&record(S::Crashed { panicked: true }, false, false)),
                true,
                true
            ),
            O::Crashed {
                during_startup: true,
                has_crash_report: true
            }
        );
        assert_eq!(
            previous_outcome(
                Some(&record(S::Crashed { panicked: true }, true, false)),
                true,
                true
            ),
            O::Crashed {
                during_startup: false,
                has_crash_report: true
            }
        );
    }

    #[test]
    fn outcome_acknowledged_crash_is_nothing() {
        assert_eq!(
            previous_outcome(
                Some(&record(S::Crashed { panicked: true }, false, true)),
                true,
                true
            ),
            O::Nothing
        );
        assert_eq!(
            previous_outcome(Some(&record(S::Running, true, true)), true, false),
            O::Nothing
        );
    }

    #[test]
    fn outcome_clean_with_a_crash_report_is_a_crash() {
        assert_eq!(
            previous_outcome(Some(&record(S::Clean, true, false)), true, true),
            O::Crashed {
                during_startup: false,
                has_crash_report: true
            }
        );
        assert_eq!(
            previous_outcome(Some(&record(S::Clean, true, true)), true, true),
            O::Nothing
        );
    }

    #[test]
    fn a_deliberate_stop_is_clean_whatever_the_exit_code() {
        // No usable GPU, a failed data check: the player or CI was told why, so the next
        // launch must not show the crash notice. A recorded crash is still never downgraded.
        assert_eq!(stopped_status(S::Running), S::Clean);
        assert_eq!(stopped_status(S::Clean), S::Clean);
        assert_eq!(
            stopped_status(S::Crashed { panicked: true }),
            S::Crashed { panicked: true }
        );
        assert_eq!(
            stopped_status(S::Crashed { panicked: false }),
            S::Crashed { panicked: false }
        );
    }

    #[test]
    fn finish_status_only_cleans_a_running_success() {
        use bevy::app::AppExit;
        let error = AppExit::error();
        assert_eq!(finish_status(S::Running, &AppExit::Success), S::Clean);
        // Crashed, then `run()` returned: never downgraded.
        for exit in [&AppExit::Success, &error] {
            assert_eq!(
                finish_status(S::Crashed { panicked: true }, exit),
                S::Crashed { panicked: true }
            );
            assert_eq!(
                finish_status(S::Crashed { panicked: false }, exit),
                S::Crashed { panicked: false }
            );
        }
        // A check that finished and failed (the scrub bench) is a result, not a crash.
        assert_eq!(
            finish_status(S::Running, &AppExit::from_code(FAILED_RESULT_EXIT_CODE)),
            S::Clean
        );
        assert_eq!(
            finish_status(
                S::Crashed { panicked: true },
                &AppExit::from_code(FAILED_RESULT_EXIT_CODE)
            ),
            S::Crashed { panicked: true }
        );
        // Render-thread panic, wgpu error: `run()` returns `AppExit::Error`.
        assert_eq!(
            finish_status(S::Running, &error),
            S::Crashed { panicked: false }
        );
    }

    #[test]
    fn record_round_trips_through_json() {
        let r = record(S::Crashed { panicked: true }, true, false);
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            json.contains(r#""status":{"kind":"crashed","panicked":true}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<SessionRecord>(&json).unwrap(), r);
    }

    #[test]
    fn utc_time_from_millis() {
        // 2026-11-22T09:33:00.123Z
        let t = UtcTime::from_unix_millis(1_795_339_980_123);
        assert_eq!(t.rfc3339(), "2026-11-22T09:33:00.123Z");
        assert_eq!(t.compact(), "20261122T093300.123Z");
        assert_eq!(
            UtcTime::from_unix_millis(0).rfc3339(),
            "1970-01-01T00:00:00.000Z"
        );
        // Leap day.
        assert_eq!(
            UtcTime::from_unix_millis(951_782_400_000).rfc3339(),
            "2000-02-29T00:00:00.000Z"
        );
    }

    #[test]
    fn session_dir_names() {
        let t = UtcTime::from_unix_millis(1_795_339_980_123);
        let name = session_dir_name(&t, "0123abcd");
        assert_eq!(name, "20261122T093300.123Z_0123abcd");
        assert!(is_session_dir_name(&name));
        for other in [
            "",
            ".DS_Store",
            "notes",
            "20261122T093300.123Z",
            "20261122T093300.123Z_0123abc",
            "20261122T093300.123Z_0123ABCD",
            "x0261122T093300.123Z_0123abcd",
        ] {
            assert!(!is_session_dir_name(other), "{other}");
        }
    }

    #[test]
    fn newest_and_prune() {
        let names = [
            "20261122T093300.123Z_aaaaaaaa",
            "notes",
            "20261122T093301.000Z_bbbbbbbb",
            "20261121T235959.999Z_cccccccc",
        ];
        assert_eq!(newest_session(names), Some("20261122T093301.000Z_bbbbbbbb"));
        assert_eq!(newest_session(["notes"]), None);
        assert_eq!(
            sessions_to_prune(names, 2, "20261122T093301.000Z_bbbbbbbb"),
            vec!["20261121T235959.999Z_cccccccc"]
        );
        assert_eq!(
            sessions_to_prune(names, 10, "20261122T093301.000Z_bbbbbbbb"),
            Vec::<&str>::new()
        );
        // The current session survives even if it sorts oldest.
        assert_eq!(
            sessions_to_prune(names, 1, "20261121T235959.999Z_cccccccc"),
            vec![
                "20261122T093300.123Z_aaaaaaaa",
                "20261122T093301.000Z_bbbbbbbb"
            ]
        );
    }

    #[test]
    fn log_root_per_os() {
        let env = LogRootEnv {
            override_dir: None,
            home: Some(PathBuf::from("/Users/ann")),
            xdg_state_home: None,
            local_app_data: None,
        };
        assert_eq!(
            resolve_log_root("macos", &env),
            Some(PathBuf::from("/Users/ann/Library/Logs/Time Fly"))
        );

        let linux = LogRootEnv {
            home: Some(PathBuf::from("/home/ann")),
            ..env.clone()
        };
        assert_eq!(
            resolve_log_root("linux", &linux),
            Some(PathBuf::from("/home/ann/.local/state/time-fly/logs"))
        );
        let linux_xdg = LogRootEnv {
            xdg_state_home: Some(PathBuf::from("/srv/state")),
            ..linux.clone()
        };
        assert_eq!(
            resolve_log_root("linux", &linux_xdg),
            Some(PathBuf::from("/srv/state/time-fly/logs"))
        );
        // A relative XDG path is invalid per the spec and ignored.
        let linux_rel = LogRootEnv {
            xdg_state_home: Some(PathBuf::from("state")),
            ..linux.clone()
        };
        assert_eq!(
            resolve_log_root("linux", &linux_rel),
            Some(PathBuf::from("/home/ann/.local/state/time-fly/logs"))
        );

        let windows = LogRootEnv {
            home: Some(PathBuf::from(r"C:\Users\Ann")),
            local_app_data: Some(PathBuf::from(r"C:\Users\Ann\AppData\Local")),
            ..LogRootEnv::default()
        };
        assert_eq!(
            resolve_log_root("windows", &windows),
            Some(
                PathBuf::from(r"C:\Users\Ann\AppData\Local")
                    .join("Time Fly")
                    .join("logs")
            )
        );

        assert_eq!(resolve_log_root("macos", &LogRootEnv::default()), None);
        assert_eq!(resolve_log_root("windows", &LogRootEnv::default()), None);
    }

    #[test]
    fn log_root_override_wins_everywhere() {
        let env = LogRootEnv {
            override_dir: Some(PathBuf::from("/tmp/logs")),
            home: Some(PathBuf::from("/Users/ann")),
            xdg_state_home: Some(PathBuf::from("/srv/state")),
            local_app_data: Some(PathBuf::from(r"C:\Local")),
        };
        for os in ["macos", "linux", "windows", "freebsd"] {
            assert_eq!(
                resolve_log_root(os, &env),
                Some(PathBuf::from("/tmp/logs")),
                "{os}"
            );
        }
    }

    #[test]
    fn crash_log_prefers_the_report() {
        let dir = Path::new("/logs/s");
        assert_eq!(crash_log_path(dir, true), dir.join("crash.txt"));
        assert_eq!(crash_log_path(dir, false), dir.join("session.log"));
    }

    #[test]
    fn platform_summary() {
        let mut p = PlatformInfo {
            os: "macos".into(),
            arch: "aarch64".into(),
            adapter: None,
            backend: None,
        };
        assert_eq!(p.summary(), "macos aarch64");
        p.adapter = Some("Apple M2 Pro".into());
        p.backend = Some("Metal".into());
        assert_eq!(p.summary(), "macos aarch64 (Apple M2 Pro, Metal)");
    }
}
