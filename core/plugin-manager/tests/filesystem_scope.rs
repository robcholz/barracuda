//! Opt-in Plugin filesystem namespace behavior.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::boxed::Box;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use barracuda_event_router::{EventRouter, RpcLaneStorage};
use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{install_global_memory_vfs, memory_partition, MemoryPartition};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginManager, PluginRegisterContext,
    PluginRequirements, PluginResult, PluginStorage, PluginVfs,
};
use barracuda_vfs::{MountOptions, Vfs};
use futures_lite::future::block_on;

const FRAME_SIZE: usize = 64;

async fn router() -> EventRouter<8, FRAME_SIZE, 8> {
    install_global_memory_vfs()
        .await
        .expect("install global test VFS");
    let lanes = Box::leak(Box::new(RpcLaneStorage::new()));
    EventRouter::new(lanes).await.expect("create Event Router")
}

async fn manager() -> PluginManager<FRAME_SIZE, MemoryPartition> {
    let partition = memory_partition(MAX_CAPACITY)
        .await
        .expect("create database partition");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    let mut filesystem = Vfs::new();
    filesystem
        .mount(
            "/",
            barracuda_vfs_memfs::MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .expect("mount System filesystem");
    manager.install_vfs(filesystem);
    manager
}

struct FilesystemPlugin<const KIND: u8> {
    filesystem: Rc<RefCell<Option<PluginVfs>>>,
}

impl<const KIND: u8> PluginDeclaration for FilesystemPlugin<KIND> {
    const ID: &'static str = if KIND == 0 { "first" } else { "second" };
}

impl<const KIND: u8> Plugin<FRAME_SIZE> for FilesystemPlugin<KIND> {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        *self.filesystem.borrow_mut() = Some(context.filesystem()?.clone());
        Ok(())
    }
}

struct KvOnlyPlugin {
    filesystem_rejected: Rc<Cell<bool>>,
}

impl PluginDeclaration for KvOnlyPlugin {
    const ID: &'static str = "kv-only";
}

impl Plugin<FRAME_SIZE> for KvOnlyPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, FRAME_SIZE, Storage>,
    ) -> PluginResult<()>
    where
        Storage: PluginStorage,
    {
        self.filesystem_rejected.set(matches!(
            context.filesystem(),
            Err(PluginError::FilesystemNotDeclared)
        ));
        block_on(context.storage().put("state", &7_u32))?;
        Ok(())
    }
}

#[test]
fn declared_plugins_receive_isolated_filesystem_roots() {
    block_on(async {
        let mut manager = manager().await;
        let mut router = router().await;
        let first = Rc::new(RefCell::new(None));
        let second = Rc::new(RefCell::new(None));

        manager
            .register(
                &mut router,
                FilesystemPlugin::<0> {
                    filesystem: Rc::clone(&first),
                },
            )
            .unwrap();
        manager
            .register(
                &mut router,
                FilesystemPlugin::<1> {
                    filesystem: Rc::clone(&second),
                },
            )
            .unwrap();

        let first = first.borrow().clone().unwrap();
        let second = second.borrow().clone().unwrap();
        first.write("/state", b"first").await.unwrap();
        second.write("/state", b"second").await.unwrap();

        assert_eq!(first.read("/state").await.unwrap(), b"first");
        assert_eq!(second.read("/state").await.unwrap(), b"second");
        assert!(first.read("../second/state").await.is_err());
    });
}

#[test]
fn kv_is_always_available_but_filesystem_requires_declaration() {
    block_on(async {
        let mut manager = manager().await;
        let mut router = router().await;
        let filesystem_rejected = Rc::new(Cell::new(false));

        manager
            .register(
                &mut router,
                KvOnlyPlugin {
                    filesystem_rejected: Rc::clone(&filesystem_rejected),
                },
            )
            .unwrap();

        assert!(filesystem_rejected.get());
    });
}
