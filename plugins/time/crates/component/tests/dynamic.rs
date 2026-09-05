#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::rc::Rc;

use barracuda_event_router::{JsonRpcSchema, RpcAddress, RpcError, RpcLaneStorage, RpcRegistry};
use barracuda_time_component::now::{Now, now_handler};
use barracuda_time_component::{SyncSample, TimeConfig, UtcClock, utc_clock};
use embassy_time::Instant;
use serde_json::Value;

fn registry(clock: Rc<UtcClock>) -> RpcRegistry<2, 128, 2> {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 128, 2>::new()));
    let registry = RpcRegistry::new(lanes);
    let _registration = registry
        .register_json::<Now, _>("*", now_handler(clock))
        .expect("register time.now");
    registry
}

#[test]
fn time_now_returns_rfc3339_utc() {
    let (clock, updater) = utc_clock(TimeConfig::new(1_000, 60_000, u64::MAX));
    updater.synchronize(SyncSample::new(1_800_000_000_123, Instant::now()));
    let registry = registry(clock);
    let address = RpcAddress::try_from(Now::ADDRESS).expect("valid address");

    let client = registry.client();
    let call = client
        .call_json(&address, "{}")
        .expect("start time.now JSON call");
    let response = futures_lite::future::block_on(call).expect("time.now JSON call");
    let response: Value = response.deserialize().expect("valid response JSON");
    let utc = response
        .get("utc")
        .and_then(Value::as_str)
        .expect("UTC string");

    assert!(utc.starts_with("2027-01-15T08:00:00."));
    assert!(utc.ends_with('Z'));
    assert_eq!(utc.len(), 24);
}

#[test]
fn time_now_returns_a_stable_business_error_document() {
    let (clock, _updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    let registry = registry(clock);
    let address = RpcAddress::try_from(Now::ADDRESS).expect("valid address");

    let client = registry.client();
    let call = client
        .call_json(&address, "{}")
        .expect("start time.now JSON call");
    let response = futures_lite::future::block_on(call).expect("time.now JSON call");

    assert_eq!(
        response.as_str().expect("response JSON"),
        r#"{"error":"unsynchronized"}"#
    );
}

#[test]
fn time_now_rejects_non_object_requests() {
    let (clock, _updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    let registry = registry(clock);
    let address = RpcAddress::try_from(Now::ADDRESS).expect("valid address");

    let error = futures_lite::future::block_on(
        registry
            .client()
            .call_json(&address, "[]")
            .expect("start JSON call"),
    )
    .expect_err("request must be an empty object");

    assert_eq!(error, RpcError::InvalidJson);
}

#[test]
fn time_now_is_discoverable_by_public_visibility() {
    let (clock, _updater) = utc_clock(TimeConfig::new(1_000, 60_000, 120_000));
    let registry = registry(clock);

    assert_eq!(
        registry
            .client()
            .rpcs_by_visibility("*")
            .expect("discover public RPCs"),
        [RpcAddress::try_from("time.now").expect("valid address")]
    );
    assert!(
        registry
            .client()
            .rpcs_by_visibility("agent")
            .expect("discover old visibility")
            .is_empty()
    );
}
