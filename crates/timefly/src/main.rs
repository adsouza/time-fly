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

fn main() -> AppExit {
    // Diagnostics start before the App exists, so a crash during startup is still recorded.
    let session = diagnostics::start(launch_args());
    if let Some(mode) = session.probe().filter(|m| m.is_headless()) {
        return diagnostics::run_headless_probe(session, mode);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(check) = session.args().check_data.clone() {
        return timefly::check_data::run_data_check(session, &check);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Bevy panics without an adapter; say so instead (after the window-less modes above).
        let gpu = timefly::gpu::probe_native();
        if !gpu.can_start() {
            return timefly::gpu::stop_without_gpu(session, &gpu);
        }
        let exit = timefly::app(&session).run();
        session.finish(&exit);
        exit
    }
    #[cfg(target_arch = "wasm32")]
    {
        // `requestAdapter()` is asynchronous; the app starts (and `run()` returns at once, the
        // browser driving its frames) once the browser has answered.
        wasm_bindgen_futures::spawn_local(async move {
            let gpu = timefly::gpu::probe_browser().await;
            if gpu.can_start() {
                let exit = timefly::app(&session).run();
                session.finish(&exit);
            } else {
                timefly::gpu::stop_without_gpu(session, &gpu);
            }
        });
        AppExit::Success
    }
}
