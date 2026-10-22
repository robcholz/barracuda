//! Classic CAN on the ESP TWAI controller, through esp-hal's async driver.
//!
//! The controller stays in reset, off the bus, until a bit rate is chosen.
//! esp-hal fixes the bit rate when it builds the driver, so each start drops
//! the running driver and builds a new one from the same peripheral and pins
//! the binding owns.

use core::{
    future::Future,
    pin::pin,
    task::{Context, Waker},
};

use barracuda_board_hal::{can, CanController};
use esp_hal::{
    gpio::{AnyPin, Pin},
    peripherals::TWAI0,
    twai::{
        BaudRate, EspTwaiError, EspTwaiFrame, ExtendedId, Id, StandardId, TimingConfig, Twai,
        TwaiConfiguration, TwaiMode,
    },
    Async,
};

/// The TWAI counts its time quanta in the 80 MHz APB clock (esp-hal asserts
/// it on the ESP32-S3).
const APB_CLOCK_HZ: u32 = 80_000_000;

/// Time quanta per bit tried in order: ESP-IDF's 20 for most rates and 25
/// for 800 kbit/s (`twai_types.h`), then 16 and 10 for other rates.
const QUANTA_PER_BIT: [u32; 4] = [20, 25, 16, 10];

/// The prescaler's range: esp-hal writes `prescaler / 2 - 1` into a 14-bit
/// field, so it is even and at most 32 768.
const MAX_PRESCALER: u32 = 32_768;

/// Error-counter level from which the controller is error passive (ISO
/// 11898-1).
const ERROR_PASSIVE_COUNT: u8 = 128;

/// Bus timing for `bitrate` with the sample point near 80 %, or `None` when
/// the APB clock does not divide to it exactly.
fn timing(bitrate: u32) -> Option<TimingConfig> {
    QUANTA_PER_BIT.into_iter().find_map(|quanta| {
        let per_bit = bitrate.checked_mul(quanta)?;
        if per_bit == 0 || !APB_CLOCK_HZ.is_multiple_of(per_bit) {
            return None;
        }
        let prescaler = APB_CLOCK_HZ / per_bit;
        if !(2..=MAX_PRESCALER).contains(&prescaler) || !prescaler.is_multiple_of(2) {
            return None;
        }
        // One synchronization quantum, then segment 1 (at most 16 quanta)
        // and segment 2 (at most 8) around the sample point.
        let tseg_2 = (quanta / 5).max(quanta.saturating_sub(17));
        let tseg_1 = quanta.checked_sub(1)?.checked_sub(tseg_2)?;
        if !(1..=16).contains(&tseg_1) || !(1..=8).contains(&tseg_2) {
            return None;
        }
        Some(TimingConfig {
            baud_rate_prescaler: u16::try_from(prescaler).ok()?,
            sync_jump_width: u8::try_from(tseg_2.min(3)).ok()?,
            tseg_1: u8::try_from(tseg_1).ok()?,
            tseg_2: u8::try_from(tseg_2).ok()?,
            triple_sample: false,
        })
    })
}

/// TWAI controller failure.
#[derive(Debug)]
pub enum TwaiError {
    /// The controller has not been started.
    NotStarted,
    /// The APB clock does not divide to the requested bit rate.
    UnsupportedBitrate(u32),
    /// The controller failed a transfer or left the bus.
    Bus(EspTwaiError),
}

/// The Board's TWAI controller and the pins of its transceiver.
pub struct TwaiController {
    twai: TWAI0<'static>,
    tx: AnyPin<'static>,
    rx: AnyPin<'static>,
    driver: Option<Twai<'static, Async>>,
}

/// Takes the TWAI controller and its transceiver's TXD and RXD pins; the
/// controller joins no bus until started.
#[must_use]
pub fn twai_controller(
    twai: TWAI0<'static>,
    tx: impl Pin + 'static,
    rx: impl Pin + 'static,
) -> TwaiController {
    TwaiController {
        twai,
        tx: tx.degrade(),
        rx: rx.degrade(),
        driver: None,
    }
}

// SAFETY: esp-hal marks its async drivers `!Send` because their interrupt
// handler is bound to the core that built them. Barracuda runs every task on
// the core the Platform entry starts (the second core is never started), so
// the controller and the driver it holds never leave the core whose
// interrupt serves them. The binding owns the only TWAI0 and pin tokens, and
// at most one driver exists at a time.
#[allow(unsafe_code)]
unsafe impl Send for TwaiController {}

impl TwaiController {
    fn driver(&mut self) -> Result<&mut Twai<'static, Async>, TwaiError> {
        self.driver.as_mut().ok_or(TwaiError::NotStarted)
    }

    fn leave_bus(&mut self) {
        if let Some(driver) = self.driver.take() {
            // Back in reset mode the controller sends only recessive bits;
            // dropping the configuration then releases the peripheral.
            drop(driver.stop());
        }
    }
}

fn frame_to_hal(frame: &can::Frame) -> Option<EspTwaiFrame> {
    let id: Id = match frame.id() {
        can::Id::Standard(raw) => StandardId::new(raw)?.into(),
        can::Id::Extended(raw) => ExtendedId::new(raw)?.into(),
    };
    if frame.is_remote() {
        EspTwaiFrame::new_remote(id, usize::from(frame.dlc()))
    } else {
        EspTwaiFrame::new(id, frame.data())
    }
}

fn frame_from_hal(frame: &EspTwaiFrame) -> Option<can::Frame> {
    let id = match frame.identifier() {
        Id::Standard(id) => can::Id::standard(id.as_raw())?,
        Id::Extended(id) => can::Id::extended(id.as_raw())?,
    };
    if frame.is_remote_request() {
        can::Frame::remote(id, u8::try_from(frame.data_length_code()).ok()?)
    } else {
        can::Frame::new(id, frame.data())
    }
}

impl CanController for TwaiController {
    type Error = TwaiError;

    fn supports_bitrate(&self, bitrate: u32) -> bool {
        timing(bitrate).is_some()
    }

    #[allow(unsafe_code)]
    fn start(&mut self, bitrate: u32) -> Result<(), Self::Error> {
        let timing = timing(bitrate).ok_or(TwaiError::UnsupportedBitrate(bitrate))?;
        self.leave_bus();
        // SAFETY: this binding received the only TWAI0 and transceiver pin
        // tokens of the Board and never hands them out. The driver built from
        // their copies is the only user of the peripheral and pins, and the
        // previous one has just been dropped, so at most one driver exists.
        let (twai, rx, tx) = unsafe {
            (
                self.twai.clone_unchecked(),
                self.rx.clone_unchecked(),
                self.tx.clone_unchecked(),
            )
        };
        let driver =
            TwaiConfiguration::new(twai, rx, tx, BaudRate::Custom(timing), TwaiMode::Normal)
                .into_async()
                .start();
        let driver = self.driver.insert(driver);
        // esp-hal queues received frames in a static channel that outlives
        // the previous driver; discard what an earlier start left there.
        let mut context = Context::from_waker(Waker::noop());
        while pin!(driver.receive_async()).poll(&mut context).is_ready() {}
        Ok(())
    }

    fn stop(&mut self) {
        self.leave_bus();
    }

    fn state(&self) -> can::BusState {
        let Some(driver) = self.driver.as_ref() else {
            return can::BusState::Stopped;
        };
        if driver.is_bus_off() {
            can::BusState::BusOff
        } else if driver.transmit_error_count() >= ERROR_PASSIVE_COUNT
            || driver.receive_error_count() >= ERROR_PASSIVE_COUNT
        {
            can::BusState::ErrorPassive
        } else {
            can::BusState::ErrorActive
        }
    }

    async fn transmit(&mut self, frame: &can::Frame) -> Result<(), Self::Error> {
        // The semantic frame is valid by construction, so the conversion
        // always succeeds; a failure would be esp-hal's own limit.
        let frame = frame_to_hal(frame)
            .ok_or(TwaiError::Bus(EspTwaiError::InvalidDataLength(frame.dlc())))?;
        self.driver()?
            .transmit_async(&frame)
            .await
            .map_err(TwaiError::Bus)
    }

    async fn receive(&mut self) -> Result<can::Frame, Self::Error> {
        loop {
            let frame = self
                .driver()?
                .receive_async()
                .await
                .map_err(TwaiError::Bus)?;
            // The controller only delivers compliant frames; skip any other.
            if let Some(frame) = frame_from_hal(&frame) {
                return Ok(frame);
            }
        }
    }
}
