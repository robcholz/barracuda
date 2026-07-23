//! Ownership and lifecycle for every Session in one runtime.

use core::task::{Context, Poll};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use async_channel::Sender;
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawFs, ClawHttp, ClawTimer};
use claw_persistence::{DurableState, PersistenceError, SharedPersistence};
use claw_tool::ToolRegistry;

use crate::agent::{AgentManager, AgentManagerError};
use crate::config::SharedApiManager;
use crate::scheduler::AgentRunSchedulerHandle;

use super::actor::{SessionActor, SessionActorExit, SessionActorStatus};
use super::api::{OpenSessionError, SessionControlError, SessionCreateError};
use super::command::{SessionCommand, SessionEndpoint};
use super::manager_state::{
    ensure_next_session, next_session, SessionManagerState, SESSION_MANAGER_STATE_NAME,
};
use super::persistent_state::{session_instance, SessionPersistentState, SESSION_STATE_NAME};
use super::{SessionEvent, SessionId, SessionPersistence};

struct ActorTask<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    commands: Sender<SessionCommand>,
    actor: SessionActor<Filesystem, Http, Timer>,
    span: tracing::Span,
}

struct ManagedSession<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    persistence: SessionPersistence,
    state: DurableState<SessionPersistentState>,
    actor: Option<ActorTask<Filesystem, Http, Timer>>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionManagerInitError {
    #[error(transparent)]
    AgentManager(#[from] AgentManagerError),
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
    agent_manager: Rc<AgentManager<Filesystem, Http, Timer>>,
    api_manager: SharedApiManager,
    scheduler: AgentRunSchedulerHandle<Http, Timer>,
    sessions: BTreeMap<SessionId, ManagedSession<Filesystem, Http, Timer>>,
    actor_order: VecDeque<SessionId>,
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
        let states = persistence.collection::<SessionPersistentState>(SESSION_STATE_NAME)?;
        let mut sessions: BTreeMap<SessionId, ManagedSession<Filesystem, Http, Timer>> =
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
                ManagedSession {
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

        Ok(Self {
            persistence,
            state,
            agent_manager,
            api_manager,
            scheduler,
            sessions,
            actor_order: VecDeque::new(),
        })
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
            ManagedSession {
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
        events: Sender<SessionEvent>,
        ack: Sender<Result<SessionEndpoint, OpenSessionError>>,
    ) {
        if !self.sessions.contains_key(&session) {
            let _ = ack.try_send(Err(OpenSessionError::SessionNotFound(session)));
            return;
        }
        self.ensure_actor(session);
        let task = self
            .sessions
            .get(&session)
            .and_then(|entry| entry.actor.as_ref())
            .expect("a known Session was just materialized");
        if task
            .commands
            .try_send(SessionCommand::Open {
                events,
                commands: task.commands.clone(),
                ack: ack.clone(),
            })
            .is_err()
        {
            let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
        }
    }

    pub(crate) fn delete(
        &mut self,
        session: SessionId,
        ack: Sender<Result<(), SessionControlError>>,
    ) {
        if !self.sessions.contains_key(&session) {
            let _ = ack.try_send(Err(SessionControlError::SessionClosed(session)));
            return;
        }
        self.ensure_actor(session);
        let task = self
            .sessions
            .get(&session)
            .and_then(|entry| entry.actor.as_ref())
            .expect("a known Session was just materialized");
        if task
            .commands
            .try_send(SessionCommand::Delete { ack: ack.clone() })
            .is_err()
        {
            let _ = ack.try_send(Err(SessionControlError::WorkerStopped));
        }
    }

    pub(crate) fn shutdown(&self) {
        for entry in self.sessions.values() {
            if let Some(task) = &entry.actor {
                let _ = task.commands.try_send(SessionCommand::Shutdown);
            }
        }
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.actor_order.is_empty()
    }

    pub(crate) fn poll(&mut self, context: &mut Context<'_>) -> Poll<SessionManagerStatus> {
        let actor_count = self.actor_order.len();
        for _ in 0..actor_count {
            let Some(session) = self.actor_order.pop_front() else {
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
                self.actor_order.push_back(session);
                let _entered = task.span.enter();
                task.actor.poll(context, &self.state)
            };
            match status {
                Poll::Ready(SessionActorStatus::Progress) => {
                    return Poll::Ready(SessionManagerStatus::Progress);
                }
                Poll::Ready(SessionActorStatus::Exit(exit)) => {
                    self.finish_actor(exit);
                    return Poll::Ready(SessionManagerStatus::Progress);
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
            state,
            Arc::clone(&self.api_manager),
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
            .actor = Some(ActorTask {
            commands,
            actor,
            span,
        });
        self.actor_order.push_back(session);
    }

    fn finish_actor(&mut self, exit: SessionActorExit) {
        let session = exit.session();
        self.actor_order.retain(|queued| *queued != session);
        if let Some(entry) = self.sessions.get_mut(&session) {
            entry.actor = None;
        }
        match exit {
            SessionActorExit::Deleted { acks, .. } => {
                let result = self.remove_session_record(session);
                if result.is_ok() {
                    self.sessions.remove(&session);
                }
                for ack in acks {
                    let _ = ack.try_send(result.clone());
                }
            }
            SessionActorExit::Shutdown { .. } => {}
        }
    }

    fn remove_session_record(&self, session: SessionId) -> Result<(), SessionControlError> {
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

pub(crate) enum SessionManagerStatus {
    Progress,
}
