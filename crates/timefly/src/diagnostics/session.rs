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
/// lock is held is another instance that is still running.
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
        SessionStatus::Clean => PreviousOutcome::Clean,
        SessionStatus::Running if !lock_free => PreviousOutcome::OtherInstanceRunning,
        SessionStatus::Running | SessionStatus::Crashed { .. } if record.acknowledged => {
            PreviousOutcome::Nothing
        }
        SessionStatus::Running | SessionStatus::Crashed { .. } => crashed,
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
pub(crate) mod store {
    use std::fs::{self, File};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};

    use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

    use super::*;
    use crate::diagnostics::report::{Home, redact};
    use crate::diagnostics::{Findings, KEEP_SESSIONS};

    /// Names of the session directories in `root`, in no particular order.
    pub fn session_names(root: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(root) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| is_session_dir_name(n))
            .collect()
    }

    pub fn read_record(dir: &Path) -> Option<SessionRecord> {
        serde_json::from_str(&fs::read_to_string(dir.join(RECORD_FILE)).ok()?).ok()
    }

    /// Writes `session.json` through a temporary file, so a crash mid-write can't leave half
    /// a record.
    pub fn write_record(dir: &Path, record: &SessionRecord) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        let tmp = dir.join(format!("{RECORD_FILE}.tmp"));
        fs::write(&tmp, json)?;
        fs::rename(tmp, dir.join(RECORD_FILE))
    }

    /// Marks a previous session's crash as answered, so its notice doesn't show again.
    pub fn acknowledge(dir: &Path) -> io::Result<()> {
        let mut record =
            read_record(dir).ok_or_else(|| io::Error::other("no readable session record"))?;
        record.acknowledged = true;
        write_record(dir, &record)
    }

    /// Whether nothing holds the session's lock. A missing lock file counts as free.
    pub fn lock_is_free(dir: &Path) -> bool {
        let Ok(file) = File::options()
            .read(true)
            .write(true)
            .open(dir.join(LOCK_FILE))
        else {
            return true;
        };
        let mut lock = fd_lock::RwLock::new(file);
        lock.try_write().is_ok()
    }

    /// Takes the session lock for the life of the process. The OS releases it when the process
    /// ends, however it ends.
    fn hold_lock(dir: &Path) -> io::Result<()> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE))?;
        let lock: &'static mut fd_lock::RwLock<File> =
            Box::leak(Box::new(fd_lock::RwLock::new(file)));
        let guard = lock.try_write()?;
        std::mem::forget(guard);
        Ok(())
    }

    /// Deletes old session directories so that `keep` remain. Skips sessions that are still
    /// running. Returns the names deleted and any problems.
    pub fn prune(root: &Path, keep: usize, current: &str) -> (Vec<String>, Vec<String>) {
        let names = session_names(root);
        let mut pruned = Vec::new();
        let mut problems = Vec::new();
        for name in sessions_to_prune(names.iter().map(String::as_str), keep, current) {
            let dir = root.join(name);
            if !lock_is_free(&dir) {
                continue;
            }
            match fs::remove_dir_all(&dir) {
                Ok(()) => pruned.push(name.to_string()),
                Err(e) => problems.push(format!("could not prune session {name}: {e}")),
            }
        }
        (pruned, problems)
    }

    /// Writes each buffer with the home directory replaced by `~`. The log worker hands it one
    /// formatted event per write.
    pub struct RedactingWriter<W> {
        inner: W,
        home: Home,
    }

    impl<W: Write> Write for RedactingWriter<W> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner
                .write_all(redact(&String::from_utf8_lossy(buf), &self.home).as_bytes())?;
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    fn log_root_env() -> LogRootEnv {
        let dirs = directories::BaseDirs::new();
        LogRootEnv {
            override_dir: std::env::var_os("TIMEFLY_LOG_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            home: dirs.as_ref().map(|d| d.home_dir().to_path_buf()),
            xdg_state_home: dirs
                .as_ref()
                .and_then(|d| d.state_dir())
                .map(Path::to_path_buf),
            local_app_data: dirs.as_ref().map(|d| d.data_local_dir().to_path_buf()),
        }
    }

    /// Steps 1-3 of `diagnostics::start` on native: find the log root, inspect the previous
    /// session, open this one (directory, lock, record, log writer) and prune old ones.
    pub fn open(
        record: &SessionRecord,
        started: &UtcTime,
        home: &Home,
    ) -> (Findings, Option<NonBlocking>, Option<WorkerGuard>) {
        let mut findings = Findings::default();
        let Some(root) = resolve_log_root(std::env::consts::OS, &log_root_env()) else {
            findings
                .problems
                .push("no log location on this platform; logging to the console only".into());
            return (findings, None, None);
        };
        if let Err(e) = fs::create_dir_all(&root) {
            findings.problems.push(format!(
                "could not create the log directory {}: {e}",
                root.display()
            ));
            return (findings, None, None);
        }
        findings.log_root = Some(root.clone());
        findings.previous = inspect_newest_session(&root);

        let name = session_dir_name(started, &record.id);
        let dir = root.join(&name);
        let opened = fs::create_dir_all(&dir)
            .and_then(|()| hold_lock(&dir))
            .and_then(|()| write_record(&dir, record))
            .and_then(|()| {
                File::options()
                    .create(true)
                    .append(true)
                    .open(dir.join(LOG_FILE))
            });
        let file = match opened {
            Ok(file) => file,
            Err(e) => {
                findings.problems.push(format!(
                    "could not open the session directory {}: {e}",
                    dir.display()
                ));
                return (findings, None, None);
            }
        };
        findings.session_dir = Some(dir);
        let (pruned, problems) = prune(&root, KEEP_SESSIONS, &name);
        findings.pruned = pruned;
        findings.problems.extend(problems);

        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(false)
            .thread_name("timefly-log")
            .finish(RedactingWriter {
                inner: file,
                home: home.clone(),
            });
        (findings, Some(writer), Some(guard))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp_root(name: &str) -> PathBuf {
            let root =
                std::env::temp_dir().join(format!("timefly-store-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            root
        }

        fn running_record() -> SessionRecord {
            SessionRecord {
                id: "0123abcd".into(),
                started_utc: "2026-11-22T09:33:00.123Z".into(),
                build: BuildInfo::current(),
                platform: PlatformInfo::current(),
                status: SessionStatus::Running,
                reached_first_frame: true,
                acknowledged: false,
            }
        }

        #[test]
        fn acknowledging_hides_the_crash() {
            let root = temp_root("ack");
            let dir = root.join("20261122T093300.123Z_0123abcd");
            fs::create_dir_all(&dir).unwrap();
            write_record(&dir, &running_record()).unwrap();
            let outcome = |root: &Path| inspect_newest_session(root).unwrap().outcome;
            assert_eq!(
                outcome(&root),
                PreviousOutcome::Crashed {
                    during_startup: false,
                    has_crash_report: false
                }
            );
            acknowledge(&dir).unwrap();
            assert_eq!(outcome(&root), PreviousOutcome::Nothing);
            let _ = fs::remove_dir_all(&root);
        }

        #[test]
        fn a_held_lock_is_another_instance_and_is_not_pruned() {
            let root = temp_root("lock");
            let dir = root.join("20261122T093300.123Z_0123abcd");
            fs::create_dir_all(&dir).unwrap();
            write_record(&dir, &running_record()).unwrap();
            assert!(lock_is_free(&dir));
            // Held until this test process exits.
            hold_lock(&dir).unwrap();
            assert!(!lock_is_free(&dir));
            assert_eq!(
                inspect_newest_session(&root).unwrap().outcome,
                PreviousOutcome::OtherInstanceRunning
            );
            let (pruned, problems) = prune(&root, 1, "20261122T093301.000Z_ffffffff");
            assert!(pruned.is_empty() && problems.is_empty());
            assert!(dir.exists());
            let _ = fs::remove_dir_all(&root);
        }
    }
}

/// Session records and logs in the browser's `localStorage`, under a `timefly.` namespace.
#[cfg(target_arch = "wasm32")]
pub(crate) mod web {
    use std::sync::Arc;

    use bevy::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use super::*;
    use crate::diagnostics::{DiagnosticsHandle, Findings, Shared, lock};

    /// Web sessions kept.
    const KEEP_WEB_SESSIONS: usize = 3;
    /// Bytes of recorder text saved per session.
    const MAX_LOG_BYTES: usize = 1024 * 1024;
    const SESSIONS_KEY: &str = "timefly.sessions";

    pub fn record_key(id: &str) -> String {
        format!("timefly.session.{id}")
    }
    pub fn log_key(id: &str) -> String {
        format!("timefly.log.{id}")
    }
    pub fn crash_key(id: &str) -> String {
        format!("timefly.crash.{id}")
    }

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn get(key: &str) -> Option<String> {
        storage()?.get_item(key).ok()?
    }

    pub fn set(key: &str, value: &str) {
        if let Some(storage) = storage()
            && storage.set_item(key, value).is_err()
        {
            warn!(key, "could not save to localStorage");
        }
    }

    fn remove(key: &str) {
        if let Some(storage) = storage() {
            let _ = storage.remove_item(key);
        }
    }

    fn session_ids() -> Vec<String> {
        get(SESSIONS_KEY)
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn read_record(id: &str) -> Option<SessionRecord> {
        serde_json::from_str(&get(&record_key(id))?).ok()
    }

    pub fn save_record(record: &SessionRecord) {
        if let Ok(json) = serde_json::to_string(record) {
            set(&record_key(&record.id), &json);
        }
    }

    pub fn acknowledge(id: &str) {
        if let Some(mut record) = read_record(id) {
            record.acknowledged = true;
            save_record(&record);
        }
    }

    /// Inspects the previous web session, registers this one and keeps the newest three.
    pub fn open(record: &SessionRecord) -> Findings {
        let mut findings = Findings::default();
        let mut ids = session_ids();
        if let Some(previous_id) = ids.last() {
            let previous = read_record(previous_id);
            let has_crash_report = get(&crash_key(previous_id)).is_some();
            // The web has no lock: a session still marked running did not reach `pagehide`.
            let outcome = previous_outcome(previous.as_ref(), true, has_crash_report);
            findings.previous = Some(PreviousSession {
                dir: PathBuf::from(previous_id),
                record: previous,
                has_crash_report,
                outcome,
            });
        }
        ids.push(record.id.clone());
        while ids.len() > KEEP_WEB_SESSIONS {
            let old = ids.remove(0);
            for key in [record_key(&old), log_key(&old), crash_key(&old)] {
                remove(&key);
            }
            findings.pruned.push(old);
        }
        if let Ok(json) = serde_json::to_string(&ids) {
            set(SESSIONS_KEY, &json);
        }
        save_record(record);
        findings
    }

    /// The newest `max` bytes of `text`, cut at a line start when possible.
    pub fn newest_bytes(text: &str, max: usize) -> &str {
        if text.len() <= max {
            return text;
        }
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let tail = &text[start..];
        tail.find('\n').map_or(tail, |i| &tail[i + 1..])
    }

    /// Saves the flight recorder (newest 1 MiB, redacted) for this session.
    pub fn save_log(shared: &Shared) {
        let Ok(recorder) = shared.recorder.try_lock() else {
            return;
        };
        let text = recorder.snapshot().join("\n");
        drop(recorder);
        let id = lock(&shared.record).id.clone();
        let text = crate::diagnostics::report::redact(&text, &shared.home);
        set(&log_key(&id), newest_bytes(&text, MAX_LOG_BYTES));
    }

    /// Every 2 s while running.
    pub fn save_log_periodically(
        handle: Res<DiagnosticsHandle>,
        time: Res<Time>,
        mut timer: Local<Option<Timer>>,
    ) {
        let timer = timer.get_or_insert_with(|| Timer::from_seconds(2.0, TimerMode::Repeating));
        if timer.tick(time.delta()).just_finished() {
            save_log(&handle.0);
        }
    }

    /// On `pagehide`, saves the recorder and marks a still-running session clean. A crashed
    /// session stays crashed.
    pub fn install_pagehide(shared: Arc<Shared>) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let callback = Closure::<dyn FnMut()>::new(move || {
            save_log(&shared);
            let Ok(record) = shared.record.try_lock() else {
                return;
            };
            let id = record.id.clone();
            drop(record);
            if let Some(mut stored) = read_record(&id)
                && stored.status == SessionStatus::Running
            {
                stored.status = SessionStatus::Clean;
                save_record(&stored);
            }
        });
        let _ =
            window.add_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref());
        callback.forget();
    }
}

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
