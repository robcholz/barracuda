use alloc::{boxed::Box, rc::Rc};

use barracuda_workflow_plugin::{
    WorkflowActionFuture, WorkflowActionHandler, WorkflowActionRegistration,
    WorkflowActionRegistry, WorkflowActionRegistryError, WorkflowActionSchema,
    workflow_action_schema,
};
use serde::Serialize;

use crate::{Http, HttpError, HttpRequest, HttpResponse};

struct HttpRequestAction {
    http: Rc<Http>,
}

impl WorkflowActionHandler for HttpRequestAction {
    type Request = HttpRequest;
    type Response = HttpActionResponse;

    const SCHEMA: WorkflowActionSchema = workflow_action_schema!("http.request");

    fn invoke(&self, request: Self::Request) -> WorkflowActionFuture<'_, Self::Response> {
        let http = Rc::clone(&self.http);
        Box::pin(async move {
            Ok(match http.request(request).await {
                Ok(response) => HttpActionResponse::Success(response),
                Err(error) => HttpActionResponse::Error(ErrorResponse { error }),
            })
        })
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum HttpActionResponse {
    Success(HttpResponse),
    Error(ErrorResponse),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: HttpError,
}

pub(crate) fn register_action(
    actions: &WorkflowActionRegistry,
    http: Rc<Http>,
) -> Result<WorkflowActionRegistration, WorkflowActionRegistryError> {
    actions.add_action(HttpRequestAction { http })
}
