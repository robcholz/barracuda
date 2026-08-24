//! Plugin entry point that provides the portable WebServer capability.

#![no_std]
#![recursion_limit = "256"]

extern crate alloc;

use alloc::rc::Rc;

use barracuda_plugin_manager::{
    Plugin, PluginContext, PluginError, PluginResult, PluginStartContext,
};
use embassy_net::Stack;

pub use webserver::*;

mod task;
mod webserver;

/// Stable identity of the WebServer capability provider.
pub const PLUGIN_ID: &str = "webserver";

/// TCP port served by the WebServer Plugin.
pub const WEB_SERVER_PORT: u16 = 8787;

/// Number of connections that the WebServer task serves concurrently.
pub const WEB_SERVER_CONNECTION_SLOTS: usize = 4;

/// Plugin that owns and publishes the portable WebServer.
///
/// The server is available to dependent Plugins only through the typed
/// capability registry.
///
/// ```compile_fail
/// use barracuda_webserver_plugin::WebServerPlugin;
///
/// fn expose(plugin: WebServerPlugin) {
///     let _server = plugin.webserver();
/// }
/// ```
pub struct WebServerPlugin {
    stack: Stack<'static>,
    runtime: Option<Rc<WebServer>>,
}

impl WebServerPlugin {
    /// Creates the Plugin with the IP stack used by its owned server task.
    #[must_use]
    pub const fn new(stack: Stack<'static>) -> Self {
        Self {
            stack,
            runtime: None,
        }
    }
}

impl<const M: usize> Plugin<M> for WebServerPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let webserver = Rc::new(WebServer::new());
        context.provide(Rc::clone(&webserver))?;
        self.runtime = Some(webserver);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let webserver = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(WebServerRuntimeUnavailable))?;
        context
            .task_spawner()?
            .spawn(task::web_server(webserver, self.stack))
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("WebServer runtime was not prepared during Plugin registration")]
struct WebServerRuntimeUnavailable;
