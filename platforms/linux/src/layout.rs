//! Linux file-backed flash image layout.

use embedded_storage::nor_flash::NorFlash;

/// Whether standard-platform runtime code may mutate one image region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileRegionAccess {
    /// Provisioned bytes are immutable at runtime.
    ReadOnly,
    /// Runtime code may erase and program the region.
    ReadWrite,
}

/// One named region in a file-backed flash image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileRegion {
    name: &'static str,
    offset: u32,
    size: u32,
    access: FileRegionAccess,
}

impl FileRegion {
    /// Declares a writable file-backed image region.
    #[must_use]
    pub const fn read_write(name: &'static str, offset: u32, size: u32) -> Self {
        Self {
            name,
            offset,
            size,
            access: FileRegionAccess::ReadWrite,
        }
    }

    /// Declares a provisioned read-only file-backed image region.
    #[must_use]
    pub const fn read_only(name: &'static str, offset: u32, size: u32) -> Self {
        Self {
            name,
            offset,
            size,
            access: FileRegionAccess::ReadOnly,
        }
    }

    /// Returns the file-backed region label.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the byte offset within the file-backed flash image.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Returns the region capacity in bytes.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Returns the runtime access discipline.
    #[must_use]
    pub const fn access(&self) -> FileRegionAccess {
        self.access
    }
}

/// Physical layout of one file-backed flash image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileLayout {
    capacity: usize,
    regions: &'static [FileRegion],
}

impl FileLayout {
    /// Creates a standard-platform layout generated from the selected Board bundle.
    #[must_use]
    pub const fn new(capacity: usize, regions: &'static [FileRegion]) -> Self {
        Self { capacity, regions }
    }

    /// Returns the complete image capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Validates the physical image and returns every native region in layout order.
    ///
    /// # Errors
    ///
    /// Returns an error when geometry, alignment, names, or ranges are invalid.
    pub fn regions<F: NorFlash>(
        &self,
        flash: &F,
    ) -> Result<&'static [FileRegion], FileLayoutError> {
        self.validate::<F>(flash)?;
        Ok(self.regions)
    }

    /// Validates the native layout against the opened flash and resolves a writable region.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed layouts, incompatible flash geometry, missing labels,
    /// or a logical database binding that names a read-only region.
    pub fn writable_region<F: NorFlash>(
        &self,
        flash: &F,
        name: &str,
    ) -> Result<&'static FileRegion, FileLayoutError> {
        let region = self.region(flash, name)?;
        if region.access == FileRegionAccess::ReadOnly {
            return Err(FileLayoutError::RegionReadOnly);
        }
        Ok(region)
    }

    /// Validates the native layout and resolves a provisioned read-only region.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed layouts, missing labels, or a logical
    /// read-only binding that names a writable region.
    pub fn read_only_region<F: NorFlash>(
        &self,
        flash: &F,
        name: &str,
    ) -> Result<&'static FileRegion, FileLayoutError> {
        let region = self.region(flash, name)?;
        if region.access != FileRegionAccess::ReadOnly {
            return Err(FileLayoutError::RegionWritable);
        }
        Ok(region)
    }

    fn region<F: NorFlash>(
        &self,
        flash: &F,
        name: &str,
    ) -> Result<&'static FileRegion, FileLayoutError> {
        self.validate::<F>(flash)?;
        self.regions
            .iter()
            .find(|region| region.name == name)
            .ok_or(FileLayoutError::RegionMissing)
    }

    fn validate<F: NorFlash>(&self, flash: &F) -> Result<(), FileLayoutError> {
        if self.capacity != flash.capacity()
            || F::READ_SIZE == 0
            || F::WRITE_SIZE == 0
            || F::ERASE_SIZE == 0
        {
            return Err(FileLayoutError::Geometry);
        }
        for (index, region) in self.regions.iter().enumerate() {
            let offset = usize::try_from(region.offset).map_err(|_error| FileLayoutError::Range)?;
            let size = usize::try_from(region.size).map_err(|_error| FileLayoutError::Range)?;
            let end = offset.checked_add(size).ok_or(FileLayoutError::Range)?;
            if region.name.is_empty()
                || size == 0
                || end > self.capacity
                || offset.checked_rem(F::ERASE_SIZE) != Some(0)
                || size.checked_rem(F::ERASE_SIZE) != Some(0)
            {
                return Err(FileLayoutError::Range);
            }
            for other in self.regions.iter().skip(index.saturating_add(1)) {
                let other_offset =
                    usize::try_from(other.offset).map_err(|_error| FileLayoutError::Range)?;
                let other_size =
                    usize::try_from(other.size).map_err(|_error| FileLayoutError::Range)?;
                let other_end = other_offset
                    .checked_add(other_size)
                    .ok_or(FileLayoutError::Range)?;
                if region.name == other.name || (offset < other_end && other_offset < end) {
                    return Err(FileLayoutError::Range);
                }
            }
        }
        Ok(())
    }
}

/// Invalid file-backed image layout or logical binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileLayoutError {
    /// Image capacity or operation geometry does not match the native layout.
    #[error("file-backed flash geometry does not match its native layout")]
    Geometry,
    /// A region is empty, overlapping, duplicated, unaligned, or out of bounds.
    #[error("file-backed flash layout contains an invalid region")]
    Range,
    /// A Board storage binding names no file-backed region.
    #[error("Board storage binding names no file-backed region")]
    RegionMissing,
    /// A writable capability was bound to a read-only file-backed region.
    #[error("Board writable storage binding names a read-only file-backed region")]
    RegionReadOnly,
    /// A read-only capability was bound to a writable file-backed region.
    #[error("Board read-only storage binding names a writable file-backed region")]
    RegionWritable,
}
