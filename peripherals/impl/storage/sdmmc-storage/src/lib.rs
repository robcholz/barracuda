//! Removable-storage implementation over a Platform-owned SDMMC byte device.

#![no_std]

use core::marker::PhantomData;

use barracuda_peripheral::{PeripheralImplementation, storage::Storage};
use embedded_io::{ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};

/// Move-only SDMMC device consumed by this implementation.
pub struct SdMmcStorageBindings<DEVICE> {
    device: DEVICE,
}

impl<DEVICE> SdMmcStorageBindings<DEVICE> {
    /// Wraps the Platform SDMMC data plane.
    #[must_use]
    pub const fn new(device: DEVICE) -> Self {
        Self { device }
    }
}

/// Initialized removable SD card.
pub struct SdMmcStorage<DEVICE> {
    device: DEVICE,
}

impl<DEVICE: ErrorType> ErrorType for SdMmcStorage<DEVICE> {
    type Error = DEVICE::Error;
}

impl<DEVICE: Read + ErrorType> Read for SdMmcStorage<DEVICE> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.device.read(buffer).await
    }
}

impl<DEVICE: Write + ErrorType> Write for SdMmcStorage<DEVICE> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.device.write(buffer).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.device.flush().await
    }
}

impl<DEVICE: Seek + ErrorType> Seek for SdMmcStorage<DEVICE> {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.device.seek(position).await
    }
}

impl<DEVICE> Storage for SdMmcStorage<DEVICE>
where
    DEVICE: Storage,
{
    fn capacity(&self) -> u64 {
        self.device.capacity()
    }
}

/// Static factory used by generated Board composition.
pub struct SdMmcStorageImplementation<DEVICE>(PhantomData<DEVICE>);

impl<DEVICE> PeripheralImplementation for SdMmcStorageImplementation<DEVICE>
where
    DEVICE: Storage,
{
    type Bindings = SdMmcStorageBindings<DEVICE>;
    type Config = ();
    type Peripheral = SdMmcStorage<DEVICE>;
    type Error = core::convert::Infallible;

    async fn initialize(
        bindings: Self::Bindings,
        (): Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Ok(SdMmcStorage {
            device: bindings.device,
        })
    }
}
