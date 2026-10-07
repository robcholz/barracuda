//! Unbounded single-consumer channel between tasks of one executor.
//!
//! Queued messages occupy one growable ring buffer that is allocated on the
//! first send, so an idle channel costs one small shared allocation and a
//! channel holding a message or two costs a few slots. Nothing is atomic or
//! padded for other cores: both ends are `!Send`, so the compiler keeps the
//! channel on the executor that created it. Work that crosses executors or
//! cores needs a `critical-section` channel instead.

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use core::fmt;
use core::future::poll_fn;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use futures_core::Stream;

/// Creates a connected sender and receiver.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Rc::new(RefCell::new(Shared {
        queue: VecDeque::new(),
        waker: None,
        senders: 1,
        receiving: true,
    }));
    (Sender(Rc::clone(&shared)), Receiver(shared))
}

struct Shared<T> {
    queue: VecDeque<T>,
    waker: Option<Waker>,
    senders: usize,
    receiving: bool,
}

/// Sending half; clones feed the same receiver.
pub struct Sender<T>(Rc<RefCell<Shared<T>>>);

/// Receiving half; the channel ends once every sender is dropped and the
/// queue is drained.
pub struct Receiver<T>(Rc<RefCell<Shared<T>>>);

/// The receiver was dropped; the unsent message is returned.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Closed<T>(pub T);

impl<T> fmt::Debug for Closed<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Closed(..)")
    }
}

impl<T> fmt::Display for Closed<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("channel receiver dropped")
    }
}

/// Why [`Receiver::try_recv`] returned no message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TryRecvError {
    /// No message is queued, but senders remain.
    Empty,
    /// No message is queued and every sender was dropped.
    Closed,
}

impl<T> Sender<T> {
    /// Queues `message` and wakes the receiver.
    ///
    /// # Errors
    ///
    /// [`Closed`] with the message when the receiver was dropped.
    pub fn send(&self, message: T) -> Result<(), Closed<T>> {
        let waker = {
            let mut shared = self.0.borrow_mut();
            if !shared.receiving {
                return Err(Closed(message));
            }
            shared.queue.push_back(message);
            shared.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut shared = self.0.borrow_mut();
        shared.senders = shared.senders.saturating_add(1);
        drop(shared);
        Self(Rc::clone(&self.0))
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut shared = self.0.borrow_mut();
            shared.senders = shared.senders.saturating_sub(1);
            if shared.senders == 0 {
                shared.waker.take()
            } else {
                None
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Receiver<T> {
    /// Takes the next queued message without waiting.
    ///
    /// # Errors
    ///
    /// [`TryRecvError`] when nothing is queued.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut shared = self.0.borrow_mut();
        match shared.queue.pop_front() {
            Some(message) => Ok(message),
            None if shared.senders == 0 => Err(TryRecvError::Closed),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Polls for the next message; `None` once every sender is dropped and the
    /// queue is empty.
    pub fn poll_recv(&self, context: &mut Context<'_>) -> Poll<Option<T>> {
        let mut shared = self.0.borrow_mut();
        if let Some(message) = shared.queue.pop_front() {
            return Poll::Ready(Some(message));
        }
        if shared.senders == 0 {
            return Poll::Ready(None);
        }
        match &mut shared.waker {
            Some(waker) if waker.will_wake(context.waker()) => {}
            slot => *slot = Some(context.waker().clone()),
        }
        Poll::Pending
    }

    /// Waits for the next message; `None` once the channel has ended.
    pub async fn recv(&self) -> Option<T> {
        poll_fn(|context| self.poll_recv(context)).await
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
        self.poll_recv(context)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Dropping queued messages can run arbitrary destructors, so release
        // the borrow first.
        let queue = {
            let mut shared = self.0.borrow_mut();
            shared.receiving = false;
            shared.waker = None;
            core::mem::take(&mut shared.queue)
        };
        drop(queue);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    use futures_lite::future::block_on;

    #[test]
    fn messages_arrive_in_order_and_the_channel_ends_with_its_senders() {
        let (sender, receiver) = channel();
        let second = sender.clone();
        sender.send(1).unwrap();
        second.send(2).unwrap();
        assert_eq!(receiver.try_recv(), Ok(1));
        drop(sender);
        assert_eq!(block_on(receiver.recv()), Some(2));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        drop(second);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Closed));
        assert_eq!(block_on(receiver.recv()), None);
    }

    #[test]
    fn sending_after_the_receiver_drops_returns_the_message() {
        let (sender, receiver) = channel();
        sender.send(1).unwrap();
        drop(receiver);
        assert_eq!(sender.send(2), Err(Closed(2)));
    }

    #[test]
    fn an_idle_channel_allocates_no_queue() {
        let (sender, receiver) = channel::<u64>();
        assert_eq!(receiver.0.borrow().queue.capacity(), 0);
        sender.send(1).unwrap();
        assert!(receiver.0.borrow().queue.capacity() >= 1);
    }
}
