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

/// A text node's rows, one per line, and its rect.
type Shown = (String, (f32, f32, f32, f32));

/// Each text node's rows and rect, in tree order.
fn texts(document: &Document) -> Vec<Shown> {
    fn walk(core: &pocketjs_core::Ui, id: i32, out: &mut Vec<Shown>) {
        for &child in core.node_children(id) {
            if let Some(text) = core.node_text(child).filter(|text| !text.is_empty()) {
                let rows =
                    pocketjs_core::text::with_soft_breaks(text, &core.node_soft_breaks(child));
                out.push((rows, core.layout_of(child).unwrap()));
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
    let (_, (_, _, value, _)) = &shown[2];
    assert!(wrapped.contains('\n'), "{wrapped:?}");
    // The label keeps to the room the value's box leaves: 160 px inside the
    // column, less 8 px of gap and the box's 28 px of padding.
    assert!(
        *width + 8.0 + value + 28.0 <= 160.5,
        "{width} beside {value}"
    );
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
fn a_box_sized_by_its_text_keeps_it_on_one_line() {
    // The value's bordered, padded box is as wide as its text.
    let document = shown(200.0, "Standby", "Volume", "Thanks!");
    let shown = texts(&document);
    assert_eq!(shown[2].0, "Thanks!");
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

#[test]
fn layout_breaks_where_the_wrap_text_op_does() {
    for width in [160.0, 220.0, 300.0] {
        let document = shown(width, LONG, "Volume", "80%");
        let core = document.ui().core();
        let node = (1..256)
            .find(|&id| core.node_text(id) == Some(LONG))
            .unwrap();
        let (_, _, box_width, _) = core.layout_unrounded_of(node).unwrap();
        let slot = core.resolved_style(node).unwrap().font_slot as u8;
        assert!(!core.node_soft_breaks(node).is_empty(), "at {width}");
        // The run is ASCII, so the op's UTF-16 columns are byte offsets.
        assert_eq!(
            core.node_soft_breaks(node),
            core.wrap_text(LONG, slot, box_width),
            "at {width}"
        );
    }
}
