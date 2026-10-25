//! Six-axis IMU implementation for the MPU6886 and the MPU9250 and MPU6050
//! that share its accelerometer and gyroscope registers.
//!
//! M5Stack fitted several of these parts to one product over its life (an
//! MPU9250 in Gray and Fire units before mid-2019, an MPU6886 after). The
//! implementation identifies the part by `WHO_AM_I` when it starts, as
//! M5Unified `IMU_Class::begin` does, and converts its temperature with that
//! part's own scale. The MPU9250's AK8963 magnetometer is not used.
//!
//! M5Stack fitted a BMM150 magnetometer at 10h on the same bus beside the
//! MPU6886, and none beside the older MPU9250. With
//! [`Magnetometer::Bmm150IfPresent`] the implementation looks for it as
//! M5Unified `IMU_Class::begin` does and reads its field with each sample
//! when it answers; a unit without it starts without a magnetometer.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_bmm150::{
    Bmm150, DataRate as Bmm150DataRate, Error as Bmm150Error, I2cRegisters, Preset as Bmm150Preset,
};
use barracuda_driver_mpu6886::{
    AccelRange, Error as ChipError, GyroRange, Mpu6886, RawSample, Variant,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample, Vector3},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

/// The BMM150's address on M5Stack's internal bus (M5Unified
/// `BMM150_Class`).
const BMM150_ADDRESS: u8 = 0x10;
/// The BMM150 measures at 30 Hz, as M5Unified sets it (4Ch = 38h).
const BMM150_RATE: Bmm150DataRate = Bmm150DataRate::Hz30;

/// A magnetometer read beside the IMU on its bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magnetometer {
    /// None.
    None,
    /// A BMM150 at 10h when one answers, its axes mapped to the Board's by
    /// the transform; nothing otherwise.
    Bmm150IfPresent(AxisTransform),
}

/// The magnetometer part, as a Board names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MagnetometerPart {
    /// None.
    None,
    /// A BMM150 at 10h on the units that have one.
    Bmm150IfPresent,
}

impl MagnetometerPart {
    /// Places the part with its Board mounting.
    #[must_use]
    pub const fn mounted(self, axes: AxisTransform) -> Magnetometer {
        match self {
            Self::None => Magnetometer::None,
            Self::Bmm150IfPresent => Magnetometer::Bmm150IfPresent(axes),
        }
    }
}

pub use barracuda_driver_mpu6886::Variant as Mpu6886Variant;

/// Accelerometer full-scale range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccelerometerRange {
    /// ±2 g.
    G2,
    /// ±4 g.
    G4,
    /// ±8 g.
    G8,
    /// ±16 g.
    G16,
}

impl AccelerometerRange {
    const fn driver(self) -> AccelRange {
        match self {
            Self::G2 => AccelRange::G2,
            Self::G4 => AccelRange::G4,
            Self::G8 => AccelRange::G8,
            Self::G16 => AccelRange::G16,
        }
    }

    /// Returns the full scale in g.
    #[must_use]
    pub const fn full_scale_g(self) -> i32 {
        match self {
            Self::G2 => 2,
            Self::G4 => 4,
            Self::G8 => 8,
            Self::G16 => 16,
        }
    }
}

/// Gyroscope full-scale range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GyroscopeRange {
    /// ±250 degrees per second.
    Dps250,
    /// ±500 degrees per second.
    Dps500,
    /// ±1000 degrees per second.
    Dps1000,
    /// ±2000 degrees per second.
    Dps2000,
}

impl GyroscopeRange {
    const fn driver(self) -> GyroRange {
        match self {
            Self::Dps250 => GyroRange::Dps250,
            Self::Dps500 => GyroRange::Dps500,
            Self::Dps1000 => GyroRange::Dps1000,
            Self::Dps2000 => GyroRange::Dps2000,
        }
    }

    /// Returns the full scale in degrees per second.
    #[must_use]
    pub const fn full_scale_dps(self) -> i32 {
        match self {
            Self::Dps250 => 250,
            Self::Dps500 => 500,
            Self::Dps1000 => 1_000,
            Self::Dps2000 => 2_000,
        }
    }
}

/// Output data rate: the 1 kHz internal rate divided by `1 + SMPLRT_DIV`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleRate {
    /// 50 Hz.
    Hz50,
    /// 100 Hz.
    Hz100,
    /// 200 Hz.
    Hz200,
    /// 250 Hz, M5Unified's rate.
    Hz250,
    /// 500 Hz.
    Hz500,
}

impl SampleRate {
    const fn divider(self) -> u8 {
        match self {
            Self::Hz50 => 19,
            Self::Hz100 => 9,
            Self::Hz200 => 4,
            Self::Hz250 => 3,
            Self::Hz500 => 1,
        }
    }

    const fn hertz(self) -> u32 {
        match self {
            Self::Hz50 => 50,
            Self::Hz100 => 100,
            Self::Hz200 => 200,
            Self::Hz250 => 250,
            Self::Hz500 => 500,
        }
    }
}

/// Board-owned sensor profile and mounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mpu6886Config {
    address: u8,
    accelerometer_range: AccelerometerRange,
    gyroscope_range: GyroscopeRange,
    sample_rate: SampleRate,
    axes: AxisTransform,
    magnetometer: Magnetometer,
}

impl Mpu6886Config {
    /// Creates one static sensor profile.
    #[must_use]
    pub const fn new(
        address: u8,
        accelerometer_range: AccelerometerRange,
        gyroscope_range: GyroscopeRange,
        sample_rate: SampleRate,
        axes: AxisTransform,
    ) -> Self {
        Self {
            address,
            accelerometer_range,
            gyroscope_range,
            sample_rate,
            axes,
            magnetometer: Magnetometer::None,
        }
    }

    /// Adds the magnetometer read beside the IMU.
    #[must_use]
    pub const fn with_magnetometer(self, magnetometer: Magnetometer) -> Self {
        Self {
            magnetometer,
            ..self
        }
    }

    /// Returns the configured seven-bit address.
    #[must_use]
    pub const fn address(&self) -> u8 {
        self.address
    }

    /// Returns the Board mounting transform.
    #[must_use]
    pub const fn axes(&self) -> AxisTransform {
        self.axes
    }
}

/// Move-only resources consumed by the implementation.
pub struct Mpu6886Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Mpu6886Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the initialization delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Mpu6886InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The mounting transform does not use each sensor axis exactly once.
    InvalidAxes,
    /// The chip rejected identification or configuration.
    Chip(ChipError<BusError>),
    /// The BMM150 answered but did not set up.
    Magnetometer(Bmm150Error<BusError>),
}

/// Initialized MPU6886-family IMU.
pub struct Mpu6886Imu<I2C> {
    i2c: I2C,
    chip: Mpu6886,
    variant: Variant,
    config: Mpu6886Config,
    descriptor: ImuDescriptor,
    magnetometer: Option<(Bmm150, AxisTransform)>,
}

impl<I2C: I2c> Mpu6886Imu<I2C> {
    /// Identifies and configures the sensor on an already borrowed bus.
    ///
    /// # Errors
    ///
    /// Returns an address, mounting, identity, or bus failure.
    pub fn start<DELAY: DelayNs>(
        mut i2c: I2C,
        delay: &mut DELAY,
        config: Mpu6886Config,
    ) -> Result<Self, Mpu6886InitError<I2C::Error>> {
        if !config.axes.is_permutation()
            || matches!(config.magnetometer, Magnetometer::Bmm150IfPresent(axes) if !axes.is_permutation())
        {
            return Err(Mpu6886InitError::InvalidAxes);
        }
        let chip =
            Mpu6886::new(config.address).ok_or(Mpu6886InitError::InvalidAddress(config.address))?;
        let variant = chip
            .initialize(
                &mut i2c,
                delay,
                config.accelerometer_range.driver(),
                config.gyroscope_range.driver(),
                config.sample_rate.divider(),
            )
            .map_err(Mpu6886InitError::Chip)?;
        let magnetometer = match config.magnetometer {
            Magnetometer::None => None,
            Magnetometer::Bmm150IfPresent(axes) => {
                start_bmm150(&mut i2c, delay)?.map(|bmm150| (bmm150, axes))
            }
        };
        let rate = config.sample_rate.hertz();
        let descriptor = ImuDescriptor::new(
            rate,
            rate,
            full_scale_milli(config.accelerometer_range.full_scale_g()),
            full_scale_milli(config.gyroscope_range.full_scale_dps()),
            true,
        );
        Ok(Self {
            i2c,
            chip,
            variant,
            config,
            descriptor: match magnetometer {
                Some((bmm150, _)) => descriptor.with_magnetometer(bmm150.rate().hertz()),
                None => descriptor,
            },
            magnetometer,
        })
    }

    /// Returns which part answered at initialization.
    #[must_use]
    pub const fn variant(&self) -> Variant {
        self.variant
    }

    fn convert(&self, raw: RawSample) -> ImuSample {
        let accel_scale = self.config.accelerometer_range.full_scale_g();
        let gyro_scale = self.config.gyroscope_range.full_scale_dps();
        let [ax, ay, az] = raw.acceleration;
        let [gx, gy, gz] = raw.angular_velocity;
        ImuSample::new(
            self.config.axes.apply(Vector3::new(
                scale(ax, accel_scale),
                scale(ay, accel_scale),
                scale(az, accel_scale),
            )),
            self.config.axes.apply(Vector3::new(
                scale(gx, gyro_scale),
                scale(gy, gyro_scale),
                scale(gz, gyro_scale),
            )),
            Some(self.variant.temperature_millicelsius(raw.temperature)),
        )
    }
}

/// Static factory used by generated Board composition.
pub struct Mpu6886ImuImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Mpu6886ImuImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Mpu6886Bindings<I2C, DELAY>;
    type Config = Mpu6886Config;
    type Peripheral = Mpu6886Imu<I2C>;
    type Error = Mpu6886InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Mpu6886Bindings { i2c, mut delay } = bindings;
        Mpu6886Imu::start(i2c, &mut delay, config)
    }
}

impl<I2C: I2c> Imu for Mpu6886Imu<I2C> {
    type Error = I2C::Error;

    fn descriptor(&self) -> ImuDescriptor {
        self.descriptor
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        let raw = self.chip.read_raw(&mut self.i2c)?;
        let sample = self.convert(raw);
        let Some((bmm150, axes)) = self.magnetometer else {
            return Ok(sample);
        };
        match bmm150.read(&mut I2cRegisters::new(&mut self.i2c, BMM150_ADDRESS)) {
            Ok(Some([x, y, z])) => {
                Ok(sample.with_magnetic_field(axes.apply(Vector3::new(x, y, z))))
            }
            Ok(None) | Err(Bmm150Error::ChipId(_)) => Ok(sample),
            Err(Bmm150Error::Bus(error)) => Err(error),
        }
    }
}

/// Looks for the BMM150 and sets it up when it answers as one; a bus
/// failure while looking means no chip answers its address.
fn start_bmm150<I2C: I2c, DELAY: DelayNs>(
    i2c: &mut I2C,
    delay: &mut DELAY,
) -> Result<Option<Bmm150>, Mpu6886InitError<I2C::Error>> {
    let mut registers = I2cRegisters::new(i2c, BMM150_ADDRESS);
    if !Bmm150::probe(&mut registers, delay).unwrap_or(false) {
        return Ok(None);
    }
    Bmm150::initialize(&mut registers, delay, Bmm150Preset::Regular, BMM150_RATE)
        .map(Some)
        .map_err(Mpu6886InitError::Magnetometer)
}

fn full_scale_milli(full_scale: i32) -> u32 {
    u32::try_from(full_scale)
        .unwrap_or_default()
        .saturating_mul(1_000)
}

fn scale(raw: i16, full_scale: i32) -> i32 {
    i32::from(raw)
        .saturating_mul(full_scale)
        .saturating_mul(1_000)
        .checked_div(32_768)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::arithmetic_side_effects)]

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        imu::{AxisTransform, Imu, SignedAxis},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        AccelerometerRange, GyroscopeRange, Mpu6886Bindings, Mpu6886Config,
        Mpu6886ImuImplementation, Mpu6886InitError, Mpu6886Variant, SampleRate,
    };

    struct Sensor {
        registers: [u8; 0x80],
    }

    impl Sensor {
        fn new(who_am_i: u8) -> Self {
            let mut registers = [0; 0x80];
            registers[0x75] = who_am_i;
            // +1 g on X at ±4 g, -1 g on Y, 0 on Z; 29 °C on an MPU6886;
            // +15.625 °/s on gyroscope X at ±500 °/s.
            registers[0x3b..0x49].copy_from_slice(&[
                0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x05, 0x1b, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]);
            Self { registers }
        }
    }

    impl ErrorType for Sensor {
        type Error = Infallible;
    }

    impl I2c for Sensor {
        fn transaction(
            &mut self,
            _address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => pointer = usize::from(*register),
                    Operation::Write([register, value]) => {
                        // Leave the canned sample alone; keep config writes.
                        if *register < 0x3b || *register > 0x48 {
                            self.registers[usize::from(*register)] = *value;
                        }
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                    Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn config(axes: AxisTransform) -> Mpu6886Config {
        Mpu6886Config::new(
            0x68,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            axes,
        )
    }

    #[test]
    fn converts_a_sample_into_portable_units() {
        let mut imu = block_on(Mpu6886ImuImplementation::initialize(
            Mpu6886Bindings::new(Sensor::new(0x19), Delay),
            config(AxisTransform::IDENTITY),
        ))
        .expect("initialize");
        assert_eq!(imu.variant(), Mpu6886Variant::Mpu6886);
        assert_eq!(imu.i2c.registers[0x19], 9, "100 Hz divider");
        assert_eq!(imu.descriptor().accelerometer_range_mg(), 4_000);
        assert_eq!(imu.descriptor().gyroscope_range_mdps(), 500_000);
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.acceleration_mg().x, 1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
        assert_eq!(sample.angular_velocity_mdps().x, 15_625);
        assert_eq!(sample.temperature_mc(), Some(28_999));
    }

    #[test]
    fn applies_the_atom_matrix_mounting_to_an_mpu9250() {
        // M5Unified AtomMatrix: X and Z inverted.
        let mounting = AxisTransform::new(
            SignedAxis::NegativeX,
            SignedAxis::PositiveY,
            SignedAxis::NegativeZ,
        );
        let mut imu = block_on(Mpu6886ImuImplementation::initialize(
            Mpu6886Bindings::new(Sensor::new(0x71), Delay),
            config(mounting),
        ))
        .expect("initialize");
        assert_eq!(imu.variant(), Mpu6886Variant::Mpu9250);
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.acceleration_mg().x, -1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
        assert_eq!(sample.angular_velocity_mdps().x, -15_625);
    }

    #[test]
    fn rejects_a_degenerate_mounting_and_another_chip() {
        let degenerate = AxisTransform::new(
            SignedAxis::PositiveZ,
            SignedAxis::PositiveY,
            SignedAxis::NegativeZ,
        );
        assert!(matches!(
            block_on(Mpu6886ImuImplementation::initialize(
                Mpu6886Bindings::new(Sensor::new(0x19), Delay),
                config(degenerate),
            )),
            Err(Mpu6886InitError::InvalidAxes)
        ));
        assert!(matches!(
            block_on(Mpu6886ImuImplementation::initialize(
                Mpu6886Bindings::new(Sensor::new(0x24), Delay),
                config(AxisTransform::IDENTITY),
            )),
            Err(Mpu6886InitError::Chip(_))
        ));
    }

    /// The MPU6886, and a BMM150 at 10h on units that have one; an absent
    /// chip does not acknowledge its address.
    struct Bus {
        mpu: Sensor,
        bmm150: Option<[u8; 0x80]>,
    }

    impl Bus {
        fn new(with_bmm150: bool) -> Self {
            let mut bmm150 = [0; 0x80];
            bmm150[0x40] = 0x32;
            // Trim that makes X and Y read (raw + DIG_X1) / 2 uT and Z raw uT.
            bmm150[0x64] = 96;
            bmm150[0x65] = 96;
            [bmm150[0x68], bmm150[0x69]] = (-6_144_i16).to_le_bytes();
            [bmm150[0x6A], bmm150[0x6B]] = 32_768_u16.to_le_bytes();
            [bmm150[0x6C], bmm150[0x6D]] = 8_192_u16.to_le_bytes();
            // X 100, Y -60, Z -40, hall 8192.
            bmm150[0x42..0x4A].copy_from_slice(&[0x20, 0x03, 0x20, 0xFE, 0xB0, 0xFF, 0x01, 0x80]);
            Self {
                mpu: Sensor::new(0x19),
                bmm150: with_bmm150.then_some(bmm150),
            }
        }
    }

    impl embedded_hal::i2c::ErrorType for Bus {
        type Error = embedded_hal::i2c::ErrorKind;
    }

    impl I2c for Bus {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            if address != 0x10 {
                return self
                    .mpu
                    .transaction(address, operations)
                    .map_err(|never| match never {});
            }
            let Some(registers) = &mut self.bmm150 else {
                return Err(embedded_hal::i2c::ErrorKind::NoAcknowledge(
                    embedded_hal::i2c::NoAcknowledgeSource::Address,
                ));
            };
            let mut pointer = 0;
            for operation in operations {
                match operation {
                    Operation::Write([register]) => pointer = usize::from(*register),
                    Operation::Write([register, value]) => {
                        registers[usize::from(*register)] = *value;
                    }
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = registers[pointer + offset];
                        }
                    }
                    Operation::Write(_) => {}
                }
            }
            Ok(())
        }
    }

    fn with_bmm150(present: bool) -> super::Mpu6886Imu<Bus> {
        // M5Unified M5Stack MPU6886 + BMM150: the magnetometer's X and Z inverted.
        let mounting = AxisTransform::new(
            SignedAxis::NegativeX,
            SignedAxis::PositiveY,
            SignedAxis::NegativeZ,
        );
        block_on(Mpu6886ImuImplementation::initialize(
            Mpu6886Bindings::new(Bus::new(present), Delay),
            config(AxisTransform::IDENTITY)
                .with_magnetometer(super::MagnetometerPart::Bmm150IfPresent.mounted(mounting)),
        ))
        .expect("initialize")
    }

    #[test]
    fn reads_the_bmm150_beside_the_mpu6886() {
        let mut imu = with_bmm150(true);
        assert_eq!(imu.descriptor().magnetometer_rate_hz(), Some(30));
        let sample = block_on(imu.read_sample()).expect("sample");
        let field = sample.magnetic_field_nt().expect("field");
        assert_eq!((field.x, field.y, field.z), (-50_000, -30_000, 40_000));
        assert_eq!(imu.i2c.bmm150.expect("bmm150")[0x4C], 0x38);
    }

    #[test]
    fn starts_without_a_bmm150_that_does_not_answer() {
        let mut imu = with_bmm150(false);
        assert!(!imu.descriptor().has_magnetometer());
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.magnetic_field_nt(), None);
        assert_eq!(sample.acceleration_mg().x, 1_000);
    }
}
