//! Core VFS behavior exercised through the in-memory backend.

use barracuda_vfs::{FsError, MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_memfs::MemFs;
use embedded_io_async::{Read, Seek, Write};

async fn mounted(path: &str) -> Vfs {
    let mut vfs = Vfs::new();
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

        let mut vfs = Vfs::new();
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

        let mut vfs = Vfs::new();
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

        let mut a = Vfs::new();
        a.mount_scoped(
            "/workspace",
            backend.clone(),
            "/sandboxes/a",
            MountOptions::read_write(),
        )
        .await
        .unwrap();

        let mut b = Vfs::new();
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
        let mut vfs = Vfs::new();
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
        let mut vfs = mounted("/data").await;
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
