//! Low-level Driver for one Solomon Systech SSD1306 monochrome OLED controller
//! on a four-wire SPI bus (chip select, clock, data, and a data/command line).
//!
//! The SSD1306 owns a 128 x 64 bit GDDRAM arranged as eight pages of eight
//! rows; each data byte fills one column of one page, least-significant bit
//! on top. Smaller glasses use a window of it: a column offset, fewer
//! multiplexed rows, and their COM pin wiring.

#![no_std]

use embedded_hal::{digital::OutputPin, spi::SpiDevice};

/// GDDRAM columns.
pub const COLUMNS: u8 = 128;
/// GDDRAM rows.
pub const ROWS: u8 = 64;
/// Rows per GDDRAM page.
pub const PAGE_ROWS: u8 = 8;

const DISPLAY_OFF: u8 = 0xae;
const DISPLAY_ON: u8 = 0xaf;
const SET_CLOCK_DIVIDE: u8 = 0xd5;
const SET_MULTIPLEX: u8 = 0xa8;
const SET_DISPLAY_OFFSET: u8 = 0xd3;
const SET_START_LINE: u8 = 0x40;
const CHARGE_PUMP: u8 = 0x8d;
const MEMORY_MODE: u8 = 0x20;
const SEGMENT_REMAP_OFF: u8 = 0xa0;
const SEGMENT_REMAP_ON: u8 = 0xa1;
const COM_SCAN_INCREMENT: u8 = 0xc0;
const COM_SCAN_DECREMENT: u8 = 0xc8;
const SET_COM_PINS: u8 = 0xda;
const SET_CONTRAST: u8 = 0x81;
const SET_PRECHARGE: u8 = 0xd9;
const SET_VCOM_DESELECT: u8 = 0xdb;
const RESUME_FROM_RAM: u8 = 0xa4;
const NORMAL_DISPLAY: u8 = 0xa6;
const INVERTED_DISPLAY: u8 = 0xa7;
const DEACTIVATE_SCROLL: u8 = 0x2e;
const COLUMN_ADDRESS: u8 = 0x21;
const PAGE_ADDRESS: u8 = 0x22;

/// Oscillator frequency and divide ratio after reset.
const CLOCK_DIVIDE_DEFAULT: u8 = 0x80;
/// Internal charge pump on (`8Dh 14h`).
const CHARGE_PUMP_ENABLE: u8 = 0x14;
/// Horizontal addressing: columns advance, then pages.
const HORIZONTAL_ADDRESSING: u8 = 0x00;
/// Pre-charge and VCOMH levels used by M5Stack's M5GFX `Panel_SSD1306`.
const PRECHARGE_PERIODS: u8 = 0x11;
const VCOM_DESELECT_LEVEL: u8 = 0x10;

/// The glass's window into GDDRAM and its COM pin wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    width: u8,
    height: u8,
    column_offset: u8,
    com_pins: u8,
}

impl Geometry {
    /// Describes a `width` x `height` glass starting at GDDRAM column
    /// `column_offset`, with the `DAh` COM pins configuration byte
    /// (`0x02` sequential, `0x12` alternative, `0x22`/`0x32` remapped).
    #[must_use]
    pub const fn new(width: u8, height: u8, column_offset: u8, com_pins: u8) -> Self {
        Self {
            width,
            height,
            column_offset,
            com_pins,
        }
    }

    /// Returns the visible width in pixels.
    #[must_use]
    pub const fn width(self) -> u8 {
        self.width
    }

    /// Returns the visible height in pixels.
    #[must_use]
    pub const fn height(self) -> u8 {
        self.height
    }

    /// Returns the number of eight-row pages the glass covers.
    #[must_use]
    pub const fn pages(self) -> u8 {
        self.height >> 3
    }

    /// Returns the bytes of one full frame: one per column per page.
    #[must_use]
    pub const fn frame_len(self) -> usize {
        (self.width as usize).saturating_mul(self.pages() as usize)
    }

    const fn is_valid(self) -> bool {
        let fits = match COLUMNS.checked_sub(self.width) {
            Some(room) => self.column_offset <= room,
            None => false,
        };
        self.width != 0
            && fits
            && self.height != 0
            && self.height <= ROWS
            && self.height & (PAGE_ROWS - 1) == 0
            && self.com_pins & !0x30 == 0x02
    }
}

/// SSD1306 configuration or transport failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<SpiError, DcError> {
    /// The geometry does not fit the controller's GDDRAM.
    InvalidGeometry,
    /// A page write did not cover whole rows inside the glass.
    InvalidLength,
    /// The SPI transfer failed.
    Spi(SpiError),
    /// The data/command output failed.
    Dc(DcError),
}

/// One SSD1306 on its SPI device and data/command output.
pub struct Ssd1306<SPI, DC> {
    spi: SPI,
    dc: DC,
    geometry: Geometry,
}

/// [`Error`] for one SPI device and data/command output.
pub type ErrorFor<SPI, DC> = Error<
    <SPI as embedded_hal::spi::ErrorType>::Error,
    <DC as embedded_hal::digital::ErrorType>::Error,
>;

impl<SPI, DC> Ssd1306<SPI, DC>
where
    SPI: SpiDevice,
    DC: OutputPin,
{
    /// Takes the controller's bus resources and its glass geometry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidGeometry`] for a window outside GDDRAM.
    pub fn new(spi: SPI, dc: DC, geometry: Geometry) -> Result<Self, ErrorFor<SPI, DC>> {
        if !geometry.is_valid() {
            return Err(Error::InvalidGeometry);
        }
        Ok(Self { spi, dc, geometry })
    }

    /// Returns the glass geometry.
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Configures the controller for the glass and turns the panel on with
    /// `contrast`. The GDDRAM content is undefined until written.
    ///
    /// # Errors
    ///
    /// Returns a transport failure.
    pub fn init(&mut self, contrast: u8) -> Result<(), ErrorFor<SPI, DC>> {
        let multiplex = self.geometry.height.saturating_sub(1);
        self.commands(&[
            DISPLAY_OFF,
            SET_CLOCK_DIVIDE,
            CLOCK_DIVIDE_DEFAULT,
            SET_MULTIPLEX,
            multiplex,
            SET_DISPLAY_OFFSET,
            0,
            SET_START_LINE,
            CHARGE_PUMP,
            CHARGE_PUMP_ENABLE,
            MEMORY_MODE,
            HORIZONTAL_ADDRESSING,
            SEGMENT_REMAP_OFF,
            COM_SCAN_INCREMENT,
            SET_COM_PINS,
            self.geometry.com_pins,
            SET_CONTRAST,
            contrast,
            SET_PRECHARGE,
            PRECHARGE_PERIODS,
            SET_VCOM_DESELECT,
            VCOM_DESELECT_LEVEL,
            RESUME_FROM_RAM,
            NORMAL_DISPLAY,
            DEACTIVATE_SCROLL,
            DISPLAY_ON,
        ])
    }

    /// Sets the OLED drive contrast, the panel's brightness.
    ///
    /// # Errors
    ///
    /// Returns a transport failure.
    pub fn set_contrast(&mut self, contrast: u8) -> Result<(), ErrorFor<SPI, DC>> {
        self.commands(&[SET_CONTRAST, contrast])
    }

    /// Turns the panel on or puts the controller to sleep.
    ///
    /// # Errors
    ///
    /// Returns a transport failure.
    pub fn set_display_on(&mut self, on: bool) -> Result<(), ErrorFor<SPI, DC>> {
        self.commands(&[if on { DISPLAY_ON } else { DISPLAY_OFF }])
    }

    /// Shows GDDRAM bits inverted.
    ///
    /// # Errors
    ///
    /// Returns a transport failure.
    pub fn set_inverted(&mut self, inverted: bool) -> Result<(), ErrorFor<SPI, DC>> {
        self.commands(&[if inverted {
            INVERTED_DISPLAY
        } else {
            NORMAL_DISPLAY
        }])
    }

    /// Mirrors both scan directions, turning the image a half turn.
    ///
    /// Rotated, the glass occupies the mirrored column window, so the
    /// following writes address it there.
    ///
    /// # Errors
    ///
    /// Returns a transport failure.
    pub fn set_rotated_half_turn(&mut self, rotated: bool) -> Result<(), ErrorFor<SPI, DC>> {
        if rotated {
            self.commands(&[SEGMENT_REMAP_ON, COM_SCAN_DECREMENT])
        } else {
            self.commands(&[SEGMENT_REMAP_OFF, COM_SCAN_INCREMENT])
        }
    }

    /// Writes whole pages starting at `first_page`: `data` holds the glass
    /// width of column bytes for each page, in order.
    ///
    /// `column_offset` is the glass's first GDDRAM column for the current
    /// scan direction; see [`Self::column_offset`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLength`] for data that is not whole pages
    /// inside the glass, or a transport failure.
    pub fn write_pages(
        &mut self,
        first_page: u8,
        column_offset: u8,
        data: &[u8],
    ) -> Result<(), ErrorFor<SPI, DC>> {
        let width = usize::from(self.geometry.width);
        if data.is_empty() || data.len().checked_rem(width) != Some(0) {
            return Err(Error::InvalidLength);
        }
        let pages = data
            .len()
            .checked_div(width)
            .and_then(|pages| u8::try_from(pages).ok())
            .ok_or(Error::InvalidLength)?;
        let last_page = first_page
            .checked_add(pages)
            .and_then(|end| end.checked_sub(1))
            .filter(|last| *last < self.geometry.pages())
            .ok_or(Error::InvalidLength)?;
        let last_column = column_offset
            .checked_add(self.geometry.width)
            .and_then(|end| end.checked_sub(1))
            .filter(|last| *last < COLUMNS)
            .ok_or(Error::InvalidLength)?;
        self.commands(&[
            COLUMN_ADDRESS,
            column_offset,
            last_column,
            PAGE_ADDRESS,
            first_page,
            last_page,
        ])?;
        self.dc.set_high().map_err(Error::Dc)?;
        self.spi.write(data).map_err(Error::Spi)
    }

    /// Returns the glass's first GDDRAM column for the given scan direction:
    /// the configured offset normally, its mirror when rotated a half turn.
    pub const fn column_offset(&self, rotated_half_turn: bool) -> u8 {
        if rotated_half_turn {
            COLUMNS
                .saturating_sub(self.geometry.width)
                .saturating_sub(self.geometry.column_offset)
        } else {
            self.geometry.column_offset
        }
    }

    /// Returns the bus resources.
    pub fn release(self) -> (SPI, DC) {
        (self.spi, self.dc)
    }

    fn commands(&mut self, bytes: &[u8]) -> Result<(), ErrorFor<SPI, DC>> {
        self.dc.set_low().map_err(Error::Dc)?;
        self.spi.write(bytes).map_err(Error::Spi)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use std::{cell::RefCell, rc::Rc, vec::Vec};

    use embedded_hal::{
        digital::{self, OutputPin},
        spi::{self, Operation, SpiDevice},
    };

    use super::{Error, Geometry, Ssd1306};

    /// One recorded transfer and the data/command level it was sent with.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Transfer {
        Command(Vec<u8>),
        Data(Vec<u8>),
    }

    #[derive(Default)]
    struct Wire {
        dc_high: bool,
        transfers: Vec<Transfer>,
    }

    struct Spi(Rc<RefCell<Wire>>);
    struct Dc(Rc<RefCell<Wire>>);

    impl spi::ErrorType for Spi {
        type Error = spi::ErrorKind;
    }

    impl SpiDevice for Spi {
        fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
            let mut wire = self.0.borrow_mut();
            for operation in operations {
                if let Operation::Write(bytes) = operation {
                    let bytes = bytes.to_vec();
                    let transfer = if wire.dc_high {
                        Transfer::Data(bytes)
                    } else {
                        Transfer::Command(bytes)
                    };
                    wire.transfers.push(transfer);
                }
            }
            Ok(())
        }
    }

    impl digital::ErrorType for Dc {
        type Error = digital::ErrorKind;
    }

    impl OutputPin for Dc {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.0.borrow_mut().dc_high = false;
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.0.borrow_mut().dc_high = true;
            Ok(())
        }
    }

    /// The M5Stack Unit C6L's 64 x 48 glass.
    const GLASS: Geometry = Geometry::new(64, 48, 32, 0x12);

    fn controller(geometry: Geometry) -> (Ssd1306<Spi, Dc>, Rc<RefCell<Wire>>) {
        let wire = Rc::new(RefCell::new(Wire::default()));
        let controller =
            Ssd1306::new(Spi(wire.clone()), Dc(wire.clone()), geometry).expect("valid geometry");
        (controller, wire)
    }

    #[test]
    fn init_multiplexes_the_glass_rows_with_its_com_wiring() {
        let (mut controller, wire) = controller(GLASS);
        controller.init(0x7f).expect("init");
        let transfers = &wire.borrow().transfers;
        let [Transfer::Command(sequence)] = transfers.as_slice() else {
            panic!("one command transfer expected: {transfers:?}");
        };
        assert_eq!(sequence.first(), Some(&0xae));
        assert_eq!(sequence.last(), Some(&0xaf));
        let after = |command: u8| {
            let index = sequence
                .iter()
                .position(|byte| *byte == command)
                .expect("command present");
            sequence[index + 1]
        };
        assert_eq!(after(0xa8), 47);
        assert_eq!(after(0xda), 0x12);
        assert_eq!(after(0x81), 0x7f);
        assert_eq!(after(0x8d), 0x14);
    }

    #[test]
    fn page_writes_address_the_glass_window() {
        let (mut controller, wire) = controller(GLASS);
        let frame = [0x55; 64 * 6];
        controller
            .write_pages(0, controller.column_offset(false), &frame)
            .expect("write frame");
        controller
            .write_pages(5, controller.column_offset(true), &frame[..64])
            .expect("write last page");
        let transfers = &wire.borrow().transfers;
        assert_eq!(
            transfers.as_slice(),
            [
                Transfer::Command(std::vec![0x21, 32, 95, 0x22, 0, 5]),
                Transfer::Data(frame.to_vec()),
                Transfer::Command(std::vec![0x21, 32, 95, 0x22, 5, 5]),
                Transfer::Data(frame[..64].to_vec()),
            ]
        );
    }

    #[test]
    fn rejects_windows_and_writes_outside_the_glass() {
        let wire = Rc::new(RefCell::new(Wire::default()));
        assert!(matches!(
            Ssd1306::new(
                Spi(wire.clone()),
                Dc(wire.clone()),
                Geometry::new(64, 48, 80, 0x12)
            ),
            Err(Error::InvalidGeometry)
        ));
        assert!(matches!(
            Ssd1306::new(
                Spi(wire.clone()),
                Dc(wire.clone()),
                Geometry::new(64, 44, 0, 0x12)
            ),
            Err(Error::InvalidGeometry)
        ));
        let (mut controller, _) = controller(GLASS);
        assert_eq!(
            controller.write_pages(0, 32, &[0; 63]),
            Err(Error::InvalidLength)
        );
        assert_eq!(
            controller.write_pages(5, 32, &[0; 128]),
            Err(Error::InvalidLength)
        );
    }

    #[test]
    fn control_commands_toggle_state() {
        let (mut controller, wire) = controller(GLASS);
        controller.set_display_on(false).expect("off");
        controller.set_inverted(true).expect("invert");
        controller.set_rotated_half_turn(true).expect("rotate");
        controller.set_contrast(3).expect("contrast");
        assert_eq!(
            wire.borrow().transfers.as_slice(),
            [
                Transfer::Command(std::vec![0xae]),
                Transfer::Command(std::vec![0xa7]),
                Transfer::Command(std::vec![0xa1, 0xc8]),
                Transfer::Command(std::vec![0x81, 3]),
            ]
        );
    }
}
