//! Behavior tests for the supported mapping-only YAML subset.

#![allow(clippy::expect_used)]

use std::borrow::Cow;

use barracuda_minimal_yaml::{mapping, ErrorKind};

#[test]
fn plain_and_quoted_scalars_are_read_without_a_yaml_tree() {
    let yaml = "name: light-switch\nversion: \"1.0\"\nowner: 'Barracuda'\n";
    let mut entries = mapping(yaml).entries();

    let name = entries.next().expect("name entry").expect("valid name");
    assert_eq!(name.key(), "name");
    assert!(matches!(name.scalar(), Ok(Cow::Borrowed("light-switch"))));

    let version = entries
        .next()
        .expect("version entry")
        .expect("valid version");
    assert!(matches!(version.scalar(), Ok(Cow::Borrowed("1.0"))));

    let owner = entries.next().expect("owner entry").expect("valid owner");
    assert!(matches!(owner.scalar(), Ok(Cow::Borrowed("Barracuda"))));
    assert!(entries.next().is_none());
}

#[test]
fn escaped_quotes_allocate_only_the_decoded_scalar() {
    let mut entries = mapping("description: \"Use \\\"quoted\\\" text\"\n").entries();
    let description = entries
        .next()
        .expect("description entry")
        .expect("valid description");

    assert!(matches!(
        description.scalar(),
        Ok(Cow::Owned(value)) if value == "Use \"quoted\" text"
    ));
}

#[test]
fn folded_and_literal_block_scalars_have_explicit_semantics() {
    let yaml = "description: >\n  Does a useful thing.\n  Use for examples.\nscript: |\n  first\n  second\n";
    let mut entries = mapping(yaml).entries();

    let description = entries
        .next()
        .expect("description entry")
        .expect("valid description");
    assert_eq!(
        description.scalar().expect("folded scalar"),
        "Does a useful thing. Use for examples.\n"
    );

    let script = entries.next().expect("script entry").expect("valid script");
    assert_eq!(script.scalar().expect("literal scalar"), "first\nsecond\n");
}

#[test]
fn nested_string_mapping_can_be_iterated() {
    let yaml = "name: example\nmetadata:\n  author: example-org\n  version: \"1.0\"\n";
    let mut entries = mapping(yaml).entries();
    let _name = entries.next().expect("name entry").expect("valid name");
    let metadata = entries
        .next()
        .expect("metadata entry")
        .expect("valid metadata");
    let nested = metadata.mapping().expect("metadata mapping");
    let values = nested
        .entries()
        .map(|entry| {
            let entry = entry.expect("valid metadata entry");
            (
                entry.key().to_owned(),
                entry.scalar().expect("string metadata").into_owned(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        values,
        vec![
            ("author".to_owned(), "example-org".to_owned()),
            ("version".to_owned(), "1.0".to_owned()),
        ]
    );
}

#[test]
fn ignoring_an_unknown_entry_skips_its_complete_subtree() {
    let yaml = "future-field:\n  nested:\n    - unsupported\n    - syntax\nname: example\n";
    let mut entries = mapping(yaml).entries();

    let unknown = entries
        .next()
        .expect("unknown entry")
        .expect("valid entry header");
    assert_eq!(unknown.key(), "future-field");

    let name = entries.next().expect("name entry").expect("valid name");
    assert_eq!(name.key(), "name");
    assert_eq!(name.scalar().expect("name scalar"), "example");
    assert!(entries.next().is_none());
}

#[test]
fn known_fields_reject_the_wrong_value_shape() {
    let mut entries = mapping("name:\n  nested: value\n").entries();
    let name = entries.next().expect("name entry").expect("valid header");

    let error = name.scalar().expect_err("mapping is not a scalar");
    assert_eq!(error.kind(), ErrorKind::ExpectedScalar);
    assert_eq!(error.line(), 1);
}

#[test]
fn comments_blank_lines_and_crlf_are_accepted() {
    let yaml = "# generated\r\n\r\nname: example\r\n# trailing comment\r\n";
    let mut entries = mapping(yaml).entries();
    let name = entries.next().expect("name entry").expect("valid name");

    assert_eq!(name.key(), "name");
    assert_eq!(name.scalar().expect("name scalar"), "example");
    assert!(entries.next().is_none());
}

#[test]
fn malformed_top_level_lines_are_not_silently_skipped() {
    let mut entries = mapping("name: example\nthis is not an entry\n").entries();
    assert!(entries.next().expect("name entry").is_ok());

    let error = entries
        .next()
        .expect("malformed entry")
        .expect_err("malformed YAML must fail");
    assert_eq!(error.line(), 2);
}

#[test]
fn block_scalar_indentation_comes_from_its_first_line() {
    let more_indented_later = "script: |\n  first\n    nested\n  last\n";
    let script = mapping(more_indented_later)
        .entries()
        .next()
        .expect("script entry")
        .expect("valid script");
    assert_eq!(
        script.scalar().expect("literal scalar"),
        "first\n  nested\nlast\n"
    );

    let less_indented_later = "script: |\n    first\n  second\n";
    let script = mapping(less_indented_later)
        .entries()
        .next()
        .expect("script entry")
        .expect("valid script");
    assert_eq!(
        script.scalar().expect_err("dedented content").kind(),
        ErrorKind::UnexpectedIndent
    );
}
