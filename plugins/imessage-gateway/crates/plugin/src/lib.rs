//! Base IMessage Gateway Plugin and its provider-registration capability.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;

use barracuda_imessage_gateway_component::component::{GatewayComponent, GatewayIngress};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginStartFuture};
use gateway::{GatewayError, MessageChannel, MessageChannelRegistration, MessageGateway};

pub use barracuda_imessage_gateway_component::component::GatewayIngressError;
pub use barracuda_imessage_gateway_component::gateway_message_received::GatewayInboundMessage;
pub use barracuda_imessage_gateway_component::route::GatewayRoute;

/// Stable identity of the IMessage Gateway Plugin.
pub const PLUGIN_ID: &str = "imessage-gateway";

const GATEWAY_INGRESS_CAPACITY: usize = 16;

/// Typed capability used by IMessage provider Plugins.
///
/// Providers require this capability during startup, register one channel, and
/// retain the returned guard for their Plugin lifetime. Inbound providers also
/// publish normalized messages through the same capability.
pub struct IMessageGateway {
    gateway: Rc<MessageGateway>,
    ingress: GatewayIngress,
}

impl IMessageGateway {
    /// Registers one outbound message channel.
    ///
    /// Dropping the returned guard unregisters the channel.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayError::DuplicateChannel`] when another provider owns
    /// the same stable channel name.
    pub fn register(
        &self,
        channel: Rc<dyn MessageChannel>,
    ) -> Result<MessageChannelRegistration, GatewayError> {
        self.gateway.register(channel)
    }

    /// Publishes one normalized inbound message into the Gateway Component.
    ///
    /// # Errors
    ///
    /// Returns [`GatewayIngressError::Stopped`] after the Gateway Component
    /// stops accepting messages.
    pub async fn publish(&self, message: GatewayInboundMessage) -> Result<(), GatewayIngressError> {
        self.ingress.publish(message).await
    }
}

/// Plugin that owns the shared IMessage Gateway Component and capability.
#[derive(Default)]
pub struct IMessageGatewayPlugin;

impl IMessageGatewayPlugin {
    /// Creates the base IMessage Gateway Plugin.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for IMessageGatewayPlugin {
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
            let gateway = Rc::new(MessageGateway::new());
            let (component, ingress) =
                GatewayComponent::new(gateway.as_ref().clone(), GATEWAY_INGRESS_CAPACITY);
            context.load(component)?;
            context.provide(Rc::new(IMessageGateway { gateway, ingress }))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::IMessageGatewayPlugin;
    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{memory_partition, MemFs};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    #[test]
    fn plugin_loads_the_shared_gateway_component() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
        let mut router = EventRouter::new(lanes, MemFs::new(), "workflows").expect("create router");
        let id = PluginId::try_from("imessage-gateway").expect("valid Plugin ID");
        let plugin = IMessageGatewayPlugin::new();
        assert_eq!(Plugin::<512>::id(&plugin), "imessage-gateway");

        block_on(manager.register(&mut router, plugin)).expect("register IMessage Gateway Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
