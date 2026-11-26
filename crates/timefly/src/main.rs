use bevy::app::AppExit;
use timefly::args::LaunchArgs;
use timefly::diagnostics;

fn launch_args() -> LaunchArgs {
    #[cfg(not(target_arch = "wasm32"))]
    {
        timefly::args::parse_launch_args(std::env::args().skip(1))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let query = web_sys::window()
            .and_then(|w| w.location().search().ok())
            .unwrap_or_default();
        timefly::args::parse_query_args(&query)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> AppExit {
    // Diagnostics start before the App exists, so a crash during startup is still recorded.
    let session = diagnostics::start(launch_args());
    if let Some(mode) = session.probe().filter(|m| m.is_headless()) {
        return diagnostics::run_headless_probe(session, mode);
    }
    if let Some(check) = session.args().check_data.clone() {
        return timefly::check_data::run_data_check(session, &check);
    }
    if let Some(check) = session.args().hydro_check.clone() {
        return timefly::hydro_check::run_hydro_check(session, &check);
    }
    let gpu = timefly::gpu::probe_native();
    if !gpu.can_start() {
        return timefly::gpu::stop_without_gpu(session, &gpu);
    }
    let exit = timefly::app(&session).run();
    session.finish(&exit);
    exit
}

#[cfg(target_arch = "wasm32")]
fn main() -> AppExit {
    #[wasm_bindgen::prelude::wasm_bindgen(module = "/src/favicon.js")]
    extern "C" {
        #[wasm_bindgen::prelude::wasm_bindgen(js_name = setFavicon)]
        fn set_favicon(svg: &str);
    }
    set_favicon(include_str!(
        "../../../packaging/linux/io.github.adsouza.TimeFly.svg"
    ));
    diagnostics::start_web(launch_args(), |session| {
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(mode) = session.probe().filter(|m| m.is_headless()) {
                diagnostics::run_headless_probe(session, mode);
                return;
            }
            let gpu = timefly::gpu::probe_browser().await;
            if gpu.can_start() {
                let exit = timefly::app(&session).run();
                session.finish(&exit);
            } else {
                timefly::gpu::stop_without_gpu(session, &gpu);
            }
        });
    });
    AppExit::Success
}
