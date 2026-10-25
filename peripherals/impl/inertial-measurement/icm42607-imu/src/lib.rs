//! Six-axis IMU implementation for the ICM-42607-P and the ICM-42670-P that
//! shares its registers, configured as Espressif's ESP32-S3-BOX-3 BSP does:
//! both sensors in low-noise mode at one output data rate.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_icm42607::{
    AccelRange, DataRate, Error as ChipError, GyroRange, Icm42607, RawSample, Variant,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample, Vector3},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

pub use barracuda_driver_icm42607::Variant as Icm42607Variant;

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

    const fn full_scale_g(self) -> i32 {
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

    const fn full_scale_dps(self) -> i32 {
        match self {
            Self::Dps250 => 250,
            Self::Dps500 => 500,
            Self::Dps1000 => 1_000,
            Self::Dps2000 => 2_000,
        }
    }
}

/// Output data rate of both sensors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleRate {
    /// 25 Hz.
    Hz25,
    /// 50 Hz.
    Hz50,
    /// 100 Hz.
    Hz100,
    /// 200 Hz.
    Hz200,
    /// 400 Hz.
    Hz400,
    /// 800 Hz.
    Hz800,
}

impl SampleRate {
    const fn driver(self) -> DataRate {
        match self {
            Self::Hz25 => DataRate::Hz25,
            Self::Hz50 => DataRate::Hz50,
            Self::Hz100 => DataRate::Hz100,
            Self::Hz200 => DataRate::Hz200,
            Self::Hz400 => DataRate::Hz400,
            Self::Hz800 => DataRate::Hz800,
        }
    }

    const fn hertz(self) -> u32 {
        match self {
            Self::Hz25 => 25,
            Self::Hz50 => 50,
            Self::Hz100 => 100,
            Self::Hz200 => 200,
            Self::Hz400 => 400,
            Self::Hz800 => 800,
        }
    }
}

/// Board-owned sensor profile and mounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icm42607Config {
    address: u8,
    accelerometer_range: AccelerometerRange,
    gyroscope_range: GyroscopeRange,
    sample_rate: SampleRate,
    axes: AxisTransform,
}

impl Icm42607Config {
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
}

/// Move-only resources consumed by the implementation.
pub struct Icm42607Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Icm42607Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the initialization delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Icm42607InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// The mounting transform does not use each sensor axis exactly once.
    InvalidAxes,
    /// The chip rejected identification or configuration.
    Chip(ChipError<BusError>),
}

/// Initialized ICM-42607 IMU.
pub struct Icm42607Imu<I2C> {
    i2c: I2C,
    chip: Icm42607,
    variant: Variant,
    config: Icm42607Config,
    descriptor: ImuDescriptor,
}

impl<I2C> Icm42607Imu<I2C> {
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
            Some(raw.temperature_millicelsius()),
        )
    }
}

/// Static factory used by generated Board composition.
pub struct Icm42607ImuImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Icm42607ImuImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Icm42607Bindings<I2C, DELAY>;
    type Config = Icm42607Config;
    type Peripheral = Icm42607Imu<I2C>;
    type Error = Icm42607InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if !config.axes.is_permutation() {
            return Err(Icm42607InitError::InvalidAxes);
        }
        let Icm42607Bindings { mut i2c, mut delay } = bindings;
        let chip = Icm42607::new(config.address)
            .ok_or(Icm42607InitError::InvalidAddress(config.address))?;
        let variant = chip
            .initialize(
                &mut i2c,
                &mut delay,
                config.accelerometer_range.driver(),
                config.gyroscope_range.driver(),
                config.sample_rate.driver(),
            )
            .map_err(Icm42607InitError::Chip)?;
        let rate = config.sample_rate.hertz();
        Ok(Icm42607Imu {
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
}

impl<I2C: I2c> Imu for Icm42607Imu<I2C> {
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
        AccelerometerRange, GyroscopeRange, Icm42607Bindings, Icm42607Config,
        Icm42607ImuImplementation, Icm42607InitError, Icm42607Variant, SampleRate,
    };

    struct Sensor {
        registers: [u8; 0x80],
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
                    Operation::Write(bytes) => pointer = usize::from(bytes[0]),
                    Operation::Read(bytes) => {
                        for (offset, value) in bytes.iter_mut().enumerate() {
                            *value = self.registers[pointer + offset];
                        }
                    }
                }
            }
            Ok(())
        }
    }

    struct Delay;

    impl DelayNs for Delay {
        fn delay_ns(&mut self, _ns: u32) {}
    }

    fn sensor(who_am_i: u8) -> Sensor {
        let mut registers = [0; 0x80];
        registers[0x75] = who_am_i;
        // 30 °C; +1 g on X at ±4 g, -1 g on Y; +15.625 °/s on gyroscope Z.
        registers[0x09..0x17].copy_from_slice(&[
            0x02, 0x80, 0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00,
        ]);
        Sensor { registers }
    }

    fn config(axes: AxisTransform) -> Icm42607Config {
        Icm42607Config::new(
            0x68,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            axes,
        )
    }

    #[test]
    fn converts_a_sample_through_the_mounting() {
        let swap = AxisTransform::new(
            SignedAxis::PositiveY,
            SignedAxis::NegativeX,
            SignedAxis::PositiveZ,
        );
        let mut imu = block_on(Icm42607ImuImplementation::initialize(
            Icm42607Bindings::new(sensor(0x60), Delay),
            config(swap),
        ))
        .expect("initialize");
        assert_eq!(imu.variant(), Icm42607Variant::Icm42607);
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.acceleration_mg().x, -1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
        assert_eq!(sample.angular_velocity_mdps().z, 15_625);
        assert_eq!(sample.temperature_mc(), Some(30_000));
    }

    #[test]
    fn rejects_another_chip_and_a_degenerate_mounting() {
        assert!(matches!(
            block_on(Icm42607ImuImplementation::initialize(
                Icm42607Bindings::new(sensor(0x19), Delay),
                config(AxisTransform::IDENTITY),
            )),
            Err(Icm42607InitError::Chip(_))
        ));
        let degenerate = AxisTransform::new(
            SignedAxis::PositiveY,
            SignedAxis::PositiveY,
            SignedAxis::PositiveZ,
        );
        assert!(matches!(
            block_on(Icm42607ImuImplementation::initialize(
                Icm42607Bindings::new(sensor(0x60), Delay),
                config(degenerate),
            )),
            Err(Icm42607InitError::InvalidAxes)
        ));
    }
}
