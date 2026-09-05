use alloc::{boxed::Box, rc::Rc};
use core::future::pending;

use barracuda_agent_runtime::{AgentRuntime, RuntimeService};
use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
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

impl<const M: usize> Component<M> for AgentComponent {
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
            if let Some(service) = self.service.take() {
                let runtime = async move {
                    service.await;
                    Ok(())
                };
                return future::or(
                    runtime,
                    session::emit_session_events::<M>(self.sessions.clone(), context.rpc().clone()),
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
