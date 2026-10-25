extern crate std;

use core::convert::Infallible;
use std::{vec, vec::Vec};

use barracuda_peripheral::{
    PeripheralImplementation,
    display::{Display, DsiHost, DsiPanelConfig},
};
use embedded_graphics_core::{
    geometry::{Point, Size},
    pixelcolor::Rgb565,
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    i2c::{ErrorType, I2c, Operation},
};

use super::{
    CoreP4xDsiDisplayBindings, CoreP4xDsiDisplayConfig, CoreP4xDsiDisplayImplementation, HEIGHT,
    PANEL, WIDTH, backlight_duty,
};

#[derive(Debug, PartialEq, Eq)]
enum HostCall {
    Initialize(DsiPanelConfig),
    StartVideo,
    Command(u8),
    Present,
    Draw {
        x: u16,
        y: u16,
        width: u16,
        pixels: Vec<u16>,
    },
}

#[derive(Default)]
struct Host {
    calls: Vec<HostCall>,
}

impl DsiHost for Host {
    type Error = Infallible;

    fn initialize(&mut self, config: DsiPanelConfig) -> Result<(), Self::Error> {
        self.calls.push(HostCall::Initialize(config));
        Ok(())
    }

    fn start_video(&mut self) -> Result<(), Self::Error> {
        self.calls.push(HostCall::StartVideo);
        Ok(())
    }

    fn write_command(&mut self, command: u8, _data: &[u8]) -> Result<(), Self::Error> {
        self.calls.push(HostCall::Command(command));
        Ok(())
    }

    fn present(&mut self) -> Result<(), Self::Error> {
        self.calls.push(HostCall::Present);
        Ok(())
    }

    fn draw_rgb565(
        &mut self,
        x: u16,
        y: u16,
        width: u16,
        _height: u16,
        pixels: &[u16],
    ) -> Result<(), Self::Error> {
        self.calls.push(HostCall::Draw {
            x,
            y,
            width,
            pixels: pixels.to_vec(),
        });
        Ok(())
    }

    fn set_display_enabled(&mut self, _enabled: bool) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_sleep(&mut self, _sleep: bool) -> Result<(), Self::Error> {
        Ok(())
    }

    fn set_backlight(&mut self, _brightness: u8) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Register files of the M5PM1 (0x6E) and the M5IOE1 (0x4F).
struct Bus {
    power: [u8; 0x40],
    expander: [u8; 0x40],
}

impl Bus {
    fn new() -> Self {
        Self {
            power: [0; 0x40],
            expander: [0; 0x40],
        }
    }

    fn registers(&mut self, address: u8) -> &mut [u8; 0x40] {
        match address {
            0x6e => &mut self.power,
            0x4f => &mut self.expander,
            other => panic!("unexpected I2C address {other:#x}"),
        }
    }
}

impl ErrorType for Bus {
    type Error = Infallible;
}

impl I2c for Bus {
    fn read(&mut self, _address: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
        bytes.fill(0);
        Ok(())
    }

    fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
        if let [register, values @ ..] = bytes {
            let start = usize::from(*register);
            self.registers(address)[start..start + values.len()].copy_from_slice(values);
        }
        Ok(())
    }

    fn write_read(
        &mut self,
        address: u8,
        bytes: &[u8],
        read: &mut [u8],
    ) -> Result<(), Self::Error> {
        if let ([register], [value]) = (bytes, read) {
            *value = self.registers(address)[usize::from(*register)];
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

struct Delay;

impl DelayNs for Delay {
    fn delay_ns(&mut self, _ns: u32) {}
}

/// Polls a future that never waits, as every operation here is synchronous.
fn block_on<F: core::future::Future>(future: F) -> F::Output {
    use core::task::{Context, Poll, Waker};

    let mut context = Context::from_waker(Waker::noop());
    let mut future = core::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

fn initialized() -> super::CoreP4xDsiDisplay<Host, Bus> {
    block_on(
        CoreP4xDsiDisplayImplementation::<Host, Bus, Delay>::initialize(
            CoreP4xDsiDisplayBindings::new(Host::default(), Bus::new(), Delay),
            CoreP4xDsiDisplayConfig::new(0x4f, 0x6e),
        ),
    )
    .expect("CoreP4X display")
}

#[test]
fn brings_the_panel_up_in_m5gfx_order() {
    let display = initialized();
    // M5PM1: I2C sleep and watchdog off, 5 V boost (bit 3) on.
    assert_eq!(display.i2c.power[0x09], 0x00);
    assert_eq!(display.i2c.power[0x0a], 0x00);
    assert_eq!(display.i2c.power[0x06] & 0x08, 0x08);
    // M5IOE1: G8 and G9 to G12 push-pull outputs driven high.
    assert_eq!(display.i2c.expander[0x03] & 0x80, 0x80);
    assert_eq!(display.i2c.expander[0x04] & 0x0f, 0x0f);
    assert_eq!(display.i2c.expander[0x05] & 0x80, 0x80);
    assert_eq!(display.i2c.expander[0x06] & 0x0f, 0x0f);
    // 1 kHz backlight PWM on G9, left at full brightness.
    assert_eq!(display.i2c.expander[0x25..0x27], [0xe8, 0x03]);
    assert_eq!(display.i2c.expander[0x1b..0x1d], [0xff, 0x8f]);

    let calls = &display.host.calls;
    assert_eq!(calls[0], HostCall::Initialize(PANEL));
    assert_eq!(calls[1], HostCall::StartVideo);
    let commands: Vec<u8> = calls[2..]
        .iter()
        .filter_map(|call| match call {
            HostCall::Command(command) => Some(*command),
            _ => None,
        })
        .collect();
    assert_eq!(commands.first(), Some(&0x99));
    assert_eq!(commands[commands.len() - 3..], [0x11, 0x36, 0x29]);
}

#[test]
fn backlight_duty_spreads_the_level_over_twelve_bits() {
    assert_eq!(backlight_duty(0), 0x000);
    assert_eq!(backlight_duty(0x80), 0x808);
    assert_eq!(backlight_duty(0xff), 0xfff);
}

#[test]
fn draws_rotated_half_a_turn() {
    let mut display = initialized();
    display.host.calls.clear();
    let area = Rectangle::new(Point::new(10, 20), Size::new(3, 2));
    let pixels: Vec<Rgb565> = (0..6).map(|value| Rgb565::new(value, 0, 0)).collect();
    display.draw_rgb565(area, &pixels).expect("draw");
    let red = |value: u16| value << 11;
    assert_eq!(
        display.host.calls,
        vec![
            HostCall::Draw {
                x: WIDTH - 13,
                y: HEIGHT - 21,
                width: 3,
                pixels: vec![red(2), red(1), red(0)],
            },
            HostCall::Draw {
                x: WIDTH - 13,
                y: HEIGHT - 22,
                width: 3,
                pixels: vec![red(5), red(4), red(3)],
            },
        ]
    );
    block_on(
        display.flush(barracuda_peripheral::display::RefreshRequest::new(
            None,
            barracuda_peripheral::display::RefreshMode::Automatic,
        )),
    )
    .expect("flush");
    assert_eq!(display.host.calls.last(), Some(&HostCall::Present));
}
