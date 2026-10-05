//! Single-thread process runtime loop.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::Receiver;
use barracuda_agent_persistence::{PersistenceError, SharedPersistence};
use barracuda_agent_tool::ToolRegistry;
use barracuda_model_api::ModelApiFactory;
use barracuda_runtime_utils::oneshot;
use barracuda_vfs::ScopedVfs;
use futures_core::Stream;
use http_client::embedded_nal_async::{Dns, TcpConnect};
use portable_atomic_util::Arc;

use barracuda_agent::AgentCreateError;
use barracuda_agent::SharedApiManager;
use barracuda_agent_session::{
    OpenSessionError, SessionControl, SessionCreateError, SessionDeleteError, SessionId,
    SessionManager, SessionManagerInitError, SessionPersistence, SessionStream,
};

use crate::service::RuntimeBuildError;

pub(super) enum RuntimeCommand {
    CreateSession {
        persistence: SessionPersistence,
        ack: oneshot::Sender<Result<SessionId, SessionCreateError>>,
    },
    ListSessions {
        ack: oneshot::Sender<Vec<SessionId>>,
    },
    OpenSession {
        session: SessionId,
        ack: oneshot::Sender<Result<(SessionControl, SessionStream), OpenSessionError>>,
    },
    DeleteSession {
        session: SessionId,
        ack: oneshot::Sender<Result<(), SessionDeleteError>>,
    },
    Stop,
}

pub(super) struct RuntimeWorkerInit<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(super) filesystem: ScopedVfs,
    pub(super) tool_registry: Arc<ToolRegistry>,
    pub(super) persistence: SharedPersistence,
    pub(super) persistence_dir: String,
    pub(super) skill_roots: Vec<String>,
    pub(super) api_manager: SharedApiManager,
    pub(super) llm_factory: ModelApiFactory<Tcp, Resolver>,
    pub(super) commands: Receiver<RuntimeCommand>,
}

pub(super) struct RuntimeWorker<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    persistence: Option<SharedPersistence>,
    persistence_task: Option<PersistenceTask>,
    maintenance_task: Option<MaintenanceTask>,
    session_manager: SessionManager<Tcp, Resolver>,
    commands: Pin<Box<Receiver<RuntimeCommand>>>,
    stopping: bool,
    next_task: WorkerTask,
}

type PersistenceTask =
    Pin<Box<dyn Future<Output = (SharedPersistence, Result<(), PersistenceError>)>>>;
type MaintenanceTask = Pin<Box<dyn Future<Output = Result<(), AgentCreateError>>>>;

impl<Tcp, Resolver> RuntimeWorker<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    pub(super) async fn new(
        init: RuntimeWorkerInit<Tcp, Resolver>,
    ) -> Result<Self, RuntimeBuildError> {
        let RuntimeWorkerInit {
            filesystem,
            tool_registry,
            persistence,
            persistence_dir,
            skill_roots,
            api_manager,
            llm_factory,
            commands,
        } = init;
        let session_manager = SessionManager::new(
            filesystem,
            tool_registry,
            Arc::clone(&persistence),
            persistence_dir,
            skill_roots,
            api_manager,
            llm_factory,
        )
        .await
        .map_err(map_session_manager_init_error)?;
        Ok(Self {
            persistence: Some(persistence),
            persistence_task: None,
            maintenance_task: None,
            session_manager,
            commands: Box::pin(commands),
            stopping: false,
            next_task: WorkerTask::Ingress,
        })
    }

    fn handle_command(&mut self, command: Option<RuntimeCommand>) {
        match command {
            Some(RuntimeCommand::CreateSession { persistence, ack }) => {
                let _ = ack.send(self.session_manager.create(persistence));
            }
            Some(RuntimeCommand::ListSessions { ack }) => {
                let _ = ack.send(self.session_manager.list());
            }
            Some(RuntimeCommand::OpenSession { session, ack }) => {
                let _ = ack.send(self.session_manager.open(session));
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
                    let _ = ack.send(Err(SessionCreateError::WorkerStopped));
                }
                RuntimeCommand::ListSessions { ack } => {
                    let _ = ack.send(Vec::new());
                }
                RuntimeCommand::OpenSession { ack, .. } => {
                    let _ = ack.send(Err(OpenSessionError::WorkerStopped));
                }
                RuntimeCommand::DeleteSession { ack, .. } => {
                    let _ = ack.send(Err(SessionDeleteError::WorkerStopped));
                }
                RuntimeCommand::Stop => {}
            }
        }
    }

    fn shutdown_complete(&self) -> bool {
        !self.session_manager.has_live_actors()
    }

    fn poll_persistence(&mut self, context: &mut Context<'_>) {
        if self.persistence_task.is_none() {
            let Some(persistence) = self.persistence.take() else {
                return;
            };
            self.persistence_task = Some(Box::pin(async move {
                let result = persistence.maybe_persist().await;
                (persistence, result)
            }));
        }
        let Some(task) = self.persistence_task.as_mut() else {
            return;
        };
        if let Poll::Ready((persistence, result)) = task.as_mut().poll(context) {
            self.persistence_task = None;
            self.persistence = Some(persistence);
            if let Err(error) = result {
                log::error!("runtime persistence failed: {error}");
                tracing::error!(name: "persistence_failed", error = %error);
            }
        }
    }

    fn poll_maintenance(&mut self, context: &mut Context<'_>) {
        if self.maintenance_task.is_none() {
            self.maintenance_task = Some(self.session_manager.storage_maintenance());
        }
        let Some(task) = self.maintenance_task.as_mut() else {
            return;
        };
        if let Poll::Ready(result) = task.as_mut().poll(context) {
            self.maintenance_task = None;
            if let Err(error) = result {
                log::error!("runtime storage maintenance failed: {error}");
                tracing::error!(name: "storage_maintenance_failed", error = %error);
            }
        }
    }
}

impl<Tcp, Resolver> Unpin for RuntimeWorker<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
}

impl<Tcp, Resolver> Future for RuntimeWorker<Tcp, Resolver>
where
    Tcp: TcpConnect + 'static,
    Resolver: Dns + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut progressed = false;

        for _ in 0..2 {
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
                WorkerTask::Ingress => {}
            }
        }

        this.poll_persistence(context);
        this.poll_maintenance(context);

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
}

impl WorkerTask {
    fn next(self) -> Self {
        match self {
            Self::Ingress => Self::Sessions,
            Self::Sessions => Self::Ingress,
        }
    }
}

fn map_session_manager_init_error(error: SessionManagerInitError) -> RuntimeBuildError {
    match error {
        SessionManagerInitError::AgentManager(error) => error.into(),
        SessionManagerInitError::AgentReconciliation(error) => error.into(),
        SessionManagerInitError::Persistence(error) => error.into(),
        SessionManagerInitError::InvalidSessionId(error) => error.into(),
        SessionManagerInitError::MissingState(session) => {
            RuntimeBuildError::MissingPersistedSessionState(session)
        }
    }
}
