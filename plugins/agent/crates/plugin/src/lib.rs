//! Plugin that owns the Agent runtime and exposes it to Workflow.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;

mod bundled_workflows;
mod model_api_http;
mod workflow;

use barracuda_agent_runtime::{ModelApiFactory, RuntimeService, RuntimeStorageConfig};
use barracuda_captive_portal_plugin::{CaptivePortal, ResourceFiles, WebEntry, WebGroup, WebText};
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
/// Skills a user or another Plugin installs. The shared durable Workspace
/// keeps them reachable by every Plugin that writes files.
const USER_SKILLS_ROOT: &str = "/workspace/media/skills";
const BUNDLED_SKILLS_ROOT: &str = "/workspace/resources/skills";

fn runtime_storage_config() -> RuntimeStorageConfig {
    RuntimeStorageConfig {
        persistence_root: PERSISTENCE_ROOT.into(),
        skill_roots: [USER_SKILLS_ROOT, BUNDLED_SKILLS_ROOT]
            .into_iter()
            .map(Into::into)
            .collect(),
    }
}

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
                        group: WebGroup::Agent,
                        order: 10,
                        title: WebText {
                            zh: "模型配置",
                            en: "Models",
                        },
                        summary: WebText {
                            zh: "为主 Agent、子 Agent、记忆与压缩注册模型",
                            en: "Register models for the main agent, sub-agents, memory and compaction",
                        },
                        icon: Some("icon.svg"),
                        figure: Some("figure.js"),
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
        let storage = runtime_storage_config();
        let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
            .map_err(PluginError::registration)?;
        let runtime = Rc::new(runtime);
        let api_configuration =
            embassy_futures::block_on(model_api_http::load_configuration(context.storage()))?;
        runtime.replace_api_configuration(api_configuration.clone());
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
                model_api_http::SetApiEndpoint::new(
                    Rc::clone(&runtime),
                    context.storage().clone(),
                    api_configuration,
                ),
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
        let task = agent_task(
            runtime_service,
            workflow_adapter,
            workflow_service,
            filesystem,
            cancellation,
        )
        .map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
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

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::runtime_storage_config;

    #[test]
    fn runtime_scans_bundled_and_user_skill_roots_without_selection_priority() {
        let mut roots = runtime_storage_config().skill_roots;
        roots.sort();
        assert_eq!(
            roots,
            [
                String::from("/workspace/media/skills"),
                String::from("/workspace/resources/skills"),
            ]
        );
    }
}
