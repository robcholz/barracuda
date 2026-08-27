//! `no_std` aggregation of Barracuda's fixed Plugin set.
//!
//! The selected-target crate supplies initialized capabilities. System consumes
//! those handles, constructs the fixed Plugin set, and owns their shared runtime
//! services. Its build script watches `plugins/` and rejects stale generated
//! registry blocks before compiling System.

#![no_std]

extern crate alloc;

mod resources;

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use barracuda_event_router::{EventRouter, EventRouterCreateError, RouterError, RpcLaneStorage};
use barracuda_platform::{Partitions, PlatformResources};
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{
    PluginManager, PluginManagerInitError, PluginRegisterError, PluginStartError,
};
use barracuda_target_api::TargetResources;
use barracuda_tls::ClientTls;
use barracuda_vfs::{global_namespace, mount, FsError, MountOptions};
use barracuda_vfs_littlefs::mount_or_format_partition;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embedded_storage::nor_flash::NorFlash;

macro_rules! register_plugins {
    ($manager:ident, $router:ident; $($plugin:expr),* $(,)?) => {
        $(
            $manager.add($plugin)?;
        )*
        $manager.register_all(&mut $router)?;
    };
}

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
    /// The writable System partition could not be mounted as LittleFS.
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
    pub async fn new<Tls: ClientTls>(
        lanes: &'static RpcLaneStorage<N, M, Q>,
        resources: TargetResources<PlatformResources<Tls, Partitions<Region, P>>, BoardHal>,
        spawner: Spawner,
    ) -> Result<Self, SystemCreateError> {
        log::info!("assembling Barracuda System");
        let prepared = resources::prepare(resources)?;
        log::info!("assigned selected Target resources to System roles");
        let backend = mount_or_format_partition(prepared.partitions.system)?;
        mount("/", backend, MountOptions::read_write()).await?;
        log::info!("mounted System filesystem");
        let mut router = EventRouter::new(lanes).await?;
        log::info!("initialized Event Router");
        let mut plugins =
            PluginManager::open(BlockingAsync::new(prepared.partitions.kv_database)).await?;
        log::info!("opened Plugin Manager storage");
        plugins.install_vfs(global_namespace().await);
        plugins.install_task_spawner(spawner);

        let http_clients =
            http_client::ClientFactory::new(prepared.ip_stack, move || prepared.tls.config());
        let plugin_context = PluginContext::new(prepared.ip_stack, http_clients);

        // BEGIN GENERATED PLUGINS
        register_plugins!(plugins, router;
            barracuda_agent_plugin::AgentPlugin::new(&plugin_context),
            barracuda_captive_portal_plugin::CaptivePortalPlugin::new(&plugin_context),
            barracuda_file_plugin::FilePlugin::new(&plugin_context),
            barracuda_gateway_agent_plugin::GatewayAgentPlugin::new(&plugin_context),
            barracuda_http_plugin::HttpPlugin::new(&plugin_context),
            barracuda_imessage_bluebubble_plugin::IMessageBlueBubblePlugin::new(&plugin_context),
            barracuda_imessage_gateway_plugin::IMessageGatewayPlugin::new(&plugin_context),
            barracuda_imessage_inkbox_plugin::IMessageInkboxPlugin::new(&plugin_context),
            barracuda_imessage_qq_plugin::IMessageQQPlugin::new(&plugin_context),
            barracuda_imessage_telegram_plugin::IMessageTelegramPlugin::new(&plugin_context),
            barracuda_imessage_web_plugin::IMessageWebPlugin::new(&plugin_context),
            barracuda_imessage_wechat_plugin::IMessageWechatPlugin::new(&plugin_context),
            barracuda_scheduler_plugin::SchedulerPlugin::new(&plugin_context),
            barracuda_tavily_plugin::TavilyPlugin::new(&plugin_context),
            barracuda_time_plugin::TimePlugin::new(&plugin_context),
            barracuda_vm_plugin::VmPlugin::new(&plugin_context),
            barracuda_webserver_plugin::WebServerPlugin::new(&plugin_context),
        );
        // END GENERATED PLUGINS
        plugins.start(&mut router)?;
        log::info!("Barracuda System started");

        Ok(Self {
            router,
            _plugins: plugins,
            _web_assets: prepared.partitions.web_assets,
            _remaining_partitions: prepared.partitions.remaining,
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
