//! Native Agent Tool adapter for the HTTP capability.

#![no_std]

extern crate alloc;

use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_http_plugin::{Http, HttpError, HttpRequest};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use serde::Serialize;

/// Plugin registering outbound HTTP with the Agent runtime.
#[barracuda_plugin::macros::plugin]
pub struct AgentHttpPlugin;

impl AgentHttpPlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentHttpPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let http = context.require::<Http>("http")?;
        tools
            .register_group(ToolGroup::new(
                "http",
                false,
                [Tool::new(HttpRequestTool { http })],
            ))
            .map_err(PluginError::registration)
    }
}

struct HttpRequestTool {
    http: Rc<Http>,
}

impl ToolSpec for HttpRequestTool {
    barracuda_agent_plugin::tools::tool_metadata!("http_request");
}

impl ToolHandler for HttpRequestTool {
    type Args = HttpRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move {
            match self.http.request(request).await {
                Ok(response) => encode(&response, true),
                Err(error) => encode(&ErrorResponse { error }, false),
            }
        })
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: HttpError,
}

fn encode(response: &impl Serialize, ok: bool) -> Result<ToolOutput, ToolInvokeError> {
    let content = serde_json::to_string(response).map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode HTTP Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}
