//! The power key of the M5Stack M5PM1 power controller as one button.
//!
//! M5Stack's StickS3, StopWatch, CoreP4X, Paper boards, and StampS3-Bat wire
//! their power key to the M5PM1's PWR_BTN input. The M5PM1 reports the key's
//! level in `BTN_STATUS` (`48h`) bit 0 and records a press in bit 7, which
//! clears when read (M5Stack's M5PM1 library, `btnGetState` and
//! `btnGetFlag`). This implementation reports the key as button 0, pressed
//! while it is down and in the first [`Buttons::read`] after a press too short
//! to fall between two reads. [`Buttons::poll_event`] follows the level
//! through the shared [`ButtonTracker`], without a debounce of its own
//! because the M5PM1 reports a settled level, and reports a press that came
//! and went between two reads as pressed, released, and click.
//!
//! M5Unified `M5PM1_Class::getPekPress` reads the single- and double-click
//! interrupts in `42h` instead. The M5PM1 clears those by itself unless one
//! of its pins is configured as the interrupt output (M5Unified
//! `Power_Class::begin`), and each Board uses its pins for something else, so
//! this implementation leaves the interrupt registers alone.
//!
//! With its factory settings the M5PM1 resets the system on a single click,
//! turns it off on a double click, and enters download mode on a long press
//! (M5PM1 library `M5PM1_REG_BTN_CFG_1` and `M5PM1_REG_BTN_CFG_2`; M5Stack's
//! StickS3 and CoreP4X pages, "Button Operation Instructions").
//! [`SingleClickReset::Disable`] sets `SINGLE_RST_DIS` so a click reaches
//! the application; the double click and the long press keep their factory
//! actions. The M5PM1 keeps the setting while it has power.
//!
//! # Shared ownership
//!
//! Display and battery-monitor implementations use the same M5PM1 through
//! their own views of the bus. This implementation reads only `BTN_STATUS`,
//! writes only the single-click-reset bit of `BTN_CFG_1` when the Board asks
//! for it, and repeats the idempotent housekeeping writes (I2C sleep and
//! watchdog off) the others also make.

#![no_std]

use core::marker::PhantomData;

use barracuda_driver_m5pm1::{Error as M5pm1Error, M5pm1};
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonEvent, ButtonSnapshot, ButtonTiming, ButtonTracker, Buttons},
};
use embassy_time::Instant;
use embedded_hal::i2c::I2c;

/// What a single click of the key does to the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SingleClickReset {
    /// Leave the M5PM1's setting as it is; factory units reset.
    Keep,
    /// Turn the reset off, so the click reaches the application.
    Disable,
}

/// Board-owned address, click behaviour, and long-press time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct M5pm1PowerKeyConfig {
    address: u8,
    single_click_reset: SingleClickReset,
    long_press_ms: u32,
}

impl M5pm1PowerKeyConfig {
    /// Creates configuration for the M5PM1 at `address`.
    #[must_use]
    pub const fn new(
        address: u8,
        single_click_reset: SingleClickReset,
        long_press_ms: u32,
    ) -> Self {
        Self {
            address,
            single_click_reset,
            long_press_ms,
        }
    }
}

/// Move-only resources consumed by the implementation.
pub struct M5pm1PowerKeyBindings<I2C> {
    i2c: I2C,
}

impl<I2C> M5pm1PowerKeyBindings<I2C> {
    /// Wraps this implementation's own view of the M5PM1's bus.
    #[must_use]
    pub const fn new(i2c: I2C) -> Self {
        Self { i2c }
    }
}

/// Initialized power key.
pub struct M5pm1PowerKey<I2C> {
    i2c: I2C,
    address: u8,
    tracker: ButtonTracker,
    /// The key was down at the previous sample.
    down: bool,
    /// The M5PM1 recorded a press that no [`Buttons::read`] has reported.
    unread_press: bool,
}

/// Static factory used by generated Board composition.
pub struct M5pm1PowerKeyImplementation<I2C>(PhantomData<fn() -> I2C>);

impl<I2C> PeripheralImplementation for M5pm1PowerKeyImplementation<I2C>
where
    I2C: I2c + 'static,
{
    type Bindings = M5pm1PowerKeyBindings<I2C>;
    type Config = M5pm1PowerKeyConfig;
    type Peripheral = M5pm1PowerKey<I2C>;
    type Error = M5pm1Error<I2C::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let mut i2c = bindings.i2c;
        let down = {
            let mut pm1 = M5pm1::new(&mut i2c, config.address)?;
            pm1.disable_i2c_sleep()?;
            pm1.disable_watchdog()?;
            if config.single_click_reset == SingleClickReset::Disable {
                pm1.set_single_click_reset(false)?;
            }
            // A press recorded before start, such as the one that switched
            // the Board on, is not a press the application made.
            pm1.button_status()?.pressed
        };
        Ok(M5pm1PowerKey {
            i2c,
            address: config.address,
            tracker: ButtonTracker::new(
                &[down],
                Instant::now().as_millis(),
                ButtonTiming::new(0, config.long_press_ms),
            ),
            down,
            unread_press: false,
        })
    }
}

impl<I2C: I2c> M5pm1PowerKey<I2C> {
    /// Reads the key, feeds the tracker, and returns whether it is down.
    fn sample_at(&mut self, now_ms: u64) -> Result<bool, M5pm1Error<I2C::Error>> {
        let status = M5pm1::new(&mut self.i2c, self.address)?.button_status()?;
        let level = status.pressed;
        if status.pressed_since_last_read {
            self.unread_press = true;
            // The key went down since the previous read, yet its level did
            // not change: a whole press came and went, or the key came up
            // and went down again, between the two reads.
            if self.down == level {
                self.tracker.sample(&[!level], now_ms);
            }
        }
        self.tracker.sample(&[level], now_ms);
        self.down = level;
        Ok(level)
    }

    fn poll_event_at(
        &mut self,
        now_ms: u64,
    ) -> Result<Option<ButtonEvent>, M5pm1Error<I2C::Error>> {
        if !self.tracker.has_events() {
            self.sample_at(now_ms)?;
        }
        Ok(self.tracker.pop_event())
    }
}

impl<I2C: I2c> Buttons for M5pm1PowerKey<I2C> {
    type Error = M5pm1Error<I2C::Error>;

    /// Reports the key as button 0.
    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        let down = self.sample_at(Instant::now().as_millis())?;
        let recorded = core::mem::take(&mut self.unread_press);
        Ok(ButtonSnapshot::new(&[down || recorded]))
    }

    fn poll_event(&mut self) -> Result<Option<ButtonEvent>, Self::Error> {
        self.poll_event_at(Instant::now().as_millis())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_peripheral::{
        PeripheralImplementation,
        buttons::{
            ButtonEventKind::{self, Click, LongPress, Pressed, Released},
            Buttons,
        },
    };
    use embassy_futures::block_on;
    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{
        M5pm1PowerKey, M5pm1PowerKeyBindings, M5pm1PowerKeyConfig, M5pm1PowerKeyImplementation,
        SingleClickReset,
    };

    /// Register file at 6Eh whose `BTN_STATUS` press flag clears when read.
    struct Chip {
        registers: [u8; 0x50],
        writes: std::vec::Vec<(u8, u8)>,
    }

    impl Chip {
        fn new() -> Self {
            let mut registers = [0; 0x50];
            // Factory BTN_CFG_1, and the power-on press still recorded.
            registers[0x49] = 0x2a;
            registers[0x48] = 0x80;
            Self {
                registers,
                writes: std::vec::Vec::new(),
            }
        }
    }

    impl ErrorType for Chip {
        type Error = Infallible;
    }

    impl I2c for Chip {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x6e);
            let mut register = 0;
            for operation in operations {
                match operation {
                    Operation::Write([reg]) => register = *reg,
                    Operation::Write([reg, value]) => {
                        self.registers[usize::from(*reg)] = *value;
                        self.writes.push((*reg, *value));
                    }
                    Operation::Read([value]) => {
                        *value = self.registers[usize::from(register)];
                        if register == 0x48 {
                            self.registers[0x48] &= !0x80;
                        }
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }

    #[test]
    fn follows_the_key_and_catches_a_press_between_reads() {
        let mut key = block_on(M5pm1PowerKeyImplementation::initialize(
            M5pm1PowerKeyBindings::new(Chip::new()),
            M5pm1PowerKeyConfig::new(0x6e, SingleClickReset::Keep, 500),
        ))
        .expect("start");
        // Housekeeping only; the button configuration is left as it was.
        assert_eq!(key.i2c.writes, [(0x09, 0x00), (0x0a, 0x00)]);
        let snapshot = key.read().expect("read");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot.is_pressed(0), Some(false));

        // Held down across reads.
        key.i2c.registers[0x48] = 0x81;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        key.i2c.registers[0x48] = 0x00;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));

        // Pressed and released between two reads.
        key.i2c.registers[0x48] = 0x80;
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
    }

    #[test]
    fn disabling_the_click_reset_sets_only_bit_0() {
        let key = block_on(M5pm1PowerKeyImplementation::initialize(
            M5pm1PowerKeyBindings::new(Chip::new()),
            M5pm1PowerKeyConfig::new(0x6e, SingleClickReset::Disable, 500),
        ))
        .expect("start");
        assert_eq!(key.i2c.registers[0x49], 0x2b);
        assert_eq!(key.i2c.writes, [(0x09, 0x00), (0x0a, 0x00), (0x49, 0x2b)]);
    }

    fn drain(
        key: &mut M5pm1PowerKey<Chip>,
        now: u64,
        events: &mut std::vec::Vec<(usize, ButtonEventKind)>,
    ) {
        while let Some(event) = key.poll_event_at(now).expect("poll") {
            events.push((event.index(), event.kind()));
        }
    }

    #[test]
    fn events_follow_the_level_and_catch_a_press_between_reads() {
        let mut key = block_on(M5pm1PowerKeyImplementation::initialize(
            M5pm1PowerKeyBindings::new(Chip::new()),
            M5pm1PowerKeyConfig::new(0x6e, SingleClickReset::Disable, 500),
        ))
        .expect("start");
        let start = embassy_time::Instant::now().as_millis() + 100;
        let mut events = std::vec::Vec::new();
        // The power-on press raises nothing.
        drain(&mut key, start, &mut events);
        assert_eq!(events, []);

        // Held 600 ms, polled every 100 ms.
        key.i2c.registers[0x48] = 0x81;
        for step in 0..=6 {
            drain(&mut key, start + 100 + 100 * step, &mut events);
        }
        key.i2c.registers[0x48] = 0x00;
        drain(&mut key, start + 800, &mut events);
        assert_eq!(events, [(0, Pressed), (0, LongPress), (0, Released)]);
        events.clear();

        // Clicked between two polls: only the press flag tells.
        key.i2c.registers[0x48] = 0x80;
        drain(&mut key, start + 900, &mut events);
        assert_eq!(events, [(0, Pressed), (0, Released), (0, Click)]);
        // The next read still reports that press once.
        assert_eq!(key.read().expect("read").is_pressed(0), Some(true));
        assert_eq!(key.read().expect("read").is_pressed(0), Some(false));
    }
}
