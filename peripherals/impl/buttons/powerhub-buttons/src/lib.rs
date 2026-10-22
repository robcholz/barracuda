//! The M5Stack PowerHub's user buttons.
//!
//! M5Unified `M5Unified::update` (board_M5PowerHub) reads button A from
//! GPIO11, low while pressed, and button B from bit 0 of the power
//! co-processor's register A0h, low while pressed. This implementation
//! samples both in one [`Buttons::read`], A as index 0 and B as index 1, and
//! debounces them with M5Unified's `Button_Class::setRawState` rule: a new
//! level counts once it has held for the debounce time, or at once when the
//! previous read is older than that.

#![no_std]

use core::marker::PhantomData;

use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, Pull};
use barracuda_driver_m5powerhub::M5PowerHub;
use barracuda_peripheral::{
    PeripheralImplementation,
    buttons::{ButtonSnapshot, Buttons},
};
use embassy_time::Instant;
use embedded_hal::i2c::I2c;

/// Board-owned co-processor address and debounce time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerHubButtonsConfig {
    address: u8,
    debounce_ms: u32,
}

impl PowerHubButtonsConfig {
    /// Creates configuration for the co-processor at `address`.
    #[must_use]
    pub const fn new(address: u8, debounce_ms: u32) -> Self {
        Self {
            address,
            debounce_ms,
        }
    }
}

/// Move-only resources: the co-processor's bus and button A's pin.
pub struct PowerHubButtonsBindings<I2C, P> {
    i2c: I2C,
    button_a: P,
}

impl<I2C, P> PowerHubButtonsBindings<I2C, P> {
    /// Combines the bus and the pin.
    #[must_use]
    pub const fn new(i2c: I2C, button_a: P) -> Self {
        Self { i2c, button_a }
    }
}

/// Button failure.
#[derive(Debug)]
pub enum PowerHubButtonsError<BusError, PinError> {
    /// The address is not a seven-bit address.
    InvalidAddress(u8),
    /// The co-processor could not be read.
    Bus(BusError),
    /// Button A's pin could not be configured or read.
    Pin(PinError),
}

/// Per-button debounce state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Debounce {
    pressed: bool,
    raw: bool,
    raw_since_ms: u64,
}

impl Debounce {
    const fn settled(pressed: bool, now_ms: u64) -> Self {
        Self {
            pressed,
            raw: pressed,
            raw_since_ms: now_ms,
        }
    }

    fn sample(&mut self, raw: bool, now_ms: u64, last_read_ms: u64, debounce_ms: u32) -> bool {
        if raw != self.raw {
            self.raw = raw;
            self.raw_since_ms = now_ms;
        }
        let debounce = u64::from(debounce_ms);
        let idle = now_ms.saturating_sub(last_read_ms) > debounce;
        if idle || now_ms.saturating_sub(self.raw_since_ms) >= debounce {
            self.pressed = raw;
        }
        self.pressed
    }
}

/// Initialized buttons.
pub struct PowerHubButtons<I2C, P> {
    i2c: I2C,
    button_a: P,
    hub: M5PowerHub,
    state: [Debounce; 2],
    debounce_ms: u32,
    last_read_ms: u64,
}

/// Static factory used by generated Board composition.
pub struct PowerHubButtonsImplementation<I2C, P>(PhantomData<fn() -> (I2C, P)>);

impl<I2C, P> PeripheralImplementation for PowerHubButtonsImplementation<I2C, P>
where
    I2C: I2c + 'static,
    P: ConfigurableDigitalPin + 'static,
{
    type Bindings = PowerHubButtonsBindings<I2C, P>;
    type Config = PowerHubButtonsConfig;
    type Peripheral = PowerHubButtons<I2C, P>;
    type Error = PowerHubButtonsError<I2C::Error, P::Error>;

    async fn initialize(
        bindings: Self::Bindings,
        config: Self::Config,
    ) -> Result<Self::Peripheral, Self::Error> {
        let hub = M5PowerHub::new(config.address)
            .ok_or(PowerHubButtonsError::InvalidAddress(config.address))?;
        let PowerHubButtonsBindings {
            mut i2c,
            mut button_a,
        } = bindings;
        // M5Unified sets GPIO11 as a plain input; the Board pulls it up.
        button_a
            .configure_input(InputConfig { pull: Pull::None })
            .map_err(PowerHubButtonsError::Pin)?;
        let raw = raw_states(&hub, &mut i2c, &mut button_a)?;
        let now_ms = Instant::now().as_millis();
        Ok(PowerHubButtons {
            i2c,
            button_a,
            hub,
            state: raw.map(|pressed| Debounce::settled(pressed, now_ms)),
            debounce_ms: config.debounce_ms,
            last_read_ms: now_ms,
        })
    }
}

/// Reads button A's pin and button B's register bit.
fn raw_states<I2C: I2c, P: ConfigurableDigitalPin>(
    hub: &M5PowerHub,
    i2c: &mut I2C,
    button_a: &mut P,
) -> Result<[bool; 2], PowerHubButtonsError<I2C::Error, P::Error>> {
    let a = button_a.is_low().map_err(PowerHubButtonsError::Pin)?;
    let b = hub
        .button_b_pressed(i2c)
        .map_err(PowerHubButtonsError::Bus)?;
    Ok([a, b])
}

impl<I2C: I2c, P: ConfigurableDigitalPin> PowerHubButtons<I2C, P> {
    fn sample_at(
        &mut self,
        now_ms: u64,
    ) -> Result<ButtonSnapshot, PowerHubButtonsError<I2C::Error, P::Error>> {
        let raw = raw_states(&self.hub, &mut self.i2c, &mut self.button_a)?;
        let mut pressed = [false; 2];
        for ((debounce, raw), target) in self.state.iter_mut().zip(raw).zip(&mut pressed) {
            *target = debounce.sample(raw, now_ms, self.last_read_ms, self.debounce_ms);
        }
        self.last_read_ms = now_ms;
        Ok(ButtonSnapshot::new(&pressed))
    }
}

impl<I2C: I2c, P: ConfigurableDigitalPin> Buttons for PowerHubButtons<I2C, P> {
    type Error = PowerHubButtonsError<I2C::Error, P::Error>;

    fn read(&mut self) -> Result<ButtonSnapshot, Self::Error> {
        self.sample_at(Instant::now().as_millis())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use barracuda_board_hal::{ConfigurableDigitalPin, InputConfig, OutputConfig};
    use barracuda_peripheral::PeripheralImplementation;
    use embassy_futures::block_on;
    use embedded_hal::{
        digital::{ErrorType as PinErrorType, InputPin, OutputPin, StatefulOutputPin},
        i2c::{ErrorType, I2c, Operation},
    };

    use super::{PowerHubButtonsBindings, PowerHubButtonsConfig, PowerHubButtonsImplementation};

    /// The co-processor's button register.
    struct Hub(u8);

    impl ErrorType for Hub {
        type Error = Infallible;
    }

    impl I2c for Hub {
        fn transaction(
            &mut self,
            address: u8,
            operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            assert_eq!(address, 0x50);
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => assert_eq!(bytes, &[0xa0]),
                    Operation::Read(values) => values[0] = self.0,
                }
            }
            Ok(())
        }
    }

    /// An input pin at a settable level.
    struct Pin {
        low: bool,
    }

    impl PinErrorType for Pin {
        type Error = Infallible;
    }

    impl InputPin for Pin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.low)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(self.low)
        }
    }

    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl StatefulOutputPin for Pin {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            Ok(false)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    impl ConfigurableDigitalPin for Pin {
        fn configure_input(&mut self, _config: InputConfig) -> Result<(), Self::Error> {
            Ok(())
        }

        fn configure_output(&mut self, _config: OutputConfig) -> Result<(), Self::Error> {
            Ok(())
        }

        fn disable(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn reads_button_a_from_its_pin_and_b_from_bit_0() {
        let mut buttons = block_on(PowerHubButtonsImplementation::initialize(
            PowerHubButtonsBindings::new(Hub(0xff), Pin { low: false }),
            PowerHubButtonsConfig::new(0x50, 10),
        ))
        .expect("start");
        let snapshot = buttons.sample_at(1_000).expect("read");
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot.is_pressed(0), Some(false));
        assert_eq!(snapshot.is_pressed(1), Some(false));

        buttons.button_a.low = true;
        buttons.i2c.0 = 0xfe;
        let snapshot = buttons.sample_at(2_000).expect("read");
        assert_eq!(snapshot.is_pressed(0), Some(true));
        assert_eq!(snapshot.is_pressed(1), Some(true));
    }

    #[test]
    fn a_short_bounce_does_not_count() {
        let mut buttons = block_on(PowerHubButtonsImplementation::initialize(
            PowerHubButtonsBindings::new(Hub(0xff), Pin { low: false }),
            PowerHubButtonsConfig::new(0x50, 10),
        ))
        .expect("start");
        buttons.sample_at(1_000).expect("read");
        buttons.i2c.0 = 0xfe;
        assert_eq!(
            buttons.sample_at(1_002).expect("read").is_pressed(1),
            Some(false)
        );
        assert_eq!(
            buttons.sample_at(1_012).expect("read").is_pressed(1),
            Some(true)
        );
    }
}
