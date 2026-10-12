use alloc::{boxed::Box, rc::Rc};
use core::{cell::RefCell, future::Future, pin::Pin};

use embassy_time::{Duration, Instant, Timer};
use getset::CopyGetters;

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
    /// Maximum age of an anchor accepted by readers.
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

struct ClockState {
    config: TimeConfig,
    anchor: Option<ClockAnchor>,
}

impl ClockState {
    const fn new(config: TimeConfig) -> Self {
        Self {
            config,
            anchor: None,
        }
    }
}

/// Failure reading the synchronized UTC clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClockError {
    /// No successful network synchronization has completed since boot.
    #[error("UTC clock is not synchronized")]
    Unsynchronized,
    /// The last synchronization exceeded the configured holdover.
    #[error("UTC clock synchronization is stale")]
    Stale,
    /// The synchronized value cannot be represented by the requested format.
    #[error("UTC clock value is out of range")]
    OutOfRange,
}

impl ClockError {
    /// Stable machine-readable error code used by external adapters.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unsynchronized => "unsynchronized",
            Self::Stale => "stale",
            Self::OutOfRange => "out_of_range",
        }
    }
}

/// An absolute UTC timestamp measured from the Unix epoch in milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UnixMillis(u64);

impl From<UnixMillis> for u64 {
    fn from(timestamp: UnixMillis) -> Self {
        timestamp.0
    }
}

/// Shared read-only UTC clock capability published by the Time Plugin.
pub struct UtcClock {
    state: RefCell<ClockState>,
}

impl UtcClock {
    /// Reads the current absolute UTC timestamp.
    pub fn now(&self) -> Result<UnixMillis, ClockError> {
        self.now_at(Instant::now())
    }

    /// Reads the UTC timestamp at an explicit monotonic instant.
    pub fn now_at(&self, instant: Instant) -> Result<UnixMillis, ClockError> {
        let state = self.state.borrow();
        let anchor = state.anchor.ok_or(ClockError::Unsynchronized)?;
        let elapsed = instant
            .checked_duration_since(anchor.sample.monotonic_anchor)
            .ok_or(ClockError::Unsynchronized)?;
        if elapsed.as_millis() > state.config.max_holdover_millis {
            return Err(ClockError::Stale);
        }
        Ok(UnixMillis(
            anchor
                .sample
                .unix_millis
                .saturating_add(elapsed.as_millis()),
        ))
    }
}

/// Time Plugin-owned write side of a [`UtcClock`].
pub struct UtcClockUpdater {
    clock: Rc<UtcClock>,
}

impl UtcClockUpdater {
    /// Replaces the authoritative network anchor.
    pub fn synchronize(&self, sample: SyncSample) {
        self.clock.state.borrow_mut().anchor = Some(ClockAnchor { sample });
    }

    fn config(&self) -> TimeConfig {
        self.clock.state.borrow().config
    }
}

/// Creates one read-only UTC clock capability and its Plugin-owned updater.
#[must_use]
pub fn utc_clock(config: TimeConfig) -> (Rc<UtcClock>, UtcClockUpdater) {
    let clock = Rc::new(UtcClock {
        state: RefCell::new(ClockState::new(config)),
    });
    let updater = UtcClockUpdater {
        clock: Rc::clone(&clock),
    };
    (clock, updater)
}

/// Runs the network synchronization loop owned by the Time Plugin task.
pub async fn synchronize_clock<Source>(mut source: Source, updater: UtcClockUpdater)
where
    Source: TimeSource,
{
    // Only the previous outcome is logged so a persistent failure is reported
    // once instead of on every retry.
    let mut last_failure = None;
    let mut ever_synchronized = false;
    loop {
        let synchronized = match source.synchronize().await {
            Ok(sample) => {
                updater.synchronize(sample);
                if !ever_synchronized || last_failure.is_some() {
                    log::info!("UTC clock synchronized from network time");
                }
                ever_synchronized = true;
                last_failure = None;
                true
            }
            Err(error) => {
                if last_failure != Some(error) {
                    log::warn!("network time synchronization failed: {error}");
                }
                last_failure = Some(error);
                false
            }
        };
        let config = updater.config();
        let delay = if synchronized {
            config.resync_interval_millis
        } else {
            config.retry_delay_millis
        };
        Timer::after(Duration::from_millis(delay.max(1))).await;
    }
}
