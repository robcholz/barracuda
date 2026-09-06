//! Public facade for Barracuda Plugin construction and lifecycle.

#![no_std]

/// System-owned resources available while constructing a Plugin.
pub mod api {
    pub use barracuda_plugin_api::*;
}

/// Plugin lifecycle, capability, and scoped-storage management.
pub mod manager {
    pub use barracuda_plugin_manager::*;
}

/// Compile-time Plugin declaration macros.
pub mod macros {
    pub use barracuda_plugin_macros::*;
}

/// Host-side parsing for `plugin.toml` declarations.
#[cfg(feature = "manifest")]
pub mod manifest {
    pub use barracuda_plugin_manifest::*;
}
