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

use barracuda_board_hal::{BoardHalResources, ConfigurableDigitalPin, ExposedIo, ResourceSet};
use barracuda_event_router::{EventRouter, EventRouterCreateError, RouterError, RpcLaneStorage};
use barracuda_platform::{Partitions, PlatformResources};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    PluginManager, PluginManagerInitError, PluginRegisterError, PluginStartError, PluginUnloadError,
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
    Builtins,
    Io,
    const P: usize,
> {
    router: EventRouter<N, M, Q>,
    plugins: PluginManager<M, BlockingAsync<Region>>,
    _web_assets: Region,
    _remaining_partitions: Partitions<Region, P>,
    _plugin_context: PluginContext<Builtins, Io>,
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

impl<const N: usize, const M: usize, const Q: usize, Region, Builtins, Io, const P: usize>
    System<N, M, Q, Region, Builtins, Io, P>
where
    Region: NorFlash + Send + Unpin + 'static,
    Region::Error: core::fmt::Debug,
    Builtins: Unpin,
    Io: ExposedIo + Unpin,
    Io::Gpio: ResourceSet + Send + 'static,
    <Io::Gpio as ResourceSet>::Resource: ConfigurableDigitalPin + Send,
    <<Io::Gpio as ResourceSet>::Resource as embedded_hal::digital::ErrorType>::Error:
        core::fmt::Debug,
    Io::I2c: ResourceSet + Send + 'static,
    <Io::I2c as ResourceSet>::Resource: embedded_hal_async::i2c::I2c + Send,
    <<Io::I2c as ResourceSet>::Resource as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
    Io::Spi: ResourceSet + Send + 'static,
    <Io::Spi as ResourceSet>::Resource: embedded_hal_async::spi::SpiBus + Send,
    <<Io::Spi as ResourceSet>::Resource as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
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
        resources: TargetResources<
            PlatformResources<Tls, Partitions<Region, P>>,
            BoardHalResources<Builtins, Io>,
        >,
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
        let mut plugin_context =
            PluginContext::from_hal(prepared.ip_stack, http_clients, prepared.board_hal);

        // BEGIN GENERATED PLUGINS
        register_plugins!(plugins, router;
            barracuda_agent_plugin::AgentPlugin::new(&mut plugin_context),
            barracuda_file_plugin::FilePlugin::new(&mut plugin_context),
            barracuda_gpio_plugin::GpioPlugin::new(&mut plugin_context),
            barracuda_http_plugin::HttpPlugin::new(&mut plugin_context),
            barracuda_i2c_plugin::I2cPlugin::new(&mut plugin_context),
            barracuda_imessage_bluebubble_plugin::IMessageBlueBubblePlugin::new(&mut plugin_context),
            barracuda_imessage_bridge_plugin::ImessageBridgePlugin::new(&mut plugin_context),
            barracuda_imessage_gateway_plugin::IMessageGatewayPlugin::new(&mut plugin_context),
            barracuda_imessage_inkbox_plugin::IMessageInkboxPlugin::new(&mut plugin_context),
            barracuda_imessage_qq_plugin::IMessageQQPlugin::new(&mut plugin_context),
            barracuda_imessage_telegram_plugin::IMessageTelegramPlugin::new(&mut plugin_context),
            barracuda_imessage_web_plugin::IMessageWebPlugin::new(&mut plugin_context),
            barracuda_imessage_wechat_plugin::IMessageWechatPlugin::new(&mut plugin_context),
            barracuda_message_queue_plugin::MessageQueuePlugin::new(&mut plugin_context),
            barracuda_scheduler_plugin::SchedulerPlugin::new(&mut plugin_context),
            barracuda_spi_plugin::SpiPlugin::new(&mut plugin_context),
            barracuda_time_plugin::TimePlugin::new(&mut plugin_context),
            barracuda_vm_plugin::VmPlugin::new(&mut plugin_context),
            barracuda_web_search_plugin::WebSearchPlugin::new(&mut plugin_context),
            barracuda_webserver_plugin::WebServerPlugin::new(&mut plugin_context),
            barracuda_workflow_plugin::WorkflowPlugin::new(&mut plugin_context),
        );
        // END GENERATED PLUGINS
        plugins.start(&mut router)?;
        log::info!("Barracuda System started");

        Ok(Self {
            router,
            plugins,
            _web_assets: prepared.partitions.web_assets,
            _remaining_partitions: prepared.partitions.remaining,
            _plugin_context: plugin_context,
        })
    }

    /// Stops every Plugin in reverse dependency order and waits for its tasks.
    ///
    /// Consuming the System prevents its Event Router from being polled again
    /// after shutdown. Remaining System-owned resources are released normally
    /// when this method returns.
    ///
    /// # Errors
    ///
    /// Returns the first Plugin unload failure.
    pub async fn shutdown(mut self) -> Result<(), PluginUnloadError> {
        self.plugins.shutdown(&mut self.router).await
    }
}

impl<const N: usize, const M: usize, const Q: usize, Region, Builtins, Io, const P: usize> Future
    for System<N, M, Q, Region, Builtins, Io, P>
where
    Region: NorFlash + Send + Unpin + 'static,
    Region::Error: core::fmt::Debug,
    Builtins: Unpin,
    Io: Unpin,
{
    type Output = Result<(), RouterError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().router).poll(context)
    }
}
