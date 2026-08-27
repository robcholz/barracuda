//! Fixed-layout wire contracts for Tavily web search.
#![cfg_attr(not(feature = "schema"), no_std)]

extern crate alloc;

use alloc::string::String;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

/// Maximum query bytes, including the terminating NUL.
pub const QUERY_CAPACITY: usize = 256;
/// Maximum result title bytes, including the terminating NUL.
pub const TITLE_CAPACITY: usize = 96;
/// Maximum result URL bytes, including the terminating NUL.
pub const URL_CAPACITY: usize = 160;
/// Maximum result excerpt bytes, including the terminating NUL.
pub const CONTENT_CAPACITY: usize = 224;

/// Failure to encode or decode bounded Tavily text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TavilyWireError {
    /// Text does not fit in its RPC field.
    #[error("text exceeds the Tavily RPC field capacity")]
    TextTooLong,
    /// Text contains a NUL byte.
    #[error("text contains a NUL byte")]
    EmbeddedNul,
    /// Wire text is not canonically NUL-terminated.
    #[error("text is not canonically NUL-terminated")]
    InvalidTerminator,
    /// Wire text is not valid UTF-8.
    #[error("text is not valid UTF-8")]
    InvalidUtf8,
}

/// Fixed-capacity, NUL-terminated UTF-8 text serialized as a JSON string.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, KnownLayout, PartialEq, TryFromBytes)]
pub struct TavilyText<const N: usize>([u8; N]);

impl<const N: usize> TavilyText<N> {
    /// Encodes a string in this field.
    pub fn new(value: &str) -> Result<Self, TavilyWireError> {
        let bytes = value.as_bytes();
        if bytes.len() >= N {
            return Err(TavilyWireError::TextTooLong);
        }
        if bytes.contains(&0) {
            return Err(TavilyWireError::EmbeddedNul);
        }
        let mut buffer = [0; N];
        buffer
            .get_mut(..bytes.len())
            .ok_or(TavilyWireError::TextTooLong)?
            .copy_from_slice(bytes);
        Ok(Self(buffer))
    }

    /// Decodes the canonical UTF-8 string.
    pub fn as_str(&self) -> Result<&str, TavilyWireError> {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(TavilyWireError::InvalidTerminator)?;
        if self
            .0
            .get(end..)
            .ok_or(TavilyWireError::InvalidTerminator)?
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(TavilyWireError::InvalidTerminator);
        }
        core::str::from_utf8(
            self.0
                .get(..end)
                .ok_or(TavilyWireError::InvalidTerminator)?,
        )
        .map_err(|_error| TavilyWireError::InvalidUtf8)
    }
}

impl<const N: usize> Serialize for TavilyText<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de, const N: usize> Deserialize<'de> for TavilyText<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Request accepted by `tavily.search`.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TavilySearchRequest {
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    query: TavilyText<QUERY_CAPACITY>,
    /// Maximum number of results, from one through ten.
    pub max_results: u8,
}

impl TavilySearchRequest {
    /// Creates a bounded search request.
    pub fn new(query: &str, max_results: u8) -> Result<Self, TavilyWireError> {
        Ok(Self {
            query: TavilyText::new(query)?,
            max_results,
        })
    }

    /// Returns the search query.
    pub fn query(&self) -> Result<&str, TavilyWireError> {
        self.query.as_str()
    }
}

#[cfg(feature = "schema")]
barracuda_rpc_schema::register!(TavilySearchRequest);

/// One result frame returned by `tavily.search`.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TavilySearchResult {
    title: TavilyText<TITLE_CAPACITY>,
    url: TavilyText<URL_CAPACITY>,
    content: TavilyText<CONTENT_CAPACITY>,
    /// Tavily relevance score.
    pub score: f32,
}

impl TavilySearchResult {
    /// Creates a bounded result frame.
    pub fn new(title: &str, url: &str, content: &str, score: f32) -> Result<Self, TavilyWireError> {
        Ok(Self {
            title: TavilyText::new(title)?,
            url: TavilyText::new(url)?,
            content: TavilyText::new(content)?,
            score,
        })
    }
    /// Returns the result title.
    pub fn title(&self) -> Result<&str, TavilyWireError> {
        self.title.as_str()
    }
    /// Returns the result URL.
    pub fn url(&self) -> Result<&str, TavilyWireError> {
        self.url.as_str()
    }
    /// Returns the result excerpt.
    pub fn content(&self) -> Result<&str, TavilyWireError> {
        self.content.as_str()
    }
}

/// Business failure returned by `tavily.search`.
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
#[serde(rename_all = "snake_case")]
pub enum TavilySearchError {
    /// The Plugin has not received configuration.
    NotConfigured,
    /// The request is invalid.
    InvalidRequest,
    /// The HTTP request failed.
    Transport,
    /// Tavily rejected the request.
    Service,
    /// Tavily returned an invalid response.
    InvalidResponse,
}
