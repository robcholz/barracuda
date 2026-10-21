//! Text wraps to the room it has, as on a web page.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use barracuda_ui_runtime::{Document, DocumentAssets, Value, format, pocketjs_core};

const VIEW: &[u8] = include_bytes!("fixtures/wrap.bapp/view.bvir");
const STYLES: &[u8] = include_bytes!("fixtures/wrap.bapp/styles.bin");
const LONG: &str = "Sunny and warm, with a light breeze from the west";

/// The system fonts, as the device hands them to every view.
fn system_fonts() -> Vec<Vec<u8>> {
    let dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../system-fonts/filesystem/resources/fonts"
    );
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect()
}

/// The view at `width` × 200 showing `message`, `label` and `value`.
fn shown(width: f32, message: &str, label: &str, value: &str) -> Document {
    let fonts = system_fonts();
    let fonts: Vec<&[u8]> = fonts.iter().map(Vec::as_slice).collect();
    let mut document = Document::new(
        format::read(VIEW).unwrap(),
        DocumentAssets {
            styles: STYLES,
            fonts: &fonts,
        },
        width,
        200.0,
    )
    .unwrap();
    write(&mut document, message, label, value);
    document
}

fn write(document: &mut Document, message: &str, label: &str, value: &str) {
    for (name, text) in [("message", message), ("label", label), ("value", value)] {
        let slot = document.program().slot(name).unwrap();
        document.set(slot, Value::Str(text.into())).unwrap();
    }
    document.frame();
}

/// Each text node's shown text and rect, in tree order.
fn texts(document: &Document) -> Vec<(String, (f32, f32, f32, f32))> {
    fn walk(core: &pocketjs_core::Ui, id: i32, out: &mut Vec<(String, (f32, f32, f32, f32))>) {
        for &child in core.node_children(id) {
            if let Some(text) = core.node_text(child).filter(|text| !text.is_empty()) {
                out.push((text.into(), core.layout_of(child).unwrap()));
            }
            walk(core, child, out);
        }
    }
    let mut out = Vec::new();
    walk(document.ui().core(), pocketjs_core::spec::ROOT_ID, &mut out);
    out
}

#[test]
fn a_paragraph_wraps_inside_its_padded_column() {
    let document = shown(200.0, LONG, "Volume", "80%");
    let shown = texts(&document);
    let (message, (_, _, width, height)) = &shown[0];
    // The column is 200 wide with 20 px on each side.
    assert!(message.contains('\n'), "{message:?}");
    assert!(*width <= 160.0, "{width}");
    assert!(*height > 30.0, "one line is about 20 px: {height}");
    assert_eq!(message.replace('\n', " "), LONG);
    // What the view was given, not how it shows it.
    assert_eq!(document.texts(), [LONG, "Volume", "80%"]);
}

#[test]
fn text_that_fits_stays_on_one_line() {
    let document = shown(200.0, "Standby", "Volume", "80%");
    assert!(
        texts(&document)
            .iter()
            .all(|(text, _)| !text.contains('\n'))
    );
}

#[test]
fn a_flex_share_wraps_at_its_share() {
    let label = "Brightness of the display";
    let document = shown(200.0, "Standby", label, "80%");
    let shown = texts(&document);
    let (wrapped, (_, _, width, _)) = &shown[1];
    let (_, (value_x, _, _, _)) = &shown[2];
    assert!(wrapped.contains('\n'), "{wrapped:?}");
    // The label keeps to the room the value leaves, 8 px of gap before it.
    assert!(*width + 8.0 <= *value_x + 0.5, "{width} then {value_x}");
}

#[test]
fn a_wider_display_wraps_less_and_new_text_rewraps() {
    let narrow = shown(200.0, LONG, "Volume", "80%");
    let wide = shown(400.0, LONG, "Volume", "80%");
    let lines = |document: &Document| texts(document)[0].0.lines().count();
    assert!(lines(&wide) < lines(&narrow));

    let mut document = narrow;
    write(&mut document, "Standby", "Volume", "80%");
    assert_eq!(texts(&document)[0].0, "Standby");
    write(&mut document, LONG, "Volume", "80%");
    assert_eq!(
        lines(&document),
        lines(&shown(200.0, LONG, "Volume", "80%"))
    );
}

#[test]
fn wrapping_settles() {
    let mut document = shown(200.0, LONG, "Brightness of the display", "80%");
    let before = texts(&document);
    for _ in 0..5 {
        document.frame();
    }
    assert_eq!(texts(&document), before);
}
