//! One chat message as shared compact JSON.

use alloc::boxed::Box;
use core::fmt;

use serde::de::{Deserialize, Deserializer, Error as _, IgnoredAny};
use serde::ser::{Error as _, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::Value;

use crate::json::{write_compact, write_value, Object};
use barracuda_bulk_memory::BulkText;

/// One chat message object, stored as compact JSON in bulk memory.
///
/// The bytes are exactly what `serde_json` writes for the message, so they
/// persist, travel through context assembly, and enter request bodies without
/// being re-encoded. Cloning shares the allocation.
///
/// # Examples
///
/// ```
/// use barracuda_agent_message::ChatMessage;
/// use serde_json::json;
///
/// let message = ChatMessage::new(&json!({"role": "user", "content": "hi"}));
/// assert_eq!(message.as_str(), r#"{"content":"hi","role":"user"}"#);
/// assert_eq!(message.to_value()["content"], "hi");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ChatMessage(BulkText);

/// Text that is not one JSON object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChatMessageError;

impl fmt::Display for ChatMessageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("chat message is not a JSON object")
    }
}

impl core::error::Error for ChatMessageError {}

impl ChatMessage {
    /// Encodes a message object.
    #[must_use]
    pub fn new(value: &Value) -> Self {
        Self(BulkText::encode(|sink| write_value(sink, value)))
    }

    /// Encodes a message object field by field.
    ///
    /// `fields` runs once to measure and once to fill, so it must write the
    /// same fields each time. Writing fields in key order reproduces the
    /// `serde_json` encoding of the equivalent `Value`.
    #[must_use]
    pub fn encode_object(fields: impl Fn(&mut Object<'_>)) -> Self {
        Self(BulkText::encode(|sink| {
            let mut object = Object::begin(sink);
            fields(&mut object);
            object.end();
        }))
    }

    /// Stores already encoded JSON, dropping insignificant whitespace.
    ///
    /// # Errors
    ///
    /// [`ChatMessageError`] when `json` is not exactly one JSON object.
    pub fn from_json(json: &str) -> Result<Self, ChatMessageError> {
        if !json.trim_start().starts_with('{') {
            return Err(ChatMessageError);
        }
        serde_json::from_str::<IgnoredAny>(json).map_err(|_error| ChatMessageError)?;
        Ok(Self(BulkText::encode(|sink| write_compact(sink, json))))
    }

    /// The encoded message.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// The encoded message as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Encoded length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the encoding is empty, which a constructed message never is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Decodes the fields a caller needs, borrowing strings where possible.
    ///
    /// # Errors
    ///
    /// Returns the `serde_json` error when the message does not match `T`.
    pub fn parse<'a, T>(&'a self) -> Result<T, serde_json::Error>
    where
        T: Deserialize<'a>,
    {
        serde_json::from_slice(self.as_bytes())
    }

    /// Decodes the whole message into a `serde_json` tree.
    ///
    /// Prefer [`parse`](Self::parse) for individual fields; this builds an
    /// ordinary-heap copy of the message.
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.parse().unwrap_or(Value::Null)
    }

    /// The message as a raw JSON value borrowed from its encoding.
    fn raw(&self) -> Result<&RawValue, serde_json::Error> {
        serde_json::from_slice(self.as_bytes())
    }
}

impl From<&Value> for ChatMessage {
    fn from(value: &Value) -> Self {
        Self::new(value)
    }
}

impl From<Value> for ChatMessage {
    fn from(value: Value) -> Self {
        Self::new(&value)
    }
}

/// Compares decoded content, so field order and whitespace do not matter.
impl PartialEq<Value> for ChatMessage {
    fn eq(&self, other: &Value) -> bool {
        self.to_value() == *other
    }
}

impl fmt::Debug for ChatMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for ChatMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Serializes as the stored JSON, without re-encoding it.
impl Serialize for ChatMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.raw().map_err(S::Error::custom)?.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ChatMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::from_json(raw.get()).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use serde::Deserialize;
    use serde_json::json;

    use super::*;

    #[test]
    fn messages_store_serde_json_bytes_and_round_trip() {
        let value =
            json!({"role": "tool", "tool_call_id": "c1", "content": "a\n\"b\"", "is_error": false});
        let message = ChatMessage::new(&value);
        assert_eq!(message.as_str(), serde_json::to_string(&value).unwrap());
        assert_eq!(message.to_value(), value);
        assert_eq!(ChatMessage::from_json(message.as_str()).unwrap(), message);
    }

    #[test]
    fn encode_object_matches_the_equivalent_value() {
        let message = ChatMessage::encode_object(|object| {
            object.str("content", "x");
            object.value("is_error", &Value::Bool(true));
            object.str("role", "tool");
        });
        assert_eq!(
            message,
            ChatMessage::new(&json!({"role": "tool", "content": "x", "is_error": true}))
        );
    }

    #[test]
    fn from_json_compacts_and_rejects_non_objects() {
        let message =
            ChatMessage::from_json("{ \"role\" : \"user\",\n \"content\": \"x y\" }").unwrap();
        assert_eq!(message.as_str(), r#"{"role":"user","content":"x y"}"#);
        assert_eq!(ChatMessage::from_json("[1]"), Err(ChatMessageError));
        assert_eq!(
            ChatMessage::from_json("{\"a\":1} trailing"),
            Err(ChatMessageError)
        );
    }

    #[test]
    fn serde_writes_and_reads_messages_verbatim() {
        #[derive(serde::Serialize, Deserialize)]
        struct Record {
            id: u32,
            msgs: Vec<ChatMessage>,
        }
        let record = Record {
            id: 7,
            msgs: vec![
                ChatMessage::new(&json!({"role": "user", "content": "hi"})),
                ChatMessage::new(&json!({"content": "yo", "role": "assistant"})),
            ],
        };
        let line = serde_json::to_string(&record).unwrap();
        assert_eq!(
            line,
            r#"{"id":7,"msgs":[{"content":"hi","role":"user"},{"content":"yo","role":"assistant"}]}"#
        );
        let decoded: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded.msgs, record.msgs);

        let mut array = String::new();
        let encoded = crate::json::encode(|sink| crate::json::write_messages(sink, &record.msgs));
        array.push_str(core::str::from_utf8(&encoded).unwrap());
        assert_eq!(array, serde_json::to_string(&record.msgs).unwrap());
    }

    #[test]
    fn parse_borrows_requested_fields() {
        #[derive(Deserialize)]
        struct Role<'a> {
            role: &'a str,
        }
        let message = ChatMessage::new(&json!({"role": "assistant", "content": "x"}));
        assert_eq!(message.parse::<Role<'_>>().unwrap().role, "assistant");
    }
}
