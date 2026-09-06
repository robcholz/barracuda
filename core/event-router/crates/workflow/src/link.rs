//! Link classification for Workflow steps.
//!
//! The link feeding each executed RPC is implicit in the step JSON and is never
//! named in the document:
//!
//! - no `arguments` → [`LinkKind::Direct`]: the first executed RPC receives the
//!   Event input unchanged; later RPCs receive the most recently executed RPC's
//!   JSON response unchanged.
//! - `arguments` with at least one `$` reference → [`LinkKind::Mapping`]: the
//!   next request is built from the literal arguments, then each referenced
//!   JSON value is copied out of the selected Event input or previous response.
//! - `arguments` with no `$` reference → [`LinkKind::Literal`]: the next request
//!   is built entirely from the literal arguments, independent of the previous
//!   step.
//!
//! A reference has the grammar `$<selector>.<channel>.<field>`. The supported
//! sources are `$event.input.<field>` and `$previous.output.<field>`. The field
//! is mandatory and a single top-level name; whole-document passthrough uses a
//! Direct link and nested paths are rejected.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::{Map, Value};

/// Source selected by one Workflow argument reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceSelector {
    /// The original Event JSON that triggered the Workflow.
    EventInput,
    /// The most recently executed RPC step's response JSON.
    PreviousOutput,
}

/// One selected source field bound to a request field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldRef {
    /// Request field the value is written into (the arguments key).
    pub(crate) dest_field: String,
    /// Event input or most recently executed step output selected by the reference.
    pub(crate) selector: SourceSelector,
    /// Top-level JSON field read from the selected source.
    pub(crate) source_field: String,
}

/// The classified link feeding one Workflow step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    /// Unchanged JSON passthrough of the Event input or previous response.
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
        /// JSON fields copied out of their selected sources.
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
    /// The source selector was neither `event` nor `previous`.
    #[error("Workflow reference source selector is neither `event` nor `previous`")]
    UnknownStepSelector,
    /// The selected source does not support the requested channel.
    #[error("Workflow reference channel is invalid for its source selector")]
    UnknownChannel,
    /// The reference addressed a nested field path.
    #[error("Workflow reference nested field paths are unsupported")]
    NestedFieldPath,
    /// The reference named a step and channel but no field.
    #[error(
        "Workflow reference names no field; use a Direct link to pass the whole source document"
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
    let object = arguments.as_object().ok_or(LinkError::ArgumentsNotObject)?;

    let mut literal = Map::new();
    let mut references = Vec::new();
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            if let Some(body) = text.strip_prefix('$') {
                let (selector, source_field) = parse_reference(body)?;
                references.push(FieldRef {
                    dest_field: key.clone(),
                    selector,
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

/// Parses a source selector and returns it with its top-level field name.
pub(crate) fn parse_reference(body: &str) -> Result<(SourceSelector, String), LinkError> {
    if body.is_empty() {
        return Err(LinkError::EmptyReference);
    }
    let mut parts = body.split('.');
    let step = parts.next().ok_or(LinkError::MalformedReference)?;
    let channel = parts.next().ok_or(LinkError::MalformedReference)?;
    let selector = match (step, channel) {
        ("event", "input") => SourceSelector::EventInput,
        ("previous", "output") => SourceSelector::PreviousOutput,
        ("event" | "previous", _) => return Err(LinkError::UnknownChannel),
        _ => return Err(LinkError::UnknownStepSelector),
    };
    let field = parts.next().ok_or(LinkError::MissingField)?;
    if parts.next().is_some() {
        return Err(LinkError::NestedFieldPath);
    }
    if field.is_empty() {
        return Err(LinkError::MalformedReference);
    }
    Ok((selector, field.to_string()))
}

/// Parses a condition source selecting either a whole JSON document or one
/// top-level field from it.
pub(crate) fn parse_condition_source(
    body: &str,
) -> Result<(SourceSelector, Option<String>), LinkError> {
    if body.is_empty() {
        return Err(LinkError::EmptyReference);
    }
    let mut parts = body.split('.');
    let step = parts.next().ok_or(LinkError::MalformedReference)?;
    let channel = parts.next().ok_or(LinkError::MalformedReference)?;
    let selector = match (step, channel) {
        ("event", "input") => SourceSelector::EventInput,
        ("previous", "output") => SourceSelector::PreviousOutput,
        ("event" | "previous", _) => return Err(LinkError::UnknownChannel),
        _ => return Err(LinkError::UnknownStepSelector),
    };
    let Some(field) = parts.next() else {
        return Ok((selector, None));
    };
    if parts.next().is_some() {
        return Err(LinkError::NestedFieldPath);
    }
    if field.is_empty() {
        return Err(LinkError::MalformedReference);
    }
    Ok((selector, Some(field.to_string())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(clippy::panic)]
    #![allow(clippy::indexing_slicing)]
    #![allow(missing_docs)]

    use super::{classify, parse_condition_source, LinkError, LinkKind, SourceSelector};
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
        assert_eq!(references[0].selector, SourceSelector::PreviousOutput);
        assert_eq!(references[0].source_field, "text");
    }

    #[test]
    fn event_input_field_is_a_mapping_source() {
        let arguments = json!({ "prompt": "$event.input.message" });
        let LinkKind::Mapping { references, .. } =
            classify(Some(&arguments)).expect("classify Event input reference")
        else {
            panic!("expected a mapping link");
        };

        assert_eq!(references.len(), 1);
        assert_eq!(references[0].dest_field, "prompt");
        assert_eq!(references[0].selector, SourceSelector::EventInput);
        assert_eq!(references[0].source_field, "message");
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

    #[test]
    fn condition_source_can_select_a_document_or_top_level_field() {
        assert_eq!(
            parse_condition_source("previous.output"),
            Ok((SourceSelector::PreviousOutput, None))
        );
        assert_eq!(
            parse_condition_source("event.input.status"),
            Ok((SourceSelector::EventInput, Some("status".into())))
        );
        assert_eq!(
            parse_condition_source("event.input.status.code"),
            Err(LinkError::NestedFieldPath)
        );
    }
}
