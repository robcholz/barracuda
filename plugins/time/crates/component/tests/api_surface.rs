#![allow(missing_docs)]
use barracuda_event_router::JsonRpcSchema;
use barracuda_time_component::now::Now;

#[test]
fn time_now_exposes_the_json_contract() {
    assert_eq!(Now::ADDRESS, "time.now");
    assert_eq!(Now::MAX_REQUEST_BYTES, 2);
    assert_eq!(Now::MAX_RESPONSE_BYTES, 34);

    let request = Now::REQUEST_SCHEMA.as_str();
    assert!(request.contains(r#""type": "object""#));
    assert!(request.contains(r#""additionalProperties": false"#));

    let response = Now::RESPONSE_SCHEMA.as_str();
    assert!(response.contains(r#""utc""#));
    assert!(response.contains(r#""format": "date-time""#));
    for error in ["unsynchronized", "stale", "out_of_range"] {
        assert!(response.contains(error));
    }
}
