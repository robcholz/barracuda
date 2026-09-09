//! BMI270 six-axis IMU implementation built on the `bmi2` register driver.

#![no_std]

use core::marker::PhantomData;

use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample, Vector3},
};
use bmi2::{
    Bmi2, I2cAddr,
    config::BMI270_CONFIG_FILE,
    interface::I2cInterface,
    types::{
        AccBwp, AccConf, AccRange, Burst, Error as Bmi2Error, GyrBwp, GyrConf, GyrRange,
        GyrRangeVal, Odr, OisRange, PerfMode, PwrCtrl,
    },
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const TRANSFER_BUFFER_BYTES: usize = 512;

/// BMI270 accelerometer full-scale range.
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
    const fn driver(self) -> AccRange {
        match self {
            Self::G2 => AccRange::Range2g,
            Self::G4 => AccRange::Range4g,
            Self::G8 => AccRange::Range8g,
            Self::G16 => AccRange::Range16g,
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

/// BMI270 gyroscope full-scale range.
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
    const fn driver(self) -> GyrRangeVal {
        match self {
            Self::Dps250 => GyrRangeVal::Range250,
            Self::Dps500 => GyrRangeVal::Range500,
            Self::Dps1000 => GyrRangeVal::Range1000,
            Self::Dps2000 => GyrRangeVal::Range2000,
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

/// BMI270 accelerometer and gyroscope output data rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleRate {
    /// 50 Hz.
    Hz50,
    /// 100 Hz.
    Hz100,
    /// 200 Hz.
    Hz200,
    /// 400 Hz.
    Hz400,
}

impl SampleRate {
    const fn driver(self) -> Odr {
        match self {
            Self::Hz50 => Odr::Odr50,
            Self::Hz100 => Odr::Odr100,
            Self::Hz200 => Odr::Odr200,
            Self::Hz400 => Odr::Odr400,
        }
    }

    const fn hertz(self) -> u32 {
        match self {
            Self::Hz50 => 50,
            Self::Hz100 => 100,
            Self::Hz200 => 200,
            Self::Hz400 => 400,
        }
    }
}

/// Board-owned BMI270 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bmi270Config {
    address: u8,
    accelerometer_range: AccelerometerRange,
    gyroscope_range: GyroscopeRange,
    sample_rate: SampleRate,
    axes: AxisTransform,
}

impl Bmi270Config {
    /// Creates one validated static sensor profile.
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

/// Move-only resources consumed by the BMI270 implementation.
pub struct Bmi270Bindings<I2C, DELAY> {
    i2c: I2C,
    delay: DELAY,
}

impl<I2C, DELAY> Bmi270Bindings<I2C, DELAY> {
    /// Combines the shared I2C device view and initialization delay.
    #[must_use]
    pub const fn new(i2c: I2C, delay: DELAY) -> Self {
        Self { i2c, delay }
    }
}

/// BMI270 initialization failure.
#[derive(Debug)]
pub enum Bmi270InitError<BusError> {
    /// Only the hardware addresses `0x68` and `0x69` are valid.
    InvalidAddress(u8),
    /// The underlying BMI2 register operation failed.
    Driver(Bmi2Error<BusError>),
}

/// BMI270 sampling failure.
#[derive(Debug)]
pub enum Bmi270Error<BusError> {
    /// The underlying BMI2 register operation failed.
    Driver(Bmi2Error<BusError>),
}

type Device<I2C, DELAY> = Bmi2<I2cInterface<I2C>, DELAY, TRANSFER_BUFFER_BYTES>;

/// Initialized BMI270 inertial-measurement peripheral.
pub struct Bmi270Imu<I2C, DELAY> {
    device: Device<I2C, DELAY>,
    config: Bmi270Config,
    descriptor: ImuDescriptor,
}

/// Static BMI270 factory used by generated Board composition.
pub struct Bmi270ImuImplementation<I2C, DELAY>(PhantomData<I2C>, PhantomData<DELAY>);

impl<I2C, DELAY> PeripheralImplementation for Bmi270ImuImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Bmi270Bindings<I2C, DELAY>;
    type Config = Bmi270Config;
    type Peripheral = Bmi270Imu<I2C, DELAY>;
    type Error = Bmi270InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let address = match config.address {
            0x68 => I2cAddr::Default,
            0x69 => I2cAddr::Alternative,
            other => return Err(Bmi270InitError::InvalidAddress(other)),
        };
        let mut device = Bmi2::new_i2c(
            bindings.i2c,
            bindings.delay,
            address,
            Burst::new(TRANSFER_BUFFER_BYTES as u16),
        );
        device
            .init(&BMI270_CONFIG_FILE)
            .map_err(Bmi270InitError::Driver)?;
        device
            .disable_power_save()
            .map_err(Bmi270InitError::Driver)?;
        device
            .set_acc_conf(AccConf {
                odr: config.sample_rate.driver(),
                bwp: AccBwp::NormAvg4,
                filter_perf: PerfMode::Perf,
            })
            .map_err(Bmi270InitError::Driver)?;
        device
            .set_acc_range(config.accelerometer_range.driver())
            .map_err(Bmi270InitError::Driver)?;
        device
            .set_gyr_conf(GyrConf {
                odr: config.sample_rate.driver(),
                bwp: GyrBwp::Norm,
                noise_perf: PerfMode::Perf,
                filter_perf: PerfMode::Perf,
            })
            .map_err(Bmi270InitError::Driver)?;
        device
            .set_gyr_range(GyrRange {
                range: config.gyroscope_range.driver(),
                ois_range: OisRange::Range250,
            })
            .map_err(Bmi270InitError::Driver)?;
        device
            .set_pwr_ctrl(PwrCtrl {
                aux_en: false,
                gyr_en: true,
                acc_en: true,
                temp_en: true,
            })
            .map_err(Bmi270InitError::Driver)?;
        device
            .enable_power_save()
            .map_err(Bmi270InitError::Driver)?;

        let rate = config.sample_rate.hertz();
        Ok(Bmi270Imu {
            device,
            config,
            descriptor: ImuDescriptor::new(
                rate,
                rate,
                u32::try_from(config.accelerometer_range.full_scale_g())
                    .unwrap_or_default()
                    .saturating_mul(1_000),
                u32::try_from(config.gyroscope_range.full_scale_dps())
                    .unwrap_or_default()
                    .saturating_mul(1_000),
                true,
            ),
        })
    }
}

impl<I2C, DELAY> Imu for Bmi270Imu<I2C, DELAY>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    type Error = Bmi270Error<I2C::Error>;

    fn descriptor(&self) -> ImuDescriptor {
        self.descriptor
    }

    async fn read_sample(&mut self) -> Result<ImuSample, Self::Error> {
        let data = self.device.get_data().map_err(Bmi270Error::Driver)?;
        let temperature = self
            .device
            .get_temperature()
            .map_err(Bmi270Error::Driver)?
            .map(|celsius| (celsius * 1_000.0) as i32);
        let acceleration = self.config.axes.apply(Vector3::new(
            scale(data.acc.x, self.config.accelerometer_range.full_scale_g()),
            scale(data.acc.y, self.config.accelerometer_range.full_scale_g()),
            scale(data.acc.z, self.config.accelerometer_range.full_scale_g()),
        ));
        let angular_velocity = self.config.axes.apply(Vector3::new(
            scale(data.gyr.x, self.config.gyroscope_range.full_scale_dps()),
            scale(data.gyr.y, self.config.gyroscope_range.full_scale_dps()),
            scale(data.gyr.z, self.config.gyroscope_range.full_scale_dps()),
        ));
        Ok(ImuSample::new(acceleration, angular_velocity, temperature))
    }
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
    #![allow(clippy::expect_used)]

    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        imu::{AxisTransform, Imu},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        AccelerometerRange, Bmi270Bindings, Bmi270Config, Bmi270ImuImplementation, GyroscopeRange,
        SampleRate,
    };

    struct TestBus;

    impl ErrorType for TestBus {
        type Error = Infallible;
    }

    impl I2c for TestBus {
        fn read(&mut self, _address: u8, _read: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write(&mut self, _address: u8, _write: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write_read(
            &mut self,
            _address: u8,
            write: &[u8],
            read: &mut [u8],
        ) -> Result<(), Self::Error> {
            match write.first().copied() {
                Some(0x00) => read[0] = 0x24,
                Some(0x21) => read[0] = 0x01,
                Some(0x0c) if read.len() == 15 => read.copy_from_slice(&[
                    0x00, 0x20, 0x00, 0xe0, 0x00, 0x00, 0x00, 0x04, 0x00, 0xfc, 0x00, 0x00, 0x00,
                    0x00, 0x00,
                ]),
                Some(0x22) => read.copy_from_slice(&[0x00, 0x02]),
                _ => read.fill(0),
            }
            Ok(())
        }

        fn transaction(
            &mut self,
            _address: u8,
            _operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct TestDelay;

    impl DelayNs for TestDelay {
        fn delay_ns(&mut self, _nanoseconds: u32) {}
    }

    #[test]
    fn initializes_with_the_bosch_blob_and_converts_samples() {
        let config = Bmi270Config::new(
            0x68,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            AxisTransform::IDENTITY,
        );
        let mut imu = block_on(Bmi270ImuImplementation::initialize(
            Bmi270Bindings::new(TestBus, TestDelay),
            config,
        ))
        .expect("initialize BMI270");

        let sample = block_on(imu.read_sample()).expect("read sample");
        assert_eq!(sample.acceleration_mg().x, 1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
        assert_eq!(sample.angular_velocity_mdps().x, 15_625);
        assert_eq!(sample.angular_velocity_mdps().y, -15_625);
        assert_eq!(sample.temperature_mc(), Some(24_000));
    }
}
