//! Fixed-layout wire contracts for web search.
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

/// Failure to encode or decode bounded web-search text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WebSearchWireError {
    /// Text does not fit in its RPC field.
    #[error("text exceeds the web-search RPC field capacity")]
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
pub struct WebSearchText<const N: usize>([u8; N]);

impl<const N: usize> WebSearchText<N> {
    /// Encodes a string in this field.
    pub fn new(value: &str) -> Result<Self, WebSearchWireError> {
        let bytes = value.as_bytes();
        if bytes.len() >= N {
            return Err(WebSearchWireError::TextTooLong);
        }
        if bytes.contains(&0) {
            return Err(WebSearchWireError::EmbeddedNul);
        }
        let mut buffer = [0; N];
        buffer
            .get_mut(..bytes.len())
            .ok_or(WebSearchWireError::TextTooLong)?
            .copy_from_slice(bytes);
        Ok(Self(buffer))
    }

    /// Decodes the canonical UTF-8 string.
    pub fn as_str(&self) -> Result<&str, WebSearchWireError> {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .ok_or(WebSearchWireError::InvalidTerminator)?;
        if self
            .0
            .get(end..)
            .ok_or(WebSearchWireError::InvalidTerminator)?
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(WebSearchWireError::InvalidTerminator);
        }
        core::str::from_utf8(
            self.0
                .get(..end)
                .ok_or(WebSearchWireError::InvalidTerminator)?,
        )
        .map_err(|_error| WebSearchWireError::InvalidUtf8)
    }
}

impl<const N: usize> Serialize for WebSearchText<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str().map_err(serde::ser::Error::custom)?)
    }
}

impl<'de, const N: usize> Deserialize<'de> for WebSearchText<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Request accepted by `web_search.search`.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebSearchRequest {
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    query: WebSearchText<QUERY_CAPACITY>,
    /// Maximum number of results, from one through ten.
    pub max_results: u8,
}

impl WebSearchRequest {
    /// Creates a bounded search request.
    pub fn new(query: &str, max_results: u8) -> Result<Self, WebSearchWireError> {
        Ok(Self {
            query: WebSearchText::new(query)?,
            max_results,
        })
    }

    /// Returns the search query.
    pub fn query(&self) -> Result<&str, WebSearchWireError> {
        self.query.as_str()
    }
}

#[cfg(feature = "schema")]
barracuda_rpc_schema::register!(WebSearchRequest);

/// One result frame returned by `web_search.search`.
#[repr(C)]
#[barracuda_rpc::rpc_message]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WebSearchResult {
    title: WebSearchText<TITLE_CAPACITY>,
    url: WebSearchText<URL_CAPACITY>,
    content: WebSearchText<CONTENT_CAPACITY>,
    /// Search-provider relevance score.
    pub score: f32,
}

impl WebSearchResult {
    /// Creates a bounded result frame.
    pub fn new(
        title: &str,
        url: &str,
        content: &str,
        score: f32,
    ) -> Result<Self, WebSearchWireError> {
        Ok(Self {
            title: WebSearchText::new(title)?,
            url: WebSearchText::new(url)?,
            content: WebSearchText::new(content)?,
            score,
        })
    }
    /// Returns the result title.
    pub fn title(&self) -> Result<&str, WebSearchWireError> {
        self.title.as_str()
    }
    /// Returns the result URL.
    pub fn url(&self) -> Result<&str, WebSearchWireError> {
        self.url.as_str()
    }
    /// Returns the result excerpt.
    pub fn content(&self) -> Result<&str, WebSearchWireError> {
        self.content.as_str()
    }
}

/// Business failure returned by `web_search.search`.
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
pub enum WebSearchError {
    /// The Plugin has not received configuration.
    NotConfigured,
    /// The request is invalid.
    InvalidRequest,
    /// The HTTP request failed.
    Transport,
    /// The search provider rejected the request.
    Service,
    /// The search provider returned an invalid response.
    InvalidResponse,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn public_request_and_result_text_round_trip_through_json() {
        let request = WebSearchRequest::new("rust embedded", 3).unwrap();
        assert_eq!(request.query(), Ok("rust embedded"));
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({"query":"rust embedded","max_results":3})
        );
        let decoded: WebSearchRequest = serde_json::from_value(serde_json::json!({
            "query":"coverage",
            "max_results":2
        }))
        .unwrap();
        assert_eq!(decoded.query(), Ok("coverage"));

        let result = WebSearchResult::new(
            "Coverage report",
            "https://example.test/report",
            "Measured behavior",
            0.9,
        )
        .unwrap();
        assert_eq!(result.title(), Ok("Coverage report"));
        assert_eq!(result.url(), Ok("https://example.test/report"));
        assert_eq!(result.content(), Ok("Measured behavior"));
    }

    #[test]
    fn bounded_text_rejects_ambiguous_and_noncanonical_wire_values() {
        assert_eq!(
            WebSearchText::<4>::new("four"),
            Err(WebSearchWireError::TextTooLong)
        );
        assert_eq!(
            WebSearchText::<8>::new("a\0b"),
            Err(WebSearchWireError::EmbeddedNul)
        );

        let no_terminator = WebSearchText::<4>(*b"four");
        assert_eq!(
            no_terminator.as_str(),
            Err(WebSearchWireError::InvalidTerminator)
        );
        let trailing_data = WebSearchText::<4>([b'a', 0, b'b', 0]);
        assert_eq!(
            trailing_data.as_str(),
            Err(WebSearchWireError::InvalidTerminator)
        );
        let invalid_utf8 = WebSearchText::<4>([0xff, 0, 0, 0]);
        assert_eq!(invalid_utf8.as_str(), Err(WebSearchWireError::InvalidUtf8));
    }
}
