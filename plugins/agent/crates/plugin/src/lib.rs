//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use alloc::vec::Vec;

mod model_api_http;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, ModelApiFactory, RuntimeStorageConfig};
use barracuda_model_api::ModelApi;
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginFilesystem, PluginRegisterContext, PluginRequirements, PluginResult,
    PluginStartContext,
};
use barracuda_webserver_plugin::WebServer;
use http_client::ClientFactory;

pub use barracuda_agent_runtime::{tools, AgentToolRegistry};
pub use model_api_http::SET_API_PATH;

const PERSISTENCE_ROOT: &str = "/";

/// Plugin that constructs and owns the Agent runtime and Component.
#[barracuda_plugin::macros::plugin]
pub struct AgentPlugin {
    http_clients: ClientFactory<'static>,
    runtime: Option<Rc<AgentRuntime>>,
}

impl AgentPlugin {
    /// Creates the Plugin with Platform HTTP resources.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            http_clients: context.http_clients.clone(),
            runtime: None,
        }
    }
}

impl<const M: usize> Plugin<M> for AgentPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let webserver = context.require::<WebServer>(
            <Self as barracuda_plugin::manager::PluginDeclaration>::DEPENDS_ON[0],
        )?;
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
        context.provide(Rc::new(runtime.tool_registry()))?;
        let registration = webserver
            .serve_http(
                SET_API_PATH,
                model_api_http::SetApiEndpoint::new(Rc::clone(&runtime)),
            )
            .map_err(PluginError::registration)?;
        context.retain(registration);
        context
            .event_router
            .load(AgentComponent::from_shared(Rc::clone(&runtime), service))?;
        self.runtime = Some(runtime);
        Ok(())
    }

    fn start<Storage>(&mut self, _context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        self.runtime
            .as_ref()
            .ok_or_else(|| PluginError::registration(AgentRuntimeUnavailable))?
            .start_all()
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Agent runtime was not prepared during Plugin registration")]
struct AgentRuntimeUnavailable;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::{boxed::Box, rc::Rc, string::ToString};
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, memory_vfs_root, never_embassy_stack,
    };
    use barracuda_plugin::api::PluginContext;
    use barracuda_plugin::manager::{
        Plugin, PluginDeclaration, PluginError, PluginId, PluginManager, PluginRegisterContext,
        PluginResult,
    };
    use barracuda_webserver_plugin::WebServer;
    use core::cell::RefCell;
    use futures_lite::future::block_on;

    use http_client::ClientFactory;

    use super::{tools, AgentPlugin, AgentToolRegistry};

    struct JoinedTool;

    impl tools::ToolSpec for JoinedTool {
        fn name(&self) -> &str {
            "joined"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"joined","parameters":{"type":"object","properties":{},"additionalProperties":false}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator = json_validator::validator_source!(
                r#"{"type":"object","properties":{},"additionalProperties":false}"#
            );
            &VALIDATOR
        }
    }

    impl tools::ToolHandler for JoinedTool {
        type Args = tools::EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> tools::ToolFuture<'a> {
            Box::pin(async {
                Ok(tools::ToolOutput {
                    content: "joined".into(),
                    ok: true,
                })
            })
        }
    }

    struct BackgroundTool;

    impl tools::ToolSpec for BackgroundTool {
        fn name(&self) -> &str {
            "background"
        }

        fn schema(&self) -> &str {
            r#"{"type":"function","function":{"name":"background","parameters":{"type":"object","properties":{},"additionalProperties":false}}}"#
        }

        fn arguments_validator(&self) -> &'static json_validator::Validator {
            const VALIDATOR: json_validator::Validator = json_validator::validator_source!(
                r#"{"type":"object","properties":{},"additionalProperties":false}"#
            );
            &VALIDATOR
        }
    }

    impl tools::DetachedToolHandler for BackgroundTool {
        type Args = tools::EmptyArgs;

        fn invoke<'a>(&'a self, _args: Self::Args) -> tools::DetachedToolFuture<'a> {
            Box::pin(async {
                Ok(tools::DetachedTool::new(
                    tools::ToolOutput {
                        content: "accepted".into(),
                        ok: true,
                    },
                    Box::pin(async {
                        Ok(tools::ToolOutput {
                            content: "completed".into(),
                            ok: true,
                        })
                    }),
                ))
            })
        }
    }

    struct WebServerProvider(Rc<WebServer>);

    impl PluginDeclaration for WebServerProvider {
        const ID: &'static str = "webserver";
    }

    impl Plugin<512> for WebServerProvider {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, 512, Storage>,
        ) -> PluginResult<()>
        where
            Storage: barracuda_plugin::manager::PluginStorage,
        {
            context.provide(Rc::clone(&self.0))?;
            Ok(())
        }
    }

    struct ToolConsumer {
        observed: Rc<RefCell<Option<Rc<AgentToolRegistry>>>>,
    }

    impl PluginDeclaration for ToolConsumer {
        const ID: &'static str = "tool-consumer";
        const DEPENDS_ON: &'static [&'static str] = &["agent"];
    }

    impl Plugin<512> for ToolConsumer {
        fn register<Storage>(
            &mut self,
            context: &mut PluginRegisterContext<'_, 512, Storage>,
        ) -> PluginResult<()>
        where
            Storage: barracuda_plugin::manager::PluginStorage,
        {
            let registry = context.require::<AgentToolRegistry>(Self::DEPENDS_ON[0])?;
            registry
                .register_group(tools::ToolGroup::new(
                    "consumer",
                    true,
                    [
                        tools::Tool::new(JoinedTool),
                        tools::Tool::from_detached(BackgroundTool),
                    ],
                ))
                .map_err(PluginError::registration)?;
            self.observed.replace(Some(registry));
            Ok(())
        }
    }

    #[test]
    fn plugin_loads_its_agent_component() {
        block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create database partition");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            manager.install_vfs(memory_vfs_root().await.expect("create System VFS"));
            install_global_memory_vfs()
                .await
                .expect("install global test VFS");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let id = PluginId::try_from("agent").expect("valid Plugin ID");

            manager
                .register(&mut router, WebServerProvider(Rc::new(WebServer::new())))
                .expect("register WebServer provider");

            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            let plugin = AgentPlugin::new(&mut context);
            assert_eq!(
                <AgentPlugin as barracuda_plugin::manager::PluginDeclaration>::ID,
                "agent"
            );

            manager
                .register(&mut router, plugin)
                .expect("register Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }

    #[test]
    fn plugin_provides_agent_tool_registry_to_a_dependent_plugin() {
        block_on(async {
            let partition = memory_partition(64 * 1024)
                .await
                .expect("create database partition");
            let mut manager = PluginManager::open(partition)
                .await
                .expect("open Plugin storage");
            manager.install_vfs(memory_vfs_root().await.expect("create System VFS"));
            install_global_memory_vfs()
                .await
                .expect("install global test VFS");
            let lanes = Box::leak(Box::new(RpcLaneStorage::<16, 512, 8>::new()));
            let mut router = EventRouter::new(lanes).await.expect("create router");
            let observed = Rc::new(RefCell::new(None));

            let stack = never_embassy_stack();
            let mut context = PluginContext::new(stack, ClientFactory::plaintext(stack));
            manager
                .add(ToolConsumer {
                    observed: Rc::clone(&observed),
                })
                .expect("queue Tool consumer");
            manager
                .add(AgentPlugin::new(&mut context))
                .expect("queue Agent Plugin");
            manager
                .add(WebServerProvider(Rc::new(WebServer::new())))
                .expect("queue WebServer provider");

            manager
                .register_all(&mut router)
                .expect("register Plugin graph");
            assert!(observed.borrow().is_some());

            manager.start(&mut router).expect("start Plugins");
            assert!(futures_lite::future::poll_once(&mut router).await.is_none());

            let duplicate = observed
                .borrow()
                .as_ref()
                .expect("Agent Tool Registry capability observed")
                .register_group(tools::ToolGroup::new(
                    "consumer",
                    true,
                    [tools::Tool::new(JoinedTool)],
                ));
            let error = duplicate.expect_err("group must exist in the loaded Tool Registry");
            assert!(matches!(
                error,
                barracuda_agent_runtime::RuntimeError::Tool(_)
            ));
            assert_eq!(error.to_string(), "tool group already exists: consumer");
        });
    }
}
