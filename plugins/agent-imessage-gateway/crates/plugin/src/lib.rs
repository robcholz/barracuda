//! Native Agent Tool adapter for the IMessage Gateway capability.

#![no_std]

extern crate alloc;

mod bridge;
mod state;
mod to_agent;
mod to_gateway;

use alloc::{boxed::Box, rc::Rc, string::String};

use barracuda_agent_plugin::{
    AgentToolRegistry,
    tools::{
        Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_imessage_gateway_plugin::{
    GatewayOperationError, GatewaySendMediaRequest, GatewaySendRequest, IMessageGateway,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_workflow_plugin::WorkflowActionRegistry;
use serde::Serialize;

use crate::bridge::ImessageBridge;

/// Plugin registering IMessage Gateway operations with the Agent runtime.
#[barracuda_plugin::macros::plugin]
pub struct AgentIMessageGatewayPlugin;

impl AgentIMessageGatewayPlugin {
    /// Creates the stateless Agent adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self
    }
}

impl Plugin for AgentIMessageGatewayPlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let tools = context.require::<AgentToolRegistry>("agent")?;
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let bridge = embassy_futures::block_on(ImessageBridge::load(context.storage().clone()))
            .map_err(PluginError::registration)?;
        for registration in bridge
            .register_actions(&actions)
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        tools
            .register_group(ToolGroup::new(
                "gateway",
                true,
                [
                    Tool::new(GatewaySendTool {
                        gateway: Rc::clone(&gateway),
                    }),
                    Tool::new(GatewaySendMediaTool { gateway }),
                ],
            ))
            .map_err(PluginError::registration)
    }
}

struct GatewaySendTool {
    gateway: Rc<IMessageGateway>,
}

impl ToolSpec for GatewaySendTool {
    barracuda_agent_plugin::tools::tool_metadata!("gateway_send");
}

impl ToolHandler for GatewaySendTool {
    type Args = GatewaySendRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.gateway.send(request).await) })
    }
}

struct GatewaySendMediaTool {
    gateway: Rc<IMessageGateway>,
}

impl ToolSpec for GatewaySendMediaTool {
    barracuda_agent_plugin::tools::tool_metadata!("gateway_send_media");
}

impl ToolHandler for GatewaySendMediaTool {
    type Args = GatewaySendMediaRequest;

    fn invoke<'a>(&'a self, request: Self::Args) -> ToolFuture<'a> {
        Box::pin(async move { tool_output(self.gateway.send_media(request)) })
    }
}

#[derive(Serialize)]
struct ErrorResponse {
    error: GatewayOperationError,
}

fn tool_output<Response: Serialize>(
    result: Result<Response, GatewayOperationError>,
) -> Result<ToolOutput, ToolInvokeError> {
    let (content, ok) = match result {
        Ok(response) => (serde_json::to_string(&response), true),
        Err(error) => (serde_json::to_string(&ErrorResponse { error }), false),
    };
    let content = content.map_err(|_error| {
        ToolError::InvokeRejected(String::from("failed to encode Gateway Tool response"))
    })?;
    Ok(ToolOutput { content, ok })
}
