//! OV2640 QVGA JPEG Camera Driver using embedded-hal control bindings.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver::{
    PeripheralDriver,
    camera::{Camera, CameraDescriptor, CameraPixelFormat, CapturedFrame, FrameReceiver},
};
use embedded_hal::{delay::DelayNs, digital::OutputPin, i2c::I2c};

const DESCRIPTOR: CameraDescriptor = CameraDescriptor::new(320, 240, CameraPixelFormat::Jpeg);

// OV2640 CIF sensor and DSP baseline followed by QVGA JPEG output setup.
const QVGA_JPEG_REGISTERS: &[(u8, u8)] = &[
    (0xff, 0x00),
    (0x2c, 0xff),
    (0x2e, 0xdf),
    (0xff, 0x01),
    (0x3c, 0x32),
    (0x11, 0x01),
    (0x09, 0x02),
    (0x04, 0x28),
    (0x13, 0xe5),
    (0x14, 0x48),
    (0x2c, 0x0c),
    (0x33, 0x78),
    (0x3a, 0x33),
    (0x3b, 0xfb),
    (0x3e, 0x00),
    (0x43, 0x11),
    (0x16, 0x10),
    (0x39, 0x92),
    (0x35, 0xda),
    (0x22, 0x1a),
    (0x37, 0xc3),
    (0x23, 0x00),
    (0x34, 0xc0),
    (0x06, 0x88),
    (0x07, 0xc0),
    (0x0d, 0x87),
    (0x0e, 0x41),
    (0x4c, 0x00),
    (0x4a, 0x81),
    (0x21, 0x99),
    (0x24, 0x40),
    (0x25, 0x38),
    (0x26, 0x82),
    (0x5c, 0x00),
    (0x63, 0x00),
    (0x61, 0x70),
    (0x62, 0x80),
    (0x7c, 0x05),
    (0x20, 0x80),
    (0x28, 0x30),
    (0x6c, 0x00),
    (0x6d, 0x80),
    (0x6e, 0x00),
    (0x70, 0x02),
    (0x71, 0x94),
    (0x73, 0xc1),
    (0x3d, 0x34),
    (0x5a, 0x57),
    (0x4f, 0xbb),
    (0x50, 0x9c),
    (0x12, 0x20),
    (0x17, 0x11),
    (0x18, 0x43),
    (0x19, 0x00),
    (0x1a, 0x25),
    (0x32, 0x89),
    (0x37, 0xc0),
    (0x4f, 0xca),
    (0x50, 0xa8),
    (0x6d, 0x00),
    (0x3d, 0x38),
    (0xff, 0x00),
    (0xe5, 0x7f),
    (0xf9, 0xc0),
    (0x41, 0x24),
    (0xe0, 0x14),
    (0x76, 0xff),
    (0x33, 0xa0),
    (0x42, 0x20),
    (0x43, 0x18),
    (0x4c, 0x00),
    (0x97, 0x02),
    (0x97, 0x0c),
    (0x97, 0x24),
    (0x97, 0x30),
    (0x97, 0x28),
    (0x97, 0x26),
    (0x97, 0x02),
    (0x97, 0x98),
    (0x97, 0x80),
    (0x97, 0x00),
    (0x97, 0x00),
    (0xa4, 0x00),
    (0xa8, 0x00),
    (0xc5, 0x11),
    (0xc6, 0x51),
    (0xbf, 0x80),
    (0xc7, 0x10),
    (0xb6, 0x66),
    (0xb8, 0xa5),
    (0xb7, 0x64),
    (0xb9, 0x7c),
    (0xb3, 0xaf),
    (0xb4, 0x97),
    (0xb5, 0xff),
    (0xb0, 0xc5),
    (0xb1, 0x94),
    (0xb2, 0x0f),
    (0xc4, 0x5c),
    (0xc3, 0xfd),
    (0x7f, 0x00),
    (0xe5, 0x1f),
    (0xe1, 0x67),
    (0xdd, 0x7f),
    (0xda, 0x00),
    (0xe0, 0x00),
    (0x05, 0x00),
    // QVGA output window inside the CIF source frame.
    (0x51, 0x64),
    (0x52, 0x4a),
    (0x53, 0x00),
    (0x54, 0x00),
    (0x55, 0x00),
    (0x57, 0x00),
    (0x5a, 0x50),
    (0x5b, 0x3c),
    (0x5c, 0x00),
    (0xd3, 0x08),
    // JPEG encoder output with HREF/VSYNC framing.
    (0xe0, 0x14),
    (0xda, 0x12),
    (0xd7, 0x03),
    (0xe1, 0x77),
    (0xe5, 0x1f),
    (0xd9, 0x10),
    (0xdf, 0x80),
    (0x33, 0x80),
    (0x3c, 0x10),
    (0xeb, 0x30),
    (0xdd, 0x7f),
    (0xe0, 0x00),
];

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

/// Move-only resources consumed by the OV2640 Driver.
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
    /// SCCB/I2C access failed.
    Control(I2cError),
    /// Reset output failed.
    Reset(ResetError),
    /// Power-down output failed.
    Power(PowerError),
    /// The SCCB identity was not an OV2640 (`0x26`, `0x42`).
    UnexpectedIdentity {
        /// Product identifier read from register `0x0a`.
        product: u8,
        /// Product version read from register `0x0b`.
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
pub struct Ov2640CameraDriver<I2C, CAPTURE, RESET, PWDN, DELAY>(
    PhantomData<I2C>,
    PhantomData<CAPTURE>,
    PhantomData<RESET>,
    PhantomData<PWDN>,
    PhantomData<DELAY>,
);

impl<I2C, CAPTURE, RESET, PWDN, DELAY> PeripheralDriver
    for Ov2640CameraDriver<I2C, CAPTURE, RESET, PWDN, DELAY>
where
    I2C: I2c + 'static,
    CAPTURE: FrameReceiver + 'static,
    RESET: OutputPin + 'static,
    PWDN: OutputPin + 'static,
    DELAY: DelayNs + 'static,
{
    type Bindings = Ov2640Bindings<I2C, CAPTURE, RESET, PWDN, DELAY>;
    type Config = Ov2640Config;
    type Capability = Ov2640Camera<CAPTURE>;
    type Error = Ov2640InitError<I2C::Error, RESET::Error, PWDN::Error>;

    async fn initialize(
        mut bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Capability, Self::Error> {
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
        write_register(&mut bindings.control, config.address, 0xff, 0x01)
            .map_err(Ov2640InitError::Control)?;
        let product = read_register(&mut bindings.control, config.address, 0x0a)
            .map_err(Ov2640InitError::Control)?;
        let version = read_register(&mut bindings.control, config.address, 0x0b)
            .map_err(Ov2640InitError::Control)?;
        if product != 0x26 || version != 0x42 {
            return Err(Ov2640InitError::UnexpectedIdentity { product, version });
        }
        write_register(&mut bindings.control, config.address, 0x12, 0x80)
            .map_err(Ov2640InitError::Control)?;
        bindings.delay.delay_ms(10);
        for &(register, value) in QVGA_JPEG_REGISTERS {
            write_register(&mut bindings.control, config.address, register, value)
                .map_err(Ov2640InitError::Control)?;
        }
        bindings.delay.delay_ms(10);
        Ok(Ov2640Camera {
            capture: bindings.capture,
        })
    }
}

impl<CAPTURE> Camera for Ov2640Camera<CAPTURE>
where
    CAPTURE: FrameReceiver,
{
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

fn write_register<I2C: I2c>(
    i2c: &mut I2C,
    address: u8,
    register: u8,
    value: u8,
) -> Result<(), I2C::Error> {
    i2c.write(address, &[register, value])
}

fn read_register<I2C: I2c>(i2c: &mut I2C, address: u8, register: u8) -> Result<u8, I2C::Error> {
    let mut value = [0];
    i2c.write_read(address, &[register], &mut value)?;
    Ok(value[0])
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{Ov2640Bindings, Ov2640CameraDriver, Ov2640Config};
    use barracuda_driver::{
        PeripheralDriver,
        camera::{Camera, FrameReceiver, FrameReceiverErrorType},
    };
    use core::{convert::Infallible, future::ready};
    use embassy_futures::block_on;
    use embedded_hal::{
        delay::DelayNs,
        digital::{ErrorType as DigitalErrorType, OutputPin},
        i2c::{ErrorType, I2c, Operation},
    };

    #[derive(Default)]
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
    #[derive(Default)]
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
    #[derive(Default)]
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
    fn initializes_sensor_and_captures_into_caller_memory() {
        let bindings = Ov2640Bindings::<Control, Capture, Pin, Pin, Delay>::new(
            Control, Capture, None, None, Delay,
        );
        let mut camera = block_on(Ov2640CameraDriver::initialize(
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
