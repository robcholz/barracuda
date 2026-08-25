//! Event ID matching rules.

use alloc::string::String;
use core::fmt;

use super::EventId;

/// A validated Event ID glob used by the Workflow Runtime.
///
/// `*` is the only pattern operator. It matches zero or more Event ID
/// characters, including dots. Every other character is matched literally.
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
        glob_matches(self.0.as_bytes(), event_id.as_str().as_bytes())
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
        validate_pattern(&value)?;
        Ok(Self(value))
    }
}

impl AsRef<str> for Rule {
    fn as_ref(&self) -> &str {
        self.as_str()
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

impl From<ValidationError> for RuleError {
    fn from(error: ValidationError) -> Self {
        match error {
            ValidationError::Empty => Self::Empty,
            ValidationError::InvalidCharacter { index, character } => {
                Self::InvalidCharacter { index, character }
            }
        }
    }
}

enum ValidationError {
    Empty,
    InvalidCharacter { index: usize, character: char },
}

fn validate_pattern(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Empty);
    }

    for (index, character) in value.char_indices() {
        let valid = character.is_ascii_alphanumeric()
            || matches!(character, '_' | '-' | '.')
            || character == '*';
        if !valid {
            return Err(ValidationError::InvalidCharacter { index, character });
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    #![allow(missing_docs)]

    use super::super::EventId;
    use super::{Rule, RuleError};

    fn event_id(value: &str) -> EventId {
        EventId::try_from(value).expect("valid test Event ID")
    }

    fn rule(value: &str) -> Rule {
        Rule::try_from(value).expect("valid test Rule")
    }

    #[test]
    fn rule_accepts_wildcards_anywhere() {
        for value in ["*", "aaa.*", "*bbb", "aaa*bbb", "a**b"] {
            assert!(Rule::try_from(value).is_ok());
        }
    }

    #[test]
    fn rule_rejects_empty_or_non_pattern_characters() {
        assert_eq!(Rule::try_from(""), Err(RuleError::Empty));
        for value in ["aaa bbb", "aaa/bbb", "事件"] {
            assert!(matches!(
                Rule::try_from(value),
                Err(RuleError::InvalidCharacter { .. })
            ));
        }
    }

    #[test]
    fn exact_rule_matches_only_the_same_event_id() {
        let rule = rule("aaa.bbb");

        assert!(rule.matches(&event_id("aaa.bbb")));
        assert!(!rule.matches(&event_id("aaa.bbb.ccc")));
        assert!(!rule.matches(&event_id("aaa.BBB")));
    }

    #[test]
    fn wildcard_matches_zero_or_more_characters() {
        let rule = rule("aaa*bbb");

        assert!(rule.matches(&event_id("aaabbb")));
        assert!(rule.matches(&event_id("aaa.middle.bbb")));
        assert!(!rule.matches(&event_id("aaa.middle.ccc")));
    }

    #[test]
    fn wildcard_can_match_dots_but_literal_dots_remain_required() {
        let rule = rule("aaa.*");

        assert!(rule.matches(&event_id("aaa.bbb")));
        assert!(rule.matches(&event_id("aaa.bbb.ccc")));
        assert!(!rule.matches(&event_id("aaa")));
    }

    #[test]
    fn multiple_and_consecutive_wildcards_match_generically() {
        assert!(rule("a*b*c").matches(&event_id("ax.by.c")));
        assert!(rule("a*b*c").matches(&event_id("abc")));
        assert!(!rule("a*b*c").matches(&event_id("acb")));
        assert!(rule("a**b").matches(&event_id("axyzb")));
    }

    #[test]
    fn all_wildcard_rule_matches_every_valid_event_id() {
        let rule = rule("*");

        assert!(rule.matches(&event_id("a")));
        assert!(rule.matches(&event_id("aaa.bbb.ccc")));
    }
}
