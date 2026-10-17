//! Runs the counter app's compiled view end to end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use barracuda_ui_runtime::format::{self, FormatError};
use barracuda_ui_runtime::{Document, DocumentAssets, Program, Type, Value, ValueError, ViewEvent};
use std::rc::Rc;

const VIEW_BVIR: &[u8] = include_bytes!("fixtures/counter.bvir");
const STYLES: &[u8] = include_bytes!("fixtures/counter.styles.bin");

fn program() -> Program {
    format::read(VIEW_BVIR).expect("fixture loads")
}

fn document() -> Document {
    Document::new(
        program(),
        DocumentAssets {
            styles: STYLES,
            fonts: &[],
        },
        360.0,
        360.0,
    )
    .expect("document")
}

fn entry(program: &Program, id: i32, label: &str) -> Value {
    let Type::Array(element) = &program.slots[program.slot("history").unwrap()].ty else {
        panic!("history is an array")
    };
    assert!(matches!(**element, Type::Struct(_)));
    Value::Struct(Rc::from(vec![Value::I32(id), Value::Str(label.into())]))
}

/// Taps a grid over the viewport and returns the first event.
fn tap_anything(document: &mut Document) -> Option<ViewEvent> {
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            if let Some(event) = document.press_at(x as f32, y as f32) {
                return Some(event);
            }
        }
    }
    None
}

#[test]
fn contract_keeps_names_and_types() {
    let program = program();
    assert_eq!(
        program
            .slots
            .iter()
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>(),
        ["count", "history"]
    );
    assert_eq!(program.slots[0].ty, Type::I32);
    let Type::Array(element) = &program.slots[1].ty else {
        panic!("history is an array")
    };
    let Type::Struct(entry) = &**element else {
        panic!("history holds structs")
    };
    assert_eq!(entry.name, "Entry");
    assert_eq!(
        entry
            .fields
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["id", "label"]
    );
    assert_eq!(
        program
            .handlers
            .iter()
            .map(|handler| handler.name.as_str())
            .collect::<Vec<_>>(),
        ["increment", "reset"]
    );
    assert_eq!(program.handlers[0].params, [Type::I32]);
    assert!(program.handlers[1].params.is_empty());
}

#[test]
fn every_truncation_and_trailing_byte_is_rejected() {
    for cut in 0..VIEW_BVIR.len() {
        assert!(
            format::read(&VIEW_BVIR[..cut]).is_err(),
            "accepted a file cut at {cut}"
        );
    }
    let mut longer = VIEW_BVIR.to_vec();
    longer.push(0);
    assert_eq!(format::read(&longer), Err(FormatError::TrailingBytes));
    let mut wrong = VIEW_BVIR.to_vec();
    wrong[0] = b'X';
    assert_eq!(format::read(&wrong), Err(FormatError::Magic));
    let mut old = VIEW_BVIR.to_vec();
    old[4] = 1;
    assert_eq!(format::read(&old), Err(FormatError::Version(1)));
}

#[test]
fn values_drive_the_tree() {
    let mut document = document();
    assert_eq!(document.texts(), ["0", "+1", "Reset"]);

    let program = document.program().clone();
    let count = program.slot("count").unwrap();
    let history = program.slot("history").unwrap();
    document.set(count, Value::I32(2)).unwrap();
    document
        .set(
            history,
            Value::Array(Rc::from(vec![
                entry(&program, 2, "+1 = 2"),
                entry(&program, 1, "+1 = 1"),
            ])),
        )
        .unwrap();
    assert!(document.is_dirty());
    document.frame();
    assert!(!document.is_dirty());
    assert_eq!(document.texts(), ["2", "+1", "Reset", "+1 = 2", "+1 = 1"]);

    // Reordering and removing rows keeps the keyed list in step.
    document
        .set(
            history,
            Value::Array(Rc::from(vec![entry(&program, 1, "+1 = 1")])),
        )
        .unwrap();
    document.frame();
    assert_eq!(document.texts(), ["2", "+1", "Reset", "+1 = 1"]);

    // The `Show` branch unmounts the list when the count drops to zero.
    document.set(count, Value::I32(0)).unwrap();
    document.frame();
    assert_eq!(document.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn writes_are_type_checked() {
    let mut document = document();
    let count = document.program().slot("count").unwrap();
    assert_eq!(
        document.set(count, Value::Str("2".into())),
        Err(ValueError::TypeMismatch)
    );
    assert_eq!(
        document.set(99, Value::I32(1)),
        Err(ValueError::UnknownName)
    );
    let history = document.program().slot("history").unwrap();
    let bad_row = Value::Array(Rc::from(vec![Value::Struct(Rc::from(vec![Value::Str(
        "x".into(),
    )]))]));
    assert_eq!(
        document.set(history, bad_row),
        Err(ValueError::TypeMismatch)
    );
    assert!(!document.is_dirty());
}

#[test]
fn tapping_a_button_calls_its_handler() {
    let mut document = document();
    let event = tap_anything(&mut document).expect("a button is under some point");
    let handler = &document.program().handlers[event.handler];
    match handler.name.as_str() {
        "increment" => assert_eq!(event.args, [Value::I32(1)]),
        "reset" => assert!(event.args.is_empty()),
        other => panic!("unexpected handler {other}"),
    }
    // Text and empty space are not pressable.
    assert_eq!(document.press_at(1.0, 1.0), None);
}
