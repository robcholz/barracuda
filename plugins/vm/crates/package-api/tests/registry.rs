#![allow(clippy::expect_used, missing_docs)]

use barracuda_lua::{Lua, Package, Result};
use barracuda_vm_package_api::{
    LuaPackage, LuaPackageRegistry, LuaPackageRegistryError, LuaPackageScope, PackageLoading,
    MAX_BUILTIN_EXTENSIONS,
};
use portable_atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// A package counting its installations that registers a library under its
/// name, or sets a global when it extends the built-ins; when given the
/// registry, it reads it while installing.
struct Counted {
    name: &'static str,
    loading: PackageLoading,
    installs: Arc<AtomicUsize>,
    registry: Option<LuaPackageRegistry>,
}

impl Counted {
    fn new(name: &'static str, installs: &Arc<AtomicUsize>) -> Self {
        Self {
            name,
            loading: PackageLoading::OnRequire,
            installs: Arc::clone(installs),
            registry: None,
        }
    }
}

impl Package for Counted {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        self.installs.fetch_add(1, Ordering::Relaxed);
        let packages = self
            .registry
            .as_ref()
            .map_or(0, |registry| registry.packages().len());
        let packages = i64::try_from(packages).unwrap_or(i64::MAX);
        match self.loading {
            PackageLoading::OnRequire => {
                lua.register_lib(self.name, move |package| package.set("packages", packages))
            }
            PackageLoading::WithBuiltins => lua.set(self.name, true),
        }
    }
}

impl LuaPackage for Counted {
    fn name(&self) -> &'static str {
        self.name
    }

    fn loading(&self) -> PackageLoading {
        self.loading
    }
}

fn requires(lua: &mut Lua, name: &str) -> bool {
    lua.load(&format!("return (pcall(require, '{name}'))"))
        .eval()
        .expect("try to require the package")
}

#[test]
fn a_package_installs_only_when_a_script_requires_it() {
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let _registration = registry
        .register(Counted::new("counted", &installs))
        .expect("register package");

    let mut lua = Lua::new().expect("create Lua");
    registry.install(&mut lua).expect("install registry");
    assert_eq!(installs.load(Ordering::Relaxed), 0);
    let ran: bool = lua.load("return true").eval().expect("run a script");
    assert!(ran);
    assert_eq!(installs.load(Ordering::Relaxed), 0);

    let same: bool = lua
        .load("return rawequal(require('counted'), require('counted'))")
        .eval()
        .expect("require the package twice");
    assert!(same);
    assert_eq!(installs.load(Ordering::Relaxed), 1);
}

#[test]
fn the_registry_holds_many_packages_and_each_can_be_required() {
    const COUNT: usize = 64;
    let names: Vec<&'static str> = (0..COUNT)
        .map(|index| &*format!("p{index:02}").leak())
        .collect();
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let _registrations = names
        .iter()
        .map(|name| {
            registry
                .register(Counted::new(name, &installs))
                .expect("register package")
        })
        .collect::<Vec<_>>();
    assert_eq!(registry.packages().len(), COUNT);

    let mut lua = Lua::new().expect("create Lua");
    registry.install(&mut lua).expect("install registry");
    assert!(requires(&mut lua, "p63"));
    assert_eq!(installs.load(Ordering::Relaxed), 1);
    for name in &names {
        assert!(requires(&mut lua, name), "{name} cannot be required");
    }
    assert_eq!(installs.load(Ordering::Relaxed), COUNT);
}

#[test]
fn a_package_outside_the_scope_is_never_installed() {
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let _secret = registry
        .register(Counted::new("secret", &installs))
        .expect("register secret package");
    let _open = registry
        .register(Counted::new("open", &installs))
        .expect("register open package");

    let mut lua = Lua::new().expect("create Lua");
    registry
        .install_scoped(&mut lua, &LuaPackageScope::Only(vec!["open".into()]))
        .expect("install scoped registry");
    assert!(!requires(&mut lua, "secret"));
    assert_eq!(installs.load(Ordering::Relaxed), 0);
    assert!(requires(&mut lua, "open"));
    assert_eq!(installs.load(Ordering::Relaxed), 1);
}

#[test]
fn a_package_dropped_before_its_first_require_cannot_be_required() {
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let registration = registry
        .register(Counted::new("dropped", &installs))
        .expect("register package");

    let mut running = Lua::new().expect("create Lua");
    registry.install(&mut running).expect("install registry");
    drop(registration);

    assert!(!requires(&mut running, "dropped"));
    let mut next = Lua::new().expect("create Lua");
    registry.install(&mut next).expect("install registry");
    assert!(!requires(&mut next, "dropped"));
    assert_eq!(installs.load(Ordering::Relaxed), 0);
}

#[test]
fn a_package_registered_after_a_state_was_configured_is_not_in_it() {
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let mut earlier = Lua::new().expect("create Lua");
    registry.install(&mut earlier).expect("install registry");
    let _registration = registry
        .register(Counted::new("later", &installs))
        .expect("register package");

    assert!(!requires(&mut earlier, "later"));
    let mut next = Lua::new().expect("create Lua");
    registry.install(&mut next).expect("install registry");
    assert!(requires(&mut next, "later"));
}

#[test]
fn an_installation_may_use_the_registry() {
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let _registration = registry
        .register(Counted {
            registry: Some(registry.clone()),
            ..Counted::new("reader", &installs)
        })
        .expect("register package");

    let mut lua = Lua::new().expect("create Lua");
    registry.install(&mut lua).expect("install registry");
    let packages: i64 = lua
        .load("return require('reader').packages")
        .eval()
        .expect("require the package");
    assert_eq!(packages, 1);
}

#[test]
fn built_in_extensions_install_before_the_script_and_are_bounded() {
    const NAMES: [&str; 5] = ["e0", "e1", "e2", "e3", "e4"];
    let registry = LuaPackageRegistry::new();
    let installs = Arc::new(AtomicUsize::new(0));
    let extension = |name| Counted {
        loading: PackageLoading::WithBuiltins,
        ..Counted::new(name, &installs)
    };
    let _registrations = NAMES[..MAX_BUILTIN_EXTENSIONS]
        .iter()
        .map(|name| {
            registry
                .register(extension(name))
                .expect("register extension")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        registry
            .register(extension(NAMES[MAX_BUILTIN_EXTENSIONS]))
            .err(),
        Some(LuaPackageRegistryError::TooManyBuiltinExtensions)
    );
    let _required = registry
        .register(Counted::new("required", &installs))
        .expect("register a package installed on require");

    let mut lua = Lua::new().expect("create Lua");
    registry.install(&mut lua).expect("install registry");
    assert_eq!(installs.load(Ordering::Relaxed), MAX_BUILTIN_EXTENSIONS);
    let installed: bool = lua
        .load("return e0 and e3")
        .eval()
        .expect("read extensions");
    assert!(installed);
    // As before, an extension registers no library to require.
    assert!(!requires(&mut lua, "e0"));
    assert_eq!(installs.load(Ordering::Relaxed), MAX_BUILTIN_EXTENSIONS);

    let mut scoped = Lua::new().expect("create Lua");
    registry
        .install_scoped(&mut scoped, &LuaPackageScope::Only(vec!["e1".into()]))
        .expect("install scoped registry");
    assert_eq!(installs.load(Ordering::Relaxed), MAX_BUILTIN_EXTENSIONS + 1);
}
