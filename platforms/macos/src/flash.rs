//! macOS file-backed and volatile NOR flash implementations.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;
use std::io::{Read as _, Seek as _, Write as _};
use std::path::Path;

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

/// Durable standard-platform NOR flash backed by one fixed-size file.
pub struct FileNorFlash {
    file: std::fs::File,
    capacity: usize,
}

impl FileNorFlash {
    /// Opens a fixed-size flash image, initializing a new image to erased bytes.
    ///
    /// Existing images must have exactly `capacity` bytes. A partially created
    /// or differently configured image is rejected rather than truncated.
    ///
    /// # Errors
    ///
    /// Returns an error for filesystem I/O failure or a capacity mismatch.
    pub fn open(path: impl AsRef<Path>, capacity: usize) -> Result<Self, FileNorFlashError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(FileNorFlashError::Io)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(FileNorFlashError::Io)?;
        let actual = usize::try_from(file.metadata().map_err(FileNorFlashError::Io)?.len())
            .map_err(|_| FileNorFlashError::OutOfBounds)?;
        if actual == 0 {
            let erased = [0xff; 4096];
            let mut written = 0usize;
            while written < capacity {
                let remaining = capacity
                    .checked_sub(written)
                    .ok_or(FileNorFlashError::OutOfBounds)?;
                let length = remaining.min(erased.len());
                file.write_all(erased.get(..length).ok_or(FileNorFlashError::OutOfBounds)?)
                    .map_err(FileNorFlashError::Io)?;
                written = written
                    .checked_add(length)
                    .ok_or(FileNorFlashError::OutOfBounds)?;
            }
            file.sync_data().map_err(FileNorFlashError::Io)?;
        } else if actual != capacity {
            return Err(FileNorFlashError::CapacityMismatch {
                expected: capacity,
                actual,
            });
        }
        Ok(Self { file, capacity })
    }

    fn range(
        &self,
        offset: u32,
        length: usize,
        alignment: usize,
    ) -> Result<Range<usize>, FileNorFlashError> {
        let start = usize::try_from(offset).map_err(|_| FileNorFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(FileNorFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.capacity)
            .ok_or(FileNorFlashError::OutOfBounds)?;
        Ok(start..end)
    }
}

/// Failure from [`FileNorFlash`].
#[derive(Debug, thiserror::Error)]
pub enum FileNorFlashError {
    /// standard-platform filesystem I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// An operation exceeded the configured image.
    #[error("flash operation is out of bounds")]
    OutOfBounds,
    /// An address or length violated the operation's granularity.
    #[error("flash operation is not aligned")]
    NotAligned,
    /// An existing image has a different configured capacity.
    #[error("flash image capacity is {actual} bytes, expected {expected}")]
    CapacityMismatch {
        /// Configured capacity.
        expected: usize,
        /// Existing image capacity.
        actual: usize,
    },
}

impl NorFlashError for FileNorFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds | Self::CapacityMismatch { .. } => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            Self::Io(_) => NorFlashErrorKind::Other,
        }
    }
}

impl ErrorType for FileNorFlash {
    type Error = FileNorFlashError;
}

impl ReadNorFlash for FileNorFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::READ_SIZE)?;
        let position = u64::try_from(range.start).map_err(|_| FileNorFlashError::OutOfBounds)?;
        self.file
            .seek(std::io::SeekFrom::Start(position))
            .map_err(FileNorFlashError::Io)?;
        self.file.read_exact(bytes).map_err(FileNorFlashError::Io)?;
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}

impl NorFlash for FileNorFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = to
            .checked_sub(from)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(FileNorFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        let position = u64::try_from(range.start).map_err(|_| FileNorFlashError::OutOfBounds)?;
        self.file
            .seek(std::io::SeekFrom::Start(position))
            .map_err(FileNorFlashError::Io)?;
        self.file
            .write_all(&vec![0xff; length])
            .map_err(FileNorFlashError::Io)?;
        self.file.sync_data().map_err(FileNorFlashError::Io)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let position = u64::try_from(range.start).map_err(|_| FileNorFlashError::OutOfBounds)?;
        self.file
            .seek(std::io::SeekFrom::Start(position))
            .map_err(FileNorFlashError::Io)?;
        self.file.write_all(bytes).map_err(FileNorFlashError::Io)?;
        self.file.sync_data().map_err(FileNorFlashError::Io)
    }
}

/// Volatile standard-platform NOR flash used when durable storage is not required.
pub struct VolatileNorFlash {
    bytes: Vec<u8>,
}

impl VolatileNorFlash {
    /// Creates a completely erased volatile region with `capacity` bytes.
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
    ) -> Result<Range<usize>, VolatileNorFlashError> {
        let start = usize::try_from(offset).map_err(|_| VolatileNorFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(VolatileNorFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(VolatileNorFlashError::OutOfBounds)?;
        Ok(start..end)
    }
}

/// Failure from [`VolatileNorFlash`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolatileNorFlashError {
    /// An operation exceeded the configured region.
    OutOfBounds,
    /// An address or length violated the operation's granularity.
    NotAligned,
}

impl NorFlashError for VolatileNorFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
        }
    }
}

impl ErrorType for VolatileNorFlash {
    type Error = VolatileNorFlashError;
}

impl ReadNorFlash for VolatileNorFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::READ_SIZE)?;
        let source = self
            .bytes
            .get(range)
            .ok_or(VolatileNorFlashError::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.len()
    }
}

impl NorFlash for VolatileNorFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = to
            .checked_sub(from)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(VolatileNorFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        self.bytes
            .get_mut(range)
            .ok_or(VolatileNorFlashError::OutOfBounds)?
            .fill(0xff);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let target = self
            .bytes
            .get_mut(range)
            .ok_or(VolatileNorFlashError::OutOfBounds)?;
        for (target, source) in target.iter_mut().zip(bytes) {
            *target &= *source;
        }
        Ok(())
    }
}
