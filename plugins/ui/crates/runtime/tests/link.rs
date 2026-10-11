//! The committed counter fixture is exactly what the linker produces.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use barracuda_ui_runtime::{compiler, format};

#[test]
fn committed_bvir_is_what_the_linker_produces() {
    let linked = compiler::link(include_bytes!("fixtures/counter.view.json")).expect("links");
    let committed = include_bytes!("fixtures/counter.bvir");
    assert_eq!(format::write(&linked), committed);
    assert_eq!(format::read(committed).expect("reads"), linked);
}

#[test]
fn unsupported_ir_is_reported() {
    assert_eq!(compiler::link(b"{}"), Err(compiler::LinkError::Json));
    assert_eq!(
        compiler::link(br#"{"version":9,"types":[],"components":[]}"#),
        Err(compiler::LinkError::Version)
    );
}
