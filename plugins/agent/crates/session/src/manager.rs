//! Ownership and lifecycle for every Session in one runtime.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::task::{Context, Poll};
use core::{future::Future, pin::Pin};

use async_channel::Sender;
use barracuda_agent_persistence::{
    DurableState, InvalidInstanceId, PersistenceError, SharedPersistence,
};
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::ModelApiFactory;
use barracuda_vfs::ScopedVfs;
use futures_channel::oneshot;
use http_client::embedded_nal_async::{Dns, TcpConnect};

use barracuda_agent::SharedApiManager;
use barracuda_agent::{AgentCreateError, AgentId, AgentManager, AgentManagerError};

use super::actor::{SessionActor, SessionActorExit, SessionActorStatus};
use super::approval::{LlmApprovalResolver, SharedApprovalResolver};
use super::control::{SessionCommand, SessionControl};
use super::persistence::{session_instance, SESSION_MANAGER_STATE_NAME, SESSION_STATE_NAME};
use super::state::{
    allocate_session_id, ensure_next_agent_id, ensure_next_session_id, AgentIdAllocatorHandle,
    SessionManagerState, SessionPersistentState,
};
use super::{SessionEvent, SessionStream};

pub(super) type SharedAgentManager<Tcp, Resolver> = Rc<AgentManager<Tcp, Resolver>>;

barracuda_runtime_utils::define_prefixed_id!(SessionId, "session-", "session");

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
    #[error(transparent)]
    InvalidInstanceId(#[from] InvalidInstanceId),
}

/// Failure permanently deleting one Session and all of its Agent-owned stores.
#[derive(Debug, thiserror::Error)]
pub enum SessionDeleteError {
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    #[error("session deletion is already in progress: {0}")]
    AlreadyDeleting(SessionId),
    #[error("agent runtime is not running")]
    WorkerStopped,
    #[error(transparent)]
    Agent(#[from] AgentCreateError),
    #[error("failed to delete persisted session state: {0}")]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    InvalidInstanceId(#[from] InvalidInstanceId),
}

struct LiveActor<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    commands: Sender<SessionCommand>,
    actor: SessionActor<Tcp, Resolver>,
    span: tracing::Span,
}

struct SessionEntry<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    persistence: SessionPersistence,
    state: DurableState<SessionPersistentState>,
    actor: Option<LiveActor<Tcp, Resolver>>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionManagerInitError {
    #[error(transparent)]
    AgentManager(#[from] AgentManagerError),
    #[error("failed to reconcile persisted agents: {0}")]
    AgentReconciliation(#[from] AgentCreateError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    InvalidSessionId(#[from] barracuda_runtime_utils::IdParseError),
    #[error("persisted session state is missing: {0}")]
    MissingState(SessionId),
}

/// Owns the complete Session aggregate lifecycle.
///
/// Session-owned metadata stays in `SessionPersistentState`; Agent records and
/// transcripts remain canonical in `AgentManager`. A live `SessionActor`
/// coordinates both without exposing either store to the worker loop.
pub struct SessionManager<Tcp = http_client::Tcp, Resolver = http_client::Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    persistence: SharedPersistence,
    state: DurableState<SessionManagerState>,
    agent_manager: SharedAgentManager<Tcp, Resolver>,
    approval_resolver: SharedApprovalResolver<Tcp, Resolver>,
    sessions: BTreeMap<SessionId, SessionEntry<Tcp, Resolver>>,
    actor_poll_queue: VecDeque<SessionId>,
}

impl<Tcp, Resolver> SessionManager<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub async fn new(
        filesystem: ScopedVfs,
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence,
        persistence_dir: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
        llm_factory: ModelApiFactory<Tcp, Resolver>,
    ) -> Result<Self, SessionManagerInitError> {
        let state = {
            let entry = persistence.singleton::<SessionManagerState>(SESSION_MANAGER_STATE_NAME)?;
            let state = DurableState::new(entry.load().await?.unwrap_or_default());
            entry.register(&state)?;
            state
        };
        let agent_manager = Rc::new(
            AgentManager::new(
                filesystem,
                tool_registry,
                Arc::clone(&persistence),
                persistence_dir,
                skill_roots,
                Arc::clone(&api_manager),
                llm_factory.clone(),
            )
            .await?,
        );
        let approval_resolver: SharedApprovalResolver<Tcp, Resolver> =
            Rc::new(LlmApprovalResolver::new(api_manager, llm_factory));
        let states = persistence.collection::<SessionPersistentState>(SESSION_STATE_NAME)?;
        let mut sessions: BTreeMap<SessionId, SessionEntry<Tcp, Resolver>> = BTreeMap::new();
        for instance in states.list().await? {
            let session = SessionId::from_wire(instance.as_str())?;
            let persisted = states
                .load(&instance)
                .await?
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
        let next_session_id = sessions
            .last_key_value()
            .map(|(session, _)| session.0.saturating_add(1))
            .unwrap_or(1);
        ensure_next_session_id(&state, SessionId::new(next_session_id));

        let mut manager = Self {
            persistence,
            state,
            agent_manager,
            approval_resolver,
            sessions,
            actor_poll_queue: VecDeque::new(),
        };
        manager.purge_dead()?;
        Ok(manager)
    }

    pub fn create(
        &mut self,
        persistence: SessionPersistence,
    ) -> Result<SessionId, SessionCreateError> {
        let session = allocate_session_id(&self.state);
        let state = DurableState::new(SessionPersistentState::default());
        if persistence == SessionPersistence::Persistent {
            let instance = session_instance(session)?;
            self.persistence
                .collection::<SessionPersistentState>(SESSION_STATE_NAME)?
                .register(&instance, &state)?;
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

    pub fn list(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    pub fn open(
        &mut self,
        session: SessionId,
    ) -> Result<(SessionControl, SessionStream), OpenSessionError> {
        if !self.sessions.contains_key(&session) {
            return Err(OpenSessionError::SessionNotFound(session));
        }
        if !self.ensure_actor(session) {
            return Err(OpenSessionError::SessionNotFound(session));
        }
        let (events, receiver) = async_channel::unbounded::<SessionEvent>();
        let Some(task) = self
            .sessions
            .get_mut(&session)
            .and_then(|entry| entry.actor.as_mut())
        else {
            return Err(OpenSessionError::SessionNotFound(session));
        };
        let lease = task.actor.open(events)?;
        let control = SessionControl::new(lease, task.commands.clone());
        let stream = SessionStream::new(lease, task.commands.clone(), receiver);
        Ok((control, stream))
    }

    pub fn delete(
        &mut self,
        session: SessionId,
        ack: oneshot::Sender<Result<(), SessionDeleteError>>,
    ) {
        if !self.sessions.contains_key(&session) {
            let _ = ack.send(Err(SessionDeleteError::SessionNotFound(session)));
            return;
        }
        if !self.ensure_actor(session) {
            let _ = ack.send(Err(SessionDeleteError::SessionNotFound(session)));
            return;
        }
        let Some(task) = self
            .sessions
            .get_mut(&session)
            .and_then(|entry| entry.actor.as_mut())
        else {
            let _ = ack.send(Err(SessionDeleteError::SessionNotFound(session)));
            return;
        };
        task.actor.request_delete(ack);
    }

    pub fn shutdown(&mut self) {
        for entry in self.sessions.values_mut() {
            if let Some(task) = &mut entry.actor {
                task.actor.request_shutdown();
            }
        }
    }

    pub fn has_live_actors(&self) -> bool {
        !self.actor_poll_queue.is_empty()
    }

    /// Returns one owned maintenance future for deferred Agent storage cleanup.
    pub fn storage_maintenance(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentCreateError>>>> {
        let manager = Rc::clone(&self.agent_manager);
        Box::pin(async move { manager.flush_storage().await })
    }

    fn purge_dead(&mut self) -> Result<(), AgentCreateError> {
        let persisted_agents = self.agent_manager.list_persisted_agents()?;
        let next_agent_id = persisted_agents
            .iter()
            .map(|agent| agent.0)
            .max()
            .map(|agent| agent.saturating_add(1))
            .unwrap_or(1);
        ensure_next_agent_id(&self.state, AgentId::new(next_agent_id));

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

    /// Fairly advance the currently live Session actors once.
    pub fn poll_actors(&mut self, context: &mut Context<'_>) -> Poll<()> {
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

    fn ensure_actor(&mut self, session: SessionId) -> bool {
        let Some(entry) = self.sessions.get(&session) else {
            return false;
        };
        if entry.actor.is_some() {
            return true;
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
        );
        let span = tracing::info_span!(
            "session",
            trace.task = %session,
            run.session = %session,
        );
        let Some(entry) = self.sessions.get_mut(&session) else {
            return false;
        };
        entry.actor = Some(LiveActor {
            commands,
            actor,
            span,
        });
        self.actor_poll_queue.push_back(session);
        true
    }

    fn finish_actor(&mut self, exit: SessionActorExit) {
        let session = exit.session();
        match exit {
            SessionActorExit::DeleteReady { .. } => {
                let result = self.remove_persistent_state(session);
                let Some(task) = self
                    .sessions
                    .get_mut(&session)
                    .and_then(|entry| entry.actor.as_mut())
                else {
                    log::error!("session {session} delete-ready actor is missing");
                    tracing::error!(
                        name: "delete_ready_actor_missing",
                        session = %session,
                    );
                    return;
                };
                let deleted = task.actor.complete_delete(result);
                if deleted {
                    self.actor_poll_queue.retain(|queued| *queued != session);
                    self.sessions.remove(&session);
                }
            }
            SessionActorExit::Shutdown { .. } => {
                self.actor_poll_queue.retain(|queued| *queued != session);
                if let Some(entry) = self.sessions.get_mut(&session) {
                    entry.actor = None;
                }
            }
        }
    }

    fn remove_persistent_state(&self, session: SessionId) -> Result<(), SessionDeleteError> {
        let Some(entry) = self.sessions.get(&session) else {
            return Err(SessionDeleteError::SessionNotFound(session));
        };
        if entry.persistence == SessionPersistence::Ephemeral {
            return Ok(());
        }
        let instance = session_instance(session)?;
        self.persistence
            .collection::<SessionPersistentState>(SESSION_STATE_NAME)
            .and_then(|states| states.remove(&instance))
            .map_err(|error| {
                log::error!("session {session} state removal failed: {error}");
                tracing::error!(
                    name: "session_state_remove_failed",
                    session = %session,
                    error = %error,
                );
                SessionDeleteError::Persistence(error)
            })
    }
}
