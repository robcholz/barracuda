//! Epson RX8130CE real-time clock module.
//!
//! Source: Epson application manual ETM50E-10, "Real Time Clock Module
//! RX8130CE". Section, table, and figure numbers below refer to that
//! revision.
//!
//! Modelled: the user register set (13.2.1 Table 12) with its power-on values
//! (13.2.2 Table 13) and fixed-zero bits, address auto-increment and its
//! circulation (19.6), the clock and calendar counters with the STOP bit
//! (13.3, 18.5) and the year-2000–2099 leap rule (13.1), and the VLF flag
//! (14.5 Table 31). Not modelled: alarms, the wakeup timer, the update
//! interrupt, FOUT, battery switchover, the digital offset, and the 0.95 s
//! bus timeout (a virtual transaction is instantaneous).
//!
//! Backdoor layout: byte offset equals the register address (10h–3Fh).
//! Registers Table 13 marks undefined (X) start at zero.

use std::time::Duration;

use crate::device::{DataNack, DeviceContext, I2cDevice, RegisterRangeError};

const SECONDS: u8 = 0x10;
const YEAR: u8 = 0x16;
const WEEK: u8 = 0x13;
const FLAG: u8 = 0x1d;
const CONTROL0: u8 = 0x1e;
const REGISTER_END: usize = 0x40;

/// 14.5 Table 31: VLF, bit 1 of the Flag Register.
const VLF: u8 = 1 << 1;
/// 13.2.1 Table 12: STOP, bit 6 of Control Register0.
const STOP: u8 = 1 << 6;
/// 13.2.1 Table 12: TEST, bit 7 of Control Register0.
const TEST: u8 = 1 << 7;

/// 10.1 Figure 14: VLF can be read 30 ms after the initial power-on.
const POWER_ON_ACCESS_WAIT: Duration = Duration::from_millis(30);
/// Table 4: oscillation start time t_str, maximum 1.0 s.
const OSCILLATION_START_MAX: Duration = Duration::from_secs(1);

/// 13.2.1 Table 12: bits that always read 0 ("writing is invalid").
const fn writable_mask(register: u8) -> u8 {
    match register {
        0x10 | 0x11 | 0x13 => 0x7f,
        0x12 | 0x14 => 0x3f,
        0x15 => 0x1f,
        // Flag Register bit 6 is fixed at 0.
        0x1d => 0xbf,
        _ => 0xff,
    }
}

/// 13.2.1: "Please make sure to only access above mentioned user registers."
const fn is_user_register(register: u8) -> bool {
    matches!(register, 0x10..=0x23 | 0x30 | 0x31)
}

/// Registers that 14.5 requires to be initialized after VLF = 1: every user
/// register except the free RAM (20h–23h). See [`rules::INITIALIZE`].
const fn needs_initialization(register: u8) -> bool {
    matches!(register, 0x10..=0x1f | 0x30 | 0x31)
}

/// Rule identifiers reported by this model.
pub mod rules {
    /// 10.1 Figure 14: no access until 30 ms after the initial power-on.
    pub const POWER_ON_ACCESS: &str = "RX8130CE ETM50E-10 10.1 Figure 14 power-on access wait";
    /// 10.1 Figure 14 and Table 4: with VLF = 1, initial settings wait for
    /// the oscillation start time t_str (max 1.0 s).
    pub const OSCILLATION_START: &str =
        "RX8130CE ETM50E-10 10.1 Figure 14 initial setting after t_str";
    /// 13.2.1: only the listed user registers may be accessed.
    pub const USER_REGISTERS: &str = "RX8130CE ETM50E-10 13.2.1 user registers only";
    /// 13.2.1: the TEST bit must be written as 0.
    pub const TEST_BIT: &str = "RX8130CE ETM50E-10 13.2.1 TEST bit written as 0";
    /// 13.2.1: incorrect date and time data must not be entered.
    pub const DATE_TIME: &str = "RX8130CE ETM50E-10 13.2.1 valid date and time";
    /// 13.2.1 and 14.5: after the initial power-on or VLF = 1, initialize all
    /// registers before use. The model reads "all registers" as every user
    /// register except RAM (10h–1Fh, 30h, 31h) and "before use" as before
    /// VLF is cleared; both readings are interpretations of the manual.
    pub const INITIALIZE: &str = "RX8130CE ETM50E-10 14.5 initialize all registers after VLF";
}

/// Behavioural model of one RX8130CE.
#[derive(Clone, Debug)]
pub struct Rx8130ceModel {
    registers: [u8; REGISTER_END],
    pointer: u8,
    powered_at: Duration,
    /// Start of the current counting second while the clock runs.
    counting_since: Option<Duration>,
    /// Registers written since VLF was last set.
    initialized: [bool; REGISTER_END],
    wrote_clock: bool,
    /// Whether this attach is the initial power-on (from 0 V) rather than a
    /// device that kept time on its backup supply.
    initial_power_on: bool,
    power_on_reported: bool,
}

impl Rx8130ceModel {
    /// Creates a device at its initial power-on (Table 13: VLF = 1).
    #[must_use]
    pub const fn new() -> Self {
        let mut registers = [0; REGISTER_END];
        registers[0x1c] = 0x04;
        registers[0x1d] = 0x06;
        Self {
            registers,
            pointer: SECONDS,
            powered_at: Duration::ZERO,
            counting_since: None,
            initialized: [false; REGISTER_END],
            wrote_clock: false,
            initial_power_on: true,
            power_on_reported: false,
        }
    }

    /// Creates a device whose clock was set and kept: VLF = 0 and the clock
    /// running from `clock` (`[sec, min, hour, week, day, month, year]` BCD).
    #[must_use]
    pub const fn running(clock: [u8; 7]) -> Self {
        let mut model = Self::new();
        let mut index = 0;
        while index < 7 {
            model.registers[SECONDS as usize + index] = clock[index];
            index += 1;
        }
        model.registers[FLAG as usize] &= !VLF;
        model.initialized = [true; REGISTER_END];
        model.initial_power_on = false;
        model
    }

    fn stopped(&self) -> bool {
        self.registers[usize::from(CONTROL0)] & STOP != 0
    }

    fn vlf(&self) -> bool {
        self.registers[usize::from(FLAG)] & VLF != 0
    }

    /// Advances the clock registers to `now` (13.3, 18.5).
    fn advance(&mut self, now: Duration) {
        let Some(since) = self.counting_since else {
            return;
        };
        let elapsed = now.saturating_sub(since).as_secs();
        if elapsed == 0 {
            return;
        }
        if let Some(clock) = Calendar::decode(&self.registers) {
            clock.add_seconds(elapsed).encode(&mut self.registers);
        }
        self.counting_since = Some(since + Duration::from_secs(elapsed));
    }

    /// Starts counting at `now` unless STOP is set. Oscillation must have
    /// started (Table 4 t_str) for the counters to update (10.1 Figure 14).
    fn restart(&mut self, now: Duration) {
        let oscillating = if self.initial_power_on {
            self.powered_at + OSCILLATION_START_MAX
        } else {
            self.powered_at
        };
        self.counting_since = (!self.stopped()).then(|| now.max(oscillating));
    }

    fn next(pointer: u8) -> u8 {
        // 19.6: 10h->1Fh->10h, 20h->2Fh->20h, 30h->3Fh->30h.
        (pointer & 0xf0) | (pointer.wrapping_add(1) & 0x0f)
    }

    fn check_access(&self, context: &mut DeviceContext<'_>, register: u8, write: bool) {
        if !is_user_register(register) {
            let kind = if write { "write" } else { "read" };
            context.violation(
                rules::USER_REGISTERS,
                format!("{kind} of register {register:#04x}, which is not a user register"),
            );
        }
    }

    fn store(&mut self, context: &mut DeviceContext<'_>, register: u8, value: u8) {
        self.check_access(context, register, true);
        if usize::from(register) >= REGISTER_END {
            return;
        }
        if self.initial_power_on
            && self.vlf()
            && context.now() < self.powered_at + OSCILLATION_START_MAX
        {
            context.violation(
                rules::OSCILLATION_START,
                format!(
                    "register {register:#04x} written {} ms after power-on while VLF = 1; \
                     initial settings wait for t_str (max {} ms)",
                    (context.now() - self.powered_at).as_millis(),
                    OSCILLATION_START_MAX.as_millis()
                ),
            );
        }
        let value = value & writable_mask(register);
        match register {
            CONTROL0 => {
                if value & TEST != 0 {
                    context.violation(
                        rules::TEST_BIT,
                        format!("Control Register0 written as {value:#04x}"),
                    );
                }
                let was_stopped = self.stopped();
                self.registers[usize::from(register)] = value;
                if was_stopped != self.stopped() {
                    // 18.5: writing STOP = 0 starts the clock at that time.
                    self.restart(context.now());
                }
            }
            FLAG => {
                // 14.5 Table 31: writing 0 clears VLF, writing 1 is ignored.
                let vlf = self.registers[usize::from(FLAG)] & VLF;
                let kept = if value & VLF == 0 { 0 } else { vlf };
                if vlf != 0 && kept == 0 {
                    let missing: Vec<String> = (0..REGISTER_END)
                        .filter_map(|index| u8::try_from(index).ok())
                        .filter(|register| {
                            needs_initialization(*register)
                                && *register != FLAG
                                && !self.initialized[usize::from(*register)]
                        })
                        .map(|register| format!("{register:02x}h"))
                        .collect();
                    if !missing.is_empty() {
                        context.violation(
                            rules::INITIALIZE,
                            format!("VLF cleared before initializing {}", missing.join(", ")),
                        );
                    }
                }
                self.registers[usize::from(FLAG)] = (value & !VLF) | kept;
            }
            SECONDS..=YEAR => {
                self.registers[usize::from(register)] = value;
                self.wrote_clock = true;
                // 18.5: without STOP the clock starts when the time is written.
                self.restart(context.now());
            }
            _ => self.registers[usize::from(register)] = value,
        }
        if let Some(initialized) = self.initialized.get_mut(usize::from(register)) {
            *initialized = true;
        }
    }
}

impl Default for Rx8130ceModel {
    fn default() -> Self {
        Self::new()
    }
}

impl I2cDevice for Rx8130ceModel {
    fn model(&self) -> &'static str {
        "rx8130ce"
    }

    fn attached(&mut self, context: &mut DeviceContext<'_>) {
        self.powered_at = context.now();
        if self.vlf() {
            self.counting_since = None;
        } else {
            self.restart(context.now());
        }
    }

    fn write(&mut self, context: &mut DeviceContext<'_>, bytes: &[u8]) -> Result<(), DataNack> {
        self.check_power_on(context);
        self.advance(context.now());
        let Some((&pointer, data)) = bytes.split_first() else {
            return Ok(());
        };
        self.pointer = pointer;
        for &value in data {
            let register = self.pointer;
            self.store(context, register, value);
            self.pointer = Self::next(self.pointer);
        }
        Ok(())
    }

    fn read(&mut self, context: &mut DeviceContext<'_>, buffer: &mut [u8]) {
        self.check_power_on(context);
        self.advance(context.now());
        for byte in buffer {
            self.check_access(context, self.pointer, false);
            *byte = self
                .registers
                .get(usize::from(self.pointer))
                .copied()
                .unwrap_or(0);
            self.pointer = Self::next(self.pointer);
        }
    }

    fn stop(&mut self, context: &mut DeviceContext<'_>) {
        self.power_on_reported = false;
        if std::mem::take(&mut self.wrote_clock) && Calendar::decode(&self.registers).is_none() {
            let clock = &self.registers[usize::from(SECONDS)..=usize::from(YEAR)];
            context.violation(
                rules::DATE_TIME,
                format!("clock registers 10h–16h hold invalid data {clock:02x?}"),
            );
        }
    }

    fn peek(&self, offset: usize, buffer: &mut [u8]) -> Result<(), RegisterRangeError> {
        let source = crate::device::register_range(&self.registers, offset, buffer.len())?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn poke(&mut self, offset: usize, bytes: &[u8]) -> Result<(), RegisterRangeError> {
        let length = bytes.len();
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= REGISTER_END && offset >= usize::from(SECONDS))
            .ok_or(RegisterRangeError { offset, length })?;
        self.registers[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

impl Rx8130ceModel {
    /// Reports at most one early access per transaction.
    fn check_power_on(&mut self, context: &mut DeviceContext<'_>) {
        let since = context.now().saturating_sub(self.powered_at);
        if self.initial_power_on && since < POWER_ON_ACCESS_WAIT && !self.power_on_reported {
            self.power_on_reported = true;
            context.violation(
                rules::POWER_ON_ACCESS,
                format!(
                    "access {} µs after power-on; wait {} ms",
                    since.as_micros(),
                    POWER_ON_ACCESS_WAIT.as_millis()
                ),
            );
        }
    }
}

/// Decoded clock and calendar registers 10h–16h.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Calendar {
    seconds_since_2000: u64,
    weekday: u32,
}

impl Calendar {
    fn decode(registers: &[u8; REGISTER_END]) -> Option<Self> {
        let field = |register: u8| bcd(registers[usize::from(register)]);
        let second = field(0x10)?;
        let minute = field(0x11)?;
        let hour = field(0x12)?;
        let week = registers[usize::from(WEEK)];
        let day = field(0x14)?;
        let month = field(0x15)?;
        let year = field(0x16)?;
        if second > 59
            || minute > 59
            || hour > 23
            || week.count_ones() != 1
            || week & 0x80 != 0
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
        {
            return None;
        }
        let days = (0..year).map(days_in_year).sum::<u64>()
            + (1..month)
                .map(|m| u64::from(days_in_month(year, m)))
                .sum::<u64>()
            + u64::from(day - 1);
        Some(Self {
            seconds_since_2000: ((days * 24 + u64::from(hour)) * 60 + u64::from(minute)) * 60
                + u64::from(second),
            weekday: week.trailing_zeros(),
        })
    }

    fn add_seconds(self, seconds: u64) -> Self {
        let days_before = self.seconds_since_2000 / 86_400;
        let seconds_since_2000 = self.seconds_since_2000 + seconds;
        let days_after = seconds_since_2000 / 86_400;
        Self {
            seconds_since_2000,
            weekday: ((u64::from(self.weekday) + days_after - days_before) % 7) as u32,
        }
    }

    fn encode(self, registers: &mut [u8; REGISTER_END]) {
        let mut days = self.seconds_since_2000 / 86_400;
        let mut rest = self.seconds_since_2000 % 86_400;
        let mut year = 0;
        // 13.1: the calendar runs to 2099 and wraps the two-digit year.
        while days >= days_in_year(year) {
            days -= days_in_year(year);
            year = (year + 1) % 100;
        }
        let mut month = 1;
        while days >= u64::from(days_in_month(year, month)) {
            days -= u64::from(days_in_month(year, month));
            month += 1;
        }
        let second = rest % 60;
        rest /= 60;
        let values = [
            to_bcd(second as u8),
            to_bcd((rest % 60) as u8),
            to_bcd((rest / 60) as u8),
            1 << self.weekday,
            to_bcd(days as u8 + 1),
            to_bcd(month),
            to_bcd(year),
        ];
        registers[usize::from(SECONDS)..=usize::from(YEAR)].copy_from_slice(&values);
    }
}

/// 13.1: a two-digit year that is a multiple of 4 is a leap year.
const fn days_in_year(year: u8) -> u64 {
    if year.is_multiple_of(4) {
        366
    } else {
        365
    }
}

const fn days_in_month(year: u8, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const fn bcd(value: u8) -> Option<u8> {
    let (high, low) = (value >> 4, value & 0x0f);
    if high > 9 || low > 9 {
        None
    } else {
        Some(high * 10 + low)
    }
}

const fn to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use embedded_hal::i2c::I2c as _;

    use super::*;
    use crate::{Clock, VirtualHardware};

    const ADDRESS: u8 = 0x32;
    /// 14.1 Table 14 example: Sun, 29-Feb-88 17:39:45 (but year 2088).
    const EXAMPLE: [u8; 7] = [0x45, 0x39, 0x17, 0x01, 0x29, 0x02, 0x88];

    fn setup(model: Rx8130ceModel) -> (VirtualHardware, crate::VirtualI2cBus) {
        let hardware = VirtualHardware::new(&[], &["I2C0"], Clock::manual());
        hardware
            .attach("I2C0", u64::from(ADDRESS), Box::new(model))
            .expect("attach");
        (hardware.clone(), hardware.i2c_bus("I2C0").expect("bus"))
    }

    fn rules(hardware: &VirtualHardware) -> Vec<String> {
        hardware
            .violations()
            .into_iter()
            .map(|violation| violation.violation.rule)
            .collect()
    }

    #[test]
    fn the_running_clock_carries_into_the_calendar() {
        let (hardware, mut bus) = setup(Rx8130ceModel::running(EXAMPLE));
        hardware
            .clock()
            .advance(Duration::from_secs(6 * 3600 + 20 * 60 + 15));
        let mut clock = [0; 7];
        bus.write_read(ADDRESS, &[SECONDS], &mut clock)
            .expect("read");
        // 17:39:45 + 6:20:15 = next day 00:00:00, Monday 1 March 2088.
        assert_eq!(clock, [0x00, 0x00, 0x00, 0x02, 0x01, 0x03, 0x88]);
        assert!(rules(&hardware).is_empty(), "{:?}", hardware.violations());
    }

    #[test]
    fn auto_increment_circulates_within_the_register_block() {
        let (hardware, mut bus) = setup(Rx8130ceModel::running(EXAMPLE));
        hardware.clock().advance(Duration::from_millis(30));
        bus.write(ADDRESS, &[0x23, 0xaa, 0xbb]).expect("write RAM");
        assert_eq!(
            hardware
                .read_registers("I2C0", 0x32, 0x23, 1)
                .expect("peek"),
            [0xaa]
        );
        assert_eq!(
            rules(&hardware),
            [rules::USER_REGISTERS],
            "24h is not a user register"
        );
    }

    #[test]
    fn stop_holds_the_clock_and_clearing_it_restarts_counting() {
        let (hardware, mut bus) = setup(Rx8130ceModel::running(EXAMPLE));
        hardware.clock().advance(Duration::from_millis(500));
        bus.write(ADDRESS, &[CONTROL0, STOP]).expect("stop");
        hardware.clock().advance(Duration::from_secs(10));
        bus.write(ADDRESS, &[CONTROL0, 0]).expect("run");
        hardware.clock().advance(Duration::from_millis(999));
        let mut second = [0];
        bus.write_read(ADDRESS, &[SECONDS], &mut second)
            .expect("read");
        assert_eq!(second, [0x45]);
        hardware.clock().advance(Duration::from_millis(1));
        bus.write_read(ADDRESS, &[SECONDS], &mut second)
            .expect("read");
        assert_eq!(second, [0x46]);
    }

    #[test]
    fn power_on_rules_follow_figure_14() {
        let (hardware, mut bus) = setup(Rx8130ceModel::new());
        let mut flag = [0];
        bus.write_read(ADDRESS, &[FLAG], &mut flag)
            .expect("early read");
        assert_eq!(flag, [0x06], "Table 13: VLF and RSF are set at power-on");
        hardware.clock().advance(Duration::from_millis(30));
        bus.write(ADDRESS, &[CONTROL0, 0]).expect("early setting");
        hardware.clock().advance(Duration::from_millis(970));
        bus.write(ADDRESS, &[0x30, 0, 0])
            .expect("setting after t_str");
        assert_eq!(
            rules(&hardware),
            [rules::POWER_ON_ACCESS, rules::OSCILLATION_START]
        );
    }

    #[test]
    fn data_rules_cover_test_bit_dates_and_initialization() {
        let (hardware, mut bus) = setup(Rx8130ceModel::new());
        hardware.clock().advance(Duration::from_secs(1));
        bus.write(ADDRESS, &[CONTROL0, TEST]).expect("test bit");
        bus.write(ADDRESS, &[SECONDS, 0x60])
            .expect("invalid second");
        bus.write(ADDRESS, &[FLAG, 0]).expect("clear VLF early");
        assert_eq!(
            rules(&hardware),
            [rules::TEST_BIT, rules::DATE_TIME, rules::INITIALIZE]
        );
        let mut flag = [0];
        bus.write_read(ADDRESS, &[FLAG], &mut flag).expect("read");
        assert_eq!(flag[0] & VLF, 0);
        bus.write(ADDRESS, &[FLAG, VLF]).expect("write 1");
        bus.write_read(ADDRESS, &[FLAG], &mut flag).expect("read");
        assert_eq!(flag[0] & VLF, 0, "Table 31: writing 1 is ignored");
    }

    #[test]
    fn a_full_initialization_satisfies_every_rule() {
        let (hardware, mut bus) = setup(Rx8130ceModel::new());
        hardware.clock().advance(Duration::from_secs(1));
        let mut block = vec![SECONDS];
        block.extend_from_slice(&EXAMPLE);
        block.extend_from_slice(&[0; 6]); // 17h–1Ch alarms, timer, extension
        bus.write(ADDRESS, &block).expect("clock and alarms");
        bus.write(ADDRESS, &[0x1e, 0, 0]).expect("controls");
        bus.write(ADDRESS, &[0x30, 0, 0])
            .expect("offset and extension 1");
        bus.write(ADDRESS, &[FLAG, 0]).expect("clear VLF");
        assert!(rules(&hardware).is_empty(), "{:?}", hardware.violations());
    }
}
