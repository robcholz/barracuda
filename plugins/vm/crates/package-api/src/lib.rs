//! Registration capability for Lua packages owned outside the VM Plugin.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::cell::{Cell, RefCell};

use barracuda_event_router::RpcClient;
use barracuda_lua::{Lua, Package};

/// Default maximum number of externally registered packages.
pub const DEFAULT_EXTERNAL_PACKAGE_CAPACITY: usize = 16;

type PackageFactory = dyn Fn(RpcClient) -> Box<dyn Package>;

struct Entry {
    id: u32,
    name: String,
    factory: Rc<PackageFactory>,
}

struct RegistryState {
    entries: RefCell<Vec<Entry>>,
    next_id: Cell<u32>,
    capacity: usize,
}

/// VM-published capability used by dependent Plugins to register Lua packages.
#[derive(Clone)]
pub struct VmPackageRegistry(Rc<RegistryState>);

impl VmPackageRegistry {
    /// Creates an empty external-package registry with an explicit capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self(Rc::new(RegistryState {
            entries: RefCell::new(Vec::new()),
            next_id: Cell::new(0),
            capacity,
        }))
    }

    /// Registers a per-execution package factory.
    ///
    /// The returned registration removes the factory when dropped. Consumers
    /// retain it through `PluginRegisterContext::retain` for their lifetime.
    pub fn register<F, P>(
        &self,
        name: impl Into<String>,
        factory: F,
    ) -> Result<VmPackageRegistration, VmPackageRegistryError>
    where
        F: Fn(RpcClient) -> P + 'static,
        P: Package + 'static,
    {
        let name = name.into();
        if name.is_empty() {
            return Err(VmPackageRegistryError::EmptyName);
        }
        let mut entries = self.0.entries.borrow_mut();
        if entries.iter().any(|entry| entry.name == name) {
            return Err(VmPackageRegistryError::AlreadyRegistered);
        }
        if entries.len() >= self.0.capacity {
            return Err(VmPackageRegistryError::CapacityReached);
        }
        let id = self.0.next_id.get();
        self.0.next_id.set(
            id.checked_add(1)
                .ok_or(VmPackageRegistryError::CapacityReached)?,
        );
        entries.push(Entry {
            id,
            name,
            factory: Rc::new(move |client| Box::new(factory(client))),
        });
        Ok(VmPackageRegistration {
            registry: self.clone(),
            id,
        })
    }

    /// Installs fresh instances of every registered external package.
    pub fn install(&self, lua: &mut Lua, client: RpcClient) -> barracuda_lua::Result<()> {
        let factories: Vec<Rc<PackageFactory>> = self
            .0
            .entries
            .borrow()
            .iter()
            .map(|entry| Rc::clone(&entry.factory))
            .collect();
        for factory in factories {
            factory(client.clone()).install(lua)?;
        }
        Ok(())
    }
}

impl Default for VmPackageRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_EXTERNAL_PACKAGE_CAPACITY)
    }
}

/// Scoped external-package registration.
pub struct VmPackageRegistration {
    registry: VmPackageRegistry,
    id: u32,
}

impl Drop for VmPackageRegistration {
    fn drop(&mut self) {
        self.registry
            .0
            .entries
            .borrow_mut()
            .retain(|entry| entry.id != self.id);
    }
}

/// Failure registering an external VM package.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VmPackageRegistryError {
    /// The package name is empty.
    #[error("VM package name cannot be empty")]
    EmptyName,
    /// The package name is already registered.
    #[error("VM package is already registered")]
    AlreadyRegistered,
    /// The external package registry is full.
    #[error("VM package registry reached capacity")]
    CapacityReached,
}
