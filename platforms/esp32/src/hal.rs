// Shared ESP vendor adaptation instantiated inside each supported Platform.

extern crate alloc;

use alloc::boxed::Box;

use barracuda_board_hal::{
    AnalogErrorType, AnalogInput, ConfigurableDigitalPin, DigitalLevel, InputConfig, OutputConfig,
    OutputDrive, Pull as BoardPull, RuntimeAnalogPlatform, RuntimePlatform, RuntimePwmPlatform,
    RuntimeUartPlatform, SharedI2cBus, UartConfig, UartDataBits, UartParity, UartStopBits,
    UnavailableAnalogOutput,
};
use embedded_hal::{
    digital::{ErrorType, StatefulOutputPin},
    i2c::Operation as I2cOperation,
    spi::{Mode as EmbeddedMode, Phase, Polarity},
};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    analog::adc::{Adc, AdcChannel, AdcConfig, AdcPin, Attenuation},
    delay::Delay,
    gpio::{
        interconnect::{PeripheralInput, PeripheralOutput},
        AnalogPin, AnyPin, DriveMode, Flex, Input, InputConfig as HalInputConfig, InputPin, Level,
        Output, OutputConfig as HalOutputConfig, OutputPin, Pin, Pull,
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

#[cfg(not(feature = "runtime-pwm"))]
use barracuda_board_hal::UnavailablePwm;
#[cfg(feature = "runtime-pwm")]
use esp_hal::ledc::{
    channel::{Channel, ChannelIFace, Error as LedcChannelError, Number as LedcChannelNumber},
    timer::{
        config::{Config as LedcTimerConfig, Duty as LedcDuty},
        Error as LedcTimerError, LSClockSource, Number as LedcTimerNumber, Timer, TimerIFace,
    },
    Ledc, LowSpeed,
};

#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

#[cfg(feature = "runtime-i2s")]
type PlatformI2sResource = RuntimeI2sResource;
#[cfg(not(feature = "runtime-i2s"))]
type PlatformI2sResource = core::convert::Infallible;

#[cfg(feature = "runtime-pwm")]
type PlatformPwmResource = RuntimePwmResource;
#[cfg(not(feature = "runtime-pwm"))]
type PlatformPwmResource = core::convert::Infallible;

#[doc(hidden)]
pub use esp_hal as __vendor;

/// Type-erased push-pull output consumed by peripheral implementations.
pub type DigitalOutput = Output<'static>;
/// Type-erased digital input consumed by peripheral implementations.
pub type DigitalInput = Input<'static>;
/// Blocking SPI bus used by statically selected devices.
pub type SpiBus = Spi<'static, Blocking>;
/// Standards-compliant single-owner SPI device.
pub type SpiDevice = ExclusiveDevice<SpiBus, DigitalOutput, Delay>;
/// Blocking I2C controller passed to a peripheral implementation.
pub type I2cBus = I2c<'static, Blocking>;
/// Static owner of one I2C bus shared by peripheral implementations.
pub type I2cBusManager = SharedI2cBus<I2cBus>;
/// Blocking I2C view handed to one peripheral implementation.
pub type I2cDevice = embedded_hal_bus::i2c::CriticalSectionDevice<'static, I2cBus>;

/// Statically allocates one shared built-in I2C bus owner at the Board call site.
#[macro_export]
macro_rules! __barracuda_esp_i2c_bus_manager {
    ($bus:expr) => {{
        static MANAGER: $crate::hal::__StaticCell<$crate::hal::I2cBusManager> =
            $crate::hal::__StaticCell::new();
        MANAGER.init($crate::hal::I2cBusManager::new($bus))
    }};
}

#[doc(hidden)]
pub use __barracuda_esp_i2c_bus_manager as i2c_bus_manager;
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

/// ESP LEDC channel exposed through the standard PWM contract.
#[cfg(feature = "runtime-pwm")]
pub type ExposedPwm = Channel<'static, LowSpeed>;
/// Uninhabited PWM output on ESP targets without a vendor LEDC driver.
#[cfg(not(feature = "runtime-pwm"))]
pub type ExposedPwm = UnavailablePwm;

/// Statically allocated timer descriptor generated from a Platform manifest.
#[cfg(feature = "runtime-pwm")]
#[derive(Clone, Copy)]
pub struct RuntimePwmTimer {
    number: LedcTimerNumber,
    storage: &'static __StaticCell<Timer<'static, LowSpeed>>,
}

#[cfg(feature = "runtime-pwm")]
impl RuntimePwmTimer {
    /// Creates one move-only runtime timer descriptor.
    #[must_use]
    pub const fn new(
        number: LedcTimerNumber,
        storage: &'static __StaticCell<Timer<'static, LowSpeed>>,
    ) -> Self {
        Self { number, storage }
    }
}

/// Platform-owned LEDC controller, timer, and channel allocation.
#[cfg(feature = "runtime-pwm")]
pub struct RuntimePwmResource {
    controller: esp_hal::peripherals::LEDC<'static>,
    timer: Option<RuntimePwmTimer>,
    channel: Option<LedcChannelNumber>,
}

type AdcFactory = fn(
    esp_hal::peripherals::ADC1<'static>,
    AnyPin<'static>,
) -> Result<ExposedAnalogInput, ExposedAnalogError>;

/// One typed ADC channel route declared by the Platform manifest.
#[derive(Clone, Copy)]
pub struct RuntimeAdcChannel {
    pin_number: u8,
    factory: AdcFactory,
}

/// Platform-owned ADC1 controller and its statically valid channel routes.
pub struct RuntimeAdcResource {
    controller: esp_hal::peripherals::ADC1<'static>,
    channels: [Option<RuntimeAdcChannel>; 10],
}

/// Failure while constructing or sampling an exposed ESP ADC input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposedAnalogError {
    /// The selected exposed pin does not match the statically declared channel.
    InvalidPin,
    /// The ADC reported a conversion failure.
    Conversion,
}

impl core::fmt::Display for ExposedAnalogError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidPin => formatter.write_str("invalid ESP ADC channel route"),
            Self::Conversion => formatter.write_str("ESP ADC conversion failed"),
        }
    }
}

impl core::error::Error for ExposedAnalogError {}

/// Type-erased, typed-channel ADC input used by the synchronous Lua handle.
pub struct ExposedAnalogInput(Box<dyn AnalogInput<Error = ExposedAnalogError>>);

impl AnalogErrorType for ExposedAnalogInput {
    type Error = ExposedAnalogError;
}

impl AnalogInput for ExposedAnalogInput {
    fn max_value(&self) -> u32 {
        self.0.max_value()
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        self.0.read()
    }
}

struct TypedAdcInput<P> {
    adc: Adc<'static, esp_hal::peripherals::ADC1<'static>, Blocking>,
    pin: AdcPin<P, esp_hal::peripherals::ADC1<'static>>,
}

impl<P> AnalogErrorType for TypedAdcInput<P> {
    type Error = ExposedAnalogError;
}

impl<P> AnalogInput for TypedAdcInput<P>
where
    P: AdcChannel,
{
    fn max_value(&self) -> u32 {
        4095
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        loop {
            match self.adc.read_oneshot(&mut self.pin) {
                Ok(value) => return Ok(u32::from(value)),
                Err(nb::Error::WouldBlock) => {}
                Err(nb::Error::Other(())) => return Err(ExposedAnalogError::Conversion),
            }
        }
    }
}

/// Failure while constructing an exposed ESP LEDC output.
#[cfg(feature = "runtime-pwm")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExposedPwmError {
    /// The manifest did not provide a timer and channel pair.
    MissingResource,
    /// None of the supported duty resolutions can produce the requested frequency.
    Timer(LedcTimerError),
    /// The LEDC channel rejected its timer or output configuration.
    Channel(LedcChannelError),
}

#[cfg(feature = "runtime-pwm")]
impl core::fmt::Display for ExposedPwmError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingResource => formatter.write_str("missing ESP LEDC timer or channel"),
            Self::Timer(_) => formatter.write_str("invalid ESP LEDC timer configuration"),
            Self::Channel(_) => formatter.write_str("invalid ESP LEDC channel configuration"),
        }
    }
}

#[cfg(feature = "runtime-pwm")]
impl core::error::Error for ExposedPwmError {}

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
/// Delay provider used during peripheral initialization.
pub type PeripheralDelay = Delay;
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
    type AdcResource = RuntimeAdcResource;
    type PwmResource = PlatformPwmResource;
    type I2sResource = PlatformI2sResource;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        DynamicPin {
            pin: Flex::new(pin),
        }
    }

    fn supports_i2c_config(
        _controller: &Self::I2cController,
        _scl: &Self::PinToken,
        _sda: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0
    }

    fn i2c(
        controller: Self::I2cController,
        scl: Self::PinToken,
        sda: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError> {
        exposed_i2c(controller, scl, sda, frequency_hz)
    }

    fn supports_spi_config(
        _controller: &Self::SpiController,
        _sck: &Self::PinToken,
        mosi: Option<&Self::PinToken>,
        miso: Option<&Self::PinToken>,
        frequency_hz: u32,
        _mode: EmbeddedMode,
    ) -> bool {
        frequency_hz > 0 && (mosi.is_some() || miso.is_some())
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
    type AnalogInput = ExposedAnalogInput;
    type AnalogOutput = UnavailableAnalogOutput;
    type AnalogError = ExposedAnalogError;

    fn supports_analog_input(resource: &Self::AdcResource, pin: &Self::PinToken) -> bool {
        resource
            .channels
            .iter()
            .flatten()
            .any(|channel| channel.pin_number == pin.number())
    }

    fn supports_analog_output(_pin: &Self::PinToken) -> bool {
        false
    }

    fn analog_input(
        resource: Self::AdcResource,
        pin: Self::PinToken,
    ) -> Result<Self::AnalogInput, Self::AnalogError> {
        let factory = resource
            .channels
            .iter()
            .flatten()
            .find(|channel| channel.pin_number == pin.number())
            .map(|channel| channel.factory)
            .ok_or(ExposedAnalogError::InvalidPin)?;
        factory(resource.controller, pin)
    }

    fn analog_output(_pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Err(ExposedAnalogError::InvalidPin)
    }
}

#[cfg(feature = "runtime-pwm")]
impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = ExposedPwm;
    type PwmError = ExposedPwmError;

    fn supports_pwm(resource: &Self::PwmResource, _pin: &Self::PinToken) -> bool {
        resource.timer.is_some() && resource.channel.is_some()
    }

    fn supports_pwm_config(
        resource: &Self::PwmResource,
        pin: &Self::PinToken,
        frequency_hz: u32,
    ) -> bool {
        frequency_hz > 0 && Self::supports_pwm(resource, pin)
    }

    fn pwm(
        mut resource: Self::PwmResource,
        pin: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        let timer = resource
            .timer
            .take()
            .ok_or(ExposedPwmError::MissingResource)?;
        let channel_number = resource
            .channel
            .take()
            .ok_or(ExposedPwmError::MissingResource)?;
        let ledc = Ledc::new(resource.controller);
        let mut configured_timer = ledc.timer::<LowSpeed>(timer.number);
        configure_ledc_timer(&mut configured_timer, frequency_hz)?;
        let configured_timer = timer.storage.init(configured_timer);
        let mut channel = ledc.channel::<LowSpeed>(channel_number, pin);
        channel
            .configure(esp_hal::ledc::channel::config::Config {
                timer: configured_timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .map_err(ExposedPwmError::Channel)?;
        Ok(channel)
    }
}

#[cfg(not(feature = "runtime-pwm"))]
impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = ExposedPwm;
    type PwmError = core::convert::Infallible;

    fn supports_pwm(_resource: &Self::PwmResource, _pin: &Self::PinToken) -> bool {
        false
    }

    fn supports_pwm_config(
        _resource: &Self::PwmResource,
        _pin: &Self::PinToken,
        _frequency_hz: u32,
    ) -> bool {
        false
    }

    fn pwm(
        resource: Self::PwmResource,
        _pin: Self::PinToken,
        _frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError> {
        match resource {}
    }
}

#[cfg(feature = "runtime-pwm")]
fn configure_ledc_timer(
    timer: &mut Timer<'static, LowSpeed>,
    frequency_hz: u32,
) -> Result<(), ExposedPwmError> {
    let duties = [
        LedcDuty::Duty14Bit,
        LedcDuty::Duty13Bit,
        LedcDuty::Duty12Bit,
        LedcDuty::Duty11Bit,
        LedcDuty::Duty10Bit,
        LedcDuty::Duty9Bit,
        LedcDuty::Duty8Bit,
        LedcDuty::Duty7Bit,
        LedcDuty::Duty6Bit,
        LedcDuty::Duty5Bit,
        LedcDuty::Duty4Bit,
        LedcDuty::Duty3Bit,
        LedcDuty::Duty2Bit,
        LedcDuty::Duty1Bit,
    ];
    let mut last_error = LedcTimerError::Divisor;
    for duty in duties {
        match timer.configure(LedcTimerConfig {
            duty,
            clock_source: LSClockSource::APBClk,
            frequency: Rate::from_hz(frequency_hz),
        }) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(ExposedPwmError::Timer(last_error))
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

#[cfg(not(feature = "runtime-i2s"))]
impl ::barracuda_board_hal::RuntimeI2sPlatform for RuntimeAdapter {
    type I2s = ::barracuda_board_hal::UnavailableI2s;
    type I2sError = ::barracuda_board_hal::UnsupportedFunction;

    fn supports_i2s(
        _resource: &Self::I2sResource,
        _bclk: &Self::PinToken,
        _ws: &Self::PinToken,
        _dout: Option<&Self::PinToken>,
        _din: Option<&Self::PinToken>,
        _mclk: Option<&Self::PinToken>,
    ) -> bool {
        false
    }

    fn i2s(
        resource: Self::I2sResource,
        _bclk: Self::PinToken,
        _ws: Self::PinToken,
        _dout: Option<Self::PinToken>,
        _din: Option<Self::PinToken>,
        _mclk: Option<Self::PinToken>,
        _format: ::barracuda_board_hal::audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        match resource {}
    }
}

/// Generated exposed-I/O owner specialized to the ESP Platform adapter.
pub type RuntimeIo<
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
> = barracuda_board_hal::RuntimeIo<RuntimeAdapter, P, I, S, U, A, W, T>;

/// Constructs the selected Board's unified runtime I/O owner.
#[must_use]
pub fn runtime_io<
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
>(
    pins: [(&'static str, AnyPin<'static>); P],
    i2c: [AnyI2c<'static>; I],
    spi: [AnySpi<'static>; S],
    uart: [AnyUart<'static>; U],
    adc: [RuntimeAdcResource; A],
    pwm: [PlatformPwmResource; W],
    i2s: [PlatformI2sResource; T],
) -> RuntimeIo<P, I, S, U, A, W, T> {
    RuntimeIo::new_with_resources(pins, i2c, spi, uart, adc, pwm, i2s)
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

/// Builds one Platform-owned ESP ADC1 allocation from generated channel routes.
#[must_use]
pub fn runtime_adc_resource(
    controller: esp_hal::peripherals::ADC1<'static>,
    channels: &[RuntimeAdcChannel],
) -> RuntimeAdcResource {
    let mut declared = [None; 10];
    for (target, channel) in declared.iter_mut().zip(channels.iter().copied()) {
        *target = Some(channel);
    }
    RuntimeAdcResource {
        controller,
        channels: declared,
    }
}

/// Creates one typed ADC route without erasing the vendor pin/channel identity.
#[must_use]
pub fn runtime_adc_channel_for<P>(pin_number: u8) -> RuntimeAdcChannel
where
    P: Pin + AnalogPin + AdcChannel + 'static,
    AnyPin<'static>: TryInto<P, Error = AnyPin<'static>>,
{
    RuntimeAdcChannel {
        pin_number,
        factory: construct_adc_input::<P>,
    }
}

fn construct_adc_input<P>(
    controller: esp_hal::peripherals::ADC1<'static>,
    pin: AnyPin<'static>,
) -> Result<ExposedAnalogInput, ExposedAnalogError>
where
    P: Pin + AnalogPin + AdcChannel + 'static,
    AnyPin<'static>: TryInto<P, Error = AnyPin<'static>>,
{
    let pin = pin
        .downcast::<P>()
        .map_err(|_| ExposedAnalogError::InvalidPin)?;
    let mut config = AdcConfig::new();
    let pin = config.enable_pin(pin, Attenuation::_11dB);
    let adc = Adc::new(controller, config);
    Ok(ExposedAnalogInput(Box::new(TypedAdcInput { adc, pin })))
}

/// Resolves the typed ADC channel and pin pair from a Platform manifest.
#[macro_export]
macro_rules! __barracuda_esp32_runtime_adc_channel {
    ($channel:ident, $pin:ident) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::$pin<'static>>(
            $crate::hal::runtime_gpio_number!($pin),
        )
    };
}

/// Resolves an ESP GPIO singleton name to its hardware number.
#[macro_export]
macro_rules! __barracuda_esp32_runtime_gpio_number {
    (GPIO0) => {
        0
    };
    (GPIO1) => {
        1
    };
    (GPIO2) => {
        2
    };
    (GPIO3) => {
        3
    };
    (GPIO4) => {
        4
    };
    (GPIO5) => {
        5
    };
    (GPIO6) => {
        6
    };
    (GPIO7) => {
        7
    };
    (GPIO8) => {
        8
    };
    (GPIO9) => {
        9
    };
    (GPIO10) => {
        10
    };
    (GPIO16) => {
        16
    };
    (GPIO17) => {
        17
    };
    (GPIO18) => {
        18
    };
    (GPIO19) => {
        19
    };
    (GPIO20) => {
        20
    };
    (GPIO21) => {
        21
    };
    (GPIO22) => {
        22
    };
    (GPIO23) => {
        23
    };
    (GPIO32) => {
        32
    };
    (GPIO33) => {
        33
    };
    (GPIO34) => {
        34
    };
    (GPIO35) => {
        35
    };
    (GPIO36) => {
        36
    };
    (GPIO37) => {
        37
    };
    (GPIO38) => {
        38
    };
    (GPIO39) => {
        39
    };
}

#[doc(hidden)]
pub use __barracuda_esp32_runtime_adc_channel as runtime_adc_channel;
#[doc(hidden)]
pub use __barracuda_esp32_runtime_gpio_number as runtime_gpio_number;

/// Builds one Platform-owned ESP LEDC allocation from generated descriptors.
#[must_use]
#[cfg(feature = "runtime-pwm")]
pub fn runtime_pwm_resource(
    controller: esp_hal::peripherals::LEDC<'static>,
    timers: &[RuntimePwmTimer],
    channels: &[LedcChannelNumber],
) -> RuntimePwmResource {
    RuntimePwmResource {
        controller,
        timer: timers.first().copied(),
        channel: channels.first().copied(),
    }
}

/// Resolves one manifest timer name and allocates its static vendor storage.
#[macro_export]
#[cfg(feature = "runtime-pwm")]
macro_rules! __barracuda_esp32_runtime_pwm_timer {
    ($timer:ident) => {{
        static STORAGE: $crate::hal::__StaticCell<
            $crate::hal::__vendor::ledc::timer::Timer<
                'static,
                $crate::hal::__vendor::ledc::LowSpeed,
            >,
        > = $crate::hal::__StaticCell::new();
        $crate::hal::RuntimePwmTimer::new(
            $crate::hal::__vendor::ledc::timer::Number::$timer,
            &STORAGE,
        )
    }};
}

/// Resolves one manifest LEDC channel name.
#[macro_export]
#[cfg(feature = "runtime-pwm")]
macro_rules! __barracuda_esp32_runtime_pwm_channel {
    ($channel:ident) => {
        $crate::hal::__vendor::ledc::channel::Number::$channel
    };
}

#[doc(hidden)]
#[cfg(feature = "runtime-pwm")]
pub use __barracuda_esp32_runtime_pwm_channel as runtime_pwm_channel;
#[doc(hidden)]
#[cfg(feature = "runtime-pwm")]
pub use __barracuda_esp32_runtime_pwm_timer as runtime_pwm_timer;

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

/// Constructs one data-only SPI waveform output from a Platform-owned controller.
pub fn spi_output(
    spi: impl SpiInstance + 'static,
    data: impl PeripheralOutput<'static> + 'static,
    frequency_hz: u32,
) -> Result<SpiBus, SpiConfigError> {
    Ok(Spi::new(
        spi,
        SpiConfig::default().with_frequency(Rate::from_hz(frequency_hz)),
    )?
    .with_mosi(data))
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

/// Constructs the delay provider used by synchronous peripheral implementations.
#[must_use]
pub fn delay() -> PeripheralDelay {
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
