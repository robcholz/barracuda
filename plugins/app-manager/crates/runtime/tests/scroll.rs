//! A list in an `overflow-y-auto` box scrolls with a dragging finger, carries
//! on after it lifts, and springs back from past either end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use barracuda_ui_runtime::{Document, DocumentAssets, Type, Value, format};

const VIEW: &[u8] = include_bytes!("fixtures/scroll.bapp/view.bvir");
const STYLES: &[u8] = include_bytes!("fixtures/scroll.bapp/styles.bin");
/// The display: a 40 px title over a 160 px list of 40 px rows.
const SIZE: (f32, f32) = (200.0, 200.0);
const ROW: f32 = 40.0;

/// The view showing `count` rows.
fn shown(count: i32) -> Document {
    shown_at(count, false)
}

/// The view showing `count` rows, at the list's bottom when `end`.
fn shown_at(count: i32, end: bool) -> Document {
    let mut document = Document::new(
        format::read(VIEW).unwrap(),
        DocumentAssets {
            styles: STYLES,
            fonts: &[],
        },
        SIZE.0,
        SIZE.1,
    )
    .unwrap();
    let slot = document.program().slot("rows").unwrap();
    let Type::Array(row) = &document.program().slots[slot].ty else {
        panic!("rows is an array")
    };
    let Type::Struct(row) = row.as_ref() else {
        panic!("a row is a struct")
    };
    let rows: Vec<Value> = (0..count)
        .map(|id| {
            let fields: Vec<Value> = row
                .fields
                .iter()
                .map(|(name, _)| match name.as_ref() {
                    "id" => Value::I32(id),
                    _ => Value::Str(format!("Row {id}").into()),
                })
                .collect();
            Value::Struct(fields.into())
        })
        .collect();
    document.set(slot, Value::Array(rows.into())).unwrap();
    let slot = document.program().slot("end").unwrap();
    document.set(slot, Value::Bool(end)).unwrap();
    document.frame();
    document
}

/// The row a tap 20 px into the list picks.
fn picked(document: &mut Document) -> i32 {
    let event = document.press_at(100.0, 60.0).expect("a row is there");
    match event.args[..] {
        [Value::I32(id)] => id,
        _ => panic!("pick takes a row id"),
    }
}

fn scrolls((top, bottom): (f32, f32)) -> bool {
    bottom > top
}

/// The list box and how far it is scrolled.
fn scrolled(document: &Document) -> f32 {
    let core = document.ui().core();
    let list = (1..512)
        .find(|&id| scrolls(core.scroll_bounds(id)))
        .expect("the list scrolls");
    core.scroll_offset(list)
}

/// Steps frames until the list rests; returns how many it took.
fn settle(document: &mut Document) -> usize {
    (1..600)
        .find(|_| {
            document.frame();
            !document.is_moving()
        })
        .expect("the list comes to rest")
}

/// Drags the finger `dy` px per frame for `frames` frames, then holds it
/// still until it has no speed left, then lifts it.
fn drag(document: &mut Document, dy: f32, frames: usize) {
    assert!(document.drag_start(100.0, 100.0));
    for _ in 0..frames {
        document.drag_by(dy);
        document.frame();
    }
    for _ in 0..20 {
        document.drag_by(0.0);
        document.frame();
    }
    document.drag_end();
}

#[test]
fn a_dragged_list_moves_with_the_finger_and_taps_follow_it() {
    let mut document = shown(20);
    assert_eq!(picked(&mut document), 0);
    drag(&mut document, -10.0, 10);
    settle(&mut document);
    assert!(
        (scrolled(&document) - 100.0).abs() < 1.0,
        "{}",
        scrolled(&document)
    );
    // The tap lands 20 px into the list, past the 100 px of rows that went
    // up out of it.
    assert_eq!(picked(&mut document), ((20.0 + 100.0) / ROW) as i32);
}

#[test]
fn a_flung_list_carries_on_and_rests_within_its_range() {
    let mut document = shown(20);
    assert!(document.drag_start(100.0, 100.0));
    for _ in 0..5 {
        document.drag_by(-30.0);
        document.frame();
    }
    document.drag_end();
    let frames = settle(&mut document);
    let offset = scrolled(&document);
    assert!(
        offset > 150.0 + 50.0,
        "it carried on past the drag: {offset}"
    );
    assert!(offset <= 20.0 * ROW - 160.0, "{offset}");
    assert!(frames > 10, "it slowed down over time: {frames}");
}

#[test]
fn a_list_dragged_past_its_top_resists_and_springs_back() {
    let mut document = shown(20);
    assert!(document.drag_start(100.0, 100.0));
    document.drag_by(50.0);
    document.frame();
    let core = document.ui().core();
    let list = (1..512)
        .find(|&id| scrolls(core.scroll_bounds(id)))
        .unwrap();
    let offset = core.scroll_offset(list);
    assert!(
        offset < 0.0 && offset > -50.0,
        "it follows the finger by less: {offset}"
    );
    document.drag_end();
    settle(&mut document);
    assert_eq!(scrolled(&document), 0.0);
}

#[test]
fn a_list_that_fits_does_not_take_the_drag() {
    let mut document = shown(3);
    assert!(!document.drag_start(100.0, 100.0));
    assert!(!document.is_moving());
}

#[test]
fn the_list_shows_its_indicator_while_it_moves_and_then_hides_it() {
    let mut document = shown(20);
    let mut words = Vec::new();
    document.draw(&mut words);
    let still = words.clone();
    drag(&mut document, -10.0, 4);
    document.draw(&mut words);
    let moving = words.clone();
    assert_ne!(moving, still);
    settle(&mut document);
    // Scrolled back to where it was, the list draws as it did before.
    assert!(document.drag_start(100.0, 100.0));
    for _ in 0..4 {
        document.drag_by(10.0);
        document.frame();
    }
    for _ in 0..20 {
        document.drag_by(0.0);
        document.frame();
    }
    document.drag_end();
    settle(&mut document);
    assert_eq!(scrolled(&document), 0.0);
    document.draw(&mut words);
    assert_eq!(words, still);
}

#[test]
fn rows_kept_at_the_bottom_show_the_last_and_scroll_down_to_the_first() {
    let mut document = shown_at(20, true);
    // The list shows its last four rows; the tap lands in the first of them.
    assert_eq!(picked(&mut document), 16);
    let core = document.ui().core();
    let list = (1..512)
        .find(|&id| scrolls(core.scroll_bounds(id)))
        .unwrap();
    assert_eq!(core.scroll_bounds(list), (-640.0, 0.0));
    drag(&mut document, 10.0, 8);
    settle(&mut document);
    assert!(
        (scrolled(&document) + 80.0).abs() < 1.0,
        "{}",
        scrolled(&document)
    );
    assert_eq!(picked(&mut document), 14);
}
