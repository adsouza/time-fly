//! Globe time: the age shown, keyboard snapshot stepping and the `TimeChanged` domain event.

use bevy::prelude::*;
use timefly_core::deep_time::{YearsBp, next_snapshot, prev_snapshot};

use crate::diagnostics::DiagnosticsContext;
use crate::diagnostics::aspects::{DiagnosticsAppExt, DomainEventLevel};

/// The age the globe shows. Defaults to the present.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct GlobeTime {
    pub years: YearsBp,
}

impl Default for GlobeTime {
    fn default() -> Self {
        GlobeTime {
            years: YearsBp(0.0),
        }
    }
}

/// What changed the globe time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeChangeCause {
    Key,
    Slider,
    Bench,
}

/// Domain event: the globe time changed.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct TimeChanged {
    pub from: YearsBp,
    pub to: YearsBp,
    pub cause: TimeChangeCause,
}

impl DomainEventLevel for TimeChanged {
    /// A key step logs at `info`. A slider drag sends one per frame: `debug` (the slider logs
    /// the drag's start and end at `info`). The scrub bench's scripted scrub sends one per frame
    /// for 40 s: `trace`, below even the flight recorder (its phases and result log at `info`).
    fn log_level(&self) -> bevy::log::Level {
        match self.cause {
            TimeChangeCause::Key => bevy::log::Level::INFO,
            TimeChangeCause::Slider => bevy::log::Level::DEBUG,
            TimeChangeCause::Bench => bevy::log::Level::TRACE,
        }
    }
}

/// `[` steps to the next older snapshot, `]` to the next younger one.
pub const OLDER_KEY: KeyCode = KeyCode::BracketLeft;
pub const YOUNGER_KEY: KeyCode = KeyCode::BracketRight;

pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GlobeTime>()
            .add_leveled_domain_event::<TimeChanged>()
            .add_systems(Update, step_time_on_keys)
            .add_systems(
                PostUpdate,
                publish_time_context.run_if(resource_changed::<GlobeTime>),
            );
    }
}

/// The age after a key step, or `None` when the step changes nothing (no key, both keys, or
/// already at the end of the snapshot list).
pub fn key_step(current: YearsBp, older: bool, younger: bool) -> Option<YearsBp> {
    let next = match (older, younger) {
        (true, false) => next_snapshot(current),
        (false, true) => prev_snapshot(current),
        _ => return None,
    };
    (next != current).then_some(next)
}

fn step_time_on_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut time: ResMut<GlobeTime>,
    mut changes: MessageWriter<TimeChanged>,
) {
    let Some(keys) = keys else { return };
    let from = time.years;
    if let Some(to) = key_step(
        from,
        keys.just_pressed(OLDER_KEY),
        keys.just_pressed(YOUNGER_KEY),
    ) {
        time.years = to;
        changes.write(TimeChanged {
            from,
            to,
            cause: TimeChangeCause::Key,
        });
    }
}

fn publish_time_context(time: Res<GlobeTime>, context: Option<ResMut<DiagnosticsContext>>) {
    if let Some(mut context) = context {
        context.game_age_years = Some(time.years.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_time_changes_log_below_info() {
        use crate::diagnostics::aspects::DomainEventLevel;
        use bevy::log::Level;
        let change = |cause| TimeChanged {
            from: YearsBp(0.0),
            to: YearsBp(100.0),
            cause,
        };
        // A key press is one step: info.
        assert_eq!(change(TimeChangeCause::Key).log_level(), Level::INFO);
        // A drag changes the time every frame: debug (the slider logs its start and end at
        // info). The scrub bench's scripted scrub (~2,400 frames) would evict the whole flight
        // recorder even at debug: trace (its phases and result block log at info).
        assert_eq!(change(TimeChangeCause::Slider).log_level(), Level::DEBUG);
        assert_eq!(change(TimeChangeCause::Bench).log_level(), Level::TRACE);
    }

    #[test]
    fn key_step_uses_snapshot_stepping() {
        assert_eq!(key_step(YearsBp(0.0), true, false), Some(YearsBp(100.0)));
        assert_eq!(key_step(YearsBp(150.0), false, true), Some(YearsBp(100.0)));
        assert_eq!(key_step(YearsBp(0.0), false, true), None);
        assert_eq!(key_step(YearsBp(540.0e6), true, false), None);
        assert_eq!(key_step(YearsBp(100.0), true, true), None);
        assert_eq!(key_step(YearsBp(100.0), false, false), None);
    }

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(TimePlugin);
        app.update();
        app
    }

    /// Presses `key` for one update and returns the `TimeChanged` events of that update.
    fn tap(app: &mut App, key: KeyCode) -> Vec<TimeChanged> {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        let mut input = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        input.release(key);
        input.clear();
        app.world()
            .resource::<Messages<TimeChanged>>()
            .iter_current_update_messages()
            .copied()
            .collect()
    }

    fn years(app: &App) -> f64 {
        app.world().resource::<GlobeTime>().years.0
    }

    #[test]
    fn keys_step_globe_time() {
        let mut app = app();
        assert_eq!(years(&app), 0.0);

        let events = tap(&mut app, OLDER_KEY);
        assert_eq!(years(&app), 100.0);
        assert_eq!(
            events,
            vec![TimeChanged {
                from: YearsBp(0.0),
                to: YearsBp(100.0),
                cause: TimeChangeCause::Key,
            }]
        );

        tap(&mut app, OLDER_KEY);
        assert_eq!(years(&app), 500.0);

        let events = tap(&mut app, YOUNGER_KEY);
        assert_eq!(years(&app), 100.0);
        assert_eq!(events[0].from, YearsBp(500.0));

        tap(&mut app, YOUNGER_KEY);
        assert_eq!(years(&app), 0.0);
        // At the present, `]` changes nothing and emits nothing.
        assert!(tap(&mut app, YOUNGER_KEY).is_empty());
        assert_eq!(years(&app), 0.0);
    }

    #[test]
    fn time_is_published_to_diagnostics() {
        let mut app = app();
        app.init_resource::<DiagnosticsContext>();
        tap(&mut app, OLDER_KEY);
        assert_eq!(
            app.world().resource::<DiagnosticsContext>().game_age_years,
            Some(100.0)
        );
    }
}
