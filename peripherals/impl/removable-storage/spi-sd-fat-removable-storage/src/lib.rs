//! FAT removable-filesystem implementation over a microSD card in SPI mode.
//!
//! Most M5Stack slots wire the card's SPI lines, often on the controller the
//! display uses. `embedded-sdmmc`'s `SdCard` speaks the SD SPI protocol; this
//! implementation wakes and identifies the card the way the SD specification
//! asks, which `embedded-hal`'s `SpiDevice` cannot express on its own: at
//! least 74 clock cycles with the chip select released, and identification
//! at no more than 400 kHz, through the Platform's [`SpiDeviceControl`]. It
//! then runs the card at the clock the Board declares and presents it as a
//! byte stream to the FAT slot of `barracuda-sdmmc-fat-removable-storage`,
//! which mounts, watches, and remounts it like a card on an SD host.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::SpiDeviceControl;
use barracuda_peripheral::PeripheralImplementation;
use barracuda_peripheral::removable_storage::{
    RemovableStorage, RemovableStorageEvent, RemovableStorageStatus,
};
use barracuda_sdmmc_fat_removable_storage::{
    SdMmcFatRemovableStorage, SdMmcFatRemovableStorageBindings, SdMmcFatRemovableStorageConfig,
    SdMmcFatRemovableStorageImplementation,
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, spi::SpiDevice};
use embedded_io::{ErrorKind, ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use embedded_sdmmc::{Block, BlockDevice, BlockIdx, SdCard, SdCardError};

/// Bytes in one SD block.
pub const BLOCK_BYTES: usize = 512;
/// The highest identification clock the SD specification allows.
const IDENTIFICATION_FREQUENCY_HZ: u32 = 400_000;
/// Bytes of `0xFF` clocked with the chip select released before
/// identification: 80 cycles, at least the specification's 74.
const WAKE_BYTES: [u8; 10] = [0xFF; 10];

/// Stable slot identity and supply polarity.
pub struct SpiSdConfig {
    slot_id: &'static str,
    power_active_high: bool,
}

impl SpiSdConfig {
    /// Creates configuration for one generated Board peripheral instance.
    #[must_use]
    pub const fn new(slot_id: &'static str, power_active_high: bool) -> Self {
        Self {
            slot_id,
            power_active_high,
        }
    }
}

/// Move-only SPI device, delay, and optional supply switch.
pub struct SpiSdBindings<SPI, DELAY, POWER> {
    spi: SPI,
    delay: DELAY,
    power_enable: Option<POWER>,
}

impl<SPI, DELAY, POWER> SpiSdBindings<SPI, DELAY, POWER> {
    /// Combines the card's SPI device with the delay `embedded-sdmmc` paces
    /// its retries with and the slot's supply switch.
    #[must_use]
    pub const fn new(spi: SPI, delay: DELAY, power_enable: Option<POWER>) -> Self {
        Self {
            spi,
            delay,
            power_enable,
        }
    }
}

/// Block access the byte stream needs from a card.
pub trait SdBlocks {
    /// Card or bus failure.
    type Error: core::fmt::Debug;

    /// Wakes and identifies the card and returns its capacity in bytes.
    fn prepare(&mut self) -> Result<u64, Self::Error>;

    /// Reads one block.
    fn read_block(&mut self, index: u32, block: &mut [u8; BLOCK_BYTES]) -> Result<(), Self::Error>;

    /// Writes one block.
    fn write_block(&mut self, index: u32, block: &[u8; BLOCK_BYTES]) -> Result<(), Self::Error>;
}

/// Card failure seen by [`SpiSdCard`].
#[derive(Debug, Clone)]
pub enum SpiSdCardError {
    /// The SD protocol failed or no card answered.
    Card(SdCardError),
    /// The SPI device rejected a clock change or the wake-up clocks.
    Bus,
}

/// `embedded-sdmmc`'s card driver with the SD wake-up and clock sequence.
pub struct SpiSdCard<SPI, DELAY>
where
    SPI: SpiDevice<u8>,
    DELAY: DelayNs,
{
    card: SdCard<SPI, DELAY>,
    frequency_hz: u32,
}

impl<SPI, DELAY> SpiSdCard<SPI, DELAY>
where
    SPI: SpiDevice<u8> + SpiDeviceControl,
    DELAY: DelayNs,
{
    /// Wraps the card's SPI device; the clock it holds now is the one the
    /// card runs at once identified.
    pub fn new(spi: SPI, delay: DELAY) -> Self {
        let frequency_hz = spi.frequency_hz();
        Self {
            card: SdCard::new(spi, delay),
            frequency_hz,
        }
    }
}

impl<SPI, DELAY> SdBlocks for SpiSdCard<SPI, DELAY>
where
    SPI: SpiDevice<u8> + SpiDeviceControl,
    DELAY: DelayNs,
{
    type Error = SpiSdCardError;

    fn prepare(&mut self) -> Result<u64, Self::Error> {
        self.card
            .spi(|spi| {
                spi.set_frequency_hz(IDENTIFICATION_FREQUENCY_HZ)?;
                spi.write_deselected(&WAKE_BYTES)
            })
            .map_err(|_| SpiSdCardError::Bus)?;
        // A card inserted again is identified from the start.
        self.card.mark_card_uninit();
        let identified = self.card.num_bytes().map_err(SpiSdCardError::Card);
        let frequency_hz = self.frequency_hz;
        self.card
            .spi(|spi| spi.set_frequency_hz(frequency_hz))
            .map_err(|_| SpiSdCardError::Bus)?;
        identified
    }

    fn read_block(&mut self, index: u32, block: &mut [u8; BLOCK_BYTES]) -> Result<(), Self::Error> {
        let mut blocks = [Block::new()];
        self.card
            .read(&mut blocks, BlockIdx(index))
            .map_err(SpiSdCardError::Card)?;
        block.copy_from_slice(&blocks[0].contents);
        Ok(())
    }

    fn write_block(&mut self, index: u32, block: &[u8; BLOCK_BYTES]) -> Result<(), Self::Error> {
        let mut blocks = [Block::new()];
        blocks[0].contents.copy_from_slice(block);
        self.card
            .write(&blocks, BlockIdx(index))
            .map_err(SpiSdCardError::Card)
    }
}

/// Byte-stream access failure.
#[derive(Debug)]
pub enum SpiSdError<CardError> {
    /// The card failed or is gone; the next access identifies it again.
    Card(CardError),
    /// The position is outside the card or its block addresses.
    InvalidSeek,
}

impl<CardError: core::fmt::Debug> core::fmt::Display for SpiSdError<CardError> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Card(error) => write!(formatter, "SD card operation failed: {error:?}"),
            Self::InvalidSeek => formatter.write_str("invalid SD card seek position"),
        }
    }
}

impl<CardError: core::fmt::Debug> core::error::Error for SpiSdError<CardError> {}

impl<CardError: core::fmt::Debug> embedded_io::Error for SpiSdError<CardError> {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Card(_) => ErrorKind::Other,
            Self::InvalidSeek => ErrorKind::InvalidInput,
        }
    }
}

/// A card presented as a byte-addressed stream through one block buffer.
pub struct SdByteStream<CARD> {
    card: CARD,
    block: [u8; BLOCK_BYTES],
    ready: bool,
    position: u64,
    capacity: u64,
}

impl<CARD: SdBlocks> SdByteStream<CARD> {
    /// Wraps a card; it is identified on first access.
    pub const fn new(card: CARD) -> Self {
        Self {
            card,
            block: [0; BLOCK_BYTES],
            ready: false,
            position: 0,
            capacity: 0,
        }
    }

    fn ensure_ready(&mut self) -> Result<(), SpiSdError<CARD::Error>> {
        if !self.ready {
            self.capacity = self.card.prepare().map_err(SpiSdError::Card)?;
            self.position = 0;
            self.ready = true;
        }
        Ok(())
    }

    fn fail(&mut self, error: CARD::Error) -> SpiSdError<CARD::Error> {
        self.ready = false;
        SpiSdError::Card(error)
    }

    /// Returns the block holding the current position and the offset in it.
    fn locate(&self) -> Result<(u32, usize), SpiSdError<CARD::Error>> {
        let index = u32::try_from(self.position / BLOCK_BYTES as u64)
            .map_err(|_| SpiSdError::InvalidSeek)?;
        Ok((index, (self.position % BLOCK_BYTES as u64) as usize))
    }

    fn available(&self, requested: usize) -> usize {
        let remaining = self.capacity.saturating_sub(self.position);
        requested.min(usize::try_from(remaining).unwrap_or(usize::MAX))
    }
}

impl<CARD: SdBlocks> ErrorType for SdByteStream<CARD> {
    type Error = SpiSdError<CARD::Error>;
}

impl<CARD: SdBlocks> Read for SdByteStream<CARD> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.ensure_ready()?;
        let length = self.available(buffer.len());
        let mut copied = 0;
        while copied < length {
            let (index, offset) = self.locate()?;
            let count = (BLOCK_BYTES - offset).min(length - copied);
            if let Err(error) = self.card.read_block(index, &mut self.block) {
                return Err(self.fail(error));
            }
            buffer[copied..copied + count].copy_from_slice(&self.block[offset..offset + count]);
            self.position += count as u64;
            copied += count;
        }
        Ok(copied)
    }
}

impl<CARD: SdBlocks> Write for SdByteStream<CARD> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.ensure_ready()?;
        let length = self.available(buffer.len());
        let mut copied = 0;
        while copied < length {
            let (index, offset) = self.locate()?;
            let count = (BLOCK_BYTES - offset).min(length - copied);
            // A partial block keeps the bytes around the written range.
            if (offset != 0 || count != BLOCK_BYTES)
                && let Err(error) = self.card.read_block(index, &mut self.block)
            {
                return Err(self.fail(error));
            }
            self.block[offset..offset + count].copy_from_slice(&buffer[copied..copied + count]);
            if let Err(error) = self.card.write_block(index, &self.block) {
                return Err(self.fail(error));
            }
            self.position += count as u64;
            copied += count;
        }
        Ok(copied)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<CARD: SdBlocks> Seek for SdByteStream<CARD> {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.ensure_ready()?;
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.capacity) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        let target = u64::try_from(target).map_err(|_| SpiSdError::InvalidSeek)?;
        if target > self.capacity {
            return Err(SpiSdError::InvalidSeek);
        }
        self.position = target;
        Ok(target)
    }
}

/// The FAT slot over an SPI-mode card.
pub type SpiSdFatRemovableStorage<SPI, DELAY> =
    SdMmcFatRemovableStorage<SdByteStream<SpiSdCard<SPI, DELAY>>>;

/// Initialized slot and the supply switch it keeps on.
pub struct SpiSdRemovableStorage<SPI, DELAY, POWER>
where
    SPI: SpiDevice<u8>,
    DELAY: DelayNs,
{
    slot: SpiSdFatRemovableStorage<SPI, DELAY>,
    _power_enable: Option<POWER>,
}

impl<SPI, DELAY, POWER> RemovableStorage for SpiSdRemovableStorage<SPI, DELAY, POWER>
where
    SPI: SpiDevice<u8> + SpiDeviceControl + Send + 'static,
    DELAY: DelayNs + Send + 'static,
    POWER: Send + 'static,
{
    fn slot_id(&self) -> &'static str {
        self.slot.slot_id()
    }

    fn status(&self) -> RemovableStorageStatus {
        self.slot.status()
    }

    async fn next_event(&mut self) -> RemovableStorageEvent {
        self.slot.next_event().await
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum SpiSdInitError<PowerError> {
    /// The slot's supply switch could not be driven.
    Power(PowerError),
}

/// Bindings carried only at the type level by the static factory.
type Parts<SPI, DELAY, POWER> = PhantomData<fn() -> (SPI, DELAY, POWER)>;

/// Static factory used by generated Board composition.
pub struct SpiSdFatRemovableStorageImplementation<SPI, DELAY, POWER>(Parts<SPI, DELAY, POWER>);

impl<SPI, DELAY, POWER> PeripheralImplementation
    for SpiSdFatRemovableStorageImplementation<SPI, DELAY, POWER>
where
    SPI: SpiDevice<u8> + SpiDeviceControl + Send + 'static,
    DELAY: DelayNs + Send + 'static,
    POWER: OutputPin + Send + 'static,
{
    type Bindings = SpiSdBindings<SPI, DELAY, POWER>;
    type Config = SpiSdConfig;
    type Peripheral = SpiSdRemovableStorage<SPI, DELAY, POWER>;
    type Error = SpiSdInitError<POWER::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if let Some(power) = bindings.power_enable.as_mut() {
            power
                .set_state(config.power_active_high.into())
                .map_err(SpiSdInitError::Power)?;
        }
        let stream = SdByteStream::new(SpiSdCard::new(bindings.spi, bindings.delay));
        let Ok(slot) = SdMmcFatRemovableStorageImplementation::initialize(
            SdMmcFatRemovableStorageBindings::new(stream),
            SdMmcFatRemovableStorageConfig::new(config.slot_id),
        )
        .await;
        Ok(SpiSdRemovableStorage {
            slot,
            _power_enable: bindings.power_enable,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    extern crate std;

    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// An in-memory card of `blocks` blocks that counts its preparations.
    struct MemoryCard {
        data: Vec<u8>,
        prepared: usize,
        fail_next: bool,
    }

    impl MemoryCard {
        fn new(blocks: usize) -> Self {
            Self {
                data: (0..blocks * BLOCK_BYTES).map(|byte| byte as u8).collect(),
                prepared: 0,
                fail_next: false,
            }
        }
    }

    impl SdBlocks for MemoryCard {
        type Error = ();

        fn prepare(&mut self) -> Result<u64, ()> {
            self.prepared += 1;
            Ok(self.data.len() as u64)
        }

        fn read_block(&mut self, index: u32, block: &mut [u8; BLOCK_BYTES]) -> Result<(), ()> {
            if core::mem::take(&mut self.fail_next) {
                return Err(());
            }
            let start = index as usize * BLOCK_BYTES;
            block.copy_from_slice(&self.data[start..start + BLOCK_BYTES]);
            Ok(())
        }

        fn write_block(&mut self, index: u32, block: &[u8; BLOCK_BYTES]) -> Result<(), ()> {
            let start = index as usize * BLOCK_BYTES;
            self.data[start..start + BLOCK_BYTES].copy_from_slice(block);
            Ok(())
        }
    }

    fn run<T>(future: impl core::future::Future<Output = T>) -> T {
        embassy_futures::block_on(future)
    }

    #[test]
    fn reads_across_block_boundaries() {
        let mut stream = SdByteStream::new(MemoryCard::new(4));
        run(stream.seek(SeekFrom::Start(510))).unwrap();
        let mut bytes = [0; 4];
        assert_eq!(run(stream.read(&mut bytes)).unwrap(), 4);
        assert_eq!(bytes, [254, 255, 0, 1]);
        assert_eq!(stream.card.prepared, 1);
    }

    #[test]
    fn partial_writes_keep_neighboring_bytes() {
        let mut stream = SdByteStream::new(MemoryCard::new(2));
        run(stream.seek(SeekFrom::Start(511))).unwrap();
        assert_eq!(run(stream.write(&[0xAA, 0xBB])).unwrap(), 2);
        assert_eq!(stream.card.data[510..514], [254, 0xAA, 0xBB, 1]);
    }

    #[test]
    fn stops_at_the_card_end_and_rejects_seeks_past_it() {
        let mut stream = SdByteStream::new(MemoryCard::new(1));
        run(stream.seek(SeekFrom::End(-2))).unwrap();
        let mut bytes = vec![0; 8];
        assert_eq!(run(stream.read(&mut bytes)).unwrap(), 2);
        assert!(matches!(
            run(stream.seek(SeekFrom::Start(513))),
            Err(SpiSdError::InvalidSeek)
        ));
    }

    #[test]
    fn identifies_the_card_again_after_a_failure() {
        let mut stream = SdByteStream::new(MemoryCard::new(1));
        let mut byte = [0; 1];
        run(stream.read(&mut byte)).unwrap();
        stream.card.fail_next = true;
        assert!(matches!(
            run(stream.read(&mut byte)),
            Err(SpiSdError::Card(()))
        ));
        run(stream.read(&mut byte)).unwrap();
        assert_eq!(stream.card.prepared, 2);
    }
}
