//! Process-wide VFS behavior.

use barracuda_vfs::{
    create, create_dir_all, global_namespace, metadata, mount, open, read, read_dir, remove_dir,
    remove_file, rename, unmount, write, MountOptions,
};
use barracuda_vfs_memfs::MemFs;
use embedded_io_async::{Read, Write};

const ROOT: &str = "/__barracuda_vfs_global_test";

#[test]
fn global_namespace_has_std_fs_style_operations() {
    embassy_futures::block_on(async {
        mount(
            ROOT,
            MemFs::new().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();
        create_dir_all("/__barracuda_vfs_global_test/assets")
            .await
            .unwrap();
        write("/__barracuda_vfs_global_test/assets/index.html", b"index")
            .await
            .unwrap();

        let mut created = create("/__barracuda_vfs_global_test/assets/app.js")
            .await
            .unwrap();
        created.write_all(b"app").await.unwrap();
        created.flush().await.unwrap();
        drop(created);

        let mut opened = open("/__barracuda_vfs_global_test/assets/app.js")
            .await
            .unwrap();
        let mut bytes = [0; 3];
        opened.read_exact(&mut bytes).await.unwrap();
        drop(opened);

        assert_eq!(&bytes, b"app");
        assert_eq!(
            read("/__barracuda_vfs_global_test/assets/index.html")
                .await
                .unwrap(),
            b"index"
        );
        assert!(metadata("/__barracuda_vfs_global_test/assets/app.js")
            .await
            .unwrap()
            .is_file());
        assert_eq!(
            read_dir("/__barracuda_vfs_global_test/assets")
                .await
                .unwrap()
                .count(),
            2
        );
        let scoped = global_namespace().await.scoped(ROOT).unwrap();
        assert_eq!(scoped.read("/assets/index.html").await.unwrap(), b"index");

        rename(
            "/__barracuda_vfs_global_test/assets/app.js",
            "/__barracuda_vfs_global_test/assets/main.js",
        )
        .await
        .unwrap();
        remove_file("/__barracuda_vfs_global_test/assets/main.js")
            .await
            .unwrap();
        remove_file("/__barracuda_vfs_global_test/assets/index.html")
            .await
            .unwrap();
        remove_dir("/__barracuda_vfs_global_test/assets")
            .await
            .unwrap();
        unmount(ROOT).await.unwrap();
    });
}
