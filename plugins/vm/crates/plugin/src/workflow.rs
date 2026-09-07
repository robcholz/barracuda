use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_vm_component::{
    Vm, VmControlAccepted, VmError, VmInputRequest, VmRunAccepted, VmRunReference, VmRunRequest,
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
    type Response = VmActionResponse<VmRunAccepted>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("vm.run");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let result = self.0.run(request);
        Box::pin(async move { Ok(VmActionResponse::from(result)) })
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
