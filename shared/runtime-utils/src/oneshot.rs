//! Single-value channel that works on targets without native atomic CAS.
//!
//! The API mirrors the subset of `futures_channel::oneshot` used in this
//! workspace. `futures-channel` compiles its channels out on targets such as
//! `riscv32imc`, so this implementation keeps its shared state behind a
//! `critical-section` mutex and a portable [`Arc`].

use core::cell::RefCell;
use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use critical_section::Mutex;
use portable_atomic_util::Arc;

/// The sending half was dropped before a value was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canceled;

impl fmt::Display for Canceled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("oneshot canceled")
    }
}

impl core::error::Error for Canceled {}

struct State<T> {
    value: Option<T>,
    sender_alive: bool,
    receiver_alive: bool,
    receiver_waker: Option<Waker>,
}

struct Shared<T> {
    state: Mutex<RefCell<State<T>>>,
}

impl<T> Shared<T> {
    fn with<R>(&self, operation: impl FnOnce(&mut State<T>) -> R) -> R {
        critical_section::with(|token| operation(&mut self.state.borrow_ref_mut(token)))
    }
}

/// Creates a connected sender and receiver for exactly one value.
#[must_use]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        state: Mutex::new(RefCell::new(State {
            value: None,
            sender_alive: true,
            receiver_alive: true,
            receiver_waker: None,
        })),
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

/// Sending half of a [`channel`].
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Sender<T> {
    /// Delivers `value`, or returns it when the receiver is gone.
    ///
    /// # Errors
    ///
    /// Returns the value back when the receiver was dropped or closed.
    pub fn send(self, value: T) -> Result<(), T> {
        let waker = self.shared.with(|state| {
            if !state.receiver_alive {
                return Err(value);
            }
            state.value = Some(value);
            Ok(state.receiver_waker.take())
        })?;
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    /// Returns whether the receiver was dropped or closed.
    #[must_use]
    pub fn is_canceled(&self) -> bool {
        self.shared.with(|state| !state.receiver_alive)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = self.shared.with(|state| {
            state.sender_alive = false;
            state.receiver_waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> fmt::Debug for Sender<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Sender").finish_non_exhaustive()
    }
}

/// Receiving half of a [`channel`]; awaiting it yields the sent value.
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Receiver<T> {
    /// Prevents any further value from being sent.
    pub fn close(&mut self) {
        self.shared.with(|state| state.receiver_alive = false);
    }

    /// Takes the value if it already arrived.
    ///
    /// Returns `Ok(None)` while the sender is still alive without a value.
    ///
    /// # Errors
    ///
    /// Returns [`Canceled`] when the sender was dropped without sending.
    pub fn try_recv(&mut self) -> Result<Option<T>, Canceled> {
        self.shared.with(|state| match state.value.take() {
            Some(value) => Ok(Some(value)),
            None if state.sender_alive => Ok(None),
            None => Err(Canceled),
        })
    }
}

impl<T> Future for Receiver<T> {
    type Output = Result<T, Canceled>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.shared.with(|state| {
            if let Some(value) = state.value.take() {
                return Poll::Ready(Ok(value));
            }
            if !state.sender_alive {
                return Poll::Ready(Err(Canceled));
            }
            match &mut state.receiver_waker {
                Some(waker) if waker.will_wake(context.waker()) => {}
                waker => *waker = Some(context.waker().clone()),
            }
            Poll::Pending
        })
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        // Drop any undelivered value outside the critical section.
        let value = self.shared.with(|state| {
            state.receiver_alive = false;
            state.receiver_waker = None;
            state.value.take()
        });
        drop(value);
    }
}

impl<T> fmt::Debug for Receiver<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Receiver").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{channel, Canceled};
    use futures_lite::future::block_on;

    #[test]
    fn delivers_the_sent_value() {
        let (sender, receiver) = channel();
        assert_eq!(sender.send(7), Ok(()));
        assert_eq!(block_on(receiver), Ok(7));
    }

    #[test]
    fn dropped_sender_cancels_the_receiver() {
        let (sender, mut receiver) = channel::<u8>();
        assert_eq!(receiver.try_recv(), Ok(None));
        drop(sender);
        assert_eq!(receiver.try_recv(), Err(Canceled));
        assert_eq!(block_on(receiver), Err(Canceled));
    }

    #[test]
    fn dropped_or_closed_receiver_returns_the_value() {
        let (sender, receiver) = channel();
        assert!(!sender.is_canceled());
        drop(receiver);
        assert!(sender.is_canceled());
        assert_eq!(sender.send(3), Err(3));

        let (sender, mut receiver) = channel();
        receiver.close();
        assert_eq!(sender.send(4), Err(4));
    }

    #[test]
    fn try_recv_takes_an_arrived_value() {
        let (sender, mut receiver) = channel();
        sender.send("ready").expect("receiver alive");
        assert_eq!(receiver.try_recv(), Ok(Some("ready")));
    }
}
