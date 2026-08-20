use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use core::future::pending;

use barracuda_agent_runtime::SessionId;
use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};

use crate::agent_to_gateway::{agent_to_gateway_handler, AgentToGateway};
use crate::bind_route::{bind_route_handler, BindRoute};
use crate::gateway_to_agent::{gateway_to_agent_handler, GatewayToAgent};
use barracuda_message_gateway_component::route::GatewayRoute;

#[derive(Clone)]
pub(crate) struct Binding {
    pub(crate) route: GatewayRoute,
    pub(crate) latest_reply_to: Option<String>,
}

#[derive(Default)]
pub(crate) struct AdapterState {
    by_route: BTreeMap<GatewayRoute, SessionId>,
    pub(crate) by_session: BTreeMap<SessionId, Binding>,
    pub(crate) output: BTreeMap<SessionId, String>,
}

/// Shared route correlation used by the three Adapter RPC handlers.
#[derive(Clone, Default)]
pub struct AdapterRegistry(pub(crate) Rc<RefCell<AdapterState>>);

impl AdapterRegistry {
    /// Clears every route binding and partial output buffer.
    pub fn clear(&self) {
        *self.0.borrow_mut() = AdapterState::default();
    }
}

impl AdapterState {
    pub(crate) fn bind(&mut self, route: GatewayRoute, session: SessionId) {
        if let Some(previous_session) = self.by_route.insert(route.clone(), session) {
            self.by_session.remove(&previous_session);
            self.output.remove(&previous_session);
        }
        if let Some(previous) = self.by_session.insert(
            session,
            Binding {
                route: route.clone(),
                latest_reply_to: None,
            },
        ) {
            self.by_route.remove(&previous.route);
        }
    }

    pub(crate) fn session_for_route(&self, route: &GatewayRoute) -> Option<SessionId> {
        self.by_route.get(route).copied()
    }
}

/// Component providing Gateway/Agent conversion and route correlation RPCs.
#[derive(Default)]
pub struct GatewayAgentAdapter {
    state: AdapterRegistry,
}

impl<const M: usize> Component<M> for GatewayAgentAdapter {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<BindRoute, _>(bind_route_handler(self.state.clone()))?;
        context.register_rpc::<GatewayToAgent, _>(gateway_to_agent_handler(self.state.clone()))?;
        context.register_rpc::<AgentToGateway, _>(agent_to_gateway_handler(self.state.clone()))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.state.clear();
        Ok(())
    }
}
