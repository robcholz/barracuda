//! Plugin that owns the Gateway-Agent Bridge Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_gateway_agent_component::component::GatewayAgentBridge;
use barracuda_imessage_gateway_plugin::PLUGIN_ID as IMESSAGE_GATEWAY_PLUGIN_ID;
use barracuda_imessage_web_plugin::{IMessageWebRoute, PLUGIN_ID as IMESSAGE_WEB_PLUGIN_ID};
use barracuda_plugin_manager::{Plugin, PluginContext, PluginStartFuture};

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
    const DEPENDS_ON: &'static [&'static str] =
        &["agent", IMESSAGE_GATEWAY_PLUGIN_ID, IMESSAGE_WEB_PLUGIN_ID];

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
            let route = context.require::<IMessageWebRoute>(IMESSAGE_WEB_PLUGIN_ID)?;
            context.load(GatewayAgentBridge::new(route.route()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use alloc::rc::Rc;

    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_imessage_gateway_plugin::GatewayRoute;
    use barracuda_imessage_web_plugin::IMessageWebRoute;
    use barracuda_platform_test::{memory_partition, MemFs};
    use barracuda_plugin_manager::{
        Plugin, PluginContext, PluginId, PluginManager, PluginStartFuture,
    };
    use futures_lite::future::block_on;

    use super::GatewayAgentPlugin;

    struct Dependency(&'static str);

    impl<const M: usize> Plugin<M> for Dependency {
        fn id(&self) -> &'static str {
            self.0
        }

        fn start<'a, Storage>(
            &'a mut self,
            _context: &'a mut PluginContext<'_, M, Storage>,
        ) -> PluginStartFuture<'a>
        where
            Storage: barracuda_plugin_manager::PluginStorage,
        {
            Box::pin(async { Ok(()) })
        }
    }

    struct WebDependency;

    impl<const M: usize> Plugin<M> for WebDependency {
        const DEPENDS_ON: &'static [&'static str] = &["imessage-gateway"];

        fn id(&self) -> &'static str {
            "imessage-web"
        }

        fn start<'a, Storage>(
            &'a mut self,
            context: &'a mut PluginContext<'_, M, Storage>,
        ) -> PluginStartFuture<'a>
        where
            Storage: barracuda_plugin_manager::PluginStorage,
        {
            Box::pin(async move {
                context.provide(Rc::new(IMessageWebRoute::new(GatewayRoute::new(
                    "web",
                    "conversation",
                ))))?;
                Ok(())
            })
        }
    }

    #[test]
    fn plugin_loads_its_bridge_component() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let mut router = EventRouter::new(lanes, MemFs::new(), "workflows").expect("create router");
        let id = PluginId::try_from("gateway-agent").expect("valid Plugin ID");
        let plugin = GatewayAgentPlugin::new();
        assert_eq!(Plugin::<512>::id(&plugin), "gateway-agent");

        block_on(manager.register(&mut router, Dependency("agent")))
            .expect("register Agent dependency");
        block_on(manager.register(&mut router, Dependency("imessage-gateway")))
            .expect("register IMessage Gateway dependency");
        block_on(manager.register(&mut router, WebDependency))
            .expect("register IMessage Web dependency");
        block_on(manager.register(&mut router, plugin)).expect("register Gateway-Agent Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
