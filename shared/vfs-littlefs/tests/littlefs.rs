//! LittleFS backend integration behavior.

use barracuda_vfs::{MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_littlefs::{
    mount_or_format_partition, mount_partition, LittleFs, PartitionStorage,
};
use embedded_io_async::{Read, Seek, Write};
use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use typenum::{U1, U16};

const CAPACITY: usize = 4096;
const BLOCKS: usize = CAPACITY / MemoryFlash::ERASE_SIZE;

struct MemoryFlash {
    bytes: [u8; CAPACITY],
}

impl MemoryFlash {
    fn new() -> Self {
        Self {
            bytes: [0xff; CAPACITY],
        }
    }
}

impl ErrorType for MemoryFlash {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for MemoryFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        let end = start + bytes.len();
        bytes.copy_from_slice(
            self.bytes
                .get(start..end)
                .ok_or(NorFlashErrorKind::OutOfBounds)?,
        );
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for MemoryFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 128;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.bytes
            .get_mut(from as usize..to as usize)
            .ok_or(NorFlashErrorKind::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        let end = start + bytes.len();
        let destination = self
            .bytes
            .get_mut(start..end)
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        for (destination, source) in destination.iter_mut().zip(bytes) {
            *destination &= *source;
        }
        Ok(())
    }
}

type Storage = PartitionStorage<MemoryFlash, U16, U1, BLOCKS>;

#[test]
fn partition_backed_littlefs_supports_streaming_file_io() {
    embassy_futures::block_on(async {
        let storage = Storage::new(MemoryFlash::new()).unwrap();
        let littlefs = LittleFs::format(storage).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/data", littlefs.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.create_dir_all("/data/logs").await.unwrap();

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        let mut file = vfs
            .open_with("/data/logs/events.bin", &options)
            .await
            .unwrap();
        file.write_all(b"abcdef").await.unwrap();
        file.seek(SeekFrom::Start(2)).await.unwrap();
        let mut bytes = [0; 3];
        file.read_exact(&mut bytes).await.unwrap();

        assert_eq!(&bytes, b"cde");
    });
}

#[test]
fn littlefs_backend_can_be_mounted_again_without_formatting() {
    let storage = Storage::new(MemoryFlash::new()).unwrap();
    let littlefs = LittleFs::format(storage).unwrap();
    littlefs.write_file("/state", b"kept").unwrap();
    let storage = littlefs.into_storage().unwrap();

    let mounted = LittleFs::mount(storage).unwrap();
    assert_eq!(mounted.read_file("/state").unwrap(), b"kept");
}

#[test]
fn littlefs_backend_supports_namespace_mutations_and_append() {
    embassy_futures::block_on(async {
        let storage = Storage::new(MemoryFlash::new()).unwrap();
        let littlefs = LittleFs::mount_or_format(storage).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/data", littlefs.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.create_dir_all("/data/logs/archive").await.unwrap();
        vfs.write("/data/logs/current", b"one").await.unwrap();

        let mut append = OpenOptions::new();
        append.write(true).append(true);
        let mut file = vfs.open_with("/data/logs/current", &append).await.unwrap();
        file.write_all(b"-two").await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        vfs.rename("/data/logs/current", "/data/logs/archive/run")
            .await
            .unwrap();
        assert_eq!(
            vfs.read("/data/logs/archive/run").await.unwrap(),
            b"one-two"
        );
        assert_eq!(vfs.read_dir("/data/logs/archive").await.unwrap().count(), 1);
        vfs.remove_file("/data/logs/archive/run").await.unwrap();
        vfs.remove_dir("/data/logs/archive").await.unwrap();
        vfs.remove_dir("/data/logs").await.unwrap();
    });
}

#[test]
fn generic_partition_entry_mounts_the_complete_supported_region() {
    embassy_futures::block_on(async {
        let backend = mount_or_format_partition(MemoryFlash::new()).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/", backend, MountOptions::read_write())
            .await
            .unwrap();

        vfs.write("/state", b"mounted from a generic NOR partition")
            .await
            .unwrap();

        assert_eq!(
            vfs.read("/state").await.unwrap(),
            b"mounted from a generic NOR partition"
        );
    });
}

#[test]
fn provisioned_partition_entry_mounts_without_formatting() {
    embassy_futures::block_on(async {
        assert!(mount_partition(MemoryFlash::new()).is_err());

        let storage = Storage::new(MemoryFlash::new()).unwrap();
        let littlefs = LittleFs::format(storage).unwrap();
        littlefs.write_file("/resource", b"bundled").unwrap();
        let flash = littlefs.into_storage().unwrap().into_inner();
        let backend = mount_partition(flash).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/", backend, MountOptions::read_only())
            .await
            .unwrap();

        assert_eq!(vfs.read("/resource").await.unwrap(), b"bundled");
    });
}

#[test]
fn littlefs_backend_keeps_any_utf8_name() {
    embassy_futures::block_on(async {
        let storage = Storage::new(MemoryFlash::new()).unwrap();
        let littlefs = LittleFs::mount_or_format(storage).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/data", littlefs.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.create_dir_all("/data/笔记").await.unwrap();
        for name in ["café ☕.txt", "100%.txt", "%41.txt"] {
            let path = format!("/data/笔记/{name}");
            vfs.write(&path, name.as_bytes()).await.unwrap();
            assert_eq!(vfs.read(&path).await.unwrap(), name.as_bytes(), "{name}");
        }
        let mut names: Vec<String> = vfs
            .read_dir("/data/笔记")
            .await
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["%41.txt", "100%.txt", "café ☕.txt"]);
        assert_eq!(
            vfs.read_dir("/data")
                .await
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .file_name(),
            "笔记"
        );
        vfs.rename("/data/笔记/café ☕.txt", "/data/笔记/thé.txt")
            .await
            .unwrap();
        assert_eq!(
            vfs.read("/data/笔记/thé.txt").await.unwrap(),
            "café ☕.txt".as_bytes()
        );
    });
}
