//! Plugin that owns the Lua VM Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_plugin_manager::{Plugin, PluginContext, PluginStartFuture};
use barracuda_vm_component::VmComponent;

/// Stable identity of the VM Plugin.
pub const PLUGIN_ID: &str = "vm";

/// Registers the Lua VM Component.
#[derive(Default)]
pub struct VmPlugin;

impl<const M: usize> Plugin<M> for VmPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn start<'a>(&'a mut self, context: &'a mut PluginContext<'_, M>) -> PluginStartFuture<'a> {
        Box::pin(async move {
            context.load(VmComponent::default())?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;

    use barracuda_event_router::{EventRouter, MemFs, RpcLaneStorage};
    use barracuda_plugin_manager::{EkvStore, NoopRawMutex, Plugin, PluginId, PluginManager};
    use ekv::{flash::MemFlash, Config};
    use futures_lite::future::block_on;

    use super::VmPlugin;

    #[test]
    fn plugin_loads_its_vm_component() {
        let store = EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), Config::default());
        block_on(store.format()).expect("format store");
        let mut manager = PluginManager::new(store);
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let filesystem = Box::leak(Box::new(MemFs::new()));
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        let id = PluginId::try_from("vm").expect("valid Plugin ID");
        let plugin = VmPlugin;
        assert_eq!(Plugin::<512>::id(&plugin), "vm");

        block_on(manager.register(&mut router, plugin)).expect("register VM Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
