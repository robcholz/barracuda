use core::any::type_name;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;

use super::lane::{LaneFrameKind, LaneReader, LaneWriter};
use super::payload::RpcPayloadFrame;
use super::{RpcError, RpcMessage, RpcResult};

/// A typed, zero-copy view over one RPC frame.
///
/// This value retains the lane buffer. The frame is not consumed and the lane
/// is not reused until this value is dropped. Use [`view`](Self::view) to
/// borrow the validated message in place.
pub struct RpcFrame<T> {
    payload: RpcPayloadFrame,
    message: PhantomData<fn() -> T>,
}

impl<T> core::fmt::Debug for RpcFrame<T>
where
    T: RpcMessage + core::fmt::Debug,
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.view() {
            Ok(message) => formatter.debug_tuple("RpcFrame").field(message).finish(),
            Err(error) => formatter
                .debug_struct("RpcFrame")
                .field("error", &error)
                .finish(),
        }
    }
}

impl<T> RpcFrame<T>
where
    T: RpcMessage,
{
    pub(crate) fn from_payload(payload: RpcPayloadFrame) -> RpcResult<Self> {
        T::try_ref_from_bytes(payload.as_ref()).map_err(|_| RpcError::InvalidMessageFrame {
            message_type: type_name::<T>(),
        })?;
        Ok(Self {
            payload,
            message: PhantomData,
        })
    }

    /// Borrows the message directly from its backing lane frame.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::InvalidMessageFrame`] if the bytes no longer satisfy
    /// the message's size, alignment, or bit-validity requirements.
    pub fn view(&self) -> RpcResult<&T> {
        T::try_ref_from_bytes(self.payload.as_ref()).map_err(|_| RpcError::InvalidMessageFrame {
            message_type: type_name::<T>(),
        })
    }
}

pub(crate) async fn write_frame<T>(writer: &mut LaneWriter, message: &T) -> RpcResult<()>
where
    T: RpcMessage,
{
    write_tagged_frame(writer, message, LaneFrameKind::Message).await
}

pub(crate) async fn write_method_error_frame<T>(writer: &mut LaneWriter, error: &T) -> RpcResult<()>
where
    T: RpcMessage,
{
    write_tagged_frame(writer, error, LaneFrameKind::MethodError).await
}

async fn write_tagged_frame<T>(
    writer: &mut LaneWriter,
    message: &T,
    kind: LaneFrameKind,
) -> RpcResult<()>
where
    T: RpcMessage,
{
    let payload = message.as_bytes();
    let mut frame = core::future::poll_fn(|context| writer.poll_reserve_frame(context))
        .await?
        .limit(payload.len())?;
    frame.as_mut().copy_from_slice(payload);
    frame.commit(payload.len(), kind)
}

pub(crate) struct FramedReader<T> {
    reader: LaneReader,
    finished: bool,
    message: PhantomData<fn() -> T>,
}

impl<T> FramedReader<T> {
    pub(crate) fn new(reader: LaneReader) -> Self {
        Self {
            reader,
            finished: false,
            message: PhantomData,
        }
    }
}

impl<T> Unpin for FramedReader<T> {}

impl<T> Stream for FramedReader<T>
where
    T: RpcMessage,
{
    type Item = RpcResult<RpcFrame<T>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        match this.reader.poll_borrow_frame(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => {
                this.finished = true;
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(Ok(Some(frame))) if frame.kind() == LaneFrameKind::Message => {
                let payload = RpcPayloadFrame::from_frame(frame);
                Poll::Ready(Some(RpcFrame::from_payload(payload)))
            }
            Poll::Ready(Ok(Some(_))) => {
                this.finished = true;
                Poll::Ready(Some(Err(RpcError::InvalidFrameState)))
            }
            Poll::Ready(Ok(None)) => {
                this.finished = true;
                Poll::Ready(None)
            }
        }
    }
}
