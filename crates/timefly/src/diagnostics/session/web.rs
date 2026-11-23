//! Session records and logs in the browser's `localStorage`, under a `timefly.` namespace.

use std::sync::Arc;

use bevy::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::*;
use crate::diagnostics::{DiagnosticsHandle, Findings, HOOK_LOCK_WAIT, Shared, try_lock_for};

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

thread_local! {
    static TAB_STORAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn storage() -> Option<web_sys::Storage> {
    let window = web_sys::window()?;
    if TAB_STORAGE.get() {
        window.session_storage().ok()?
    } else {
        window.local_storage().ok()?
    }
}

pub fn get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok()?
}

/// Stores `value`. Errors (no storage, quota exceeded) are returned for the caller to report:
/// some callers run before the log subscriber exists.
pub fn set(key: &str, value: &str) -> Result<(), String> {
    storage()
        .ok_or_else(|| "localStorage is unavailable".to_string())?
        .set_item(key, value)
        .map_err(|_| format!("could not save {key} to localStorage"))
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

pub fn save_record(record: &SessionRecord) -> Result<(), String> {
    let json = serde_json::to_string(record).map_err(|e| e.to_string())?;
    set(&record_key(&record.id), &json)
}

pub fn acknowledge(id: &str) -> Result<(), String> {
    let mut record =
        read_record(id).ok_or_else(|| format!("no readable record for session {id}"))?;
    record.acknowledged = true;
    save_record(&record)
}

/// Registers this session and prunes old sessions, keeping live tabs and any pending notice.
pub fn open(record: &SessionRecord, held: Option<js_sys::Array>) -> Findings {
    // Without locks use tab-local storage. A cloned tab can inherit a running record;
    // in that case only an explicit crash status or report is evidence of a crash.
    TAB_STORAGE.set(held.is_none());
    let live: std::collections::HashSet<_> = held
        .iter()
        .flat_map(|a| a.iter())
        .filter_map(|v| v.as_string())
        .collect();
    let mut findings = Findings::default();
    let mut ids = session_ids();
    if let Some(previous_id) = ids.last() {
        let previous = read_record(previous_id);
        let has_crash_report = get(&crash_key(previous_id)).is_some();
        let lock_free = !live.contains(previous_id) && (held.is_some() || has_crash_report);
        let outcome = previous_outcome(previous.as_ref(), lock_free, has_crash_report);
        findings.previous = Some(PreviousSession {
            dir: PathBuf::from(previous_id),
            record: previous,
            has_crash_report,
            outcome,
        });
    }
    let pending_notice = findings
        .previous
        .as_ref()
        .filter(|previous| matches!(previous.outcome, PreviousOutcome::Crashed { .. }))
        .and_then(|previous| previous.record.as_ref())
        .map(|record| &record.id);
    ids.push(record.id.clone());
    while ids.len() > KEEP_WEB_SESSIONS {
        let Some(index) = ids
            .iter()
            .position(|id| id != &record.id && !live.contains(id) && Some(id) != pending_notice)
        else {
            break;
        };
        let old = ids.remove(index);
        for key in [record_key(&old), log_key(&old), crash_key(&old)] {
            remove(&key);
        }
        findings.pruned.push(old);
    }
    let saved = serde_json::to_string(&ids)
        .map_err(|e| e.to_string())
        .and_then(|json| set(SESSIONS_KEY, &json))
        .and_then(|()| save_record(record));
    if let Err(e) = saved {
        findings.problems.push(e);
    }
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

/// Saves the flight recorder (newest 1 MiB, redacted) for this session. Skips quietly when a
/// lock is busy (the next save catches up); returns storage errors.
pub fn save_log(shared: &Shared) -> Result<(), String> {
    let Ok(recorder) = shared.recorder.try_lock() else {
        return Ok(());
    };
    let text = recorder.snapshot().join("\n");
    drop(recorder);
    let Some(id) = try_lock_for(&shared.record, HOOK_LOCK_WAIT).map(|r| r.id.clone()) else {
        return Ok(());
    };
    let text = crate::diagnostics::report::redact(&text, &shared.home);
    set(&log_key(&id), newest_bytes(&text, MAX_LOG_BYTES))
}

/// Every 2 s while running.
pub fn save_log_periodically(
    handle: Res<DiagnosticsHandle>,
    time: Res<Time>,
    mut timer: Local<Option<Timer>>,
) {
    let timer = timer.get_or_insert_with(|| Timer::from_seconds(2.0, TimerMode::Repeating));
    if timer.tick(time.delta()).just_finished()
        && let Err(e) = save_log(&handle.0)
    {
        handle.0.problem(e);
    }
}

/// A page in the back/forward cache may be discarded without another event. Mark it
/// clean when hidden and restore its running status only if it actually resumes.
pub fn install_page_lifecycle(shared: Arc<Shared>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::<dyn FnMut(web_sys::PageTransitionEvent)>::new(
        move |event: web_sys::PageTransitionEvent| {
            let hiding = event.type_() == "pagehide";
            if hiding {
                if let Err(e) = save_log(&shared) {
                    shared.problem(e);
                }
            } else if !event.persisted() {
                return;
            }

            let Ok(record) = shared.record.try_lock() else {
                return;
            };
            if record.status != SessionStatus::Running {
                return;
            }
            let id = record.id.clone();
            drop(record);
            if let Some(mut stored) = read_record(&id) {
                let (from, to) = if hiding {
                    (SessionStatus::Running, SessionStatus::Clean)
                } else {
                    (SessionStatus::Clean, SessionStatus::Running)
                };
                if stored.status == from {
                    stored.status = to;
                    if let Err(e) = save_record(&stored) {
                        shared.problem(e);
                    }
                }
            }
        },
    );
    for event in ["pagehide", "pageshow"] {
        let _ = window.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
    }
    callback.forget();
}
