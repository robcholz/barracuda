//! Runs compiled MicroTS views for apps and binds them to Lua.
//!
//! An app's view is a BVIR program plus its stylesheet and fonts. A caller such
//! as the app manager opens it with [`UiService::open`] and hands the session's
//! [`ViewChannel`] to the app's Lua package, which gives Lua a [`LuaView`]. Lua
//! writes values and receives handler calls; this Plugin's task applies the
//! writes and keeps the UI tree laid out. Rendering to a display is not
//! connected yet; views are laid out at [`DEFAULT_VIEWPORT`].

#![no_std]

extern crate alloc;

mod datum;
mod mailbox;
mod service;
#[cfg(test)]
mod tests;
mod view;

use alloc::rc::Rc;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use embassy_futures::select::select;

pub use mailbox::MAX_EVENTS;
pub use service::{DEFAULT_VIEWPORT, UiAssets, UiError, UiService, UiSession};
pub use view::{LuaView, MAX_ARRAY_ITEMS, ViewCall, ViewChannel, ViewNext};

/// Publishes [`UiService`].
#[barracuda_plugin::macros::plugin]
pub struct UiPlugin {
    runtime: Option<UiRuntime>,
}

struct UiRuntime {
    service: Rc<UiService>,
}

impl UiPlugin {
    /// Creates an unregistered UI Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
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

impl Plugin for UiPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let service = Rc::new(UiService::new(DEFAULT_VIEWPORT));
        context.provide(Rc::clone(&service))?;
        self.runtime = Some(UiRuntime { service });
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
        let task = ui_task(runtime, context.task_token()).map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

/// Applies Lua writes to the views as they arrive.
#[embassy_executor::task]
async fn ui_task(runtime: UiRuntime, cancellation: PluginTaskToken) {
    select(cancellation.cancelled(), runtime.service.drive()).await;
    runtime.service.close_all();
}
