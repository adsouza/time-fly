//! Session files on native.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

use super::*;
use crate::diagnostics::report::{Home, RedactingWriter};
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
    let lock: &'static mut fd_lock::RwLock<File> = Box::leak(Box::new(fd_lock::RwLock::new(file)));
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

/// Writes a crash report without overwriting an earlier one: `crash.txt`, then `crash-2.txt` …
/// up to [`MAX_CRASH_REPORTS`]. `create_new` also keeps simultaneous panics from interleaving.
pub fn write_crash_report(dir: &Path, report: &str) -> io::Result<PathBuf> {
    for n in 1..=MAX_CRASH_REPORTS {
        let path = dir.join(crash_file_name(n));
        match File::options().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(report.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "{MAX_CRASH_REPORTS} crash reports already saved"
    )))
}

pub(crate) fn log_root_env() -> LogRootEnv {
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
    data_check: bool,
) -> (Findings, Option<NonBlocking>, Option<WorkerGuard>) {
    let mut findings = Findings::default();
    let Some(root) = resolve_log_root(std::env::consts::OS, &log_root_env()) else {
        findings
            .problems
            .push("no log location on this platform; logging to the console only".into());
        return (findings, None, None);
    };
    // Headless checks have their own retention and never displace a player's crash record.
    let root = if data_check {
        root.join("data-checks")
    } else {
        root
    };
    if let Err(e) = fs::create_dir_all(&root) {
        findings.problems.push(format!(
            "could not create the log directory {}: {e}",
            root.display()
        ));
        return (findings, None, None);
    }
    findings.log_root = Some(root.clone());
    if !data_check {
        findings.previous = inspect_newest_session(&root);
    }

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
        .finish(RedactingWriter::new(file, home.clone()));
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
    fn crash_reports_never_overwrite() {
        let dir = temp_root("crash");
        assert_eq!(
            write_crash_report(&dir, "first").unwrap(),
            dir.join("crash.txt")
        );
        assert_eq!(
            write_crash_report(&dir, "second").unwrap(),
            dir.join("crash-2.txt")
        );
        assert_eq!(fs::read_to_string(dir.join("crash.txt")).unwrap(), "first");
        for n in 3..=MAX_CRASH_REPORTS {
            let path = write_crash_report(&dir, &format!("report {n}")).unwrap();
            assert_eq!(path, dir.join(format!("crash-{n}.txt")));
        }
        assert_eq!(MAX_CRASH_REPORTS, 5);
        assert!(write_crash_report(&dir, "too many").is_err());
        // Nothing was overwritten, and no sixth file appeared.
        assert_eq!(fs::read_to_string(dir.join("crash.txt")).unwrap(), "first");
        assert_eq!(
            fs::read_to_string(dir.join("crash-2.txt")).unwrap(),
            "second"
        );
        assert_eq!(
            fs::read_to_string(dir.join("crash-5.txt")).unwrap(),
            "report 5"
        );
        assert!(!dir.join("crash-6.txt").exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 5);
        let _ = fs::remove_dir_all(&dir);
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
