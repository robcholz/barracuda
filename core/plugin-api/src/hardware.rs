//! Move-only hardware values adapted for Lua-owning Plugins.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{future::Future, pin::Pin};

use barracuda_board_hal::{InputConfig, NoExposedIo, OutputConfig};

/// Result returned by one Lua-owned hardware operation.
pub type LuaHardwareResult<T> = Result<T, LuaHardwareError>;

/// Asynchronous operation borrowing one Lua-owned hardware value mutably.
pub type LuaHardwareFuture<'a, T> = Pin<Box<dyn Future<Output = LuaHardwareResult<T>> + Send + 'a>>;

/// Hardware or adapter failure converted at the Lua ownership boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LuaHardwareError {
    message: String,
}

impl LuaHardwareError {
    /// Creates an error with a Lua-facing diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the adapter diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for LuaHardwareError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for LuaHardwareError {}

/// Move-owned named GPIO values adapted for the Lua GPIO Plugin.
pub trait LuaGpioHardware: Send {
    /// Returns whether this value owns the named Board-exposed pin.
    fn contains(&self, name: &str) -> bool;

    /// Configures one pin as an input.
    fn configure_input(&mut self, name: String, config: InputConfig) -> LuaHardwareFuture<'_, ()>;

    /// Configures one pin as an output.
    fn configure_output(&mut self, name: String, config: OutputConfig)
        -> LuaHardwareFuture<'_, ()>;

    /// Places one pin in its disconnected state.
    fn disable(&mut self, name: String) -> LuaHardwareFuture<'_, ()>;

    /// Reads one pin's logical level.
    fn read(&mut self, name: String) -> LuaHardwareFuture<'_, bool>;

    /// Writes one pin's logical level.
    fn write(&mut self, name: String, high: bool) -> LuaHardwareFuture<'_, ()>;
}

/// Move-owned named I2C controllers adapted for the Lua I2C Plugin.
pub trait LuaI2cHardware: Send {
    /// Returns whether this value owns the named Board-exposed controller.
    fn contains(&self, name: &str) -> bool;

    /// Reads bytes from one target address.
    fn read(&mut self, name: String, address: u16, length: usize)
        -> LuaHardwareFuture<'_, Vec<u8>>;

    /// Writes bytes to one target address.
    fn write(&mut self, name: String, address: u16, bytes: Vec<u8>) -> LuaHardwareFuture<'_, ()>;

    /// Performs one write followed by a repeated-start read.
    fn write_read(
        &mut self,
        name: String,
        address: u16,
        bytes: Vec<u8>,
        read_length: usize,
    ) -> LuaHardwareFuture<'_, Vec<u8>>;
}

/// Move-owned named SPI buses adapted for the Lua SPI Plugin.
pub trait LuaSpiHardware: Send {
    /// Returns whether this value owns the named Board-exposed bus.
    fn contains(&self, name: &str) -> bool;

    /// Reads bytes while transmitting the adapter's fill value.
    fn read(&mut self, name: String, length: usize) -> LuaHardwareFuture<'_, Vec<u8>>;

    /// Writes bytes and discards simultaneously received data.
    fn write(&mut self, name: String, bytes: Vec<u8>) -> LuaHardwareFuture<'_, ()>;

    /// Transfers independent write and read buffers.
    fn transfer(
        &mut self,
        name: String,
        write: Vec<u8>,
        read_length: usize,
    ) -> LuaHardwareFuture<'_, Vec<u8>>;

    /// Transfers one buffer in place and returns its received contents.
    fn transfer_in_place(&mut self, name: String, bytes: Vec<u8>)
        -> LuaHardwareFuture<'_, Vec<u8>>;
}

/// Move-only hardware values waiting for their owning Lua Plugins to take them.
#[derive(Default)]
pub struct LuaHardwareResources {
    gpio: Option<Box<dyn LuaGpioHardware>>,
    i2c: Option<Box<dyn LuaI2cHardware>>,
    spi: Option<Box<dyn LuaSpiHardware>>,
}

impl LuaHardwareResources {
    /// Creates an empty value bundle.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gpio: None,
            i2c: None,
            spi: None,
        }
    }

    /// Adds the GPIO value produced by selected-target composition.
    #[must_use]
    pub fn with_gpio(mut self, hardware: Box<dyn LuaGpioHardware>) -> Self {
        self.gpio = Some(hardware);
        self
    }

    /// Adds the I2C value produced by selected-target composition.
    #[must_use]
    pub fn with_i2c(mut self, hardware: Box<dyn LuaI2cHardware>) -> Self {
        self.i2c = Some(hardware);
        self
    }

    /// Adds the SPI value produced by selected-target composition.
    #[must_use]
    pub fn with_spi(mut self, hardware: Box<dyn LuaSpiHardware>) -> Self {
        self.spi = Some(hardware);
        self
    }

    /// Moves the GPIO value to its owning Plugin exactly once.
    pub fn take_gpio(&mut self) -> Option<Box<dyn LuaGpioHardware>> {
        self.gpio.take()
    }

    /// Moves the I2C value to its owning Plugin exactly once.
    pub fn take_i2c(&mut self) -> Option<Box<dyn LuaI2cHardware>> {
        self.i2c.take()
    }

    /// Moves the SPI value to its owning Plugin exactly once.
    pub fn take_spi(&mut self) -> Option<Box<dyn LuaSpiHardware>> {
        self.spi.take()
    }
}

/// Selected-target adaptation from raw Board I/O values into Lua-owned values.
///
/// Concrete implementations belong to target composition rather than the
/// Board HAL contract so Lua naming and async adaptation remain outside HAL.
pub trait IntoLuaHardwareResources {
    /// Consumes the selected Board values into the Lua construction boundary.
    fn into_lua_hardware_resources(self) -> LuaHardwareResources;
}

impl IntoLuaHardwareResources for NoExposedIo {
    fn into_lua_hardware_resources(self) -> LuaHardwareResources {
        LuaHardwareResources::new()
    }
}
