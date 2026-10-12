//! MemFS file contents, handles, and tree operations.

use barracuda_vfs::{File, FileType, FsError, MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_memfs::MemFs;
use embedded_io_async::{Read, Seek, Write};

/// MemFS's page size, so sizes and offsets below land on page boundaries.
const PAGE_SIZE: usize = 4096;

async fn mounted() -> Vfs {
    let vfs = Vfs::new();
    vfs.mount("/", MemFs::new().into_backend(), MountOptions::read_write())
        .await
        .unwrap();
    vfs
}

async fn open(vfs: &Vfs, path: &str, configure: impl FnOnce(&mut OpenOptions)) -> File {
    let mut options = OpenOptions::new();
    configure(&mut options);
    vfs.open_with(path, &options).await.unwrap()
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

async fn read_to_end(file: &mut File) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1000];
    loop {
        let read = file.read(&mut chunk).await.unwrap();
        if read == 0 {
            return bytes;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

#[test]
fn small_writes_stream_into_a_file_spanning_many_pages() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        let expected = pattern(5 * PAGE_SIZE + 123);

        let mut file = vfs.create("/stream.bin").await.unwrap();
        for chunk in expected.chunks(100) {
            assert_eq!(file.write(chunk).await.unwrap(), chunk.len());
        }
        file.flush().await.unwrap();
        drop(file);

        assert_eq!(
            vfs.metadata("/stream.bin").await.unwrap().len(),
            expected.len() as u64
        );
        let mut file = vfs.open("/stream.bin").await.unwrap();
        assert_eq!(read_to_end(&mut file).await, expected);

        // one read crossing the boundary between the second and third pages
        let start = 2 * PAGE_SIZE - 10;
        file.seek(SeekFrom::Start(start as u64)).await.unwrap();
        let mut across = [0; 20];
        file.read_exact(&mut across).await.unwrap();
        assert_eq!(&across[..], &expected[start..start + 20]);
        assert_eq!(vfs.read("/stream.bin").await.unwrap(), expected);
    });
}

#[test]
fn a_two_megabyte_file_streams_in_and_out_in_unaligned_chunks() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        let expected = pattern(2 * 1024 * 1024 + 7);

        let mut file = vfs.create("/large.bin").await.unwrap();
        for chunk in expected.chunks(4000) {
            file.write_all(chunk).await.unwrap();
        }
        drop(file);

        let mut file = vfs.open("/large.bin").await.unwrap();
        assert_eq!(
            file.seek(SeekFrom::End(0)).await.unwrap(),
            expected.len() as u64
        );
        file.seek(SeekFrom::Start(0)).await.unwrap();
        assert_eq!(read_to_end(&mut file).await, expected);
    });
}

#[test]
fn overwriting_the_middle_spans_pages_without_changing_the_length() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        let mut expected = vec![b'a'; 3 * PAGE_SIZE];
        vfs.write("/middle.bin", &expected).await.unwrap();

        let start = PAGE_SIZE - 5;
        let patch = vec![b'b'; PAGE_SIZE + 10];
        let mut file = open(&vfs, "/middle.bin", |options| {
            options.read(true).write(true);
        })
        .await;
        file.seek(SeekFrom::Start(start as u64)).await.unwrap();
        file.write_all(&patch).await.unwrap();
        assert_eq!(
            file.seek(SeekFrom::Current(0)).await.unwrap(),
            (start + patch.len()) as u64
        );
        drop(file);

        expected[start..start + patch.len()].copy_from_slice(&patch);
        assert_eq!(vfs.read("/middle.bin").await.unwrap(), expected);
    });
}

#[test]
fn opening_with_truncate_empties_the_file_and_without_it_overwrites_in_place() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.write("/notes.txt", &pattern(2 * PAGE_SIZE))
            .await
            .unwrap();

        let mut file = open(&vfs, "/notes.txt", |options| {
            options.write(true).truncate(true);
        })
        .await;
        assert_eq!(vfs.metadata("/notes.txt").await.unwrap().len(), 0);
        file.write_all(b"short").await.unwrap();
        drop(file);
        assert_eq!(vfs.read("/notes.txt").await.unwrap(), b"short");

        let mut file = open(&vfs, "/notes.txt", |options| {
            options.write(true);
        })
        .await;
        file.write_all(b"SH").await.unwrap();
        drop(file);
        assert_eq!(vfs.read("/notes.txt").await.unwrap(), b"SHort");
    });
}

#[test]
fn append_mode_writes_at_the_end_wherever_the_cursor_is() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.write("/log.txt", b"one").await.unwrap();

        let mut file = open(&vfs, "/log.txt", |options| {
            options.read(true).append(true);
        })
        .await;
        assert_eq!(file.seek(SeekFrom::Current(0)).await.unwrap(), 3);
        file.seek(SeekFrom::Start(0)).await.unwrap();
        file.write_all(b"-two").await.unwrap();
        assert_eq!(file.seek(SeekFrom::Current(0)).await.unwrap(), 7);

        // another writer extends the file; the next append still lands last
        vfs.scoped("/")
            .unwrap()
            .append("/log.txt", b"-three")
            .await
            .unwrap();
        file.write_all(b"-four").await.unwrap();
        drop(file);
        assert_eq!(vfs.read("/log.txt").await.unwrap(), b"one-two-three-four");
    });
}

#[test]
fn seeking_from_the_end_and_the_cursor_uses_the_live_length() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        let contents = pattern(PAGE_SIZE + 10);
        vfs.write("/seek.bin", &contents).await.unwrap();

        let mut file = open(&vfs, "/seek.bin", |options| {
            options.read(true).write(true);
        })
        .await;
        assert_eq!(
            file.seek(SeekFrom::End(-10)).await.unwrap(),
            PAGE_SIZE as u64
        );
        let mut tail = [0; 10];
        file.read_exact(&mut tail).await.unwrap();
        assert_eq!(&tail[..], &contents[PAGE_SIZE..]);
        assert_eq!(
            file.seek(SeekFrom::Current(-4)).await.unwrap(),
            (PAGE_SIZE + 6) as u64
        );
        assert_eq!(
            file.seek(SeekFrom::End(-(contents.len() as i64) - 1))
                .await
                .unwrap_err(),
            FsError::InvalidInput
        );
        assert_eq!(
            file.seek(SeekFrom::Current(i64::MIN)).await.unwrap_err(),
            FsError::InvalidInput
        );

        // a write past the end fills the gap with zeros
        let end = file.seek(SeekFrom::End(PAGE_SIZE as i64)).await.unwrap();
        assert_eq!(file.read(&mut tail).await.unwrap(), 0);
        file.write_all(b"z").await.unwrap();
        drop(file);

        let bytes = vfs.read("/seek.bin").await.unwrap();
        assert_eq!(bytes.len() as u64, end + 1);
        assert_eq!(&bytes[..contents.len()], &contents[..]);
        assert!(bytes[contents.len()..bytes.len() - 1]
            .iter()
            .all(|byte| *byte == 0));
        assert_eq!(bytes.last(), Some(&b'z'));
    });
}

#[test]
fn an_open_handle_keeps_its_file_after_remove_and_rename() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.write("/kept.txt", b"before").await.unwrap();
        let mut removed = open(&vfs, "/kept.txt", |options| {
            options.read(true).write(true);
        })
        .await;

        vfs.remove_file("/kept.txt").await.unwrap();
        assert_eq!(
            vfs.metadata("/kept.txt").await.unwrap_err(),
            FsError::NotFound
        );
        vfs.write("/kept.txt", b"replacement").await.unwrap();
        removed.seek(SeekFrom::End(0)).await.unwrap();
        removed.write_all(b"+after").await.unwrap();
        removed.seek(SeekFrom::Start(0)).await.unwrap();
        assert_eq!(read_to_end(&mut removed).await, b"before+after");
        assert_eq!(vfs.read("/kept.txt").await.unwrap(), b"replacement");

        let mut moved = open(&vfs, "/kept.txt", |options| {
            options.read(true).write(true);
        })
        .await;
        vfs.create_dir_all("/moved").await.unwrap();
        vfs.rename("/kept.txt", "/moved/kept.txt").await.unwrap();
        moved.write_all(b"REPLACE").await.unwrap();
        moved.seek(SeekFrom::Start(0)).await.unwrap();
        assert_eq!(read_to_end(&mut moved).await, b"REPLACEment");
        assert_eq!(vfs.read("/moved/kept.txt").await.unwrap(), b"REPLACEment");
    });
}

#[test]
fn replacing_a_file_directly_updates_its_open_handles() {
    embassy_futures::block_on(async {
        let memory = MemFs::new();
        memory.write_file("/shared.txt", b"first").unwrap();
        let vfs = Vfs::new();
        vfs.mount(
            "/",
            memory.clone().into_backend(),
            MountOptions::read_write(),
        )
        .await
        .unwrap();
        let mut file = vfs.open("/shared.txt").await.unwrap();

        memory.write_file("/shared.txt", b"second").unwrap();
        assert_eq!(read_to_end(&mut file).await, b"second");
    });
}

#[test]
fn directories_list_their_entries_in_name_order() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.create_dir_all("/list/c").await.unwrap();
        for name in ["b", "a", "B"] {
            vfs.write(&format!("/list/{name}"), name.as_bytes())
                .await
                .unwrap();
        }
        vfs.write("/list/long", &pattern(PAGE_SIZE + 1))
            .await
            .unwrap();

        let entries: Vec<_> = vfs
            .read_dir("/list")
            .await
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let metadata = entry.metadata();
                (
                    String::from(entry.file_name()),
                    metadata.file_type(),
                    metadata.len(),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                (String::from("B"), FileType::File, 1),
                (String::from("a"), FileType::File, 1),
                (String::from("b"), FileType::File, 1),
                (String::from("c"), FileType::Directory, 0),
                (String::from("long"), FileType::File, PAGE_SIZE as u64 + 1),
            ]
        );
    });
}

#[test]
fn rename_replaces_an_existing_destination() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.write("/new.txt", b"new").await.unwrap();
        vfs.write("/old.txt", b"old").await.unwrap();
        vfs.rename("/new.txt", "/old.txt").await.unwrap();
        assert_eq!(vfs.read("/old.txt").await.unwrap(), b"new");
        assert_eq!(
            vfs.metadata("/new.txt").await.unwrap_err(),
            FsError::NotFound
        );

        vfs.create_dir_all("/from").await.unwrap();
        vfs.write("/from/a", b"a").await.unwrap();
        vfs.create_dir_all("/to/stale").await.unwrap();
        vfs.rename("/from", "/to").await.unwrap();
        assert_eq!(vfs.read("/to/a").await.unwrap(), b"a");
        assert_eq!(
            vfs.metadata("/to/stale").await.unwrap_err(),
            FsError::NotFound
        );

        vfs.rename("/old.txt", "/old.txt").await.unwrap();
        assert_eq!(vfs.read("/old.txt").await.unwrap(), b"new");
        assert_eq!(
            vfs.rename("/missing", "/elsewhere").await.unwrap_err(),
            FsError::NotFound
        );
        assert_eq!(
            vfs.rename("/old.txt", "/absent/old.txt").await.unwrap_err(),
            FsError::NotFound
        );
        assert_eq!(vfs.read("/old.txt").await.unwrap(), b"new");
    });
}

#[test]
fn operations_report_the_kind_of_entry_they_found() {
    embassy_futures::block_on(async {
        let vfs = mounted().await;
        vfs.create_dir_all("/dir").await.unwrap();
        vfs.write("/dir/file", b"f").await.unwrap();

        assert_eq!(vfs.open("/missing").await.unwrap_err(), FsError::NotFound);
        assert_eq!(vfs.open("/dir").await.unwrap_err(), FsError::IsDirectory);
        let mut exclusive = OpenOptions::new();
        exclusive.write(true).create_new(true);
        assert_eq!(
            vfs.open_with("/dir/file", &exclusive).await.unwrap_err(),
            FsError::AlreadyExists
        );
        assert_eq!(
            vfs.write("/absent/file", b"x").await.unwrap_err(),
            FsError::NotFound
        );
        assert_eq!(
            vfs.create_dir_all("/dir/file/sub").await.unwrap_err(),
            FsError::NotDirectory
        );
        assert_eq!(
            vfs.read_dir("/dir/file").await.map(drop).unwrap_err(),
            FsError::NotDirectory
        );
        assert_eq!(
            vfs.remove_file("/dir").await.unwrap_err(),
            FsError::IsDirectory
        );
        assert_eq!(
            vfs.remove_dir("/dir/file").await.unwrap_err(),
            FsError::NotDirectory
        );
        assert_eq!(
            vfs.remove_dir("/dir").await.unwrap_err(),
            FsError::DirectoryNotEmpty
        );
        assert_eq!(
            vfs.remove_dir("/").await.unwrap_err(),
            FsError::PermissionDenied
        );

        let mut read_only = vfs.open("/dir/file").await.unwrap();
        assert_eq!(
            read_only.write(b"x").await.unwrap_err(),
            FsError::PermissionDenied
        );
        vfs.remove_file("/dir/file").await.unwrap();
        vfs.remove_dir("/dir").await.unwrap();
        assert_eq!(vfs.metadata("/dir").await.unwrap_err(), FsError::NotFound);
    });
}
