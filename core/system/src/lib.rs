//! `no_std` aggregation of Barracuda's fixed Plugin set.
//!
//! The outer Platform/Board entry supplies low-level capabilities. The System
//! constructs, registers, and starts the fixed Plugin set and owns their shared
//! runtime services.

#![no_std]

extern crate alloc;

use alloc::string::String;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_plugin::{AgentPlugin, ModelApiFactory, RuntimeStorageConfig};
use barracuda_event_router::{
    EventRouter, EventRouterCreateError, FileSystem, RouterError, RpcLaneStorage,
};
use barracuda_gateway_agent_plugin::GatewayAgentPlugin;
use barracuda_message_gateway_plugin::MessageGatewayPlugin;
use barracuda_net::{Dns, TcpConnect, UdpStack};
use barracuda_plugin_manager::{
    EkvFlash, EkvStore, PluginManager, PluginRegisterError, PluginStartError, RawMutex,
};
use barracuda_scheduler_plugin::SchedulerPlugin;
use barracuda_time_plugin::TimePlugin;
use barracuda_vm_plugin::VmPlugin;
use barracuda_webserver_plugin::{WebServerListener, WebServerPlugin};

/// Fully assembled portable Barracuda system.
///
/// The outer entry owns the executor, listeners, sockets, timers, and concrete
/// capability implementations used to drive this System.
pub struct System<const N: usize, const M: usize, const Q: usize> {
    router: EventRouter<N, M, Q>,
    _plugins: PluginManager<M>,
}

/// Failure while constructing the System or registering its fixed Plugins.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SystemCreateError {
    /// Event Router initialization failed.
    #[error(transparent)]
    Router(#[from] EventRouterCreateError),
    /// A Plugin failed during registration.
    #[error(transparent)]
    Plugin(#[from] PluginRegisterError),
    /// A registered Plugin failed to start.
    #[error(transparent)]
    PluginStart(#[from] PluginStartError),
}

impl<const N: usize, const M: usize, const Q: usize> System<N, M, Q> {
    /// Constructs, registers, and starts the fixed Plugin set.
    ///
    /// The caller supplies platform capabilities and configuration. The System
    /// constructs the fixed Plugin set and owns the complete registration
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`SystemCreateError`] when Event Router initialization or Plugin
    /// registration or startup fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn new<
        WorkflowFilesystem,
        WebServerNetwork,
        TimeNetwork,
        AgentFilesystem,
        Http,
        Flash,
        Mutex,
    >(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        workflow_filesystem: &'static WorkflowFilesystem,
        workflow_directory: impl Into<String>,
        plugin_store: EkvStore<Flash, Mutex>,
        webserver_network: WebServerNetwork,
        time_network: TimeNetwork,
        agent_filesystem: AgentFilesystem,
        agent_storage: RuntimeStorageConfig,
        model_api_factory: ModelApiFactory<Http>,
    ) -> Result<Self, SystemCreateError>
    where
        WorkflowFilesystem: FileSystem,
        WebServerNetwork: Clone + WebServerListener + 'static,
        TimeNetwork: Dns + UdpStack + 'static,
        AgentFilesystem: FileSystem + 'static,
        Http: TcpConnect + Dns + 'static,
        Flash: EkvFlash + 'static,
        Mutex: RawMutex + 'static,
    {
        let mut router = EventRouter::new(lanes, workflow_filesystem, workflow_directory)?;
        let mut plugins = PluginManager::new(plugin_store);

        plugins
            .register(&mut router, WebServerPlugin::new(webserver_network))
            .await?;
        plugins.register(&mut router, VmPlugin).await?;
        plugins
            .register(&mut router, TimePlugin::new(time_network))
            .await?;
        plugins.register(&mut router, SchedulerPlugin).await?;
        plugins
            .register(
                &mut router,
                AgentPlugin::new(agent_filesystem, agent_storage, model_api_factory),
            )
            .await?;
        plugins
            .register(&mut router, MessageGatewayPlugin::new())
            .await?;
        plugins
            .register(&mut router, GatewayAgentPlugin::new())
            .await?;
        plugins.start(&mut router).await?;

        Ok(Self {
            router,
            _plugins: plugins,
        })
    }
}

impl<const N: usize, const M: usize, const Q: usize> Future for System<N, M, Q> {
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use barracuda_agent_plugin::{ModelApiFactory, RuntimeStorageConfig};
    use barracuda_event_router::{MemFs, RpcLaneStorage};
    use barracuda_model_api::ModelApi;
    use barracuda_net::testing::NeverStack;
    use barracuda_plugin_manager::{EkvConfig, EkvStore, NoopRawMutex, PluginId};
    use barracuda_webserver_plugin::{WebServer, WebServerListenFuture, WebServerListener};
    use core::convert::Infallible;
    use core::future::pending;
    use ekv::flash::MemFlash;
    use futures_lite::future::block_on;

    use super::System;

    static NETWORK: NeverStack = NeverStack;

    #[derive(Clone, Copy)]
    struct NeverWebServerListener;

    impl WebServerListener for NeverWebServerListener {
        type Error = Infallible;

        fn listen<'a>(
            &'a mut self,
            _server: &'a WebServer,
            _port: u16,
        ) -> WebServerListenFuture<'a, Self::Error> {
            Box::pin(pending())
        }
    }

    #[test]
    fn system_registers_fixed_plugin_set() {
        let lanes = Box::leak(Box::new(RpcLaneStorage::<32, 512, 8>::new()));
        let workflow_filesystem = Box::leak(Box::new(MemFs::new()));
        let model_api_factory = ModelApiFactory::new(|| ModelApi::new(&NETWORK, 1024, 1024));
        let plugin_store =
            EkvStore::<MemFlash, NoopRawMutex>::new(MemFlash::new(), EkvConfig::default());
        block_on(plugin_store.format()).expect("format Plugin store");

        let system = block_on(System::new(
            lanes,
            workflow_filesystem,
            "/workflows",
            plugin_store,
            NeverWebServerListener,
            NeverStack,
            MemFs::new(),
            RuntimeStorageConfig {
                persistence_root: "/agent".into(),
                skill_roots: Vec::new(),
            },
            model_api_factory,
        ));

        let system = system.expect("create System");
        assert!(system
            ._plugins
            .is_loaded(&PluginId::try_from("time").expect("valid Time Plugin ID")));
        assert!(system
            ._plugins
            .is_loaded(&PluginId::try_from("scheduler").expect("valid Scheduler Plugin ID")));
    }
}
