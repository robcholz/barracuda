//! Opt-in Plugin filesystem namespace behavior.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use barracuda_kv::MAX_CAPACITY;
use barracuda_platform_test::{memory_partition, MemoryPartition};
use barracuda_plugin_manager::{
    Plugin, PluginDeclaration, PluginError, PluginFilesystem, PluginManager, PluginRegisterContext,
    PluginRequirements, PluginResult, PluginStorage,
};
use barracuda_vfs::{FsError, MountOptions, OpenOptions, ScopedVfs, Vfs};
use barracuda_vfs_memfs::MemFs;
use futures_lite::future::block_on;

async fn manager() -> PluginManager<MemoryPartition> {
    let partition = memory_partition(MAX_CAPACITY)
        .await
        .expect("create database partition");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    let mut filesystem = Vfs::new();
    filesystem
        .mount(
            "/data",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .expect("mount Plugin data volume");
    let resources = MemFs::new();
    resources
        .create_dir_all("/plugins/first")
        .expect("create Plugin resource directory");
    resources
        .write_file("/plugins/first/config.json", br#"{"enabled":true}"#)
        .expect("write Plugin resource");
    filesystem
        .mount(
            "/resources",
            resources.into_backend(),
            MountOptions::read_only(),
        )
        .await
        .expect("mount Plugin resources volume");
    manager.install_vfs(filesystem);
    manager
}

struct FilesystemPlugin<const KIND: u8> {
    filesystem: Rc<RefCell<Option<ScopedVfs>>>,
}

impl<const KIND: u8> PluginDeclaration for FilesystemPlugin<KIND> {
    const ID: &'static str = if KIND == 0 { "first" } else { "second" };
}

impl<const KIND: u8> Plugin for FilesystemPlugin<KIND> {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
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

impl Plugin for KvOnlyPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
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
        let first = Rc::new(RefCell::new(None));
        let second = Rc::new(RefCell::new(None));

        manager
            .register(FilesystemPlugin::<0> {
                filesystem: Rc::clone(&first),
            })
            .unwrap();
        manager
            .register(FilesystemPlugin::<1> {
                filesystem: Rc::clone(&second),
            })
            .unwrap();

        let first = first.borrow().clone().unwrap();
        let second = second.borrow().clone().unwrap();
        let mut create = OpenOptions::new();
        create.write(true).create(true);
        drop(
            first
                .open_with("/data/open-created", &create)
                .await
                .unwrap(),
        );
        first.write("/data/state", b"first").await.unwrap();
        second.write("/data/state", b"second").await.unwrap();

        assert_eq!(first.read("/data/state").await.unwrap(), b"first");
        assert_eq!(second.read("/data/state").await.unwrap(), b"second");
        assert_eq!(
            first.read("/resources/config.json").await.unwrap(),
            br#"{"enabled":true}"#
        );
        assert_eq!(
            first.write("/resources/config.json", b"{}").await,
            Err(FsError::ReadOnly)
        );
        assert_eq!(
            first.rename("/data/state", "/resources/state").await,
            Err(FsError::CrossMount)
        );
        assert_eq!(first.read("/cache/file").await, Err(FsError::NotMounted));
        assert_eq!(first.read("/media/file").await, Err(FsError::NotMounted));
        assert_eq!(first.read("/state").await, Err(FsError::NotMounted));
        assert_eq!(
            first.write("/state", b"root").await,
            Err(FsError::NotMounted)
        );
        assert!(first.read("../second/data/state").await.is_err());
    });
}

#[test]
fn kv_is_always_available_but_filesystem_requires_declaration() {
    block_on(async {
        let mut manager = manager().await;
        let filesystem_rejected = Rc::new(Cell::new(false));

        manager
            .register(KvOnlyPlugin {
                filesystem_rejected: Rc::clone(&filesystem_rejected),
            })
            .unwrap();

        assert!(filesystem_rejected.get());
    });
}
