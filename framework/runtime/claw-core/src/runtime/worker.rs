//! Single-thread process runtime loop.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::{mpsc, Arc};

use async_channel::{Receiver, Sender};
use claw_interface::http::StreamingHttp;
use claw_interface::{ClawExecutor, ClawFs, ClawHttp, ClawTimer};
use claw_persistence::SharedPersistence;
use claw_tool::ToolRegistry;
use futures_core::Stream;
use tracing::Instrument as _;

use crate::config::SharedApiManager;
use crate::scheduler::AgentRunScheduler;
use crate::session::{
    OpenSessionError, SessionControl, SessionCreateError, SessionDeleteError, SessionId,
    SessionManager, SessionManagerInitError, SessionPersistence, SessionStream,
};
use crate::SYSTEM_TRACE_SCOPE;

use super::agent_runtime::{AgentRuntimeBuildError, RUNTIME_TRACE_TASK};

pub(super) enum RuntimeCommand {
    CreateSession {
        persistence: SessionPersistence,
        ack: Sender<Result<SessionId, SessionCreateError>>,
    },
    ListSessions {
        ack: Sender<Vec<SessionId>>,
    },
    OpenSession {
        session: SessionId,
        ack: Sender<Result<(SessionControl, SessionStream), OpenSessionError>>,
    },
    DeleteSession {
        session: SessionId,
        ack: Sender<Result<(), SessionDeleteError>>,
    },
    Stop,
}

struct RuntimeWorker<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    persistence: SharedPersistence<Filesystem>,
    session_manager: SessionManager<Filesystem, Http, Timer>,
    scheduler: AgentRunScheduler<Http, Timer>,
    commands: Pin<Box<Receiver<RuntimeCommand>>>,
    stopping: bool,
    next_task: WorkerTask,
}

impl<Filesystem, Http, Timer> RuntimeWorker<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        tool_registry: Arc<ToolRegistry>,
        persistence: SharedPersistence<Filesystem>,
        persistence_dir: String,
        skill_roots: Vec<String>,
        api_manager: SharedApiManager,
        commands: Receiver<RuntimeCommand>,
    ) -> Result<Self, AgentRuntimeBuildError> {
        let (scheduler, scheduler_handle) = AgentRunScheduler::new();
        let session_manager = SessionManager::new(
            tool_registry,
            Arc::clone(&persistence),
            persistence_dir,
            skill_roots,
            api_manager,
            scheduler_handle,
        )
        .map_err(map_session_manager_init_error)?;
        Ok(Self {
            persistence,
            session_manager,
            scheduler,
            commands: Box::pin(commands),
            stopping: false,
            next_task: WorkerTask::Ingress,
        })
    }

    fn handle_command(&mut self, command: Option<RuntimeCommand>) {
        match command {
            Some(RuntimeCommand::CreateSession { persistence, ack }) => {
                let _ = ack.try_send(self.session_manager.create(persistence));
            }
            Some(RuntimeCommand::ListSessions { ack }) => {
                let _ = ack.try_send(self.session_manager.list());
            }
            Some(RuntimeCommand::OpenSession { session, ack }) => {
                let _ = ack.try_send(self.session_manager.open(session));
            }
            Some(RuntimeCommand::DeleteSession { session, ack }) => {
                self.session_manager.delete(session, ack);
            }
            Some(RuntimeCommand::Stop) | None => {
                self.stopping = true;
                self.session_manager.shutdown();
            }
        }
    }

    fn reject_commands(&self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                RuntimeCommand::CreateSession { ack, .. } => {
                    let _ = ack.try_send(Err(SessionCreateError::WorkerStopped));
                }
                RuntimeCommand::ListSessions { ack } => {
                    let _ = ack.try_send(Vec::new());
                }
                RuntimeCommand::OpenSession { ack, .. } => {
                    let _ = ack.try_send(Err(OpenSessionError::WorkerStopped));
                }
                RuntimeCommand::DeleteSession { ack, .. } => {
                    let _ = ack.try_send(Err(SessionDeleteError::WorkerStopped));
                }
                RuntimeCommand::Stop => {}
            }
        }
    }

    fn shutdown_complete(&self) -> bool {
        !self.session_manager.has_live_actors() && self.scheduler.is_idle()
    }
}

impl<Filesystem, Http, Timer> Unpin for RuntimeWorker<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
}

impl<Filesystem, Http, Timer> Future for RuntimeWorker<Filesystem, Http, Timer>
where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut progressed = false;

        for _ in 0..3 {
            let task = this.next_task;
            this.next_task = task.next();
            match task {
                WorkerTask::Ingress if !this.stopping => {
                    if let Poll::Ready(command) = this.commands.as_mut().poll_next(context) {
                        this.handle_command(command);
                        progressed = true;
                        break;
                    }
                }
                WorkerTask::Sessions => {
                    if let Poll::Ready(()) = this.session_manager.poll_actors(context) {
                        progressed = true;
                        break;
                    }
                }
                WorkerTask::Scheduler => {
                    if let Poll::Ready(Some(())) = Pin::new(&mut this.scheduler).poll_next(context)
                    {
                        progressed = true;
                        break;
                    }
                }
                WorkerTask::Ingress => {}
            }
        }

        if let Err(error) = this.persistence.maybe_persist() {
            tracing::error!(name: "persistence_failed", error = %error);
        }

        if this.stopping && this.shutdown_complete() {
            this.reject_commands();
            Poll::Ready(())
        } else {
            if progressed {
                context.waker().wake_by_ref();
            }
            Poll::Pending
        }
    }
}

#[derive(Clone, Copy)]
enum WorkerTask {
    Ingress,
    Sessions,
    Scheduler,
}

impl WorkerTask {
    fn next(self) -> Self {
        match self {
            Self::Ingress => Self::Sessions,
            Self::Sessions => Self::Scheduler,
            Self::Scheduler => Self::Ingress,
        }
    }
}

fn map_session_manager_init_error(error: SessionManagerInitError) -> AgentRuntimeBuildError {
    match error {
        SessionManagerInitError::AgentManager(error) => error.into(),
        SessionManagerInitError::AgentReconciliation(error) => {
            AgentRuntimeBuildError::AgentReconciliation(error.to_string())
        }
        SessionManagerInitError::Persistence(error) => error.into(),
        SessionManagerInitError::InvalidSessionId(error) => error.into(),
        SessionManagerInitError::MissingState(session) => {
            AgentRuntimeBuildError::MissingPersistedSessionState(session)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_runtime_worker<Filesystem, Http, Timer, Executor>(
    tool_registry: Arc<ToolRegistry>,
    persistence: SharedPersistence<Filesystem>,
    persistence_dir: String,
    skill_roots: Vec<String>,
    commands: Receiver<RuntimeCommand>,
    init_result: mpsc::Sender<Result<(), AgentRuntimeBuildError>>,
    api_manager: SharedApiManager,
) where
    Filesystem: ClawFs + 'static,
    Http: ClawHttp + StreamingHttp + Default + 'static,
    Timer: ClawTimer + Default + 'static,
    Executor: ClawExecutor,
{
    let span = tracing::info_span!(
        "agent.runtime",
        trace.task = RUNTIME_TRACE_TASK,
        run.system = SYSTEM_TRACE_SCOPE,
    );
    let worker = match span.in_scope(|| {
        RuntimeWorker::<Filesystem, Http, Timer>::new(
            tool_registry,
            persistence,
            persistence_dir,
            skill_roots,
            api_manager,
            commands,
        )
    }) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = init_result.send(Err(error));
            return;
        }
    };
    let _ = init_result.send(Ok(()));
    Executor::block_on(worker.instrument(span));
}
