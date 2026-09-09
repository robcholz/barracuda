//! OV2640 QVGA JPEG camera peripheral implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ov2640::Ov2640;
use barracuda_peripheral::{
    PeripheralImplementation,
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame, FrameReceiver},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

const DESCRIPTOR: CameraDescriptor = CameraDescriptor::new(320, 240, CameraPixelFormat::Jpeg);

/// Fixed QVGA JPEG configuration for the OV2640.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ov2640Config {
    address: u8,
}

impl Ov2640Config {
    /// Creates the supported 320x240 JPEG profile.
    #[must_use]
    pub const fn qvga_jpeg(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the OV2640 implementation.
pub struct Ov2640Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
    control: I2C,
    capture: CAPTURE,
    reset: Option<RESET>,
    power_down: Option<PWDN>,
    delay: DELAY,
}

impl<I2C, CAPTURE, RESET, PWDN, DELAY> Ov2640Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
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

/// OV2640 initialization failure.
#[derive(Debug)]
pub enum Ov2640InitError<I2cError, ResetError, PowerError> {
    /// The configured SCCB address is not seven-bit.
    InvalidAddress,
    /// SCCB/I2C access failed.
    Control(I2cError),
    /// Reset output failed.
    Reset(ResetError),
    /// Power-down output failed.
    Power(PowerError),
    /// The SCCB identity was not an OV2640 (`0x26`, `0x42`).
    UnexpectedIdentity {
        /// Product identifier read from the sensor.
        product: u8,
        /// Product version read from the sensor.
        version: u8,
    },
}

/// OV2640 capture failure.
#[derive(Debug)]
pub enum Ov2640Error<CaptureError> {
    /// Platform DVP capture failed.
    Capture(CaptureError),
}

/// Initialized OV2640 camera.
pub struct Ov2640Camera<CAPTURE> {
    capture: CAPTURE,
}

/// Static factory used by generated Board composition.
pub struct Ov2640CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>(
    PhantomData<I2C>,
    PhantomData<CAPTURE>,
    PhantomData<RESET>,
    PhantomData<PWDN>,
    PhantomData<DELAY>,
);

impl<I2C, CAPTURE, RESET, PWDN, DELAY> PeripheralImplementation
    for Ov2640CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>
where
    I2C: I2c + 'static,
    CAPTURE: FrameReceiver + 'static,
    RESET: OutputPin + 'static,
    PWDN: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ov2640Bindings<I2C, CAPTURE, RESET, PWDN, DELAY>;
    type Config = Ov2640Config;
    type Peripheral = Ov2640Camera<CAPTURE>;
    type Error = Ov2640InitError<I2C::Error, RESET::Error, PWDN::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let sensor = Ov2640::new(config.address).ok_or(Ov2640InitError::InvalidAddress)?;
        if let Some(power_down) = bindings.power_down.as_mut() {
            power_down.set_low().map_err(Ov2640InitError::Power)?;
            bindings.delay.delay_ms(2);
        }
        if let Some(reset) = bindings.reset.as_mut() {
            reset.set_low().map_err(Ov2640InitError::Reset)?;
            bindings.delay.delay_ms(2);
            reset.set_high().map_err(Ov2640InitError::Reset)?;
            bindings.delay.delay_ms(10);
        }
        let identity = sensor
            .identity(&mut bindings.control)
            .map_err(Ov2640InitError::Control)?;
        if !identity.is_ov2640() {
            return Err(Ov2640InitError::UnexpectedIdentity {
                product: identity.product,
                version: identity.version,
            });
        }
        sensor
            .initialize_qvga_jpeg(&mut bindings.control, &mut bindings.delay)
            .map_err(Ov2640InitError::Control)?;
        Ok(Ov2640Camera {
            capture: bindings.capture,
        })
    }
}

impl<CAPTURE: FrameReceiver> Camera for Ov2640Camera<CAPTURE> {
    type Error = Ov2640Error<CAPTURE::Error>;

    fn descriptor(&self) -> CameraDescriptor {
        DESCRIPTOR
    }

    async fn capture(&mut self, buffer: &mut [u8]) -> Result<CapturedFrame, Self::Error> {
        let bytes_used = self
            .capture
            .receive(buffer)
            .await
            .map_err(Ov2640Error::Capture)?;
        Ok(CapturedFrame::new(bytes_used, DESCRIPTOR))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    extern crate std;

    use core::{convert::Infallible, future::ready};

    use barracuda_peripheral::{
        PeripheralImplementation,
        camera::{Camera, FrameReceiver, FrameReceiverErrorType},
    };
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as DigitalErrorType, OutputPin},
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{Ov2640Bindings, Ov2640CameraImplementation, Ov2640Config};

    struct Control;
    impl ErrorType for Control {
        type Error = Infallible;
    }
    impl I2c for Control {
        fn read(&mut self, _: u8, _: &mut [u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write(&mut self, _: u8, _: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write_read(
            &mut self,
            _: u8,
            register: &[u8],
            value: &mut [u8],
        ) -> Result<(), Self::Error> {
            value[0] = if register[0] == 0x0a { 0x26 } else { 0x42 };
            Ok(())
        }
        fn transaction(&mut self, _: u8, _: &mut [Operation<'_>]) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct Pin;
    impl DigitalErrorType for Pin {
        type Error = Infallible;
    }
    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct Delay;
    impl DelayNs for Delay {
        fn delay_ns(&mut self, _: u32) {}
    }

    struct Capture;
    impl FrameReceiverErrorType for Capture {
        type Error = Infallible;
    }
    impl FrameReceiver for Capture {
        fn receive<'a>(
            &'a mut self,
            frame: &'a mut [u8],
        ) -> impl core::future::Future<Output = Result<usize, Self::Error>> + 'a {
            frame[..4].copy_from_slice(&[0xff, 0xd8, 0xff, 0xd9]);
            ready(Ok(4))
        }
    }

    #[test]
    fn initializes_sensor_driver_and_captures_into_caller_memory() {
        let bindings = Ov2640Bindings::<Control, Capture, Pin, Pin, Delay>::new(
            Control, Capture, None, None, Delay,
        );
        let mut camera = block_on(Ov2640CameraImplementation::initialize(
            bindings,
            Ov2640Config::qvga_jpeg(0x30),
        ))
        .expect("initialize camera");
        let mut buffer = [0_u8; 16];
        let frame = block_on(camera.capture(&mut buffer)).expect("capture frame");
        assert_eq!(frame.bytes_used(), 4);
        assert_eq!(&buffer[..4], &[0xff, 0xd8, 0xff, 0xd9]);
    }
}
