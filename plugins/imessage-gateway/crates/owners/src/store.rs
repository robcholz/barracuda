//! Persistent owner book shared by a channel's receive loop and endpoints.

use alloc::vec::Vec;
use core::cell::RefCell;

use barracuda_platform::EntropyUnavailable;
use barracuda_plugin::manager::{PluginStorage, StorageError};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::Instant;

use crate::book::{Classification, Owner, OwnerBook, PairingView};
use crate::code::PairingEntropy;

/// Plugin storage key holding the channel's owner list as a JSON array.
pub const OWNERS_STORAGE_KEY: &str = "owners";

/// Failure loading or persisting the owner list.
#[derive(Debug, thiserror::Error)]
pub enum OwnersError {
    /// Plugin storage failed.
    #[error("owner storage failed: {0}")]
    Storage(#[from] StorageError),
    /// The stored owner list is not valid JSON.
    #[error("stored owner list is invalid")]
    Invalid,
}

/// Owner book of one channel, persisted in that channel Plugin's storage.
///
/// The owner list survives reboots under [`OWNERS_STORAGE_KEY`]. The pairing
/// code and the ignored-message counter live in RAM only, so a reboot retires
/// the code.
pub struct Owners<Storage> {
    storage: Storage,
    book: RefCell<OwnerBook>,
    /// Serializes writes so the latest list is always the one stored last.
    writes: Mutex<NoopRawMutex, ()>,
}

impl<Storage: PluginStorage> Owners<Storage> {
    /// Loads the stored owner list; a channel without one starts empty.
    ///
    /// # Errors
    ///
    /// Returns [`OwnersError`] when storage fails or the stored list is not
    /// valid.
    pub async fn load(storage: Storage, entropy: PairingEntropy) -> Result<Self, OwnersError> {
        let owners = match storage.get_bytes(OWNERS_STORAGE_KEY).await? {
            Some(bytes) => serde_json::from_slice::<Vec<Owner>>(&bytes)
                .map_err(|_error| OwnersError::Invalid)?,
            None => Vec::new(),
        };
        Ok(Self {
            storage,
            book: RefCell::new(OwnerBook::new(owners, entropy)),
            writes: Mutex::new(()),
        })
    }

    /// Classifies one inbound message and stores a newly paired owner.
    ///
    /// A pairing whose write fails still holds until reboot; the failure is
    /// logged.
    pub async fn classify(&self, sender: &str, label: Option<&str>, text: &str) -> Classification {
        let classification = self
            .book
            .borrow_mut()
            .classify(sender, label, text, Instant::now());
        if classification == Classification::Paired {
            log::info!("paired a new channel owner");
            if let Err(error) = self.persist().await {
                log::error!("failed to store the paired owner: {error}");
            }
        }
        classification
    }

    /// Whether `id` is an owner.
    #[must_use]
    pub fn is_owner(&self, id: &str) -> bool {
        self.book.borrow().is_owner(id)
    }

    /// Number of owners.
    #[must_use]
    pub fn count(&self) -> usize {
        self.book.borrow().owners().len()
    }

    /// Messages dropped since boot because their sender is not an owner.
    #[must_use]
    pub fn ignored(&self) -> u32 {
        self.book.borrow().ignored()
    }

    /// Copies the owner list.
    #[must_use]
    pub fn owners(&self) -> Vec<Owner> {
        self.book.borrow().owners().to_vec()
    }

    /// Returns the valid pairing code, minting one when none is valid.
    ///
    /// Returns `None` while the list is full or no entropy is available.
    #[must_use]
    pub fn pairing(&self) -> Option<PairingView> {
        self.book.borrow_mut().pairing(Instant::now())
    }

    /// Replaces the pairing code with a freshly minted one.
    ///
    /// # Errors
    ///
    /// Returns [`EntropyUnavailable`] when no code can be minted.
    pub fn rotate(&self) -> Result<(), EntropyUnavailable> {
        self.book.borrow_mut().rotate(Instant::now())
    }

    /// Removes and stores the removal of owner `id`. Returns whether it was
    /// an owner.
    ///
    /// # Errors
    ///
    /// Returns [`OwnersError`] when the shortened list cannot be stored; the
    /// owner is removed until reboot.
    pub async fn remove(&self, id: &str) -> Result<bool, OwnersError> {
        if !self.book.borrow_mut().remove(id) {
            return Ok(false);
        }
        self.persist().await?;
        Ok(true)
    }

    /// Stores the current list; the lock orders concurrent writers.
    async fn persist(&self) -> Result<(), OwnersError> {
        let _write = self.writes.lock().await;
        let bytes = serde_json::to_vec(self.book.borrow().owners())
            .map_err(|_error| OwnersError::Invalid)?;
        self.storage
            .put(OWNERS_STORAGE_KEY, bytes.as_slice())
            .await?;
        Ok(())
    }
}
