use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use super::{Cancel, ClawTimer, SleepOutcome, TimerFuture};

/// A test timer that completes immediately without waiting.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImmediateTimer;

impl ClawTimer for ImmediateTimer {
    fn sleep<'a>(&'a mut self, _duration: Duration, cancel: Cancel<'a>) -> TimerFuture<'a> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                SleepOutcome::Cancelled
            } else {
                SleepOutcome::Completed
            }
        })
    }
}

/// A test timer that yields a fixed number of polls before completing.
///
/// This models a cooperative executor shape without depending on wall-clock
/// time. It is not a production timer.
#[derive(Debug, Clone, Copy)]
pub struct YieldingTimer {
    yields: u32,
}

impl YieldingTimer {
    /// Create a timer that returns `Poll::Pending` `yields` times before
    /// completing.
    pub const fn new(yields: u32) -> Self {
        Self { yields }
    }
}

impl ClawTimer for YieldingTimer {
    fn sleep<'a>(&'a mut self, _duration: Duration, cancel: Cancel<'a>) -> TimerFuture<'a> {
        let yields = self.yields;
        Box::pin(async move {
            for _ in 0..yields {
                if cancel.is_cancelled() {
                    return SleepOutcome::Cancelled;
                }
                yield_once().await;
            }
            if cancel.is_cancelled() {
                SleepOutcome::Cancelled
            } else {
                SleepOutcome::Completed
            }
        })
    }
}

async fn yield_once() {
    struct YieldOnce(bool);

    impl Future for YieldOnce {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<()> {
            if self.0 {
                Poll::Ready(())
            } else {
                self.0 = true;
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    YieldOnce(false).await;
}
