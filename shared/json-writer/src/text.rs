//! One JSON value shared as compact text in bulk memory.

use alloc::string::String;
use core::fmt;

use barracuda_bulk_memory::{BulkAllocError, BulkText};
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::ser::{Error as _, Serialize, Serializer};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

use crate::{write_compact, write_value, Object, Sink};

/// One JSON value stored as compact text in bulk memory.
///
/// Cloning shares the allocation, and a [`field`](Self::field) of an object
/// is a view into the same allocation, so passing a value along or selecting
/// part of it copies nothing. Only the reference count lives in the ordinary
/// heap.
///
/// # Examples
///
/// ```
/// use barracuda_json_writer::JsonText;
/// use serde_json::json;
///
/// let event = JsonText::from_value(&json!({"type": "delta", "payload": {"text": "hi"}}));
/// let payload = event.field("payload").expect("payload");
/// assert_eq!(payload.as_str(), r#"{"text":"hi"}"#);
/// assert_eq!(payload, json!({"text": "hi"}));
/// ```
#[derive(Clone)]
pub struct JsonText {
    text: BulkText,
    start: usize,
    end: usize,
}

/// Failure while building a [`JsonText`].
#[derive(Debug, thiserror::Error)]
pub enum JsonTextError {
    /// The input was not exactly one JSON value.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Bulk memory could not hold the value.
    #[error("not enough bulk memory for a JSON value")]
    OutOfMemory(#[from] BulkAllocError),
}

impl JsonText {
    fn whole(text: BulkText) -> Self {
        let end = text.len();
        Self {
            text,
            start: 0,
            end,
        }
    }

    /// Encodes `value`; exhausting bulk memory aborts like any allocation.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        Self::whole(BulkText::encode(|sink| write_value(sink, value)))
    }

    /// Encodes `value`, reporting bulk-memory exhaustion.
    ///
    /// # Errors
    ///
    /// [`BulkAllocError`] when the encoding cannot be allocated.
    pub fn try_from_value(value: &Value) -> Result<Self, BulkAllocError> {
        Self::try_encode(|sink| write_value(sink, value))
    }

    /// Encodes one object field by field.
    ///
    /// `fields` runs once to measure and once to fill, so it must write the
    /// same fields each time, using the [`Object`] writer for every value.
    ///
    /// # Errors
    ///
    /// [`BulkAllocError`] when the encoding cannot be allocated.
    pub fn try_encode_object(fields: impl Fn(&mut Object<'_>)) -> Result<Self, BulkAllocError> {
        Self::try_encode(|sink| {
            let mut object = Object::begin(sink);
            fields(&mut object);
            object.end();
        })
    }

    /// Encodes one value written with this crate's writers.
    ///
    /// `write` runs once to measure and once to fill; it must write exactly
    /// one compact JSON value, the same each time.
    ///
    /// # Errors
    ///
    /// [`BulkAllocError`] when the encoding cannot be allocated.
    pub fn try_encode(write: impl Fn(&mut dyn Sink)) -> Result<Self, BulkAllocError> {
        BulkText::try_encode(write).map(Self::whole)
    }

    /// Stores already encoded JSON, dropping insignificant whitespace.
    ///
    /// # Errors
    ///
    /// [`JsonTextError`] when `json` is not exactly one JSON value or cannot
    /// be stored.
    pub fn parse(json: &str) -> Result<Self, JsonTextError> {
        serde_json::from_str::<IgnoredAny>(json)?;
        Ok(Self::try_encode(|sink| write_compact(sink, json))?)
    }

    /// Encodes any serializable value.
    ///
    /// The value is serialized once into an ordinary-heap string and then
    /// copied, so reserve this for values that are small or not yet JSON.
    ///
    /// # Errors
    ///
    /// [`JsonTextError`] when serialization fails or the value cannot be
    /// stored.
    pub fn from_serialize<T>(value: &T) -> Result<Self, JsonTextError>
    where
        T: Serialize + ?Sized,
    {
        let json: String = serde_json::to_string(value)?;
        Ok(Self::try_encode(|sink| sink.put(json.as_bytes()))?)
    }

    /// The compact JSON text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.text
            .as_str()
            .get(self.start..self.end)
            .unwrap_or_default()
    }

    /// Whether the value is a JSON object.
    #[must_use]
    pub fn is_object(&self) -> bool {
        self.as_str().starts_with('{')
    }

    /// The top-level `name` field of an object, sharing this allocation.
    ///
    /// `None` when the value is not an object or has no such field.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<Self> {
        let json = self.as_str();
        let raw = serde_json::Deserializer::from_str(json)
            .deserialize_map(FindField(name))
            .ok()??;
        let offset = (raw.get().as_ptr() as usize).checked_sub(json.as_ptr() as usize)?;
        let start = self.start.checked_add(offset)?;
        let end = start.checked_add(raw.get().len())?;
        Some(Self {
            text: self.text.clone(),
            start,
            end,
        })
    }

    /// Decodes the value as `T`, borrowing strings where `T` allows.
    ///
    /// # Errors
    ///
    /// Returns the `serde_json` error when the value does not match `T`.
    pub fn parse_as<'a, T>(&'a self) -> Result<T, serde_json::Error>
    where
        T: Deserialize<'a>,
    {
        serde_json::from_str(self.as_str())
    }

    /// Decodes the whole value into a `serde_json` tree.
    ///
    /// Prefer [`field`](Self::field) and [`parse_as`](Self::parse_as); this
    /// builds an ordinary-heap copy.
    #[must_use]
    pub fn to_value(&self) -> Value {
        self.parse_as().unwrap_or(Value::Null)
    }

    /// The text as one bulk allocation, copying only when this is a view
    /// into a larger value.
    #[must_use]
    pub fn into_bulk_text(self) -> BulkText {
        if self.start == 0 && self.end == self.text.len() {
            self.text
        } else {
            BulkText::new(self.as_str())
        }
    }
}

/// Finds one top-level field without decoding the others.
struct FindField<'n>(&'n str);

impl<'de> Visitor<'de> for FindField<'_> {
    type Value = Option<&'de RawValue>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        // The last duplicate wins, as when decoding into a map.
        let mut found = None;
        while let Some(matches) = map.next_key_seed(KeyIs(self.0))? {
            if matches {
                found = Some(map.next_value::<&'de RawValue>()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

/// Compares an object key with `name` without allocating it.
struct KeyIs<'n>(&'n str);

impl<'de> DeserializeSeed<'de> for KeyIs<'_> {
    type Value = bool;

    fn deserialize<D>(self, deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for KeyIs<'_> {
    type Value = bool;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_str<E>(self, key: &str) -> Result<bool, E>
    where
        E: de::Error,
    {
        Ok(key == self.0)
    }
}

/// Compares decoded content, so key order and number spelling do not matter.
impl PartialEq<Value> for JsonText {
    fn eq(&self, other: &Value) -> bool {
        self.to_value() == *other
    }
}

/// Compares the stored text.
impl PartialEq for JsonText {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for JsonText {}

impl fmt::Debug for JsonText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for JsonText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&Value> for JsonText {
    fn from(value: &Value) -> Self {
        Self::from_value(value)
    }
}

impl From<Value> for JsonText {
    fn from(value: Value) -> Self {
        Self::from_value(&value)
    }
}

/// Serializes as the stored JSON, without re-encoding it.
impl Serialize for JsonText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::from_str::<&RawValue>(self.as_str())
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

/// Copies the value's text into bulk memory without decoding it.
///
/// The input must be borrowable JSON text, as with `serde_json::from_str`.
impl<'de> Deserialize<'de> for JsonText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <&'de RawValue>::deserialize(deserializer)?;
        Self::try_encode(|sink| write_compact(sink, raw.get())).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alloc::vec::Vec;

    use serde_json::json;

    use super::*;

    #[test]
    fn fields_are_views_into_the_same_allocation() {
        let value = json!({"a": 1, "nested": {"x": [1, "y\"z"]}, "s": "ü \\ \n"});
        let text = JsonText::from_value(&value);
        assert_eq!(text.as_str(), serde_json::to_string(&value).unwrap());
        let nested = text.field("nested").unwrap();
        assert_eq!(nested.as_str(), r#"{"x":[1,"y\"z"]}"#);
        assert_eq!(nested.field("x").unwrap().as_str(), r#"[1,"y\"z"]"#);
        assert_eq!(text.field("s").unwrap(), json!("ü \\ \n"));
        assert!(text.field("missing").is_none());
        assert!(text.field("a").unwrap().field("x").is_none());
        assert_eq!(nested.clone().into_bulk_text().as_str(), nested.as_str());
    }

    #[test]
    fn escaped_and_duplicate_keys_resolve_like_a_map() {
        let text = JsonText::parse(r#"{ "key" : 1, "key": 2 }"#).unwrap();
        assert_eq!(text.as_str(), r#"{"key":1,"key":2}"#);
        assert_eq!(text.field("key").unwrap().as_str(), "2");
    }

    #[test]
    fn serde_round_trips_without_reencoding() {
        #[derive(serde::Serialize, Deserialize)]
        struct Request {
            kind: alloc::string::String,
            payload: JsonText,
        }
        let request: Request =
            serde_json::from_str(r#"{"kind":"k","payload":{"b":[1, 2],"a":null}}"#).unwrap();
        assert_eq!(request.payload.as_str(), r#"{"b":[1,2],"a":null}"#);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"kind":"k","payload":{"b":[1,2],"a":null}}"#
        );
        assert!(JsonText::parse("{").is_err());
        let encoded = JsonText::try_encode_object(|object| {
            object.str("text", "hi");
            object.value("n", &json!(3));
        })
        .unwrap();
        assert_eq!(encoded, json!({"text": "hi", "n": 3}));
        let list: Vec<u8> = JsonText::from_value(&json!([1, 2])).parse_as().unwrap();
        assert_eq!(list, [1, 2]);
    }
}
