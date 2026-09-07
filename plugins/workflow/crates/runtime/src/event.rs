//! JSON Event identity used by Workflow matching.

use alloc::string::String;
use core::fmt;

/// A validated identifier for one Event type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(String);

impl EventId {
    /// Returns this Event ID as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for EventId {
    type Error = EventIdError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(String::from(value))
    }
}

impl TryFrom<String> for EventId {
    type Error = EventIdError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(EventIdError::Empty);
        }
        for (index, character) in value.char_indices() {
            let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.');
            if !valid {
                return Err(EventIdError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while parsing an [`EventId`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EventIdError {
    /// Event IDs cannot be empty.
    #[error("Event ID cannot be empty")]
    Empty,
    /// Event IDs contain only ASCII letters, digits, `_`, `-`, and `.`.
    #[error("invalid Event ID character {character:?} at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
        /// Invalid character found in the input.
        character: char,
    },
}

/// Compile-time identity of one JSON Event.
pub trait Event: 'static {
    /// Stable Event identifier used by Workflow matching.
    const ID: &'static str;
}

/// Failure returned when emitting a Workflow Event.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EmitError {
    /// The Event type declares an invalid Event ID.
    #[error("invalid Event ID: {0}")]
    InvalidEventId(#[from] EventIdError),
}
