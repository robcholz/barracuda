//! Event ID matching rules.

use super::EventId;
use alloc::string::String;
use core::fmt;

/// A validated Event ID glob. `*` matches zero or more characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rule(String);

impl Rule {
    /// Returns this rule's source pattern.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns whether this rule matches `event_id`.
    #[must_use]
    pub fn matches(&self, event_id: &EventId) -> bool {
        self.matches_str(event_id.as_str())
    }

    /// Returns whether this rule matches the Event ID text `event_id`.
    #[must_use]
    pub(crate) fn matches_str(&self, event_id: &str) -> bool {
        glob_matches(self.0.as_bytes(), event_id.as_bytes())
    }
}

impl TryFrom<&str> for Rule {
    type Error = RuleError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(String::from(value))
    }
}

impl TryFrom<String> for Rule {
    type Error = RuleError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(RuleError::Empty);
        }
        for (index, character) in value.char_indices() {
            let valid =
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '*');
            if !valid {
                return Err(RuleError::InvalidCharacter { index, character });
            }
        }
        Ok(Self(value))
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Failure returned while parsing a [`Rule`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RuleError {
    /// Rules cannot be empty.
    #[error("Rule cannot be empty")]
    Empty,
    /// Rules contain only Event ID characters and `*`.
    #[error("invalid Rule character {character:?} at byte {index}")]
    InvalidCharacter {
        /// Byte offset of the invalid character.
        index: usize,
        /// Invalid character found in the input.
        character: char,
    },
}

fn glob_matches(pattern: &[u8], event_id: &[u8]) -> bool {
    let mut pattern_index = 0usize;
    let mut event_index = 0usize;
    let mut last_wildcard = None;
    let mut wildcard_match_start = 0usize;
    while let Some(event_byte) = event_id.get(event_index) {
        match pattern.get(pattern_index) {
            Some(b'*') => {
                last_wildcard = Some(pattern_index);
                pattern_index = pattern_index.saturating_add(1);
                wildcard_match_start = event_index;
            }
            Some(pattern_byte) if pattern_byte == event_byte => {
                pattern_index = pattern_index.saturating_add(1);
                event_index = event_index.saturating_add(1);
            }
            _ => {
                let Some(wildcard_index) = last_wildcard else {
                    return false;
                };
                wildcard_match_start = wildcard_match_start.saturating_add(1);
                event_index = wildcard_match_start;
                pattern_index = wildcard_index.saturating_add(1);
            }
        }
    }
    while matches!(pattern.get(pattern_index), Some(b'*')) {
        pattern_index = pattern_index.saturating_add(1);
    }
    pattern_index == pattern.len()
}
