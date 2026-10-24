//! Registration and installation of Lua packages owned by other Plugins.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use barracuda_lua::{Lua, Package, Result as LuaResult};
use portable_atomic_util::{Arc, Weak};
use spin::Mutex;

/// Lua heap one installed package may keep in a run once its installation's
/// garbage is collected.
///
/// A package installs into a run when the run's script first requires it,
/// and what it keeps there comes out of the script's own share of the heap.
/// Each package proves it stays within this budget and
/// [`LUA_PACKAGE_INSTALL_PEAK_BYTES`].
pub const LUA_PACKAGE_HEAP_BUDGET_BYTES: usize = 12 * 1024;

/// Heap one package's installation may use beyond what it keeps, such as
/// compiling Lua source. Packages install one at a time, so the VM leaves
/// room for one such peak beside the built-ins and the script.
pub const LUA_PACKAGE_INSTALL_PEAK_BYTES: usize = 32 * 1024;

/// Bytes a run's heap must have free in one block before a package installs
/// on first require: what the package may keep and its installation's peak.
pub const LUA_PACKAGE_INSTALL_HEADROOM_BYTES: usize =
    LUA_PACKAGE_HEAP_BUDGET_BYTES + LUA_PACKAGE_INSTALL_PEAK_BYTES;

/// Most registered packages that install with the built-ins at once.
///
/// Such a package is in every run's heap whether the script uses it or not,
/// so the VM counts this many of them, at [`LUA_PACKAGE_HEAP_BUDGET_BYTES`]
/// each, with its built-ins.
pub const MAX_BUILTIN_EXTENSIONS: usize = 4;

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

/// When a run installs a registered package.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackageLoading {
    /// The first time the run's script requires the package's name; the
    /// package registers its library under that name with
    /// [`Lua::register_lib`]. A run whose script never requires it holds
    /// none of it.
    #[default]
    OnRequire,
    /// With the built-ins, before the run's script starts, for a package
    /// that adds functions to the built-in `io` or `os` tables instead of
    /// registering a library of its own. At most
    /// [`MAX_BUILTIN_EXTENSIONS`] are registered at once.
    WithBuiltins,
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

    /// When a run installs the package.
    fn loading(&self) -> PackageLoading {
        PackageLoading::OnRequire
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
    /// or when the package installs with the built-ins and the registry
    /// already holds [`MAX_BUILTIN_EXTENSIONS`] such packages.
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
        if state
            .entries
            .iter()
            .any(|entry| entry.package.name() == name)
        {
            return Err(LuaPackageRegistryError::DuplicateName(name));
        }
        if package.loading() == PackageLoading::WithBuiltins
            && state
                .entries
                .iter()
                .filter(|entry| entry.package.loading() == PackageLoading::WithBuiltins)
                .count()
                >= MAX_BUILTIN_EXTENSIONS
        {
            return Err(LuaPackageRegistryError::TooManyBuiltinExtensions);
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

    /// Lets `lua` require every package registered during Plugin composition.
    ///
    /// # Errors
    ///
    /// Returns the error of a package that installs with the built-ins and
    /// fails to.
    pub fn install(&self, lua: &mut Lua) -> LuaResult<()> {
        self.install_scoped(lua, &LuaPackageScope::All)
    }

    /// Lets `lua` require the registered packages `scope` admits; the others
    /// cannot be required in `lua`.
    ///
    /// The packages that install with the built-ins install now. Each other
    /// package installs the first time a script in `lua` requires it, if it
    /// is still registered then and was registered before this call;
    /// [`LUA_PACKAGE_INSTALL_HEADROOM_BYTES`] must be free first, or that
    /// `require` raises Lua's out-of-memory error.
    ///
    /// # Errors
    ///
    /// Returns the error of a package that installs with the built-ins and
    /// fails to.
    pub fn install_scoped(&self, lua: &mut Lua, scope: &LuaPackageScope) -> LuaResult<()> {
        let (extensions, registered_before) = {
            let state = self.state.lock();
            let extensions = state
                .entries
                .iter()
                .filter(|entry| {
                    entry.package.loading() == PackageLoading::WithBuiltins
                        && scope.admits(entry.package.name())
                })
                .map(|entry| Arc::clone(&entry.package))
                .collect::<Vec<_>>();
            (extensions, state.next_id)
        };
        // The lock is released first: an installation may use the registry.
        for package in extensions {
            package.install(lua)?;
        }

        let registry = Arc::downgrade(&self.state);
        let scope = scope.clone();
        lua.set_package_finder(LUA_PACKAGE_INSTALL_HEADROOM_BYTES, move |name| {
            if !scope.admits(name) {
                return None;
            }
            let registry = registry.upgrade()?;
            let state = registry.lock();
            state
                .entries
                .iter()
                .find(|entry| {
                    entry.id < registered_before
                        && entry.package.name() == name
                        && entry.package.loading() == PackageLoading::OnRequire
                })
                .map(|entry| {
                    Box::new(RequiredPackage(Arc::clone(&entry.package))) as Box<dyn Package>
                })
        });
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

/// A registered package a script has just required, installing outside the
/// registry's lock.
struct RequiredPackage(Arc<dyn LuaPackage>);

impl Package for RequiredPackage {
    fn install(&self, lua: &mut Lua) -> LuaResult<()> {
        self.0.install(lua)
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
    /// Whether a run of this scope may install the package `name`.
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
/// Dropping the guard removes the package from the VM registry: no run can
/// require it any longer, and the callbacks runs already installed are
/// revoked.
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
    /// The registry already holds [`MAX_BUILTIN_EXTENSIONS`] packages that
    /// install with the built-ins.
    #[error("the VM holds at most {MAX_BUILTIN_EXTENSIONS} Lua packages that install with the built-ins")]
    TooManyBuiltinExtensions,
}
