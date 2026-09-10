//! Board HAL exposed-I/O behavior.

use core::convert::Infallible;

use barracuda_board_hal::{
    AnalogErrorType, AnalogInput, AnalogOutput, ConfigurableDigitalPin, DigitalLevel, InputConfig,
    OutputConfig, OutputDrive, Pull,
};
use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Disabled,
    Input(Pull),
    Output(OutputDrive),
}

struct TestPin {
    mode: Mode,
    level: DigitalLevel,
}

impl TestPin {
    const fn new() -> Self {
        Self {
            mode: Mode::Disabled,
            level: DigitalLevel::Low,
        }
    }
}

impl ErrorType for TestPin {
    type Error = Infallible;
}

impl InputPin for TestPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level == DigitalLevel::High)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level == DigitalLevel::Low)
    }
}

impl OutputPin for TestPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.level = DigitalLevel::Low;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.level = DigitalLevel::High;
        Ok(())
    }
}

impl StatefulOutputPin for TestPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level == DigitalLevel::High)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.level == DigitalLevel::Low)
    }
}

impl ConfigurableDigitalPin for TestPin {
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
        self.mode = Mode::Input(config.pull);
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        self.level = config.initial;
        self.mode = Mode::Output(config.drive);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.mode = Mode::Disabled;
        Ok(())
    }
}

#[test]
fn digital_pin_changes_mode_without_replacing_embedded_hal_operations() {
    let mut pin = TestPin::new();

    pin.configure_input(InputConfig { pull: Pull::Up })
        .expect("input configuration");
    assert_eq!(pin.mode, Mode::Input(Pull::Up));
    assert!(pin.is_low().expect("digital input"));

    pin.configure_output(OutputConfig {
        initial: DigitalLevel::High,
        drive: OutputDrive::OpenDrain,
    })
    .expect("output configuration");
    assert_eq!(pin.mode, Mode::Output(OutputDrive::OpenDrain));
    assert!(pin.is_set_high().expect("output state"));

    pin.set_low().expect("embedded-hal output operation");
    assert!(pin.is_set_low().expect("output state"));

    pin.disable().expect("disable pin");
    assert_eq!(pin.mode, Mode::Disabled);
}

struct TestAnalog {
    value: u32,
    maximum: u32,
}

impl AnalogErrorType for TestAnalog {
    type Error = Infallible;
}

impl AnalogInput for TestAnalog {
    fn max_value(&self) -> u32 {
        self.maximum
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        Ok(self.value)
    }
}

impl AnalogOutput for TestAnalog {
    fn max_value(&self) -> u32 {
        self.maximum
    }

    fn write(&mut self, value: u32) -> Result<(), Self::Error> {
        self.value = value;
        Ok(())
    }
}

#[test]
fn analog_interfaces_report_their_native_range() {
    let mut analog = TestAnalog {
        value: 512,
        maximum: 4095,
    };

    assert_eq!(AnalogInput::max_value(&analog), 4095);
    assert_eq!(analog.read().expect("analog read"), 512);
    analog.write(1024).expect("analog write");
    assert_eq!(analog.read().expect("analog read"), 1024);
}
