//! Ownership and lifecycle for every Session in one runtime.

use core::task::{Context, Poll};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use async_channel::Sender;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::{DurableState, PersistenceError, SharedPersistence};
use claw_tool::ToolRegistry;

use crate::agent::{AgentCreateError, AgentId, AgentManager, AgentManagerError};
use crate::config::SharedApiManager;
use crate::scheduler::AgentRunSchedulerHandle;

use super::actor::{SessionActor, SessionActorExit, SessionActorStatus};
use super::approval::{LlmApprovalResolver, SharedApprovalResolver};
use super::control::{SessionCommand, SessionControl, SessionControlError};
use super::persistence::{session_instance, SESSION_MANAGER_STATE_NAME, SESSION_STATE_NAME};
use super::state::{
    ensure_next_agent, ensure_next_session, next_session, AgentIdAllocatorHandle,
    SessionManagerState, SessionPersistentState,
};
use super::{SessionEvent, SessionStream};

pub(super) type SharedAgentManager<Filesystem, Http, Timer> =
    Rc<AgentManager<Filesystem, Http, Timer>>;

crate::define_prefixed_id!(SessionId, "session-", "session");

/// Whether a session survives a runtime restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPersistence {
    /// Persist session state and write the root transcript to storage.
    Persistent,
    /// Keep session state and transcript in memory for this process only.
    Ephemeral,
}

/// Failure opening a session event stream.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OpenSessionError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("session is already open: {0}")]
    AlreadyOpen(SessionId),
    #[error("agent runtime is not running")]
    WorkerStopped,
}

/// Failure creating a session through the session manager.
#[derive(Debug, thiserror::Error)]
pub enum SessionCreateError {
    #[error("agent runtime is not running")]
    WorkerStopped,
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
}

struct LiveActor<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    commands: Sender<SessionCommand>,
    actor: SessionActor<Filesystem, Http, Timer>,
    span: tracing::Span,
}

struct SessionEntry<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    persistence: SessionPersistence,
    state: DurableState<SessionPersistentState>,
    actor: Option<LiveActor<Filesystem, Http, Timer>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionManagerInitError {
    #[error(transparent)]
    AgentManager(#[from] AgentManagerError),
    #[error("failed to reconcile persisted agents: {0}")]
    AgentReconciliation(#[from] AgentCreateError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    InvalidSessionId(#[from] claw_utils::IdParseError),
    #[error("persisted session state is missing: {0}")]
    MissingState(SessionId),
}

/// Owns the complete Session aggregate lifecycle.
///
/// Session-owned metadata stays in `SessionPersistentState`; Agent records and
/// transcripts remain canonical in `AgentManager`. A live `SessionActor`
/// coordinates both without exposing either store to the worker loop.
pub(crate) struct SessionManager<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    persistence: SharedPersistence<Filesystem>,
    state: DurableState<SessionManagerState>,
    agent_manager: SharedAgentManager<Filesystem, Http, Timer>,
    approval_resolver: SharedApprovalResolver<Http, Timer>,
    scheduler: AgentRunSchedulerHandle<Http, Timer>,
    sessions: BTreeMap<SessionId, SessionEntry<Filesystem, Http, Timer>>,
    actor_poll_queue: VecDeque<SessionId>,
}

impl<Filesystem, Http, Timer> SessionManager<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tools: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
        scheduler: AgentRunSchedulerHandle<Http, Timer>,
    ) -> Result<Self, SessionManagerInitError> {
        let state = {
            let entry = persistence.singleton::<SessionManagerState>(SESSION_MANAGER_STATE_NAME)?;
            let state = DurableState::new(entry.load()?.unwrap_or_default());
            entry.register(&state)?;
            state
        };
        let agent_manager = Rc::new(AgentManager::new(
            tools,
            Arc::clone(&persistence),
            persistence_dir,
            skill_roots,
            Arc::clone(&api_manager),
        )?);
        let approval_resolver: SharedApprovalResolver<Http, Timer> =
            Rc::new(LlmApprovalResolver::<Http, Timer>::new(api_manager));
        let states = persistence.collection::<SessionPersistentState>(SESSION_STATE_NAME)?;
        let mut sessions: BTreeMap<SessionId, SessionEntry<Filesystem, Http, Timer>> =
            BTreeMap::new();
        for instance in states.list()? {
            let session = SessionId::from_wire(instance.as_str())?;
            let persisted = states
                .load(&instance)?
                .ok_or(SessionManagerInitError::MissingState(session))?;
            let state = DurableState::new(persisted);
            states.register(&instance, &state)?;
            sessions.insert(
                session,
                SessionEntry {
                    persistence: SessionPersistence::Persistent,
                    state,
                    actor: None,
                },
            );
        }
        let discovered_next = sessions
            .last_key_value()
            .map(|(session, _)| session.0.saturating_add(1))
            .unwrap_or(1);
        ensure_next_session(&state, SessionId::new(discovered_next));

        let mut manager = Self {
            persistence,
            state,
            agent_manager,
            approval_resolver,
            scheduler,
            sessions,
            actor_poll_queue: VecDeque::new(),
        };
        manager.purge_dead()?;
        Ok(manager)
    }

    pub(crate) fn create(
        &mut self,
        persistence: SessionPersistence,
    ) -> Result<SessionId, SessionCreateError> {
        let session = next_session(&self.state);
        let state = DurableState::new(SessionPersistentState::default());
        if persistence == SessionPersistence::Persistent {
            self.persistence
                .collection::<SessionPersistentState>(SESSION_STATE_NAME)?
                .register(&session_instance(session), &state)?;
        }
        let previous = self.sessions.insert(
            session,
            SessionEntry {
                persistence,
                state,
                actor: None,
            },
        );
        debug_assert!(previous.is_none());
        Ok(session)
    }

    pub(crate) fn list(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    pub(crate) fn open(
        &mut self,
        session: SessionId,
    ) -> Result<(SessionControl, SessionStream), OpenSessionError> {
        if !self.sessions.contains_key(&session) {
            return Err(OpenSessionError::SessionNotFound(session));
        }
        self.ensure_actor(session);
        let (events, receiver) = async_channel::unbounded::<SessionEvent>();
        let task = self
            .sessions
            .get_mut(&session)
            .and_then(|entry| entry.actor.as_mut())
            .expect("a known Session was just materialized");
        let lease = task.actor.open(events)?;
        let control = SessionControl::new(lease, task.commands.clone());
        let stream = SessionStream::new(lease, task.commands.clone(), receiver);
        Ok((control, stream))
    }

    pub(crate) fn delete(&mut self, session: SessionId) -> Result<(), SessionControlError> {
        if !self.sessions.contains_key(&session) {
            return Err(SessionControlError::SessionClosed(session));
        }
        self.ensure_actor(session);
        let task = self
            .sessions
            .get_mut(&session)
            .and_then(|entry| entry.actor.as_mut())
            .expect("a known Session was just materialized");
        task.actor.request_delete();
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) {
        for entry in self.sessions.values_mut() {
            if let Some(task) = &mut entry.actor {
                task.actor.request_shutdown();
            }
        }
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.actor_poll_queue.is_empty()
    }

    fn purge_dead(&mut self) -> Result<(), AgentCreateError> {
        let persisted_agents = self.agent_manager.list_persisted_agents()?;
        let next_agent_id = persisted_agents
            .iter()
            .map(|agent| agent.0)
            .max()
            .map(|agent| agent.saturating_add(1))
            .unwrap_or(1);
        ensure_next_agent(&self.state, AgentId::new(next_agent_id));

        let persisted_agents = persisted_agents.into_iter().collect::<BTreeSet<_>>();
        let mut reachable_agents = BTreeSet::new();

        for entry in self.sessions.values() {
            let Some(root) = entry.state.get().root_agent else {
                continue;
            };
            if persisted_agents.contains(&root) {
                reachable_agents.insert(root);
            } else {
                entry.state.get_mut().clear_root();
            }
        }

        for agent in persisted_agents.difference(&reachable_agents).copied() {
            self.agent_manager.remove(agent)?;
        }
        Ok(())
    }

    pub(crate) fn poll(&mut self, context: &mut Context<'_>) -> Poll<()> {
        let actor_count = self.actor_poll_queue.len();
        for _ in 0..actor_count {
            let Some(session) = self.actor_poll_queue.pop_front() else {
                break;
            };
            let status = {
                let Some(task) = self
                    .sessions
                    .get_mut(&session)
                    .and_then(|entry| entry.actor.as_mut())
                else {
                    continue;
                };
                self.actor_poll_queue.push_back(session);
                let _entered = task.span.enter();
                task.actor.poll(context)
            };
            match status {
                Poll::Ready(SessionActorStatus::Progress) => {
                    return Poll::Ready(());
                }
                Poll::Ready(SessionActorStatus::Exit(exit)) => {
                    self.finish_actor(exit);
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }
        }
        Poll::Pending
    }

    fn ensure_actor(&mut self, session: SessionId) {
        let entry = self
            .sessions
            .get(&session)
            .expect("only a known Session can be materialized");
        if entry.actor.is_some() {
            return;
        }
        let persistence = entry.persistence;
        let state = entry.state.clone();
        let (actor, commands) = SessionActor::new(
            session,
            persistence,
            Rc::clone(&self.agent_manager),
            AgentIdAllocatorHandle::new(&self.state),
            state,
            Rc::clone(&self.approval_resolver),
            self.scheduler.clone(),
        );
        let span = tracing::info_span!(
            "session",
            trace.task = %session,
            run.session = %session,
        );
        self.sessions
            .get_mut(&session)
            .expect("the Session remains registered")
            .actor = Some(LiveActor {
            commands,
            actor,
            span,
        });
        self.actor_poll_queue.push_back(session);
    }

    fn finish_actor(&mut self, exit: SessionActorExit) {
        let session = exit.session();
        self.actor_poll_queue.retain(|queued| *queued != session);
        if let Some(entry) = self.sessions.get_mut(&session) {
            entry.actor = None;
        }
        match exit {
            SessionActorExit::Deleted { .. } => {
                let result = self.remove_persistent_state(session);
                if result.is_ok() {
                    self.sessions.remove(&session);
                }
            }
            SessionActorExit::Shutdown { .. } => {}
        }
    }

    fn remove_persistent_state(&self, session: SessionId) -> Result<(), SessionControlError> {
        let Some(entry) = self.sessions.get(&session) else {
            return Err(SessionControlError::SessionClosed(session));
        };
        if entry.persistence == SessionPersistence::Ephemeral {
            return Ok(());
        }
        self.persistence
            .collection::<SessionPersistentState>(SESSION_STATE_NAME)
            .and_then(|states| states.remove(&session_instance(session)))
            .map_err(|error| {
                tracing::error!(
                    name: "session_state_remove_failed",
                    session = %session,
                    error = %error,
                );
                SessionControlError::Persistence
            })
    }
}
