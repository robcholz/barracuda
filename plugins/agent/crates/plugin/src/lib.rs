//! Plugin that owns the Agent runtime and exposes it to Workflow.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;

mod bundled_workflows;
mod model_api_http;
mod workflow;

use barracuda_agent_runtime::{ModelApiFactory, RuntimeService, RuntimeStorageConfig};
use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry};
use barracuda_model_api::ModelApi;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext, PluginTaskToken,
};
use barracuda_webserver_plugin::WebServer;
use barracuda_workflow_plugin::{WorkflowActionRegistry, WorkflowService};
use embassy_futures::select::select;
use http_client::ClientFactory;
use workflow::AgentWorkflowAdapter;

pub use barracuda_agent_runtime::{
    stream, tools, AgentRuntime, AgentToolRegistry, IterationEvent, Message, PermissionLevel,
    SessionEvent, SessionPersistence, TurnEvent,
};
pub use model_api_http::SET_API_PATH;

const PERSISTENCE_ROOT: &str = "/data";

/// Plugin that constructs the Agent runtime and registers its Workflow Actions.
#[barracuda_plugin::macros::plugin]
pub struct AgentPlugin {
    http_clients: ClientFactory<'static>,
    runtime: Option<Rc<AgentRuntime>>,
    runtime_service: Option<RuntimeService>,
    workflow_adapter: Option<AgentWorkflowAdapter>,
    workflow_service: Option<Rc<WorkflowService>>,
}

impl AgentPlugin {
    /// Creates the unregistered Agent Plugin.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
            runtime: None,
            runtime_service: None,
            workflow_adapter: None,
            workflow_service: None,
        }
    }
}

impl Plugin for AgentPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let portal = context.require::<CaptivePortal>("captive-portal")?;
        context.retain(
            portal
                .register(
                    WebEntry {
                        id: "agent",
                        title: "模型配置",
                        module: "entry.js",
                    },
                    ResourceFiles::from(context.filesystem()?.clone()),
                )
                .map_err(PluginError::registration)?,
        );
        let webserver = context.require::<WebServer>("webserver")?;
        let actions = context.require::<WorkflowActionRegistry>("workflow")?;
        let workflow_service = context.require::<WorkflowService>("workflow")?;
        let filesystem = context.filesystem()?.clone();
        let http_clients = self.http_clients.clone();
        let model_api_factory = ModelApiFactory::new(move || ModelApi::new(http_clients.clone()));
        let storage = RuntimeStorageConfig {
            persistence_root: PERSISTENCE_ROOT.into(),
            skill_roots: Vec::new(),
        };
        let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
            .map_err(PluginError::registration)?;
        let runtime = Rc::new(runtime);
        let workflow_adapter = AgentWorkflowAdapter::new(Rc::clone(&runtime));
        let action_registrations = workflow_adapter
            .register_actions(&actions)
            .map_err(PluginError::registration)?;

        context.retain(action_registrations);
        context.provide(Rc::clone(&runtime))?;
        context.provide(Rc::new(runtime.tool_registry()))?;
        let route_registration = webserver
            .serve_http(
                SET_API_PATH,
                model_api_http::SetApiEndpoint::new(Rc::clone(&runtime)),
            )
            .map_err(PluginError::registration)?;
        context.retain(route_registration);

        self.runtime = Some(runtime);
        self.runtime_service = Some(service);
        self.workflow_adapter = Some(workflow_adapter);
        self.workflow_service = Some(workflow_service);
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        self.runtime
            .as_ref()
            .ok_or_else(|| PluginError::registration(AgentRuntimeUnavailable))?
            .start_all()
            .map_err(PluginError::registration)?;
        let workflow_adapter = self
            .workflow_adapter
            .take()
            .ok_or_else(|| PluginError::registration(AgentRuntimeUnavailable))?;
        let runtime_service = self
            .runtime_service
            .take()
            .ok_or_else(|| PluginError::registration(AgentRuntimeUnavailable))?;
        let workflow_service = self
            .workflow_service
            .take()
            .ok_or_else(|| PluginError::registration(AgentRuntimeUnavailable))?;
        let cancellation = context.task_token();
        let filesystem = context.filesystem()?.clone();
        context
            .task_spawner()?
            .spawn(agent_task(
                runtime_service,
                workflow_adapter,
                workflow_service,
                filesystem,
                cancellation,
            ))
            .map_err(PluginError::registration)
    }
}

#[embassy_executor::task]
async fn agent_task(
    runtime_service: RuntimeService,
    workflow_adapter: AgentWorkflowAdapter,
    workflow_service: Rc<WorkflowService>,
    filesystem: barracuda_vfs::ScopedVfs,
    cancellation: PluginTaskToken,
) {
    let running = async move {
        if let Err(error) = bundled_workflows::load(&filesystem, &workflow_service).await {
            log::error!("failed to load Agent bundled workflows: {error}");
            return;
        }
        let runtime = async move {
            runtime_service.await;
            log::info!("Agent runtime service stopped");
        };
        let events = async move {
            if let Err(error) = workflow_adapter
                .forward_session_events(workflow_service)
                .await
            {
                log::error!("Agent session Event forwarding stopped: {error}");
            }
        };
        let _completed = select(runtime, events).await;
    };
    let _completed = select(cancellation.cancelled(), running).await;
    log::info!("stopped Agent runtime task");
}

#[derive(Debug, thiserror::Error)]
#[error("Agent runtime was not prepared during Plugin registration")]
struct AgentRuntimeUnavailable;
