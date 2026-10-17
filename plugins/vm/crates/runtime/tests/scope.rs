#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_lua::{Lua, Package};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry, LuaPackageScope};
use barracuda_vm_runtime::{SeedSource, Vm, VmRunRequest};
use embassy_executor::{Executor, Spawner};

/// A package that only some runs may require.
struct Secret;

impl Package for Secret {
    fn install(&self, lua: &mut Lua) -> barracuda_lua::Result<()> {
        lua.register_lib("secret", |_package| Ok(()))
    }
}

impl LuaPackage for Secret {
    fn name(&self) -> &'static str {
        "secret"
    }
}

#[embassy_executor::task]
async fn exercise_vm_package_scope(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let packages = LuaPackageRegistry::new();
        let _registration = packages
            .register(Secret)
            .map_err(|error| error.to_string())?;
        let vm = Vm::new(packages, SeedSource::unavailable()).map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;
        let mut lines = Vec::new();
        for scope in [
            LuaPackageScope::All,
            LuaPackageScope::Only(Vec::new()),
            LuaPackageScope::Only(vec!["secret".into()]),
        ] {
            let completion = vm
                .run_scoped(
                    VmRunRequest {
                        source: "print((pcall(require, 'secret')))".into(),
                    },
                    scope,
                )
                .map_err(|error| error.to_string())?
                .await
                .map_err(|error| error.to_string())?;
            lines.push(completion.output.join("\n"));
        }
        if lines == ["true", "false", "true"] {
            Ok(())
        } else {
            Err(format!("unexpected scoped requires: {lines:?}"))
        }
    }
    .await;
    let _ignored = completed.send(result);
}

#[test]
fn a_scoped_run_requires_only_the_packages_it_admits() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(exercise_vm_package_scope(spawner, completed).expect("spawn VM scope test"));
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM scope test timed out")
        .expect("VM scope test failed");
}
