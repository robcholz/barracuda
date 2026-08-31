//! Board HAL composition contract.
//!
//! A Board HAL owns peripheral Driver construction and returns semantic
//! capabilities. It does not construct or contain Platform resources.

#![no_std]

extern crate alloc;

use core::{convert::Infallible, future::Future};

use barracuda_board::Board;
use embassy_executor::Spawner;
use embedded_hal::digital::{InputPin, StatefulOutputPin};

mod io;

pub use io::{
    GpioService, HardwareServices, I2cService, IntoHardwareServices, IoServiceError,
    IoServiceResult, ServiceFuture, SpiService,
};

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
    /// Semantic hardware capabilities exposed to System or Plugins.
    type Resources;
    /// Board HAL initialization failure.
    type Error;

    /// Initializes peripheral Drivers for one concrete Board matrix.
    fn initialize(
        spawner: Spawner,
        board: &'static Board,
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

impl IntoHardwareServices for NoExposedIo {
    fn into_hardware_services(self) -> HardwareServices {
        HardwareServices::new()
    }
}

/// Complete empty Board hardware surface.
pub type NoBoardCapabilities = BoardHalResources<NoBuiltinCapabilities, NoExposedIo>;

impl BoardHal for EmptyBoardHal {
    type Resources = NoBoardCapabilities;
    type Error = Infallible;

    async fn initialize(_spawner: Spawner, _board: &'static Board) -> BoardHalInitResult<Self> {
        Ok(BoardHalResources::new(NoBuiltinCapabilities, NoExposedIo))
    }
}
