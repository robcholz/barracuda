//! Board HAL composition contract.
//!
//! A Board HAL owns peripheral Driver construction and returns semantic
//! capabilities. It does not construct or contain Platform resources.

#![no_std]

extern crate alloc;

mod providers;
mod runtime;

use core::{convert::Infallible, future::Future};

use embassy_executor::Spawner;
use embedded_hal::{
    digital::{InputPin, OutputPin, StatefulOutputPin},
    i2c, spi,
};

pub use providers::{
    AnalogProvider, DigitalProvider, I2cProvider, I2cRequest, PwmProvider, PwmRequest,
    RuntimeAnalogPlatform, RuntimeIo, RuntimeOpenError, RuntimePlatform, RuntimePwmPlatform,
    SpiProvider, SpiRequest, UnsupportedFunction,
};
pub use runtime::{LeaseError, ResourceKind};

/// Stable built-in audio capability API implemented by codec Drivers.
pub use barracuda_driver::audio;
/// Stable built-in camera capability API implemented by Camera Drivers.
pub use barracuda_driver::camera;
/// Stable built-in display capability API implemented by display Drivers.
pub use barracuda_driver::display;
/// Stable built-in indicator capability API implemented by indicator Drivers.
pub use barracuda_driver::indicator;
/// Stable built-in LED-strip capability API implemented by LED Drivers.
pub use barracuda_driver::led_strip;
/// Stable factory contract implemented by every peripheral Driver.
pub use barracuda_driver::PeripheralDriver;

/// Input bias selected while a VM-exposed GPIO operates as an input.
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

/// Electrical output driver selected for a VM-exposed GPIO.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputDrive {
    /// Actively drive both high and low levels.
    #[default]
    PushPull,
    /// Actively drive low and release the line for a high level.
    OpenDrain,
}

/// Digital latch level used while changing a VM-exposed GPIO to output mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DigitalLevel {
    /// Drive the logical low level.
    #[default]
    Low,
    /// Drive the logical high level.
    High,
}

/// Runtime configuration for a VM-exposed digital input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputConfig {
    /// Input bias.
    pub pull: Pull,
}

/// Runtime configuration for a VM-exposed digital output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputConfig {
    /// Output latch installed before enabling the driver.
    pub initial: DigitalLevel,
    /// Electrical output driver.
    pub drive: OutputDrive,
}

/// An exposed GPIO whose runtime owner may change its digital mode.
///
/// Reads and writes remain standard `embedded-hal` operations. This extension
/// exists for the VM GPIO package because `embedded-hal` does not standardize
/// runtime transitions between disabled, input, and output modes.
pub trait ConfigurableDigitalPin: InputPin + StatefulOutputPin {
    /// Configures the pin as a digital input.
    fn configure_input(&mut self, config: InputConfig) -> Result<(), Self::Error>;

    /// Installs the initial latch and configures the pin as an output.
    fn configure_output(&mut self, config: OutputConfig) -> Result<(), Self::Error>;

    /// Places the pin in its Platform-defined disconnected state.
    fn disable(&mut self) -> Result<(), Self::Error>;
}

/// Error family shared by one VM-exposed analog capability.
pub trait AnalogErrorType {
    /// Error returned by analog conversions.
    type Error: core::error::Error;
}

/// One Board-exposed analog input channel.
pub trait AnalogInput: AnalogErrorType {
    /// Highest raw sample value produced by this channel.
    fn max_value(&self) -> u32;

    /// Samples the channel in its native integer range.
    fn read(&mut self) -> Result<u32, Self::Error>;
}

/// One Board-exposed analog output channel.
pub trait AnalogOutput: AnalogErrorType {
    /// Highest raw value accepted by this channel.
    fn max_value(&self) -> u32;

    /// Writes one value in the channel's native integer range.
    fn write(&mut self, value: u32) -> Result<(), Self::Error>;
}

/// Unified owner of physical resources explicitly exposed by one Board.
///
/// Protocol-specific provider traits are implemented on this same value. They
/// atomically claim its move-only tokens before constructing standard HAL
/// values, preventing protocol packages from creating conflicting views.
pub trait ExposedIo: Send + Sync + 'static {}

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

/// Uninhabited LED-strip capability used when a Board has no built-in strip.
pub struct UnavailableLedStrip {
    never: Infallible,
}

impl led_strip::LedStrip for UnavailableLedStrip {
    type Error = Infallible;

    fn len(&self) -> usize {
        match self.never {}
    }

    fn write(&mut self, _pixels: &[led_strip::Rgb8]) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl led_strip::BuiltinLedStrip for NoBuiltinCapabilities {
    type LedStrip = UnavailableLedStrip;

    fn take_led_strip(&mut self) -> Option<Self::LedStrip> {
        None
    }
}

/// Uninhabited camera capability used when a Board has no built-in camera.
pub struct UnavailableCamera {
    never: Infallible,
}

impl camera::Camera for UnavailableCamera {
    type Error = Infallible;

    fn descriptor(&self) -> camera::CameraDescriptor {
        match self.never {}
    }

    async fn capture<'a>(
        &'a mut self,
        _buffer: &'a mut [u8],
    ) -> Result<camera::CapturedFrame, Self::Error> {
        match self.never {}
    }
}

impl camera::BuiltinCamera for NoBuiltinCapabilities {
    type Camera = UnavailableCamera;

    fn take_camera(&mut self) -> Option<Self::Camera> {
        None
    }
}

/// Uninhabited display capability used when a Board has no built-in display.
pub struct UnavailableDisplay {
    never: Infallible,
}

impl embedded_graphics_core::geometry::OriginDimensions for UnavailableDisplay {
    fn size(&self) -> embedded_graphics_core::geometry::Size {
        match self.never {}
    }
}

impl embedded_graphics_core::draw_target::DrawTarget for UnavailableDisplay {
    type Color = embedded_graphics_core::pixelcolor::Rgb888;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, _pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics_core::Pixel<Self::Color>>,
    {
        match self.never {}
    }
}

impl display::Display for UnavailableDisplay {
    type ControlError = Infallible;
    type RenderError = Infallible;

    fn descriptor(&self) -> display::DisplayDescriptor {
        match self.never {}
    }

    fn draw_rgb888(
        &mut self,
        _area: embedded_graphics_core::primitives::Rectangle,
        _pixels: &[embedded_graphics_core::pixelcolor::Rgb888],
    ) -> Result<(), Self::RenderError> {
        match self.never {}
    }

    async fn flush(&mut self, _request: display::RefreshRequest) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_power(&mut self, _power: display::DisplayPower) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_brightness(&mut self, _brightness: u8) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn set_orientation(
        &mut self,
        _orientation: display::DisplayOrientation,
    ) -> Result<(), Self::ControlError> {
        match self.never {}
    }

    async fn wait_ready(&mut self) -> Result<(), Self::ControlError> {
        match self.never {}
    }
}

impl display::BuiltinDisplay for NoBuiltinCapabilities {
    type Display = UnavailableDisplay;

    fn take_display(&mut self) -> Option<Self::Display> {
        None
    }
}

/// Explicit absence of exposed Board I/O capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoExposedIo;

/// Uninhabited GPIO value that makes an empty I/O surface fully typed.
pub struct UnavailableGpio {
    never: Infallible,
}

/// Uninhabited analog input used by Boards without runtime analog support.
pub struct UnavailableAnalogInput {
    never: Infallible,
}

impl AnalogErrorType for UnavailableAnalogInput {
    type Error = Infallible;
}

impl AnalogInput for UnavailableAnalogInput {
    fn max_value(&self) -> u32 {
        match self.never {}
    }

    fn read(&mut self) -> Result<u32, Self::Error> {
        match self.never {}
    }
}

/// Uninhabited analog output used by Boards without runtime analog support.
pub struct UnavailableAnalogOutput {
    never: Infallible,
}

/// Uninhabited PWM output used by Boards without runtime PWM support.
pub struct UnavailablePwm {
    never: Infallible,
}

impl embedded_hal::pwm::ErrorType for UnavailablePwm {
    type Error = Infallible;
}

impl embedded_hal::pwm::SetDutyCycle for UnavailablePwm {
    fn max_duty_cycle(&self) -> u16 {
        match self.never {}
    }

    fn set_duty_cycle(&mut self, _duty: u16) -> Result<(), Self::Error> {
        match self.never {}
    }
}

impl AnalogErrorType for UnavailableAnalogOutput {
    type Error = Infallible;
}

impl AnalogOutput for UnavailableAnalogOutput {
    fn max_value(&self) -> u32 {
        match self.never {}
    }

    fn write(&mut self, _value: u32) -> Result<(), Self::Error> {
        match self.never {}
    }
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

impl ExposedIo for NoExposedIo {}

impl DigitalProvider for NoExposedIo {
    type Pin = UnavailableGpio;
    type Error = UnsupportedFunction;

    fn digital_available(&self, _name: &str) -> bool {
        false
    }

    fn acquire_digital(&self, _name: &str) -> Result<Self::Pin, Self::Error> {
        Err(UnsupportedFunction::new("digital I/O"))
    }
}

impl AnalogProvider for NoExposedIo {
    type Input = UnavailableAnalogInput;
    type Output = UnavailableAnalogOutput;
    type Error = UnsupportedFunction;

    fn analog_input_available(&self, _name: &str) -> bool {
        false
    }

    fn analog_output_available(&self, _name: &str) -> bool {
        false
    }

    fn acquire_analog_input(&self, _name: &str) -> Result<Self::Input, Self::Error> {
        Err(UnsupportedFunction::new("analog input"))
    }

    fn acquire_analog_output(&self, _name: &str) -> Result<Self::Output, Self::Error> {
        Err(UnsupportedFunction::new("analog output"))
    }
}

impl PwmProvider for NoExposedIo {
    type Output = UnavailablePwm;
    type Error = UnsupportedFunction;

    fn pwm_available(&self, _name: &str) -> bool {
        false
    }

    fn open_pwm(&self, _request: PwmRequest<'_>) -> Result<Self::Output, Self::Error> {
        Err(UnsupportedFunction::new("PWM"))
    }
}

impl I2cProvider for NoExposedIo {
    type Bus = UnavailableI2c;
    type Error = UnsupportedFunction;

    fn open_i2c(&self, _request: I2cRequest<'_>) -> Result<Self::Bus, Self::Error> {
        Err(UnsupportedFunction::new("I2C"))
    }
}

impl SpiProvider for NoExposedIo {
    type Bus = UnavailableSpi;
    type Error = UnsupportedFunction;

    fn open_spi(&self, _request: SpiRequest<'_>) -> Result<Self::Bus, Self::Error> {
        Err(UnsupportedFunction::new("SPI"))
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
