//! Dynamic outbound HTTP RPC Plugin.
#![no_std]

mod component;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};
use http_client::ClientFactory;

use component::HttpComponent;

/// Stable identity of the HTTP Plugin.
pub const PLUGIN_ID: &str = "http";

/// Plugin exposing the shared HTTP client through a dynamic RPC.
pub struct HttpPlugin {
    clients: ClientFactory<'static>,
}
impl HttpPlugin {
    /// Creates a Plugin from System's shared HTTP client factory.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            clients: context.http_clients.clone(),
        }
    }
}
impl<const M: usize> Plugin<M> for HttpPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }
    fn register<S: barracuda_plugin_manager::PluginStorage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, S>,
    ) -> PluginResult<()> {
        context
            .event_router
            .load(HttpComponent::new(self.clients.clone()))?;
        Ok(())
    }
}
