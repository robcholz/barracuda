//! Runtime-agnostic async timer seam.
//!
//! `claw-api` needs retry backoff in async code, but it must not depend on a
//! specific executor such as tokio, edge-executor, or embedded-executor. This
//! trait is the injected boundary: each runtime supplies a small wrapper that
//! waits using that runtime's timer primitive.

use core::future::Future;
use core::pin::Pin;
use core::time::Duration;

use crate::http::Cancel;

#[cfg(feature = "tokiotimer")]
#[path = "tokio.rs"]
pub mod tokio_timer;

#[cfg(feature = "timermock")]
#[path = "testing.rs"]
pub mod mock;

/// Boxed future returned by [`ClawTimer::sleep`].
///
/// The future borrows the timer implementation and the caller-owned
/// cancellation token, so it cannot outlive either one.
pub type TimerFuture<'a> = Pin<Box<dyn Future<Output = SleepOutcome> + 'a>>;

/// Result of an abortable sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepOutcome {
    /// The requested duration elapsed.
    Completed,
    /// Cancellation was observed before the duration elapsed.
    Cancelled,
}

impl SleepOutcome {
    /// Whether the sleep completed normally.
    pub const fn is_completed(self) -> bool {
        matches!(self, Self::Completed)
    }

    /// Whether cancellation interrupted the sleep.
    pub const fn is_cancelled(self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Async timer injection point.
///
/// Thread-safety is intentionally not a supertrait requirement. A host wrapper
/// around tokio may be freely shareable, while an embedded timer may be
/// task-local. Require `Send`/`Sync` only at the caller boundary that actually
/// crosses threads.
///
/// Implementations should check [`Cancel`] before sleeping and at their natural
/// timer granularity while sleeping. For runtimes whose sleep future cannot be
/// externally woken by an atomic flag, implement this by sleeping in short
/// slices and checking `cancel` between slices.
pub trait ClawTimer {
    /// Sleep for `duration`, returning [`SleepOutcome::Cancelled`] if
    /// cancellation is observed first.
    fn sleep<'a>(&'a mut self, duration: Duration, cancel: Cancel<'a>) -> TimerFuture<'a>;
}
