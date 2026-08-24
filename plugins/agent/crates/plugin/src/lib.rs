//! Plugin that owns the Agent Component.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use barracuda_agent_component::component::AgentComponent;
use barracuda_agent_runtime::{AgentRuntime, RuntimeStorageConfig};
use barracuda_model_api::ModelApi;
use barracuda_plugin_manager::{
    Plugin, PluginContext, PluginError, PluginFilesystem, PluginRequirements, PluginResult,
};
use embassy_net::dns::DnsSocket;
use embassy_net::tcp::client::{TcpClient, TcpClientState};
use embassy_net::Stack;
use static_cell::StaticCell;

pub use barracuda_agent_runtime::{ApiPurpose, ModelApiConfig, ModelApiFactory};
pub use barracuda_model_api::{BackendKind, InitError};

const PERSISTENCE_ROOT: &str = "/";
const HTTP_HEADER_BYTES: usize = 16 * 1024;
const HTTP_READ_BYTES: usize = 8 * 1024;
const HTTP_CONNECTIONS: usize = 4;
const HTTP_TX_BYTES: usize = 4 * 1024;
const HTTP_RX_BYTES: usize = 4 * 1024;

type AgentTcpClient = TcpClient<'static, HTTP_CONNECTIONS, HTTP_TX_BYTES, HTTP_RX_BYTES>;
type AgentDnsResolver = DnsSocket<'static>;

static HTTP_TCP_STATE: StaticCell<TcpClientState<HTTP_CONNECTIONS, HTTP_TX_BYTES, HTTP_RX_BYTES>> =
    StaticCell::new();
static HTTP_TCP_CLIENT: StaticCell<AgentTcpClient> = StaticCell::new();
static HTTP_DNS_RESOLVER: StaticCell<AgentDnsResolver> = StaticCell::new();
/// Stable identity of the Agent Plugin.
pub const PLUGIN_ID: &str = "agent";

type SetApiHandler = dyn Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError>;

/// Typed capability for configuring the Agent's model APIs.
pub struct AgentSetApi {
    handler: Box<SetApiHandler>,
}

impl AgentSetApi {
    /// Creates a model API configuration capability around one synchronous handler.
    #[must_use]
    pub fn new(
        handler: impl Fn(ModelApiConfig, ApiPurpose, bool) -> Result<(), InitError> + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// Applies one model API configuration.
    ///
    /// # Errors
    ///
    /// Returns [`InitError`] when the configuration is invalid.
    pub fn set_api(
        &self,
        api: ModelApiConfig,
        purpose: ApiPurpose,
        default: bool,
    ) -> Result<(), InitError> {
        (self.handler)(api, purpose, default)
    }
}

/// Plugin that constructs and owns the Agent runtime and Component.
pub struct AgentPlugin {
    stack: Stack<'static>,
}

impl AgentPlugin {
    /// Creates the Plugin with its IP construction input.
    #[must_use]
    pub const fn new(stack: Stack<'static>) -> Self {
        Self { stack }
    }
}

impl<const M: usize> Plugin<M> for AgentPlugin {
    const REQUIREMENTS: PluginRequirements =
        PluginRequirements::new().with_filesystem(PluginFilesystem::Private);

    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register<Storage>(&mut self, context: &mut PluginContext<'_, M, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let filesystem = context.filesystem()?.clone();
        let tcp_state = HTTP_TCP_STATE
            .try_init(TcpClientState::new())
            .ok_or_else(|| PluginError::registration(AgentNetworkAlreadyInitialized))?;
        let tcp: &'static AgentTcpClient = HTTP_TCP_CLIENT
            .try_init(TcpClient::new(self.stack, tcp_state))
            .ok_or_else(|| PluginError::registration(AgentNetworkAlreadyInitialized))?;
        let resolver: &'static AgentDnsResolver = HTTP_DNS_RESOLVER
            .try_init(DnsSocket::new(self.stack))
            .ok_or_else(|| PluginError::registration(AgentNetworkAlreadyInitialized))?;
        #[cfg(feature = "mbedtls-host")]
        let host_tls = barracuda_model_api::HostTls::from_system_certificates()
            .map_err(PluginError::registration)?;
        #[cfg(feature = "mbedtls-host")]
        let model_api_factory = ModelApiFactory::new(move || {
            ModelApi::new_with_tls(
                tcp,
                resolver,
                host_tls.config(),
                HTTP_HEADER_BYTES,
                HTTP_READ_BYTES,
            )
        });
        #[cfg(not(feature = "mbedtls-host"))]
        let model_api_factory = ModelApiFactory::new(move || {
            ModelApi::new(tcp, resolver, HTTP_HEADER_BYTES, HTTP_READ_BYTES)
        });
        let storage = RuntimeStorageConfig {
            persistence_root: PERSISTENCE_ROOT.into(),
            skill_roots: Vec::new(),
        };
        let (runtime, service) = AgentRuntime::new(filesystem, storage, model_api_factory)
            .map_err(PluginError::registration)?;
        runtime.start_all().map_err(PluginError::registration)?;
        let runtime = Rc::new(runtime);
        let set_api_runtime = Rc::clone(&runtime);
        context.provide(Rc::new(AgentSetApi::new(move |api, purpose, default| {
            set_api_runtime.set_api(api, purpose, default)
        })))?;
        context
            .event_router
            .load(AgentComponent::from_shared(runtime, service))?;
        Ok(())
    }
}

#[derive(Debug)]
struct AgentNetworkAlreadyInitialized;

impl core::fmt::Display for AgentNetworkAlreadyInitialized {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Agent HTTP resources are already initialized")
    }
}

impl core::error::Error for AgentNetworkAlreadyInitialized {}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use barracuda_event_router::{EventRouter, RpcLaneStorage};
    use barracuda_platform_test::{
        install_global_memory_vfs, memory_partition, memory_vfs_root, never_embassy_stack,
    };
    use barracuda_plugin_manager::{Plugin, PluginId, PluginManager};
    use futures_lite::future::block_on;

    use super::AgentPlugin;

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

            let plugin = AgentPlugin::new(never_embassy_stack());
            assert_eq!(Plugin::<512>::id(&plugin), "agent");

            manager
                .register(&mut router, plugin)
                .expect("register Agent Plugin");
            manager.start(&mut router).expect("start Plugins");

            assert_eq!(manager.component_ids(&id).map(<[_]>::len), Some(1));
        });
    }
}
