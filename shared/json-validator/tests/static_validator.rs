#![allow(clippy::expect_used)]

use json_validator::{validator, ErrorKind, Type, Validator};
use serde_json::json;

const TOOL_ARGUMENTS: Validator = validator!("tests/fixtures/tool.json");
const RPC_DOCUMENT: Validator = validator!("tests/fixtures/rpc.json");
const RECURSIVE_DOCUMENT: Validator = validator!("tests/fixtures/recursive.json");

#[test]
fn validates_the_current_tool_schema_subset() {
    assert!(TOOL_ARGUMENTS
        .validate(&json!({
            "name": "worker",
            "labels": ["one", "two"],
            "limit": 2,
            "mode": "fast"
        }))
        .is_ok());
}

#[test]
fn rejects_missing_wrong_typed_and_unknown_fields() {
    assert!(TOOL_ARGUMENTS
        .validate(&json!({"labels": [], "limit": 2, "mode": "fast"}))
        .is_err());
    assert!(TOOL_ARGUMENTS
        .validate(&json!({"name": 1, "labels": [], "limit": 2, "mode": "fast"}))
        .is_err());
    assert!(TOOL_ARGUMENTS
        .validate(&json!({
            "name": "worker",
            "labels": [],
            "limit": 2,
            "mode": "fast",
            "extra": true
        }))
        .is_err());
}

#[test]
fn rejects_empty_and_whitespace_only_strings() {
    for name in ["", "   ", "\t\n"] {
        let error = TOOL_ARGUMENTS
            .validate(&json!({
                "name": name,
                "labels": [],
                "limit": 2,
                "mode": "fast"
            }))
            .expect_err("trimmed-empty name must fail");
        assert_eq!(error.path, "$.name");
        assert_eq!(error.kind, ErrorKind::TrimmedEmpty);
    }
}

#[test]
fn rejects_nested_item_enum_and_minimum_violations() {
    let item_error = TOOL_ARGUMENTS
        .validate(&json!({"name": "worker", "labels": [1], "limit": 2, "mode": "fast"}))
        .expect_err("number item must fail");
    assert_eq!(item_error.path, "$.labels[0]");
    assert_eq!(
        item_error.kind,
        ErrorKind::Type {
            expected: Type::String
        }
    );

    let minimum_error = TOOL_ARGUMENTS
        .validate(&json!({"name": "worker", "labels": [], "limit": 0, "mode": "fast"}))
        .expect_err("minimum must fail");
    assert_eq!(minimum_error.path, "$.limit");
    assert_eq!(minimum_error.kind, ErrorKind::Minimum);

    let enum_error = TOOL_ARGUMENTS
        .validate(&json!({"name": "worker", "labels": [], "limit": 2, "mode": "turbo"}))
        .expect_err("enum must fail");
    assert_eq!(enum_error.path, "$.mode");
    assert_eq!(enum_error.kind, ErrorKind::Enum);
}

#[test]
fn validates_borrowed_json_without_a_value_tree() {
    assert!(RPC_DOCUMENT
        .validate_str(
            r#"{"action":"start","id":"task-1","sequence":0,"at":"2026-09-05T12:30:45.123Z"}"#,
        )
        .is_ok());
    assert!(RPC_DOCUMENT
        .validate_str(r#"{"action":"chunk","id":"task_2","sequence":1,"data":"YQ=="}"#)
        .is_ok());
}

#[test]
fn borrowed_validation_enforces_composition_and_constraints() {
    for invalid in [
        r#"{"action":"start","id":"","sequence":0,"at":"2026-09-05T12:30:45.123Z"}"#,
        r#"{"action":"start","id":"bad/id","sequence":0,"at":"2026-09-05T12:30:45.123Z"}"#,
        r#"{"action":"start","id":"task-1","sequence":1,"at":"2026-09-05T12:30:45.123Z"}"#,
        r#"{"action":"start","id":"task-1","sequence":0,"at":"not-a-date"}"#,
        r#"{"action":"chunk","id":"task-1","sequence":0,"data":"YQ=="}"#,
        r#"{"action":"chunk","id":"task-1","sequence":1,"data":"%%%"}"#,
        r#"{"action":"chunk","id":"task-1","sequence":1,"data":"YQ==","extra":true}"#,
    ] {
        assert!(RPC_DOCUMENT.validate_str(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn borrowed_validation_follows_recursive_local_references() {
    assert!(RECURSIVE_DOCUMENT
        .validate_str(r#"{"name":"root","child":{"name":"leaf"}}"#)
        .is_ok());
    let error = RECURSIVE_DOCUMENT
        .validate_str(r#"{"name":"root","child":{"name":1}}"#)
        .expect_err("nested reference must retain the schema contract");
    assert_eq!(error.path, "$.child.name");
}
