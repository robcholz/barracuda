//! Low-level Driver for the Bosch BMM150 three-axis geomagnetic sensor.
//!
//! The BMM150 answers on I2C at 10h through 13h, or behind a host sensor's
//! auxiliary interface, as on M5Stack's CoreS3 and AtomS3R, where the BMI270
//! reaches it and mirrors its data registers. [`Registers`] abstracts that
//! difference: [`I2cRegisters`] reaches the chip directly, and a host
//! sensor's driver implements it over its auxiliary interface.
//!
//! The register map, start-up sequence, presets, and the floating-point
//! compensation follow Bosch's BMM150 SensorAPI (`bmm150.c`, `bmm150_defs.h`).

#![no_std]

use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The value of the chip-identification register.
pub const CHIP_ID: u8 = 0x32;
/// The first of the eight data registers: X, Y, Z, and the hall resistance,
/// least-significant byte first.
pub const DATA_REGISTER: u8 = 0x42;
/// Bytes from [`DATA_REGISTER`] through the hall resistance.
pub const DATA_LENGTH: usize = 8;
/// The operation-mode and data-rate register.
pub const OP_MODE_REGISTER: u8 = 0x4C;

const CHIP_ID_REGISTER: u8 = 0x40;
const POWER_CONTROL_REGISTER: u8 = 0x4B;
const REPETITIONS_XY_REGISTER: u8 = 0x51;
const REPETITIONS_Z_REGISTER: u8 = 0x52;
/// `DIG_X1`, the first trim register; `DIG_XY1` at 71h is the last.
const TRIM_FIRST_REGISTER: u8 = 0x5D;
const TRIM_LENGTH: usize = 21;

/// Power-control bit: leaves suspend for sleep mode.
const POWER_ON: u8 = 0x01;
/// Bosch `BMM150_SET_SOFT_RESET`, kept with the power-control bit.
const SOFT_RESET: u8 = 0x82;
/// Bosch `BMM150_START_UP_TIME`.
const START_UP_US: u32 = 3_000;
/// Bosch `BMM150_DELAY_SOFT_RESET`.
const SOFT_RESET_US: u32 = 1_000;

/// Bosch `BMM150_OVERFLOW_ADCVAL_XYAXES_FLIP`.
const XY_OVERFLOW: i16 = -4096;
/// Bosch `BMM150_OVERFLOW_ADCVAL_ZAXIS_HALL`.
const Z_OVERFLOW: i16 = -16384;

/// Register access to one BMM150.
pub trait Registers {
    /// Transport failure.
    type Error;

    /// Reads consecutive registers starting at `register`.
    ///
    /// # Errors
    ///
    /// Returns the transport's failure.
    fn read(&mut self, register: u8, bytes: &mut [u8]) -> Result<(), Self::Error>;

    /// Writes one register.
    ///
    /// # Errors
    ///
    /// Returns the transport's failure.
    fn write(&mut self, register: u8, value: u8) -> Result<(), Self::Error>;
}

/// A BMM150 reached directly on an I2C bus.
pub struct I2cRegisters<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> I2cRegisters<'a, I2C> {
    /// Borrows the bus for the BMM150 at `address`.
    #[must_use]
    pub const fn new(i2c: &'a mut I2C, address: u8) -> Self {
        Self { i2c, address }
    }
}

impl<I2C: I2c> Registers for I2cRegisters<'_, I2C> {
    type Error = I2C::Error;

    fn read(&mut self, register: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.i2c.write_read(self.address, &[register], bytes)
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Self::Error> {
        self.i2c.write(self.address, &[register, value])
    }
}

/// BMM150 initialization or sampling failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// The chip-identification register held this value instead of
    /// [`CHIP_ID`].
    ChipId(u8),
    /// A register transfer failed.
    Bus(E),
}

/// How often the BMM150 measures in normal mode (`Data rate` of register
/// 4Ch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRate {
    /// 2 Hz.
    Hz2,
    /// 6 Hz.
    Hz6,
    /// 8 Hz.
    Hz8,
    /// 10 Hz, the reset value.
    Hz10,
    /// 15 Hz.
    Hz15,
    /// 20 Hz.
    Hz20,
    /// 25 Hz.
    Hz25,
    /// 30 Hz.
    Hz30,
}

impl DataRate {
    const fn bits(self) -> u8 {
        match self {
            Self::Hz10 => 0b000,
            Self::Hz2 => 0b001,
            Self::Hz6 => 0b010,
            Self::Hz8 => 0b011,
            Self::Hz15 => 0b100,
            Self::Hz20 => 0b101,
            Self::Hz25 => 0b110,
            Self::Hz30 => 0b111,
        }
    }

    /// Returns the rate in hertz.
    #[must_use]
    pub const fn hertz(self) -> u32 {
        match self {
            Self::Hz2 => 2,
            Self::Hz6 => 6,
            Self::Hz8 => 8,
            Self::Hz10 => 10,
            Self::Hz15 => 15,
            Self::Hz20 => 20,
            Self::Hz25 => 25,
            Self::Hz30 => 30,
        }
    }
}

/// Bosch's repetition presets, trading current for noise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// `BMM150_PRESETMODE_LOWPOWER`.
    LowPower,
    /// `BMM150_PRESETMODE_REGULAR`.
    Regular,
    /// `BMM150_PRESETMODE_ENHANCED`.
    Enhanced,
    /// `BMM150_PRESETMODE_HIGHACCURACY`.
    HighAccuracy,
}

impl Preset {
    /// Returns the `REPXY` and `REPZ` register values (`bmm150_defs.h`).
    const fn repetitions(self) -> (u8, u8) {
        match self {
            Self::LowPower => (0x01, 0x01),
            Self::Regular => (0x04, 0x07),
            Self::Enhanced => (0x07, 0x0D),
            Self::HighAccuracy => (0x17, 0x29),
        }
    }
}

/// Factory trim values stored in the chip, which the compensation needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trim {
    x1: i8,
    y1: i8,
    x2: i8,
    y2: i8,
    z1: u16,
    z2: i16,
    z3: i16,
    z4: i16,
    xy1: u8,
    xy2: i8,
    xyz1: u16,
}

impl Trim {
    /// Decodes registers 5Dh through 71h, read in one block
    /// (Bosch `read_trim_registers`).
    #[must_use]
    pub const fn from_registers(block: &[u8; TRIM_LENGTH]) -> Self {
        // Offsets from 5Dh.
        Self {
            x1: block[0] as i8,
            y1: block[1] as i8,
            z4: i16::from_le_bytes([block[5], block[6]]),
            x2: block[7] as i8,
            y2: block[8] as i8,
            z2: i16::from_le_bytes([block[11], block[12]]),
            z1: u16::from_le_bytes([block[13], block[14]]),
            xyz1: u16::from_le_bytes([block[15], block[16] & 0x7F]),
            z3: i16::from_le_bytes([block[17], block[18]]),
            xy2: block[19] as i8,
            xy1: block[20],
        }
    }

    /// Converts one raw measurement into the field in nanotesla, in the
    /// sensor's own axes, with Bosch's floating-point compensation; `None`
    /// when an axis overflowed or the hall resistance is not valid yet.
    #[must_use]
    pub fn compensate(&self, raw: RawMeasurement) -> Option<[i32; 3]> {
        let x = self.compensate_xy(raw.x, raw.hall, self.x1, self.x2)?;
        let y = self.compensate_xy(raw.y, raw.hall, self.y1, self.y2)?;
        let z = self.compensate_z(raw.z, raw.hall)?;
        Some([nanotesla(x), nanotesla(y), nanotesla(z)])
    }

    /// Bosch `compensate_x` and `compensate_y`, in microtesla.
    fn compensate_xy(&self, raw: i16, hall: u16, axis1: i8, axis2: i8) -> Option<f32> {
        if raw == XY_OVERFLOW || hall == 0 || self.xyz1 == 0 {
            return None;
        }
        let base = f32::from(self.xyz1) * 16_384.0 / f32::from(hall) - 16_384.0;
        let quadratic = f32::from(self.xy2) * (base * base / 268_435_456.0);
        let linear = quadratic + base * f32::from(self.xy1) / 16_384.0;
        let sensitivity = f32::from(axis2) + 160.0;
        let scaled = f32::from(raw) * ((linear + 256.0) * sensitivity);
        Some((scaled / 8_192.0 + f32::from(axis1) * 8.0) / 16.0)
    }

    /// Bosch `compensate_z`, in microtesla.
    fn compensate_z(&self, raw: i16, hall: u16) -> Option<f32> {
        if raw == Z_OVERFLOW || self.z2 == 0 || self.z1 == 0 || self.xyz1 == 0 || hall == 0 {
            return None;
        }
        let offset = f32::from(raw) - f32::from(self.z4);
        let hall_offset = f32::from(hall) - f32::from(self.xyz1);
        let hall_term = f32::from(self.z3) * hall_offset;
        let gain = f32::from(self.z2) + f32::from(self.z1) * f32::from(hall) / 32_768.0;
        Some(((offset * 131_072.0 - hall_term) / (gain * 4.0)) / 16.0)
    }
}

/// Rounds microtesla to whole nanotesla; `as` saturates outside `i32`.
fn nanotesla(microtesla: f32) -> i32 {
    let nanotesla = microtesla * 1_000.0;
    let rounded = if nanotesla < 0.0 {
        nanotesla - 0.5
    } else {
        nanotesla + 0.5
    };
    rounded as i32
}

/// One uncompensated measurement from the eight data registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RawMeasurement {
    x: i16,
    y: i16,
    z: i16,
    hall: u16,
    ready: bool,
}

impl RawMeasurement {
    /// Decodes registers 42h through 49h: thirteen-bit X and Y, fifteen-bit
    /// Z, and the fourteen-bit hall resistance with the data-ready bit
    /// (Bosch `bmm150_read_mag_data`).
    #[must_use]
    pub const fn from_registers(bytes: [u8; DATA_LENGTH]) -> Self {
        Self {
            x: i16::from_le_bytes([bytes[0], bytes[1]]).wrapping_shr(3),
            y: i16::from_le_bytes([bytes[2], bytes[3]]).wrapping_shr(3),
            z: i16::from_le_bytes([bytes[4], bytes[5]]).wrapping_shr(1),
            hall: u16::from_le_bytes([bytes[6], bytes[7]]).wrapping_shr(2),
            ready: bytes[6] & 0x01 != 0,
        }
    }

    /// Returns the raw X, Y, and Z values.
    #[must_use]
    pub const fn axes(self) -> [i16; 3] {
        [self.x, self.y, self.z]
    }

    /// Returns the raw hall resistance.
    #[must_use]
    pub const fn hall(self) -> u16 {
        self.hall
    }

    /// Returns whether a measurement completed since the last read.
    #[must_use]
    pub const fn is_ready(self) -> bool {
        self.ready
    }
}

/// An initialized BMM150 measuring continuously in normal mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bmm150 {
    trim: Trim,
    rate: DataRate,
}

impl Bmm150 {
    /// Takes the chip out of suspend and returns whether it identifies as a
    /// BMM150. Its registers are readable only once it has left suspend.
    ///
    /// # Errors
    ///
    /// Returns the transport's failure, such as no answer on its address.
    pub fn probe<R: Registers, D: DelayNs>(
        registers: &mut R,
        delay: &mut D,
    ) -> Result<bool, Error<R::Error>> {
        registers
            .write(POWER_CONTROL_REGISTER, POWER_ON)
            .map_err(Error::Bus)?;
        delay.delay_us(START_UP_US);
        Ok(read_chip_id(registers)? == CHIP_ID)
    }

    /// Resets the chip, reads its trim, and starts normal mode at `rate`
    /// with the `preset` repetitions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ChipId`] when the chip is not a BMM150, or the
    /// transport's failure.
    pub fn initialize<R: Registers, D: DelayNs>(
        registers: &mut R,
        delay: &mut D,
        preset: Preset,
        rate: DataRate,
    ) -> Result<Self, Error<R::Error>> {
        if !Self::probe(registers, delay)? {
            return Err(Error::ChipId(read_chip_id(registers)?));
        }
        // A soft reset leaves the power-control bit set and every other
        // register at its reset value.
        registers
            .write(POWER_CONTROL_REGISTER, SOFT_RESET | POWER_ON)
            .map_err(Error::Bus)?;
        delay.delay_us(SOFT_RESET_US);
        let mut block = [0; TRIM_LENGTH];
        registers
            .read(TRIM_FIRST_REGISTER, &mut block)
            .map_err(Error::Bus)?;
        let (repetitions_xy, repetitions_z) = preset.repetitions();
        registers
            .write(REPETITIONS_XY_REGISTER, repetitions_xy)
            .map_err(Error::Bus)?;
        registers
            .write(REPETITIONS_Z_REGISTER, repetitions_z)
            .map_err(Error::Bus)?;
        registers
            .write(OP_MODE_REGISTER, normal_mode(rate))
            .map_err(Error::Bus)?;
        Ok(Self {
            trim: Trim::from_registers(&block),
            rate,
        })
    }

    /// Returns the factory trim read at initialization.
    #[must_use]
    pub const fn trim(&self) -> &Trim {
        &self.trim
    }

    /// Returns the measurement rate.
    #[must_use]
    pub const fn rate(&self) -> DataRate {
        self.rate
    }

    /// Returns the operation-mode register value that keeps the chip in
    /// normal mode at this rate. A host that writes the register before each
    /// read, as the BMI270's auxiliary interface does, writes this value.
    #[must_use]
    pub const fn op_mode(&self) -> u8 {
        normal_mode(self.rate)
    }

    /// Reads the latest measurement and compensates it into nanotesla;
    /// `None` when it overflowed.
    ///
    /// # Errors
    ///
    /// Returns the transport's failure.
    pub fn read<R: Registers>(
        &self,
        registers: &mut R,
    ) -> Result<Option<[i32; 3]>, Error<R::Error>> {
        let mut bytes = [0; DATA_LENGTH];
        registers
            .read(DATA_REGISTER, &mut bytes)
            .map_err(Error::Bus)?;
        Ok(self.trim.compensate(RawMeasurement::from_registers(bytes)))
    }
}

/// Normal mode (`Opmode` 00b) at `rate`.
const fn normal_mode(rate: DataRate) -> u8 {
    rate.bits() << 3
}

fn read_chip_id<R: Registers>(registers: &mut R) -> Result<u8, Error<R::Error>> {
    let mut id = [0];
    registers
        .read(CHIP_ID_REGISTER, &mut id)
        .map_err(Error::Bus)?;
    Ok(id[0])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        Bmm150, CHIP_ID, DataRate, Error, I2cRegisters, Preset, RawMeasurement, Registers, Trim,
    };

    /// A BMM150 register file that answers only after leaving suspend.
    struct Chip {
        registers: [u8; 0x80],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Chip {
        fn new(id: u8) -> Self {
            let mut registers = [0; 0x80];
            registers[0x40] = id;
            // Trim chosen so the compensation reduces to simple sums:
            // DIG_XYZ1 equals the hall resistance, DIG_X2 and DIG_Y2 make the
            // sensitivity 256, and DIG_Z1, DIG_Z2 make the Z gain 2048.
            registers[0x5D] = 4; // DIG_X1
            registers[0x5E] = (-6_i8) as u8; // DIG_Y1
            registers[0x64] = 96; // DIG_X2
            registers[0x65] = 96; // DIG_Y2
            [registers[0x68], registers[0x69]] = (-6_144_i16).to_le_bytes(); // DIG_Z2
            [registers[0x6A], registers[0x6B]] = 32_768_u16.to_le_bytes(); // DIG_Z1
            // DIG_XYZ1 8192, with the reserved top bit set to show it is masked.
            [registers[0x6C], registers[0x6D]] = [0x00, 0xA0];
            // X 100, Y -60, Z -40, hall 8192 with data ready.
            registers[0x42..0x4A]
                .copy_from_slice(&[0x20, 0x03, 0x20, 0xFE, 0xB0, 0xFF, 0x01, 0x80]);
            Self {
                registers,
                writes: std::vec::Vec::new(),
            }
        }

        fn powered(&self) -> bool {
            self.registers[0x4B] & 0x01 != 0
        }
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x10);
            let mut register = 0usize;
            for operation in operations {
                match operation {
                    Operation::Write([first, value]) => {
                        self.writes.push((*first, *value));
                        self.registers[usize::from(*first)] = *value;
                    }
                    Operation::Write([first]) => register = usize::from(*first),
                    Operation::Write(_) => {}
                    Operation::Read(bytes) => {
                        for (offset, byte) in bytes.iter_mut().enumerate() {
                            *byte = if self.powered() {
                                self.registers[register + offset]
                            } else {
                                0
                            };
                        }
                    }
                }
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Delay {
        elapsed_us: u32,
    }

    impl DelayNs for Delay {
        fn delay_ns(&mut self, nanoseconds: u32) {
            self.elapsed_us += nanoseconds / 1_000;
        }
    }

    #[test]
    fn initializes_like_the_bosch_sensor_api() {
        let mut chip = Chip::new(CHIP_ID);
        let mut delay = Delay::default();
        let device = Bmm150::initialize(
            &mut I2cRegisters::new(&mut chip, 0x10),
            &mut delay,
            Preset::Regular,
            DataRate::Hz30,
        )
        .expect("initialize");
        assert_eq!(
            chip.writes,
            [
                (0x4B, 0x01),
                (0x4B, 0x83),
                (0x51, 0x04),
                (0x52, 0x07),
                (0x4C, 0x38)
            ]
        );
        // Start-up after power-on, then the soft reset.
        assert_eq!(delay.elapsed_us, 4_000);
        assert_eq!(device.op_mode(), 0x38);
        assert_eq!(device.rate().hertz(), 30);
    }

    #[test]
    fn compensates_into_nanotesla() {
        let mut chip = Chip::new(CHIP_ID);
        let device = Bmm150::initialize(
            &mut I2cRegisters::new(&mut chip, 0x10),
            &mut Delay::default(),
            Preset::Regular,
            DataRate::Hz10,
        )
        .expect("initialize");
        // X: (100 + 4) / 2 uT; Y: (-60 - 6) / 2 uT; Z: -40 uT.
        assert_eq!(
            device.read(&mut I2cRegisters::new(&mut chip, 0x10)),
            Ok(Some([52_000, -33_000, -40_000]))
        );
    }

    #[test]
    fn decodes_the_data_registers() {
        let raw = RawMeasurement::from_registers([0x20, 0x03, 0x20, 0xFE, 0xB0, 0xFF, 0x01, 0x80]);
        assert_eq!(raw.axes(), [100, -60, -40]);
        assert_eq!(raw.hall(), 8_192);
        assert!(raw.is_ready());
    }

    #[test]
    fn reports_an_overflow_as_no_field() {
        let mut block = [0; 21];
        block[7] = 96;
        block[8] = 96;
        [block[11], block[12]] = (-6_144_i16).to_le_bytes();
        [block[13], block[14]] = 32_768_u16.to_le_bytes();
        [block[15], block[16]] = 8_192_u16.to_le_bytes();
        let trim = Trim::from_registers(&block);
        // X at -4096 is Bosch's flip overflow.
        let overflow =
            RawMeasurement::from_registers([0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x01, 0x80]);
        assert_eq!(overflow.axes()[0], -4_096);
        assert_eq!(trim.compensate(overflow), None);
        // A zero hall resistance has no valid compensation either.
        assert_eq!(trim.compensate(RawMeasurement::default()), None);
    }

    #[test]
    fn rejects_another_chip() {
        let mut chip = Chip::new(0x24);
        assert_eq!(
            Bmm150::initialize(
                &mut I2cRegisters::new(&mut chip, 0x10),
                &mut Delay::default(),
                Preset::Regular,
                DataRate::Hz10,
            ),
            Err(Error::ChipId(0x24))
        );
        let mut absent = Chip::new(CHIP_ID);
        // Probing powers the chip on before it reads the identifier.
        assert_eq!(
            Bmm150::probe(
                &mut I2cRegisters::new(&mut absent, 0x10),
                &mut Delay::default()
            ),
            Ok(true)
        );
    }

    struct Failing;

    impl Registers for Failing {
        type Error = ();

        fn read(&mut self, _register: u8, _bytes: &mut [u8]) -> Result<(), Self::Error> {
            Err(())
        }

        fn write(&mut self, _register: u8, _value: u8) -> Result<(), Self::Error> {
            Err(())
        }
    }

    #[test]
    fn reports_transport_failures() {
        assert_eq!(
            Bmm150::probe(&mut Failing, &mut Delay::default()),
            Err(Error::Bus(()))
        );
    }
}
