#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_time_component::{ClockError, SyncSample, TimeConfig, utc_clock};
use embassy_time::{Duration, Instant};

#[test]
fn synchronized_clock_returns_typed_unix_milliseconds() {
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    let anchor = Instant::from_millis(10_000);
    updater.synchronize(SyncSample::new(1_800_000_000_000, anchor));

    let now = clock
        .now_at(anchor + Duration::from_millis(1_250))
        .expect("synchronized clock");
    assert_eq!(u64::from(now), 1_800_000_001_250);
}

#[test]
fn clock_is_unavailable_before_first_network_sync() {
    let (clock, _updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    assert_eq!(
        clock.now_at(Instant::from_millis(1)),
        Err(ClockError::Unsynchronized)
    );
}

#[test]
fn clock_becomes_stale_after_holdover_limit() {
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, 5_000));
    let anchor = Instant::from_millis(2_000);
    updater.synchronize(SyncSample::new(1_800_000_000_000, anchor));

    assert_eq!(
        clock.now_at(anchor + Duration::from_millis(5_001)),
        Err(ClockError::Stale)
    );
}

#[test]
fn resynchronization_replaces_the_utc_anchor() {
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    let first = Instant::from_millis(1_000);
    updater.synchronize(SyncSample::new(1_800_000_000_000, first));
    let second = Instant::from_millis(2_000);
    updater.synchronize(SyncSample::new(1_800_000_100_000, second));

    let now = clock.now_at(second).expect("resynchronized clock");
    assert_eq!(u64::from(now), 1_800_000_100_000);
}
