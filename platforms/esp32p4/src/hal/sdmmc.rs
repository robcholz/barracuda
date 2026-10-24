//! A Board's microSD card on slot 0 of the SD host the Platform owns.
//!
//! The Platform entry builds the SD host and takes slot 0 when the Board
//! declares a card there (`crate::sd_host`); the generated Board composition
//! receives that slot as its `sdmmc-device` binding, wires the card's IO_MUX
//! pads to it, and gets the card as an `embedded_io_async` byte device.

use embassy_time::Delay;
use esp_hal::{
    sdmmc::{ConfigError, Slot, SlotClk, SlotCmd, SlotData},
    Async,
};
use sdio::{
    BlockReadCommand, BlockWriteCommand, BusWidth, ByteReadCommand, ByteWriteCommand,
    ControlCommand, MmcBus, MmcError,
};

use crate::sd_card::SdCard;

pub use crate::sd_card::SdCardError as SdmmcError;

/// The on-chip LDO channel slot 0's pads run from (VO4).
const POWER_CHANNEL: u8 = 4;
/// The voltage esp-hal ties VO4 to when it starts the SD host.
const POWER_MILLIVOLTS: u16 = 3_300;

/// Slot 0 of the Platform's SD host, as the entry hands it to the Board
/// composition, or why the SD host refused it.
pub struct SdCardSlot(Result<Slot<'static, 0, Async>, ConfigError>);

impl SdCardSlot {
    #[cfg_attr(not(barracuda_esp32p4_sd_card), allow(dead_code))]
    pub(crate) const fn new(slot: Result<Slot<'static, 0, Async>, ConfigError>) -> Self {
        Self(slot)
    }
}

/// The Board's card, read and written as bytes over slot 0.
pub type SdmmcDevice = SdCard<CardBus, Delay>;

/// Slot 0 once wired to the card's pads: the bus the card protocol drives.
pub struct CardBus(Slot<'static, 0, Async>);

// SAFETY: esp-hal marks an async driver `!Send` because its interrupt handler
// runs on the core that bound it, and a driver moved to the other core would
// race that handler. The ESP32-P4 Platform starts the second core nowhere:
// the entry binds the SD host interrupt in `main` and runs its one executor on
// the same core, so every task this card reaches, including the System task
// the removable-storage API (`Send`) hands it to, runs where the handler does.
#[allow(unsafe_code)]
unsafe impl Send for CardBus {}

impl MmcBus for CardBus {
    async fn send_command<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ControlCommand + 'a,
    {
        self.0.send_command(cmd).await
    }

    async fn read_blocks<'a, C>(&mut self, cmd: C, auto_stop: bool) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockReadCommand + 'a,
    {
        self.0.read_blocks(cmd, auto_stop).await
    }

    async fn write_blocks<'a, C>(
        &mut self,
        cmd: C,
        auto_stop: bool,
    ) -> Result<C::Resp<'a>, MmcError>
    where
        C: BlockWriteCommand + 'a,
    {
        self.0.write_blocks(cmd, auto_stop).await
    }

    async fn read_bytes<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteReadCommand + 'a,
    {
        self.0.read_bytes(cmd).await
    }

    async fn write_bytes<'a, C>(&mut self, cmd: C) -> Result<C::Resp<'a>, MmcError>
    where
        C: ByteWriteCommand + 'a,
    {
        self.0.write_bytes(cmd).await
    }

    async fn init_idle(&mut self, hz: u32) -> Result<(), MmcError> {
        self.0.init_idle(hz).await
    }

    fn set_bus(&mut self, width: BusWidth, hz: u32) -> Result<(), MmcError> {
        self.0.set_bus(width, hz)
    }

    fn supports_mmc(&self) -> bool {
        self.0.supports_mmc()
    }

    fn supports_auto_stop(&self) -> bool {
        self.0.supports_auto_stop()
    }

    fn supports_bus_width(&self) -> BusWidth {
        self.0.supports_bus_width()
    }

    fn supports_frequency(&self) -> u32 {
        self.0.supports_frequency()
    }
}

/// The Board's card slot cannot be built.
#[derive(Debug)]
pub enum SdmmcConfigError {
    /// esp-hal refused the SD host or the slot.
    Host(ConfigError),
    /// The Board declares a card supply other than LDO VO4 at 3.3 V, the
    /// one slot 0's pads run from.
    UnsupportedPower {
        /// Board-declared LDO channel.
        channel: u8,
        /// Board-declared voltage.
        millivolts: u16,
    },
    /// The Board declares a bus width other than one or four lines.
    UnsupportedBusWidth(u8),
}

impl core::fmt::Display for SdmmcConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Host(error) => write!(formatter, "SD host slot 0: {error}"),
            Self::UnsupportedPower {
                channel,
                millivolts,
            } => write!(
                formatter,
                "SD host slot 0 runs from LDO channel {POWER_CHANNEL} at {POWER_MILLIVOLTS} mV, not channel {channel} at {millivolts} mV"
            ),
            Self::UnsupportedBusWidth(width) => {
                write!(formatter, "an SD card bus has 1 or 4 lines, not {width}")
            }
        }
    }
}

impl core::error::Error for SdmmcConfigError {}

/// Wires a four-line card to slot 0 at `frequency_hz`.
///
/// # Errors
///
/// Returns [`SdmmcConfigError`] when the SD host refused the slot, or the
/// Board declares a bus width or supply slot 0 does not have.
#[allow(clippy::too_many_arguments)]
pub fn sdmmc_device(
    slot: SdCardSlot,
    clk: impl SlotClk<'static, 0>,
    cmd: impl SlotCmd<'static, 0>,
    d0: impl SlotData<'static, 0, 0>,
    d1: impl SlotData<'static, 0, 1>,
    d2: impl SlotData<'static, 0, 2>,
    d3: impl SlotData<'static, 0, 3>,
    bus_width: u8,
    power_channel: u8,
    power_millivolts: u16,
    frequency_hz: u32,
) -> Result<SdmmcDevice, SdmmcConfigError> {
    if bus_width != 4 {
        return Err(SdmmcConfigError::UnsupportedBusWidth(bus_width));
    }
    if power_channel != POWER_CHANNEL || power_millivolts != POWER_MILLIVOLTS {
        return Err(SdmmcConfigError::UnsupportedPower {
            channel: power_channel,
            millivolts: power_millivolts,
        });
    }
    let slot = slot
        .0
        .map_err(SdmmcConfigError::Host)?
        .with_clk(clk)
        .with_cmd(cmd)
        .with_data0(d0)
        .with_data1(d1)
        .with_data2(d2)
        .with_data3(d3);
    Ok(SdCard::new(CardBus(slot), Delay, true, frequency_hz))
}

/// Wires a one-line card to slot 0 at `frequency_hz`. Board selection has
/// checked that the Board declares LDO VO4 at 3.3 V for it.
///
/// # Errors
///
/// Returns [`SdmmcConfigError::Host`] when the SD host refused the slot.
pub fn sdmmc_device_1bit(
    slot: SdCardSlot,
    clk: impl SlotClk<'static, 0>,
    cmd: impl SlotCmd<'static, 0>,
    d0: impl SlotData<'static, 0, 0>,
    frequency_hz: u32,
) -> Result<SdmmcDevice, SdmmcConfigError> {
    let slot = slot
        .0
        .map_err(SdmmcConfigError::Host)?
        .with_clk(clk)
        .with_cmd(cmd)
        .with_data0(d0);
    Ok(SdCard::new(CardBus(slot), Delay, false, frequency_hz))
}

/// Resolves a Board's SD host slot binding to its type: slot 0 of the SD
/// host the Platform owns. The Platform keeps slot 1 for a co-processor
/// link and hands no other slot to a Board.
#[macro_export]
macro_rules! __barracuda_esp32p4_sdmmc_slot_binding_type {
    (SDHOST, 0) => {
        $crate::hal::SdCardSlot
    };
}

/// Takes a Board's SD host slot binding from the entry's hand-off.
#[macro_export]
macro_rules! __barracuda_esp32p4_sdmmc_slot_binding {
    ($peripherals:ident, $handoff:expr, SDHOST, 0) => {
        $handoff.slot0
    };
}

#[doc(hidden)]
pub use __barracuda_esp32p4_sdmmc_slot_binding as sdmmc_slot_binding;
#[doc(hidden)]
pub use __barracuda_esp32p4_sdmmc_slot_binding_type as sdmmc_slot_binding_type;
