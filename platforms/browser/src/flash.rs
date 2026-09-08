//! OPFS-backed NOR flash used by the Browser Platform.

use core::ops::Range;

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    DedicatedWorkerGlobalScope, FileSystemDirectoryHandle, FileSystemFileHandle,
    FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemReadWriteOptions,
    FileSystemSyncAccessHandle,
};

const ERASED_CHUNK: usize = 4096;

/// Durable fixed-size NOR flash stored in the browser's Origin Private File System.
pub struct OpfsNorFlash {
    handle: FileSystemSyncAccessHandle,
    capacity: usize,
}

impl OpfsNorFlash {
    /// Opens or initializes one fixed-size OPFS flash image.
    ///
    /// # Errors
    ///
    /// Returns an error outside a dedicated worker, when OPFS is unavailable,
    /// or when an existing image has a different capacity.
    pub async fn open(name: &str, capacity: usize) -> Result<Self, OpfsNorFlashError> {
        let worker = js_sys::global()
            .dyn_into::<DedicatedWorkerGlobalScope>()
            .map_err(|_| OpfsNorFlashError::Unavailable)?;
        let root = JsFuture::from(worker.navigator().storage().get_directory())
            .await
            .map_err(OpfsNorFlashError::javascript)?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(|_| OpfsNorFlashError::Unavailable)?;

        let directory_options = FileSystemGetDirectoryOptions::new();
        directory_options.set_create(true);
        let directory =
            JsFuture::from(root.get_directory_handle_with_options("barracuda", &directory_options))
                .await
                .map_err(OpfsNorFlashError::javascript)?
                .dyn_into::<FileSystemDirectoryHandle>()
                .map_err(|_| OpfsNorFlashError::Unavailable)?;

        let file_options = FileSystemGetFileOptions::new();
        file_options.set_create(true);
        let file = JsFuture::from(directory.get_file_handle_with_options(name, &file_options))
            .await
            .map_err(OpfsNorFlashError::javascript)?
            .dyn_into::<FileSystemFileHandle>()
            .map_err(|_| OpfsNorFlashError::Unavailable)?;
        let handle = JsFuture::from(file.create_sync_access_handle())
            .await
            .map_err(OpfsNorFlashError::javascript)?
            .dyn_into::<FileSystemSyncAccessHandle>()
            .map_err(|_| OpfsNorFlashError::Unavailable)?;
        let actual = handle.get_size().map_err(OpfsNorFlashError::javascript)? as usize;

        let flash = Self { handle, capacity };
        if actual == 0 {
            flash.initialize_erased()?;
        } else if actual != capacity {
            return Err(OpfsNorFlashError::CapacityMismatch {
                expected: capacity,
                actual,
            });
        }
        Ok(flash)
    }

    /// Replaces one complete aligned region, used to provision a downloaded image.
    ///
    /// # Errors
    ///
    /// Returns an error when the range or image size is invalid or OPFS fails.
    pub fn replace_region(
        &mut self,
        offset: u32,
        size: usize,
        image: &[u8],
    ) -> Result<(), OpfsNorFlashError> {
        if image.len() != size {
            return Err(OpfsNorFlashError::CapacityMismatch {
                expected: size,
                actual: image.len(),
            });
        }
        let to = offset
            .checked_add(u32::try_from(size).map_err(|_| OpfsNorFlashError::OutOfBounds)?)
            .ok_or(OpfsNorFlashError::OutOfBounds)?;
        self.erase(offset, to)?;
        self.write(offset, image)
    }

    fn initialize_erased(&self) -> Result<(), OpfsNorFlashError> {
        self.handle
            .truncate_with_f64(self.capacity as f64)
            .map_err(OpfsNorFlashError::javascript)?;
        let erased = [0xff; ERASED_CHUNK];
        let mut offset = 0usize;
        while offset < self.capacity {
            let length = self.capacity.saturating_sub(offset).min(erased.len());
            self.write_exact(offset, &erased[..length])?;
            offset = offset
                .checked_add(length)
                .ok_or(OpfsNorFlashError::OutOfBounds)?;
        }
        self.flush()
    }

    fn range(
        &self,
        offset: u32,
        length: usize,
        alignment: usize,
    ) -> Result<Range<usize>, OpfsNorFlashError> {
        let start = usize::try_from(offset).map_err(|_| OpfsNorFlashError::OutOfBounds)?;
        if start.checked_rem(alignment) != Some(0) || length.checked_rem(alignment) != Some(0) {
            return Err(OpfsNorFlashError::NotAligned);
        }
        let end = start
            .checked_add(length)
            .filter(|end| *end <= self.capacity)
            .ok_or(OpfsNorFlashError::OutOfBounds)?;
        Ok(start..end)
    }

    fn read_exact(&self, offset: usize, bytes: &mut [u8]) -> Result<(), OpfsNorFlashError> {
        let options = FileSystemReadWriteOptions::new();
        options.set_at_f64(offset as f64);
        let read = self
            .handle
            .read_with_u8_array_and_options(bytes, &options)
            .map_err(OpfsNorFlashError::javascript)? as usize;
        if read == bytes.len() {
            Ok(())
        } else {
            Err(OpfsNorFlashError::ShortIo)
        }
    }

    fn write_exact(&self, offset: usize, bytes: &[u8]) -> Result<(), OpfsNorFlashError> {
        let options = FileSystemReadWriteOptions::new();
        options.set_at_f64(offset as f64);
        let written = self
            .handle
            .write_with_u8_array_and_options(bytes, &options)
            .map_err(OpfsNorFlashError::javascript)? as usize;
        if written == bytes.len() {
            Ok(())
        } else {
            Err(OpfsNorFlashError::ShortIo)
        }
    }

    fn flush(&self) -> Result<(), OpfsNorFlashError> {
        self.handle.flush().map_err(OpfsNorFlashError::javascript)
    }
}

impl Drop for OpfsNorFlash {
    fn drop(&mut self) {
        self.handle.close();
    }
}

/// OPFS flash access failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpfsNorFlashError {
    /// OPFS synchronous access is unavailable in the current global context.
    #[error("OPFS synchronous access is unavailable")]
    Unavailable,
    /// An operation exceeded the configured flash image.
    #[error("OPFS flash operation is out of bounds")]
    OutOfBounds,
    /// An address or length violated the flash operation granularity.
    #[error("OPFS flash operation is not aligned")]
    NotAligned,
    /// A write attempted to change a programmed zero bit back to one.
    #[error("OPFS flash write requires an erase")]
    RequiresErase,
    /// An existing image or provisioned region has a different size.
    #[error("OPFS flash size is {actual} bytes, expected {expected}")]
    CapacityMismatch {
        /// Configured size.
        expected: usize,
        /// Observed size.
        actual: usize,
    },
    /// A synchronous OPFS operation transferred fewer bytes than requested.
    #[error("OPFS returned a short read or write")]
    ShortIo,
    /// A browser filesystem operation failed.
    #[error("OPFS operation failed: {0}")]
    Javascript(String),
}

impl OpfsNorFlashError {
    fn javascript(value: JsValue) -> Self {
        Self::Javascript(
            value
                .as_string()
                .unwrap_or_else(|| String::from("unknown JavaScript exception")),
        )
    }
}

impl NorFlashError for OpfsNorFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds | Self::CapacityMismatch { .. } => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            Self::Unavailable | Self::RequiresErase | Self::ShortIo | Self::Javascript(_) => {
                NorFlashErrorKind::Other
            }
        }
    }
}

impl ErrorType for OpfsNorFlash {
    type Error = OpfsNorFlashError;
}

impl ReadNorFlash for OpfsNorFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::READ_SIZE)?;
        self.read_exact(range.start, bytes)
    }

    fn capacity(&self) -> usize {
        self.capacity
    }
}

impl NorFlash for OpfsNorFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = ERASED_CHUNK;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let length = to
            .checked_sub(from)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or(OpfsNorFlashError::OutOfBounds)?;
        let range = self.range(from, length, Self::ERASE_SIZE)?;
        let erased = [0xff; ERASED_CHUNK];
        for offset in (range.start..range.end).step_by(ERASED_CHUNK) {
            self.write_exact(offset, &erased)?;
        }
        self.flush()
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let range = self.range(offset, bytes.len(), Self::WRITE_SIZE)?;
        let mut current = vec![0; bytes.len()];
        self.read_exact(range.start, &mut current)?;
        if current
            .iter()
            .zip(bytes)
            .any(|(current, requested)| current & requested != *requested)
        {
            return Err(OpfsNorFlashError::RequiresErase);
        }
        self.write_exact(range.start, bytes)?;
        self.flush()
    }
}
