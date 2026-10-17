//! Per-Board Plugin enablement and peripheral claim resolution.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::{fs, path::Path};

use barracuda_plugin_build::selection::{discover, select, ClaimedDevice, Filled, Selection};

const REPOSITORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../..");

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn plugin(root: &Path, id: &str, depends_on: &[&str], extra: &str) {
    let depends_on = depends_on
        .iter()
        .map(|id| format!("{id:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    write(
        root,
        &format!("plugins/{id}/plugin.toml"),
        &format!("id = {id:?}\ndepends-on = [{depends_on}]\ndescription = \"Test.\"\n{extra}"),
    );
    write(root, &format!("plugins/{id}/crates/plugin/Cargo.toml"), "");
}

/// A workspace with a display and a touch implementation, a `headless` and a
/// `screen` profile, and Plugins shaped like the real ones.
fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path();
    for (kind, id) in [
        ("display", "gdeh0154d67-display"),
        ("touch", "cst816t-touch"),
    ] {
        let source = Path::new(REPOSITORY)
            .join("peripherals/impl")
            .join(kind)
            .join(id)
            .join("peripheral.yml");
        let text = fs::read_to_string(&source).unwrap_or_else(|_| panic!("{}", source.display()));
        write(
            root_path,
            &format!("peripherals/impl/{kind}/{id}/peripheral.yml"),
            &text,
        );
    }
    write(
        root_path,
        "boards/profiles/headless.yml",
        "enable: [workflow]\n",
    );
    write(
        root_path,
        "boards/profiles/screen.yml",
        "extends: headless\nenable: [app-manager, display]\n",
    );
    plugin(root_path, "workflow", &[], "");
    plugin(
        root_path,
        "ui",
        &["workflow"],
        "[peripherals.display]\nmain = \"primary\"\n[peripherals.touch]\nmain = { fill = \"primary\", optional = true }\n",
    );
    plugin(root_path, "app-manager", &["ui"], "");
    plugin(
        root_path,
        "display",
        &[],
        "[peripherals.display]\nrest = \"*\"\n",
    );
    root
}

fn board(root: &Path, body: &str) -> barracuda_board_config::BoardDefinition {
    barracuda_board_config::parse(&format!(
        "name: test\nhardware:\n  chip: test\nnative-layout:\n  artifact: memory.x\n{body}"
    ))
    .unwrap_or_else(|error| panic!("{error}: {}", root.display()))
}

fn run(root: &Path, body: &str, local: &[&str]) -> Result<Selection, String> {
    let plugins = discover(root).unwrap();
    let local = local
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    select(root, &board(root, body), &plugins, &local).map_err(|error| error.to_string())
}

fn device(name: &str, index: u8) -> ClaimedDevice {
    ClaimedDevice {
        name: name.to_owned(),
        index,
    }
}

fn claims(selection: &Selection, id: &str) -> Vec<(String, String, Filled)> {
    selection
        .claims(id)
        .map(|(kind, slot, filled)| (kind.to_owned(), slot.to_owned(), filled.clone()))
        .collect()
}

const ONE_DISPLAY: &str =
    "peripherals:\n  devices:\n    panel:\n      implementation: gdeh0154d67-display\n";
const THREE_DISPLAYS: &str = "peripherals:\n  devices:\n    front:\n      implementation: gdeh0154d67-display\n      primary: true\n    back-eink:\n      implementation: gdeh0154d67-display\n    status:\n      implementation: gdeh0154d67-display\n";

#[test]
fn boards_without_a_profile_use_base() {
    let root = workspace();
    let selection = run(root.path(), "", &[]).unwrap();
    assert_eq!(selection.enabled(), ["workflow"]);
}

#[test]
fn profiles_overrides_and_dependencies_decide_the_enabled_set() {
    let root = workspace();
    let selection = run(
        root.path(),
        &format!("{ONE_DISPLAY}profile: screen\nplugins:\n  workflow:\n    enabled: true\n"),
        &[],
    )
    .unwrap();
    // `app-manager` brings `ui`, which brings `workflow`.
    assert_eq!(
        selection.enabled(),
        ["app-manager", "display", "ui", "workflow"]
    );

    let error = run(
        root.path(),
        &format!("{ONE_DISPLAY}profile: screen\nplugins:\n  ui:\n    enabled: false\n"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.contains("enables `app-manager`, which depends on `ui`, but disables `ui`"),
        "{error}"
    );

    let error = run(root.path(), "profile: missing\n", &[]).unwrap_err();
    assert!(error.contains("unknown profile `missing`"), "{error}");
    let error = run(root.path(), "plugins:\n  nope:\n    enabled: true\n", &[]).unwrap_err();
    assert!(error.contains("unknown Plugin `nope`"), "{error}");
}

#[test]
fn a_profile_extends_several_and_each_counts_once() {
    let root = workspace();
    write(
        root.path(),
        "boards/profiles/ui.yml",
        "enable: [app-manager]\n",
    );
    write(
        root.path(),
        "boards/profiles/lua.yml",
        "extends: headless\nenable: [display]\n",
    );
    write(
        root.path(),
        "boards/profiles/both.yml",
        "extends: [lua, ui, headless]\n",
    );
    let selection = run(root.path(), &format!("{ONE_DISPLAY}profile: both\n"), &[]).unwrap();
    assert_eq!(
        selection.enabled(),
        ["app-manager", "display", "ui", "workflow"]
    );

    write(
        root.path(),
        "boards/profiles/loop-a.yml",
        "extends: [headless, loop-b]\n",
    );
    write(
        root.path(),
        "boards/profiles/loop-b.yml",
        "extends: loop-a\n",
    );
    let error = run(root.path(), "profile: loop-a\n", &[]).unwrap_err();
    assert!(
        error.contains("profile `loop-a` extends itself through `loop-a`, `loop-b`"),
        "{error}"
    );
}

#[test]
fn the_local_override_disables_dependents_too() {
    let root = workspace();
    let selection = run(
        root.path(),
        &format!("{ONE_DISPLAY}profile: screen\n"),
        &["ui"],
    )
    .unwrap();
    assert_eq!(selection.enabled(), ["display", "workflow"]);
    assert_eq!(
        selection.notes(),
        ["`app-manager` is disabled because `ui` is disabled locally"]
    );
}

#[test]
fn the_system_ui_takes_the_primary_screen_and_lua_the_rest() {
    let root = workspace();
    let selection = run(root.path(), &format!("{ONE_DISPLAY}profile: screen\n"), &[]).unwrap();
    assert_eq!(
        claims(&selection, "ui"),
        [
            (
                "display".into(),
                "main".into(),
                Filled::One(Some(device("panel", 0)))
            ),
            ("touch".into(), "main".into(), Filled::One(None)),
        ]
    );
    assert_eq!(
        claims(&selection, "display"),
        [("display".into(), "rest".into(), Filled::Many(Vec::new()))]
    );

    let selection = run(
        root.path(),
        &format!("{THREE_DISPLAYS}profile: screen\n"),
        &[],
    )
    .unwrap();
    assert_eq!(
        claims(&selection, "ui")[0].2,
        Filled::One(Some(device("front", 1)))
    );
    assert_eq!(
        claims(&selection, "display"),
        [(
            "display".into(),
            "rest".into(),
            Filled::Many(vec![device("back-eink", 0), device("status", 2)])
        )]
    );

    // Without the system UI, Lua keeps the main screen.
    let selection = run(
        root.path(),
        &format!("{ONE_DISPLAY}plugins:\n  display:\n    enabled: true\n"),
        &[],
    )
    .unwrap();
    assert_eq!(
        claims(&selection, "display"),
        [(
            "display".into(),
            "rest".into(),
            Filled::Many(vec![device("panel", 0)])
        )]
    );
}

#[test]
fn wiring_overrides_defaults_and_is_checked() {
    let root = workspace();
    let selection = run(
        root.path(),
        &format!("{THREE_DISPLAYS}profile: screen\nplugins:\n  ui:\n    peripherals:\n      display.main: status\n"),
        &[],
    )
    .unwrap();
    assert_eq!(
        claims(&selection, "ui")[0].2,
        Filled::One(Some(device("status", 2)))
    );
    assert_eq!(
        claims(&selection, "display")[0].2,
        Filled::Many(vec![device("back-eink", 0), device("front", 1)])
    );

    for (wiring, expected) in [
        ("display.main: nowhere", "not a device of this Board"),
        ("display.side: front", "a slot the Plugin does not declare"),
        ("display-main: front", "expected `<kind>.<slot>`"),
    ] {
        let error = run(
            root.path(),
            &format!("{THREE_DISPLAYS}profile: screen\nplugins:\n  ui:\n    peripherals:\n      {wiring}\n"),
            &[],
        )
        .unwrap_err();
        assert!(error.contains(expected), "{wiring}: {error}");
    }
}

#[test]
fn conflicts_and_missing_devices_fail_the_build() {
    let root = workspace();
    // Several displays need one marked primary.
    let error = run(
        root.path(),
        &format!(
            "{}profile: screen\n",
            THREE_DISPLAYS.replace("      primary: true\n", "")
        ),
        &[],
    )
    .unwrap_err();
    assert!(
        error.contains("mark exactly one `primary: true`"),
        "{error}"
    );

    // A required slot the Board cannot fill.
    let error = run(root.path(), "profile: screen\n", &[]).unwrap_err();
    assert!(
        error
            .contains("enables `ui`, but its required slot `display.main` (primary) has no device"),
        "{error}"
    );

    // Two Plugins taking every unclaimed display.
    plugin(
        root.path(),
        "mirror",
        &[],
        "[peripherals.display]\nall = \"*\"\n",
    );
    let error = run(
        root.path(),
        &format!("{ONE_DISPLAY}profile: screen\nplugins:\n  mirror:\n    enabled: true\n"),
        &[],
    )
    .unwrap_err();
    assert!(error.contains("only one Plugin may"), "{error}");

    // `any` with several free devices asks for wiring.
    plugin(
        root.path(),
        "side",
        &[],
        "[peripherals.display]\nscreen = \"any\"\n",
    );
    let error = run(
        root.path(),
        &format!("{THREE_DISPLAYS}plugins:\n  side:\n    enabled: true\n"),
        &[],
    )
    .unwrap_err();
    assert!(
        error.contains("display.screen: back-eink | front | status"),
        "{error}"
    );

    // Two slots wired to one device.
    let error = run(
        root.path(),
        &format!("{THREE_DISPLAYS}profile: screen\nplugins:\n  ui:\n    peripherals:\n      display.main: status\n  side:\n    enabled: true\n    peripherals:\n      display.screen: status\n"),
        &[],
    )
    .unwrap_err();
    assert!(error.contains("gives device `status` to both"), "{error}");
}
