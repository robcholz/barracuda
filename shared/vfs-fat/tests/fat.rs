//! FAT backend integration behavior.

use barracuda_vfs::{FsError, MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_fat::FatFs;
use embedded_io::ErrorType;
use embedded_io_async::{Read, Seek, Write};
use std::sync::{Arc, Mutex};

const IMAGE_SIZE: usize = 2 * 1024 * 1024;

struct MemoryDisk {
    bytes: Arc<Mutex<Vec<u8>>>,
    cursor: usize,
}

impl MemoryDisk {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Mutex::new(vec![0; IMAGE_SIZE])),
            cursor: 0,
        }
    }

    fn image(&self) -> Arc<Mutex<Vec<u8>>> {
        Arc::clone(&self.bytes)
    }

    fn reopen(bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        Self { bytes, cursor: 0 }
    }
}

impl ErrorType for MemoryDisk {
    type Error = embedded_io::ErrorKind;
}

impl Read for MemoryDisk {
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let bytes = self
            .bytes
            .lock()
            .map_err(|_| embedded_io::ErrorKind::Other)?;
        let amount = output.len().min(bytes.len().saturating_sub(self.cursor));
        output[..amount].copy_from_slice(&bytes[self.cursor..self.cursor + amount]);
        self.cursor += amount;
        Ok(amount)
    }
}

impl Write for MemoryDisk {
    async fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        let mut bytes = self
            .bytes
            .lock()
            .map_err(|_| embedded_io::ErrorKind::Other)?;
        let end = self
            .cursor
            .checked_add(input.len())
            .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        if end > bytes.len() {
            return Err(embedded_io::ErrorKind::InvalidInput);
        }
        bytes[self.cursor..end].copy_from_slice(input);
        self.cursor = end;
        Ok(input.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for MemoryDisk {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        let length = self
            .bytes
            .lock()
            .map_err(|_| embedded_io::ErrorKind::Other)?
            .len();
        let next = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => length as i128 + offset as i128,
            SeekFrom::Current(offset) => self.cursor as i128 + offset as i128,
        };
        if next < 0 || next > length as i128 {
            return Err(embedded_io::ErrorKind::InvalidInput);
        }
        self.cursor = next as usize;
        Ok(self.cursor as u64)
    }
}

#[test]
fn fat_backend_preserves_long_names_and_normal_file_semantics() {
    embassy_futures::block_on(async {
        let fat = FatFs::format(MemoryDisk::new(), IMAGE_SIZE as u64)
            .await
            .unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/web", fat.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.create_dir_all("/web/assets").await.unwrap();

        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        let mut file = vfs
            .open_with("/web/assets/application.a1b2c3.js", &options)
            .await
            .unwrap();
        file.write_all(b"0123456789").await.unwrap();
        file.flush().await.unwrap();
        file.seek(SeekFrom::Start(4)).await.unwrap();
        let mut bytes = [0; 3];
        file.read_exact(&mut bytes).await.unwrap();
        drop(file);

        assert_eq!(&bytes, b"456");
        assert_eq!(
            vfs.read("/web/assets/application.a1b2c3.js").await.unwrap(),
            b"0123456789"
        );
        let names: Vec<_> = vfs
            .read_dir("/web/assets")
            .await
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_owned())
            .collect();
        assert_eq!(names, ["application.a1b2c3.js"]);
    });
}

#[test]
fn fat_backend_supports_namespace_mutations_and_append() {
    embassy_futures::block_on(async {
        let fat = FatFs::format(MemoryDisk::new(), IMAGE_SIZE as u64)
            .await
            .unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/sd", fat.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        vfs.create_dir_all("/sd/logs/archive").await.unwrap();
        vfs.write("/sd/logs/current.txt", b"first").await.unwrap();

        let mut append = OpenOptions::new();
        append.write(true).append(true);
        let mut file = vfs
            .open_with("/sd/logs/current.txt", &append)
            .await
            .unwrap();
        file.write_all(b"-second").await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        assert_eq!(
            vfs.metadata("/sd/logs/current.txt").await.unwrap().len(),
            12
        );
        vfs.rename("/sd/logs/current.txt", "/sd/logs/archive/run.txt")
            .await
            .unwrap();
        assert_eq!(
            vfs.read("/sd/logs/archive/run.txt").await.unwrap(),
            b"first-second"
        );
        vfs.remove_file("/sd/logs/archive/run.txt").await.unwrap();
        vfs.remove_dir("/sd/logs/archive").await.unwrap();
        vfs.remove_dir("/sd/logs").await.unwrap();
    });
}

#[test]
fn formatted_image_remounts_with_flushed_data_intact() {
    embassy_futures::block_on(async {
        let disk = MemoryDisk::new();
        let image = disk.image();
        let fat = FatFs::format(disk, IMAGE_SIZE as u64).await.unwrap();
        let mut writer = Vfs::new();
        writer
            .mount("/data", fat.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        writer
            .write("/data/persisted.txt", b"survives remount")
            .await
            .unwrap();
        drop(writer);

        let fat = FatFs::mount(MemoryDisk::reopen(image)).await.unwrap();
        let mut reader = Vfs::new();
        reader
            .mount("/data", fat.into_backend(), MountOptions::read_only())
            .await
            .unwrap();

        assert_eq!(
            reader.read("/data/persisted.txt").await.unwrap(),
            b"survives remount"
        );
    });
}

#[test]
fn fat_backend_enforces_open_modes_types_and_seek_boundaries() {
    embassy_futures::block_on(async {
        assert!(matches!(
            FatFs::mount(MemoryDisk::new()).await,
            Err(FsError::Io)
        ));

        let fat = FatFs::format(MemoryDisk::new(), IMAGE_SIZE as u64)
            .await
            .unwrap();
        let mut vfs = Vfs::new();
        vfs.mount("/disk", fat.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        assert!(vfs.metadata("/disk").await.unwrap().is_dir());
        assert_eq!(vfs.create_dir_all("/disk").await, Ok(()));
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
        writer.seek(SeekFrom::Start(8)).await.unwrap();
        writer.write_all(b"z").await.unwrap();
        writer.flush().await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(vfs.read("/disk/dir/file").await.unwrap(), b"abcdef\0\0z");

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
