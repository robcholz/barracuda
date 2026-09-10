//! Register-level Sitronix ST7121/ST7123 display-touch controller driver.

#![no_std]

use embedded_hal::i2c::I2c;

/// One display-controller command and its required settling delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    /// Eight-bit DCS/DBI command.
    pub command: u8,
    /// Command parameters.
    pub data: &'static [u8],
    /// Delay required after the command completes.
    pub delay_ms: u16,
}

include!(concat!(env!("OUT_DIR"), "/panel_commands.rs"));

const FIRMWARE_VERSION: u16 = 0x0000;
const GEOMETRY: u16 = 0x0005;
const ADVANCED_INFO: u16 = 0x0010;
const FIRST_POINT: u16 = 0x0014;
const REPORT_BYTES: usize = 7;
const HAS_COORDINATES: u8 = 1 << 3;

/// Maximum contacts represented by the shared ST712x report type.
pub const MAX_POINTS: usize = 10;

/// Display-controller model encoded in the firmware register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// ST7121 controller.
    St7121,
    /// ST7123 controller.
    St7123,
}

/// Touch geometry reported by the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchInfo {
    /// Horizontal coordinate range.
    pub width: u16,
    /// Vertical coordinate range.
    pub height: u16,
    /// Maximum simultaneous contacts.
    pub max_points: u8,
}

/// One chip-native contact report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    /// Report-slot identifier.
    pub id: u8,
    /// Horizontal coordinate.
    pub x: u16,
    /// Vertical coordinate.
    pub y: u16,
    /// Contact strength.
    pub strength: u16,
}

/// One fixed-capacity ST712x report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    points: [Point; MAX_POINTS],
    len: u8,
}

impl Report {
    /// Returns the valid contacts in this report.
    #[must_use]
    pub fn points(&self) -> &[Point] {
        &self.points[..usize::from(self.len)]
    }
}

/// An ST7121/ST7123 at a caller-selected seven-bit I2C address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct St712x {
    address: u8,
    touch: TouchInfo,
}

impl St712x {
    /// Probes and validates the touch geometry at `address`.
    pub fn probe<I2C: I2c>(i2c: &mut I2C, address: u8) -> Result<Option<Self>, I2C::Error> {
        if address > 0x7f {
            return Ok(None);
        }
        let mut firmware = [0];
        read(i2c, address, FIRMWARE_VERSION, &mut firmware)?;
        let mut geometry = [0; 5];
        read(i2c, address, GEOMETRY, &mut geometry)?;
        let width = u16::from_be_bytes([geometry[0] & 0x3f, geometry[1]]);
        let height = u16::from_be_bytes([geometry[2] & 0x3f, geometry[3]]);
        let max_points = geometry[4].min(MAX_POINTS as u8);
        if firmware[0] == 0 || width == 0 || height == 0 || max_points == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            address,
            touch: TouchInfo {
                width,
                height,
                max_points,
            },
        }))
    }

    /// Reads the display-controller model without requiring valid touch geometry.
    pub fn probe_model<I2C: I2c>(i2c: &mut I2C, address: u8) -> Result<Option<Model>, I2C::Error> {
        if address > 0x7f {
            return Ok(None);
        }
        let mut firmware = [0];
        read(i2c, address, FIRMWARE_VERSION, &mut firmware)?;
        Ok(match firmware[0] {
            1 => Some(Model::St7121),
            3 => Some(Model::St7123),
            _ => None,
        })
    }

    /// Returns the geometry captured during probing.
    #[must_use]
    pub const fn touch_info(&self) -> TouchInfo {
        self.touch
    }

    /// Reads the latest valid contact slots.
    pub fn read_report<I2C: I2c>(&self, i2c: &mut I2C) -> Result<Report, I2C::Error> {
        let mut advanced = [0];
        read(i2c, self.address, ADVANCED_INFO, &mut advanced)?;
        if advanced[0] & HAS_COORDINATES == 0 {
            return Ok(Report {
                points: [Point::default(); MAX_POINTS],
                len: 0,
            });
        }
        let mut data = [0; MAX_POINTS * REPORT_BYTES];
        let length = usize::from(self.touch.max_points).saturating_mul(REPORT_BYTES);
        read(i2c, self.address, FIRST_POINT, &mut data[..length])?;
        let mut points = [Point::default(); MAX_POINTS];
        let mut len = 0_u8;
        let (reports, _) = data[..length].as_chunks::<REPORT_BYTES>();
        for (id, report) in reports.iter().enumerate() {
            if report[0] & 0x80 == 0 {
                continue;
            }
            points[usize::from(len)] = Point {
                id: u8::try_from(id).unwrap_or_default(),
                x: u16::from_be_bytes([report[0] & 0x3f, report[1]]),
                y: u16::from_be_bytes([report[2] & 0x3f, report[3]]),
                strength: u16::from(report[4]),
            };
            len = len.saturating_add(1);
        }
        Ok(Report { points, len })
    }
}

fn read<I2C: I2c>(
    i2c: &mut I2C,
    address: u8,
    register: u16,
    data: &mut [u8],
) -> Result<(), I2C::Error> {
    i2c.write_read(address, &register.to_be_bytes(), data)
}
