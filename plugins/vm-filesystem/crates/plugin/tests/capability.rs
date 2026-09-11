//! VM file transfer capability integration test.

#![allow(clippy::expect_used)]

use std::cell::Cell;
use std::rc::Rc;

use barracuda_platform_test::{install_global_memory_vfs, memory_partition};
use barracuda_plugin::manager::{
    Plugin, PluginDeclaration, PluginManager, PluginRegisterContext, PluginResult,
};
use barracuda_vm_filesystem_plugin::{VmFileTransfer, VmFilesystemPlugin};
use barracuda_vm_plugin::LuaPackageRegistry;
use futures_lite::future::block_on;

struct VmProvider;

impl PluginDeclaration for VmProvider {
    const ID: &'static str = "vm";
}

impl Plugin for VmProvider {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        context.provide(Rc::new(LuaPackageRegistry::new()))
    }
}

struct Consumer {
    observed: Rc<Cell<bool>>,
}

impl PluginDeclaration for Consumer {
    const ID: &'static str = "consumer";
    const DEPENDS_ON: &'static [&'static str] = &["vm-filesystem"];
}

impl Plugin for Consumer {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let _transfer = context.require::<VmFileTransfer>("vm-filesystem")?;
        self.observed.set(true);
        Ok(())
    }
}

#[test]
fn plugin_provides_file_transfer_only_to_a_declared_dependent() {
    block_on(async {
        install_global_memory_vfs()
            .await
            .expect("install global test VFS");
        let partition = memory_partition(64 * 1024)
            .await
            .expect("create database partition");
        let mut manager = PluginManager::open(partition)
            .await
            .expect("open Plugin storage");
        manager.install_vfs(barracuda_vfs::global_namespace().await);
        let observed = Rc::new(Cell::new(false));

        manager.register(VmProvider).expect("register VM provider");
        manager
            .register(VmFilesystemPlugin)
            .expect("register VM Filesystem Plugin");
        manager
            .register(Consumer {
                observed: Rc::clone(&observed),
            })
            .expect("register capability consumer");

        assert!(observed.get());
    });
}
