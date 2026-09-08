//! Browser OPFS flash layout projected from the selected Board.

/// Whether runtime code may mutate one native image region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileRegionAccess {
    /// Provisioned bytes are refreshed from the distributed image at boot.
    ReadOnly,
    /// Persisted bytes survive application restarts.
    ReadWrite,
}

/// One named region in the Browser Platform's OPFS flash image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileRegion {
    name: &'static str,
    offset: u32,
    size: u32,
    access: FileRegionAccess,
    filesystem: barracuda_platform::PartitionFilesystem,
}

impl FileRegion {
    pub(crate) const fn new(
        name: &'static str,
        offset: u32,
        size: u32,
        access: FileRegionAccess,
        filesystem: barracuda_platform::PartitionFilesystem,
    ) -> Self {
        Self {
            name,
            offset,
            size,
            access,
            filesystem,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        self.name
    }

    pub(crate) const fn offset(self) -> u32 {
        self.offset
    }

    pub(crate) const fn size(self) -> u32 {
        self.size
    }

    pub(crate) const fn access(self) -> FileRegionAccess {
        self.access
    }

    pub(crate) const fn filesystem(self) -> barracuda_platform::PartitionFilesystem {
        self.filesystem
    }
}

/// Complete file-backed layout generated from the selected Board bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileLayout {
    capacity: usize,
    regions: &'static [FileRegion],
}

impl FileLayout {
    pub(crate) const fn new(capacity: usize, regions: &'static [FileRegion]) -> Self {
        Self { capacity, regions }
    }

    pub(crate) fn validate(
        self,
        image_size: usize,
    ) -> Result<&'static [FileRegion], FileLayoutError> {
        if self.capacity == 0 || self.capacity != image_size {
            return Err(FileLayoutError::Geometry);
        }
        for (index, region) in self.regions.iter().enumerate() {
            let offset = usize::try_from(region.offset).map_err(|_error| FileLayoutError::Range)?;
            let size = usize::try_from(region.size).map_err(|_error| FileLayoutError::Range)?;
            let end = offset.checked_add(size).ok_or(FileLayoutError::Range)?;
            if region.name.is_empty() || size == 0 || end > self.capacity {
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
        Ok(self.regions)
    }

    pub(crate) const fn capacity(self) -> usize {
        self.capacity
    }
}

/// Invalid Browser-native file-backed image layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileLayoutError {
    /// The image size does not match the selected Board layout.
    #[error("Browser flash geometry does not match the selected Board layout")]
    Geometry,
    /// A region is empty, overlapping, duplicated, or out of bounds.
    #[error("Browser Board layout contains an invalid native region")]
    Range,
}

include!(concat!(env!("OUT_DIR"), "/browser_layout.rs"));

pub(crate) const fn selected_layout() -> FileLayout {
    BOARD_FILE_LAYOUT
}
