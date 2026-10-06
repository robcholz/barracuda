//! Built-in Lua packages owned and installed by the VM Plugin.

#![no_std]

extern crate alloc;

use barracuda_lua::{Environment, Lua, Result};

/// Message-based input and output for sandboxed Lua states.
pub mod io;
/// The standard `math` library, seeded from the Platform's entropy source.
pub mod math;
/// Empty standard `os` table extended by capability Plugins.
pub mod os;

use io::{Input, Io, Output};
use math::{Math, SeedSource};
use os::Os;

/// Immutable installation plan for the VM-owned Lua packages.
#[derive(Clone)]
pub struct BuiltinPackages {
    seeds: SeedSource,
}

impl BuiltinPackages {
    /// Selects the complete built-in package set, seeding `math.random` from
    /// `seeds`.
    #[must_use]
    pub fn new(seeds: SeedSource) -> Self {
        Self { seeds }
    }

    /// Installs fresh instances of every built-in package into one Lua state.
    ///
    /// Stateful package resources are created per call so concurrent VM
    /// executions never share input or output channels.
    pub fn install(&self, lua: &mut Lua) -> Result<InstalledBuiltinPackages> {
        let (io, input, output) = Io::new();
        Environment::new()
            .with_package(io)
            .with_package(Os)
            .with_package(Math::new(self.seeds.clone()))
            .install(lua)?;
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
