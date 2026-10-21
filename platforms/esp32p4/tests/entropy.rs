//! ESP-IDF's ESP32-P4 SAR ADC entropy source, as the Platform programs it.

#![allow(missing_docs)]

use std::cell::Cell;

use barracuda_platform_esp32p4::entropy_source::{
    fill_bytes, initial_code_high, initial_code_low, lp_trng_config, pattern_item, pattern_table,
    read_command, read_data, write_command, DataRegister, SampleClock, LP_TRNG_DATA,
    NOISE_ATTENUATION, NOISE_CHANNEL, SAR1_INITIAL_CODE, SAR1_INITIAL_CODE_HIGH,
    SAR1_INITIAL_CODE_LOW, SAR_ADC_ANALOG_SETUP, SAR_DTEST_VDD_GRP1, SAR_ENT_VDD_GRP1,
};

#[test]
fn the_initial_code_is_esp_idfs_2166_split_into_the_two_sar1_registers() {
    // `adc_ll_set_calibration_param(ADC_UNIT_1, 2166)`: 0x876.
    assert_eq!(initial_code_high(SAR1_INITIAL_CODE), 0x08);
    assert_eq!(initial_code_low(SAR1_INITIAL_CODE), 0x76);
    assert_eq!(
        SAR_ADC_ANALOG_SETUP,
        [
            (SAR_DTEST_VDD_GRP1, 0),
            (SAR_ENT_VDD_GRP1, 1),
            (SAR1_INITIAL_CODE_HIGH, 0x08),
            (SAR1_INITIAL_CODE_LOW, 0x76),
        ]
    );
}

#[test]
fn a_masked_write_changes_only_its_field() {
    // ENT_VDD_GRP1 is bit 4 of register 9; DTEST_VDD_GRP1 its bits 3:0.
    assert_eq!(SAR_ENT_VDD_GRP1.apply(0b1110_1111, 1), 0b1111_1111);
    assert_eq!(SAR_ENT_VDD_GRP1.apply(0b1111_1111, 0), 0b1110_1111);
    assert_eq!(SAR_DTEST_VDD_GRP1.apply(0b1111_0101, 0), 0b1111_0000);
    // The high initial code keeps the register's upper nibble.
    assert_eq!(SAR1_INITIAL_CODE_HIGH.apply(0xA0, 0x08), 0xA8);
    assert_eq!(SAR1_INITIAL_CODE_LOW.apply(0xFF, 0x76), 0x76);
    // A value wider than its field is cut to the field.
    assert_eq!(SAR_ENT_VDD_GRP1.apply(0, 0xFF), 0b0001_0000);
}

#[test]
fn analog_bus_commands_carry_slave_address_data_and_the_write_bit() {
    assert_eq!(read_command(0x69, 9), 0x0000_0969);
    assert_eq!(write_command(0x69, 9, 0x10), 0x0110_0969);
    assert_eq!(read_data(0x00AB_0969), 0xAB);
}

#[test]
fn the_pattern_table_samples_internal_channel_10_at_12_db_four_times() {
    let item = pattern_item(NOISE_CHANNEL, NOISE_ATTENUATION);
    assert_eq!(item, 0b10_1011);
    assert_eq!(
        pattern_table(item),
        (0x2B << 18) | (0x2B << 12) | (0x2B << 6) | 0x2B
    );
    assert!(pattern_table(item) <= 0x00FF_FFFF);
}

#[test]
fn revision_3_reads_the_lp_trng_block_and_earlier_chips_the_lp_system_register() {
    assert_eq!(DataRegister::for_major_revision(0), DataRegister::LpSystem);
    assert_eq!(DataRegister::for_major_revision(1), DataRegister::LpSystem);
    assert_eq!(DataRegister::for_major_revision(3), DataRegister::LpTrng);
    assert_eq!(LP_TRNG_DATA, 0x5012_6004);
}

#[test]
fn the_lp_trng_runs_with_esp_idfs_prescaler_timer_and_sampling() {
    // Reset value: prescaler 255, timer on, sampling off.
    let reset = (255 << 1) | (1 << 9);
    assert_eq!(lp_trng_config(reset), reset | 1);
    // A cleared register gets every field, and the sample count is kept.
    assert_eq!(
        lp_trng_config(0x0700_0000),
        0x0700_0000 | (255 << 1) | (1 << 9) | 1
    );
}

/// A clock that advances one tick per reading and reports the byte sampled
/// after `ticks` of them.
struct TickClock {
    now: Cell<u32>,
    ticks: u32,
}

impl SampleClock for TickClock {
    type Instant = u32;

    fn now(&self) -> u32 {
        self.now.get()
    }

    fn sampled_since(&self, start: u32) -> bool {
        self.now.get() - start >= self.ticks
    }
}

#[test]
fn each_byte_folds_every_read_over_its_sample_period() {
    let clock = TickClock {
        now: Cell::new(0),
        ticks: 3,
    };
    let words = [
        0x0000_0001_u32,
        0x0000_0102,
        0x0000_0204,
        0x0000_0008,
        0xFFFF_FF10,
    ];
    let next = Cell::new(0);
    let mut bytes = [0_u8; 1];
    fill_bytes(
        &mut bytes,
        || {
            let word = words[next.get()];
            next.set(next.get() + 1);
            clock.now.set(clock.now.get() + 1);
            word
        },
        &clock,
    );
    // Three reads reach the third tick: 0x001 ^ 0x102 ^ 0x204, low byte only.
    assert_eq!(next.get(), 3);
    assert_eq!(bytes, [0x07]);
}

#[test]
fn even_an_elapsed_period_folds_two_reads_per_byte() {
    let clock = TickClock {
        now: Cell::new(0),
        ticks: 0,
    };
    let reads = Cell::new(0_u32);
    let mut bytes = [0_u8; 5];
    fill_bytes(
        &mut bytes,
        || {
            reads.set(reads.get() + 1);
            reads.get()
        },
        &clock,
    );
    assert_eq!(reads.get(), 10);
    assert_eq!(bytes, [1 ^ 2, 3 ^ 4, 5 ^ 6, 7 ^ 8, 9 ^ 10]);
}
