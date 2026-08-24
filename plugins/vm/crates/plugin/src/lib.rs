//! Plugin that owns the Lua VM Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use barracuda_plugin_manager::{
    Plugin, PluginContext, PluginError, PluginRegisterFuture, PluginStartFuture,
};
use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_component::VmComponent;

/// Stable identity of the VM Plugin.
pub const PLUGIN_ID: &str = "vm";

/// Registers the built-in Lua package plan and starts the VM Component.
#[derive(Default)]
pub struct VmPlugin {
    builtin_packages: Option<BuiltinPackages>,
}

impl<const M: usize> Plugin<M> for VmPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<'a, Storage>(
        &'a mut self,
        _context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginRegisterFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            self.builtin_packages = Some(BuiltinPackages::all());
            Ok(())
        })
    }

    fn start<'a, Storage>(
        &'a mut self,
        context: &'a mut PluginContext<'_, M, Storage>,
    ) -> PluginStartFuture<'a>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        Box::pin(async move {
            let builtin_packages = self
                .builtin_packages
                .take()
                .ok_or_else(|| PluginError::registration(BuiltinPackagesNotRegistered))?;
            context.load(VmComponent::new(builtin_packages))?;
            Ok(())
        })
    }
}

#[derive(Debug)]
struct BuiltinPackagesNotRegistered;

impl core::fmt::Display for BuiltinPackagesNotRegistered {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("VM builtin packages were not registered")
    }
}

impl core::error::Error for BuiltinPackagesNotRegistered {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;

    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{memory_partition, MemFs};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use super::VmPlugin;

    #[test]
    fn plugin_loads_its_vm_component() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let filesystem = MemFs::new();
        let mut router = EventRouter::new(lanes, filesystem, "workflows").expect("create router");
        let id = PluginId::try_from("vm").expect("valid Plugin ID");
        let plugin = VmPlugin::default();
        assert_eq!(Plugin::<512>::id(&plugin), "vm");

        block_on(manager.register(&mut router, plugin)).expect("register VM Plugin");
        block_on(manager.start(&mut router)).expect("start Plugins");

        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }
}
