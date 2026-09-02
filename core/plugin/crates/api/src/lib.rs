//! System-owned construction resources shared by Barracuda Plugins.

#![no_std]

extern crate alloc;

use barracuda_board_hal::{BoardHalResources, NoBuiltinCapabilities, NoExposedIo};
pub use barracuda_plugin_macros::plugin;
pub use embassy_net::Stack;
pub use http_client::ClientFactory;

/// Portable capacity profile used by Plugins with bounded runtime queues.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PluginResourceProfile {
    /// Server and desktop capacities.
    #[default]
    Standard,
    /// Memory-bounded capacities for embedded System applications.
    Embedded,
}

/// Portable resource budget for script-runtime Plugins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptRuntimeBudget {
    /// Maximum number of concurrent script executions.
    pub slots: usize,
    /// Fixed heap bytes reserved for each execution slot.
    pub bytes_per_slot: usize,
}

impl ScriptRuntimeBudget {
    /// Creates an explicit script runtime budget.
    #[must_use]
    pub const fn new(slots: usize, bytes_per_slot: usize) -> Self {
        Self {
            slots,
            bytes_per_slot,
        }
    }
}

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
    /// Application policy for script-runtime concurrency and memory.
    pub script_runtime: ScriptRuntimeBudget,
    /// Application policy for other bounded Plugin runtime resources.
    pub resource_profile: PluginResourceProfile,
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
            script_runtime: ScriptRuntimeBudget::new(4, 64 * 1024),
            resource_profile: PluginResourceProfile::Standard,
        }
    }

    /// Replaces the default script-runtime budget.
    #[must_use]
    pub const fn with_script_runtime(mut self, budget: ScriptRuntimeBudget) -> Self {
        self.script_runtime = budget;
        self
    }

    /// Replaces the default bounded Plugin resource profile.
    #[must_use]
    pub const fn with_resource_profile(mut self, profile: PluginResourceProfile) -> Self {
        self.resource_profile = profile;
        self
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
