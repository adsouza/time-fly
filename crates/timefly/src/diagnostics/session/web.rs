//! Session records and logs in the browser's `localStorage`, under a `timefly.` namespace.

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
    let _ = window.add_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref());
    callback.forget();
}
