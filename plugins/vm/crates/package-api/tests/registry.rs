#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_event_router::{RpcLaneStorage, RpcRegistry};
use barracuda_lua::{Lua, Package, Result};
use barracuda_vm_package_api::{VmPackageRegistry, VmPackageRegistryError};

struct Marker;

impl Package for Marker {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        lua.register_lib("external", |package| package.set("installed", true))
    }
}

fn rpc_client() -> barracuda_event_router::RpcClient {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 1>::new()));
    RpcRegistry::new(lanes).client()
}

#[test]
fn registration_installs_a_fresh_external_package() -> Result<()> {
    let registry = VmPackageRegistry::new(1);
    let registration = registry
        .register("external", |_client| Marker)
        .expect("register package");
    let mut lua = Lua::new()?;

    registry.install(&mut lua, rpc_client())?;

    assert!(
        lua.load("return require('external').installed")
            .eval::<bool>()?
    );
    drop(registration);
    Ok(())
}

#[test]
fn registration_is_unique_bounded_and_scoped() {
    let registry = VmPackageRegistry::new(1);
    let registration = registry
        .register("external", |_client| Marker)
        .expect("register package");
    assert!(matches!(
        registry.register("external", |_client| Marker),
        Err(VmPackageRegistryError::AlreadyRegistered)
    ));
    assert!(matches!(
        registry.register("another", |_client| Marker),
        Err(VmPackageRegistryError::CapacityReached)
    ));

    drop(registration);
    assert!(registry.register("external", |_client| Marker).is_ok());
}
