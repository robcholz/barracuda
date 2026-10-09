//! Latest iLink `context_token` per user, attached to sends automatically.
//!
//! iLink hands out a `context_token` with every inbound message and expects
//! the latest one on replies to that user; without it, sends become
//! unreliable. The map is bounded: at most [`MAX_CONTEXT_TOKENS`] users, the
//! least recently used dropped first. The Plugin persists it.

use alloc::{collections::VecDeque, string::String, vec::Vec};
use core::cell::RefCell;

use serde::{Deserialize, Serialize};

/// Users whose latest token is kept.
pub const MAX_CONTEXT_TOKENS: usize = 16;

/// Longest user id kept, in bytes.
const USER_ID_MAX: usize = 128;

/// Longest token kept, in bytes.
const CONTEXT_TOKEN_MAX: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    user: String,
    token: String,
}

/// The latest `context_token` of up to [`MAX_CONTEXT_TOKENS`] users.
#[derive(Debug, Default)]
pub struct ContextTokens {
    /// Most recently used first.
    entries: RefCell<VecDeque<Entry>>,
}

impl ContextTokens {
    /// Creates an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restores a map stored by [`Self::encode`], keeping at most
    /// [`MAX_CONTEXT_TOKENS`] entries.
    ///
    /// # Errors
    ///
    /// Returns the decoder's error for malformed bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let mut entries: VecDeque<Entry> = serde_json::from_slice::<Vec<Entry>>(bytes)?
            .into_iter()
            .filter(|entry| valid(&entry.user, &entry.token))
            .collect();
        entries.truncate(MAX_CONTEXT_TOKENS);
        Ok(Self {
            entries: RefCell::new(entries),
        })
    }

    /// Encodes the map as a JSON array, most recently used first.
    ///
    /// # Errors
    ///
    /// Returns the encoder's error, which string entries cannot produce.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&*self.entries.borrow())
    }

    /// Returns the latest token of `user` and marks it recently used.
    #[must_use]
    pub fn get(&self, user: &str) -> Option<String> {
        let mut entries = self.entries.borrow_mut();
        let index = entries.iter().position(|entry| entry.user == user)?;
        let entry = entries.remove(index)?;
        let token = entry.token.clone();
        entries.push_front(entry);
        Some(token)
    }

    /// Stores `token` as the latest of `user`, dropping the least recently
    /// used user when full. Returns whether the stored map changed.
    ///
    /// Oversized ids or tokens are not kept.
    pub fn remember(&self, user: &str, token: &str) -> bool {
        if !valid(user, token) {
            return false;
        }
        let mut entries = self.entries.borrow_mut();
        let previous = entries
            .iter()
            .position(|entry| entry.user == user)
            .and_then(|index| entries.remove(index));
        let changed = previous.as_ref().is_none_or(|entry| entry.token != token);
        let entry = match previous {
            Some(mut entry) => {
                if changed {
                    entry.token = token.into();
                }
                entry
            }
            None => Entry {
                user: user.into(),
                token: token.into(),
            },
        };
        entries.push_front(entry);
        entries.truncate(MAX_CONTEXT_TOKENS);
        changed
    }

    /// Forgets every token, as after the bot changed.
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    /// Number of users with a token.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether no user has a token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

fn valid(user: &str, token: &str) -> bool {
    !user.is_empty()
        && user.len() <= USER_ID_MAX
        && !token.is_empty()
        && token.len() <= CONTEXT_TOKEN_MAX
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use alloc::format;

    use super::*;

    #[test]
    fn keeps_the_latest_token_per_user() {
        let tokens = ContextTokens::new();
        assert!(tokens.remember("a", "1"));
        assert!(!tokens.remember("a", "1"));
        assert!(tokens.remember("a", "2"));
        assert_eq!(tokens.get("a").as_deref(), Some("2"));
        assert_eq!(tokens.get("b"), None);
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn drops_the_least_recently_used_user() {
        let tokens = ContextTokens::new();
        for user in 0..MAX_CONTEXT_TOKENS {
            tokens.remember(&format!("user-{user}"), "token");
        }
        // user-0 is used again, so user-1 is now the oldest.
        assert!(tokens.get("user-0").is_some());
        tokens.remember("newcomer", "token");

        assert_eq!(tokens.len(), MAX_CONTEXT_TOKENS);
        assert!(tokens.get("user-0").is_some());
        assert_eq!(tokens.get("user-1"), None);
        assert!(tokens.get("newcomer").is_some());
    }

    #[test]
    fn round_trips_and_rejects_oversized_entries() {
        let tokens = ContextTokens::new();
        tokens.remember("a", "1");
        tokens.remember("b", "2");
        assert!(!tokens.remember("c", &"x".repeat(CONTEXT_TOKEN_MAX + 1)));

        let restored = ContextTokens::decode(&tokens.encode().expect("encode")).expect("decode");

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get("a").as_deref(), Some("1"));
        assert_eq!(restored.get("b").as_deref(), Some("2"));
        restored.clear();
        assert!(restored.is_empty());
    }
}
