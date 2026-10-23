//! Button events, and the one rule every level-sampling button implementation
//! uses to derive them.
//!
//! The rule is M5Unified's `Button_Class::setRawState`: a new level counts
//! once it has held for the debounce time, or at once when the previous
//! sample is older than that, so a slow poller sees a press without a second
//! sample. A button held for the long-press time reports a long press while
//! it is still down, and one released before that reports a click. M5Unified
//! defaults to a 10 ms debounce and a 500 ms hold, which
//! [`ButtonTiming::default`] repeats.

use crate::buttons::{ButtonSnapshot, MAX_BUTTONS};

/// What happened to one button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ButtonEventKind {
    /// The button went down.
    Pressed,
    /// The button came up.
    Released,
    /// The button came up before the long-press time: a short press. It
    /// follows that press's [`Released`](Self::Released).
    Click,
    /// The button has been down for the long-press time. It comes while the
    /// button is still down, before its [`Released`](Self::Released), and no
    /// [`Click`](Self::Click) follows that release.
    LongPress,
}

impl ButtonEventKind {
    /// Returns the event's stable lower-case name: `pressed`, `released`,
    /// `click`, or `long_press`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pressed => "pressed",
            Self::Released => "released",
            Self::Click => "click",
            Self::LongPress => "long_press",
        }
    }
}

/// One event of one button, by the button's index in the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ButtonEvent {
    index: u8,
    kind: ButtonEventKind,
}

impl ButtonEvent {
    /// Creates an event of the button at `index`.
    #[must_use]
    pub const fn new(index: u8, kind: ButtonEventKind) -> Self {
        Self { index, kind }
    }

    /// Returns the button's stable zero-based index, as in
    /// [`ButtonSnapshot::is_pressed`].
    #[must_use]
    pub const fn index(self) -> usize {
        self.index as usize
    }

    /// Returns what happened.
    #[must_use]
    pub const fn kind(self) -> ButtonEventKind {
        self.kind
    }
}

/// Events one implementation holds until they are taken: two for each of
/// [`MAX_BUTTONS`], the most one sample of every button raises.
pub const EVENT_QUEUE_CAPACITY: usize = 2 * MAX_BUTTONS;

/// A bounded first-in, first-out queue of button events.
///
/// When it is full, a new event drops the oldest one, so a reader that comes
/// back after a pause sees the latest events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonEventQueue {
    events: [ButtonEvent; EVENT_QUEUE_CAPACITY],
    head: usize,
    len: usize,
}

impl Default for ButtonEventQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonEventQueue {
    /// Creates an empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: [ButtonEvent::new(0, ButtonEventKind::Pressed); EVENT_QUEUE_CAPACITY],
            head: 0,
            len: 0,
        }
    }

    /// Returns how many events wait.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether no event waits.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends one event, dropping the oldest when the queue is full.
    pub fn push(&mut self, event: ButtonEvent) {
        if self.len == EVENT_QUEUE_CAPACITY {
            self.head = (self.head + 1) % EVENT_QUEUE_CAPACITY;
            self.len -= 1;
        }
        self.events[(self.head + self.len) % EVENT_QUEUE_CAPACITY] = event;
        self.len += 1;
    }

    /// Removes and returns the oldest event.
    pub fn pop(&mut self) -> Option<ButtonEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head];
        self.head = (self.head + 1) % EVENT_QUEUE_CAPACITY;
        self.len -= 1;
        Some(event)
    }

    /// Appends the events of a press that hardware reports only once it has
    /// happened, such as a PMIC's latched power-key interrupt: pressed,
    /// released, and click for a short press; pressed, long press, and
    /// released for a long one.
    pub fn push_latched_press(&mut self, index: u8, long: bool) {
        self.push(ButtonEvent::new(index, ButtonEventKind::Pressed));
        if long {
            self.push(ButtonEvent::new(index, ButtonEventKind::LongPress));
            self.push(ButtonEvent::new(index, ButtonEventKind::Released));
        } else {
            self.push(ButtonEvent::new(index, ButtonEventKind::Released));
            self.push(ButtonEvent::new(index, ButtonEventKind::Click));
        }
    }
}

/// How long a new level must hold before it counts, and how long a press
/// must last to be a long press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonTiming {
    debounce_ms: u32,
    long_press_ms: u32,
}

impl Default for ButtonTiming {
    /// M5Unified's `Button_Class` defaults: 10 ms debounce, 500 ms hold.
    fn default() -> Self {
        Self::new(Self::DEFAULT_DEBOUNCE_MS, Self::DEFAULT_LONG_PRESS_MS)
    }
}

impl ButtonTiming {
    /// M5Unified `Button_Class::_msecDebounce`.
    pub const DEFAULT_DEBOUNCE_MS: u32 = 10;
    /// M5Unified `Button_Class::_msecHold`.
    pub const DEFAULT_LONG_PRESS_MS: u32 = 500;

    /// Creates timing from the debounce and long-press times.
    #[must_use]
    pub const fn new(debounce_ms: u32, long_press_ms: u32) -> Self {
        Self {
            debounce_ms,
            long_press_ms,
        }
    }

    /// Returns how long a new level must hold before it counts.
    #[must_use]
    pub const fn debounce_ms(self) -> u32 {
        self.debounce_ms
    }

    /// Returns how long a press lasts before it is a long press.
    #[must_use]
    pub const fn long_press_ms(self) -> u32 {
        self.long_press_ms
    }
}

/// One button's debounce and press state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tracked {
    raw: bool,
    raw_since_ms: u64,
    pressed: bool,
    pressed_since_ms: u64,
    long_reported: bool,
    /// Down since the tracker started: the press is not one the application
    /// made, so it raises no events.
    silent: bool,
}

/// Debounced button states and the events their changes raise, fed with raw
/// samples and the time they were taken.
///
/// An implementation that reads button levels keeps one tracker, calls
/// [`sample`](Self::sample) from both [`Buttons::read`] and
/// [`Buttons::poll_event`], the latter only when no event waits, and returns
/// [`pop_event`](Self::pop_event) from `poll_event`.
///
/// [`Buttons::read`]: crate::buttons::Buttons::read
/// [`Buttons::poll_event`]: crate::buttons::Buttons::poll_event
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonTracker {
    buttons: [Tracked; MAX_BUTTONS],
    count: usize,
    timing: ButtonTiming,
    last_sample_ms: u64,
    events: ButtonEventQueue,
}

impl ButtonTracker {
    /// Starts tracking the buttons in `initial`, sampled at `now_ms`. A
    /// button already down raises no events until it has been released.
    #[must_use]
    pub fn new(initial: &[bool], now_ms: u64, timing: ButtonTiming) -> Self {
        let mut buttons = [Tracked::default(); MAX_BUTTONS];
        let mut count = 0;
        for (button, &pressed) in buttons.iter_mut().zip(initial) {
            *button = Tracked {
                raw: pressed,
                raw_since_ms: now_ms,
                pressed,
                pressed_since_ms: now_ms,
                long_reported: false,
                silent: pressed,
            };
            count += 1;
        }
        Self {
            buttons,
            count,
            timing,
            last_sample_ms: now_ms,
            events: ButtonEventQueue::new(),
        }
    }

    /// Returns how many buttons are tracked.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Returns whether no button is tracked.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the timing the tracker applies.
    #[must_use]
    pub const fn timing(&self) -> ButtonTiming {
        self.timing
    }

    /// Returns whether an event waits.
    #[must_use]
    pub const fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Takes one raw sample of every button, taken at `now_ms`, queues the
    /// events its debounced changes raise, and returns the debounced states.
    /// A button missing from `raw` reads released.
    pub fn sample(&mut self, raw: &[bool], now_ms: u64) -> ButtonSnapshot {
        let debounce = u64::from(self.timing.debounce_ms);
        let long_press = u64::from(self.timing.long_press_ms);
        let idle = now_ms.saturating_sub(self.last_sample_ms) > debounce;
        let mut states = [false; MAX_BUTTONS];
        for (index, (button, state)) in self.buttons[..self.count]
            .iter_mut()
            .zip(&mut states)
            .enumerate()
        {
            let index = u8::try_from(index).unwrap_or(u8::MAX);
            let level = raw.get(usize::from(index)).copied().unwrap_or(false);
            if level != button.raw {
                button.raw = level;
                button.raw_since_ms = now_ms;
            }
            let settled = idle || now_ms.saturating_sub(button.raw_since_ms) >= debounce;
            if settled && level != button.pressed {
                button.pressed = level;
                if level {
                    button.pressed_since_ms = now_ms;
                    button.long_reported = false;
                    button.silent = false;
                    self.events
                        .push(ButtonEvent::new(index, ButtonEventKind::Pressed));
                } else if button.silent {
                    button.silent = false;
                } else {
                    let held = now_ms.saturating_sub(button.pressed_since_ms);
                    if !button.long_reported && held >= long_press {
                        button.long_reported = true;
                        self.events
                            .push(ButtonEvent::new(index, ButtonEventKind::LongPress));
                    }
                    self.events
                        .push(ButtonEvent::new(index, ButtonEventKind::Released));
                    if !button.long_reported {
                        self.events
                            .push(ButtonEvent::new(index, ButtonEventKind::Click));
                    }
                }
            }
            if button.pressed
                && !button.silent
                && !button.long_reported
                && now_ms.saturating_sub(button.pressed_since_ms) >= long_press
            {
                button.long_reported = true;
                self.events
                    .push(ButtonEvent::new(index, ButtonEventKind::LongPress));
            }
            *state = button.pressed;
        }
        self.last_sample_ms = now_ms;
        ButtonSnapshot::new(&states[..self.count])
    }

    /// Removes and returns the oldest event.
    pub fn pop_event(&mut self) -> Option<ButtonEvent> {
        self.events.pop()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::{
        ButtonEvent, ButtonEventKind, ButtonEventQueue, ButtonTiming, ButtonTracker,
        EVENT_QUEUE_CAPACITY,
    };
    use ButtonEventKind::{Click, LongPress, Pressed, Released};

    fn drain(tracker: &mut ButtonTracker) -> Vec<(usize, ButtonEventKind)> {
        core::iter::from_fn(|| tracker.pop_event())
            .map(|event| (event.index(), event.kind()))
            .collect()
    }

    #[test]
    fn a_short_press_is_pressed_released_and_a_click() {
        let mut tracker = ButtonTracker::new(&[false, false], 0, ButtonTiming::default());
        // Polled every 5 ms: the press counts after it held 10 ms.
        assert_eq!(tracker.sample(&[true, false], 5).is_pressed(0), Some(false));
        assert_eq!(
            tracker.sample(&[true, false], 10).is_pressed(0),
            Some(false)
        );
        assert_eq!(tracker.sample(&[true, false], 15).is_pressed(0), Some(true));
        assert_eq!(drain(&mut tracker), [(0, Pressed)]);
        tracker.sample(&[true, false], 95);
        tracker.sample(&[false, false], 100);
        tracker.sample(&[false, false], 105);
        assert!(!tracker.has_events());
        assert_eq!(
            tracker.sample(&[false, false], 110).is_pressed(0),
            Some(false)
        );
        assert_eq!(drain(&mut tracker), [(0, Released), (0, Click)]);
    }

    #[test]
    fn a_bounce_shorter_than_the_debounce_time_raises_nothing() {
        let mut tracker = ButtonTracker::new(&[false], 0, ButtonTiming::default());
        tracker.sample(&[true], 2);
        tracker.sample(&[false], 4);
        tracker.sample(&[false], 6);
        tracker.sample(&[false], 20);
        assert_eq!(drain(&mut tracker), []);
    }

    #[test]
    fn a_held_button_reports_a_long_press_once_and_no_click() {
        let mut tracker = ButtonTracker::new(&[false], 0, ButtonTiming::new(10, 500));
        tracker.sample(&[true], 50);
        let mut now = 50;
        while now < 600 {
            now += 10;
            tracker.sample(&[true], now);
        }
        assert_eq!(drain(&mut tracker), [(0, Pressed), (0, LongPress)]);
        tracker.sample(&[false], 700);
        assert_eq!(drain(&mut tracker), [(0, Released)]);
    }

    #[test]
    fn a_slow_poller_sees_a_long_press_before_its_release() {
        let mut tracker = ButtonTracker::new(&[false], 0, ButtonTiming::new(10, 500));
        tracker.sample(&[true], 100);
        tracker.sample(&[false], 900);
        assert_eq!(
            drain(&mut tracker),
            [(0, Pressed), (0, LongPress), (0, Released)]
        );
    }

    #[test]
    fn a_press_held_at_start_raises_no_events() {
        let mut tracker = ButtonTracker::new(&[true, false], 0, ButtonTiming::default());
        assert_eq!(
            tracker.sample(&[true, true], 1_000).is_pressed(0),
            Some(true)
        );
        tracker.sample(&[false, false], 1_100);
        assert_eq!(
            drain(&mut tracker),
            [(1, Pressed), (1, Released), (1, Click)]
        );
        tracker.sample(&[true, false], 3_000);
        assert_eq!(drain(&mut tracker), [(0, Pressed)]);
    }

    #[test]
    fn a_full_queue_drops_the_oldest_event() {
        let mut queue = ButtonEventQueue::new();
        for index in 0..=EVENT_QUEUE_CAPACITY {
            queue.push(ButtonEvent::new(
                u8::try_from(index).unwrap_or(u8::MAX),
                Pressed,
            ));
        }
        assert_eq!(queue.len(), EVENT_QUEUE_CAPACITY);
        assert_eq!(queue.pop().map(ButtonEvent::index), Some(1));
        queue.push_latched_press(3, true);
        queue.push_latched_press(4, false);
        let tail: Vec<_> = core::iter::from_fn(|| queue.pop())
            .skip(EVENT_QUEUE_CAPACITY - 6)
            .map(|event| (event.index(), event.kind()))
            .collect();
        assert_eq!(
            tail,
            [
                (3, Pressed),
                (3, LongPress),
                (3, Released),
                (4, Pressed),
                (4, Released),
                (4, Click)
            ]
        );
        assert!(queue.is_empty());
    }

    #[test]
    fn event_names_are_stable() {
        assert_eq!(Pressed.name(), "pressed");
        assert_eq!(Released.name(), "released");
        assert_eq!(Click.name(), "click");
        assert_eq!(LongPress.name(), "long_press");
    }
}
