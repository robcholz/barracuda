//! Native Agent Tool adapter for the IMessage Gateway capability.

#![no_std]

extern crate alloc;

mod bridge;
mod sessions;
mod state;
mod to_agent;
mod to_gateway;

use alloc::{boxed::Box, format, rc::Rc, string::String, vec::Vec};

use barracuda_agent_plugin::{
    AgentRuntime, AgentToolRegistry, SessionDeleteError, SessionId, SessionRenameError,
    tools::{
        Tool, ToolError, ToolFuture, ToolGroup, ToolHandler, ToolInvokeError, ToolOutput, ToolSpec,
    },
};
use barracuda_imessage_gateway_plugin::{
    GatewayOperationError, GatewaySendMediaRequest, GatewaySendRequest, IMessageGateway,
    SendSessionsRequest,
};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{Plugin, PluginError, PluginRegisterContext, PluginResult};
use barracuda_time_plugin::UtcClock;
use barracuda_workflow_plugin::WorkflowActionRegistry;
use serde::Serialize;

use crate::bridge::ImessageBridge;
use crate::sessions::{LocalFuture, RenameFailure, SessionMeta, SessionReplies, SessionStore};

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
        let runtime = context.require::<AgentRuntime>("agent")?;
        let gateway = context.require::<IMessageGateway>("imessage-gateway")?;
        let clock = context.require::<UtcClock>("time")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let bridge = embassy_futures::block_on(ImessageBridge::load(context.storage().clone()))
            .map_err(PluginError::registration)?;
        let replies = Rc::new(GatewaySessionReplies {
            gateway: Rc::clone(&gateway),
            clock,
        });
        for registration in bridge
            .register_actions(&actions, Rc::new(AgentSessions(runtime)), replies)
            .map_err(PluginError::registration)?
        {
            context.retain(registration);
        }
        tools
            .register_group(ToolGroup::new(
                "gateway",
                false,
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

/// The Agent's sessions, as session commands list, rename and delete them.
struct AgentSessions(Rc<AgentRuntime>);

impl SessionStore for AgentSessions {
    fn describe(&self) -> LocalFuture<'_, Vec<SessionMeta>> {
        Box::pin(async move {
            self.0
                .describe_sessions()
                .await
                .into_iter()
                .map(|info| SessionMeta {
                    session: format!("{}", info.session),
                    title: info.title,
                    updated_at: info.updated_at,
                })
                .collect()
        })
    }

    fn rename<'a>(
        &'a self,
        session: &'a str,
        title: &'a str,
    ) -> LocalFuture<'a, Result<(), RenameFailure>> {
        Box::pin(async move {
            let session =
                SessionId::from_wire(session).map_err(|_error| RenameFailure::NotFound)?;
            self.0
                .rename_session(session, title)
                .await
                .map_err(|error| match error {
                    SessionRenameError::InvalidTitle => RenameFailure::InvalidTitle,
                    SessionRenameError::SessionNotFound(_) => RenameFailure::NotFound,
                    SessionRenameError::WorkerStopped => RenameFailure::Failed,
                })
        })
    }

    fn delete<'a>(&'a self, session: &'a str) -> LocalFuture<'a, Result<(), ()>> {
        Box::pin(async move {
            let Ok(session) = SessionId::from_wire(session) else {
                return Ok(());
            };
            match self.0.delete_session(session).await {
                Ok(()) | Err(SessionDeleteError::SessionNotFound(_)) => Ok(()),
                Err(error) => {
                    log::warn!("IMessage Bridge failed to delete `{session}`: {error}");
                    Err(())
                }
            }
        })
    }
}

/// Answers session commands through the Gateway, with the device's clock.
struct GatewaySessionReplies {
    gateway: Rc<IMessageGateway>,
    clock: Rc<UtcClock>,
}

impl SessionReplies for GatewaySessionReplies {
    fn send(&self, request: SendSessionsRequest) -> LocalFuture<'_, ()> {
        Box::pin(async move {
            if let Err(error) = self.gateway.send_sessions(request).await {
                log::warn!("IMessage Bridge could not answer a session command: {error}");
            }
        })
    }

    fn now(&self) -> Option<u64> {
        self.clock.now().ok().map(u64::from)
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
