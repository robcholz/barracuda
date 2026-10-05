//! Request bodies encoded directly into bulk memory.
//!
//! A request is written twice by the shared message encoder: once to measure
//! its exact length and once into a single bulk allocation of that length.
//! History messages are copied as their stored JSON, the pre-rendered tools
//! array is copied as text, and inline media is base64-encoded in place, so
//! building a request needs no ordinary-heap copy of its content.

use alloc::string::String;

use barracuda_agent_message::json::{self, write_object, Sink};
use barracuda_bulk_memory::BulkVec;
use serde::de::IgnoredAny;
use serde_json::{Map, Value};

use super::super::errors::Error;

pub(super) use barracuda_agent_message::json::{
    write_base64_str, write_compact, write_str, Object,
};

/// Encodes a request into one exactly sized bulk buffer.
pub(super) fn encode(write: impl Fn(&mut dyn Sink)) -> Result<BulkVec<u8>, Error> {
    json::try_encode(write).map_err(|_error| Error::Api("out of memory encoding request"))
}

/// Encodes an already built JSON object into one exactly sized bulk buffer.
pub(super) fn encode_object(object: &Map<String, Value>) -> Result<BulkVec<u8>, Error> {
    encode(|sink| write_object(sink, object))
}

/// Checks that pre-rendered tools text is one JSON array.
pub(super) fn validate_tools(tools_json: &str) -> Result<(), Error> {
    if !tools_json.trim_start().starts_with('[') {
        return Err(Error::InvalidToolsJson);
    }
    serde_json::from_str::<IgnoredAny>(tools_json)
        .map(|_| ())
        .map_err(|_| Error::InvalidToolsJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_must_be_one_json_array() {
        assert!(validate_tools(" [{\"a\":1}]").is_ok());
        for invalid in ["{\"a\":1}", "[{\"a\":1}", "[] trailing"] {
            assert!(matches!(
                validate_tools(invalid),
                Err(Error::InvalidToolsJson)
            ));
        }
    }
}
