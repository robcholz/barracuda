//! Runtime JSON dispatch over fixed-layout typed RPC frames.
//!
//! [`RpcClient::call_json`](crate::RpcClient::call_json) addresses an endpoint
//! by a runtime string and moves the exact same fixed-layout bytes a typed
//! [`call`](crate::RpcClient::call) would: the request value is transcoded into
//! the method's `Request` struct and that struct's byte image goes on the lane,
//! while each `Response`/`Error` frame is viewed in place and transcoded back to
//! a value. Nothing JSON ever travels over a lane.
//!
//! A runtime address cannot recover a Rust type, so the transcoder is captured
//! while the concrete method type is still known — at
//! [`RpcMethod::dynamic`], which `#[rpc_dynamic]` fills — and stored beside the
//! endpoint. Transcoding reuses two hardened derives instead of reflecting field
//! offsets: `serde` maps the value to and from the concrete struct, and
//! `zerocopy` maps the struct to and from its wire bytes.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use core::any::type_name;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_core::Stream;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

use super::payload::RpcPayloadReader;
use super::registry::RpcFuture;
use super::typed::RpcMethod;
use super::{RpcError, RpcResult};

/// Opaque JSON transcoder for one runtime-addressed [`RpcMethod`].
///
/// Built by [`JsonCodec::of`] where the concrete method type is known, then
/// stored beside the endpoint so [`RpcClient::call_json`](crate::RpcClient::call_json)
/// can transcode with only a runtime address. Methods normally obtain one
/// through `#[rpc_dynamic]`, which fills [`RpcMethod::dynamic`].
#[derive(Clone)]
pub struct JsonCodec(Rc<dyn ErasedJsonCodec>);

impl JsonCodec {
    /// Builds the transcoder for method `M`.
    ///
    /// `#[rpc_dynamic]` emits this call; write it by hand only to fill
    /// [`RpcMethod::dynamic`] without the attribute.
    #[must_use]
    pub fn of<M>() -> Self
    where
        M: RpcMethod,
        M::Request: DeserializeOwned,
        M::Response: Serialize,
        M::Error: Serialize,
    {
        Self(Rc::new(TypedJsonCodec::<M>::new()))
    }

    pub(crate) fn encode_request(&self, value: &Value) -> RpcResult<Vec<u8>> {
        self.0.encode_request(value)
    }

    pub(crate) fn decode_response(&self, bytes: &[u8]) -> RpcResult<Value> {
        self.0.decode_response(bytes)
    }

    pub(crate) fn decode_error(&self, bytes: &[u8]) -> RpcResult<Value> {
        self.0.decode_error(bytes)
    }
}

impl core::fmt::Debug for JsonCodec {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("JsonCodec").finish_non_exhaustive()
    }
}

/// Drives one JSON-coded RPC call, mirroring a typed call frame for frame.
///
/// The boxed `input` future writes every request frame (one per JSON input
/// value) and closes the request direction. Each response or method-error frame
/// is then transcoded to a JSON value, so streaming inputs and outputs work the
/// same way they do in a typed call.
pub(crate) struct JsonCallDriver {
    input: Option<RpcFuture<'static>>,
    reader: RpcPayloadReader,
    codec: JsonCodec,
    response_eof: bool,
    finished: bool,
}

impl JsonCallDriver {
    pub(crate) fn new(
        input: RpcFuture<'static>,
        reader: RpcPayloadReader,
        codec: JsonCodec,
    ) -> Self {
        Self {
            input: Some(input),
            reader,
            codec,
            response_eof: false,
            finished: false,
        }
    }
}

impl Unpin for JsonCallDriver {}

impl Stream for JsonCallDriver {
    type Item = RpcResult<Result<Value, Value>>;

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
                    return Poll::Ready(Some(
                        this.codec.decode_response(payload.as_ref()).map(Ok),
                    ));
                }
                Poll::Ready(Ok(Some(Err(payload)))) => {
                    this.input = None;
                    this.response_eof = true;
                    return Poll::Ready(Some(
                        this.codec.decode_error(payload.as_ref()).map(Err),
                    ));
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

/// Type-erased half of a [`JsonCodec`]; hides the method type behind a vtable.
trait ErasedJsonCodec {
    fn encode_request(&self, value: &Value) -> RpcResult<Vec<u8>>;
    fn decode_response(&self, bytes: &[u8]) -> RpcResult<Value>;
    fn decode_error(&self, bytes: &[u8]) -> RpcResult<Value>;
}

/// [`ErasedJsonCodec`] bound to one concrete [`RpcMethod`].
struct TypedJsonCodec<Method> {
    method: PhantomData<fn() -> Method>,
}

impl<Method> TypedJsonCodec<Method> {
    fn new() -> Self {
        Self {
            method: PhantomData,
        }
    }
}

impl<Method> ErasedJsonCodec for TypedJsonCodec<Method>
where
    Method: RpcMethod,
    Method::Request: DeserializeOwned,
    Method::Response: Serialize,
    Method::Error: Serialize,
{
    fn encode_request(&self, value: &Value) -> RpcResult<Vec<u8>> {
        let request: Method::Request =
            Deserialize::deserialize(value).map_err(|error| RpcError::JsonRequestInvalid {
                message_type: type_name::<Method::Request>(),
                message: error.to_string(),
            })?;
        Ok(request.as_bytes().to_vec())
    }

    fn decode_response(&self, bytes: &[u8]) -> RpcResult<Value> {
        decode_frame::<Method::Response>(bytes)
    }

    fn decode_error(&self, bytes: &[u8]) -> RpcResult<Value> {
        decode_frame::<Method::Error>(bytes)
    }
}

fn decode_frame<T>(bytes: &[u8]) -> RpcResult<Value>
where
    T: TryFromBytes + KnownLayout + Immutable + Serialize,
{
    let message = T::try_ref_from_bytes(bytes).map_err(|_error| RpcError::InvalidMessageFrame {
        message_type: type_name::<T>(),
    })?;
    serde_json::to_value(message).map_err(|_error| RpcError::JsonResponseInvalid {
        message_type: type_name::<T>(),
    })
}

/// Wraps one transcoded frame in the `{ "ok": bool, ... }` result envelope.
///
/// A response frame becomes `{ "ok": true, "value": <response> }`; a terminal
/// method-error frame becomes `{ "ok": false, "error": <error> }`.
pub(crate) fn envelope(ok: bool, value: Value) -> Value {
    let mut object = Map::new();
    object.insert(String::from("ok"), Value::Bool(ok));
    object.insert(String::from(if ok { "value" } else { "error" }), value);
    Value::Object(object)
}
