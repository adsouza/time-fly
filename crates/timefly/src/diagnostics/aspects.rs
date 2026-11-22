//! Tracing as an aspect (spec §20): registering a domain event or a state also registers its
//! logger, so no call site logs them by hand. Later tasks register events and states only
//! through [`DiagnosticsAppExt`]. Events sent every frame during a continuous interaction pick
//! a quieter level through [`DomainEventLevel`].

use std::any::type_name;
use std::fmt::Debug;

use bevy::ecs::message::Message;
use bevy::log::Level;
use bevy::prelude::*;
use bevy::state::state::FreelyMutableState;

/// How loudly a domain event logs. Most events are rare and log at `info`; an event a
/// continuous interaction sends every frame (a slider drag, a scripted scrub) logs below it, so
/// it doesn't flood the session log or the flight recorder (4,096 events).
pub trait DomainEventLevel {
    fn log_level(&self) -> Level;
}

pub trait DiagnosticsAppExt {
    /// Registers message `E` and a system that logs each one at `info`.
    fn add_domain_event<E: Message + Debug>(&mut self) -> &mut Self;

    /// Registers message `E` and a system that logs each one at its [`DomainEventLevel`].
    fn add_leveled_domain_event<E: Message + Debug + DomainEventLevel>(&mut self) -> &mut Self;

    /// Initializes state `S` and logs every transition at `info`.
    fn add_logged_state<S: FreelyMutableState + FromWorld>(&mut self) -> &mut Self;
}

impl DiagnosticsAppExt for App {
    fn add_domain_event<E: Message + Debug>(&mut self) -> &mut Self {
        self.add_message::<E>()
            .add_systems(Last, log_domain_events::<E>)
    }

    fn add_leveled_domain_event<E: Message + Debug + DomainEventLevel>(&mut self) -> &mut Self {
        self.add_message::<E>()
            .add_systems(Last, log_leveled_domain_events::<E>)
    }

    fn add_logged_state<S: FreelyMutableState + FromWorld>(&mut self) -> &mut Self {
        self.init_state::<S>()
            .add_systems(Last, log_state_transitions::<S>)
    }
}

fn log_domain_events<E: Message + Debug>(mut events: MessageReader<E>) {
    for event in events.read() {
        info!(event = type_name::<E>(), "{event:?}");
    }
}

fn log_leveled_domain_events<E: Message + Debug + DomainEventLevel>(mut events: MessageReader<E>) {
    for event in events.read() {
        let event_type = type_name::<E>();
        // The tracing macros need the level at compile time.
        match event.log_level() {
            Level::ERROR => error!(event = event_type, "{event:?}"),
            Level::WARN => warn!(event = event_type, "{event:?}"),
            Level::INFO => info!(event = event_type, "{event:?}"),
            Level::DEBUG => debug!(event = event_type, "{event:?}"),
            _ => trace!(event = event_type, "{event:?}"),
        }
    }
}

fn log_state_transitions<S: States>(mut transitions: MessageReader<StateTransitionEvent<S>>) {
    for transition in transitions.read() {
        info!(
            state = type_name::<S>(),
            exited = ?transition.exited,
            entered = ?transition.entered,
            "state transition"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bevy::ecs::schedule::SingleThreadedExecutor;
    use bevy::log::tracing::{self, Subscriber};
    use bevy::log::tracing_subscriber::layer::{Context, SubscriberExt};
    use bevy::log::tracing_subscriber::{Layer, Registry};
    use bevy::state::app::StatesPlugin;

    use super::*;
    use crate::diagnostics::recorder::format_line;

    #[derive(Message, Debug)]
    #[expect(dead_code, reason = "read only through Debug, by the logger")]
    struct TimeChanged {
        game_age_years: f64,
    }

    #[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
    enum Load {
        #[default]
        Loading,
        Ready,
    }

    /// Captures formatted events for one test thread.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl<S: Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut fields = String::new();
            event.record(&mut |field: &tracing::field::Field, value: &dyn Debug| {
                fields.push_str(&format!(" {}={value:?}", field.name()));
            });
            let meta = event.metadata();
            self.0.lock().unwrap().push(format_line(
                Default::default(),
                meta.level(),
                meta.target(),
                "",
                &fields,
            ));
        }
    }

    /// The loggers run in `Last`; keep them on this thread, where the test subscriber is.
    fn single_threaded_app() -> App {
        let mut app = App::new();
        app.edit_schedule(Last, |s| {
            s.set_executor(SingleThreadedExecutor::new());
        });
        app
    }

    #[test]
    fn domain_events_are_logged() {
        let capture = Capture::default();
        let subscriber = Registry::default().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut app = single_threaded_app();
            app.add_domain_event::<TimeChanged>();
            app.world_mut().write_message(TimeChanged {
                game_age_years: 66.0e6,
            });
            app.update();
        });
        let lines = capture.0.lock().unwrap().join("\n");
        assert!(
            lines.contains("TimeChanged { game_age_years: 66000000.0 }"),
            "{lines}"
        );
        assert!(lines.contains("INFO"), "{lines}");
    }

    #[derive(Message, Debug)]
    struct Tick {
        loud: bool,
    }

    impl DomainEventLevel for Tick {
        fn log_level(&self) -> Level {
            if self.loud { Level::INFO } else { Level::TRACE }
        }
    }

    #[test]
    fn leveled_domain_events_log_at_their_own_level() {
        let capture = Capture::default();
        let subscriber = Registry::default().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut app = single_threaded_app();
            app.add_leveled_domain_event::<Tick>();
            app.world_mut().write_message(Tick { loud: true });
            app.world_mut().write_message(Tick { loud: false });
            app.update();
        });
        let lines = capture.0.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("INFO") && lines[0].contains("Tick { loud: true }"),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("TRACE") && lines[1].contains("Tick { loud: false }"),
            "{lines:?}"
        );
    }

    #[test]
    fn state_transitions_are_logged() {
        let capture = Capture::default();
        let subscriber = Registry::default().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let mut app = single_threaded_app();
            app.add_plugins(StatesPlugin).add_logged_state::<Load>();
            app.update();
            app.world_mut()
                .resource_mut::<NextState<Load>>()
                .set(Load::Ready);
            app.update();
        });
        let lines = capture.0.lock().unwrap().join("\n");
        assert!(lines.contains("entered=Some(Loading)"), "{lines}");
        assert!(
            lines.contains("exited=Some(Loading) entered=Some(Ready)"),
            "{lines}"
        );
    }
}
