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

use crate::errors::{ChatError, ChatJsonError, ClawApiError, InferMediaError};
use crate::transport::Error as HttpError;
use crate::RetryPolicy;

pub(crate) struct RetryState {
    policy: RetryPolicy,
    retries: u32,
}

pub(crate) struct RetryDelay {
    pub(crate) failed_attempt: u64,
    pub(crate) next_attempt: u64,
    pub(crate) backoff_ms: u32,
}

impl RetryState {
    pub(crate) fn new(policy: RetryPolicy) -> Self {
        Self { policy, retries: 0 }
    }

    pub(crate) fn attempt(&self) -> u64 {
        u64::from(self.retries).saturating_add(1)
    }

    pub(crate) fn max_attempts(&self) -> u64 {
        u64::from(self.policy.max_retries).saturating_add(1)
    }

    pub(crate) fn can_retry(&self, retryable: bool) -> bool {
        retryable && self.retries < self.policy.max_retries
    }

    pub(crate) fn advance(&mut self) -> RetryDelay {
        let failed_attempt = self.attempt();
        self.retries = self.retries.saturating_add(1);
        RetryDelay {
            failed_attempt,
            next_attempt: self.attempt(),
            backoff_ms: self.policy.backoff_ms(self.retries),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeadlineError {
    Cancelled,
    Elapsed,
}

pub(crate) fn deadline_error(error: DeadlineError) -> ClawApiError {
    match error {
        DeadlineError::Cancelled => ClawApiError::Transport(HttpError::Cancelled),
        DeadlineError::Elapsed => ClawApiError::Timeout,
    }
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

pub(crate) async fn timed<T, E>(
    future: impl Future<Output = Result<T, E>>,
    timeout_ms: u32,
    cancel: Cancel<'_>,
) -> Result<T, E>
where
    E: From<ClawApiError>,
{
    match with_timeout(future, timeout_ms, cancel).await {
        Ok(result) => result,
        Err(error) => Err(deadline_error(error).into()),
    }
}

pub(crate) trait RetryError: Sized {
    fn is_retryable(&self) -> bool;
    fn cancelled() -> Self;
}

impl RetryError for ChatJsonError {
    fn is_retryable(&self) -> bool {
        self.is_retryable()
    }

    fn cancelled() -> Self {
        ChatError::Api(ClawApiError::Transport(HttpError::Cancelled)).into()
    }
}

impl RetryError for InferMediaError {
    fn is_retryable(&self) -> bool {
        self.is_retryable()
    }

    fn cancelled() -> Self {
        ClawApiError::Transport(HttpError::Cancelled).into()
    }
}

pub(crate) async fn retry_call<T, E, F>(
    policy: RetryPolicy,
    cancel: Cancel<'_>,
    mut call: F,
) -> Result<T, E>
where
    E: RetryError,
    F: AsyncFnMut() -> Result<T, E>,
{
    let mut retry = RetryState::new(policy);
    loop {
        match call().await {
            Ok(value) => return Ok(value),
            Err(error) if retry.can_retry(error.is_retryable()) => {
                if !sleep_or_cancel(retry.advance().backoff_ms, cancel).await {
                    return Err(E::cancelled());
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Async backoff sleep that wakes immediately when cancellation is requested.
pub(crate) async fn sleep_or_cancel(total_ms: u32, cancel: Cancel<'_>) -> bool {
    if cancel.is_cancelled() {
        return false;
    }
    match select(cancel.cancelled(), Timer::after_millis(u64::from(total_ms))).await {
        Either::First(()) => false,
        Either::Second(()) => true,
    }
}
