//! Bare-metal ESP32-S3 SDMMC storage binding backed directly by `esp-hal`.

use embedded_io::{ErrorKind, ErrorType, SeekFrom};
use embedded_io_async::{Read, Seek, Write};
use esp_hal::{
    dma::aligned::InternalMemory,
    peripherals::SDHOST,
    sdmmc::{
        BusWidth, CommandFlags, Config as HostConfig, ConfigError as HostConfigError, ResponseLen,
        SdHostController, Slot, SlotClk, SlotCmd, SlotConfig, SlotData,
    },
    Blocking,
};
use sdio::{
    common,
    sd::{self, CSD, OCR, SD},
    ControlCommand, MmcError, Response,
};
use static_cell::StaticCell;

const BLOCK_BYTES: usize = 512;
const IDENTIFICATION_FREQUENCY_HZ: u32 = 400_000;
const CARD_FREQUENCY_HZ: u32 = 25_000_000;
const POWER_UP_ATTEMPTS: usize = 100;

/// Failure while constructing the ESP32-S3 SDMMC host.
#[derive(Debug)]
pub enum SdmmcConfigError {
    /// `esp-hal` rejected the SD host or slot configuration.
    Host(HostConfigError),
}

/// Byte-addressed SD card backed by `esp-hal`'s blocking SDMMC slot.
pub struct SdmmcDevice {
    slot: Slot<'static, 0, Blocking>,
    sector: InternalMemory<[u8; BLOCK_BYTES]>,
    initialized: bool,
    high_capacity: bool,
    position: u64,
    capacity: u64,
}

/// Runtime SD card access failure.
#[derive(Debug)]
pub enum SdmmcError {
    /// Card negotiation or a block transfer failed.
    Card(MmcError),
    /// The requested seek position is outside the media or address space.
    InvalidSeek,
}

impl core::fmt::Display for SdmmcError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Card(error) => write!(formatter, "SD card operation failed: {error:?}"),
            Self::InvalidSeek => formatter.write_str("invalid SD card seek position"),
        }
    }
}

impl core::error::Error for SdmmcError {}

impl From<MmcError> for SdmmcError {
    fn from(error: MmcError) -> Self {
        Self::Card(error)
    }
}

impl embedded_io::Error for SdmmcError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidSeek => ErrorKind::InvalidInput,
            Self::Card(MmcError::Timeout) => ErrorKind::TimedOut,
            Self::Card(_) => ErrorKind::Other,
        }
    }
}

impl ErrorType for SdmmcDevice {
    type Error = SdmmcError;
}

/// Constructs a one-bit slot from the actual ESP32-S3 `SDHOST` token.
pub fn sdmmc_device_1bit<CLK, CMD, D0>(
    host: SDHOST<'static>,
    clk: CLK,
    cmd: CMD,
    data0: D0,
) -> Result<SdmmcDevice, SdmmcConfigError>
where
    CLK: SlotClk<'static, 0>,
    CMD: SlotCmd<'static, 0>,
    D0: SlotData<'static, 0, 0>,
{
    let controller =
        SdHostController::new(host, HostConfig::default()).map_err(SdmmcConfigError::Host)?;
    static CONTROLLER: StaticCell<SdHostController<'static>> = StaticCell::new();
    let controller = CONTROLLER.init(controller);
    let slot = controller
        .slot::<0>(SlotConfig::default())
        .map_err(SdmmcConfigError::Host)?
        .with_clk(clk)
        .with_cmd(cmd)
        .with_data0(data0);

    Ok(SdmmcDevice {
        slot,
        sector: InternalMemory::new([0; BLOCK_BYTES]),
        initialized: false,
        high_capacity: false,
        position: 0,
        capacity: 0,
    })
}

impl SdmmcDevice {
    fn mark_disconnected(&mut self) {
        self.initialized = false;
        self.high_capacity = false;
        self.position = 0;
        self.capacity = 0;
    }

    fn send_command<'a, C>(&mut self, command: C) -> Result<C::Resp<'a>, SdmmcError>
    where
        C: ControlCommand + 'a,
    {
        let response_len = match <C::Resp<'a> as Response>::LEN {
            sdio::ResponseLen::Zero => ResponseLen::None,
            sdio::ResponseLen::R48 => ResponseLen::Short,
            sdio::ResponseLen::R136 => ResponseLen::Long,
        };
        let stop_abort = C::INDEX == 12;
        let words = self
            .slot
            .command_blocking(
                C::INDEX,
                command.arg(),
                response_len,
                <C::Resp<'a> as Response>::CRC,
                CommandFlags {
                    wait_complete: !stop_abort,
                    stop_abort,
                    busy: <C::Resp<'a> as Response>::BUSY,
                },
            )
            .map_err(|error| SdmmcError::Card(error.into()))?;
        Ok(<C::Resp<'a> as Response>::from_words(&words))
    }

    fn send_app_command<'a, C>(&mut self, rca: u16, command: C) -> Result<C::Resp<'a>, SdmmcError>
    where
        C: ControlCommand + 'a,
    {
        self.send_command(common::app_cmd(rca))?.to_result()?;
        self.send_command(command)
    }

    async fn ensure_initialized(&mut self) -> Result<(), SdmmcError> {
        if self.initialized {
            return Ok(());
        }

        self.slot
            .set_bus_low_level(BusWidth::Bit1, IDENTIFICATION_FREQUENCY_HZ)
            .map_err(|error| SdmmcError::Card(error.into()))?;
        self.slot
            .send_init_sequence()
            .map_err(|error| SdmmcError::Card(error.into()))?;
        self.send_command(common::idle())?;

        let condition = self.send_command(sd::send_if_cond(1, 0xAA))?;
        if condition.check_pattern != 0xAA || condition.voltage & 1 == 0 {
            return Err(SdmmcError::Card(MmcError::Voltage));
        }

        let mut ocr = None;
        for _ in 0..POWER_UP_ATTEMPTS {
            let response =
                self.send_app_command(0, sd::sd_send_op_cond(true, false, false, 1 << 5))?;
            let candidate = OCR::<SD>::from(response);
            if !candidate.is_busy() {
                ocr = Some(candidate);
                break;
            }
            embassy_time::Timer::after_millis(10).await;
        }
        let ocr = ocr.ok_or(SdmmcError::Card(MmcError::Timeout))?;

        self.send_command(common::all_send_cid())?;
        let rca = self.send_command(sd::send_relative_address())?.rca;
        let csd = CSD::<SD>::from(self.send_command(common::send_csd(rca))?);
        let capacity = csd.card_size();
        if capacity == 0 {
            return Err(SdmmcError::Card(MmcError::CardType));
        }

        self.send_command(common::select_card(rca))?.to_result()?;
        self.slot
            .set_bus_low_level(BusWidth::Bit1, CARD_FREQUENCY_HZ)
            .map_err(|error| SdmmcError::Card(error.into()))?;
        self.send_command(common::set_block_length(BLOCK_BYTES as u32))?
            .to_result()?;

        self.high_capacity = ocr.high_capacity();
        self.capacity = capacity;
        self.initialized = true;
        Ok(())
    }

    fn card_address(&self, block: u32) -> Result<u32, SdmmcError> {
        if self.high_capacity {
            Ok(block)
        } else {
            block
                .checked_mul(BLOCK_BYTES as u32)
                .ok_or(SdmmcError::InvalidSeek)
        }
    }

    fn read_sector(&mut self, block: u32) -> Result<(), SdmmcError> {
        let address = self.card_address(block)?;
        let words = self
            .slot
            .read_blocks_blocking(
                17,
                address,
                self.sector.get_mut().unsize(),
                BLOCK_BYTES as u16,
                1,
            )
            .map_err(|error| SdmmcError::Card(error.into()))?;
        sdio::R1::from_words(&words).to_result()?;
        Ok(())
    }

    fn write_sector(&mut self, block: u32) -> Result<(), SdmmcError> {
        let address = self.card_address(block)?;
        let words = self
            .slot
            .write_blocks_blocking(
                24,
                address,
                self.sector.get_mut().unsize(),
                BLOCK_BYTES as u16,
                1,
            )
            .map_err(|error| SdmmcError::Card(error.into()))?;
        sdio::R1b::from_words(&words).to_response().to_result()?;
        Ok(())
    }
}

impl Read for SdmmcDevice {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.ensure_initialized().await?;
        let available = self.capacity.saturating_sub(self.position);
        let length = buffer
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let mut copied = 0;
        while copied < length {
            let block = u32::try_from(self.position / BLOCK_BYTES as u64)
                .map_err(|_| SdmmcError::InvalidSeek)?;
            let offset = self.position as usize % BLOCK_BYTES;
            let count = (BLOCK_BYTES - offset).min(length - copied);
            if let Err(error) = self.read_sector(block) {
                self.mark_disconnected();
                return Err(error);
            }
            let sector = self.sector.get_ref();
            buffer[copied..copied + count].copy_from_slice(&sector[offset..offset + count]);
            self.position += count as u64;
            copied += count;
        }
        Ok(copied)
    }
}

impl Write for SdmmcDevice {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.ensure_initialized().await?;
        let available = self.capacity.saturating_sub(self.position);
        let length = buffer
            .len()
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let mut copied = 0;
        while copied < length {
            let block = u32::try_from(self.position / BLOCK_BYTES as u64)
                .map_err(|_| SdmmcError::InvalidSeek)?;
            let offset = self.position as usize % BLOCK_BYTES;
            let count = (BLOCK_BYTES - offset).min(length - copied);
            if offset != 0 || count != BLOCK_BYTES {
                if let Err(error) = self.read_sector(block) {
                    self.mark_disconnected();
                    return Err(error);
                }
            }
            self.sector.get_mut()[offset..offset + count]
                .copy_from_slice(&buffer[copied..copied + count]);
            if let Err(error) = self.write_sector(block) {
                self.mark_disconnected();
                return Err(error);
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

impl Seek for SdmmcDevice {
    async fn seek(&mut self, position: SeekFrom) -> Result<u64, Self::Error> {
        self.ensure_initialized().await?;
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => i128::from(self.capacity) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        let target = u64::try_from(target).map_err(|_| SdmmcError::InvalidSeek)?;
        if target > self.capacity {
            return Err(SdmmcError::InvalidSeek);
        }
        self.position = target;
        Ok(self.position)
    }
}
