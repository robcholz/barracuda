extern crate std;

use core::future::ready;
use std::{cell::Cell, rc::Rc, vec::Vec};

use barracuda_peripheral::{
    PeripheralImplementation,
    camera::{Camera, CameraPixelFormat, FrameReceiver, FrameReceiverErrorType},
};
use embassy_futures::block_on;
use embedded_hal::{
    delay::DelayNs,
    digital::{ErrorType as DigitalErrorType, OutputPin},
    i2c::{Error as I2cError, ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation},
};

use super::{
    AlternateSensor, Gc0308Bindings, Gc0308CameraImplementation, Gc0308Config, Gc0308Error,
    Gc0308InitError, PowerLayout, RGB565_FRAME_BYTES,
};

/// No device acknowledged the address.
#[derive(Debug, PartialEq, Eq)]
struct Nack;

impl I2cError for Nack {
    fn kind(&self) -> ErrorKind {
        ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
    }
}

/// The CoreS3 internal bus: the sensors that are fitted, the AXP2101, and
/// the AW9523B, each a flat register file.
#[derive(Default)]
struct Bus {
    gc0308: bool,
    ov3660: bool,
    pmic: Vec<u8>,
    expander: Vec<u8>,
    gc0308_registers: Vec<u8>,
}

impl Bus {
    fn new(gc0308: bool, ov3660: bool) -> Self {
        let mut gc0308_registers = std::vec![0; 256];
        gc0308_registers[0x00] = 0x9b;
        Self {
            gc0308,
            ov3660,
            pmic: std::vec![0; 256],
            expander: std::vec![0; 256],
            gc0308_registers,
        }
    }
}

impl ErrorType for Bus {
    type Error = Nack;
}

impl I2c for Bus {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        let registers = match address {
            0x21 if self.gc0308 => &mut self.gc0308_registers,
            0x34 => &mut self.pmic,
            0x58 => &mut self.expander,
            0x3c if self.ov3660 => {
                // OV3660 product ID 3660h at registers 300Ah and 300Bh.
                let mut register = [0; 2];
                for operation in operations {
                    match operation {
                        Operation::Write(bytes) if bytes.len() == 2 => {
                            register.copy_from_slice(bytes);
                        }
                        Operation::Read(value) => {
                            value[0] = match register {
                                [0x30, 0x0a] => 0x36,
                                [0x30, 0x0b] => 0x60,
                                _ => 0,
                            };
                        }
                        _ => {}
                    }
                }
                return Ok(());
            }
            _ => return Err(Nack),
        };
        let mut register = 0;
        for operation in operations {
            match operation {
                Operation::Write([reg]) => register = usize::from(*reg),
                Operation::Write([reg, value]) => registers[usize::from(*reg)] = *value,
                Operation::Read([value]) => *value = registers[register],
                _ => {}
            }
        }
        Ok(())
    }
}

/// An output that remembers its last level.
#[derive(Clone, Default)]
struct Pin(Rc<Cell<Option<bool>>>);

impl DigitalErrorType for Pin {
    type Error = core::convert::Infallible;
}

impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0.set(Some(false));
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0.set(Some(true));
        Ok(())
    }
}

struct Delay;

impl DelayNs for Delay {
    fn delay_ns(&mut self, _: u32) {}
}

/// A receiver that delivers one fixed frame image, cut to the destination.
struct Capture {
    frame: Vec<u8>,
}

impl FrameReceiverErrorType for Capture {
    type Error = core::convert::Infallible;
}

impl FrameReceiver for Capture {
    fn receive<'a>(
        &'a mut self,
        frame: &'a mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> + 'a {
        let length = self.frame.len().min(frame.len());
        frame[..length].copy_from_slice(&self.frame[..length]);
        ready(Ok(length))
    }
}

type Implementation = Gc0308CameraImplementation<Bus, Capture, Pin, Pin, Delay>;

fn bindings(
    bus: Bus,
    frame: Vec<u8>,
    power_down: Option<Pin>,
) -> Gc0308Bindings<Bus, Capture, Pin, Pin, Delay> {
    Gc0308Bindings::new(bus, Capture { frame }, None, power_down, Delay)
}

#[test]
fn powers_the_cores3_camera_and_captures_a_whole_rgb565_frame() {
    let mut camera = block_on(Implementation::initialize(
        bindings(
            Bus::new(true, false),
            std::vec![0x5a; RGB565_FRAME_BYTES],
            None,
        ),
        Gc0308Config::new(0x21, PowerLayout::CoreS3, AlternateSensor::None),
    ))
    .expect("initialize camera");
    assert_eq!(
        camera.descriptor().pixel_format(),
        CameraPixelFormat::Rgb565
    );
    assert_eq!(
        (camera.descriptor().width(), camera.descriptor().height()),
        (320, 240)
    );

    let mut buffer = std::vec![0; RGB565_FRAME_BYTES + 100];
    let frame = block_on(camera.capture(&mut buffer)).expect("capture");
    assert_eq!(frame.bytes_used(), RGB565_FRAME_BYTES);

    let mut small = std::vec![0; 1024];
    assert!(matches!(
        block_on(camera.capture(&mut small)),
        Err(Gc0308Error::BufferTooSmall { needed }) if needed == RGB565_FRAME_BYTES
    ));
}

#[test]
fn programs_only_the_cores3_camera_rails_and_reset() {
    let mut bus = Bus::new(true, false);
    bus.pmic[0x90] = 0x80;
    bus.expander[0x03] = 0x02;
    super::cores3_power_up::<_, _, (), ()>(&mut bus, &mut Delay).expect("power up");
    // ALDO3 at 3.3 V (code 28), then ALDO3, BLDO1, and BLDO2 on beside the
    // backlight's DLDO1.
    assert_eq!(bus.pmic[0x94] & 0x1f, 28);
    assert_eq!(bus.pmic[0x90], 0x80 | 0x34);
    // P1_0 is a released output; P1_1 (the LCD reset) keeps its level.
    assert_eq!(bus.expander[0x03], 0x03);
    assert_eq!(bus.expander[0x05] & 0x01, 0x00);
    assert_eq!(bus.expander[0x13] & 0x01, 0x01);
}

#[test]
fn reports_a_short_frame() {
    let mut camera = block_on(Implementation::initialize(
        bindings(Bus::new(true, false), std::vec![0; 4096], None),
        Gc0308Config::new(0x21, PowerLayout::None, AlternateSensor::None),
    ))
    .expect("initialize camera");
    let mut buffer = std::vec![0; RGB565_FRAME_BYTES];
    assert!(matches!(
        block_on(camera.capture(&mut buffer)),
        Err(Gc0308Error::IncompleteFrame { received: 4096 })
    ));
}

#[test]
fn powers_up_through_the_power_down_line() {
    let power = Pin::default();
    block_on(Implementation::initialize(
        bindings(Bus::new(true, false), Vec::new(), Some(power.clone())),
        Gc0308Config::new(0x21, PowerLayout::None, AlternateSensor::None),
    ))
    .expect("initialize camera");
    assert_eq!(power.0.get(), Some(false));
}

#[test]
fn falls_back_to_an_ov3660_and_trims_its_jpeg() {
    let mut jpeg = std::vec![0xff, 0xd8, 0x01, 0x02, 0xff, 0xd9];
    jpeg.extend_from_slice(&[0; 58]);
    let mut camera = block_on(Implementation::initialize(
        bindings(Bus::new(false, true), jpeg, None),
        Gc0308Config::new(0x21, PowerLayout::None, AlternateSensor::Ov3660),
    ))
    .expect("initialize the M12 camera");
    assert_eq!(camera.descriptor().pixel_format(), CameraPixelFormat::Jpeg);
    let mut buffer = std::vec![0; 256];
    let frame = block_on(camera.capture(&mut buffer)).expect("capture JPEG");
    assert_eq!(frame.bytes_used(), 6);
}

#[test]
fn fails_without_a_sensor() {
    let result = block_on(Implementation::initialize(
        bindings(Bus::new(false, true), Vec::new(), None),
        Gc0308Config::new(0x21, PowerLayout::None, AlternateSensor::None),
    ));
    assert!(matches!(result, Err(Gc0308InitError::Sensor(_))));

    let result = block_on(Implementation::initialize(
        bindings(Bus::new(false, false), Vec::new(), None),
        Gc0308Config::new(0x21, PowerLayout::None, AlternateSensor::Ov3660),
    ));
    assert!(matches!(result, Err(Gc0308InitError::Sensor(_))));
}

#[test]
fn rejects_an_eight_bit_address() {
    let result = block_on(Implementation::initialize(
        bindings(Bus::new(true, false), Vec::new(), None),
        Gc0308Config::new(0xa1, PowerLayout::None, AlternateSensor::None),
    ));
    assert!(matches!(result, Err(Gc0308InitError::InvalidAddress(0xa1))));
}
