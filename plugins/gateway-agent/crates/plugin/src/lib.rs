//! Plugin that owns the Gateway-Agent Bridge Component.

#![no_std]

extern crate alloc;

use barracuda_gateway_agent_component::component::GatewayAgentBridge;
use barracuda_imessage_gateway_plugin::PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID;
use barracuda_plugin_manager::{Plugin, PluginContext, PluginResult};

/// Stable identity of the Gateway-Agent Plugin.
pub const PLUGIN_ID: &str = "gateway-agent";

/// Plugin that owns the Gateway-Agent Bridge Component.
#[derive(Default)]
pub struct GatewayAgentPlugin;

impl GatewayAgentPlugin {
    /// Creates the self-contained Gateway-Agent Plugin.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl<const M: usize> Plugin<M> for GatewayAgentPlugin {
    const DEPENDS_ON: &'static [&'static str] = &["agent", IMESSAGE_GATEWAY_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
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
    use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
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
            let plugin = GatewayAgentPlugin::new();
            assert_eq!(Plugin::<512>::id(&plugin), "gateway-agent");

            manager
                .register(&mut router, Dependency("agent"))
                .expect("register Agent dependency");
            manager
                .register(&mut router, IMessageGatewayPlugin::new())
                .expect("register IMessage Gateway dependency");
            manager
                .register(&mut router, plugin)
                .expect("register Gateway-Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
