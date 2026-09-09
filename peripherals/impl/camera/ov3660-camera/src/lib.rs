//! OV3660 QVGA JPEG camera peripheral implementation.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_ov3660::Ov3660;
use barracuda_peripheral::{
    PeripheralImplementation,
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame, FrameReceiver},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

const DESCRIPTOR: CameraDescriptor = CameraDescriptor::new(320, 240, CameraPixelFormat::Jpeg);

/// Fixed QVGA JPEG configuration for the OV3660.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ov3660Config {
    address: u8,
}

impl Ov3660Config {
    /// Creates the supported 320x240 JPEG profile.
    #[must_use]
    pub const fn qvga_jpeg(address: u8) -> Self {
        Self { address }
    }
}

/// Move-only resources consumed by the OV3660 implementation.
pub struct Ov3660Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
    control: I2C,
    capture: CAPTURE,
    reset: Option<RESET>,
    power_down: Option<PWDN>,
    delay: DELAY,
}

impl<I2C, CAPTURE, RESET, PWDN, DELAY> Ov3660Bindings<I2C, CAPTURE, RESET, PWDN, DELAY> {
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

/// OV3660 initialization failure.
#[derive(Debug)]
pub enum Ov3660InitError<I2cError, ResetError, PowerError> {
    /// The configured SCCB address is not seven-bit.
    InvalidAddress,
    /// SCCB/I2C access failed.
    Control(I2cError),
    /// Reset output failed.
    Reset(ResetError),
    /// Power-down output failed.
    Power(PowerError),
    /// Product-ID registers did not contain `0x3660`.
    UnexpectedIdentity(u16),
}

/// OV3660 capture failure.
#[derive(Debug)]
pub enum Ov3660Error<CaptureError> {
    /// Platform DVP capture failed.
    Capture(CaptureError),
    /// The received frame did not contain complete JPEG boundaries.
    IncompleteJpeg,
}

/// Initialized OV3660 camera.
pub struct Ov3660Camera<CAPTURE> {
    capture: CAPTURE,
}

/// Static factory used by generated Board composition.
pub struct Ov3660CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>(
    PhantomData<I2C>,
    PhantomData<CAPTURE>,
    PhantomData<RESET>,
    PhantomData<PWDN>,
    PhantomData<DELAY>,
);

impl<I2C, CAPTURE, RESET, PWDN, DELAY> PeripheralImplementation
    for Ov3660CameraImplementation<I2C, CAPTURE, RESET, PWDN, DELAY>
where
    I2C: I2c + 'static,
    CAPTURE: FrameReceiver + 'static,
    RESET: OutputPin + 'static,
    PWDN: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ov3660Bindings<I2C, CAPTURE, RESET, PWDN, DELAY>;
    type Config = Ov3660Config;
    type Peripheral = Ov3660Camera<CAPTURE>;
    type Error = Ov3660InitError<I2C::Error, RESET::Error, PWDN::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let sensor = Ov3660::new(config.address).ok_or(Ov3660InitError::InvalidAddress)?;
        if let Some(power_down) = bindings.power_down.as_mut() {
            power_down.set_low().map_err(Ov3660InitError::Power)?;
            bindings.delay.delay_ms(2);
        }
        if let Some(reset) = bindings.reset.as_mut() {
            reset.set_low().map_err(Ov3660InitError::Reset)?;
            bindings.delay.delay_ms(2);
            reset.set_high().map_err(Ov3660InitError::Reset)?;
            bindings.delay.delay_ms(20);
        }
        let product = sensor
            .product_id(&mut bindings.control)
            .map_err(Ov3660InitError::Control)?;
        if product != 0x3660 {
            return Err(Ov3660InitError::UnexpectedIdentity(product));
        }
        sensor
            .initialize_qvga_jpeg(&mut bindings.control, &mut bindings.delay)
            .map_err(Ov3660InitError::Control)?;
        Ok(Ov3660Camera {
            capture: bindings.capture,
        })
    }
}

impl<CAPTURE: FrameReceiver> Camera for Ov3660Camera<CAPTURE> {
    type Error = Ov3660Error<CAPTURE::Error>;

    fn descriptor(&self) -> CameraDescriptor {
        DESCRIPTOR
    }

    async fn capture(&mut self, buffer: &mut [u8]) -> Result<CapturedFrame, Self::Error> {
        let bytes_used = self
            .capture
            .receive(buffer)
            .await
            .map_err(Ov3660Error::Capture)?;
        let frame = buffer
            .get(..bytes_used)
            .ok_or(Ov3660Error::IncompleteJpeg)?;
        if !frame.starts_with(&[0xff, 0xd8]) || !frame.ends_with(&[0xff, 0xd9]) {
            return Err(Ov3660Error::IncompleteJpeg);
        }
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

    use super::{Ov3660Bindings, Ov3660CameraImplementation, Ov3660Config};

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
            value[0] = if register == [0x30, 0x0a] { 0x36 } else { 0x60 };
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
    fn initializes_sensor_driver_and_captures_jpeg() {
        let bindings = Ov3660Bindings::<Control, Capture, Pin, Pin, Delay>::new(
            Control, Capture, None, None, Delay,
        );
        let mut camera = block_on(Ov3660CameraImplementation::initialize(
            bindings,
            Ov3660Config::qvga_jpeg(0x3c),
        ))
        .expect("initialize camera");
        let mut buffer = [0_u8; 16];
        let frame = block_on(camera.capture(&mut buffer)).expect("capture JPEG");
        assert_eq!(frame.bytes_used(), 4);
        assert_eq!(camera.descriptor().width(), 320);
    }
}
