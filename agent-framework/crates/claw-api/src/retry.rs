//! Per-call retry loop used by [`crate::ClawApi`].
//!
//! Retry is a per-request policy ([`crate::RetryPolicy`] on each request),
//! so the loop lives just above the backend call rather than in the transport.
//! Only operations whose error reports [`is_retryable`](crate::ClawApiError::is_retryable)
//! are retried; deadlines and backoff use the global `embassy-time` driver.

use claw_utils::Cancel;
use core::future::{poll_fn, Future};
use core::task::Poll;
use embassy_time::Timer;

const CANCEL_POLL_INTERVAL_MS: u32 = 50;

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
    let mut operation = core::pin::pin!(future);
    let mut timeout = core::pin::pin!(Timer::after_millis(u64::from(timeout_ms)));
    poll_fn(move |context| {
        if cancel.is_cancelled() {
            return Poll::Ready(Err(DeadlineError::Cancelled));
        }
        if let Poll::Ready(output) = operation.as_mut().poll(context) {
            return Poll::Ready(Ok(output));
        }
        timeout
            .as_mut()
            .poll(context)
            .map(|()| Err(DeadlineError::Elapsed))
    })
    .await
}

/// Async backoff sleep with frequent cancellation checks.
pub(crate) async fn sleep_abortable_async(total_ms: u32, cancel: Cancel<'_>) -> bool {
    let mut remaining_ms = total_ms;
    while remaining_ms > 0 {
        if cancel.is_cancelled() {
            return false;
        }
        let slice_ms = remaining_ms.min(CANCEL_POLL_INTERVAL_MS);
        Timer::after_millis(u64::from(slice_ms)).await;
        remaining_ms = remaining_ms.saturating_sub(slice_ms);
    }
    !cancel.is_cancelled()
}
