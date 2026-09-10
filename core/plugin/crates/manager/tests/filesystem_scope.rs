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

async fn manager_with_vfs() -> (PluginManager<MemoryPartition>, Vfs) {
    let partition = memory_partition(MAX_CAPACITY)
        .await
        .expect("create database partition");
    let mut manager = PluginManager::open(partition)
        .await
        .expect("open Plugin storage");
    let filesystem = Vfs::new();
    let durable = MemFs::new().into_backend();
    filesystem
        .mount("/data", durable.clone(), MountOptions::read_write())
        .await
        .expect("mount Plugin data volume");
    filesystem
        .create_dir_all("/data/media")
        .await
        .expect("create durable media root");
    filesystem
        .mount_scoped("/media", durable, "/media", MountOptions::read_write())
        .await
        .expect("mount media volume");
    let resources = MemFs::new();
    resources
        .create_dir_all("/plugins/first")
        .expect("create Plugin resource directory");
    resources
        .write_file("/plugins/first/config.json", br#"{"enabled":true}"#)
        .expect("write Plugin resource");
    resources
        .create_dir_all("/workspace")
        .expect("create Workspace resource directory");
    resources
        .write_file("/workspace/common.txt", b"shared")
        .expect("write Workspace resource");
    filesystem
        .mount(
            "/resources",
            resources.into_backend(),
            MountOptions::read_only(),
        )
        .await
        .expect("mount Plugin resources volume");
    filesystem
        .mount(
            "/cache",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .expect("mount cache volume");
    filesystem
        .mount(
            "/removable",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .expect("mount removable namespace");
    let system_vfs = filesystem.clone();
    manager.install_vfs(filesystem);
    (manager, system_vfs)
}

async fn manager() -> PluginManager<MemoryPartition> {
    manager_with_vfs().await.0
}

#[test]
fn existing_plugin_view_observes_removable_filesystems_mounted_by_system() {
    block_on(async {
        let (mut manager, system_vfs) = manager_with_vfs().await;
        let filesystem = Rc::new(RefCell::new(None));
        manager
            .register(FilesystemPlugin::<0> {
                filesystem: Rc::clone(&filesystem),
            })
            .unwrap();
        let filesystem = filesystem.borrow().clone().unwrap();

        let card = MemFs::new();
        card.write_file("/identity", b"micro-sd").unwrap();
        system_vfs
            .mount(
                "/removable/micro-sd",
                card.into_backend(),
                MountOptions::read_write(),
            )
            .await
            .unwrap();

        assert_eq!(
            filesystem
                .read("/workspace/removable/micro-sd/identity")
                .await
                .unwrap(),
            b"micro-sd"
        );
        assert_eq!(
            filesystem
                .read_dir("/workspace/removable")
                .await
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_owned())
                .collect::<Vec<_>>(),
            ["micro-sd"]
        );

        system_vfs.detach("/removable/micro-sd").await.unwrap();
        assert_eq!(
            filesystem.metadata("/workspace/removable/micro-sd").await,
            Err(FsError::NotFound)
        );
    });
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
        first.write("/cache/private", b"first-cache").await.unwrap();
        second
            .write("/cache/private", b"second-cache")
            .await
            .unwrap();
        assert_eq!(first.read("/cache/private").await.unwrap(), b"first-cache");
        assert_eq!(
            second.read("/cache/private").await.unwrap(),
            b"second-cache"
        );

        assert_eq!(
            first.read("/workspace/resources/common.txt").await.unwrap(),
            b"shared"
        );
        assert_eq!(
            first
                .write("/workspace/resources/common.txt", b"changed")
                .await,
            Err(FsError::ReadOnly)
        );

        first
            .write("/workspace/cache/vm-output", b"temporary")
            .await
            .unwrap();
        assert_eq!(
            second.read("/workspace/cache/vm-output").await.unwrap(),
            b"temporary"
        );
        second
            .write("/workspace/media/report.txt", b"durable")
            .await
            .unwrap();
        assert_eq!(
            first.read("/workspace/media/report.txt").await.unwrap(),
            b"durable"
        );
        assert_eq!(
            first
                .rename("/workspace/cache/vm-output", "/workspace/media/vm-output")
                .await,
            Err(FsError::CrossMount)
        );
        assert_eq!(
            first.read("/workspace/data/state").await,
            Err(FsError::NotMounted)
        );
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
