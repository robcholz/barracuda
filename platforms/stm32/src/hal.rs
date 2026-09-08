//! STM32 adaptation from Board-selected tokens to embedded-hal resources.

use core::convert::Infallible;

use barracuda_board_hal::{
    audio, ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig, OutputDrive,
    Pull as BoardPull, RuntimeAnalogPlatform, RuntimeI2sPlatform, RuntimePlatform,
    RuntimePwmPlatform, RuntimeUartPlatform, UartConfig, UnavailableAnalogInput,
    UnavailableAnalogOutput, UnavailableI2c, UnavailableI2s, UnavailablePwm, UnavailableSpi,
    UnavailableUart, UnsupportedFunction,
};
use embassy_stm32::gpio::{AnyPin, Flex, Input, Level, Output, Pin, Pull, Speed};
use embedded_hal::{
    digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin},
    spi::Mode,
};

#[doc(hidden)]
pub use embassy_stm32 as __vendor;

/// Type-erased push-pull output consumed by peripheral Drivers.
pub type DigitalOutput = Output<'static>;
/// Type-erased digital input consumed by peripheral Drivers.
pub type DigitalInput = Input<'static>;

/// Platform adapter used by generated GPIO-only runtime I/O composition.
pub struct RuntimeAdapter;

impl RuntimePlatform for RuntimeAdapter {
    type PinToken = embassy_stm32::Peri<'static, AnyPin>;
    type DigitalPin = DynamicPin;
    type I2cController = Infallible;
    type I2cBus = UnavailableI2c;
    type I2cError = UnsupportedFunction;
    type SpiController = Infallible;
    type SpiBus = UnavailableSpi;
    type SpiError = UnsupportedFunction;
    type UartController = Infallible;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        DynamicPin {
            pin: Flex::new(pin),
        }
    }

    fn i2c(
        controller: Self::I2cController,
        _scl: Self::PinToken,
        _sda: Self::PinToken,
        _frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError> {
        match controller {}
    }

    fn spi(
        controller: Self::SpiController,
        _sck: Self::PinToken,
        _mosi: Option<Self::PinToken>,
        _miso: Option<Self::PinToken>,
        _frequency_hz: u32,
        _mode: Mode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        match controller {}
    }
}

impl RuntimeAnalogPlatform for RuntimeAdapter {
    type AnalogInput = UnavailableAnalogInput;
    type AnalogOutput = UnavailableAnalogOutput;
    type AnalogError = UnsupportedFunction;

    fn supports_analog_input(_pin: &Self::PinToken) -> bool {
        false
    }

    fn supports_analog_output(_pin: &Self::PinToken) -> bool {
        false
    }

    fn analog_input(_pin: Self::PinToken) -> Result<Self::AnalogInput, Self::AnalogError> {
        Err(UnsupportedFunction::new("analog input"))
    }

    fn analog_output(_pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Err(UnsupportedFunction::new("analog output"))
    }
}

impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = UnavailablePwm;
    type PwmError = UnsupportedFunction;

    fn supports_pwm(_pin: &Self::PinToken) -> bool {
        false
    }

    fn pwm(_pin: Self::PinToken, _frequency_hz: u32) -> Result<Self::Pwm, Self::PwmError> {
        Err(UnsupportedFunction::new("PWM"))
    }
}

impl RuntimeUartPlatform for RuntimeAdapter {
    type Uart = UnavailableUart;
    type UartError = UnsupportedFunction;

    fn supports_uart(
        controller: &Self::UartController,
        _tx: Option<&Self::PinToken>,
        _rx: Option<&Self::PinToken>,
    ) -> bool {
        match *controller {}
    }

    fn uart(
        controller: Self::UartController,
        _tx: Option<Self::PinToken>,
        _rx: Option<Self::PinToken>,
        _config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError> {
        match controller {}
    }
}

impl RuntimeI2sPlatform for RuntimeAdapter {
    type I2s = UnavailableI2s;
    type I2sError = UnsupportedFunction;

    fn supports_i2s(
        _bclk: &Self::PinToken,
        _ws: &Self::PinToken,
        _dout: Option<&Self::PinToken>,
        _din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        false
    }

    fn i2s(
        _bclk: Self::PinToken,
        _ws: Self::PinToken,
        _dout: Option<Self::PinToken>,
        _din: Option<Self::PinToken>,
        _mclk: Option<Self::PinToken>,
        _format: audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        Err(UnsupportedFunction::new("I2S"))
    }
}

/// Generated exposed-I/O owner specialized to the STM32 Platform adapter.
pub type RuntimeIo<const P: usize, const I: usize, const S: usize, const U: usize> =
    barracuda_board_hal::RuntimeIo<RuntimeAdapter, P, I, S, U>;

/// Constructs the selected Board's unified runtime I/O owner.
#[must_use]
pub fn runtime_io<const P: usize>(
    pins: [(&'static str, embassy_stm32::Peri<'static, AnyPin>); P],
    i2c: [Infallible; 0],
    spi: [Infallible; 0],
    uart: [Infallible; 0],
) -> RuntimeIo<P, 0, 0, 0> {
    RuntimeIo::new_with_uart(pins, i2c, spi, uart)
}

/// Erases one selected pin token while preserving its exclusive ownership.
#[must_use]
pub fn runtime_pin(
    pin: embassy_stm32::Peri<'static, impl Pin>,
) -> embassy_stm32::Peri<'static, AnyPin> {
    pin.into()
}

/// Converts a selected raw pin token into a digital output.
#[must_use]
pub fn digital_output(
    pin: embassy_stm32::Peri<'static, impl embassy_stm32::gpio::Pin>,
    initial: DigitalLevel,
) -> DigitalOutput {
    let level = match initial {
        DigitalLevel::Low => Level::Low,
        DigitalLevel::High => Level::High,
    };
    Output::new(pin, level, Speed::Low)
}

/// Converts a selected raw pin token into a digital input.
#[must_use]
pub fn digital_input(
    pin: embassy_stm32::Peri<'static, impl embassy_stm32::gpio::Pin>,
) -> DigitalInput {
    Input::new(pin, Pull::None)
}

/// Runtime-configurable STM32 GPIO exposed through `embedded-hal`.
pub struct DynamicPin {
    pin: Flex<'static>,
}

/// Converts a selected raw token into a runtime-configurable GPIO.
#[must_use]
pub fn dynamic_pin(pin: embassy_stm32::Peri<'static, impl embassy_stm32::gpio::Pin>) -> DynamicPin {
    DynamicPin {
        pin: Flex::new(pin),
    }
}

impl ErrorType for DynamicPin {
    type Error = Infallible;
}

impl InputPin for DynamicPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        InputPin::is_high(&mut self.pin)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        InputPin::is_low(&mut self.pin)
    }
}

impl OutputPin for DynamicPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        OutputPin::set_low(&mut self.pin)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        OutputPin::set_high(&mut self.pin)
    }
}

impl StatefulOutputPin for DynamicPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        StatefulOutputPin::is_set_high(&mut self.pin)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        StatefulOutputPin::is_set_low(&mut self.pin)
    }
}

impl ConfigurableDigitalPin for DynamicPin {
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
        let pull = match config.pull {
            BoardPull::None => Pull::None,
            BoardPull::Up => Pull::Up,
            BoardPull::Down => Pull::Down,
        };
        self.pin.set_as_input(pull);
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        match config.initial {
            DigitalLevel::Low => OutputPin::set_low(&mut self.pin)?,
            DigitalLevel::High => OutputPin::set_high(&mut self.pin)?,
        }
        match config.drive {
            OutputDrive::PushPull => self.pin.set_as_output(Speed::Low),
            OutputDrive::OpenDrain => self.pin.set_as_input_output(Speed::Low),
        }
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.pin.set_as_analog();
        Ok(())
    }
}

/// Resolves a Board-selected pin name to its move-only STM32 HAL token type.
#[macro_export]
macro_rules! __barracuda_stm32_pin_binding_type {
    ($pin:ident) => {
        $crate::hal::__vendor::Peri<'static, $crate::hal::__vendor::peripherals::$pin>
    };
}

#[doc(hidden)]
pub use __barracuda_stm32_pin_binding_type as pin_binding_type;
