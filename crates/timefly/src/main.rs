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
    let exit = timefly::app(&session).run();
    session.finish(&exit);
    exit
}
