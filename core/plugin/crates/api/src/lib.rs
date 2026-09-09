//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use barracuda_board_hal::{BoardResources, NoExposedIo, NoPeripherals};
pub use embassy_net::Stack;
pub use http_client::ClientFactory;

/// Fixed System resources available while constructing a Plugin.
///
/// Plugins move owned resources or copy shared capabilities during `new` and
/// do not retain a reference to the context itself.
pub struct PluginContext<Peripherals = NoPeripherals, ExposedIo = NoExposedIo> {
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// Factory for constructing HTTP clients over the Platform network and TLS
    /// capabilities.
    pub http_clients: ClientFactory<'static>,
    /// Complete HAL produced by the selected Board composition.
    pub hal: BoardResources<Peripherals, Arc<ExposedIo>>,
}

impl<Peripherals, ExposedIo> PluginContext<Peripherals, ExposedIo> {
    /// Creates the unified Plugin construction context with the selected HAL.
    #[must_use]
    pub fn from_hal(
        ip_stack: Stack<'static>,
        http_clients: ClientFactory<'static>,
        hal: BoardResources<Peripherals, ExposedIo>,
    ) -> Self {
        let BoardResources {
            peripherals,
            exposed_io,
        } = hal;
        Self {
            ip_stack,
            http_clients,
            hal: BoardResources::new(peripherals, Arc::new(exposed_io)),
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
            BoardResources::new(NoPeripherals, NoExposedIo),
        )
    }
}
