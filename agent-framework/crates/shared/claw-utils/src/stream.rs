//! Shared vocabulary for logical incremental streams.

/// One part of a logical content stream carried inside a larger event stream.
///
/// [`Delta`](Self::Delta) carries one append fragment or one complete item.
/// [`End`](Self::End) explicitly closes the logical stream, including streams
/// that emitted no deltas. A plain Rust `Stream` does not need this wrapper
/// when its own `None` already expresses the only relevant boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamPart<T> {
    /// One incremental fragment or complete list item.
    Delta(T),
    /// No more deltas will be emitted for this logical stream.
    End,
}
