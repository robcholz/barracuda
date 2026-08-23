//! Plugin entry point that provides the portable WebServer capability.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;
#[cfg(feature = "tokio")]
extern crate std;

use alloc::boxed::Box;
use alloc::rc::Rc;

use barracuda_plugin_manager::{Plugin, PluginContext, PluginStartFuture};

pub use listener::{WebServerListenFuture, WebServerListener};
pub use webserver::*;

mod component;
#[cfg(feature = "embassy")]
mod embassy_listener;
mod listener;
#[cfg(feature = "tokio")]
mod tokio_listener;
mod webserver;

/// Stable identity of the WebServer capability provider.
pub const PLUGIN_ID: &str = "webserver";

/// TCP port served by the WebServer Plugin.
pub const WEB_SERVER_PORT: u16 = 8787;

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
    webserver: Rc<WebServer>,
    listener: Listener,
}

impl<Listener> WebServerPlugin<Listener> {
    /// Creates the WebServer Plugin from the platform listener capability.
    #[must_use]
    pub fn new(listener: Listener) -> Self {
        Self {
            webserver: Rc::new(WebServer::new()),
            listener,
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

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.provide(Rc::clone(&self.webserver))?;
            context.load(component::WebServerComponent::new(
                Rc::clone(&self.webserver),
                self.listener.clone(),
            ))?;
            Ok(())
        })
    }
}
