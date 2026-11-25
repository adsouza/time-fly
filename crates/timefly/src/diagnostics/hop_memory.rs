//! The hop-memory trace's product side (M3), off by default: with
//! `TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace`, a native region generation logs
//! a `generation` line at each step from its worker thread, with the bytes the step holds and
//! the process's memory split ([`trace_generation`]). The bench's own sampler
//! (`crate::bench::journey::trace`) logs the frames under the same [`TARGET`]. With the target
//! off nothing is counted or sampled.

use tracing::{Level, trace};

use super::memory;

/// The trace's target.
pub const TARGET: &str = "timefly::hop_memory";

/// Whether the trace is on.
pub fn enabled() -> bool {
    tracing::enabled!(target: TARGET, Level::TRACE)
}

/// One step of a region's generation, from the thread that runs it, with the bytes it holds:
/// `bytes` runs only while the trace is on.
pub fn trace_generation(step: &'static str, bytes: impl FnOnce() -> usize) {
    if enabled() {
        trace!(target: TARGET, step, bytes = bytes(), memory = ?memory::split(), "generation");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// The final review: product code (`crate::region::generate`) calls this, not the bench, and
    /// the bytes a step holds are counted only while the trace is on. No subscriber enables
    /// [`TARGET`] in the unit tests, so the trace is off.
    #[test]
    fn nothing_is_counted_while_the_trace_is_off() {
        assert!(!enabled());
        let counted = Cell::new(false);
        trace_generation("a step", || {
            counted.set(true);
            1
        });
        assert!(!counted.get(), "the byte count ran with the trace off");
    }
}
