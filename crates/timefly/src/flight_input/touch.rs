//! Recognizes flight gestures in logical window pixels.

use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

const TAP_RADIUS_PX: f32 = 10.0;
const DOUBLE_TAP_RADIUS_PX: f32 = 30.0;
const TAP_MAX_S: f64 = 0.25;
const DOUBLE_TAP_MAX_S: f64 = 0.35;
const MIN_SPAN_PX: f32 = 24.0;

#[derive(Clone, Copy)]
struct Tap {
    position: Vec2,
    at_s: f64,
}

struct Contact {
    position: Vec2,
    tap: Option<Tap>,
}

impl Contact {
    fn move_to(&mut self, position: Vec2) {
        self.position = position;
        if self
            .tap
            .is_some_and(|tap| position.distance(tap.position) > TAP_RADIUS_PX)
        {
            self.tap = None;
        }
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Single(Vec2),
    Pair { center: Vec2, span: f32 },
}

#[derive(Default)]
pub(super) struct Gestures {
    contacts: HashMap<u64, Contact>,
    blocked: bool,
    last_tap: Option<Tap>,
    baseline: Option<Shape>,
}

#[derive(Default)]
pub(super) struct TouchIntent {
    pub look: Vec2,
    pub pan: Vec2,
    pub pinch: f32,
    pub double_tap: Option<Vec2>,
    pub started: bool,
}

impl Gestures {
    pub fn cancel_taps(&mut self) {
        self.last_tap = None;
        for contact in self.contacts.values_mut() {
            contact.tap = None;
        }
        self.baseline = self.shape();
    }

    /// Keep the whole sequence with the UI if any finger starts on it.
    pub fn block(&mut self) {
        self.blocked = !self.contacts.is_empty();
        self.cancel_taps();
        self.baseline = None;
    }

    pub fn read(
        &mut self,
        events: impl Iterator<Item = (TouchInput, bool)>,
        now_s: f64,
    ) -> TouchIntent {
        let mut intent = TouchIntent::default();
        for (event, over_ui) in events {
            let id = event.id;
            let position = event.position;
            if !position.is_finite()
                && matches!(event.phase, TouchPhase::Started | TouchPhase::Moved)
            {
                continue;
            }
            match event.phase {
                TouchPhase::Started => {
                    self.flush(&mut intent);
                    intent.started = true;
                    intent.double_tap = None;
                    let single = self.contacts.is_empty() && !over_ui;
                    if !single {
                        self.cancel_taps();
                    }
                    self.contacts.insert(
                        id,
                        Contact {
                            position,
                            tap: single.then_some(Tap {
                                position,
                                at_s: now_s,
                            }),
                        },
                    );
                    if over_ui {
                        self.block();
                        // UI fingers can arrive in the same frame as scene fingers.
                        intent = TouchIntent {
                            started: true,
                            ..default()
                        };
                    }
                    self.baseline = self.shape();
                }
                TouchPhase::Moved => {
                    if let Some(contact) = self.contacts.get_mut(&id) {
                        contact.move_to(position);
                    }
                }
                TouchPhase::Ended | TouchPhase::Canceled => {
                    let Some(contact) = self.contacts.get_mut(&id) else {
                        continue;
                    };
                    let ended = event.phase == TouchPhase::Ended && position.is_finite();
                    if ended {
                        contact.move_to(position);
                    }
                    self.flush(&mut intent);
                    let contact = self.contacts.remove(&id).unwrap();
                    let tap = contact.tap.filter(|t| now_s - t.at_s <= TAP_MAX_S);
                    if ended
                        && !self.blocked
                        && let Some(tap) = tap
                    {
                        if self.last_tap.take().is_some_and(|first| {
                            tap.at_s - first.at_s <= DOUBLE_TAP_MAX_S
                                && first.position.distance(position) <= DOUBLE_TAP_RADIUS_PX
                        }) {
                            intent.double_tap = Some(position);
                        } else {
                            self.last_tap = Some(Tap {
                                position,
                                at_s: now_s,
                            });
                        }
                    } else {
                        self.last_tap = None;
                    }
                    if self.contacts.is_empty() {
                        self.blocked = false;
                    }
                    self.baseline = self.shape();
                }
            }
        }
        self.flush(&mut intent);
        intent
    }

    fn shape(&self) -> Option<Shape> {
        if self.blocked {
            return None;
        }
        let mut contacts = self.contacts.values();
        let first = contacts.next()?;
        match contacts.next() {
            None => Some(Shape::Single(
                first.tap.map_or(first.position, |t| t.position),
            )),
            Some(second) if contacts.next().is_none() => Some(Shape::Pair {
                center: (first.position + second.position) * 0.5,
                span: first.position.distance(second.position).max(MIN_SPAN_PX),
            }),
            _ => None,
        }
    }

    // Coalesce moves within a frame so two parallel fingers don't register a pinch just
    // because the OS delivered the fingers' events serially. Rebase at each finger change.
    fn flush(&mut self, intent: &mut TouchIntent) {
        let next = self.shape();
        match (self.baseline, next) {
            (Some(Shape::Single(before)), Some(Shape::Single(after))) => {
                intent.look += after - before;
            }
            (
                Some(Shape::Pair { center, span }),
                Some(Shape::Pair {
                    center: next_center,
                    span: next_span,
                }),
            ) => {
                intent.pan += next_center - center;
                intent.pinch += (next_span / span).ln();
            }
            _ => {}
        }
        self.baseline = next;
    }
}
