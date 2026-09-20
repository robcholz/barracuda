//! `no_std` aggregation of Barracuda's fixed Plugin set.
//!
//! The selected-target crate supplies initialized resources. System consumes
//! those handles, constructs the fixed Plugin set, and owns their shared runtime
//! services. Its build script watches `plugins/` and rejects stale generated
//! registry blocks before compiling System.

#![no_std]

extern crate alloc;

mod read_only_flash;
mod resources;

use alloc::boxed::Box;
use alloc::format;
use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;

use barracuda_board_hal::{
    audio::AudioCodecPeripheral,
    camera::CameraPeripheral,
    display::DisplayPeripheral,
    imu::ImuPeripheral,
    led_strip::LedStripPeripheral,
    power::PowerMonitorPeripheral,
    real_time_clock::RealTimeClockPeripheral,
    removable_storage::{RemovableStorage, RemovableStorageEvent, RemovableStoragePeripheral},
    touch::TouchPeripheral,
    AnalogProvider, BoardResources, DigitalProvider, ExposedIo, I2cProvider, I2sProvider,
    PwmProvider, SpiProvider, UartProvider,
};
use barracuda_platform::{Partitions, PlatformResources, WifiDevice};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    PluginManager, PluginManagerInitError, PluginRegisterError, PluginStartError, PluginUnloadError,
};
use barracuda_target_api::{TargetIdentity, TargetResources};
use barracuda_tls::ClientTls;
use barracuda_vfs::{
    create_dir_all, detach, global_namespace, mount, mount_scoped, unmount, FsError, MountOptions,
};
use barracuda_vfs_littlefs::mount_or_format_partition;
use barracuda_vfs_memfs::MemFs;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_storage::nor_flash::NorFlash;
use read_only_flash::mount_resources_partition;

macro_rules! register_plugins {
    ($manager:ident; $($plugin:expr),* $(,)?) => {
        $(
            $manager.add($plugin)?;
        )*
        $manager.register_all()?;
    };
}

type BoxedSystemTask = Pin<Box<dyn Future<Output = ()> + 'static>>;

struct RemovableStorageTaskControl {
    cancel: Signal<CriticalSectionRawMutex, ()>,
    stopped: Signal<CriticalSectionRawMutex, ()>,
}

impl RemovableStorageTaskControl {
    const fn new() -> Self {
        Self {
            cancel: Signal::new(),
            stopped: Signal::new(),
        }
    }
}

struct RemovableStorageTask {
    control: Arc<RemovableStorageTaskControl>,
}

impl RemovableStorageTask {
    async fn shutdown(self) {
        self.control.cancel.signal(());
        self.control.stopped.wait().await;
    }
}

impl Drop for RemovableStorageTask {
    fn drop(&mut self) {
        self.control.cancel.signal(());
    }
}

#[embassy_executor::task]
async fn run_boxed_system_task(task: BoxedSystemTask) {
    task.await;
}

async fn manage_removable_storage<Storage>(
    mut storage: Storage,
    control: Arc<RemovableStorageTaskControl>,
) where
    Storage: RemovableStorage,
{
    let mount_point = format!("/removable/{}", storage.slot_id());
    let mut mounted = false;
    loop {
        match select(storage.next_event(), control.cancel.wait()).await {
            Either::First(RemovableStorageEvent::Mounted {
                generation,
                filesystem,
            }) => match mount(&mount_point, filesystem, MountOptions::read_write()).await {
                Ok(()) => {
                    mounted = true;
                    log::info!(
                        "mounted removable filesystem {} generation {}",
                        mount_point,
                        generation
                    );
                }
                Err(error) => {
                    log::error!("failed to mount removable filesystem {mount_point}: {error}");
                }
            },
            Either::First(RemovableStorageEvent::Removed { generation }) => {
                if mounted {
                    match detach(&mount_point).await {
                        Ok(()) | Err(FsError::NotMounted) => {}
                        Err(error) => {
                            log::error!(
                                "failed to detach removable filesystem {mount_point}: {error}"
                            );
                        }
                    }
                    mounted = false;
                }
                log::info!(
                    "removed removable filesystem {} generation {}",
                    mount_point,
                    generation
                );
            }
            Either::Second(()) => break,
        }
    }
    if mounted {
        match unmount(&mount_point).await {
            Ok(()) | Err(FsError::NotMounted) => {}
            Err(FsError::Busy) => {
                let _ignored = detach(&mount_point).await;
            }
            Err(error) => log::error!(
                "failed to unmount removable filesystem {mount_point} during shutdown: {error}"
            ),
        }
    }
    control.stopped.signal(());
}

fn start_removable_storage_task<Storage>(
    storage: Storage,
    spawner: Spawner,
) -> Result<RemovableStorageTask, embassy_executor::SpawnError>
where
    Storage: RemovableStorage,
{
    let control = Arc::new(RemovableStorageTaskControl::new());
    let future = Box::pin(manage_removable_storage(storage, Arc::clone(&control)));
    let token = run_boxed_system_task(future)?;
    spawner.spawn(token);
    Ok(RemovableStorageTask { control })
}

pub use resources::SystemResourceError;

/// Fully assembled portable Barracuda system.
///
/// Platform owns the executor-facing runners and concrete implementations;
/// System owns the portable handles and fixed Plugin graph.
pub struct System<Region: NorFlash + Send + 'static, Peripherals, Io, const P: usize> {
    plugins: PluginManager<BlockingAsync<Region>>,
    removable_storage_task: Option<RemovableStorageTask>,
    _remaining_partitions: Partitions<Region, P>,
    _plugin_context: PluginContext<Peripherals, Io>,
}

/// Failure while constructing the System or registering its fixed Plugins.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SystemCreateError {
    /// Selected Target resources cannot be assigned to required System roles.
    #[error(transparent)]
    Resources(#[from] SystemResourceError),
    /// A required System-owned filesystem could not be mounted.
    #[error(transparent)]
    Filesystem(#[from] FsError),
    /// A Plugin failed during registration.
    #[error(transparent)]
    Plugin(#[from] PluginRegisterError),
    /// A registered Plugin failed to start.
    #[error(transparent)]
    PluginStart(#[from] PluginStartError),
    /// Plugin persistence could not be opened from its validated partition.
    #[error(transparent)]
    PluginManager(#[from] PluginManagerInitError),
    /// The System-owned removable-filesystem lifecycle task could not start.
    #[error("failed to start removable-filesystem lifecycle task")]
    RemovableStorageTask,
}

impl<Region, Peripherals, Io, const P: usize> System<Region, Peripherals, Io, P>
where
    Region: NorFlash + Send + Unpin + 'static,
    Region::Error: core::fmt::Debug,
    Peripherals: AudioCodecPeripheral
        + CameraPeripheral
        + DisplayPeripheral
        + ImuPeripheral
        + LedStripPeripheral
        + PowerMonitorPeripheral
        + RealTimeClockPeripheral
        + RemovableStoragePeripheral
        + TouchPeripheral
        + Unpin,
    Peripherals::AudioCodec: Send + 'static,
    <Peripherals::AudioCodec as barracuda_board_hal::audio::AudioCodec>::Error: core::fmt::Debug,
    Peripherals::Display: Send + 'static,
    <Peripherals::Display as barracuda_board_hal::display::Display>::ControlError: core::fmt::Debug,
    <Peripherals::Display as barracuda_board_hal::display::Display>::RenderError: core::fmt::Debug,
    Peripherals::Camera: Send + 'static,
    <Peripherals::Camera as barracuda_board_hal::camera::Camera>::Error: core::fmt::Debug,
    Peripherals::LedStrip: Send + 'static,
    <Peripherals::LedStrip as barracuda_board_hal::led_strip::LedStrip>::Error: core::fmt::Debug,
    Peripherals::Imu: Send + 'static,
    <Peripherals::Imu as barracuda_board_hal::imu::Imu>::Error: core::fmt::Debug,
    Peripherals::RemovableStorage: Send + 'static,
    Io: ExposedIo
        + AnalogProvider
        + DigitalProvider
        + I2cProvider
        + I2sProvider
        + PwmProvider
        + SpiProvider
        + UartProvider
        + Unpin,
    <Io::Input as barracuda_board_hal::AnalogErrorType>::Error: core::fmt::Debug,
    <<Io as AnalogProvider>::Output as barracuda_board_hal::AnalogErrorType>::Error:
        core::fmt::Debug,
    <Io as AnalogProvider>::Error: core::fmt::Display,
    <<Io as PwmProvider>::Output as embedded_hal::pwm::ErrorType>::Error: core::fmt::Debug,
    <Io as PwmProvider>::Error: core::fmt::Display,
    Io::Pin: Send,
    <Io::Pin as embedded_hal::digital::ErrorType>::Error: core::fmt::Debug,
    <Io as DigitalProvider>::Error: core::fmt::Display,
    <Io as I2cProvider>::Bus: Send,
    <<Io as I2cProvider>::Bus as embedded_hal::i2c::ErrorType>::Error: core::fmt::Debug,
    <Io as I2cProvider>::Error: core::fmt::Display,
    <Io as I2sProvider>::Stream: Send,
    <<Io as I2sProvider>::Stream as barracuda_board_hal::audio::PcmStream>::Error: core::fmt::Debug,
    <Io as I2sProvider>::Error: core::fmt::Display,
    <Io as SpiProvider>::Bus: Send,
    <<Io as SpiProvider>::Bus as embedded_hal::spi::ErrorType>::Error: core::fmt::Debug,
    <Io as SpiProvider>::Error: core::fmt::Display,
    <Io as UartProvider>::Port: Send,
    <<Io as UartProvider>::Port as embedded_io::ErrorType>::Error: core::fmt::Debug,
    <Io as UartProvider>::Error: core::fmt::Display,
{
    /// Constructs, registers, and starts the fixed Plugin set.
    ///
    /// The caller supplies resources from the selected-target resource
    /// factory and the current Embassy executor spawner. System consumes them,
    /// owns the complete Plugin registration order, and exposes the spawner
    /// only to Plugin startup hooks.
    ///
    /// # Errors
    ///
    /// Returns [`SystemCreateError`] when Plugin registration or startup fails.
    pub async fn new<Tls: ClientTls, Wifi: WifiDevice>(
        resources: TargetResources<
            PlatformResources<Tls, Partitions<Region, P>, Wifi>,
            BoardResources<Peripherals, Io>,
        >,
        target_identity: TargetIdentity,
        spawner: Spawner,
    ) -> Result<Self, SystemCreateError> {
        log::info!("assembling Barracuda System");
        let mut prepared = resources::prepare(resources)?;
        log::info!("assigned selected Target resources to System roles");
        let backend = mount_or_format_partition(prepared.partitions.system)?;
        mount("/data", backend.clone(), MountOptions::read_write()).await?;
        log::info!("mounted System data filesystem");
        create_dir_all("/data/media").await?;
        mount_scoped("/media", backend, "/media", MountOptions::read_write()).await?;
        log::info!("mounted durable media filesystem from System storage");
        let resources_filesystem = prepared.partitions.resources.filesystem;
        let resources =
            mount_resources_partition(prepared.partitions.resources.region, resources_filesystem)
                .await?;
        mount("/resources", resources, MountOptions::read_only()).await?;
        log::info!(
            "mounted bundled Plugin resources from read-only {:?}",
            resources_filesystem
        );
        let cache = MemFs::new().into_backend();
        mount("/cache", cache, MountOptions::read_write()).await?;
        log::info!("mounted System cache filesystem");
        let removable_namespace = MemFs::new().into_backend();
        mount(
            "/removable",
            removable_namespace,
            MountOptions::read_write(),
        )
        .await?;
        log::info!("mounted removable-filesystem namespace");
        let removable_storage = prepared.board_hal.peripherals.take_removable_storage();
        let mut plugins =
            PluginManager::open(BlockingAsync::new(prepared.partitions.kv_database)).await?;
        log::info!("opened Plugin Manager storage");
        plugins.install_vfs(global_namespace().await);
        plugins.install_task_spawner(spawner);

        let http_clients =
            http_client::ClientFactory::new(prepared.ip_stack, move || prepared.tls.config());
        let mut plugin_context = PluginContext::from_hal(
            target_identity,
            prepared.ip_stack,
            http_clients,
            prepared.board_hal,
        );

        // BEGIN GENERATED PLUGINS
        register_plugins!(plugins;
            barracuda_agent_plugin::AgentPlugin::new(&mut plugin_context),
            barracuda_agent_file_plugin::AgentFilePlugin::new(&mut plugin_context),
            barracuda_agent_http_plugin::AgentHttpPlugin::new(&mut plugin_context),
            barracuda_agent_imessage_gateway_plugin::AgentIMessageGatewayPlugin::new(&mut plugin_context),
            barracuda_agent_scheduler_plugin::AgentSchedulerPlugin::new(&mut plugin_context),
            barracuda_agent_time_plugin::AgentTimePlugin::new(&mut plugin_context),
            barracuda_agent_vm_plugin::AgentVmPlugin::new(&mut plugin_context),
            barracuda_agent_websearch_plugin::AgentWebsearchPlugin::new(&mut plugin_context),
            barracuda_agent_workflow_plugin::AgentWorkflowPlugin::new(&mut plugin_context),
            barracuda_captive_portal_plugin::CaptivePortalPlugin::new(&mut plugin_context),
            barracuda_http_plugin::HttpPlugin::new(&mut plugin_context),
            barracuda_imessage_bluebubble_plugin::IMessageBlueBubblePlugin::new(&mut plugin_context),
            barracuda_imessage_gateway_plugin::IMessageGatewayPlugin::new(&mut plugin_context),
            barracuda_imessage_inkbox_plugin::IMessageInkboxPlugin::new(&mut plugin_context),
            barracuda_imessage_qq_plugin::IMessageQQPlugin::new(&mut plugin_context),
            barracuda_imessage_telegram_plugin::IMessageTelegramPlugin::new(&mut plugin_context),
            barracuda_imessage_web_plugin::IMessageWebPlugin::new(&mut plugin_context),
            barracuda_imessage_wechat_plugin::IMessageWechatPlugin::new(&mut plugin_context),
            barracuda_scheduler_plugin::SchedulerPlugin::new(&mut plugin_context),
            barracuda_time_plugin::TimePlugin::new(&mut plugin_context),
            barracuda_vm_plugin::VmPlugin::new(&mut plugin_context),
            barracuda_vm_agent_plugin::VmAgentPlugin::new(&mut plugin_context),
            barracuda_analog_plugin::AnalogPlugin::new(&mut plugin_context),
            barracuda_audio_plugin::AudioPlugin::new(&mut plugin_context),
            barracuda_camera_plugin::CameraPlugin::new(&mut plugin_context),
            barracuda_display_plugin::DisplayPlugin::new(&mut plugin_context),
            barracuda_vm_filesystem_plugin::VmFilesystemPlugin::new(&mut plugin_context),
            barracuda_gpio_plugin::GpioPlugin::new(&mut plugin_context),
            barracuda_vm_http_plugin::VmHttpPlugin::new(&mut plugin_context),
            barracuda_i2c_plugin::I2cPlugin::new(&mut plugin_context),
            barracuda_i2s_plugin::I2sPlugin::new(&mut plugin_context),
            barracuda_imu_plugin::ImuPlugin::new(&mut plugin_context),
            barracuda_led_strip_plugin::LedStripPlugin::new(&mut plugin_context),
            barracuda_message_queue_plugin::MessageQueuePlugin::new(&mut plugin_context),
            barracuda_pwm_plugin::PwmPlugin::new(&mut plugin_context),
            barracuda_spi_plugin::SpiPlugin::new(&mut plugin_context),
            barracuda_vm_systeminfo_plugin::VmSystemInfoPlugin::new(&mut plugin_context),
            barracuda_vm_time_plugin::VmTimePlugin::new(&mut plugin_context),
            barracuda_uart_plugin::UartPlugin::new(&mut plugin_context),
            barracuda_vm_webserver_plugin::VmWebServerPlugin::new(&mut plugin_context),
            barracuda_webserver_plugin::WebServerPlugin::new(&mut plugin_context),
            barracuda_wifi_plugin::WifiPlugin::new(&mut plugin_context, prepared.wifi),
            barracuda_workflow_plugin::WorkflowPlugin::new(&mut plugin_context),
        );
        // END GENERATED PLUGINS
        let mut removable_storage_task = removable_storage
            .map(|storage| start_removable_storage_task(storage, spawner))
            .transpose()
            .map_err(|_error| SystemCreateError::RemovableStorageTask)?;
        if let Err(error) = plugins.start() {
            if let Some(task) = removable_storage_task.take() {
                task.shutdown().await;
            }
            return Err(error.into());
        }
        log::info!("Barracuda System started");

        Ok(Self {
            plugins,
            removable_storage_task,
            _remaining_partitions: prepared.partitions.remaining,
            _plugin_context: plugin_context,
        })
    }

    /// Stops every Plugin in reverse dependency order and waits for its tasks.
    ///
    /// Remaining System-owned resources are released normally when this method
    /// returns.
    ///
    /// # Errors
    ///
    /// Returns the first Plugin unload failure.
    pub async fn shutdown(mut self) -> Result<(), PluginUnloadError> {
        let result = self.plugins.shutdown().await;
        if let Some(task) = self.removable_storage_task.take() {
            task.shutdown().await;
        }
        result
    }
}
