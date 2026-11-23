//! Regions in the shell (design spec §6.1): turning a [`RegionRequested`] into an
//! [`ActiveRegion`].
//!
//! A request runs through three stages, one job at a time (a new request replaces the job in
//! flight, which cancels it):
//!
//! 1. **Frames.** The globe frames the snapshot's coarse field reads (`frames_needed`), from the
//!    bounded [`FrameCache`] or loaded again as [`CpuGrid`]s; the tile manifest on the first
//!    request.
//! 2. **Tiles.** A probe field gives the tiles the inner field reads (`tiles_needed`, which
//!    follows plate rotations). Tiles not in the [`TileCache`] load through the asset server,
//!    checked against the manifest. The overview loads once per session, outside any job, so
//!    cancelling a request never cancels it. A missing or
//!    corrupt tile is a `warn` and makes the region fall back (`fallback == true`), never a
//!    failure.
//! 3. **Generation.** Native: the prep on an
//!    [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) task, then the rows
//!    across the pool. Web: time-boxed slices of [`WEB_GEN_BUDGET_MS`] per frame, or
//!    [`WEB_GEN_LARGE_BUDGET_MS`] while no region is on screen to keep smooth: a first entry
//!    from the globe, or a snapshot change held under the fade overlay ([`LargeGenBudget`],
//!    [`gen_budget`]).
//!
//! Then [`ActiveRegion`] is inserted, [`RegionGenerated`] and [`RegionEntered`] (or
//! [`RegionSnapshotChanged`], when it replaces a region at another snapshot) are sent, and the
//! state enters [`RegionState::InRegion`]. A [`RegionLeft`] removes the region (or cancels the
//! job) and returns to [`RegionState::Globe`]. A failure (a frame that will not load, an
//! invalid field) logs a `warn` and also returns to the globe.

pub mod frames;
pub mod generate;
mod job;
pub mod launch;
pub mod navigate;
pub mod tiles;

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::RenderDir;
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_RES, REGION_RES};

use crate::diagnostics::aspects::DiagnosticsAppExt;
use frames::{CpuGrid, CpuGridLoader, FrameCache};
pub use job::{LOAD_CLOCK_MAX_DELTA_S, LOAD_TIMEOUT_S, LoadClock, advance_load_clock};
use job::{
    TileIndex, advance_frames, advance_tiles, leave_region, poll_generation, poll_overview,
    poll_tile_manifest, start_region_request,
};
use tiles::{TileCache, TileManifest, TileManifestLoader};

/// The web's generation budget per frame (design spec §6.1).
pub const WEB_GEN_BUDGET_MS: f64 = 6.0;
/// The web's generation budget per frame while no region is on screen to keep smooth (a first
/// entry, generated while the globe stays in view, or a snapshot change held under the fade
/// overlay): the run takes most of a 60 Hz frame. At 6 ms a slice gets about a third of each
/// 16.7 ms frame (wall time ≈ 2.8 × CPU time); at 12 ms about two thirds (≈ 1.4 ×). The
/// Aegean's first entry took 628 slices of 6 ms in Chrome (≈ 10.5 s); at 12 ms about half as
/// many.
pub const WEB_GEN_LARGE_BUDGET_MS: f64 = 12.0;
/// The region camera's starting height above the ground at the region centre (design §6.1).
pub const CAMERA_START_ALTITUDE_M: f32 = 8_000.0;
/// The detail seed of every region: a region is a function of its centre and snapshot.
pub const REGION_SEED: u32 = 0x7F1E_0002;

/// Where the player is.
#[derive(States, Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum RegionState {
    #[default]
    Globe,
    /// A region is being prepared (frames, tiles, generation). An [`ActiveRegion`] being
    /// replaced stays until the new one is ready.
    Generating,
    /// [`ActiveRegion`] holds the current region.
    InRegion,
}

/// Request: generate the region centred on `era_dir` (an era-frame direction) at `snapshot`.
/// Accepted in any state; it replaces any request in flight.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionRequested {
    pub era_dir: RenderDir,
    pub snapshot: YearsBp,
}

/// Domain event: a region's grid is ready. `millis` runs from the request to the grid being
/// ready; `tiles_fetched` counts tiles loaded for it (cache hits excluded) and `bytes_fetched`
/// their file bytes (the session's one-time manifest and overview not included); `fallback`
/// says some tile it needs is missing, so part of the inner field uses globe-resolution data.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionGenerated {
    pub spec: RegionSpec,
    pub millis: u32,
    pub tiles_fetched: u32,
    pub bytes_fetched: u64,
    /// Globe frame files loaded for it (0 when the frame cache held them all).
    pub frames_fetched: u32,
    pub fallback: bool,
}

/// Domain event: the player entered a region from the globe (or from a region at the same
/// snapshot). Sent with [`RegionGenerated`], when [`ActiveRegion`] is inserted.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionEntered {
    pub spec: RegionSpec,
}

/// Leaving the region, written by whoever leaves (Task 9's `Backspace`, once the view is back on
/// the globe). The plugin then removes [`ActiveRegion`], cancels any job and enters
/// [`RegionState::Globe`]; the logger records it as a domain event.
#[derive(Message, Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct RegionLeft;

/// Domain event: a region replaced the active one at another snapshot (a snapshot change inside
/// a region, design spec §6.2).
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionSnapshotChanged {
    pub from: YearsBp,
    pub to: YearsBp,
    pub spec: RegionSpec,
}

/// The current region: its grid and its frame (local metres ↔ era directions).
#[derive(Resource, Clone, Debug)]
pub struct ActiveRegion {
    pub grid: Arc<RegionGrid>,
    pub frame: RegionFrame,
}

/// How rows are generated.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GenMode {
    /// Across the async compute pool (native only; the web uses `Budgeted` instead).
    Parallel,
    /// Time-boxed slices on the main thread, `budget` per frame.
    Budgeted { budget: Duration },
}

/// [`WEB_GEN_BUDGET_MS`] as a duration.
pub fn web_budget() -> Duration {
    Duration::from_secs_f64(WEB_GEN_BUDGET_MS / 1000.0)
}

/// The slice budget for a time-boxed run started with `base`: at least
/// [`WEB_GEN_LARGE_BUDGET_MS`] when it may take the large budget, `base` otherwise.
pub fn gen_budget(base: Duration, large: bool) -> Duration {
    if large {
        base.max(Duration::from_secs_f64(WEB_GEN_LARGE_BUDGET_MS / 1000.0))
    } else {
        base
    }
}

/// Whether a time-boxed generation may take the large budget ([`gen_budget`]): no region is on
/// screen to keep smooth. Written by region navigation each frame
/// (`NavPhase::allows_large_gen_budget`); `false` without it.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LargeGenBudget(pub bool);

impl GenMode {
    /// The web's time box.
    pub fn web() -> GenMode {
        GenMode::Budgeted {
            budget: web_budget(),
        }
    }
}

impl Default for GenMode {
    fn default() -> Self {
        if cfg!(target_arch = "wasm32") {
            GenMode::web()
        } else {
            GenMode::Parallel
        }
    }
}

/// Region generation settings. The resolutions default to the standard ones; tests lower them.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct RegionSettings {
    pub inner_res: u32,
    pub apron_res: u32,
    pub mode: GenMode,
}

impl Default for RegionSettings {
    fn default() -> Self {
        RegionSettings {
            inner_res: REGION_RES,
            apron_res: APRON_RES,
            mode: GenMode::default(),
        }
    }
}

/// The region job's systems (requests, loading, generation). They insert [`ActiveRegion`] and
/// send the region events; region navigation orders itself after them, so it sees a new region
/// in the same frame it is inserted.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionSystems;

pub struct RegionPlugin;

impl Plugin for RegionPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CpuGrid>()
            .init_asset_loader::<CpuGridLoader>()
            .init_asset::<TileManifest>()
            .init_asset_loader::<TileManifestLoader>()
            .init_resource::<TileCache>()
            .init_resource::<FrameCache>()
            .init_resource::<TileIndex>()
            .init_resource::<RegionSettings>()
            .init_resource::<LargeGenBudget>()
            .init_resource::<LoadClock>()
            .add_logged_state::<RegionState>()
            .add_domain_event::<RegionRequested>()
            .add_domain_event::<RegionGenerated>()
            .add_domain_event::<RegionEntered>()
            .add_domain_event::<RegionLeft>()
            .add_domain_event::<RegionSnapshotChanged>()
            .add_systems(
                Update,
                (
                    job::tick_load_clock,
                    leave_region,
                    start_region_request,
                    poll_tile_manifest,
                    poll_overview,
                    advance_frames,
                    advance_tiles,
                    poll_generation,
                )
                    .chain()
                    .in_set(RegionSystems),
            );
        launch::add_launch_systems(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_large_budget_doubles_the_web_budget() {
        let ms = |v: f64| Duration::from_secs_f64(v / 1000.0);
        assert_eq!(gen_budget(web_budget(), false), ms(WEB_GEN_BUDGET_MS));
        assert_eq!(gen_budget(web_budget(), true), ms(WEB_GEN_LARGE_BUDGET_MS));
        assert_eq!(WEB_GEN_LARGE_BUDGET_MS, 2.0 * WEB_GEN_BUDGET_MS);
        // A larger base (a test's) is never shrunk.
        assert_eq!(gen_budget(ms(20.0), true), ms(20.0));
        assert_eq!(gen_budget(ms(0.5), false), ms(0.5));
    }
}
