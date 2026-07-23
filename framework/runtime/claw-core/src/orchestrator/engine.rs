use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{mpsc, Arc};

use async_channel::{Receiver, Sender};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawExecutor, ClawFs, ClawHttp, ClawTimer};
use claw_persistence::{DurableState, SharedPersistence};
use claw_tool::ToolRegistry;
use futures_core::Stream;
use tracing::Instrument as _;

use crate::agent::FsAgentFactory;
use crate::config::SharedApiManager;
use crate::multiagent::AgentIdAllocator;
use crate::protocol::{AgentId, EventSink, SessionId, SessionPersistence};
use crate::session::{
    session_instance, OpenSessionError, SessionActor, SessionActorExit, SessionCommand,
    SessionControlError, SessionCreateError, SessionEndpoint, SessionState, SessionStore,
    SESSION_STATE_NAME,
};

use super::{OrchestratorBuildError, ORCHESTRATOR_TRACE_TASK, SYSTEM_TRACE_SCOPE};

/// Process-wide commands. Turn and control commands go directly to a session actor.
pub(super) enum Command {
    CreateSession {
        persistence: SessionPersistence,
        ack: Sender<Result<SessionId, SessionCreateError>>,
    },
    OpenSession {
        session: SessionId,
        events: EventSink,
        ack: Sender<Result<SessionEndpoint, OpenSessionError>>,
    },
    DeleteSession {
        session: SessionId,
        ack: Sender<Result<(), SessionControlError>>,
    },
    Stop,
}

type ActorFuture = Pin<Box<dyn Future<Output = SessionActorExit>>>;

struct ActorTask {
    commands: Sender<SessionCommand>,
    future: ActorFuture,
}

struct Engine<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    factory: Rc<FsAgentFactory<Filesystem, Http, Timer>>,
    persistence: SharedPersistence<Filesystem>,
    sessions: Arc<SessionStore>,
    id_allocator: AgentIdAllocator,
    api_manager: SharedApiManager,
    actors: HashMap<SessionId, ActorTask>,
}

impl<Filesystem, Http, Timer> Engine<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        tools: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
        sessions: Arc<SessionStore>,
        id_allocator: AgentIdAllocator,
        api_manager: SharedApiManager,
    ) -> Result<Self, OrchestratorBuildError> {
        let factory = Rc::new(FsAgentFactory::new(
            tools,
            Arc::clone(&persistence),
            persistence_dir,
            skill_roots,
            Arc::clone(&api_manager),
        )?);
        let session_states = persistence.collection::<SessionState>(SESSION_STATE_NAME)?;
        let persistent_sessions = sessions
            .list()
            .into_iter()
            .filter(|session| {
                sessions.persistence(*session) == Some(SessionPersistence::Persistent)
            })
            .collect::<Vec<_>>();
        for session in persistent_sessions {
            let _ = session_states.load(&session_instance(session))?;
        }

        Ok(Self {
            factory,
            persistence,
            sessions,
            id_allocator,
            api_manager,
            actors: HashMap::new(),
        })
    }

    fn run(self, commands: Receiver<Command>) -> EngineDriver<Filesystem, Http, Timer> {
        EngineDriver {
            engine: self,
            commands: Box::pin(commands),
            stopping: false,
        }
    }

    fn handle_event(&mut self, event: EngineEvent, stopping: &mut bool) {
        match event {
            EngineEvent::ActorExited(exit) => {
                let session = exit.session();
                self.actors.remove(&session);
                match exit {
                    SessionActorExit::Deleted {
                        root_agent, acks, ..
                    } => self.finish_session_deletion(session, root_agent, acks),
                    SessionActorExit::Shutdown { state, .. } => {
                        if let Err(error) = self.persistence.maybe_persist() {
                            tracing::error!(name: "persistence_failed", error = %error);
                        }
                        drop(state);
                    }
                }
            }
            EngineEvent::Command(Some(Command::CreateSession { persistence, ack })) => {
                self.create_session(persistence, ack);
            }
            EngineEvent::Command(Some(Command::OpenSession {
                session,
                events,
                ack,
            })) => {
                self.open_session(session, events, ack);
            }
            EngineEvent::Command(Some(Command::DeleteSession { session, ack })) => {
                self.delete_session(session, ack);
            }
            EngineEvent::Command(Some(Command::Stop)) | EngineEvent::Command(None) => {
                *stopping = true;
                for actor in self.actors.values() {
                    let _ = actor.commands.try_send(SessionCommand::Shutdown);
                }
            }
        }
    }

    fn create_session(
        &mut self,
        session_persistence: SessionPersistence,
        ack: Sender<Result<SessionId, SessionCreateError>>,
    ) {
        let session = self.sessions.allocate();
        let state = DurableState::new(SessionState::default());
        if session_persistence == SessionPersistence::Persistent {
            let registration = self
                .persistence
                .collection::<SessionState>(SESSION_STATE_NAME)
                .and_then(|sessions| sessions.register(&session_instance(session), &state));
            if let Err(error) = registration {
                let _ = ack.try_send(Err(SessionCreateError::from(error)));
                return;
            }
        }
        let actor = SessionActor::new(
            session,
            session_persistence,
            Rc::clone(&self.factory),
            self.id_allocator.clone(),
            state,
            Arc::clone(&self.api_manager),
        );
        self.spawn_actor(session, actor);
        self.sessions.publish(session, session_persistence);
        let _ = ack.try_send(Ok(session));
    }

    fn open_session(
        &mut self,
        session: SessionId,
        events: EventSink,
        ack: Sender<Result<SessionEndpoint, OpenSessionError>>,
    ) {
        let Some(session_persistence) = self.sessions.persistence(session) else {
            let _ = ack.try_send(Err(OpenSessionError::SessionNotFound(session)));
            return;
        };
        if !self.actors.contains_key(&session) {
            let state = if session_persistence == SessionPersistence::Persistent {
                let instance = session_instance(session);
                match self
                    .persistence
                    .collection::<SessionState>(SESSION_STATE_NAME)
                    .and_then(|sessions| sessions.load(&instance))
                {
                    Ok(Some(dto)) => DurableState::new(dto),
                    Ok(None) => {
                        tracing::error!(
                            name: "session_state_load_failed",
                            session = %session,
                            error = "persisted session state is missing",
                        );
                        let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
                        return;
                    }
                    Err(error) => {
                        tracing::error!(
                            name: "session_state_load_failed",
                            session = %session,
                            error = %error,
                        );
                        let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
                        return;
                    }
                }
            } else {
                DurableState::new(SessionState::default())
            };
            if session_persistence == SessionPersistence::Persistent {
                let registration = self
                    .persistence
                    .collection::<SessionState>(SESSION_STATE_NAME)
                    .and_then(|sessions| sessions.register(&session_instance(session), &state));
                if let Err(error) = registration {
                    tracing::error!(
                        name: "session_state_registration_failed",
                        session = %session,
                        error = %error,
                    );
                    let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
                    return;
                }
            }
            let actor = SessionActor::new(
                session,
                session_persistence,
                Rc::clone(&self.factory),
                self.id_allocator.clone(),
                state,
                Arc::clone(&self.api_manager),
            );
            self.spawn_actor(session, actor);
        }
        let task = self
            .actors
            .get(&session)
            .expect("the session actor was just created");
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

    fn delete_session(&mut self, session: SessionId, ack: Sender<Result<(), SessionControlError>>) {
        let Some(persistence) = self.sessions.persistence(session) else {
            let _ = ack.try_send(Err(SessionControlError::SessionClosed(session)));
            return;
        };

        let Some(task) = self.actors.get(&session) else {
            let root_agent = match self.load_root_agent(session, persistence) {
                Ok(root_agent) => root_agent,
                Err(error) => {
                    let _ = ack.try_send(Err(error));
                    return;
                }
            };
            self.finish_session_deletion(session, root_agent, vec![ack]);
            return;
        };
        if task
            .commands
            .try_send(SessionCommand::Delete { ack: ack.clone() })
            .is_err()
        {
            let _ = ack.try_send(Err(SessionControlError::WorkerStopped));
        }
    }

    fn load_root_agent(
        &self,
        session: SessionId,
        persistence: SessionPersistence,
    ) -> Result<Option<AgentId>, SessionControlError> {
        if persistence == SessionPersistence::Ephemeral {
            return Ok(None);
        }
        self.persistence
            .collection::<SessionState>(SESSION_STATE_NAME)
            .and_then(|sessions| sessions.load(&session_instance(session)))
            .map_err(|error| {
                tracing::error!(
                    name: "session_state_load_failed",
                    session = %session,
                    error = %error,
                );
                SessionControlError::Persistence
            })?
            .map(|state| state.root_agent())
            .ok_or_else(|| {
                tracing::error!(
                    name: "session_state_load_failed",
                    session = %session,
                    error = "persisted session state is missing",
                );
                SessionControlError::Persistence
            })
    }

    fn finish_session_deletion(
        &mut self,
        session: SessionId,
        root_agent: Option<AgentId>,
        acks: Vec<Sender<Result<(), SessionControlError>>>,
    ) {
        let result = self.remove_session_storage(session, root_agent);
        if result.is_ok() {
            self.sessions.delete(session);
        }
        for ack in acks {
            let _ = ack.try_send(result.clone());
        }
    }

    fn remove_session_storage(
        &self,
        session: SessionId,
        root_agent: Option<AgentId>,
    ) -> Result<(), SessionControlError> {
        let Some(persistence) = self.sessions.persistence(session) else {
            return Err(SessionControlError::SessionClosed(session));
        };
        if persistence == SessionPersistence::Ephemeral {
            return Ok(());
        }
        if let Some(root_agent) = root_agent {
            self.factory.remove(root_agent).map_err(|error| {
                tracing::error!(
                    name: "root_agent_remove_failed",
                    session = %session,
                    agent = %root_agent,
                    error = %error,
                );
                SessionControlError::Persistence
            })?;
        }
        self.persistence
            .collection::<SessionState>(SESSION_STATE_NAME)
            .and_then(|sessions| sessions.remove(&session_instance(session)))
            .map_err(|error| {
                tracing::error!(
                    name: "session_state_remove_failed",
                    session = %session,
                    error = %error,
                );
                SessionControlError::Persistence
            })
    }

    fn spawn_actor(&mut self, session: SessionId, actor: SessionActor<Filesystem, Http, Timer>) {
        let (commands, receiver) = async_channel::unbounded();
        let future = Box::pin(actor.run(receiver).instrument(tracing::info_span!(
            "session",
            trace.task = %session,
            run.session = %session,
        )));
        let previous = self.actors.insert(session, ActorTask { commands, future });
        debug_assert!(previous.is_none());
    }

    fn reject_queued_commands(&self, commands: &Receiver<Command>) {
        while let Ok(command) = commands.try_recv() {
            match command {
                Command::CreateSession { ack, .. } => {
                    let _ = ack.try_send(Err(SessionCreateError::WorkerStopped));
                }
                Command::OpenSession { ack, .. } => {
                    let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
                }
                Command::DeleteSession { ack, .. } => {
                    let _ = ack.try_send(Err(SessionControlError::WorkerStopped));
                }
                Command::Stop => {}
            }
        }
    }
}

enum EngineEvent {
    ActorExited(SessionActorExit),
    Command(Option<Command>),
}

struct EnginePoll<'a> {
    commands: Option<Pin<&'a mut Receiver<Command>>>,
    actors: &'a mut HashMap<SessionId, ActorTask>,
}

impl Future for EnginePoll<'_> {
    type Output = EngineEvent;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(commands) = this.commands.as_mut() {
            if let Poll::Ready(command) = commands.as_mut().poll_next(context) {
                return Poll::Ready(EngineEvent::Command(command));
            }
        }
        for actor in this.actors.values_mut() {
            if let Poll::Ready(exit) = actor.future.as_mut().poll(context) {
                return Poll::Ready(EngineEvent::ActorExited(exit));
            }
        }
        Poll::Pending
    }
}

struct EngineDriver<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    engine: Engine<Filesystem, Http, Timer>,
    commands: Pin<Box<Receiver<Command>>>,
    stopping: bool,
}

impl<Filesystem, Http, Timer> Unpin for EngineDriver<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
}

impl<Filesystem, Http, Timer> Future for EngineDriver<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let event = if this.stopping && this.engine.actors.is_empty() {
            None
        } else {
            let mut engine_poll = EnginePoll {
                commands: (!this.stopping).then_some(this.commands.as_mut()),
                actors: &mut this.engine.actors,
            };
            match Pin::new(&mut engine_poll).poll(context) {
                Poll::Ready(event) => Some(event),
                Poll::Pending => None,
            }
        };

        let handled_event = event.is_some();
        if let Some(event) = event {
            this.engine.handle_event(event, &mut this.stopping);
        }

        // The entire runtime has one persistence boundary: after every
        // top-level async poll, including polls where a tool future yields.
        if let Err(error) = this.engine.persistence.maybe_persist() {
            tracing::error!(name: "persistence_failed", error = %error);
        }

        if this.stopping && this.engine.actors.is_empty() {
            this.engine
                .reject_queued_commands(this.commands.as_ref().get_ref());
            Poll::Ready(())
        } else {
            if handled_event {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_engine<Filesystem, Http, Timer, Executor>(
    tools: Arc<ToolRegistry>,
    persistence: SharedPersistence<Filesystem>,
    persistence_dir: String,
    skill_roots: Vec<String>,
    sessions: Arc<SessionStore>,
    id_allocator: AgentIdAllocator,
    command_rx: Receiver<Command>,
    init_result_tx: mpsc::Sender<Result<(), OrchestratorBuildError>>,
    api_manager: SharedApiManager,
) where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
    Executor: ClawExecutor,
{
    let span = tracing::info_span!(
        "orchestrator",
        trace.task = ORCHESTRATOR_TRACE_TASK,
        run.system = SYSTEM_TRACE_SCOPE,
    );
    let engine = match span.in_scope(|| {
        Engine::<Filesystem, Http, Timer>::new(
            tools,
            persistence,
            persistence_dir,
            skill_roots,
            sessions,
            id_allocator,
            api_manager,
        )
    }) {
        Ok(engine) => engine,
        Err(error) => {
            let _ = init_result_tx.send(Err(error));
            return;
        }
    };
    let _ = init_result_tx.send(Ok(()));
    Executor::block_on(engine.run(command_rx).instrument(span));
}
