//! Plugin manifest contract tests.

#![allow(clippy::expect_used)]

use barracuda_plugin_manifest::{parse, MAX_DESCRIPTION_CHARS};

const VALID: &str = r#"
id = "web-search"
depends-on = ["webserver", "agent"]
description = "Provides Web search."
"#;

#[test]
fn parses_a_canonical_manifest() {
    let manifest = parse(VALID).expect("valid Plugin manifest");

    assert_eq!(manifest.id(), "web-search");
    assert_eq!(manifest.dependencies(), ["webserver", "agent"]);
    assert_eq!(manifest.description(), "Provides Web search.");
}

#[test]
fn rejects_empty_or_noncanonical_identity() {
    for id in ["", "   ", " web-search", "web-search "] {
        let manifest =
            format!("id = {id:?}\ndepends-on = []\ndescription = \"Valid description.\"\n");
        assert!(parse(&manifest).is_err(), "accepted identity {id:?}");
    }
}

#[test]
fn rejects_empty_noncanonical_duplicate_and_self_dependencies() {
    for dependencies in [
        "[\"\"]",
        "[\"   \"]",
        "[\" base\"]",
        "[\"base \"]",
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
