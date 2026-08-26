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
}

impl PluginContext {
    /// Creates the unified Plugin construction context.
    #[must_use]
    pub const fn new(ip_stack: Stack<'static>, http_clients: ClientFactory<'static>) -> Self {
        Self {
            ip_stack,
            http_clients,
        }
    }
}
