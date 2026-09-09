//! Network-synchronized UTC capability Plugin.

#![no_std]

extern crate alloc;

mod clock;
/// SNTP source implementation owned by the Time Plugin.
pub mod sntp;

use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use embassy_futures::select::select;
use embassy_net::Stack;
use sntp::{SntpConfig, SntpSource};

pub use clock::{
    ClockError, SyncSample, TimeConfig, TimeSource, TimeSourceError, TimeSourceFuture, UnixMillis,
    UtcClock, UtcClockUpdater, synchronize_clock, utc_clock,
};

const SNTP_SERVER: &str = "pool.ntp.org";
const MINIMUM_UNIX_SECONDS: u64 = 1_704_067_200;
const RETRY_DELAY_MILLIS: u64 = 30_000;
const RESYNC_INTERVAL_MILLIS: u64 = 3_600_000;
const MAX_HOLDOVER_MILLIS: u64 = 86_400_000;

/// Plugin that owns and publishes the network-synchronized UTC clock.
#[barracuda_plugin::macros::plugin]
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

impl Plugin for TimePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
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
        context.provide(clock)?;
        self.runtime = Some(TimeRuntime { source, updater });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(TimeRuntimeUnavailable))?;
        let spawner = context.task_spawner()?;
        let cancellation = context.task_token();
        let task = time_sync_task(runtime.source, runtime.updater, cancellation)
            .map_err(PluginError::registration)?;
        spawner.spawn(task);
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
