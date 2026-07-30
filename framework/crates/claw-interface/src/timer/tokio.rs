use core::time::Duration;

use super::{Cancel, ClawTimer, SleepOutcome, TimerFuture};

const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Host timer backed by `tokio::time::sleep`.
///
/// Cancellation is an atomic flag rather than a waker-aware token, so long
/// sleeps are split into short slices and the flag is checked between them.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioTimer;

impl ClawTimer for TokioTimer {
    fn sleep<'a>(&'a mut self, duration: Duration, cancel: Cancel<'a>) -> TimerFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return SleepOutcome::Cancelled;
            }

            let mut remaining = duration;
            while remaining > Duration::ZERO {
                let slice = remaining.min(CANCEL_POLL_INTERVAL);
                tokio::time::sleep(slice).await;
                if cancel.is_cancelled() {
                    return SleepOutcome::Cancelled;
                }
                remaining = remaining.saturating_sub(slice);
            }

            SleepOutcome::Completed
        })
    }
}
