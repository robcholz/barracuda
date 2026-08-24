//! Plugin entry point that provides the portable WebServer capability.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::marker::PhantomData;

use barracuda_plugin_manager::{Plugin, PluginContext, PluginStartFuture};

pub use listener::{WebServerListenFuture, WebServerListener};
pub use webserver::*;

mod component;
mod listener;
mod webserver;

/// Stable identity of the WebServer capability provider.
pub const PLUGIN_ID: &str = "webserver";

/// TCP port served by the WebServer Plugin.
pub const WEB_SERVER_PORT: u16 = 8787;

/// Number of connections that the platform listener serves concurrently.
pub const WEB_SERVER_CONNECTION_SLOTS: usize = 4;

/// Plugin that owns and publishes the portable WebServer.
///
/// The server is available to dependent Plugins only through the typed
/// capability registry.
///
/// ```compile_fail
/// use barracuda_webserver_plugin::WebServerPlugin;
///
/// fn expose<Listener>(plugin: WebServerPlugin<Listener>) {
///     let _server = plugin.webserver();
/// }
/// ```
pub struct WebServerPlugin<Listener> {
    listener: PhantomData<fn() -> Listener>,
}

impl<Listener> Default for WebServerPlugin<Listener> {
    fn default() -> Self {
        Self {
            listener: PhantomData,
        }
    }
}

impl<const M: usize, Listener> Plugin<M> for WebServerPlugin<Listener>
where
    Listener: Clone + WebServerListener + 'static,
{
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let listener = context.require_system::<&'static Listener>()?;
            let listener = (**listener).clone();
            let webserver = Rc::new(WebServer::new());
            context.provide(Rc::clone(&webserver))?;
            context.load(component::WebServerComponent::new(webserver, listener))?;
            Ok(())
        })
    }
}
