//! Deterministic hardware Drivers owned by the e2e composition.

use core::convert::Infallible;

use barracuda_board_hal::{ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig};
use embedded_hal::{digital, i2c, spi};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Deterministic configurable GPIO used only by the e2e Board.
pub struct MockGpio {
    mode: MockGpioMode,
    level: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum MockGpioMode {
    #[default]
    Disabled,
    Input,
    Output,
}

impl MockGpio {
    pub(crate) const fn new() -> Self {
        Self {
            mode: MockGpioMode::Disabled,
            level: false,
        }
    }
}

impl digital::ErrorType for MockGpio {
    type Error = Infallible;
}

impl digital::InputPin for MockGpio {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.level)
    }
}

impl digital::OutputPin for MockGpio {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.level = false;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.level = true;
        Ok(())
    }
}

impl digital::StatefulOutputPin for MockGpio {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.level)
    }
}

impl ConfigurableDigitalPin for MockGpio {
    fn configure_input(&mut self, _config: InputConfig) -> Result<(), Self::Error> {
        self.mode = MockGpioMode::Input;
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        self.level = config.initial == DigitalLevel::High;
        self.mode = MockGpioMode::Output;
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.mode = MockGpioMode::Disabled;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Deterministic I2C bus used only by the e2e Board.
pub struct MockI2c;

impl i2c::ErrorType for MockI2c {
    type Error = Infallible;
}

impl embedded_hal_async::i2c::I2c for MockI2c {
    async fn transaction(
        &mut self,
        _address: u8,
        operations: &mut [i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        for operation in operations {
            if let i2c::Operation::Read(bytes) = operation {
                bytes.fill(0);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Deterministic SPI bus used only by the e2e Board.
pub struct MockSpi;

impl spi::ErrorType for MockSpi {
    type Error = Infallible;
}

impl embedded_hal_async::spi::SpiBus for MockSpi {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        words.fill(0);
        Ok(())
    }

    async fn write(&mut self, _words: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        for (index, target) in read.iter_mut().enumerate() {
            *target = write.get(index).copied().unwrap_or(0);
        }
        Ok(())
    }

    async fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
