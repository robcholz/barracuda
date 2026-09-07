//! Board HAL composition contract.
//!
//! A Board HAL owns peripheral Driver construction and returns semantic
//! capabilities. It does not construct or contain Platform resources.

#![no_std]

use core::{convert::Infallible, future::Future};

use embassy_executor::Spawner;
use embedded_hal::{
    digital::{InputPin, OutputPin, StatefulOutputPin},
    i2c, spi,
};

/// Stable built-in display capability API implemented by display Drivers.
pub use barracuda_driver::display;
/// Stable built-in indicator capability API implemented by indicator Drivers.
pub use barracuda_driver::indicator;
/// Stable factory contract implemented by every peripheral Driver.
pub use barracuda_driver::PeripheralDriver;

/// Input bias selected while a GPIO operates as a digital input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pull {
    /// Leave the input electrically unbiased.
    #[default]
    None,
    /// Enable the pin's pull-up resistor.
    Up,
    /// Enable the pin's pull-down resistor.
    Down,
}

/// Electrical output driver selected for a digital GPIO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputDrive {
    /// Actively drive both high and low levels.
    #[default]
    PushPull,
    /// Actively drive low and release the line for a high level.
    OpenDrain,
}

/// Digital latch level used while changing a GPIO into output mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DigitalLevel {
    /// Drive the logical low level.
    #[default]
    Low,
    /// Drive the logical high level.
    High,
}

/// Runtime configuration for a digital input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputConfig {
    /// Input bias.
    pub pull: Pull,
}

/// Runtime configuration for a digital output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputConfig {
    /// Output latch value installed before enabling the driver.
    pub initial: DigitalLevel,
    /// Electrical output driver.
    pub drive: OutputDrive,
}

/// A GPIO whose owner may change its digital mode at runtime.
///
/// Reading and writing remain the standard [`InputPin`] and
/// [`StatefulOutputPin`] operations. Implementations configure the selected
/// mode before those operations are handed to a consumer.
pub trait ConfigurableDigitalPin: InputPin + StatefulOutputPin {
    /// Configures the pin as a digital input.
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error>;

    /// Installs the initial latch value and configures the pin as an output.
    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error>;

    /// Places the pin in its Platform-defined disconnected state.
    fn disable(&mut self) -> Result<(), Self::Error>;
}

/// Error family shared by one analog capability.
pub trait AnalogErrorType {
    /// Error returned by analog conversions.
    type Error: core::error::Error;
}

/// One Board-exposed analog input channel.
pub trait AnalogInput: AnalogErrorType {
    /// Highest raw sample value produced by this channel's current configuration.
    fn max_value(&self) -> u32;

    /// Samples the channel in its native integer range.
    fn read(&mut self) -> Result<u32, Self::Error>;
}

/// One Board-exposed analog output channel.
pub trait AnalogOutput: AnalogErrorType {
    /// Highest raw value accepted by this channel's current configuration.
    fn max_value(&self) -> u32;

    /// Writes one value in the channel's native integer range.
    fn write(&mut self, value: u32) -> Result<(), Self::Error>;
}

/// A fixed-capacity set of concrete hardware values addressable by Board name.
///
/// Naming is the only abstraction added here. Each stored value keeps its
/// concrete type and implements the corresponding `embedded-hal` trait
/// directly.
pub struct NamedResources<T, const N: usize> {
    entries: [Option<NamedResource<T>>; N],
}

struct NamedResource<T> {
    name: &'static str,
    resource: T,
}

impl<T, const N: usize> NamedResources<T, N> {
    /// Creates a named set from Board-generated entries.
    #[must_use]
    pub fn new(entries: [(&'static str, T); N]) -> Self {
        Self {
            entries: entries.map(|(name, resource)| Some(NamedResource { name, resource })),
        }
    }
}

impl<T> NamedResources<T, 0> {
    /// Creates an empty typed resource set.
    #[must_use]
    pub const fn empty() -> Self {
        Self { entries: [] }
    }
}

/// Runtime name lookup over a set of otherwise concrete hardware values.
pub trait ResourceSet {
    /// Concrete hardware value stored in this set.
    type Resource;

    /// Returns whether the Board exposed `name` in this set.
    fn contains(&self, name: &str) -> bool;

    /// Borrows one exposed resource by Board name.
    fn get(&self, name: &str) -> Option<&Self::Resource>;

    /// Mutably borrows one exposed resource by Board name.
    fn get_mut(&mut self, name: &str) -> Option<&mut Self::Resource>;
}

impl<T, const N: usize> ResourceSet for NamedResources<T, N> {
    type Resource = T;

    fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    fn get(&self, name: &str) -> Option<&Self::Resource> {
        self.entries
            .iter()
            .filter_map(Option::as_ref)
            .find(|entry| entry.name == name)
            .map(|entry| &entry.resource)
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Self::Resource> {
        self.entries
            .iter_mut()
            .filter_map(Option::as_mut)
            .find(|entry| entry.name == name)
            .map(|entry| &mut entry.resource)
    }
}

/// Move-only ownership access to the I/O explicitly exposed by one Board.
///
/// Implementations normally store each set in an `Option`. Calling a `take_*`
/// method transfers the concrete set to its sole consumer and subsequent calls
/// return `None`.
pub trait ExposedIo {
    /// Concrete named GPIO set.
    type Gpio: ResourceSet;
    /// Concrete named I2C set.
    type I2c: ResourceSet;
    /// Concrete named SPI set.
    type Spi: ResourceSet;

    /// Moves the Board-exposed GPIO set to its owner once.
    fn take_gpio(&mut self) -> Option<Self::Gpio>;

    /// Moves the Board-exposed I2C set to its owner once.
    fn take_i2c(&mut self) -> Option<Self::I2c>;

    /// Moves the Board-exposed SPI set to its owner once.
    fn take_spi(&mut self) -> Option<Self::Spi>;
}

/// Resources produced by one selected Board HAL.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoardHalResources<Builtins, Io> {
    /// Fully constructed built-in peripheral capabilities.
    pub builtins: Builtins,
    /// Explicitly exposed Board I/O capabilities.
    pub io: Io,
}

impl<Builtins, Io> BoardHalResources<Builtins, Io> {
    /// Combines the two independently named Board hardware surfaces.
    #[must_use]
    pub const fn new(builtins: Builtins, io: Io) -> Self {
        Self { builtins, io }
    }
}

/// Result of initializing one statically selected [`BoardHal`].
pub type BoardHalInitResult<H> = Result<<H as BoardHal>::Resources, <H as BoardHal>::Error>;

/// Statically composed Board matrix and peripheral Drivers.
pub trait BoardHal: Sized + 'static {
    /// Move-only chip resources assigned to this Board HAL by Target.
    type Bindings;
    /// Semantic hardware capabilities exposed to System or Plugins.
    type Resources;
    /// Board HAL initialization failure.
    type Error;

    /// Initializes peripheral Drivers for one concrete Board matrix.
    fn initialize(
        spawner: Spawner,
        bindings: Self::Bindings,
    ) -> impl Future<Output = BoardHalInitResult<Self>>;
}

/// HAL for desktop Boards that declare no peripheral Drivers.
pub struct EmptyBoardHal;

/// Explicit absence of built-in peripheral capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoBuiltinCapabilities;

/// Explicit absence of exposed Board I/O capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoExposedIo;

/// Uninhabited GPIO value that makes an empty I/O surface fully typed.
pub struct UnavailableGpio {
    never: Infallible,
}

impl UnavailableGpio {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl embedded_hal::digital::ErrorType for UnavailableGpio {
    type Error = Infallible;
}

impl InputPin for UnavailableGpio {
    fn is_high(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }

    fn is_low(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }
}

impl OutputPin for UnavailableGpio {
    fn set_low(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn set_high(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

impl StatefulOutputPin for UnavailableGpio {
    fn is_set_high(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }

    fn is_set_low(&mut self) -> Result<bool, Self::Error> {
        self.unreachable()
    }
}

impl ConfigurableDigitalPin for UnavailableGpio {
    fn configure_input(&mut self, _config: InputConfig) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn configure_output(&mut self, _config: OutputConfig) -> Result<(), Self::Error> {
        self.unreachable()
    }

    fn disable(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

/// Uninhabited I2C value that makes an empty I/O surface fully typed.
pub struct UnavailableI2c {
    never: Infallible,
}

impl UnavailableI2c {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl i2c::ErrorType for UnavailableI2c {
    type Error = Infallible;
}

impl embedded_hal_async::i2c::I2c for UnavailableI2c {
    async fn transaction(
        &mut self,
        _address: u8,
        _operations: &mut [i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

/// Uninhabited SPI value that makes an empty I/O surface fully typed.
pub struct UnavailableSpi {
    never: Infallible,
}

impl UnavailableSpi {
    fn unreachable<T>(&self) -> T {
        match self.never {}
    }
}

impl spi::ErrorType for UnavailableSpi {
    type Error = Infallible;
}

impl embedded_hal_async::spi::SpiBus for UnavailableSpi {
    async fn read(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn write(&mut self, _words: &[u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn transfer(&mut self, _read: &mut [u8], _write: &[u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn transfer_in_place(&mut self, _words: &mut [u8]) -> Result<(), Self::Error> {
        self.unreachable()
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.unreachable()
    }
}

impl ExposedIo for NoExposedIo {
    type Gpio = NamedResources<UnavailableGpio, 0>;
    type I2c = NamedResources<UnavailableI2c, 0>;
    type Spi = NamedResources<UnavailableSpi, 0>;

    fn take_gpio(&mut self) -> Option<Self::Gpio> {
        None
    }

    fn take_i2c(&mut self) -> Option<Self::I2c> {
        None
    }

    fn take_spi(&mut self) -> Option<Self::Spi> {
        None
    }
}

/// Complete empty Board hardware surface.
pub type NoBoardCapabilities = BoardHalResources<NoBuiltinCapabilities, NoExposedIo>;

impl BoardHal for EmptyBoardHal {
    type Bindings = ();
    type Resources = NoBoardCapabilities;
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _bindings: ()) -> BoardHalInitResult<Self> {
        Ok(BoardHalResources::new(NoBuiltinCapabilities, NoExposedIo))
    }
}
