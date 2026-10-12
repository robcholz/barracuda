#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_lua::{Lua, Package};
use barracuda_vm_package_api::{LuaPackage, LuaPackageRegistry};
use barracuda_vm_runtime::{
    SeedSource, Vm, VmExecutionError, VmInputRequest, VmRunInfo, VmRunOutcome, VmRunProgress,
    VmRunRequest, VmRunState, VmRunUpdate,
};
use embassy_executor::{Executor, Spawner};
use embassy_time::Timer;

async fn wait_for_no_active_runs(vm: &Vm) -> Result<(), String> {
    for _attempt in 0..50 {
        if vm.list().runs.is_empty() {
            return Ok(());
        }
        Timer::after_millis(10).await;
    }
    Err(format!(
        "VM runs remained active after handle drop: {:?}",
        vm.list()
    ))
}

#[embassy_executor::task]
async fn exercise_vm_completion(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let vm = Vm::new(LuaPackageRegistry::new(), SeedSource::unavailable()).map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;
        if !vm.list().runs.is_empty() {
            return Err("new VM runtime listed active runs".into());
        }

        let mut run = vm
            .run(VmRunRequest {
                source: "local name = io.read(); print('hello', name)".into(),
            })
            .map_err(|error| error.to_string())?;
        let run_id = run.run_id();
        let update = run.next_update().await.map_err(|error| error.to_string())?;
        if update != VmRunUpdate::Progress(VmRunProgress::InputRequired { run_id }) {
            return Err(format!("unexpected VM progress: {update:?}"));
        }
        if vm.list().runs
            != [VmRunInfo {
                run_id,
                state: VmRunState::InputRequired,
            }]
        {
            return Err(format!("unexpected active VM list: {:?}", vm.list()));
        }
        vm.input(VmInputRequest {
            run_id,
            input: Some("barracuda".into()),
            eof: None,
        })
        .map_err(|error| error.to_string())?;
        let completion = match run.next_update().await.map_err(|error| error.to_string())? {
            VmRunUpdate::Completed(completion) => completion,
            update => return Err(format!("expected completion, got: {update:?}")),
        };
        if completion.run_id != run_id
            || completion.outcome != VmRunOutcome::Success
            || completion.output != ["hello\tbarracuda"]
            || completion.error.is_some()
            || completion.diagnostic.is_some()
        {
            return Err(format!("unexpected successful completion: {completion:?}"));
        }
        if !vm.list().runs.is_empty() {
            return Err("completed VM remained in active list".into());
        }

        let completion = vm
            .run(VmRunRequest {
                source: "io.write('partial'); io.write(' line\\nlast')".into(),
            })
            .map_err(|error| error.to_string())?
            .await
            .map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Success
            || completion.output != ["partial line", "last"]
        {
            return Err(format!("unexpected io.write completion: {completion:?}"));
        }

        let completion = vm
            .run(VmRunRequest {
                source: "for _ = 1, 65 do io.write(string.rep('x', 1024)) end".into(),
            })
            .map_err(|error| error.to_string())?
            .await
            .map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Error
            || completion.error != Some(VmExecutionError::LuaRuntime)
            || completion.diagnostic.as_deref() != Some("Lua output limit exceeded")
            || completion.output.len() != 1
            || completion.output.first().map(String::len) != Some(64 * 1024)
        {
            return Err(format!(
                "unexpected output-limit completion: outcome={:?}, error={:?}, diagnostic={:?}, output lengths={:?}",
                completion.outcome,
                completion.error,
                completion.diagnostic,
                completion
                    .output
                    .iter()
                    .map(String::len)
                    .collect::<Vec<_>>()
            ));
        }

        let completion = vm
            .run(VmRunRequest {
                source: "for _ = 1, 1025 do io.write('\\n') end".into(),
            })
            .map_err(|error| error.to_string())?
            .await
            .map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Error
            || completion.error != Some(VmExecutionError::LuaRuntime)
            || completion.diagnostic.as_deref() != Some("Lua output limit exceeded")
            || completion.output.len() != 1024
        {
            return Err(format!(
                "unexpected output-line-limit completion: outcome={:?}, error={:?}, diagnostic={:?}, output lines={}",
                completion.outcome,
                completion.error,
                completion.diagnostic,
                completion.output.len()
            ));
        }

        let run = vm
            .run(VmRunRequest {
                source: "while true do end".into(),
            })
            .map_err(|error| error.to_string())?;
        let run_id = run.run_id();
        if vm.list().runs
            != [VmRunInfo {
                run_id,
                state: VmRunState::Running,
            }]
        {
            return Err(format!("unexpected running VM list: {:?}", vm.list()));
        }
        vm.cancel(barracuda_vm_runtime::VmRunReference { run_id })
            .map_err(|error| error.to_string())?;
        let completion = run.await.map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Cancelled {
            return Err(format!("unexpected cancelled completion: {completion:?}"));
        }
        if !vm.list().runs.is_empty() {
            return Err("cancelled VM remained in active list".into());
        }

        let completion = vm
            .run(VmRunRequest {
                source: "this is not valid lua".into(),
            })
            .map_err(|error| error.to_string())?
            .await
            .map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Error
            || completion.error != Some(VmExecutionError::LuaLoad)
            || completion.diagnostic.is_none()
        {
            return Err(format!("unexpected failed completion: {completion:?}"));
        }

        Ok(())
    }
    .await;
    let _ignored = completed.send(result);
}

/// A package whose only function never completes, like a server loop.
struct Parked;

impl Package for Parked {
    fn install(&self, lua: &mut Lua) -> barracuda_lua::Result<()> {
        lua.register_lib("parked", |package| {
            package.register_async("wait", |(): ()| async {
                core::future::pending::<Option<barracuda_lua::Result<()>>>().await
            })
        })
    }
}

impl LuaPackage for Parked {
    fn name(&self) -> &'static str {
        "parked"
    }
}

#[embassy_executor::task]
async fn exercise_vm_parked_cancel(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let packages = LuaPackageRegistry::new();
        let _registration = packages
            .register(Parked)
            .map_err(|error| error.to_string())?;
        let vm = Vm::new(packages, SeedSource::unavailable()).map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;
        let run = vm
            .run(VmRunRequest {
                source: "require('parked').wait()".into(),
            })
            .map_err(|error| error.to_string())?;
        let run_id = run.run_id();
        // Let the execution park inside the native call before cancelling it.
        Timer::after_millis(50).await;
        vm.cancel(barracuda_vm_runtime::VmRunReference { run_id })
            .map_err(|error| error.to_string())?;
        let completion = run.await.map_err(|error| error.to_string())?;
        if completion.outcome != VmRunOutcome::Cancelled {
            return Err(format!("unexpected parked completion: {completion:?}"));
        }
        wait_for_no_active_runs(&vm).await
    }
    .await;
    let _ignored = completed.send(result);
}

#[embassy_executor::task]
async fn exercise_vm_handle_drop(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let vm = Vm::new(LuaPackageRegistry::new(), SeedSource::unavailable())
            .map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;
        let mut run = vm
            .run(VmRunRequest {
                source: "local value = io.read(); print(value)".into(),
            })
            .map_err(|error| error.to_string())?;
        let run_id = run.run_id();
        let update = run.next_update().await.map_err(|error| error.to_string())?;
        if update != VmRunUpdate::Progress(VmRunProgress::InputRequired { run_id }) {
            return Err(format!("unexpected VM progress: {update:?}"));
        }

        drop(run);
        wait_for_no_active_runs(&vm).await?;
        Ok(())
    }
    .await;
    let _ignored = completed.send(result);
}

#[test]
fn run_completion_is_awaited_and_contains_the_terminal_result() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner.spawn(
                exercise_vm_completion(spawner, completed).expect("spawn VM completion test"),
            );
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM completion test timed out")
        .expect("VM completion test failed");
}

#[test]
fn dropping_run_handle_cancels_the_execution() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner.spawn(
                exercise_vm_handle_drop(spawner, completed).expect("spawn VM handle-drop test"),
            );
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM handle-drop test timed out")
        .expect("VM handle-drop test failed");
}

#[test]
fn cancelling_a_run_parked_in_an_async_call_completes_it() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner.spawn(
                exercise_vm_parked_cancel(spawner, completed).expect("spawn VM parked test"),
            );
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM parked-cancel test timed out")
        .expect("VM parked-cancel test failed");
}
