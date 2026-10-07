#![allow(clippy::expect_used, missing_docs)]

use barracuda_lua::{Lua, Package, Result};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry, LuaPackageRegistryError};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;

struct Marker {
    name: &'static str,
    value: i64,
}

impl Package for Marker {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let value = self.value;
        lua.register_lib(self.name, move |package| package.set("value", value))
    }
}

impl LuaPackage for Marker {
    fn name(&self) -> &'static str {
        self.name
    }
}

#[test]
fn registry_installs_registered_packages() {
    let registry = LuaPackageRegistry::new();
    let _registration = registry
        .register(Marker {
            name: "marker",
            value: 42,
        })
        .expect("register package");

    let mut lua = Lua::new().expect("create Lua");
    registry
        .install(&mut lua)
        .expect("install registered packages");

    let value: i64 = lua
        .load("return require('marker').value")
        .eval()
        .expect("read marker value");
    assert_eq!(value, 42);
}

#[test]
fn rejects_duplicate_package_names() {
    let registry = LuaPackageRegistry::new();
    let _registration = registry
        .register(Marker {
            name: "marker",
            value: 1,
        })
        .expect("register first package");

    let error = registry
        .register(Marker {
            name: "marker",
            value: 2,
        })
        .err()
        .expect("duplicate must fail");
    assert_eq!(error, LuaPackageRegistryError::DuplicateName("marker"));
}

#[test]
fn dropping_registration_removes_the_package() {
    let registry = LuaPackageRegistry::new();
    let registration = registry
        .register(Marker {
            name: "marker",
            value: 7,
        })
        .expect("register package");
    drop(registration);

    assert!(registry.is_empty());
    let mut lua = Lua::new().expect("create Lua");
    registry.install(&mut lua).expect("install empty registry");
    let loaded: bool = lua
        .load("local ok = pcall(require, 'marker'); return ok")
        .eval()
        .expect("check marker absence");
    assert!(!loaded);
}

#[test]
fn rejects_empty_package_names() {
    let registry = LuaPackageRegistry::new();
    let error = registry
        .register(Marker { name: "", value: 0 })
        .err()
        .expect("empty name must fail");
    assert_eq!(error, LuaPackageRegistryError::EmptyName);
}

struct RevocableMarker {
    revoked: Arc<AtomicBool>,
}

impl Package for RevocableMarker {
    fn install(&self, _lua: &mut Lua) -> Result<()> {
        Ok(())
    }
}

impl LuaPackage for RevocableMarker {
    fn name(&self) -> &'static str {
        "revocable"
    }

    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
}

#[test]
fn dropping_registration_revokes_the_registered_package() {
    let registry = LuaPackageRegistry::new();
    let revoked = Arc::new(AtomicBool::new(false));
    let registration = registry
        .register(RevocableMarker {
            revoked: Arc::clone(&revoked),
        })
        .expect("register package");

    drop(registration);

    assert!(revoked.load(Ordering::Acquire));
}
