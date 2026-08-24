use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

use embassy_embedded_hal::flash::partition::Partition;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embedded_storage_async::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

/// Deterministic in-memory asynchronous NOR flash for Platform tests.
pub struct MemoryNorFlash {
    bytes: Vec<u8>,
}

impl MemoryNorFlash {
    /// Creates a completely erased flash region with `capacity` bytes.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0xff; capacity],
        }
    }

    fn range(
        &self,
        offset: u32,
        length: usize,
        alignment: usize,
    ) -> Result<Range<usize>, MemoryNorFlashError> {
        let start = usize::try_from(offset).map_err(|_| MemoryNorFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(MemoryNorFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        Ok(start..end)
    }
}

/// Failure from [`MemoryNorFlash`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryNorFlashError {
    /// An operation exceeded the configured region.
    OutOfBounds,
    /// An address or length violated the operation's granularity.
    NotAligned,
}

impl NorFlashError for MemoryNorFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

impl ErrorType for MemoryNorFlash {
    type Error = MemoryNorFlashError;
}

impl ReadNorFlash for MemoryNorFlash {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::READ_SIZE)?;
        let source = self
            .bytes
            .get(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for MemoryNorFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = to
            .checked_sub(from)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        self.bytes
            .get_mut(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let target = self
            .bytes
            .get_mut(range)
            .ok_or(MemoryNorFlashError::OutOfBounds)?;
        for (target, source) in target.iter_mut().zip(bytes) {
            *target &= *source;
        }
        Ok(())
    }
}

/// Writable validated partition backed by process-lifetime test memory.
pub type MemoryPartition = Partition<'static, NoopRawMutex, MemoryNorFlash>;

/// Failure while constructing a validated in-memory test partition.
#[derive(Debug, thiserror::Error)]
pub enum MemoryPartitionError {
    /// Requested capacity cannot be represented by the partition wire format.
    #[error("test partition capacity exceeds u32")]
    Capacity,
    /// Requested capacity is not aligned to the test flash erase size.
    #[error("test partition capacity is not erase aligned")]
    UnalignedCapacity,
}

/// Creates one process-lifetime writable partition for database tests.
///
/// The returned capability has the same Embassy `Partition` type shape used by
/// real Platforms; tests do not introduce a Barracuda-specific table format.
pub async fn memory_partition(capacity: usize) -> Result<MemoryPartition, MemoryPartitionError> {
    let size = u32::try_from(capacity).map_err(|_error| MemoryPartitionError::Capacity)?;
    if capacity.checked_rem(MemoryNorFlash::ERASE_SIZE) != Some(0) {
        return Err(MemoryPartitionError::UnalignedCapacity);
    }
    let flash = Box::leak(Box::new(Mutex::<NoopRawMutex, _>::new(
        MemoryNorFlash::new(capacity),
    )));
    Ok(Partition::new(flash, 0, size))
}
