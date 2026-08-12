//! Minimal safe, single-task async generator adapters.
//!
//! A producer and consumer rendezvous through one slot. After producing an
//! item, the producer remains suspended until [`Stream::poll_next`] takes it.

use alloc::{boxed::Box, rc::Rc};
use core::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use futures_core::Stream;

/// Handle passed to a producer so it can yield one item at a time.
#[derive(Debug)]
pub struct Yielder<T> {
    slot: Rc<RefCell<Option<T>>>,
}

impl<T> Yielder<T> {
    /// Yield one item and remain suspended until the consumer takes it.
    pub fn yield_one(&self, value: T) -> YieldOne<T> {
        YieldOne {
            slot: Rc::clone(&self.slot),
            value: Some(value),
        }
    }
}

/// Build a boxed stream from an asynchronous producer.
pub fn yield_stream<'a, T, Build, Producer>(build: Build) -> Pin<Box<dyn Stream<Item = T> + 'a>>
where
    T: 'a,
    Build: FnOnce(Yielder<T>) -> Producer,
    Producer: Future<Output = ()> + 'a,
{
    let slot = Rc::new(RefCell::new(None));
    let producer = Box::pin(build(Yielder {
        slot: Rc::clone(&slot),
    }));
    Box::pin(YieldStream {
        slot,
        producer,
        done: false,
    })
}

/// Build a boxed stream whose producer can return one terminal error.
pub fn try_yield_stream<'a, T, E, Build, Producer>(
    build: Build,
) -> Pin<Box<dyn Stream<Item = Result<T, E>> + 'a>>
where
    T: 'a,
    E: 'a,
    Build: FnOnce(Yielder<T>) -> Producer,
    Producer: Future<Output = Result<(), E>> + 'a,
{
    let slot = Rc::new(RefCell::new(None));
    let producer = Box::pin(build(Yielder {
        slot: Rc::clone(&slot),
    }));
    Box::pin(TryYieldStream {
        slot,
        producer,
        terminal_error: None,
        done: false,
    })
}

struct YieldStream<'a, T> {
    slot: Rc<RefCell<Option<T>>>,
    producer: Pin<Box<dyn Future<Output = ()> + 'a>>,
    done: bool,
}

impl<T> Stream for YieldStream<'_, T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<T>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if let Some(item) = this.slot.borrow_mut().take() {
            return Poll::Ready(Some(item));
        }
        let producer = this.producer.as_mut().poll(context);
        if let Some(item) = this.slot.borrow_mut().take() {
            return Poll::Ready(Some(item));
        }
        match producer {
            Poll::Ready(()) => {
                this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

struct TryYieldStream<'a, T, E> {
    slot: Rc<RefCell<Option<T>>>,
    producer: Pin<Box<dyn Future<Output = Result<(), E>> + 'a>>,
    terminal_error: Option<E>,
    done: bool,
}

impl<T, E> Unpin for TryYieldStream<'_, T, E> {}

impl<T, E> Stream for TryYieldStream<'_, T, E> {
    type Item = Result<T, E>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(error) = this.terminal_error.take() {
            this.done = true;
            return Poll::Ready(Some(Err(error)));
        }
        if this.done {
            return Poll::Ready(None);
        }
        if let Some(item) = this.slot.borrow_mut().take() {
            return Poll::Ready(Some(Ok(item)));
        }
        match this.producer.as_mut().poll(context) {
            Poll::Ready(Ok(())) => this.done = true,
            Poll::Ready(Err(error)) => this.terminal_error = Some(error),
            Poll::Pending => {}
        }
        if let Some(item) = this.slot.borrow_mut().take() {
            Poll::Ready(Some(Ok(item)))
        } else if let Some(error) = this.terminal_error.take() {
            this.done = true;
            Poll::Ready(Some(Err(error)))
        } else if this.done {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

/// Future returned by [`Yielder::yield_one`].
pub struct YieldOne<T> {
    slot: Rc<RefCell<Option<T>>>,
    value: Option<T>,
}

impl<T> Unpin for YieldOne<T> {}

impl<T> Future for YieldOne<T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        if self.value.is_some() {
            if self.slot.borrow().is_some() {
                return Poll::Pending;
            }
            if let Some(value) = self.value.take() {
                self.slot.borrow_mut().replace(value);
            }
            return Poll::Pending;
        }
        if self.slot.borrow().is_none() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::Cell;

    use futures_lite::{future::block_on, StreamExt as _};

    use super::*;

    #[test]
    fn yields_each_item_in_order_then_ends() {
        let stream = yield_stream(|yielder| async move {
            yielder.yield_one(1).await;
            yielder.yield_one(2).await;
            yielder.yield_one(3).await;
        });

        assert_eq!(block_on(stream.collect::<Vec<_>>()), vec![1, 2, 3]);
    }

    #[test]
    fn try_stream_yields_items_before_its_terminal_error() {
        let stream = try_yield_stream(|yielder| async move {
            yielder.yield_one(1).await;
            yielder.yield_one(2).await;
            Err("failed")
        });

        assert_eq!(
            block_on(stream.collect::<Vec<_>>()),
            vec![Ok(1), Ok(2), Err("failed")]
        );
    }

    #[test]
    fn dropping_consumer_releases_producer_state() {
        struct DropFlag(Rc<Cell<bool>>);

        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }

        let dropped = Rc::new(Cell::new(false));
        let owned = DropFlag(Rc::clone(&dropped));
        let stream = yield_stream(|yielder| async move {
            let _owned = owned;
            yielder.yield_one(1).await;
            core::future::pending::<()>().await;
        });

        drop(stream);
        assert!(dropped.get());
    }
}
