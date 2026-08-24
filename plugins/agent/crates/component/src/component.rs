use alloc::{boxed::Box, rc::Rc};
use core::future::pending;

use barracuda_agent_runtime::{AgentRuntime, RuntimeService};
use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use barracuda_fs::FileSystem;
use barracuda_net::{Dns, TcpConnect};

use crate::delete_session::{delete_session_handler, DeleteSession};
use crate::list_sessions::{list_sessions_handler, ListSessions};
use crate::new_session::{new_session_handler, NewSession};
use crate::open_session::{open_session_handler, OpenSession};
use crate::session;

/// Event Router Component exposing the existing Agent runtime API.
pub struct AgentComponent<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    runtime: Rc<AgentRuntime<Filesystem, Http>>,
    service: Option<RuntimeService<Filesystem, Http>>,
    sessions: session::SessionRegistry,
}

impl<Filesystem, Http> AgentComponent<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    /// Creates a Component around the two values returned by `AgentRuntime::new`.
    #[must_use]
    pub fn new(
        runtime: AgentRuntime<Filesystem, Http>,
        service: RuntimeService<Filesystem, Http>,
    ) -> Self {
        Self::from_shared(Rc::new(runtime), service)
    }

    /// Creates a Component sharing an Agent runtime with another adapter.
    #[must_use]
    pub fn from_shared(
        runtime: Rc<AgentRuntime<Filesystem, Http>>,
        service: RuntimeService<Filesystem, Http>,
    ) -> Self {
        Self {
            runtime,
            service: Some(service),
            sessions: session::SessionRegistry::default(),
        }
    }
}

impl<Filesystem, Http, const M: usize> Component<M> for AgentComponent<Filesystem, Http>
where
    Filesystem: FileSystem + 'static,
    Http: TcpConnect + Dns + 'static,
{
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<NewSession, _>(new_session_handler(Rc::clone(&self.runtime)))?;
        context.register_rpc::<ListSessions, _>(list_sessions_handler(Rc::clone(&self.runtime)))?;
        context.register_rpc::<OpenSession, _>(open_session_handler(
            Rc::clone(&self.runtime),
            self.sessions.clone(),
        ))?;
        context
            .register_rpc::<DeleteSession, _>(delete_session_handler(Rc::clone(&self.runtime)))?;

        context.register_rpc::<session::append::Append, _>(session::append::append_handler(
            self.sessions.clone(),
        ))?;
        context.register_rpc::<session::respond::Respond, _>(session::respond::respond_handler(
            self.sessions.clone(),
        ))?;
        context.register_rpc::<session::set_reasoning_effort::SetReasoningEffort, _>(
            session::set_reasoning_effort::set_reasoning_effort_handler(self.sessions.clone()),
        )?;
        context.register_rpc::<session::set_permission_level::SetPermissionLevel, _>(
            session::set_permission_level::set_permission_level_handler(self.sessions.clone()),
        )?;
        context.register_rpc::<session::interrupt::Interrupt, _>(
            session::interrupt::interrupt_handler(self.sessions.clone()),
        )?;
        context.register_rpc::<session::cancel::Cancel, _>(session::cancel::cancel_handler(
            self.sessions.clone(),
        ))?;
        context.register_rpc::<session::close::Close, _>(session::close::close_handler(
            self.sessions.clone(),
        ))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(async move {
            if let Some(service) = self.service.take() {
                service.await;
            }
            pending().await
        })
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        self.sessions.clear();
        Ok(())
    }
}
