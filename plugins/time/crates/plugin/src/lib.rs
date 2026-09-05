//! Plugin entry point for the network-synchronized Time Component.

#![no_std]

extern crate alloc;

use alloc::rc::Rc;
use barracuda_plugin_api::PluginContext;
use barracuda_plugin_manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_time_component::{
    TimeComponent, TimeConfig, UtcClockUpdater,
    sntp::{SntpConfig, SntpSource},
    synchronize_clock, utc_clock,
};
use embassy_futures::select::select;
use embassy_net::Stack;

const SNTP_SERVER: &str = "pool.ntp.org";
const MINIMUM_UNIX_SECONDS: u64 = 1_704_067_200;
const RETRY_DELAY_MILLIS: u64 = 30_000;
const RESYNC_INTERVAL_MILLIS: u64 = 3_600_000;
const MAX_HOLDOVER_MILLIS: u64 = 86_400_000;

/// Plugin that owns the network-synchronized Time Component.
#[barracuda_plugin_api::plugin]
pub struct TimePlugin {
    network: Stack<'static>,
    runtime: Option<TimeRuntime>,
}

struct TimeRuntime {
    source: SntpSource,
    updater: UtcClockUpdater,
}

impl TimePlugin {
    /// Creates the Plugin with the IP stack used by its SNTP source.
    #[must_use]
    pub const fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        Self {
            network: context.ip_stack,
            runtime: None,
        }
    }
}

impl<const M: usize> Plugin<M> for TimePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, M, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let source = SntpSource::new(
            self.network,
            SntpConfig::new(SNTP_SERVER, MINIMUM_UNIX_SECONDS),
        );
        let config = TimeConfig::new(
            RETRY_DELAY_MILLIS,
            RESYNC_INTERVAL_MILLIS,
            MAX_HOLDOVER_MILLIS,
        );
        let (clock, updater) = utc_clock(config);
        let component = TimeComponent::new(Rc::clone(&clock));
        context.provide(Rc::clone(&clock))?;
        context.event_router.load(component)?;
        self.runtime = Some(TimeRuntime { source, updater });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin_manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(TimeRuntimeUnavailable))?;
        let spawner = context.task_spawner()?;
        let cancellation = context.task_token();
        spawner
            .spawn(time_sync_task(
                runtime.source,
                runtime.updater,
                cancellation,
            ))
            .map_err(PluginError::registration)?;
        Ok(())
    }
}

#[embassy_executor::task]
async fn time_sync_task(
    source: SntpSource,
    updater: UtcClockUpdater,
    cancellation: PluginTaskToken,
) {
    let _completed = select(cancellation.cancelled(), synchronize_clock(source, updater)).await;
    log::info!("stopped Time synchronization task");
}

#[derive(Debug, thiserror::Error)]
#[error("Time runtime was not prepared during Plugin registration")]
struct TimeRuntimeUnavailable;
