//! Low-level Driver for the JD79653A e-paper controller.
//!
//! The JD79653A drives Good Display's GDEW0154M09, the 200 x 200 1.54-inch
//! panel that M5Stack fits to later CoreInk and AirQ units. Its command set
//! follows the UC81xx family: an "old" (`0x10`) and a "new" (`0x13`) frame
//! RAM, a partial window, register-loaded waveform LUTs, and an active-low
//! `BUSY_N` line.
//!
//! The initialization list, LUTs, partial-window encoding, and data-interval
//! values are M5Stack's, from M5GFX `Panel_GDEW0154M09`.

#![no_std]

use embedded_hal::{
    delay::DelayNs,
    digital::{InputPin, OutputPin},
    spi::SpiDevice,
};

/// Native panel width in pixels.
pub const WIDTH: u16 = 200;
/// Native panel height in pixels.
pub const HEIGHT: u16 = 200;
/// Bytes in one packed one-bit-per-pixel row.
pub const ROW_BYTES: usize = 25;

const PANEL_SETTING: u8 = 0x00;
const POWER_OFF: u8 = 0x02;
const POWER_ON: u8 = 0x04;
const DEEP_SLEEP: u8 = 0x07;
const DATA_START_OLD: u8 = 0x10;
const DISPLAY_REFRESH: u8 = 0x12;
const DATA_START_NEW: u8 = 0x13;
const DATA_INTERVAL: u8 = 0x50;
const PARTIAL_WINDOW: u8 = 0x90;
const PARTIAL_IN: u8 = 0x91;
const DEEP_SLEEP_CHECK: u8 = 0xA5;

/// `VCOM and data interval` value for normal (non-inverted) data.
const DATA_INTERVAL_NORMAL: u8 = 0xD7;
/// `VCOM and data interval` value for inverted data.
const DATA_INTERVAL_INVERTED: u8 = 0xC7;
/// `VCOM and data interval` value that ignores the old frame before power-off.
const DATA_INTERVAL_NO_OLD: u8 = 0xF7;
/// `VCOM and data interval` value that ignores the old frame, inverted.
const DATA_INTERVAL_NO_OLD_INVERTED: u8 = 0xE7;

/// Settle time after the second panel-setting command (M5GFX: 160 ms).
const PANEL_SETTING_DELAY_MS: u32 = 160;

/// Default `BUSY_N` timeout in milliseconds (M5GFX `_wait_busy`: 1000 ms).
pub const DEFAULT_BUSY_TIMEOUT_MS: u32 = 1_000;

/// One command of the initialization list: opcode, data, and post-delay.
struct InitCommand {
    command: u8,
    data: &'static [u8],
    delay_ms: u32,
}

const fn command(command: u8, data: &'static [u8]) -> InitCommand {
    InitCommand {
        command,
        data,
        delay_ms: 0,
    }
}

const LUT_VCOM: [u8; 56] = lut56([0x01, 0x04, 0x04, 0x03, 0x01, 0x01, 0x01], 0x01);
const LUT_WW: [u8; 42] = lut42([0x01, 0x04, 0x04, 0x03, 0x01, 0x01, 0x01]);
const LUT_BW: [u8; 56] = lut56([0x01, 0x84, 0x84, 0x83, 0x01, 0x01, 0x01], 0x00);
const LUT_WB: [u8; 56] = lut56([0x01, 0x44, 0x44, 0x43, 0x01, 0x01, 0x01], 0x00);
const LUT_BB: [u8; 56] = lut56([0x01, 0x04, 0x04, 0x03, 0x01, 0x01, 0x01], 0x00);

/// Builds one 56-byte LUT whose first group is `head` and whose byte 12 (the
/// second group's repeat count, used only by the VCOM LUT) is `repeat`.
const fn lut56(head: [u8; 7], repeat: u8) -> [u8; 56] {
    let mut lut = [0_u8; 56];
    let mut index = 0;
    while index < head.len() {
        lut[index] = head[index];
        index = index.wrapping_add(1);
    }
    lut[12] = repeat;
    lut
}

const fn lut42(head: [u8; 7]) -> [u8; 42] {
    let mut lut = [0_u8; 42];
    let mut index = 0;
    while index < head.len() {
        lut[index] = head[index];
        index = index.wrapping_add(1);
    }
    lut
}

/// M5GFX `Panel_GDEW0154M09::getInitCommands` list 0, in order.
const INIT_SEQUENCE: &[InitCommand] = &[
    command(PANEL_SETTING, &[0xDF, 0x0E]),
    command(0x4D, &[0x55]),
    command(0xAA, &[0x0F]),
    command(0xE9, &[0x02]),
    command(0xB6, &[0x11]),
    command(0xF3, &[0x0A]),
    // Resolution: 200 x 200.
    command(0x61, &[0xC8, 0x00, 0xC8]),
    command(0x60, &[0x00]),
    command(0xE3, &[0x00]),
    command(POWER_ON, &[]),
    // Panel setting again with REG_EN: waveform LUTs come from registers.
    InitCommand {
        command: PANEL_SETTING,
        data: &[0xFF, 0x0E],
        delay_ms: PANEL_SETTING_DELAY_MS,
    },
    command(0x20, &LUT_VCOM),
    command(0x21, &LUT_WW),
    command(0x22, &LUT_BW),
    command(0x23, &LUT_WB),
    command(0x24, &LUT_BB),
];

/// Frame RAM addressed by a data transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRam {
    /// The previous image (`DTM1`, command `0x10`).
    Old,
    /// The image to present (`DTM2`, command `0x13`).
    New,
}

impl FrameRam {
    const fn command(self) -> u8 {
        match self {
            Self::Old => DATA_START_OLD,
            Self::New => DATA_START_NEW,
        }
    }
}

/// Byte-aligned partial window in native panel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    first_byte: u8,
    last_byte: u8,
    top: u16,
    bottom: u16,
}

impl Window {
    /// The whole panel.
    pub const FULL: Self = Self {
        first_byte: 0,
        last_byte: 24,
        top: 0,
        bottom: 199,
    };

    /// Covers pixel columns `left..=right` and rows `top..=bottom`, widened to
    /// whole bytes of eight columns.
    ///
    /// Returns `None` when the range is empty or outside the panel.
    #[must_use]
    pub fn covering(left: u16, top: u16, right: u16, bottom: u16) -> Option<Self> {
        if left > right || top > bottom || right >= WIDTH || bottom >= HEIGHT {
            return None;
        }
        Some(Self {
            first_byte: u8::try_from(left / 8).ok()?,
            last_byte: u8::try_from(right / 8).ok()?,
            top,
            bottom,
        })
    }

    /// Returns the first byte column.
    #[must_use]
    pub const fn first_byte(self) -> usize {
        self.first_byte as usize
    }

    /// Returns the last byte column.
    #[must_use]
    pub const fn last_byte(self) -> usize {
        self.last_byte as usize
    }

    /// Returns the first row.
    #[must_use]
    pub const fn top(self) -> u16 {
        self.top
    }

    /// Returns the last row.
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.bottom
    }

    /// Partial-window parameters as M5GFX sends them: horizontal start and end
    /// as the first pixel of the first and last byte, then the vertical start
    /// and end big-endian, then `PT_SCAN` = 1.
    fn parameters(self) -> [u8; 7] {
        let [top_high, top_low] = self.top.to_be_bytes();
        let [bottom_high, bottom_low] = self.bottom.to_be_bytes();
        [
            self.first_byte.wrapping_mul(8),
            self.last_byte.wrapping_mul(8),
            top_high,
            top_low,
            bottom_high,
            bottom_low,
            0x01,
        ]
    }
}

/// JD79653A transport or timing failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, DcError, BusyError> {
    /// The SPI transfer failed.
    Spi(SpiError),
    /// The data/command output failed.
    DataCommand(DcError),
    /// The `BUSY_N` input could not be read.
    Busy(BusyError),
    /// `BUSY_N` stayed low past the timeout.
    Timeout,
    /// A frame transfer supplied too few bytes for its window.
    ShortFrame,
}

/// Result type of [`Jd79653a`] operations.
pub type Result<T, SPI, DC, BUSY> = core::result::Result<
    T,
    Error<
        <SPI as embedded_hal::spi::ErrorType>::Error,
        <DC as embedded_hal::digital::ErrorType>::Error,
        <BUSY as embedded_hal::digital::ErrorType>::Error,
    >,
>;

/// One JD79653A on a write-only four-wire SPI device.
pub struct Jd79653a<SPI, DC, BUSY> {
    spi: SPI,
    dc: DC,
    busy: BUSY,
    inverted: bool,
}

impl<SPI, DC, BUSY> Jd79653a<SPI, DC, BUSY>
where
    SPI: SpiDevice,
    DC: OutputPin,
    BUSY: InputPin,
{
    /// Takes the controller's SPI device, data/command output, and `BUSY_N`
    /// input. Nothing is sent.
    pub const fn new(spi: SPI, dc: DC, busy: BUSY) -> Self {
        Self {
            spi,
            dc,
            busy,
            inverted: false,
        }
    }

    /// Returns the bus and pins.
    pub fn release(self) -> (SPI, DC, BUSY) {
        (self.spi, self.dc, self.busy)
    }

    /// Returns whether the controller is busy (`BUSY_N` low).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Busy`] when the input cannot be read.
    pub fn is_busy(&mut self) -> Result<bool, SPI, DC, BUSY> {
        self.busy.is_low().map_err(Error::Busy)
    }

    /// Polls `BUSY_N` every millisecond until it is high.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] after `timeout_ms`, or a pin error.
    pub fn wait_until_idle(
        &mut self,
        delay: &mut impl DelayNs,
        timeout_ms: u32,
    ) -> Result<(), SPI, DC, BUSY> {
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
            self.dc.set_high().map_err(Error::DataCommand)?;
            self.spi.write(data).map_err(Error::Spi)?;
        }
        Ok(())
    }

    /// Runs M5GFX's initialization list: panel and power settings, power on,
    /// register-loaded LUTs, then the normal data interval.
    ///
    /// The caller has reset the controller.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn initialize(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        for step in INIT_SEQUENCE {
            self.command(step.command, step.data)?;
            if step.delay_ms > 0 {
                delay.delay_ms(step.delay_ms);
            }
        }
        self.set_inverted(delay, false)
    }

    /// Selects normal or inverted pixel data (`CDI`).
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn set_inverted(
        &mut self,
        delay: &mut impl DelayNs,
        inverted: bool,
    ) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.inverted = inverted;
        self.command(DATA_INTERVAL, &[self.interval()])
    }

    const fn interval(&self) -> u8 {
        if self.inverted {
            DATA_INTERVAL_INVERTED
        } else {
            DATA_INTERVAL_NORMAL
        }
    }

    const fn interval_without_old(&self) -> u8 {
        if self.inverted {
            DATA_INTERVAL_NO_OLD_INVERTED
        } else {
            DATA_INTERVAL_NO_OLD
        }
    }

    /// Selects `window` and writes it into `ram` from a packed frame whose rows
    /// are [`ROW_BYTES`] wide, MSB first, `1` white. With `invert`, every byte
    /// is complemented on the way out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ShortFrame`] when `frame` is smaller than the panel,
    /// a transport error, or [`Error::Timeout`].
    pub fn write_frame(
        &mut self,
        delay: &mut impl DelayNs,
        ram: FrameRam,
        window: Window,
        frame: &[u8],
        invert: bool,
    ) -> Result<(), SPI, DC, BUSY> {
        if frame.len() < ROW_BYTES.saturating_mul(usize::from(HEIGHT)) {
            return Err(Error::ShortFrame);
        }
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.dc.set_low().map_err(Error::DataCommand)?;
        self.spi
            .write(&[PARTIAL_IN, PARTIAL_WINDOW])
            .map_err(Error::Spi)?;
        self.dc.set_high().map_err(Error::DataCommand)?;
        self.spi.write(&window.parameters()).map_err(Error::Spi)?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.dc.set_low().map_err(Error::DataCommand)?;
        self.spi.write(&[ram.command()]).map_err(Error::Spi)?;
        self.dc.set_high().map_err(Error::DataCommand)?;
        let mut inverted = [0_u8; ROW_BYTES];
        for row in window.top..=window.bottom {
            let start = usize::from(row)
                .saturating_mul(ROW_BYTES)
                .saturating_add(window.first_byte());
            let end = usize::from(row)
                .saturating_mul(ROW_BYTES)
                .saturating_add(window.last_byte())
                .saturating_add(1);
            let bytes = frame.get(start..end).ok_or(Error::ShortFrame)?;
            if invert {
                let out = inverted.get_mut(..bytes.len()).ok_or(Error::ShortFrame)?;
                for (target, source) in out.iter_mut().zip(bytes) {
                    *target = !*source;
                }
                self.spi.write(out).map_err(Error::Spi)?;
            } else {
                self.spi.write(bytes).map_err(Error::Spi)?;
            }
        }
        Ok(())
    }

    /// Starts a display refresh of the loaded frames.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`] while waiting to start.
    pub fn refresh(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(DISPLAY_REFRESH, &[])
    }

    /// Turns the charge pumps on and restores the data interval.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn power_on(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.command(DATA_INTERVAL, &[self.interval()])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(POWER_ON, &[])
    }

    /// Turns the charge pumps off, keeping the controller's RAM.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn power_off(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.command(DATA_INTERVAL, &[self.interval_without_old()])?;
        self.wait_until_idle(delay, DEFAULT_BUSY_TIMEOUT_MS)?;
        self.command(POWER_OFF, &[])
    }

    /// Powers off and enters deep sleep; only a hardware reset wakes it.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn deep_sleep(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, DC, BUSY> {
        self.power_off(delay)?;
        self.command(DEEP_SLEEP, &[DEEP_SLEEP_CHECK])
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::{vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as PinErrorType, InputPin, OutputPin},
        spi::{ErrorType, Operation, SpiDevice},
    };

    use super::{Error, FrameRam, HEIGHT, Jd79653a, ROW_BYTES, Window};

    /// One SPI write and the D/C level it was sent with.
    #[derive(Debug, PartialEq, Eq)]
    enum Sent {
        Command(Vec<u8>),
        Data(Vec<u8>),
    }

    #[derive(Default)]
    struct Wire {
        dc_high: bool,
        sent: Vec<Sent>,
    }

    struct Spi<'a>(&'a core::cell::RefCell<Wire>);
    struct Dc<'a>(&'a core::cell::RefCell<Wire>);

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
                        Sent::Command(bytes.to_vec())
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

    /// `BUSY_N` that reads low for `busy_reads` polls, then high.
    struct Busy {
        busy_reads: u32,
    }

    impl PinErrorType for Busy {
        type Error = Infallible;
    }

    impl InputPin for Busy {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(!self.is_low()?)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            if self.busy_reads == 0 {
                Ok(false)
            } else {
                self.busy_reads -= 1;
                Ok(true)
            }
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

    fn driver(
        wire: &core::cell::RefCell<Wire>,
        busy_reads: u32,
    ) -> Jd79653a<Spi<'_>, Dc<'_>, Busy> {
        Jd79653a::new(Spi(wire), Dc(wire), Busy { busy_reads })
    }

    #[test]
    fn initialization_loads_register_luts_and_normal_interval() {
        let wire = core::cell::RefCell::new(Wire::default());
        let mut delay = Delay::default();
        driver(&wire, 0)
            .initialize(&mut delay)
            .expect("initialization");
        let wire = wire.into_inner();
        let commands: Vec<u8> = wire
            .sent
            .iter()
            .filter_map(|sent| match sent {
                Sent::Command(bytes) => Some(bytes[0]),
                Sent::Data(_) => None,
            })
            .collect();
        assert_eq!(
            commands,
            vec![
                0x00, 0x4D, 0xAA, 0xE9, 0xB6, 0xF3, 0x61, 0x60, 0xE3, 0x04, 0x00, 0x20, 0x21, 0x22,
                0x23, 0x24, 0x50
            ]
        );
        let resolution = wire
            .sent
            .iter()
            .position(|sent| *sent == Sent::Command(vec![0x61]))
            .expect("resolution");
        assert_eq!(
            wire.sent[resolution + 1],
            Sent::Data(vec![0xC8, 0x00, 0xC8])
        );
        assert!(wire.sent.contains(&Sent::Data(vec![0xFF, 0x0E])));
        assert_eq!(wire.sent.last(), Some(&Sent::Data(vec![0xD7])));
        assert_eq!(delay.ms, 160);
        let bw = wire
            .sent
            .iter()
            .position(|sent| *sent == Sent::Command(vec![0x22]))
            .expect("BW LUT");
        let Sent::Data(lut) = &wire.sent[bw + 1] else {
            panic!("BW LUT data");
        };
        assert_eq!(lut.len(), 56);
        assert_eq!(&lut[..7], &[0x01, 0x84, 0x84, 0x83, 0x01, 0x01, 0x01]);
        assert!(lut[7..].iter().all(|byte| *byte == 0));
        let vcom = wire
            .sent
            .iter()
            .position(|sent| *sent == Sent::Command(vec![0x20]))
            .expect("VCOM LUT");
        let Sent::Data(lut) = &wire.sent[vcom + 1] else {
            panic!("VCOM LUT data");
        };
        assert_eq!(lut[12], 0x01);
    }

    #[test]
    fn frame_transfer_selects_the_partial_window_then_streams_rows() {
        let wire = core::cell::RefCell::new(Wire::default());
        let mut delay = Delay::default();
        let mut frame = vec![0_u8; ROW_BYTES * usize::from(HEIGHT)];
        frame[ROW_BYTES * 10 + 1] = 0xA5;
        frame[ROW_BYTES * 11 + 2] = 0x5A;
        let window = Window::covering(8, 10, 23, 11).expect("window");
        driver(&wire, 3)
            .write_frame(&mut delay, FrameRam::New, window, &frame, false)
            .expect("transfer");
        let wire = wire.into_inner();
        assert_eq!(
            wire.sent,
            vec![
                Sent::Command(vec![0x91, 0x90]),
                Sent::Data(vec![8, 16, 0, 10, 0, 11, 1]),
                Sent::Command(vec![0x13]),
                Sent::Data(vec![0xA5, 0x00]),
                Sent::Data(vec![0x00, 0x5A]),
            ]
        );
        assert_eq!(delay.ms, 3);
    }

    #[test]
    fn inverted_old_frame_transfer_complements_each_byte() {
        let wire = core::cell::RefCell::new(Wire::default());
        let mut delay = Delay::default();
        let frame = vec![0x0F_u8; ROW_BYTES * usize::from(HEIGHT)];
        driver(&wire, 0)
            .write_frame(&mut delay, FrameRam::Old, Window::FULL, &frame, true)
            .expect("transfer");
        let wire = wire.into_inner();
        assert_eq!(wire.sent[1], Sent::Data(vec![0, 192, 0, 0, 0, 199, 1]));
        assert_eq!(wire.sent[2], Sent::Command(vec![0x10]));
        assert_eq!(wire.sent.len(), 3 + usize::from(HEIGHT));
        assert_eq!(wire.sent[3], Sent::Data(vec![0xF0; ROW_BYTES]));
    }

    #[test]
    fn window_rejects_ranges_outside_the_panel() {
        assert!(Window::covering(0, 0, 200, 10).is_none());
        assert!(Window::covering(10, 0, 9, 10).is_none());
        assert_eq!(Window::covering(0, 0, 199, 199), Some(Window::FULL));
    }

    #[test]
    fn short_frames_and_stuck_busy_fail() {
        let wire = core::cell::RefCell::new(Wire::default());
        let mut delay = Delay::default();
        let mut jd = driver(&wire, u32::MAX);
        assert_eq!(
            jd.write_frame(&mut delay, FrameRam::New, Window::FULL, &[0; 10], false),
            Err(Error::ShortFrame)
        );
        assert_eq!(jd.refresh(&mut delay), Err(Error::Timeout));
        assert_eq!(delay.ms, super::DEFAULT_BUSY_TIMEOUT_MS);
    }

    #[test]
    fn sleep_drops_the_old_frame_before_power_off() {
        let wire = core::cell::RefCell::new(Wire::default());
        let mut delay = Delay::default();
        driver(&wire, 0).deep_sleep(&mut delay).expect("sleep");
        assert_eq!(
            wire.into_inner().sent,
            vec![
                Sent::Command(vec![0x50]),
                Sent::Data(vec![0xF7]),
                Sent::Command(vec![0x02]),
                Sent::Command(vec![0x07]),
                Sent::Data(vec![0xA5]),
            ]
        );
    }
}
