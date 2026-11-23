//! Pre-flight GPU check, before the Bevy `App` exists.
//!
//! Bevy panics while initialising the renderer when no graphics adapter exists, which leaves a
//! blank page (web) or a crash record that makes every later launch show the crash notice. So
//! `main` asks first: on native with the same wgpu backends and environment variables Bevy uses
//! ([`WgpuSettings`](bevy::render::settings::WgpuSettings): `WGPU_BACKEND`, `WGPU_POWER_PREF`,
//! `WGPU_FORCE_FALLBACK_ADAPTER`, ...); on the web through `navigator.gpu.requestAdapter()`.
//! Without an adapter, [`stop_without_gpu`] says so (a dialog on native, an HTML message on the
//! web), logs it and records the session as clean instead of starting Bevy.
//!
//! The native check asks for an adapter without a window surface, so an adapter that exists
//! but can't present to the window still reaches Bevy (rare; it then fails as before).

use bevy::prelude::*;

use crate::diagnostics::{self, ProbeMode, Session};

/// What the pre-flight check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuCheck {
    /// An adapter exists: start the game.
    Ready { adapter: String },
    /// No graphics API at all (web: no `navigator.gpu`).
    NoApi,
    /// The API exists but offers no adapter.
    NoAdapter { detail: String },
}

impl GpuCheck {
    pub fn can_start(&self) -> bool {
        matches!(self, GpuCheck::Ready { .. })
    }
}

/// What the browser answered to `navigator.gpu.requestAdapter()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserAnswer {
    /// `navigator.gpu` (or its `requestAdapter`) is missing.
    NoGpuObject,
    /// The promise resolved to `null`: WebGPU exists but offers no adapter here.
    NullAdapter,
    /// The call threw or the promise rejected.
    RequestFailed(String),
    Adapter,
}

/// Decides the check from the browser's answer.
pub fn browser_check(answer: BrowserAnswer) -> GpuCheck {
    match answer {
        BrowserAnswer::NoGpuObject => GpuCheck::NoApi,
        BrowserAnswer::NullAdapter => GpuCheck::NoAdapter {
            detail: "requestAdapter() returned null".to_string(),
        },
        BrowserAnswer::RequestFailed(e) => GpuCheck::NoAdapter {
            detail: format!("requestAdapter() failed: {e}"),
        },
        BrowserAnswer::Adapter => GpuCheck::Ready {
            adapter: "WebGPU".to_string(),
        },
    }
}

/// The HTML message shown instead of the game.
pub fn web_message(check: &GpuCheck) -> String {
    let why = match check {
        GpuCheck::NoAdapter { .. } => {
            "This browser has WebGPU but offered no graphics adapter; WebGPU may be blocked for \
             this graphics card or driver."
        }
        GpuCheck::NoApi | GpuCheck::Ready { .. } => {
            "This browser doesn't offer WebGPU, or it is turned off."
        }
    };
    format!("Time Fly needs a browser with WebGPU (e.g. a recent Chrome or Edge). {why}")
}

/// The native dialog text; `log` is the session log's location, home directory as `~`.
pub fn native_message(log: &str) -> String {
    format!(
        "Time Fly can't start. No compatible GPU/driver found (Vulkan/Metal). Install or update \
         the graphics driver, then try again.\n\nThe log is at {log}."
    )
}

/// The dialog blocks until answered, so probes (tests and manual checks) skip it.
pub fn shows_dialog(probe: Option<ProbeMode>) -> bool {
    probe.is_none()
}

/// The exit code: native exits 1; the web has no exit code (the page stays open).
pub fn stop_exit(web: bool) -> AppExit {
    if web {
        AppExit::Success
    } else {
        AppExit::from_code(1)
    }
}

/// Native: asks wgpu for an adapter the way Bevy's renderer will.
#[cfg(not(target_arch = "wasm32"))]
pub fn probe_native() -> GpuCheck {
    use bevy::render::settings::WgpuSettings;
    let settings = WgpuSettings::default();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: settings.backends.unwrap_or(wgpu::Backends::all()),
        flags: settings.instance_flags,
        memory_budget_thresholds: settings.instance_memory_budget_thresholds,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    // As Bevy reads it in `initialize_renderer`.
    let force_fallback_adapter = std::env::var("WGPU_FORCE_FALLBACK_ADAPTER")
        .map_or(settings.force_fallback_adapter, |v| {
            !(v.is_empty() || v == "0" || v == "false")
        });
    let request = instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: settings.power_preference,
        compatible_surface: None,
        force_fallback_adapter,
        apply_limit_buckets: false,
    });
    match bevy::tasks::block_on(request) {
        Ok(adapter) => {
            let info = adapter.get_info();
            GpuCheck::Ready {
                adapter: format!("{} ({:?})", info.name, info.backend),
            }
        }
        Err(e) => GpuCheck::NoAdapter {
            detail: format!("{e} (backends {:?})", settings.backends),
        },
    }
}

/// Web: asks `navigator.gpu.requestAdapter()` with Bevy's power preference.
#[cfg(target_arch = "wasm32")]
pub async fn probe_browser() -> GpuCheck {
    use js_sys::{Function, Object, Promise, Reflect};
    use wasm_bindgen::{JsCast, JsValue};

    async fn ask() -> BrowserAnswer {
        let Some(navigator) = web_sys::window().map(|w| w.navigator()) else {
            return BrowserAnswer::NoGpuObject;
        };
        let gpu = Reflect::get(&navigator, &"gpu".into()).unwrap_or(JsValue::UNDEFINED);
        if gpu.is_undefined() || gpu.is_null() {
            return BrowserAnswer::NoGpuObject;
        }
        let Some(request) = Reflect::get(&gpu, &"requestAdapter".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
        else {
            return BrowserAnswer::NoGpuObject;
        };
        let options = Object::new();
        let _ = Reflect::set(
            &options,
            &"powerPreference".into(),
            &"high-performance".into(),
        );
        let promise = match request.call1(&gpu, &options) {
            Ok(value) => value,
            Err(e) => return BrowserAnswer::RequestFailed(format!("{e:?}")),
        };
        let Ok(promise) = promise.dyn_into::<Promise>() else {
            return BrowserAnswer::RequestFailed("did not return a promise".to_string());
        };
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(adapter) if adapter.is_null() || adapter.is_undefined() => {
                BrowserAnswer::NullAdapter
            }
            Ok(_) => BrowserAnswer::Adapter,
            Err(e) => BrowserAnswer::RequestFailed(format!("{e:?}")),
        }
    }
    browser_check(ask().await)
}

/// Runs instead of the game when [`GpuCheck::can_start`] is false: logs why through the real
/// log layers, tells the player (native: stderr and a dialog; web: an HTML message), records
/// the session as clean and returns the exit (native: 1).
pub fn stop_without_gpu(session: Session, check: &GpuCheck) -> AppExit {
    let web = cfg!(target_arch = "wasm32");
    let text = if web {
        web_message(check)
    } else {
        native_message(&session.log_location())
    };
    let detail = match check {
        GpuCheck::NoAdapter { detail } => detail.as_str(),
        GpuCheck::NoApi => "no navigator.gpu",
        GpuCheck::Ready { .. } => "",
    };
    diagnostics::log_error_without_app(&session, &format!("{text} [{detail}]"));
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.eprint(&text);
        if shows_dialog(session.probe()) {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Time Fly")
                .set_description(&text)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();
        }
    }
    #[cfg(target_arch = "wasm32")]
    diagnostics::show_web_message(&session, &text);
    let exit = stop_exit(web);
    session.finish_clean(&exit);
    exit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::ProbeMode;

    #[test]
    fn only_an_adapter_starts_the_game() {
        assert!(
            GpuCheck::Ready {
                adapter: "Apple M4 (Metal)".into()
            }
            .can_start()
        );
        assert!(!GpuCheck::NoApi.can_start());
        assert!(
            !GpuCheck::NoAdapter {
                detail: "none".into()
            }
            .can_start()
        );
    }

    #[test]
    fn the_browser_answer_decides_the_check() {
        assert_eq!(browser_check(BrowserAnswer::NoGpuObject), GpuCheck::NoApi);
        assert!(matches!(
            browser_check(BrowserAnswer::NullAdapter),
            GpuCheck::NoAdapter { .. }
        ));
        assert_eq!(
            browser_check(BrowserAnswer::RequestFailed("OperationError".into())),
            GpuCheck::NoAdapter {
                detail: "requestAdapter() failed: OperationError".into()
            }
        );
        assert!(browser_check(BrowserAnswer::Adapter).can_start());
    }

    #[test]
    fn the_web_message_asks_for_a_webgpu_browser() {
        for check in [
            GpuCheck::NoApi,
            GpuCheck::NoAdapter {
                detail: "null".into(),
            },
        ] {
            let text = web_message(&check);
            assert!(
                text.starts_with(
                    "Time Fly needs a browser with WebGPU (e.g. a recent Chrome or Edge)."
                ),
                "{text}"
            );
        }
        assert!(web_message(&GpuCheck::NoApi).contains("doesn't offer WebGPU"));
        assert!(
            web_message(&GpuCheck::NoAdapter {
                detail: "null".into()
            })
            .contains("no graphics adapter")
        );
    }

    #[test]
    fn the_native_message_names_the_drivers_and_the_log() {
        let text = native_message("~/Library/Logs/Time Fly/x/session.log");
        assert!(
            text.contains("No compatible GPU/driver found (Vulkan/Metal)"),
            "{text}"
        );
        assert!(
            text.contains("The log is at ~/Library/Logs/Time Fly/x/session.log."),
            "{text}"
        );
    }

    #[test]
    fn probes_never_block_on_the_dialog() {
        assert!(shows_dialog(None));
        assert!(!shows_dialog(Some(ProbeMode::PanicAfterFrame)));
    }

    #[test]
    fn web_has_no_exit_code_and_native_exits_1() {
        assert_eq!(stop_exit(true), AppExit::Success);
        assert_eq!(stop_exit(false), AppExit::from_code(1));
    }
}
