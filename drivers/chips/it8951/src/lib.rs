//! Low-level Driver for the ITE IT8951 e-paper timing controller over its SPI
//! host interface.
//!
//! Every SPI packet starts with a 16-bit preamble (`0x6000` command, `0x0000`
//! write data, `0x1000` read data) followed by big-endian 16-bit words. The
//! host must see `HRDY` high before it starts a packet. This Driver waits for
//! `HRDY`, then sends the preamble and the packet's words in one chip-select
//! period, as M5Stack's M5GFX `Panel_IT8951` does for commands, single words,
//! reads, and image rows. Multi-word argument lists are sent as one
//! write-data packet per word, each behind its own `HRDY` check (the form the
//! IT8951 SPI programming guide gives for `LCDWriteNData`), because an
//! `embedded-hal` [`SpiDevice`] cannot poll a pin with chip-select held.
//!
//! Command codes, register addresses, and the display-area width guard come
//! from M5GFX `Panel_IT8951.inl`.

#![no_std]

use embedded_hal::{
    delay::DelayNs,
    digital::InputPin,
    spi::{Operation, SpiDevice},
};

const PREAMBLE_COMMAND: [u8; 2] = [0x60, 0x00];
const PREAMBLE_WRITE: [u8; 2] = [0x00, 0x00];
const PREAMBLE_READ: [u8; 2] = [0x10, 0x00];

/// Built-in I80 command: leave standby or sleep.
pub const TCON_SYS_RUN: u16 = 0x0001;
/// Built-in I80 command: standby.
pub const TCON_STANDBY: u16 = 0x0002;
/// Built-in I80 command: sleep.
pub const TCON_SLEEP: u16 = 0x0003;
/// Built-in I80 command: register read.
pub const TCON_REG_RD: u16 = 0x0010;
/// Built-in I80 command: register write.
pub const TCON_REG_WR: u16 = 0x0011;
/// Built-in I80 command: start loading an image area.
pub const TCON_LD_IMG_AREA: u16 = 0x0021;
/// Built-in I80 command: finish loading an image.
pub const TCON_LD_IMG_END: u16 = 0x0022;
/// User command: display an area of a given buffer.
pub const I80_CMD_DPY_BUF_AREA: u16 = 0x0037;
/// User command: read or write VCOM.
pub const I80_CMD_VCOM: u16 = 0x0039;
/// User command: read device information.
pub const I80_CMD_GET_DEV_INFO: u16 = 0x0302;

/// I80 command-protocol control register (bit 0: packed write).
pub const REG_I80CPCR: u16 = 0x0004;
/// Load-image start address register (low word; high word at `+ 2`).
pub const REG_LISAR: u16 = 0x0208;
/// LUT engine status: non-zero while any update is running.
pub const REG_LUTAFSR: u16 = 0x1224;

/// Image buffer address M5GFX falls back to on M5Paper when the device
/// information cannot be read.
pub const M5PAPER_DEFAULT_IMAGE_BUFFER: u32 = 0x0012_36E0;

/// Longest read supported in one packet (the device information block).
pub const MAX_READ_WORDS: usize = 20;

/// Default `HRDY` timeout in milliseconds (M5GFX `_wait_busy`: 4096 ms).
pub const DEFAULT_READY_TIMEOUT_MS: u32 = 4_096;
/// Default display-engine timeout in milliseconds (M5GFX `_check_afsr`).
pub const DEFAULT_DISPLAY_TIMEOUT_MS: u32 = 3_000;

/// Waveform mode of a display update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum UpdateMode {
    /// Full-screen initialization to white (about 2000 ms).
    Init = 0,
    /// Fast monochrome direct update (about 260 ms).
    Du = 1,
    /// High-quality 16-level update (about 450 ms).
    Gc16 = 2,
    /// 16-level update for text on white (about 450 ms).
    Gl16 = 3,
    /// Low-ghosting text update (about 450 ms).
    Glr16 = 4,
    /// Text and graphics update (about 450 ms).
    Gld16 = 5,
    /// Four-level fast update (about 120 ms).
    Du4 = 6,
    /// Anti-aliased monochrome update (about 290 ms).
    A2 = 7,
}

/// Pixel depth of a loaded image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum PixelDepth {
    /// Two bits per pixel.
    Bpp2 = 0,
    /// Three bits per pixel.
    Bpp3 = 1,
    /// Four bits per pixel.
    Bpp4 = 2,
    /// Eight bits per pixel.
    Bpp8 = 3,
}

/// Rotation applied by the controller while loading an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Rotation {
    /// Native orientation.
    Deg0 = 0,
    /// A quarter turn.
    Deg90 = 1,
    /// A half turn.
    Deg180 = 2,
    /// Three quarter turns.
    Deg270 = 3,
}

/// Device information returned by [`I80_CMD_GET_DEV_INFO`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Panel width in pixels.
    pub width: u16,
    /// Panel height in pixels.
    pub height: u16,
    /// Address of the controller's image buffer.
    pub image_buffer_address: u32,
}

impl DeviceInfo {
    /// Decodes the first four information words.
    #[must_use]
    pub fn from_words(words: &[u16; 20]) -> Self {
        Self {
            width: words[0],
            height: words[1],
            image_buffer_address: u32::from(words[3]) << 16 | u32::from(words[2]),
        }
    }

    /// Returns whether the image buffer address looks like a real answer
    /// (M5GFX treats `0` and all ones as a failed read).
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.image_buffer_address != 0 && self.image_buffer_address != u32::MAX
    }
}

/// Rectangle handed to [`It8951::display_area`], in controller coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in pixels; at least one.
    pub width: u16,
    /// Height in pixels; at least one.
    pub height: u16,
}

impl Area {
    /// Widens an area whose left and right edges fall in the same group of
    /// four columns. M5GFX notes that the IT8951 can freeze on such a narrow
    /// update while an overlapping one is still running, and widens it the
    /// same way: to the left when there is more room there, else to the right.
    #[must_use]
    pub fn widened_for_display(self) -> Self {
        let left = self.x;
        let right = self.x.saturating_add(self.width.saturating_sub(1));
        if left & !3 != right & !3 {
            return self;
        }
        let (left, right) = if (left & 3) < 3_u16.saturating_sub(right & 3) {
            ((left & !3).saturating_sub(1), right)
        } else {
            (left, (right.saturating_add(4)) & !3)
        };
        Self {
            x: left,
            y: self.y,
            width: right.saturating_sub(left).saturating_add(1),
            height: self.height,
        }
    }
}

/// IT8951 transport or timing failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, PinError> {
    /// The SPI transfer failed.
    Spi(SpiError),
    /// The `HRDY` input could not be read.
    Ready(PinError),
    /// `HRDY` stayed low, or the display engine stayed busy, past the timeout.
    Timeout,
    /// The controller answered all ones, which M5GFX treats as a crash.
    NotResponding,
    /// More than [`MAX_READ_WORDS`] words were requested in one read.
    ReadTooLong,
}

/// Result type of [`It8951`] operations.
pub type Result<T, SPI, HRDY> = core::result::Result<
    T,
    Error<
        <SPI as embedded_hal::spi::ErrorType>::Error,
        <HRDY as embedded_hal::digital::ErrorType>::Error,
    >,
>;

/// One IT8951 on a full-duplex SPI device with its `HRDY` line.
pub struct It8951<SPI, HRDY> {
    spi: SPI,
    hrdy: HRDY,
}

impl<SPI, HRDY> It8951<SPI, HRDY>
where
    SPI: SpiDevice,
    HRDY: InputPin,
{
    /// Takes the controller's SPI device and `HRDY` input. Nothing is sent.
    pub const fn new(spi: SPI, hrdy: HRDY) -> Self {
        Self { spi, hrdy }
    }

    /// Returns the bus and pin.
    pub fn release(self) -> (SPI, HRDY) {
        (self.spi, self.hrdy)
    }

    /// Polls `HRDY` every millisecond until the host interface is ready.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] after `timeout_ms`, or a pin error.
    pub fn wait_ready(
        &mut self,
        delay: &mut impl DelayNs,
        timeout_ms: u32,
    ) -> Result<(), SPI, HRDY> {
        let mut waited = 0_u32;
        while self.hrdy.is_low().map_err(Error::Ready)? {
            if waited >= timeout_ms {
                return Err(Error::Timeout);
            }
            delay.delay_ms(1);
            waited = waited.saturating_add(1);
        }
        Ok(())
    }

    fn packet(
        &mut self,
        delay: &mut impl DelayNs,
        preamble: [u8; 2],
        word: u16,
    ) -> Result<(), SPI, HRDY> {
        self.wait_ready(delay, DEFAULT_READY_TIMEOUT_MS)?;
        let [high, low] = word.to_be_bytes();
        self.spi
            .write(&[preamble[0], preamble[1], high, low])
            .map_err(Error::Spi)
    }

    /// Sends one command code.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn write_command(
        &mut self,
        delay: &mut impl DelayNs,
        command: u16,
    ) -> Result<(), SPI, HRDY> {
        self.packet(delay, PREAMBLE_COMMAND, command)
    }

    /// Sends one data word.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn write_word(&mut self, delay: &mut impl DelayNs, word: u16) -> Result<(), SPI, HRDY> {
        self.packet(delay, PREAMBLE_WRITE, word)
    }

    /// Sends a command followed by its argument words.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn write_args(
        &mut self,
        delay: &mut impl DelayNs,
        command: u16,
        args: &[u16],
    ) -> Result<(), SPI, HRDY> {
        self.write_command(delay, command)?;
        for arg in args {
            self.write_word(delay, *arg)?;
        }
        Ok(())
    }

    /// Reads `words.len()` data words, at most [`MAX_READ_WORDS`], after the
    /// 16-bit dummy word, in one packet.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ReadTooLong`], a transport error, or
    /// [`Error::Timeout`].
    pub fn read_words(
        &mut self,
        delay: &mut impl DelayNs,
        words: &mut [u16],
    ) -> Result<(), SPI, HRDY> {
        let mut raw = [0_u8; MAX_READ_WORDS * 2];
        let bytes = raw
            .get_mut(..words.len().saturating_mul(2))
            .ok_or(Error::ReadTooLong)?;
        let mut dummy = [0_u8; 2];
        self.wait_ready(delay, DEFAULT_READY_TIMEOUT_MS)?;
        self.spi
            .transaction(&mut [
                Operation::Write(&PREAMBLE_READ),
                Operation::Read(&mut dummy),
                Operation::Read(bytes),
            ])
            .map_err(Error::Spi)?;
        for (word, pair) in words.iter_mut().zip(raw.as_chunks::<2>().0) {
            *word = u16::from_be_bytes(*pair);
        }
        Ok(())
    }

    /// Reads one register.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn read_register(
        &mut self,
        delay: &mut impl DelayNs,
        address: u16,
    ) -> Result<u16, SPI, HRDY> {
        self.write_command(delay, TCON_REG_RD)?;
        self.write_word(delay, address)?;
        let mut value = [0_u16; 1];
        self.read_words(delay, &mut value)?;
        Ok(value[0])
    }

    /// Writes one register.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn write_register(
        &mut self,
        delay: &mut impl DelayNs,
        address: u16,
        value: u16,
    ) -> Result<(), SPI, HRDY> {
        self.write_command(delay, TCON_REG_WR)?;
        self.write_word(delay, address)?;
        self.write_word(delay, value)
    }

    /// Reads the device information block.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn device_info(&mut self, delay: &mut impl DelayNs) -> Result<DeviceInfo, SPI, HRDY> {
        self.write_command(delay, I80_CMD_GET_DEV_INFO)?;
        delay.delay_ms(1);
        let mut words = [0_u16; 20];
        self.read_words(delay, &mut words)?;
        Ok(DeviceInfo::from_words(&words))
    }

    /// Leaves standby or sleep.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn system_run(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, HRDY> {
        self.write_command(delay, TCON_SYS_RUN)
    }

    /// Enters standby.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn standby(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, HRDY> {
        self.write_command(delay, TCON_STANDBY)
    }

    /// Enters sleep.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn sleep(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, HRDY> {
        self.write_command(delay, TCON_SLEEP)
    }

    /// Enables packed writes so image words stream back to back.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn enable_packed_write(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, HRDY> {
        self.write_register(delay, REG_I80CPCR, 0x0001)
    }

    /// Points image loads at `address`.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn set_image_buffer_address(
        &mut self,
        delay: &mut impl DelayNs,
        address: u32,
    ) -> Result<(), SPI, HRDY> {
        let [high0, high1, low0, low1] = address.to_be_bytes();
        self.write_register(
            delay,
            REG_LISAR.saturating_add(2),
            u16::from_be_bytes([high0, high1]),
        )?;
        self.write_register(delay, REG_LISAR, u16::from_be_bytes([low0, low1]))
    }

    /// Reads VCOM in millivolts (magnitude).
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn vcom(&mut self, delay: &mut impl DelayNs) -> Result<u16, SPI, HRDY> {
        self.write_command(delay, I80_CMD_VCOM)?;
        self.write_word(delay, 0x0000)?;
        delay.delay_ms(1);
        let mut value = [0_u16; 1];
        self.read_words(delay, &mut value)?;
        Ok(value[0])
    }

    /// Writes VCOM in millivolts (magnitude).
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn set_vcom(&mut self, delay: &mut impl DelayNs, millivolts: u16) -> Result<(), SPI, HRDY> {
        self.write_command(delay, I80_CMD_VCOM)?;
        self.write_word(delay, 0x0001)?;
        self.write_word(delay, millivolts)
    }

    /// Starts loading `area` of big-endian packed pixels into the image buffer.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn load_image_area(
        &mut self,
        delay: &mut impl DelayNs,
        depth: PixelDepth,
        rotation: Rotation,
        area: Area,
    ) -> Result<(), SPI, HRDY> {
        // Bit 8: big-endian words, so the first pixel is the high nibble of
        // the first byte on the wire.
        let format = 1_u16 << 8 | (depth as u16) << 4 | rotation as u16;
        self.write_args(
            delay,
            TCON_LD_IMG_AREA,
            &[format, area.x, area.y, area.width, area.height],
        )
    }

    /// Streams one row (or any whole number of words) of packed pixels.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn write_pixels(
        &mut self,
        delay: &mut impl DelayNs,
        bytes: &[u8],
    ) -> Result<(), SPI, HRDY> {
        self.wait_ready(delay, DEFAULT_READY_TIMEOUT_MS)?;
        self.spi
            .transaction(&mut [Operation::Write(&PREAMBLE_WRITE), Operation::Write(bytes)])
            .map_err(Error::Spi)
    }

    /// Ends an image load.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn load_image_end(&mut self, delay: &mut impl DelayNs) -> Result<(), SPI, HRDY> {
        self.write_command(delay, TCON_LD_IMG_END)
    }

    /// Starts a display update of `area` from the image buffer at `address`.
    /// The area is widened as [`Area::widened_for_display`] describes.
    ///
    /// # Errors
    ///
    /// Returns a transport error or [`Error::Timeout`].
    pub fn display_area(
        &mut self,
        delay: &mut impl DelayNs,
        area: Area,
        mode: UpdateMode,
        address: u32,
    ) -> Result<(), SPI, HRDY> {
        let area = area.widened_for_display();
        let [high0, high1, low0, low1] = address.to_be_bytes();
        self.write_args(
            delay,
            I80_CMD_DPY_BUF_AREA,
            &[
                area.x,
                area.y,
                area.width,
                area.height,
                mode as u16,
                u16::from_be_bytes([low0, low1]),
                u16::from_be_bytes([high0, high1]),
            ],
        )
    }

    /// Returns whether a display update is still running.
    ///
    /// # Errors
    ///
    /// Returns a transport error, [`Error::Timeout`], or
    /// [`Error::NotResponding`].
    pub fn display_busy(&mut self, delay: &mut impl DelayNs) -> Result<bool, SPI, HRDY> {
        match self.read_register(delay, REG_LUTAFSR)? {
            u16::MAX => Err(Error::NotResponding),
            status => Ok(status != 0),
        }
    }

    /// Polls the LUT engine status every millisecond until every update has
    /// finished.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Timeout`] after `timeout_ms`, a transport error, or
    /// [`Error::NotResponding`].
    pub fn wait_display_done(
        &mut self,
        delay: &mut impl DelayNs,
        timeout_ms: u32,
    ) -> Result<(), SPI, HRDY> {
        let mut waited = 0_u32;
        while self.display_busy(delay)? {
            if waited >= timeout_ms {
                return Err(Error::Timeout);
            }
            delay.delay_ms(1);
            waited = waited.saturating_add(1);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;
    use std::{collections::VecDeque, vec, vec::Vec};

    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as PinErrorType, InputPin},
        spi::{ErrorType, Operation, SpiDevice},
    };

    use super::{
        Area, DeviceInfo, Error, It8951, M5PAPER_DEFAULT_IMAGE_BUFFER, PixelDepth, Rotation,
        UpdateMode,
    };

    /// Records each chip-select period's written bytes and answers reads.
    #[derive(Default)]
    struct Spi {
        packets: Vec<Vec<u8>>,
        answers: VecDeque<u8>,
    }

    impl ErrorType for Spi {
        type Error = Infallible;
    }

    impl SpiDevice for Spi {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
            let mut packet = Vec::new();
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => packet.extend_from_slice(bytes),
                    Operation::Read(bytes) => {
                        for byte in bytes.iter_mut() {
                            *byte = self.answers.pop_front().unwrap_or(0);
                        }
                    }
                    _ => {}
                }
            }
            self.packets.push(packet);
            Ok(())
        }
    }

    struct Hrdy {
        low_reads: u32,
    }

    impl PinErrorType for Hrdy {
        type Error = Infallible;
    }

    impl InputPin for Hrdy {
        fn is_high(&mut self) -> Result<bool, Infallible> {
            Ok(!self.is_low()?)
        }

        fn is_low(&mut self) -> Result<bool, Infallible> {
            if self.low_reads == 0 {
                Ok(false)
            } else {
                self.low_reads -= 1;
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

    fn controller(low_reads: u32) -> It8951<Spi, Hrdy> {
        It8951::new(Spi::default(), Hrdy { low_reads })
    }

    #[test]
    fn commands_and_words_carry_their_preambles_big_endian() {
        let mut delay = Delay::default();
        let mut it = controller(2);
        it.write_register(&mut delay, 0x0004, 0x0001)
            .expect("register write");
        let (spi, _) = it.release();
        assert_eq!(
            spi.packets,
            vec![
                vec![0x60, 0x00, 0x00, 0x11],
                vec![0x00, 0x00, 0x00, 0x04],
                vec![0x00, 0x00, 0x00, 0x01],
            ]
        );
        assert_eq!(delay.ms, 2);
    }

    #[test]
    fn device_information_decodes_m5paper_geometry_and_buffer() {
        let mut delay = Delay::default();
        let mut it = controller(0);
        // Dummy word, then width 960, height 540, address 0x0012_36E0.
        it.spi.answers = [0x00, 0x00, 0x03, 0xC0, 0x02, 0x1C, 0x36, 0xE0, 0x00, 0x12]
            .into_iter()
            .collect();
        let info = it.device_info(&mut delay).expect("device info");
        assert_eq!(
            info,
            DeviceInfo {
                width: 960,
                height: 540,
                image_buffer_address: M5PAPER_DEFAULT_IMAGE_BUFFER,
            }
        );
        assert!(info.is_valid());
        let (spi, _) = it.release();
        assert_eq!(spi.packets[0], vec![0x60, 0x00, 0x03, 0x02]);
        assert_eq!(spi.packets[1], vec![0x10, 0x00]);
        assert_eq!(delay.ms, 1);
    }

    #[test]
    fn single_word_reads_skip_the_dummy_word() {
        let mut delay = Delay::default();
        let mut it = controller(0);
        it.spi.answers = [0xAB, 0xCD, 0x00, 0x00].into_iter().collect();
        assert_eq!(it.display_busy(&mut delay), Ok(false));
        let (spi, _) = it.release();
        assert_eq!(
            spi.packets,
            vec![
                vec![0x60, 0x00, 0x00, 0x10],
                vec![0x00, 0x00, 0x12, 0x24],
                vec![0x10, 0x00],
            ]
        );
    }

    #[test]
    fn crashed_controller_is_reported() {
        let mut delay = Delay::default();
        let mut it = controller(0);
        it.spi.answers = [0x00, 0x00, 0xFF, 0xFF].into_iter().collect();
        assert_eq!(it.display_busy(&mut delay), Err(Error::NotResponding));
    }

    #[test]
    fn image_load_and_display_arguments_follow_m5gfx() {
        let mut delay = Delay::default();
        let mut it = controller(0);
        let area = Area {
            x: 16,
            y: 8,
            width: 32,
            height: 2,
        };
        it.load_image_area(&mut delay, PixelDepth::Bpp4, Rotation::Deg0, area)
            .expect("load area");
        it.write_pixels(&mut delay, &[0x12, 0x34]).expect("pixels");
        it.load_image_end(&mut delay).expect("end");
        it.display_area(&mut delay, area, UpdateMode::Gc16, 0x0012_36E0)
            .expect("display");
        let (spi, _) = it.release();
        let words: Vec<Vec<u8>> = spi.packets;
        assert_eq!(words[0], vec![0x60, 0x00, 0x00, 0x21]);
        assert_eq!(words[1], vec![0x00, 0x00, 0x01, 0x20]);
        assert_eq!(words[2], vec![0x00, 0x00, 0x00, 16]);
        assert_eq!(words[5], vec![0x00, 0x00, 0x00, 2]);
        assert_eq!(words[6], vec![0x00, 0x00, 0x12, 0x34]);
        assert_eq!(words[7], vec![0x60, 0x00, 0x00, 0x22]);
        assert_eq!(words[8], vec![0x60, 0x00, 0x00, 0x37]);
        assert_eq!(words[13], vec![0x00, 0x00, 0x00, 0x02]);
        assert_eq!(words[14], vec![0x00, 0x00, 0x36, 0xE0]);
        assert_eq!(words[15], vec![0x00, 0x00, 0x00, 0x12]);
    }

    #[test]
    fn narrow_display_areas_are_widened() {
        let narrow = Area {
            x: 9,
            y: 0,
            width: 2,
            height: 1,
        };
        // Columns 9..=10 share the group 8..=11; more room on the right.
        assert_eq!(
            narrow.widened_for_display(),
            Area {
                x: 9,
                y: 0,
                width: 4,
                height: 1,
            }
        );
        let left = Area {
            x: 8,
            y: 0,
            width: 1,
            height: 1,
        };
        assert_eq!(
            left.widened_for_display(),
            Area {
                x: 7,
                y: 0,
                width: 2,
                height: 1,
            }
        );
        let wide = Area {
            x: 0,
            y: 0,
            width: 960,
            height: 540,
        };
        assert_eq!(wide.widened_for_display(), wide);
    }

    #[test]
    fn stuck_ready_line_times_out() {
        let mut delay = Delay::default();
        let mut it = controller(u32::MAX);
        assert_eq!(it.system_run(&mut delay), Err(Error::Timeout));
        assert_eq!(delay.ms, super::DEFAULT_READY_TIMEOUT_MS);
    }
}
