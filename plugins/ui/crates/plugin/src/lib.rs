//! Runs compiled MicroTS views and shows one on the Board display.
//!
//! A view is a BVIR program plus its stylesheet and fonts. Its owner opens it
//! with [`UiService::open`] and keeps the session; the session's
//! [`ViewChannel`] writes typed [`ViewValue`]s and receives handler calls.
//! [`UiSession::show`] puts the view on the display claimed through the
//! `display.main` slot, and touches from the `touch.main` slot press it. This
//! Plugin's task applies the writes, draws what changed, and polls touch.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

mod datum;
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
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use core::{future::Future, pin::Pin};
use embassy_futures::select::select;
use embassy_time::{Duration, Timer};
use screen::{Pointer, Screen};

pub use datum::{ViewType, ViewValue};
pub use mailbox::MAX_EVENTS;
pub use service::{DEFAULT_VIEWPORT, UiAssets, UiError, UiService, UiSession};
pub use view::{MAX_ARRAY_ITEMS, ViewCall, ViewChannel, ViewError, ViewNext};

/// How often the task steps an animating view and polls touch.
const FRAME: Duration = Duration::from_micros(16_667);

/// Publishes [`UiService`] and drives the display and touch input it claims.
#[barracuda_plugin::macros::plugin]
pub struct UiPlugin<Panel, Input> {
    display: Option<Panel>,
    touch: Option<Input>,
    runtime: Option<UiRuntime<Panel, Input>>,
}

struct UiRuntime<Panel, Input> {
    service: Rc<UiService>,
    screen: Option<Screen<Panel>>,
    pointer: Option<Pointer<Input>>,
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
        let service = Rc::new(UiService::new(viewport));
        context.provide(Rc::clone(&service))?;
        self.runtime = Some(UiRuntime {
            service,
            screen,
            pointer,
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

/// Applies owners' writes, presses the shown view where it is touched, and
/// draws what changed, until cancelled.
async fn run<Panel: Display, Input: Touch>(
    mut runtime: UiRuntime<Panel, Input>,
    cancellation: PluginTaskToken,
) {
    let service = Rc::clone(&runtime.service);
    let work = async {
        let mut animating = false;
        loop {
            let polling = runtime.pointer.is_some() && service.shown().input;
            if animating || polling {
                select(service.changed(), Timer::after(FRAME)).await;
            } else {
                service.changed().await;
            }
            service.pump();
            if let Some((x, y)) = runtime.pointer.as_mut().and_then(Pointer::poll) {
                let _handler = service.press_shown(x, y);
            }
            animating = match runtime.screen.as_mut() {
                Some(screen) => screen.refresh(&service).await,
                None => false,
            };
        }
    };
    select(cancellation.cancelled(), work).await;
    service.close_all();
}
