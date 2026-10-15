//! Runs compiled MicroTS views and shows one on the Board display.
//!
//! A view is a BVIR program plus its stylesheet and fonts. Its owner opens it
//! with [`UiService::open`] (apps) or [`UiService::open_on`] and keeps the
//! session; the session's [`ViewChannel`] writes typed [`ViewValue`]s and
//! receives handler calls. [`UiSession::show`] puts the view on its
//! [`Layer`]; the display claimed through the `display.main` slot shows the
//! system layer over the app layer. Taps from the `touch.main` slot press the
//! view on the display, and long presses and swipes go to the system UI
//! through [`UiService::next_gesture`]. This Plugin's task applies the writes,
//! draws what changed, and polls touch.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

mod datum;
mod gesture;
pub mod icons;
mod mailbox;
mod screen;
mod service;
#[cfg(test)]
mod tests;
mod view;

use alloc::boxed::Box;
use alloc::rc::Rc;
use barracuda_board_hal::display::{Display, DisplayPeripheral};
use barracuda_board_hal::touch::{Touch, TouchPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_vfs::ScopedVfs;
use core::{future::Future, pin::Pin};
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

/// Publishes [`UiService`] and drives the display and touch input it claims.
#[barracuda_plugin::macros::plugin]
pub struct UiPlugin<Panel, Input> {
    display: Option<Panel>,
    touch: Option<Input>,
    runtime: Option<UiRuntime<Panel, Input>>,
}

struct UiRuntime<Panel, Input> {
    service: Rc<UiService>,
    filesystem: ScopedVfs,
    screen: Option<Screen<Panel>>,
    pointer: Option<Pointer<Input>>,
    recognizer: Recognizer,
}

impl<Panel, Input> UiPlugin<Panel, Input> {
    /// Takes the display and touch input the Board gives this Plugin's slots.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: DisplayPeripheral<Display = Panel> + TouchPeripheral<Touch = Input>,
    {
        let peripherals = &mut context.hal.peripherals;
        Self {
            display: config::peripherals::DISPLAY_MAIN
                .and_then(|instance| peripherals.take_display(instance)),
            touch: config::peripherals::TOUCH_MAIN
                .and_then(|instance| peripherals.take_touch(instance)),
            runtime: None,
        }
    }
}

/// The Plugin started before it registered.
#[derive(Debug)]
struct UiRuntimeUnavailable;

impl core::fmt::Display for UiRuntimeUnavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ui runtime is unavailable")
    }
}

impl core::error::Error for UiRuntimeUnavailable {}

impl<Panel, Input> Plugin for UiPlugin<Panel, Input>
where
    Panel: Display + 'static,
    Input: Touch + 'static,
{
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let screen = self.display.take().map(Screen::new);
        let viewport = screen.as_ref().map_or(DEFAULT_VIEWPORT, Screen::viewport);
        let pointer = self.touch.take().map(|touch| Pointer::new(touch, viewport));
        let service = Rc::new(UiService::loading(viewport));
        context.provide(Rc::clone(&service))?;
        self.runtime = Some(UiRuntime {
            service,
            filesystem: context.filesystem()?.clone(),
            screen,
            pointer,
            recognizer: Recognizer::new(viewport),
        });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(UiRuntimeUnavailable))?;
        let task = ui_task(Box::pin(run(runtime, context.task_token())))
            .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// The UI loop, boxed once so one task serves every Board's display type.
type UiLoop = Pin<Box<dyn Future<Output = ()> + 'static>>;

#[embassy_executor::task]
async fn ui_task(ui: UiLoop) {
    ui.await;
}

/// Applies owners' writes, presses the shown view where it is tapped, hands
/// gestures to the system UI, and draws what changed, until cancelled.
async fn run<Panel: Display, Input: Touch>(
    mut runtime: UiRuntime<Panel, Input>,
    cancellation: PluginTaskToken,
) {
    let service = Rc::clone(&runtime.service);
    let work = async {
        service.set_system_fonts(system_fonts(&runtime.filesystem).await);
        let mut animating = false;
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
            if let Some(pointer) = runtime.pointer.as_mut() {
                let point = pointer.poll();
                match runtime.recognizer.sample(point, Instant::now().as_millis()) {
                    Some(Touched::Tap(x, y)) => {
                        let _handler = service.press_shown(x, y);
                    }
                    Some(Touched::Gesture(gesture)) => service.post_gesture(gesture),
                    None => {}
                }
            }
            if let Some(screen) = runtime.screen.as_mut() {
                if let Some(brightness) = service.take_brightness() {
                    screen.set_brightness(brightness).await;
                }
                animating = screen.refresh(&service).await;
            }
        }
    };
    select(cancellation.cancelled(), work).await;
    service.close_all();
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
