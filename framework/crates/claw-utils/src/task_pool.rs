//! Single-thread cooperative tasks driven by an owning event loop.

use core::cell::{Cell, RefCell};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;
use futures_core::Stream;
use futures_util::stream::FuturesUnordered;

type PooledFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;

/// A cloneable, single-thread task pool driven by an external event loop.
///
/// Clones refer to the same pool. Components submit owned futures through any
/// clone, while the engine that owns the event loop calls [`drive`](Self::drive)
/// with the [`Context`] received by its own [`Future::poll`]. The pool creates
/// no thread, executor, or runner of its own.
///
/// Dropping a [`JobHandle`] discards only the caller's ability to observe the
/// result; the submitted job remains owned by the pool and runs to completion.
#[derive(Clone)]
pub struct TaskPool {
    inner: Rc<TaskPoolInner>,
}

struct TaskPoolInner {
    /// Submissions stay separate from `active` so a future may submit another
    /// future while `active` is mutably borrowed by `drive`.
    inbox: RefCell<VecDeque<PooledFuture>>,
    active: RefCell<FuturesUnordered<PooledFuture>>,
    driving: Cell<bool>,
}

/// The pool was dropped before a submitted job produced its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("task pool dropped before the job completed")]
pub struct JobCancelled;

/// A result observer for one job submitted to a [`TaskPool`].
///
/// Awaiting the handle waits for the output. [`try_take`](Self::try_take)
/// checks for a completed output without waiting, which lets stateful callers
/// collect a background result the next time they are polled or invoked.
/// Dropping the handle does not cancel the job.
pub struct JobHandle<T> {
    receiver: oneshot::Receiver<T>,
}

impl TaskPool {
    /// Submit an owned future and return a handle for its eventual output.
    ///
    /// Submission only transfers ownership into the pool. The future is first
    /// polled by a subsequent [`drive`](Self::drive) call. Dropping the returned
    /// handle does not cancel the submitted future.
    pub fn submit<F>(&self, future: F) -> JobHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let (sender, receiver) = oneshot::channel();
        let pooled = async move {
            let output = future.await;
            // A caller may intentionally discard its handle while leaving the
            // pool-owned job running. In that case the output is simply dropped.
            let _ = sender.send(output);
        };

        self.inner.inbox.borrow_mut().push_back(Box::pin(pooled));

        JobHandle { receiver }
    }

    /// Advance jobs that are ready to make progress.
    ///
    /// The caller must pass the [`Context`] from the owning event loop's current
    /// [`Future::poll`] call. Each invocation starts newly submitted jobs and
    /// drains at most the jobs that were already active at the start of this
    /// call, so completion chains cannot monopolize the event loop.
    ///
    /// Reentrant calls are harmless no-ops. The outer call remains responsible
    /// for progressing the pool. Returns the number of jobs completed by this
    /// invocation.
    pub fn drive(&self, task_context: &mut Context<'_>) -> usize {
        if self.inner.driving.replace(true) {
            return 0;
        }
        let _guard = DriveGuard(&self.inner.driving);

        self.drain_inbox();

        let mut completed = 0usize;
        let mut active = self.inner.active.borrow_mut();
        let completion_budget = active.len();
        while completed < completion_budget {
            match Pin::new(&mut *active).poll_next(task_context) {
                Poll::Ready(Some(())) => {
                    completed = completed.saturating_add(1);
                }
                Poll::Ready(None) | Poll::Pending => break,
            }
        }
        drop(active);

        // Futures submitted by a job during the loop above are deliberately
        // left in the inbox until the next drive call. Ensure the owning event
        // loop gets that next poll even if the submitting job itself returned
        // `Pending` without arranging another wake-up.
        if !self.inner.inbox.borrow().is_empty() {
            task_context.waker().wake_by_ref();
        }

        completed
    }
    fn drain_inbox(&self) {
        let mut inbox = self.inner.inbox.borrow_mut();
        if inbox.is_empty() {
            return;
        }
        let active = self.inner.active.borrow();
        for future in inbox.drain(..) {
            active.push(future);
        }
    }
}

impl Default for TaskPool {
    /// Create an empty task pool.
    fn default() -> Self {
        Self {
            inner: Rc::new(TaskPoolInner {
                inbox: RefCell::new(VecDeque::new()),
                active: RefCell::new(FuturesUnordered::new()),
                driving: Cell::new(false),
            }),
        }
    }
}

impl<T> JobHandle<T> {
    /// Take the completed output without waiting.
    ///
    /// Returns `Ok(None)` while the job is unfinished, `Ok(Some(output))` once
    /// it completes, and [`JobCancelled`] if the pool dropped the job first.
    ///
    /// # Errors
    ///
    /// Returns [`JobCancelled`] when the pool no longer owns the job and no
    /// output was produced.
    pub fn try_take(&mut self) -> Result<Option<T>, JobCancelled> {
        self.receiver.try_recv().map_err(|_| JobCancelled)
    }
}

impl<T> Future for JobHandle<T> {
    type Output = Result<T, JobCancelled>;

    fn poll(mut self: Pin<&mut Self>, task_context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll(task_context)
            .map(|result| result.map_err(|_| JobCancelled))
    }
}

struct DriveGuard<'a>(&'a Cell<bool>);

impl Drop for DriveGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use core::future::{pending, poll_fn};
    use core::task::{Context, Poll};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures_util::task::{noop_waker, waker_ref, ArcWake};

    use super::*;

    fn drive(pool: &TaskPool) -> usize {
        let waker = noop_waker();
        let mut context = Context::from_waker(&waker);
        pool.drive(&mut context)
    }

    #[test]
    fn submit_returns_output_after_pool_is_driven() {
        let pool = TaskPool::default();
        let mut job = pool.submit(async { 42 });

        assert_eq!(job.try_take(), Ok(None));
        assert_eq!(drive(&pool), 1);
        assert_eq!(job.try_take(), Ok(Some(42)));
        assert_eq!(drive(&pool), 0);
    }

    #[test]
    fn cloned_pool_submits_into_the_same_queue() {
        let pool = TaskPool::default();
        let submitter = pool.clone();
        let mut job = submitter.submit(async { "done" });

        assert_eq!(drive(&pool), 1);
        assert_eq!(job.try_take(), Ok(Some("done")));
    }

    #[test]
    fn dropping_handle_does_not_cancel_job() {
        let pool = TaskPool::default();
        let completed = Rc::new(Cell::new(false));
        let completed_by_job = Rc::clone(&completed);
        drop(pool.submit(async move {
            completed_by_job.set(true);
        }));

        assert!(!completed.get());
        assert_eq!(drive(&pool), 1);
        assert!(completed.get());
    }

    #[test]
    fn pending_job_completes_on_a_later_drive() {
        let pool = TaskPool::default();
        let first_poll = Rc::new(Cell::new(true));
        let first_poll_by_job = Rc::clone(&first_poll);
        let mut job = pool.submit(poll_fn(move |context| {
            if first_poll_by_job.replace(false) {
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(7)
            }
        }));

        assert_eq!(drive(&pool), 0);
        assert_eq!(job.try_take(), Ok(None));
        assert_eq!(drive(&pool), 1);
        assert_eq!(job.try_take(), Ok(Some(7)));
    }

    #[test]
    fn job_can_submit_another_job_while_driven() {
        let pool = TaskPool::default();
        let child_completed = Rc::new(Cell::new(false));
        let pool_by_job = pool.clone();
        let child_completed_by_job = Rc::clone(&child_completed);
        let mut parent = pool.submit(async move {
            drop(pool_by_job.submit(async move {
                child_completed_by_job.set(true);
            }));
            1
        });

        assert_eq!(drive(&pool), 1);
        assert_eq!(parent.try_take(), Ok(Some(1)));
        assert!(!child_completed.get());

        assert_eq!(drive(&pool), 1);
        assert!(child_completed.get());
        assert_eq!(drive(&pool), 0);
    }

    #[test]
    fn job_submission_during_drive_wakes_owner_for_next_drive() {
        let pool = TaskPool::default();
        let pool_by_job = pool.clone();
        drop(pool.submit(async move {
            drop(pool_by_job.submit(async {}));
        }));
        let wakes = Arc::new(WakeCounter::default());
        let waker = waker_ref(&wakes);
        let mut context = Context::from_waker(&waker);

        assert_eq!(pool.drive(&mut context), 1);
        assert!(wakes.count.load(Ordering::Relaxed) > 0);
        assert_eq!(pool.drive(&mut context), 1);
    }

    #[test]
    fn dropping_pool_cancels_unfinished_job() {
        let mut job = {
            let pool = TaskPool::default();
            pool.submit(pending::<()>())
        };

        assert_eq!(job.try_take(), Err(JobCancelled));
    }

    #[derive(Default)]
    struct WakeCounter {
        count: AtomicUsize,
    }

    impl ArcWake for WakeCounter {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.count.fetch_add(1, Ordering::Relaxed);
        }
    }
}
