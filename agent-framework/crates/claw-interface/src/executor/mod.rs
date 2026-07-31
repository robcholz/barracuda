//! Runtime-agnostic executor seam.
//!
//! The orchestrator drives its `!Send` engine future to completion on one owned
//! worker thread. *How* that `block_on` works is runtime-specific: the device
//! uses a cooperative `edge-executor` `block_on` (no reactor; the esp HTTP/timer
//! seams self-wake), while the host needs a tokio runtime so async `reqwest` and
//! `TokioTimer` — which poll against tokio's reactor — make progress. This trait
//! is that injection point, so `claw-core` stays executor-agnostic (it depends
//! only on the trait, never on `edge-executor` or `tokio`).

use core::future::Future;

#[cfg(feature = "tokioexecutor")]
mod tokio;

#[cfg(feature = "tokioexecutor")]
pub use tokio::TokioExecutor;

/// Injection point for "run this future to completion on the current thread".
///
/// The engine future is `!Send` and self-driving — it multiplexes every session
/// via its own poll loop and never spawns onto the executor — so an
/// implementation only needs a `block_on`, not a task spawner. The method is
/// generic, so this is a static-dispatch seam (never used as `dyn`), like
/// [`crate::ClawThread`].
pub trait ClawExecutor {
    /// Drive `future` (which may be `!Send`) to completion, returning its output.
    fn block_on<Fut: Future>(future: Fut) -> Fut::Output;
}
