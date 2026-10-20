//! The browser's linker gives what `ui-link` gives.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use barracuda_live_editor_wasm::link_view;

const FIXTURES: &str = "../../../plugins/app-manager/crates/runtime/tests/fixtures";

#[test]
fn a_view_links_to_the_committed_bvir() {
    let json = std::fs::read(format!("{FIXTURES}/counter.view.json")).unwrap();
    let committed = std::fs::read(format!("{FIXTURES}/counter.bvir")).unwrap();
    assert_eq!(link_view(&json).unwrap(), committed);
}

#[test]
fn a_bad_view_is_explained() {
    assert_eq!(
        link_view(b"not json").unwrap_err(),
        "the view is not View IR JSON"
    );
    assert_eq!(
        link_view(br#"{"version":9,"types":[],"components":[]}"#).unwrap_err(),
        "the View IR version is not 1"
    );
}
