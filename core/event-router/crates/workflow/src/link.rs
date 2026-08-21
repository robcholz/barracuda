//! Link classification for Workflow steps.
//!
//! The link between two adjacent RPC steps is implicit in the step JSON and is
//! never named in the document:
//!
//! - no `arguments` → [`LinkKind::Direct`]: the previous response is passed
//!   through byte-for-byte (validated by response/request type identity).
//! - `arguments` with at least one `$` reference → [`LinkKind::Mapping`]: the
//!   next request is built from the literal arguments, then each referenced
//!   field is copied wire-to-wire out of the previous response.
//! - `arguments` with no `$` reference → [`LinkKind::Literal`]: the next request
//!   is built entirely from the literal arguments, independent of the previous
//!   step.
//!
//! A reference has the grammar `$<step>.<channel>.<field>`. Only `previous`
//! and `output` are defined today; both slots are reserved for future forms
//! (`$previous.input`, `$previous.error`, absolute step selectors). The field
//! is mandatory and a single top-level name: a bare `$previous.output` is
//! rejected, because whole-frame passthrough is `Link::Direct`, and nested
//! paths are rejected.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Map, Value};

/// One `$previous.output.<source>` reference bound to a request field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldRef {
    /// Request field the value is written into (the arguments key).
    pub(crate) dest_field: String,
    /// Response field the value is read from (after `output.`).
    pub(crate) source_field: String,
}

/// The classified link feeding one Workflow step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// Byte-for-byte passthrough of the previous response.
    Direct,
    /// Request built solely from literal arguments.
    Literal {
        /// Literal arguments object.
        arguments: Value,
    },
    /// Request built from literal arguments plus wire-copied reference fields.
    Mapping {
        /// Literal arguments object (references removed).
        arguments: Value,
        /// Fields copied out of the previous response.
        references: Vec<FieldRef>,
    },
}

/// Failure while classifying a step link.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LinkError {
    /// `arguments` was present but was not a JSON object.
    #[error("Workflow step arguments must be a JSON object")]
    ArgumentsNotObject,
    /// A `$` reference had no body.
    #[error("Workflow reference is empty")]
    EmptyReference,
    /// The step selector was not `previous`.
    #[error("Workflow reference step selector is not `previous`")]
    UnknownStepSelector,
    /// The channel selector was not `output`.
    #[error("Workflow reference channel is not `output`")]
    UnknownChannel,
    /// The reference addressed a nested field path.
    #[error("Workflow reference nested field paths are unsupported")]
    NestedFieldPath,
    /// The reference named a step and channel but no field.
    #[error(
        "Workflow reference names no field; use a Direct link to pass the whole previous response"
    )]
    MissingField,
    /// The reference did not have the `$step.channel.field` shape.
    #[error("Workflow reference is malformed")]
    MalformedReference,
}

/// Classifies the link feeding a step from its optional `arguments`.
pub(crate) fn classify(arguments: Option<&Value>) -> Result<LinkKind, LinkError> {
    let Some(arguments) = arguments else {
        return Ok(LinkKind::Direct);
    };
    let object = arguments
        .as_object()
        .ok_or(LinkError::ArgumentsNotObject)?;

    let mut literal = Map::new();
    let mut references = Vec::new();
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            if let Some(body) = text.strip_prefix('$') {
                let source_field = parse_reference(body)?;
                references.push(FieldRef {
                    dest_field: key.clone(),
                    source_field,
                });
                continue;
            }
        }
        literal.insert(key.clone(), value.clone());
    }

    let arguments = Value::Object(literal);
    if references.is_empty() {
        Ok(LinkKind::Literal { arguments })
    } else {
        Ok(LinkKind::Mapping {
            arguments,
            references,
        })
    }
}

/// Parses `previous.output.<field>` and returns the source field name.
fn parse_reference(body: &str) -> Result<String, LinkError> {
    if body.is_empty() {
        return Err(LinkError::EmptyReference);
    }
    let mut parts = body.split('.');
    let step = parts.next().ok_or(LinkError::MalformedReference)?;
    let channel = parts.next().ok_or(LinkError::MalformedReference)?;
    if step != "previous" {
        return Err(LinkError::UnknownStepSelector);
    }
    if channel != "output" {
        return Err(LinkError::UnknownChannel);
    }
    let field = parts.next().ok_or(LinkError::MissingField)?;
    if parts.next().is_some() {
        return Err(LinkError::NestedFieldPath);
    }
    if field.is_empty() {
        return Err(LinkError::MalformedReference);
    }
    Ok(field.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::panic)]
    #![allow(clippy::indexing_slicing)]
    #![allow(missing_docs)]

    use super::{classify, LinkError, LinkKind};
    use serde_json::json;

    #[test]
    fn absent_arguments_is_a_direct_link() {
        assert_eq!(classify(None).expect("classify"), LinkKind::Direct);
    }

    #[test]
    fn literal_only_arguments_is_a_literal_link() {
        let arguments = json!({ "channel": "imessage" });
        let kind = classify(Some(&arguments)).expect("classify");
        assert_eq!(kind, LinkKind::Literal { arguments });
    }

    #[test]
    fn referenced_field_is_a_mapping_link_with_literals_stripped() {
        let arguments = json!({ "channel": "imessage", "message": "$previous.output.text" });
        let LinkKind::Mapping {
            arguments: literal,
            references,
        } = classify(Some(&arguments)).expect("classify")
        else {
            panic!("expected a mapping link");
        };
        assert_eq!(literal, json!({ "channel": "imessage" }));
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].dest_field, "message");
        assert_eq!(references[0].source_field, "text");
    }

    #[test]
    fn nested_paths_and_unknown_selectors_are_rejected() {
        assert_eq!(
            classify(Some(&json!({ "a": "$previous.output.a.b" }))),
            Err(LinkError::NestedFieldPath)
        );
        assert_eq!(
            classify(Some(&json!({ "a": "$first.output.a" }))),
            Err(LinkError::UnknownStepSelector)
        );
        assert_eq!(
            classify(Some(&json!({ "a": "$previous.input.a" }))),
            Err(LinkError::UnknownChannel)
        );
        assert_eq!(
            classify(Some(&json!({ "a": "$previous" }))),
            Err(LinkError::MalformedReference)
        );
        assert_eq!(
            classify(Some(&json!([1, 2]))),
            Err(LinkError::ArgumentsNotObject)
        );
    }

    #[test]
    fn reference_without_a_field_requires_a_direct_link() {
        assert_eq!(
            classify(Some(&json!({ "a": "$previous.output" }))),
            Err(LinkError::MissingField)
        );
        assert_eq!(
            classify(Some(&json!({ "a": "$previous.output." }))),
            Err(LinkError::MalformedReference)
        );
    }
}
