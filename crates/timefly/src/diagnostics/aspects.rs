//! Tracing as an aspect (spec §20): registering a domain event or a state also registers its
//! logger, so no call site logs them by hand. Later tasks register events and states only
//! through [`DiagnosticsAppExt`].

use std::any::type_name;
use std::fmt::Debug;

use bevy::ecs::message::Message;
use bevy::prelude::*;
use bevy::state::state::FreelyMutableState;

pub trait DiagnosticsAppExt {
    /// Registers message `E` and a system that logs each one at `info`.
    fn add_domain_event<E: Message + Debug>(&mut self) -> &mut Self;

    /// Initializes state `S` and logs every transition at `info`.
    fn add_logged_state<S: FreelyMutableState + FromWorld>(&mut self) -> &mut Self;
}

impl DiagnosticsAppExt for App {
    fn add_domain_event<E: Message + Debug>(&mut self) -> &mut Self {
        self.add_message::<E>()
            .add_systems(Last, log_domain_events::<E>)
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
