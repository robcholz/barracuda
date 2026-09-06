use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::future::pending;

use barracuda_agent_runtime::{
    tools::{Tool, ToolGroup},
    AgentRuntime, RuntimeService,
};
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, RegisterContext, RpcClient,
    RunContext, UnregisterContext,
};
use futures_lite::future;

use crate::delete_session::{delete_session_handler, DeleteSession};
use crate::list_sessions::{list_sessions_handler, ListSessions};
use crate::new_session::{new_session_handler, NewSession};
use crate::open_session::{open_session_handler, OpenSession};
use crate::session;

/// Event Router Component exposing the Agent runtime's JSON API and Events.
pub struct AgentComponent {
    runtime: Rc<AgentRuntime>,
    service: Option<RuntimeService>,
    sessions: session::SessionRegistry,
}

impl AgentComponent {
    /// Creates a Component around the two values returned by `AgentRuntime::new`.
    #[must_use]
    pub fn new(runtime: AgentRuntime, service: RuntimeService) -> Self {
        Self::from_shared(Rc::new(runtime), service)
    }

    /// Creates a Component sharing an Agent runtime with another adapter.
    #[must_use]
    pub fn from_shared(runtime: Rc<AgentRuntime>, service: RuntimeService) -> Self {
        Self {
            runtime,
            service: Some(service),
            sessions: session::SessionRegistry::default(),
        }
    }
}

fn agent_rpc_tools(client: &RpcClient) -> ComponentResult<Vec<Tool>> {
    client
        .rpcs_by_visibility("agent")?
        .into_iter()
        .map(|address| {
            let info = client.json_method_info(&address)?;
            Ok(Tool::from_json_rpc(client.clone(), info))
        })
        .collect()
}

impl<const M: usize> Component<M> for AgentComponent {
    fn name(&self) -> &'static str {
        "agent-runtime"
    }

    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_json::<NewSession, _>(
            "system",
            new_session_handler(Rc::clone(&self.runtime)),
        )?;
        context.register_json::<ListSessions, _>(
            "system",
            list_sessions_handler(Rc::clone(&self.runtime)),
        )?;
        context.register_json::<OpenSession, _>(
            "system",
            open_session_handler(Rc::clone(&self.runtime), self.sessions.clone()),
        )?;
        context.register_json::<DeleteSession, _>(
            "system",
            delete_session_handler(Rc::clone(&self.runtime)),
        )?;

        context.register_json::<session::append::Append, _>(
            "system",
            session::append::append_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::respond::Respond, _>(
            "system",
            session::respond::respond_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::set_reasoning_effort::SetReasoningEffort, _>(
            "system",
            session::set_reasoning_effort::set_reasoning_effort_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::set_permission_level::SetPermissionLevel, _>(
            "system",
            session::set_permission_level::set_permission_level_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::interrupt::Interrupt, _>(
            "system",
            session::interrupt::interrupt_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::cancel::Cancel, _>(
            "system",
            session::cancel::cancel_handler(self.sessions.clone()),
        )?;
        context.register_json::<session::close::Close, _>(
            "system",
            session::close::close_handler(self.sessions.clone()),
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            let rpc = context.rpc().clone();
            let tools = agent_rpc_tools(&rpc)?;
            if !tools.is_empty() {
                self.runtime
                    .register_tool_group(ToolGroup::new("rpc", true, tools))
                    .map_err(ComponentError::lifecycle)?;
            }
            if let Some(service) = self.service.take() {
                let runtime = async move {
                    service.await;
                    Ok(())
                };
                return future::or(
                    runtime,
                    session::emit_session_events::<M>(self.sessions.clone(), rpc),
                )
                .await;
            }
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.sessions.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use barracuda_event_router::{
        ComponentError, JsonRef, JsonRpcSchema, JsonSchema, JsonWriter, RpcError, RpcFrame,
        RpcLaneStorage, RpcMethod, RpcRegistry, Unary,
    };

    use super::agent_rpc_tools;

    struct AgentJson;
    struct GlobalJson;
    struct SystemJson;

    macro_rules! json_method {
        ($method:ty, $address:literal) => {
            impl JsonRpcSchema for $method {
                const ADDRESS: &'static str = $address;
                const REQUEST_SCHEMA: JsonSchema = barracuda_event_router::json_schema_inline!(
                    r#"{"type":"object","properties":{},"additionalProperties":false}"#
                );
                const RESPONSE_SCHEMA: JsonSchema = barracuda_event_router::json_schema_inline!(
                    r#"{"type":"object","properties":{},"additionalProperties":false}"#
                );
                const MAX_REQUEST_BYTES: usize = 2;
                const MAX_RESPONSE_BYTES: usize = 2;
            }
        };
    }

    json_method!(AgentJson, "demo.agent");
    json_method!(GlobalJson, "demo.global");
    json_method!(SystemJson, "demo.system");

    #[test]
    fn collects_agent_and_global_json_rpcs_but_not_system_rpcs(
    ) -> Result<(), Box<dyn core::error::Error>> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 4>::new()));
        let registry = RpcRegistry::new(lanes);
        registry.register_json::<AgentJson, _>("agent", echo)?;
        registry.register_json::<GlobalJson, _>("*", echo)?;
        registry.register_json::<SystemJson, _>("system", echo)?;

        let tools = agent_rpc_tools(&registry.client())?;
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name())
                .collect::<alloc::vec::Vec<_>>(),
            [AgentJson::ADDRESS, GlobalJson::ADDRESS]
        );
        Ok(())
    }

    async fn echo(
        _context: barracuda_event_router::RpcContext,
        request: JsonRef,
        response: JsonWriter,
    ) -> barracuda_event_router::RpcResult<()> {
        response.write(request.as_str()?).await
    }

    struct NativeAgent;

    impl RpcMethod for NativeAgent {
        const ADDRESS: &'static str = "demo.native";
        type Request = [u8; 1];
        type Response = [u8; 1];
        type Error = [u8; 1];
        type Input = Unary;
        type Output = Unary;
    }

    #[test]
    fn rejects_native_rpc_exposed_to_agent() -> Result<(), Box<dyn core::error::Error>> {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<1, 64, 1>::new()));
        let registry = RpcRegistry::new(lanes);
        registry.register::<NativeAgent, _>(
            "agent",
            |_context, request: RpcFrame<[u8; 1]>| async move { Ok(Ok(*request.view()?)) },
        )?;

        assert!(matches!(
            agent_rpc_tools(&registry.client()),
            Err(ComponentError::Rpc(RpcError::NotJsonEndpoint(address)))
                if address.as_ref() == NativeAgent::ADDRESS
        ));
        Ok(())
    }
}
