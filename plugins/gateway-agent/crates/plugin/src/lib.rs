//! Plugin that owns the Gateway-Agent Bridge Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_gateway_agent_component::component::GatewayAgentBridge;
use barracuda_message_gateway_plugin::{default_route, PLUGIN_ID as MESSAGE_GATEWAY_PLUGIN_ID};
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
    const DEPENDS_ON: &'static [&'static str] = &["agent", MESSAGE_GATEWAY_PLUGIN_ID];

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.load(GatewayAgentBridge::new(default_route()))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;

    use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
    use barracuda_plugin_manager::{
        EkvStore, NoopRawMutex, Plugin, PluginContext, PluginId, PluginManager, PluginStartFuture,
    };
    use ekv::{flash::MemFlash, Config};
    use futures_lite::future::block_on;

    use super::GatewayAgentPlugin;

    struct Dependency(&'static str);

    impl<const M: usize> Plugin<M> for Dependency {
        fn id(&self) -> &'static str {
            self.0
        }

        fn start<'a>(
            &'a mut self,
            _context: &'a mut PluginContext<'_, M>,
        ) -> PluginStartFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn plugin_loads_its_bridge_component() {
        let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
        block_on(store.format()).expect("format store");
        let mut manager = PluginManager::new(store);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        let id = PluginId::try_from("gateway-agent").expect("valid Plugin ID");
        let plugin = GatewayAgentPlugin::new();
        assert_eq!(Plugin::<512>::id(&plugin), "gateway-agent");

        block_on(manager.register(&mut router, Dependency("agent")))
            .expect("register Agent dependency");
        block_on(manager.register(&mut router, Dependency("message-gateway")))
            .expect("register Message Gateway dependency");
        block_on(manager.register(&mut router, plugin)).expect("register Gateway-Agent Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
