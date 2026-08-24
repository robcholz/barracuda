//! FAT backend integration behavior.

use barracuda_vfs::{MountOptions, OpenOptions, SeekFrom, Vfs};
use barracuda_vfs_fat::FatFs;
use embedded_io::ErrorType;
use embedded_io_async::{Read, Seek, Write};

const IMAGE_SIZE: usize = 2 * 1024 * 1024;

struct MemoryDisk {
    bytes: Vec<u8>,
    cursor: usize,
}

impl MemoryDisk {
    fn new() -> Self {
        Self {
            bytes: vec![0; IMAGE_SIZE],
            cursor: 0,
        }
    }
}

impl ErrorType for MemoryDisk {
    type Error = embedded_io::ErrorKind;
}

impl Read for MemoryDisk {
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let amount = output
            .len()
            .min(self.bytes.len().saturating_sub(self.cursor));
        output[..amount].copy_from_slice(&self.bytes[self.cursor..self.cursor + amount]);
        self.cursor += amount;
        Ok(amount)
    }
}

impl Write for MemoryDisk {
    async fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        let end = self
            .cursor
            .checked_add(input.len())
            .ok_or(embedded_io::ErrorKind::InvalidInput)?;
        if end > self.bytes.len() {
            return Err(embedded_io::ErrorKind::InvalidInput);
        }
        self.bytes[self.cursor..end].copy_from_slice(input);
        self.cursor = end;
        Ok(input.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for MemoryDisk {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        let next = match position {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => self.bytes.len() as i128 + offset as i128,
            SeekFrom::Current(offset) => self.cursor as i128 + offset as i128,
        };
        if next < 0 || next > self.bytes.len() as i128 {
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
