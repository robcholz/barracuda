//! System-owned adaptation of a provisioned NOR partition to async byte I/O.

use alloc::vec;

use barracuda_platform::PartitionFilesystem;
use barracuda_vfs::{Backend, FsError};
use barracuda_vfs_fat::FatFs;
use barracuda_vfs_littlefs::mount_partition;
use embedded_io::{ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use embedded_storage::nor_flash::ReadNorFlash;

pub(super) struct ReadOnlyFlashDisk<Flash> {
    flash: Flash,
    cursor: usize,
}

pub(super) async fn mount_resources_partition<Flash>(
    flash: Flash,
    filesystem: PartitionFilesystem,
) -> Result<Backend, FsError>
where
    Flash: embedded_storage::nor_flash::NorFlash + Send + 'static,
{
    let backend = match filesystem {
        PartitionFilesystem::FatFs => FatFs::mount(ReadOnlyFlashDisk::new(flash))
            .await?
            .into_backend(),
        PartitionFilesystem::LittleFs => mount_partition(flash)?,
        PartitionFilesystem::Raw => return Err(FsError::InvalidInput),
    };
    Ok(backend)
}

impl<Flash> ReadOnlyFlashDisk<Flash> {
    pub(super) const fn new(flash: Flash) -> Self {
        Self { flash, cursor: 0 }
    }
}

impl<Flash> ErrorType for ReadOnlyFlashDisk<Flash> {
    type Error = FsError;
}

impl<Flash> Read for ReadOnlyFlashDisk<Flash>
where
    Flash: ReadNorFlash + Send,
{
    async fn read(&mut self, output: &mut [u8]) -> Result<usize, Self::Error> {
        let amount = output
            .len()
            .min(self.flash.capacity().saturating_sub(self.cursor));
        if amount == 0 {
            return Ok(0);
        }
        let remainder = self
            .cursor
            .checked_rem(Flash::READ_SIZE)
            .ok_or(FsError::Io)?;
        let aligned_start = self.cursor.checked_sub(remainder).ok_or(FsError::Io)?;
        let end = self.cursor.checked_add(amount).ok_or(FsError::Io)?;
        let aligned_end = end
            .checked_add(Flash::READ_SIZE.saturating_sub(1))
            .and_then(|end| end.checked_div(Flash::READ_SIZE))
            .and_then(|blocks| blocks.checked_mul(Flash::READ_SIZE))
            .filter(|end| *end <= self.flash.capacity())
            .ok_or(FsError::Io)?;
        let aligned_len = aligned_end.checked_sub(aligned_start).ok_or(FsError::Io)?;
        let mut aligned = vec![0; aligned_len];
        let offset = u32::try_from(aligned_start).map_err(|_error| FsError::Io)?;
        self.flash
            .read(offset, &mut aligned)
            .map_err(|_error| FsError::Io)?;
        let source_start = self.cursor.checked_sub(aligned_start).ok_or(FsError::Io)?;
        let source_end = source_start.checked_add(amount).ok_or(FsError::Io)?;
        let destination = output.get_mut(..amount).ok_or(FsError::Io)?;
        let source = aligned.get(source_start..source_end).ok_or(FsError::Io)?;
        destination.copy_from_slice(source);
        self.cursor = end;
        Ok(amount)
    }
}

impl<Flash> Write for ReadOnlyFlashDisk<Flash> {
    async fn write(&mut self, _input: &[u8]) -> Result<usize, Self::Error> {
        Err(FsError::ReadOnly)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<Flash> Seek for ReadOnlyFlashDisk<Flash>
where
    Flash: ReadNorFlash,
{
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        let capacity = i128::try_from(self.flash.capacity()).map_err(|_error| FsError::Io)?;
        let cursor = i128::try_from(self.cursor).map_err(|_error| FsError::Io)?;
        let next = match position {
            SeekFrom::Start(offset) => Some(i128::from(offset)),
            SeekFrom::End(offset) => capacity.checked_add(i128::from(offset)),
            SeekFrom::Current(offset) => cursor.checked_add(i128::from(offset)),
        }
        .filter(|next| *next >= 0 && *next <= capacity)
        .ok_or(FsError::InvalidInput)?;
        self.cursor = usize::try_from(next).map_err(|_error| FsError::Io)?;
        u64::try_from(self.cursor).map_err(|_error| FsError::Io)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use embedded_storage::nor_flash::{NorFlashError, NorFlashErrorKind};

    struct MemoryFlash([u8; 8]);

    impl embedded_storage::nor_flash::ErrorType for MemoryFlash {
        type Error = MemoryFlashError;
    }

    #[derive(Debug)]
    struct MemoryFlashError;

    impl NorFlashError for MemoryFlashError {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }

    impl ReadNorFlash for MemoryFlash {
        const READ_SIZE: usize = 4;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = usize::try_from(offset).map_err(|_error| MemoryFlashError)?;
            let end = start.checked_add(bytes.len()).ok_or(MemoryFlashError)?;
            bytes.copy_from_slice(self.0.get(start..end).ok_or(MemoryFlashError)?);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.0.len()
        }
    }

    #[test]
    fn reads_unaligned_ranges_but_rejects_writes() {
        futures_lite::future::block_on(async {
            let mut disk = ReadOnlyFlashDisk::new(MemoryFlash(*b"abcdefgh"));
            disk.seek(SeekFrom::Start(2)).await.unwrap();
            let mut bytes = [0; 3];
            disk.read(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"cde");
            assert_eq!(disk.write(b"x").await, Err(FsError::ReadOnly));
        });
    }
}
