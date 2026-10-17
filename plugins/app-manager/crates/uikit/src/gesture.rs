//! Turns touch contacts into taps and the single-finger gestures the system
//! UI navigates by.
//!
//! The recognizer sees one sample per frame: where the finger is, or that it
//! is lifted. A tap is a short contact that stays put; it presses the view on
//! the display. Long presses and swipes are [`Gesture`]s, which the UI hands
//! to the system UI through [`crate::UiService::next_gesture`] and never to
//! an app's view.

/// How far a finger may drift and still tap or long-press, in view pixels.
const SLOP: f32 = 12.0;
/// How far a finger must travel along one axis to swipe, in view pixels.
const SWIPE: f32 = 40.0;
/// How long a finger must rest to long-press.
pub(crate) const LONG_PRESS_MS: u64 = 500;
/// The share of the viewport along each side where an edge swipe starts.
const EDGE_SHARE: f32 = 0.12;

/// Which way a swipe travelled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// Toward the top of the display.
    Up,
    /// Toward the bottom of the display.
    Down,
    /// Toward the left of the display.
    Left,
    /// Toward the right of the display.
    Right,
}

/// A side of the display a swipe started at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    /// The top side; swipes down from it.
    Top,
    /// The bottom side; swipes up from it.
    Bottom,
    /// The left side; swipes right from it.
    Left,
    /// The right side; swipes left from it.
    Right,
}

/// A gesture for the system UI, in view coordinates.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gesture {
    /// A finger rested in place for [`LONG_PRESS_MS`]; reported while it is
    /// still down, and the contact taps nothing.
    LongPress {
        /// Where the finger rests.
        x: f32,
        /// Where the finger rests.
        y: f32,
    },
    /// A finger travelled along one axis and lifted.
    Swipe {
        /// Which way it travelled.
        direction: Direction,
        /// The side it started at, when it started at the side it travelled
        /// away from.
        edge: Option<Edge>,
        /// Where it started.
        x: f32,
        /// Where it started.
        y: f32,
    },
}

/// What one contact amounted to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Touched {
    /// A short contact that stayed put: press the view under it.
    Tap(f32, f32),
    /// A gesture for the system UI.
    Gesture(Gesture),
}

struct Contact {
    start: (f32, f32),
    at: (f32, f32),
    since_ms: u64,
    /// The contact already long-pressed or moved, so it cannot tap.
    spent: bool,
}

/// Classifies contacts from per-frame samples.
pub(crate) struct Recognizer {
    viewport: (f32, f32),
    contact: Option<Contact>,
}

impl Recognizer {
    pub(crate) const fn new(viewport: (f32, f32)) -> Self {
        Self {
            viewport,
            contact: None,
        }
    }

    /// Whether a finger is on the panel.
    pub(crate) const fn is_touching(&self) -> bool {
        self.contact.is_some()
    }

    /// Takes the finger's position at `now_ms`, or `None` once it lifted.
    pub(crate) fn sample(&mut self, point: Option<(f32, f32)>, now_ms: u64) -> Option<Touched> {
        match (point, self.contact.as_mut()) {
            (Some(point), None) => {
                self.contact = Some(Contact {
                    start: point,
                    at: point,
                    since_ms: now_ms,
                    spent: false,
                });
                None
            }
            (Some(point), Some(contact)) => {
                contact.at = point;
                if contact.spent {
                    return None;
                }
                if beyond_slop(contact.start, point) {
                    contact.spent = true;
                    return None;
                }
                if now_ms.saturating_sub(contact.since_ms) >= LONG_PRESS_MS {
                    contact.spent = true;
                    let (x, y) = contact.start;
                    return Some(Touched::Gesture(Gesture::LongPress { x, y }));
                }
                None
            }
            (None, Some(_)) => {
                let contact = self.contact.take()?;
                self.lifted(&contact)
            }
            (None, None) => None,
        }
    }

    fn lifted(&self, contact: &Contact) -> Option<Touched> {
        let (x, y) = contact.start;
        let (dx, dy) = (contact.at.0 - x, contact.at.1 - y);
        if !contact.spent && !beyond_slop(contact.start, contact.at) {
            return Some(Touched::Tap(x, y));
        }
        let direction = if dx.abs() >= dy.abs() {
            if dx.abs() < SWIPE {
                return None;
            }
            if dx < 0.0 {
                Direction::Left
            } else {
                Direction::Right
            }
        } else {
            if dy.abs() < SWIPE {
                return None;
            }
            if dy < 0.0 {
                Direction::Up
            } else {
                Direction::Down
            }
        };
        let (width, height) = self.viewport;
        let edge = match direction {
            Direction::Down if y < height * EDGE_SHARE => Some(Edge::Top),
            Direction::Up if y > height * (1.0 - EDGE_SHARE) => Some(Edge::Bottom),
            Direction::Right if x < width * EDGE_SHARE => Some(Edge::Left),
            Direction::Left if x > width * (1.0 - EDGE_SHARE) => Some(Edge::Right),
            _ => None,
        };
        Some(Touched::Gesture(Gesture::Swipe {
            direction,
            edge,
            x,
            y,
        }))
    }
}

fn beyond_slop(a: (f32, f32), b: (f32, f32)) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    dx * dx + dy * dy > SLOP * SLOP
}
