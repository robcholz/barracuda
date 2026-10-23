//! Low-level Driver for the M5Stack M5PM1 power-management controller.
//!
//! The M5PM1 is the microcontroller M5Stack's "Stamp Timer Power2" uses for
//! charging, power rails, and wake sources. Registers and bits follow
//! M5Unified's `M5PM1_Class` (src/utility/power/M5PM1_Class.inl, MIT
//! license). This driver covers the rail and housekeeping registers, the
//! five general-purpose pins G0 to G4 with their PWM function, which M5Stack's
//! Paper boards use for the e-paper supply and the front light, and the
//! battery voltage and power-source readings; the charger and wake functions
//! are left out until a consumer needs them.
//!
//! | Register | Address |
//! | --- | --- |
//! | `PWR_SRC` (bit 0 VIN, bit 1 5 V in/out, bit 2 battery) | `04h` |
//! | `PWR_CFG` (bit 1 DC/DC, bit 2 LDO, bit 3 boost) | `06h` |
//! | `I2C_CFG` | `09h` |
//! | `WDT_CNT` | `0Ah` |
//! | `GPIO_MODE` (1 = output) | `10h` |
//! | `GPIO_OUT` | `11h` |
//! | `GPIO_IN` | `12h` |
//! | `GPIO_DRV` (1 = open drain) | `13h` |
//! | `GPIO_PUPD0` (G0..G3) / `GPIO_PUPD1` (G4), two bits per pin | `14h`/`15h` |
//! | `GPIO_FUNC0` (G0..G3) / `GPIO_FUNC1` (G4), two bits per pin | `16h`/`17h` |
//! | `VBAT` (millivolts, little-endian) | `22h`/`23h` |
//! | `PWMn` (12-bit, bit 12 enable, bit 13 inverted) | `30h` + 2n |
//! | `PWM_FREQ` (Hz, shared) | `34h`/`35h` |
//! | `BTN_STATUS` (bit 7 pressed since read, bit 0 pressed; read-only) | `48h` |
//! | `BTN_CFG_1` (bit 0 single-click reset disabled) | `49h` |
//!
//! The power key's registers come from M5Stack's M5PM1 library (`M5PM1.h`,
//! `M5PM1_REG_BTN_STATUS`, `M5PM1_REG_BTN_CFG_1`) and the M5PM1 user manual
//! (v1.9, Button Configuration): `BTN_STATUS` bit 0 is the key's level and
//! bit 7 records a press and clears when read. `BTN_CFG_1` also holds the
//! download-mode lock and the click, double-click, and long-press times,
//! which this driver leaves as they are.

#![no_std]

use embedded_hal::i2c::I2c;

/// Factory I2C address.
pub const DEFAULT_ADDRESS: u8 = 0x6E;

const POWER_SOURCE: u8 = 0x04;
const POWER_CONFIG: u8 = 0x06;
const I2C_CONFIG: u8 = 0x09;
const WATCHDOG_COUNT: u8 = 0x0A;
const GPIO_MODE: u8 = 0x10;
const GPIO_OUTPUT: u8 = 0x11;
const GPIO_INPUT: u8 = 0x12;
const GPIO_DRIVE: u8 = 0x13;
const GPIO_PULL_LOW: u8 = 0x14;
const GPIO_PULL_HIGH: u8 = 0x15;
const BATTERY_VOLTAGE: u8 = 0x22;
const GPIO_FUNCTION_LOW: u8 = 0x16;
const GPIO_FUNCTION_HIGH: u8 = 0x17;
const PWM0_DUTY: u8 = 0x30;
const PWM_FREQUENCY: u8 = 0x34;
const BUTTON_STATUS: u8 = 0x48;
const BUTTON_CONFIG_1: u8 = 0x49;
/// `BTN_STATUS` bit 0, `BTN_STATE`: the key is down.
const BUTTON_DOWN: u8 = 1 << 0;
/// `BTN_STATUS` bit 7, `BTN_FLAG`: pressed since the last read.
const BUTTON_FLAG: u8 = 1 << 7;
/// `BTN_CFG_1` bit 0, `SINGLE_RST_DIS`: a single click does not reset.
const SINGLE_CLICK_RESET_DISABLED: u8 = 1 << 0;

/// Number of general-purpose pins, G0 to G4.
pub const GPIO_COUNT: u8 = 5;
/// Highest 12-bit PWM duty.
pub const MAX_DUTY: u16 = 0x0FFF;
/// PWM enable bit in a duty register's high byte.
const PWM_ENABLE: u8 = 1 << 4;
/// PWM output-inversion bit in a duty register's high byte.
const PWM_INVERTED: u8 = 1 << 5;

/// Function of one general-purpose pin (two bits in `GPIO_FUNC0/1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinFunction {
    /// Plain input or output.
    Gpio = 0b00,
    /// The pin's special function; PWM on the PWM-capable pins.
    Special = 0b11,
}

/// One of the two PWM channels and the pin it drives (M5Unified
/// `pwm_channel_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PwmChannel {
    /// Channel 0 on G3.
    Ch0,
    /// Channel 1 on G4.
    Ch1,
}

impl PwmChannel {
    const fn duty_register(self) -> u8 {
        // Each channel's duty is a low/high byte pair after PWM0's.
        match self {
            Self::Ch0 => PWM0_DUTY,
            Self::Ch1 => 0x32,
        }
    }

    /// Returns the `G` pin number the channel drives.
    #[must_use]
    pub const fn pin(self) -> u8 {
        match self {
            Self::Ch0 => 3,
            Self::Ch1 => 4,
        }
    }
}

/// Bias of one general-purpose input (two bits in `GPIO_PUPD0/1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// No bias.
    None = 0b00,
    /// Pull-up.
    Up = 0b01,
    /// Pull-down.
    Down = 0b10,
}

/// The supplies present, from `PWR_SRC` (M5Unified `pwr_src_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerSource(u8);

impl PowerSource {
    /// Whether external power reaches the VIN input.
    #[must_use]
    pub const fn vin(self) -> bool {
        self.0 & 0b001 != 0
    }

    /// Whether external power reaches the 5 V input/output.
    #[must_use]
    pub const fn five_volt(self) -> bool {
        self.0 & 0b010 != 0
    }

    /// Whether the battery node is powered.
    #[must_use]
    pub const fn battery(self) -> bool {
        self.0 & 0b100 != 0
    }
}

/// The power key's state from `BTN_STATUS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonStatus {
    /// The key is down now (`BTN_STATE`).
    pub pressed: bool,
    /// The key went down since the register was last read (`BTN_FLAG`).
    pub pressed_since_last_read: bool,
}

/// One power rail switched in the power-configuration register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rail {
    /// The DC/DC converter.
    DcDc,
    /// The LDO.
    Ldo,
    /// The 5 V boost converter.
    Boost,
}

impl Rail {
    const fn mask(self) -> u8 {
        match self {
            Self::DcDc => 1 << 1,
            Self::Ldo => 1 << 2,
            Self::Boost => 1 << 3,
        }
    }
}

/// M5PM1 register or configuration failure.
#[derive(Debug)]
pub enum Error<BusError> {
    /// The address is outside the seven-bit I2C range.
    InvalidAddress,
    /// The pin is outside G0 to G4.
    InvalidPin,
    /// The duty exceeds [`MAX_DUTY`].
    InvalidDuty,
    /// An I2C register operation failed.
    Bus(BusError),
}

/// Borrowed access to one M5PM1 on a shared I2C bus.
pub struct M5pm1<'a, I2C> {
    i2c: &'a mut I2C,
    address: u8,
}

impl<'a, I2C> M5pm1<'a, I2C>
where
    I2C: I2c,
{
    /// Borrows one addressed controller.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAddress`] for a non-seven-bit address.
    pub fn new(i2c: &'a mut I2C, address: u8) -> Result<Self, Error<I2C::Error>> {
        if address > 0x7f {
            return Err(Error::InvalidAddress);
        }
        Ok(Self { i2c, address })
    }

    /// Keeps the controller's I2C interface awake, as M5Unified does before
    /// using it.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable_i2c_sleep(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(I2C_CONFIG, 0x00)
    }

    /// Stops the controller's watchdog.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn disable_watchdog(&mut self) -> Result<(), Error<I2C::Error>> {
        self.write(WATCHDOG_COUNT, 0x00)
    }

    /// Switches one rail on or off without touching the others.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_rail_enabled(&mut self, rail: Rail, enabled: bool) -> Result<(), Error<I2C::Error>> {
        let value = self.read(POWER_CONFIG)?;
        let mask = rail.mask();
        self.write(
            POWER_CONFIG,
            if enabled { value | mask } else { value & !mask },
        )
    }

    /// Selects the function of pin `g` (0 to 4).
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_pin_function(
        &mut self,
        g: u8,
        function: PinFunction,
    ) -> Result<(), Error<I2C::Error>> {
        if g >= GPIO_COUNT {
            return Err(Error::InvalidPin);
        }
        let (register, shift) = if g < 4 {
            (GPIO_FUNCTION_LOW, g.wrapping_mul(2))
        } else {
            (GPIO_FUNCTION_HIGH, 0)
        };
        let mask = 0b11_u8 << shift;
        let bits = (function as u8) << shift;
        self.update(register, |value| (value & !mask) | bits)
    }

    /// Configures `G<g>` (0 to 4) as a push-pull GPIO output driving
    /// `initial_high`. The level is latched before the pin starts driving.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_output(&mut self, g: u8, initial_high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.set_pin_function(g, PinFunction::Gpio)?;
        self.update(GPIO_OUTPUT, |value| set_bits(value, mask, initial_high))?;
        self.update(GPIO_DRIVE, |value| value & !mask)?;
        self.update(GPIO_MODE, |value| value | mask)
    }

    /// Configures `G<g>` (0 to 4) as a GPIO input with `pull`.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn configure_input(&mut self, g: u8, pull: Pull) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.set_pin_function(g, PinFunction::Gpio)?;
        let (register, shift) = if g < 4 {
            (GPIO_PULL_LOW, g.wrapping_mul(2))
        } else {
            (GPIO_PULL_HIGH, 0)
        };
        let field = 0b11_u8 << shift;
        let bits = (pull as u8) << shift;
        self.update(register, |value| (value & !field) | bits)?;
        self.update(GPIO_MODE, |value| value & !mask)
    }

    /// Reads the level of `G<g>` (0 to 4).
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn input_is_high(&mut self, g: u8) -> Result<bool, Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        Ok(self.read(GPIO_INPUT)? & mask != 0)
    }

    /// Reads the battery voltage in millivolts.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn battery_millivolts(&mut self) -> Result<u16, Error<I2C::Error>> {
        let mut bytes = [0; 2];
        self.i2c
            .write_read(self.address, &[BATTERY_VOLTAGE], &mut bytes)
            .map_err(Error::Bus)?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Reads which supplies are present.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn power_source(&mut self) -> Result<PowerSource, Error<I2C::Error>> {
        Ok(PowerSource(self.read(POWER_SOURCE)? & 0b111))
    }

    /// Drives a previously configured `G<g>` (0 to 4) output.
    ///
    /// # Errors
    ///
    /// Returns an invalid-pin or I2C register error.
    pub fn set_output(&mut self, g: u8, high: bool) -> Result<(), Error<I2C::Error>> {
        let mask = pin_mask(g)?;
        self.update(GPIO_OUTPUT, |value| set_bits(value, mask, high))
    }

    /// Hands the channel's pin to its PWM function with a push-pull driver.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn configure_pwm_output(&mut self, channel: PwmChannel) -> Result<(), Error<I2C::Error>> {
        let g = channel.pin();
        let mask = pin_mask(g)?;
        self.update(GPIO_DRIVE, |value| value & !mask)?;
        self.set_pin_function(g, PinFunction::Special)
    }

    /// Sets the frequency shared by both PWM channels.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_pwm_frequency(&mut self, hertz: u16) -> Result<(), Error<I2C::Error>> {
        let [low, high] = hertz.to_le_bytes();
        self.i2c
            .write(self.address, &[PWM_FREQUENCY, low, high])
            .map_err(Error::Bus)
    }

    /// Sets one channel's 12-bit duty, its polarity, and whether it runs.
    /// The channel's pin must be in its PWM function
    /// ([`M5pm1::configure_pwm_output`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidDuty`] above [`MAX_DUTY`], or an I2C error.
    pub fn set_pwm_duty(
        &mut self,
        channel: PwmChannel,
        duty: u16,
        inverted: bool,
        enabled: bool,
    ) -> Result<(), Error<I2C::Error>> {
        if duty > MAX_DUTY {
            return Err(Error::InvalidDuty);
        }
        let [low, mut high] = duty.to_le_bytes();
        if enabled {
            high |= PWM_ENABLE;
        }
        if inverted {
            high |= PWM_INVERTED;
        }
        self.i2c
            .write(self.address, &[channel.duty_register(), low, high])
            .map_err(Error::Bus)
    }

    /// Stops one channel by clearing its duty's high byte (enable, polarity,
    /// and the duty's top bits), as M5GFX's Paper Mono front light does for
    /// brightness zero.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn stop_pwm(&mut self, channel: PwmChannel) -> Result<(), Error<I2C::Error>> {
        self.write(channel.duty_register().wrapping_add(1), 0x00)
    }

    /// Reads the power key's level and whether it went down since the last
    /// read; reading clears that record (M5PM1 library `btnGetState` and
    /// `btnGetFlag`).
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn button_status(&mut self) -> Result<ButtonStatus, Error<I2C::Error>> {
        let status = self.read(BUTTON_STATUS)?;
        Ok(ButtonStatus {
            pressed: status & BUTTON_DOWN != 0,
            pressed_since_last_read: status & BUTTON_FLAG != 0,
        })
    }

    /// Chooses whether a single click of the power key resets the system,
    /// the M5PM1's factory behaviour, leaving the rest of `BTN_CFG_1` as it
    /// is. The M5PM1 keeps the setting while it has power, across resets of
    /// the host.
    ///
    /// # Errors
    ///
    /// Returns an I2C register error.
    pub fn set_single_click_reset(&mut self, enabled: bool) -> Result<(), Error<I2C::Error>> {
        self.update(BUTTON_CONFIG_1, |value| {
            set_bits(value, SINGLE_CLICK_RESET_DISABLED, !enabled)
        })
    }

    fn update(
        &mut self,
        register: u8,
        update: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error<I2C::Error>> {
        let value = self.read(register)?;
        self.write(register, update(value))
    }

    fn read(&mut self, register: u8) -> Result<u8, Error<I2C::Error>> {
        let mut value = [0];
        self.i2c
            .write_read(self.address, &[register], &mut value)
            .map_err(Error::Bus)?;
        Ok(value[0])
    }

    fn write(&mut self, register: u8, value: u8) -> Result<(), Error<I2C::Error>> {
        self.i2c
            .write(self.address, &[register, value])
            .map_err(Error::Bus)
    }
}

fn pin_mask<BusError>(g: u8) -> Result<u8, Error<BusError>> {
    if g >= GPIO_COUNT {
        return Err(Error::InvalidPin);
    }
    1_u8.checked_shl(u32::from(g)).ok_or(Error::InvalidPin)
}

const fn set_bits(value: u8, mask: u8, high: bool) -> u8 {
    if high { value | mask } else { value & !mask }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    extern crate std;

    use core::convert::Infallible;

    use embedded_hal::i2c::{ErrorType, I2c, Operation};

    use super::{ButtonStatus, Error, M5pm1, Pull, PwmChannel, Rail};

    #[test]
    fn power_key_reads_its_status_and_changes_only_the_reset_bit() {
        // M5PM1 library: BTN_STATUS bit 0 the level, bit 7 the press flag;
        // BTN_CFG_1 factory value 2Ah with SINGLE_RST_DIS (bit 0) clear.
        let mut bus = Bus {
            registers: [0x00; 0x50],
        };
        bus.registers[0x48] = 0b1000_0000;
        bus.registers[0x49] = 0x2a;
        let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
        assert_eq!(
            controller.button_status().expect("status"),
            ButtonStatus {
                pressed: false,
                pressed_since_last_read: true,
            }
        );
        controller
            .set_single_click_reset(false)
            .expect("single-click reset off");
        controller
            .set_single_click_reset(true)
            .expect("single-click reset on");
        controller
            .set_single_click_reset(false)
            .expect("single-click reset off");
        assert_eq!(bus.registers[0x49], 0x2b);
        bus.registers[0x48] = 0b0000_0001;
        let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
        let status = controller.button_status().expect("status");
        assert!(status.pressed && !status.pressed_since_last_read);
        // Reading writes nothing.
        assert_eq!(bus.registers[0x48], 0b0000_0001);
    }

    #[test]
    fn epd_supply_pin_matches_m5gfx_paper_diy_bring_up() {
        // M5GFX board_M5PaperDIY: 0x16 &= !(0b11 << 4), 0x10 |= 1 << 2,
        // 0x13 &= !(1 << 2), 0x11 |= 1 << 2.
        let mut bus = Bus {
            registers: [0xff; 0x50],
        };
        bus.registers[0x10] = 0x00;
        bus.registers[0x11] = 0x00;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller
                .configure_output(2, true)
                .expect("G2 output high");
        }
        assert_eq!(bus.registers[0x16], 0xcf);
        assert_eq!(bus.registers[0x10], 0x04);
        assert_eq!(bus.registers[0x13], 0xfb);
        assert_eq!(bus.registers[0x11], 0x04);
    }

    #[test]
    fn front_light_pwm_matches_m5gfx_paper_mono() {
        // M5GFX Light_M5PaperMono: G3 push-pull, 0x16 |= 0xC0, 5 kHz, then a
        // squared 12-bit duty with the enable bit.
        let mut bus = Bus {
            registers: [0x00; 0x50],
        };
        bus.registers[0x13] = 0xff;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller
                .configure_pwm_output(PwmChannel::Ch0)
                .expect("G3 PWM");
            controller.set_pwm_frequency(5_000).expect("frequency");
            controller
                .set_pwm_duty(PwmChannel::Ch0, 0x0fe0, false, true)
                .expect("duty");
        }
        assert_eq!(bus.registers[0x13], 0xf7);
        assert_eq!(bus.registers[0x16], 0xc0);
        assert_eq!(&bus.registers[0x34..0x36], &[0x88, 0x13]);
        assert_eq!(&bus.registers[0x30..0x32], &[0xe0, 0x1f]);
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.stop_pwm(PwmChannel::Ch0).expect("stop");
            assert!(matches!(
                controller.configure_output(5, false),
                Err(Error::InvalidPin)
            ));
            assert!(matches!(
                controller.set_pwm_duty(PwmChannel::Ch0, 0x1000, false, true),
                Err(Error::InvalidDuty)
            ));
        }
        assert_eq!(bus.registers[0x31], 0x00);
    }

    #[test]
    fn second_channel_is_g4_with_polarity_in_its_high_byte() {
        // M5Unified M5PM1_Class: PWM1 at 0x32/0x33, G4's function in bits
        // 1:0 of GPIO_FUNC1 (0x17), enable bit 4 and polarity bit 5.
        let mut bus = Bus {
            registers: [0x00; 0x50],
        };
        bus.registers[0x13] = 0xff;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller
                .configure_pwm_output(PwmChannel::Ch1)
                .expect("G4 PWM");
            controller
                .set_pwm_duty(PwmChannel::Ch1, 0x0123, true, true)
                .expect("duty");
        }
        assert_eq!(bus.registers[0x13], 0xef);
        assert_eq!(bus.registers[0x17], 0x03);
        assert_eq!(bus.registers[0x16], 0x00);
        assert_eq!(&bus.registers[0x32..0x34], &[0x23, 0x31]);
    }

    #[test]
    fn output_latches_its_level_before_driving() {
        let mut bus = Bus {
            registers: [0x00; 0x50],
        };
        bus.registers[0x16] = 0xff;
        bus.registers[0x13] = 0xff;
        bus.registers[0x11] = 0xff;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller
                .configure_output(0, false)
                .expect("G0 output low");
        }
        assert_eq!(bus.registers[0x16], 0xfc);
        assert_eq!(bus.registers[0x11], 0xfe);
        assert_eq!(bus.registers[0x13], 0xfe);
        assert_eq!(bus.registers[0x10], 0x01);
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.set_output(0, true).expect("G0 high");
        }
        assert_eq!(bus.registers[0x11], 0xff);
    }

    #[test]
    fn charge_status_input_and_battery_readings_match_m5unified() {
        // M5Unified PaperDIY: G3 a GPIO input with a pull-up (CHG_STAT).
        let mut bus = Bus {
            registers: [0x00; 0x50],
        };
        bus.registers[0x10] = 0xff;
        bus.registers[0x16] = 0xff;
        bus.registers[0x12] = 0b0000_0100;
        bus.registers[0x04] = 0b1111_0101;
        bus.registers[0x22] = 0x6c;
        bus.registers[0x23] = 0x0e;
        let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
        controller.configure_input(3, Pull::Up).expect("G3 input");
        assert!(!controller.input_is_high(3).expect("G3"));
        assert!(controller.input_is_high(2).expect("G2"));
        assert_eq!(controller.battery_millivolts().expect("VBAT"), 3_692);
        let source = controller.power_source().expect("source");
        assert!(source.vin() && !source.five_volt() && source.battery());
        assert!(matches!(
            controller.configure_input(5, Pull::None),
            Err(Error::InvalidPin)
        ));
        assert_eq!(bus.registers[0x16], 0x3f);
        assert_eq!(bus.registers[0x14], 0b0100_0000);
        assert_eq!(bus.registers[0x10], 0xf7);
    }

    struct Bus {
        registers: [u8; 0x50],
    }

    impl ErrorType for Bus {
        type Error = Infallible;
    }

    impl I2c for Bus {
        fn read(&mut self, _address: u8, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.fill(0);
            Ok(())
        }

        fn write(&mut self, _address: u8, bytes: &[u8]) -> Result<(), Self::Error> {
            if let [register, values @ ..] = bytes {
                for (offset, value) in values.iter().enumerate() {
                    self.registers[usize::from(*register) + offset] = *value;
                }
            }
            Ok(())
        }

        fn write_read(
            &mut self,
            _address: u8,
            bytes: &[u8],
            read: &mut [u8],
        ) -> Result<(), Self::Error> {
            if let [register] = bytes {
                for (offset, value) in read.iter_mut().enumerate() {
                    *value = self.registers[usize::from(*register) + offset];
                }
            }
            Ok(())
        }

        fn transaction(
            &mut self,
            _address: u8,
            _operations: &mut [Operation<'_>],
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn housekeeping_and_rails_match_m5gfx_corep4x_bring_up() {
        // M5GFX's CoreP4X bring-up writes 0x09 = 0, 0x0A = 0 and sets bit 3
        // of 0x06; the charger bit 0 stays as it was.
        let mut bus = Bus {
            registers: [0xff; 0x50],
        };
        bus.registers[0x06] = 0x01;
        {
            let mut controller = M5pm1::new(&mut bus, 0x6e).expect("controller");
            controller.disable_i2c_sleep().expect("I2C sleep");
            controller.disable_watchdog().expect("watchdog");
            controller
                .set_rail_enabled(Rail::Boost, true)
                .expect("boost on");
            controller
                .set_rail_enabled(Rail::Ldo, true)
                .expect("LDO on");
            controller
                .set_rail_enabled(Rail::Ldo, false)
                .expect("LDO off");
        }
        assert_eq!(bus.registers[0x09], 0x00);
        assert_eq!(bus.registers[0x0a], 0x00);
        assert_eq!(bus.registers[0x06], 0x09);
    }

    #[test]
    fn rejects_an_invalid_address() {
        let mut bus = Bus {
            registers: [0; 0x50],
        };
        assert!(matches!(
            M5pm1::new(&mut bus, 0x80),
            Err(Error::InvalidAddress)
        ));
    }
}
