//! A pool of background tasks owned by one task, addressed by pool-assigned ids.
//!
//! [`BackgroundPool`] stores each task's progress stream and completion future
//! beside caller metadata `M` it never inspects. One owner drives
//! [`poll_next`](BackgroundPool::poll_next) for updates; any holder of a clone
//! can list, wait for, or remove a task by id. Removing a task drops its
//! futures, which cancels work they own. Like [`crate::unordered`], each wake
//! polls every member, which suits the handful of tasks one owner holds.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use futures_core::Stream;
use thiserror::Error;

type Completion<T> = Pin<Box<dyn Future<Output = T>>>;
type Progress<T> = Pin<Box<dyn Stream<Item = T>>>;

/// The work of one background task: a terminal completion and optional
/// non-terminal progress.
pub struct BackgroundTask<T> {
    progress: Option<Progress<T>>,
    completion: Completion<T>,
}

impl<T> BackgroundTask<T> {
    /// A task that reports only its completion.
    pub fn new(completion: impl Future<Output = T> + 'static) -> Self {
        Self {
            progress: None,
            completion: Box::pin(completion),
        }
    }

    /// Also reports every item of `progress` before the completion.
    #[must_use]
    pub fn with_progress(mut self, progress: impl Stream<Item = T> + 'static) -> Self {
        self.progress = Some(Box::pin(progress));
        self
    }
}

/// One update from a pooled task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundUpdate<T> {
    /// Non-terminal output; the task stays in the pool.
    Progress(T),
    /// Terminal output; the task has left the pool.
    Completed(T),
}

/// One update with the task that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundEvent<M, T> {
    /// Pool-assigned id returned by [`BackgroundPool::insert`].
    pub id: u32,
    pub meta: M,
    pub update: BackgroundUpdate<T>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum BackgroundError {
    /// The id never existed or its task already left the pool.
    #[error("no background task {0}")]
    NotFound(u32),
    /// Another [`BackgroundPool::wait`] already owns the task's completion.
    #[error("background task {0} is already being waited for")]
    AlreadyWaited(u32),
}

/// Background tasks owned by one executor-local owner.
///
/// Cloning shares the pool.
pub struct BackgroundPool<M, T> {
    state: Rc<RefCell<PoolState<M, T>>>,
}

impl<M, T> Clone for BackgroundPool<M, T> {
    fn clone(&self) -> Self {
        Self {
            state: Rc::clone(&self.state),
        }
    }
}

impl<M, T> Default for BackgroundPool<M, T> {
    fn default() -> Self {
        Self {
            state: Rc::new(RefCell::new(PoolState {
                tasks: Vec::new(),
                last_id: 0,
                next_poll: 0,
                waker: None,
            })),
        }
    }
}

struct PoolState<M, T> {
    tasks: Vec<Entry<M, T>>,
    last_id: u32,
    next_poll: usize,
    waker: Option<Waker>,
}

struct Entry<M, T> {
    id: u32,
    meta: M,
    progress: Option<Progress<T>>,
    completion: Option<Completion<T>>,
    terminal: Option<T>,
    /// A [`BackgroundWait`] owns the terminal output instead of `poll_next`.
    waited: bool,
}

impl<M, T> Entry<M, T> {
    fn poll_completion(&mut self, context: &mut Context<'_>) {
        let Some(completion) = self.completion.as_mut() else {
            return;
        };
        if let Poll::Ready(output) = completion.as_mut().poll(context) {
            self.completion = None;
            self.terminal = Some(output);
        }
    }

    fn poll_progress(&mut self, context: &mut Context<'_>) -> Poll<T> {
        let Some(progress) = self.progress.as_mut() else {
            return Poll::Pending;
        };
        match progress.as_mut().poll_next(context) {
            Poll::Ready(Some(output)) => Poll::Ready(output),
            Poll::Ready(None) => {
                self.progress = None;
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// Progress queued before completion is reported before the terminal
    /// output. A waited task reports progress only.
    fn poll_update(&mut self, context: &mut Context<'_>) -> Poll<BackgroundUpdate<T>> {
        if !self.waited {
            self.poll_completion(context);
        }
        if let Poll::Ready(output) = self.poll_progress(context) {
            return Poll::Ready(BackgroundUpdate::Progress(output));
        }
        if self.waited {
            return Poll::Pending;
        }
        match self.terminal.take() {
            Some(output) => Poll::Ready(BackgroundUpdate::Completed(output)),
            None => Poll::Pending,
        }
    }
}

impl<M, T> BackgroundPool<M, T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no task is pending.
    pub fn is_empty(&self) -> bool {
        self.state.borrow().tasks.is_empty()
    }

    /// Takes ownership of `task` and returns its id. Ids are never reused.
    pub fn insert(&self, meta: M, task: BackgroundTask<T>) -> u32 {
        let mut state = self.state.borrow_mut();
        state.last_id = state.last_id.wrapping_add(1);
        let id = state.last_id;
        state.tasks.push(Entry {
            id,
            meta,
            progress: task.progress,
            completion: Some(task.completion),
            terminal: None,
            waited: false,
        });
        let waker = state.waker.take();
        drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        id
    }

    /// Removes one task and drops its futures, returning its metadata.
    pub fn remove(&self, id: u32) -> Result<M, BackgroundError> {
        let entry = {
            let mut state = self.state.borrow_mut();
            let index = state
                .tasks
                .iter()
                .position(|entry| entry.id == id)
                .ok_or(BackgroundError::NotFound(id))?;
            state.tasks.remove(index)
        };
        // Futures drop outside the pool borrow.
        Ok(entry.meta)
    }

    /// Drops every task.
    pub fn clear(&self) {
        let tasks = core::mem::take(&mut self.state.borrow_mut().tasks);
        drop(tasks);
    }

    /// Hands one task's terminal output to the returned future instead of
    /// [`poll_next`](Self::poll_next). Dropping the future before it completes
    /// hands the output back.
    pub fn wait(&self, id: u32) -> Result<BackgroundWait<M, T>, BackgroundError> {
        let mut state = self.state.borrow_mut();
        let entry = state
            .tasks
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(BackgroundError::NotFound(id))?;
        if entry.waited {
            return Err(BackgroundError::AlreadyWaited(id));
        }
        entry.waited = true;
        Ok(BackgroundWait {
            pool: self.clone(),
            id,
        })
    }

    fn wake(&self) {
        let waker = self.state.borrow_mut().waker.take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<M: Clone, T> BackgroundPool<M, T> {
    /// Metadata of one pending task.
    pub fn get(&self, id: u32) -> Result<M, BackgroundError> {
        self.state
            .borrow()
            .tasks
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.meta.clone())
            .ok_or(BackgroundError::NotFound(id))
    }

    /// Ids and metadata of every pending task in insertion order.
    pub fn list(&self) -> Vec<(u32, M)> {
        self.state
            .borrow()
            .tasks
            .iter()
            .map(|entry| (entry.id, entry.meta.clone()))
            .collect()
    }

    /// Polls every task for its next update, starting after the task that last
    /// made progress so none is starved.
    ///
    /// An empty pool stays pending; [`insert`](Self::insert) wakes it.
    pub fn poll_next(&self, context: &mut Context<'_>) -> Poll<BackgroundEvent<M, T>> {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        if !state
            .waker
            .as_ref()
            .is_some_and(|waker| waker.will_wake(context.waker()))
        {
            state.waker = Some(context.waker().clone());
        }
        let count = state.tasks.len();
        let start = state.next_poll.min(count);
        for index in (start..count).chain(0..start) {
            let Some(entry) = state.tasks.get_mut(index) else {
                continue;
            };
            let Poll::Ready(update) = entry.poll_update(context) else {
                continue;
            };
            let id = entry.id;
            let meta = if matches!(update, BackgroundUpdate::Completed(_)) {
                state.next_poll = index;
                state.tasks.remove(index).meta
            } else {
                state.next_poll = index.wrapping_add(1);
                entry.meta.clone()
            };
            return Poll::Ready(BackgroundEvent { id, meta, update });
        }
        Poll::Pending
    }
}

/// Future returned by [`BackgroundPool::wait`]: the task's metadata and
/// terminal output, after which the task has left the pool.
pub struct BackgroundWait<M, T> {
    pool: BackgroundPool<M, T>,
    id: u32,
}

impl<M, T> Future for BackgroundWait<M, T> {
    type Output = Result<(M, T), BackgroundError>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.pool.state.borrow_mut();
        let Some(index) = state.tasks.iter().position(|entry| entry.id == self.id) else {
            return Poll::Ready(Err(BackgroundError::NotFound(self.id)));
        };
        let Some(entry) = state.tasks.get_mut(index) else {
            return Poll::Ready(Err(BackgroundError::NotFound(self.id)));
        };
        entry.poll_completion(context);
        let Some(output) = entry.terminal.take() else {
            return Poll::Pending;
        };
        let entry = state.tasks.remove(index);
        drop(state);
        Poll::Ready(Ok((entry.meta, output)))
    }
}

impl<M, T> Drop for BackgroundWait<M, T> {
    fn drop(&mut self) {
        let released = self
            .pool
            .state
            .borrow_mut()
            .tasks
            .iter_mut()
            .find(|entry| entry.id == self.id)
            .map(|entry| entry.waited = false)
            .is_some();
        if released {
            self.pool.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use core::future::{pending, ready};

    use futures_lite::future::{block_on, poll_fn};
    use futures_lite::stream;

    use super::*;

    fn next<M: Clone, T>(pool: &BackgroundPool<M, T>) -> BackgroundEvent<M, T> {
        block_on(poll_fn(|context| pool.poll_next(context)))
    }

    #[test]
    fn progress_precedes_completion_and_completion_removes_the_task() {
        let pool = BackgroundPool::new();
        let id = pool.insert(
            "meta",
            BackgroundTask::new(ready(3)).with_progress(stream::iter(vec![1, 2])),
        );
        assert_eq!(id, 1);
        let updates = [next(&pool), next(&pool), next(&pool)].map(|event| event.update);
        assert_eq!(
            updates,
            [
                BackgroundUpdate::Progress(1),
                BackgroundUpdate::Progress(2),
                BackgroundUpdate::Completed(3)
            ]
        );
        assert!(pool.is_empty());
    }

    #[test]
    fn tasks_are_listed_and_removed_by_id_without_reusing_ids() {
        let pool = BackgroundPool::<&str, u32>::new();
        let first = pool.insert("first", BackgroundTask::new(pending()));
        let second = pool.insert("second", BackgroundTask::new(pending()));
        assert_eq!(pool.list(), vec![(first, "first"), (second, "second")]);
        assert_eq!(pool.get(second), Ok("second"));

        assert_eq!(pool.remove(first), Ok("first"));
        assert_eq!(pool.remove(first), Err(BackgroundError::NotFound(first)));
        assert_eq!(pool.insert("third", BackgroundTask::new(pending())), 3);
        pool.clear();
        assert!(pool.is_empty());
    }

    #[test]
    fn waited_output_bypasses_poll_next() {
        let pool = BackgroundPool::new();
        let id = pool.insert("meta", BackgroundTask::new(ready(7)));
        let wait = pool.wait(id).ok();
        assert_eq!(
            pool.wait(id).err(),
            Some(BackgroundError::AlreadyWaited(id))
        );
        assert_eq!(wait.map(block_on), Some(Ok(("meta", 7))));
        assert!(pool.is_empty());
    }

    #[test]
    fn dropped_wait_returns_the_output_to_poll_next() {
        let pool = BackgroundPool::new();
        let id = pool.insert("meta", BackgroundTask::new(ready(7)));
        drop(pool.wait(id));
        assert_eq!(next(&pool).update, BackgroundUpdate::Completed(7));
    }
}
