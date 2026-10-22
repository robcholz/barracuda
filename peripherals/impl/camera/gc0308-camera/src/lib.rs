//! GC0308 camera peripheral implementation: 320x240 RGB565 frames.
//!
//! Initialization switches the sensor's supply on, takes it out of reset,
//! checks its chip ID, and loads esp32-camera's GC0308 table with RGB565
//! output and the 320x240 window, so a Board whose sensor does not answer
//! fails to start. One capture is one whole frame of 153600 bytes.
//!
//! A product that ships an OV3660 in place of the GC0308 on the same wiring
//! names it as the alternate sensor: when no GC0308 answers, the
//! implementation probes the OV3660 at 3Ch, as M5Unified's AtomS3R-CAM
//! detection does, and streams its 320x240 JPEG instead. The descriptor names
//! whichever stream the sensor found produces.
//!
//! On the M5Stack CoreS3 and StackChan the helper chips stay private, shared
//! by register with their other users: this implementation programs only the
//! AXP2101's ALDO3 voltage (the same 3.3 V the ambient-light sensor on the
//! camera board sets) and the ALDO3, BLDO1, and BLDO2 enable bits, and only
//! the AW9523B's P1_0.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_aw9523::{Aw9523, Error as Aw9523Error};
use barracuda_driver_axp2101::{Axp2101, Error as Axp2101Error, Ldo};
use barracuda_driver_gc0308::{Error as SensorError, Gc0308, PixelFormat as SensorPixelFormat};
use barracuda_driver_ov3660::Ov3660;
use barracuda_peripheral::{
    PeripheralImplementation,
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame, FrameReceiver},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
/// Bytes of one 320x240 RGB565 frame.
const RGB565_FRAME_BYTES: usize = 320 * 240 * 2;
const GC0308_DESCRIPTOR: CameraDescriptor =
    CameraDescriptor::new(WIDTH, HEIGHT, CameraPixelFormat::Rgb565);
const OV3660_DESCRIPTOR: CameraDescriptor =
    CameraDescriptor::new(WIDTH, HEIGHT, CameraPixelFormat::Jpeg);

/// The OV3660's SCCB address (M5Unified AtomS3R-CAM detection).
const OV3660_ADDRESS: u8 = 0x3c;
const OV3660_PRODUCT_ID: u16 = 0x3660;

/// AXP2101 and AW9523B addresses on the CoreS3's internal bus.
const PMIC_ADDRESS: u8 = 0x34;
const EXPANDER_ADDRESS: u8 = 0x58;
/// M5Unified `Power_Class::begin` CoreS3: ALDO3 at 3.3 V "for camera".
const CAMERA_SUPPLY_MILLIVOLTS: u16 = 3300;
/// M5Stack CoreS3 pin map: CAM_RST on AW9523B P1_0, which Espressif's
/// m5stack_cores3 power manager drives high to enable the camera.
const CORES3_CAMERA_RESET_PIN: u8 = 8;

/// Supply settling after power-up, and reset low and release times.
const POWER_SETTLE_MS: u32 = 10;
const RESET_LOW_MS: u32 = 10;
const RESET_SETTLE_MS: u32 = 20;

/// JPEG start-of-image and end-of-image markers.
const JPEG_START: [u8; 2] = [0xff, 0xd8];
const JPEG_END: [u8; 2] = [0xff, 0xd9];

/// Which helper chips supply and reset the sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLayout {
    /// Only the Board's reset and power-down bindings, if any.
    None,
    /// CoreS3 and StackChan: AXP2101 ALDO3, BLDO1, and BLDO2, and the reset
    /// on AW9523B P1_0.
    CoreS3,
}

/// Another sensor a product ships on the same wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlternateSensor {
    /// The GC0308 only.
    None,
    /// An OV3660 at 3Ch, streaming 320x240 JPEG.
    Ov3660,
}

/// Board-owned camera configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gc0308Config {
    address: u8,
    layout: PowerLayout,
    alternate: AlternateSensor,
}

impl Gc0308Config {
    /// Creates a configuration for one seven-bit SCCB address, supply, and
    /// alternate sensor.
    #[must_use]
    pub const fn new(address: u8, layout: PowerLayout, alternate: AlternateSensor) -> Self {
        Self {
            address,
            layout,
            alternate,
        }
    }
}

/// Move-only resources consumed by the GC0308 implementation.
pub struct Gc0308Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
    control: I2C,
    capture: CAPTURE,
    reset: Option<RESET>,
    power_down: Option<PWDN>,
    delay: DELAY,
}

impl<I2C, CAPTURE, RESET, PWDN, DELAY> Gc0308Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
    /// Combines the SCCB bus, DVP receiver, control outputs, and delay.
    #[must_use]
    pub const fn new(
        control: I2C,
        capture: CAPTURE,
        reset: Option<RESET>,
        power_down: Option<PWDN>,
        delay: DELAY,
    ) -> Self {
        Self {
            control,
            capture,
            reset,
            power_down,
            delay,
        }
    }
}

/// Camera initialization failure.
#[derive(Debug)]
pub enum Gc0308InitError<I2cError, ResetError, PowerError> {
    /// The configured SCCB address is not seven-bit.
    InvalidAddress(u8),
    /// The reset output failed.
    Reset(ResetError),
    /// The power-down output failed.
    Power(PowerError),
    /// The CoreS3's PMIC did not switch the camera supply on.
    Supply(Axp2101Error<I2cError>),
    /// The CoreS3's expander did not release the camera reset.
    Expander(Aw9523Error<I2cError>),
    /// The GC0308 did not answer, answered as another part, or rejected
    /// its configuration, and no alternate sensor answered.
    Sensor(SensorError<I2cError>),
    /// The alternate OV3660 answered but failed its configuration.
    Alternate(I2cError),
}

/// Camera capture failure.
#[derive(Debug)]
pub enum Gc0308Error<CaptureError> {
    /// Platform DVP capture failed.
    Capture(CaptureError),
    /// The destination holds less than one frame.
    BufferTooSmall {
        /// Bytes one frame needs.
        needed: usize,
    },
    /// The sensor sent fewer bytes than one RGB565 frame.
    IncompleteFrame {
        /// Bytes received.
        received: usize,
    },
    /// The received JPEG lacked its start or end marker.
    IncompleteJpeg,
}

/// The sensor initialization found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sensor {
    Gc0308,
    Ov3660,
}

/// Initialized camera.
pub struct Gc0308Camera<CAPTURE> {
    capture: CAPTURE,
    sensor: Sensor,
}

/// The binding types a factory names without holding them.
type BindingTypes<I2C, CAPTURE, RESET, PWDN, DELAY> = fn() -> (I2C, CAPTURE, RESET, PWDN, DELAY);

/// Static factory used by generated Board composition.
pub struct Gc0308CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>(
    PhantomData<BindingTypes<I2C, CAPTURE, RESET, PWDN, DELAY>>,
);

impl<I2C, CAPTURE, RESET, PWDN, DELAY> PeripheralImplementation
    for Gc0308CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>
where
    I2C: I2c + 'static,
    CAPTURE: FrameReceiver + 'static,
    RESET: OutputPin + 'static,
    PWDN: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Gc0308Bindings<I2C, CAPTURE, RESET, PWDN, DELAY>;
    type Config = Gc0308Config;
    type Peripheral = Gc0308Camera<CAPTURE>;
    type Error = Gc0308InitError<I2C::Error, RESET::Error, PWDN::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let Gc0308Bindings {
            control: mut i2c,
            capture,
            reset,
            power_down,
            mut delay,
        } = bindings;
        let sensor =
            Gc0308::new(config.address).ok_or(Gc0308InitError::InvalidAddress(config.address))?;
        if let Some(mut power_down) = power_down {
            power_down.set_low().map_err(Gc0308InitError::Power)?;
            delay.delay_ms(POWER_SETTLE_MS);
        }
        if config.layout == PowerLayout::CoreS3 {
            cores3_power_up(&mut i2c, &mut delay)?;
        }
        if let Some(mut reset) = reset {
            reset.set_low().map_err(Gc0308InitError::Reset)?;
            delay.delay_ms(RESET_LOW_MS);
            reset.set_high().map_err(Gc0308InitError::Reset)?;
            delay.delay_ms(RESET_SETTLE_MS);
        }
        let found = match sensor.probe(&mut i2c) {
            Ok(()) => Sensor::Gc0308,
            Err(error) => match config.alternate {
                AlternateSensor::Ov3660 if ov3660_answers(&mut i2c) => Sensor::Ov3660,
                _ => return Err(Gc0308InitError::Sensor(error)),
            },
        };
        match found {
            Sensor::Gc0308 => sensor
                .initialize(
                    &mut i2c,
                    &mut delay,
                    SensorPixelFormat::Rgb565,
                    WIDTH,
                    HEIGHT,
                )
                .map_err(Gc0308InitError::Sensor)?,
            Sensor::Ov3660 => Ov3660::new(OV3660_ADDRESS)
                .ok_or(Gc0308InitError::InvalidAddress(OV3660_ADDRESS))?
                .initialize_qvga_jpeg(&mut i2c, &mut delay)
                .map_err(Gc0308InitError::Alternate)?,
        }
        Ok(Gc0308Camera {
            capture,
            sensor: found,
        })
    }
}

/// Whether an OV3660 answers at its address with its product ID.
fn ov3660_answers<I2C: I2c>(i2c: &mut I2C) -> bool {
    Ov3660::new(OV3660_ADDRESS)
        .and_then(|sensor| sensor.product_id(i2c).ok())
        .is_some_and(|product| product == OV3660_PRODUCT_ID)
}

/// Switches the CoreS3 camera supply on and pulses the sensor's reset.
fn cores3_power_up<I2C, DELAY, ResetError, PowerError>(
    i2c: &mut I2C,
    delay: &mut DELAY,
) -> Result<(), Gc0308InitError<I2C::Error, ResetError, PowerError>>
where
    I2C: I2c,
    DELAY: DelayNs,
{
    let mut pmic = Axp2101::new(i2c, PMIC_ADDRESS).map_err(Gc0308InitError::Supply)?;
    pmic.set_millivolts(Ldo::Aldo3, CAMERA_SUPPLY_MILLIVOLTS)
        .map_err(Gc0308InitError::Supply)?;
    // Espressif's m5stack_cores3 power manager, camera branch: REG90H bits
    // 2, 4, and 5 (ALDO3, BLDO1, BLDO2); M5Unified switches them on too.
    for ldo in [Ldo::Aldo3, Ldo::Bldo1, Ldo::Bldo2] {
        pmic.set_enabled(ldo, true)
            .map_err(Gc0308InitError::Supply)?;
    }
    delay.delay_ms(POWER_SETTLE_MS);
    Aw9523::new(i2c, EXPANDER_ADDRESS)
        .and_then(|mut expander| {
            expander.pulse_reset_low(
                CORES3_CAMERA_RESET_PIN,
                RESET_LOW_MS,
                RESET_SETTLE_MS,
                delay,
            )
        })
        .map_err(Gc0308InitError::Expander)
}

impl<CAPTURE: FrameReceiver> Camera for Gc0308Camera<CAPTURE> {
    type Error = Gc0308Error<CAPTURE::Error>;

    fn descriptor(&self) -> CameraDescriptor {
        match self.sensor {
            Sensor::Gc0308 => GC0308_DESCRIPTOR,
            Sensor::Ov3660 => OV3660_DESCRIPTOR,
        }
    }

    async fn capture(&mut self, buffer: &mut [u8]) -> Result<CapturedFrame, Self::Error> {
        match self.sensor {
            Sensor::Gc0308 => {
                let frame =
                    buffer
                        .get_mut(..RGB565_FRAME_BYTES)
                        .ok_or(Gc0308Error::BufferTooSmall {
                            needed: RGB565_FRAME_BYTES,
                        })?;
                let received = self
                    .capture
                    .receive(frame)
                    .await
                    .map_err(Gc0308Error::Capture)?;
                if received != RGB565_FRAME_BYTES {
                    return Err(Gc0308Error::IncompleteFrame { received });
                }
                Ok(CapturedFrame::new(received, GC0308_DESCRIPTOR))
            }
            Sensor::Ov3660 => {
                let received = self
                    .capture
                    .receive(buffer)
                    .await
                    .map_err(Gc0308Error::Capture)?;
                let length = jpeg_length(buffer.get(..received).unwrap_or_default())
                    .ok_or(Gc0308Error::IncompleteJpeg)?;
                Ok(CapturedFrame::new(length, OV3660_DESCRIPTOR))
            }
        }
    }
}

/// The length of the JPEG at the start of `frame` through its last end
/// marker; the sensor pads the frame after the image.
fn jpeg_length(frame: &[u8]) -> Option<usize> {
    if !frame.starts_with(&JPEG_START) {
        return None;
    }
    frame
        .windows(JPEG_END.len())
        .rposition(|window| window == JPEG_END)
        .and_then(|position| position.checked_add(JPEG_END.len()))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests;
