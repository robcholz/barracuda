//! Application-shaped tests for the protocol-neutral exposed-I/O owner.

#![allow(clippy::expect_used)]

use core::convert::Infallible;

use barracuda_board_hal::{
    AnalogErrorType, AnalogInput, AnalogProvider, ConfigurableDigitalPin, DigitalLevel,
    DigitalProvider, I2cProvider, I2cRequest, InputConfig, LeaseError, OutputConfig,
    RuntimeAnalogPlatform, RuntimeIo, RuntimeOpenError, RuntimePlatform, SpiProvider, SpiRequest,
};
use embedded_hal::{
    digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin},
    i2c, spi,
};

struct FakePin(u8);

impl ErrorType for FakePin {
    type Error = Infallible;
}

impl InputPin for FakePin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 != 0)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 == 0)
    }
}

impl OutputPin for FakePin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.0 = 0;
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.0 = 1;
        Ok(())
    }
}

impl StatefulOutputPin for FakePin {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 != 0)
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        Ok(self.0 == 0)
    }
}

impl ConfigurableDigitalPin for FakePin {
    fn configure_input(&mut self, _config: InputConfig) -> Result<(), Self::Error> {
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        self.0 = u8::from(config.initial == DigitalLevel::High);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FakeI2c {
    controller: u8,
    scl: u8,
    sda: u8,
    frequency_hz: u32,
}

impl i2c::ErrorType for FakeI2c {
    type Error = Infallible;
}

impl embedded_hal_async::i2c::I2c for FakeI2c {
    async fn transaction(
        &mut self,
        _address: u8,
        _operations: &mut [i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FakeSpi {
    controller: u8,
    sck: u8,
    mosi: Option<u8>,
    miso: Option<u8>,
    frequency_hz: u32,
    mode: spi::Mode,
}

impl spi::ErrorType for FakeSpi {
    type Error = Infallible;
}

impl embedded_hal_async::spi::SpiBus for FakeSpi {
    async fn read(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn write(&mut self, _words: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn transfer(&mut self, _read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct FakePlatform;

struct FakeAnalog(u8);

impl AnalogErrorType for FakeAnalog {
    type Error = Infallible;
}

impl AnalogInput for FakeAnalog {
    fn max_value(&self) -> u32 {
        4095
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        Ok(u32::from(self.0))
    }
}

impl barracuda_board_hal::AnalogOutput for FakeAnalog {
    fn max_value(&self) -> u32 {
        255
    }

    fn write(&mut self, value: u32) -> Result<(), Self::Error> {
        self.0 = value as u8;
        Ok(())
    }
}

impl RuntimePlatform for FakePlatform {
    type PinToken = u8;
    type DigitalPin = FakePin;
    type I2cController = u8;
    type I2cBus = FakeI2c;
    type I2cError = Infallible;
    type SpiController = u8;
    type SpiBus = FakeSpi;
    type SpiError = Infallible;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        FakePin(pin)
    }

    fn i2c(
        controller: Self::I2cController,
        scl: Self::PinToken,
        sda: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError> {
        Ok(FakeI2c {
            controller,
            scl,
            sda,
            frequency_hz,
        })
    }

    fn spi(
        controller: Self::SpiController,
        sck: Self::PinToken,
        mosi: Option<Self::PinToken>,
        miso: Option<Self::PinToken>,
        frequency_hz: u32,
        mode: spi::Mode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        Ok(FakeSpi {
            controller,
            sck,
            mosi,
            miso,
            frequency_hz,
            mode,
        })
    }
}

impl RuntimeAnalogPlatform for FakePlatform {
    type AnalogInput = FakeAnalog;
    type AnalogOutput = FakeAnalog;
    type AnalogError = Infallible;

    fn supports_analog_input(pin: &Self::PinToken) -> bool {
        *pin == 1
    }

    fn supports_analog_output(pin: &Self::PinToken) -> bool {
        *pin == 2
    }

    fn analog_input(pin: Self::PinToken) -> Result<Self::AnalogInput, Self::AnalogError> {
        Ok(FakeAnalog(pin))
    }

    fn analog_output(pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Ok(FakeAnalog(pin))
    }
}

fn runtime_io() -> RuntimeIo<FakePlatform, 4, 1, 1> {
    RuntimeIo::new([("D1", 1), ("D2", 2), ("D3", 3), ("D4", 4)], [10], [20])
}

#[test]
fn generated_owner_claims_real_tokens_once_per_boot() {
    let io = runtime_io();
    {
        let _digital = io.acquire_digital("D1").expect("digital token");
    }

    assert!(!io.digital_available("D1"));
    assert!(matches!(
        io.open_i2c(I2cRequest {
            scl: "D1",
            sda: "D2",
            frequency_hz: 400_000,
        }),
        Err(RuntimeOpenError::Resource(LeaseError::Busy {
            resource: "D1",
            owner: "digital",
        }))
    ));

    let bus = io
        .open_i2c(I2cRequest {
            scl: "D2",
            sda: "D3",
            frequency_hz: 400_000,
        })
        .expect("failed claim did not consume the controller");
    assert_eq!(
        bus,
        FakeI2c {
            controller: 10,
            scl: 2,
            sda: 3,
            frequency_hz: 400_000,
        }
    );
}

#[test]
fn runtime_spi_claim_is_atomic_and_preserves_standard_mode() {
    let io = runtime_io();
    assert!(matches!(
        io.open_spi(SpiRequest {
            sck: "D1",
            mosi: None,
            miso: None,
            frequency_hz: 8_000_000,
            mode: spi::MODE_0,
        }),
        Err(RuntimeOpenError::MissingSpiData)
    ));
    assert!(matches!(
        io.open_spi(SpiRequest {
            sck: "D1",
            mosi: Some("D1"),
            miso: None,
            frequency_hz: 8_000_000,
            mode: spi::MODE_3,
        }),
        Err(RuntimeOpenError::Resource(LeaseError::Duplicate { .. }))
    ));

    let bus = io
        .open_spi(SpiRequest {
            sck: "D1",
            mosi: Some("D2"),
            miso: Some("D3"),
            frequency_hz: 8_000_000,
            mode: spi::MODE_3,
        })
        .expect("SPI bus");
    assert_eq!(bus.controller, 20);
    assert_eq!(bus.sck, 1);
    assert_eq!(bus.mosi, Some(2));
    assert_eq!(bus.miso, Some(3));
    assert_eq!(bus.frequency_hz, 8_000_000);
    assert_eq!(bus.mode, spi::MODE_3);
}

#[test]
fn runtime_controller_exhaustion_does_not_consume_more_pins() {
    let io = runtime_io();
    let _first = io
        .open_i2c(I2cRequest {
            scl: "D1",
            sda: "D2",
            frequency_hz: 100_000,
        })
        .expect("first I2C controller");
    assert!(matches!(
        io.open_i2c(I2cRequest {
            scl: "D3",
            sda: "D4",
            frequency_hz: 100_000,
        }),
        Err(RuntimeOpenError::NoController { protocol: "I2C" })
    ));
    assert!(io.digital_available("D3"));
    assert!(io.digital_available("D4"));
}

#[test]
fn analog_support_is_checked_before_the_shared_pin_is_consumed() {
    let io = runtime_io();
    assert!(io.analog_input_available("D1"));
    assert!(!io.analog_input_available("D2"));
    assert!(matches!(
        io.acquire_analog_input("D2"),
        Err(RuntimeOpenError::Unsupported {
            function: "analog input"
        })
    ));
    assert!(io.digital_available("D2"));

    let mut input = io.acquire_analog_input("D1").expect("analog input");
    assert_eq!(input.read(), Ok(1));
    assert!(!io.digital_available("D1"));
}
