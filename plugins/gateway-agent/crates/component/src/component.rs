use alloc::boxed::Box;
use alloc::rc::Rc;

use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, RegisterContext, RunContext,
    UnregisterContext, WorkflowClient, WorkflowControlError, WorkflowControlRejection,
};

use crate::respond::{gateway_agent_respond_handler, GatewayAgentRespond, RespondState};

/// Workflow that routes each inbound gateway message to the bridge.
const BRIDGE_WORKFLOW: &str = r#"{
    "id": "gateway-agent-bridge",
    "match": { "event": "gateway.message.received" },
    "steps": [
        { "call": "gateway_agent.respond" },
        { "call": "gateway.send_stream" }
    ]
}"#;

/// Bridge Component binding each Gateway conversation to one Agent session.
///
/// Sessions are opened lazily by `gateway_agent.respond`; the Component itself
/// owns only the mapper registry and the two-step Workflow.
pub struct GatewayAgentBridge {
    state: Rc<RespondState>,
}

impl GatewayAgentBridge {
    /// Builds an empty stateful bridge.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Rc::new(RespondState::default()),
        }
    }
}

impl Default for GatewayAgentBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl<const M: usize> Component<M> for GatewayAgentBridge {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<GatewayAgentRespond, _>(
            "system",
            gateway_agent_respond_handler(Rc::clone(&self.state)),
        )
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            match WorkflowClient::<M>::new(context.rpc().clone())
                .load(BRIDGE_WORKFLOW)
                .await
            {
                Ok(())
                | Err(WorkflowControlError::Rejected(WorkflowControlRejection::DuplicateId)) => {}
                Err(error) => return Err(ComponentError::lifecycle(error)),
            }
            let () = core::future::pending().await;
            Ok(())
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.clear();
        Ok(())
    }
}
