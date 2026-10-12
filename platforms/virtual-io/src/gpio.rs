//! Virtual runtime-configurable digital pin.

use core::convert::Infallible;

use barracuda_board_hal::{
    ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig, OutputDrive, Pull,
};
use embedded_hal::digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin};

use crate::hardware::{PinDrive, PinMode, PinPull, VirtualHardware};

/// One claimed virtual pin exposed through `embedded-hal` digital traits.
#[derive(Debug)]
pub struct VirtualDigitalPin {
    hardware: VirtualHardware,
    pin: usize,
    pull: PinPull,
    drive: PinDrive,
}

impl VirtualDigitalPin {
    pub(crate) fn new(hardware: VirtualHardware, pin: usize) -> Self {
        hardware.claim_digital(pin);
        Self {
            hardware,
            pin,
            pull: PinPull::None,
            drive: PinDrive::PushPull,
        }
    }
}

impl ErrorType for VirtualDigitalPin {
    type Error = Infallible;
}

impl InputPin for VirtualDigitalPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.hardware.level(self.pin))
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.hardware.level(self.pin))
    }
}

impl OutputPin for VirtualDigitalPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.hardware.set_output(self.pin, false);
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.hardware.set_output(self.pin, true);
        Ok(())
    }
}

impl StatefulOutputPin for VirtualDigitalPin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.hardware.output_latch(self.pin))
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(!self.hardware.output_latch(self.pin))
    }
}

impl ConfigurableDigitalPin for VirtualDigitalPin {
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error> {
        self.pull = match config.pull {
            Pull::None => PinPull::None,
            Pull::Up => PinPull::Up,
            Pull::Down => PinPull::Down,
        };
        self.hardware
            .configure_pin(self.pin, PinMode::Input, self.pull, self.drive, None);
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        self.drive = match config.drive {
            OutputDrive::PushPull => PinDrive::PushPull,
            OutputDrive::OpenDrain => PinDrive::OpenDrain,
        };
        self.hardware.configure_pin(
            self.pin,
            PinMode::Output,
            self.pull,
            self.drive,
            Some(config.initial == DigitalLevel::High),
        );
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.hardware
            .configure_pin(self.pin, PinMode::Disabled, self.pull, self.drive, None);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::clock::Clock;

    #[test]
    fn mode_changes_reach_the_shared_model() {
        let hardware = VirtualHardware::new(&["GPIO0"], &[], Clock::manual());
        let mut pin = VirtualDigitalPin::new(hardware.clone(), 0);
        pin.configure_output(OutputConfig {
            initial: DigitalLevel::High,
            drive: OutputDrive::PushPull,
        })
        .expect("output");
        assert!(pin.is_set_high().expect("latch"));
        assert!(hardware.pin("GPIO0").expect("pin").level);
        pin.set_low().expect("low");
        assert!(pin.is_low().expect("read"));

        pin.configure_input(InputConfig { pull: Pull::Down })
            .expect("input");
        hardware.drive("GPIO0", Some(true)).expect("drive");
        assert!(pin.is_high().expect("read"));
        hardware.drive("GPIO0", None).expect("release");
        assert!(pin.is_low().expect("read"));

        pin.disable().expect("disable");
        let snapshot = hardware.pin("GPIO0").expect("pin");
        assert_eq!(snapshot.mode, PinMode::Disabled);
        assert_eq!(snapshot.function.as_deref(), Some("digital"));
    }
}
