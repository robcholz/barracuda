//! Runs compiled MicroTS views and shows them on a display.
//!
//! A view is a BVIR program plus its stylesheet and fonts. Its owner opens it
//! with [`UiService::open`] (apps) or [`UiService::open_on`] and keeps the
//! session; the session's [`ViewChannel`] writes typed [`ViewValue`]s and
//! receives handler calls. [`UiSession::show`] puts the view on its
//! [`Layer`]; the display shows the system layer over the app layer. [`Ui`]
//! drives a display and its touch input: taps press the view on the display,
//! vertical drags scroll it where it has something to scroll, and long
//! presses and swipes go to the system UI through
//! [`UiService::next_gesture`]. This crate knows nothing about apps.

#![no_std]

extern crate alloc;

mod datum;
mod gesture;
pub mod icons;
mod mailbox;
mod screen;
mod service;
#[cfg(test)]
mod tests;
mod view;

use alloc::rc::Rc;
use barracuda_board_hal::display::Display;
use barracuda_board_hal::touch::Touch;
use embassy_futures::select::select;
use embassy_time::{Duration, Instant, Timer};
use gesture::{Recognizer, Touched};
use screen::{Pointer, Screen};

pub use datum::{ViewType, ViewValue};
pub use gesture::{Direction, Edge, Gesture};
pub use mailbox::MAX_EVENTS;
pub use service::{DEFAULT_VIEWPORT, Layer, MAX_GESTURES, UiAssets, UiError, UiService, UiSession};
pub use view::{MAX_ARRAY_ITEMS, ViewCall, ViewChannel, ViewError, ViewNext};

/// How often the task steps an animating view and polls touch.
const FRAME: Duration = Duration::from_micros(16_667);

/// The display views are drawn on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
    /// Width in view pixels.
    pub width: u32,
    /// Height in view pixels.
    pub height: u32,
    /// Display pixels per view pixel along each axis.
    pub density: u8,
    /// Whether only the circle inscribed in the frame is visible.
    pub round: bool,
}

/// A display and its touch input, drawing the views of one [`UiService`].
pub struct Ui<Panel, Input> {
    service: Rc<UiService>,
    screen: Screen<Panel>,
    pointer: Option<Pointer<Input>>,
    recognizer: Recognizer,
}

impl<Panel: Display, Input: Touch> Ui<Panel, Input> {
    /// Lays views out at `display`'s size divided by `density`, the display
    /// pixels each view pixel covers along each axis, read by `touch` when it
    /// has one. Views opened before [`Ui::run`] reads the system fonts wait
    /// in [`UiService::ready`].
    pub fn new(display: Panel, touch: Option<Input>, density: u8) -> Self {
        let screen = Screen::new(display, density);
        let viewport = screen.viewport();
        Self {
            service: Rc::new(UiService::loading(viewport)),
            pointer: touch.map(|touch| Pointer::new(touch, screen.info())),
            recognizer: Recognizer::new(viewport),
            screen,
        }
    }

    /// The display's size in view pixels, density and shape.
    #[must_use]
    pub const fn display(&self) -> DisplayInfo {
        self.screen.info()
    }

    /// Whether the display reads touch.
    #[must_use]
    pub const fn touch(&self) -> bool {
        self.pointer.is_some()
    }

    /// The views this display shows.
    #[must_use]
    pub fn service(&self) -> Rc<UiService> {
        Rc::clone(&self.service)
    }

    /// Draws with the system font atlases `fonts`, none when no Plugin
    /// ships them, then applies owners' writes, presses the shown view where
    /// it is tapped, hands gestures to the system UI, and draws what changed,
    /// forever. Whoever stops it calls [`UiService::close_all`].
    pub async fn run(self, fonts: alloc::vec::Vec<alloc::vec::Vec<u8>>) {
        let mut runtime = self;
        let service = Rc::clone(&runtime.service);
        if fonts.is_empty() {
            log::info!("ui: no system fonts; views draw with their own atlases");
        }
        service.set_system_fonts(fonts);
        let mut animating = false;
        // The brightness the person picked, put back when the screen wakes.
        let mut level = u8::MAX;
        let mut last_touch = Instant::now();
        let mut asleep = false;
        // A contact that woke the screen, ignored until it ends.
        let mut waking = false;
        loop {
            // Touch is read while anything is on the display, and until a
            // contact that began ends.
            let polling = runtime.pointer.is_some()
                && (service.shown().view.is_some() || runtime.recognizer.is_touching());
            if animating || polling {
                select(service.changed(), Timer::after(FRAME)).await;
            } else {
                service.changed().await;
            }
            service.pump();
            let mut wake = service.take_wake();
            if let Some(pointer) = runtime.pointer.as_mut() {
                let point = pointer.poll();
                if point.is_some() {
                    last_touch = Instant::now();
                    if asleep {
                        wake = true;
                        waking = true;
                    }
                }
                if waking {
                    waking = point.is_some();
                } else {
                    match runtime.recognizer.sample(point, Instant::now().as_millis()) {
                        Some(Touched::Tap(x, y)) => {
                            let _handler = service.press_shown(x, y);
                        }
                        Some(Touched::Gesture(gesture)) => service.post_gesture(gesture),
                        Some(Touched::DragStart { x, y, dy }) => {
                            if !service.drag_start(x, y, dy) {
                                runtime.recognizer.decline_drag();
                            }
                        }
                        Some(Touched::Drag(dy)) => service.drag_by(dy),
                        Some(Touched::DragEnd) => service.drag_end(),
                        None => {}
                    }
                }
            }
            if wake {
                last_touch = Instant::now();
            }
            {
                let screen = &mut runtime.screen;
                if let Some(brightness) = service.take_brightness() {
                    level = brightness;
                    if !asleep {
                        screen.set_brightness(level).await;
                    }
                }
                if asleep && wake {
                    asleep = false;
                    screen.set_brightness(level).await;
                } else if !asleep
                    && runtime.pointer.is_some()
                    && service
                        .screen_timeout()
                        .is_some_and(|timeout| last_touch.elapsed() >= timeout)
                {
                    asleep = true;
                    screen.set_brightness(0).await;
                }
                animating = screen.refresh(&service).await;
            }
        }
    }
}
