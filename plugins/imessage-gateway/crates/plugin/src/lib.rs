//! Base IMessage Gateway Plugin and its provider-registration capability.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;

use barracuda_imessage_gateway_component::component::{GatewayComponent, GatewayIngress};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};
use gateway::{GatewayError, MessageChannel, MessageChannelRegistration, MessageGateway};

pub use barracuda_imessage_gateway_component::component::GatewayIngressError;
pub use barracuda_imessage_gateway_component::gateway_message_received::GatewayInboundMessage;
pub use barracuda_imessage_gateway_component::route::GatewayRoute;

/// Stable identity of the IMessage Gateway Plugin.
pub const PLUGIN_ID: &str = "imessage-gateway";

const GATEWAY_INGRESS_CAPACITY: usize = 16;

/// Typed capability used by IMessage provider Plugins.
///
/// Providers require this capability during registration, register one channel,
/// and retain the returned guard for their Plugin lifetime. Inbound providers
/// also publish normalized messages through the same capability.
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
pub struct IMessageGatewayPlugin;

impl IMessageGatewayPlugin {
    /// Creates the base IMessage Gateway Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for IMessageGatewayPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let gateway = Rc::new(MessageGateway::new());
        let (component, ingress) =
            GatewayComponent::new(gateway.as_ref().clone(), GATEWAY_INGRESS_CAPACITY);
        context.event_router.load(component)?;
        context.provide(Rc::new(IMessageGateway { gateway, ingress }))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::IMessageGatewayPlugin;
    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack,
    };
    use barracuda_plugin_api::{ClientFactory, PluginContext};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    #[test]
    fn plugin_loads_the_shared_gateway_component() {
        block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create database partition");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            install_global_memory_vfs()
                .await
                .expect("install global test VFS");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<8, 512, 8>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let id = PluginId::try_from("imessage-gateway").expect("valid Plugin ID");
            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            let plugin = IMessageGatewayPlugin::new(&mut context);
            assert_eq!(Plugin::<512>::id(&plugin), "imessage-gateway");

            manager
                .register(&mut router, plugin)
                .expect("register IMessage Gateway Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
