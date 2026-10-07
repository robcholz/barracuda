//! Exactly sized encodings and immutable shared text in bulk memory.

use alloc::rc::Rc;
use core::fmt;

use crate::{BulkAllocError, BulkBox, BulkVec};

/// Destination of bytes written by a two-pass encoder.
pub trait ByteSink {
    /// Appends `bytes` to the output.
    fn put(&mut self, bytes: &[u8]);
}

/// Counts the bytes an encoding will occupy.
struct Measure(usize);

impl ByteSink for Measure {
    fn put(&mut self, bytes: &[u8]) {
        self.0 = self.0.saturating_add(bytes.len());
    }
}

impl ByteSink for BulkVec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

fn measure(write: &impl Fn(&mut dyn ByteSink)) -> usize {
    let mut measure = Measure(0);
    write(&mut measure);
    measure.0
}

impl BulkVec<u8> {
    /// Runs `write` once to measure and once into an exactly sized buffer.
    ///
    /// `write` must emit the same bytes on every call. Exhausting bulk memory
    /// aborts like any other allocation; use [`try_encode`](Self::try_encode)
    /// for large payloads whose failure the caller can report.
    #[must_use]
    pub fn encode(write: impl Fn(&mut dyn ByteSink)) -> Self {
        let mut output = Self::with_capacity(measure(&write));
        write(&mut output);
        output
    }

    /// Like [`encode`](Self::encode), but reports bulk-memory exhaustion.
    ///
    /// # Errors
    ///
    /// [`BulkAllocError`] when the measured length cannot be allocated.
    pub fn try_encode(write: impl Fn(&mut dyn ByteSink)) -> Result<Self, BulkAllocError> {
        let mut output = Self::try_with_capacity(measure(&write))?;
        write(&mut output);
        Ok(output)
    }
}

/// Immutable UTF-8 text in one exactly sized bulk allocation.
///
/// Cloning shares the allocation; only the reference count lives in the
/// ordinary heap.
#[derive(Clone)]
pub struct BulkText(Rc<BulkBox<[u8]>>);

impl BulkText {
    /// Copies `text` into bulk memory.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self::encode(|sink| sink.put(text.as_bytes()))
    }

    /// Builds text by running `write` once to measure and once to fill.
    ///
    /// `write` must emit the same UTF-8 bytes on every call.
    #[must_use]
    pub fn encode(write: impl Fn(&mut dyn ByteSink)) -> Self {
        Self(Rc::new(BulkVec::encode(write).into_boxed_slice()))
    }

    /// Like [`encode`](Self::encode), but reports bulk-memory exhaustion.
    ///
    /// # Errors
    ///
    /// [`BulkAllocError`] when the measured length cannot be allocated.
    pub fn try_encode(write: impl Fn(&mut dyn ByteSink)) -> Result<Self, BulkAllocError> {
        Ok(Self(Rc::new(
            BulkVec::try_encode(write)?.into_boxed_slice(),
        )))
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

impl core::ops::Deref for BulkText {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Default for BulkText {
    fn default() -> Self {
        Self::new("")
    }
}

impl PartialEq for BulkText {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Eq for BulkText {}

impl PartialEq<str> for BulkText {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for BulkText {
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl fmt::Debug for BulkText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

impl fmt::Display for BulkText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for BulkText {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_sizes_the_buffer_exactly() {
        let bytes = BulkVec::encode(|sink| {
            sink.put(b"ab");
            sink.put(b"cde");
        });
        assert_eq!(bytes.as_slice(), b"abcde");
        assert_eq!(bytes.capacity(), 5);
    }

    #[test]
    fn text_is_shared_and_compares_by_content() {
        let text = BulkText::new("héllo");
        let clone = text.clone();
        assert_eq!(clone.as_str(), "héllo");
        assert_eq!(text, "héllo");
        assert_eq!(text, BulkText::from("héllo"));
        assert_eq!(BulkText::default().len(), 0);
    }
}
