//! SC202CS camera implementation for the M5Stack Tab5 MIPI-CSI/ISP pipeline.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_pi4ioe5v6408::{Error as ExpanderError, Pi4ioe5v6408};
use barracuda_peripheral::{
    PeripheralImplementation,
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame, MipiCsiHost},
};
use embedded_hal::{delay::DelayNs, i2c::I2c};

const CONTROL_EXPANDER_ADDRESS: u8 = 0x43;
const CAMERA_RESET_PIN: u8 = 6;

/// Move-only MIPI-CSI host consumed by the sensor implementation.
pub struct Tab5Sc202csCameraBindings<HOST, I2C, DELAY> {
    host: HOST,
    control: I2C,
    delay: DELAY,
}

impl<HOST, I2C, DELAY> Tab5Sc202csCameraBindings<HOST, I2C, DELAY> {
    /// Wraps the Platform-owned CSI/ISP data plane.
    #[must_use]
    pub const fn new(host: HOST, control: I2C, delay: DELAY) -> Self {
        Self {
            host,
            control,
            delay,
        }
    }
}

/// Statically selected camera output dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tab5Sc202csCameraConfig {
    width: u16,
    height: u16,
}

impl Tab5Sc202csCameraConfig {
    /// Creates one RGB565 stream configuration.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// Initialized Tab5 camera peripheral.
pub struct Tab5Sc202csCamera<HOST> {
    host: HOST,
    descriptor: CameraDescriptor,
}

/// Camera initialization failure.
#[derive(Debug)]
pub enum Tab5Sc202csCameraInitError<BusError, HostError> {
    /// The private Tab5 camera-reset expander could not be configured.
    Expander(ExpanderError<BusError>),
    /// The Platform CSI host rejected stream initialization.
    Host(HostError),
}

impl<HOST> Camera for Tab5Sc202csCamera<HOST>
where
    HOST: MipiCsiHost,
{
    type Error = HOST::Error;

    fn descriptor(&self) -> CameraDescriptor {
        self.descriptor
    }

    async fn capture(&mut self, buffer: &mut [u8]) -> Result<CapturedFrame, Self::Error> {
        let bytes_used = self.host.capture(buffer).await?;
        Ok(CapturedFrame::new(bytes_used, self.descriptor))
    }
}

/// Static factory used by generated Board composition.
pub struct Tab5Sc202csCameraImplementation<HOST, I2C, DELAY>(
    PhantomData<HOST>,
    PhantomData<I2C>,
    PhantomData<DELAY>,
);

impl<HOST, I2C, DELAY> PeripheralImplementation
    for Tab5Sc202csCameraImplementation<HOST, I2C, DELAY>
where
    HOST: MipiCsiHost + 'static,
    I2C: I2c + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Tab5Sc202csCameraBindings<HOST, I2C, DELAY>;
    type Config = Tab5Sc202csCameraConfig;
    type Peripheral = Tab5Sc202csCamera<HOST>;
    type Error = Tab5Sc202csCameraInitError<I2C::Error, HOST::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        Pi4ioe5v6408::new(&mut bindings.control, CONTROL_EXPANDER_ADDRESS)
            .map_err(Tab5Sc202csCameraInitError::Expander)?
            .pulse_reset_low(CAMERA_RESET_PIN, 10, 10, &mut bindings.delay)
            .map_err(Tab5Sc202csCameraInitError::Expander)?;
        let descriptor =
            CameraDescriptor::new(config.width, config.height, CameraPixelFormat::Rgb565);
        bindings
            .host
            .initialize(descriptor)
            .await
            .map_err(Tab5Sc202csCameraInitError::Host)?;
        Ok(Tab5Sc202csCamera {
            host: bindings.host,
            descriptor,
        })
    }
}
