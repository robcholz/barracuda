//! Removable FAT filesystem composition behavior.

#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use barracuda_peripheral::PeripheralImplementation;
use barracuda_peripheral::removable_storage::{
    RemovableStorage, RemovableStorageEvent, RemovableStorageStatus,
};
use barracuda_sdmmc_fat_removable_storage::{
    SdMmcFatRemovableStorageBindings, SdMmcFatRemovableStorageConfig,
    SdMmcFatRemovableStorageImplementation,
};
use barracuda_vfs::{FsError, MountOptions, Vfs};
use barracuda_vfs_fat::FatFs;
use embedded_io::{ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};

const IMAGE_SIZE: usize = 2 * 1024 * 1024;

#[derive(Clone)]
struct SharedMemoryDisk {
    bytes: Arc<Mutex<Vec<u8>>>,
    present: Arc<AtomicBool>,
    cursor: usize,
}

impl SharedMemoryDisk {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Mutex::new(vec![0; IMAGE_SIZE])),
            present: Arc::new(AtomicBool::new(true)),
            cursor: 0,
        }
    }

    fn eject(&self) {
        self.present.store(false, Ordering::Release);
    }

    fn ensure_present(&self) -> Result<(), embedded_io::ErrorKind> {
        if self.present.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(embedded_io::ErrorKind::NotConnected)
        }
    }
}

impl ErrorType for SharedMemoryDisk {
    type Error = embedded_io::ErrorKind;
}

impl Read for SharedMemoryDisk {
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        self.ensure_present()?;
        let bytes = self.bytes.lock().unwrap();
        let amount = output.len().min(bytes.len().saturating_sub(self.cursor));
        output[..amount].copy_from_slice(&bytes[self.cursor..self.cursor + amount]);
        self.cursor += amount;
        Ok(amount)
    }
}

impl Write for SharedMemoryDisk {
    async fn write(&mut self, input: &[u8]) -> Result<usize, Self::Error> {
        self.ensure_present()?;
        let mut bytes = self.bytes.lock().unwrap();
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
        self.ensure_present()?;
        Ok(())
    }
}

impl Seek for SharedMemoryDisk {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.ensure_present()?;
        let length = self.bytes.lock().unwrap().len();
        let next = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => length as i128 + i128::from(offset),
            SeekFrom::Current(offset) => self.cursor as i128 + i128::from(offset),
        };
        if next < 0 || next > length as i128 {
            return Err(embedded_io::ErrorKind::InvalidInput);
        }
        self.cursor = next as usize;
        Ok(self.cursor as u64)
    }
}

#[test]
fn inserted_fat_medium_becomes_a_named_mountable_filesystem() {
    embassy_futures::block_on(async {
        let disk = SharedMemoryDisk::new();
        let fat = FatFs::format(disk.clone(), IMAGE_SIZE as u64)
            .await
            .unwrap();
        let seed = Vfs::new();
        seed.mount("/seed", fat.into_backend(), MountOptions::read_write())
            .await
            .unwrap();
        seed.write("/seed/identity", b"card").await.unwrap();
        drop(seed);

        let ejector = disk.clone();
        let mut storage = SdMmcFatRemovableStorageImplementation::initialize(
            SdMmcFatRemovableStorageBindings::new(disk),
            SdMmcFatRemovableStorageConfig::new("micro-sd"),
        )
        .await
        .unwrap();

        assert_eq!(storage.slot_id(), "micro-sd");
        assert_eq!(storage.status(), RemovableStorageStatus::Absent);

        let event = storage.next_event().await;
        assert!(matches!(event, RemovableStorageEvent::Mounted { .. }));
        let RemovableStorageEvent::Mounted {
            generation,
            filesystem,
        } = event
        else {
            return;
        };
        assert_eq!(
            storage.status(),
            RemovableStorageStatus::Mounted { generation }
        );

        let namespace = Vfs::new();
        namespace
            .mount(
                "/removable/micro-sd",
                filesystem,
                MountOptions::read_write(),
            )
            .await
            .unwrap();
        assert_eq!(
            namespace
                .read("/removable/micro-sd/identity")
                .await
                .unwrap(),
            b"card"
        );
        let mut retained = namespace
            .open("/removable/micro-sd/identity")
            .await
            .unwrap();

        ejector.eject();
        assert!(matches!(
            storage.next_event().await,
            RemovableStorageEvent::Removed {
                generation: removed
            } if removed == generation
        ));
        assert_eq!(storage.status(), RemovableStorageStatus::Absent);
        namespace.detach("/removable/micro-sd").await.unwrap();
        assert_eq!(
            namespace.metadata("/removable/micro-sd/identity").await,
            Err(FsError::NotMounted)
        );
        assert_eq!(
            retained.read(&mut [0_u8; 4]).await,
            Err(FsError::MediaRemoved)
        );
    });
}
