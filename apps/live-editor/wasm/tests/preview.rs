//! The preview runs a view as the device does.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use barracuda_live_editor_wasm::preview::Preview;
use serde_json::json;

const FIXTURES: &str = "../../../plugins/app-manager/crates/runtime/tests/fixtures";
const SYSTEM_FONTS: &str = "../../../plugins/system-fonts/filesystem/resources/fonts";

fn read(path: String) -> Vec<u8> {
    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn counter(system: &[&[u8]]) -> Preview {
    let program = read(format!("{FIXTURES}/counter.bvir"));
    let styles = read(format!("{FIXTURES}/counter.styles.bin"));
    let fonts: Vec<Vec<u8>> = ["1", "2", "12"]
        .iter()
        .map(|slot| read(format!("{FIXTURES}/counter.font-{slot}.bin")))
        .collect();
    let own: Vec<&[u8]> = fonts.iter().map(Vec::as_slice).collect();
    Preview::open(&program, &styles, &own, system, 360, 360).unwrap()
}

#[test]
fn values_are_checked_as_view_set_checks_them() {
    let mut preview = counter(&[]);
    preview.set("count", &json!(3)).unwrap();
    preview.set("count", &json!(4.0)).unwrap();
    preview
        .set("history", &json!([{ "id": 1, "label": "+1 = 1" }]))
        .unwrap();
    preview.set("history", &json!({})).unwrap();
    assert_eq!(
        preview.set("count", &json!("3")).unwrap_err(),
        "'count': expected an integer"
    );
    assert_eq!(
        preview.set("count", &json!(1.5)).unwrap_err(),
        "'count': expected an i32"
    );
    assert_eq!(
        preview.set("total", &json!(1)).unwrap_err(),
        "the view has no value named 'total'"
    );
    assert_eq!(
        preview.set("history", &json!([{ "id": 1 }])).unwrap_err(),
        "'history': expected a string"
    );
    assert_eq!(
        preview.contract(),
        json!({
            "values": { "count": "i32", "history": "Entry[]" },
            "handlers": { "increment": ["i32"], "reset": [] },
        })
    );
}

#[test]
fn a_frame_draws_once_and_taps_reach_the_handlers() {
    let mut preview = counter(&[]);
    preview.set("count", &json!(7)).unwrap();
    preview.frame();
    let pixels = preview.draw(1).unwrap().to_vec();
    assert_eq!(pixels.len(), 360 * 360 * 4);
    assert!(pixels.chunks(4).any(|pixel| pixel[..3] != pixels[..3]));
    assert!(
        preview.draw(1).is_none(),
        "an unchanged picture is not drawn again"
    );
    assert_eq!(preview.draw(2).unwrap().len(), 720 * 720 * 4);
    assert!(preview.texts().iter().any(|text| text == "7"));

    let mut calls = Vec::new();
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            if let Some(call) = preview.press(x as f32, y as f32) {
                calls.push(call);
            }
        }
    }
    assert!(calls.contains(&json!({ "handler": "increment", "args": [1] })));
    assert!(calls.contains(&json!({ "handler": "reset", "args": [] })));
}

#[test]
fn system_fonts_fill_the_slots_a_view_ships_none_for() {
    let system: Vec<Vec<u8>> = std::fs::read_dir(SYSTEM_FONTS)
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    let system: Vec<&[u8]> = system.iter().map(Vec::as_slice).collect();
    let mut preview = counter(&system);
    preview.frame();
    assert!(preview.draw(1).is_some());
    assert_eq!(
        Preview::open(b"BVIR", b"", &[], &[], 360, 360)
            .err()
            .unwrap(),
        "view.bvir is not BVIR"
    );
}
