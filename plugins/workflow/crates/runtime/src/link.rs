//! Link classification for Workflow Action steps.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceSelector {
    EventInput,
    PreviousOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldRef {
    pub(crate) dest_field: String,
    pub(crate) selector: SourceSelector,
    pub(crate) source_field: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkKind {
    Direct,
    Literal {
        arguments: Value,
    },
    Mapping {
        arguments: Value,
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
    /// The reference shape was malformed.
    #[error("Workflow reference is malformed")]
    MalformedReference,
}

/// Classifies a step's arguments, moving literal values into the link.
pub(crate) fn classify(arguments: Option<Value>) -> Result<LinkKind, LinkError> {
    let Some(arguments) = arguments else {
        return Ok(LinkKind::Direct);
    };
    let Value::Object(object) = arguments else {
        return Err(LinkError::ArgumentsNotObject);
    };
    let mut literal = Map::new();
    let mut references = Vec::new();
    for (key, value) in object {
        if let Some(body) = value.as_str().and_then(|text| text.strip_prefix('$')) {
            let (selector, source_field) = parse_reference(body)?;
            references.push(FieldRef {
                dest_field: key,
                selector,
                source_field,
            });
        } else {
            literal.insert(key, value);
        }
    }
    references.shrink_to_fit();
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

fn parse_reference(body: &str) -> Result<(SourceSelector, Option<String>), LinkError> {
    parse_source(body)
}

pub(crate) fn parse_condition_source(
    body: &str,
) -> Result<(SourceSelector, Option<String>), LinkError> {
    parse_source(body)
}

fn parse_source(body: &str) -> Result<(SourceSelector, Option<String>), LinkError> {
    if body.is_empty() {
        return Err(LinkError::EmptyReference);
    }
    let mut parts = body.split('.');
    let owner = parts.next().ok_or(LinkError::MalformedReference)?;
    let channel = parts.next().ok_or(LinkError::MalformedReference)?;
    let selector = match (owner, channel) {
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
