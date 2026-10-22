//! Registration and installation of Lua packages owned by other Plugins.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use barracuda_lua::{Lua, Package, Result as LuaResult};
use portable_atomic_util::{Arc, Weak};
use spin::Mutex;

mod devices;

pub use devices::{BoardDevices, LentDevice};

/// Most Lua packages the registry holds at once.
pub const MAX_LUA_PACKAGES: usize = 32;

/// Lua heap one installed package may keep in a run once its installation's
/// garbage is collected.
///
/// The VM sizes each run's heap so that the built-in packages,
/// [`MAX_LUA_PACKAGES`] packages at this budget each, one installation's
/// transient peak, and the script all fit; each package proves it stays
/// within both budgets.
pub const LUA_PACKAGE_HEAP_BUDGET_BYTES: usize = 12 * 1024;

/// Heap one package's installation may use beyond what it keeps, such as
/// compiling Lua source. Packages install one after another, so only one
/// such peak is ever live.
pub const LUA_PACKAGE_INSTALL_PEAK_BYTES: usize = 32 * 1024;

/// What a package promises the code that requires it, for any consumer of
/// the registry that decides which code gets which packages. The VM itself
/// grants every registered package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageInterface {
    /// Gives code nothing it could misuse, such as reading the time: any code
    /// may have it without being granted it.
    Ambient {
        /// Counts the additions to its functions; it starts at 1.
        version: u32,
    },
    /// Works the same wherever it is registered: no function is removed,
    /// renamed, or given other arguments or results.
    Stable {
        /// Counts the additions to its functions; it starts at 1.
        version: u32,
        /// What code may do with it, in a few plain words for a person.
        purpose: &'static str,
    },
    /// Reaches a Board's pins, buses, or a peripheral directly, so what it
    /// does depends on the Board's wiring.
    BoardSpecific {
        /// What code may do with it, in a few plain words for a person.
        purpose: &'static str,
    },
}

/// A named Lua package registered by a Plugin during System composition.
pub trait LuaPackage: Package + Send + Sync {
    /// Returns the stable name accepted by Lua's `require` function.
    fn name(&self) -> &'static str;

    /// What the package promises; `None` when it promises nothing, such as a
    /// package whose interface may change with any update, or a device's
    /// package on a Board without the device.
    fn interface(&self) -> Option<PackageInterface> {
        None
    }

    /// Revokes resources captured by callbacks already installed into a Lua state.
    ///
    /// Stateless packages may keep the default no-op implementation. Packages
    /// that expose owned resources must make installed callbacks reject future
    /// calls after revocation.
    fn revoke(&self) {}
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
    /// Returns an error when the package name is empty or already registered,
    /// or when the registry already holds [`MAX_LUA_PACKAGES`] packages.
    pub fn register(
        &self,
        package: impl LuaPackage + 'static,
    ) -> Result<LuaPackageRegistration, LuaPackageRegistryError> {
        let package: Arc<dyn LuaPackage> = Arc::from(Box::new(package) as Box<dyn LuaPackage>);
        let name = package.name();
        if name.is_empty() {
            return Err(LuaPackageRegistryError::EmptyName);
        }

        let mut state = self.state.lock();
        if state.entries.len() >= MAX_LUA_PACKAGES {
            return Err(LuaPackageRegistryError::Full);
        }
        if state
            .entries
            .iter()
            .any(|entry| entry.package.name() == name)
        {
            return Err(LuaPackageRegistryError::DuplicateName(name));
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.entries.push(RegistryEntry {
            id,
            package: Arc::clone(&package),
        });

        Ok(LuaPackageRegistration {
            state: Arc::downgrade(&self.state),
            id,
            package,
        })
    }

    /// Installs every package registered during Plugin composition.
    pub fn install(&self, lua: &mut Lua) -> LuaResult<()> {
        self.install_scoped(lua, &LuaPackageScope::All)
    }

    /// Installs the registered packages `scope` admits; the others cannot be
    /// required in `lua`.
    pub fn install_scoped(&self, lua: &mut Lua, scope: &LuaPackageScope) -> LuaResult<()> {
        for entry in &self.state.lock().entries {
            if scope.admits(entry.package.name()) {
                entry.package.install(lua)?;
            }
        }
        Ok(())
    }

    /// Every registered package's name and what it promises.
    #[must_use]
    pub fn packages(&self) -> Vec<(&'static str, Option<PackageInterface>)> {
        self.state
            .lock()
            .entries
            .iter()
            .map(|entry| (entry.package.name(), entry.package.interface()))
            .collect()
    }

    /// Returns whether no packages are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.lock().entries.is_empty()
    }
}

/// The registered packages one Lua run may `require`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LuaPackageScope {
    /// Every registered package.
    #[default]
    All,
    /// Only the registered packages with these names.
    Only(Vec<String>),
}

impl LuaPackageScope {
    /// Whether the package `name` is installed in a run of this scope.
    #[must_use]
    pub fn admits(&self, name: &str) -> bool {
        match self {
            LuaPackageScope::All => true,
            LuaPackageScope::Only(names) => names.iter().any(|admitted| admitted == name),
        }
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
    package: Arc<dyn LuaPackage>,
}

impl Drop for LuaPackageRegistration {
    fn drop(&mut self) {
        self.package.revoke();
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
    /// The registry already holds [`MAX_LUA_PACKAGES`] packages.
    #[error("the VM holds at most {MAX_LUA_PACKAGES} Lua packages")]
    Full,
}
