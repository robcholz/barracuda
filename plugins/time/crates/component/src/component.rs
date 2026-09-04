use alloc::{boxed::Box, rc::Rc};
use core::{
    cell::RefCell,
    future::{Future, pending},
    pin::Pin,
};

use barracuda_event_router::{
    Component, ComponentFuture, ComponentResult, RegisterContext, RunContext, UnregisterContext,
};
use embassy_time::{Duration, Instant, Timer};
use getset::CopyGetters;
use time::OffsetDateTime;

use crate::now::{Now, TimeNow, TimeRpcError, now_handler};

/// Future returned by a [`TimeSource`] synchronization attempt.
pub type TimeSourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SyncSample, TimeSourceError>> + 'a>>;

/// Failure obtaining or validating one authoritative network time sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TimeSourceError {
    /// DNS lookup failed.
    #[error("network time DNS lookup failed")]
    Dns,
    /// UDP socket creation or IO failed.
    #[error("network time UDP operation failed")]
    Network,
    /// The network request exceeded its deadline.
    #[error("network time request timed out")]
    Timeout,
    /// The SNTP response failed protocol validation.
    #[error("network time response was invalid")]
    Protocol,
    /// The result violated the configured minimum plausible Unix time.
    #[error("network time result was implausible")]
    Implausible,
}

/// Source capable of producing authoritative UTC synchronization samples.
pub trait TimeSource {
    /// Starts one synchronization attempt.
    fn synchronize(&mut self) -> TimeSourceFuture<'_>;
}

/// One accepted network time sample anchored to a monotonic instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct SyncSample {
    /// UTC milliseconds at `monotonic_anchor`.
    #[getset(get_copy = "pub")]
    unix_millis: u64,
    /// Local instant corresponding to `unix_millis`.
    #[getset(get_copy = "pub")]
    monotonic_anchor: Instant,
}

impl SyncSample {
    /// Creates one validated synchronization sample.
    #[must_use]
    pub const fn new(unix_millis: u64, monotonic_anchor: Instant) -> Self {
        Self {
            unix_millis,
            monotonic_anchor,
        }
    }
}

/// Network RTC retry, resynchronization, and holdover policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, CopyGetters)]
pub struct TimeConfig {
    /// Delay after a failed synchronization attempt.
    #[getset(get_copy = "pub")]
    retry_delay_millis: u64,
    /// Delay between successful periodic synchronizations.
    #[getset(get_copy = "pub")]
    resync_interval_millis: u64,
    /// Maximum age of an anchor accepted by `time.now`.
    #[getset(get_copy = "pub")]
    max_holdover_millis: u64,
}

impl TimeConfig {
    /// Creates an explicit RTC synchronization policy.
    #[must_use]
    pub const fn new(
        retry_delay_millis: u64,
        resync_interval_millis: u64,
        max_holdover_millis: u64,
    ) -> Self {
        Self {
            retry_delay_millis,
            resync_interval_millis,
            max_holdover_millis,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ClockAnchor {
    sample: SyncSample,
}

/// RTC state shared by the synchronization loop and typed RPC handler.
pub struct ClockState {
    config: TimeConfig,
    anchor: Option<ClockAnchor>,
}

impl ClockState {
    /// Creates an unsynchronized RTC.
    #[must_use]
    pub const fn new(config: TimeConfig) -> Self {
        Self {
            config,
            anchor: None,
        }
    }

    /// Replaces the authoritative network anchor.
    pub fn synchronize(&mut self, sample: SyncSample) {
        self.anchor = Some(ClockAnchor { sample });
    }

    /// Reads the RTC at the current local instant.
    pub fn now(&self) -> Result<TimeNow, TimeRpcError> {
        self.now_at(Instant::now())
    }

    /// Reads the RTC at an explicit local instant.
    pub fn now_at(&self, instant: Instant) -> Result<TimeNow, TimeRpcError> {
        let anchor = self.anchor.ok_or(TimeRpcError::Unsynchronized)?;
        let elapsed = instant
            .checked_duration_since(anchor.sample.monotonic_anchor)
            .ok_or(TimeRpcError::Unsynchronized)?;
        if elapsed.as_millis() > self.config.max_holdover_millis {
            return Err(TimeRpcError::Stale);
        }
        let unix_millis = anchor
            .sample
            .unix_millis
            .saturating_add(elapsed.as_millis());
        let unix_seconds =
            i64::try_from(unix_millis / 1_000).map_err(|_error| TimeRpcError::OutOfRange)?;
        let calendar = OffsetDateTime::from_unix_timestamp(unix_seconds)
            .map_err(|_error| TimeRpcError::OutOfRange)?;
        let year = u16::try_from(calendar.year()).map_err(|_error| TimeRpcError::OutOfRange)?;
        Ok(TimeNow::new(
            year,
            u8::from(calendar.month()),
            calendar.day(),
            calendar.hour(),
            calendar.minute(),
            calendar.second(),
        ))
    }
}

/// Event Router Component exposing a network-synchronized RTC through RPC.
pub struct TimeComponent {
    state: Rc<RefCell<ClockState>>,
}

impl TimeComponent {
    /// Creates an unsynchronized RPC Component.
    #[must_use]
    pub fn new(config: TimeConfig) -> Self {
        Self {
            state: Rc::new(RefCell::new(ClockState::new(config))),
        }
    }

    /// Clones the clock state handle used by the Plugin-owned sync task.
    #[must_use]
    pub fn shared_state(&self) -> Rc<RefCell<ClockState>> {
        Rc::clone(&self.state)
    }
}

impl<const M: usize> Component<M> for TimeComponent {
    fn register(&mut self, context: &mut RegisterContext<'_, M>) -> ComponentResult<()> {
        context.register_rpc::<Now, _>("agent", now_handler(Rc::clone(&self.state)))
    }

    fn run<'a>(&'a mut self, _context: RunContext<M>) -> ComponentFuture<'a> {
        Box::pin(pending())
    }

    fn unregister(&mut self, _context: &mut UnregisterContext<'_>) -> ComponentResult<()> {
        Ok(())
    }
}

/// Runs the network synchronization loop owned by the Time Plugin task.
pub async fn synchronize_clock<Source>(mut source: Source, state: Rc<RefCell<ClockState>>)
where
    Source: TimeSource,
{
    loop {
        let synchronized = match source.synchronize().await {
            Ok(sample) => {
                state.borrow_mut().synchronize(sample);
                true
            }
            Err(_error) => false,
        };
        let config = state.borrow().config;
        let delay = if synchronized {
            config.resync_interval_millis
        } else {
            config.retry_delay_millis
        };
        Timer::after(Duration::from_millis(delay.max(1))).await;
    }
}
