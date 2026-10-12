//! Futures and streams driven together by one task.
//!
//! [`Unordered`] yields each future's output as it completes, and [`Merged`]
//! interleaves the items of several streams. Unlike their `futures-util`
//! counterparts they need no `Arc` or atomic read-modify-write, so they also
//! build on targets such as ESP32-C3. Each wake polls every
//! member, which suits the handful of members these sets hold; polling
//! starts after the member that last made progress so none is starved.

use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;

/// Futures polled together; a [`Stream`] of their outputs in completion order.
///
/// The stream ends once it holds no futures.
pub struct Unordered<F> {
    futures: Vec<F>,
    next: usize,
}

impl<F> Unordered<F> {
    /// An empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            futures: Vec::new(),
            next: 0,
        }
    }

    /// Adds a future; it is first polled on the next poll of the set.
    pub fn push(&mut self, future: F) {
        self.futures.push(future);
    }

    /// Whether no future is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.futures.is_empty()
    }

    /// The number of pending futures.
    #[must_use]
    pub fn len(&self) -> usize {
        self.futures.len()
    }

    /// Drops every pending future.
    pub fn clear(&mut self) {
        self.futures.clear();
        self.next = 0;
    }
}

impl<F> Default for Unordered<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F> Extend<F> for Unordered<F> {
    fn extend<I: IntoIterator<Item = F>>(&mut self, futures: I) {
        self.futures.extend(futures);
    }
}

impl<F> IntoIterator for Unordered<F> {
    type Item = F;
    type IntoIter = alloc::vec::IntoIter<F>;

    fn into_iter(self) -> Self::IntoIter {
        self.futures.into_iter()
    }
}

impl<F: Future + Unpin> Stream for Unordered<F> {
    type Item = F::Output;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<F::Output>> {
        let this = &mut *self;
        let count = this.futures.len();
        if count == 0 {
            return Poll::Ready(None);
        }
        for offset in 0..count {
            let index = this.next.wrapping_add(offset) % count;
            let Some(future) = this.futures.get_mut(index) else {
                continue;
            };
            if let Poll::Ready(output) = Pin::new(future).poll(context) {
                drop(this.futures.swap_remove(index));
                this.next = index;
                return Poll::Ready(Some(output));
            }
        }
        Poll::Pending
    }
}

/// Streams polled together; a [`Stream`] of all their items.
///
/// A stream that ends is dropped, and the merge ends once it holds none.
pub struct Merged<S> {
    streams: Vec<S>,
    next: usize,
}

impl<S> Merged<S> {
    /// An empty merge.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            streams: Vec::new(),
            next: 0,
        }
    }

    /// Adds a stream; it is first polled on the next poll of the merge.
    pub fn push(&mut self, stream: S) {
        self.streams.push(stream);
    }

    /// Whether no stream remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// The number of streams that have not ended.
    #[must_use]
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    /// Drops every stream.
    pub fn clear(&mut self) {
        self.streams.clear();
        self.next = 0;
    }
}

impl<S> Default for Merged<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: Stream + Unpin> Stream for Merged<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<S::Item>> {
        let this = &mut *self;
        let mut index = this.next;
        let mut remaining = this.streams.len();
        while remaining > 0 {
            if index >= this.streams.len() {
                index = 0;
            }
            let Some(stream) = this.streams.get_mut(index) else {
                break;
            };
            match Pin::new(stream).poll_next(context) {
                Poll::Ready(Some(item)) => {
                    this.next = index.wrapping_add(1);
                    return Poll::Ready(Some(item));
                }
                Poll::Ready(None) => drop(this.streams.swap_remove(index)),
                Poll::Pending => index = index.wrapping_add(1),
            }
            remaining = remaining.saturating_sub(1);
        }
        if this.streams.is_empty() {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use core::future::{pending, ready, Ready};

    use futures_lite::future::block_on;
    use futures_lite::stream::{self, StreamExt as _};

    use super::*;

    type Boxed = Pin<alloc::boxed::Box<dyn Future<Output = u32>>>;

    #[test]
    fn unordered_yields_ready_outputs_and_ends_when_empty() {
        let mut futures: Unordered<Boxed> = Unordered::new();
        futures.push(alloc::boxed::Box::pin(pending()));
        futures.push(alloc::boxed::Box::pin(ready(1)));
        futures.push(alloc::boxed::Box::pin(ready(2)));
        let mut outputs = [
            block_on(futures.next()).unwrap_or_default(),
            block_on(futures.next()).unwrap_or_default(),
        ];
        outputs.sort_unstable();
        assert_eq!(outputs, [1, 2]);
        assert_eq!(futures.len(), 1);
        futures.clear();
        assert_eq!(block_on(futures.next()), None);
    }

    #[test]
    fn unordered_extends_from_another_set() {
        let mut first: Unordered<Ready<u32>> = Unordered::new();
        first.push(ready(1));
        let mut second = Unordered::new();
        second.push(ready(2));
        first.extend(second);
        assert_eq!(block_on(first.collect::<Vec<_>>()).len(), 2);
    }

    #[test]
    fn merged_interleaves_streams_and_drops_ended_ones() {
        let mut merged = Merged::new();
        merged.push(stream::iter(alloc::vec![1, 2]));
        merged.push(stream::iter(alloc::vec![10]));
        let mut items = block_on(merged.collect::<Vec<_>>());
        items.sort_unstable();
        assert_eq!(items, [1, 2, 10]);
        let empty: Merged<stream::Iter<alloc::vec::IntoIter<u32>>> = Merged::new();
        assert!(empty.is_empty());
    }
}
