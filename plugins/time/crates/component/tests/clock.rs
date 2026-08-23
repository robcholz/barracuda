#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_time_component::{ClockState, SyncSample, TimeConfig};
use embassy_time::{Duration, Instant};

#[test]
fn synchronized_clock_advances_from_network_anchor() {
    let mut clock = ClockState::new(TimeConfig::new(1_000, 60_000, 120_000));
    let anchor = Instant::from_millis(10_000);
    clock.synchronize(SyncSample::new(1_800_000_000_000, anchor));

    let now = clock
        .now_at(anchor + Duration::from_millis(1_250))
        .expect("synchronized clock");
    assert_eq!(now.year(), 2027);
    assert_eq!(now.month(), 1);
    assert_eq!(now.day(), 15);
    assert_eq!(now.hour(), 8);
    assert_eq!(now.minute(), 0);
    assert_eq!(now.second(), 1);
}

#[test]
fn clock_is_unavailable_before_first_network_sync() {
    let clock = ClockState::new(TimeConfig::new(1_000, 60_000, 120_000));
    assert!(clock.now_at(Instant::from_millis(1)).is_err());
}

#[test]
fn clock_becomes_stale_after_holdover_limit() {
    let mut clock = ClockState::new(TimeConfig::new(1_000, 60_000, 5_000));
    let anchor = Instant::from_millis(2_000);
    clock.synchronize(SyncSample::new(1_800_000_000_000, anchor));

    assert!(clock.now_at(anchor + Duration::from_millis(5_001)).is_err());
}

#[test]
fn resynchronization_replaces_the_rtc_anchor() {
    let mut clock = ClockState::new(TimeConfig::new(1_000, 60_000, 120_000));
    let first = Instant::from_millis(1_000);
    clock.synchronize(SyncSample::new(1_800_000_000_000, first));
    let second = Instant::from_millis(2_000);
    clock.synchronize(SyncSample::new(1_800_000_100_000, second));

    let now = clock.now_at(second).expect("resynchronized clock");
    assert_eq!(now.year(), 2027);
    assert_eq!(now.month(), 1);
    assert_eq!(now.day(), 15);
    assert_eq!(now.hour(), 8);
    assert_eq!(now.minute(), 1);
    assert_eq!(now.second(), 40);
}
