//! `no_std` aggregation of Barracuda's fixed Plugin set.
//!
//! The selected-target crate supplies initialized capabilities. System consumes
//! those handles, constructs the fixed Plugin set, and owns their shared runtime
//! services.

#![no_std]

extern crate alloc;

mod resources;

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_agent_plugin::AgentPlugin;
use barracuda_captive_portal_plugin::CaptivePortalPlugin;
use barracuda_event_router::{EventRouter, EventRouterCreateError, RouterError, RpcLaneStorage};
use barracuda_gateway_agent_plugin::GatewayAgentPlugin;
use barracuda_imessage_gateway_plugin::IMessageGatewayPlugin;
use barracuda_imessage_web_plugin::IMessageWebPlugin;
use barracuda_platform::{Partitions, PlatformResources};
use barracuda_plugin_manager::{
    PluginManager, PluginManagerInitError, PluginRegisterError, PluginStartError,
};
use barracuda_scheduler_plugin::SchedulerPlugin;
use barracuda_target_api::TargetResources;
use barracuda_time_plugin::TimePlugin;
use barracuda_vfs::{global_namespace, mount, FsError, MountOptions};
use barracuda_vfs_littlefs::mount_or_format_partition;
use barracuda_vm_plugin::VmPlugin;
use barracuda_webserver_plugin::WebServerPlugin;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embedded_storage::nor_flash::NorFlash;

pub use resources::SystemResourceError;

/// Fully assembled portable Barracuda system.
///
/// Platform owns the executor-facing runners and concrete implementations;
/// System owns the portable handles and fixed Plugin graph.
pub struct System<
    const N: usize,
    const M: usize,
    const Q: usize,
    Region: NorFlash + Send + 'static,
    BoardHal,
    const P: usize,
> {
    router: EventRouter<N, M, Q>,
    _plugins: PluginManager<M, BlockingAsync<Region>>,
    _web_assets: Region,
    _remaining_partitions: Partitions<Region, P>,
    _board_hal: BoardHal,
}

/// Failure while constructing the System or registering its fixed Plugins.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SystemCreateError {
    /// Selected Target resources cannot be assigned to required System roles.
    #[error(transparent)]
    Resources(#[from] SystemResourceError),
    /// The writable filesystem partition could not be mounted as LittleFS.
    #[error(transparent)]
    Filesystem(#[from] FsError),
    /// Event Router initialization failed.
    #[error(transparent)]
    Router(#[from] EventRouterCreateError),
    /// A Plugin failed during registration.
    #[error(transparent)]
    Plugin(#[from] PluginRegisterError),
    /// A registered Plugin failed to start.
    #[error(transparent)]
    PluginStart(#[from] PluginStartError),
    /// Plugin persistence could not be opened from its validated partition.
    #[error(transparent)]
    PluginManager(#[from] PluginManagerInitError),
}

impl<const N: usize, const M: usize, const Q: usize, Region, BoardHal, const P: usize>
    System<N, M, Q, Region, BoardHal, P>
where
    Region: NorFlash + Send + Unpin + 'static,
    Region::Error: core::fmt::Debug,
    BoardHal: Unpin,
{
    /// Constructs, registers, and starts the fixed Plugin set.
    ///
    /// The caller supplies capabilities from the selected-target resource
    /// factory and the current Embassy executor spawner. System consumes them,
    /// owns the complete Plugin registration order, and exposes the spawner
    /// only to Plugin startup hooks.
    ///
    /// # Errors
    ///
    /// Returns [`SystemCreateError`] when Event Router initialization or Plugin
    /// registration or startup fails.
    pub async fn new(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        resources: TargetResources<PlatformResources<Partitions<Region, P>>, BoardHal>,
        spawner: Spawner,
    ) -> Result<Self, SystemCreateError> {
        let prepared = resources::prepare(resources)?;
        let backend = mount_or_format_partition(prepared.filesystem)?;
        mount("/", backend, MountOptions::read_write()).await?;
        let mut router = EventRouter::new(lanes).await?;
        let mut plugins = PluginManager::open(BlockingAsync::new(prepared.database)).await?;
        plugins.install_vfs(global_namespace().await);
        plugins.install_task_spawner(spawner);

        plugins.register(&mut router, WebServerPlugin::new(prepared.ip_stack))?;
        plugins.register(&mut router, VmPlugin::default())?;
        plugins.register(&mut router, TimePlugin::new(prepared.ip_stack))?;
        plugins.register(&mut router, SchedulerPlugin)?;
        plugins.register(&mut router, AgentPlugin::new(prepared.ip_stack))?;
        plugins.register(&mut router, CaptivePortalPlugin::new())?;
        plugins.register(&mut router, IMessageGatewayPlugin::new())?;
        plugins.register(&mut router, IMessageWebPlugin::new())?;
        plugins.register(&mut router, GatewayAgentPlugin::new())?;
        plugins.start(&mut router)?;

        Ok(Self {
            router,
            _plugins: plugins,
            _web_assets: prepared.web_assets,
            _remaining_partitions: prepared.partitions,
            _board_hal: prepared.board_hal,
        })
    }
}

impl<const N: usize, const M: usize, const Q: usize, Region, BoardHal, const P: usize> Future
    for System<N, M, Q, Region, BoardHal, P>
where
    Region: NorFlash + Send + Unpin + 'static,
    Region::Error: core::fmt::Debug,
    BoardHal: Unpin,
{
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}
