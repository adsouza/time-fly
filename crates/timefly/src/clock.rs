//! The clock waits time out on: region loads, the globe's GPU preparation and a descent's start.
//!
//! Wall-clock time is the wrong measure for "has this had long enough?": a hidden tab, a laptop
//! asleep or a host that pauses `requestAnimationFrame` stops frames for tens of seconds, and
//! the first frame after would see every wait as timed out. The [`LoadClock`] counts running
//! time instead, so a long stall counts as one short frame.

use bevy::prelude::*;

/// The most one frame adds to the [`LoadClock`] (Bevy's `Time<Virtual>` convention).
pub const LOAD_CLOCK_MAX_DELTA_S: f64 = 0.25;

/// The [`LoadClock`] after a frame whose real delta was `real_delta_s`: at most
/// [`LOAD_CLOCK_MAX_DELTA_S`] more, never less.
pub fn advance_load_clock(elapsed_s: f64, real_delta_s: f64) -> f64 {
    elapsed_s + real_delta_s.clamp(0.0, LOAD_CLOCK_MAX_DELTA_S)
}

/// The sum of each frame's real delta clamped to [`LOAD_CLOCK_MAX_DELTA_S`]. The game never
/// pauses or scales it. Advanced in `PreUpdate`, so every `Update` system sees the same value.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct LoadClock {
    pub elapsed_s: f64,
}

/// Adds and advances the [`LoadClock`]. Each plugin that reads the clock adds this one through
/// [`add_load_clock`], so any of them works alone.
pub struct LoadClockPlugin;

impl Plugin for LoadClockPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoadClock>()
            .add_systems(PreUpdate, tick_load_clock);
    }
}

/// Adds the [`LoadClockPlugin`] unless an earlier plugin did.
pub fn add_load_clock(app: &mut App) {
    if !app.is_plugin_added::<LoadClockPlugin>() {
        app.add_plugins(LoadClockPlugin);
    }
}

fn tick_load_clock(real: Res<Time<Real>>, mut clock: ResMut<LoadClock>) {
    clock.elapsed_s = advance_load_clock(clock.elapsed_s, real.delta_secs_f64());
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::*;

    /// Adding it from two plugins is harmless, and it ticks once per frame.
    #[test]
    fn the_plugin_ticks_once_per_frame() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                100,
            )));
        add_load_clock(&mut app);
        add_load_clock(&mut app);
        app.update();
        let before = app.world().resource::<LoadClock>().elapsed_s;
        app.update();
        let after = app.world().resource::<LoadClock>().elapsed_s;
        assert!((after - before - 0.1).abs() < 1e-6, "{before} → {after}");
    }
}
