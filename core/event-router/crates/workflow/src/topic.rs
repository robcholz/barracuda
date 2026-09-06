//! Fixed-capacity Event topics and Workflow topic matching rules.

use alloc::string::String;
use core::fmt;

/// Maximum number of ASCII bytes in an Event topic.
pub const TOPIC_MAX_BYTES: usize = 16;
pub(crate) const TOPIC_STORAGE_BYTES: usize = TOPIC_MAX_BYTES + 1;

/// A fixed-capacity, NUL-terminated Event topic.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Topic([u8; TOPIC_STORAGE_BYTES]);

impl TryFrom<&str> for Topic {
    type Error = TopicError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(TopicError::Empty);
        }
        if value.len() > TOPIC_MAX_BYTES {
            return Err(TopicError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(TopicError::InvalidCharacter);
        }

        let mut bytes = [0; TOPIC_STORAGE_BYTES];
        bytes
            .get_mut(..value.len())
            .ok_or(TopicError::TooLong)?
            .copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Topic {
    type Error = TopicError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl Topic {
    /// Returns the topic text without its trailing NUL padding.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(TOPIC_STORAGE_BYTES);
        core::str::from_utf8(self.0.get(..end).unwrap_or_default()).unwrap_or_default()
    }
}

impl AsRef<str> for Topic {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while constructing a [`Topic`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum TopicError {
    /// Topics cannot be empty.
    #[error("Event topic cannot be empty")]
    Empty,
    /// Topics contain at most [`TOPIC_MAX_BYTES`] bytes.
    #[error("Event topic exceeds its fixed capacity")]
    TooLong,
    /// Topics contain only ASCII letters, digits, `_`, `-`, and `.`.
    #[error("Event topic contains an invalid character")]
    InvalidCharacter,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use super::{Topic, TopicError, TOPIC_MAX_BYTES, TOPIC_STORAGE_BYTES};

    #[test]
    fn topic_is_a_fixed_nul_terminated_string() {
        let topic = Topic::try_from("morning.alarm").expect("valid topic");

        assert_eq!(topic.as_str(), "morning.alarm");
        assert_eq!(topic.0.len(), TOPIC_STORAGE_BYTES);
        assert_eq!(topic.0[13], 0);
    }

    #[test]
    fn topic_enforces_the_public_capacity_and_character_set() {
        assert!(Topic::try_from("a".repeat(TOPIC_MAX_BYTES)).is_ok());
        assert_eq!(
            Topic::try_from("a".repeat(TOPIC_MAX_BYTES + 1)),
            Err(TopicError::TooLong)
        );
        assert_eq!(
            Topic::try_from("alarm.*"),
            Err(TopicError::InvalidCharacter)
        );
    }
}
