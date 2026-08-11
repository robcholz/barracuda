//! Per-call retry loop used by [`crate::ClawApi`].
//!
//! Retry is a per-request policy ([`crate::RetryPolicy`] on each request),
//! so the loop lives just above the backend call rather than in the transport.
//! Only operations whose error reports [`is_retryable`](crate::ClawApiError::is_retryable)
//! are retried; deadlines and backoff use the global `embassy-time` driver.

use claw_utils::Cancel;
use core::future::Future;
use embassy_futures::select::{select, select3, Either, Either3};
use embassy_time::Timer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeadlineError {
    Cancelled,
    Elapsed,
}

pub(crate) async fn with_timeout<F>(
    future: F,
    timeout_ms: u32,
    cancel: Cancel<'_>,
) -> Result<F::Output, DeadlineError>
where
    F: Future,
{
    if cancel.is_cancelled() {
        return Err(DeadlineError::Cancelled);
    }
    match select3(
        cancel.cancelled(),
        future,
        Timer::after_millis(u64::from(timeout_ms)),
    )
    .await
    {
        Either3::First(()) => Err(DeadlineError::Cancelled),
        Either3::Second(output) => Ok(output),
        Either3::Third(()) => Err(DeadlineError::Elapsed),
    }
}

/// Async backoff sleep that wakes immediately when cancellation is requested.
pub(crate) async fn sleep_abortable_async(total_ms: u32, cancel: Cancel<'_>) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    match select(cancel.cancelled(), Timer::after_millis(u64::from(total_ms))).await {
        Either::First(()) => false,
        Either::Second(()) => true,
    }
}
