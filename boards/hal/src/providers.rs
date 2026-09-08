//! Runtime construction contracts for functions placed on exposed resources.

extern crate alloc;

use alloc::string::String;
use core::{cell::RefCell, fmt};

use embedded_hal::spi::Mode;

use crate::{audio, AnalogInput, AnalogOutput, ConfigurableDigitalPin, LeaseError, ResourceKind};

/// Platform adapter used by the Board-generated runtime I/O owner.
///
/// The adapter consumes vendor HAL singleton tokens and returns ecosystem HAL
/// values. Tokens are deliberately move-only: once one is used to construct a
/// function, it is not recreated or returned to the runtime catalog.
pub trait RuntimePlatform: Send + Sync + 'static {
    /// Type-erased physical pin token supplied by the selected Platform HAL.
    type PinToken: Send + 'static;
    /// Runtime-configurable digital value.
    type DigitalPin: ConfigurableDigitalPin + Send + 'static;
    /// Type-erased I2C controller token.
    type I2cController: Send + 'static;
    /// Async I2C bus returned to runtime consumers.
    type I2cBus: embedded_hal_async::i2c::I2c + Send + 'static;
    /// I2C configuration failure reported by the vendor HAL.
    type I2cError: core::error::Error;
    /// Type-erased SPI controller token.
    type SpiController: Send + 'static;
    /// Async SPI bus returned to runtime consumers.
    type SpiBus: embedded_hal_async::spi::SpiBus + Send + 'static;
    /// SPI configuration failure reported by the vendor HAL.
    type SpiError: core::error::Error;
    /// Type-erased UART controller token.
    type UartController: Send + 'static;
    /// Platform-owned ADC controller and its statically declared channel routes.
    type AdcResource: Send + 'static;
    /// Platform-owned PWM controller and its timer/channel slots.
    type PwmResource: Send + 'static;
    /// Platform-owned I2S controller, DMA channel, and bounded buffers.
    type I2sResource: Send + 'static;

    /// Consumes one raw pin token as a configurable digital pin.
    fn digital(pin: Self::PinToken) -> Self::DigitalPin;

    /// Consumes one controller and two pins as an async I2C bus.
    fn i2c(
        controller: Self::I2cController,
        scl: Self::PinToken,
        sda: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::I2cBus, Self::I2cError>;

    /// Consumes one controller and selected pins as an async SPI bus.
    fn spi(
        controller: Self::SpiController,
        sck: Self::PinToken,
        mosi: Option<Self::PinToken>,
        miso: Option<Self::PinToken>,
        frequency_hz: u32,
        mode: Mode,
    ) -> Result<Self::SpiBus, Self::SpiError>;
}

/// Optional Platform construction contract for analog functions.
pub trait RuntimeAnalogPlatform: RuntimePlatform {
    /// Analog input returned to an application handle.
    type AnalogInput: AnalogInput + 'static;
    /// Analog output returned to an application handle.
    type AnalogOutput: AnalogOutput + 'static;
    /// Platform analog configuration failure.
    type AnalogError: core::error::Error;

    /// Returns whether this physical pin supports analog input.
    fn supports_analog_input(resource: &Self::AdcResource, pin: &Self::PinToken) -> bool;
    /// Returns whether this physical pin supports analog output.
    fn supports_analog_output(pin: &Self::PinToken) -> bool;
    /// Consumes a physical pin as an analog input.
    fn analog_input(
        resource: Self::AdcResource,
        pin: Self::PinToken,
    ) -> Result<Self::AnalogInput, Self::AnalogError>;
    /// Consumes a physical pin as an analog output.
    fn analog_output(pin: Self::PinToken) -> Result<Self::AnalogOutput, Self::AnalogError>;
}

/// Optional Platform construction contract for PWM functions.
pub trait RuntimePwmPlatform: RuntimePlatform {
    /// PWM output returned to an application handle.
    type Pwm: embedded_hal::pwm::SetDutyCycle + 'static;
    /// Platform PWM construction failure.
    type PwmError: core::error::Error;

    /// Returns whether this physical pin can be routed to a PWM output.
    fn supports_pwm(resource: &Self::PwmResource, pin: &Self::PinToken) -> bool;
    /// Consumes a pin and one timer/channel resource at the requested frequency.
    fn pwm(
        resource: Self::PwmResource,
        pin: Self::PinToken,
        frequency_hz: u32,
    ) -> Result<Self::Pwm, Self::PwmError>;
}

/// Optional Platform construction contract for UART functions.
pub trait RuntimeUartPlatform: RuntimePlatform {
    /// Bidirectional byte stream returned to an application handle.
    type Uart: embedded_io_async::Read + embedded_io_async::Write + Send + 'static;
    /// Platform UART construction failure.
    type UartError: core::error::Error;

    /// Returns whether the selected physical pins can be routed as UART.
    fn supports_uart(
        controller: &Self::UartController,
        tx: Option<&Self::PinToken>,
        rx: Option<&Self::PinToken>,
    ) -> bool;
    /// Consumes one Platform-owned controller and the selected pins.
    fn uart(
        controller: Self::UartController,
        tx: Option<Self::PinToken>,
        rx: Option<Self::PinToken>,
        config: UartConfig,
    ) -> Result<Self::Uart, Self::UartError>;
}

/// Optional Platform construction contract for I2S functions.
pub trait RuntimeI2sPlatform: RuntimePlatform {
    /// PCM stream returned to an application handle.
    type I2s: audio::PcmStream + Send + 'static;
    /// Platform I2S construction failure.
    type I2sError: core::error::Error;

    /// Returns whether the selected physical pins can be routed as I2S.
    fn supports_i2s(
        resource: &Self::I2sResource,
        bclk: &Self::PinToken,
        ws: &Self::PinToken,
        dout: Option<&Self::PinToken>,
        din: Option<&Self::PinToken>,
        mclk: Option<&Self::PinToken>,
    ) -> bool;
    /// Consumes selected pins and allocates an I2S controller and DMA resources.
    fn i2s(
        resource: Self::I2sResource,
        bclk: Self::PinToken,
        ws: Self::PinToken,
        dout: Option<Self::PinToken>,
        din: Option<Self::PinToken>,
        mclk: Option<Self::PinToken>,
        format: audio::PcmFormat,
    ) -> Result<Self::I2s, Self::I2sError>;
}

struct RuntimePin<Pin> {
    name: &'static str,
    token: Option<Pin>,
    owner: Option<&'static str>,
}

struct RuntimeIoState<
    H: RuntimePlatform,
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize,
    const A: usize,
    const W: usize,
    const T: usize,
> {
    pins: [RuntimePin<H::PinToken>; P],
    i2c: [Option<H::I2cController>; I],
    spi: [Option<H::SpiController>; S],
    uart: [Option<H::UartController>; U],
    adc: [Option<H::AdcResource>; A],
    pwm: [Option<H::PwmResource>; W],
    i2s: [Option<H::I2sResource>; T],
}

/// Board-generated owner of exposed pins and Platform-owned runtime controllers.
///
/// Every physical token can be claimed once during a boot. An acquisition that
/// needs several resources checks all of them under one critical section before
/// moving any token, so a failed conflict check cannot partially consume state.
pub struct RuntimeIo<
    H: RuntimePlatform,
    const P: usize,
    const I: usize,
    const S: usize,
    const U: usize = 0,
    const A: usize = 0,
    const W: usize = 0,
    const T: usize = 0,
> {
    state: critical_section::Mutex<RefCell<RuntimeIoState<H, P, I, S, U, A, W, T>>>,
}

impl<H: RuntimePlatform, const P: usize, const I: usize, const S: usize> RuntimeIo<H, P, I, S, 0> {
    /// Creates the unified runtime owner from Board-named pin tokens and the
    /// compatible controller pools declared by the Platform.
    #[must_use]
    pub fn new(
        pins: [(&'static str, H::PinToken); P],
        i2c: [H::I2cController; I],
        spi: [H::SpiController; S],
    ) -> Self {
        Self {
            state: critical_section::Mutex::new(RefCell::new(RuntimeIoState {
                pins: pins.map(|(name, token)| RuntimePin {
                    name,
                    token: Some(token),
                    owner: None,
                }),
                i2c: i2c.map(Some),
                spi: spi.map(Some),
                uart: [],
                adc: [],
                pwm: [],
                i2s: [],
            })),
        }
    }
}

impl<H: RuntimePlatform, const P: usize, const I: usize, const S: usize, const U: usize>
    RuntimeIo<H, P, I, S, U, 0, 0, 0>
{
    /// Creates the unified runtime owner with a Platform-owned UART pool.
    #[must_use]
    pub fn new_with_uart(
        pins: [(&'static str, H::PinToken); P],
        i2c: [H::I2cController; I],
        spi: [H::SpiController; S],
        uart: [H::UartController; U],
    ) -> Self {
        Self {
            state: critical_section::Mutex::new(RefCell::new(RuntimeIoState {
                pins: pins.map(|(name, token)| RuntimePin {
                    name,
                    token: Some(token),
                    owner: None,
                }),
                i2c: i2c.map(Some),
                spi: spi.map(Some),
                uart: uart.map(Some),
                adc: [],
                pwm: [],
                i2s: [],
            })),
        }
    }
}

impl<
        H: RuntimePlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > RuntimeIo<H, P, I, S, U, A, W, T>
{
    /// Creates the unified owner with every Platform-owned runtime resource pool.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_resources(
        pins: [(&'static str, H::PinToken); P],
        i2c: [H::I2cController; I],
        spi: [H::SpiController; S],
        uart: [H::UartController; U],
        adc: [H::AdcResource; A],
        pwm: [H::PwmResource; W],
        i2s: [H::I2sResource; T],
    ) -> Self {
        Self {
            state: critical_section::Mutex::new(RefCell::new(RuntimeIoState {
                pins: pins.map(|(name, token)| RuntimePin {
                    name,
                    token: Some(token),
                    owner: None,
                }),
                i2c: i2c.map(Some),
                spi: spi.map(Some),
                uart: uart.map(Some),
                adc: adc.map(Some),
                pwm: pwm.map(Some),
                i2s: i2s.map(Some),
            })),
        }
    }

    fn resolve_pin(&self, name: &str) -> Result<usize, LeaseError> {
        critical_section::with(|section| {
            self.state
                .borrow(section)
                .borrow()
                .pins
                .iter()
                .position(|pin| pin.name == name)
                .ok_or_else(|| LeaseError::NotExposed {
                    resource: String::from(name),
                    kind: ResourceKind::Pin,
                })
        })
    }

    fn ensure_distinct(&self, names: &[(&str, usize)]) -> Result<(), LeaseError> {
        for (position, (name, index)) in names.iter().enumerate() {
            if names[..position]
                .iter()
                .any(|(_, previous)| previous == index)
            {
                return Err(LeaseError::Duplicate {
                    resource: String::from(*name),
                });
            }
        }
        Ok(())
    }
}

/// Failure while claiming or constructing one runtime protocol function.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeOpenError<E> {
    /// A requested exposed pin is missing, repeated, or already claimed.
    Resource(LeaseError),
    /// Every compatible Platform controller has already been consumed.
    NoController {
        /// Protocol whose controller pool was exhausted.
        protocol: &'static str,
    },
    /// SPI was requested without either data signal.
    MissingSpiData,
    /// UART was requested without either data signal.
    MissingUartData,
    /// I2S was requested without either data signal.
    MissingI2sData,
    /// The selected physical resource cannot implement the requested function.
    Unsupported {
        /// Function rejected by the Platform.
        function: &'static str,
    },
    /// The selected Platform HAL rejected the requested configuration.
    Platform(E),
}

impl<E: fmt::Display> fmt::Display for RuntimeOpenError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource(error) => error.fmt(formatter),
            Self::NoController { protocol } => {
                write!(
                    formatter,
                    "no free runtime {protocol} controller is available"
                )
            }
            Self::MissingSpiData => formatter.write_str("SPI requires MOSI or MISO"),
            Self::MissingUartData => formatter.write_str("UART requires TX or RX"),
            Self::MissingI2sData => formatter.write_str("I2S requires DOUT or DIN"),
            Self::Unsupported { function } => {
                write!(formatter, "the selected pin does not support {function}")
            }
            Self::Platform(error) => {
                write!(formatter, "Platform HAL configuration failed: {error}")
            }
        }
    }
}

impl<E: core::error::Error + 'static> core::error::Error for RuntimeOpenError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Resource(error) => Some(error),
            Self::Platform(error) => Some(error),
            Self::NoController { .. }
            | Self::MissingSpiData
            | Self::MissingUartData
            | Self::MissingI2sData
            | Self::Unsupported { .. } => None,
        }
    }
}

impl<E> From<LeaseError> for RuntimeOpenError<E> {
    fn from(error: LeaseError) -> Self {
        Self::Resource(error)
    }
}

impl<
        H: RuntimePlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > DigitalProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Pin = H::DigitalPin;
    type Error = LeaseError;

    fn digital_available(&self, name: &str) -> bool {
        let Ok(index) = self.resolve_pin(name) else {
            return false;
        };
        critical_section::with(|section| {
            self.state.borrow(section).borrow().pins[index]
                .token
                .is_some()
        })
    }

    fn acquire_digital(&self, name: &str) -> Result<Self::Pin, Self::Error> {
        let index = self.resolve_pin(name)?;
        let token = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            let pin = &mut state.pins[index];
            let token = pin.token.take().ok_or(LeaseError::Busy {
                resource: pin.name,
                owner: pin.owner.unwrap_or("runtime function"),
            })?;
            pin.owner = Some("digital");
            Ok(token)
        })?;
        Ok(H::digital(token))
    }
}

impl<
        H: RuntimeAnalogPlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > AnalogProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Input = H::AnalogInput;
    type Output = H::AnalogOutput;
    type Error = RuntimeOpenError<H::AnalogError>;

    fn analog_input_available(&self, name: &str) -> bool {
        let Ok(index) = self.resolve_pin(name) else {
            return false;
        };
        critical_section::with(|section| {
            let state = self.state.borrow(section).borrow();
            let Some(pin) = state.pins[index].token.as_ref() else {
                return false;
            };
            state
                .adc
                .iter()
                .flatten()
                .any(|resource| H::supports_analog_input(resource, pin))
        })
    }

    fn analog_output_available(&self, name: &str) -> bool {
        let Ok(index) = self.resolve_pin(name) else {
            return false;
        };
        critical_section::with(|section| {
            let state = self.state.borrow(section).borrow();
            state.pins[index]
                .token
                .as_ref()
                .is_some_and(H::supports_analog_output)
        })
    }

    fn acquire_analog_input(&self, name: &str) -> Result<Self::Input, Self::Error> {
        let index = self.resolve_pin(name)?;
        let (resource, token) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            let pin = &state.pins[index];
            let token = pin.token.as_ref().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: pin.name,
                    owner: pin.owner.unwrap_or("runtime function"),
                })
            })?;
            let resource_index = state.adc.iter().position(|resource| {
                resource
                    .as_ref()
                    .is_some_and(|resource| H::supports_analog_input(resource, token))
            });
            let resource_index = match resource_index {
                Some(index) => index,
                None if state.adc.iter().any(Option::is_some) => {
                    return Err(RuntimeOpenError::Unsupported {
                        function: "analog input",
                    });
                }
                None => return Err(RuntimeOpenError::NoController { protocol: "ADC" }),
            };
            let resource = state.adc[resource_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "ADC" })?;
            let token = state.pins[index].token.take().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: state.pins[index].name,
                    owner: "runtime function",
                })
            })?;
            state.pins[index].owner = Some("analog input");
            Ok((resource, token))
        })?;
        H::analog_input(resource, token).map_err(RuntimeOpenError::Platform)
    }

    fn acquire_analog_output(&self, name: &str) -> Result<Self::Output, Self::Error> {
        self.acquire_analog_output(
            name,
            "analog output",
            H::supports_analog_output,
            H::analog_output,
        )
    }
}

impl<
        H: RuntimeAnalogPlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > RuntimeIo<H, P, I, S, U, A, W, T>
{
    fn acquire_analog_output<Value>(
        &self,
        name: &str,
        function: &'static str,
        supported: fn(&H::PinToken) -> bool,
        construct: fn(H::PinToken) -> Result<Value, H::AnalogError>,
    ) -> Result<Value, RuntimeOpenError<H::AnalogError>> {
        let index = self.resolve_pin(name)?;
        let token = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            let pin = &mut state.pins[index];
            let token = pin.token.as_ref().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: pin.name,
                    owner: pin.owner.unwrap_or("runtime function"),
                })
            })?;
            if !supported(token) {
                return Err(RuntimeOpenError::Unsupported { function });
            }
            let token = pin.token.take().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: pin.name,
                    owner: "runtime function",
                })
            })?;
            pin.owner = Some(function);
            Ok(token)
        })?;
        construct(token).map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimePwmPlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > PwmProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Output = H::Pwm;
    type Error = RuntimeOpenError<H::PwmError>;

    fn pwm_available(&self, name: &str) -> bool {
        let Ok(index) = self.resolve_pin(name) else {
            return false;
        };
        critical_section::with(|section| {
            let state = self.state.borrow(section).borrow();
            let Some(pin) = state.pins[index].token.as_ref() else {
                return false;
            };
            state
                .pwm
                .iter()
                .flatten()
                .any(|resource| H::supports_pwm(resource, pin))
        })
    }

    fn open_pwm(&self, request: PwmRequest<'_>) -> Result<Self::Output, Self::Error> {
        let index = self.resolve_pin(request.pin)?;
        let (resource, token) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            let pin = &state.pins[index];
            let token = pin.token.as_ref().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: pin.name,
                    owner: pin.owner.unwrap_or("runtime function"),
                })
            })?;
            let resource_index = state.pwm.iter().position(|resource| {
                resource
                    .as_ref()
                    .is_some_and(|resource| H::supports_pwm(resource, token))
            });
            let resource_index = match resource_index {
                Some(index) => index,
                None if state.pwm.iter().any(Option::is_some) => {
                    return Err(RuntimeOpenError::Unsupported { function: "PWM" });
                }
                None => return Err(RuntimeOpenError::NoController { protocol: "PWM" }),
            };
            let resource = state.pwm[resource_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "PWM" })?;
            let token = state.pins[index].token.take().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: state.pins[index].name,
                    owner: "runtime function",
                })
            })?;
            state.pins[index].owner = Some("PWM");
            Ok((resource, token))
        })?;
        H::pwm(resource, token, request.frequency_hz).map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimeUartPlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > UartProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Port = H::Uart;
    type Error = RuntimeOpenError<H::UartError>;

    fn uart_available(&self, tx: Option<&str>, rx: Option<&str>) -> bool {
        if tx.is_none() && rx.is_none() {
            return false;
        }
        let tx_index = tx.and_then(|name| self.resolve_pin(name).ok());
        let rx_index = rx.and_then(|name| self.resolve_pin(name).ok());
        if tx.is_some_and(|_| tx_index.is_none())
            || rx.is_some_and(|_| rx_index.is_none())
            || tx_index.is_some() && tx_index == rx_index
        {
            return false;
        }
        critical_section::with(|section| {
            let state = self.state.borrow(section).borrow();
            let tx = tx_index.and_then(|index| state.pins[index].token.as_ref());
            let rx = rx_index.and_then(|index| state.pins[index].token.as_ref());
            (tx_index.is_none() || tx.is_some())
                && (rx_index.is_none() || rx.is_some())
                && state
                    .uart
                    .iter()
                    .flatten()
                    .any(|controller| H::supports_uart(controller, tx, rx))
        })
    }

    fn open_uart(&self, request: UartRequest<'_>) -> Result<Self::Port, Self::Error> {
        if request.tx.is_none() && request.rx.is_none() {
            return Err(RuntimeOpenError::MissingUartData);
        }
        let tx_index = request
            .tx
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let rx_index = request
            .rx
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let mut roles = alloc::vec![];
        roles.extend(tx_index);
        roles.extend(rx_index);
        self.ensure_distinct(&roles)?;

        let (controller, tx, rx) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            for (_, index) in &roles {
                let pin = &state.pins[*index];
                if pin.token.is_none() {
                    return Err(RuntimeOpenError::Resource(LeaseError::Busy {
                        resource: pin.name,
                        owner: pin.owner.unwrap_or("runtime function"),
                    }));
                }
            }
            let tx_ref = tx_index.and_then(|(_, index)| state.pins[index].token.as_ref());
            let rx_ref = rx_index.and_then(|(_, index)| state.pins[index].token.as_ref());
            let controller_index = state.uart.iter().position(|controller| {
                controller
                    .as_ref()
                    .is_some_and(|controller| H::supports_uart(controller, tx_ref, rx_ref))
            });
            let controller_index = match controller_index {
                Some(index) => index,
                None if state.uart.iter().any(Option::is_some) => {
                    return Err(RuntimeOpenError::Unsupported { function: "UART" });
                }
                None => return Err(RuntimeOpenError::NoController { protocol: "UART" }),
            };
            let controller = state.uart[controller_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "UART" })?;
            let tx = tx_index.and_then(|(_, index)| state.pins[index].token.take());
            let rx = rx_index.and_then(|(_, index)| state.pins[index].token.take());
            if let Some((_, index)) = tx_index {
                state.pins[index].owner = Some("UART TX");
            }
            if let Some((_, index)) = rx_index {
                state.pins[index].owner = Some("UART RX");
            }
            Ok((controller, tx, rx))
        })?;
        H::uart(controller, tx, rx, request.config).map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimeI2sPlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > I2sProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Stream = H::I2s;
    type Error = RuntimeOpenError<H::I2sError>;

    fn open_i2s(&self, request: I2sRequest<'_>) -> Result<Self::Stream, Self::Error> {
        if request.dout.is_none() && request.din.is_none() {
            return Err(RuntimeOpenError::MissingI2sData);
        }
        let bclk_index = self.resolve_pin(request.bclk)?;
        let ws_index = self.resolve_pin(request.ws)?;
        let dout_index = request
            .dout
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let din_index = request
            .din
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let mclk_index = request
            .mclk
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let mut roles = alloc::vec![(request.bclk, bclk_index), (request.ws, ws_index)];
        roles.extend(dout_index);
        roles.extend(din_index);
        roles.extend(mclk_index);
        self.ensure_distinct(&roles)?;

        let (resource, bclk, ws, dout, din, mclk) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            for (_, index) in &roles {
                let pin = &state.pins[*index];
                if pin.token.is_none() {
                    return Err(RuntimeOpenError::Resource(LeaseError::Busy {
                        resource: pin.name,
                        owner: pin.owner.unwrap_or("runtime function"),
                    }));
                }
            }
            let bclk_ref = state.pins[bclk_index]
                .token
                .as_ref()
                .ok_or(RuntimeOpenError::Unsupported { function: "I2S" })?;
            let ws_ref = state.pins[ws_index]
                .token
                .as_ref()
                .ok_or(RuntimeOpenError::Unsupported { function: "I2S" })?;
            let dout_ref = dout_index.and_then(|(_, index)| state.pins[index].token.as_ref());
            let din_ref = din_index.and_then(|(_, index)| state.pins[index].token.as_ref());
            let mclk_ref = mclk_index.and_then(|(_, index)| state.pins[index].token.as_ref());
            let resource_index = state.i2s.iter().position(|resource| {
                resource.as_ref().is_some_and(|resource| {
                    H::supports_i2s(resource, bclk_ref, ws_ref, dout_ref, din_ref, mclk_ref)
                })
            });
            let resource_index = match resource_index {
                Some(index) => index,
                None if state.i2s.iter().any(Option::is_some) => {
                    return Err(RuntimeOpenError::Unsupported { function: "I2S" });
                }
                None => return Err(RuntimeOpenError::NoController { protocol: "I2S" }),
            };
            let resource = state.i2s[resource_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "I2S" })?;
            let bclk = state.pins[bclk_index]
                .token
                .take()
                .ok_or(RuntimeOpenError::Unsupported { function: "I2S" })?;
            let ws = state.pins[ws_index]
                .token
                .take()
                .ok_or(RuntimeOpenError::Unsupported { function: "I2S" })?;
            let dout = dout_index.and_then(|(_, index)| state.pins[index].token.take());
            let din = din_index.and_then(|(_, index)| state.pins[index].token.take());
            let mclk = mclk_index.and_then(|(_, index)| state.pins[index].token.take());
            state.pins[bclk_index].owner = Some("I2S BCLK");
            state.pins[ws_index].owner = Some("I2S WS");
            if let Some((_, index)) = dout_index {
                state.pins[index].owner = Some("I2S DOUT");
            }
            if let Some((_, index)) = din_index {
                state.pins[index].owner = Some("I2S DIN");
            }
            if let Some((_, index)) = mclk_index {
                state.pins[index].owner = Some("I2S MCLK");
            }
            Ok((resource, bclk, ws, dout, din, mclk))
        })?;
        H::i2s(resource, bclk, ws, dout, din, mclk, request.format)
            .map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimePlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > I2cProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Bus = H::I2cBus;
    type Error = RuntimeOpenError<H::I2cError>;

    fn open_i2c(&self, request: I2cRequest<'_>) -> Result<Self::Bus, Self::Error> {
        let scl_index = self.resolve_pin(request.scl)?;
        let sda_index = self.resolve_pin(request.sda)?;
        self.ensure_distinct(&[(request.scl, scl_index), (request.sda, sda_index)])?;

        let (controller, scl, sda) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            for index in [scl_index, sda_index] {
                let pin = &state.pins[index];
                if pin.token.is_none() {
                    return Err(RuntimeOpenError::Resource(LeaseError::Busy {
                        resource: pin.name,
                        owner: pin.owner.unwrap_or("runtime function"),
                    }));
                }
            }
            let controller_index = state
                .i2c
                .iter()
                .position(Option::is_some)
                .ok_or(RuntimeOpenError::NoController { protocol: "I2C" })?;
            let controller = state.i2c[controller_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "I2C" })?;
            let scl = state.pins[scl_index]
                .token
                .take()
                .ok_or(RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: state.pins[scl_index].name,
                    owner: "runtime function",
                }))?;
            let sda = state.pins[sda_index]
                .token
                .take()
                .ok_or(RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: state.pins[sda_index].name,
                    owner: "runtime function",
                }))?;
            state.pins[scl_index].owner = Some("I2C SCL");
            state.pins[sda_index].owner = Some("I2C SDA");
            Ok((controller, scl, sda))
        })?;

        H::i2c(controller, scl, sda, request.frequency_hz).map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimePlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > SpiProvider for RuntimeIo<H, P, I, S, U, A, W, T>
{
    type Bus = H::SpiBus;
    type Error = RuntimeOpenError<H::SpiError>;

    fn open_spi(&self, request: SpiRequest<'_>) -> Result<Self::Bus, Self::Error> {
        if request.mosi.is_none() && request.miso.is_none() {
            return Err(RuntimeOpenError::MissingSpiData);
        }
        let sck_index = self.resolve_pin(request.sck)?;
        let mosi_index = request
            .mosi
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let miso_index = request
            .miso
            .map(|name| self.resolve_pin(name).map(|index| (name, index)))
            .transpose()?;
        let mut roles = alloc::vec![(request.sck, sck_index)];
        roles.extend(mosi_index);
        roles.extend(miso_index);
        self.ensure_distinct(&roles)?;

        let (controller, sck, mosi, miso) = critical_section::with(|section| {
            let mut state = self.state.borrow(section).borrow_mut();
            for (_, index) in &roles {
                let pin = &state.pins[*index];
                if pin.token.is_none() {
                    return Err(RuntimeOpenError::Resource(LeaseError::Busy {
                        resource: pin.name,
                        owner: pin.owner.unwrap_or("runtime function"),
                    }));
                }
            }
            let controller_index = state
                .spi
                .iter()
                .position(Option::is_some)
                .ok_or(RuntimeOpenError::NoController { protocol: "SPI" })?;
            let controller = state.spi[controller_index]
                .take()
                .ok_or(RuntimeOpenError::NoController { protocol: "SPI" })?;
            let sck = state.pins[sck_index].token.take().ok_or_else(|| {
                RuntimeOpenError::Resource(LeaseError::Busy {
                    resource: state.pins[sck_index].name,
                    owner: "runtime function",
                })
            })?;
            let mosi = match mosi_index {
                Some((_, index)) => state.pins[index].token.take(),
                None => None,
            };
            let miso = match miso_index {
                Some((_, index)) => state.pins[index].token.take(),
                None => None,
            };
            state.pins[sck_index].owner = Some("SPI SCK");
            if let Some((_, index)) = mosi_index {
                state.pins[index].owner = Some("SPI MOSI");
            }
            if let Some((_, index)) = miso_index {
                state.pins[index].owner = Some("SPI MISO");
            }
            Ok((controller, sck, mosi, miso))
        })?;

        H::spi(
            controller,
            sck,
            mosi,
            miso,
            request.frequency_hz,
            request.mode,
        )
        .map_err(RuntimeOpenError::Platform)
    }
}

impl<
        H: RuntimePlatform,
        const P: usize,
        const I: usize,
        const S: usize,
        const U: usize,
        const A: usize,
        const W: usize,
        const T: usize,
    > crate::ExposedIo for RuntimeIo<H, P, I, S, U, A, W, T>
{
}

/// Failure reported when a selected Board exposes no provider for a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedFunction {
    function: &'static str,
}

impl UnsupportedFunction {
    /// Creates an unsupported-function error for diagnostics.
    #[must_use]
    pub const fn new(function: &'static str) -> Self {
        Self { function }
    }
}

impl fmt::Display for UnsupportedFunction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "the selected Board does not expose runtime {} resources",
            self.function
        )
    }
}

impl core::error::Error for UnsupportedFunction {}

/// Constructs exclusive digital values from the shared exposed-I/O owner.
pub trait DigitalProvider {
    /// Digital pin value held by one application handle.
    type Pin: ConfigurableDigitalPin + Send + 'static;
    /// Failure while resolving or claiming a pin.
    type Error: core::error::Error;

    /// Returns whether a Board-visible pin name can be requested.
    fn digital_available(&self, name: &str) -> bool;

    /// Claims one exposed pin for digital use.
    ///
    /// Configuration is applied to the returned standard HAL value. The
    /// generated owner does not recreate the consumed physical pin token when
    /// the value is dropped.
    fn acquire_digital(&self, name: &str) -> Result<Self::Pin, Self::Error>;
}

/// Constructs analog functions from the shared exposed-I/O owner.
pub trait AnalogProvider {
    /// Analog input held by one application handle.
    type Input: AnalogInput;
    /// Analog output held by one application handle.
    type Output: AnalogOutput;
    /// Failure while resolving, claiming, or configuring an analog function.
    type Error: core::error::Error;

    /// Returns whether a free named pin supports analog input.
    fn analog_input_available(&self, name: &str) -> bool;
    /// Returns whether a free named pin supports analog output.
    fn analog_output_available(&self, name: &str) -> bool;
    /// Claims one exposed pin as an analog input.
    fn acquire_analog_input(&self, name: &str) -> Result<Self::Input, Self::Error>;
    /// Claims one exposed pin as an analog output.
    fn acquire_analog_output(&self, name: &str) -> Result<Self::Output, Self::Error>;
}

/// Runtime request for one PWM output on an exposed pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PwmRequest<'a> {
    /// Board-visible output pin.
    pub pin: &'a str,
    /// Requested carrier frequency.
    pub frequency_hz: u32,
}

/// Constructs PWM outputs from the shared exposed-I/O owner.
pub trait PwmProvider {
    /// Output implementing the ecosystem PWM contract.
    type Output: embedded_hal::pwm::SetDutyCycle + 'static;
    /// Failure while resolving, claiming, or configuring PWM.
    type Error: core::error::Error;

    /// Returns whether a free named pin supports PWM output.
    fn pwm_available(&self, name: &str) -> bool;
    /// Claims a pin and constructs one PWM output.
    fn open_pwm(&self, request: PwmRequest<'_>) -> Result<Self::Output, Self::Error>;
}

/// Number of data bits in one UART frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UartDataBits {
    /// Seven data bits.
    Seven,
    /// Eight data bits.
    #[default]
    Eight,
    /// Nine data bits.
    Nine,
}

/// UART parity mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UartParity {
    /// No parity bit.
    #[default]
    None,
    /// Even parity.
    Even,
    /// Odd parity.
    Odd,
}

/// Number of UART stop bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UartStopBits {
    /// One stop bit.
    #[default]
    One,
    /// Two stop bits.
    Two,
}

/// Portable UART line configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartConfig {
    /// Baud rate in symbols per second.
    pub baud: u32,
    /// Data bits per frame.
    pub data_bits: UartDataBits,
    /// Parity mode.
    pub parity: UartParity,
    /// Stop bits per frame.
    pub stop_bits: UartStopBits,
}

/// Runtime request for a UART function on selected exposed pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UartRequest<'a> {
    /// Optional Board-visible transmit pin.
    pub tx: Option<&'a str>,
    /// Optional Board-visible receive pin.
    pub rx: Option<&'a str>,
    /// Portable line configuration.
    pub config: UartConfig,
}

/// Constructs UART streams from runtime-selected exposed resources.
pub trait UartProvider {
    /// Byte stream implementing the ecosystem async I/O contracts.
    type Port: embedded_io_async::Read + embedded_io_async::Write + Send + 'static;
    /// Failure while validating, claiming, or configuring UART.
    type Error: core::error::Error;

    /// Returns whether the free named pins can form a UART function.
    fn uart_available(&self, tx: Option<&str>, rx: Option<&str>) -> bool;
    /// Atomically claims the selected pins and constructs a UART stream.
    fn open_uart(&self, request: UartRequest<'_>) -> Result<Self::Port, Self::Error>;
}

/// Runtime request for an I2S function on selected exposed pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2sRequest<'a> {
    /// Board-visible bit-clock pin.
    pub bclk: &'a str,
    /// Board-visible word-select pin.
    pub ws: &'a str,
    /// Optional Board-visible audio output pin.
    pub dout: Option<&'a str>,
    /// Optional Board-visible audio input pin.
    pub din: Option<&'a str>,
    /// Optional Board-visible master-clock pin.
    pub mclk: Option<&'a str>,
    /// PCM wire format.
    pub format: audio::PcmFormat,
}

/// Constructs I2S PCM streams from runtime-selected exposed resources.
pub trait I2sProvider {
    /// PCM data-plane stream.
    type Stream: audio::PcmStream + Send + 'static;
    /// Failure while validating, claiming, or configuring I2S.
    type Error: core::error::Error;

    /// Atomically claims selected pins and constructs an I2S stream.
    fn open_i2s(&self, request: I2sRequest<'_>) -> Result<Self::Stream, Self::Error>;
}

/// Runtime request for an I2C function on two exposed pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct I2cRequest<'a> {
    /// Board-visible pin used for SCL.
    pub scl: &'a str,
    /// Board-visible pin used for SDA.
    pub sda: &'a str,
    /// Requested bus frequency.
    pub frequency_hz: u32,
}

/// Constructs I2C buses from runtime-selected exposed resources.
pub trait I2cProvider {
    /// Bus value implementing the ecosystem I2C contract.
    type Bus: embedded_hal_async::i2c::I2c + Send + 'static;
    /// Failure while validating, claiming, or configuring the bus.
    type Error: core::error::Error;

    /// Selects a compatible free controller, then atomically claims and
    /// configures that controller, SCL, and SDA.
    fn open_i2c(&self, request: I2cRequest<'_>) -> Result<Self::Bus, Self::Error>;
}

/// Runtime request for an SPI function on selected exposed pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpiRequest<'a> {
    /// Board-visible serial-clock pin.
    pub sck: &'a str,
    /// Optional Board-visible controller-to-device data pin.
    pub mosi: Option<&'a str>,
    /// Optional Board-visible device-to-controller data pin.
    pub miso: Option<&'a str>,
    /// Requested bus frequency.
    pub frequency_hz: u32,
    /// Clock polarity and phase.
    pub mode: Mode,
}

/// Constructs SPI buses from runtime-selected exposed resources.
pub trait SpiProvider {
    /// Bus value implementing the ecosystem SPI contract.
    type Bus: embedded_hal_async::spi::SpiBus + Send + 'static;
    /// Failure while validating, claiming, or configuring the bus.
    type Error: core::error::Error;

    /// Selects a compatible free controller, then atomically claims and
    /// configures that controller and the selected signal pins.
    fn open_spi(&self, request: SpiRequest<'_>) -> Result<Self::Bus, Self::Error>;
}
