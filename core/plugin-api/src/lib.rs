//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

pub use embassy_net::Stack;
pub use http_client::ClientFactory;

/// Fixed System resources available while constructing a Plugin.
///
/// Plugins take or clone the handles they need during `new` and do not retain
/// a reference to the context itself.
pub struct PluginContext {
    /// Platform IP stack shared by network consumers.
    pub ip_stack: Stack<'static>,
    /// Factory for constructing HTTP clients over the Platform network and TLS
    /// capabilities.
    pub http_clients: ClientFactory<'static>,
    /// Board-exposed hardware services assigned to hardware-owning Plugins.
    pub hardware_services: barracuda_board_hal::HardwareServices,
}

impl PluginContext {
    /// Creates the unified Plugin construction context.
    #[must_use]
    pub const fn new(ip_stack: Stack<'static>, http_clients: ClientFactory<'static>) -> Self {
        Self {
            ip_stack,
            http_clients,
            hardware_services: barracuda_board_hal::HardwareServices::new(),
        }
    }

    /// Installs the exposed-I/O services produced by the selected Board HAL.
    #[must_use]
    pub fn with_hardware_services(
        mut self,
        hardware_services: barracuda_board_hal::HardwareServices,
    ) -> Self {
        self.hardware_services = hardware_services;
        self
    }
}
