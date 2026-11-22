//! The flight recorder: an in-memory ring of the most recent detailed events (spec §20).
//!
//! [`Recorder`] is the pure ring. [`RecorderLayer`] is the tracing layer that fills it and, on
//! native, copies it into the session log at the first error.

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::log::tracing::field::{Field, Visit};
use bevy::log::tracing::{Event, Level, Subscriber};
use bevy::log::tracing_subscriber::Layer;
use bevy::log::tracing_subscriber::layer::Context;
use bevy::platform::time::Instant;

use super::{Shared, lock};

/// At most this many events are kept.
pub const MAX_EVENTS: usize = 4096;
/// At most this many bytes of event text are kept.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

/// A ring of formatted events, capped by count and by bytes, dropping the oldest first.
#[derive(Clone, Debug)]
pub struct Recorder {
    events: VecDeque<String>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl Default for Recorder {
    fn default() -> Self {
        Self::with_caps(MAX_EVENTS, MAX_BYTES)
    }
}

impl Recorder {
    pub fn with_caps(max_events: usize, max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            max_events: max_events.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// Adds an event. An event longer than the byte cap is cut to fit.
    pub fn push(&mut self, mut event: String) {
        if event.len() > self.max_bytes {
            let mut cut = self.max_bytes;
            while !event.is_char_boundary(cut) {
                cut -= 1;
            }
            event.truncate(cut);
        }
        while !self.events.is_empty()
            && (self.events.len() >= self.max_events || self.bytes + event.len() > self.max_bytes)
        {
            if let Some(old) = self.events.pop_front() {
                self.bytes -= old.len();
            }
        }
        self.bytes += event.len();
        self.events.push_back(event);
    }

    /// The events, oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        self.events.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Bytes of event text held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Marker written before the recorder's contents in the session log.
pub const CONTEXT_MARKER: &str = "--- context before error ---";
/// After the first error, the recorder is copied to the session log at most this often.
const DUMP_INTERVAL: Duration = Duration::from_secs(60);

thread_local! {
    static IN_RECORDER: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread is inside the recorder layer (holding the recorder lock). The panic hook
/// checks it so a panic there can't deadlock on the lock.
pub(crate) fn in_recorder() -> bool {
    IN_RECORDER.with(Cell::get)
}

/// One recorder line: `[  12.345s] DEBUG target: message key=value`.
pub fn format_line(
    elapsed: Duration,
    level: &Level,
    target: &str,
    message: &str,
    fields: &str,
) -> String {
    format!(
        "[{:>9.3}s] {:>5} {target}: {message}{fields}",
        elapsed.as_secs_f64(),
        level.as_str()
    )
}

#[derive(Default)]
struct FieldText {
    message: String,
    fields: String,
}

impl Visit for FieldText {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// Fills the [`Recorder`] and, on native, copies it into the session log at the first error
/// and then at most once a minute.
pub(crate) struct RecorderLayer {
    shared: Arc<Shared>,
    last_dump: Mutex<Option<Instant>>,
}

impl RecorderLayer {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            last_dump: Mutex::new(None),
        }
    }

    fn dump_due(&self) -> bool {
        let mut last = lock(&self.last_dump);
        let due = last.is_none_or(|t| t.elapsed() >= DUMP_INTERVAL);
        if due {
            *last = Some(Instant::now());
        }
        due
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn dump(&self, events: &[String]) {
        use std::io::Write as _;
        const CONTEXT_END: &str = "--- end of context ---";
        let Some(mut writer) = self.shared.log_writer.clone() else {
            return;
        };
        let mut text = format!("{CONTEXT_MARKER}\n");
        for event in events {
            text.push_str(event);
            text.push('\n');
        }
        text.push_str(CONTEXT_END);
        text.push('\n');
        let _ = writer.write_all(text.as_bytes());
    }

    #[cfg(target_arch = "wasm32")]
    fn dump(&self, _events: &[String]) {
        // The web saves the whole recorder to localStorage instead.
        super::session::web::save_log(&self.shared);
    }
}

impl<S: Subscriber> Layer<S> for RecorderLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut text = FieldText::default();
        event.record(&mut text);
        let line = format_line(
            self.shared.started.elapsed(),
            meta.level(),
            meta.target(),
            &text.message,
            &text.fields,
        );
        let is_error = *meta.level() == Level::ERROR;

        IN_RECORDER.with(|f| f.set(true));
        let dump = {
            let mut recorder = lock(&self.shared.recorder);
            recorder.push(line);
            (is_error && self.dump_due()).then(|| recorder.snapshot())
        };
        IN_RECORDER.with(|f| f.set(false));
        if let Some(events) = dump {
            self.dump(&events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_oldest_first() {
        let mut r = Recorder::default();
        for i in 0..5 {
            r.push(format!("e{i}"));
        }
        assert_eq!(r.snapshot(), vec!["e0", "e1", "e2", "e3", "e4"]);
    }

    #[test]
    fn count_cap_drops_oldest() {
        let mut r = Recorder::with_caps(3, MAX_BYTES);
        for i in 0..5 {
            r.push(format!("e{i}"));
        }
        assert_eq!(r.snapshot(), vec!["e2", "e3", "e4"]);
        assert_eq!(r.bytes(), 6);
    }

    #[test]
    fn default_caps_are_the_spec_values() {
        let mut r = Recorder::default();
        for i in 0..(MAX_EVENTS + 10) {
            r.push(format!("{i}"));
        }
        assert_eq!(r.len(), 4096);
        assert_eq!(r.snapshot().first().map(String::as_str), Some("10"));
        assert_eq!(MAX_BYTES, 2 * 1024 * 1024);
    }

    #[test]
    fn byte_cap_drops_oldest() {
        let mut r = Recorder::with_caps(100, 10);
        r.push("aaaa".into());
        r.push("bbbb".into());
        r.push("cccc".into());
        assert_eq!(r.snapshot(), vec!["bbbb", "cccc"]);
        assert!(r.bytes() <= 10);
    }

    #[test]
    fn line_format() {
        let line = format_line(
            Duration::from_millis(12_345),
            &Level::DEBUG,
            "timefly::probe",
            "probe step 2",
            " step=2",
        );
        assert_eq!(
            line,
            "[   12.345s] DEBUG timefly::probe: probe step 2 step=2"
        );
    }

    #[test]
    fn oversized_event_is_cut_to_the_cap() {
        let mut r = Recorder::with_caps(100, 8);
        r.push("old".into());
        r.push("é123456789".into());
        let events = r.snapshot();
        assert_eq!(events.len(), 1);
        assert!(events[0].len() <= 8);
        assert!(events[0].starts_with("é12"));
    }
}
