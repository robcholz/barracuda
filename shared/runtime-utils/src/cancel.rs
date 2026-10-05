use core::fmt;
use core::future::{poll_fn, Future};
use core::task::Poll;

use futures_util::task::AtomicWaker;
use portable_atomic::{AtomicBool, Ordering};

/// Caller-owned, wakeable cooperative cancellation state.
pub struct CancellationFlag {
    cancelled: AtomicBool,
    waker: AtomicWaker,
}

impl fmt::Debug for CancellationFlag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationFlag")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl CancellationFlag {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.waker.wake();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for CancellationFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// Cooperative cancellation token backed by a caller-owned wakeable flag.
#[derive(Debug, Clone, Copy)]
pub struct Cancel<'a>(&'a CancellationFlag);

impl<'a> Cancel<'a> {
    #[must_use]
    pub const fn new(flag: &'a CancellationFlag) -> Self {
        Self(flag)
    }

    #[must_use]
    pub fn is_cancelled(self) -> bool {
        self.0.is_cancelled()
    }

    /// Wait until cancellation is requested.
    pub fn cancelled(self) -> impl Future<Output = ()> + 'a {
        poll_fn(move |context| {
            if self.is_cancelled() {
                return Poll::Ready(());
            }
            self.0.waker.register(context.waker());
            if self.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
    }
}

impl Cancel<'static> {
    #[must_use]
    pub fn never() -> Self {
        static NEVER: CancellationFlag = CancellationFlag::new();
        Self(&NEVER)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::pin::pin;
    use core::task::{Context, Poll};

    use futures_util::task::{waker_ref, ArcWake};
    use portable_atomic::{AtomicUsize, Ordering};

    use super::*;

    struct WakeCounter(AtomicUsize);

    impl ArcWake for WakeCounter {
        fn wake_by_ref(counter: &Arc<Self>) {
            counter.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn cancellation_wakes_a_registered_waiter() {
        let flag = CancellationFlag::new();
        let mut cancelled = pin!(Cancel::new(&flag).cancelled());
        let counter = Arc::new(WakeCounter(AtomicUsize::new(0)));
        let waker = waker_ref(&counter);
        let mut context = Context::from_waker(&waker);

        assert_eq!(cancelled.as_mut().poll(&mut context), Poll::Pending);
        flag.cancel();
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);
        assert_eq!(cancelled.as_mut().poll(&mut context), Poll::Ready(()));
    }
}
