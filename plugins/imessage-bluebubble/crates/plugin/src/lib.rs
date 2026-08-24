//! IMessage BlueBubbles provider Plugin.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;

use barracuda_imessage_gateway_plugin::{IMessageGateway, PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginError, PluginResult};
use bluebubbles::{BlueBubbles, BlueBubblesConfig};
use gateway::MessageChannel;
use http_client::HttpClient;

/// Stable identity of the IMessage BlueBubble Plugin.
pub const PLUGIN_ID: &str = "imessage-bluebubble";

/// Plugin that registers a BlueBubbles channel with the IMessage Gateway.
pub struct IMessageBlueBubblePlugin {
    channel: Rc<BlueBubbles>,
}

impl IMessageBlueBubblePlugin {
    /// Creates a BlueBubbles provider from its portable HTTP client and settings.
    #[must_use]
    pub fn new(http: Rc<dyn HttpClient>, config: BlueBubblesConfig) -> Self {
        Self {
            channel: Rc::new(BlueBubbles::new(http, config)),
        }
    }
}

impl<const M: usize> Plugin<M> for IMessageBlueBubblePlugin {
    const DEPENDS_ON: &'static [&'static str] = &[IMESSAGE_GATEWAY_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let gateway = context.require::<IMessageGateway>(IMESSAGE_GATEWAY_PLUGIN_ID)?;
        let channel: Rc<dyn MessageChannel> = self.channel.clone();
        let registration = gateway
            .register(channel)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        Ok(())
    }
}
