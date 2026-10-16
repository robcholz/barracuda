//! Runs compiled MicroTS views and shows them on a display.
//!
//! A view is a BVIR program plus its stylesheet and fonts. Its owner opens it
//! with [`UiService::open`] (apps) or [`UiService::open_on`] and keeps the
//! session; the session's [`ViewChannel`] writes typed [`ViewValue`]s and
//! receives handler calls. [`UiSession::show`] puts the view on its
//! [`Layer`]; the display shows the system layer over the app layer. [`Ui`]
//! drives a display and its touch input: taps press the view on the display,
//! and long presses and swipes go to the system UI through
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
use barracuda_vfs::ScopedVfs;
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
/// Where the system fonts live in this Plugin's filesystem.
pub const SYSTEM_FONTS: &str = "/resources/fonts";

/// A display and its touch input, drawing the views of one [`UiService`].
pub struct Ui<Panel, Input> {
    service: Rc<UiService>,
    screen: Screen<Panel>,
    pointer: Option<Pointer<Input>>,
    recognizer: Recognizer,
}

impl<Panel: Display, Input: Touch> Ui<Panel, Input> {
    /// Lays views out at `display`'s size, read by `touch` when it has one.
    /// Views opened before [`Ui::run`] reads the system fonts wait in
    /// [`UiService::ready`].
    pub fn new(display: Panel, touch: Option<Input>) -> Self {
        let screen = Screen::new(display);
        let viewport = screen.viewport();
        Self {
            service: Rc::new(UiService::loading(viewport)),
            pointer: touch.map(|touch| Pointer::new(touch, viewport)),
            recognizer: Recognizer::new(viewport),
            screen,
        }
    }

    /// The views this display shows.
    #[must_use]
    pub fn service(&self) -> Rc<UiService> {
        Rc::clone(&self.service)
    }

    /// Reads the system fonts from `filesystem`, then applies owners'
    /// writes, presses the shown view where it is tapped, hands gestures to
    /// the system UI, and draws what changed, forever. Whoever stops it
    /// calls [`UiService::close_all`].
    pub async fn run(self, filesystem: &ScopedVfs) {
        let mut runtime = self;
        let service = Rc::clone(&runtime.service);
        service.set_system_fonts(system_fonts(filesystem).await);
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

/// Reads every system font atlas; a missing or unreadable one is left out.
async fn system_fonts(filesystem: &ScopedVfs) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
    let mut names = match filesystem.list_dir(SYSTEM_FONTS).await {
        Ok(names) => names,
        Err(error) => {
            log::warn!("ui: no system fonts under {SYSTEM_FONTS}: {error:?}");
            return alloc::vec::Vec::new();
        }
    };
    names.retain(|name| name.starts_with("font-") && name.ends_with(".bin"));
    names.sort_unstable();
    let mut fonts = alloc::vec::Vec::with_capacity(names.len());
    for name in names {
        match filesystem
            .read(&alloc::format!("{SYSTEM_FONTS}/{name}"))
            .await
        {
            Ok(font) => fonts.push(font),
            Err(error) => log::warn!("ui: cannot read system font {name}: {error:?}"),
        }
    }
    fonts
}
