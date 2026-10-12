//! Receive bookkeeping a channel holds only once it receives.

use alloc::boxed::Box;
use core::cell::OnceCell;
use core::future::Future;

/// State a receive loop loads from storage when its first session starts,
/// such as a cursor and the message ids it recently handled.
///
/// A channel that is not configured, or never enters `send_receive`, holds
/// one pointer instead of the state and never reads it from storage. Once
/// loaded it is kept, across sessions, until the Plugin is dropped.
pub struct OnDemand<T>(OnceCell<Box<T>>);

impl<T> OnDemand<T> {
    /// Nothing loaded yet.
    #[must_use]
    pub const fn new() -> Self {
        Self(OnceCell::new())
    }

    /// The state, once a session loaded it.
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.0.get().map(|state| &**state)
    }

    /// The state, loading it with `load` first if no session has yet.
    ///
    /// Only the channel's receive task loads; if a load completed meanwhile,
    /// that one is kept and this one dropped.
    ///
    /// # Errors
    ///
    /// Returns `load`'s error; nothing is kept and the next call loads again.
    pub async fn get_or_load<E, F>(&self, load: impl FnOnce() -> F) -> Result<&T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        if let Some(state) = self.get() {
            return Ok(state);
        }
        let state = load().await?;
        Ok(&**self.0.get_or_init(|| Box::new(state)))
    }
}

impl<T> Default for OnDemand<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use futures_lite::future::block_on;

    #[test]
    fn loads_once_and_retries_after_an_error() {
        let state = OnDemand::<u32>::new();
        assert_eq!(state.get(), None);
        let failed = block_on(state.get_or_load(|| async { Err::<u32, &str>("busy") }));
        assert_eq!(failed, Err("busy"));
        assert_eq!(state.get(), None);
        let loaded = block_on(state.get_or_load(|| async { Ok::<u32, &str>(7) }));
        assert_eq!(loaded, Ok(&7));
        let again = block_on(state.get_or_load(|| async { Ok::<u32, &str>(9) }));
        assert_eq!(again, Ok(&7), "a loaded state is kept");
        assert_eq!(state.get(), Some(&7));
    }
}
