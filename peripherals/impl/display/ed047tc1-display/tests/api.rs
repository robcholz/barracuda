//! Refresh behavior of the ED047TC1 display against a recording parallel bus.

#![allow(clippy::expect_used, clippy::panic)]

use core::{cell::RefCell, convert::Infallible};
use std::rc::Rc;

use barracuda_driver_ed047tc1::Mode;
use barracuda_driver_ed047tc1::{HEIGHT, LINE_TRANSFER_BYTES};
use barracuda_ed047tc1_display::{
    Ed047tc1Bindings, Ed047tc1Display, Ed047tc1DisplayConfig, dithered_level, mode_for,
};
use barracuda_peripheral::display::{
    Display, DisplayOrientation, DisplayTechnology, ParallelOutput, PixelFormat, RefreshMode,
    RefreshRequest,
};
use embedded_graphics_core::{
    geometry::{Point, Size},
    pixelcolor::Rgb565,
    primitives::Rectangle,
};
use embedded_hal::{
    delay::DelayNs,
    digital::{ErrorType, OutputPin},
};

#[derive(Default)]
struct Record {
    lines: usize,
    driving_lines: usize,
    supply_cycles: usize,
}

struct Bus(Rc<RefCell<Record>>);

impl ParallelOutput for Bus {
    type Error = Infallible;

    fn capacity(&self) -> usize {
        256
    }

    fn write_command(&mut self, _command: u8, _data: &[u8]) -> Result<(), Infallible> {
        panic!("the ED047TC1 source driver takes no commands");
    }

    fn write_data(&mut self, data: &[u8]) -> Result<(), Infallible> {
        assert_eq!(data.len(), LINE_TRANSFER_BYTES);
        let mut record = self.0.borrow_mut();
        record.lines += 1;
        if data.iter().any(|byte| *byte != 0) {
            record.driving_lines += 1;
        }
        Ok(())
    }
}

struct Pin {
    supply: bool,
    record: Rc<RefCell<Record>>,
}

impl ErrorType for Pin {
    type Error = Infallible;
}

impl OutputPin for Pin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Infallible> {
        if self.supply {
            self.record.borrow_mut().supply_cycles += 1;
        }
        Ok(())
    }
}

struct Delay;

impl DelayNs for Delay {
    fn delay_ns(&mut self, _ns: u32) {}
}

fn display(record: &Rc<RefCell<Record>>) -> Ed047tc1Display<Bus, Pin, Delay> {
    let pin = |supply| Pin {
        supply,
        record: record.clone(),
    };
    Ed047tc1Display::new(
        Ed047tc1Bindings::new(
            Bus(record.clone()),
            pin(false),
            pin(false),
            pin(false),
            pin(false),
            pin(true),
            Delay,
        ),
        Ed047tc1DisplayConfig::new(DisplayOrientation::Deg270),
    )
    .expect("display")
}

fn block_on<F: core::future::Future>(future: F) -> F::Output {
    let mut future = core::pin::pin!(future);
    let waker = core::task::Waker::noop();
    let mut context = core::task::Context::from_waker(waker);
    loop {
        if let core::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
    }
}

#[test]
fn papers3_is_upright_portrait_gray4() {
    let descriptor = Ed047tc1DisplayConfig::new(DisplayOrientation::Deg270).descriptor();
    assert_eq!(descriptor.size(), Size::new(540, 960));
    assert_eq!(descriptor.pixel_format(), PixelFormat::Gray4);
    assert_eq!(descriptor.technology(), DisplayTechnology::Epaper);
}

#[test]
fn quality_refresh_runs_eraser_and_waveform_then_powers_off() {
    let record = Rc::new(RefCell::new(Record::default()));
    let mut display = display(&record);
    display
        .draw_rgb565(
            Rectangle::new(Point::new(0, 0), Size::new(2, 1)),
            &[Rgb565::new(0, 0, 0), Rgb565::new(0, 0, 0)],
        )
        .expect("draw");
    block_on(display.flush(RefreshRequest::new(
        Some(Rectangle::new(Point::new(0, 0), Size::new(2, 1))),
        RefreshMode::Full,
    )))
    .expect("flush");
    let record = record.borrow();
    // 36 frames of waveform plus the final idle frame, every line each frame.
    assert_eq!(record.lines, 37 * HEIGHT);
    // Only the two native lines holding the drawn pixels are ever driven.
    assert!(record.driving_lines > 0 && record.driving_lines <= 2 * 36);
    assert_eq!(record.supply_cycles, 1);
}

#[test]
fn modes_and_dithering_follow_m5gfx() {
    assert_eq!(mode_for(RefreshMode::Automatic), Mode::Quality);
    assert_eq!(mode_for(RefreshMode::Partial), Mode::Text);
    assert_eq!(mode_for(RefreshMode::Fast), Mode::Fast);
    assert_eq!(dithered_level(Rgb565::new(0, 0, 0), 0, 0), 0);
    assert_eq!(dithered_level(Rgb565::new(31, 63, 31), 3, 3), 15);
}
