//! Immutable shared text in bulk memory.

use alloc::rc::Rc;
use core::fmt;

use barracuda_bulk_memory::BulkBox;

use crate::json::{self, Sink};

/// Immutable UTF-8 text in one exactly sized bulk allocation.
///
/// Cloning shares the allocation; only the reference count lives in the
/// ordinary heap.
#[derive(Clone)]
pub struct Text(Rc<BulkBox<[u8]>>);

impl Text {
    /// Copies `text` into bulk memory.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self::encode(|sink| sink.put(text.as_bytes()))
    }

    /// Builds text by running `write` once to measure and once to fill.
    ///
    /// `write` must emit the same UTF-8 bytes on every call.
    #[must_use]
    pub fn encode(write: impl Fn(&mut dyn Sink)) -> Self {
        Self(Rc::new(json::encode(write).into_boxed_slice()))
    }

    /// The text as bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Every constructor writes UTF-8, so this never falls back.
        core::str::from_utf8(&self.0).unwrap_or_default()
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new("")
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for Text {}

impl fmt::Debug for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}
