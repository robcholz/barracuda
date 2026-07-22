use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use async_channel::{Receiver, Sender};
use futures_core::Stream;

use super::{IterationLoopError, IterationLoopEvent};

type IterationItem = Result<IterationLoopEvent, IterationLoopError>;

pub(super) struct IterationEnvelope {
    item: IterationItem,
    resume: Sender<()>,
}

/// Producer side of one iteration stream.
///
/// Every send waits for the next consumer poll. Besides providing backpressure,
/// this makes `BeforeToolCalls` a real execution boundary.
#[derive(Clone)]
pub(crate) struct IterationEmitter {
    sender: Sender<IterationEnvelope>,
}

impl IterationEmitter {
    pub(crate) async fn send(&self, event: IterationLoopEvent) {
        self.send_item(Ok(event)).await;
    }

    pub(super) async fn send_error(&self, error: IterationLoopError) {
        self.send_item(Err(error)).await;
    }

    async fn send_item(&self, item: IterationItem) {
        let (resume, resumed) = async_channel::bounded(1);
        if self
            .sender
            .send(IterationEnvelope { item, resume })
            .await
            .is_ok()
        {
            let _ = resumed.recv().await;
        }
    }
}

type IterationDriver<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// The only output surface of one [`super::IterationLoop`].
///
/// A successful iteration ends with `None`. Failures are yielded once as an
/// `Err` item, after which the stream ends.
pub(crate) struct IterationStream<'a> {
    driver: Option<IterationDriver<'a>>,
    events: Pin<Box<Receiver<IterationEnvelope>>>,
    resume: Option<Sender<()>>,
}

impl<'a> IterationStream<'a> {
    pub(super) fn new(driver: IterationDriver<'a>, events: Receiver<IterationEnvelope>) -> Self {
        Self {
            driver: Some(driver),
            events: Box::pin(events),
            resume: None,
        }
    }

    pub(super) fn channel() -> (IterationEmitter, Receiver<IterationEnvelope>) {
        let (sender, receiver) = async_channel::bounded(1);
        (IterationEmitter { sender }, receiver)
    }

    fn take_event(&mut self, context: &mut Context<'_>) -> Poll<Option<IterationItem>> {
        match self.events.as_mut().poll_next(context) {
            Poll::Ready(Some(envelope)) => {
                self.resume = Some(envelope.resume);
                Poll::Ready(Some(envelope.item))
            }
            Poll::Ready(None) if self.driver.is_none() => Poll::Ready(None),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

impl Stream for IterationStream<'_> {
    type Item = IterationItem;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Some(resume) = this.resume.take() {
            let _ = resume.try_send(());
        }
        if let Poll::Ready(event) = this.take_event(context) {
            return Poll::Ready(event);
        }
        if let Some(driver) = this.driver.as_mut() {
            if driver.as_mut().poll(context).is_ready() {
                this.driver = None;
            }
        }
        this.take_event(context)
    }
}

impl Drop for IterationStream<'_> {
    fn drop(&mut self) {
        if let Some(resume) = self.resume.take() {
            let _ = resume.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use futures_lite::future::block_on;
    use futures_lite::StreamExt as _;

    use super::*;

    #[test]
    fn event_is_a_real_poll_boundary() {
        let (events, receiver) = IterationStream::channel();
        let phase = Rc::new(Cell::new(0));
        let producer_phase = Rc::clone(&phase);
        let driver = Box::pin(async move {
            events
                .send(IterationLoopEvent::BeforeToolCalls(Vec::new()))
                .await;
            producer_phase.set(1);
            events.send(IterationLoopEvent::Cancelled).await;
            producer_phase.set(2);
        });
        let mut stream = IterationStream::new(driver, receiver);

        block_on(async {
            assert_eq!(
                stream.next().await,
                Some(Ok(IterationLoopEvent::BeforeToolCalls(Vec::new())))
            );
            assert_eq!(phase.get(), 0, "producer remains parked at the boundary");

            assert_eq!(stream.next().await, Some(Ok(IterationLoopEvent::Cancelled)));
            assert_eq!(phase.get(), 1, "the next poll resumes the producer once");

            assert_eq!(stream.next().await, None);
            assert_eq!(phase.get(), 2);
        });
    }
}
