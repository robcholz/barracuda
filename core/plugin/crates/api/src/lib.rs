//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use barracuda_board_hal::{BoardHalResources, NoBuiltinCapabilities, NoExposedIo};
pub use embassy_net::Stack;
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
    pub hal: BoardHalResources<Builtins, Arc<Io>>,
}

impl<Builtins, Io> PluginContext<Builtins, Io> {
    /// Creates the unified Plugin construction context with the selected HAL.
    #[must_use]
    pub fn from_hal(
        ip_stack: Stack<'static>,
        http_clients: ClientFactory<'static>,
        hal: BoardHalResources<Builtins, Io>,
    ) -> Self {
        let BoardHalResources { builtins, io } = hal;
        Self {
            ip_stack,
            http_clients,
            hal: BoardHalResources::new(builtins, Arc::new(io)),
        }
    }
}

impl PluginContext {
    /// Creates a construction context carrying an explicitly empty HAL.
    #[must_use]
    pub fn new(ip_stack: Stack<'static>, http_clients: ClientFactory<'static>) -> Self {
        Self::from_hal(
            ip_stack,
            http_clients,
            BoardHalResources::new(NoBuiltinCapabilities, NoExposedIo),
        )
    }
}
