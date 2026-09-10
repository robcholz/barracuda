//! Core VFS behavior exercised through the in-memory backend.

use barracuda_vfs::{FsError, MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_memfs::MemFs;
use embedded_io_async::{Read, Seek, Write};

async fn mounted(path: &str) -> Vfs {
    let vfs = Vfs::new();
    vfs.mount(
        path,
        MemFs::new().into_backend(),
        MountOptions::read_write(),
    )
    .await
    .unwrap();
    vfs
}

#[test]
fn mounted_backend_provides_normal_file_handles() {
    embassy_futures::block_on(async {
        let vfs = mounted("/data").await;
        vfs.create_dir_all("/data/logs").await.unwrap();

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        let mut file = vfs
            .open_with("/data/logs/boot.txt", &options)
            .await
            .unwrap();
        file.write_all(b"booted").await.unwrap();
        file.seek(SeekFrom::Start(0)).await.unwrap();

        let mut bytes = [0; 6];
        file.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"booted");
    });
}

#[test]
fn longest_mount_point_wins() {
    embassy_futures::block_on(async {
        let root = MemFs::new();
        let nested = MemFs::new();
        root.write_file("/marker", b"root").unwrap();
        nested.write_file("/marker", b"nested").unwrap();

        let vfs = Vfs::new();
        vfs.mount("/data", root.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.mount(
            "/data/cache",
            nested.into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();

        assert_eq!(vfs.read("/data/marker").await.unwrap(), b"root");
        assert_eq!(vfs.read("/data/cache/marker").await.unwrap(), b"nested");
    });
}

#[test]
fn read_only_mount_rejects_every_mutation() {
    embassy_futures::block_on(async {
        let memory = MemFs::new();
        memory.write_file("/asset.txt", b"asset").unwrap();

        let vfs = Vfs::new();
        vfs.mount("/web", memory.into_backend(), MountOptions::read_only())
            .await
            .unwrap();

        assert_eq!(vfs.read("/web/asset.txt").await.unwrap(), b"asset");
        assert_eq!(
            vfs.create("/web/new.txt").await.unwrap_err(),
            FsError::ReadOnly
        );
        assert_eq!(
            vfs.remove_file("/web/asset.txt").await.unwrap_err(),
            FsError::ReadOnly
        );
    });
}

#[test]
fn scoped_mounts_share_storage_without_sharing_namespace() {
    embassy_futures::block_on(async {
        let storage = MemFs::new();
        storage.create_dir_all("/sandboxes/a").unwrap();
        storage.create_dir_all("/sandboxes/b").unwrap();
        storage.write_file("/sandboxes/a/id", b"a").unwrap();
        storage.write_file("/sandboxes/b/id", b"b").unwrap();
        let backend = storage.into_backend();

        let a = Vfs::new();
        a.mount_scoped(
            "/workspace",
            backend.clone(),
            "/sandboxes/a",
            MountOptions::read_write(),
        )
        .await
        .unwrap();

        let b = Vfs::new();
        b.mount_scoped(
            "/workspace",
            backend,
            "/sandboxes/b",
            MountOptions::read_write(),
        )
        .await
        .unwrap();

        assert_eq!(a.read("/workspace/id").await.unwrap(), b"a");
        assert_eq!(b.read("/workspace/id").await.unwrap(), b"b");
        assert_eq!(
            a.metadata("/sandboxes/b/id").await.unwrap_err(),
            FsError::NotMounted
        );
    });
}

#[test]
fn paths_cannot_escape_the_vfs_root() {
    embassy_futures::block_on(async {
        let vfs = mounted("/").await;
        assert_eq!(
            vfs.open("/../../secret").await.unwrap_err(),
            FsError::InvalidPath
        );
        assert_eq!(
            vfs.open("relative").await.unwrap_err(),
            FsError::InvalidPath
        );
    });
}

#[test]
fn rename_cannot_cross_mounts() {
    embassy_futures::block_on(async {
        let vfs = Vfs::new();
        vfs.mount(
            "/a",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();
        vfs.mount(
            "/b",
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();
        vfs.create("/a/file").await.unwrap();

        assert_eq!(
            vfs.rename("/a/file", "/b/file").await.unwrap_err(),
            FsError::CrossMount
        );
    });
}

#[test]
fn mount_cannot_be_removed_while_a_file_is_open() {
    embassy_futures::block_on(async {
        let vfs = mounted("/data").await;
        let file = vfs.create("/data/file").await.unwrap();

        assert_eq!(vfs.unmount("/data").await.unwrap_err(), FsError::Busy);
        drop(file);
        vfs.unmount("/data").await.unwrap();
        assert_eq!(
            vfs.metadata("/data/file").await.unwrap_err(),
            FsError::NotMounted
        );
    });
}

#[test]
fn cloned_and_scoped_namespaces_observe_live_mount_changes() {
    embassy_futures::block_on(async {
        let root = MemFs::new();
        root.create_dir_all("/workspace/removable").unwrap();
        let vfs = Vfs::new();
        vfs.mount("/", root.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        let clone = vfs.clone();
        let scoped = vfs
            .scoped_mounts([("/workspace/removable", "/workspace/removable")])
            .unwrap();

        let volume = MemFs::new();
        volume.write_file("/identity", b"card").unwrap();
        clone
            .mount(
                "/workspace/removable/micro-sd",
                volume.into_backend(),
                MountOptions::read_write(),
            )
            .await
            .unwrap();

        assert_eq!(
            scoped
                .read("/workspace/removable/micro-sd/identity")
                .await
                .unwrap(),
            b"card"
        );
        assert_eq!(
            scoped.list_dir("/workspace/removable").await.unwrap(),
            ["micro-sd"]
        );

        vfs.unmount("/workspace/removable/micro-sd").await.unwrap();
        assert_eq!(
            scoped
                .metadata("/workspace/removable/micro-sd")
                .await
                .unwrap_err(),
            FsError::NotFound
        );
    });
}

#[test]
fn detached_mount_disappears_while_an_open_file_keeps_its_backend() {
    embassy_futures::block_on(async {
        let vfs = mounted("/data").await;
        vfs.write("/data/file", b"before").await.unwrap();
        let mut file = vfs.open("/data/file").await.unwrap();

        vfs.detach("/data").await.unwrap();
        assert_eq!(
            vfs.metadata("/data/file").await.unwrap_err(),
            FsError::NotMounted
        );

        let mut bytes = [0; 6];
        file.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"before");
    });
}

#[test]
fn scoped_vfs_provides_complete_persistence_operations() {
    embassy_futures::block_on(async {
        let vfs = mounted("/").await;
        let scope = vfs.scoped("/plugins/agent").unwrap();

        scope.create_dir_all("/sessions").await.unwrap();
        scope.write("/sessions/index", b"old").await.unwrap();
        scope
            .write_atomic("/sessions/index", b"first")
            .await
            .unwrap();
        scope.append("/sessions/index", b"-second").await.unwrap();

        assert!(scope.exists("/sessions/index").await.unwrap());
        assert_eq!(scope.len("/sessions/index").await.unwrap(), 12);
        assert_eq!(
            scope.read_at("/sessions/index", 6, 6).await.unwrap(),
            b"second"
        );
        assert_eq!(scope.list_dir("/sessions").await.unwrap(), ["index"]);

        scope.remove("/sessions/index").await.unwrap();
        scope.remove("/sessions/index").await.unwrap();
        assert!(!scope.exists("/sessions/index").await.unwrap());
    });
}

#[test]
fn scoped_vfs_accepts_paths_relative_to_its_private_root() {
    embassy_futures::block_on(async {
        let vfs = mounted("/").await;
        let scope = vfs.scoped("/plugins/agent").unwrap();

        scope
            .write_atomic("skills/demo/SKILL.md", b"demo")
            .await
            .unwrap();

        assert_eq!(scope.read("/skills/demo/SKILL.md").await.unwrap(), b"demo");
        assert_eq!(scope.list_dir("skills/demo").await.unwrap(), ["SKILL.md"]);
        assert_eq!(
            vfs.read("/plugins/agent/skills/demo/SKILL.md")
                .await
                .unwrap(),
            b"demo"
        );
    });
}
