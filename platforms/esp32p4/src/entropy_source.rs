//! ESP-IDF's SAR ADC entropy source for the ESP32-P4, as plain values.
//!
//! The ESP32-P4 has no radio, so its random number generator is only fed by
//! physical noise while SAR ADC1 samples an internal channel under the
//! digital controller. ESP-IDF's `bootloader_random_enable`
//! (`components/bootloader_support/src/bootloader_random_esp32p4.c`) sets
//! that up, and ESP-IDF keeps it running for the application on chips
//! without a radio (`docs/en/api-reference/system/random.rst`, "Startup").
//! This module holds every value that sequence writes and the sampling loop
//! of `bootloader_fill_random`, free of registers so host tests cover them.

/// The SAR ADC1 initial code ESP-IDF programs for the noise source
/// (`I2C_SAR_ADC_INIT_CODE_VAL`).
pub const SAR1_INITIAL_CODE: u16 = 2166;

/// The internal SAR ADC1 channel ESP-IDF samples (`ADC_CHANNEL_10`).
pub const NOISE_CHANNEL: u8 = 10;

/// The attenuation ESP-IDF samples it at (`ADC_ATTEN_DB_12`).
pub const NOISE_ATTENUATION: u8 = 3;

/// SAR clock divider of the digital controller (`adc_ll_digi_set_clk_div(15)`).
pub const SAR_CLOCK_DIVIDER: u8 = 15;

/// Digital controller trigger interval in controller clock cycles
/// (`adc_ll_digi_set_trigger_interval(100)`).
pub const TRIGGER_INTERVAL: u16 = 100;

/// Software power-up of a SAR unit (`LP_ADC_FORCE_XPD_SAR_PU`).
pub const FORCE_SAR_POWER_UP: u8 = 3;

/// The shortest time one output byte folds register reads over.
///
/// ESP-IDF tested the ESP32-P4 generator at about 63 kHz bytes in the
/// bootloader (`RNG_CPU_WAIT_CYCLE_NUM`, CPU MHz x 16 cycles) and 75 kHz in
/// the application (`APB_CYCLE_WAIT_NUM`, CPU MHz x 14); 16 us per byte keeps
/// to the slower of the two at any CPU clock.
pub const BYTE_SAMPLE_MICROS: u64 = 16;

/// The analog configuration bus slave holding the SAR ADC registers
/// (`I2C_SAR_ADC`, host 0).
pub const SAR_ADC_BLOCK: u8 = 0x69;

/// The bit that hands the analog I2C master to the SAR ADC block
/// (`REGI2C_SAR_I2C_MST_SEL` in `ANA_CONF2`).
pub const SAR_ADC_MASTER_SELECT: u32 = 1 << 7;

/// One field of a register on the analog configuration bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegI2cField {
    /// Register address within the block.
    pub register: u8,
    /// Most significant bit of the field.
    pub msb: u8,
    /// Least significant bit of the field.
    pub lsb: u8,
}

impl RegI2cField {
    /// Returns `current` with this field replaced by `value`, as ESP-IDF's
    /// `REGI2C_WRITE_MASK` does.
    #[must_use]
    pub const fn apply(self, current: u8, value: u8) -> u8 {
        let width = self.msb - self.lsb + 1;
        let mask = (((1_u16 << width) - 1) << self.lsb) as u8;
        (current & !mask) | ((value << self.lsb) & mask)
    }
}

/// `ADC_SAR1_INITIAL_CODE_LOW_ADDR` (`soc/regi2c_saradc.h`).
pub const SAR1_INITIAL_CODE_LOW: RegI2cField = RegI2cField {
    register: 0,
    msb: 7,
    lsb: 0,
};

/// `ADC_SAR1_INITIAL_CODE_HIGH_ADDR` (`soc/regi2c_saradc.h`).
pub const SAR1_INITIAL_CODE_HIGH: RegI2cField = RegI2cField {
    register: 1,
    msb: 3,
    lsb: 0,
};

/// `I2C_SAR_ADC_DTEST_VDD_GRP1` (`soc/regi2c_saradc.h`).
pub const SAR_DTEST_VDD_GRP1: RegI2cField = RegI2cField {
    register: 9,
    msb: 3,
    lsb: 0,
};

/// `I2C_SAR_ADC_ENT_VDD_GRP1` (`soc/regi2c_saradc.h`).
pub const SAR_ENT_VDD_GRP1: RegI2cField = RegI2cField {
    register: 9,
    msb: 4,
    lsb: 4,
};

/// The analog configuration bus writes of `adc_ll_regi2c_init` and
/// `adc_ll_set_calibration_param(ADC_UNIT_1, 2166)`, in order.
pub const SAR_ADC_ANALOG_SETUP: [(RegI2cField, u8); 4] = [
    (SAR_DTEST_VDD_GRP1, 0),
    (SAR_ENT_VDD_GRP1, 1),
    (SAR1_INITIAL_CODE_HIGH, initial_code_high(SAR1_INITIAL_CODE)),
    (SAR1_INITIAL_CODE_LOW, initial_code_low(SAR1_INITIAL_CODE)),
];

/// The high byte of an initial code (`param >> 8`).
#[must_use]
pub const fn initial_code_high(code: u16) -> u8 {
    (code >> 8) as u8
}

/// The low byte of an initial code (`param & 0xFF`).
#[must_use]
pub const fn initial_code_low(code: u16) -> u8 {
    (code & 0xFF) as u8
}

/// The `I2C0_CTRL` word that reads `register` of `block`
/// (slave ID in bits 7:0, address in bits 15:8).
#[must_use]
pub const fn read_command(block: u8, register: u8) -> u32 {
    (block as u32) | ((register as u32) << 8)
}

/// The `I2C0_CTRL` word that writes `data` to `register` of `block`
/// (data in bits 23:16, write control in bit 24).
#[must_use]
pub const fn write_command(block: u8, register: u8, data: u8) -> u32 {
    read_command(block, register) | ((data as u32) << 16) | (1 << 24)
}

/// The data byte an analog bus read returns in `I2C0_CTRL` bits 23:16.
#[must_use]
pub const fn read_data(control: u32) -> u8 {
    ((control >> 16) & 0xFF) as u8
}

/// One digital controller pattern table item: attenuation in bits 1:0, the
/// channel in bits 5:2.
#[must_use]
pub const fn pattern_item(channel: u8, attenuation: u8) -> u8 {
    (attenuation & 0x3) | ((channel & 0xF) << 2)
}

/// The first SAR ADC1 pattern table register with all four items set to
/// `item`, as `adc_ll_digi_set_pattern_table` writes entries 0 to 3, entry
/// 0 in bits 23:18.
#[must_use]
pub const fn pattern_table(item: u8) -> u32 {
    let item = (item & 0x3F) as u32;
    (item << 18) | (item << 12) | (item << 6) | item
}

/// The register the generator's output is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRegister {
    /// `LP_SYSTEM_REG_RNG_DATA_REG`, read by ESP-IDF for chip revisions
    /// before v3.0.
    LpSystem,
    /// The LP_TRNG block's `RNG_DATA_REG`, read by ESP-IDF from chip
    /// revision v3.0 (`hal/esp32p4/include/hal/rng_ll.h`).
    LpTrng,
}

impl DataRegister {
    /// Selects the register ESP-IDF reads on a chip of this major revision.
    #[must_use]
    pub const fn for_major_revision(major: u8) -> Self {
        if major >= 3 {
            Self::LpTrng
        } else {
            Self::LpSystem
        }
    }
}

/// The LP_TRNG block of chip revision v3.0 and later: `DR_REG_LP_TRNG_BASE`
/// in `soc/esp32p4/register/hw_ver3/soc/reg_base.h`.
pub const LP_TRNG_BASE: usize = 0x5012_6000;

/// `RNG_CFG_REG` (`hw_ver3/soc/trng_reg.h`).
pub const LP_TRNG_CFG: usize = LP_TRNG_BASE;
/// `RNG_DATA_REG` (`hw_ver3/soc/trng_reg.h`).
pub const LP_TRNG_DATA: usize = LP_TRNG_BASE + 0x4;
/// `RNG_RSTN_REG` (`hw_ver3/soc/trng_reg.h`).
pub const LP_TRNG_RSTN: usize = LP_TRNG_BASE + 0x8;
/// `RNG_DATE_REG` (`hw_ver3/soc/trng_reg.h`).
pub const LP_TRNG_DATE: usize = LP_TRNG_BASE + 0xC;

/// `RNG_CLK_EN` in `RNG_DATE_REG`.
pub const LP_TRNG_CLOCK_ENABLE: u32 = 1 << 31;
/// `RNG_RSTN`: 1 releases the block from reset.
pub const LP_TRNG_RUN: u32 = 1;
/// `RNG_LL_CFG_PSCALE`, the timer prescaler ESP-IDF sets.
pub const LP_TRNG_PRESCALER: u8 = 255;

/// `RNG_CFG_REG` after `rng_ll_enable`: the prescaler, the timer XOR
/// (`RNG_TIMER_EN`, bit 9), and ring oscillator sampling
/// (`RNG_SAMPLE_ENABLE`, bit 0) set; read-only bits are kept.
#[must_use]
pub const fn lp_trng_config(current: u32) -> u32 {
    let prescaler_mask = 0xFF << 1;
    (current & !prescaler_mask) | ((LP_TRNG_PRESCALER as u32) << 1) | (1 << 9) | 1
}

/// A clock the sampling loop measures each byte's folding time with.
pub trait SampleClock {
    /// A point in time.
    type Instant: Copy;

    /// Returns the current time.
    fn now(&self) -> Self::Instant;

    /// Returns whether [`BYTE_SAMPLE_MICROS`] passed since `start`.
    fn sampled_since(&self, start: Self::Instant) -> bool;
}

/// Fills `bytes` the way ESP-IDF's `bootloader_fill_random` does on the
/// ESP32-P4: each byte is the low byte of the exclusive-or of every data
/// register read made over at least [`BYTE_SAMPLE_MICROS`], and of at least
/// two reads.
pub fn fill_bytes<C: SampleClock>(bytes: &mut [u8], mut read: impl FnMut() -> u32, clock: &C) {
    for byte in bytes {
        let start = clock.now();
        let mut random = read();
        loop {
            random ^= read();
            if clock.sampled_since(start) {
                break;
            }
        }
        *byte = (random & 0xFF) as u8;
    }
}
