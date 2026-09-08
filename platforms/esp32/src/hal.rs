// Shared ESP vendor adaptation instantiated inside each supported Platform.

use barracuda_board_hal::{
    audio, ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig, OutputDrive,
    Pull as BoardPull, RuntimeAnalogPlatform, RuntimeI2sPlatform, RuntimePlatform,
    RuntimePwmPlatform, RuntimeUartPlatform, UartConfig, UartDataBits, UartParity, UartStopBits,
    UnavailableAnalogInput, UnavailableAnalogOutput, UnavailableI2s, UnavailablePwm,
    UnsupportedFunction,
};
use embedded_hal::{
    digital::{ErrorType, StatefulOutputPin},
    i2c::Operation as I2cOperation,
    spi::{Mode as EmbeddedMode, Phase, Polarity},
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    delay::Delay,
    gpio::{
        interconnect::{PeripheralInput, PeripheralOutput},
        AnyPin, DriveMode, Flex, Input, InputConfig as HalInputConfig, InputPin, Level, Output,
        OutputConfig as HalOutputConfig, OutputPin, Pin, Pull,
    },
    i2c::master::{
        AnyI2c, Config as I2cConfig, ConfigError as I2cConfigErrorInner, I2c,
        Instance as I2cInstance,
    },
    spi::master::{
        AnySpi, Config as SpiConfig, ConfigError as SpiConfigErrorInner, Instance as SpiInstance,
        Spi,
    },
    spi::Mode as HalSpiMode,
    time::Rate,
    uart::{
        AnyUart, Config as HalUartConfig, ConfigError as UartConfigError, DataBits,
        Instance as UartInstance, Parity, StopBits, Uart,
    },
    Blocking,
};

#[doc(hidden)]
pub use esp_hal as __vendor;

/// Type-erased push-pull output consumed by peripheral Drivers.
pub type DigitalOutput = Output<'static>;
/// Type-erased digital input consumed by peripheral Drivers.
pub type DigitalInput = Input<'static>;
/// Blocking SPI bus used by statically selected devices.
pub type SpiBus = Spi<'static, Blocking>;
/// Standards-compliant single-owner SPI device.
pub type SpiDevice = ExclusiveDevice<SpiBus, DigitalOutput, Delay>;
/// Blocking I2C controller passed to a peripheral Driver.
pub type I2cBus = I2c<'static, Blocking>;
/// Send-safe I2C adapter exposed through the async ecosystem contract.
///
/// ESP HAL's interrupt-backed async marker is intentionally `!Send`, while a
/// Barracuda Plugin handle can move between executor tasks. Runtime buses use
/// the blocking peripheral driver behind async trait methods so ownership can
/// remain safe without an unsafe `Send` assertion.
pub struct ExposedI2cBus(I2c<'static, Blocking>);

impl embedded_hal::i2c::ErrorType for ExposedI2cBus {
    type Error = esp_hal::i2c::master::Error;
}

impl embedded_hal_async::i2c::I2c for ExposedI2cBus {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [I2cOperation<'_>],
    ) -> Result<(), Self::Error> {
        embedded_hal::i2c::I2c::transaction(&mut self.0, address, operations)
    }
}

/// Send-safe SPI adapter exposed through the async ecosystem contract.
pub struct ExposedSpiBus(Spi<'static, Blocking>);

impl embedded_hal::spi::ErrorType for ExposedSpiBus {
    type Error = esp_hal::spi::Error;
}

impl embedded_hal_async::spi::SpiBus for ExposedSpiBus {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::read(&mut self.0, words)
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::write(&mut self.0, words)
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer(&mut self.0, read, write)
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer_in_place(&mut self.0, words)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::flush(&mut self.0)
    }
}

/// Send-safe UART adapter exposed through the async byte-stream contract.
pub struct ExposedUart(Uart<'static, Blocking>);

impl embedded_io::ErrorType for ExposedUart {
    type Error = esp_hal::uart::IoError;
}

impl embedded_io_async::Read for ExposedUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io::Read::read(&mut self.0, buffer)
    }
}

impl embedded_io_async::Write for ExposedUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        embedded_io::Write::write(&mut self.0, buffer)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io::Write::flush(&mut self.0)
    }
}
/// Delay provider used during peripheral Driver initialization.
pub type DriverDelay = Delay;
/// SPI configuration failure surfaced by generated Board initialization.
pub type SpiConfigError = SpiConfigErrorInner;
/// I2C configuration failure surfaced by generated Board initialization.
pub type I2cConfigError = I2cConfigErrorInner;

/// Failure while configuring a runtime ESP UART.
#[derive(Debug)]
pub enum ExposedUartConfigError {
    /// The ESP HAL rejected the requested line configuration.
    Peripheral(UartConfigError),
    /// ESP UART hardware does not support nine data bits.
    UnsupportedDataBits,
}

impl core::fmt::Display for ExposedUartConfigError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Peripheral(error) => error.fmt(formatter),
            Self::UnsupportedDataBits => formatter.write_str("ESP UART supports 7 or 8 data bits"),
        }
    }
}

impl core::error::Error for ExposedUartConfigError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Peripheral(error) => Some(error),
            Self::UnsupportedDataBits => None,
        }
    }
}

/// Type-erased Platform adapter used by generated runtime I/O composition.
pub struct RuntimeAdapter;

impl RuntimePlatform for RuntimeAdapter {
    type PinToken = AnyPin<'static>;
    type DigitalPin = DynamicPin;
    type I2cController = AnyI2c<'static>;
    type I2cBus = ExposedI2cBus;
    type I2cError = I2cConfigError;
    type SpiController = AnySpi<'static>;
    type SpiBus = ExposedSpiBus;
    type SpiError = SpiConfigError;
    type UartController = AnyUart<'static>;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        DynamicPin {
            pin: Flex::new(pin),
        }
    }

    fn i2c(
        controller: Self::I2cController,
        scl: Self::PinToken,
        sda: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError> {
        exposed_i2c(controller, scl, sda, frequency_hz)
    }

    fn spi(
        controller: Self::SpiController,
        sck: Self::PinToken,
        mosi: Option<Self::PinToken>,
        miso: Option<Self::PinToken>,
        frequency_hz: u32,
        mode: EmbeddedMode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        let mode = match (mode.polarity, mode.phase) {
            (Polarity::IdleLow, Phase::CaptureOnFirstTransition) => HalSpiMode::_0,
            (Polarity::IdleLow, Phase::CaptureOnSecondTransition) => HalSpiMode::_1,
            (Polarity::IdleHigh, Phase::CaptureOnFirstTransition) => HalSpiMode::_2,
            (Polarity::IdleHigh, Phase::CaptureOnSecondTransition) => HalSpiMode::_3,
        };
        let bus = Spi::new(
            controller,
            SpiConfig::default()
                .with_frequency(Rate::from_hz(frequency_hz))
                .with_mode(mode),
        )?
        .with_sck(sck);
        let bus = if let Some(mosi) = mosi {
            bus.with_mosi(mosi)
        } else {
            bus
        };
        let bus = if let Some(miso) = miso {
            bus.with_miso(miso)
        } else {
            bus
        };
        Ok(ExposedSpiBus(bus))
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
    type Uart = ExposedUart;
    type UartError = ExposedUartConfigError;

    fn supports_uart(
        _controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
    ) -> bool {
        tx.is_some() || rx.is_some()
    }

    fn uart(
        controller: Self::UartController,
        tx: Option<Self::PinToken>,
        rx: Option<Self::PinToken>,
        config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError> {
        let data_bits = match config.data_bits {
            UartDataBits::Seven => DataBits::_7,
            UartDataBits::Eight => DataBits::_8,
            UartDataBits::Nine => return Err(ExposedUartConfigError::UnsupportedDataBits),
        };
        let parity = match config.parity {
            UartParity::None => Parity::None,
            UartParity::Even => Parity::Even,
            UartParity::Odd => Parity::Odd,
        };
        let stop_bits = match config.stop_bits {
            UartStopBits::One => StopBits::_1,
            UartStopBits::Two => StopBits::_2,
        };
        let config = HalUartConfig::default()
            .with_baudrate(config.baud)
            .with_data_bits(data_bits)
            .with_parity(parity)
            .with_stop_bits(stop_bits);
        let uart = Uart::new(controller, config).map_err(ExposedUartConfigError::Peripheral)?;
        let uart = if let Some(tx) = tx {
            uart.with_tx(tx)
        } else {
            uart
        };
        let uart = if let Some(rx) = rx {
            uart.with_rx(rx)
        } else {
            uart
        };
        Ok(ExposedUart(uart))
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

/// Generated exposed-I/O owner specialized to the ESP Platform adapter.
pub type RuntimeIo<const P: usize, const I: usize, const S: usize, const U: usize> =
    barracuda_board_hal::RuntimeIo<RuntimeAdapter, P, I, S, U>;

/// Constructs the selected Board's unified runtime I/O owner.
#[must_use]
pub fn runtime_io<const P: usize, const I: usize, const S: usize, const U: usize>(
    pins: [(&'static str, AnyPin<'static>); P],
    i2c: [AnyI2c<'static>; I],
    spi: [AnySpi<'static>; S],
    uart: [AnyUart<'static>; U],
) -> RuntimeIo<P, I, S, U> {
    RuntimeIo::new_with_uart(pins, i2c, spi, uart)
}

/// Erases one selected pin token while preserving its exclusive ownership.
#[must_use]
pub fn runtime_pin(pin: impl Pin + 'static) -> AnyPin<'static> {
    pin.degrade()
}

/// Erases one selected I2C controller token for the runtime pool.
#[must_use]
pub fn runtime_i2c_controller(i2c: impl I2cInstance + 'static) -> AnyI2c<'static> {
    i2c.degrade()
}

/// Erases one selected SPI controller token for the runtime pool.
#[must_use]
pub fn runtime_spi_controller(spi: impl SpiInstance + 'static) -> AnySpi<'static> {
    spi.degrade()
}

/// Erases one selected UART controller token for the runtime pool.
#[must_use]
pub fn runtime_uart_controller(uart: impl UartInstance + 'static) -> AnyUart<'static> {
    uart.degrade()
}

/// Converts a selected raw pin token into a digital output.
#[must_use]
pub fn digital_output(pin: impl OutputPin + 'static, initial: DigitalLevel) -> DigitalOutput {
    let level = match initial {
        DigitalLevel::Low => Level::Low,
        DigitalLevel::High => Level::High,
    };
    Output::new(pin, level, HalOutputConfig::default())
}

/// Converts a selected raw pin token into a digital input.
#[must_use]
pub fn digital_input(pin: impl InputPin + 'static) -> DigitalInput {
    Input::new(pin, HalInputConfig::default())
}

/// Runtime-configurable ESP32 GPIO exposed through `embedded-hal`.
pub struct DynamicPin {
    pin: Flex<'static>,
}

/// Converts a selected raw token into a runtime-configurable GPIO.
#[must_use]
pub fn dynamic_pin(pin: impl InputPin + OutputPin + 'static) -> DynamicPin {
    DynamicPin {
        pin: Flex::new(pin),
    }
}

impl ErrorType for DynamicPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::InputPin for DynamicPin {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        embedded_hal::digital::InputPin::is_high(&mut self.pin)
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        embedded_hal::digital::InputPin::is_low(&mut self.pin)
    }
}

impl embedded_hal::digital::OutputPin for DynamicPin {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        embedded_hal::digital::OutputPin::set_low(&mut self.pin)
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        embedded_hal::digital::OutputPin::set_high(&mut self.pin)
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
        self.pin.set_output_enable(false);
        self.pin
            .apply_input_config(&HalInputConfig::default().with_pull(pull));
        self.pin.set_input_enable(true);
        Ok(())
    }

    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error> {
        match config.initial {
            DigitalLevel::Low => embedded_hal::digital::OutputPin::set_low(&mut self.pin)?,
            DigitalLevel::High => embedded_hal::digital::OutputPin::set_high(&mut self.pin)?,
        }
        let output = match config.drive {
            OutputDrive::PushPull => HalOutputConfig::default(),
            OutputDrive::OpenDrain => {
                HalOutputConfig::default().with_drive_mode(DriveMode::OpenDrain)
            }
        };
        self.pin.apply_output_config(&output);
        self.pin
            .set_input_enable(config.drive == OutputDrive::OpenDrain);
        self.pin.set_output_enable(true);
        Ok(())
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.pin.set_output_enable(false);
        self.pin.set_input_enable(false);
        Ok(())
    }
}

/// Constructs one SPI device from Board-selected controller and pins.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi);
    Ok(exclusive_spi_device(bus, chip_select))
}

/// Constructs one transmit-only SPI bus from Board-selected resources.
pub fn spi_bus(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    Ok(Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi))
}

/// Constructs one receive-only SPI bus from Board-selected resources.
pub fn spi_bus_rx_only(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    Ok(Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_miso(miso))
}

/// Constructs one full-duplex SPI bus from Board-selected resources.
pub fn spi_bus_full_duplex(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    Ok(Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso))
}

/// Constructs one receive-only SPI device from Board-selected resources.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device_rx_only(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_miso(miso);
    Ok(exclusive_spi_device(bus, chip_select))
}

/// Constructs one full-duplex SPI device from Board-selected resources.
///
/// # Errors
///
/// Returns [`SpiConfigError`] when the requested frequency is unsupported.
pub fn spi_device_full_duplex(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    chip_select: impl OutputPin + 'static,
    frequency_hz: u32,
) -> Result<SpiDevice, SpiConfigError> {
    let bus = Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_sck(sck)
    .with_mosi(mosi)
    .with_miso(miso);
    Ok(exclusive_spi_device(bus, chip_select))
}

fn exclusive_spi_device(bus: SpiBus, chip_select: impl OutputPin + 'static) -> SpiDevice {
    let chip_select = digital_output(chip_select, DigitalLevel::High);
    match ExclusiveDevice::new(bus, chip_select, Delay::new()) {
        Ok(device) => device,
        Err(never) => match never {},
    }
}

/// Constructs an I2C bus from Board-selected controller and pins.
///
/// # Errors
///
/// Returns [`I2cConfigError`] when the requested frequency is unsupported.
pub fn i2c_device(
    i2c: impl I2cInstance + 'static,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<I2cBus, I2cConfigError> {
    Ok(I2c::new(
        i2c,
        I2cConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_scl(scl)
    .with_sda(sda))
}

/// Constructs an async I2C controller explicitly exposed by the Board.
pub fn exposed_i2c(
    i2c: impl I2cInstance + 'static,
    scl: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    sda: impl PeripheralInput<'static> + PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<ExposedI2cBus, I2cConfigError> {
    Ok(ExposedI2cBus(i2c_device(i2c, scl, sda, frequency_hz)?))
}

/// Constructs a transmit-only async SPI bus explicitly exposed by the Board.
pub fn exposed_spi_bus(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<ExposedSpiBus, SpiConfigError> {
    Ok(ExposedSpiBus(spi_bus(spi, sck, mosi, frequency_hz)?))
}

/// Constructs a receive-only async SPI bus explicitly exposed by the Board.
pub fn exposed_spi_bus_rx_only(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<ExposedSpiBus, SpiConfigError> {
    Ok(ExposedSpiBus(spi_bus_rx_only(
        spi,
        sck,
        miso,
        frequency_hz,
    )?))
}

/// Constructs a full-duplex async SPI bus explicitly exposed by the Board.
pub fn exposed_spi_bus_full_duplex(
    spi: impl SpiInstance + 'static,
    sck: impl PeripheralOutput<'static> + 'static,
    mosi: impl PeripheralOutput<'static> + 'static,
    miso: impl PeripheralInput<'static> + 'static,
    frequency_hz: u32,
) -> Result<ExposedSpiBus, SpiConfigError> {
    Ok(ExposedSpiBus(spi_bus_full_duplex(
        spi,
        sck,
        mosi,
        miso,
        frequency_hz,
    )?))
}

/// Constructs the delay provider used by synchronous peripheral Drivers.
#[must_use]
pub fn delay() -> DriverDelay {
    Delay::new()
}

/// Resolves a Board-selected pin name to its move-only ESP HAL token type.
#[macro_export]
macro_rules! __barracuda_esp32_pin_binding_type {
    ($pin:ident) => {
        $crate::hal::__vendor::peripherals::$pin<'static>
    };
}

/// Resolves a Board-selected controller name to its move-only ESP HAL token type.
#[macro_export]
macro_rules! __barracuda_esp32_controller_binding_type {
    ($peripheral:ident) => {
        $crate::hal::__vendor::peripherals::$peripheral<'static>
    };
}

#[doc(hidden)]
pub use __barracuda_esp32_controller_binding_type as controller_binding_type;
#[doc(hidden)]
pub use __barracuda_esp32_pin_binding_type as pin_binding_type;
