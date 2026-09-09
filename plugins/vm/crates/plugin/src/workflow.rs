use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_vm_runtime::{
    Vm, VmControlAccepted, VmError, VmInputRequest, VmRunCompletion, VmRunReference, VmRunRequest,
};
use barracuda_workflow_plugin::{
    workflow_action_schema, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistration, WorkflowActionRegistry, WorkflowActionRegistryError,
    WorkflowActionSchema,
};
use serde::Serialize;

struct RunAction(Rc<Vm>);

impl WorkflowActionHandler for RunAction {
    type Request = VmRunRequest;
    type Response = VmActionResponse<VmRunCompletion>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("vm.run");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self.0.run(request);
        Box::pin(async move {
            let result = match result {
                Ok(run) => run.await,
                Err(error) => Err(error),
            };
            Ok(VmActionResponse::from(result))
        })
    }
}

struct InputAction(Rc<Vm>);

impl WorkflowActionHandler for InputAction {
    type Request = VmInputRequest;
    type Response = VmActionResponse<VmControlAccepted>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("vm.input");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self.0.input(request);
        Box::pin(async move { Ok(VmActionResponse::from(result)) })
    }
}

struct CancelAction(Rc<Vm>);

impl WorkflowActionHandler for CancelAction {
    type Request = VmRunReference;
    type Response = VmActionResponse<VmControlAccepted>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("vm.cancel");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self.0.cancel(request);
        Box::pin(async move { Ok(VmActionResponse::from(result)) })
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum VmActionResponse<Response> {
    Success(Response),
    Error { error: VmError },
}

impl<Response> From<Result<Response, VmError>> for VmActionResponse<Response> {
    fn from(result: Result<Response, VmError>) -> Self {
        match result {
            Ok(response) => Self::Success(response),
            Err(error) => Self::Error { error },
        }
    }
}

pub(crate) fn register_actions(
    actions: &WorkflowActionRegistry,
    vm: Rc<Vm>,
) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
    Ok(alloc::vec![
        actions.add_action(RunAction(Rc::clone(&vm)))?,
        actions.add_action(InputAction(Rc::clone(&vm)))?,
        actions.add_action(CancelAction(vm))?,
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use std::sync::mpsc::{sync_channel, SyncSender};
    use std::time::Duration;

    use alloc::{
        boxed::Box,
        format,
        rc::Rc,
        string::{String, ToString},
    };

    use barracuda_vm_package_api::LuaPackageRegistry;
    use barracuda_vm_runtime::{Vm, VmRunRequest};
    use barracuda_workflow_plugin::WorkflowActionHandler as _;
    use embassy_executor::{Executor, Spawner};

    use super::RunAction;

    #[embassy_executor::task]
    async fn exercise_awaited_action(spawner: Spawner, completed: SyncSender<Result<(), String>>) {
        let result = async {
            let vm =
                Rc::new(Vm::new(LuaPackageRegistry::new()).map_err(|error| error.to_string())?);
            vm.start(spawner).map_err(|error| error.to_string())?;
            let response = RunAction(vm)
                .invoke(VmRunRequest {
                    source: "print('workflow result')".into(),
                })
                .await
                .map_err(|error| error.to_string())?;
            let response = serde_json::to_value(response).map_err(|error| error.to_string())?;
            if response.get("outcome").and_then(serde_json::Value::as_str) != Some("success")
                || response.get("output") != Some(&serde_json::json!(["workflow result"]))
            {
                return Err(format!("invalid awaited Workflow response: {response}"));
            }
            Ok(())
        }
        .await;
        let _ignored = completed.send(result);
    }

    #[test]
    fn vm_run_action_awaits_the_terminal_result() {
        let (completed, result) = sync_channel(1);
        std::thread::spawn(move || {
            let executor = Box::leak(Box::new(Executor::new()));
            executor.run(|spawner| {
                spawner.spawn(
                    exercise_awaited_action(spawner, completed)
                        .expect("spawn awaited Workflow Action test"),
                );
            });
        });

        result
            .recv_timeout(Duration::from_secs(10))
            .expect("awaited Workflow Action test timed out")
            .expect("awaited Workflow Action test failed");
    }
}
