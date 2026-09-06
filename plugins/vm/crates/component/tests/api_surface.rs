#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use barracuda_event_router::{Event, JsonRpcSchema};
use barracuda_vm_component::run::{
    Cancel, Finished, Input, InputRequired, Output, Run, VM_DIAGNOSTIC_BYTES, VM_EVENT_INPUT_BYTES,
    VM_JSON_REQUEST_BYTES, VM_JSON_RESPONSE_BYTES, VM_OUTPUT_CHUNK_BYTES,
};

#[test]
fn vm_exposes_bounded_json_rpc_contracts() {
    assert_eq!(Run::ADDRESS, "vm.run");
    assert_eq!(Input::ADDRESS, "vm.input");
    assert_eq!(Cancel::ADDRESS, "vm.cancel");
    assert_eq!(Run::MAX_REQUEST_BYTES, 512);
    assert_eq!(Input::MAX_REQUEST_BYTES, 512);
    assert_eq!(Cancel::MAX_REQUEST_BYTES, 32);
    assert_eq!(Run::MAX_RESPONSE_BYTES, 48);
    assert_eq!(Input::MAX_RESPONSE_BYTES, 48);
    assert_eq!(Cancel::MAX_RESPONSE_BYTES, 48);
    assert_eq!(VM_JSON_REQUEST_BYTES, 512);
    assert_eq!(VM_JSON_RESPONSE_BYTES, 48);

    for schema in [
        Run::REQUEST_SCHEMA,
        Run::RESPONSE_SCHEMA,
        Input::REQUEST_SCHEMA,
        Input::RESPONSE_SCHEMA,
        Cancel::REQUEST_SCHEMA,
        Cancel::RESPONSE_SCHEMA,
    ] {
        serde_json::from_str::<serde_json::Value>(schema.as_str()).expect("valid JSON Schema");
    }
    assert!(Run::REQUEST_SCHEMA.as_str().contains(r#""source""#));
    assert!(
        Run::RESPONSE_SCHEMA
            .as_str()
            .contains("runtime_unavailable")
    );
    assert!(Input::REQUEST_SCHEMA.as_str().contains(r#""eof""#));
    assert!(
        Input::RESPONSE_SCHEMA
            .as_str()
            .contains("input_backpressure")
    );
    assert!(Cancel::RESPONSE_SCHEMA.as_str().contains("run_not_found"));
}

#[test]
fn vm_stream_events_have_stable_ids_and_small_payload_bounds() {
    assert_eq!(Output::ID, "vm.output");
    assert_eq!(InputRequired::ID, "vm.input_required");
    assert_eq!(Finished::ID, "vm.finished");
    assert_eq!(VM_OUTPUT_CHUNK_BYTES, 48);
    assert_eq!(VM_DIAGNOSTIC_BYTES, 48);
    assert_eq!(VM_EVENT_INPUT_BYTES, 416);
}
