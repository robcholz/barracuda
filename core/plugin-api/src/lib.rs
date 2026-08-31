//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

mod hardware;

use barracuda_board_hal::{BoardHalResources, NoBuiltinCapabilities, NoExposedIo};
pub use barracuda_plugin_macros::plugin;
pub use embassy_net::Stack;
pub use hardware::{
    LuaGpioHardware, LuaHardwareError, LuaHardwareFuture, LuaHardwareResult, LuaI2cHardware, LuaIo,
    LuaSpiHardware,
};
pub use http_client::ClientFactory;

/// Fixed System resources available while constructing a Plugin.
///
/// Plugins move owned resources or copy shared capabilities during `new` and
/// do not retain a reference to the context itself.
pub struct PluginContext<Builtins = NoBuiltinCapabilities, Io = NoExposedIo> {
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// Factory for constructing HTTP clients over the Platform network and TLS
    /// capabilities.
    pub http_clients: ClientFactory<'static>,
    /// Complete HAL produced by the selected Board composition.
    pub hal: BoardHalResources<Builtins, Io>,
}

impl<Builtins, Io> PluginContext<Builtins, Io> {
    /// Creates the unified Plugin construction context with the selected HAL.
    #[must_use]
    pub const fn from_hal(
        ip_stack: Stack<'static>,
        http_clients: ClientFactory<'static>,
        hal: BoardHalResources<Builtins, Io>,
    ) -> Self {
        Self {
            ip_stack,
            http_clients,
            hal,
        }
    }
}

impl PluginContext {
    /// Creates a construction context carrying an explicitly empty HAL.
    #[must_use]
    pub const fn new(ip_stack: Stack<'static>, http_clients: ClientFactory<'static>) -> Self {
        Self::from_hal(
            ip_stack,
            http_clients,
            BoardHalResources::new(NoBuiltinCapabilities, NoExposedIo),
        )
    }
}
