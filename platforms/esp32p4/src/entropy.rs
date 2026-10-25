//! The ESP32-P4 random number generator, fed by SAR ADC noise, as the
//! Platform entropy source.
//!
//! esp-hal 1.2 has no `TrngSource` for the ESP32-P4, and its `Rng` reads the
//! generator without starting a noise source. [`Esp32P4Entropy::start`]
//! starts the one ESP-IDF uses on this radio-less chip: SAR ADC1 sampling an
//! internal channel under the digital controller, the sequence of ESP-IDF's
//! `bootloader_random_enable` (`bootloader_random_esp32p4.c`). The values it
//! writes live in [`crate::entropy_source`].
//!
//! esp-hal exposes no API for these registers, so this module writes them
//! itself. It is the only place in the Platform that does, and each `unsafe`
//! block writes a field value from ESP-IDF that fits the field.

#![allow(unsafe_code)]

use barracuda_platform::{Entropy, EntropyUnavailable};
use esp_hal::{
    peripherals::{ADC1, APB_SARADC, HP_SYS_CLKRST, LP_ADC, LP_I2C_ANA_MST, LP_PERI, PMU, RNG},
    time::{Duration, Instant},
};

use crate::entropy_source::{
    self, DataRegister, RegI2cField, SampleClock, FORCE_SAR_POWER_UP, NOISE_ATTENUATION,
    NOISE_CHANNEL, SAR_ADC_ANALOG_SETUP, SAR_ADC_BLOCK, SAR_ADC_MASTER_SELECT, SAR_CLOCK_DIVIDER,
    TRIGGER_INTERVAL,
};

/// Entropy from the ESP32-P4 random number generator while SAR ADC1 noise
/// feeds it.
///
/// A value exists only once [`Esp32P4Entropy::start`] has started the noise
/// source, which then runs until reset, so [`Entropy::fill`] always returns
/// true random bytes.
#[derive(Clone, Copy, Debug)]
pub struct Esp32P4Entropy {
    data: DataRegister,
}

impl Esp32P4Entropy {
    /// Starts SAR ADC1 as the generator's noise source.
    ///
    /// It consumes the ADC1 and RNG singletons for the life of the firmware:
    /// an ADC driver would take SAR ADC1 back to the RTC controller, or reset
    /// the SAR ADC, and stop the noise. Call it once, from the entry, before
    /// any task runs: it uses the analog configuration bus that esp-hal's
    /// clock set-up also uses.
    #[must_use]
    pub fn start(_rng: RNG<'static>, _adc: ADC1<'static>) -> Self {
        let data = DataRegister::for_major_revision(esp_hal::efuse::chip_revision().major);
        enable_sar_adc_noise();
        if data == DataRegister::LpTrng {
            enable_lp_trng();
        }
        Self { data }
    }

    fn read(self) -> u32 {
        match self.data {
            DataRegister::LpSystem => RNG::regs().rng_data().read().bits(),
            // SAFETY: `LP_TRNG_DATA` is the read-only data register of the
            // LP_TRNG block every chip of revision v3.0 or later has
            // (ESP-IDF `hw_ver3/soc/trng_reg.h`); reading it has no side
            // effect. esp32p4 0.3 does not describe the block.
            DataRegister::LpTrng => unsafe {
                core::ptr::read_volatile(entropy_source::LP_TRNG_DATA as *const u32)
            },
        }
    }
}

impl Entropy for Esp32P4Entropy {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        let source = *self;
        entropy_source::fill_bytes(bytes, || source.read(), &SystemClock);
        Ok(())
    }
}

/// The system timer as the sampling loop's clock.
struct SystemClock;

impl SampleClock for SystemClock {
    type Instant = Instant;

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sampled_since(&self, start: Instant) -> bool {
        start.elapsed() >= Duration::from_micros(entropy_source::BYTE_SAMPLE_MICROS)
    }
}

/// `bootloader_random_enable` up to `rng_ll_enable`.
fn enable_sar_adc_noise() {
    let clocks = HP_SYS_CLKRST::regs();
    // `sar_periph_ctrl_adc_reset`.
    clocks.hp_rst_en2().modify(|_, w| w.rst_en_adc().set_bit());
    clocks
        .hp_rst_en2()
        .modify(|_, w| w.rst_en_adc().clear_bit());
    // `_adc_ll_enable_bus_clock(true)`.
    clocks
        .soc_clk_ctrl2()
        .modify(|_, w| w.adc_apb_clk_en().set_bit());
    clocks
        .peri_clk_ctrl23()
        .modify(|_, w| w.adc_clk_en().set_bit());

    let adc = APB_SARADC::regs();
    // `adc_ll_digi_clk_sel(ADC_DIGI_CLK_SRC_XTAL)` and
    // `adc_ll_digi_controller_clk_div(0, 0, 0)`.
    // SAFETY: 0 selects XTAL and is a valid value of every divider field.
    clocks
        .peri_clk_ctrl22()
        .modify(|_, w| unsafe { w.adc_clk_src_sel().bits(0) });
    clocks.peri_clk_ctrl23().modify(|_, w| unsafe {
        w.adc_clk_div_num().bits(0);
        w.adc_clk_div_numerator().bits(0);
        w.adc_clk_div_denominator().bits(0)
    });
    adc.ctrl().modify(|_, w| w.sar_clk_gated().set_bit());

    // `regi2c_ctrl_ll_i2c_sar_periph_enable`: power the SAR ADC's analog
    // registers, which sit in the PERIF_I2C power group.
    let pmu = PMU::regs();
    pmu.rf_pwc().modify(|_, w| w.perif_i2c_rstb().clear_bit());
    pmu.rf_pwc().modify(|_, w| w.xpd_perif_i2c().set_bit());
    pmu.rf_pwc().modify(|_, w| w.perif_i2c_rstb().set_bit());

    // `ANALOG_CLOCK_ENABLE`: the generator needs the analog I2C master clock
    // while it runs, not only for the writes below.
    LP_PERI::regs()
        .clk_en()
        .modify(|_, w| w.ck_en_lp_i2cmst().set_bit());

    // `adc_ll_regi2c_init` and `adc_ll_set_calibration_param`.
    for (field, value) in SAR_ADC_ANALOG_SETUP {
        write_sar_adc_field(field, value);
    }

    // `adc_ll_digi_set_pattern_table` for entries 0 to 3 and
    // `adc_ll_digi_set_pattern_table_len(ADC_UNIT_1, 1)`.
    let item = entropy_source::pattern_item(NOISE_CHANNEL, NOISE_ATTENUATION);
    // SAFETY: the table value fills only the register's 24 table bits.
    adc.sar1_patt_tab1()
        .modify(|_, w| unsafe { w.sar1_patt_tab1().bits(entropy_source::pattern_table(item)) });
    // SAFETY: 0 is a pattern length of one entry.
    adc.ctrl()
        .modify(|_, w| unsafe { w.sar1_patt_len().bits(0) });

    // `adc_ll_set_controller(ADC_UNIT_1, ADC_LL_CTRL_DIG)`.
    let lp_adc = LP_ADC::regs();
    lp_adc
        .meas1_mux()
        .modify(|_, w| w.sar1_dig_force().set_bit());
    lp_adc.meas1_ctrl2().modify(|_, w| {
        w.meas1_start_force().set_bit();
        w.sar1_en_pad_force().set_bit()
    });

    // `adc_ll_digi_set_power_manage(ADC_UNIT_1, ADC_LL_POWER_SW_ON)`,
    // `adc_ll_digi_set_clk_div(15)`, `adc_ll_digi_set_trigger_interval(100)`.
    // SAFETY: 3 fits the two-bit power field, 15 the eight-bit divider.
    adc.ctrl().modify(|_, w| unsafe {
        w.sar_clk_gated().set_bit();
        w.xpd_sar1_force().bits(FORCE_SAR_POWER_UP);
        w.sar_clk_div().bits(SAR_CLOCK_DIVIDER)
    });
    // SAFETY: 100 fits the twelve-bit timer target.
    adc.ctrl2()
        .modify(|_, w| unsafe { w.timer_target().bits(TRIGGER_INTERVAL) });
    // `adc_ll_digi_trigger_enable`.
    adc.ctrl2().modify(|_, w| {
        w.timer_sel().set_bit();
        w.timer_en().set_bit()
    });
}

/// `rng_ll_enable` for chip revision v3.0 and later.
fn enable_lp_trng() {
    let config = entropy_source::LP_TRNG_CFG as *mut u32;
    let reset = entropy_source::LP_TRNG_RSTN as *mut u32;
    let date = entropy_source::LP_TRNG_DATE as *mut u32;
    // SAFETY: these are the LP_TRNG block's configuration, reset, and
    // date/clock registers on every chip of revision v3.0 or later (ESP-IDF
    // `hw_ver3/soc/reg_base.h` and `trng_reg.h`), which the caller checked;
    // nothing else in the firmware uses the block. The writes are ESP-IDF's
    // `rng_ll_enable`: clock on, reset pulse, prescaler, timer, sampling.
    unsafe {
        date.write_volatile(date.read_volatile() | entropy_source::LP_TRNG_CLOCK_ENABLE);
        reset.write_volatile(reset.read_volatile() & !entropy_source::LP_TRNG_RUN);
        reset.write_volatile(reset.read_volatile() | entropy_source::LP_TRNG_RUN);
        config.write_volatile(entropy_source::lp_trng_config(config.read_volatile()));
    }
}

/// `REGI2C_WRITE_MASK(I2C_SAR_ADC, field, value)`: reads the register over
/// the analog configuration bus, replaces the field, and writes it back.
///
/// It follows esp-hal's ESP32-P4 `regi2c_read`/`regi2c_write`, which are
/// private to esp-hal.
fn write_sar_adc_field(field: RegI2cField, value: u8) {
    select_sar_adc_master();
    let master = LP_I2C_ANA_MST::regs();
    wait_analog_bus_idle();
    // SAFETY: the command word fits the 25-bit control field.
    master.i2c0_ctrl().write(|w| unsafe {
        w.i2c0_ctrl()
            .bits(entropy_source::read_command(SAR_ADC_BLOCK, field.register))
    });
    wait_analog_bus_idle();
    let current = entropy_source::read_data(master.i2c0_ctrl().read().i2c0_ctrl().bits());
    let command =
        entropy_source::write_command(SAR_ADC_BLOCK, field.register, field.apply(current, value));
    // SAFETY: as above.
    master
        .i2c0_ctrl()
        .write(|w| unsafe { w.i2c0_ctrl().bits(command) });
    wait_analog_bus_idle();
}

/// Clocks the analog I2C master from the 160 MHz source and points it at the
/// SAR ADC block (esp-hal `regi2c_enable_block`).
fn select_sar_adc_master() {
    let master = LP_I2C_ANA_MST::regs();
    master
        .clk160m()
        .modify(|_, w| w.clk_i2c_mst_sel_160m().set_bit());
    // SAFETY: 0 clears every master select; the select bit fits the field.
    master
        .ana_conf2()
        .modify(|_, w| unsafe { w.ana_conf2().bits(0) });
    master
        .ana_conf1()
        .modify(|_, w| unsafe { w.ana_conf1().bits(0) });
    master
        .ana_conf2()
        .modify(|_, w| unsafe { w.ana_conf2().bits(SAR_ADC_MASTER_SELECT) });
}

fn wait_analog_bus_idle() {
    while LP_I2C_ANA_MST::regs()
        .i2c0_ctrl()
        .read()
        .i2c0_busy()
        .bit_is_set()
    {
        core::hint::spin_loop();
    }
}
