//! Private adapter from a validated NOR partition to `ekv` pages.

use core::fmt::Debug;

use ekv::flash::{Flash, PageID};
use embedded_storage_async::nor_flash::NorFlash;

use crate::{ALIGN, ERASE_VALUE, MAX_PAGE_COUNT, PAGE_SIZE};

/// Invalid partition geometry for the compiled database format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FlashGeometryError {
    /// One of the reported operation granularities is zero.
    #[error("partition reported a zero read, write, or erase granularity")]
    ZeroGranularity,
    /// Database operations cannot satisfy the partition read granularity.
    #[error("database alignment {database} is incompatible with partition read size {partition}")]
    ReadAlignment {
        /// Database alignment.
        database: usize,
        /// Partition read size.
        partition: usize,
    },
    /// Database operations cannot satisfy the partition write granularity.
    #[error("database alignment {database} is incompatible with partition write size {partition}")]
    WriteAlignment {
        /// Database alignment.
        database: usize,
        /// Partition write size.
        partition: usize,
    },
    /// One database page cannot be erased independently.
    #[error("database page size {page} is incompatible with partition erase size {erase}")]
    EraseAlignment {
        /// Database page size.
        page: usize,
        /// Partition erase size.
        erase: usize,
    },
    /// Partition capacity is not an exact number of database pages.
    #[error("partition capacity {capacity} is not a multiple of database page size {page}")]
    CapacityAlignment {
        /// Partition capacity.
        capacity: usize,
        /// Database page size.
        page: usize,
    },
    /// The database format cannot address this page count.
    #[error("partition has {actual} pages; database supports 2..={maximum}")]
    PageCount {
        /// Actual page count.
        actual: usize,
        /// Maximum compiled into the database format.
        maximum: usize,
    },
    /// The partition cannot be addressed by the `u32` NOR API.
    #[error("partition capacity {capacity} exceeds the u32 address space")]
    AddressSpace {
        /// Partition capacity.
        capacity: usize,
    },
    /// The database format expects an incompatible erased byte.
    #[error("database erase value {actual:#04x} is incompatible with NOR flash")]
    EraseValue {
        /// Compiled database erase value.
        actual: u8,
    },
}

#[derive(Debug)]
pub(crate) enum FlashAdapterError<E: Debug> {
    Flash(E),
    AddressOverflow,
}

pub(crate) struct NorFlashAdapter<F: NorFlash> {
    flash: F,
    page_count: usize,
    capacity: usize,
}

impl<F: NorFlash> NorFlashAdapter<F> {
    pub(crate) fn new(flash: F) -> Result<Self, FlashGeometryError> {
        validate::<F>(flash.capacity())?;
        let capacity = flash.capacity();
        let page_count = capacity
            .checked_div(PAGE_SIZE)
            .ok_or(FlashGeometryError::ZeroGranularity)?;
        Ok(Self {
            flash,
            page_count,
            capacity,
        })
    }

    pub(crate) async fn is_erased(&mut self) -> Result<bool, F::Error> {
        let mut offset = 0usize;
        let mut bytes = [0u8; ALIGN];
        while offset < self.capacity {
            let address = match u32::try_from(offset) {
                Ok(address) => address,
                Err(_) => return Ok(false),
            };
            self.flash.read(address, &mut bytes).await?;
            if bytes.iter().any(|byte| *byte != 0xff) {
                return Ok(false);
            }
            offset = match offset.checked_add(bytes.len()) {
                Some(next) => next,
                None => return Ok(false),
            };
        }
        Ok(true)
    }

    fn address(page_id: PageID, offset: usize) -> Result<u32, FlashAdapterError<F::Error>> {
        page_id
            .index()
            .checked_mul(PAGE_SIZE)
            .and_then(|page| page.checked_add(offset))
            .and_then(|address| u32::try_from(address).ok())
            .ok_or(FlashAdapterError::AddressOverflow)
    }
}

impl<F> Flash for NorFlashAdapter<F>
where
    F: NorFlash,
    F::Error: Debug,
{
    type Error = FlashAdapterError<F::Error>;

    fn page_count(&self) -> usize {
        self.page_count
    }

    async fn erase(&mut self, page_id: PageID) -> Result<(), Self::Error> {
        let from = Self::address(page_id, 0)?;
        let to = Self::address(page_id, PAGE_SIZE)?;
        self.flash
            .erase(from, to)
            .await
            .map_err(FlashAdapterError::Flash)
    }

    async fn read(
        &mut self,
        page_id: PageID,
        offset: usize,
        data: &mut [u8],
    ) -> Result<(), Self::Error> {
        let address = Self::address(page_id, offset)?;
        self.flash
            .read(address, data)
            .await
            .map_err(FlashAdapterError::Flash)
    }

    async fn write(
        &mut self,
        page_id: PageID,
        offset: usize,
        data: &[u8],
    ) -> Result<(), Self::Error> {
        let address = Self::address(page_id, offset)?;
        self.flash
            .write(address, data)
            .await
            .map_err(FlashAdapterError::Flash)
    }
}

fn validate<F: NorFlash>(capacity: usize) -> Result<(), FlashGeometryError> {
    if F::READ_SIZE == 0 || F::WRITE_SIZE == 0 || F::ERASE_SIZE == 0 {
        return Err(FlashGeometryError::ZeroGranularity);
    }
    if ALIGN.checked_rem(F::READ_SIZE) != Some(0) {
        return Err(FlashGeometryError::ReadAlignment {
            database: ALIGN,
            partition: F::READ_SIZE,
        });
    }
    if ALIGN.checked_rem(F::WRITE_SIZE) != Some(0) {
        return Err(FlashGeometryError::WriteAlignment {
            database: ALIGN,
            partition: F::WRITE_SIZE,
        });
    }
    if PAGE_SIZE.checked_rem(F::ERASE_SIZE) != Some(0) {
        return Err(FlashGeometryError::EraseAlignment {
            page: PAGE_SIZE,
            erase: F::ERASE_SIZE,
        });
    }
    if capacity.checked_rem(PAGE_SIZE) != Some(0) {
        return Err(FlashGeometryError::CapacityAlignment {
            capacity,
            page: PAGE_SIZE,
        });
    }
    let pages = capacity
        .checked_div(PAGE_SIZE)
        .ok_or(FlashGeometryError::ZeroGranularity)?;
    if !(2..=MAX_PAGE_COUNT).contains(&pages) {
        return Err(FlashGeometryError::PageCount {
            actual: pages,
            maximum: MAX_PAGE_COUNT,
        });
    }
    if u32::try_from(capacity).is_err() {
        return Err(FlashGeometryError::AddressSpace { capacity });
    }
    if ERASE_VALUE != 0xff {
        return Err(FlashGeometryError::EraseValue {
            actual: ERASE_VALUE,
        });
    }
    Ok(())
}
