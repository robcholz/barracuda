#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;

use barracuda_vm_package_api::LuaPackageRegistry;
use barracuda_vm_runtime::{
    Vm, VmExecutionError, VmInputRequest, VmRunInfo, VmRunOutcome, VmRunProgress, VmRunRequest,
    VmRunState, VmRunUpdate,
};
use embassy_executor::{Executor, Spawner};

#[embassy_executor::task]
async fn exercise_vm_completion(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
    let result = async {
        let vm = Vm::new(LuaPackageRegistry::new()).map_err(|error| error.to_string())?;
        vm.start(spawner).map_err(|error| error.to_string())?;
        if !vm.list().runs.is_empty() {
            return Err("new VM runtime listed active runs".into());
        }

        let mut run = vm
            .run(VmRunRequest {
                source:
                    "local io = require('io'); local name = io.input(); io.print('hello', name)"
                        .into(),
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

#[test]
fn run_completion_is_awaited_and_contains_the_terminal_result() {
    let (completed, result) = sync_channel(1);
    std::thread::spawn(move || {
        let executor = Box::leak(Box::new(Executor::new()));
        executor.run(|spawner| {
            spawner
                .spawn(exercise_vm_completion(spawner, completed))
                .expect("spawn VM completion test");
        });
    });

    result
        .recv_timeout(Duration::from_secs(10))
        .expect("VM completion test timed out")
        .expect("VM completion test failed");
}
