//! In-memory owner list and pairing state for one channel.

use alloc::string::String;
use alloc::vec::Vec;

use barracuda_platform::EntropyUnavailable;
use embassy_time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::code::{PairingCode, PairingEntropy};

/// Most owners one channel keeps.
pub const MAX_OWNERS: usize = 8;

/// How long a pairing code stays valid after it is minted.
pub const PAIRING_CODE_LIFETIME: Duration = Duration::from_secs(600);

/// Wrong code-shaped guesses from non-owners that retire the current code.
///
/// Together with the ten-minute lifetime this bounds an attacker to a few
/// guesses per minted code out of a million.
pub const MAX_PAIRING_ATTEMPTS: u8 = 5;

/// Longest provider sender identifier accepted as an owner, in bytes.
pub const OWNER_ID_MAX: usize = 128;

/// Longest owner label kept, in bytes; longer labels are cut at a character
/// boundary.
pub const OWNER_LABEL_MAX: usize = 64;

/// One sender allowed to command the device through a channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    id: String,
    #[serde(default)]
    label: Option<String>,
}

impl Owner {
    /// Creates an owner, cutting an over-long label.
    ///
    /// Returns `None` when `id` is empty or longer than [`OWNER_ID_MAX`].
    #[must_use]
    pub fn new(id: &str, label: Option<&str>) -> Option<Self> {
        if id.is_empty() || id.len() > OWNER_ID_MAX {
            return None;
        }
        Some(Self {
            id: id.into(),
            label: label
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(|label| truncate(label, OWNER_LABEL_MAX).into()),
        })
    }

    /// Provider sender identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Display name captured at pairing, when the provider gave one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// What a channel does with one inbound message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Classification {
    /// The sender is an owner: publish the message.
    Owner,
    /// The message carried the current pairing code and its sender is now an
    /// owner: reply once with [`crate::PAIRED_REPLY`] and do not publish it.
    Paired,
    /// Anyone else: drop the message without a reply. It is counted.
    Ignored,
}

/// The current pairing code and its remaining lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PairingView {
    /// The code to show in the portal.
    pub code: PairingCode,
    /// Time until the code expires.
    pub expires_in: Duration,
}

#[derive(Clone, Copy)]
struct Pairing {
    code: PairingCode,
    expires_at: Instant,
    failures: u8,
}

/// Owner list, pairing code, and ignored-message counter of one channel.
///
/// Pure logic: the caller supplies the time, and [`crate::Owners`] persists
/// the list.
pub struct OwnerBook {
    owners: Vec<Owner>,
    pairing: Option<Pairing>,
    ignored: u32,
    entropy: PairingEntropy,
}

impl OwnerBook {
    /// Creates a book holding `owners`, keeping the first [`MAX_OWNERS`]
    /// distinct identifiers.
    #[must_use]
    pub fn new(owners: Vec<Owner>, entropy: PairingEntropy) -> Self {
        let mut book = Self {
            owners: Vec::new(),
            pairing: None,
            ignored: 0,
            entropy,
        };
        for owner in owners {
            if book.owners.len() < MAX_OWNERS && !book.is_owner(owner.id()) {
                book.owners.push(owner);
            }
        }
        book
    }

    /// Current owners in pairing order.
    #[must_use]
    pub fn owners(&self) -> &[Owner] {
        &self.owners
    }

    /// Whether `id` is an owner.
    #[must_use]
    pub fn is_owner(&self, id: &str) -> bool {
        self.owners.iter().any(|owner| owner.id() == id)
    }

    /// Whether the book holds [`MAX_OWNERS`] owners and pairing is closed.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.owners.len() >= MAX_OWNERS
    }

    /// Messages dropped since boot because their sender is not an owner.
    #[must_use]
    pub const fn ignored(&self) -> u32 {
        self.ignored
    }

    /// Returns the valid pairing code, minting one when none is valid.
    ///
    /// Returns `None` while the book is full or no entropy is available.
    pub fn pairing(&mut self, now: Instant) -> Option<PairingView> {
        if self.is_full() {
            self.pairing = None;
            return None;
        }
        if self.valid_pairing(now).is_none() && self.rotate(now).is_err() {
            return None;
        }
        self.valid_pairing(now).map(|pairing| PairingView {
            code: pairing.code,
            expires_in: pairing.expires_at.saturating_duration_since(now),
        })
    }

    /// Replaces the pairing code with a freshly minted one.
    ///
    /// # Errors
    ///
    /// Returns [`EntropyUnavailable`] when no code can be minted; the previous
    /// code is retired either way.
    pub fn rotate(&mut self, now: Instant) -> Result<(), EntropyUnavailable> {
        self.pairing = None;
        let code = PairingCode::mint(&self.entropy)?;
        self.pairing = Some(Pairing {
            code,
            expires_at: now.saturating_add(PAIRING_CODE_LIFETIME),
            failures: 0,
        });
        Ok(())
    }

    /// Classifies one inbound message from `sender`.
    ///
    /// A non-owner whose message carries the valid code becomes an owner,
    /// labelled with `label`, and the code is retired; the next
    /// [`Self::pairing`] mints a new one. [`MAX_PAIRING_ATTEMPTS`] wrong codes
    /// also retire it.
    pub fn classify(
        &mut self,
        sender: &str,
        label: Option<&str>,
        text: &str,
        now: Instant,
    ) -> Classification {
        if self.is_owner(sender) {
            return Classification::Owner;
        }
        if self.try_pair(sender, label, text, now) {
            return Classification::Paired;
        }
        self.ignored = self.ignored.saturating_add(1);
        Classification::Ignored
    }

    fn try_pair(&mut self, sender: &str, label: Option<&str>, text: &str, now: Instant) -> bool {
        let Some(candidate) = PairingCode::parse_message(text) else {
            return false;
        };
        if self.is_full() || self.valid_pairing(now).is_none() {
            return false;
        }
        let Some(pairing) = self.pairing.as_mut() else {
            return false;
        };
        if !pairing.code.matches(&candidate) {
            pairing.failures = pairing.failures.saturating_add(1);
            if pairing.failures >= MAX_PAIRING_ATTEMPTS {
                log::warn!("retired a pairing code after {MAX_PAIRING_ATTEMPTS} wrong guesses");
                self.pairing = None;
            }
            return false;
        }
        let Some(owner) = Owner::new(sender, label) else {
            return false;
        };
        self.pairing = None;
        self.owners.push(owner);
        true
    }

    /// Removes the owner `id`. Returns whether it was an owner.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.owners.len();
        self.owners.retain(|owner| owner.id() != id);
        self.owners.len() != before
    }

    /// Drops an expired code and returns the valid one.
    fn valid_pairing(&mut self, now: Instant) -> Option<Pairing> {
        if self
            .pairing
            .is_some_and(|pairing| now >= pairing.expires_at)
        {
            self.pairing = None;
        }
        self.pairing
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use alloc::format;
    use alloc::rc::Rc;
    use barracuda_platform::Entropy;
    use core::cell::Cell;

    /// Yields 0, 1, 2, ... so successive codes are 000000, 000001, ...
    #[derive(Clone)]
    struct Sequence(Rc<Cell<u32>>);

    impl Entropy for Sequence {
        fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
            let value = self.0.get();
            self.0.set(value.wrapping_add(1));
            bytes.copy_from_slice(&value.to_le_bytes()[..bytes.len()]);
            Ok(())
        }
    }

    fn book() -> OwnerBook {
        OwnerBook::new(
            Vec::new(),
            PairingEntropy::new(Sequence(Rc::new(Cell::new(0)))),
        )
    }

    fn at(seconds: u64) -> Instant {
        Instant::from_secs(seconds)
    }

    fn code(book: &mut OwnerBook, now: Instant) -> alloc::string::String {
        book.pairing(now)
            .expect("pairing code")
            .code
            .as_str()
            .into()
    }

    #[test]
    fn non_owners_are_ignored_and_counted() {
        let mut book = book();
        assert_eq!(
            book.classify("1", None, "hello", at(0)),
            Classification::Ignored
        );
        assert_eq!(
            book.classify("2", None, "/start", at(0)),
            Classification::Ignored
        );
        assert_eq!(book.ignored(), 2);
        assert!(book.owners().is_empty());
    }

    #[test]
    fn the_current_code_pairs_once_and_rotates() {
        let mut book = book();
        let first = code(&mut book, at(0));
        assert_eq!(first, "000000");
        assert_eq!(
            book.classify("ann", Some("Ann"), &first, at(10)),
            Classification::Paired
        );
        assert_eq!(
            book.owners(),
            &[Owner::new("ann", Some("Ann")).expect("owner")]
        );
        assert_eq!(
            book.classify("ann", None, "hello", at(11)),
            Classification::Owner
        );
        // The used code no longer pairs anybody.
        assert_eq!(
            book.classify("bob", None, &first, at(12)),
            Classification::Ignored
        );
        let second = code(&mut book, at(13));
        assert_ne!(second, first);
        assert_eq!(
            book.classify("bob", None, &format!("/start {second}"), at(14)),
            Classification::Paired
        );
        assert_eq!(book.owners().len(), 2);
        assert_eq!(book.ignored(), 1);
    }

    #[test]
    fn an_owner_sending_the_code_stays_an_owner_and_keeps_the_code() {
        let mut book = book();
        let first = code(&mut book, at(0));
        assert_eq!(
            book.classify("ann", None, &first, at(0)),
            Classification::Paired
        );
        let second = code(&mut book, at(0));
        assert_eq!(
            book.classify("ann", None, &second, at(0)),
            Classification::Owner
        );
        assert_eq!(code(&mut book, at(0)), second);
    }

    #[test]
    fn codes_expire_after_ten_minutes() {
        let mut book = book();
        let first = code(&mut book, at(0));
        let view = book.pairing(at(60)).expect("still valid");
        assert_eq!(view.expires_in, Duration::from_secs(540));
        assert_eq!(
            book.classify("ann", None, &first, at(600)),
            Classification::Ignored
        );
        // A new code is minted on request after expiry.
        let second = code(&mut book, at(600));
        assert_ne!(second, first);
        assert_eq!(
            book.pairing(at(600)).expect("fresh").expires_in,
            PAIRING_CODE_LIFETIME
        );
    }

    #[test]
    fn rotate_replaces_the_code() {
        let mut book = book();
        let first = code(&mut book, at(0));
        book.rotate(at(1)).expect("rotate");
        let second = code(&mut book, at(1));
        assert_ne!(first, second);
        assert_eq!(
            book.classify("ann", None, &first, at(2)),
            Classification::Ignored
        );
        assert_eq!(
            book.classify("ann", None, &second, at(2)),
            Classification::Paired
        );
    }

    #[test]
    fn wrong_guesses_retire_the_code() {
        let mut book = book();
        let current = code(&mut book, at(0));
        for _ in 0..MAX_PAIRING_ATTEMPTS {
            assert_eq!(
                book.classify("eve", None, "999999", at(1)),
                Classification::Ignored
            );
        }
        assert_eq!(
            book.classify("ann", None, &current, at(2)),
            Classification::Ignored
        );
        assert_ne!(code(&mut book, at(2)), current);
    }

    #[test]
    fn remove_drops_an_owner() {
        let mut book = book();
        let first = code(&mut book, at(0));
        assert_eq!(
            book.classify("ann", None, &first, at(0)),
            Classification::Paired
        );
        assert!(book.remove("ann"));
        assert!(!book.remove("ann"));
        assert_eq!(
            book.classify("ann", None, "hi", at(0)),
            Classification::Ignored
        );
    }

    #[test]
    fn at_most_eight_owners() {
        let mut book = book();
        for index in 0..MAX_OWNERS {
            let current = code(&mut book, at(0));
            assert_eq!(
                book.classify(&format!("owner-{index}"), None, &current, at(0)),
                Classification::Paired
            );
        }
        assert!(book.is_full());
        assert!(book.pairing(at(0)).is_none(), "pairing closes when full");
        assert_eq!(
            book.classify("late", None, "000008", at(0)),
            Classification::Ignored
        );
        assert_eq!(book.owners().len(), MAX_OWNERS);
        assert!(book.remove("owner-3"));
        assert!(book.pairing(at(0)).is_some(), "pairing reopens");
    }

    #[test]
    fn construction_keeps_eight_distinct_owners() {
        let mut owners = Vec::new();
        for index in 0..12 {
            owners.push(Owner::new(&format!("id-{}", index % 10), None).expect("owner"));
        }
        let book = OwnerBook::new(owners, PairingEntropy::unavailable());
        assert_eq!(book.owners().len(), MAX_OWNERS);
        assert_eq!(book.owners()[7].id(), "id-7");
    }

    #[test]
    fn unavailable_entropy_offers_no_code() {
        let mut book = OwnerBook::new(Vec::new(), PairingEntropy::unavailable());
        assert!(book.pairing(at(0)).is_none());
        assert_eq!(book.rotate(at(0)), Err(EntropyUnavailable));
        assert_eq!(
            book.classify("ann", None, "000000", at(0)),
            Classification::Ignored
        );
    }

    #[test]
    fn owner_identifiers_and_labels_are_bounded() {
        assert!(Owner::new("", None).is_none());
        assert!(Owner::new(&"x".repeat(OWNER_ID_MAX + 1), None).is_none());
        let label = "名".repeat(30);
        let owner = Owner::new("id", Some(&label)).expect("owner");
        assert_eq!(owner.label().expect("label").len(), 63);
        assert_eq!(Owner::new("id", Some("  ")).expect("owner").label(), None);
    }
}
