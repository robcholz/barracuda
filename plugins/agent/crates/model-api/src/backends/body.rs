//! Request bodies encoded directly into bulk memory.
//!
//! A request is written twice by the same encoder: once to measure its exact
//! length and once into a single bulk allocation of that length. Message
//! history is serialized by reference, the pre-rendered tools array is copied
//! as text, and inline media is base64-encoded in place, so building a request
//! needs no ordinary-heap copy of its content.
//!
//! Values are encoded exactly as `serde_json::to_string` would encode them.

use alloc::string::String;
use core::fmt::{self, Write as _};

use barracuda_bulk_memory::BulkVec;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::de::IgnoredAny;
use serde_json::{Map, Value};

use super::super::errors::Error;

/// Input bytes base64-encoded per step; a multiple of three keeps padding at
/// the end of the whole payload only.
const BASE64_INPUT_CHUNK: usize = 768;
const BASE64_OUTPUT_CHUNK: usize = BASE64_INPUT_CHUNK / 3 * 4;

/// Destination of encoded request bytes.
pub(super) trait Sink {
    fn put(&mut self, bytes: &[u8]);
}

/// Counts the bytes a request will occupy.
struct Measure(usize);

impl Sink for Measure {
    fn put(&mut self, bytes: &[u8]) {
        self.0 = self.0.saturating_add(bytes.len());
    }
}

impl Sink for BulkVec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

struct Formatted<'a>(&'a mut dyn Sink);

impl fmt::Write for Formatted<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.put(text.as_bytes());
        Ok(())
    }
}

/// Encodes a request into one exactly sized bulk buffer.
///
/// `write` must emit the same bytes on every call.
pub(super) fn encode(write: impl Fn(&mut dyn Sink)) -> Result<BulkVec<u8>, Error> {
    let mut measure = Measure(0);
    write(&mut measure);
    let mut body = BulkVec::try_with_capacity(measure.0)
        .map_err(|_error| Error::Api("out of memory encoding request"))?;
    write(&mut body);
    Ok(body)
}

/// Encodes an already built JSON object into one exactly sized bulk buffer.
pub(super) fn encode_object(object: &Map<String, Value>) -> Result<BulkVec<u8>, Error> {
    encode(|sink| write_object(sink, object))
}

/// Writes one JSON object field by field.
pub(super) struct Object<'s> {
    sink: &'s mut dyn Sink,
    empty: bool,
}

impl<'s> Object<'s> {
    pub(super) fn begin(sink: &'s mut dyn Sink) -> Self {
        sink.put(b"{");
        Self { sink, empty: true }
    }

    /// Starts a field and returns the sink for its value.
    pub(super) fn field(&mut self, name: &str) -> &mut dyn Sink {
        if !self.empty {
            self.sink.put(b",");
        }
        self.empty = false;
        write_str(self.sink, name);
        self.sink.put(b":");
        self.sink
    }

    pub(super) fn value(&mut self, name: &str, value: &Value) {
        write_value(self.field(name), value);
    }

    pub(super) fn str(&mut self, name: &str, text: &str) {
        write_str(self.field(name), text);
    }

    pub(super) fn end(self) {
        self.sink.put(b"}");
    }
}

/// Writes a JSON array from a sequence of element writers.
pub(super) fn write_array<'a, I>(sink: &mut dyn Sink, items: I)
where
    I: IntoIterator<Item = &'a Value>,
{
    sink.put(b"[");
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            sink.put(b",");
        }
        write_value(sink, item);
    }
    sink.put(b"]");
}

pub(super) fn write_value(sink: &mut dyn Sink, value: &Value) {
    match value {
        Value::Null => sink.put(b"null"),
        Value::Bool(true) => sink.put(b"true"),
        Value::Bool(false) => sink.put(b"false"),
        // `Number` displays with the formatter `serde_json` serializes with.
        Value::Number(number) => {
            let _ = write!(Formatted(sink), "{number}");
        }
        Value::String(text) => write_str(sink, text),
        Value::Array(items) => write_array(sink, items),
        Value::Object(map) => write_object(sink, map),
    }
}

fn write_object(sink: &mut dyn Sink, map: &Map<String, Value>) {
    let mut object = Object::begin(sink);
    for (name, value) in map {
        object.value(name, value);
    }
    object.end();
}

/// Writes a JSON string with `serde_json`'s escaping.
pub(super) fn write_str(sink: &mut dyn Sink, text: &str) {
    sink.put(b"\"");
    write_escaped(sink, text);
    sink.put(b"\"");
}

/// Writes string contents with `serde_json`'s escaping, without quotes.
fn write_escaped(sink: &mut dyn Sink, text: &str) {
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x0C => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1F => &[
                b'\\',
                b'u',
                b'0',
                b'0',
                hex_digit(byte >> 4),
                hex_digit(byte & 0xF),
            ],
            _ => continue,
        };
        sink.put(bytes.get(start..index).unwrap_or_default());
        sink.put(escape);
        start = index.saturating_add(1);
    }
    sink.put(bytes.get(start..).unwrap_or_default());
}

fn hex_digit(nibble: u8) -> u8 {
    b"0123456789abcdef"
        .get(usize::from(nibble))
        .copied()
        .unwrap_or(b'0')
}

/// Writes `prefix` followed by `bytes` base64-encoded, as one JSON string.
pub(super) fn write_base64_str(sink: &mut dyn Sink, prefix: &str, bytes: &[u8]) {
    let mut output = [0_u8; BASE64_OUTPUT_CHUNK];
    sink.put(b"\"");
    write_escaped(sink, prefix);
    for chunk in bytes.chunks(BASE64_INPUT_CHUNK) {
        if let Ok(written) = STANDARD.encode_slice(chunk, &mut output) {
            sink.put(output.get(..written).unwrap_or_default());
        }
    }
    sink.put(b"\"");
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

/// Copies valid JSON text without insignificant whitespace.
pub(super) fn write_compact(sink: &mut dyn Sink, json: &str) {
    let bytes = json.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b' ' | b'\t' | b'\n' | b'\r' => {
                sink.put(bytes.get(start..index).unwrap_or_default());
                start = index.saturating_add(1);
            }
            _ => {}
        }
    }
    sink.put(bytes.get(start..).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloc::vec::Vec;

    use serde_json::json;

    use super::*;

    fn encoded(write: impl Fn(&mut dyn Sink)) -> String {
        let body = encode(write).unwrap();
        assert_eq!(body.len(), body.capacity(), "body is sized exactly");
        String::from_utf8(body.as_slice().to_vec()).unwrap()
    }

    #[test]
    fn values_encode_exactly_like_serde_json() {
        let values = [
            json!(null),
            json!(true),
            json!([false, 0, -1, u64::MAX, i64::MIN, 0.5, -2.5e-8, 1e300, 3.0]),
            json!("plain ünïcødé 🚀 / slash"),
            json!("quote \" backslash \\ controls \u{0} \u{8} \u{c} \n \r \t \u{1f} \u{7f}"),
            json!({"b": {"nested": [1, {"deep": "x"}]}, "a": [], "c": {}}),
            json!([[[]], {"": ""}]),
        ];
        for value in values {
            let expected = serde_json::to_string(&value).unwrap();
            assert_eq!(encoded(|sink| write_value(sink, &value)), expected);
        }
    }

    #[test]
    fn base64_strings_match_the_standard_engine() {
        for length in [0, 1, 2, 3, 767, 768, 769, 2000] {
            let bytes = (0..length).map(|byte| byte as u8).collect::<Vec<_>>();
            let expected = alloc::format!("\"data:a\\\"b;base64,{}\"", STANDARD.encode(&bytes));
            assert_eq!(
                encoded(|sink| write_base64_str(sink, "data:a\"b;base64,", &bytes)),
                expected
            );
        }
    }

    #[test]
    fn compact_copy_removes_only_insignificant_whitespace() {
        let pretty = "[\n  {\"a\": \"x y\\\" \\\\\",\n   \"b\" : [1, 2]}\n]";
        let compact = encoded(|sink| write_compact(sink, pretty));
        assert_eq!(compact, r#"[{"a":"x y\" \\","b":[1,2]}]"#);
        assert_eq!(
            serde_json::from_str::<Value>(&compact).unwrap(),
            serde_json::from_str::<Value>(pretty).unwrap()
        );
    }

    #[test]
    fn tools_must_be_one_json_array() {
        assert!(validate_tools(" [{\"a\":1}]").is_ok());
        assert!(matches!(
            validate_tools("{\"a\":1}"),
            Err(Error::InvalidToolsJson)
        ));
        assert!(matches!(
            validate_tools("[{\"a\":1}"),
            Err(Error::InvalidToolsJson)
        ));
        assert!(matches!(
            validate_tools("[] trailing"),
            Err(Error::InvalidToolsJson)
        ));
    }
}
