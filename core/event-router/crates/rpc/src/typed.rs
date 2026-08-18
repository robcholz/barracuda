use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::{type_name, TypeId};
use core::future::Future;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;
use getset::{CopyGetters, Getters};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use super::frame::{write_frame, write_method_error_frame, FramedReader, RpcFrame};
use super::json::JsonCodec;
use super::lane::{LaneReader, LaneWriter, LANE_FRAME_ALIGNMENT};
use super::payload::{
    attach_input_driver, make_payload_call, RpcMulticastBranch, RpcPayloadReader, RpcPayloadWriter,
};
use super::registry::{ErasedRpcHandler, PreparedCalls, RpcFuture};
use super::{RpcAddress, RpcContext, RpcError, RpcResult};

mod private {
    use super::{
        RpcFrame, RpcHandlerFuture, RpcInputMode, RpcMessage, RpcOutputMode, RpcStream,
        TypedCallDriver,
    };

    pub trait Sealed {}

    pub trait InputMode<T, ClientInput, HandlerInput>
    where
        T: RpcMessage,
    {
        fn into_stream(input: ClientInput) -> RpcStream<T>;

        fn decode_stream(input: RpcStream<RpcFrame<T>>) -> RpcHandlerFuture<'static, HandlerInput>;
    }

    pub trait OutputMode<T, E, HandlerOutput, ClientCall>
    where
        T: RpcMessage,
        E: RpcMessage,
    {
        fn into_stream(output: HandlerOutput) -> RpcStream<Result<T, E>>;

        fn make_client_call(responses: TypedCallDriver<T, E>) -> ClientCall;
    }

    pub(super) fn into_input_stream<Mode, T>(
        input: <Mode as RpcInputMode<T>>::ClientInput,
    ) -> RpcStream<T>
    where
        Mode: RpcInputMode<T>,
        T: RpcMessage,
    {
        <Mode as InputMode<
            T,
            <Mode as RpcInputMode<T>>::ClientInput,
            <Mode as RpcInputMode<T>>::HandlerInput,
        >>::into_stream(input)
    }

    pub(super) fn decode_input_stream<Mode, T>(
        input: RpcStream<RpcFrame<T>>,
    ) -> RpcHandlerFuture<'static, <Mode as RpcInputMode<T>>::HandlerInput>
    where
        Mode: RpcInputMode<T>,
        T: RpcMessage,
    {
        <Mode as InputMode<
            T,
            <Mode as RpcInputMode<T>>::ClientInput,
            <Mode as RpcInputMode<T>>::HandlerInput,
        >>::decode_stream(input)
    }

    pub(super) fn into_output_stream<Mode, T, E>(
        output: <Mode as RpcOutputMode<T, E>>::HandlerOutput,
    ) -> RpcStream<Result<T, E>>
    where
        Mode: RpcOutputMode<T, E>,
        T: RpcMessage,
        E: RpcMessage,
    {
        <Mode as OutputMode<
            T,
            E,
            <Mode as RpcOutputMode<T, E>>::HandlerOutput,
            <Mode as RpcOutputMode<T, E>>::ClientCall,
        >>::into_stream(output)
    }

    pub(super) fn make_client_call<Mode, T, E>(
        responses: TypedCallDriver<T, E>,
    ) -> <Mode as RpcOutputMode<T, E>>::ClientCall
    where
        Mode: RpcOutputMode<T, E>,
        T: RpcMessage,
        E: RpcMessage,
    {
        <Mode as OutputMode<
            T,
            E,
            <Mode as RpcOutputMode<T, E>>::HandlerOutput,
            <Mode as RpcOutputMode<T, E>>::ClientCall,
        >>::make_client_call(responses)
    }
}

/// A fixed-layout value that may be viewed directly in a typed RPC frame.
///
/// Deriving [`TryFromBytes`], [`IntoBytes`], [`KnownLayout`], and [`Immutable`]
/// rejects pointers and uninitialized padding at compile time. Message types
/// should use an explicit representation and explicit byte-order fields for a
/// stable wire format.
pub trait RpcMessage: TryFromBytes + IntoBytes + KnownLayout + Immutable + 'static {}

impl<T> RpcMessage for T where T: TryFromBytes + IntoBytes + KnownLayout + Immutable + 'static {}

/// Marker selecting exactly one typed message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Unary;

/// Marker selecting zero or more typed messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Streaming;

impl private::Sealed for Unary {}

impl<T> private::InputMode<T, T, RpcFrame<T>> for Unary
where
    T: RpcMessage,
{
    fn into_stream(input: T) -> RpcStream<T> {
        RpcStream::once(input)
    }

    fn decode_stream(mut input: RpcStream<RpcFrame<T>>) -> RpcHandlerFuture<'static, RpcFrame<T>> {
        Box::pin(async move { input.next().await.ok_or(RpcError::MissingUnaryFrame)? })
    }
}

impl<T, E> private::OutputMode<T, E, Result<T, E>, RpcUnaryCall<T, E>> for Unary
where
    T: RpcMessage,
    E: RpcMessage,
{
    fn into_stream(output: Result<T, E>) -> RpcStream<Result<T, E>> {
        RpcStream::once(output)
    }

    fn make_client_call(responses: TypedCallDriver<T, E>) -> RpcUnaryCall<T, E> {
        RpcUnaryCall::new(responses)
    }
}

impl private::Sealed for Streaming {}

impl<T> private::InputMode<T, RpcStream<T>, RpcStream<RpcFrame<T>>> for Streaming
where
    T: RpcMessage,
{
    fn into_stream(input: RpcStream<T>) -> RpcStream<T> {
        input
    }

    fn decode_stream(
        input: RpcStream<RpcFrame<T>>,
    ) -> RpcHandlerFuture<'static, RpcStream<RpcFrame<T>>> {
        Box::pin(async move { Ok(input) })
    }
}

impl<T, E>
    private::OutputMode<T, E, RpcStream<Result<T, E>>, RpcStream<Result<RpcFrame<T>, RpcFrame<E>>>>
    for Streaming
where
    T: RpcMessage,
    E: RpcMessage,
{
    fn into_stream(output: RpcStream<Result<T, E>>) -> RpcStream<Result<T, E>> {
        output
    }

    fn make_client_call(
        responses: TypedCallDriver<T, E>,
    ) -> RpcStream<Result<RpcFrame<T>, RpcFrame<E>>> {
        RpcStream::new(responses)
    }
}

/// A task-local RPC stream whose outer [`RpcResult`] reports transport failures.
pub struct RpcStream<T> {
    inner: RpcStreamInner<T>,
}

enum RpcStreamInner<T> {
    Once(Option<T>),
    Dynamic(Pin<Box<dyn Stream<Item = RpcResult<T>> + 'static>>),
}

impl<T> RpcStream<T> {
    /// Boxes a stream for use as RPC input or output.
    #[must_use]
    pub fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = RpcResult<T>> + 'static,
    {
        Self {
            inner: RpcStreamInner::Dynamic(Box::pin(stream)),
        }
    }

    fn once(value: T) -> Self {
        Self {
            inner: RpcStreamInner::Once(Some(value)),
        }
    }

    /// Waits for the next stream item.
    ///
    /// `None` means that the peer closed this direction normally.
    pub async fn next(&mut self) -> Option<RpcResult<T>> {
        core::future::poll_fn(|context| Pin::new(&mut *self).poll_next(context)).await
    }
}

impl<T> Unpin for RpcStream<T> {}

impl<T> Stream for RpcStream<T> {
    type Item = RpcResult<T>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match &mut self.get_mut().inner {
            RpcStreamInner::Once(value) => Poll::Ready(value.take().map(Ok)),
            RpcStreamInner::Dynamic(stream) => stream.as_mut().poll_next(context),
        }
    }
}

/// Compile-time description of one typed RPC method.
///
/// Implementations are normally generated by a service macro. They may also be
/// written manually for small services and tests.
pub trait RpcMethod: 'static {
    /// Stable runtime address in `group.method` form.
    const ADDRESS: &'static str;

    /// Message carried from caller to handler.
    type Request: RpcMessage;

    /// Message carried from handler to caller.
    type Response: RpcMessage;

    /// Method-specific error carried from handler to caller.
    type Error: RpcMessage;

    /// Request-side cardinality.
    type Input: RpcInputMode<Self::Request>;

    /// Response-side cardinality.
    type Output: RpcOutputMode<Self::Response, Self::Error>;

    /// Runtime JSON transcoder for [`RpcClient::call_json`](crate::RpcClient::call_json).
    ///
    /// Defaults to `None`, leaving the method reachable only through the typed
    /// [`call`](crate::RpcClient::call). Annotating the `impl` with `#[rpc_json]`
    /// overrides this to return [`JsonCodec::of::<Self>`], which
    /// [`RpcRegistry::register`](crate::RpcRegistry::register) captures beside
    /// the endpoint. Overriding it by hand requires the same `serde` bounds as
    /// [`JsonCodec::of`].
    fn json_codec() -> Option<JsonCodec> {
        None
    }
}

/// Runtime descriptor used to reject client/handler mismatches before IO.
#[derive(Clone, CopyGetters, Debug, Getters, PartialEq, Eq)]
pub(crate) struct RpcMethodDescriptor {
    #[getset(get = "pub(crate)")]
    address: RpcAddress,
    method_type_id: TypeId,
    #[getset(get_copy = "pub(crate)")]
    method_type_name: &'static str,
    #[getset(get_copy = "pub(crate)")]
    request_frame_size: usize,
    request_type_id: TypeId,
    #[getset(get_copy = "pub(crate)")]
    request_type_name: &'static str,
    input_mode_type_id: TypeId,
    #[getset(get_copy = "pub(crate)")]
    input_mode_type_name: &'static str,
}

impl RpcMethodDescriptor {
    pub(crate) fn validate_method<M>() -> RpcResult<()>
    where
        M: RpcMethod,
    {
        const {
            assert!(
                align_of::<M::Request>() <= LANE_FRAME_ALIGNMENT,
                "RPC request message alignment exceeds lane frame alignment"
            );
            assert!(
                align_of::<M::Response>() <= LANE_FRAME_ALIGNMENT,
                "RPC response message alignment exceeds lane frame alignment"
            );
            assert!(
                align_of::<M::Error>() <= LANE_FRAME_ALIGNMENT,
                "RPC method error alignment exceeds lane frame alignment"
            );
        }
        RpcAddress::validate(M::ADDRESS)?;
        Ok(())
    }

    pub(crate) fn is_method<M>(&self) -> bool
    where
        M: RpcMethod,
    {
        self.method_type_id == TypeId::of::<M>()
    }

    pub(crate) fn for_method<M>() -> RpcResult<Self>
    where
        M: RpcMethod,
    {
        Self::validate_method::<M>()?;
        let address = RpcAddress::try_from(M::ADDRESS)?;
        Ok(Self {
            address,
            method_type_id: TypeId::of::<M>(),
            method_type_name: type_name::<M>(),
            request_frame_size: size_of::<M::Request>(),
            request_type_id: TypeId::of::<M::Request>(),
            request_type_name: type_name::<M::Request>(),
            input_mode_type_id: TypeId::of::<M::Input>(),
            input_mode_type_name: type_name::<M::Input>(),
        })
    }

    pub(crate) fn has_same_input(&self, other: &Self) -> bool {
        self.request_type_id == other.request_type_id
            && self.input_mode_type_id == other.input_mode_type_id
    }

    pub(crate) fn accepts_input<T, Mode>(&self) -> bool
    where
        T: RpcMessage,
        Mode: RpcInputMode<T>,
    {
        self.request_type_id == TypeId::of::<T>() && self.input_mode_type_id == TypeId::of::<Mode>()
    }
}

pub(crate) fn make_typed_call<M>(
    prepared: PreparedCalls,
    input: <M::Input as RpcInputMode<M::Request>>::ClientInput,
) -> <M::Output as RpcOutputMode<M::Response, M::Error>>::ClientCall
where
    M: RpcMethod,
{
    let (writer, reader) = make_payload_call(prepared);
    let input = encode_stream(
        private::into_input_stream::<M::Input, M::Request>(input),
        writer,
    );
    private::make_client_call::<M::Output, M::Response, M::Error>(TypedCallDriver::new(
        input, reader,
    ))
}

pub(crate) fn make_typed_multicast<T, Mode>(
    input: <Mode as RpcInputMode<T>>::ClientInput,
    writer: RpcPayloadWriter,
    mut branches: Vec<RpcMulticastBranch>,
) -> Vec<RpcMulticastBranch>
where
    T: RpcMessage,
    Mode: RpcInputMode<T>,
{
    let input = encode_stream(private::into_input_stream::<Mode, T>(input), writer);
    attach_input_driver(&mut branches, input);
    branches
}

/// Boxed task-local future returned by a handler implementation.
pub type RpcHandlerFuture<'a, T> = Pin<Box<dyn Future<Output = RpcResult<T>> + 'a>>;

/// Input type received by a handler for method `M`.
pub type RpcHandlerInput<M> =
    <<M as RpcMethod>::Input as RpcInputMode<<M as RpcMethod>::Request>>::HandlerInput;

/// Typed business output returned by a handler for method `M`.
///
/// Unary output is `Result<Response, Error>`. Streaming output is a stream of
/// those outcomes; a method error terminates the encoded response stream.
pub type RpcHandlerOutput<M> = <<M as RpcMethod>::Output as RpcOutputMode<
    <M as RpcMethod>::Response,
    <M as RpcMethod>::Error,
>>::HandlerOutput;

/// Business-logic implementation for one [`RpcMethod`].
pub trait RpcHandler<M>
where
    M: RpcMethod,
{
    /// Handles one request frame or request-frame stream.
    fn call<'a>(
        &'a self,
        context: RpcContext,
        input: RpcHandlerInput<M>,
    ) -> RpcHandlerFuture<'a, RpcHandlerOutput<M>>;
}

impl<M, H, Fut> RpcHandler<M> for H
where
    M: RpcMethod,
    H: Fn(RpcContext, RpcHandlerInput<M>) -> Fut,
    Fut: Future<Output = RpcResult<RpcHandlerOutput<M>>> + 'static,
{
    fn call<'a>(
        &'a self,
        context: RpcContext,
        input: RpcHandlerInput<M>,
    ) -> RpcHandlerFuture<'a, RpcHandlerOutput<M>> {
        Box::pin((self)(context, input))
    }
}

/// Type-level request cardinality behavior.
///
/// This trait is sealed; use [`Unary`] or [`Streaming`].
pub trait RpcInputMode<T>:
    private::Sealed + private::InputMode<T, Self::ClientInput, Self::HandlerInput> + 'static
where
    T: RpcMessage,
{
    /// Value accepted by [`RpcClient::call`](crate::RpcClient::call).
    type ClientInput;

    /// Value delivered to [`RpcHandler`].
    type HandlerInput;
}

/// Type-level typed-result and response-cardinality behavior.
///
/// This trait is sealed; use [`Unary`] or [`Streaming`].
pub trait RpcOutputMode<T, E>:
    private::Sealed + private::OutputMode<T, E, Self::HandlerOutput, Self::ClientCall>
where
    T: RpcMessage,
    E: RpcMessage,
{
    /// Typed `Result<Response, Error>` value or stream returned by a handler.
    type HandlerOutput;

    /// Self-driving call returned by [`RpcClient::call`](crate::RpcClient::call).
    type ClientCall;
}

impl<T> RpcInputMode<T> for Unary
where
    T: RpcMessage,
{
    type ClientInput = T;
    type HandlerInput = RpcFrame<T>;
}

impl<T> RpcInputMode<T> for Streaming
where
    T: RpcMessage,
{
    type ClientInput = RpcStream<T>;
    type HandlerInput = RpcStream<RpcFrame<T>>;
}

impl<T, E> RpcOutputMode<T, E> for Unary
where
    T: RpcMessage,
    E: RpcMessage,
{
    type HandlerOutput = Result<T, E>;
    type ClientCall = RpcUnaryCall<T, E>;
}

impl<T, E> RpcOutputMode<T, E> for Streaming
where
    T: RpcMessage,
    E: RpcMessage,
{
    type HandlerOutput = RpcStream<Result<T, E>>;
    type ClientCall = RpcStream<Result<RpcFrame<T>, RpcFrame<E>>>;
}

fn encode_stream<T>(mut messages: RpcStream<T>, mut writer: RpcPayloadWriter) -> RpcFuture<'static>
where
    T: RpcMessage,
{
    Box::pin(async move {
        while let Some(message) = messages.next().await {
            writer.write_frame(message?.as_bytes()).await?;
        }
        writer.close().await
    })
}

pub struct TypedCallDriver<T, E> {
    input: Option<RpcFuture<'static>>,
    reader: RpcPayloadReader,
    response_eof: bool,
    finished: bool,
    message: PhantomData<fn() -> (T, E)>,
}

impl<T, E> TypedCallDriver<T, E> {
    fn new(input: RpcFuture<'static>, reader: RpcPayloadReader) -> Self {
        Self {
            input: Some(input),
            reader,
            response_eof: false,
            finished: false,
            message: PhantomData,
        }
    }
}

impl<T, E> Unpin for TypedCallDriver<T, E> {}

impl<T, E> Stream for TypedCallDriver<T, E>
where
    T: RpcMessage,
    E: RpcMessage,
{
    type Item = RpcResult<Result<RpcFrame<T>, RpcFrame<E>>>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        let mut request_reader_closed = false;
        if let Some(input) = this.input.as_mut() {
            match input.as_mut().poll(context) {
                Poll::Ready(Ok(())) => this.input = None,
                Poll::Ready(Err(RpcError::FrameReaderClosed)) => {
                    this.input = None;
                    request_reader_closed = true;
                }
                Poll::Ready(Err(error)) => {
                    this.input = None;
                    this.finished = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Pending => {}
            }
        }

        if !this.response_eof {
            match this.reader.poll_read(context) {
                Poll::Ready(Ok(Some(Ok(payload)))) => {
                    return Poll::Ready(Some(RpcFrame::from_payload(payload).map(Ok)));
                }
                Poll::Ready(Ok(Some(Err(payload)))) => {
                    this.input = None;
                    this.response_eof = true;
                    return Poll::Ready(Some(RpcFrame::from_payload(payload).map(Err)));
                }
                Poll::Ready(Ok(None)) => {
                    this.input = None;
                    this.response_eof = true;
                }
                Poll::Ready(Err(error)) => {
                    this.input = None;
                    this.finished = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Pending => {}
            }
        }

        if request_reader_closed && !this.response_eof {
            this.finished = true;
            return Poll::Ready(Some(Err(RpcError::FrameReaderClosed)));
        }

        if this.input.is_none() && this.response_eof {
            this.finished = true;
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

fn encode_outcome_stream<T, E>(
    mut outcomes: RpcStream<Result<T, E>>,
    mut writer: LaneWriter,
) -> RpcFuture<'static>
where
    T: RpcMessage,
    E: RpcMessage,
{
    Box::pin(async move {
        while let Some(outcome) = outcomes.next().await {
            match outcome? {
                Ok(message) => write_frame(&mut writer, &message).await?,
                Err(error) => {
                    write_method_error_frame(&mut writer, &error).await?;
                    break;
                }
            }
        }
        drop(writer);
        Ok(())
    })
}

pub(crate) struct HandlerAdapter<M, H> {
    handler: H,
    method: PhantomData<fn() -> M>,
}

impl<M, H> HandlerAdapter<M, H> {
    pub(crate) fn new(handler: H) -> Self {
        Self {
            handler,
            method: PhantomData,
        }
    }
}

impl<M, H> ErasedRpcHandler for HandlerAdapter<M, H>
where
    M: RpcMethod,
    H: RpcHandler<M>,
{
    fn call<'a>(
        &'a self,
        context: RpcContext,
        input: LaneReader,
        output: LaneWriter,
    ) -> RpcFuture<'a> {
        Box::pin(async move {
            let input = RpcStream::new(FramedReader::new(input));
            let input = private::decode_input_stream::<M::Input, M::Request>(input).await?;
            let output_value = self.handler.call(context, input).await?;
            let output_value =
                private::into_output_stream::<M::Output, M::Response, M::Error>(output_value);
            encode_outcome_stream(output_value, output).await
        })
    }
}

/// Self-driving unary typed RPC returned by [`RpcClient::call`](crate::RpcClient::call).
///
/// Polling this future concurrently advances request encoding, handler work,
/// and response decoding. Its outer [`RpcResult`] reports transport/runtime
/// failures; its inner `Result` contains either a zero-copy response frame or
/// the method's zero-copy typed error frame. No separate call task is required.
#[must_use = "futures do nothing unless polled or awaited"]
pub struct RpcUnaryCall<T, E> {
    responses: TypedCallDriver<T, E>,
    finished: bool,
}

impl<T, E> RpcUnaryCall<T, E> {
    fn new(responses: TypedCallDriver<T, E>) -> Self {
        Self {
            responses,
            finished: false,
        }
    }
}

impl<T, E> Unpin for RpcUnaryCall<T, E> {}

impl<T, E> Future for RpcUnaryCall<T, E>
where
    T: RpcMessage,
    E: RpcMessage,
{
    type Output = RpcResult<Result<RpcFrame<T>, RpcFrame<E>>>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(Err(RpcError::CompletedCallPolled));
        }

        match Pin::new(&mut this.responses).poll_next(context) {
            Poll::Ready(Some(result)) => {
                this.finished = true;
                Poll::Ready(result)
            }
            Poll::Ready(None) => {
                this.finished = true;
                Poll::Ready(Err(RpcError::MissingUnaryFrame))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
