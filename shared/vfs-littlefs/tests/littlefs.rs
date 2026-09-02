//! LittleFS backend integration behavior.

use barracuda_vfs::{FsError, MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_littlefs::{mount_or_format_partition, LittleFs, PartitionStorage};
use embedded_io_async::{Read, Seek, Write};
use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use typenum::{U1, U16};

const CAPACITY: usize = 4096;
const BLOCKS: usize = CAPACITY / MemoryFlash::ERASE_SIZE;

#[derive(Clone)]
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
        let mut vfs = Vfs::new();
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
        let mut vfs = Vfs::new();
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
        let mut vfs = Vfs::new();
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
fn littlefs_enforces_open_modes_types_and_seek_boundaries() {
    embassy_futures::block_on(async {
        assert!(matches!(
            LittleFs::mount(Storage::new(MemoryFlash::new()).unwrap()),
            Err(FsError::Io)
        ));

        let littlefs = LittleFs::format(Storage::new(MemoryFlash::new()).unwrap()).unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/disk", littlefs.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        assert!(vfs.metadata("/disk").await.unwrap().is_dir());
        vfs.create_dir_all("/disk/dir").await.unwrap();
        vfs.write("/disk/dir/file", b"abcdef").await.unwrap();

        let mut read_only = OpenOptions::new();
        read_only.read(true);
        let mut reader = vfs.open_with("/disk/dir/file", &read_only).await.unwrap();
        assert_eq!(reader.write(b"x").await, Err(FsError::PermissionDenied));
        assert_eq!(reader.seek(SeekFrom::End(-2)).await.unwrap(), 4);
        let mut tail = [0; 2];
        reader.read_exact(&mut tail).await.unwrap();
        assert_eq!(&tail, b"ef");
        assert_eq!(
            reader.seek(SeekFrom::Current(-7)).await,
            Err(FsError::InvalidInput)
        );
        assert_eq!(reader.seek(SeekFrom::Start(99)).await.unwrap(), 99);
        assert_eq!(reader.read(&mut tail).await.unwrap(), 0);

        let mut write_only = OpenOptions::new();
        write_only.write(true);
        let mut writer = vfs.open_with("/disk/dir/file", &write_only).await.unwrap();
        assert_eq!(writer.read(&mut tail).await, Err(FsError::PermissionDenied));
        writer.seek(SeekFrom::Start(2)).await.unwrap();
        writer.write_all(b"XY").await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(vfs.read("/disk/dir/file").await.unwrap(), b"abXYef");

        let mut create_new = OpenOptions::new();
        create_new.write(true).create_new(true);
        assert!(matches!(
            vfs.open_with("/disk/dir/file", &create_new).await,
            Err(FsError::AlreadyExists)
        ));
        let mut truncate = OpenOptions::new();
        truncate.write(true).truncate(true);
        drop(vfs.open_with("/disk/dir/file", &truncate).await.unwrap());
        assert!(vfs.read("/disk/dir/file").await.unwrap().is_empty());

        assert!(matches!(
            vfs.open_with("/disk/missing", &read_only).await,
            Err(FsError::NotFound)
        ));
        assert!(matches!(
            vfs.open_with("/disk/dir", &read_only).await,
            Err(FsError::IsDirectory)
        ));
        assert_eq!(
            vfs.create_dir_all("/disk/dir/file/child").await,
            Err(FsError::NotDirectory)
        );
        assert_eq!(
            vfs.remove_file("/disk/dir").await,
            Err(FsError::IsDirectory)
        );
        assert_eq!(
            vfs.remove_dir("/disk/dir/file").await,
            Err(FsError::NotDirectory)
        );
        assert_eq!(
            vfs.remove_dir("/disk/dir").await,
            Err(FsError::DirectoryNotEmpty)
        );
        assert!(matches!(
            vfs.rename("/disk/dir/file", "/disk/missing/file").await,
            Err(FsError::NotFound | FsError::NotDirectory)
        ));
    });
}
