//! STM32 adaptation from Board-selected tokens to embedded-hal resources.

extern crate alloc;

use alloc::boxed::Box;
use core::{any::Any, convert::Infallible, fmt};

use barracuda_board_hal::{
    audio, AnalogErrorType, AnalogInput, ConfigurableDigitalPin, DigitalLevel, InputConfig,
    OutputConfig, OutputDrive, Pull as BoardPull, RuntimeAnalogPlatform, RuntimeI2sPlatform,
    RuntimePlatform, RuntimePwmPlatform, RuntimeUartPlatform, UartConfig, UartDataBits, UartParity,
    UartStopBits, UnavailableAnalogOutput,
};
use embassy_stm32::{
    adc::{Adc, AdcChannel, AnyAdcChannel, SampleTime},
    gpio::{AnyPin, Flex, Input, Level, Output, OutputType, Pin, Pull, Speed},
    i2c::{I2c, Master as I2cMaster},
    mode::Blocking,
    peripherals::{
        ADC1, DMA1_CH3, DMA1_CH4, I2C1, PA10, PA5, PA6, PA7, PA8, PA9, PB12, PB13, PB15, PB6, PB7,
        PB8, PB9, PC6, SPI1, SPI2, TIM1, USART1,
    },
    spi::Spi,
    time::Hertz,
    timer::{
        low_level::CountingMode,
        simple_pwm::{PwmPin, SimplePwm},
        Ch1,
    },
    usart::{
        Config as HalUartConfig, ConfigError as HalUartConfigError, DataBits, Parity, StopBits,
        Uart,
    },
    Peri,
};
use embedded_hal::{
    digital::{ErrorType, InputPin, OutputPin, StatefulOutputPin},
    i2c::Operation as I2cOperation,
    spi::Mode as EmbeddedMode,
};
use static_cell::StaticCell;

#[doc(hidden)]
pub use static_cell::StaticCell as __StaticCell;

#[doc(hidden)]
pub use embassy_stm32 as __vendor;

/// Type-erased push-pull output consumed by peripheral Drivers.
pub type DigitalOutput = Output<'static>;
/// Type-erased digital input consumed by peripheral Drivers.
pub type DigitalInput = Input<'static>;
/// Blocking I2C1 bus consumed by statically selected peripheral Drivers.
pub type I2cBus = I2c<'static, Blocking, I2cMaster>;
/// Blocking SPI1 bus consumed by statically selected peripheral Drivers.
pub type SpiBus = Spi<'static, Blocking, embassy_stm32::spi::mode::Master>;

/// Move-only STM32 pin token retaining the concrete singleton type.
pub struct RuntimePinToken {
    token: Box<dyn ErasedPin>,
}

impl RuntimePinToken {
    fn is<P: Pin + Send>(&self) -> bool {
        self.token.as_any().is::<OwnedPin<P>>()
    }

    fn downcast<P: Pin + Send>(self) -> Result<Peri<'static, P>, RouteError> {
        self.token
            .into_box_any()
            .downcast::<OwnedPin<P>>()
            .map(|pin| pin.0)
            .map_err(|_token| RouteError::InvalidPinRoute)
    }

    fn degrade(self) -> Peri<'static, AnyPin> {
        self.token.into_any()
    }
}

trait ErasedPin: Send {
    fn as_any(&self) -> &dyn Any;
    fn into_box_any(self: Box<Self>) -> Box<dyn Any + Send>;
    fn into_any(self: Box<Self>) -> Peri<'static, AnyPin>;
}

struct OwnedPin<P: Pin + Send>(Peri<'static, P>);

impl<P: Pin + Send> ErasedPin for OwnedPin<P> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_box_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }

    fn into_any(self: Box<Self>) -> Peri<'static, AnyPin> {
        let Self(pin) = *self;
        pin.into()
    }
}

/// Invalid runtime route selected for one STM32 peripheral.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// One or more pins are not valid for the selected controller.
    InvalidPinRoute,
    /// A zero frequency is not a valid peripheral clock request.
    InvalidFrequency,
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPinRoute => formatter.write_str("invalid STM32 peripheral pin route"),
            Self::InvalidFrequency => {
                formatter.write_str("STM32 peripheral frequency must be nonzero")
            }
        }
    }
}

impl core::error::Error for RouteError {}

/// Blocking STM32 I2C driver exposed through the async ecosystem contract.
pub struct ExposedI2cBus(I2c<'static, Blocking, I2cMaster>);

impl embedded_hal::i2c::ErrorType for ExposedI2cBus {
    type Error = embassy_stm32::i2c::Error;
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

/// Blocking STM32 SPI driver exposed through the async ecosystem contract.
pub struct ExposedSpiBus(
    Box<dyn embedded_hal::spi::SpiBus<u8, Error = embassy_stm32::spi::Error> + Send>,
);

impl embedded_hal::spi::ErrorType for ExposedSpiBus {
    type Error = embassy_stm32::spi::Error;
}

impl embedded_hal_async::spi::SpiBus for ExposedSpiBus {
    async fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::read(&mut *self.0, words)
    }

    async fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::write(&mut *self.0, words)
    }

    async fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer(&mut *self.0, read, write)
    }

    async fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::transfer_in_place(&mut *self.0, words)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_hal::spi::SpiBus::flush(&mut *self.0)
    }
}

/// STM32 USART construction failure.
#[derive(Debug)]
pub enum ExposedUartConfigError {
    /// The requested pins are not a valid USART1 route.
    InvalidPinRoute,
    /// At least one of TX or RX must be supplied.
    MissingDirection,
    /// The STM32 HAL rejected the line configuration.
    Peripheral(HalUartConfigError),
}

impl fmt::Display for ExposedUartConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPinRoute => formatter.write_str("invalid STM32 USART pin route"),
            Self::MissingDirection => formatter.write_str("STM32 USART requires TX or RX"),
            Self::Peripheral(error) => write!(formatter, "{error:?}"),
        }
    }
}

impl core::error::Error for ExposedUartConfigError {}

/// Blocking USART1 exposed through async byte-stream traits.
pub struct ExposedUart(Uart<'static, Blocking>);

impl embedded_io::ErrorType for ExposedUart {
    type Error = embassy_stm32::usart::Error;
}

impl embedded_io_async::Read for ExposedUart {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.0.blocking_read(buffer)?;
        Ok(buffer.len())
    }
}

impl embedded_io_async::Write for ExposedUart {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        self.0.blocking_write(buffer)?;
        Ok(buffer.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.blocking_flush()
    }
}

type AdcFactory =
    fn(Peri<'static, ADC1>, RuntimePinToken) -> Result<ExposedAnalogInput, RouteError>;

/// One statically valid ADC1 channel route.
#[derive(Clone, Copy)]
pub struct RuntimeAdcChannel {
    matches: fn(&RuntimePinToken) -> bool,
    factory: AdcFactory,
}

/// Platform-owned ADC1 controller and channel table.
pub struct RuntimeAdcResource {
    controller: Peri<'static, ADC1>,
    channels: [Option<RuntimeAdcChannel>; 16],
}

/// Type-erased ADC1 input preserving the selected channel identity.
pub struct ExposedAnalogInput {
    adc: Adc<'static, ADC1>,
    channel: AnyAdcChannel<'static, ADC1>,
}

impl AnalogErrorType for ExposedAnalogInput {
    type Error = Infallible;
}

impl AnalogInput for ExposedAnalogInput {
    fn max_value(&self) -> u32 {
        4095
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        Ok(u32::from(
            self.adc
                .blocking_read(&mut self.channel, SampleTime::CYCLES480),
        ))
    }
}

/// One TIM1 channel allocation from the Platform manifest.
pub struct RuntimePwmResource {
    controller: Peri<'static, TIM1>,
    channel: Option<RuntimePwmChannel>,
}

/// Statically declared TIM1 timer slot.
#[derive(Clone, Copy)]
pub struct RuntimePwmTimer;

/// Statically declared TIM1 output channel.
#[derive(Clone, Copy)]
pub enum RuntimePwmChannel {
    /// TIM1 channel 1.
    Ch1,
}

/// TIM1 channel exposed through `embedded-hal` PWM.
pub type ExposedPwm = embassy_stm32::timer::simple_pwm::SimplePwmChannel<'static, TIM1>;

embassy_stm32::bind_interrupts!(struct RuntimeI2sIrqs {
    DMA1_STREAM3 => embassy_stm32::dma::InterruptHandler<DMA1_CH3>;
    DMA1_STREAM4 => embassy_stm32::dma::InterruptHandler<DMA1_CH4>;
});

/// One Platform-owned DMA stream valid for SPI2 I2S.
pub enum RuntimeI2sDma {
    /// SPI2 transmit DMA stream.
    Tx(Peri<'static, DMA1_CH4>),
    /// SPI2 receive DMA stream.
    Rx(Peri<'static, DMA1_CH3>),
}

/// Platform-owned SPI2, DMA streams, and bounded PCM buffers.
pub struct RuntimeI2sResource {
    controller: Peri<'static, SPI2>,
    tx_dma: Option<Peri<'static, DMA1_CH4>>,
    rx_dma: Option<Peri<'static, DMA1_CH3>>,
    tx_buffer: &'static mut [u16],
    rx_buffer: &'static mut [u16],
}

/// Invalid generated I2S buffer allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeI2sResourceError;

impl fmt::Display for RuntimeI2sResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("STM32 I2S DMA buffer must hold at least one stereo frame")
    }
}

impl core::error::Error for RuntimeI2sResourceError {}

/// STM32 I2S route, format, or transfer failure.
#[derive(Debug)]
pub enum ExposedI2sError {
    /// The requested physical route is not a valid SPI2 I2S mapping.
    InvalidPinRoute,
    /// STM32F429 SPI2 I2S is half duplex and accepts one data direction.
    UnsupportedDirection,
    /// Only stereo, signed 16-bit PCM is implemented by this adapter.
    UnsupportedFormat,
    /// A required DMA stream was absent from the generated resource group.
    MissingDma,
    /// The vendor I2S ring buffer reported a transfer failure.
    Transfer(embassy_stm32::i2s::Error),
}

impl fmt::Display for ExposedI2sError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPinRoute => formatter.write_str("invalid STM32 SPI2 I2S pin route"),
            Self::UnsupportedDirection => {
                formatter.write_str("STM32F429 SPI2 I2S supports TX or RX, not both")
            }
            Self::UnsupportedFormat => {
                formatter.write_str("STM32 I2S requires stereo signed 16-bit PCM")
            }
            Self::MissingDma => formatter.write_str("missing STM32 SPI2 I2S DMA stream"),
            Self::Transfer(error) => write!(formatter, "STM32 I2S transfer failed: {error:?}"),
        }
    }
}

impl core::error::Error for ExposedI2sError {}

enum I2sDirection {
    Tx(embassy_stm32::i2s::I2S<'static, u16>),
    Rx(embassy_stm32::i2s::I2S<'static, u16>),
}

/// SPI2 I2S stream implementing Barracuda's narrow PCM capability.
pub struct ExposedI2s {
    stream: I2sDirection,
    format: audio::PcmFormat,
}

impl audio::PcmStream for ExposedI2s {
    type Error = ExposedI2sError;

    fn format(&self) -> audio::PcmFormat {
        self.format
    }

    async fn write(&mut self, samples: &[i16]) -> Result<(), Self::Error> {
        match &mut self.stream {
            I2sDirection::Tx(stream) => {
                let mut converted = [0_u16; 64];
                for chunk in samples.chunks(converted.len()) {
                    for (target, sample) in converted.iter_mut().zip(chunk) {
                        *target = *sample as u16;
                    }
                    stream
                        .write(&converted[..chunk.len()])
                        .await
                        .map_err(ExposedI2sError::Transfer)?;
                }
                Ok(())
            }
            I2sDirection::Rx(_) => Err(ExposedI2sError::UnsupportedDirection),
        }
    }

    async fn read(&mut self, samples: &mut [i16]) -> Result<(), Self::Error> {
        match &mut self.stream {
            I2sDirection::Rx(stream) => {
                let mut converted = [0_u16; 64];
                for chunk in samples.chunks_mut(converted.len()) {
                    stream
                        .read(&mut converted[..chunk.len()])
                        .await
                        .map_err(ExposedI2sError::Transfer)?;
                    for (sample, source) in chunk.iter_mut().zip(&converted) {
                        *sample = *source as i16;
                    }
                }
                Ok(())
            }
            I2sDirection::Tx(_) => Err(ExposedI2sError::UnsupportedDirection),
        }
    }
}

/// Platform adapter used by generated runtime I/O composition.
pub struct RuntimeAdapter;

impl RuntimePlatform for RuntimeAdapter {
    type PinToken = RuntimePinToken;
    type DigitalPin = DynamicPin;
    type I2cController = Peri<'static, I2C1>;
    type I2cBus = ExposedI2cBus;
    type I2cError = RouteError;
    type SpiController = Peri<'static, SPI1>;
    type SpiBus = ExposedSpiBus;
    type SpiError = RouteError;
    type UartController = Peri<'static, USART1>;
    type AdcResource = RuntimeAdcResource;
    type PwmResource = RuntimePwmResource;
    type I2sResource = RuntimeI2sResource;

    fn digital(pin: Self::PinToken) -> Self::DigitalPin {
        DynamicPin {
            pin: Flex::new(pin.degrade()),
        }
    }

    fn supports_i2c(
        _controller: &Self::I2cController,
        scl: &Self::PinToken,
        sda: &Self::PinToken,
    ) -> bool {
        (scl.is::<PB6>() || scl.is::<PB8>()) && (sda.is::<PB7>() || sda.is::<PB9>())
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
        if frequency_hz == 0 {
            return Err(RouteError::InvalidFrequency);
        }
        let mut config = embassy_stm32::i2c::Config::default();
        config.frequency = Hertz(frequency_hz);
        let bus = if scl.is::<PB6>() && sda.is::<PB7>() {
            I2c::new_blocking(
                controller,
                scl.downcast::<PB6>()?,
                sda.downcast::<PB7>()?,
                config,
            )
        } else if scl.is::<PB6>() && sda.is::<PB9>() {
            I2c::new_blocking(
                controller,
                scl.downcast::<PB6>()?,
                sda.downcast::<PB9>()?,
                config,
            )
        } else if scl.is::<PB8>() && sda.is::<PB7>() {
            I2c::new_blocking(
                controller,
                scl.downcast::<PB8>()?,
                sda.downcast::<PB7>()?,
                config,
            )
        } else if scl.is::<PB8>() && sda.is::<PB9>() {
            I2c::new_blocking(
                controller,
                scl.downcast::<PB8>()?,
                sda.downcast::<PB9>()?,
                config,
            )
        } else {
            return Err(RouteError::InvalidPinRoute);
        };
        Ok(ExposedI2cBus(bus))
    }

    fn supports_spi(
        _controller: &Self::SpiController,
        sck: &Self::PinToken,
        mosi: Option<&Self::PinToken>,
        miso: Option<&Self::PinToken>,
    ) -> bool {
        sck.is::<PA5>()
            && mosi.is_none_or(|pin| pin.is::<PA7>())
            && miso.is_none_or(|pin| pin.is::<PA6>())
            && (mosi.is_some() || miso.is_some())
    }

    fn supports_spi_config(
        controller: &Self::SpiController,
        sck: &Self::PinToken,
        mosi: Option<&Self::PinToken>,
        miso: Option<&Self::PinToken>,
        frequency_hz: u32,
        _mode: EmbeddedMode,
    ) -> bool {
        frequency_hz > 0 && Self::supports_spi(controller, sck, mosi, miso)
    }

    fn spi(
        controller: Self::SpiController,
        sck: Self::PinToken,
        mosi: Option<Self::PinToken>,
        miso: Option<Self::PinToken>,
        frequency_hz: u32,
        mode: EmbeddedMode,
    ) -> Result<Self::SpiBus, Self::SpiError> {
        if frequency_hz == 0 {
            return Err(RouteError::InvalidFrequency);
        }
        let mut config = embassy_stm32::spi::Config::default();
        config.frequency = Hertz(frequency_hz);
        config.mode = match (mode.polarity, mode.phase) {
            (
                embedded_hal::spi::Polarity::IdleLow,
                embedded_hal::spi::Phase::CaptureOnFirstTransition,
            ) => embassy_stm32::spi::MODE_0,
            (
                embedded_hal::spi::Polarity::IdleLow,
                embedded_hal::spi::Phase::CaptureOnSecondTransition,
            ) => embassy_stm32::spi::MODE_1,
            (
                embedded_hal::spi::Polarity::IdleHigh,
                embedded_hal::spi::Phase::CaptureOnFirstTransition,
            ) => embassy_stm32::spi::MODE_2,
            (
                embedded_hal::spi::Polarity::IdleHigh,
                embedded_hal::spi::Phase::CaptureOnSecondTransition,
            ) => embassy_stm32::spi::MODE_3,
        };
        let sck = sck.downcast::<PA5>()?;
        let bus = match (mosi, miso) {
            (Some(mosi), Some(miso)) => Spi::new_blocking(
                controller,
                sck,
                mosi.downcast::<PA7>()?,
                miso.downcast::<PA6>()?,
                config,
            ),
            (Some(mosi), None) => {
                Spi::new_blocking_txonly(controller, sck, mosi.downcast::<PA7>()?, config)
            }
            (None, Some(miso)) => {
                Spi::new_blocking_rxonly(controller, sck, miso.downcast::<PA6>()?, config)
            }
            (None, None) => return Err(RouteError::InvalidPinRoute),
        };
        Ok(ExposedSpiBus(Box::new(bus)))
    }
}

impl RuntimeAnalogPlatform for RuntimeAdapter {
    type AnalogInput = ExposedAnalogInput;
    type AnalogOutput = UnavailableAnalogOutput;
    type AnalogError = RouteError;

    fn supports_analog_input(resource: &Self::AdcResource, pin: &Self::PinToken) -> bool {
        resource
            .channels
            .iter()
            .flatten()
            .any(|channel| (channel.matches)(pin))
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
            .find(|channel| (channel.matches)(&pin))
            .map(|channel| channel.factory)
            .ok_or(RouteError::InvalidPinRoute)?;
        factory(resource.controller, pin)
    }

    fn analog_output(_pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError> {
        Err(RouteError::InvalidPinRoute)
    }
}

impl RuntimePwmPlatform for RuntimeAdapter {
    type Pwm = ExposedPwm;
    type PwmError = RouteError;

    fn supports_pwm(resource: &Self::PwmResource, pin: &Self::PinToken) -> bool {
        resource.channel.is_some() && pin.is::<PA8>()
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
        if frequency_hz == 0 {
            return Err(RouteError::InvalidFrequency);
        }
        match resource.channel.take() {
            Some(RuntimePwmChannel::Ch1) => {
                let pin = PwmPin::<TIM1, Ch1>::new(pin.downcast::<PA8>()?, OutputType::PushPull);
                let pwm = SimplePwm::new(
                    resource.controller,
                    Some(pin),
                    None,
                    None,
                    None,
                    Hertz(frequency_hz),
                    CountingMode::EdgeAlignedUp,
                );
                Ok(pwm.split().ch1)
            }
            None => Err(RouteError::InvalidPinRoute),
        }
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
        tx.is_some_and(|pin| pin.is::<PA9>()) && rx.is_some_and(|pin| pin.is::<PA10>())
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
        let tx = tx.ok_or(ExposedUartConfigError::MissingDirection)?;
        let rx = rx.ok_or(ExposedUartConfigError::MissingDirection)?;
        let mut hal_config = HalUartConfig::default();
        hal_config.baudrate = config.baud;
        hal_config.data_bits = match config.data_bits {
            UartDataBits::Seven => DataBits::DataBits7,
            UartDataBits::Eight => DataBits::DataBits8,
            UartDataBits::Nine => DataBits::DataBits9,
        };
        hal_config.parity = match config.parity {
            UartParity::None => Parity::ParityNone,
            UartParity::Even => Parity::ParityEven,
            UartParity::Odd => Parity::ParityOdd,
        };
        hal_config.stop_bits = match config.stop_bits {
            UartStopBits::One => StopBits::STOP1,
            UartStopBits::Two => StopBits::STOP2,
        };
        let uart = Uart::new_blocking(
            controller,
            rx.downcast::<PA10>()
                .map_err(|_error| ExposedUartConfigError::InvalidPinRoute)?,
            tx.downcast::<PA9>()
                .map_err(|_error| ExposedUartConfigError::InvalidPinRoute)?,
            hal_config,
        )
        .map_err(ExposedUartConfigError::Peripheral)?;
        Ok(ExposedUart(uart))
    }
}

impl RuntimeI2sPlatform for RuntimeAdapter {
    type I2s = ExposedI2s;
    type I2sError = ExposedI2sError;

    fn supports_i2s(
        resource: &Self::I2sResource,
        bclk: &Self::PinToken,
        ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        mclk: Option<&Self::PinToken>,
    ) -> bool {
        let direction_available = match (dout, din) {
            (Some(pin), None) => resource.tx_dma.is_some() && pin.is::<PB15>(),
            (None, Some(pin)) => resource.rx_dma.is_some() && pin.is::<PB15>(),
            _ => false,
        };
        bclk.is::<PB13>()
            && ws.is::<PB12>()
            && mclk.is_none_or(|pin| pin.is::<PC6>())
            && direction_available
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
        let valid_clock = match (mclk, format.master_clock_hz) {
            (None, None) => true,
            (Some(_), Some(clock)) => format
                .sample_rate_hz
                .checked_mul(256)
                .is_some_and(|expected| expected == clock),
            _ => false,
        };
        format.sample_rate_hz > 0
            && format.channels == 2
            && format.bits_per_sample == 16
            && valid_clock
            && Self::supports_i2s(resource, bclk, ws, dout, din, mclk)
    }

    fn i2s(
        mut resource: Self::I2sResource,
        bclk: Self::PinToken,
        ws: Self::PinToken,
        dout: Option<Self::PinToken>,
        din: Option<Self::PinToken>,
        mclk: Option<Self::PinToken>,
        format: audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError> {
        if format.sample_rate_hz == 0 || format.channels != 2 || format.bits_per_sample != 16 {
            return Err(ExposedI2sError::UnsupportedFormat);
        }
        let mut config = embassy_stm32::i2s::Config::default();
        config.frequency = Hertz(format.sample_rate_hz);
        config.format = embassy_stm32::i2s::Format::Data16Channel16;
        config.master_clock = mclk.is_some();
        let bclk = bclk
            .downcast::<PB13>()
            .map_err(|_error| ExposedI2sError::InvalidPinRoute)?;
        let ws = ws
            .downcast::<PB12>()
            .map_err(|_error| ExposedI2sError::InvalidPinRoute)?;
        let transmit = dout.is_some() && din.is_none();
        let stream = match (dout, din, mclk) {
            (Some(dout), None, Some(mclk)) => embassy_stm32::i2s::I2S::new_txonly(
                resource.controller,
                dout.downcast::<PB15>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                ws,
                bclk,
                mclk.downcast::<PC6>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                resource.tx_dma.take().ok_or(ExposedI2sError::MissingDma)?,
                resource.tx_buffer,
                RuntimeI2sIrqs,
                config,
            ),
            (Some(dout), None, None) => embassy_stm32::i2s::I2S::new_txonly_nomck(
                resource.controller,
                dout.downcast::<PB15>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                ws,
                bclk,
                resource.tx_dma.take().ok_or(ExposedI2sError::MissingDma)?,
                resource.tx_buffer,
                RuntimeI2sIrqs,
                config,
            ),
            (None, Some(din), Some(mclk)) => embassy_stm32::i2s::I2S::new_rxonly(
                resource.controller,
                din.downcast::<PB15>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                ws,
                bclk,
                mclk.downcast::<PC6>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                resource.rx_dma.take().ok_or(ExposedI2sError::MissingDma)?,
                resource.rx_buffer,
                RuntimeI2sIrqs,
                config,
            ),
            (None, Some(din), None) => embassy_stm32::i2s::I2S::new_rxonly_nomck(
                resource.controller,
                din.downcast::<PB15>()
                    .map_err(|_error| ExposedI2sError::InvalidPinRoute)?,
                ws,
                bclk,
                resource.rx_dma.take().ok_or(ExposedI2sError::MissingDma)?,
                resource.rx_buffer,
                RuntimeI2sIrqs,
                config,
            ),
            _ => return Err(ExposedI2sError::UnsupportedDirection),
        };
        let mut stream = if transmit {
            I2sDirection::Tx(stream)
        } else {
            I2sDirection::Rx(stream)
        };
        match &mut stream {
            I2sDirection::Tx(stream) | I2sDirection::Rx(stream) => stream.start(),
        }
        Ok(ExposedI2s { stream, format })
    }
}

/// Generated exposed-I/O owner specialized to the STM32 Platform adapter.
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
pub fn runtime_io<const P: usize>(
    pins: [(&'static str, RuntimePinToken); P],
    i2c: [Peri<'static, I2C1>; 1],
    spi: [Peri<'static, SPI1>; 1],
    uart: [Peri<'static, USART1>; 1],
    adc: [RuntimeAdcResource; 1],
    pwm: [RuntimePwmResource; 1],
    i2s: [RuntimeI2sResource; 1],
) -> RuntimeIo<P, 1, 1, 1, 1, 1, 1> {
    RuntimeIo::new_with_resources(pins, i2c, spi, uart, adc, pwm, i2s)
}

/// Erases one selected pin token while preserving its exclusive ownership.
#[must_use]
pub fn runtime_pin<P: Pin + Send>(pin: Peri<'static, P>) -> RuntimePinToken {
    RuntimePinToken {
        token: Box::new(OwnedPin(pin)),
    }
}

/// Preserves one I2C1 token in the Platform runtime pool.
#[must_use]
pub const fn runtime_i2c_controller(controller: Peri<'static, I2C1>) -> Peri<'static, I2C1> {
    controller
}

/// Preserves one SPI1 token in the Platform runtime pool.
#[must_use]
pub const fn runtime_spi_controller(controller: Peri<'static, SPI1>) -> Peri<'static, SPI1> {
    controller
}

/// Preserves one USART1 token in the Platform runtime pool.
#[must_use]
pub const fn runtime_uart_controller(controller: Peri<'static, USART1>) -> Peri<'static, USART1> {
    controller
}

/// Builds the Platform-owned ADC1 allocation from generated route descriptors.
#[must_use]
pub fn runtime_adc_resource(
    controller: Peri<'static, ADC1>,
    channels: &[RuntimeAdcChannel],
) -> RuntimeAdcResource {
    let mut declared = [None; 16];
    for (target, channel) in declared.iter_mut().zip(channels.iter().copied()) {
        *target = Some(channel);
    }
    RuntimeAdcResource {
        controller,
        channels: declared,
    }
}

#[doc(hidden)]
#[must_use]
pub fn runtime_adc_channel_for<P>() -> RuntimeAdcChannel
where
    P: Pin + Send,
    Peri<'static, P>: AdcChannel<ADC1>,
{
    RuntimeAdcChannel {
        matches: runtime_pin_is::<P>,
        factory: construct_adc_input::<P>,
    }
}

fn runtime_pin_is<P: Pin + Send>(pin: &RuntimePinToken) -> bool {
    pin.is::<P>()
}

fn construct_adc_input<P>(
    controller: Peri<'static, ADC1>,
    pin: RuntimePinToken,
) -> Result<ExposedAnalogInput, RouteError>
where
    P: Pin + Send,
    Peri<'static, P>: AdcChannel<ADC1>,
{
    let channel = AdcChannel::degrade_adc(pin.downcast::<P>()?);
    Ok(ExposedAnalogInput {
        adc: Adc::new(controller),
        channel,
    })
}

/// Builds the Platform-owned TIM1 allocation.
#[must_use]
pub fn runtime_pwm_resource(
    controller: Peri<'static, TIM1>,
    _timers: &[RuntimePwmTimer],
    channels: &[RuntimePwmChannel],
) -> RuntimePwmResource {
    RuntimePwmResource {
        controller,
        channel: channels.first().copied(),
    }
}

/// Converts one generated DMA token into the SPI2 I2S DMA pool.
#[doc(hidden)]
pub trait RuntimeI2sDmaToken: embassy_stm32::PeripheralType + Send {
    /// Preserves the concrete DMA stream identity.
    fn erase(token: Peri<'static, Self>) -> RuntimeI2sDma;
}

impl RuntimeI2sDmaToken for DMA1_CH4 {
    fn erase(token: Peri<'static, Self>) -> RuntimeI2sDma {
        RuntimeI2sDma::Tx(token)
    }
}

impl RuntimeI2sDmaToken for DMA1_CH3 {
    fn erase(token: Peri<'static, Self>) -> RuntimeI2sDma {
        RuntimeI2sDma::Rx(token)
    }
}

/// Preserves one generated SPI2 I2S DMA stream.
#[must_use]
pub fn runtime_i2s_dma<D: RuntimeI2sDmaToken>(token: Peri<'static, D>) -> RuntimeI2sDma {
    D::erase(token)
}

/// Allocates independent, bounded transmit and receive PCM ring buffers.
///
/// # Errors
///
/// Returns [`RuntimeI2sResourceError`] when the byte count is not aligned to
/// signed 16-bit samples or cannot hold one stereo frame.
#[doc(hidden)]
pub fn runtime_i2s_buffers<const N: usize>(
    bytes: usize,
    tx: &'static StaticCell<[u16; N]>,
    rx: &'static StaticCell<[u16; N]>,
) -> Result<(&'static mut [u16], &'static mut [u16]), RuntimeI2sResourceError> {
    if !bytes.is_multiple_of(core::mem::size_of::<u16>()) || N < 4 {
        return Err(RuntimeI2sResourceError);
    }
    Ok((
        tx.init([0; N]).as_mut_slice(),
        rx.init([0; N]).as_mut_slice(),
    ))
}

/// Builds one Platform-owned SPI2 I2S allocation.
#[must_use]
pub fn runtime_i2s_resource(
    controller: Peri<'static, SPI2>,
    dma: [RuntimeI2sDma; 2],
    buffers: (&'static mut [u16], &'static mut [u16]),
) -> RuntimeI2sResource {
    let mut tx_dma = None;
    let mut rx_dma = None;
    for stream in dma {
        match stream {
            RuntimeI2sDma::Tx(token) => tx_dma = Some(token),
            RuntimeI2sDma::Rx(token) => rx_dma = Some(token),
        }
    }
    RuntimeI2sResource {
        controller,
        tx_dma,
        rx_dma,
        tx_buffer: buffers.0,
        rx_buffer: buffers.1,
    }
}

/// Constructs a blocking I2C1 bus for a statically selected peripheral Driver.
///
/// # Errors
///
/// Returns [`RouteError::InvalidFrequency`] for a zero bus clock.
pub fn i2c_device<T: embassy_stm32::i2c::Instance>(
    controller: Peri<'static, T>,
    scl: Peri<'static, impl embassy_stm32::i2c::SclPin<T>>,
    sda: Peri<'static, impl embassy_stm32::i2c::SdaPin<T>>,
    frequency_hz: u32,
) -> Result<I2cBus, RouteError> {
    if frequency_hz == 0 {
        return Err(RouteError::InvalidFrequency);
    }
    let mut config = embassy_stm32::i2c::Config::default();
    config.frequency = Hertz(frequency_hz);
    Ok(I2c::new_blocking(controller, scl, sda, config))
}

/// Constructs a blocking transmit-only SPI1 bus for a selected Driver.
///
/// # Errors
///
/// Returns [`RouteError::InvalidFrequency`] for a zero bus clock.
pub fn spi_bus<T: embassy_stm32::spi::Instance>(
    controller: Peri<'static, T>,
    sck: Peri<'static, impl embassy_stm32::spi::SckPin<T>>,
    mosi: Peri<'static, impl embassy_stm32::spi::MosiPin<T>>,
    frequency_hz: u32,
) -> Result<SpiBus, RouteError> {
    if frequency_hz == 0 {
        return Err(RouteError::InvalidFrequency);
    }
    let mut config = embassy_stm32::spi::Config::default();
    config.frequency = Hertz(frequency_hz);
    Ok(Spi::new_blocking_txonly(controller, sck, mosi, config))
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

/// Resolves a manifest ADC1 channel/pin pair to a typed Embassy route.
#[macro_export]
macro_rules! __barracuda_stm32_runtime_adc_channel {
    (ADC1_CH0, PA0) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA0>()
    };
    (ADC1_CH1, PA1) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA1>()
    };
    (ADC1_CH2, PA2) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA2>()
    };
    (ADC1_CH3, PA3) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA3>()
    };
    (ADC1_CH4, PA4) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA4>()
    };
    (ADC1_CH5, PA5) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA5>()
    };
    (ADC1_CH6, PA6) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA6>()
    };
    (ADC1_CH7, PA7) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PA7>()
    };
    (ADC1_CH8, PB0) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PB0>()
    };
    (ADC1_CH9, PB1) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PB1>()
    };
    (ADC1_CH10, PC0) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC0>()
    };
    (ADC1_CH11, PC1) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC1>()
    };
    (ADC1_CH12, PC2) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC2>()
    };
    (ADC1_CH13, PC3) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC3>()
    };
    (ADC1_CH14, PC4) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC4>()
    };
    (ADC1_CH15, PC5) => {
        $crate::hal::runtime_adc_channel_for::<$crate::hal::__vendor::peripherals::PC5>()
    };
}

/// Resolves the sole TIM1 timer allocation declared by the Platform.
#[macro_export]
macro_rules! __barracuda_stm32_runtime_pwm_timer {
    (TIM1) => {
        $crate::hal::RuntimePwmTimer
    };
}

/// Resolves one TIM1 output channel declared by the Platform.
#[macro_export]
macro_rules! __barracuda_stm32_runtime_pwm_channel {
    (Ch1) => {
        $crate::hal::RuntimePwmChannel::Ch1
    };
}

/// Statically allocates independent SPI2 I2S transmit and receive buffers.
#[macro_export]
macro_rules! __barracuda_stm32_runtime_i2s_dma_buffers {
    ($bytes:expr) => {{
        static TX: $crate::hal::__StaticCell<[u16; $bytes / 2]> = $crate::hal::__StaticCell::new();
        static RX: $crate::hal::__StaticCell<[u16; $bytes / 2]> = $crate::hal::__StaticCell::new();
        $crate::hal::runtime_i2s_buffers($bytes, &TX, &RX)
    }};
}

pub use __barracuda_stm32_runtime_adc_channel as runtime_adc_channel;
#[doc(hidden)]
pub use __barracuda_stm32_runtime_i2s_dma_buffers as runtime_i2s_dma_buffers;
#[doc(hidden)]
pub use __barracuda_stm32_runtime_pwm_channel as runtime_pwm_channel;
#[doc(hidden)]
pub use __barracuda_stm32_runtime_pwm_timer as runtime_pwm_timer;

/// Resolves a Board-selected pin name to its move-only STM32 HAL token type.
#[macro_export]
macro_rules! __barracuda_stm32_pin_binding_type {
    ($pin:ident) => {
        $crate::hal::__vendor::Peri<'static, $crate::hal::__vendor::peripherals::$pin>
    };
}

/// Resolves a Platform-owned controller name to its STM32 singleton token.
#[macro_export]
macro_rules! __barracuda_stm32_controller_binding_type {
    ($peripheral:ident) => {
        $crate::hal::__vendor::Peri<'static, $crate::hal::__vendor::peripherals::$peripheral>
    };
}

#[doc(hidden)]
pub use __barracuda_stm32_controller_binding_type as controller_binding_type;
#[doc(hidden)]
pub use __barracuda_stm32_pin_binding_type as pin_binding_type;
