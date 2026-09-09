//! Native Agent Tools for discovering and controlling Workflow.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        EmptyArgs, Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError,
        ToolOutput, ToolSpec,
    },
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_workflow_plugin::{
    WorkflowActionRegistry, WorkflowControlRejection, WorkflowId, WorkflowService,
    WorkflowServiceError, WorkflowValue,
};
use serde::{Deserialize, Serialize};

/// Plugin registering Workflow discovery and control with the Agent runtime.
#[barracuda_plugin::macros::plugin]
pub struct AgentWorkflowPlugin;

impl AgentWorkflowPlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentWorkflowPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let service = context.require::<WorkflowService>("workflow")?;
        tools
            .register_group(ToolGroup::new(
                "workflow",
                false,
                [
                    Tool::new(ActionsTool { actions }),
                    Tool::new(ListTool {
                        service: Rc::clone(&service),
                    }),
                    Tool::new(LoadTool {
                        service: Rc::clone(&service),
                    }),
                    Tool::new(UnloadTool { service }),
                ],
            ))
            .map_err(PluginError::registration)
    }
}

struct ActionsTool {
    actions: Rc<WorkflowActionRegistry>,
}

impl ToolSpec for ActionsTool {
    barracuda_agent_plugin::tools::tool_metadata!("workflow_actions");
}

impl ToolHandler for ActionsTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let actions = self
                .actions
                .descriptors()
                .into_iter()
                .map(|descriptor| {
                    Ok(ActionDescription {
                        address: String::from(descriptor.address().as_str()),
                        request_schema: serde_json::from_str(descriptor.request_schema().as_str())?,
                        response_schema: serde_json::from_str(
                            descriptor.response_schema().as_str(),
                        )?,
                    })
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()
                .map_err(|_error| {
                    ToolError::InvokeRejected(String::from(
                        "failed to encode Workflow Action catalog",
                    ))
                })?;
            encode(&ActionsResponse { actions }, true)
        })
    }
}

#[derive(Serialize)]
struct ActionDescription {
    address: String,
    request_schema: WorkflowValue,
    response_schema: WorkflowValue,
}

#[derive(Serialize)]
struct ActionsResponse {
    actions: Vec<ActionDescription>,
}

struct ListTool {
    service: Rc<WorkflowService>,
}

impl ToolSpec for ListTool {
    barracuda_agent_plugin::tools::tool_metadata!("workflow_list");
}

impl ToolHandler for ListTool {
    type Args = EmptyArgs;

    fn invoke<'a>(&'a self, _args: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let workflows = self
                .service
                .definitions()
                .into_iter()
                .map(|definition| String::from(definition.id().as_str()))
                .collect();
            encode(&ListResponse { workflows }, true)
        })
    }
}

#[derive(Serialize)]
struct ListResponse {
    workflows: Vec<String>,
}

struct LoadTool {
    service: Rc<WorkflowService>,
}

impl ToolSpec for LoadTool {
    barracuda_agent_plugin::tools::tool_metadata!("workflow_load");
}

impl ToolHandler for LoadTool {
    type Args = WorkflowValue;

    fn invoke<'a>(&'a self, definition: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let definition = serde_json::to_string(&definition).map_err(|_error| {
                ToolError::InvokeRejected(String::from("failed to encode Workflow definition"))
            })?;
            match self.service.load(&definition).await {
                Ok(()) => encode(&EmptyResponse {}, true),
                Err(error) => encode(&ErrorResponse::from(error), false),
            }
        })
    }
}

struct UnloadTool {
    service: Rc<WorkflowService>,
}

impl ToolSpec for UnloadTool {
    barracuda_agent_plugin::tools::tool_metadata!("workflow_unload");
}

impl ToolHandler for UnloadTool {
    type Args = UnloadRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            let id = match WorkflowId::try_from(request.id) {
                Ok(id) => id,
                Err(_error) => {
                    return encode(
                        &ErrorResponse {
                            error: "invalid_workflow_id",
                        },
                        false,
                    );
                }
            };
            match self.service.unload(&id).await {
                Ok(()) => encode(&EmptyResponse {}, true),
                Err(error) => encode(&ErrorResponse::from(error), false),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnloadRequest {
    id: String,
}

#[derive(Serialize)]
struct EmptyResponse {}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

impl From<WorkflowServiceError> for ErrorResponse {
    fn from(error: WorkflowServiceError) -> Self {
        let error = match error {
            WorkflowServiceError::Rejected(rejection)
            | WorkflowServiceError::InvalidCatalog(rejection) => rejection_code(rejection),
            WorkflowServiceError::Filesystem(_) => "persistence",
            _ => "workflow_service",
        };
        Self { error }
    }
}

const fn rejection_code(rejection: WorkflowControlRejection) -> &'static str {
    match rejection {
        WorkflowControlRejection::InvalidJson => "invalid_json",
        WorkflowControlRejection::InvalidWorkflowId => "invalid_workflow_id",
        WorkflowControlRejection::InvalidRule => "invalid_rule",
        WorkflowControlRejection::InvalidActionAddress => "invalid_action_address",
        WorkflowControlRejection::EmptySteps => "empty_steps",
        WorkflowControlRejection::DuplicateId => "duplicate_id",
        WorkflowControlRejection::NotFound => "not_found",
        WorkflowControlRejection::Persistence => "persistence",
        WorkflowControlRejection::InvalidArguments => "invalid_arguments",
        WorkflowControlRejection::UnknownAction => "unknown_action",
        WorkflowControlRejection::InvalidLink => "invalid_link",
        WorkflowControlRejection::InvalidTopic => "invalid_topic",
        WorkflowControlRejection::InvalidControlFlow => "invalid_control_flow",
        _ => "workflow_control",
    }
}

fn encode(response: &impl Serialize, ok: bool) -> Result<ToolOutput, ToolInvokeError> {
    let content = serde_json::to_string(response).map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode Workflow Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}
