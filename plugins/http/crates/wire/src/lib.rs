//! Fixed-layout types for the dynamic HTTP RPC.
#![cfg_attr(not(feature = "schema"), no_std)]

extern crate alloc;

use core::fmt;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Maximum URL length, excluding its NUL terminator.
pub const URL_CAPACITY: usize = 192;
/// Maximum header name length, excluding its NUL terminator.
pub const HEADER_NAME_CAPACITY: usize = 32;
/// Maximum header value length, excluding its NUL terminator.
pub const HEADER_VALUE_CAPACITY: usize = 64;
/// Maximum request body length, excluding its NUL terminator.
pub const REQUEST_BODY_CAPACITY: usize = 96;
/// Maximum response body length, excluding its NUL terminator.
pub const RESPONSE_BODY_CAPACITY: usize = 508;
/// Maximum number of request headers.
pub const HEADER_CAPACITY: usize = 2;

/// A fixed-capacity UTF-8, NUL-terminated string.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub struct HttpText<const N: usize>([u8; N]);

impl<const N: usize> HttpText<N> {
    /// Creates text, rejecting embedded NULs and values without terminator space.
    pub fn new(value: &str) -> Result<Self, HttpTextError> {
        if value.as_bytes().contains(&0) {
            return Err(HttpTextError::EmbeddedNul);
        }
        if value.len() >= N {
            return Err(HttpTextError::TooLong);
        }
        let mut bytes = [0; N];
        bytes
            .get_mut(..value.len())
            .ok_or(HttpTextError::TooLong)?
            .copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }

    /// Returns the contained string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        let end = self.0.iter().position(|byte| *byte == 0).unwrap_or(N);
        core::str::from_utf8(self.0.get(..end).unwrap_or_default()).unwrap_or_default()
    }
}

impl<const N: usize> Default for HttpText<N> {
    fn default() -> Self {
        Self([0; N])
    }
}
impl<const N: usize> Serialize for HttpText<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de, const N: usize> Deserialize<'de> for HttpText<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <&str>::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}
#[cfg(feature = "schema")]
impl<const N: usize> schemars::JsonSchema for HttpText<N> {
    fn schema_name() -> alloc::string::String {
        alloc::format!("HttpText_{N}")
    }
    fn json_schema(generator: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = <alloc::string::String>::json_schema(generator);
        if let schemars::schema::Schema::Object(object) = &mut schema {
            object.string().max_length = Some((N.saturating_sub(1)) as u32);
        }
        schema
    }
}

/// Failure constructing fixed HTTP text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpTextError {
    /// Text exceeds its capacity.
    TooLong,
    /// Text contains a NUL byte.
    EmbeddedNul,
}
impl fmt::Display for HttpTextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLong => "text exceeds capacity",
            Self::EmbeddedNul => "text contains NUL",
        })
    }
}

/// HTTP method used by [`HttpRequest`].
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
    /// HEAD.
    Head,
}

/// One outbound HTTP header.
#[repr(C)]
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HttpHeader {
    /// Header name.
    pub name: HttpText<{ HEADER_NAME_CAPACITY + 1 }>,
    /// Header value.
    pub value: HttpText<{ HEADER_VALUE_CAPACITY + 1 }>,
}

/// Executes one buffered outbound HTTP request.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpRequest {
    /// Request method.
    pub method: HttpMethod,
    /// Absolute HTTP or HTTPS URL.
    pub url: HttpText<{ URL_CAPACITY + 1 }>,
    /// Request headers. Empty-name entries are ignored.
    pub headers: [HttpHeader; HEADER_CAPACITY],
    /// UTF-8 request body; use an empty string when absent.
    pub body: HttpText<{ REQUEST_BODY_CAPACITY + 1 }>,
}

#[cfg(feature = "schema")]
barracuda_rpc_schema::register!(HttpRequest);

/// Buffered response from an outbound HTTP request.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// UTF-8 response body.
    pub body: HttpText<{ RESPONSE_BODY_CAPACITY + 1 }>,
    #[serde(skip)]
    reserved: u8,
}

impl HttpResponse {
    /// Creates a buffered response.
    #[must_use]
    pub const fn new(status: u16, body: HttpText<{ RESPONSE_BODY_CAPACITY + 1 }>) -> Self {
        Self {
            status,
            body,
            reserved: 0,
        }
    }
}

/// Business-level error returned by `http.request`.
#[repr(u8)]
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    Immutable,
    IntoBytes,
    KnownLayout,
    PartialEq,
    Serialize,
    TryFromBytes,
)]
pub enum HttpRpcError {
    /// URL scheme is unsupported.
    InvalidUrl,
    /// HTTPS was requested without Platform TLS.
    TlsNotConfigured,
    /// Header metadata is invalid.
    InvalidHeader,
    /// Network or HTTP protocol operation failed.
    Transport,
    /// Response body is not UTF-8.
    InvalidResponseText,
    /// Response exceeds the fixed buffer.
    ResponseTooLarge,
}
