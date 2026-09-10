//! ESP32-S3 capacitive-touch transport.

// The PAC marks arbitrary-width field writes as unsafe even when the value is
// constrained to the documented field width. Keep those reviewed writes in
// this module instead of weakening the safety policy for the rest of Platform.
#![allow(unsafe_code)]

use barracuda_board_hal::CapacitiveTouchChannels;
use esp_hal::{
    analog::adc::{AdcChannel, AdcConfig, AdcPin, Attenuation},
    gpio::AnalogPin,
    peripherals::{ADC1, RTC_CNTL, RTC_IO, SENS},
};

const CHANNEL_0: usize = 6;
const CHANNEL_1: usize = 7;
const CHANNEL_MASK: u16 = (1 << CHANNEL_0) | (1 << CHANNEL_1);

/// ESP32-S3 two-channel capacitive-touch sampler.
pub struct CapacitiveTouchButtons<P0, P1> {
    _pin0: AdcPin<P0, ADC1<'static>>,
    _pin1: AdcPin<P1, ADC1<'static>>,
    sens: SENS<'static>,
    _rtc_control: RTC_CNTL<'static>,
    _rtc_io: RTC_IO<'static>,
}

impl<P0, P1> CapacitiveTouchChannels for CapacitiveTouchButtons<P0, P1> {
    type Error = core::convert::Infallible;

    fn read(&mut self) -> Result<[u32; 2], Self::Error> {
        let registers = self.sens.register_block();
        registers
            .sar_touch_conf()
            .modify(|_, writer| unsafe { writer.sar_touch_data_sel().bits(0) });
        Ok([
            registers
                .sar_touch_status(CHANNEL_0 - 1)
                .read()
                .data()
                .bits(),
            registers
                .sar_touch_status(CHANNEL_1 - 1)
                .read()
                .data()
                .bits(),
        ])
    }
}

/// Configures GPIO6 and GPIO7 as continuously sampled ESP32-S3 touch channels.
#[must_use]
pub fn capacitive_touch_buttons<P0, P1>(
    sens: SENS<'static>,
    rtc_control: RTC_CNTL<'static>,
    rtc_io: RTC_IO<'static>,
    pin0: P0,
    pin1: P1,
) -> CapacitiveTouchButtons<P0, P1>
where
    P0: AdcChannel + AnalogPin,
    P1: AdcChannel + AnalogPin,
{
    let mut analog = AdcConfig::<ADC1<'static>>::new();
    let pin0 = analog.enable_pin(pin0, Attenuation::_0dB);
    let pin1 = analog.enable_pin(pin1, Attenuation::_0dB);

    configure_touch_controller(&sens, &rtc_control, &rtc_io);

    CapacitiveTouchButtons {
        _pin0: pin0,
        _pin1: pin1,
        sens,
        _rtc_control: rtc_control,
        _rtc_io: rtc_io,
    }
}

fn configure_touch_controller(
    sens: &SENS<'static>,
    rtc_control: &RTC_CNTL<'static>,
    rtc_io: &RTC_IO<'static>,
) {
    let rtc = rtc_control.register_block();
    let sensor = sens.register_block();
    let io = rtc_io.register_block();

    // Stop and reset the FSM before changing scan membership.
    rtc.touch_ctrl2().modify(|_, writer| {
        writer.touch_start_en().clear_bit();
        writer.touch_slp_timer_en().clear_bit()
    });
    force_current_measurement_done(rtc_control);
    rtc.touch_ctrl2()
        .modify(|_, writer| writer.touch_reset().set_bit());
    rtc.touch_ctrl2()
        .modify(|_, writer| writer.touch_reset().clear_bit());

    // Espressif's ESP32-S3 legacy defaults: 500 charge/discharge cycles,
    // 15 slow-clock sleep cycles, 2.7 V high, 0.5 V low, 0.5 V attenuation.
    rtc.touch_ctrl1().write(|writer| unsafe {
        writer
            .touch_sleep_cycles()
            .bits(15)
            .touch_meas_num()
            .bits(500)
    });
    rtc.touch_ctrl2().modify(|_, writer| unsafe {
        writer
            .touch_drefh()
            .bits(3)
            .touch_drefl()
            .bits(0)
            .touch_drange()
            .bits(2)
            .touch_xpd_wait()
            .bits(0xff)
            .touch_dbias()
            .set_bit()
            .touch_clkgate_en()
            .set_bit()
    });

    // Float the initial charge level and remove digital pulls from both pads.
    for channel in [CHANNEL_0, CHANNEL_1] {
        io.touch_pad(channel).modify(|_, writer| {
            writer
                .fun_ie()
                .clear_bit()
                .mux_sel()
                .set_bit()
                .xpd()
                .clear_bit()
                .rue()
                .clear_bit()
                .rde()
                .clear_bit()
        });
    }

    // Maximum charge current is the documented default for ESP32-S3 touch pads.
    rtc.touch_dac()
        .modify(|_, writer| unsafe { writer.touch_pad6_dac().bits(7).touch_pad7_dac().bits(7) });

    rtc.touch_scan_ctrl().modify(|_, writer| unsafe {
        writer
            .touch_inactive_connection()
            .set_bit()
            .touch_scan_pad_map()
            .bits(CHANNEL_MASK)
    });
    sensor.sar_touch_conf().modify(|_, writer| unsafe {
        writer
            .sar_touch_outen()
            .bits(CHANNEL_MASK)
            .sar_touch_data_sel()
            .bits(0)
            .sar_touch_status_clr()
            .set_bit()
    });
    sensor
        .sar_touch_chn_st()
        .write(|writer| unsafe { writer.sar_touch_channel_clr().bits(CHANNEL_MASK) });

    // Timer mode continuously refreshes both raw counters for cheap polling.
    rtc.touch_ctrl2()
        .modify(|_, writer| writer.touch_start_force().clear_bit());
    force_current_measurement_done(rtc_control);
    rtc.touch_ctrl2()
        .modify(|_, writer| writer.touch_slp_timer_en().set_bit());
}

fn force_current_measurement_done(rtc_control: &RTC_CNTL<'static>) {
    let rtc = rtc_control.register_block();
    rtc.touch_ctrl2()
        .modify(|_, writer| unsafe { writer.touch_timer_force_done().bits(3) });
    rtc.touch_ctrl2()
        .modify(|_, writer| unsafe { writer.touch_timer_force_done().bits(0) });
}
