use alloc::{boxed::Box, vec::Vec};

use crate::{Lua, Result};

/// A capability package that can be installed into a sandboxed [Lua] state.
///
/// Package implementations live outside this crate. The Lua wrapper only
/// defines how callers compose and install them.
pub trait Package {
    /// Installs this package into the Lua state.
    fn install(&self, lua: &mut Lua) -> Result<()>;
}

/// A caller-selected collection of packages for one sandboxed [Lua] state.
#[derive(Default)]
pub struct Environment {
    packages: Vec<Box<dyn Package>>,
}

impl Environment {
    /// Creates an empty execution environment.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            packages: Vec::new(),
        }
    }

    /// Adds one package, preserving installation order.
    #[must_use]
    pub fn with_package(mut self, package: impl Package + 'static) -> Self {
        self.packages.push(Box::new(package));
        self
    }

    /// Installs every selected package into an existing sandbox.
    pub fn install(self, lua: &mut Lua) -> Result<()> {
        for package in self.packages {
            package.install(lua)?;
        }
        Ok(())
    }
}
