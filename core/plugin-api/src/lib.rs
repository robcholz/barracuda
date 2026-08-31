//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

mod hardware;

use core::cell::RefCell;

pub use embassy_net::Stack;
pub use hardware::{
    IntoLuaHardwareResources, LuaGpioHardware, LuaHardwareError, LuaHardwareFuture,
    LuaHardwareResources, LuaHardwareResult, LuaI2cHardware, LuaSpiHardware,
};
pub use http_client::ClientFactory;

/// Fixed System resources available while constructing a Plugin.
///
/// Plugins move owned resources or copy shared capabilities during `new` and
/// do not retain a reference to the context itself.
pub struct PluginContext {
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// Factory for constructing HTTP clients over the Platform network and TLS
    /// capabilities.
    pub http_clients: ClientFactory<'static>,
    lua_hardware: RefCell<LuaHardwareResources>,
}

impl PluginContext {
    /// Creates the unified Plugin construction context.
    #[must_use]
    pub const fn new(ip_stack: Stack<'static>, http_clients: ClientFactory<'static>) -> Self {
        Self {
            ip_stack,
            http_clients,
            lua_hardware: RefCell::new(LuaHardwareResources::new()),
        }
    }

    /// Installs move-only hardware values adapted by selected-target composition.
    #[must_use]
    pub fn with_lua_hardware(self, hardware: LuaHardwareResources) -> Self {
        *self.lua_hardware.borrow_mut() = hardware;
        self
    }

    /// Moves the Board-exposed GPIO value to its owning Plugin once.
    pub fn take_gpio(&self) -> Option<alloc::boxed::Box<dyn LuaGpioHardware>> {
        self.lua_hardware.borrow_mut().take_gpio()
    }

    /// Moves the Board-exposed I2C value to its owning Plugin once.
    pub fn take_i2c(&self) -> Option<alloc::boxed::Box<dyn LuaI2cHardware>> {
        self.lua_hardware.borrow_mut().take_i2c()
    }

    /// Moves the Board-exposed SPI value to its owning Plugin once.
    pub fn take_spi(&self) -> Option<alloc::boxed::Box<dyn LuaSpiHardware>> {
        self.lua_hardware.borrow_mut().take_spi()
    }
}
