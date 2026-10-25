//! SD memory card protocol over one slot of an SD host.
//!
//! The `sdmmc-device` binding hands a peripheral implementation a card as a
//! byte device: `embedded_io_async` reads, writes and seeks over the card's
//! blocks. This module drives any [`MmcBus`] that way; on the ESP32-P4 the bus
//! is slot 0 of the SD host the Platform owns.
//!
//! # Sharing the SD host
//!
//! Both slots of the ESP32-P4's SD host share one transfer engine. esp-hal
//! serializes the slots with an async mutex that each command and each data
//! transfer takes and releases, first come first served, with no priority
//! between the slots. The card therefore never holds the engine for long:
//! every command goes out on its own, and one data transfer moves at most
//! [`TRANSFER_BLOCKS`] blocks (4 KiB, about 0.2 ms at 40 MHz on four lines).
//! A Wi-Fi co-processor on the other slot waits at most that long for one
//! card transfer, however large the file a Plugin reads or writes.

use aligned::{Aligned, A4};
use embedded_hal_async::delay::DelayNs;
use embedded_io::{ErrorKind, ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use sdio::{
    common,
    sd::{self, BlockSize, CSD, OCR, SCR, SD},
    BlockCommand, BlockReadCommand, BlockWriteCommand, BusWidth, Command, MmcBus, MmcError, R1b,
    R1,
};

/// Bytes in one card block.
pub const BLOCK_BYTES: usize = 512;
/// Most blocks one data transfer moves, which bounds how long the card holds
/// the SD host's shared transfer engine.
pub const TRANSFER_BLOCKS: usize = 8;
const TRANSFER_BYTES: usize = BLOCK_BYTES * TRANSFER_BLOCKS;
/// The card clock while the card is identified (SD Physical Layer 4.2).
pub const IDENTIFICATION_FREQUENCY_HZ: u32 = 400_000;
/// The highest clock of the default bus speed mode.
pub const DEFAULT_SPEED_HZ: u32 = 25_000_000;
/// ACMD41 polls while the card powers up: up to one second, as the SD
/// Physical Layer allows.
const POWER_UP_ATTEMPTS: usize = 100;
const POWER_UP_POLL_MS: u32 = 10;
/// CMD13 polls after a write until the card is ready for data again.
const READY_ATTEMPTS: usize = 750;
const READY_POLL_MS: u32 = 1;
/// CMD6 argument that switches function group 1 (access mode) to high speed
/// and leaves the other groups as they are.
const SWITCH_HIGH_SPEED: u32 = 0x80FF_FFF1;
/// Byte of the CMD6 status that holds the function group 1 selection in its
/// low nibble (status bits 379:376).
const SWITCH_SELECTION_BYTE: usize = 16;
const HIGH_SPEED_FUNCTION: u8 = 1;

/// The card as identified: what addressing it uses and how large it is.
#[derive(Clone, Copy, Debug)]
struct Card {
    rca: u16,
    high_capacity: bool,
    capacity: u64,
}

impl Card {
    /// Turns a block number into the card's address: SDHC and SDXC cards
    /// address blocks, SDSC cards bytes.
    fn address(&self, block: u32) -> Result<u32, SdCardError> {
        if self.high_capacity {
            Ok(block)
        } else {
            block
                .checked_mul(BLOCK_BYTES as u32)
                .ok_or(SdCardError::InvalidSeek)
        }
    }
}

/// An SD memory card in one slot, read and written as bytes.
///
/// The card is identified on first use and again after any failure, so a
/// card that is removed and inserted again is picked up by the next access.
pub struct SdCard<B, D> {
    bus: B,
    delay: D,
    four_bit: bool,
    frequency_hz: u32,
    buffer: Aligned<A4, [u8; TRANSFER_BYTES]>,
    card: Option<Card>,
    position: u64,
}

/// An SD card access failed.
#[derive(Debug)]
pub enum SdCardError {
    /// A command or transfer failed, or the card refused it; a missing card
    /// times out.
    Card(MmcError),
    /// The requested seek position is outside the card or its address space.
    InvalidSeek,
}

impl From<MmcError> for SdCardError {
    fn from(error: MmcError) -> Self {
        Self::Card(error)
    }
}

impl core::fmt::Display for SdCardError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Card(error) => write!(formatter, "SD card operation failed: {error:?}"),
            Self::InvalidSeek => formatter.write_str("invalid SD card seek position"),
        }
    }
}

impl core::error::Error for SdCardError {}

impl embedded_io::Error for SdCardError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidSeek => ErrorKind::InvalidInput,
            Self::Card(MmcError::Timeout) => ErrorKind::TimedOut,
            Self::Card(_) => ErrorKind::Other,
        }
    }
}

/// CMD17 (one block) or CMD18 (several) into `buffer`.
struct ReadBlocks<'a, const INDEX: u8> {
    address: u32,
    buffer: &'a mut Aligned<A4, [u8]>,
}

impl<const INDEX: u8> Command for ReadBlocks<'_, INDEX> {
    const INDEX: u8 = INDEX;
    type Resp<'b>
        = R1
    where
        Self: 'b;

    fn arg(&self) -> u32 {
        self.address
    }
}

impl<const INDEX: u8> BlockCommand for ReadBlocks<'_, INDEX> {
    fn block_size(&self) -> BlockSize {
        BlockSize::B512
    }

    fn block_count(&self) -> u32 {
        (self.buffer.len() / BLOCK_BYTES) as u32
    }
}

impl<const INDEX: u8> BlockReadCommand for ReadBlocks<'_, INDEX> {
    fn buf(&mut self) -> &mut Aligned<A4, [u8]> {
        self.buffer
    }
}

/// CMD24 (one block) or CMD25 (several) from `buffer`.
struct WriteBlocks<'a, const INDEX: u8> {
    address: u32,
    buffer: &'a Aligned<A4, [u8]>,
}

impl<const INDEX: u8> Command for WriteBlocks<'_, INDEX> {
    const INDEX: u8 = INDEX;
    type Resp<'b>
        = R1b
    where
        Self: 'b;

    fn arg(&self) -> u32 {
        self.address
    }
}

impl<const INDEX: u8> BlockCommand for WriteBlocks<'_, INDEX> {
    fn block_size(&self) -> BlockSize {
        BlockSize::B512
    }

    fn block_count(&self) -> u32 {
        (self.buffer.len() / BLOCK_BYTES) as u32
    }
}

impl<const INDEX: u8> BlockWriteCommand for WriteBlocks<'_, INDEX> {
    fn buf(&self) -> &Aligned<A4, [u8]> {
        self.buffer
    }
}

/// How many blocks a transfer starting `offset` bytes into its first block
/// spans to cover `bytes`, at most [`TRANSFER_BLOCKS`].
#[must_use]
pub const fn transfer_blocks(offset: usize, bytes: usize) -> usize {
    let spanned = (offset + bytes).div_ceil(BLOCK_BYTES);
    if spanned < TRANSFER_BLOCKS {
        spanned
    } else {
        TRANSFER_BLOCKS
    }
}

impl<B: MmcBus, D: DelayNs> SdCard<B, D> {
    /// Wraps the slot a card sits in; the card is identified on first use.
    ///
    /// `four_bit` is the Board's bus width, `frequency_hz` the clock it runs
    /// the card at once identified. A card that does not offer four lines or
    /// high speed runs on one line or at [`DEFAULT_SPEED_HZ`] instead.
    #[must_use]
    pub const fn new(bus: B, delay: D, four_bit: bool, frequency_hz: u32) -> Self {
        Self {
            bus,
            delay,
            four_bit,
            frequency_hz,
            buffer: Aligned([0; TRANSFER_BYTES]),
            card: None,
            position: 0,
        }
    }

    /// Returns the card's size in bytes, or `None` before it is identified.
    #[must_use]
    pub fn capacity(&self) -> Option<u64> {
        self.card.map(|card| card.capacity)
    }

    /// Returns the bus the card sits on.
    #[must_use]
    pub const fn bus(&self) -> &B {
        &self.bus
    }

    fn forget_card(&mut self) {
        self.card = None;
        self.position = 0;
    }

    async fn app_command<'a, C>(&mut self, rca: u16, command: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: sdio::ControlCommand + 'a,
    {
        self.bus
            .send_command(common::app_cmd(rca))
            .await?
            .to_result()?;
        self.bus.send_command(command).await
    }

    async fn ensure_identified(&mut self) -> Result<Card, SdCardError> {
        if let Some(card) = self.card {
            return Ok(card);
        }
        let card = self.identify().await?;
        self.card = Some(card);
        Ok(card)
    }

    /// Brings the card from power-up to the transfer state at the Board's
    /// bus width and clock (SD Physical Layer 4.2 and 4.3).
    async fn identify(&mut self) -> Result<Card, MmcError> {
        self.bus.init_idle(IDENTIFICATION_FREQUENCY_HZ).await?;
        // 74 clocks at 400 kHz before the first command.
        self.delay.delay_ms(1).await;
        self.bus.send_command(common::idle()).await?;

        let condition = self.bus.send_command(sd::send_if_cond(1, 0xAA)).await?;
        if condition.check_pattern != 0xAA || condition.voltage & 1 == 0 {
            return Err(MmcError::Voltage);
        }

        let mut ocr = None;
        for _ in 0..POWER_UP_ATTEMPTS {
            let response = self
                .app_command(0, sd::sd_send_op_cond(true, false, false, 1 << 5))
                .await?;
            let candidate = OCR::<SD>::from(response);
            if !candidate.is_busy() {
                ocr = Some(candidate);
                break;
            }
            self.delay.delay_ms(POWER_UP_POLL_MS).await;
        }
        let ocr = ocr.ok_or(MmcError::Timeout)?;

        self.bus.send_command(common::all_send_cid()).await?;
        let rca = self
            .bus
            .send_command(sd::send_relative_address())
            .await?
            .rca;
        let csd = CSD::<SD>::from(self.bus.send_command(common::send_csd(rca)).await?);
        let capacity = csd.card_size();
        if capacity == 0 {
            return Err(MmcError::CardType);
        }
        self.bus
            .send_command(common::select_card(rca))
            .await?
            .to_result()?;

        let mut width = BusWidth::W1;
        if self.four_bit {
            let mut scr = SCR::new();
            self.bus
                .send_command(common::app_cmd(rca))
                .await?
                .to_result()?;
            self.bus
                .read_blocks(sd::send_scr(&mut scr), false)
                .await?
                .to_result()?;
            if scr.bus_width_four() {
                self.app_command(rca, sd::set_bus_width(true))
                    .await?
                    .to_result()?;
                width = BusWidth::W4;
            }
        }

        let target = self.frequency_hz.min(self.bus.supports_frequency());
        self.bus.set_bus(width, target.min(DEFAULT_SPEED_HZ))?;
        if target > DEFAULT_SPEED_HZ && self.switch_to_high_speed().await {
            self.bus.set_bus(width, target)?;
        }

        self.bus
            .send_command(common::set_block_length(BLOCK_BYTES as u32))
            .await?
            .to_result()?;
        Ok(Card {
            rca,
            high_capacity: ocr.high_capacity(),
            capacity,
        })
    }

    /// Asks the card for the high-speed access mode (CMD6); a card that
    /// predates it, or refuses, stays at the default speed.
    async fn switch_to_high_speed(&mut self) -> bool {
        let mut status = Aligned([0_u8; 64]);
        let switched = self
            .bus
            .read_blocks(sd::cmd6(SWITCH_HIGH_SPEED, &mut status), false)
            .await
            .is_ok_and(|response| response.to_result().is_ok());
        if switched {
            // The card takes the new mode 8 clocks after the command.
            self.delay.delay_us(1).await;
        }
        switched && status[SWITCH_SELECTION_BYTE] & 0x0F == HIGH_SPEED_FUNCTION
    }

    /// Reads `blocks` blocks from `block` into the buffer at block `at`.
    async fn read_into(
        &mut self,
        card: Card,
        block: u32,
        at: usize,
        blocks: usize,
    ) -> Result<(), SdCardError> {
        let address = card.address(block)?;
        let buffer: &mut Aligned<A4, [u8]> = &mut self.buffer;
        let buffer = &mut buffer[at * BLOCK_BYTES..(at + blocks) * BLOCK_BYTES];
        let response = if blocks == 1 {
            self.bus
                .read_blocks(ReadBlocks::<17> { address, buffer }, false)
                .await?
        } else {
            let auto_stop = self.bus.supports_auto_stop();
            let response = self
                .bus
                .read_blocks(ReadBlocks::<18> { address, buffer }, auto_stop)
                .await?;
            if !auto_stop {
                self.bus.send_command(common::stop_transmission()).await?;
            }
            response
        };
        response.to_result()?;
        Ok(())
    }

    /// Writes the buffer's first `blocks` blocks to `block`, then waits until
    /// the card has programmed them.
    async fn write_from(
        &mut self,
        card: Card,
        block: u32,
        blocks: usize,
    ) -> Result<(), SdCardError> {
        let address = card.address(block)?;
        let buffer: &Aligned<A4, [u8]> = &self.buffer;
        let buffer = &buffer[..blocks * BLOCK_BYTES];
        let response = if blocks == 1 {
            self.bus
                .write_blocks(WriteBlocks::<24> { address, buffer }, false)
                .await?
        } else {
            let auto_stop = self.bus.supports_auto_stop();
            let response = self
                .bus
                .write_blocks(WriteBlocks::<25> { address, buffer }, auto_stop)
                .await?;
            if !auto_stop {
                self.bus.send_command(common::stop_transmission()).await?;
            }
            response
        };
        response.to_response().to_result()?;
        self.wait_ready(card).await
    }

    /// Polls the card's status (CMD13) until it is ready for data. Each poll
    /// takes the SD host only for its own command.
    async fn wait_ready(&mut self, card: Card) -> Result<(), SdCardError> {
        for _ in 0..READY_ATTEMPTS {
            let status = self
                .bus
                .send_command(common::card_status(card.rca, false))
                .await?;
            status.to_result()?;
            if status.ready_for_data() {
                return Ok(());
            }
            self.delay.delay_ms(READY_POLL_MS).await;
        }
        Err(SdCardError::Card(MmcError::Timeout))
    }

    /// The block and in-block offset of the current position.
    fn here(&self) -> Result<(u32, usize), SdCardError> {
        let block = u32::try_from(self.position / BLOCK_BYTES as u64)
            .map_err(|_| SdCardError::InvalidSeek)?;
        Ok((block, (self.position % BLOCK_BYTES as u64) as usize))
    }

    /// Bytes from the position to the end of the card, capped at `wanted`.
    fn available(&self, card: Card, wanted: usize) -> usize {
        let left = card.capacity.saturating_sub(self.position);
        wanted.min(usize::try_from(left).unwrap_or(usize::MAX))
    }

    async fn read_bytes(&mut self, buffer: &mut [u8]) -> Result<usize, SdCardError> {
        let card = self.ensure_identified().await?;
        let length = self.available(card, buffer.len());
        let mut copied = 0;
        while copied < length {
            let (block, offset) = self.here()?;
            let blocks = transfer_blocks(offset, length - copied);
            self.read_into(card, block, 0, blocks).await?;
            let count = (blocks * BLOCK_BYTES - offset).min(length - copied);
            buffer[copied..copied + count].copy_from_slice(&self.buffer[offset..offset + count]);
            self.position += count as u64;
            copied += count;
        }
        Ok(copied)
    }

    async fn write_bytes(&mut self, buffer: &[u8]) -> Result<usize, SdCardError> {
        let card = self.ensure_identified().await?;
        let length = self.available(card, buffer.len());
        let mut copied = 0;
        while copied < length {
            let (block, offset) = self.here()?;
            let blocks = transfer_blocks(offset, length - copied);
            let count = (blocks * BLOCK_BYTES - offset).min(length - copied);
            // Blocks the write covers only in part keep their other bytes.
            if offset != 0 {
                self.read_into(card, block, 0, 1).await?;
            }
            let last = blocks - 1;
            if !(offset + count).is_multiple_of(BLOCK_BYTES) && (last != 0 || offset == 0) {
                self.read_into(card, block + last as u32, last, 1).await?;
            }
            self.buffer[offset..offset + count].copy_from_slice(&buffer[copied..copied + count]);
            self.write_from(card, block, blocks).await?;
            self.position += count as u64;
            copied += count;
        }
        Ok(copied)
    }

    async fn seek_to(&mut self, position: SeekFrom) -> Result<u64, SdCardError> {
        let card = self.ensure_identified().await?;
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(card.capacity) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        let target = u64::try_from(target).map_err(|_| SdCardError::InvalidSeek)?;
        if target > card.capacity {
            return Err(SdCardError::InvalidSeek);
        }
        self.position = target;
        Ok(target)
    }

    /// Forgets the card after a failure other than a bad argument, so the
    /// next access identifies whatever card the slot holds then.
    fn settle<T>(&mut self, result: Result<T, SdCardError>) -> Result<T, SdCardError> {
        if let Err(SdCardError::Card(_)) = &result {
            self.forget_card();
        }
        result
    }
}

impl<B, D> ErrorType for SdCard<B, D> {
    type Error = SdCardError;
}

impl<B: MmcBus, D: DelayNs> Read for SdCard<B, D> {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        let result = self.read_bytes(buffer).await;
        self.settle(result)
    }
}

impl<B: MmcBus, D: DelayNs> Write for SdCard<B, D> {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let result = self.write_bytes(buffer).await;
        self.settle(result)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<B: MmcBus, D: DelayNs> Seek for SdCard<B, D> {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        let result = self.seek_to(position).await;
        self.settle(result)
    }
}
