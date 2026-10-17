//! Network-synchronized UTC capability Plugin.

#![no_std]

extern crate alloc;

mod config {
    barracuda_plugin::config!();
}

mod calendar;
mod clock;
/// SNTP source implementation owned by the Time Plugin.
pub mod sntp;

use alloc::boxed::Box;
use barracuda_board_hal::real_time_clock::{RealTimeClock, RealTimeClockPeripheral};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use embassy_futures::select::select;
use embassy_net::Stack;
use embassy_time::Instant;
use sntp::{SntpConfig, SntpSource};

pub use clock::{
    ClockError, ClockSource, SyncSample, TimeConfig, TimeSource, TimeSourceError, TimeSourceFuture,
    UnixMillis, UtcClock, UtcClockUpdater, synchronize_clock, synchronize_clock_into, utc_clock,
};

const SNTP_SERVER: &str = "pool.ntp.org";
const MINIMUM_UNIX_SECONDS: u64 = 1_704_067_200;
const RETRY_DELAY_MILLIS: u64 = 30_000;
const RESYNC_INTERVAL_MILLIS: u64 = 3_600_000;
const MAX_HOLDOVER_MILLIS: u64 = 86_400_000;

/// Plugin that owns and publishes the network-synchronized UTC clock, and
/// the Board's real-time clock that keeps time between boots.
#[barracuda_plugin::macros::plugin]
pub struct TimePlugin<Rtc> {
    network: Stack<'static>,
    rtc: Option<Rtc>,
    runtime: Option<TimeRuntime>,
}

/// Writes network time to the Board's real-time clock.
type RtcWriter = Box<dyn FnMut(SyncSample)>;

struct TimeRuntime {
    source: SntpSource,
    updater: UtcClockUpdater,
    rtc: Option<RtcWriter>,
}

impl<Rtc> TimePlugin<Rtc> {
    /// Creates the Plugin with the IP stack used by its SNTP source and the
    /// Board's real-time clock, when it has one.
    #[must_use]
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self
    where
        Builtins: RealTimeClockPeripheral<RealTimeClock = Rtc>,
    {
        Self {
            network: context.ip_stack,
            rtc: config::peripherals::REAL_TIME_CLOCK_DEVICE
                .and_then(|instance| context.hal.peripherals.take_real_time_clock(instance)),
            runtime: None,
        }
    }
}

/// The time the real-time clock keeps, when it kept it through power loss
/// and it is no earlier than any plausible time.
fn retained_time<Rtc: RealTimeClock>(rtc: &mut Rtc) -> Option<u64> {
    if rtc.lost_power().ok()? {
        log::warn!("real-time clock lost power; waiting for network time");
        return None;
    }
    let millis = calendar::unix_millis(rtc.read_datetime().ok()?)?;
    (millis / 1_000 >= MINIMUM_UNIX_SECONDS).then_some(millis)
}

impl<Rtc> Plugin for TimePlugin<Rtc>
where
    Rtc: RealTimeClock + 'static,
{
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
        let rtc = self.rtc.take().map(|mut rtc| {
            if let Some(millis) = retained_time(&mut rtc) {
                updater.seed(SyncSample::new(millis, Instant::now()));
                log::info!("UTC clock started from the real-time clock");
            }
            Box::new(move |sample: SyncSample| {
                let now = sample.unix_millis().saturating_add(
                    Instant::now()
                        .checked_duration_since(sample.monotonic_anchor())
                        .map_or(0, |elapsed| elapsed.as_millis()),
                );
                let written = calendar::datetime(now).map(|datetime| rtc.set_datetime(datetime));
                if !matches!(written, Some(Ok(()))) {
                    log::warn!("cannot set the real-time clock to network time");
                }
            }) as RtcWriter
        });
        context.provide(clock)?;
        self.runtime = Some(TimeRuntime {
            source,
            updater,
            rtc,
        });
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
        let task = time_sync_task(runtime.source, runtime.updater, runtime.rtc, cancellation)
            .map_err(PluginError::registration)?;
        spawner.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn time_sync_task(
    source: SntpSource,
    updater: UtcClockUpdater,
    mut rtc: Option<RtcWriter>,
    cancellation: PluginTaskToken,
) {
    let synchronize = synchronize_clock_into(source, updater, |sample| {
        if let Some(rtc) = rtc.as_mut() {
            rtc(sample);
        }
    });
    let _completed = select(cancellation.cancelled(), synchronize).await;
    log::info!("stopped Time synchronization task");
}

#[derive(Debug, thiserror::Error)]
#[error("Time runtime was not prepared during Plugin registration")]
struct TimeRuntimeUnavailable;
