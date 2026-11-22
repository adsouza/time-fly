//! Crash reports, redaction and the report-page URL.

use std::backtrace::Backtrace;
use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::sync::Arc;

use super::session::{BuildInfo, PlatformInfo, SessionStatus};
use super::{DiagnosticsContext, Shared, lock, recorder};

/// The issue form for crash reports. `<session-id>` is replaced by [`report_url`].
pub const REPORT_URL: &str = "https://github.com/adsouza/time-fly/issues/new?template=crash_report.yml&title=Crash%20report%20<session-id>";

/// The parts of a panic that go into a report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanicSummary {
    pub message: String,
    pub location: Option<String>,
    pub thread: Option<String>,
    pub backtrace: String,
}

/// `crash.txt`: message, location, backtrace, build, last known game state and recent events.
pub fn format_crash_report(
    panic: &PanicSummary,
    build: &BuildInfo,
    platform: &PlatformInfo,
    context: &DiagnosticsContext,
    recent: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let none = || "unknown".to_string();
    // Writing to a String cannot fail.
    let _ = writeln!(out, "Time Fly crash report");
    let _ = writeln!(out);
    let _ = writeln!(out, "Panic:    {}", panic.message);
    let _ = writeln!(
        out,
        "Location: {}",
        panic.location.clone().unwrap_or_else(none)
    );
    let _ = writeln!(
        out,
        "Thread:   {}",
        panic.thread.clone().unwrap_or_else(none)
    );
    let _ = writeln!(out, "Build:    {}", build.summary());
    let _ = writeln!(out, "Platform: {}", platform.summary());
    let _ = writeln!(out);
    let _ = writeln!(out, "Last known state:");
    for line in context.describe() {
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Backtrace:");
    let _ = writeln!(out, "{}", panic.backtrace.trim_end());
    let _ = writeln!(out);
    let _ = writeln!(out, "Recent events ({}):", recent.len());
    for event in recent {
        let _ = writeln!(out, "{event}");
    }
    out
}

/// The forms of the home directory to hide from logs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Home {
    forms: Vec<String>,
}

impl Home {
    /// `raw` as the environment gives it, `canonical` as the file system resolves it.
    pub fn new(raw: &str, canonical: Option<&str>) -> Self {
        let mut forms = Vec::new();
        for form in [Some(raw), canonical].into_iter().flatten() {
            let trimmed = form.trim_end_matches(['/', '\\']);
            // An empty or root home would redact every absolute path.
            if trimmed.is_empty() || trimmed.ends_with(':') {
                continue;
            }
            forms.push(trimmed.to_string());
            // Windows paths also appear with forward slashes.
            if trimmed.contains('\\') {
                forms.push(trimmed.replace('\\', "/"));
            }
        }
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        forms.dedup();
        Self { forms }
    }
}

/// Replaces the home directory with `~`.
pub fn redact(text: &str, home: &Home) -> String {
    let mut text = text.to_string();
    for form in &home.forms {
        text = replace_path_prefix(&text, form);
    }
    text
}

/// Replaces `form` with `~` where it is a whole path component: `/Users/ann/x` and
/// `/Users/ann` match `/Users/ann`, `/Users/anna` does not.
fn replace_path_prefix(text: &str, form: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(form) {
        let after = &rest[at + form.len()..];
        let continues_name = after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-');
        out.push_str(&rest[..at]);
        out.push_str(if continues_name { form } else { "~" });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The crash-report form for one session, with the title, session ID and platform filled in.
pub fn report_url(session_id: &str, platform_summary: &str) -> String {
    let id = percent_encode(session_id);
    format!(
        "{}&session_id={id}&platform={}",
        REPORT_URL.replace("<session-id>", &id),
        percent_encode(platform_summary)
    )
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Installs the panic hook: it writes the crash report synchronously, marks the session
/// crashed, then calls the previous hook (and, on the web, `console_error_panic_hook`).
pub(crate) fn install_panic_hook(shared: Arc<Shared>) {
    thread_local! {
        static IN_HOOK: Cell<bool> = const { Cell::new(false) };
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic inside the hook must not recurse into it.
        if !IN_HOOK.with(|f| f.replace(true)) {
            record_panic(&shared, info);
            IN_HOOK.with(|f| f.set(false));
        }
        previous(info);
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::hook(info);
    }));
}

fn record_panic(shared: &Shared, info: &PanicHookInfo<'_>) {
    let panic = PanicSummary {
        message: info
            .payload_as_str()
            .unwrap_or("(non-string panic payload)")
            .to_string(),
        location: info.location().map(ToString::to_string),
        thread: std::thread::current().name().map(str::to_string),
        backtrace: Backtrace::force_capture().to_string(),
    };
    let context = lock(&shared.context).clone();
    // If this thread panicked inside the recorder layer it holds the recorder lock.
    let recent = if recorder::in_recorder() {
        vec!["(flight recorder unavailable: the panic happened while recording)".to_string()]
    } else {
        lock(&shared.recorder).snapshot()
    };
    let (build, platform, id) = {
        let record = lock(&shared.record);
        (
            record.build.clone(),
            record.platform.clone(),
            record.id.clone(),
        )
    };
    let report = redact(
        &format_crash_report(&panic, &build, &platform, &context, &recent),
        &shared.home,
    );

    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dir) = shared.session_dir() {
        let path = dir.join(super::session::CRASH_FILE);
        match std::fs::write(&path, &report) {
            Ok(()) => eprintln!(
                "Time Fly crashed. A crash report is saved at {}",
                path.display()
            ),
            Err(e) => eprintln!("Time Fly crashed, and the crash report could not be saved: {e}"),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        super::session::web::set(&super::session::web::crash_key(&id), &report);
        super::session::web::save_log(shared);
    }

    shared.update_record(|r| r.status = SessionStatus::Crashed { panicked: true });
    if !recorder::in_recorder() {
        bevy::log::error!(
            session = %id,
            location = panic.location.as_deref().unwrap_or("unknown"),
            "panic: {}",
            panic.message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_unix_home() {
        let home = Home::new("/Users/ann", None);
        assert_eq!(
            redact(
                "opened /Users/ann/Library/Logs/Time Fly/x and /Users/ann",
                &home
            ),
            "opened ~/Library/Logs/Time Fly/x and ~"
        );
    }

    #[test]
    fn redacts_canonical_form_too() {
        let home = Home::new("/home/ann", Some("/data/home/ann"));
        assert_eq!(
            redact("a /home/ann/x b /data/home/ann/y", &home),
            "a ~/x b ~/y"
        );
    }

    #[test]
    fn redacts_windows_home() {
        let home = Home::new(r"C:\Users\Ann", Some(r"\\?\C:\Users\Ann"));
        assert_eq!(
            redact(
                r"log at C:\Users\Ann\AppData\Local\Time Fly\logs and \\?\C:\Users\Ann\x and C:/Users/Ann/y",
                &home
            ),
            r"log at ~\AppData\Local\Time Fly\logs and ~\x and ~/y"
        );
    }

    #[test]
    fn leaves_other_text_unchanged() {
        let home = Home::new("/Users/ann", None);
        for text in ["nothing here", "/Users/anna/file", "/Users/bob/x", ""] {
            assert_eq!(redact(text, &home), text);
        }
    }

    #[test]
    fn trailing_separator_and_root_home() {
        assert_eq!(
            redact("/Users/ann/x", &Home::new("/Users/ann/", None)),
            "~/x"
        );
        // A root or empty home must not redact every path.
        assert_eq!(redact("/etc/hosts", &Home::new("/", None)), "/etc/hosts");
        assert_eq!(redact("/etc/hosts", &Home::new("", None)), "/etc/hosts");
    }

    #[test]
    fn crash_report_contents() {
        let panic = PanicSummary {
            message: "index out of bounds".into(),
            location: Some("crates/timefly/src/globe.rs:42:7".into()),
            thread: Some("main".into()),
            backtrace: "   0: timefly::globe::draw".into(),
        };
        let build = BuildInfo {
            version: "0.1.0".into(),
            git_sha: "abc1234".into(),
            profile: "release".into(),
            target: "aarch64-apple-darwin".into(),
            features: vec!["system-spans".into()],
        };
        let platform = PlatformInfo {
            os: "macos".into(),
            arch: "aarch64".into(),
            adapter: Some("Apple M2 Pro".into()),
            backend: Some("Metal".into()),
        };
        let context = DiagnosticsContext {
            game_age_years: Some(66_000_000.0),
            load_state: Some("Ready".into()),
            window_size: Some((1280, 720)),
            ..Default::default()
        };
        let recent = vec![
            "[   1.000s] DEBUG timefly: step one".to_string(),
            "[   2.000s]  INFO timefly: step two".to_string(),
        ];
        let report = format_crash_report(&panic, &build, &platform, &context, &recent);
        for needle in [
            "index out of bounds",
            "crates/timefly/src/globe.rs:42:7",
            "main",
            "timefly::globe::draw",
            "0.1.0",
            "abc1234",
            "release",
            "aarch64-apple-darwin",
            "system-spans",
            "Apple M2 Pro",
            "Metal",
            "66000000",
            "Ready",
            "1280x720",
            "step one",
            "step two",
        ] {
            assert!(report.contains(needle), "missing {needle:?} in:\n{report}");
        }
        assert!(report.find("step one") < report.find("step two"));
    }

    #[test]
    fn report_url_encodes_values() {
        let url = report_url("0123abcd", "macos aarch64 (Apple M2 Pro, Metal)");
        assert_eq!(
            url,
            "https://github.com/adsouza/time-fly/issues/new?template=crash_report.yml\
             &title=Crash%20report%200123abcd&session_id=0123abcd\
             &platform=macos%20aarch64%20%28Apple%20M2%20Pro%2C%20Metal%29"
        );
        let odd = report_url("a b&c", "x");
        assert!(
            odd.contains("title=Crash%20report%20a%20b%26c&session_id=a%20b%26c&platform=x"),
            "{odd}"
        );
    }

    #[test]
    fn percent_encoding() {
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(percent_encode("a b(c)/é"), "a%20b%28c%29%2F%C3%A9");
    }
}
