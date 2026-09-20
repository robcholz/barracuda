//! Plugin manifest contract tests.

#![allow(clippy::expect_used)]

use barracuda_plugin_manifest::{parse, ManifestError, MAX_DESCRIPTION_CHARS, PLUGIN_ID_PATTERN};

const VALID: &str = r#"
id = "web-search"
depends-on = ["webserver", "agent"]
description = "Provides Web search."
"#;

#[test]
fn parses_generic_tasks_and_rejects_invalid_commands() {
    let source = format!("{VALID}\n[tasks.build]\ncwd = 'resources/web'\ncommand = ['bun', 'run', 'build']\ninputs = ['resources/web/src']\noutputs = ['filesystem/resources/entry.js']\n");
    let manifest = parse(&source).expect("task manifest");
    assert_eq!(manifest.tasks["build"].command, ["bun", "run", "build"]);
    for task in [
        "command = []",
        "command = ['']",
        "command = ['x']\ncwd = '../outside'",
        "command = ['x']\noutputs = ['/outside']",
        "command = ['x']\nunknown = true",
    ] {
        assert!(parse(&format!("{VALID}\n[tasks.build]\n{task}\n")).is_err());
    }
}

#[test]
fn parses_a_canonical_manifest() {
    let manifest = parse(VALID).expect("valid Plugin manifest");

    assert_eq!(manifest.id(), "web-search");
    assert_eq!(manifest.dependencies(), ["webserver", "agent"]);
    assert_eq!(manifest.description(), "Provides Web search.");
}

#[test]
fn publishes_the_plugin_identity_pattern() {
    assert_eq!(PLUGIN_ID_PATTERN, "^[a-z0-9]+(?:-[a-z0-9]+)*$");
}

#[test]
fn rejects_empty_or_noncanonical_identity() {
    for id in [
        "",
        "   ",
        " web-search",
        "web-search ",
        "Web-search",
        "web_search",
        "web.search",
        "web/search",
        r"web\search",
        "web search",
        "-web-search",
        "web-search-",
        "web--search",
        "..",
        "搜索",
    ] {
        let manifest =
            format!("id = {id:?}\ndepends-on = []\ndescription = \"Valid description.\"\n");
        assert!(parse(&manifest).is_err(), "accepted identity {id:?}");
    }

    let manifest = "id = \"Web-search\"\ndepends-on = []\ndescription = \"Valid.\"\n";
    assert!(matches!(parse(manifest), Err(ManifestError::InvalidId(_))));
}

#[test]
fn rejects_empty_noncanonical_duplicate_and_self_dependencies() {
    for dependencies in [
        "[\"\"]",
        "[\"   \"]",
        "[\" base\"]",
        "[\"base \"]",
        "[\"Base\"]",
        "[\"base_plugin\"]",
        "[\"base/plugin\"]",
        "[\"base--plugin\"]",
        "[\"..\"]",
        "[\"base\", \"base\"]",
        "[\"demo\"]",
    ] {
        let manifest = format!(
            "id = \"demo\"\ndepends-on = {dependencies}\ndescription = \"Valid description.\"\n"
        );
        assert!(
            parse(&manifest).is_err(),
            "accepted dependencies {dependencies}"
        );
    }

    let manifest = "id = \"demo\"\ndepends-on = [\"base/plugin\"]\ndescription = \"Valid.\"\n";
    assert!(matches!(
        parse(manifest),
        Err(ManifestError::InvalidDependency(_))
    ));
}

#[test]
fn rejects_empty_and_oversized_descriptions() {
    for description in [
        String::new(),
        String::from("   "),
        "x".repeat(MAX_DESCRIPTION_CHARS + 1),
    ] {
        let manifest = format!("id = \"demo\"\ndepends-on = []\ndescription = {description:?}\n");
        assert!(
            parse(&manifest).is_err(),
            "accepted description {description:?}"
        );
    }
}

#[test]
fn rejects_unknown_fields() {
    let manifest = format!("{VALID}\nplatform = \"linux\"\n");

    assert!(parse(&manifest).is_err());
}
