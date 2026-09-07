use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_workflow_plugin::{
    workflow_action_schema, WorkflowActionFuture, WorkflowActionHandler,
    WorkflowActionRegistration, WorkflowActionRegistry, WorkflowActionRegistryError,
    WorkflowActionSchema,
};
use serde::Serialize;

use crate::{
    GatewayAccepted, GatewayOperationError, GatewaySendMediaRequest, GatewaySendRequest,
    GatewaySendResponse, GatewaySendStreamRequest, IMessageGateway,
};

struct SendAction(Rc<IMessageGateway>);

impl WorkflowActionHandler for SendAction {
    type Request = GatewaySendRequest;
    type Response = GatewayActionResponse<GatewaySendResponse>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("gateway.send");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let gateway = Rc::clone(&self.0);
        Box::pin(async move { Ok(GatewayActionResponse::from(gateway.send(request).await)) })
    }
}

struct SendStreamAction(Rc<IMessageGateway>);

impl WorkflowActionHandler for SendStreamAction {
    type Request = GatewaySendStreamRequest;
    type Response = GatewayActionResponse<GatewayAccepted>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("gateway.send_stream");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let response = self.0.send_stream(request);
        Box::pin(async move { Ok(GatewayActionResponse::from(response)) })
    }
}

struct SendMediaAction(Rc<IMessageGateway>);

impl WorkflowActionHandler for SendMediaAction {
    type Request = GatewaySendMediaRequest;
    type Response = GatewayActionResponse<GatewayAccepted>;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("gateway.send_media");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let response = self.0.send_media(request);
        Box::pin(async move { Ok(GatewayActionResponse::from(response)) })
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum GatewayActionResponse<Response> {
    Success(Response),
    Error { error: GatewayOperationError },
}

impl<Response> From<Result<Response, GatewayOperationError>> for GatewayActionResponse<Response> {
    fn from(result: Result<Response, GatewayOperationError>) -> Self {
        match result {
            Ok(response) => Self::Success(response),
            Err(error) => Self::Error { error },
        }
    }
}

pub(crate) fn register_actions(
    actions: &WorkflowActionRegistry,
    gateway: Rc<IMessageGateway>,
) -> Result<Vec<WorkflowActionRegistration>, WorkflowActionRegistryError> {
    Ok(alloc::vec![
        actions.add_action(SendAction(Rc::clone(&gateway)))?,
        actions.add_action(SendStreamAction(Rc::clone(&gateway)))?,
        actions.add_action(SendMediaAction(gateway))?,
    ])
}
