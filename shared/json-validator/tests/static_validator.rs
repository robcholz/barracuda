#![allow(clippy::expect_used)]

use json_validator::{validator, ErrorKind, Type, Validator};
use serde_json::json;

const TOOL_ARGUMENTS: Validator = validator!("tests/fixtures/tool.json");

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
