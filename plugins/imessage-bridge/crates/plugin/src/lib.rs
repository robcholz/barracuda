//! Plugin entry point for the iMessage Agent bridge.

#![no_std]

extern crate alloc;

use barracuda_imessage_bridge_component::ImessageBridgeComponent;
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};

/// Plugin that owns the persistent iMessage-to-Agent Workflow bridge.
#[barracuda_plugin_api::plugin]
pub struct ImessageBridgePlugin;

impl ImessageBridgePlugin {
    /// Creates the Plugin from the shared construction context.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for ImessageBridgePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let component =
            embassy_futures::block_on(ImessageBridgeComponent::load(context.storage().clone()))
                .map_err(PluginError::registration)?;
        context.event_router.load(component)?;
        Ok(())
    }
}
