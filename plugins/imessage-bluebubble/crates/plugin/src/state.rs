//! Receive state shared by the webhook endpoint and the receive session:
//! the webhook secret, the catch-up cursor with its dedup ring, and the
//! bounded inbox between them.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use core::cell::{Cell, RefCell};

use barracuda_platform::Entropy;
use barracuda_plugin::manager::{PluginStorage, StorageError};
use bluebubbles::inbound::InboundMessage;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use serde::{Deserialize, Serialize};

/// Path prefix of the webhook; the secret is the one segment after it.
pub(crate) const HOOK_PATH: &str = "/api/gateway/bluebubbles/hook";
/// Plugin storage key of the webhook record ([`HookRecord`]).
pub(crate) const HOOK_STORAGE_KEY: &str = "webhook";
/// Plugin storage key of the catch-up cursor ([`Cursor`]).
pub(crate) const CURSOR_STORAGE_KEY: &str = "cursor";
/// Message GUIDs remembered for dedup.
pub(crate) const SEEN_GUIDS: usize = 32;
/// Messages the inbox holds before it drops deliveries.
pub(crate) const INBOX_MESSAGES: usize = 8;
/// Text bytes the inbox holds before it drops deliveries.
pub(crate) const INBOX_BYTES: usize = 16 * 1024;
/// Random bytes in a webhook secret (128 bits).
const SECRET_BYTES: usize = 16;

/// The webhook this device registered, persisted under
/// [`HOOK_STORAGE_KEY`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct HookRecord {
    /// Server URL the secret was minted for; a new server gets a new secret.
    pub(crate) server: String,
    /// 32 lower-case hex digits.
    pub(crate) secret: String,
    /// URL last registered with the server, so a stale one can be deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
}

/// Catch-up cursor, persisted under [`CURSOR_STORAGE_KEY`].
///
/// The message query's `after` is inclusive, so the newest GUIDs are kept to
/// skip messages already handled.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct Cursor {
    /// Creation time (Unix ms) of the newest message handled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) after: Option<u64>,
    /// GUIDs handled most recently, oldest first, at most [`SEEN_GUIDS`].
    #[serde(default)]
    pub(crate) seen: VecDeque<String>,
}

impl Cursor {
    fn contains(&self, guid: &str) -> bool {
        self.seen.iter().any(|seen| seen == guid)
    }

    fn record(&mut self, guid: &str, date: Option<u64>) {
        if !self.contains(guid) {
            self.seen.push_back(guid.into());
            while self.seen.len() > SEEN_GUIDS {
                self.seen.pop_front();
            }
        }
        if let Some(date) = date {
            self.after = Some(self.after.map_or(date, |after| after.max(date)));
        }
    }
}

/// What the inbox did with a delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// Queued for the receive session.
    Queued,
    /// Already handled or queued.
    Duplicate,
    /// The inbox is full; the delivery is lost until the next catch-up.
    Full,
}

/// Receive state of the channel; lives as long as the Plugin.
pub(crate) struct ReceiveBook {
    hook: RefCell<Option<HookRecord>>,
    cursor: RefCell<Cursor>,
    /// The cursor changed since it was last stored.
    dirty: Cell<bool>,
    inbox: RefCell<VecDeque<InboundMessage>>,
    inbox_bytes: Cell<usize>,
    arrived: Signal<NoopRawMutex, ()>,
    /// A delivery was lost; the session catches up.
    missed: Cell<bool>,
    lost: Cell<u32>,
    skipped: Cell<u32>,
}

impl ReceiveBook {
    /// Reads the stored webhook record and cursor; unreadable values start
    /// over.
    pub(crate) async fn load<Storage: PluginStorage>(
        storage: &Storage,
    ) -> Result<Self, StorageError> {
        let hook = read_json::<HookRecord, _>(storage, HOOK_STORAGE_KEY).await?;
        let cursor = read_json::<Cursor, _>(storage, CURSOR_STORAGE_KEY)
            .await?
            .unwrap_or_default();
        Ok(Self {
            hook: RefCell::new(hook),
            cursor: RefCell::new(cursor),
            dirty: Cell::new(false),
            inbox: RefCell::new(VecDeque::new()),
            inbox_bytes: Cell::new(0),
            arrived: Signal::new(),
            missed: Cell::new(false),
            lost: Cell::new(0),
            skipped: Cell::new(0),
        })
    }

    /// The webhook record, when one is stored.
    pub(crate) fn hook(&self) -> Option<HookRecord> {
        self.hook.borrow().clone()
    }

    /// Whether `candidate` is the current secret, compared in constant time.
    pub(crate) fn secret_matches(&self, candidate: &str) -> bool {
        self.hook
            .borrow()
            .as_ref()
            .is_some_and(|hook| constant_time_eq(hook.secret.as_bytes(), candidate.as_bytes()))
    }

    /// Returns the record for `server`, minting a new secret (and starting a
    /// new cursor) when there is none or it belongs to another server.
    ///
    /// # Errors
    ///
    /// Fails when the Platform has no entropy or storage fails.
    pub(crate) async fn secret_for<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        entropy: &impl Entropy,
        server: &str,
    ) -> Result<HookRecord, SecretError> {
        if let Some(hook) = self.hook() {
            if hook.server == server && valid_secret(&hook.secret) {
                return Ok(hook);
            }
        }
        let mut bytes = [0_u8; SECRET_BYTES];
        entropy
            .fill(&mut bytes)
            .map_err(|_unavailable| SecretError::Entropy)?;
        let hook = HookRecord {
            server: server.into(),
            secret: hex(&bytes),
            url: None,
        };
        // A new server has its own message history.
        self.cursor.replace(Cursor::default());
        self.dirty.set(false);
        storage
            .delete(CURSOR_STORAGE_KEY)
            .await
            .map_err(SecretError::Storage)?;
        self.store_hook(storage, hook.clone())
            .await
            .map_err(SecretError::Storage)?;
        Ok(hook)
    }

    /// Stores `hook` as the current record.
    pub(crate) async fn store_hook<Storage: PluginStorage>(
        &self,
        storage: &Storage,
        hook: HookRecord,
    ) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(&hook).unwrap_or_default();
        storage.put(HOOK_STORAGE_KEY, bytes.as_slice()).await?;
        self.hook.replace(Some(hook));
        Ok(())
    }

    /// Creation time of the newest message handled, if any.
    pub(crate) fn after(&self) -> Option<u64> {
        self.cursor.borrow().after
    }

    /// Whether `guid` was handled or is queued.
    pub(crate) fn is_seen(&self, guid: &str) -> bool {
        self.cursor.borrow().contains(guid)
            || self.inbox.borrow().iter().any(|queued| queued.guid == guid)
    }

    /// Marks `message` handled and advances the cursor past it.
    pub(crate) fn record(&self, message: &InboundMessage) {
        self.cursor
            .borrow_mut()
            .record(&message.guid, message.date_created);
        self.dirty.set(true);
    }

    /// Starts the cursor at `after` without a message, for a server with
    /// no messages yet.
    pub(crate) fn start_at(&self, after: u64) {
        self.cursor.borrow_mut().after = Some(after);
        self.dirty.set(true);
    }

    /// Whether the cursor has changes to store.
    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty.get()
    }

    /// Stores the cursor.
    pub(crate) async fn flush<Storage: PluginStorage>(
        &self,
        storage: &Storage,
    ) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(&*self.cursor.borrow()).unwrap_or_default();
        self.dirty.set(false);
        let stored = storage.put(CURSOR_STORAGE_KEY, bytes.as_slice()).await;
        if stored.is_err() {
            self.dirty.set(true);
        }
        stored
    }

    /// Queues a webhook delivery for the receive session.
    pub(crate) fn offer(&self, message: InboundMessage) -> Offer {
        if self.is_seen(&message.guid) {
            return Offer::Duplicate;
        }
        let size = footprint(&message);
        let mut inbox = self.inbox.borrow_mut();
        if inbox.len() >= INBOX_MESSAGES
            || self.inbox_bytes.get().saturating_add(size) > INBOX_BYTES
        {
            drop(inbox);
            self.lose();
            return Offer::Full;
        }
        inbox.push_back(message);
        self.inbox_bytes
            .set(self.inbox_bytes.get().saturating_add(size));
        self.arrived.signal(());
        Offer::Queued
    }

    /// Waits until the inbox holds a message or a delivery was lost.
    /// Cancel-safe.
    pub(crate) async fn arrival(&self) {
        while !self.has_queued() && !self.missed.get() {
            self.arrived.wait().await;
        }
    }

    /// Whether the inbox holds a message.
    pub(crate) fn has_queued(&self) -> bool {
        !self.inbox.borrow().is_empty()
    }

    /// Takes the oldest queued message.
    pub(crate) fn next(&self) -> Option<InboundMessage> {
        let message = self.inbox.borrow_mut().pop_front()?;
        self.inbox_bytes
            .set(self.inbox_bytes.get().saturating_sub(footprint(&message)));
        Some(message)
    }

    /// Drops every queued message, when receiving stops.
    pub(crate) fn clear_inbox(&self) {
        self.inbox.borrow_mut().clear();
        self.inbox_bytes.set(0);
        self.missed.set(false);
    }

    /// Counts a lost delivery and asks the session to catch up.
    pub(crate) fn lose(&self) {
        self.lost.set(self.lost.get().saturating_add(1));
        self.missed.set(true);
        self.arrived.signal(());
    }

    /// Counts deliveries lost for good, such as messages skipped because the
    /// backlog was longer than one catch-up takes.
    pub(crate) fn lose_many(&self, count: u32) {
        self.lost.set(self.lost.get().saturating_add(count));
    }

    /// Takes the request to catch up.
    pub(crate) fn take_missed(&self) -> bool {
        self.missed.replace(false)
    }

    /// Counts a message that is not plain text.
    pub(crate) fn skip(&self) {
        self.skipped.set(self.skipped.get().saturating_add(1));
    }

    /// Messages lost since boot: over the size cap, a full inbox, or a
    /// backlog longer than one catch-up.
    pub(crate) fn lost(&self) -> u32 {
        self.lost.get()
    }

    /// Messages skipped since boot because they are not plain text.
    pub(crate) fn skipped(&self) -> u32 {
        self.skipped.get()
    }
}

/// Why the webhook secret is unavailable.
#[derive(Debug)]
pub(crate) enum SecretError {
    /// The Platform has no entropy source.
    Entropy,
    /// Storing the record failed.
    Storage(StorageError),
}

/// Bytes a queued message holds.
fn footprint(message: &InboundMessage) -> usize {
    [
        message.guid.len(),
        message.text.as_deref().map_or(0, str::len),
        message.sender().map_or(0, str::len),
        message.chat_guid().map_or(0, str::len),
    ]
    .iter()
    .fold(0_usize, |total, size| total.saturating_add(*size))
}

async fn read_json<T, Storage>(storage: &Storage, key: &str) -> Result<Option<T>, StorageError>
where
    T: for<'de> Deserialize<'de>,
    Storage: PluginStorage,
{
    let Some(bytes) = storage.get_bytes(key).await? else {
        return Ok(None);
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(_error) => {
            log::warn!("replacing unreadable BlueBubbles receive state `{key}`");
            Ok(None)
        }
    }
}

fn valid_secret(secret: &str) -> bool {
    secret.len() == SECRET_BYTES.saturating_mul(2)
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Lower-case hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        text.push_str(&format!("{byte:02x}"));
        text
    })
}

/// Compares two byte strings in time that depends only on their lengths.
pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cursor_keeps_the_newest_guids_and_time() {
        let mut cursor = Cursor::default();
        for index in 0..40_u64 {
            cursor.record(&format!("guid-{index}"), Some(1_000 + index));
        }
        cursor.record("late", Some(5));
        assert_eq!(cursor.seen.len(), SEEN_GUIDS);
        assert!(!cursor.contains("guid-0"));
        assert!(cursor.contains("guid-39") && cursor.contains("late"));
        assert_eq!(cursor.after, Some(1_039));
    }

    #[test]
    fn secrets_compare_whole_strings() {
        assert!(constant_time_eq(b"00ff", b"00ff"));
        assert!(!constant_time_eq(b"00ff", b"00fe"));
        assert!(!constant_time_eq(b"00ff", b"00f"));
        assert!(valid_secret(&hex(&[0xab; SECRET_BYTES])));
        assert!(!valid_secret("ABAB"));
    }
}
