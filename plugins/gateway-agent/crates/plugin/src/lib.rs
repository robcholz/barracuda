//! Plugin that owns the Gateway-Agent Bridge Component.

#![no_std]

extern crate alloc;

use barracuda_gateway_agent_component::component::GatewayAgentBridge;
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{Plugin, PluginRegisterContext, PluginResult};

/// Plugin that owns the Gateway-Agent Bridge Component.
pub struct GatewayAgentPlugin;

impl GatewayAgentPlugin {
    /// Creates the self-contained Gateway-Agent Plugin.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

#[barracuda_plugin_api::plugin]
impl<const M: usize> Plugin<M> for GatewayAgentPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        context.event_router.load(GatewayAgentBridge::new())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack,
    };
    use barracuda_plugin_api::{ClientFactory, PluginContext};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use super::GatewayAgentPlugin;

    struct Dependency(&'static str);

    impl<const M: usize> Plugin<M> for Dependency {
        fn id(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn plugin_loads_its_bridge_component() {
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
            let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let id = PluginId::try_from("gateway-agent").expect("valid Plugin ID");
            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            let plugin = GatewayAgentPlugin::new(&mut context);
            assert_eq!(Plugin::<512>::id(&plugin), "gateway-agent");

            manager
                .register(&mut router, Dependency("agent"))
                .expect("register Agent dependency");
            manager
                .register(&mut router, IMessageGatewayPlugin::new(&mut context))
                .expect("register IMessage Gateway dependency");
            manager
                .register(&mut router, plugin)
                .expect("register Gateway-Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
