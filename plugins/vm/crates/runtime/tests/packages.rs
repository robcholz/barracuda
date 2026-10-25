#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_lua::{Lua, Package};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_runtime::{
    SeedSource, Vm, VmInputRequest, VmRunCompletion, VmRunOutcome, VmRunProgress, VmRunRequest,
    VmRunUpdate,
};
use embassy_executor::{Executor, Spawner};

/// A package whose installation runs a chunk far longer than the VM's
/// instruction-hook interval.
struct Busy(&'static str);

impl Package for Busy {
    fn install(&self, lua: &mut Lua) -> barracuda_lua::Result<()> {
        lua.register_lib(self.0, |package| package.set("ready", true))?;
        lua.load(&format!(
            "local n = 0; for i = 1, 30000 do n = n + i end; require('{}').sum = n",
            self.0
        ))
        .exec()
    }
}

impl LuaPackage for Busy {
    fn name(&self) -> &'static str {
        self.0
    }
}

async fn run(vm: &Vm, source: &str) -> Result<VmRunCompletion, String> {
    vm.run(VmRunRequest {
        source: source.into(),
    })
    .map_err(|error| error.to_string())?
    .await
    .map_err(|error| error.to_string())
}

fn expect_output(completion: &VmRunCompletion, output: &[&str]) -> Result<(), String> {
    if completion.outcome == VmRunOutcome::Success && completion.output == output {
        Ok(())
    } else {
        Err(format!("unexpected completion: {completion:?}"))
    }
}

#[embassy_executor::task]
async fn exercise_lazy_packages(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        const COUNT: usize = 40;
        let packages = LuaPackageRegistry::new();
        let _registrations = (0..COUNT)
            .map(|index| {
                let name: &'static str = format!("p{index:02}").leak();
                packages.register(Busy(name))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let revocable = packages
            .register(Busy("revocable"))
            .map_err(|error| error.to_string())?;
        let vm = Vm::new(packages, SeedSource::unavailable()).map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;

        // The script yields to the executor before, while it is requiring,
        // and after; the installation itself runs to its end.
        let completion = run(
            &vm,
            "local n = 0; for i = 1, 30000 do n = n + i end; \
             local busy = require('p07'); \
             for i = 1, 30000 do n = n + i end; \
             print(busy.ready, busy.sum, n, rawequal(busy, require('p07')))",
        )
        .await?;
        expect_output(&completion, &["true\t450015000\t900030000\ttrue"])?;

        // Every one of more packages than the registry once held can be
        // required in one run.
        let completion = run(
            &vm,
            &format!(
                "local sum = 0; for i = 0, {} do \
                 sum = sum + require(string.format('p%02d', i)).sum end; print(sum)",
                COUNT - 1
            ),
        )
        .await?;
        let sum = (COUNT * 450_015_000).to_string();
        expect_output(&completion, &[&sum])?;

        // A run that has not required a package yet cannot once its
        // registration is dropped, and neither can later runs.
        let mut waiting = vm
            .run(VmRunRequest {
                source: "io.read(); print((pcall(require, 'revocable')))".into(),
            })
            .map_err(|error| error.to_string())?;
        let run_id = waiting.run_id();
        let update = waiting
            .next_update()
            .await
            .map_err(|error| error.to_string())?;
        if update != VmRunUpdate::Progress(VmRunProgress::InputRequired { run_id }) {
            return Err(format!("unexpected VM progress: {update:?}"));
        }
        drop(revocable);
        vm.input(VmInputRequest {
            run_id,
            input: Some("go".into()),
            eof: None,
        })
        .map_err(|error| error.to_string())?;
        let completion = waiting.await.map_err(|error| error.to_string())?;
        expect_output(&completion, &["false"])?;
        let completion = run(&vm, "print((pcall(require, 'revocable')))").await?;
        expect_output(&completion, &["false"])?;

        // A script that has filled its heap gets Lua's memory error from a
        // require, and the package once it has freed memory.
        let completion = run(
            &vm,
            "local held, ok = {}, true; \
             while ok do ok = pcall(function() held[#held + 1] = string.rep('x', 4096) .. #held end) end; \
             local full, error = pcall(require, 'p01'); \
             held = nil; \
             print(full, error, require('p01').ready)",
        )
        .await?;
        expect_output(&completion, &["false\tnot enough memory\ttrue"])
    }
    .await;
    let _ignored = completed.send(result);
}

#[test]
fn runs_install_the_packages_their_scripts_require() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(exercise_lazy_packages(spawner, completed).expect("spawn VM package test"));
        });
    });

    result
        .recv_timeout(Duration::from_secs(60))
        .expect("VM package test timed out")
        .expect("VM package test failed");
}
