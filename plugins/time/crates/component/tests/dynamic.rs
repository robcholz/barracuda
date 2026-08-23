#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::{cell::RefCell, rc::Rc};

use barracuda_event_router::{RpcAddress, RpcLaneStorage, RpcMethod, RpcRegistry};
use barracuda_time_component::now::{Now, now_handler};
use barracuda_time_component::{ClockState, SyncSample, TimeConfig};
use embassy_time::Instant;
use serde_json::json;

#[test]
fn time_now_accepts_dynamic_empty_object_calls() {
    let lanes = Box::leak(Box::new(RpcLaneStorage::<2, 128, 2>::new()));
    let registry = RpcRegistry::new(lanes);
    let mut clock = ClockState::new(TimeConfig::new(1_000, 60_000, 120_000));
    clock.synchronize(SyncSample::new(1_800_000_000_000, Instant::now()));
    let _registration = registry
        .register::<Now, _>(now_handler(Rc::new(RefCell::new(clock))))
        .expect("register time.now");
    let address = RpcAddress::try_from(Now::ADDRESS).expect("valid address");

    let response =
        futures_lite::future::block_on(registry.client().call_json(&address, &json!({})))
            .expect("dynamic time.now call");

    assert_eq!(response.pointer("/ok"), Some(&json!(true)));
    for field in ["year", "month", "day", "hour", "minute", "second"] {
        assert!(
            response
                .pointer(&format!("/value/{field}"))
                .and_then(serde_json::Value::as_u64)
                .is_some()
        );
    }
}
