//! The SDIO transport to an ESP-Hosted co-processor.
//!
//! The co-processor is an ESP SDIO slave: function 1 exposes its SLC host
//! registers at addresses below 0x400 and its packet FIFO at addresses ending
//! at 0x1F800. This follows ESP-Hosted-MCU `host/drivers/transport/sdio/`
//! (`sdio_reg.h` for the register map, `sdio_drv.c` for the flow) and the
//! card set-up in `host/port/esp/freertos/src/sdio_wrapper.c`:
//!
//! 1. enable function 1, wait until it reports ready, enable its interrupt,
//!    and set the block size of functions 0 and 1 to 512 bytes;
//! 2. write `ESP_OPEN_DATA_PATH` to scratch register 7 so the co-processor
//!    starts sending;
//! 3. on each card interrupt read the interrupt and packet-length registers
//!    together, clear the interrupts, and read the new bytes, which end at
//!    0x1F800;
//! 4. before writing, count the co-processor's free receive buffers from its
//!    token register, then write the packet so it ends at 0x1F800.

use aligned::{A4, Aligned};
use core::fmt;
use embedded_hal_async::delay::DelayNs;
use sdio::sdio::SdioCard;
use sdio::{MmcBus, MmcError};

/// The SDIO function the co-processor's registers and FIFO live on.
pub const FUNCTION: u8 = 1;
/// SDIO block size the host and co-processor agree on (`ESP_BLOCK_SIZE`).
pub const BLOCK_SIZE: usize = 512;
/// Size of one co-processor receive buffer, the transmit credit unit
/// (`ESP_RX_BUFFER_SIZE`).
pub const CREDIT_BYTES: u32 = 1536;

/// SLC host register addresses, `ESP_SLAVE_SLCHOST_BASE + offset` masked to
/// the 10-bit function-1 register window (`ESP_ADDRESS_MASK`).
pub mod register {
    /// `ESP_SLAVE_TOKEN_RDATA`: free receive buffers in bits 16-27.
    pub const TOKEN_RDATA: u32 = 0x044;
    /// `ESP_SLAVE_INT_RAW_REG`: pending co-processor interrupts.
    pub const INT_RAW: u32 = 0x050;
    /// `ESP_SLAVE_PACKET_LEN_REG`: bytes queued toward the host, cumulative.
    pub const PACKET_LEN: u32 = 0x060;
    /// `ESP_SLAVE_SCRATCH_REG_7`: the host-to-co-processor interrupt.
    pub const HOST_TO_SLAVE_INTERRUPT: u32 = 0x08c;
    /// `ESP_SLAVE_INT_CLR_REG`: write 1s to clear interrupts.
    pub const INT_CLEAR: u32 = 0x0d4;
}

/// Packet-FIFO address every transfer ends at (`ESP_SLAVE_CMD53_END_ADDR`).
pub const FIFO_END: u32 = 0x1f800;

/// Interrupt bit: a new packet is queued toward the host
/// (`SDIO_INT_NEW_PACKET` on ESP32, ESP32-C5, and ESP32-C6 co-processors).
pub const INTERRUPT_NEW_PACKET: u32 = 1 << 23;
/// Interrupt bit: stop sending Wi-Fi data (`SDIO_INT_START_THROTTLE`).
pub const INTERRUPT_START_THROTTLE: u32 = 1 << 7;
/// Interrupt bit: Wi-Fi data may flow again (`SDIO_INT_STOP_THROTTLE`).
pub const INTERRUPT_STOP_THROTTLE: u32 = 1 << 6;

/// `ESP_OPEN_DATA_PATH`, written as bit 0 of the host interrupt.
const OPEN_DATA_PATH: u8 = 1 << 0;

/// Bytes read from `INT_RAW` through `PACKET_LEN` in one transfer.
const STATUS_LEN: usize = (register::PACKET_LEN - register::INT_RAW + 4) as usize;

const LENGTH_MASK: u32 = 0x000f_ffff;
const RX_BYTE_WRAP: u32 = 0x0010_0000;
const TOKEN_MASK: u32 = 0x0fff;
const TOKEN_WRAP: u32 = 0x1000;

const CCCR_IO_ENABLE: u32 = 0x02;
const CCCR_IO_READY: u32 = 0x03;
const CCCR_INT_ENABLE: u32 = 0x04;
const FBR_BLOCK_SIZE: u32 = 0x10;
const FUNCTION_1_MASK: u8 = 1 << FUNCTION;
/// `SDIO_INIT_MAX_RETRY` readiness polls, 10 ms apart.
const READY_POLLS: u32 = 10;

/// The new bytes the co-processor queued, from a cumulative `PACKET_LEN`
/// value and the bytes the host already read (`sdio_get_len_from_slave`).
///
/// # Errors
///
/// Returns [`TransportError::BusFault`] for an all-ones register, which a
/// dead bus reads back.
pub const fn pending_bytes(packet_len: u32, consumed: u32) -> Result<u32, TransportError> {
    if packet_len == u32::MAX {
        return Err(TransportError::BusFault);
    }
    let queued = packet_len & LENGTH_MASK;
    Ok((queued + RX_BYTE_WRAP - (consumed % RX_BYTE_WRAP)) % RX_BYTE_WRAP)
}

/// The co-processor's free receive buffers, from its token register and the
/// buffers the host already used (`sdio_get_tx_buffer_num`).
#[must_use]
pub const fn free_buffers(token: u32, used: u32) -> u32 {
    (((token >> 16) & TOKEN_MASK) + TOKEN_WRAP - (used % TOKEN_WRAP)) % TOKEN_WRAP
}

/// The first `length` bytes of a word-aligned buffer, still word-aligned as
/// the SDIO DMA requires.
///
/// # Panics
///
/// Never: `length` is clamped to the buffer.
pub fn aligned<const N: usize>(buffer: &Aligned<A4, [u8; N]>, length: usize) -> &Aligned<A4, [u8]> {
    let whole: &Aligned<A4, [u8]> = buffer;
    &whole[..length.min(N)]
}

/// Mutable form of [`aligned`].
pub fn aligned_mut<const N: usize>(
    buffer: &mut Aligned<A4, [u8; N]>,
    length: usize,
) -> &mut Aligned<A4, [u8]> {
    let whole: &mut Aligned<A4, [u8]> = buffer;
    &mut whole[..length.min(N)]
}

/// What one status read found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The interrupts the co-processor raised, already cleared.
    pub interrupts: u32,
    /// Bytes waiting to be read.
    pub pending: u32,
}

/// An initialized SDIO link to the co-processor.
pub struct SdioTransport<B: MmcBus, D: DelayNs> {
    card: SdioCard<B, D>,
    consumed: u32,
    buffers_used: u32,
    free_cached: u32,
    scratch: Aligned<A4, [u8; STATUS_LEN]>,
}

impl<B: MmcBus, D: DelayNs> SdioTransport<B, D> {
    /// Brings the card up at `frequency_hz` and prepares function 1, trying
    /// the card set-up up to `attempts` times, `retry_delay` apart, while the
    /// co-processor finishes booting.
    ///
    /// # Errors
    ///
    /// Returns an error when the card does not answer, function 1 never gets
    /// ready, or a register write is not read back.
    pub async fn new(
        bus: B,
        delay: D,
        frequency_hz: u32,
        attempts: u32,
        retry_delay: embassy_time::Duration,
    ) -> Result<Self, TransportError> {
        let mut card = SdioCard::new_uninit(bus, delay);
        let mut attempt = 0;
        loop {
            attempt += 1;
            match card.reacquire(frequency_hz).await {
                Ok(()) => break,
                Err(error) if attempt >= attempts => return Err(TransportError::Card(error)),
                Err(_) => embassy_time::Timer::after(retry_delay).await,
            }
        }
        let mut transport = Self {
            card,
            consumed: 0,
            buffers_used: 0,
            free_cached: 0,
            scratch: Aligned([0; STATUS_LEN]),
        };
        transport.prepare_function().await?;
        Ok(transport)
    }

    /// Returns the bus frequency the card runs at.
    #[must_use]
    pub const fn frequency_hz(&self) -> u32 {
        self.card.freq()
    }

    async fn prepare_function(&mut self) -> Result<(), TransportError> {
        let card = &mut self.card;
        let enabled = card.cmd52_read(0, CCCR_IO_ENABLE).await?;
        card.cmd52_write(0, CCCR_IO_ENABLE, enabled | FUNCTION_1_MASK)
            .await?;
        let mut ready = false;
        for _ in 0..READY_POLLS {
            if card.cmd52_read(0, CCCR_IO_READY).await? & FUNCTION_1_MASK != 0 {
                ready = true;
                break;
            }
            embassy_time::Timer::after_millis(10).await;
        }
        if !ready {
            return Err(TransportError::FunctionNotReady);
        }
        // Master interrupt enable plus function 1.
        let interrupts = card.cmd52_read(0, CCCR_INT_ENABLE).await?;
        card.cmd52_write(0, CCCR_INT_ENABLE, interrupts | 0x01 | FUNCTION_1_MASK)
            .await?;
        for function in [0_u8, FUNCTION] {
            let base = u32::from(function) * 0x100 + FBR_BLOCK_SIZE;
            let [low, high] = (BLOCK_SIZE as u16).to_le_bytes();
            card.cmd52_write(0, base, low).await?;
            card.cmd52_write(0, base + 1, high).await?;
            let read = u16::from_le_bytes([
                card.cmd52_read(0, base).await?,
                card.cmd52_read(0, base + 1).await?,
            ]);
            if usize::from(read) != BLOCK_SIZE {
                return Err(TransportError::BlockSize(read));
            }
        }
        Ok(())
    }

    /// Tells the co-processor the host is ready to receive.
    ///
    /// # Errors
    ///
    /// Returns an error when the register write fails.
    pub async fn open_data_path(&mut self) -> Result<(), TransportError> {
        self.card
            .cmd52_write(FUNCTION, register::HOST_TO_SLAVE_INTERRUPT, OPEN_DATA_PATH)
            .await?;
        Ok(())
    }

    /// Waits for the card interrupt on DAT1.
    ///
    /// # Errors
    ///
    /// Returns an error from the host controller.
    pub async fn wait_interrupt(&mut self) -> Result<(), TransportError> {
        self.card.wait_for_event().await?;
        Ok(())
    }

    /// Reads and clears the pending interrupts and the queued byte count.
    ///
    /// # Errors
    ///
    /// Returns an error when a register transfer fails or the bus reads back
    /// all ones.
    pub async fn status(&mut self) -> Result<Status, TransportError> {
        self.card
            .cmd53_read_bytes(
                FUNCTION,
                true,
                register::INT_RAW,
                aligned_mut(&mut self.scratch, STATUS_LEN),
            )
            .await?;
        let word = |bytes: &[u8], at: usize| {
            let mut value = [0; 4];
            value.copy_from_slice(&bytes[at..at + 4]);
            u32::from_le_bytes(value)
        };
        let interrupts = word(&self.scratch[..], 0);
        let packet_len = word(
            &self.scratch[..],
            (register::PACKET_LEN - register::INT_RAW) as usize,
        );
        if interrupts != 0 {
            self.scratch[..4].copy_from_slice(&interrupts.to_le_bytes());
            self.card
                .cmd53_write_bytes(
                    FUNCTION,
                    true,
                    register::INT_CLEAR,
                    aligned(&self.scratch, 4),
                )
                .await?;
        }
        Ok(Status {
            interrupts,
            pending: pending_bytes(packet_len, self.consumed)?,
        })
    }

    /// Reads the next `buffer.len()` queued bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the transfer fails; the bytes count as read
    /// either way, as ESP-Hosted's host does, so the next read starts after
    /// them.
    pub async fn read(&mut self, buffer: &mut Aligned<A4, [u8]>) -> Result<(), TransportError> {
        let length = u32::try_from(buffer.len()).map_err(|_| TransportError::TooLarge)?;
        if length == 0 {
            return Ok(());
        }
        let address = FIFO_END
            .checked_sub(length)
            .ok_or(TransportError::TooLarge)?;
        let result = self
            .card
            .cmd53_read::<BLOCK_SIZE>(FUNCTION, address, buffer)
            .await;
        self.consumed = (self.consumed + length) % RX_BYTE_WRAP;
        result.map_err(TransportError::Card)
    }

    /// Returns whether `credits` receive buffers are free, reading the token
    /// register only when the cached count is short.
    ///
    /// # Errors
    ///
    /// Returns an error when the register read fails.
    pub async fn take_credits(&mut self, credits: u32) -> Result<bool, TransportError> {
        if self.free_cached < credits {
            self.card
                .cmd53_read_bytes(
                    FUNCTION,
                    true,
                    register::TOKEN_RDATA,
                    aligned_mut(&mut self.scratch, 4),
                )
                .await?;
            let mut token = [0; 4];
            token.copy_from_slice(&self.scratch[..4]);
            self.free_cached = free_buffers(u32::from_le_bytes(token), self.buffers_used);
        }
        if self.free_cached < credits {
            return Ok(false);
        }
        self.free_cached -= credits;
        Ok(true)
    }

    /// Writes one transfer that already took its `credits`.
    ///
    /// # Errors
    ///
    /// Returns an error when the transfer fails.
    pub async fn write(
        &mut self,
        data: &Aligned<A4, [u8]>,
        credits: u32,
    ) -> Result<(), TransportError> {
        let length = u32::try_from(data.len()).map_err(|_| TransportError::TooLarge)?;
        let address = FIFO_END
            .checked_sub(length)
            .ok_or(TransportError::TooLarge)?;
        self.card
            .cmd53_write::<BLOCK_SIZE>(FUNCTION, address, data)
            .await?;
        self.buffers_used = (self.buffers_used + credits) % TOKEN_WRAP;
        Ok(())
    }
}

/// The SDIO link failed.
#[derive(Clone, Copy, Debug)]
pub enum TransportError {
    /// The SDIO host or card reported an error.
    Card(MmcError),
    /// Function 1 never reported ready.
    FunctionNotReady,
    /// A block size did not read back as written.
    BlockSize(u16),
    /// The packet-length register reads back all ones: the bus is dead.
    BusFault,
    /// A transfer exceeds the FIFO window.
    TooLarge,
}

impl From<MmcError> for TransportError {
    fn from(error: MmcError) -> Self {
        Self::Card(error)
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Card(error) => write!(formatter, "SDIO error: {error:?}"),
            Self::FunctionNotReady => {
                formatter.write_str("co-processor SDIO function 1 never got ready")
            }
            Self::BlockSize(size) => {
                write!(
                    formatter,
                    "co-processor kept SDIO block size {size}, not 512"
                )
            }
            Self::BusFault => formatter.write_str("SDIO bus reads back all ones"),
            Self::TooLarge => formatter.write_str("SDIO transfer exceeds the FIFO window"),
        }
    }
}

impl core::error::Error for TransportError {}

impl PartialEq for TransportError {
    fn eq(&self, other: &Self) -> bool {
        use TransportError::{BlockSize, BusFault, Card, FunctionNotReady, TooLarge};
        match (self, other) {
            (Card(_), Card(_))
            | (FunctionNotReady, FunctionNotReady)
            | (BusFault, BusFault)
            | (TooLarge, TooLarge) => true,
            (BlockSize(left), BlockSize(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for TransportError {}
