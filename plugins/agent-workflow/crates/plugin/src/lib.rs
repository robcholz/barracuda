//! Native Agent Tools for discovering and controlling Workflow.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};

use barracuda_agent_message::json;
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
    WorkflowActionDescriptor, WorkflowActionRegistry, WorkflowControlRejection, WorkflowId,
    WorkflowService, WorkflowServiceError, WorkflowValue,
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
            Ok(ToolOutput {
                content: render_actions(&self.actions.descriptors()),
                ok: true,
            })
        })
    }
}

/// Writes `{"actions":[{"address", "request_schema", "response_schema"}]}`
/// straight from each Action's static schema text. Parsing every schema into
/// a tree first used about ten times the size of the result.
fn render_actions(descriptors: &[WorkflowActionDescriptor]) -> String {
    json::encode_string(|sink| {
        sink.put(br#"{"actions":["#);
        for (index, descriptor) in descriptors.iter().enumerate() {
            if index > 0 {
                sink.put(b",");
            }
            sink.put(br#"{"address":"#);
            json::write_str(sink, descriptor.address().as_str());
            sink.put(br#","request_schema":"#);
            json::write_compact(sink, descriptor.request_schema().as_str());
            sink.put(br#","response_schema":"#);
            json::write_compact(sink, descriptor.response_schema().as_str());
            sink.put(b"}");
        }
        sink.put(b"]}");
    })
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;

    use barracuda_workflow_plugin::{
        WorkflowActionFuture, WorkflowActionHandler, WorkflowActionSchema,
        workflow_action_schema_inline,
    };
    use serde_json::{Value, json};

    use super::*;

    struct Echo;

    impl WorkflowActionHandler for Echo {
        type Request = Value;
        type Response = Value;

        const SCHEMA: WorkflowActionSchema = workflow_action_schema_inline!(
            "test.echo",
            r#"{ "type": "object",
                 "properties": { "text": { "type": "string", "description": "a \"quoted\" line" } } }"#,
            r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#
        );

        fn invoke(&self, request: Value) -> WorkflowActionFuture<'_, Value> {
            Box::pin(async move { Ok(request) })
        }
    }

    #[test]
    fn catalog_matches_the_parsed_schema_rendering() {
        let registry = WorkflowActionRegistry::new();
        let _registration = registry.add_action(Echo).expect("register echo");
        let descriptors = registry.descriptors();

        let rendered: Value =
            serde_json::from_str(&render_actions(&descriptors)).expect("catalog is JSON");
        let expected = json!({"actions": descriptors.iter().map(|descriptor| json!({
            "address": descriptor.address().as_str(),
            "request_schema": serde_json::from_str::<Value>(descriptor.request_schema().as_str())
                .expect("request schema"),
            "response_schema": serde_json::from_str::<Value>(descriptor.response_schema().as_str())
                .expect("response schema"),
        })).collect::<Vec<_>>()});
        assert_eq!(rendered, expected);
    }
}
