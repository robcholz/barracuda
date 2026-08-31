//! Registration and installation of Lua packages owned by other Plugins.

#![no_std]

extern crate alloc;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use barracuda_lua::{Lua, Package, Result as LuaResult};
use spin::Mutex;

/// A named Lua package registered by a Plugin during System composition.
pub trait LuaPackage: Package + Send + Sync {
    /// Returns the stable name accepted by Lua's `require` function.
    fn name(&self) -> &'static str;
}

/// Registry capability published by the VM Plugin.
///
/// Other Plugins register packages during their registration phase. The VM
/// uses this same registry when configuring every new Lua state.
#[derive(Clone, Default)]
pub struct LuaPackageRegistry {
    state: Arc<Mutex<RegistryState>>,
}

impl LuaPackageRegistry {
    /// Creates an empty package registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one package until the returned guard is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the package name is empty or already registered.
    pub fn register(
        &self,
        package: impl LuaPackage + 'static,
    ) -> Result<LuaPackageRegistration, LuaPackageRegistryError> {
        let package: Arc<dyn LuaPackage> = Arc::new(package);
        let name = package.name();
        if name.is_empty() {
            return Err(LuaPackageRegistryError::EmptyName);
        }

        let mut state = self.state.lock();
        if state
            .entries
            .iter()
            .any(|entry| entry.package.name() == name)
        {
            return Err(LuaPackageRegistryError::DuplicateName(name));
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.entries.push(RegistryEntry { id, package });

        Ok(LuaPackageRegistration {
            state: Arc::downgrade(&self.state),
            id,
        })
    }

    /// Installs every package registered during Plugin composition.
    pub fn install(&self, lua: &mut Lua) -> LuaResult<()> {
        for entry in &self.state.lock().entries {
            entry.package.install(lua)?;
        }
        Ok(())
    }

    /// Returns the number of registered packages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().entries.len()
    }

    /// Returns whether no packages are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.lock().entries.is_empty()
    }
}

#[derive(Default)]
struct RegistryState {
    next_id: u64,
    entries: Vec<RegistryEntry>,
}

struct RegistryEntry {
    id: u64,
    package: Arc<dyn LuaPackage>,
}

/// Retained registration for one Lua package.
///
/// Dropping the guard removes the package from the VM registry.
pub struct LuaPackageRegistration {
    state: Weak<Mutex<RegistryState>>,
    id: u64,
}

impl Drop for LuaPackageRegistration {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state.lock().entries.retain(|entry| entry.id != self.id);
    }
}

/// Failure while registering a VM package.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LuaPackageRegistryError {
    /// Package names must be nonempty.
    #[error("Lua package name must not be empty")]
    EmptyName,
    /// Only one Plugin may own a given Lua package name.
    #[error("Lua package `{0}` is already registered")]
    DuplicateName(&'static str),
}
