//! Quadrature decoding on one ESP pulse-counter (PCNT) unit.
//!
//! The ESP32, ESP32-S3, and ESP32-C6 share esp-hal's PCNT driver, so their
//! Platforms share this adapter. Unit 0 counts every edge of both encoder
//! signals: channel 0 counts B's edges and channel 1 A's, each reversed by the
//! other signal's level. The wiring is esp-hal's own quadrature example
//! (`esp_hal::pcnt`); it counts up while B leads A, the direction PJRC's
//! Encoder library, which M5Stack's Dial and DinMeter libraries ship, calls
//! positive.

use barracuda_board_hal::QuadratureDecoder;
use esp_hal::{
    gpio::{Input, InputConfig, InputPin, Pull},
    pcnt::{
        channel::{CtrlMode, EdgeMode},
        unit::Unit,
        Pcnt,
    },
    peripherals::PCNT,
};

/// Pulses shorter than 800 APB cycles (10 µs at 80 MHz) are glitches, as in
/// esp-hal's PCNT encoder example.
const FILTER_APB_CYCLES: u16 = 800;

/// One quadrature encoder counted by PCNT unit 0.
///
/// The hardware counter is sixteen bits wide and wraps; each read adds the
/// change since the previous one to a wide software count, so a reader that
/// looks at least once every 16 384 edges loses none.
pub struct QuadratureCounter {
    unit: Unit<'static, 0>,
    _a: Input<'static>,
    _b: Input<'static>,
    last: i16,
    total: i64,
}

impl QuadratureDecoder for QuadratureCounter {
    type Error = core::convert::Infallible;

    fn count(&mut self) -> Result<i64, Self::Error> {
        let now = self.unit.value();
        let delta = now.wrapping_sub(self.last);
        self.last = now;
        self.total = self.total.saturating_add(i64::from(delta));
        Ok(self.total)
    }
}

/// Routes the encoder's A and B pins, with their internal pull-ups, into PCNT
/// unit 0 and starts counting from zero.
#[must_use]
pub fn quadrature_counter(
    pcnt: PCNT<'static>,
    a: impl InputPin + 'static,
    b: impl InputPin + 'static,
) -> QuadratureCounter {
    let config = InputConfig::default().with_pull(Pull::Up);
    let a = Input::new(a, config);
    let b = Input::new(b, config);
    let unit = Pcnt::new(pcnt).unit0;
    // No limits: the counter wraps through the whole sixteen-bit range, which
    // `QuadratureCounter::count` unwraps. Both calls accept `None`, and the
    // filter threshold is below the hardware's 1023-cycle maximum.
    unit.set_low_limit(None).unwrap_or(());
    unit.set_high_limit(None).unwrap_or(());
    unit.set_filter(Some(FILTER_APB_CYCLES)).unwrap_or(());

    let a_signal = a.peripheral_input();
    let b_signal = b.peripheral_input();
    unit.channel0.set_ctrl_signal(a_signal.clone());
    unit.channel0.set_edge_signal(b_signal.clone());
    unit.channel0
        .set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
    unit.channel0
        .set_input_mode(EdgeMode::Increment, EdgeMode::Decrement);
    unit.channel1.set_ctrl_signal(b_signal);
    unit.channel1.set_edge_signal(a_signal);
    unit.channel1
        .set_ctrl_mode(CtrlMode::Reverse, CtrlMode::Keep);
    unit.channel1
        .set_input_mode(EdgeMode::Decrement, EdgeMode::Increment);

    unit.clear();
    unit.resume();
    QuadratureCounter {
        unit,
        _a: a,
        _b: b,
        last: 0,
        total: 0,
    }
}
