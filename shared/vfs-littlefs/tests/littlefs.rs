//! LittleFS backend integration behavior.

use barracuda_vfs::{MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_littlefs::{
    filesystem_block_count, mount_or_format_partition, mount_partition, LittleFs, PartitionStorage,
};
use embedded_io_async::{Read, Seek, Write};
use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use littlefs2::{driver::Storage as _, fs::Filesystem, path::PathBuf};
use typenum::{U128, U8};

const CAPACITY: usize = 4096;

struct MemoryFlash {
    bytes: Vec<u8>,
}

impl MemoryFlash {
    fn new() -> Self {
        Self::with_blocks(CAPACITY / Self::ERASE_SIZE)
    }

    fn with_blocks(blocks: usize) -> Self {
        Self {
            bytes: vec![0xff; blocks * Self::ERASE_SIZE],
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

type Storage = PartitionStorage<MemoryFlash>;

/// The storage every earlier firmware formatted and mounted LittleFS with:
/// one Rust type per filesystem block count, fixed at compile time.
struct FixedGeometryStorage<const BLOCKS: usize> {
    flash: MemoryFlash,
}

impl<const BLOCKS: usize> littlefs2::driver::Storage for FixedGeometryStorage<BLOCKS> {
    const READ_SIZE: usize = MemoryFlash::READ_SIZE;
    const WRITE_SIZE: usize = MemoryFlash::WRITE_SIZE;
    const BLOCK_SIZE: usize = MemoryFlash::ERASE_SIZE;
    const BLOCK_COUNT: usize = BLOCKS;

    type CACHE_SIZE = U128;
    type LOOKAHEAD_SIZE = U8;

    fn read(&mut self, offset: usize, buffer: &mut [u8]) -> littlefs2::io::Result<usize> {
        let offset = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        ReadNorFlash::read(&mut self.flash, offset, buffer)
            .map_err(|_| littlefs2::io::Error::IO)?;
        Ok(buffer.len())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> littlefs2::io::Result<usize> {
        let offset = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        NorFlash::write(&mut self.flash, offset, bytes).map_err(|_| littlefs2::io::Error::IO)?;
        Ok(bytes.len())
    }

    fn erase(&mut self, offset: usize, len: usize) -> littlefs2::io::Result<usize> {
        let from = u32::try_from(offset).map_err(|_| littlefs2::io::Error::IO)?;
        let to = u32::try_from(offset + len).map_err(|_| littlefs2::io::Error::IO)?;
        NorFlash::erase(&mut self.flash, from, to).map_err(|_| littlefs2::io::Error::IO)?;
        Ok(len)
    }
}

const KEPT: &[u8] = b"formatted with a fixed geometry";

/// Formats `flash` as a fixed `BLOCKS`-block filesystem holding `/kept`.
fn format_fixed<const BLOCKS: usize>(flash: MemoryFlash) -> MemoryFlash {
    let mut storage = FixedGeometryStorage::<BLOCKS> { flash };
    assert_eq!(storage.block_count(), BLOCKS);
    Filesystem::format(&mut storage).unwrap();
    Filesystem::mount_and_then(&mut storage, |filesystem| {
        filesystem.write(&path("/kept"), KEPT)
    })
    .unwrap();
    storage.flash
}

/// Reads `/kept` through a fixed `BLOCKS`-block filesystem, which mounts only
/// when the superblock records exactly `BLOCKS` blocks.
fn read_fixed<const BLOCKS: usize>(flash: MemoryFlash) -> littlefs2::io::Result<Vec<u8>> {
    let mut storage = FixedGeometryStorage::<BLOCKS> { flash };
    Filesystem::mount_and_then(&mut storage, |filesystem| {
        filesystem
            .read::<64>(&path("/kept"))
            .map(|bytes| bytes.to_vec())
    })
}

fn path(path: &str) -> PathBuf {
    PathBuf::try_from(path.as_bytes()).unwrap()
}

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

#[test]
fn littlefs_reports_its_size_and_free_space() {
    embassy_futures::block_on(async {
        let storage = Storage::new(MemoryFlash::new()).unwrap();
        let littlefs = LittleFs::format(storage).unwrap();
        let vfs = Vfs::new();
        vfs.mount("/data", littlefs.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        let empty = vfs.usage("/data").await.unwrap();
        assert_eq!(empty.total(), CAPACITY as u64);
        assert!(empty.free() < empty.total(), "metadata takes blocks");

        // A file larger than an inline one takes blocks of its own.
        vfs.write("/data/blob", &[7; 1024]).await.unwrap();
        let written = vfs.usage("/data/blob").await.unwrap();
        assert_eq!(written.total(), CAPACITY as u64);
        assert!(written.free() < empty.free());
        assert_eq!(written.used(), written.total() - written.free());
    });
}

#[test]
fn filesystem_block_count_keeps_the_fixed_geometries() {
    let expected = [
        (0, None),
        (1, None),
        (2, Some(2)),
        (3, Some(2)),
        (4, Some(4)),
        (63, Some(32)),
        (64, Some(64)),
        (255, Some(128)),
        (256, Some(256)),
        (383, Some(256)),
        (384, Some(384)),
        (511, Some(384)),
        (512, Some(512)),
        (767, Some(512)),
        (768, Some(768)),
        (1_023, Some(768)),
        (1_024, Some(1_024)),
        (1_535, Some(1_024)),
        (1_536, Some(1_536)),
        (2_047, Some(1_536)),
        (2_048, Some(2_048)),
        (3_071, Some(2_048)),
        (3_072, Some(3_072)),
        (4_095, Some(3_072)),
        (4_096, Some(4_096)),
        (100_000, Some(4_096)),
    ];
    for (partition_blocks, filesystem_blocks) in expected {
        assert_eq!(
            filesystem_block_count(partition_blocks),
            filesystem_blocks,
            "{partition_blocks} partition blocks"
        );
    }
}

/// A filesystem that earlier firmware formatted, with one compiled geometry
/// per partition size, still mounts without being formatted again, and a blank
/// partition formats to the block count that firmware used.
#[test]
fn partitions_keep_the_geometry_of_earlier_firmware() {
    macro_rules! check {
        ($partition_blocks:expr, $filesystem_blocks:expr) => {{
            let partition_blocks: usize = $partition_blocks;
            assert_eq!(
                filesystem_block_count(partition_blocks),
                Some($filesystem_blocks)
            );

            let flash =
                format_fixed::<$filesystem_blocks>(MemoryFlash::with_blocks(partition_blocks));
            let backend = mount_partition(flash).unwrap();
            let kept = embassy_futures::block_on(async {
                let vfs = Vfs::new();
                vfs.mount("/", backend, MountOptions::read_only())
                    .await
                    .unwrap();
                vfs.read("/kept").await.unwrap()
            });
            assert_eq!(kept, KEPT);

            let flash =
                format_fixed::<$filesystem_blocks>(MemoryFlash::with_blocks(partition_blocks));
            let littlefs = LittleFs::mount_or_format(Storage::new(flash).unwrap()).unwrap();
            assert_eq!(littlefs.read_file("/kept").unwrap(), KEPT);

            let blank = Storage::new(MemoryFlash::with_blocks(partition_blocks)).unwrap();
            let littlefs = LittleFs::mount_or_format(blank).unwrap();
            littlefs.write_file("/kept", KEPT).unwrap();
            let flash = littlefs.into_storage().unwrap().into_inner();
            assert_eq!(read_fixed::<$filesystem_blocks>(flash).unwrap(), KEPT);
        }};
    }

    check!(9, 8);
    check!(20, 16);
    check!(32, 32);
    check!(100, 64);
    check!(400, 384);
    check!(1_000, 768);
    check!(3_100, 3_072);
    check!(5_000, 4_096);
}

/// The superblock's block count is still checked: a filesystem of another
/// geometry is refused, and formatting replaces it.
#[test]
fn a_filesystem_of_another_geometry_is_not_mounted() {
    let flash = format_fixed::<64>(MemoryFlash::with_blocks(128));
    assert!(mount_partition(flash).is_err());

    let flash = format_fixed::<64>(MemoryFlash::with_blocks(128));
    let littlefs = LittleFs::mount_or_format(Storage::new(flash).unwrap()).unwrap();
    assert!(littlefs.read_file("/kept").is_err());
    let flash = littlefs.into_storage().unwrap().into_inner();
    assert!(read_fixed::<64>(flash).is_err());
}
