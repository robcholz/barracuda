//! `no_std` aggregation of Barracuda's fixed Plugin set.
//!
//! The selected-target crate supplies initialized capabilities. System consumes
//! those handles, constructs the fixed Plugin set, and owns their shared runtime
//! services.

#![no_std]

extern crate alloc;

use alloc::{rc::Rc, string::String};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_plugin::{AgentPlugin, ModelApiFactory};
use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_event_router::{EventRouter, EventRouterCreateError, RouterError, RpcLaneStorage};
use barracuda_gateway_agent_plugin::GatewayAgentPlugin;
use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
use barracuda_imessage_web_plugin::IMessageWebPlugin;
use barracuda_net::{Dns, TcpConnect, UdpStack};
use barracuda_platform::PlatformResources;
use barracuda_plugin_manager::{
    CapabilityError, PluginManager, PluginManagerInitError, PluginRegisterError, PluginStartError,
};
use barracuda_scheduler_plugin::SchedulerPlugin;
use barracuda_time_plugin::TimePlugin;
use barracuda_vm_plugin::VmPlugin;
use barracuda_webserver_plugin::{WebServerListener, WebServerPlugin};
use embedded_storage_async::nor_flash::NorFlash;

/// Fully assembled portable Barracuda system.
///
/// Platform owns the executor-facing runners and concrete implementations;
/// System owns the portable handles and fixed Plugin graph.
pub struct System<
    const N: usize,
    const M: usize,
    const Q: usize,
    DatabaseRegion: NorFlash + 'static,
> {
    router: EventRouter<N, M, Q>,
    _plugins: PluginManager<M, DatabaseRegion>,
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
    /// A required System capability could not be installed.
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    /// Plugin persistence could not be opened from its validated partition.
    #[error(transparent)]
    PluginManager(#[from] PluginManagerInitError),
}

impl<const N: usize, const M: usize, const Q: usize, DatabaseRegion> System<N, M, Q, DatabaseRegion>
where
    DatabaseRegion: NorFlash + 'static,
    DatabaseRegion::Error: core::fmt::Debug,
{
    /// Constructs, registers, and starts the fixed Plugin set.
    ///
    /// The caller supplies capabilities from the selected-target resource
    /// factory. System consumes them and owns the complete Plugin registration
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`SystemCreateError`] when Event Router initialization or Plugin
    /// registration or startup fails.
    pub async fn new<Filesystem, Network>(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        resources: PlatformResources<Network, Filesystem, DatabaseRegion, ModelApiFactory<Network>>,
        workflow_directory: impl Into<String>,
    ) -> Result<Self, SystemCreateError>
    where
        Filesystem: barracuda_event_router::FileSystem,
        Network: Clone + WebServerListener + TcpConnect + Dns + UdpStack + 'static,
    {
        let PlatformResources {
            network,
            filesystem,
            database_region,
            model_api_factory,
        } = resources;
        let mut router = EventRouter::new(lanes, filesystem.clone(), workflow_directory)?;
        let mut plugins = PluginManager::open(database_region).await?;
        plugins.provide_system(Rc::new(filesystem))?;
        plugins.provide_system(Rc::new(network))?;
        plugins.provide_system(Rc::new(model_api_factory))?;

        plugins
            .register(&mut router, WebServerPlugin::<Network>::default())
            .await?;
        plugins.register(&mut router, VmPlugin::default()).await?;
        plugins
            .register(&mut router, TimePlugin::<Network>::default())
            .await?;
        plugins.register(&mut router, SchedulerPlugin).await?;
        plugins
            .register(&mut router, AgentPlugin::<Filesystem, Network>::default())
            .await?;
        plugins
            .register(&mut router, CaptivePortalPlugin::new())
            .await?;
        plugins
            .register(&mut router, IMessageGatewayPlugin::new())
            .await?;
        plugins
            .register(&mut router, IMessageWebPlugin::new())
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

impl<const N: usize, const M: usize, const Q: usize, DatabaseRegion> Future
    for System<N, M, Q, DatabaseRegion>
where
    DatabaseRegion: NorFlash + 'static,
    DatabaseRegion::Error: core::fmt::Debug,
{
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}
