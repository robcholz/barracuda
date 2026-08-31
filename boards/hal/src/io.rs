//! Type-erased exposed-I/O handles at the System-to-Plugin ownership boundary.

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{future::Future, pin::Pin};

use crate::{InputConfig, OutputConfig};

/// Result returned by an exposed-I/O service operation.
pub type IoServiceResult<T> = Result<T, IoServiceError>;

/// Owned asynchronous operation returned by an exposed-I/O service.
pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = IoServiceResult<T>> + Send + 'a>>;

/// Hardware or adapter failure reported across the scripting ownership boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IoServiceError {
    message: String,
}

impl IoServiceError {
    /// Creates an error with a caller-facing diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic supplied by the concrete adapter.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for IoServiceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl core::error::Error for IoServiceError {}

/// Named digital GPIO operations owned by the GPIO Plugin.
pub trait GpioService: Send + Sync {
    /// Returns whether the selected Board exposed this logical pin name.
    fn contains(&self, name: &str) -> bool;

    /// Configures one exposed pin as an input.
    fn configure_input(&self, name: String, config: InputConfig) -> ServiceFuture<'_, ()>;

    /// Configures one exposed pin as an output.
    fn configure_output(&self, name: String, config: OutputConfig) -> ServiceFuture<'_, ()>;

    /// Places one exposed pin in its disconnected state.
    fn disable(&self, name: String) -> ServiceFuture<'_, ()>;

    /// Reads the logical input level of one exposed pin.
    fn read(&self, name: String) -> ServiceFuture<'_, bool>;

    /// Writes the logical output level of one exposed pin.
    fn write(&self, name: String, high: bool) -> ServiceFuture<'_, ()>;
}

/// Named I2C controller operations owned by the I2C Plugin.
pub trait I2cService: Send + Sync {
    /// Returns whether the selected Board exposed this logical controller name.
    fn contains(&self, name: &str) -> bool;

    /// Reads bytes from one target address.
    fn read(&self, name: String, address: u16, length: usize) -> ServiceFuture<'_, Vec<u8>>;

    /// Writes bytes to one target address.
    fn write(&self, name: String, address: u16, bytes: Vec<u8>) -> ServiceFuture<'_, ()>;

    /// Performs one write followed by a repeated-start read.
    fn write_read(
        &self,
        name: String,
        address: u16,
        bytes: Vec<u8>,
        read_length: usize,
    ) -> ServiceFuture<'_, Vec<u8>>;
}

/// Named SPI bus operations owned by the SPI Plugin.
pub trait SpiService: Send + Sync {
    /// Returns whether the selected Board exposed this logical bus name.
    fn contains(&self, name: &str) -> bool;

    /// Reads bytes while transmitting the adapter's fill value.
    fn read(&self, name: String, length: usize) -> ServiceFuture<'_, Vec<u8>>;

    /// Writes bytes and discards simultaneously received data.
    fn write(&self, name: String, bytes: Vec<u8>) -> ServiceFuture<'_, ()>;

    /// Transfers independent write and read buffers in one bus operation.
    fn transfer(
        &self,
        name: String,
        write: Vec<u8>,
        read_length: usize,
    ) -> ServiceFuture<'_, Vec<u8>>;

    /// Transfers one buffer in place and returns its received contents.
    fn transfer_in_place(&self, name: String, bytes: Vec<u8>) -> ServiceFuture<'_, Vec<u8>>;
}

/// Type-erased exposed-I/O handles installed into the shared Plugin context.
#[derive(Clone, Default)]
pub struct HardwareServices {
    gpio: Option<Arc<dyn GpioService>>,
    i2c: Option<Arc<dyn I2cService>>,
    spi: Option<Arc<dyn SpiService>>,
}

impl HardwareServices {
    /// Creates a context with no exposed hardware services.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gpio: None,
            i2c: None,
            spi: None,
        }
    }

    /// Installs the GPIO service produced by the selected Board adapter.
    #[must_use]
    pub fn with_gpio(mut self, service: Arc<dyn GpioService>) -> Self {
        self.gpio = Some(service);
        self
    }

    /// Installs the I2C service produced by the selected Board adapter.
    #[must_use]
    pub fn with_i2c(mut self, service: Arc<dyn I2cService>) -> Self {
        self.i2c = Some(service);
        self
    }

    /// Installs the SPI service produced by the selected Board adapter.
    #[must_use]
    pub fn with_spi(mut self, service: Arc<dyn SpiService>) -> Self {
        self.spi = Some(service);
        self
    }

    /// Clones the selected Board's GPIO service handle, when present.
    #[must_use]
    pub fn gpio(&self) -> Option<Arc<dyn GpioService>> {
        self.gpio.clone()
    }

    /// Clones the selected Board's I2C service handle, when present.
    #[must_use]
    pub fn i2c(&self) -> Option<Arc<dyn I2cService>> {
        self.i2c.clone()
    }

    /// Clones the selected Board's SPI service handle, when present.
    #[must_use]
    pub fn spi(&self) -> Option<Arc<dyn SpiService>> {
        self.spi.clone()
    }
}

/// Conversion implemented by each generated Board exposed-I/O bundle.
pub trait IntoHardwareServices {
    /// Moves Board resources into the Plugin-owned service boundary.
    fn into_hardware_services(self) -> HardwareServices;
}
