#![allow(missing_docs)]

use barracuda_event_router::{Event, JsonRpcSchema};
use barracuda_scheduler_component::{
    cancel::Cancel, event::SchedulerTriggered, schedule::Schedule,
};

#[test]
fn scheduler_exposes_two_bounded_json_rpcs() {
    assert_eq!(Schedule::ADDRESS, "scheduler.schedule");
    assert_eq!(Schedule::MAX_REQUEST_BYTES, 512);
    assert_eq!(Schedule::MAX_RESPONSE_BYTES, 32);
    assert_eq!(Cancel::ADDRESS, "scheduler.cancel");
    assert_eq!(Cancel::MAX_REQUEST_BYTES, 512);
    assert_eq!(Cancel::MAX_RESPONSE_BYTES, 64);
}

#[test]
fn schedule_schema_describes_public_json_contract() {
    let request = Schedule::REQUEST_SCHEMA.as_str();
    assert!(request.contains(r#""id""#));
    assert!(request.contains(r#""trigger""#));
    assert!(request.contains(r#""once""#));
    assert!(request.contains(r#""interval""#));
    assert!(request.contains(r#""every_seconds""#));
    assert!(request.contains(r#""type": "string""#));
    assert!(request.contains(r#""RFC3339 UTC timestamp matching time.now.utc.""#));
    assert!(!request.contains(r#""year""#));

    let response = Schedule::RESPONSE_SCHEMA.as_str();
    assert!(response.contains(r#""duplicate_id""#));
    assert!(response.contains(r#""time_unavailable""#));
    assert!(response.contains(r#""trigger_in_past""#));
    assert!(response.contains(r#""storage_unavailable""#));
}

#[test]
fn cancel_schema_describes_success_and_not_found() {
    let request = Cancel::REQUEST_SCHEMA.as_str();
    assert!(request.contains(r#""maxLength": 16"#));

    let response = Cancel::RESPONSE_SCHEMA.as_str();
    assert!(response.contains(r#""completed_runs""#));
    assert!(response.contains(r#""not_found""#));
}

#[test]
fn schedule_ids_are_direct_bounded_ascii() {
    let schedule = Schedule::REQUEST_SCHEMA.as_str();
    let cancel = Cancel::REQUEST_SCHEMA.as_str();
    for schema in [schedule, cancel] {
        assert!(schema.contains(r#""maxLength": 16"#));
        assert!(schema.contains(r#""pattern": "^[A-Za-z0-9_.-]+$""#));
    }
}

#[test]
fn scheduler_triggered_is_a_json_event_identity() {
    assert_eq!(SchedulerTriggered::ID, "scheduler.triggered");
}
