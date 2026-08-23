use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use barracuda_agent_component::dto::SessionIdDto;
use barracuda_event_router::{
    Component, ComponentError, ComponentFuture, ComponentResult, RegisterContext, RunContext,
    UnregisterContext, WorkflowClient,
};
use barracuda_message_gateway_component::route::GatewayRoute;

use crate::handle::{bridge_handle_handler, BridgeHandle};
use crate::outbound::{create_session, pump_session_events};

/// Workflow that routes each inbound gateway message to the bridge.
const BRIDGE_WORKFLOW: &str = r#"{
    "id": "gateway-agent-bridge",
    "match": { "event": "gateway.message.received" },
    "steps": [ { "call": "bridge.handle" } ]
}"#;

/// Correlation shared by the inbound handler and the outbound pump.
pub(crate) struct BridgeState {
    /// Gateway route replies are delivered to.
    pub(crate) route: GatewayRoute,
    /// Agent session backing the bound conversation, created by the pump.
    pub(crate) session: Cell<Option<SessionIdDto>>,
    /// Provider message id of the most recent inbound message, sent as the
    /// `reply_to` of the next primary reply.
    pub(crate) latest_reply_to: RefCell<Option<String>>,
    /// Set once the outbound pump has created and opened the Agent session, so
    /// the inbound handler only appends after the session lease exists.
    pub(crate) opened: Cell<bool>,
}

/// Bridge Component binding one gateway route to one Agent session.
///
/// The Component creates its own Agent session when it starts and delivers the
/// Agent's replies to `route`. Keeping session creation inside the Component
/// avoids any ordering coupling with the host: the Agent runtime service only
/// runs once its Component is polled by the router.
pub struct GatewayAgentBridge {
    state: Rc<BridgeState>,
}

impl GatewayAgentBridge {
    /// Builds a bridge whose replies are delivered to `route`.
    #[must_use]
    pub fn new(route: GatewayRoute) -> Self {
        Self {
            state: Rc::new(BridgeState {
                route,
                session: Cell::new(None),
                latest_reply_to: RefCell::new(None),
                opened: Cell::new(false),
            }),
        }
    }
}

impl<const M: usize> Component<M> for GatewayAgentBridge {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<BridgeHandle, _>(bridge_handle_handler(Rc::clone(&self.state)))
    }

    fn run<'a>(&'a mut self, context: RunContext<M>) -> ComponentFuture<'a> {
        let state = Rc::clone(&self.state);
        Box::pin(async move {
            let client = context.rpc().clone();
            WorkflowClient::<M>::new(client.clone())
                .load(BRIDGE_WORKFLOW)
                .await
                .map_err(ComponentError::lifecycle)?;
            let session = create_session(&client)
                .await
                .map_err(ComponentError::lifecycle)?;
            state.session.set(Some(session));
            pump_session_events(client, state, session)
                .await
                .map_err(ComponentError::lifecycle)
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.opened.set(false);
        *self.state.latest_reply_to.borrow_mut() = None;
        Ok(())
    }
}
