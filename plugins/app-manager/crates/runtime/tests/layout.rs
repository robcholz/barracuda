//! Rects a build solved travel through BVIR and hold on the device.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use barracuda_ui_runtime::{
    Document, DocumentAssets, Layout, Node, Program, compiler, format, pocketjs_core::spec,
};

const VIEW_JSON: &[u8] = include_bytes!("fixtures/counter.view.json");
const STYLES: &[u8] = include_bytes!("fixtures/counter.styles.bin");

/// The counter view with `layout` on its root element.
fn linked(layout: serde_json::Value) -> Program {
    let mut ir: serde_json::Value = serde_json::from_slice(VIEW_JSON).unwrap();
    ir["components"][0]["nodes"][0]["layout"] = layout;
    compiler::link(&serde_json::to_vec(&ir).unwrap()).expect("links")
}

fn root_layout(program: &Program) -> Layout {
    let Node::Element(element) = &program.nodes[0] else {
        panic!("the root is an element")
    };
    element.layout
}

#[test]
fn a_static_rect_links_round_trips_and_holds() {
    let program = linked(serde_json::json!({"mode": "static", "rect": [10, 20, 200, 100]}));
    assert_eq!(
        root_layout(&program),
        Layout::Static {
            x: 10.0,
            y: 20.0,
            w: 200.0,
            h: 100.0
        }
    );
    assert_eq!(format::read(&format::write(&program)).unwrap(), program);

    let mut document = Document::new(
        program,
        DocumentAssets {
            styles: STYLES,
            fonts: &[],
        },
        360.0,
        360.0,
    )
    .expect("document");
    document.frame();
    let core = document.ui().core();
    let root = core.node_children(spec::ROOT_ID)[0];
    // Without the rect the root fills the 360 × 360 viewport.
    assert_eq!(core.layout_of(root), Some((10.0, 20.0, 200.0, 100.0)));
}

#[test]
fn a_formula_rect_round_trips() {
    let program = linked(serde_json::json!({"mode": "formula"}));
    assert_eq!(root_layout(&program), Layout::Formula);
    assert_eq!(format::read(&format::write(&program)).unwrap(), program);
}

#[test]
fn views_without_rects_are_unchanged() {
    let program = compiler::link(VIEW_JSON).unwrap();
    assert_eq!(root_layout(&program), Layout::Live);
}

#[test]
fn a_rect_that_is_not_a_number_is_refused() {
    let mut ir: serde_json::Value = serde_json::from_slice(VIEW_JSON).unwrap();
    ir["components"][0]["nodes"][0]["layout"] =
        serde_json::json!({"mode": "static", "rect": [1, 2]});
    assert_eq!(
        compiler::link(&serde_json::to_vec(&ir).unwrap()),
        Err(compiler::LinkError::Json)
    );
}

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

/// Every element's rect, in tree order.
fn rects(document: &Document) -> Vec<(f32, f32, f32, f32)> {
    fn walk(
        core: &barracuda_ui_runtime::pocketjs_core::Ui,
        id: i32,
        out: &mut Vec<(f32, f32, f32, f32)>,
    ) {
        for &child in core.node_children(id) {
            out.push(core.layout_of(child).unwrap());
            walk(core, child, out);
        }
    }
    let mut out = Vec::new();
    walk(document.ui().core(), spec::ROOT_ID, &mut out);
    out
}

/// Lays `program` out at `size` with the system fonts.
fn laid_out(program: Program, styles: &[u8], (width, height): (f32, f32)) -> Document {
    let fonts = system_fonts();
    let fonts: Vec<&[u8]> = fonts.iter().map(Vec::as_slice).collect();
    let mut document = Document::new(
        program,
        DocumentAssets {
            styles,
            fonts: &fonts,
        },
        width,
        height,
    )
    .expect("document");
    document.frame();
    document
}

/// The program with every rect left to the solver.
fn live(mut program: Program) -> Program {
    fn clear(nodes: &mut [Node]) {
        for node in nodes {
            match node {
                Node::Element(element) => {
                    element.layout = Layout::Live;
                    clear(&mut element.children);
                }
                Node::If(branches) => {
                    for branch in branches {
                        clear(&mut branch.children);
                    }
                }
                Node::For(each) => clear(&mut each.children),
            }
        }
    }
    clear(&mut program.nodes);
    program
}

fn statics(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|node| match node {
            Node::Element(element) => {
                usize::from(matches!(element.layout, Layout::Static { .. }))
                    + statics(&element.children)
            }
            _ => 0,
        })
        .sum()
}

#[test]
fn a_build_for_a_display_solves_the_rects_the_device_would() {
    for (bvir, styles, size) in [
        (
            include_bytes!("fixtures/baked-360x360.bapp/view.bvir").as_slice(),
            include_bytes!("fixtures/baked-360x360.bapp/styles.bin").as_slice(),
            (360.0, 360.0),
        ),
        (
            include_bytes!("fixtures/baked-320x240.bapp/view.bvir").as_slice(),
            include_bytes!("fixtures/baked-320x240.bapp/styles.bin").as_slice(),
            (320.0, 240.0),
        ),
    ] {
        let program = format::read(bvir).expect("fixture loads");
        // The card, its title, the row, its two boxes, and the label.
        assert_eq!(statics(&program.nodes), 6, "{size:?}");
        let baked = laid_out(program.clone(), styles, size);
        let solved = laid_out(live(program), styles, size);
        assert_eq!(rects(&baked), rects(&solved), "{size:?}");
        // The root fills the display it was built for.
        assert_eq!(rects(&baked)[0], (0.0, 0.0, size.0, size.1));
    }
}

#[test]
fn the_same_view_is_solved_differently_for_each_display() {
    let large = format::read(include_bytes!("fixtures/baked-360x360.bapp/view.bvir")).unwrap();
    let small = format::read(include_bytes!("fixtures/baked-320x240.bapp/view.bvir")).unwrap();
    assert_ne!(large, small);
    assert_eq!(live(large), live(small));
}
