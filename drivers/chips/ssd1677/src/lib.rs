//! Low-level Driver for the Solomon SSD1677 e-paper controller.
//!
//! The SSD1677 drives up to 960 x 680 panels from two one-bit RAM planes,
//! `BW` (`0x24`) and `RED` (`0x26`). With a register-loaded waveform the pair
//! of bits per pixel selects one of four LUT groups, which is how M5Stack's
//! M5GFX shows four gray levels on the Paper Mono's 800 x 480 GDEQ0426T82.
//!
//! The initialization list, waveforms, voltages, and update-control values
//! are M5GFX's (`Panel_SSD1677.inl`). `BUSY` is active high.

#![no_std]

use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};

const DRIVER_OUTPUT: u8 = 0x01;
const GATE_VOLTAGE: u8 = 0x03;
const SOURCE_VOLTAGE: u8 = 0x04;
const BOOSTER_SOFT_START: u8 = 0x0C;
const DEEP_SLEEP: u8 = 0x10;
const DATA_ENTRY: u8 = 0x11;
const SOFTWARE_RESET: u8 = 0x12;
const TEMPERATURE_SENSOR: u8 = 0x18;
const WRITE_TEMPERATURE: u8 = 0x1A;
const MASTER_ACTIVATION: u8 = 0x20;
const UPDATE_CONTROL_1: u8 = 0x21;
const UPDATE_CONTROL_2: u8 = 0x22;
const WRITE_VCOM: u8 = 0x2C;
const WRITE_LUT: u8 = 0x32;
const BORDER_WAVEFORM: u8 = 0x3C;
const RAM_X_RANGE: u8 = 0x44;
const RAM_Y_RANGE: u8 = 0x45;
const AUTO_WRITE_BW: u8 = 0x46;
const AUTO_WRITE_RED: u8 = 0x47;
const RAM_X_COUNTER: u8 = 0x4E;
const RAM_Y_COUNTER: u8 = 0x4F;

/// Display-update control 1: use both RAMs.
pub const CONTROL_1_NORMAL: u8 = 0x00;
/// Display-update control 1: ignore the `RED` RAM.
pub const CONTROL_1_BYPASS_RED: u8 = 0x40;
/// Display-update control 2 bits that enable the clock and analog supply.
pub const CONTROL_2_POWER_ON: u8 = 0xC0;
/// Display-update control 2 bits that disable the analog supply and clock.
pub const CONTROL_2_POWER_OFF: u8 = 0x03;

/// Default `BUSY` timeout in milliseconds (M5GFX `_wait_busy`: 4096 ms).
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 4_096;
/// Timeout for a full four-gray refresh (M5GFX `_activate`: 10 s).
pub const REFRESH_TIMEOUT_MS: u32 = 10_000;
/// Delay before `BUSY` is sampled after a command (M5GFX: 2 ms), because
/// the line does not rise in the same instant the transfer completes.
const BUSY_RISE_MS: u32 = 2;
/// Recovery after a software reset (M5GFX init list: 10 ms).
const SOFTWARE_RESET_MS: u32 = 10;

/// One of the two one-bit RAM planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ram {
    /// `BW` RAM (`0x24`): the current frame, or a pixel's low group bit.
    Bw,
    /// `RED` RAM (`0x26`): the previous frame, or a pixel's high group bit.
    Red,
}

impl Ram {
    const fn command(self) -> u8 {
        match self {
            Self::Bw => 0x24,
            Self::Red => 0x26,
        }
    }
}

/// One register-loaded waveform: 105 LUT bytes for command `0x32` (five
/// 10-byte VS groups, ten 5-byte timing groups, five frame-rate bytes)
/// followed by VGH, VSH1, VSH2, VSL, and VCOM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waveform(pub [u8; 110]);

impl Waveform {
    /// Returns the 105 bytes sent with command `0x32`.
    #[must_use]
    pub fn lut(&self) -> &[u8] {
        self.0.get(..105).unwrap_or(&[])
    }

    /// Returns VGH (`0x03`).
    #[must_use]
    pub const fn gate_voltage(&self) -> u8 {
        self.0[105]
    }

    /// Returns VSH1, VSH2, and VSL (`0x04`).
    #[must_use]
    pub const fn source_voltages(&self) -> [u8; 3] {
        [self.0[106], self.0[107], self.0[108]]
    }

    /// Returns VCOM (`0x2C`).
    #[must_use]
    pub const fn vcom(&self) -> u8 {
        self.0[109]
    }
}

/// M5GFX `lut_quality_SSD1677`: an erasing oscillation, then a four-gray tail
/// (Mode 1, groups white, light, dark, black).
pub const WAVEFORM_QUALITY: Waveform = Waveform([
    0x66, 0x66, 0x00, 0x4A, 0x88, 0x00, 0x00, 0x00, 0x00, 0x00, // white
    0x66, 0x66, 0x80, 0x62, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // light
    0x66, 0x66, 0x88, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // dark
    0x66, 0x66, 0xA8, 0x44, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, // black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // VCOM
    0x05, 0x05, 0x05, 0x05, 0x00, //
    0x05, 0x05, 0x05, 0x05, 0x01, //
    0x08, 0x0B, 0x02, 0x03, 0x01, //
    0x0C, 0x02, 0x07, 0x02, 0x01, //
    0x01, 0x00, 0x02, 0x00, 0x01, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x22, 0x22, 0x22, 0x22, 0x22, // frame rate
    0x17, 0x46, 0xA8, 0x36, 0x30, // VGH, VSH1, VSH2, VSL, VCOM
]);

/// M5GFX `lut_text_SSD1677`: 64-frame absolute four-gray waveform (Mode 1).
pub const WAVEFORM_TEXT: Waveform = Waveform([
    0x55, 0x55, 0x55, 0x55, 0x55, 0x5A, 0xAA, 0xAA, 0x00, 0x00, // white
    0xAA, 0x95, 0x55, 0x55, 0x55, 0x5A, 0x82, 0xA0, 0x00, 0x00, // light
    0xAA, 0xA5, 0x55, 0x55, 0x55, 0x5A, 0xA0, 0x00, 0x00, 0x00, // dark
    0xAA, 0xAA, 0xAA, 0xAA, 0x55, 0x55, 0x55, 0x50, 0x00, 0x00, // black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // VCOM
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F, // frame rate
    0x17, 0x41, 0xA8, 0x32, 0x30, // VGH, VSH1, VSH2, VSL, VCOM
]);

/// M5GFX `lut_fast_SSD1677`: 48-frame absolute four-gray waveform (Mode 2).
pub const WAVEFORM_FAST: Waveform = Waveform([
    0x55, 0x55, 0x55, 0x55, 0x55, 0x5A, 0xAA, 0xAA, 0x00, 0x00, // white
    0xAA, 0x95, 0x55, 0x55, 0x55, 0x5A, 0x82, 0xAC, 0x00, 0x00, // light
    0xAA, 0xA5, 0x55, 0x55, 0x55, 0x5A, 0xAC, 0x00, 0x00, 0x00, // dark
    0xAA, 0xAA, 0xAA, 0xAA, 0x55, 0x55, 0x55, 0x50, 0x00, 0x00, // black
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // VCOM
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x01, 0x01, 0x01, 0x01, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x02, 0x02, 0x02, 0x02, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, //
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F, // frame rate
    0x17, 0x41, 0xA8, 0x32, 0x30, // VGH, VSH1, VSH2, VSL, VCOM
]);

/// Panel facts the initialization needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelGeometry {
    /// Source outputs (RAM X extent) in pixels.
    pub sources: u16,
    /// Gate outputs (RAM Y extent) in lines.
    pub gates: u16,
}

/// SSD1677 transport or timing failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, DcError, BusyError> {
    /// The SPI transfer failed.
    Spi(SpiError),
    /// The data/command output failed.
    DataCommand(DcError),
    /// The `BUSY` input could not be read.
    Busy(BusyError),
    /// `BUSY` stayed high past the timeout.
    Timeout,
}

/// Result type of [`Ssd1677`] operations.
pub type Result<T, SPI, DC, BUSY> = core::result::Result<
    T,
    Error<
        <SPI as embedded_hal::spi::ErrorType>::Error,
        <DC as embedded_hal::digital::ErrorType>::Error,
        <BUSY as embedded_hal::digital::ErrorType>::Error,
    >,
>;

/// One SSD1677 on a write-only four-wire SPI device.
pub struct Ssd1677<SPI, DC, BUSY> {
    spi: SPI,
    dc: DC,
    busy: BUSY,
}

impl<SPI, DC, BUSY> Ssd1677<SPI, DC, BUSY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
{
    /// Takes the controller's SPI device, data/command output, and `BUSY`
    /// input. Nothing is sent.
    pub const fn new(spi: SPI, dc: DC, busy: BUSY) -> Self {
        Self { spi, dc, busy }
    }

    /// Returns the bus and pins.
    pub fn release(self) -> (SPI, DC, BUSY) {
        (self.spi, self.dc, self.busy)
    }

    /// Returns whether the controller is busy (`BUSY` high).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when the input cannot be read.
    pub fn is_busy(&mut self) -> Result<bool, SPI, DC, BUSY> {
        self.busy.is_high().map_err(Error::Busy)
    }

    /// Waits for `BUSY` to rise after a command, then polls it every
    /// millisecond until it falls.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] after `timeout_ms`, or a pin error.
    pub fn wait_until_idle(
        &mut self,
        delay: &mut impl DelayNs,
        timeout_ms: u32,
    ) -> Result<(), SPI, DC, BUSY> {
        delay.delay_ms(BUSY_RISE_MS);
        let mut waited = 0_u32;
        while self.is_busy()? {
            if waited >= timeout_ms {
                return Err(Error::Timeout);
            }
            delay.delay_ms(1);
            waited = waited.saturating_add(1);
        }
        Ok(())
    }

    /// Sends one command and its data bytes.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn command(&mut self, command: u8, data: &[u8]) -> Result<(), SPI, DC, BUSY> {
        self.dc.set_low().map_err(Error::DataCommand)?;
        self.spi.write(&[command]).map_err(Error::Spi)?;
        if !data.is_empty() {
            self.write_data(data)?;
        }
        Ok(())
    }

    /// Sends data bytes for the command in progress.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn write_data(&mut self, data: &[u8]) -> Result<(), SPI, DC, BUSY> {
        self.dc.set_high().map_err(Error::DataCommand)?;
        self.spi.write(data).map_err(Error::Spi)
    }

    /// Software-resets the controller and runs M5GFX's power-up list:
    /// internal temperature sensor, booster soft start, gate count, and
    /// border waveform. Then both RAMs are cleared to white over the whole
    /// panel.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn initialize(
        &mut self,
        delay: &mut impl DelayNs,
        geometry: PanelGeometry,
    ) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(SOFTWARE_RESET, &[])?;
        delay.delay_ms(SOFTWARE_RESET_MS);
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(TEMPERATURE_SENSOR, &[0x80])?;
        self.command(BOOSTER_SOFT_START, &[0xAE, 0xC7, 0xC3, 0xC0, 0x40])?;
        let [gates_low, gates_high] = geometry.gates.saturating_sub(1).to_le_bytes();
        // SM = 1, TB = 0.
        self.command(DRIVER_OUTPUT, &[gates_low, gates_high, 0x02])?;
        self.command(BORDER_WAVEFORM, &[0x01])?;
        self.set_ram_window(
            0,
            geometry.sources.saturating_sub(1),
            geometry.gates.saturating_sub(1),
            0,
        )?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(AUTO_WRITE_BW, &[0xF7])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(AUTO_WRITE_RED, &[0xF7])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)
    }

    /// Selects the RAM window `x_start..=x_end` (pixels) and
    /// `y_start..=y_end` (gates, `y_start >= y_end`), with X incrementing and
    /// Y decrementing, and points both counters at its first byte. M5GFX uses
    /// this entry mode because the Paper Mono's gates are wired in reverse.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn set_ram_window(
        &mut self,
        x_start: u16,
        x_end: u16,
        y_start: u16,
        y_end: u16,
    ) -> Result<(), SPI, DC, BUSY> {
        let [xs_low, xs_high] = x_start.to_le_bytes();
        let [xe_low, xe_high] = x_end.to_le_bytes();
        let [ys_low, ys_high] = y_start.to_le_bytes();
        let [ye_low, ye_high] = y_end.to_le_bytes();
        // X increment, Y decrement.
        self.command(DATA_ENTRY, &[0x01])?;
        self.command(RAM_X_RANGE, &[xs_low, xs_high, xe_low, xe_high])?;
        self.command(RAM_Y_RANGE, &[ys_low, ys_high, ye_low, ye_high])?;
        self.command(RAM_X_COUNTER, &[xs_low, xs_high])?;
        self.command(RAM_Y_COUNTER, &[ys_low, ys_high])
    }

    /// Starts writing `ram`; follow with [`Ssd1677::write_data`].
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn begin_ram_write(&mut self, ram: Ram) -> Result<(), SPI, DC, BUSY> {
        self.command(ram.command(), &[])
    }

    /// Loads a register waveform and its voltages.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn load_waveform(&mut self, waveform: &Waveform) -> Result<(), SPI, DC, BUSY> {
        self.command(WRITE_LUT, waveform.lut())?;
        self.command(GATE_VOLTAGE, &[waveform.gate_voltage()])?;
        self.command(SOURCE_VOLTAGE, &waveform.source_voltages())?;
        self.command(WRITE_VCOM, &[waveform.vcom()])
    }

    /// Writes the temperature register (M5GFX's half-refresh mode uses `0x5A`).
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn write_temperature(&mut self, value: u8) -> Result<(), SPI, DC, BUSY> {
        self.command(WRITE_TEMPERATURE, &[value])
    }

    /// Sets the two display-update controls and starts the update.
    ///
    /// # Errors
    ///
    /// Returns a data/command or SPI error.
    pub fn activate(&mut self, control_1: u8, control_2: u8) -> Result<(), SPI, DC, BUSY> {
        self.command(UPDATE_CONTROL_1, &[control_1])?;
        self.command(UPDATE_CONTROL_2, &[control_2])?;
        self.command(MASTER_ACTIVATION, &[])
    }

    /// Turns the analog supply and clock off, then enters deep sleep mode 1
    /// (RAM retained); only a hardware reset wakes it.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn deep_sleep(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(UPDATE_CONTROL_2, &[CONTROL_2_POWER_OFF])?;
        self.command(MASTER_ACTIVATION, &[])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(DEEP_SLEEP, &[0x01])
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::{cell::RefCell, convert::Infallible};
    use std::{vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as PinErrorType, InputPin, OutputPin},
        spi::{ErrorType, Operation, SpiDevice},
    };

    use super::{
        Error, PanelGeometry, Ram, Ssd1677, WAVEFORM_FAST, WAVEFORM_QUALITY, WAVEFORM_TEXT,
    };

    #[derive(Debug, PartialEq, Eq)]
    enum Sent {
        Command(u8),
        Data(Vec<u8>),
    }

    #[derive(Default)]
    struct Wire {
        dc_high: bool,
        sent: Vec<Sent>,
    }

    struct Spi<'a>(&'a RefCell<Wire>);
    struct Dc<'a>(&'a RefCell<Wire>);

    impl ErrorType for Spi<'_> {
        type Error = Infallible;
    }

    impl SpiDevice for Spi<'_> {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
            let mut wire = self.0.borrow_mut();
            for operation in operations {
                if let Operation::Write(bytes) = operation {
                    let sent = if wire.dc_high {
                        Sent::Data(bytes.to_vec())
                    } else {
                        Sent::Command(bytes[0])
                    };
                    wire.sent.push(sent);
                }
            }
            Ok(())
        }
    }

    impl PinErrorType for Dc<'_> {
        type Error = Infallible;
    }

    impl OutputPin for Dc<'_> {
        fn set_low(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().dc_high = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Infallible> {
            self.0.borrow_mut().dc_high = true;
            Ok(())
        }
    }

    struct Busy {
        busy_reads: u32,
    }

    impl PinErrorType for Busy {
        type Error = Infallible;
    }

    impl InputPin for Busy {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            if self.busy_reads == 0 {
                Ok(false)
            } else {
                self.busy_reads -= 1;
                Ok(true)
            }
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            Ok(!self.is_high()?)
        }
    }

    #[derive(Default)]
    struct Delay {
        ms: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, ns: u32) {
            self.ms += ns / 1_000_000;
        }

        fn delay_ms(&mut self, ms: u32) {
            self.ms += ms;
        }
    }

    fn controller(wire: &RefCell<Wire>, busy_reads: u32) -> Ssd1677<Spi<'_>, Dc<'_>, Busy> {
        Ssd1677::new(Spi(wire), Dc(wire), Busy { busy_reads })
    }

    #[test]
    fn initialization_matches_m5gfx_paper_mono_list() {
        let wire = RefCell::new(Wire::default());
        let mut delay = Delay::default();
        controller(&wire, 0)
            .initialize(
                &mut delay,
                PanelGeometry {
                    sources: 800,
                    gates: 480,
                },
            )
            .expect("initialize");
        let sent = wire.into_inner().sent;
        assert_eq!(
            &sent[..12],
            &[
                Sent::Command(0x12),
                Sent::Command(0x18),
                Sent::Data(vec![0x80]),
                Sent::Command(0x0C),
                Sent::Data(vec![0xAE, 0xC7, 0xC3, 0xC0, 0x40]),
                Sent::Command(0x01),
                Sent::Data(vec![0xDF, 0x01, 0x02]),
                Sent::Command(0x3C),
                Sent::Data(vec![0x01]),
                Sent::Command(0x11),
                Sent::Data(vec![0x01]),
                Sent::Command(0x44),
            ]
        );
        assert_eq!(sent[12], Sent::Data(vec![0x00, 0x00, 0x1F, 0x03]));
        assert_eq!(sent[14], Sent::Data(vec![0xDF, 0x01, 0x00, 0x00]));
        assert_eq!(
            &sent[sent.len() - 4..],
            &[
                Sent::Command(0x46),
                Sent::Data(vec![0xF7]),
                Sent::Command(0x47),
                Sent::Data(vec![0xF7]),
            ]
        );
    }

    #[test]
    fn waveform_splits_lut_and_voltages() {
        let wire = RefCell::new(Wire::default());
        controller(&wire, 0)
            .load_waveform(&WAVEFORM_QUALITY)
            .expect("waveform");
        let sent = wire.into_inner().sent;
        assert_eq!(sent[0], Sent::Command(0x32));
        let Sent::Data(lut) = &sent[1] else {
            panic!("LUT data");
        };
        assert_eq!(lut.len(), 105);
        assert_eq!(&lut[100..], &[0x22; 5]);
        assert_eq!(
            &sent[2..],
            &[
                Sent::Command(0x03),
                Sent::Data(vec![0x17]),
                Sent::Command(0x04),
                Sent::Data(vec![0x46, 0xA8, 0x36]),
                Sent::Command(0x2C),
                Sent::Data(vec![0x30]),
            ]
        );
        assert_eq!(WAVEFORM_TEXT.source_voltages(), [0x41, 0xA8, 0x32]);
        assert_eq!(WAVEFORM_FAST.lut()[17], 0xAC);
    }

    #[test]
    fn ram_write_and_activation_use_their_commands() {
        let wire = RefCell::new(Wire::default());
        let mut ssd = controller(&wire, 0);
        ssd.begin_ram_write(Ram::Red).expect("RED");
        ssd.write_data(&[0x55, 0xAA]).expect("data");
        ssd.activate(0x00, 0x07 | 0xC0).expect("activate");
        assert_eq!(
            wire.into_inner().sent,
            vec![
                Sent::Command(0x26),
                Sent::Data(vec![0x55, 0xAA]),
                Sent::Command(0x21),
                Sent::Data(vec![0x00]),
                Sent::Command(0x22),
                Sent::Data(vec![0xC7]),
                Sent::Command(0x20),
            ]
        );
    }

    #[test]
    fn busy_waits_and_times_out() {
        let wire = RefCell::new(Wire::default());
        let mut delay = Delay::default();
        controller(&wire, 5)
            .wait_until_idle(&mut delay, 100)
            .expect("idle");
        assert_eq!(delay.ms, 2 + 5);
        let mut stuck = controller(&wire, u32::MAX);
        assert_eq!(stuck.wait_until_idle(&mut delay, 10), Err(Error::Timeout));
    }
}
