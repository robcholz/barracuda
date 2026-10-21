//! Six-axis IMU implementation for a product that fitted an MPU6886-family
//! sensor to some units and a BMI270 to others, at the same address.
//!
//! The M5Stack Atom Matrix shipped with an MPU6886 and later with a BMI270.
//! As M5Unified `IMU_Class::begin` does, the implementation first reads the
//! MPU6886 `WHO_AM_I` register and, when no MPU6886, MPU9250, or MPU6050
//! answers, initializes a BMI270 instead. Both paths reuse the dedicated
//! implementations and apply the same Board mounting.

#![no_std]

use core::marker::PhantomData;

use barracuda_bmi270_imu::{
    AccelerometerRange as Bmi270AccelerometerRange, Bmi270Bindings, Bmi270Config, Bmi270Error,
    Bmi270Imu, Bmi270ImuImplementation, Bmi270InitError, GyroscopeRange as Bmi270GyroscopeRange,
    SampleRate as Bmi270SampleRate,
};
use barracuda_driver_mpu6886::Mpu6886;
use barracuda_mpu6886_imu::{
    AccelerometerRange as MpuAccelerometerRange, GyroscopeRange as MpuGyroscopeRange,
    Mpu6886Config, Mpu6886Imu, Mpu6886InitError, SampleRate as MpuSampleRate,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

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

/// Output data rate both parts produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleRate {
    /// 50 Hz.
    Hz50,
    /// 100 Hz.
    Hz100,
    /// 200 Hz.
    Hz200,
}

/// Board-owned sensor profile and mounting, applied to whichever part answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mpu6886Bmi270Config {
    address: u8,
    accelerometer_range: AccelerometerRange,
    gyroscope_range: GyroscopeRange,
    sample_rate: SampleRate,
    axes: AxisTransform,
}

impl Mpu6886Bmi270Config {
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

    const fn mpu6886(self) -> Mpu6886Config {
        Mpu6886Config::new(
            self.address,
            match self.accelerometer_range {
                AccelerometerRange::G2 => MpuAccelerometerRange::G2,
                AccelerometerRange::G4 => MpuAccelerometerRange::G4,
                AccelerometerRange::G8 => MpuAccelerometerRange::G8,
                AccelerometerRange::G16 => MpuAccelerometerRange::G16,
            },
            match self.gyroscope_range {
                GyroscopeRange::Dps250 => MpuGyroscopeRange::Dps250,
                GyroscopeRange::Dps500 => MpuGyroscopeRange::Dps500,
                GyroscopeRange::Dps1000 => MpuGyroscopeRange::Dps1000,
                GyroscopeRange::Dps2000 => MpuGyroscopeRange::Dps2000,
            },
            match self.sample_rate {
                SampleRate::Hz50 => MpuSampleRate::Hz50,
                SampleRate::Hz100 => MpuSampleRate::Hz100,
                SampleRate::Hz200 => MpuSampleRate::Hz200,
            },
            self.axes,
        )
    }

    const fn bmi270(self) -> Bmi270Config {
        Bmi270Config::new(
            self.address,
            match self.accelerometer_range {
                AccelerometerRange::G2 => Bmi270AccelerometerRange::G2,
                AccelerometerRange::G4 => Bmi270AccelerometerRange::G4,
                AccelerometerRange::G8 => Bmi270AccelerometerRange::G8,
                AccelerometerRange::G16 => Bmi270AccelerometerRange::G16,
            },
            match self.gyroscope_range {
                GyroscopeRange::Dps250 => Bmi270GyroscopeRange::Dps250,
                GyroscopeRange::Dps500 => Bmi270GyroscopeRange::Dps500,
                GyroscopeRange::Dps1000 => Bmi270GyroscopeRange::Dps1000,
                GyroscopeRange::Dps2000 => Bmi270GyroscopeRange::Dps2000,
            },
            match self.sample_rate {
                SampleRate::Hz50 => Bmi270SampleRate::Hz50,
                SampleRate::Hz100 => Bmi270SampleRate::Hz100,
                SampleRate::Hz200 => Bmi270SampleRate::Hz200,
            },
            self.axes,
        )
    }
}

/// Move-only resources consumed by the implementation.
pub struct Mpu6886Bmi270Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Mpu6886Bmi270Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and the delay source.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// Initialization failure.
#[derive(Debug)]
pub enum Mpu6886Bmi270InitError<BusError> {
    /// The configured address is not a seven-bit I2C address.
    InvalidAddress(u8),
    /// Nothing acknowledged the identification read.
    Bus(BusError),
    /// An MPU6886-family part answered but did not start.
    Mpu6886(Mpu6886InitError<BusError>),
    /// No MPU6886-family part answered and no BMI270 started either.
    Bmi270(Bmi270InitError<BusError>),
}

/// Sampling failure of whichever part answered.
#[derive(Debug)]
pub enum Mpu6886Bmi270Error<BusError> {
    /// The MPU6886-family read failed.
    Mpu6886(BusError),
    /// The BMI270 read failed.
    Bmi270(Bmi270Error<BusError>),
}

/// The part that answered, initialized.
pub enum Mpu6886Bmi270Imu<I2C, DELAY> {
    /// An MPU6886, MPU9250, or MPU6050.
    Mpu6886(Mpu6886Imu<I2C>),
    /// A BMI270.
    Bmi270(Bmi270Imu<I2C, DELAY>),
}

/// Static factory used by generated Board composition.
pub struct Mpu6886Bmi270ImuImplementation<I2C, DELAY>(PhantomData<fn() -> (I2C, DELAY)>);

impl<I2C, DELAY> PeripheralImplementation for Mpu6886Bmi270ImuImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Mpu6886Bmi270Bindings<I2C, DELAY>;
    type Config = Mpu6886Bmi270Config;
    type Peripheral = Mpu6886Bmi270Imu<I2C, DELAY>;
    type Error = Mpu6886Bmi270InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Mpu6886Bmi270Bindings { mut i2c, mut delay } = bindings;
        let probe = Mpu6886::new(config.address)
            .ok_or(Mpu6886Bmi270InitError::InvalidAddress(config.address))?;
        if probe
            .identify(&mut i2c)
            .map_err(Mpu6886Bmi270InitError::Bus)?
            .is_some()
        {
            return Mpu6886Imu::start(i2c, &mut delay, config.mpu6886())
                .map(Mpu6886Bmi270Imu::Mpu6886)
                .map_err(Mpu6886Bmi270InitError::Mpu6886);
        }
        Bmi270ImuImplementation::initialize(Bmi270Bindings::new(i2c, delay), config.bmi270())
            .await
            .map(Mpu6886Bmi270Imu::Bmi270)
            .map_err(Mpu6886Bmi270InitError::Bmi270)
    }
}

impl<I2C: I2c, DELAY: DelayNs> Mpu6886Bmi270Imu<I2C, DELAY> {
    /// Returns whether the BMI270 answered rather than an MPU6886-family part.
    #[must_use]
    pub const fn is_bmi270(&self) -> bool {
        matches!(self, Self::Bmi270(_))
    }
}

impl<I2C: I2c, DELAY: DelayNs> Imu for Mpu6886Bmi270Imu<I2C, DELAY> {
    type Error = Mpu6886Bmi270Error<I2C::Error>;

    fn descriptor(&self) -> ImuDescriptor {
        match self {
            Self::Mpu6886(imu) => imu.descriptor(),
            Self::Bmi270(imu) => imu.descriptor(),
        }
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        match self {
            Self::Mpu6886(imu) => imu.read_sample().await.map_err(Mpu6886Bmi270Error::Mpu6886),
            Self::Bmi270(imu) => imu.read_sample().await.map_err(Mpu6886Bmi270Error::Bmi270),
        }
    }
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
        AccelerometerRange, GyroscopeRange, Mpu6886Bmi270Bindings, Mpu6886Bmi270Config,
        Mpu6886Bmi270ImuImplementation, SampleRate,
    };

    /// Answers as an MPU6886 when `mpu` is set, else as a BMI270 whose
    /// `WHO_AM_I`-address register reads zero.
    struct Sensor {
        mpu: bool,
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
            let mut register = None;
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => register = bytes.first().copied(),
                    Operation::Read(read) => {
                        read.fill(0);
                        match (self.mpu, register) {
                            // MPU6886 WHO_AM_I; registers read back what
                            // the sequence wrote only for the ones it checks.
                            (true, Some(0x75)) => read[0] = 0x19,
                            (true, Some(0x6b)) => read[0] = 0x01,
                            (true, Some(0x1c)) => read[0] = 0x08,
                            (true, Some(0x1b)) => read[0] = 0x08,
                            (true, Some(0x1a)) => read[0] = 0x01,
                            (true, Some(0x19)) => read[0] = 0x09,
                            (true, Some(0x37)) => read[0] = 0xc0,
                            (true, Some(0x3b)) => read[..2].copy_from_slice(&[0x20, 0x00]),
                            // BMI270: chip ID, then initialization status.
                            (false, Some(0x00)) => read[0] = 0x24,
                            (false, Some(0x21)) => read[0] = 0x01,
                            (false, Some(0x0c)) if read.len() == 15 => read.copy_from_slice(&[
                                0x00, 0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x04, 0x00, 0xfc, 0x00,
                                0x00, 0x00, 0x00, 0x00,
                            ]),
                            _ => {}
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

    fn atom_matrix() -> Mpu6886Bmi270Config {
        // M5Unified AtomMatrix: X and Z inverted on both parts.
        Mpu6886Bmi270Config::new(
            0x68,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            AxisTransform::new(
                SignedAxis::NegativeX,
                SignedAxis::PositiveY,
                SignedAxis::NegativeZ,
            ),
        )
    }

    #[test]
    fn an_answering_mpu6886_is_used() {
        let mut imu = block_on(Mpu6886Bmi270ImuImplementation::initialize(
            Mpu6886Bmi270Bindings::new(Sensor { mpu: true }, Delay),
            atom_matrix(),
        ))
        .expect("initialize");
        assert!(!imu.is_bmi270());
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.acceleration_mg().x, -1_000);
    }

    #[test]
    fn a_bmi270_is_used_when_no_mpu6886_answers() {
        let mut imu = block_on(Mpu6886Bmi270ImuImplementation::initialize(
            Mpu6886Bmi270Bindings::new(Sensor { mpu: false }, Delay),
            atom_matrix(),
        ))
        .expect("initialize");
        assert!(imu.is_bmi270());
        let sample = block_on(imu.read_sample()).expect("sample");
        assert_eq!(sample.acceleration_mg().x, -1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
    }
}
