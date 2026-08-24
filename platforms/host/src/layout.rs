//! Host-native flash image layout.

use embedded_storage_async::nor_flash::NorFlash;

/// Whether Host runtime code may mutate one image region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRegionAccess {
    /// Provisioned bytes are immutable at runtime.
    ReadOnly,
    /// Runtime code may erase and program the region.
    ReadWrite,
}

/// One named region in a Host flash image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostRegion {
    name: &'static str,
    offset: u32,
    size: u32,
    access: HostRegionAccess,
}

impl HostRegion {
    /// Declares a writable Host image region.
    #[must_use]
    pub const fn read_write(name: &'static str, offset: u32, size: u32) -> Self {
        Self {
            name,
            offset,
            size,
            access: HostRegionAccess::ReadWrite,
        }
    }

    /// Declares a provisioned read-only Host image region.
    #[must_use]
    pub const fn read_only(name: &'static str, offset: u32, size: u32) -> Self {
        Self {
            name,
            offset,
            size,
            access: HostRegionAccess::ReadOnly,
        }
    }

    /// Returns the Host-native region label.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the byte offset within the Host flash image.
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
    pub const fn access(&self) -> HostRegionAccess {
        self.access
    }
}

/// Physical layout of one Host-native flash image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostLayout {
    capacity: usize,
    regions: &'static [HostRegion],
}

impl HostLayout {
    /// Creates a Host layout generated from the selected Board bundle.
    #[must_use]
    pub const fn new(capacity: usize, regions: &'static [HostRegion]) -> Self {
        Self { capacity, regions }
    }

    /// Returns the complete image capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
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
    ) -> Result<&'static HostRegion, HostLayoutError> {
        let region = self.region(flash, name)?;
        if region.access == HostRegionAccess::ReadOnly {
            return Err(HostLayoutError::RegionReadOnly);
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
    ) -> Result<&'static HostRegion, HostLayoutError> {
        let region = self.region(flash, name)?;
        if region.access != HostRegionAccess::ReadOnly {
            return Err(HostLayoutError::RegionWritable);
        }
        Ok(region)
    }

    fn region<F: NorFlash>(
        &self,
        flash: &F,
        name: &str,
    ) -> Result<&'static HostRegion, HostLayoutError> {
        self.validate::<F>(flash)?;
        self.regions
            .iter()
            .find(|region| region.name == name)
            .ok_or(HostLayoutError::RegionMissing)
    }

    fn validate<F: NorFlash>(&self, flash: &F) -> Result<(), HostLayoutError> {
        if self.capacity != flash.capacity()
            || F::READ_SIZE == 0
            || F::WRITE_SIZE == 0
            || F::ERASE_SIZE == 0
        {
            return Err(HostLayoutError::Geometry);
        }
        for (index, region) in self.regions.iter().enumerate() {
            let offset = usize::try_from(region.offset).map_err(|_error| HostLayoutError::Range)?;
            let size = usize::try_from(region.size).map_err(|_error| HostLayoutError::Range)?;
            let end = offset.checked_add(size).ok_or(HostLayoutError::Range)?;
            if region.name.is_empty()
                || size == 0
                || end > self.capacity
                || offset.checked_rem(F::ERASE_SIZE) != Some(0)
                || size.checked_rem(F::ERASE_SIZE) != Some(0)
            {
                return Err(HostLayoutError::Range);
            }
            for other in self.regions.iter().skip(index.saturating_add(1)) {
                let other_offset =
                    usize::try_from(other.offset).map_err(|_error| HostLayoutError::Range)?;
                let other_size =
                    usize::try_from(other.size).map_err(|_error| HostLayoutError::Range)?;
                let other_end = other_offset
                    .checked_add(other_size)
                    .ok_or(HostLayoutError::Range)?;
                if region.name == other.name || (offset < other_end && other_offset < end) {
                    return Err(HostLayoutError::Range);
                }
            }
        }
        Ok(())
    }
}

/// Invalid Host-native image layout or logical binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostLayoutError {
    /// Image capacity or operation geometry does not match the native layout.
    #[error("Host flash geometry does not match its native layout")]
    Geometry,
    /// A region is empty, overlapping, duplicated, unaligned, or out of bounds.
    #[error("Host flash layout contains an invalid region")]
    Range,
    /// A Board storage binding names no Host-native region.
    #[error("Board storage binding names no Host-native region")]
    RegionMissing,
    /// A writable capability was bound to a read-only Host-native region.
    #[error("Board writable storage binding names a read-only Host-native region")]
    RegionReadOnly,
    /// A read-only capability was bound to a writable Host-native region.
    #[error("Board read-only storage binding names a writable Host-native region")]
    RegionWritable,
}
