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
fn open_modes_enforce_permissions_append_and_create_new_semantics() {
    embassy_futures::block_on(async {
        let vfs = mounted("/").await;
        vfs.write("/record", b"abc").await.unwrap();

        let mut read_only = vfs.open("/record").await.unwrap();
        assert_eq!(read_only.write(b"x").await, Err(FsError::PermissionDenied));

        let mut write_only_options = OpenOptions::new();
        write_only_options.write(true);
        let mut write_only = vfs.open_with("/record", &write_only_options).await.unwrap();
        let mut byte = [0];
        assert_eq!(
            write_only.read(&mut byte).await,
            Err(FsError::PermissionDenied)
        );

        let mut append_options = OpenOptions::new();
        append_options.write(true).append(true);
        let mut first = vfs.open_with("/record", &append_options).await.unwrap();
        first.seek(SeekFrom::Start(0)).await.unwrap();
        first.write_all(b"def").await.unwrap();
        drop(first);
        let mut second = vfs.open_with("/record", &append_options).await.unwrap();
        second.write_all(b"ghi").await.unwrap();
        drop(second);
        assert_eq!(vfs.read("/record").await.unwrap(), b"abcdefghi");

        let mut create_new = OpenOptions::new();
        create_new.write(true).create_new(true);
        assert_eq!(
            vfs.open_with("/record", &create_new).await.unwrap_err(),
            FsError::AlreadyExists
        );
        assert_eq!(vfs.open("/missing").await.unwrap_err(), FsError::NotFound);
        assert_eq!(vfs.open("/").await.unwrap_err(), FsError::IsDirectory);

        let mut seekable = vfs.open("/record").await.unwrap();
        assert_eq!(
            seekable.seek(SeekFrom::Current(-1)).await,
            Err(FsError::InvalidInput)
        );
        assert_eq!(
            seekable.seek(SeekFrom::End(-20)).await,
            Err(FsError::InvalidInput)
        );
        assert_eq!(seekable.seek(SeekFrom::End(-3)).await.unwrap(), 6);
        let mut tail = [0; 3];
        seekable.read_exact(&mut tail).await.unwrap();
        assert_eq!(&tail, b"ghi");
        assert_eq!(seekable.read(&mut byte).await.unwrap(), 0);
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

#[test]
fn scoped_vfs_handles_root_paths_explicit_options_and_directory_removal() {
    embassy_futures::block_on(async {
        let vfs = mounted("/").await;
        let scope = vfs.scoped("/tenant").unwrap();
        assert!(scope.metadata("/").await.unwrap().is_dir());

        let mut created = scope.create("nested/file").await.unwrap();
        created.write_all(b"abc").await.unwrap();
        created.flush().await.unwrap();
        drop(created);

        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let mut opened = scope.open_with("/nested/file", &options).await.unwrap();
        opened.seek(SeekFrom::End(0)).await.unwrap();
        opened.write_all(b"def").await.unwrap();
        drop(opened);
        scope.rename("nested/file", "nested/renamed").await.unwrap();
        assert_eq!(scope.read("nested/renamed").await.unwrap(), b"abcdef");
        assert_eq!(
            scope.read_at("nested/renamed", 4, 3).await,
            Err(FsError::Io)
        );

        scope.remove("nested/renamed").await.unwrap();
        scope.remove("nested").await.unwrap();
        assert!(!scope.exists("nested").await.unwrap());
    });
}
