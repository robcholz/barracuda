//! Lane-backed JSON RPC adapter.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt::Write as _;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

use getset::{CopyGetters, Getters};
use serde::de::{Deserialize, IgnoredAny};
use serde_json::{from_str, Value};

use super::address::RpcAddress;
use super::lane::{LaneFrameKind, LaneReader, LaneWriter};
use super::payload::{RpcPayloadFrame, RpcPayloadReader, RpcPayloadWriter};
use super::registry::{ErasedRpcHandler, RpcFuture};
use super::{RpcContext, RpcError, RpcResult};

/// Static JSON Schema text included in the contract-owning Plugin crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsonSchema(&'static str);

impl JsonSchema {
    /// Wraps JSON Schema source included at compile time.
    #[must_use]
    pub const fn new(source: &'static str) -> Self {
        Self(source)
    }

    /// Returns the included JSON Schema source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Compile-time contract for one lane-native JSON RPC method.
pub trait JsonRpcSchema: 'static {
    /// Registry address used by callers.
    const ADDRESS: &'static str;
    /// Request JSON Schema.
    const REQUEST_SCHEMA: JsonSchema;
    /// Response JSON Schema.
    const RESPONSE_SCHEMA: JsonSchema;
    /// Maximum encoded request size accepted by the method.
    const MAX_REQUEST_BYTES: usize;
    /// Maximum encoded response size emitted by the method.
    const MAX_RESPONSE_BYTES: usize;
}

/// Read-only metadata retained for one registered JSON RPC method.
#[derive(Clone, Debug, Getters, CopyGetters)]
pub struct JsonRpcInfo {
    /// Registered method address.
    #[getset(get = "pub")]
    address: RpcAddress,
    /// Included request JSON Schema.
    #[getset(get_copy = "pub")]
    request_schema: JsonSchema,
    /// Included response JSON Schema.
    #[getset(get_copy = "pub")]
    response_schema: JsonSchema,
    /// Maximum request size in bytes.
    #[getset(get_copy = "pub")]
    max_request_bytes: usize,
    /// Maximum response size in bytes.
    #[getset(get_copy = "pub")]
    max_response_bytes: usize,
}

impl JsonRpcInfo {
    pub(crate) fn for_method<Method>() -> RpcResult<Self>
    where
        Method: JsonRpcSchema,
    {
        Ok(Self {
            address: RpcAddress::try_from(Method::ADDRESS)?,
            request_schema: Method::REQUEST_SCHEMA,
            response_schema: Method::RESPONSE_SCHEMA,
            max_request_bytes: Method::MAX_REQUEST_BYTES,
            max_response_bytes: Method::MAX_RESPONSE_BYTES,
        })
    }
}

/// Includes a conventionally named schema owned by the current Plugin.
///
/// A Plugin member crate resolves `$rpc` below
/// `plugins/<plugin>/schemas/rpc/<rpc>/`.
#[macro_export]
macro_rules! json_schema {
    ($rpc:literal, request) => {
        $crate::JsonSchema::new(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/rpc/",
            $rpc,
            "/request.json"
        )))
    };
    ($rpc:literal, response) => {
        $crate::JsonSchema::new(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/rpc/",
            $rpc,
            "/response.json"
        )))
    };
}

/// A JSON document that can be written into caller-provided fixed storage.
///
/// The built-in implementations for `str` and [`serde_json::Value`] write
/// directly into the RPC lane without allocating an intermediate serialized
/// buffer.
pub trait JsonPayload {
    /// Returns the exact number of encoded bytes.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::InvalidJson`] when the source is invalid JSON.
    fn encoded_len(&self) -> RpcResult<usize>;

    /// Encodes this document into `destination` and returns its byte length.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::FrameTooLarge`] when `destination` is too small.
    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize>;
}

impl JsonPayload for str {
    fn encoded_len(&self) -> RpcResult<usize> {
        validate_json(self)?;
        Ok(self.len())
    }

    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize> {
        let length = self.encoded_len()?;
        let capacity = destination.len();
        let output = destination
            .get_mut(..length)
            .ok_or(RpcError::FrameTooLarge {
                size: length,
                capacity,
            })?;
        output.copy_from_slice(self.as_bytes());
        Ok(length)
    }
}

impl JsonPayload for String {
    fn encoded_len(&self) -> RpcResult<usize> {
        self.as_str().encoded_len()
    }

    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize> {
        self.as_str().write_json(destination)
    }
}

impl JsonPayload for Value {
    fn encoded_len(&self) -> RpcResult<usize> {
        value_len(self)
    }

    fn write_json(&self, destination: &mut [u8]) -> RpcResult<usize> {
        let length = self.encoded_len()?;
        if length > destination.len() {
            return Err(RpcError::FrameTooLarge {
                size: length,
                capacity: destination.len(),
            });
        }
        let mut writer = SliceWriter::new(destination);
        write_value(self, &mut writer)?;
        Ok(writer.written())
    }
}

/// One validated JSON document retained in its RPC lane frame.
///
/// The lane cannot reuse this frame until the `JsonRef` is dropped. Parsing a
/// borrowed value from it does not require an intermediate `serde_json::Value`.
pub struct JsonRef {
    payload: RpcPayloadFrame,
}

impl JsonRef {
    pub(crate) fn from_payload(payload: RpcPayloadFrame) -> RpcResult<Self> {
        validate_json_bytes(payload.as_ref())?;
        Ok(Self { payload })
    }

    /// Borrows the raw JSON document directly from lane storage.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::InvalidJson`] if the lane bytes are not UTF-8. JSON
    /// syntax was already validated when the frame was created.
    pub fn as_str(&self) -> RpcResult<&str> {
        core::str::from_utf8(self.payload.as_ref()).map_err(|_| RpcError::InvalidJson)
    }

    /// Deserializes a typed view directly from the lane-backed JSON document.
    ///
    /// Borrowed fields in `T` may refer to the lane and therefore cannot outlive
    /// this `JsonRef`.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::InvalidJson`] when the document does not match `T`.
    pub fn deserialize<'de, T>(&'de self) -> RpcResult<T>
    where
        T: Deserialize<'de>,
    {
        from_str(self.as_str()?).map_err(|_| RpcError::InvalidJson)
    }
}

impl core::fmt::Debug for JsonRef {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("JsonRef")
            .field("len", &self.payload.as_ref().len())
            .finish_non_exhaustive()
    }
}

/// Response writer backed directly by the response side of an RPC lane.
///
/// [`write`](Self::write) consumes the writer, so a JSON handler publishes at
/// most one response document.
pub struct JsonWriter {
    output: LaneWriter,
    max_bytes: usize,
}

impl JsonWriter {
    fn new(output: LaneWriter, max_bytes: usize) -> Self {
        Self { output, max_bytes }
    }

    /// Validates and writes one raw JSON document directly into the lane.
    ///
    /// # Errors
    ///
    /// Returns [`RpcError::InvalidJson`] for invalid JSON or
    /// [`RpcError::FrameTooLarge`] when the document exceeds lane capacity.
    pub async fn write<J>(mut self, json: &J) -> RpcResult<()>
    where
        J: JsonPayload + ?Sized,
    {
        let length = json.encoded_len()?;
        if length > self.max_bytes {
            return Err(RpcError::FrameTooLarge {
                size: length,
                capacity: self.max_bytes,
            });
        }
        let mut frame =
            core::future::poll_fn(|context| self.output.poll_reserve_frame(context)).await?;
        let output = frame
            .as_mut()
            .get_mut(..self.max_bytes)
            .ok_or(RpcError::InvalidFrameState)?;
        let written = json.write_json(output)?;
        if written != length {
            return Err(RpcError::InvalidFrameState);
        }
        frame.commit(written, LaneFrameKind::Message)
    }
}

/// Future returned by [`RpcClient::call_json`](crate::RpcClient::call_json).
pub struct JsonCall<'a> {
    inner: Pin<Box<dyn Future<Output = RpcResult<JsonRef>> + 'a>>,
}

impl<'a> JsonCall<'a> {
    pub(crate) fn new<J>(
        request: &'a J,
        request_len: usize,
        mut writer: RpcPayloadWriter,
        mut reader: RpcPayloadReader,
    ) -> Self
    where
        J: JsonPayload + ?Sized,
    {
        Self {
            inner: Box::pin(async move {
                let mut frame = writer.reserve().await?;
                let written = request.write_json(frame.as_mut())?;
                if written != request_len {
                    return Err(RpcError::InvalidFrameState);
                }
                frame.commit(written)?;
                writer.close().await?;

                match reader.read().await? {
                    Some(Ok(payload)) => JsonRef::from_payload(payload),
                    Some(Err(_payload)) => Err(RpcError::InvalidFrameState),
                    None => Err(RpcError::MissingUnaryFrame),
                }
            }),
        }
    }
}

impl Future for JsonCall<'_> {
    type Output = RpcResult<JsonRef>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(context)
    }
}

/// Boxed task-local future returned by a JSON handler.
pub type JsonHandlerFuture<'a> = Pin<Box<dyn Future<Output = RpcResult<()>> + 'a>>;

/// Handler for one lane-native JSON RPC endpoint.
pub trait JsonHandler {
    /// Reads one lane-backed request and writes at most one response document.
    fn call<'a>(
        &'a self,
        context: RpcContext,
        request: JsonRef,
        response: JsonWriter,
    ) -> JsonHandlerFuture<'a>;
}

impl<F, Fut> JsonHandler for F
where
    F: Fn(RpcContext, JsonRef, JsonWriter) -> Fut,
    Fut: Future<Output = RpcResult<()>> + 'static,
{
    fn call<'a>(
        &'a self,
        context: RpcContext,
        request: JsonRef,
        response: JsonWriter,
    ) -> JsonHandlerFuture<'a> {
        Box::pin((self)(context, request, response))
    }
}

pub(crate) struct JsonHandlerAdapter<Method, H> {
    handler: H,
    method: PhantomData<fn() -> Method>,
}

impl<Method, H> JsonHandlerAdapter<Method, H> {
    pub(crate) const fn new(handler: H) -> Self {
        Self {
            handler,
            method: PhantomData,
        }
    }
}

impl<Method, H> ErasedRpcHandler for JsonHandlerAdapter<Method, H>
where
    Method: JsonRpcSchema,
    H: JsonHandler,
{
    fn call<'a>(
        &'a self,
        context: RpcContext,
        mut input: LaneReader,
        output: LaneWriter,
    ) -> RpcFuture<'a> {
        Box::pin(async move {
            let frame = core::future::poll_fn(|context| input.poll_borrow_frame(context))
                .await?
                .ok_or(RpcError::MissingUnaryFrame)?;
            if frame.kind() != LaneFrameKind::Message {
                return Err(RpcError::InvalidFrameState);
            }
            let request = JsonRef::from_payload(RpcPayloadFrame::from_frame(frame))?;
            self.handler
                .call(
                    context,
                    request,
                    JsonWriter::new(output, Method::MAX_RESPONSE_BYTES),
                )
                .await
        })
    }
}

pub(crate) fn validate_json(json: &str) -> RpcResult<()> {
    from_str::<IgnoredAny>(json)
        .map(|_ignored| ())
        .map_err(|_| RpcError::InvalidJson)
}

fn validate_json_bytes(bytes: &[u8]) -> RpcResult<()> {
    let json = core::str::from_utf8(bytes).map_err(|_| RpcError::InvalidJson)?;
    validate_json(json)
}

fn checked_add(left: usize, right: usize) -> RpcResult<usize> {
    left.checked_add(right).ok_or(RpcError::InvalidFrameState)
}

fn value_len(value: &Value) -> RpcResult<usize> {
    match value {
        Value::Null => Ok(4),
        Value::Bool(true) => Ok(4),
        Value::Bool(false) => Ok(5),
        Value::Number(number) => {
            let mut writer = LengthWriter::default();
            write!(&mut writer, "{number}").map_err(|_| RpcError::InvalidFrameState)?;
            Ok(writer.written)
        }
        Value::String(string) => escaped_string_len(string),
        Value::Array(values) => {
            let mut length = 2_usize;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    length = checked_add(length, 1)?;
                }
                length = checked_add(length, value_len(value)?)?;
            }
            Ok(length)
        }
        Value::Object(values) => {
            let mut length = 2_usize;
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    length = checked_add(length, 1)?;
                }
                length = checked_add(length, escaped_string_len(key)?)?;
                length = checked_add(length, 1)?;
                length = checked_add(length, value_len(value)?)?;
            }
            Ok(length)
        }
    }
}

fn escaped_string_len(value: &str) -> RpcResult<usize> {
    let mut length = 2_usize;
    for byte in value.as_bytes() {
        let encoded = match byte {
            b'"' | b'\\' => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
        length = checked_add(length, encoded)?;
    }
    Ok(length)
}

fn write_value(value: &Value, writer: &mut SliceWriter<'_>) -> RpcResult<()> {
    match value {
        Value::Null => writer.write(b"null"),
        Value::Bool(true) => writer.write(b"true"),
        Value::Bool(false) => writer.write(b"false"),
        Value::Number(number) => {
            write!(writer, "{number}").map_err(|_| RpcError::InvalidFrameState)
        }
        Value::String(string) => write_escaped_string(string, writer),
        Value::Array(values) => {
            writer.write(b"[")?;
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    writer.write(b",")?;
                }
                write_value(value, writer)?;
            }
            writer.write(b"]")
        }
        Value::Object(values) => {
            writer.write(b"{")?;
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    writer.write(b",")?;
                }
                write_escaped_string(key, writer)?;
                writer.write(b":")?;
                write_value(value, writer)?;
            }
            writer.write(b"}")
        }
    }
}

fn write_escaped_string(value: &str, writer: &mut SliceWriter<'_>) -> RpcResult<()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    writer.write(b"\"")?;
    for byte in value.as_bytes() {
        match *byte {
            b'"' => writer.write(b"\\\"")?,
            b'\\' => writer.write(b"\\\\")?,
            control @ 0x00..=0x1f => {
                let high = usize::from(control >> 4);
                let low = usize::from(control & 0x0f);
                let high = *HEX.get(high).ok_or(RpcError::InvalidFrameState)?;
                let low = *HEX.get(low).ok_or(RpcError::InvalidFrameState)?;
                writer.write(&[b'\\', b'u', b'0', b'0', high, low])?;
            }
            other => writer.write(&[other])?,
        }
    }
    writer.write(b"\"")
}

struct SliceWriter<'a> {
    destination: &'a mut [u8],
    written: usize,
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.write(value.as_bytes()).map_err(|_| core::fmt::Error)
    }
}

#[derive(Default)]
struct LengthWriter {
    written: usize,
}

impl core::fmt::Write for LengthWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.written = self
            .written
            .checked_add(value.len())
            .ok_or(core::fmt::Error)?;
        Ok(())
    }
}

impl<'a> SliceWriter<'a> {
    fn new(destination: &'a mut [u8]) -> Self {
        Self {
            destination,
            written: 0,
        }
    }

    fn written(&self) -> usize {
        self.written
    }

    fn write(&mut self, bytes: &[u8]) -> RpcResult<()> {
        let end = checked_add(self.written, bytes.len())?;
        let capacity = self.destination.len();
        let output =
            self.destination
                .get_mut(self.written..end)
                .ok_or(RpcError::FrameTooLarge {
                    size: end,
                    capacity,
                })?;
        output.copy_from_slice(bytes);
        self.written = end;
        Ok(())
    }
}
