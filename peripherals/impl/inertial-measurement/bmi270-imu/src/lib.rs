//! BMI270 six-axis IMU implementation built on the `bmi2` register driver.
//!
//! On M5Stack's CoreS3, StackChan, and AtomS3R family a BMM150 magnetometer
//! hangs off the BMI270's auxiliary I2C interface, where the host bus cannot
//! reach it. With [`Magnetometer::Bmm150`] this implementation drives that
//! interface as M5Unified's `BMI270_Class::begin` does: it sets the BMM150
//! up through manual auxiliary transfers, then lets the BMI270 read the
//! BMM150's data registers by itself into its own `AUX_DATA` registers.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_bmm150::{
    Bmm150, DATA_REGISTER as BMM150_DATA_REGISTER, DataRate as Bmm150DataRate,
    Error as Bmm150Error, Preset as Bmm150Preset, RawMeasurement, Registers, Trim,
};
use barracuda_peripheral::{
    PeripheralImplementation,
    imu::{AxisTransform, Imu, ImuDescriptor, ImuSample, Vector3},
};
use bmi2::{
    Bmi2, I2cAddr,
    config::BMI270_CONFIG_FILE,
    interface::I2cInterface,
    types::{
        AccBwp, AccConf, AccRange, AuxConf, AuxData, AuxIfConf, Burst, Error as Bmi2Error, GyrBwp,
        GyrConf, GyrRange, GyrRangeVal, IfConf, Odr, OisRange, PerfMode, PwrCtrl, ReadBurst,
        SpiMode,
    },
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const TRANSFER_BUFFER_BYTES: usize = 512;

/// The BMM150's address on the auxiliary interface (M5Unified
/// `auxSetupMode(0x10)`).
const BMM150_AUX_ADDRESS: u8 = 0x10;
/// The BMM150 measures at 30 Hz, as M5Unified sets it (4Ch = 38h).
const BMM150_RATE: Bmm150DataRate = Bmm150DataRate::Hz30;
/// How often the BMI270 reads the BMM150's data registers.
const AUX_READ_RATE: Odr = Odr::Odr25;
const AUX_READ_RATE_HZ: u32 = 25;
/// Status polls a manual auxiliary transfer may take before the next step
/// goes ahead anyway, as M5Unified's three-tick wait does.
const AUX_BUSY_POLLS: usize = 64;

/// A magnetometer the BMI270 reads through its auxiliary interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Magnetometer {
    /// Nothing is on the auxiliary interface.
    None,
    /// A BMM150 at 10h, its axes mapped to the Board's by the transform.
    Bmm150(AxisTransform),
}

/// The part on the auxiliary interface, as a Board names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MagnetometerPart {
    /// Nothing.
    None,
    /// A BMM150 at 10h.
    Bmm150,
}

impl MagnetometerPart {
    /// Places the part with its Board mounting.
    #[must_use]
    pub const fn mounted(self, axes: AxisTransform) -> Magnetometer {
        match self {
            Self::None => Magnetometer::None,
            Self::Bmm150 => Magnetometer::Bmm150(axes),
        }
    }
}

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
    magnetometer: Magnetometer,
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
            magnetometer: Magnetometer::None,
        }
    }

    /// Adds the magnetometer on the auxiliary interface.
    #[must_use]
    pub const fn with_magnetometer(self, magnetometer: Magnetometer) -> Self {
        Self {
            magnetometer,
            ..self
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
    /// The mounting transform does not use each sensor axis exactly once.
    InvalidAxes,
    /// The underlying BMI2 register operation failed.
    Driver(Bmi2Error<BusError>),
    /// The BMM150 behind the auxiliary interface did not identify or set up.
    Magnetometer(Bmm150Error<Bmi2Error<BusError>>),
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
    magnetometer: Option<(Trim, AxisTransform)>,
}

/// Static BMI270 factory used by generated Board composition.
pub struct Bmi270ImuImplementation<I2C, DELAY>(PhantomData<I2C>, PhantomData<DELAY>);

impl<I2C, DELAY> PeripheralImplementation for Bmi270ImuImplementation<I2C, DELAY>
where
    I2C: I2c + 'static,
    DELAY: DelayNs + Clone + 'static,
{
    type Bindings = Bmi270Bindings<I2C, DELAY>;
    type Config = Bmi270Config;
    type Peripheral = Bmi270Imu<I2C, DELAY>;
    type Error = Bmi270InitError<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        if !config.axes.is_permutation()
            || matches!(config.magnetometer, Magnetometer::Bmm150(axes) if !axes.is_permutation())
        {
            return Err(Bmi270InitError::InvalidAxes);
        }
        let address = match config.address {
            0x68 => I2cAddr::Default,
            0x69 => I2cAddr::Alternative,
            other => return Err(Bmi270InitError::InvalidAddress(other)),
        };
        let mut delay = bindings.delay.clone();
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
        let magnetometer = match config.magnetometer {
            Magnetometer::None => {
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
                None
            }
            // M5Unified keeps advanced power save off while the auxiliary
            // interface runs.
            Magnetometer::Bmm150(axes) => Some((start_bmm150(&mut device, &mut delay)?, axes)),
        };

        let rate = config.sample_rate.hertz();
        let descriptor = ImuDescriptor::new(
            rate,
            rate,
            u32::try_from(config.accelerometer_range.full_scale_g())
                .unwrap_or_default()
                .saturating_mul(1_000),
            u32::try_from(config.gyroscope_range.full_scale_dps())
                .unwrap_or_default()
                .saturating_mul(1_000),
            true,
        );
        Ok(Bmi270Imu {
            device,
            config,
            descriptor: if magnetometer.is_some() {
                descriptor.with_magnetometer(AUX_READ_RATE_HZ)
            } else {
                descriptor
            },
            magnetometer,
        })
    }
}

/// Sets the BMM150 up through manual auxiliary transfers and hands its data
/// registers to the BMI270's automatic reads (M5Unified
/// `BMI270_Class::begin` and `auxSetupMode`).
fn start_bmm150<I2C, DELAY>(
    device: &mut Device<I2C, DELAY>,
    delay: &mut DELAY,
) -> Result<Trim, Bmi270InitError<I2C::Error>>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    device
        .set_if_conf(IfConf {
            spi_mode: SpiMode::Spi4,
            spi_mode_ois: SpiMode::Spi4,
            ois_en: false,
            aux_en: true,
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
        .set_aux_if_conf(aux_interface(true))
        .map_err(Bmi270InitError::Driver)?;
    device
        .set_aux_dev_id(BMM150_AUX_ADDRESS)
        .map_err(Bmi270InitError::Driver)?;
    let bmm150 = Bmm150::initialize(
        &mut Auxiliary { device },
        delay,
        Bmm150Preset::Regular,
        BMM150_RATE,
    )
    .map_err(Bmi270InitError::Magnetometer)?;
    // Automatic mode: the BMI270 reads eight bytes from the data registers
    // at its auxiliary rate, rewriting the BMM150's last manual write (the
    // normal-mode setting) before each read: M5Unified's `AUX_IF_CONF` 4Fh.
    device
        .set_aux_if_conf(aux_interface(false))
        .map_err(Bmi270InitError::Driver)?;
    device
        .set_aux_rd_addr(BMM150_DATA_REGISTER)
        .map_err(Bmi270InitError::Driver)?;
    device
        .set_aux_conf(AuxConf {
            odr: AUX_READ_RATE,
            offset: 0,
        })
        .map_err(Bmi270InitError::Driver)?;
    device
        .set_pwr_ctrl(PwrCtrl {
            aux_en: true,
            gyr_en: true,
            acc_en: true,
            temp_en: true,
        })
        .map_err(Bmi270InitError::Driver)?;
    Ok(*bmm150.trim())
}

/// Eight-byte bursts, in manual mode for set-up, or in automatic mode with
/// the write before each read.
const fn aux_interface(manual: bool) -> AuxIfConf {
    AuxIfConf {
        aux_read_burst: ReadBurst::Burst8Byte,
        man_read_burst: ReadBurst::Burst8Byte,
        aux_fcu_write_en: !manual,
        aux_manual_en: manual,
    }
}

/// The BMM150's registers through the BMI270's manual auxiliary transfers.
struct Auxiliary<'a, I2C, DELAY> {
    device: &'a mut Device<I2C, DELAY>,
}

impl<I2C, DELAY> Auxiliary<'_, I2C, DELAY>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    fn wait(&mut self) -> Result<(), Bmi2Error<I2C::Error>> {
        for _ in 0..AUX_BUSY_POLLS {
            if !self.device.get_status()?.aux_dev_busy {
                break;
            }
        }
        Ok(())
    }
}

impl<I2C, DELAY> Registers for Auxiliary<'_, I2C, DELAY>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    type Error = Bmi2Error<I2C::Error>;

    fn read(&mut self, register: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let mut address = register;
        for chunk in bytes.chunks_mut(8) {
            self.device.set_aux_rd_addr(address)?;
            self.wait()?;
            let burst = aux_bytes(&self.device.get_aux_data()?);
            for (target, source) in chunk.iter_mut().zip(burst) {
                *target = source;
            }
            address = address.wrapping_add(8);
        }
        Ok(())
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Self::Error> {
        self.device.set_aux_wr_data(value)?;
        self.device.set_aux_wr_addr(register)?;
        self.wait()
    }
}

/// The eight `AUX_DATA` bytes, which `bmi2` parses into words.
fn aux_bytes(data: &AuxData) -> [u8; 8] {
    let [x0, x1] = data.axis.x.to_le_bytes();
    let [y0, y1] = data.axis.y.to_le_bytes();
    let [z0, z1] = data.axis.z.to_le_bytes();
    let [r0, r1] = data.r.to_le_bytes();
    [x0, x1, y0, y1, z0, z1, r0, r1]
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
        let sample = ImuSample::new(acceleration, angular_velocity, temperature);
        let Some((trim, axes)) = self.magnetometer else {
            return Ok(sample);
        };
        let raw = RawMeasurement::from_registers(aux_bytes(
            &self.device.get_aux_data().map_err(Bmi270Error::Driver)?,
        ));
        Ok(match trim.compensate(raw) {
            Some([x, y, z]) => sample.with_magnetic_field(axes.apply(Vector3::new(x, y, z))),
            None => sample,
        })
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
        imu::{AxisTransform, Imu, SignedAxis},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{
        AccelerometerRange, Bmi270Bindings, Bmi270Config, Bmi270ImuImplementation, Bmi270InitError,
        GyroscopeRange, Magnetometer, SampleRate,
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

    #[derive(Clone)]
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

    #[test]
    fn applies_the_board_mounting_and_rejects_a_degenerate_one() {
        // M5Unified AtomMatrix: X and Z inverted.
        let mounting = AxisTransform::new(
            SignedAxis::NegativeX,
            SignedAxis::PositiveY,
            SignedAxis::NegativeZ,
        );
        let config = |axes| {
            Bmi270Config::new(
                0x68,
                AccelerometerRange::G4,
                GyroscopeRange::Dps500,
                SampleRate::Hz100,
                axes,
            )
        };
        let mut imu = block_on(Bmi270ImuImplementation::initialize(
            Bmi270Bindings::new(TestBus, TestDelay),
            config(mounting),
        ))
        .expect("initialize BMI270");
        let sample = block_on(imu.read_sample()).expect("read sample");
        assert_eq!(sample.acceleration_mg().x, -1_000);
        assert_eq!(sample.acceleration_mg().y, -1_000);
        assert_eq!(sample.angular_velocity_mdps().x, -15_625);

        let degenerate = AxisTransform::new(
            SignedAxis::PositiveX,
            SignedAxis::PositiveX,
            SignedAxis::PositiveZ,
        );
        assert!(matches!(
            block_on(Bmi270ImuImplementation::initialize(
                Bmi270Bindings::new(TestBus, TestDelay),
                config(degenerate),
            )),
            Err(Bmi270InitError::InvalidAxes)
        ));
    }

    /// A BMI270 with a BMM150 on its auxiliary interface: manual transfers
    /// reach the BMM150's registers, and in automatic mode `AUX_DATA` holds
    /// its data registers.
    struct AuxBus {
        manual: bool,
        read_address: u8,
        write_data: u8,
        bmm150: [u8; 0x80],
        bmm150_writes: std::vec::Vec<(u8, u8)>,
        power: u8,
    }

    impl AuxBus {
        fn new() -> Self {
            let mut bmm150 = [0; 0x80];
            bmm150[0x40] = 0x32;
            // The driver crate's trim: X and Y read (raw + DIG_X1) / 2 uT, Z raw uT.
            bmm150[0x5D] = 4;
            bmm150[0x5E] = (-6_i8) as u8;
            bmm150[0x64] = 96;
            bmm150[0x65] = 96;
            [bmm150[0x68], bmm150[0x69]] = (-6_144_i16).to_le_bytes();
            [bmm150[0x6A], bmm150[0x6B]] = 32_768_u16.to_le_bytes();
            [bmm150[0x6C], bmm150[0x6D]] = 8_192_u16.to_le_bytes();
            // X 100, Y -60, Z -40, hall 8192.
            bmm150[0x42..0x4A].copy_from_slice(&[0x20, 0x03, 0x20, 0xFE, 0xB0, 0xFF, 0x01, 0x80]);
            Self {
                manual: false,
                read_address: 0,
                write_data: 0,
                bmm150,
                bmm150_writes: std::vec::Vec::new(),
                power: 0,
            }
        }
    }

    impl ErrorType for AuxBus {
        type Error = Infallible;
    }

    impl I2c for AuxBus {
        fn read(&mut self, _address: u8, _read: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn write(&mut self, _address: u8, write: &[u8]) -> Result<(), Self::Error> {
            if let [register, value] = *write {
                match register {
                    0x4C => self.manual = value & 0x80 != 0,
                    0x4D => self.read_address = value,
                    0x4E if self.manual => {
                        self.bmm150_writes.push((value, self.write_data));
                        self.bmm150[usize::from(value)] = self.write_data;
                    }
                    0x4F => self.write_data = value,
                    0x7D => self.power = value,
                    _ => {}
                }
            }
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
                Some(0x04) => {
                    let first = if self.manual { self.read_address } else { 0x42 };
                    for (offset, byte) in read.iter_mut().enumerate() {
                        *byte = self.bmm150[usize::from(first) + offset];
                    }
                }
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

    #[test]
    fn reads_the_bmm150_behind_the_auxiliary_interface() {
        // M5Unified CoreS3: the magnetometer's Y and Z inverted.
        let mounting = AxisTransform::new(
            SignedAxis::PositiveX,
            SignedAxis::NegativeY,
            SignedAxis::NegativeZ,
        );
        let config = Bmi270Config::new(
            0x69,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            AxisTransform::IDENTITY,
        )
        .with_magnetometer(Magnetometer::Bmm150(mounting));
        let mut imu = block_on(Bmi270ImuImplementation::initialize(
            Bmi270Bindings::new(AuxBus::new(), TestDelay),
            config,
        ))
        .expect("initialize BMI270 and BMM150");
        assert_eq!(imu.descriptor().magnetometer_rate_hz(), Some(25));
        let sample = block_on(imu.read_sample()).expect("read sample");
        let field = sample.magnetic_field_nt().expect("magnetic field");
        assert_eq!((field.x, field.y, field.z), (52_000, 33_000, 40_000));
        assert_eq!(sample.acceleration_mg().x, 1_000);
        let bus = imu.device.release();
        // Power on, soft reset, regular repetitions, normal mode at 30 Hz.
        assert_eq!(
            bus.bmm150_writes,
            [
                (0x4B, 0x01),
                (0x4B, 0x83),
                (0x51, 0x04),
                (0x52, 0x07),
                (0x4C, 0x38)
            ]
        );
        assert!(!bus.manual);
        assert_eq!(bus.power, 0x0F);
    }

    #[test]
    fn leaves_the_auxiliary_interface_off_without_a_magnetometer() {
        let config = Bmi270Config::new(
            0x68,
            AccelerometerRange::G4,
            GyroscopeRange::Dps500,
            SampleRate::Hz100,
            AxisTransform::IDENTITY,
        );
        let mut imu = block_on(Bmi270ImuImplementation::initialize(
            Bmi270Bindings::new(AuxBus::new(), TestDelay),
            config,
        ))
        .expect("initialize BMI270");
        assert!(!imu.descriptor().has_magnetometer());
        assert_eq!(
            block_on(imu.read_sample())
                .expect("sample")
                .magnetic_field_nt(),
            None
        );
        assert!(imu.device.release().bmm150_writes.is_empty());
    }
}
