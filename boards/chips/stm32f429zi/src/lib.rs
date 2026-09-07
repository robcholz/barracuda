//! Chip-owned translation from STM32F429ZI resource tokens to stable Board
//! binding categories.

#![no_std]

#[cfg(target_arch = "arm")]
mod chip {
    use core::convert::Infallible;

    use barracuda_board_hal::{
        ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig, OutputDrive,
        Pull as BoardPull,
    };
    use embassy_stm32::gpio::{Flex, Level, Output, Pull, Speed};
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

    /// Embassy's move-only peripheral token used in generated binding structs.
    pub use embassy_stm32::Peri;
    /// STM32F429ZI peripheral token vocabulary.
    pub use embassy_stm32::peripherals;

    /// Type-erased push-pull output consumed by digital-output Drivers.
    pub type DigitalOutput = Output<'static>;

    /// Converts one chip pin token into the canonical Driver digital output.
    #[must_use]
    pub fn digital_output(
        pin: Peri<'static, impl embassy_stm32::gpio::Pin>,
        initial: DigitalLevel,
    ) -> DigitalOutput {
        let level = match initial {
            DigitalLevel::Low => Level::Low,
            DigitalLevel::High => Level::High,
        };
        Output::new(pin, level, Speed::Low)
    }

    /// Converts one chip pin token into runtime-configurable exposed GPIO.
    #[must_use]
    pub fn dynamic_pin(pin: Peri<'static, impl embassy_stm32::gpio::Pin>) -> DynamicPin {
        DynamicPin {
            pin: Flex::new(pin),
        }
    }

    /// Runtime-configurable STM32 GPIO exposed through `embedded-hal`.
    pub struct DynamicPin {
        pin: Flex<'static>,
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
}

#[cfg(target_arch = "arm")]
pub use chip::*;
