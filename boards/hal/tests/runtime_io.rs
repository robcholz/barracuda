//! Application-shaped tests for the protocol-neutral exposed-I/O owner.

#![allow(clippy::expect_used)]

use core::convert::Infallible;

use barracuda_board_hal::{
    audio, AnalogErrorType, AnalogInput, AnalogProvider, ConfigurableDigitalPin, DigitalLevel,
    DigitalProvider, I2cProvider, I2cRequest, I2sProvider, I2sRequest, InputConfig, LeaseError,
    OutputConfig, PwmProvider, PwmRequest, RuntimeAnalogPlatform, RuntimeI2sPlatform, RuntimeIo,
    RuntimeOpenError, RuntimePlatform, RuntimePwmPlatform, RuntimeUartPlatform, SpiProvider,
    SpiRequest, UartConfig, UartDataBits, UartParity, UartProvider, UartRequest, UartStopBits,
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

struct FakePwm(u16);

#[derive(Debug, PartialEq, Eq)]
struct FakeUart {
    controller: u8,
    tx: Option<u8>,
    rx: Option<u8>,
    config: UartConfig,
}

#[derive(Debug, PartialEq, Eq)]
struct FakeI2s {
    bclk: u8,
    ws: u8,
    dout: Option<u8>,
    din: Option<u8>,
    format: audio::PcmFormat,
}

impl audio::PcmStream for FakeI2s {
    type Error = Infallible;

    fn format(&self) -> audio::PcmFormat {
        self.format
    }

    async fn write(&mut self, _samples: &[i16]) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        samples.fill(0x1234);
        Ok(())
    }
}

impl embedded_io::ErrorType for FakeUart {
    type Error = Infallible;
}

impl embedded_io_async::Read for FakeUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        buffer.fill(0x55);
        Ok(buffer.len())
    }
}

impl embedded_io_async::Write for FakeUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_hal::pwm::ErrorType for FakePwm {
    type Error = Infallible;
}

impl embedded_hal::pwm::SetDutyCycle for FakePwm {
    fn max_duty_cycle(&self) -> u16 {
        1023
    }

    fn set_duty_cycle(&mut self, duty: u16) -> Result<(), Self::Error> {
        self.0 = duty;
        Ok(())
    }
}

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
    type UartController = u8;
    type AdcResource = u8;
    type PwmResource = u8;
    type I2sResource = u8;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        FakePin(pin)
    }

    fn supports_i2c(
        controller: &Self::I2cController,
        scl: &Self::PinToken,
        _sda: &Self::PinToken,
    ) -> bool {
        *controller == 10 && *scl != 4
    }

    fn supports_i2c_config(
        controller: &Self::I2cController,
        scl: &Self::PinToken,
        sda: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0 && Self::supports_i2c(controller, scl, sda)
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

    fn supports_spi(
        controller: &Self::SpiController,
        sck: &Self::PinToken,
        _mosi: Option<&Self::PinToken>,
        _miso: Option<&Self::PinToken>,
    ) -> bool {
        *controller == 20 && *sck == 1
    }

    fn supports_spi_config(
        controller: &Self::SpiController,
        sck: &Self::PinToken,
        mosi: Option<&Self::PinToken>,
        miso: Option<&Self::PinToken>,
        frequency_hz: u32,
        _mode: spi::Mode,
    ) -> bool {
        frequency_hz > 0 && Self::supports_spi(controller, sck, mosi, miso)
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

    fn supports_analog_input(resource: &Self::AdcResource, pin: &Self::PinToken) -> bool {
        *resource == 40 && *pin == 1
    }

    fn supports_analog_output(pin: &Self::PinToken) -> bool {
        *pin == 2
    }

    fn analog_input(
        resource: Self::AdcResource,
        pin: Self::PinToken,
    ) -> Result<Self::AnalogInput, Self::AnalogError> {
        assert_eq!(resource, 40);
        Ok(FakeAnalog(pin))
    }

    fn analog_output(pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Ok(FakeAnalog(pin))
    }
}

impl RuntimePwmPlatform for FakePlatform {
    type Pwm = FakePwm;
    type PwmError = Infallible;

    fn supports_pwm(resource: &Self::PwmResource, pin: &Self::PinToken) -> bool {
        *resource == 50 && *pin == 3
    }

    fn supports_pwm_config(
        resource: &Self::PwmResource,
        pin: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0 && Self::supports_pwm(resource, pin)
    }

    fn pwm(
        resource: Self::PwmResource,
        _pin: Self::PinToken,
        _frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        assert_eq!(resource, 50);
        Ok(FakePwm(0))
    }
}

impl RuntimeUartPlatform for FakePlatform {
    type Uart = FakeUart;
    type UartError = Infallible;

    fn supports_uart(
        controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
    ) -> bool {
        *controller == 30 && (tx.is_some_and(|pin| *pin == 3) || rx.is_some_and(|pin| *pin == 4))
    }

    fn supports_uart_config(
        controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
        config: UartConfig,
    ) -> bool {
        config.baud > 0 && Self::supports_uart(controller, tx, rx)
    }

    fn uart(
        controller: Self::UartController,
        tx: Option<Self::PinToken>,
        rx: Option<Self::PinToken>,
        config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError> {
        Ok(FakeUart {
            controller,
            tx,
            rx,
            config,
        })
    }
}

impl RuntimeI2sPlatform for FakePlatform {
    type I2s = FakeI2s;
    type I2sError = Infallible;

    fn supports_i2s(
        resource: &Self::I2sResource,
        bclk: &Self::PinToken,
        ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        *resource == 60 && *bclk == 1 && *ws == 2 && (dout.is_some() || din.is_some())
    }

    fn supports_i2s_format(
        resource: &Self::I2sResource,
        bclk: &Self::PinToken,
        ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        mclk: Option<&Self::PinToken>,
        format: audio::PcmFormat,
    ) -> bool {
        format.sample_rate_hz > 0
            && format.channels == 2
            && format.bits_per_sample == 16
            && Self::supports_i2s(resource, bclk, ws, dout, din, mclk)
    }

    fn i2s(
        resource: Self::I2sResource,
        bclk: Self::PinToken,
        ws: Self::PinToken,
        dout: Option<Self::PinToken>,
        din: Option<Self::PinToken>,
        _mclk: Option<Self::PinToken>,
        format: audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        assert_eq!(resource, 60);
        Ok(FakeI2s {
            bclk,
            ws,
            dout,
            din,
            format,
        })
    }
}

fn runtime_io() -> RuntimeIo<FakePlatform, 4, 1, 1, 1, 1, 1, 1> {
    RuntimeIo::new_with_resources(
        [("D1", 1), ("D2", 2), ("D3", 3), ("D4", 4)],
        [10],
        [20],
        [30],
        [40],
        [50],
        [60],
    )
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
fn invalid_bus_routes_do_not_consume_controllers_or_pins() {
    let io = runtime_io();
    assert!(matches!(
        io.open_i2c(I2cRequest {
            scl: "D4",
            sda: "D1",
            frequency_hz: 100_000,
        }),
        Err(RuntimeOpenError::Unsupported { function: "I2C" })
    ));
    assert!(io.digital_available("D4"));
    let i2c = io
        .open_i2c(I2cRequest {
            scl: "D2",
            sda: "D3",
            frequency_hz: 100_000,
        })
        .expect("invalid route preserved I2C resources");
    assert_eq!(i2c.controller, 10);

    let io = runtime_io();
    assert!(matches!(
        io.open_spi(SpiRequest {
            sck: "D2",
            mosi: Some("D3"),
            miso: None,
            frequency_hz: 1_000_000,
            mode: spi::MODE_0,
        }),
        Err(RuntimeOpenError::Unsupported { function: "SPI" })
    ));
    assert!(io.digital_available("D2"));
    let spi = io
        .open_spi(SpiRequest {
            sck: "D1",
            mosi: Some("D2"),
            miso: None,
            frequency_hz: 1_000_000,
            mode: spi::MODE_0,
        })
        .expect("invalid route preserved SPI resources");
    assert_eq!(spi.controller, 20);
}

#[test]
fn invalid_parameters_do_not_consume_composite_resources() {
    let io = runtime_io();
    assert!(matches!(
        io.open_i2c(I2cRequest {
            scl: "D1",
            sda: "D2",
            frequency_hz: 0,
        }),
        Err(RuntimeOpenError::Unsupported { function: "I2C" })
    ));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D2"));

    let io = runtime_io();
    assert!(matches!(
        io.open_spi(SpiRequest {
            sck: "D1",
            mosi: Some("D2"),
            miso: None,
            frequency_hz: 0,
            mode: spi::MODE_0,
        }),
        Err(RuntimeOpenError::Unsupported { function: "SPI" })
    ));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D2"));

    let io = runtime_io();
    assert!(matches!(
        io.open_pwm(PwmRequest {
            pin: "D3",
            frequency_hz: 0,
        }),
        Err(RuntimeOpenError::Unsupported { function: "PWM" })
    ));
    assert!(io.digital_available("D3"));

    let io = runtime_io();
    assert!(matches!(
        io.open_uart(UartRequest {
            tx: Some("D3"),
            rx: Some("D4"),
            config: UartConfig {
                baud: 0,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
            },
        }),
        Err(RuntimeOpenError::Unsupported { function: "UART" })
    ));
    assert!(io.digital_available("D3"));
    assert!(io.digital_available("D4"));

    let io = runtime_io();
    assert!(matches!(
        io.open_i2s(I2sRequest {
            bclk: "D1",
            ws: "D2",
            dout: Some("D3"),
            din: None,
            mclk: None,
            format: audio::PcmFormat {
                sample_rate_hz: 48_000,
                channels: 1,
                bits_per_sample: 16,
                master_clock_hz: None,
            },
        }),
        Err(RuntimeOpenError::Unsupported { function: "I2S" })
    ));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D3"));
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

#[test]
fn pwm_claims_the_same_physical_pin_seen_by_gpio() {
    let io = runtime_io();
    assert!(io.pwm_available("D3"));
    assert!(matches!(
        io.open_pwm(PwmRequest {
            pin: "D2",
            frequency_hz: 1_000,
        }),
        Err(RuntimeOpenError::Unsupported { function: "PWM" })
    ));
    assert!(io.digital_available("D2"));
    let _pwm = io
        .open_pwm(PwmRequest {
            pin: "D3",
            frequency_hz: 1_000,
        })
        .expect("PWM output");
    assert!(!io.digital_available("D3"));
}

#[test]
fn uart_claims_multiple_roles_atomically_from_the_shared_owner() {
    let io = runtime_io();
    assert!(!io.uart_available(Some("D1"), Some("D2")));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D2"));
    assert!(matches!(
        io.open_uart(UartRequest {
            tx: Some("D1"),
            rx: Some("D2"),
            config: UartConfig {
                baud: 115_200,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
            },
        }),
        Err(RuntimeOpenError::Unsupported { function: "UART" })
    ));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D2"));
    assert!(matches!(
        io.open_uart(UartRequest {
            tx: Some("D3"),
            rx: Some("D3"),
            config: UartConfig {
                baud: 115_200,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
            },
        }),
        Err(RuntimeOpenError::Resource(LeaseError::Duplicate { .. }))
    ));
    let port = io
        .open_uart(UartRequest {
            tx: Some("D3"),
            rx: Some("D4"),
            config: UartConfig {
                baud: 115_200,
                data_bits: UartDataBits::Eight,
                parity: UartParity::None,
                stop_bits: UartStopBits::One,
            },
        })
        .expect("UART port");
    assert_eq!(port.tx, Some(3));
    assert_eq!(port.rx, Some(4));
    assert_eq!(port.controller, 30);
    assert!(!io.digital_available("D3"));
    assert!(!io.digital_available("D4"));
}

#[test]
fn i2s_claims_every_signal_from_the_shared_owner() {
    let io = runtime_io();
    assert!(matches!(
        io.open_i2s(I2sRequest {
            bclk: "D1",
            ws: "D3",
            dout: Some("D4"),
            din: None,
            mclk: None,
            format: audio::PcmFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                bits_per_sample: 16,
                master_clock_hz: None,
            },
        }),
        Err(RuntimeOpenError::Unsupported { function: "I2S" })
    ));
    assert!(io.digital_available("D1"));
    assert!(io.digital_available("D3"));
    assert!(io.digital_available("D4"));
    let stream = io
        .open_i2s(I2sRequest {
            bclk: "D1",
            ws: "D2",
            dout: Some("D3"),
            din: Some("D4"),
            mclk: None,
            format: audio::PcmFormat {
                sample_rate_hz: 48_000,
                channels: 2,
                bits_per_sample: 16,
                master_clock_hz: None,
            },
        })
        .expect("I2S stream");
    assert_eq!(stream.bclk, 1);
    assert_eq!(stream.ws, 2);
    assert_eq!(stream.dout, Some(3));
    assert_eq!(stream.din, Some(4));
    assert!(!io.digital_available("D1"));
    assert!(!io.digital_available("D4"));
}
