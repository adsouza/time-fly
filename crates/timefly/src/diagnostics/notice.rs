//! The crash notice (spec §20): shown once when the previous session didn't close normally.
//!
//! - Native, crash before the first frame: a system dialog before the `App` starts.
//! - Native, otherwise (or if the dialog can't open): an in-game overlay after the first frame.
//! - Web: an HTML overlay before the `App` starts, so it doesn't depend on WebGPU.
//!
//! Any button marks the previous session acknowledged.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

use bevy::prelude::*;

use std::sync::Arc;

use super::report::{redact, report_url};
use super::session::{PreviousOutcome, PreviousSession};
use super::{DiagnosticsHandle, Shared};

/// The notice text.
pub fn notice_text(log_location: &str) -> String {
    format!(
        "Time Fly didn't close normally last time. A log of that session is saved at {log_location}. \
         Please send it to the developer so the problem can be fixed. It contains game events and \
         your OS and graphics card, and no personal information."
    )
}

/// Where the previous session's log is, as shown to the player (home directory as `~`).
pub(crate) fn log_display(previous: &PreviousSession) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        super::session::crash_log_path(&previous.dir, previous.has_crash_report)
            .display()
            .to_string()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = previous;
        "your browser's storage for this site (Download log saves it as a file)".to_string()
    }
}

/// How the crash notice was shown, for the session log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeRoute {
    /// The previous session didn't crash (or its crash was acknowledged).
    None,
    /// The system dialog, answered before the `App` started.
    Dialog,
    /// Skipped the dialog (a probe is running, or it couldn't open); shown in game instead.
    InGame,
    /// The web HTML overlay.
    WebOverlay,
}

/// A notice still to show in game.
#[derive(Clone, Debug)]
pub struct PendingNotice {
    pub session_id: String,
    pub platform_summary: String,
    /// The text, with the log location redacted.
    pub text: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub previous_dir: PathBuf,
    #[cfg(not(target_arch = "wasm32"))]
    pub log_path: PathBuf,
}

impl PendingNotice {
    fn new(shared: &Shared, previous: &PreviousSession) -> Self {
        let record = previous.record.as_ref();
        Self {
            session_id: record.map_or_else(|| "unknown".to_string(), |r| r.id.clone()),
            platform_summary: record.map_or_else(String::new, |r| r.platform.summary()),
            text: notice_text(&redact(&log_display(previous), &shared.home)),
            #[cfg(not(target_arch = "wasm32"))]
            previous_dir: previous.dir.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            log_path: super::session::crash_log_path(&previous.dir, previous.has_crash_report),
        }
    }

    pub fn report_url(&self) -> String {
        report_url(&self.session_id, &self.platform_summary)
    }

    /// Marks the previous session's crash as answered. Problems go to [`Shared::problem`],
    /// which works before the log subscriber exists (the system dialog runs then).
    pub(crate) fn acknowledge(&self, shared: &Shared) {
        #[cfg(not(target_arch = "wasm32"))]
        let result =
            super::session::store::acknowledge(&self.previous_dir).map_err(|e| e.to_string());
        #[cfg(target_arch = "wasm32")]
        let result = super::session::web::acknowledge(&self.session_id);
        if let Err(e) = result {
            shared.problem(format!(
                "could not mark the previous session acknowledged: {e}"
            ));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn show_log(&self, shared: &Shared) {
        if let Err(e) = opener::reveal(&self.log_path) {
            shared.problem(format!("could not show the log: {e}"));
        }
    }
}

/// Runs in `diagnostics::start`, before the `App` exists. Prints the notice to stderr, shows
/// the system dialog (native, crash before the first frame) or the HTML overlay (web), and
/// returns how it was shown plus the notice still to show in game, if any.
pub(crate) fn before_app(shared: &Arc<Shared>) -> (NoticeRoute, Option<PendingNotice>) {
    let Some(previous) = shared.findings.previous.as_ref() else {
        return (NoticeRoute::None, None);
    };
    let PreviousOutcome::Crashed { during_startup, .. } = previous.outcome else {
        return (NoticeRoute::None, None);
    };
    let pending = PendingNotice::new(shared, previous);
    shared.eprint(&pending.text);

    #[cfg(not(target_arch = "wasm32"))]
    {
        // Probes are for tests and manual checks; a blocking dialog would hang them.
        if during_startup
            && shared.args.diagnostics_probe.is_none()
            && show_dialog(shared, &pending)
        {
            return (NoticeRoute::Dialog, None);
        }
        (NoticeRoute::InGame, Some(pending))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = during_startup;
        web::show_overlay(shared, pending);
        (NoticeRoute::WebOverlay, None)
    }
}

/// The system dialog. Returns whether the player answered it; `false` means it couldn't open
/// and the in-game notice should be shown instead.
#[cfg(not(target_arch = "wasm32"))]
fn show_dialog(shared: &Shared, pending: &PendingNotice) -> bool {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
    const SHOW_LOG: &str = "Show log";
    const NOT_NOW: &str = "Not now";
    let result = MessageDialog::new()
        .set_level(MessageLevel::Warning)
        .set_title("Time Fly")
        .set_description(&pending.text)
        .set_buttons(MessageButtons::OkCancelCustom(
            SHOW_LOG.into(),
            NOT_NOW.into(),
        ))
        .show();
    match result {
        MessageDialogResult::Custom(label) if label == SHOW_LOG => {
            pending.show_log(shared);
            pending.acknowledge(shared);
            true
        }
        // On Linux without a portal the dialog can fail and report `Cancel`; fall through to
        // the in-game notice there.
        MessageDialogResult::Cancel if cfg!(target_os = "linux") => false,
        _ => {
            pending.acknowledge(shared);
            true
        }
    }
}

/// The in-game overlay: resources and systems.
#[derive(Resource)]
struct InGameNotice(PendingNotice);

#[derive(Component)]
struct NoticeRoot;

#[derive(Clone, Copy, Debug)]
enum NoticeButton {
    #[cfg(not(target_arch = "wasm32"))]
    ShowLog,
    Report,
    Dismiss,
}

impl NoticeButton {
    fn label(self) -> &'static str {
        match self {
            #[cfg(not(target_arch = "wasm32"))]
            Self::ShowLog => "Show log",
            Self::Report => "Report problem",
            Self::Dismiss => "Dismiss",
        }
    }
}

pub(crate) fn add_in_game_notice(app: &mut App, pending: PendingNotice) {
    app.insert_resource(InGameNotice(pending))
        .add_systems(Update, spawn_notice.run_if(resource_exists::<InGameNotice>));
}

const PANEL: Color = Color::srgb(0.12, 0.12, 0.14);
const BUTTON: Color = Color::srgb(0.24, 0.24, 0.28);
const TEXT: Color = Color::srgb(0.92, 0.92, 0.92);

/// Spawns the overlay after the first frame, with its own camera on top of the scene.
fn spawn_notice(
    mut commands: Commands,
    notice: Res<InGameNotice>,
    frames: Option<Res<bevy::diagnostic::FrameCount>>,
    mut spawned: Local<bool>,
) {
    if *spawned || frames.is_none_or(|f| f.0 < 2) {
        return;
    }
    *spawned = true;
    let camera = commands
        .spawn((
            NoticeRoot,
            Camera2d,
            Camera {
                order: 1000,
                clear_color: ClearColorConfig::None,
                ..default()
            },
        ))
        .id();
    let buttons = [
        #[cfg(not(target_arch = "wasm32"))]
        NoticeButton::ShowLog,
        NoticeButton::Report,
        NoticeButton::Dismiss,
    ];
    commands
        .spawn((
            NoticeRoot,
            UiTargetCamera(camera),
            GlobalZIndex(1000),
            Node {
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    max_width: px(560),
                    padding: UiRect::all(px(20)),
                    row_gap: px(16),
                    flex_direction: FlexDirection::Column,
                    border_radius: BorderRadius::all(px(8)),
                    ..default()
                },
                BackgroundColor(PANEL),
            ))
            .with_children(|panel| {
                panel.spawn((Text::new(notice.0.text.clone()), TextColor(TEXT)));
                panel
                    .spawn(Node {
                        column_gap: px(10),
                        justify_content: JustifyContent::End,
                        ..default()
                    })
                    .with_children(|row| {
                        for kind in buttons {
                            row.spawn((
                                bevy::ui_widgets::Button,
                                Node {
                                    padding: UiRect::axes(px(14), px(8)),
                                    border_radius: BorderRadius::all(px(6)),
                                    ..default()
                                },
                                BackgroundColor(BUTTON),
                                bevy::ui_widgets::observe(
                                    move |_: On<bevy::ui_widgets::Activate>,
                                          notice: Res<InGameNotice>,
                                          handle: Res<DiagnosticsHandle>,
                                          roots: Query<Entity, With<NoticeRoot>>,
                                          mut commands: Commands| {
                                        on_button(kind, &notice.0, &handle.0, &roots, &mut commands);
                                    },
                                ),
                                children![(Text::new(kind.label()), TextColor(TEXT))],
                            ));
                        }
                    });
            });
        });
    info!("showing the crash notice");
}

fn on_button(
    kind: NoticeButton,
    notice: &PendingNotice,
    shared: &Shared,
    roots: &Query<Entity, With<NoticeRoot>>,
    commands: &mut Commands,
) {
    info!(button = ?kind, "crash notice button");
    notice.acknowledge(shared);
    match kind {
        #[cfg(not(target_arch = "wasm32"))]
        NoticeButton::ShowLog => notice.show_log(shared),
        NoticeButton::Report => open_report_page(shared, &notice.report_url()),
        NoticeButton::Dismiss => {
            for entity in roots {
                commands.entity(entity).despawn();
            }
            commands.remove_resource::<InGameNotice>();
        }
    }
}

fn open_report_page(shared: &Shared, url: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    let result = opener::open(url).map_err(|e| e.to_string());
    #[cfg(target_arch = "wasm32")]
    let result = web_sys::window()
        .ok_or_else(|| "no window".to_string())
        .and_then(|w| {
            w.open_with_url_and_target(url, "_blank")
                .map_err(|_| "blocked".to_string())
        })
        .map(|_| ());
    if let Err(e) = result {
        shared.problem(format!("could not open the report page: {e}"));
    }
}

/// The HTML overlay, built with `web-sys`.
#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use web_sys::{Document, Element, HtmlElement};

    use std::sync::Arc;

    use super::super::Shared;
    use super::super::session::web::{crash_key, get, log_key};
    use super::{PendingNotice, open_report_page};

    const OVERLAY_STYLE: &str = "position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;\
        justify-content:center;background:rgba(0,0,0,0.55);font:15px/1.45 system-ui,sans-serif";
    const PANEL_STYLE: &str = "max-width:560px;margin:16px;padding:20px;border-radius:8px;background:#1f1f24;color:#ebebeb";
    const ROW_STYLE: &str =
        "display:flex;gap:10px;justify-content:flex-end;margin-top:16px;flex-wrap:wrap";
    const BUTTON_STYLE: &str = "padding:8px 14px;border:0;border-radius:6px;background:#3d3d47;color:#ebebeb;font:inherit;cursor:pointer";

    fn element(document: &Document, tag: &str, style: &str) -> Option<Element> {
        let element = document.create_element(tag).ok()?;
        element.set_attribute("style", style).ok()?;
        Some(element)
    }

    /// The previous session's log (and crash report), for **Download log**.
    fn log_text(id: &str) -> String {
        let mut text = String::new();
        if let Some(report) = get(&crash_key(id)) {
            text.push_str(&report);
            text.push_str("\n\n--- session log ---\n");
        }
        text.push_str(&get(&log_key(id)).unwrap_or_else(|| "(no log saved)".to_string()));
        text
    }

    fn download(document: &Document, filename: &str, text: &str) -> Option<()> {
        let parts = js_sys::Array::of1(&text.into());
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("text/plain");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        let anchor: web_sys::HtmlAnchorElement =
            document.create_element("a").ok()?.dyn_into().ok()?;
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.click();
        Some(())
    }

    fn button(
        document: &Document,
        label: &str,
        on_click: impl FnMut() + 'static,
    ) -> Option<Element> {
        let button = element(document, "button", BUTTON_STYLE)?;
        button.set_text_content(Some(label));
        let callback = Closure::<dyn FnMut()>::new(on_click);
        button
            .dyn_ref::<HtmlElement>()?
            .set_onclick(Some(callback.as_ref().unchecked_ref()));
        callback.forget();
        Some(button)
    }

    /// Runs before the log subscriber exists, so a failure goes to [`Shared::problem`].
    pub(super) fn show_overlay(shared: &Arc<Shared>, pending: PendingNotice) {
        if build(shared, pending).is_none() {
            shared.problem("could not show the crash notice overlay".to_string());
        }
    }

    fn build(shared: &Arc<Shared>, pending: PendingNotice) -> Option<()> {
        let document = web_sys::window()?.document()?;
        let overlay = element(&document, "div", OVERLAY_STYLE)?;
        let panel = element(&document, "div", PANEL_STYLE)?;
        let text = element(&document, "p", "margin:0")?;
        text.set_text_content(Some(&pending.text));
        let row = element(&document, "div", ROW_STYLE)?;

        let (p, sh) = (pending.clone(), shared.clone());
        let doc = document.clone();
        let download_log = button(&document, "Download log", move || {
            p.acknowledge(&sh);
            let name = format!("timefly-{}.log", p.session_id);
            if download(&doc, &name, &log_text(&p.session_id)).is_none() {
                sh.problem("could not download the log".to_string());
            }
        })?;
        let (p, sh) = (pending.clone(), shared.clone());
        let report = button(&document, "Report problem", move || {
            p.acknowledge(&sh);
            open_report_page(&sh, &p.report_url());
        })?;
        let (p, sh) = (pending, shared.clone());
        let o = overlay.clone();
        let dismiss = button(&document, "Dismiss", move || {
            p.acknowledge(&sh);
            o.remove();
        })?;
        for b in [&download_log, &report, &dismiss] {
            row.append_child(b).ok()?;
        }

        panel.append_child(&text).ok()?;
        panel.append_child(&row).ok()?;
        overlay.append_child(&panel).ok()?;
        document.body()?.append_child(&overlay).ok()?;
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notice_text_is_the_spec_text() {
        assert_eq!(
            notice_text("~/Library/Logs/Time Fly/x/crash.txt"),
            "Time Fly didn't close normally last time. A log of that session is saved at \
             ~/Library/Logs/Time Fly/x/crash.txt. Please send it to the developer so the problem can \
             be fixed. It contains game events and your OS and graphics card, and no personal information."
        );
    }
}
