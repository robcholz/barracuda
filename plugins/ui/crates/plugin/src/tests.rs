#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

extern crate std;

use crate::datum::{ViewType, ViewValue};
use crate::service::{DEFAULT_VIEWPORT, UiAssets, UiService, UiSession};
use crate::view::{MAX_ARRAY_ITEMS, ViewChannel, ViewError};
use alloc::{string::String, vec, vec::Vec};
use futures_lite::future;

const VIEW: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.bvir");
const STYLES: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.styles.bin");

fn assets() -> UiAssets {
    UiAssets {
        view: VIEW.to_vec(),
        styles: STYLES.to_vec(),
        fonts: Vec::new(),
    }
}

fn entry(id: i32, label: &str) -> ViewValue {
    ViewValue::List(vec![ViewValue::I32(id), ViewValue::Str(label.into())])
}

fn next_call(channel: &ViewChannel) -> Option<(String, Vec<ViewValue>)> {
    future::block_on(channel.next()).map(|call| (call.handler, call.args))
}

/// Finds a point that presses `handler`; every probe that hits queues a call.
/// "Reset" sits right of "+1", so it is searched for from the right.
fn find(session: &UiSession, handler: &str) -> (f32, f32) {
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            let x = if handler == "reset" { 354 - x } else { x };
            let (x, y) = (x as f32, y as f32);
            if session.press_at(x, y).as_deref() == Some(handler) {
                return (x, y);
            }
        }
    }
    panic!("no '{handler}' button")
}

#[test]
fn writes_drive_the_view_and_taps_arrive_as_calls() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);

    channel.set("count", ViewValue::I32(2)).expect("count");
    channel
        .set(
            "history",
            ViewValue::List(vec![entry(2, "two"), entry(1, "one")]),
        )
        .expect("history");
    assert_eq!(session.texts(), ["2", "+1", "Reset", "two", "one"]);

    find(&session, "increment");
    assert_eq!(
        next_call(&channel),
        Some(("increment".into(), vec![ViewValue::I32(1)]))
    );
}

#[test]
fn writes_are_checked_against_the_contract() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel();
    assert_eq!(channel.value_type("count"), Some(&ViewType::I32));
    assert_eq!(channel.value_type("missing"), None);
    for (name, value, error) in [
        ("count", ViewValue::Str("two".into()), ViewError::Mismatch),
        ("count", ViewValue::F64(1.0), ViewError::Mismatch),
        ("missing", ViewValue::I32(1), ViewError::UnknownValue),
        (
            "history",
            ViewValue::List(vec![ViewValue::List(vec![
                ViewValue::Str("x".into()),
                ViewValue::Str("y".into()),
            ])]),
            ViewError::Mismatch,
        ),
        ("history", ViewValue::I32(3), ViewError::Mismatch),
        (
            "history",
            ViewValue::List(vec![entry(1, "one"); MAX_ARRAY_ITEMS + 1]),
            ViewError::Mismatch,
        ),
    ] {
        assert_eq!(
            channel.set(name, value.clone()),
            Err(error),
            "{name} {value:?}"
        );
    }
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn closing_ends_the_view_for_its_owner() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    assert!(!session.is_closed());
    channel.close();
    assert!(session.is_closed());
    assert!(!channel.is_open());
    assert_eq!(
        channel.set("count", ViewValue::I32(1)),
        Err(ViewError::Closed)
    );
    assert_eq!(next_call(&channel), None);
    assert_eq!(session.press_at(0.0, 0.0), None);

    // Dropping the session closes channels that outlive it.
    let other = service.open(&assets()).expect("view opens");
    let channel = other.channel().clone();
    drop(other);
    assert!(!channel.is_open());
}

#[test]
fn a_suspended_view_keeps_its_values_and_catches_up_on_resume() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let channel = session.channel().clone();
    channel.set("count", ViewValue::I32(1)).expect("count");
    channel
        .set("history", ViewValue::List(vec![entry(1, "one")]))
        .expect("history");
    assert_eq!(session.texts(), ["1", "+1", "Reset", "one"]);

    session.suspend();
    assert!(session.is_suspended());
    assert!(session.texts().is_empty());
    assert_eq!(session.press_at(180.0, 180.0), None);
    // The owner keeps writing while suspended; the writes wait for the resume.
    channel.set("count", ViewValue::I32(5)).expect("count");

    session.resume(&assets()).expect("resume");
    assert!(!session.is_suspended());
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);

    // Rejected assets leave the view suspended with its values.
    session.suspend();
    let mut broken = assets();
    broken.view.truncate(10);
    assert!(session.resume(&broken).is_err());
    assert!(session.is_suspended());
    session.resume(&assets()).expect("resume");
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);
}

#[test]
fn clearing_drops_calls_the_owner_has_not_received() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    find(&session, "increment");
    session.clear_events();
    find(&session, "reset");
    assert_eq!(
        next_call(session.channel()).map(|(handler, _)| handler),
        Some("reset".into())
    );
}
