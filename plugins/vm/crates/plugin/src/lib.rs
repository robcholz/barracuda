//! Plugin that owns the Lua VM Component.

#![no_std]

extern crate alloc;

use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext,
};
use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_component::{VmComponent, VmRuntime};

/// Stable identity of the VM Plugin.
pub const PLUGIN_ID: &str = "vm";

/// Registers the VM Component and starts its owner-managed Embassy runtime.
pub struct VmPlugin {
    runtime: Option<VmRuntime>,
}

impl VmPlugin {
    /// Creates the VM Plugin from the shared construction context.
    #[must_use]
    pub const fn new(_context: &PluginContext) -> Self {
        Self { runtime: None }
    }
}

impl<const M: usize> Plugin<M> for VmPlugin {
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
        let runtime = VmRuntime::new().map_err(PluginError::registration)?;
        context.event_router.load(VmComponent::with_runtime(
            BuiltinPackages::all(),
            runtime.clone(),
        ))?;
        self.runtime = Some(runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| PluginError::registration(VmRuntimeUnavailable))?;
        runtime
            .start(context.task_spawner()?)
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("VM runtime was not prepared during Plugin registration")]
struct VmRuntimeUnavailable;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    extern crate std;

    use alloc::boxed::Box;
    use alloc::string::{String, ToString};
    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::time::Duration;

    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, never_embassy_stack,
    };
    use barracuda_plugin_api::{ClientFactory, PluginContext};
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager, PluginStartError};
    use embassy_executor::{Executor, Spawner};
    use futures_lite::future::block_on;

    use super::VmPlugin;

    fn plugin_context() -> PluginContext {
        let stack = never_embassy_stack();
        PluginContext::new(stack, ClientFactory::plaintext(stack))
    }

    #[test]
    fn plugin_loads_its_vm_component() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        block_on(install_global_memory_vfs()).expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let mut router = block_on(EventRouter::new(lanes)).expect("create router");
        let id = PluginId::try_from("vm").expect("valid Plugin ID");
        let plugin = VmPlugin::new(&plugin_context());
        assert_eq!(Plugin::<512>::id(&plugin), "vm");

        manager
            .register(&mut router, plugin)
            .expect("register VM Plugin");
        assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
    }

    #[test]
    fn plugin_requires_an_embassy_spawner_during_startup() {
        let partition = block_on(memory_partition(64 * 1024)).expect("create database partition");
        let mut manager = block_on(PluginManager::open(partition)).expect("open Plugin storage");
        block_on(install_global_memory_vfs()).expect("install global test VFS");
        let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
        let mut router = block_on(EventRouter::new(lanes)).expect("create router");
        manager
            .register(&mut router, VmPlugin::new(&plugin_context()))
            .expect("register VM Plugin");

        let error = manager
            .start(&mut router)
            .expect_err("missing task spawner must fail");

        assert!(matches!(error, PluginStartError::Start(_)));
    }

    #[embassy_executor::task]
    async fn start_plugin(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
        let result = async {
            let partition = memory_partition(64 * 1024)
                .await
                .map_err(|error| error.to_string())?;
            let mut manager = PluginManager::open(partition)
                .await
                .map_err(|error| error.to_string())?;
            install_global_memory_vfs()
                .await
                .map_err(|error| error.to_string())?;
            let lanes = Box::leak(Box::new(RpcLaneStorage::<4, 512, 4>::new()));
            let mut router = EventRouter::new(lanes)
                .await
                .map_err(|error| error.to_string())?;
            manager.install_task_spawner(spawner);
            manager
                .register(&mut router, VmPlugin::new(&plugin_context()))
                .map_err(|error| error.to_string())?;
            manager
                .start(&mut router)
                .map_err(|error| error.to_string())
        }
        .await;
        let _result = completed.send(result);
    }

    #[test]
    fn plugin_starts_the_vm_embassy_runtime() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner
                    .spawn(start_plugin(spawner, completed))
                    .expect("spawn VM Plugin test");
            });
        });

        result
            .recv_timeout(Duration::from_secs(5))
            .expect("VM Plugin startup timed out")
            .expect("VM Plugin startup failed");
    }
}
