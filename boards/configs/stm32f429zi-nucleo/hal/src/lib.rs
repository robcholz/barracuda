//! Concrete Board HAL for the STM32F429ZI Nucleo-144.

#![no_std]

#[cfg(target_arch = "arm")]
mod board {
    use core::convert::Infallible;

    use barracuda_board_hal::{
        BoardHal, BoardHalInitResult, BoardHalResources, ConfigurableDigitalPin, DigitalLevel,
        ExposedIo, InputConfig, NamedResources, OutputConfig, OutputDrive, Pull, UnavailableI2c,
        UnavailableSpi,
    };
    use barracuda_indicator_led::{ActiveLevel, IndicatorLed};
    use embassy_executor::Spawner;
    use embassy_stm32::{
        Peri,
        gpio::{Flex, Level, Output, Pull as Stm32Pull, Speed},
        peripherals::{PB0, PC13},
    };
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

    /// Move-only pin tokens assigned to this Board HAL by Target.
    pub struct Stm32f429ziNucleoBindings {
        status_led: Peri<'static, PB0>,
        user_button: Peri<'static, PC13>,
    }

    impl Stm32f429ziNucleoBindings {
        /// Binds the two pins explicitly named by this Board configuration.
        #[must_use]
        pub const fn new(status_led: Peri<'static, PB0>, user_button: Peri<'static, PC13>) -> Self {
            Self {
                status_led,
                user_button,
            }
        }
    }

    /// Built-in peripheral drivers created by the Board HAL.
    pub struct Stm32f429ziNucleoBuiltins {
        /// Green LD1, active high on PB0 with the default solder-bridge setup.
        pub status_led: IndicatorLed<Output<'static>>,
    }

    /// I/O explicitly exposed by this Board configuration.
    pub struct Stm32f429ziNucleoIo {
        gpio: Option<NamedResources<Stm32DynamicPin, 1>>,
    }

    impl ExposedIo for Stm32f429ziNucleoIo {
        type Gpio = NamedResources<Stm32DynamicPin, 1>;
        type I2c = NamedResources<UnavailableI2c, 0>;
        type Spi = NamedResources<UnavailableSpi, 0>;

        fn take_gpio(&mut self) -> Option<Self::Gpio> {
            self.gpio.take()
        }

        fn take_i2c(&mut self) -> Option<Self::I2c> {
            None
        }

        fn take_spi(&mut self) -> Option<Self::Spi> {
            None
        }
    }

    /// Runtime-configurable STM32 GPIO value exposed through `embedded-hal`.
    pub struct Stm32DynamicPin {
        pin: Flex<'static>,
    }

    impl Stm32DynamicPin {
        fn new(pin: Peri<'static, impl embassy_stm32::gpio::Pin>) -> Self {
            Self {
                pin: Flex::new(pin),
            }
        }
    }

    impl ErrorType for Stm32DynamicPin {
        type Error = Infallible;
    }

    impl InputPin for Stm32DynamicPin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            InputPin::is_high(&mut self.pin)
        }

        fn is_low(&mut self) -> Result<bool, Self::Error> {
            InputPin::is_low(&mut self.pin)
        }
    }

    impl OutputPin for Stm32DynamicPin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            OutputPin::set_low(&mut self.pin)
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            OutputPin::set_high(&mut self.pin)
        }
    }

    impl StatefulOutputPin for Stm32DynamicPin {
        fn is_set_high(&mut self) -> Result<bool, Self::Error> {
            StatefulOutputPin::is_set_high(&mut self.pin)
        }

        fn is_set_low(&mut self) -> Result<bool, Self::Error> {
            StatefulOutputPin::is_set_low(&mut self.pin)
        }
    }

    impl ConfigurableDigitalPin for Stm32DynamicPin {
        fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
            let pull = match config.pull {
                Pull::None => Stm32Pull::None,
                Pull::Up => Stm32Pull::Up,
                Pull::Down => Stm32Pull::Down,
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

    /// Board matrix and built-in Driver composition for NUCLEO-F429ZI.
    pub struct SelectedBoardHal;

    impl BoardHal for SelectedBoardHal {
        type Bindings = Stm32f429ziNucleoBindings;
        type Resources = BoardHalResources<Stm32f429ziNucleoBuiltins, Stm32f429ziNucleoIo>;
        type Error = Infallible;

        async fn initialize(
            _spawner: Spawner,
            bindings: Self::Bindings,
        ) -> BoardHalInitResult<Self> {
            let status_led = Output::new(bindings.status_led, Level::Low, Speed::Low);
            let builtins = Stm32f429ziNucleoBuiltins {
                status_led: IndicatorLed::new(status_led, ActiveLevel::High),
            };
            let io = Stm32f429ziNucleoIo {
                gpio: Some(NamedResources::new([(
                    "user-button",
                    Stm32DynamicPin::new(bindings.user_button),
                )])),
            };
            Ok(BoardHalResources::new(builtins, io))
        }
    }
}

#[cfg(target_arch = "arm")]
pub use board::*;
