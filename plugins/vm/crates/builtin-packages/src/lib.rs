//! Built-in Lua packages owned and installed by the VM Plugin.

#![no_std]

extern crate alloc;

use barracuda_lua::{Environment, Lua, Result};

/// Message-based input and output for sandboxed Lua states.
pub mod io;

use io::{Input, Io, Output};

/// Immutable installation plan for the VM-owned Lua packages.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinPackages;

impl BuiltinPackages {
    /// Selects the complete built-in package set.
    #[must_use]
    pub const fn all() -> Self {
        Self
    }

    /// Installs fresh instances of every built-in package into one Lua state.
    ///
    /// Stateful package resources are created per call so concurrent VM
    /// executions never share input or output channels.
    pub fn install(self, lua: &mut Lua) -> Result<InstalledBuiltinPackages> {
        let (io, input, output) = Io::new();
        Environment::new().with_package(io).install(lua)?;
        Ok(InstalledBuiltinPackages { input, output })
    }
}

/// Per-execution resources created while installing built-in packages.
pub struct InstalledBuiltinPackages {
    input: Input,
    output: Output,
}

impl InstalledBuiltinPackages {
    /// Separates the VM protocol's input and output sides.
    #[must_use]
    pub fn into_io(self) -> (Input, Output) {
        (self.input, self.output)
    }
}
