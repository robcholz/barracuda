//! Six-axis IMU implementation for the MPU6886 and the MPU9250 and MPU6050
//! that share its accelerometer and gyroscope registers.
//!
//! M5Stack fitted several of these parts to one product over its life (an
//! MPU9250 in Gray and Fire units before mid-2019, an MPU6886 after). The
//! implementation identifies the part by `WHO_AM_I` when it starts, as
//! M5Unified `IMU_Class::begin` does, and converts its temperature with that
//! part's own scale. The MPU9250's AK8963 magnetometer is not used.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_mpu6886::{
    AccelRange, Error as ChipError, GyroRange, Mpu6886, RawSample, Variant,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample, Vector3},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

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
}

/// Initialized MPU6886-family IMU.
pub struct Mpu6886Imu<I2C> {
    i2c: I2C,
    chip: Mpu6886,
    variant: Variant,
    config: Mpu6886Config,
    descriptor: ImuDescriptor,
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
        if !config.axes.is_permutation() {
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
        let rate = config.sample_rate.hertz();
        Ok(Self {
            i2c,
            chip,
            variant,
            config,
            descriptor: ImuDescriptor::new(
                rate,
                rate,
                full_scale_milli(config.accelerometer_range.full_scale_g()),
                full_scale_milli(config.gyroscope_range.full_scale_dps()),
                true,
            ),
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
        Ok(self.convert(raw))
    }
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
}
